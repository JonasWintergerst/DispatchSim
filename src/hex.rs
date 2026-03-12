use serde::{Serialize, Deserialize};

use crate::types::{ DistrictId, HexId, SpawnProfileId, NodeId };
use crate::config::DistrictConfig;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HexCoord {
    pub col: i32,
    pub row: i32,
}

pub struct Hex {
    location: HexCoord,
    district: DistrictId,
    pub spawn_profile_id: SpawnProfileId,
    pub nearest_road_node: NodeId,
}

/// Given a world (x, y) position, find which hex it's in
pub fn world_to_hex(x: f64, y: f64, radius_km: f64) -> HexCoord {
    let col = (x / (radius_km * 1.5)).round() as i32;
    let row = ((y / (radius_km * 3f64.sqrt() / 2.0)) - col as f64 / 2.0).round() as i32;
    HexCoord { col, row }
}

/// Find which district owns a given hex
pub fn district_for_hex<'a>(
    hex: &HexCoord,
    districts: &'a [DistrictConfig],
) -> Option<&'a DistrictConfig> {
    districts.iter().find(|d| {
        d.hexes.iter().any(|h| h.col == hex.col && h.row == hex.row)
    })
}
