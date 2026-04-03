// Phase 2a: RoutingEngine replaces TravelMatrix.
//   - Hex grid → petgraph DiGraph, 8-connected adjacency, weight 1 per hop.
//   - All-pairs travel times precomputed at build time via Dijkstra.
//   - route_between: lazy A* with RwLock cache (Sync for Rayon districts).
// Phase 2b hook: from_graph() accepts any externally-built RoadGraph (e.g. OSM).

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

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
// Routing engine (Phase 2a)
// ---------------------------------------------------------------------------

pub struct RoutingEngine {
    graph:      RoadGraph,
    node_index: HashMap<NodeId, NodeIndex>,
    /// Precomputed all-pairs travel times; O(1) lookup.
    times:  HashMap<(NodeId, NodeId), u32>,
    /// Lazily-populated route cache; RwLock makes RoutingEngine Sync for Rayon.
    routes: RwLock<HashMap<(NodeId, NodeId), Vec<NodeId>>>,
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
    /// Only precomputes Dijkstra from `anchor_nodes` — the subset of nodes that
    /// are actual incident/station locations. Keeps `travel_time()` O(1) for
    /// anchor→any queries without running all-pairs on a large OSM subgraph.
    pub fn from_graph(graph: RoadGraph, anchor_nodes: &[NodeId]) -> Self {
        let node_index: HashMap<NodeId, NodeIndex> = graph
            .node_indices()
            .map(|nx| (graph[nx].id, nx))
            .collect();
        Self::build(graph, node_index, anchor_nodes)
    }

    fn build(graph: RoadGraph, node_index: HashMap<NodeId, NodeIndex>, anchors: &[NodeId]) -> Self {
        // Only keep anchor→anchor travel times; anchor→intermediate-node distances
        // are never queried and would otherwise inflate memory to O(|anchors|×|graph|).
        let anchor_ids: HashSet<NodeId> = anchors.iter().copied().collect();

        let times: HashMap<(NodeId, NodeId), u32> = anchors
            .par_iter()
            .filter_map(|&node_id| node_index.get(&node_id).map(|&nx| (node_id, nx)))
            .flat_map(|(node_id, nx)| {
                dijkstra(&graph, nx, None, |e| e.weight().travel_time_min)
                    .into_iter()
                    .filter(|(target_nx, _)| anchor_ids.contains(&graph[*target_nx].id))
                    .map(|(target_nx, cost)| ((node_id, graph[target_nx].id), cost))
                    .collect::<Vec<_>>()
            })
            .collect();

        Self { graph, node_index, times, routes: RwLock::new(HashMap::new()) }
    }

    pub fn contains_node(&self, id: NodeId) -> bool {
        self.node_index.contains_key(&id)
    }

    /// Travel time in simulated minutes between two nodes. O(1) lookup.
    /// Falls back to a haversine estimate at 30 km/h when the pair is not
    /// in the precomputed table (e.g. disconnected subgraph or missing anchor).
    pub fn travel_time(&self, from: NodeId, to: NodeId) -> u32 {
        if let Some(&t) = self.times.get(&(from, to)) {
            return t;
        }
        self.haversine_fallback(from, to)
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
            _ => {
                eprintln!(
                    "routing: travel_time({:?} → {:?}) node not in graph; using 1 min",
                    from, to
                );
                1
            }
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

    fn compute_route(&self, from: NodeId, to: NodeId) -> Vec<NodeId> {
        let (Some(&from_nx), Some(&to_nx)) =
            (self.node_index.get(&from), self.node_index.get(&to))
        else {
            eprintln!("routing: compute_route({:?} → {:?}) node not in graph", from, to);
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
}
