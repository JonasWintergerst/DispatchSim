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

#[derive(Deserialize)]
pub struct DistrictEntry {
    pub district_id: u32,
    pub station_lat: f64,
    pub station_lon: f64,
    pub station_name: String,
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

pub fn load_district_stations() -> Vec<DistrictEntry> {
    let path = "config/districts.json";
    let mut file = match std::fs::File::open(path) {
        Ok(f)  => f,
        Err(_) => return Vec::new(),
    };
    let mut buf = String::new();
    file.read_to_string(&mut buf).ok();
    serde_json::from_str(&buf).unwrap_or_default()
}

/// Returns minimum travel time in minutes (haversine at 30 km/h) from (lat, lon)
/// to the nearest district station. Returns `f32::MAX` if there are no stations.
pub fn min_travel_min(lat: f64, lon: f64, stations: &[DistrictEntry]) -> f32 {
    const SPEED_M_PER_MIN: f64 = 500.0; // 30 km/h = 500 m/min
    stations
        .iter()
        .map(|s| (haversine_m(lat, lon, s.station_lat, s.station_lon) / SPEED_M_PER_MIN) as f32)
        .fold(f32::MAX, f32::min)
}

fn haversine_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const R: f64 = 6_371_000.0;
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon / 2.0).sin().powi(2);
    2.0 * R * a.sqrt().atan2((1.0 - a).sqrt())
}

/// Load district unit allocations from city.toml for the what-if editor.
/// Returns (district_id, name, unit_count) tuples.
pub fn load_district_allocations() -> Vec<(u32, String, u32)> {
    let path = "config/city.toml";
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    #[derive(serde::Deserialize)]
    struct Partial {
        districts: Option<Vec<DistrictAlloc>>,
    }
    #[derive(serde::Deserialize)]
    struct DistrictAlloc {
        id: u32,
        name: String,
        unit_count: u32,
    }

    let parsed: Partial = toml::from_str(&raw).unwrap_or(Partial { districts: None });
    parsed
        .districts
        .unwrap_or_default()
        .into_iter()
        .map(|d| (d.id, d.name, d.unit_count))
        .collect()
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
