// Travel-time engine.
//
// Every dispatch endpoint in the simulator — police station, patrol waypoint,
// and (since incidents spawn at their cell anchor) incident — is a hex *anchor*
// node. So at run time the engine is nothing more than a dense anchor↔anchor
// travel-time matrix plus the anchors' positions: `travel_time` is an O(1)
// lookup and there is no road graph in the simulator at all.
//
// The matrix is built once, offline, in the `optimize` binary: it constructs the
// full OSM `RoadGraph`, runs one Dijkstra per anchor to fill the matrix, captures
// the anchor positions, and serialises only those (`RoutingSnapshot`). The graph
// is discarded after the build and never reaches the simulator.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use geo::Point;
use petgraph::algo::dijkstra;
use petgraph::graph::{Graph, NodeIndex};
use rayon::prelude::*;

use crate::geo_utils::haversine_m;
use crate::hex::HexCoord;
use crate::types::NodeId;

// ---------------------------------------------------------------------------
// Road graph
//
// Used only transiently while building the matrix (and by `osm.rs`, which parses
// OSM into these types). The simulator never holds a `RoadGraph`.
// ---------------------------------------------------------------------------

pub struct Node {
    pub id: NodeId,
    pub position: Point,
}

pub struct Edge {
    /// Edge traversal time in **seconds**. Sub-minute road segments must not be
    /// rounded per edge (that inflates summed paths); the matrix converts the
    /// accumulated seconds to minutes once, at build time.
    pub travel_time_sec: u32,
}

pub type RoadGraph = Graph<Node, Edge>;

// ---------------------------------------------------------------------------
// Serializable snapshot for the routing cache
// ---------------------------------------------------------------------------

/// What the routing cache stores: the anchor set (with positions, in matrix
/// index order) and the dense row-major K×K travel-time matrix. No road graph —
/// the simulator reconstructs the engine from this alone.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct RoutingSnapshot {
    pub anchors: Vec<(u32, f64, f64)>, // (node_id_raw, lon, lat), matrix index order
    pub matrix:  Vec<u32>,             // row-major K×K travel times; u32::MAX = unreachable
}

// ---------------------------------------------------------------------------
// Travel-time resolution counters
// ---------------------------------------------------------------------------

/// How each `travel_time` query was answered. Used purely for instrumentation
/// (surfaced in the post-run report) so the matrix-vs-fallback split is
/// observable. In a healthy run every query is a matrix hit.
#[derive(Debug, Default, Clone, Copy)]
pub struct RouteStats {
    /// Hit the precomputed anchor↔anchor matrix (the fast O(1) common case).
    pub source_forward: u64,
    /// Reserved (was symmetry-reverse row lookup); always 0 with the full matrix.
    pub source_reverse: u64,
    /// Reserved (was freshly computed A*); always 0 — the simulator has no graph.
    pub exact_computed: u64,
    /// Reserved (was the exact-cost memo cache); always 0.
    pub exact_cached:   u64,
    /// Fell back to a haversine estimate (a non-anchor endpoint or an
    /// unreachable anchor pair). Should be ~0.
    pub haversine:      u64,
}

impl RouteStats {
    pub fn total(&self) -> u64 {
        self.source_forward + self.source_reverse + self.exact_computed
            + self.exact_cached + self.haversine
    }
}

/// Atomic counters behind `RouteStats`. `travel_time` takes `&self` and is
/// called concurrently across rayon workers, so the counts are atomic; `Relaxed`
/// is sufficient since we only need an accurate final tally, not ordering.
#[derive(Default)]
struct RouteCounters {
    source_forward: AtomicU64,
    source_reverse: AtomicU64,
    exact_computed: AtomicU64,
    exact_cached:   AtomicU64,
    haversine:      AtomicU64,
}

// ---------------------------------------------------------------------------
// Routing engine — a travel-time matrix over the anchor set
// ---------------------------------------------------------------------------

pub struct RoutingEngine {
    /// Anchor nodes in matrix index order (cell anchors + station nodes). Every
    /// dispatch endpoint is one of these.
    anchors:      Vec<NodeId>,
    /// Anchor node → its row/column index in `matrix`.
    anchor_index: HashMap<NodeId, u32>,
    /// Anchor positions `(lon, lat)`, parallel to `anchors`. Used for output
    /// coordinates and the haversine safety net.
    anchor_pos:   Vec<(f64, f64)>,
    /// Dense row-major K×K travel-time matrix (`matrix[i*K + j]` = anchors[i] →
    /// anchors[j]); `u32::MAX` = unreachable. Every query is an O(1) lookup.
    matrix:       Vec<u32>,
    /// Per-tier resolution counters for `travel_time` (instrumentation only).
    counters:     RouteCounters,
}

impl RoutingEngine {
    /// Build from hex grid: every hex = one node, 8-connected adjacency, weight 1.
    /// All nodes are anchors. Used by tests and small synthetic scenarios.
    pub fn from_hex_grid(hexes: &[(NodeId, HexCoord)]) -> Self {
        let mut graph = RoadGraph::new();
        let mut node_index: HashMap<NodeId, NodeIndex> = HashMap::with_capacity(hexes.len());
        let mut coord_index: HashMap<(i32, i32), NodeIndex> = HashMap::with_capacity(hexes.len());

        for &(node_id, coord) in hexes {
            let nx = graph.add_node(Node {
                id:       node_id,
                position: Point::new(coord.col as f64, coord.row as f64),
            });
            node_index.insert(node_id, nx);
            coord_index.insert((coord.col, coord.row), nx);
        }

        const NEIGHBOURS: [(i32, i32); 8] = [
            (-1, 0), (1, 0), (0, -1), (0, 1),
            (-1, -1), (-1, 1), (1, -1), (1, 1),
        ];
        for &(node_id, coord) in hexes {
            let from_nx = node_index[&node_id];
            for (dc, dr) in NEIGHBOURS {
                if let Some(&to_nx) = coord_index.get(&(coord.col + dc, coord.row + dr)) {
                    // 60 s = 1 min per hop, so a hex step converts to 1 minute.
                    graph.add_edge(from_nx, to_nx, Edge { travel_time_sec: 60 });
                }
            }
        }

        let all_anchors: Vec<NodeId> = node_index.keys().copied().collect();
        Self::build(graph, node_index, &all_anchors)
    }

    /// Build from an external road graph (OSM subgraph). Precomputes the full
    /// anchor↔anchor travel-time matrix over the `sources` (the cell anchors +
    /// station nodes) and captures their positions; the graph is then dropped.
    pub fn from_graph(graph: RoadGraph, sources: &[NodeId]) -> Self {
        let node_index: HashMap<NodeId, NodeIndex> = graph
            .node_indices()
            .map(|nx| (graph[nx].id, nx))
            .collect();
        Self::build(graph, node_index, sources)
    }

    fn build(graph: RoadGraph, node_index: HashMap<NodeId, NodeIndex>, sources: &[NodeId]) -> Self {
        // The anchor set: every source node present in the graph, deduplicated,
        // in a stable index order. These index the K×K matrix below, and we
        // capture each anchor's position here so the simulator needs no graph.
        let mut anchors: Vec<NodeId> = Vec::with_capacity(sources.len());
        let mut anchor_index: HashMap<NodeId, u32> = HashMap::with_capacity(sources.len());
        let mut anchor_pos: Vec<(f64, f64)> = Vec::with_capacity(sources.len());
        for &s in sources {
            if let Some(&nx) = node_index.get(&s) {
                if !anchor_index.contains_key(&s) {
                    anchor_index.insert(s, anchors.len() as u32);
                    anchors.push(s);
                    let p = graph[nx].position;
                    anchor_pos.push((p.x(), p.y())); // (lon, lat)
                }
            }
        }
        let k = anchors.len();

        // One Dijkstra per anchor (in parallel); each yields that anchor's row of
        // K costs. `par_iter().flat_map_iter().collect()` preserves order, so
        // anchor i's row lands at matrix[i*K .. (i+1)*K]. `u32::MAX` marks
        // unreachable pairs.
        let matrix: Vec<u32> = anchors
            .par_iter()
            .flat_map_iter(|&src| {
                let src_nx = node_index[&src];
                // Dijkstra accumulates edge times in seconds (exact, no per-edge
                // rounding); convert the full-path total to minutes once here.
                let dist = dijkstra(&graph, src_nx, None, |e| e.weight().travel_time_sec);
                let mut row = vec![u32::MAX; k];
                for (target_nx, cost_sec) in dist {
                    if let Some(&j) = anchor_index.get(&graph[target_nx].id) {
                        row[j as usize] = (cost_sec + 30) / 60; // seconds → minutes, rounded
                    }
                }
                row
            })
            .collect();

        // The graph has done its job (matrix + positions captured); it is dropped
        // here and never enters the simulator.
        Self { anchors, anchor_index, anchor_pos, matrix, counters: RouteCounters::default() }
    }

    /// Whether `id` is a known anchor (and therefore answerable from the matrix).
    pub fn contains_node(&self, id: NodeId) -> bool {
        self.anchor_index.contains_key(&id)
    }

    /// Travel time in simulated minutes between two nodes.
    ///
    /// The common (and, in the simulator, only) path is an O(1) lookup into the
    /// precomputed anchor↔anchor matrix. A haversine estimate at 30 km/h is the
    /// sole fallback — used only if an endpoint is not an anchor or the anchor
    /// pair is unreachable, neither of which occurs in a normal run.
    pub fn travel_time(&self, from: NodeId, to: NodeId) -> u32 {
        if from == to {
            return 0;
        }
        if let (Some(&i), Some(&j)) =
            (self.anchor_index.get(&from), self.anchor_index.get(&to))
        {
            let t = self.matrix[i as usize * self.anchors.len() + j as usize];
            if t != u32::MAX {
                self.counters.source_forward.fetch_add(1, Ordering::Relaxed);
                return t;
            }
        }
        self.counters.haversine.fetch_add(1, Ordering::Relaxed);
        self.haversine_fallback(from, to)
    }

    /// Snapshot of the travel-time resolution counters (see `RouteStats`).
    /// All districts share one engine, so this reflects the whole simulation.
    pub fn route_stats(&self) -> RouteStats {
        RouteStats {
            source_forward: self.counters.source_forward.load(Ordering::Relaxed),
            source_reverse: self.counters.source_reverse.load(Ordering::Relaxed),
            exact_computed: self.counters.exact_computed.load(Ordering::Relaxed),
            exact_cached:   self.counters.exact_cached.load(Ordering::Relaxed),
            haversine:      self.counters.haversine.load(Ordering::Relaxed),
        }
    }

    /// Haversine estimate at 30 km/h between two anchors' positions. Returns 1 if
    /// either node is not a known anchor (no position to estimate from).
    fn haversine_fallback(&self, from: NodeId, to: NodeId) -> u32 {
        const SPEED_M_PER_MIN: f64 = 30_000.0 / 60.0; // 30 km/h
        let fp = self.anchor_index.get(&from).map(|&i| self.anchor_pos[i as usize]);
        let tp = self.anchor_index.get(&to).map(|&i| self.anchor_pos[i as usize]);
        match (fp, tp) {
            // Positions are (lon, lat); haversine_m takes (lat, lon, lat, lon).
            (Some((flon, flat)), Some((tlon, tlat))) => {
                let dist_m = haversine_m(flat, flon, tlat, tlon);
                ((dist_m / SPEED_M_PER_MIN) as u32).max(1)
            }
            _ => 1,
        }
    }

    /// "Route" between two anchors, for output/recording only. The simulator has
    /// no road graph, so we record a straight segment between the two anchor
    /// (hex-centre) positions — the travel *time* is still the exact matrix value.
    pub fn route_between(&self, from: NodeId, to: NodeId) -> Vec<NodeId> {
        if from == to {
            return vec![from];
        }
        vec![from, to]
    }

    /// Position (lon, lat) of an anchor node, if known.
    pub fn node_position(&self, id: NodeId) -> Option<(f64, f64)> {
        self.anchor_index.get(&id).map(|&i| self.anchor_pos[i as usize])
    }

    /// Extract a serializable snapshot of the anchor set and travel-time matrix.
    pub fn to_snapshot(&self) -> RoutingSnapshot {
        let anchors: Vec<(u32, f64, f64)> = self.anchors.iter().enumerate()
            .map(|(i, n)| {
                let (lon, lat) = self.anchor_pos[i];
                (n.value(), lon, lat)
            })
            .collect();
        RoutingSnapshot { anchors, matrix: self.matrix.clone() }
    }

    /// Reconstruct a RoutingEngine from a cached snapshot (no graph, no Dijkstra).
    pub fn from_snapshot(snap: RoutingSnapshot) -> Self {
        let mut anchors: Vec<NodeId> = Vec::with_capacity(snap.anchors.len());
        let mut anchor_index: HashMap<NodeId, u32> = HashMap::with_capacity(snap.anchors.len());
        let mut anchor_pos: Vec<(f64, f64)> = Vec::with_capacity(snap.anchors.len());
        for (i, &(id_raw, lon, lat)) in snap.anchors.iter().enumerate() {
            let id = NodeId::new(id_raw);
            anchor_index.insert(id, i as u32);
            anchors.push(id);
            anchor_pos.push((lon, lat));
        }
        Self {
            anchors,
            anchor_index,
            anchor_pos,
            matrix: snap.matrix,
            counters: RouteCounters::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_grid(cols: i32, rows: i32) -> Vec<(NodeId, HexCoord)> {
        (0..cols)
            .flat_map(|col| (0..rows).map(move |row| {
                let coord = HexCoord::new(col, row);
                (NodeId::from_hex(&coord), coord)
            }))
            .collect()
    }

    #[test]
    fn hex_grid_adjacent_cost_is_one() {
        let engine = RoutingEngine::from_hex_grid(&make_grid(3, 3));
        let a = NodeId::from_hex(&HexCoord::new(1, 1));
        let b = NodeId::from_hex(&HexCoord::new(2, 1));
        assert_eq!(engine.travel_time(a, b), 1);
    }

    #[test]
    fn hex_grid_diagonal_cost_is_one() {
        let engine = RoutingEngine::from_hex_grid(&make_grid(3, 3));
        let a = NodeId::from_hex(&HexCoord::new(0, 0));
        let b = NodeId::from_hex(&HexCoord::new(1, 1));
        assert_eq!(engine.travel_time(a, b), 1);
    }

    #[test]
    fn hex_grid_corner_to_corner_cost() {
        // 3×3 grid: corners are 2 Chebyshev steps apart.
        let engine = RoutingEngine::from_hex_grid(&make_grid(3, 3));
        let tl = NodeId::from_hex(&HexCoord::new(0, 0));
        let br = NodeId::from_hex(&HexCoord::new(2, 2));
        assert_eq!(engine.travel_time(tl, br), 2);
    }

    #[test]
    fn route_between_is_straight_segment() {
        let engine = RoutingEngine::from_hex_grid(&make_grid(3, 3));
        let tl = NodeId::from_hex(&HexCoord::new(0, 0));
        let br = NodeId::from_hex(&HexCoord::new(2, 2));
        // No road graph in the sim: a recorded route is the two endpoints.
        assert_eq!(engine.route_between(tl, br), vec![tl, br]);
    }

    #[test]
    fn route_between_same_node() {
        let engine = RoutingEngine::from_hex_grid(&make_grid(3, 3));
        let a = NodeId::from_hex(&HexCoord::new(1, 1));
        assert_eq!(engine.route_between(a, a), vec![a]);
    }

    /// A line graph A—B—C—D with bidirectional edges. Node `i` sits at
    /// lon `i*0.01`, lat 0. Edge weights are in seconds (120/180/240 s), so the
    /// A→D path totals 540 s = 9 min after the matrix's once-per-cell rounding.
    fn line_graph() -> (RoadGraph, Vec<NodeId>) {
        let mut g = RoadGraph::new();
        let ids: Vec<NodeId> = (0..4).map(NodeId::new).collect();
        let nxs: Vec<NodeIndex> = ids.iter().enumerate()
            .map(|(i, &id)| g.add_node(Node { id, position: Point::new(i as f64 * 0.01, 0.0) }))
            .collect();
        for &(a, b, w) in &[(0usize, 1usize, 120u32), (1, 2, 180), (2, 3, 240)] {
            g.add_edge(nxs[a], nxs[b], Edge { travel_time_sec: w });
            g.add_edge(nxs[b], nxs[a], Edge { travel_time_sec: w });
        }
        (g, ids)
    }

    #[test]
    fn matrix_serves_anchor_pairs_both_directions() {
        let (g, ids) = line_graph();
        let engine = RoutingEngine::from_graph(g, &ids); // all nodes are anchors
        // A → D via the matrix: 2 + 3 + 4 = 9.
        assert_eq!(engine.travel_time(ids[0], ids[3]), 9);
        // D → A is stored independently in the matrix (both directions): 9.
        assert_eq!(engine.travel_time(ids[3], ids[0]), 9);
        let s = engine.route_stats();
        assert_eq!(s.source_forward, 2); // both answered by the matrix
        assert_eq!(s.haversine, 0);
    }

    #[test]
    fn non_anchor_pair_falls_back_to_haversine() {
        let (g, ids) = line_graph();
        // Only A and D are anchors; B and C are not in the matrix.
        let engine = RoutingEngine::from_graph(g, &[ids[0], ids[3]]);
        // B → C: neither endpoint is an anchor → no matrix entry → haversine.
        assert!(engine.travel_time(ids[1], ids[2]) >= 1);
        let s = engine.route_stats();
        assert_eq!(s.haversine, 1);
        assert_eq!(s.source_forward, 0);
    }

    #[test]
    fn unreachable_or_unknown_target_falls_back_to_haversine() {
        let (g, ids) = line_graph();
        let engine = RoutingEngine::from_graph(g, &ids);
        // A node that is not an anchor at all → haversine fallback (returns 1,
        // since the unknown node has no stored position), not a panic.
        let unknown = NodeId::new(99);
        assert!(engine.travel_time(ids[0], unknown) >= 1);
        assert_eq!(engine.route_stats().haversine, 1);
    }
}
