// Phase 2a: RoutingEngine replaces TravelMatrix.
//   - Hex grid → petgraph DiGraph, 8-connected adjacency, weight 1 per hop.
//   - All-pairs travel times precomputed at build time via Dijkstra.
//   - route_between: lazy A* with RwLock cache (Sync for Rayon districts).
// Phase 2b hook: from_graph() accepts any externally-built RoadGraph (e.g. OSM).

use std::collections::HashMap;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use geo::Point;
use petgraph::algo::{astar, dijkstra};
use petgraph::graph::{Graph, NodeIndex};
use rayon::prelude::*;

use crate::geo_utils::haversine_m;
use crate::hex::HexCoord;
use crate::types::NodeId;

// ---------------------------------------------------------------------------
// Road graph (import API for Phase 2b OSM)
// ---------------------------------------------------------------------------

pub struct Node {
    pub id: NodeId,
    pub position: Point,
}

pub struct Edge {
    pub travel_time_min: u32,
}

pub type RoadGraph = Graph<Node, Edge>;

// ---------------------------------------------------------------------------
// Serializable snapshot for routing cache
// ---------------------------------------------------------------------------

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct RoutingSnapshot {
    pub nodes: Vec<(u32, f64, f64)>,    // (node_id_raw, lon, lat)
    pub edges: Vec<(u32, u32, u32)>,    // (from_id_raw, to_id_raw, travel_time_min)
    pub anchors: Vec<u32>,              // anchor node ID values, in matrix index order
    pub matrix: Vec<u32>,              // row-major K×K travel times; u32::MAX = unreachable
}

// ---------------------------------------------------------------------------
// Travel-time resolution counters
// ---------------------------------------------------------------------------

/// How each `travel_time` query was answered. Used purely for instrumentation
/// (surfaced in the post-run report) so the precomputed-vs-lazy-vs-fallback
/// split is observable.
#[derive(Debug, Default, Clone, Copy)]
pub struct RouteStats {
    /// Hit the precomputed anchor↔anchor matrix (the fast O(1) common case).
    pub source_forward: u64,
    /// Reserved (was symmetry-reverse row lookup); always 0 with the full matrix.
    pub source_reverse: u64,
    /// Resolved by a freshly computed exact A* shortest path.
    pub exact_computed: u64,
    /// Resolved from the memoised exact-cost cache (a repeat of a residual pair).
    pub exact_cached:   u64,
    /// Fell back to a haversine estimate because no path exists (disconnected).
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
// Routing engine (Phase 2a)
// ---------------------------------------------------------------------------

pub struct RoutingEngine {
    graph:      RoadGraph,
    node_index: HashMap<NodeId, NodeIndex>,
    /// Anchor nodes in matrix index order (cell anchors + station nodes). Every
    /// dispatch endpoint is one of these.
    anchors:       Vec<NodeId>,
    /// Anchor node → its row/column index in `matrix`.
    anchor_index:  HashMap<NodeId, u32>,
    /// Dense row-major K×K travel-time matrix between all anchor pairs
    /// (`matrix[i*K + j]` = anchors[i] → anchors[j]); `u32::MAX` = unreachable.
    /// Every query between two anchors is an O(1) lookup, no online search.
    matrix:        Vec<u32>,
    /// Lazily-populated route cache; RwLock makes RoutingEngine Sync for Rayon.
    routes: RwLock<HashMap<(NodeId, NodeId), Vec<NodeId>>>,
    /// Lazily-computed exact travel times for pairs where *neither* endpoint is
    /// a precomputed source (e.g. incident → incident). Memoised; RwLock for Sync.
    costs:  RwLock<HashMap<(NodeId, NodeId), u32>>,
    /// Per-tier resolution counters for `travel_time` (instrumentation only).
    counters: RouteCounters,
}

impl RoutingEngine {
    /// Build from hex grid: every hex = one node, 8-connected adjacency, weight 1.
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
                    graph.add_edge(from_nx, to_nx, Edge { travel_time_min: 1 });
                }
            }
        }

        let all_anchors: Vec<NodeId> = node_index.keys().copied().collect();
        Self::build(graph, node_index, &all_anchors)
    }

    /// Build from an external road graph (OSM subgraph).
    /// Precomputes the full anchor↔anchor travel-time matrix over the `sources`
    /// (the cell anchors + station nodes). Every dispatch endpoint is an anchor,
    /// so every `travel_time` query becomes an O(1) matrix lookup. Pairs with a
    /// non-anchor endpoint (rare) are resolved lazily via A*.
    pub fn from_graph(graph: RoadGraph, sources: &[NodeId]) -> Self {
        let node_index: HashMap<NodeId, NodeIndex> = graph
            .node_indices()
            .map(|nx| (graph[nx].id, nx))
            .collect();
        Self::build(graph, node_index, sources)
    }

    fn build(graph: RoadGraph, node_index: HashMap<NodeId, NodeIndex>, sources: &[NodeId]) -> Self {
        // The anchor set: every source node present in the graph, deduplicated,
        // in a stable index order. These index the K×K matrix below.
        let mut anchors: Vec<NodeId> = Vec::with_capacity(sources.len());
        let mut anchor_index: HashMap<NodeId, u32> = HashMap::with_capacity(sources.len());
        for &s in sources {
            if node_index.contains_key(&s) && !anchor_index.contains_key(&s) {
                anchor_index.insert(s, anchors.len() as u32);
                anchors.push(s);
            }
        }
        let k = anchors.len();

        // One Dijkstra per anchor (in parallel); each yields that anchor's row of
        // K costs. `par_iter().flat_map().collect()` preserves order, so anchor i's
        // row lands at matrix[i*K .. (i+1)*K]. `u32::MAX` marks unreachable pairs.
        let matrix: Vec<u32> = anchors
            .par_iter()
            .flat_map_iter(|&src| {
                let src_nx = node_index[&src];
                let dist = dijkstra(&graph, src_nx, None, |e| e.weight().travel_time_min);
                let mut row = vec![u32::MAX; k];
                for (target_nx, cost) in dist {
                    if let Some(&j) = anchor_index.get(&graph[target_nx].id) {
                        row[j as usize] = cost;
                    }
                }
                row
            })
            .collect();

        Self {
            graph,
            node_index,
            anchors,
            anchor_index,
            matrix,
            routes:   RwLock::new(HashMap::new()),
            costs:    RwLock::new(HashMap::new()),
            counters: RouteCounters::default(),
        }
    }

    pub fn contains_node(&self, id: NodeId) -> bool {
        self.node_index.contains_key(&id)
    }

    /// Travel time in simulated minutes between two nodes.
    ///
    /// Resolution order:
    /// 1. The precomputed anchor↔anchor matrix `from → to` (O(1), lock-free) —
    ///    the dominant path, since every dispatch endpoint (station, patrol
    ///    waypoint, incident) is a hex anchor.
    /// 2. An exact A* shortest path, memoised in `costs` — used only when an
    ///    endpoint is *not* an anchor (rare). Cached, so it stays cheap.
    /// 3. A haversine estimate at 30 km/h — only if the two nodes are not
    ///    connected in the graph at all.
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
        if let Some(&t) = self.costs.read().unwrap().get(&(from, to)) {
            self.counters.exact_cached.fetch_add(1, Ordering::Relaxed);
            return t;
        }
        let t = match self.exact_cost(from, to) {
            Some(c) => {
                self.counters.exact_computed.fetch_add(1, Ordering::Relaxed);
                c
            }
            None => {
                self.counters.haversine.fetch_add(1, Ordering::Relaxed);
                self.haversine_fallback(from, to)
            }
        };
        self.costs.write().unwrap().insert((from, to), t);
        t
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

    /// Exact point-to-point travel time via A* on the full graph. `None` if a
    /// node is absent or no path exists. Used for the residual pairs that no
    /// precomputed source row covers.
    fn exact_cost(&self, from: NodeId, to: NodeId) -> Option<u32> {
        let from_nx = *self.node_index.get(&from)?;
        let to_nx   = *self.node_index.get(&to)?;
        // Admissible heuristic: straight-line distance at the fastest road speed
        // (100 km/h ≈ 1666 m/min) can never overestimate the true travel time.
        const MAX_SPEED_M_PER_MIN: f64 = 100_000.0 / 60.0;
        let goal = self.graph[to_nx].position;
        astar(
            &self.graph,
            from_nx,
            |nx| nx == to_nx,
            |e| e.weight().travel_time_min,
            |nx| {
                let p = self.graph[nx].position;
                (haversine_m(p.y(), p.x(), goal.y(), goal.x()) / MAX_SPEED_M_PER_MIN) as u32
            },
        )
        .map(|(cost, _)| cost)
    }

    /// Haversine estimate at 30 km/h between two node positions.
    /// Falls back to 1 min only if one of the nodes is not in the graph at all.
    fn haversine_fallback(&self, from: NodeId, to: NodeId) -> u32 {
        const SPEED_M_PER_MIN: f64 = 30_000.0 / 60.0; // 30 km/h
        let fp = self.node_index.get(&from).map(|&nx| self.graph[nx].position);
        let tp = self.node_index.get(&to).map(|&nx| self.graph[nx].position);
        match (fp, tp) {
            (Some(fp), Some(tp)) => {
                // Node positions are stored as Point(lon, lat) in the geo crate.
                let dist_m = haversine_m(fp.y(), fp.x(), tp.y(), tp.x());
                ((dist_m / SPEED_M_PER_MIN) as u32).max(1)
            }
            _ => 1,
        }
    }

    /// Shortest path between two nodes; lazily computed and cached.
    pub fn route_between(&self, from: NodeId, to: NodeId) -> Vec<NodeId> {
        if from == to {
            return vec![from];
        }

        {
            let cache = self.routes.read().unwrap();
            if let Some(route) = cache.get(&(from, to)) {
                return route.clone();
            }
        }

        let route = self.compute_route(from, to);
        self.routes.write().unwrap().insert((from, to), route.clone());
        route
    }

    /// Isochrone: all nodes reachable from `source` within `max_minutes`.
    /// Returns a map of NodeId → travel_time_min using full-graph Dijkstra
    /// (not limited to precomputed anchors).
    pub fn isochrone_from(&self, source: NodeId, max_minutes: u32) -> HashMap<NodeId, u32> {
        let Some(&source_nx) = self.node_index.get(&source) else {
            return HashMap::new();
        };
        dijkstra(&self.graph, source_nx, None, |e| e.weight().travel_time_min)
            .into_iter()
            .filter(|&(_, cost)| cost <= max_minutes)
            .map(|(nx, cost)| (self.graph[nx].id, cost))
            .collect()
    }

    /// Position (lon, lat) of a node in the graph.
    pub fn node_position(&self, id: NodeId) -> Option<(f64, f64)> {
        self.node_index.get(&id).map(|&nx| {
            let p = self.graph[nx].position;
            (p.x(), p.y()) // geo::Point stores (lon, lat)
        })
    }

    /// All node IDs with their positions (lon, lat) in the graph.
    pub fn all_node_positions(&self) -> Vec<(NodeId, f64, f64)> {
        self.graph.node_indices()
            .map(|nx| {
                let n = &self.graph[nx];
                (n.id, n.position.x(), n.position.y())
            })
            .collect()
    }

    /// Extract a serializable snapshot of this engine's graph and precomputed times.
    pub fn to_snapshot(&self) -> RoutingSnapshot {
        let nodes: Vec<(u32, f64, f64)> = self.graph.node_indices().map(|nx| {
            let n = &self.graph[nx];
            (n.id.value(), n.position.x(), n.position.y()) // (id, lon, lat)
        }).collect();

        let edges: Vec<(u32, u32, u32)> = self.graph.edge_indices().map(|ex| {
            let (a, b) = self.graph.edge_endpoints(ex).unwrap();
            (self.graph[a].id.value(), self.graph[b].id.value(), self.graph[ex].travel_time_min)
        }).collect();

        let anchors: Vec<u32> = self.anchors.iter().map(|n| n.value()).collect();

        RoutingSnapshot { nodes, edges, anchors, matrix: self.matrix.clone() }
    }

    /// Reconstruct a RoutingEngine from a cached snapshot (no Dijkstra needed).
    pub fn from_snapshot(snap: RoutingSnapshot) -> Self {
        let mut graph = RoadGraph::new();
        let mut id_to_nx: HashMap<u32, NodeIndex> = HashMap::with_capacity(snap.nodes.len());

        for &(id_raw, lon, lat) in &snap.nodes {
            let nx = graph.add_node(Node {
                id: NodeId::new(id_raw),
                position: Point::new(lon, lat),
            });
            id_to_nx.insert(id_raw, nx);
        }

        for &(from_raw, to_raw, tt) in &snap.edges {
            if let (Some(&a), Some(&b)) = (id_to_nx.get(&from_raw), id_to_nx.get(&to_raw)) {
                graph.add_edge(a, b, Edge { travel_time_min: tt });
            }
        }

        let node_index: HashMap<NodeId, NodeIndex> = graph.node_indices()
            .map(|nx| (graph[nx].id, nx))
            .collect();

        let anchors: Vec<NodeId> = snap.anchors.iter().map(|&v| NodeId::new(v)).collect();
        let anchor_index: HashMap<NodeId, u32> = anchors.iter().enumerate()
            .map(|(i, &n)| (n, i as u32))
            .collect();

        Self {
            graph,
            node_index,
            anchors,
            anchor_index,
            matrix: snap.matrix,
            routes:   RwLock::new(HashMap::new()),
            costs:    RwLock::new(HashMap::new()),
            counters: RouteCounters::default(),
        }
    }

    fn compute_route(&self, from: NodeId, to: NodeId) -> Vec<NodeId> {
        let (Some(&from_nx), Some(&to_nx)) =
            (self.node_index.get(&from), self.node_index.get(&to))
        else {
            return vec![from, to];
        };

        astar(
            &self.graph,
            from_nx,
            |nx| nx == to_nx,
            |e| e.weight().travel_time_min,
            |_| 0u32,
        )
        .map(|(_, path)| path.iter().map(|&nx| self.graph[nx].id).collect())
        .unwrap_or_else(|| vec![from, to])
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
    fn route_between_valid_path() {
        let engine = RoutingEngine::from_hex_grid(&make_grid(3, 3));
        let tl = NodeId::from_hex(&HexCoord::new(0, 0));
        let br = NodeId::from_hex(&HexCoord::new(2, 2));
        let route = engine.route_between(tl, br);
        assert_eq!(route[0], tl);
        assert_eq!(*route.last().unwrap(), br);
        // Diagonal step: optimal path has 3 nodes (start, mid, end) or just 2 if direct diagonal
        assert!(route.len() >= 2 && route.len() <= 3);
    }

    #[test]
    fn route_between_same_node() {
        let engine = RoutingEngine::from_hex_grid(&make_grid(3, 3));
        let a = NodeId::from_hex(&HexCoord::new(1, 1));
        assert_eq!(engine.route_between(a, a), vec![a]);
    }

    /// A line graph A—B—C—D with bidirectional, equal-weight edges. Node `i`
    /// sits at lon `i*0.01`, lat 0 — far enough apart that a haversine estimate
    /// is clearly distinct from the true path cost.
    fn line_graph() -> (RoadGraph, Vec<NodeId>) {
        let mut g = RoadGraph::new();
        let ids: Vec<NodeId> = (0..4).map(NodeId::new).collect();
        let nxs: Vec<NodeIndex> = ids.iter().enumerate()
            .map(|(i, &id)| g.add_node(Node { id, position: Point::new(i as f64 * 0.01, 0.0) }))
            .collect();
        for &(a, b, w) in &[(0usize, 1usize, 2u32), (1, 2, 3), (2, 3, 4)] {
            g.add_edge(nxs[a], nxs[b], Edge { travel_time_min: w });
            g.add_edge(nxs[b], nxs[a], Edge { travel_time_min: w });
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
        assert_eq!(s.exact_computed, 0);
    }

    #[test]
    fn non_anchor_pair_uses_exact_path_not_haversine() {
        let (g, ids) = line_graph();
        // Only A and D are anchors; B and C are not in the matrix.
        let engine = RoutingEngine::from_graph(g, &[ids[0], ids[3]]);
        // B → C: neither endpoint is an anchor → exact A* = 3.
        // (A 30 km/h haversine over ~1.1 km would give ~2, so 3 proves the
        // real path was used.)
        assert_eq!(engine.travel_time(ids[1], ids[2]), 3);
        assert_eq!(engine.route_stats().exact_computed, 1);
    }

    #[test]
    fn disconnected_pair_falls_back_to_haversine() {
        let (mut g, ids) = line_graph();
        // An island node with no edges, ~70 km east of A.
        let island = NodeId::new(99);
        g.add_node(Node { id: island, position: Point::new(1.0, 0.0) });
        let engine = RoutingEngine::from_graph(g, &ids); // island is not a source
        // No path exists → A* fails → positive haversine estimate (not a panic).
        assert!(engine.travel_time(ids[0], island) >= 1);
        assert_eq!(engine.route_stats().haversine, 1);
    }
}
