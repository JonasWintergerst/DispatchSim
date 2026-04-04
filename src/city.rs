use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::path::Path;

use h3o::CellIndex;
use petgraph::graph::NodeIndex;
use rand::SeedableRng;
use rand::rngs::SmallRng;
use rand::RngExt;
use rayon::iter::{IntoParallelIterator, IntoParallelRefMutIterator, ParallelIterator};

use crate::clock::{SimClock, SimTime};
use crate::config::{LoadedConfig, SpawnProfileConfig};
use crate::district::District;
use crate::event_log::{Event, EventLog, RouteRecord};
use crate::event_queue::SimEvent;
use crate::geo_utils::haversine_m;
use crate::hex::Hex;
use crate::osm::OsmGraph;
use crate::routing::{Edge, Node, RoadGraph, RoutingEngine, RoutingSnapshot};
use crate::spawner::SpawnProfile;
use crate::station::Station;
use crate::types::{
    BorderNode, DistrictId, HexId, IncidentKind, NodeId, SimType, SpawnProfileId, StationId,
    UnitId,
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

        // Use par_iter_mut only when multiple districts have concurrent events —
        // for the common case (1 event, 1 district) the Rayon thread overhead
        // dominates over the tiny amount of work.
        let follow_on: Vec<(SimEvent, Event, Option<RouteRecord>)> = if district_batches.len() > 2 {
            self.districts
                .par_iter_mut()
                .flat_map(|d| {
                    let batch = district_batches.get(&d.id).map(Vec::as_slice).unwrap_or(&[]);
                    d.process_events(batch, profiles)
                })
                .collect()
        } else {
            self.districts
                .iter_mut()
                .filter(|d| district_batches.contains_key(&d.id))
                .flat_map(|d| {
                    let batch = district_batches.get(&d.id).map(Vec::as_slice).unwrap_or(&[]);
                    d.process_events(batch, profiles)
                })
                .collect()
        };

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
    pub fn from_config_with_db(cfg: &LoadedConfig, db_path: &str) -> Self {
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

        // 3. Try to load routing cache; fall back to OSM if not available.
        let cache_path = cfg.city.sim.routing_cache_path.as_deref();
        let cached = cache_path.and_then(|p| load_routing_cache(p));

        let osm: Option<OsmGraph> = if cached.is_some() {
            None // skip OSM loading — we have cached routing
        } else {
            cfg.city.sim.osm_path.as_ref().map(|p| {
                println!("Loading OSM road graph from: {}", p);
                let g = OsmGraph::load(Path::new(p)).expect("failed to load OSM PBF");
                println!("  → {} road nodes, {} edges", g.node_count(), g.edge_count());
                g
            })
        };

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

        // Phase B: build each district in parallel (routing engines are independent).
        let station_lookup = cfg.district_stations.by_district_id();

        let mut districts: Vec<District> = cfg.city.districts
            .iter()
            .zip(setups.iter())
            .collect::<Vec<_>>()
            .into_par_iter()
            .map(|(district_cfg, setup)| {
                let district_id      = DistrictId::new(district_cfg.id);
                let mut hex_cursor   = setup.hex_id_start;
                let mut unit_cursor  = setup.unit_id_start;

                // Build Hex objects from config; use pre-snapped OSM node if available.
                let use_osm = osm.is_some() || cached.is_some();
                let mut hexes: Vec<Hex> = hexes_by_district
                    .get(&district_cfg.id)
                    .map(|hs| hs.iter().map(|h| {
                        let node_id = h.nearest_osm_node.map(NodeId::new).or_else(|| {
                            if use_osm { None } else { Some(NodeId::new(hex_cursor)) }
                        });
                        let hex_id = HexId::new(hex_cursor);
                        hex_cursor += 1;
                        Hex {
                            id:                hex_id,
                            h3_index:          h.h3_index,
                            lat:               h.lat,
                            lon:               h.lon,
                            district:          district_id,
                            spawn_profile_id:  SpawnProfileId::new(h.spawn_profile_id.clone()),
                            nearest_road_node: node_id,
                        }
                    }).collect())
                    .unwrap_or_default();

                // Build routing engine: from cache, from OSM, or H3-adjacency fallback.
                let routing = if let Some(ref cache) = cached {
                    // Restore hex node assignments from cache.
                    if let Some(hex_nodes) = cache.hex_nodes.get(&district_cfg.id) {
                        for hex in hexes.iter_mut() {
                            if let Some(&node_id) = hex_nodes.get(&hex.h3_index) {
                                hex.nearest_road_node = Some(NodeId::new(node_id));
                            }
                        }
                    }
                    let snap = cache.snapshots.get(&district_cfg.id)
                        .expect("routing cache missing district");
                    RoutingEngine::from_snapshot(snap.clone())
                } else if let Some(ref osm_graph) = osm {
                    let lat_min = hexes.iter().map(|h| h.lat).fold(f64::MAX, f64::min);
                    let lat_max = hexes.iter().map(|h| h.lat).fold(f64::MIN, f64::max);
                    let lon_min = hexes.iter().map(|h| h.lon).fold(f64::MAX, f64::min);
                    let lon_max = hexes.iter().map(|h| h.lon).fold(f64::MIN, f64::max);

                    let subgraph = osm_graph.subgraph_for_bbox(
                        lat_min, lat_max, lon_min, lon_max, 0.02,
                    );

                    let mut anchors: Vec<NodeId> = Vec::with_capacity(hexes.len());
                    for hex in hexes.iter_mut() {
                        hex.nearest_road_node = Some(osm_graph.nearest_node(hex.lat, hex.lon));
                        anchors.push(hex.nearest_road_node.unwrap());
                    }
                    anchors.sort();
                    anchors.dedup();

                    RoutingEngine::from_graph(subgraph, &anchors)
                } else {
                    build_h3_routing_engine(&hexes)
                };

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

        // Save routing cache if we built from OSM and a cache path is configured.
        if cached.is_none() && osm.is_some() {
            if let Some(path) = cache_path {
                save_routing_cache(path, &districts);
            }
        }

        detect_border_nodes(&mut districts);
        let event_heap = seed_events(&districts, &profiles, &mut seed_rng);

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
        }
    }

    /// Convenience wrapper using the default output path.
    pub fn from_config(cfg: &LoadedConfig) -> Self {
        Self::from_config_with_db(cfg, "./output/dispatch_sim.db")
    }
}

// ---------------------------------------------------------------------------
// H3-adjacency routing fallback (used when no OSM path is configured)
// ---------------------------------------------------------------------------

/// Build a RoutingEngine from H3 cell adjacency with haversine-based travel times.
/// Each hex is connected to its H3 grid-disk-1 neighbors present in the district.
/// Assumes an average road speed of 30 km/h.
fn build_h3_routing_engine(hexes: &[Hex]) -> RoutingEngine {
    const SPEED_M_PER_MIN: f64 = 30_000.0 / 60.0; // 30 km/h

    let mut graph: RoadGraph = RoadGraph::new();
    let mut nx_by_h3: HashMap<u64, NodeIndex> = HashMap::with_capacity(hexes.len());

    for hex in hexes {
        let nx = graph.add_node(Node {
            id:       hex.node_id(),
            position: geo::Point::new(hex.lon, hex.lat),
        });
        nx_by_h3.insert(hex.h3_index, nx);
    }

    for hex in hexes {
        if let Ok(cell) = CellIndex::try_from(hex.h3_index) {
            let from_nx = nx_by_h3[&hex.h3_index];
            let disk: Vec<CellIndex> = cell.grid_disk::<Vec<_>>(1);
            for nbr in disk {
                let nbr_u64 = u64::from(nbr);
                if nbr_u64 == hex.h3_index { continue; }
                if let Some(&to_nx) = nx_by_h3.get(&nbr_u64) {
                    let nc = h3o::LatLng::from(nbr);
                    let dist_m = haversine_m(hex.lat, hex.lon, nc.lat(), nc.lng());
                    let time_min = ((dist_m / SPEED_M_PER_MIN) as u32).max(1);
                    graph.add_edge(from_nx, to_nx, Edge { travel_time_min: time_min });
                }
            }
        }
    }

    let anchors: Vec<NodeId> = hexes.iter().map(|h| h.node_id()).collect();
    RoutingEngine::from_graph(graph, &anchors)
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

/// Seed the initial event heap with ShiftChange and IncidentSpawn events.
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
// Routing cache (binary serialization)
// ---------------------------------------------------------------------------

/// In-memory representation of a loaded routing cache.
struct LoadedRoutingCache {
    /// district_id → RoutingSnapshot
    snapshots: HashMap<u32, RoutingSnapshot>,
    /// district_id → (h3_index → nearest_road_node raw id)
    hex_nodes: HashMap<u32, HashMap<u64, u32>>,
}

/// On-disk format for the routing cache file.
#[derive(serde::Serialize, serde::Deserialize)]
struct RoutingCacheFile {
    /// (district_id, snapshot, hex_node_assignments)
    /// hex_node_assignments: Vec<(h3_index, nearest_road_node_raw)>
    districts: Vec<(u32, RoutingSnapshot, Vec<(u64, u32)>)>,
}

fn load_routing_cache(path: &str) -> Option<LoadedRoutingCache> {
    let data = std::fs::read(path).ok()?;
    let (file, _): (RoutingCacheFile, _) = bincode::serde::decode_from_slice(
        &data,
        bincode::config::standard(),
    ).ok()?;

    println!("Loaded routing cache from: {} ({} districts)", path, file.districts.len());

    let mut snapshots = HashMap::new();
    let mut hex_nodes = HashMap::new();
    for (did, snap, nodes) in file.districts {
        snapshots.insert(did, snap);
        hex_nodes.insert(did, nodes.into_iter().collect());
    }
    Some(LoadedRoutingCache { snapshots, hex_nodes })
}

fn save_routing_cache(path: &str, districts: &[District]) {
    let file = RoutingCacheFile {
        districts: districts.iter().map(|d| {
            let snap = d.routing.to_snapshot();
            let hex_nodes: Vec<(u64, u32)> = d.hexes.iter()
                .filter_map(|h| {
                    h.nearest_road_node.map(|n| (h.h3_index, n.value()))
                })
                .collect();
            (d.id.value(), snap, hex_nodes)
        }).collect(),
    };

    let data = bincode::serde::encode_to_vec(&file, bincode::config::standard())
        .expect("failed to encode routing cache");

    if let Some(parent) = Path::new(path).parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(path, data).expect("failed to write routing cache");
    println!("Saved routing cache to: {} ({} districts)", path, districts.len());
}
