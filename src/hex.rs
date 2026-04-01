use serde::{Deserialize, Serialize};

use crate::types::{DistrictId, HexId, NodeId, SpawnProfileId};

// ---------------------------------------------------------------------------
// Synthetic grid coordinate
// ---------------------------------------------------------------------------

/// Kept for RoutingEngine::from_hex_grid and its tests.
/// Not used by the H3-based Hex struct below.
#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
pub struct HexCoord {
    pub col: i32,
    pub row: i32,
}

impl HexCoord {
    pub fn new(col: i32, row: i32) -> Self { Self { col, row } }
}

// ---------------------------------------------------------------------------
// H3-based hex cell
// ---------------------------------------------------------------------------

/// A spatial hex cell backed by an H3 cell at the configured resolution.
/// The optimizer generates these and writes them to hexes.json; the simulator
/// reads them and assigns them to districts.
pub struct Hex {
    pub id:                HexId,
    /// Raw H3 cell index (resolution-encoded u64).
    pub h3_index:          u64,
    /// Geographic centre of the H3 cell (degrees).
    pub lat:               f64,
    pub lon:               f64,
    pub district:          DistrictId,
    pub spawn_profile_id:  SpawnProfileId,
    /// Nearest OSM road node, used as the routing anchor for this hex.
    pub nearest_road_node: NodeId,
}

impl Hex {
    pub fn node_id(&self) -> NodeId { self.nearest_road_node }
}
