use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use rand::SeedableRng;
use rand::rngs::SmallRng;
use rand::RngExt;
use rayon::iter::{IntoParallelRefMutIterator, ParallelIterator};

use crate::clock::{SimClock, SimTime};
use crate::config::{LoadedConfig, SpawnProfileConfig};
use crate::district::District;
use crate::event_log::{Event, EventLog};
use crate::event_queue::SimEvent;
use crate::hex::{Hex, HexCoord};
use crate::routing::TravelMatrix;
use crate::spawner::SpawnProfile;
use crate::station::Station;
use crate::types::{DistrictId, HexId, IncidentKind, NodeId, SimType, SpawnProfileId, StationId, UnitId};
use crate::unit::Unit;

/// Flush buffered log events to SQLite every this many simulated minutes (1 sim-day).
const FLUSH_EVERY_MINS: u64 = 1_440;

pub struct City {
    pub clock:      SimClock,
    pub districts:  Vec<District>,
    pub event_heap: BinaryHeap<Reverse<SimEvent>>,
    event_log:      EventLog,
    event_buffer:   Vec<Event>,
    next_flush:     u64,
    profiles:       HashMap<SpawnProfileId, SpawnProfile>,
    travel_matrix:  TravelMatrix,
    sim_type:       SimType,
}

impl City {
    pub fn tick(&mut self) {
        let next_time = match self.event_heap.peek() {
            Some(Reverse(ev)) => match ev.time() {
                Some(t) => t,
                None    => { self.event_heap.pop(); return; } // stray NoOp
            },
            None => return,
        };

        // Drain all events scheduled for this tick and group by district.
        let mut district_batches: HashMap<DistrictId, Vec<SimEvent>> = HashMap::new();
        while let Some(Reverse(ev)) = self.event_heap.peek() {
            if ev.time() != Some(next_time) { break; }
            let ev = self.event_heap.pop().unwrap().0;
            if let Some(did) = ev.district_id() {
                district_batches.entry(did).or_default().push(ev);
            }
            // NoOp has no district — discard it
        }

        let profiles      = &self.profiles;
        let travel_matrix = &self.travel_matrix;

        let follow_on: Vec<(SimEvent, Event)> = self.districts
            .par_iter_mut()
            .flat_map(|d| {
                let batch = district_batches.get(&d.id).map(Vec::as_slice).unwrap_or(&[]);
                d.process_events(batch, profiles, travel_matrix)
            })
            .collect();

        for (sim_ev, log_ev) in follow_on {
            if !matches!(sim_ev, SimEvent::NoOp) {
                self.event_heap.push(Reverse(sim_ev));
            }
            self.event_buffer.push(log_ev);
        }

        self.clock.elapsed_min = next_time.0;

        if self.clock.elapsed_min >= self.next_flush {
            self.flush();
            self.next_flush = self.clock.elapsed_min + FLUSH_EVERY_MINS;
        }
    }

    /// Flush any buffered log events to SQLite. Called automatically every
    /// FLUSH_EVERY_MINS of sim time and once at the end of the run.
    pub fn flush(&mut self) {
        if self.event_buffer.is_empty() { return; }
        self.event_log
            .insert_batch(&self.event_buffer)
            .expect("event log write failed");
        self.event_buffer.clear();
    }

    /// Build a fully-initialised City from a loaded config.
    /// After this call the sim is ready to tick.
    pub fn from_config(cfg: &LoadedConfig) -> Self {
        let sim_type = cfg.city.sim.sim_type;

        // 1. Spawn profiles
        let profiles: HashMap<SpawnProfileId, SpawnProfile> = cfg
            .city
            .spawn_profiles
            .iter()
            .map(|(id_str, profile_cfg)| {
                (SpawnProfileId::new(id_str.clone()), build_spawn_profile(profile_cfg))
            })
            .collect();

        // 2. Group hex configs by district
        let mut hexes_by_district: HashMap<u32, Vec<&crate::config::HexConfig>> = HashMap::new();
        for hex_cfg in &cfg.hex_grid.hexes {
            hexes_by_district.entry(hex_cfg.district_id).or_default().push(hex_cfg);
        }

        // 3. Build districts; unit and hex IDs are globally unique.
        let mut next_unit_id: u32 = 0;
        let mut next_hex_id:  u32 = 0;
        let mut seed_rng = SmallRng::seed_from_u64(cfg.city.sim.rng_seed);

        let districts: Vec<District> = cfg.city.districts.iter().map(|district_cfg| {
            let district_id = DistrictId::new(district_cfg.id);

            let hexes: Vec<Hex> = hexes_by_district
                .get(&district_cfg.id)
                .map(|hs| {
                    hs.iter().map(|h| {
                        let coord   = HexCoord::new(h.col, h.row);
                        let node_id = NodeId::from_hex(&coord);
                        let hex_id  = HexId::new(next_hex_id);
                        next_hex_id += 1;
                        Hex::new(
                            hex_id,
                            coord,
                            district_id,
                            SpawnProfileId::new(h.spawn_profile_id.clone()),
                            node_id,
                        )
                    }).collect()
                })
                .unwrap_or_default();

            // Phase 1: use the first hex's node as the station location.
            let station_node = hexes.first().map(|h| h.node_id()).unwrap_or(NodeId::new(0));
            let station_id   = StationId::new(district_cfg.station.id);

            let units: Vec<Unit> = (0..district_cfg.station.unit_count).map(|_| {
                let uid = UnitId::new(next_unit_id);
                next_unit_id += 1;
                Unit::new(uid, sim_type, station_node)
            }).collect();

            let unit_ids: Vec<UnitId> = units.iter().map(|u| u.id).collect();
            let station = Station::new(
                station_id,
                district_cfg.station.name.clone(),
                sim_type,
                station_node,
                unit_ids,
            );

            let district_rng = SmallRng::seed_from_u64(seed_rng.random());
            District::new(district_id, station, units, hexes, district_rng)
        }).collect();

        // 4. Travel matrix from hex geometry.
        let travel_matrix = TravelMatrix::from_districts(&districts);

        // 5. Seed one IncidentSpawn per hex at time 0.
        //    Each spawn handler will immediately schedule the next one.
        let mut event_heap: BinaryHeap<Reverse<SimEvent>> = BinaryHeap::new();
        for district in &districts {
            for hex in &district.hexes {
                event_heap.push(Reverse(SimEvent::IncidentSpawn {
                    time:        SimTime(0),
                    hex_id:      hex.id,
                    district_id: district.id,
                }));
            }
        }

        std::fs::create_dir_all("./output").expect("could not create output directory");

        City {
            clock:         SimClock::new(),
            districts,
            event_heap,
            event_log:     EventLog::open("./output/dispatch_sim.db").expect("could not open event log"),
            event_buffer:  Vec::new(),
            next_flush:    FLUSH_EVERY_MINS,
            profiles,
            travel_matrix,
            sim_type,
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn build_spawn_profile(cfg: &SpawnProfileConfig) -> SpawnProfile {
    let incident_weights = cfg
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
        "Fire"             => IncidentKind::Fire,
        "MedicalEmergency" => IncidentKind::MedicalEmergency,
        "Crime"            => IncidentKind::Crime,
        "Accident"         => IncidentKind::Accident,
        other              => panic!("unknown incident kind '{}' in spawn profile — check city.toml", other),
    }
}
