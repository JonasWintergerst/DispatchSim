

use crate::types::{NodeId, Duration};
use crate::config::{DistrictConfig};
use crate::hex::HexCoord;

pub struct Node {
    pub id: NodeId,
    pub position: geo::Point,
}

pub struct Edge {
    pub travel_time: Duration,
}

use petgraph::graph::Graph;

pub type RoadGraph = Graph<Node, Edge>;

impl From<DistrictConfig> for RoadGraph {
    fn from(cfg: DistrictConfig) -> Self {
        let mut graph = RoadGraph::new();

        graph
    }
}

pub fn create_route(start: NodeId, end: NodeId) -> Vec<NodeId> {
    return vec![]
}

// routing.rs
// Phase 1: travel times derived from hex grid distance.
// Phase 2: replace with petgraph Dijkstra per district.

use std::collections::HashMap;

use crate::district::District;

// ---------------------------------------------------------------------------
// Travel Matrix
// ---------------------------------------------------------------------------

/// Pairwise travel durations between nodes (hex-derived NodeIds).
///
/// Phase 1: populated from straight-line hex grid distance scaled by a
/// fixed speed constant. No road graph needed.
///
/// Phase 2: replaced by per-district petgraph + Dijkstra. The interface
/// (travel_time / route_between) stays the same so callers don't change.
pub struct TravelMatrix {
    /// Keyed by (from, to) NodeId pair → travel time in simulated minutes.
    times: HashMap<(NodeId, NodeId), u32>,
}

impl TravelMatrix {
    /// Build the matrix from the districts that have already been constructed.
    /// Collects every node across all districts and computes pairwise times.
    pub fn from_districts(districts: &[District]) -> Self {
        // Collect all (NodeId, HexCoord) pairs across every district.
        let nodes: Vec<(NodeId, HexCoord)> = districts
            .iter()
            .flat_map(|d| {
                d.hexes.iter().map(|h| (h.node_id(), h.coord().clone()))
            })
            .collect();

        let mut times = HashMap::new();

        for (id_a, coord_a) in &nodes {
            for (id_b, coord_b) in &nodes {
                let mins = travel_minutes(coord_a, coord_b);
                times.insert((id_a.clone(), id_b.clone()), mins);
            }
        }

        Self { times }
    }

    /// Travel time in simulated minutes between two nodes.
    /// Returns None if either node is unknown (shouldn't happen post-validation).
    pub fn travel_time(&self, from: &NodeId, to: &NodeId) -> Option<u32> {
        self.times.get(&(from.clone(), to.clone())).copied()
    }

    /// Build a route (sequence of NodeIds) from `from` to `to`.
    ///
    /// Phase 1: direct hop — [from, to]. No intermediate nodes on the hex
    /// grid; the unit just travels straight there in `travel_time` ticks.
    ///
    /// Phase 2: replace with Dijkstra over the district's petgraph, returning
    /// the full path. The Unit struct stores this Vec<NodeId> unchanged.
    pub fn route_between(&self, from: &NodeId, to: &NodeId) -> Vec<NodeId> {
        if from == to {
            return vec![from.clone()];
        }
        vec![from.clone(), to.clone()]
    }
}

// ---------------------------------------------------------------------------
// Travel time calculation
// ---------------------------------------------------------------------------

/// Simulated minutes to travel between two hex coordinates.
///
/// Uses Chebyshev distance (max of col/row delta) which is the natural
/// distance metric for a hex grid with the offset layout used here.
/// Scaled by MINUTES_PER_HEX — tune this constant to match realistic
/// response times for your city scale.
///
/// At 50×50 hexes representing roughly a 25km × 25km city:
///   each hex ≈ 500m → at 30 km/h average urban speed → ~1 min per hex.
fn travel_minutes(a: &HexCoord, b: &HexCoord) -> u32 {
    const MINUTES_PER_HEX: u32 = 1;

    let col_dist = (a.col - b.col).unsigned_abs();
    let row_dist = (a.row - b.row).unsigned_abs();
    let hex_dist = col_dist.max(row_dist); // Chebyshev distance

    (hex_dist * MINUTES_PER_HEX).max(1) // minimum 1 minute even for adjacent hexes
}