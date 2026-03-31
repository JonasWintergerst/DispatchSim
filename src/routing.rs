// Phase 1: travel times derived from hex grid Chebyshev distance.
// Phase 2: replace TravelMatrix with per-district petgraph + Dijkstra;
//          the travel_time / route_between interface stays the same.

use std::collections::HashMap;

use geo::Point;
use petgraph::graph::Graph;

use crate::district::District;
use crate::hex::HexCoord;
use crate::types::NodeId;

// ---------------------------------------------------------------------------
// Road graph (Phase 2 placeholder)
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
// Travel matrix (Phase 1)
// ---------------------------------------------------------------------------

/// Pairwise travel durations between all nodes in the city, keyed by (from, to).
pub struct TravelMatrix {
    times: HashMap<(NodeId, NodeId), u32>,
}

impl TravelMatrix {
    pub fn from_districts(districts: &[District]) -> Self {
        let nodes: Vec<(NodeId, HexCoord)> = districts
            .iter()
            .flat_map(|d| d.hexes.iter().map(|h| (h.node_id(), h.coord())))
            .collect();

        let mut times = HashMap::new();
        for &(id_a, coord_a) in &nodes {
            for &(id_b, coord_b) in &nodes {
                times.insert((id_a, id_b), travel_minutes(&coord_a, &coord_b));
            }
        }

        Self { times }
    }

    /// Travel time in simulated minutes between two nodes.
    /// Returns 1 as a safe default if either node is unknown.
    pub fn travel_time(&self, from: NodeId, to: NodeId) -> u32 {
        self.times.get(&(from, to)).copied().unwrap_or(1)
    }

    /// Phase 1: direct [from, to] hop.
    /// Phase 2: replace with Dijkstra over the district's road graph.
    pub fn route_between(&self, from: NodeId, to: NodeId) -> Vec<NodeId> {
        if from == to { vec![from] } else { vec![from, to] }
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Chebyshev distance between two hex cells scaled to simulated minutes.
///
/// At 50×50 hexes ≈ 25km × 25km city each hex is ~500m.
/// At 30 km/h average urban speed that is roughly 1 minute per hex.
fn travel_minutes(a: &HexCoord, b: &HexCoord) -> u32 {
    const MINUTES_PER_HEX: u32 = 1;
    let col_dist = (a.col - b.col).unsigned_abs();
    let row_dist = (a.row - b.row).unsigned_abs();
    (col_dist.max(row_dist) * MINUTES_PER_HEX).max(1)
}
