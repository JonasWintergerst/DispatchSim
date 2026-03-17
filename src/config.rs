// config.rs
// Loads city.toml (sim params, stations, spawn profiles)
// and hexes.json (spatial hex grid).
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

    /// Path to hexes.json, relative to the config file location.
    pub hex_grid_path: String,

    /// One entry per district — must match district_id values in hexes.json.
    pub districts: Vec<DistrictConfig>,

    /// Spawn profiles referenced by spawn_profile_id in hexes.json.
    /// Key is the profile id string (e.g. "residential", "commercial").
    pub spawn_profiles: HashMap<String, SpawnProfileConfig>,
}

impl CityConfig {
    fn load(path: &Path) -> Result<Self, ConfigError> {
        let raw = fs::read_to_string(path)
            .map_err(|e| ConfigError::Io(path.display().to_string(), e))?;
        toml::from_str(&raw).map_err(ConfigError::Toml)
    }
}

#[derive(Debug, Deserialize)]
pub struct SimConfig {
    /// Minutes per tick (typically 1).
    pub tick_minutes: u32,
    /// Total simulated minutes to run (4 years ≈ 2_102_400).
    pub duration_minutes: u64,
    /// RNG seed for reproducibility.
    pub rng_seed: u64,
    /// The service type this entire simulation models: Fire | Police | Medical.
    /// All stations and units inherit this type — there is only one per sim run.
    pub sim_type: SimType,
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
    /// How many units this station starts with.
    pub unit_count: u32,
}

#[derive(Debug, Deserialize)]
pub struct UnitConfig {
    pub id: u32,
    // No unit_type field — all units share the city-wide sim_type.
}

#[derive(Debug, Deserialize)]
pub struct SpawnProfileConfig {
    /// Average incidents per hour at baseline.
    pub base_lambda: f64,
    /// 24 multipliers, index = hour of day.
    pub hour_multiplier: [f64; 24],
    /// 7 multipliers, index = day of week (0 = Monday).
    pub weekday_multiplier: [f64; 7],
    /// 4 multipliers: [Spring, Summer, Autumn, Winter].
    pub season_multiplier: [f64; 4],
    /// Incident type weights: list of [kind_string, weight] pairs.
    pub incident_weights: Vec<IncidentWeightConfig>,
}

#[derive(Debug, Deserialize)]
pub struct IncidentWeightConfig {
    /// "Fire" | "MedicalEmergency" | "Crime" | "Accident"
    pub kind: String,
    pub weight: f64,
}

// ---------------------------------------------------------------------------
// hexes.json structs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct HexGridConfig {
    pub cols: u32,
    pub rows: u32,
    pub districts: Vec<HexDistrictMeta>,
    pub hexes: Vec<HexConfig>,
}

impl HexGridConfig {
    fn load(path: &Path) -> Result<Self, ConfigError> {
        let raw = fs::read_to_string(path)
            .map_err(|e| ConfigError::Io(path.display().to_string(), e))?;
        serde_json::from_str(&raw).map_err(ConfigError::Json)
    }
}

/// Metadata about a district as stored in hexes.json (name only — IDs come
/// from the district_id field on each hex).
#[derive(Debug, Deserialize)]
pub struct HexDistrictMeta {
    pub id: u32,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct HexConfig {
    pub col: i32,
    pub row: i32,
    pub district_id: u32,
    pub spawn_profile_id: String,
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn validate(city: &CityConfig, grid: &HexGridConfig) -> Result<(), ConfigError> {
    // Every district_id in hexes.json must have a matching entry in city.toml.
    let city_district_ids: std::collections::HashSet<u32> =
        city.districts.iter().map(|d| d.id).collect();

    for hex in &grid.hexes {
        if !city_district_ids.contains(&hex.district_id) {
            return Err(ConfigError::Validation(format!(
                "hex ({},{}) references district_id {} which is not in city.toml",
                hex.col, hex.row, hex.district_id
            )));
        }
        if !city.spawn_profiles.contains_key(&hex.spawn_profile_id) {
            return Err(ConfigError::Validation(format!(
                "hex ({},{}) references spawn_profile_id '{}' which is not in city.toml",
                hex.col, hex.row, hex.spawn_profile_id
            )));
        }
    }

    // Every district in city.toml must have at least one hex.
    let hex_district_ids: std::collections::HashSet<u32> =
        grid.hexes.iter().map(|h| h.district_id).collect();

    for district in &city.districts {
        if !hex_district_ids.contains(&district.id) {
            return Err(ConfigError::Validation(format!(
                "district '{}' (id {}) has no hexes in the hex grid",
                district.name, district.id
            )));
        }
    }

    // Station IDs must be unique across the whole city.
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