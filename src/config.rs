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
#[derive(Clone)]
pub struct LoadedConfig {
    pub city:              CityConfig,
    pub hex_grid:          HexGridConfig,
    pub district_stations: DistrictStationGrid,
}

impl LoadedConfig {
    /// Load city.toml, hexes.json, and districts.json.
    pub fn load(city_toml_path: &Path) -> Result<Self, ConfigError> {
        let city     = CityConfig::load(city_toml_path)?;
        let hex_path = Path::new(&city.hex_grid_path);
        let hex_grid = HexGridConfig::load(hex_path)?;
        let dist_path = Path::new(&city.districts_path);
        let district_stations = DistrictStationGrid::load(dist_path)?;

        validate(&city, &hex_grid, &district_stations)?;

        Ok(Self { city, hex_grid, district_stations })
    }
}

// ---------------------------------------------------------------------------
// city.toml structs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Clone)]
pub struct CityConfig {
    pub sim: SimConfig,

    /// Path to hexes.json, relative to the working directory.
    pub hex_grid_path: String,

    /// Path to districts.json written by the optimizer (district → station mapping).
    pub districts_path: String,

    /// One entry per district — must match district_id values in hexes.json.
    pub districts: Vec<DistrictConfig>,

    /// Spawn profiles referenced by spawn_profile_id in hexes.json.
    /// Key is the profile id string (e.g. "residential", "commercial").
    pub spawn_profiles: HashMap<String, SpawnProfileConfig>,

    /// Optional patrol section. Absent → no patrols, units sit idle at their
    /// stations between calls (the pre-Phase-2 behaviour).
    #[serde(default)]
    pub patrol: Option<PatrolConfig>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PatrolConfig {
    /// Path to a JSON file produced by `patrol_gen`. Loaded at city construction.
    pub routes_path: Option<String>,
}

impl CityConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let raw = fs::read_to_string(path)
            .map_err(|e| ConfigError::Io(path.display().to_string(), e))?;
        toml::from_str(&raw).map_err(ConfigError::Toml)
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct SimConfig {
    pub tick_minutes: u32,
    pub duration_minutes: u64,
    pub rng_seed: u64,
    pub sim_type: SimType,
    /// Path to an OSM PBF file for real road routing. If absent, falls back
    /// to synthetic H3-adjacency routing.
    pub osm_path: Option<String>,
    /// When true, compute and store full A* route paths per dispatch for
    /// heatmap analysis. Disabled by default because A* is expensive.
    #[serde(default)]
    pub record_routes: bool,
    /// Path to a binary routing cache file. When set, the sim writes precomputed
    /// routing data on first run and loads it on subsequent runs, skipping OSM
    /// parsing and Dijkstra precomputation.
    pub routing_cache_path: Option<String>,

    /// Master switch for cross-district mutual aid. When `false` (or absent),
    /// the sim behaves like the pre-Phase-2 simulator: incidents that can't
    /// be served locally just queue up.
    #[serde(default)]
    pub mutual_aid_enabled: Option<bool>,

    /// Maximum travel-time (minutes) at which a neighbour district's unit is
    /// considered as a mutual-aid lender. Defaults to 8 min.
    #[serde(default)]
    pub mutual_aid_max_min: Option<u32>,

    /// Master switch for queue escalation. When enabled, pending incidents
    /// that have waited too long get their priority bumped (C→B, B→A), and
    /// low-priority calls may self-cancel after a longer threshold.
    #[serde(default)]
    pub queue_escalation_enabled: Option<bool>,

    /// How often (sim-minutes) the escalation sweep runs per district.
    #[serde(default)]
    pub queue_escalation_interval_min: Option<u64>,

    /// Minutes a Priority C incident must wait before escalating to B.
    #[serde(default)]
    pub escalation_c_to_b_min: Option<u64>,

    /// Minutes a Priority B incident must wait before escalating to A.
    #[serde(default)]
    pub escalation_b_to_a_min: Option<u64>,

    /// Minutes a Priority C incident must wait before it may self-cancel.
    #[serde(default)]
    pub cancellation_threshold_min: Option<u64>,

    /// Per-check probability [0.0, 1.0] that a C incident past the
    /// cancellation threshold resolves itself.
    #[serde(default)]
    pub cancellation_probability: Option<f64>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct DistrictConfig {
    pub id:         u32,
    pub name:       String,
    pub unit_count: u32,
    /// How many of this district's units should be assigned a patrol route.
    /// Defaults to 0 (all units station-bound).
    #[serde(default)]
    pub patrol_units: Option<u32>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SpawnProfileConfig {
    pub base_lambda: f64,
    pub hour_multiplier: [f64; 24],
    pub weekday_multiplier: [f64; 7],
    pub season_multiplier: [f64; 4],
    pub incident_weights: Vec<IncidentWeightConfig>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct IncidentWeightConfig {
    pub kind: String,
    pub weight: f64,
}

// ---------------------------------------------------------------------------
// hexes.json structs — flat JSON array (written by the optimizer)
// ---------------------------------------------------------------------------

/// The full hex grid as loaded from hexes.json.
#[derive(Clone)]
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
#[derive(Debug, Deserialize, Clone)]
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
// districts.json structs — written by the optimizer, read by the simulator
// ---------------------------------------------------------------------------

/// One entry in districts.json: the station selected for a district.
#[derive(Debug, Deserialize, Clone)]
pub struct DistrictStation {
    pub district_id:      u32,
    pub station_name:     String,
    pub station_lat:      f64,
    pub station_lon:      f64,
    pub station_osm_node: u32,
}

/// The full district-to-station mapping as loaded from districts.json.
#[derive(Clone)]
pub struct DistrictStationGrid {
    pub entries: Vec<DistrictStation>,
}

impl DistrictStationGrid {
    fn load(path: &Path) -> Result<Self, ConfigError> {
        let raw = fs::read_to_string(path)
            .map_err(|e| ConfigError::Io(path.display().to_string(), e))?;
        let entries: Vec<DistrictStation> =
            serde_json::from_str(&raw).map_err(ConfigError::Json)?;
        Ok(Self { entries })
    }

    /// Build a HashMap for O(1) lookup by district_id.
    pub fn by_district_id(&self) -> std::collections::HashMap<u32, &DistrictStation> {
        self.entries.iter().map(|e| (e.district_id, e)).collect()
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn validate(city: &CityConfig, grid: &HexGridConfig, stations: &DistrictStationGrid) -> Result<(), ConfigError> {
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

    let station_district_ids: std::collections::HashSet<u32> =
        stations.entries.iter().map(|e| e.district_id).collect();

    for district in &city.districts {
        if !hex_district_ids.contains(&district.id) {
            return Err(ConfigError::Validation(format!(
                "district '{}' (id {}) has no hexes in hexes.json — run the optimizer first",
                district.name, district.id
            )));
        }
        if !station_district_ids.contains(&district.id) {
            return Err(ConfigError::Validation(format!(
                "district '{}' (id {}) has no entry in districts.json — run the optimizer first",
                district.name, district.id
            )));
        }
        if district.unit_count == 0 {
            return Err(ConfigError::Validation(format!(
                "district '{}' (id {}) has unit_count = 0",
                district.name, district.id
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
