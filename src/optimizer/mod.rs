// optimizer/mod.rs
// Solver trait, shared types, and output serialisation.

pub mod greedy;
pub mod h3_grid;

use crate::types::DistrictId;

// ---------------------------------------------------------------------------
// Solver trait
// ---------------------------------------------------------------------------

pub trait Solver: Send + Sync {
    fn solve(&self, problem: &Problem) -> Result<Solution, OptimizerError>;
}

// ---------------------------------------------------------------------------
// Problem
// ---------------------------------------------------------------------------

pub struct Problem {
    pub hexes:       Vec<H3Hex>,
    pub n_districts: usize,
    pub constraints: Constraints,
    pub objective:   ObjectiveWeights,
}

/// A single H3 cell — the unit of spatial analysis in the optimizer.
#[derive(Clone)]
pub struct H3Hex {
    /// Raw H3 cell index (u64).
    pub index:            u64,
    /// Geographic centre of the cell (degrees).
    pub lat:              f64,
    pub lon:              f64,
    /// Expected incidents per minute at this cell (base_lambda / 60).
    pub spawn_rate:       f64,
    pub profile_id:       String,
    /// Nearest OSM road node assigned by OsmGraph::nearest_node.
    pub nearest_osm_node: u32,
}

pub struct Constraints {
    pub contiguity:         bool,
    pub max_workload_ratio: Option<f64>,
}

pub struct ObjectiveWeights {
    pub travel_time:      f64,
    pub workload_balance: f64,
}

// ---------------------------------------------------------------------------
// Solution
// ---------------------------------------------------------------------------

pub struct Solution {
    /// For each district: the index (into Problem::hexes) of the station hex.
    pub station_hex_indices: Vec<usize>,
    /// For each hex (index into Problem::hexes): which district it belongs to.
    pub assignments:         Vec<usize>,
    /// Weighted travel-time objective (lower is better).
    pub objective:           f64,
    /// Total expected incident load per district.
    pub district_loads:      Vec<f64>,
}

impl Solution {
    /// District id for hex i (district indices are 0-based and map to DistrictId).
    pub fn district_of(&self, i: usize) -> DistrictId {
        DistrictId::new(self.assignments[i] as u32)
    }
}

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct OptimizerError(pub String);

impl std::fmt::Display for OptimizerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "optimizer error: {}", self.0)
    }
}

impl std::error::Error for OptimizerError {}

// ---------------------------------------------------------------------------
// Output serialisation
// ---------------------------------------------------------------------------

/// Write the optimizer solution as a flat JSON array to `path`.
/// This is the hexes.json consumed by the simulator.
pub fn write_hexes_json(
    hexes: &[H3Hex],
    solution: &Solution,
    path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use serde_json::{json, Value};

    let entries: Vec<Value> = hexes.iter().enumerate().map(|(i, h)| {
        json!({
            "h3_index":         h.index,
            "lat":              h.lat,
            "lon":              h.lon,
            "district_id":      solution.assignments[i] as u32,
            "spawn_profile_id": h.profile_id,
            "nearest_osm_node": h.nearest_osm_node,
        })
    }).collect();

    let json_str = serde_json::to_string_pretty(&entries)?;
    std::fs::write(path, json_str)?;
    Ok(())
}
