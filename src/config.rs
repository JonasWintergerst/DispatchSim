use serde::{Serialize, Deserialize};

use crate::types::SimeType;
use crate::hex::HexCoord;


#[derive(Serialize, Deserialize)]
pub struct CityConfig {
    pub city: CityMeta,
    pub districts: Vec<DistrictConfig>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CityMeta {
    pub name: String,
    pub hex_radius: f64,        // km
    pub sim_duration_days: u64,
    pub tick_duration_min: u64,
    pub sim_type: SimeType,
}

// #[derive(Serialize, Deserialize)]
// pub struct DistrictConfig {
//     pub id: u32,
//     pub nodes: Vec<NodeConfig>,
//     pub edges: Vec<EdgeConfig>,
//     pub units: Vec<UnitConfig>,
    
// }

#[derive(Debug, Serialize, Deserialize)]
pub struct DistrictConfig {
    pub id: u32,
    pub name: String,
    pub station_hex: HexCoord,   // must be one of the hexes below
    pub hexes: Vec<HexCoord>,    // all hexes owned by this district
    pub units: UnitCounts,
}


#[derive(Debug, Serialize, Deserialize)]
pub struct UnitCounts {
    pub police: u32,
}


#[derive(Serialize, Deserialize)]
pub struct UnitConfig {

}

#[derive(Serialize, Deserialize)]
pub struct NodeConfig {

}

#[derive(Serialize, Deserialize)]
pub struct EdgeConfig {

}



