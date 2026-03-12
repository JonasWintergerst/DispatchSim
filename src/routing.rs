

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

/// Travel matrix uses station positions, not centroids
/// (units depart from the station, not the district center)
pub fn build_travel_matrix(
    districts: &[DistrictConfig],
    hex_radius: f64,
) -> Vec<Vec<f64>> {
    let stations: Vec<(i32, i32)> = districts
        .iter()
        .map(|d| (d.station_hex.col, d.station_hex.row) )
        .collect();

    stations
        .iter()
        .map(|&a| {
            stations
                .iter()
                .map(|&b| travel_time_min(a, b, 40.0))
                .collect()
        })
        .collect()
}

fn travel_time_min(a: (i32, i32), b: (i32, i32), speed: f64) -> f64 {
    let dx = (b.0 - a.0) as f64;
    let dy = (b.1 - a.1) as f64;

    let distance = (dx * dx + dy * dy).sqrt();
    
    // time in hours -> convert to minutes
    (distance / speed) * 60.0
}