use serde::{Serialize, Deserialize};

use crate::types::{ DistrictId, HexId, SpawnProfileId, NodeId };
use crate::config::DistrictConfig;

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
pub struct HexCoord {
    pub col: i32,
    pub row: i32,
}

impl HexCoord {
    pub fn new(col: i32, row: i32) -> Self {
        Self{
            col: col, 
            row: row
        }
    }
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

impl Hex {
    pub fn new(
        coord: HexCoord,
        district_id: DistrictId,
        spawn_profile_id: SpawnProfileId,
        nearest_road_node: NodeId,
    ) -> Self {
        Hex { location: coord, district: district_id, spawn_profile_id, nearest_road_node }
    }

    pub fn node_id(&self) -> NodeId {
        self.nearest_road_node
    }

    pub fn coord(&self) -> HexCoord {
        self.location
    }
}