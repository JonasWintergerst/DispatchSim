use serde::{Deserialize, Serialize};

use crate::types::{DistrictId, HexId, NodeId, SpawnProfileId};

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
pub struct HexCoord {
    pub col: i32,
    pub row: i32,
}

impl HexCoord {
    pub fn new(col: i32, row: i32) -> Self { Self { col, row } }
}

pub struct Hex {
    pub id: HexId,
    pub location: HexCoord,
    pub district: DistrictId,
    pub spawn_profile_id: SpawnProfileId,
    pub nearest_road_node: NodeId,
}

impl Hex {
    pub fn new(
        id: HexId,
        coord: HexCoord,
        district: DistrictId,
        spawn_profile_id: SpawnProfileId,
        nearest_road_node: NodeId,
    ) -> Self {
        Hex { id, location: coord, district, spawn_profile_id, nearest_road_node }
    }

    pub fn node_id(&self) -> NodeId { self.nearest_road_node }
    pub fn coord(&self) -> HexCoord { self.location }
}

/// Convert a world (x, y) position to hex grid coordinates.
pub fn world_to_hex(x: f64, y: f64, radius_km: f64) -> HexCoord {
    let col = (x / (radius_km * 1.5)).round() as i32;
    let row = ((y / (radius_km * 3f64.sqrt() / 2.0)) - col as f64 / 2.0).round() as i32;
    HexCoord { col, row }
}
