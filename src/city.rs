use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::path::Path;
use std::sync::Arc;

use h3o::{CellIndex, LatLng, Resolution};
use rand::SeedableRng;
use rand::rngs::SmallRng;
use rand::RngExt;

use crate::clock::{SimClock, SimTime};
use crate::config::{LoadedConfig, SpawnProfileConfig};
use crate::district::District;
use crate::event_log::{Event, EventLog, RouteRecord};
use crate::event_queue::SimEvent;
use crate::hex::Hex;
use crate::patrol::{self, PatrolRouteSet};
use crate::routing::RoutingEngine;
use crate::routing_cache::{self, LoadedRoutingCache};
use crate::spawner::SpawnProfile;
use crate::station::Station;
use crate::types::{
    BorderNode, DistrictId, EscalationConfig, HexId, IncidentKind, MutualAidRequest, NodeId,
    ServiceTimeConfig, SimType, SpawnProfileId, StationId, UnitId, UnitStatus,
};
use crate::unit::Unit;

/// Flush buffered log events to SQLite every this many simulated minutes (1 sim-day).
const FLUSH_EVERY_MINS: u64 = 1_440;


pub struct City {
    pub clock:      SimClock,
    pub districts:  Vec<District>,
    pub event_heap: BinaryHeap<Reverse<SimEvent>>,
    event_log:      EventLog,
    event_buffer:   Vec<Event>,
    route_buffer:   Vec<RouteRecord>,
    next_flush:     u64,
    profiles:       HashMap<SpawnProfileId, SpawnProfile>,
    sim_type:       SimType,
    /// Cap on travel time (minutes) for cross-district lending. Lender candidates
    /// further than this from the requesting incident are skipped.
    mutual_aid_max_min:  u32,
    /// Master switch for the mutual-aid pass. Disabled → behaves exactly like
    /// the pre-Phase-2 simulator.
    mutual_aid_enabled:  bool,
    /// Queue escalation config, stored centrally and passed to districts.
    pub escalation_cfg:  EscalationConfig,
    /// On-scene service-time distribution per priority, passed to districts.
    pub service_time_cfg: ServiceTimeConfig,
}

impl City {
    pub fn tick(&mut self) {
        let next_time = match self.event_heap.peek() {
            Some(Reverse(ev)) => match ev.time() {
                Some(t) => t,
                None    => { self.event_heap.pop(); return; }
            },
            None => return,
        };

        let mut district_batches: HashMap<DistrictId, Vec<SimEvent>> = HashMap::new();
        while let Some(Reverse(ev)) = self.event_heap.peek() {
            if ev.time() != Some(next_time) { break; }
            let ev = self.event_heap.pop().unwrap().0;
            if let Some(did) = ev.district_id() {
                district_batches.entry(did).or_default().push(ev);
            }
        }

        let profiles = &self.profiles;

        // Tick is strictly sequential: districts share no mutable state, so
        // parallelising across them is possible, but for a typical tick only
        // 1–2 districts have co-scheduled events and Rayon's fork/join overhead
        // dominates. Throughput comes from running whole sims in parallel
        // (see `run_whatif` in main.rs), not from parallelising inside one sim.
        let mut follow_on: Vec<(SimEvent, Event, Option<RouteRecord>)> = Vec::new();
        let mut all_aid: Vec<MutualAidRequest> = Vec::new();
        for d in self.districts.iter_mut() {
            if !district_batches.contains_key(&d.id) { continue; }
            let batch = district_batches.get(&d.id).map(Vec::as_slice).unwrap_or(&[]);
            let mut out = d.process_events(batch, profiles, &self.escalation_cfg, &self.service_time_cfg);
            follow_on.append(&mut out.events);
            all_aid.append(&mut out.aid_requests);
        }

        // Mutual-aid pass — for every incident this tick that ended up in
        // pending_queue, look for an idle/patrolling unit in a neighbour
        // district and loan it. Skipped entirely when disabled.
        if self.mutual_aid_enabled && !all_aid.is_empty() {
            self.run_mutual_aid_pass(next_time, &all_aid, &mut follow_on);
        }

        for (sim_ev, log_ev, route) in follow_on {
            if !matches!(sim_ev, SimEvent::NoOp) {
                self.event_heap.push(Reverse(sim_ev));
            }
            self.event_buffer.push(log_ev);
            if let Some(r) = route {
                self.route_buffer.push(r);
            }
        }

        self.clock.elapsed_min = next_time.0;

        if self.clock.elapsed_min >= self.next_flush {
            self.flush();
            self.next_flush = self.clock.elapsed_min + FLUSH_EVERY_MINS;
        }
    }

    pub fn flush(&mut self) {
        if !self.event_buffer.is_empty() {
            self.event_log
                .insert_batch(&self.event_buffer)
                .expect("event log write failed");
            self.event_buffer.clear();
        }
        if !self.route_buffer.is_empty() {
            self.event_log
                .insert_routes_batch(&self.route_buffer)
                .expect("route log write failed");
            self.route_buffer.clear();
        }
    }

    /// Build a City with all districts, units, and routing from the loaded config.
    /// `db_path` controls where the SQLite event log is written.
    ///
    /// Loads the routing cache from `cfg.city.sim.routing_cache_path`; the
    /// optimizer is the sole producer of this file. Panics with a clear message
    /// if the path is not configured or the file is missing.
    pub fn from_config_with_db(cfg: &LoadedConfig, db_path: &str) -> Self {
        let cache_path = cfg.city.sim.routing_cache_path.as_deref().unwrap_or_else(|| {
            panic!(
                "sim.routing_cache_path is not set in city.toml — add e.g. \
                 `routing_cache_path = \"output/routing_cache.bin\"` and run \
                 `cargo run --bin optimize` to produce it"
            );
        });
        let cached = Arc::new(routing_cache::load(cache_path).unwrap_or_else(|| {
            panic!(
                "routing cache not found at {cache_path} — run `cargo run --bin optimize` first"
            );
        }));

        let engine = Arc::new(build_routing_engine(&cached));
        Self::build_from_cache(cfg, db_path, cached, engine)
    }

    /// Build a City using an already-loaded routing cache. Single-sim path
    /// for callers that already loaded the cache themselves.
    pub fn from_config_with_routing(
        cfg:    &LoadedConfig,
        db_path: &str,
        cached:  Arc<LoadedRoutingCache>,
    ) -> Self {
        let engine = Arc::new(build_routing_engine(&cached));
        Self::build_from_cache(cfg, db_path, cached, engine)
    }

    /// Canonical builder used by `SimBatch`. Takes a pre-built city-wide
    /// `RoutingEngine` so that batches of variants can share it across
    /// runs and skip the expensive `from_snapshot` work.
    pub fn from_config_with_engine(
        cfg:     &LoadedConfig,
        db_path: &str,
        cached:  Arc<LoadedRoutingCache>,
        engine:  Arc<RoutingEngine>,
    ) -> Self {
        Self::build_from_cache(cfg, db_path, cached, engine)
    }

    fn build_from_cache(
        cfg:     &LoadedConfig,
        db_path: &str,
        cached:  Arc<LoadedRoutingCache>,
        engine:  Arc<RoutingEngine>,
    ) -> Self {
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

        // 4. Build districts
        // Phase A: assign ID ranges and RNG seeds sequentially (preserves determinism).
        struct DistrictSetup { hex_id_start: u32, unit_id_start: u32, rng_seed: u64 }
        let mut next_hex_id:  u32 = 0;
        let mut next_unit_id: u32 = 0;
        let mut seed_rng = SmallRng::seed_from_u64(cfg.city.sim.rng_seed);

        let setups: Vec<DistrictSetup> = cfg.city.districts.iter().map(|d| {
            let hex_count = hexes_by_district.get(&d.id).map(|v| v.len() as u32).unwrap_or(0);
            let s = DistrictSetup {
                hex_id_start:  next_hex_id,
                unit_id_start: next_unit_id,
                rng_seed:      seed_rng.random(),
            };
            next_hex_id  += hex_count;
            next_unit_id += d.unit_count;
            s
        }).collect();

        // Phase B: build each district in parallel (cache lookups are independent,
        // and each Arc::clone is cheap). The expensive Dijkstra work already
        // happened in the optimizer; this loop just restores snapshots.
        let station_lookup = cfg.district_stations.by_district_id();

        // Map every road node in the city-wide graph to its H3 cell so that
        // each hex can hold all road nodes inside it (not just the anchor).
        // Incidents spawn at a random node for spatial realism.
        let nodes_by_h3: HashMap<u64, Vec<NodeId>> = {
            let all_hexes: Vec<&crate::config::HexConfig> = cfg.hex_grid.hexes.iter().collect();
            let resolution = all_hexes.first()
                .and_then(|h| CellIndex::try_from(h.h3_index).ok())
                .map(|c| c.resolution())
                .unwrap_or(Resolution::Nine);

            let mut map: HashMap<u64, Vec<NodeId>> = HashMap::new();
            for (node_id, lon, lat) in engine.all_node_positions() {
                if let Ok(ll) = LatLng::from_radians(lat.to_radians(), lon.to_radians()) {
                    let cell = ll.to_cell(resolution);
                    map.entry(u64::from(cell)).or_default().push(node_id);
                }
            }
            map
        };

        // Sequential build: this is one-shot deserialization (the expensive
        // Dijkstra work already happened in the optimizer), and the outer
        // whatif runners parallelise across whole sims — nesting rayon here
        // would just add scheduler contention. See CLAUDE.md "Key Design
        // Decisions": sims are the unit of parallelism.
        let mut districts: Vec<District> = cfg.city.districts
            .iter()
            .zip(setups.iter())
            .map(|(district_cfg, setup)| {
                let district_id      = DistrictId::new(district_cfg.id);
                let mut hex_cursor   = setup.hex_id_start;
                let mut unit_cursor  = setup.unit_id_start;

                // Build Hex objects from config. The anchor node (road_nodes[0])
                // comes from the routing cache (which the optimizer wrote).
                let hexes: Vec<Hex> = hexes_by_district
                    .get(&district_cfg.id)
                    .map(|hs| hs.iter().map(|h| {
                        let hex_id = HexId::new(hex_cursor);
                        hex_cursor += 1;
                        let anchor = cached.hex_nodes
                            .get(&district_cfg.id)
                            .and_then(|m| m.get(&h.h3_index))
                            .map(|&id| NodeId::new(id))
                            .or_else(|| h.nearest_osm_node.map(NodeId::new))
                            .expect("hex has no road node — rerun optimizer");
                        // Collect all road nodes in this H3 cell, anchor first.
                        let mut road_nodes = vec![anchor];
                        if let Some(cell_nodes) = nodes_by_h3.get(&h.h3_index) {
                            for &nid in cell_nodes {
                                if nid != anchor {
                                    road_nodes.push(nid);
                                }
                            }
                        }
                        Hex {
                            id:                hex_id,
                            h3_index:          h.h3_index,
                            lat:               h.lat,
                            lon:               h.lon,
                            district:          district_id,
                            spawn_profile_id:  SpawnProfileId::new(h.spawn_profile_id.clone()),
                            road_nodes,
                        }
                    }).collect())
                    .unwrap_or_default();

                let routing = Arc::clone(&engine);

                // Station location from districts.json (real OSM-snapped station node).
                let district_station = station_lookup
                    .get(&district_cfg.id)
                    .expect("no station entry in districts.json — run the optimizer first");
                let station_node = NodeId::new(district_station.station_osm_node);
                let station_id   = StationId::new(district_cfg.id);

                let units: Vec<Unit> = (0..district_cfg.unit_count).map(|_| {
                    let uid = UnitId::new(unit_cursor);
                    unit_cursor += 1;
                    Unit::new(uid, sim_type, station_node)
                }).collect();

                let unit_ids: Vec<UnitId> = units.iter().map(|u| u.id).collect();
                let station = Station::new(
                    station_id,
                    district_station.station_name.clone(),
                    sim_type,
                    station_node,
                    unit_ids,
                );

                let district_rng = SmallRng::seed_from_u64(setup.rng_seed);
                District::new(district_id, station, units, hexes, district_rng, routing, cfg.city.sim.record_routes)
            })
            .collect();

        detect_border_nodes(&mut districts);

        // Load patrol routes (if any) and assign them to the configured number
        // of patrol units per district. Failures to load are non-fatal — we
        // just log a warning and run without patrols, which preserves the
        // pre-Phase-2 behaviour.
        let patrol_assignments = load_patrol_assignments(cfg, &districts);
        for d in districts.iter_mut() {
            if let Some((routes, n_patrol_units)) = patrol_assignments.get(&d.id).cloned() {
                d.patrol_routes = routes.clone();
                if !routes.is_empty() {
                    let n = (n_patrol_units as usize).min(d.units.len());
                    for (i, u) in d.units.iter_mut().enumerate().take(n) {
                        u.patrol_route = Some(routes[i % routes.len()].clone());
                    }
                }
            }
        }

        let escalation_cfg = EscalationConfig {
            enabled:                  cfg.city.sim.queue_escalation_enabled.unwrap_or(false),
            interval_min:             cfg.city.sim.queue_escalation_interval_min.unwrap_or(5),
            c_to_b_min:               cfg.city.sim.escalation_c_to_b_min.unwrap_or(30),
            b_to_a_min:               cfg.city.sim.escalation_b_to_a_min.unwrap_or(15),
            cancellation_threshold:   cfg.city.sim.cancellation_threshold_min.unwrap_or(60),
            cancellation_probability: cfg.city.sim.cancellation_probability.unwrap_or(0.15),
        };

        let service_time_cfg = cfg.city.service_time
            .clone()
            .map(|raw| raw.into_config())
            .unwrap_or_else(ServiceTimeConfig::default_uniform);

        let mut event_heap = seed_events(&districts, &profiles, &mut seed_rng);

        // Seed queue-escalation sweep events if enabled.
        if escalation_cfg.enabled {
            for district in &districts {
                event_heap.push(Reverse(SimEvent::QueueEscalation {
                    time:        SimTime(escalation_cfg.interval_min),
                    district_id: district.id,
                }));
            }
        }

        if let Some(parent) = Path::new(db_path).parent() {
            std::fs::create_dir_all(parent).expect("could not create output directory");
        }

        City {
            clock:         SimClock::new(),
            districts,
            event_heap,
            event_log:     EventLog::open(db_path)
                               .expect("could not open event log"),
            event_buffer:  Vec::new(),
            route_buffer:  Vec::new(),
            next_flush:    FLUSH_EVERY_MINS,
            profiles,
            sim_type,
            mutual_aid_max_min: cfg.city.sim.mutual_aid_max_min.unwrap_or(8),
            mutual_aid_enabled: cfg.city.sim.mutual_aid_enabled.unwrap_or(false),
            escalation_cfg,
            service_time_cfg,
        }
    }

    /// Convenience wrapper using the default output path.
    pub fn from_config(cfg: &LoadedConfig) -> Self {
        Self::from_config_with_db(cfg, "./output/dispatch_sim.db")
    }

    // ─────────────────────────────────────────────────────────────────────
    // Mutual-aid pass
    // ─────────────────────────────────────────────────────────────────────

    /// For each pending aid request, find a neighbour district with an
    /// available unit close enough to the requesting incident, and loan it.
    /// Modifies `follow_on` directly with any dispatch events produced by the
    /// lender district.
    fn run_mutual_aid_pass(
        &mut self,
        time:      SimTime,
        requests:  &[MutualAidRequest],
        follow_on: &mut Vec<(SimEvent, Event, Option<RouteRecord>)>,
    ) {
        for req in requests {
            // Find the requesting district's neighbour set.
            let req_idx = match self.districts.iter().position(|d| d.id == req.requesting_district) {
                Some(i) => i,
                None    => continue,
            };
            let neighbour_ids: Vec<DistrictId> = {
                let mut seen = std::collections::HashSet::new();
                self.districts[req_idx].border_nodes.iter()
                    .filter_map(|b| if seen.insert(b.neighbour_district) { Some(b.neighbour_district) } else { None })
                    .collect()
            };
            if neighbour_ids.is_empty() { continue; }

            // Pick the closest lender (by travel_time of any free unit).
            let mut best: Option<(usize, u32)> = None; // (district_index, travel_time)
            for nbr_id in &neighbour_ids {
                let Some(idx) = self.districts.iter().position(|d| d.id == *nbr_id) else { continue; };
                let tt = self.districts[idx].units.iter()
                    .filter(|u| u.status == UnitStatus::Idle || u.status == UnitStatus::Patrolling)
                    .map(|u| self.districts[idx].routing.travel_time(u.current_position(time), req.location))
                    .min();
                if let Some(t) = tt {
                    if t > self.mutual_aid_max_min { continue; }
                    if best.is_none_or(|(_, bt)| t < bt) {
                        best = Some((idx, t));
                    }
                }
            }
            let Some((lender_idx, _)) = best else { continue; };

            // Try to dispatch from the lender. If it succeeds, mark the
            // requester's incident as Assigned so its own pop_best_pending
            // skips it on subsequent ticks.
            let success = self.districts[lender_idx]
                .try_accept_loan(time, req.requesting_district, req, follow_on);
            if success {
                self.districts[req_idx].mark_loaned_out(&req.incident_id);
            }
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
        other              => panic!(
            "unknown incident kind '{}' in spawn profile — check city.toml",
            other
        ),
    }
}

// ---------------------------------------------------------------------------
// Extracted from_config helpers
// ---------------------------------------------------------------------------

/// Detect border nodes using H3 cell adjacency (grid_disk(1)).
fn detect_border_nodes(districts: &mut [District]) {
    let mut cell_to_district: HashMap<u64, DistrictId> = HashMap::new();
    for d in districts.iter() {
        for h in &d.hexes {
            cell_to_district.insert(h.h3_index, d.id);
        }
    }

    for d in districts.iter_mut() {
        let mut borders: Vec<BorderNode> = Vec::new();
        for h in &d.hexes {
            if let Ok(cell) = CellIndex::try_from(h.h3_index) {
                let disk: Vec<CellIndex> = cell.grid_disk::<Vec<_>>(1);
                for nbr in disk {
                    let nbr_u64 = u64::from(nbr);
                    if let Some(&fid) = cell_to_district.get(&nbr_u64) {
                        if fid != d.id {
                            borders.push(BorderNode {
                                node_id:            h.node_id(),
                                neighbour_district: fid,
                            });
                        }
                    }
                }
            }
        }
        borders.dedup_by_key(|b| (b.node_id, b.neighbour_district));
        d.border_nodes = borders;
    }
}

/// Seed the initial event heap with ShiftChange, IncidentSpawn, and (for any
/// units that have a patrol route assigned) initial PatrolLoop events.
fn seed_events(
    districts: &[District],
    profiles: &HashMap<SpawnProfileId, SpawnProfile>,
    rng: &mut SmallRng,
) -> BinaryHeap<Reverse<SimEvent>> {
    let mut heap: BinaryHeap<Reverse<SimEvent>> = BinaryHeap::new();

    for district in districts {
        heap.push(Reverse(SimEvent::ShiftChange {
            time:        SimTime(0),
            district_id: district.id,
        }));

        for ev in district.initial_patrol_events() {
            heap.push(Reverse(ev));
        }

        for hex in &district.hexes {
            let first_time = crate::spawner::next_spawn_time(
                SimTime(0),
                &hex.spawn_profile_id,
                profiles,
                rng,
            );
            heap.push(Reverse(SimEvent::IncidentSpawn {
                time:        first_time,
                hex_id:      hex.id,
                district_id: district.id,
            }));
        }
    }

    heap
}

// ---------------------------------------------------------------------------
// Routing engine pre-build
// ---------------------------------------------------------------------------

/// Build a single city-wide `RoutingEngine` from the loaded routing cache.
/// The result is intended to be wrapped in an `Arc` and shared across all
/// districts and every variant in a `SimBatch`.
pub fn build_routing_engine(
    cached: &LoadedRoutingCache,
) -> RoutingEngine {
    RoutingEngine::from_snapshot(cached.city_snapshot.clone())
}

// ---------------------------------------------------------------------------
// Patrol route loading
// ---------------------------------------------------------------------------

/// Load patrol routes from the path configured in `cfg.city.patrol` and pair
/// them with each district's `patrol_units` count. Returns a map from
/// `DistrictId` to `(routes, patrol_units)`. Errors are logged and produce
/// an empty map (no patrols).
fn load_patrol_assignments(
    cfg:       &LoadedConfig,
    districts: &[District],
) -> HashMap<DistrictId, (Vec<std::sync::Arc<crate::patrol::PatrolRoute>>, u32)> {
    let mut result: HashMap<DistrictId, (Vec<std::sync::Arc<crate::patrol::PatrolRoute>>, u32)> = HashMap::new();

    let routes_path = match cfg.city.patrol.as_ref().and_then(|p| p.routes_path.as_ref()) {
        Some(p) => p,
        None    => return result, // no patrol config — empty assignments
    };

    let routings: HashMap<DistrictId, std::sync::Arc<RoutingEngine>> = districts
        .iter()
        .map(|d| (d.id, d.routing.clone()))
        .collect();

    let routes: PatrolRouteSet = match patrol::load_routes(std::path::Path::new(routes_path), &routings) {
        Ok(r)  => r,
        Err(e) => {
            eprintln!("warning: could not load patrol routes from '{}': {}", routes_path, e);
            return result;
        }
    };

    // Per-district patrol_units count, looked up from city.toml.
    let patrol_unit_counts: HashMap<u32, u32> = cfg.city.districts.iter()
        .map(|d| (d.id, d.patrol_units.unwrap_or(0)))
        .collect();

    for (did, district_routes) in routes {
        let n = patrol_unit_counts.get(&did.value()).copied().unwrap_or(0);
        result.insert(did, (district_routes, n));
    }

    result
}

