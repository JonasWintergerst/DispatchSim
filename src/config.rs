// config.rs
// Loads city.toml (sim params, stations, spawn profiles)
// and hexes.json (H3 hex grid — a flat JSON array written by the optimizer).
// Everything here is plain data — no sim logic.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::types::SimType;

// ---------------------------------------------------------------------------
// Top-level entry point
// ---------------------------------------------------------------------------

/// Everything the sim needs before the first tick.
pub struct LoadedConfig {
    pub city: CityConfig,
    pub hex_grid: HexGridConfig,
}

impl LoadedConfig {
    /// Load city.toml and the hex JSON path referenced inside it.
    pub fn load(city_toml_path: &Path) -> Result<Self, ConfigError> {
        let city = CityConfig::load(city_toml_path)?;
        let hex_path = Path::new(&city.hex_grid_path);
        let hex_grid = HexGridConfig::load(hex_path)?;

        validate(&city, &hex_grid)?;

        Ok(Self { city, hex_grid })
    }
}

// ---------------------------------------------------------------------------
// city.toml structs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CityConfig {
    pub sim: SimConfig,

    /// Path to hexes.json, relative to the working directory.
    pub hex_grid_path: String,

    /// One entry per district — must match district_id values in hexes.json.
    pub districts: Vec<DistrictConfig>,

    /// Spawn profiles referenced by spawn_profile_id in hexes.json.
    /// Key is the profile id string (e.g. "residential", "commercial").
    pub spawn_profiles: HashMap<String, SpawnProfileConfig>,
}

impl CityConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let raw = fs::read_to_string(path)
            .map_err(|e| ConfigError::Io(path.display().to_string(), e))?;
        toml::from_str(&raw).map_err(ConfigError::Toml)
    }
}

#[derive(Debug, Deserialize)]
pub struct SimConfig {
    pub tick_minutes: u32,
    pub duration_minutes: u64,
    pub rng_seed: u64,
    pub sim_type: SimType,
    /// Path to an OSM PBF file for real road routing. If absent, falls back
    /// to synthetic H3-adjacency routing.
    pub osm_path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DistrictConfig {
    pub id: u32,
    pub name: String,
    pub station: StationConfig,
}

#[derive(Debug, Deserialize)]
pub struct StationConfig {
    pub id: u32,
    pub name: String,
    pub unit_count: u32,
}

#[derive(Debug, Deserialize)]
pub struct SpawnProfileConfig {
    pub base_lambda: f64,
    pub hour_multiplier: [f64; 24],
    pub weekday_multiplier: [f64; 7],
    pub season_multiplier: [f64; 4],
    pub incident_weights: Vec<IncidentWeightConfig>,
}

#[derive(Debug, Deserialize)]
pub struct IncidentWeightConfig {
    pub kind: String,
    pub weight: f64,
}

// ---------------------------------------------------------------------------
// hexes.json structs — flat JSON array (written by the optimizer)
// ---------------------------------------------------------------------------

/// The full hex grid as loaded from hexes.json.
pub struct HexGridConfig {
    pub hexes: Vec<HexConfig>,
}

impl HexGridConfig {
    fn load(path: &Path) -> Result<Self, ConfigError> {
        let raw = fs::read_to_string(path)
            .map_err(|e| ConfigError::Io(path.display().to_string(), e))?;
        let hexes: Vec<HexConfig> =
            serde_json::from_str(&raw).map_err(ConfigError::Json)?;
        Ok(Self { hexes })
    }
}

/// One entry in hexes.json, produced by the optimizer.
#[derive(Debug, Deserialize)]
pub struct HexConfig {
    /// Raw H3 cell index encoded as a u64 integer.
    pub h3_index: u64,
    /// Geographic centre of the cell (degrees).
    pub lat: f64,
    pub lon: f64,
    pub district_id: u32,
    pub spawn_profile_id: String,
    /// Nearest OSM road node id (from `osm.rs` sequential numbering).
    /// If None the simulator will snap the hex to the nearest node at startup.
    pub nearest_osm_node: Option<u32>,
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn validate(city: &CityConfig, grid: &HexGridConfig) -> Result<(), ConfigError> {
    let city_district_ids: std::collections::HashSet<u32> =
        city.districts.iter().map(|d| d.id).collect();

    for hex in &grid.hexes {
        if !city_district_ids.contains(&hex.district_id) {
            return Err(ConfigError::Validation(format!(
                "hex h3_index={} references district_id {} which is not in city.toml",
                hex.h3_index, hex.district_id
            )));
        }
        if !city.spawn_profiles.contains_key(&hex.spawn_profile_id) {
            return Err(ConfigError::Validation(format!(
                "hex h3_index={} references spawn_profile_id '{}' which is not in city.toml",
                hex.h3_index, hex.spawn_profile_id
            )));
        }
    }

    let hex_district_ids: std::collections::HashSet<u32> =
        grid.hexes.iter().map(|h| h.district_id).collect();

    for district in &city.districts {
        if !hex_district_ids.contains(&district.id) {
            return Err(ConfigError::Validation(format!(
                "district '{}' (id {}) has no hexes in hexes.json — run the optimizer first",
                district.name, district.id
            )));
        }
    }

    let mut station_ids = std::collections::HashSet::new();
    for district in &city.districts {
        let sid = district.station.id;
        if !station_ids.insert(sid) {
            return Err(ConfigError::Validation(format!(
                "duplicate station id {} in city.toml",
                sid
            )));
        }
        if district.station.unit_count == 0 {
            return Err(ConfigError::Validation(format!(
                "station '{}' (id {}) has unit_count = 0",
                district.station.name, district.station.id
            )));
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum ConfigError {
    Io(String, std::io::Error),
    Toml(toml::de::Error),
    Json(serde_json::Error),
    Validation(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io(path, e) => write!(f, "could not read '{}': {}", path, e),
            ConfigError::Toml(e) => write!(f, "TOML parse error: {}", e),
            ConfigError::Json(e) => write!(f, "JSON parse error: {}", e),
            ConfigError::Validation(msg) => write!(f, "config validation error: {}", msg),
        }
    }
}

impl std::error::Error for ConfigError {}
