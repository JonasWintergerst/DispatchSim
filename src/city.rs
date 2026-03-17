use crate::hex::{Hex, HexCoord};
use crate::routing::TravelMatrix;
use crate::station::Station;
use crate::types::{DistrictId, IncidentKind, NodeId, SimType, SpawnProfileId, StationId, UnitId};
use crate::district::District;
use crate::event_log::{EventLog, Event};
use crate::clock::{SimClock, SimTime, TimeContext};
use crate::spawner::SpawnProfile;
use crate::config::{CityConfig, LoadedConfig, SpawnProfileConfig};
use crate::unit::Unit;

use std::collections::HashMap;
use rayon::iter::IntoParallelRefMutIterator;
use rayon::iter::ParallelIterator;

pub struct City {
    pub clock: SimClock,
    pub districts: Vec<District>,
    coordinator: CityCoordinator,
    event_log: EventLog,
    profiles: HashMap<SpawnProfileId, SpawnProfile>,
    sim_type: SimType,
}

pub struct CityCoordinator;
impl CityCoordinator {
    fn new() -> Self {
        Self
    }
}

impl City {
    pub fn tick(&mut self) {
        //println!("{}", self.clock.elapsed_min);

        let profiles = &self.profiles;

        let time_context = TimeContext {
            hour: self.clock.hour_of_day(),
            day: self.clock.day_of_week(),
            season: self.clock.season(),
            current_time: SimTime { 0 : self.clock.elapsed_min },
        };
    
        let events: Vec<Event> = self.districts
            .par_iter_mut()
            .flat_map(|d| d.tick(time_context, profiles))  
            .collect();

        self.event_log.insert_batch(&events);
        self.clock.tick();
    }
}


impl City {
    /// Build a fully-initialised City from a loaded and validated config.
    ///
    /// This is the single place that translates raw config data into live sim
    /// structs. After this call the sim is ready to tick.
    pub fn from_config(cfg: &LoadedConfig) -> Self {
        let sim_type = cfg.city.sim.sim_type.clone();
 
        // --- 1. Convert spawn profiles ----------------------------------
        // Keyed by the string id used in hexes.json ("residential", etc.)
        let profiles: HashMap<SpawnProfileId, SpawnProfile> = cfg
            .city
            .spawn_profiles
            .iter()
            .map(|(id_str, profile_cfg)| {
                let id = SpawnProfileId::new(id_str.clone());
                let profile = build_spawn_profile(profile_cfg);
                (id, profile)
            })
            .collect();
 
        // --- 2. Group hexes by district_id ------------------------------
        let mut hexes_by_district: HashMap<u32, Vec<&crate::config::HexConfig>> = HashMap::new();
        for hex_cfg in &cfg.hex_grid.hexes {
            hexes_by_district
                .entry(hex_cfg.district_id)
                .or_default()
                .push(hex_cfg);
        }
 
        // --- 3. Build districts -----------------------------------------
        // Unit IDs are assigned globally so they are unique across the city.
        let mut next_unit_id: u32 = 0;
 
        let districts: Vec<District> = cfg
            .city
            .districts
            .iter()
            .map(|district_cfg| {
                let district_id = DistrictId::new(district_cfg.id);
 
                // Build hexes for this district.
                let hexes: Vec<Hex> = hexes_by_district
                    .get(&district_cfg.id)
                    .map(|hs| {
                        hs.iter()
                            .map(|h| {
                                let coord = HexCoord::new(h.col, h.row);
                                let node_id = NodeId::from_hex(&coord);
                                Hex::new(
                                    coord,
                                    district_id.clone(),
                                    SpawnProfileId::new(h.spawn_profile_id.clone()),
                                    node_id,
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
 
                // Station home node: use the first hex's node as the station
                // position. For Phase 1 this is fine — Phase 2 can pick the
                // central node of the district instead.
                let station_node = hexes
                    .first()
                    .map(|h| h.node_id())
                    .unwrap_or_else(|| NodeId::new(0));
 
                // Build units for this station.
                let station_id = StationId::new(district_cfg.station.id);
                let units: Vec<Unit> = (0..district_cfg.station.unit_count)
                    .map(|_| {
                        let uid = UnitId::new(next_unit_id);
                        next_unit_id += 1;
                        Unit::new(uid, sim_type.clone(), station_node.clone())
                    })
                    .collect();
 
                let unit_ids: Vec<UnitId> = units.iter().map(|u| u.id.clone()).collect();
 
                let station = Station::new(
                    station_id,
                    district_cfg.station.name.clone(),
                    sim_type.clone(),
                    station_node,
                    unit_ids,
                );
 
                District::new(district_id, station, units, hexes)
            })
            .collect();

        // --- 4. Build travel matrix -------------------------------------
        // Derived from hex grid geometry — no config file needed.
        // (Stored on City for now; districts borrow it during dispatch.)
        let _travel_matrix = TravelMatrix::from_districts(&districts);

        let event_path = "./output/dispatch_sim.db".to_string();
 
        City {
            clock: SimClock::new(),
            districts,
            coordinator: CityCoordinator::new(),
            event_log: EventLog::open(&event_path).expect("DataBaseError"),
            profiles,
            sim_type,
        }
    }
}
 
// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------
 
fn build_spawn_profile(cfg: &SpawnProfileConfig) -> SpawnProfile {
    let incident_weights: Vec<(IncidentKind, f64)> = cfg
        .incident_weights
        .iter()
        .map(|w| (parse_incident_kind(&w.kind), w.weight))
        .collect();
 
    SpawnProfile::new(
        cfg.base_lambda,
        cfg.hour_multiplier,
        cfg.weekday_multiplier,
        cfg.season_multiplier,
        incident_weights,
    )
}
 
fn parse_incident_kind(s: &str) -> IncidentKind {
    match s {
        "Fire" => IncidentKind::Fire,
        "MedicalEmergency" => IncidentKind::MedicalEmergency,
        "Crime" => IncidentKind::Crime,
        "Accident" => IncidentKind::Accident,
        other => panic!(
            "unknown incident kind '{}' in spawn profile — check city.toml",
            other
        ),
    }
}