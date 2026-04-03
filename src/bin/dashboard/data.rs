use serde::Deserialize;
use std::io::Read;

#[derive(Deserialize)]
pub struct HexEntry {
    pub lat: f64,
    pub lon: f64,
    pub district_id: u32,
}

#[derive(Deserialize)]
pub struct StationEntry {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
}

pub fn load_hexes() -> Vec<HexEntry> {
    let path = "config/hexes.json";
    let mut file = match std::fs::File::open(path) {
        Ok(f)  => f,
        Err(_) => return Vec::new(),
    };
    let mut buf = String::new();
    file.read_to_string(&mut buf).ok();
    serde_json::from_str(&buf).unwrap_or_default()
}

pub fn load_stations() -> Vec<StationEntry> {
    let path = "config/police_stations.json";
    let mut file = match std::fs::File::open(path) {
        Ok(f)  => f,
        Err(_) => return Vec::new(),
    };
    let mut buf = String::new();
    file.read_to_string(&mut buf).ok();
    serde_json::from_str(&buf).unwrap_or_default()
}

pub fn bounds(hexes: &[HexEntry]) -> ((f64, f64), (f64, f64)) {
    if hexes.is_empty() {
        return ((0.0, 1.0), (0.0, 1.0));
    }
    let lat_min = hexes.iter().map(|h| h.lat).fold(f64::MAX, f64::min);
    let lat_max = hexes.iter().map(|h| h.lat).fold(f64::MIN, f64::max);
    let lon_min = hexes.iter().map(|h| h.lon).fold(f64::MAX, f64::min);
    let lon_max = hexes.iter().map(|h| h.lon).fold(f64::MIN, f64::max);
    ((lat_min, lat_max), (lon_min, lon_max))
}
