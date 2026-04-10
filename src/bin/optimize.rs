// optimize binary — district optimiser entry point.
//
// Usage:
//   cargo run --bin optimize -- config/optimize.toml
//
// Reads optimize.toml, generates H3 cells for the configured area, runs the
// p-median solver, and writes the result to hexes.json (path in optimize.toml).
// The simulator binary then reads that file.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process;

use h3o::{CellIndex, Resolution};
use serde::Deserialize;

use dispatch_sim::config::SpawnProfileConfig;
use dispatch_sim::geo_utils::haversine_m;
use dispatch_sim::routing::RoutingEngine;
use dispatch_sim::routing_cache;
use dispatch_sim::types::NodeId;
use rayon::prelude::*;

use dispatch_sim::optimizer::{self, CandidateStation, Constraints, H3Hex, ObjectiveWeights, Problem, Solver};
use dispatch_sim::optimizer::greedy::GreedySolver;
use dispatch_sim::optimizer::h3_grid;
use dispatch_sim::osm::{OsmGraph, PoliceStation, extract_admin_boundary, extract_police_stations};

// ---------------------------------------------------------------------------
// Optimizer config (optimize.toml)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct OptimizeConfig {
    n_districts:             usize,
    h3_resolution:           u8,
    #[allow(dead_code)]
    area_geojson:            String, // kept for config compatibility; boundary is now extracted from OSM
    osm_path:                String,
    hex_output_path:         String,
    spawn_profiles_path:     String,
    /// Path to a JSON array of candidate stations [{name, lat, lon}].
    /// Defaults to "config/police_stations.json" (written by the optimizer itself).
    station_candidates_path: Option<String>,
    /// Output path for districts.json (district → selected station mapping).
    /// Defaults to "config/districts.json".
    districts_output_path:   Option<String>,
    /// Output path for the binary routing cache consumed by the simulator.
    /// Defaults to "output/routing_cache.bin" to match `city.toml`'s
    /// `routing_cache_path`.
    routing_cache_output_path: Option<String>,
    constraints:             ConstraintsConfig,
    objective:               ObjectiveConfig,
    solver:                  SolverConfig,
}

#[derive(Deserialize)]
struct ConstraintsConfig {
    contiguity:          bool,
    max_workload_ratio:  Option<f64>,
}

#[derive(Deserialize)]
struct ObjectiveConfig {
    travel_time_weight:       f64,
    workload_balance_weight:  f64,
}

#[derive(Deserialize)]
struct SolverConfig {
    algorithm: String,
}

/// Minimal city.toml struct — only the fields the optimizer needs.
#[derive(Deserialize)]
struct SpawnProfilesOnly {
    spawn_profiles: HashMap<String, SpawnProfileConfig>,
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cfg_path = args.get(1).unwrap_or_else(|| {
        eprintln!("usage: optimize <path/to/optimize.toml>");
        process::exit(1);
    });

    // 1. Load optimizer config.
    let cfg: OptimizeConfig = {
        let raw = std::fs::read_to_string(cfg_path).unwrap_or_else(|e| {
            eprintln!("error reading {cfg_path}: {e}"); process::exit(1);
        });
        toml::from_str(&raw).unwrap_or_else(|e| {
            eprintln!("error parsing {cfg_path}: {e}"); process::exit(1);
        })
    };

    // 2. Load spawn profiles from city.toml (only the relevant section).
    let spawn_cfg: SpawnProfilesOnly = {
        let raw = std::fs::read_to_string(&cfg.spawn_profiles_path).unwrap_or_else(|e| {
            eprintln!("error reading {}: {e}", cfg.spawn_profiles_path); process::exit(1);
        });
        toml::from_str(&raw).unwrap_or_else(|e| {
            eprintln!("error parsing spawn profiles: {e}"); process::exit(1);
        })
    };

    // 3. Extract police stations from OSM and write police_stations.json.
    //    Done early so the default station_candidates_path points to a fresh file.
    println!("Extracting police stations from OSM…");
    let police_stations_path = "config/police_stations.json";
    match extract_police_stations(Path::new(&cfg.osm_path)) {
        Ok(stations) => {
            println!("  → {} station(s) found", stations.len());
            let json = serde_json::to_string_pretty(&stations)
                .expect("failed to serialise police stations");
            std::fs::write(police_stations_path, json)
                .unwrap_or_else(|e| eprintln!("warning: could not write {police_stations_path}: {e}"));
            println!("  → written to {police_stations_path}");
        }
        Err(e) => eprintln!("warning: could not extract police stations: {e}"),
    }

    // 4. Extract Hamburg boundary from the OSM file (replaces the hand-drawn GeoJSON).
    println!("Extracting Hamburg boundary from OSM: {}", cfg.osm_path);
    let polygon = extract_admin_boundary(Path::new(&cfg.osm_path)).unwrap_or_else(|e| {
        eprintln!("error extracting boundary: {e}"); process::exit(1);
    });
    println!("  → boundary extracted (Neuwerk island excluded)");

    let resolution = Resolution::try_from(cfg.h3_resolution).unwrap_or_else(|_| {
        eprintln!("invalid h3_resolution {} (must be 0–15)", cfg.h3_resolution);
        process::exit(1);
    });

    println!("Generating H3 cells at resolution {}…", cfg.h3_resolution);
    let cells = h3_grid::cells_for_polygon(&polygon, resolution);
    println!("  → {} cells found", cells.len());

    if cells.is_empty() {
        eprintln!("error: no H3 cells found inside the polygon — check the GeoJSON boundary");
        process::exit(1);
    }

    // 5. Load OSM graph to snap hex centres to road nodes.
    println!("Loading OSM graph from: {}", cfg.osm_path);
    let osm = OsmGraph::load(Path::new(&cfg.osm_path)).unwrap_or_else(|e| {
        eprintln!("error loading OSM: {e}"); process::exit(1);
    });
    println!("  → {} road nodes, {} edges", osm.node_count(), osm.edge_count());

    // 5. Build H3Hex list (parallel OSM snap — O(log n) per cell via R-tree).
    println!("Snapping {} cells to nearest OSM nodes…", cells.len());
    let hexes: Vec<H3Hex> = cells.par_iter().map(|(cell, lat, lon)| {
        // All cells start as "residential"; commercial/mixed zones can be added
        // later via a GeoJSON overlay mapping lat/lon → profile_id.
        let profile_id = "residential".to_string();
        let spawn_rate = spawn_cfg.spawn_profiles
            .get(&profile_id)
            .map(|p| p.base_lambda / 60.0)  // per-minute rate
            .unwrap_or(0.0);

        let nearest_osm_node = osm.nearest_node(*lat, *lon).value();

        H3Hex {
            index: u64::from(*cell),
            lat: *lat,
            lon: *lon,
            spawn_rate,
            profile_id,
            nearest_osm_node,
        }
    }).collect();

    // 5b. Filter hexes to those in the main road-network connected component.

    //     Removes isolated enclaves (e.g. Neuwerk island) that have no road
    //     connection to the main Hamburg network.
    println!("Computing main road-network component…");
    let main_nodes = osm.main_component_node_ids();
    let before = hexes.len();
    let hexes: Vec<H3Hex> = hexes.into_iter()
        .filter(|h| main_nodes.contains(&dispatch_sim::types::NodeId::new(h.nearest_osm_node)))
        .collect();
    let removed = before - hexes.len();
    if removed > 0 {
        println!("  → Filtered {} disconnected hexes ({} remain)", removed, hexes.len());
    }

    // 6. Load candidate stations and snap each to the nearest OSM road node.
    let candidates_path = cfg.station_candidates_path
        .as_deref()
        .unwrap_or("config/police_stations.json");
    println!("Loading candidate stations from: {}", candidates_path);
    let raw_candidates: Vec<PoliceStation> = {
        let raw = std::fs::read_to_string(candidates_path).unwrap_or_else(|e| {
            eprintln!("error reading {candidates_path}: {e}"); process::exit(1);
        });
        serde_json::from_str(&raw).unwrap_or_else(|e| {
            eprintln!("error parsing {candidates_path}: {e}"); process::exit(1);
        })
    };
    println!("  → {} candidate stations loaded", raw_candidates.len());

    if raw_candidates.len() < cfg.n_districts {
        eprintln!(
            "error: only {} candidate stations available but n_districts={} — \
             add more candidates or reduce n_districts",
            raw_candidates.len(), cfg.n_districts
        );
        process::exit(1);
    }

    let candidates: Vec<CandidateStation> = raw_candidates.iter().map(|ps| {
        let nearest_osm_node = osm.nearest_node(ps.lat, ps.lon).value();
        CandidateStation {
            name:             ps.name.clone(),
            lat:              ps.lat,
            lon:              ps.lon,
            nearest_osm_node,
        }
    }).collect();
    println!("  → candidates snapped to OSM road network");

    // 7a. Precompute road travel times from each candidate station (Dijkstra).
    let m = candidates.len();
    let n = hexes.len();
    println!("Computing road travel times from {} candidates to {} hexes…", m, n);
    let station_travel_times: Vec<HashMap<NodeId, u32>> = candidates.par_iter()
        .map(|c| osm.single_source_travel_times(NodeId::new(c.nearest_osm_node)))
        .collect();

    let mut dist_cs: Vec<f64> = Vec::with_capacity(m * n);
    for c in 0..m {
        let tt = &station_travel_times[c];
        for h in 0..n {
            let node = NodeId::new(hexes[h].nearest_osm_node);
            let d = tt.get(&node)
                .map(|&t| t as f64)
                .unwrap_or(f64::MAX / 2.0); // unreachable → effectively infinite
            dist_cs.push(d);
        }
    }
    println!("  → {}×{} road-distance matrix built", m, n);

    // 7b. Build road-aware adjacency (filter H3 edges that cross water).
    //     Batch single-source Dijkstra from each unique hex node (bounded to
    //     a small radius), then use the lookup for O(1) adjacency checks.
    println!("Building road-aware adjacency…");
    let cell_map: HashMap<u64, usize> = hexes.iter().enumerate()
        .map(|(i, h)| (h.index, i)).collect();

    // Collect unique hex OSM nodes and precompute bounded travel times from each.
    // The bound of 10 min covers all possible H3 neighbor checks (neighbours are
    // ~200 m apart; the max_time threshold is 3× haversine / 500 m/min, min 5 min).
    let mut unique_hex_nodes: Vec<u32> = hexes.iter().map(|h| h.nearest_osm_node).collect();
    unique_hex_nodes.sort();
    unique_hex_nodes.dedup();
    let hex_node_set: HashSet<NodeId> = unique_hex_nodes.iter()
        .map(|&id| NodeId::new(id)).collect();

    println!("  Precomputing bounded travel times from {} unique hex nodes…", unique_hex_nodes.len());
    let hex_travel_times: HashMap<NodeId, HashMap<NodeId, u32>> = unique_hex_nodes
        .par_iter()
        .map(|&node_raw| {
            let node = NodeId::new(node_raw);
            let times = osm.bounded_single_source(node, &hex_node_set, 10);
            (node, times)
        })
        .collect();
    println!("  → {} bounded Dijkstra runs complete", unique_hex_nodes.len());

    let adj: Vec<Vec<usize>> = hexes.par_iter().enumerate().map(|(i, h)| {
        let Ok(cell) = CellIndex::try_from(h.index) else { return vec![]; };
        let from = NodeId::new(h.nearest_osm_node);
        let from_times = hex_travel_times.get(&from);
        cell.grid_disk::<Vec<_>>(1).iter()
            .filter_map(|nbr| cell_map.get(&u64::from(*nbr)).copied())
            .filter(|&j| j != i)
            .filter(|&j| {
                let to = NodeId::new(hexes[j].nearest_osm_node);
                let hav = haversine_m(h.lat, h.lon, hexes[j].lat, hexes[j].lon);
                // 3× haversine at 30 km/h (500 m/min), minimum 5 min.
                let max_time = ((hav * 3.0) / 500.0) as u32;
                from_times
                    .and_then(|tt| tt.get(&to))
                    .map(|&cost| cost <= max_time.max(5))
                    .unwrap_or(false)
            })
            .collect()
    }).collect();
    println!("  → adjacency filtered");

    // 7c. Build problem.
    let problem = Problem {
        hexes,
        candidate_stations: candidates,
        n_districts: cfg.n_districts,
        constraints: Constraints {
            contiguity:         cfg.constraints.contiguity,
            max_workload_ratio: cfg.constraints.max_workload_ratio,
        },
        objective: ObjectiveWeights {
            travel_time:      cfg.objective.travel_time_weight,
            workload_balance: cfg.objective.workload_balance_weight,
        },
        distance_matrix:    Some(dist_cs),
        adjacency_override: Some(adj),
    };

    // 8. Select and run solver.
    println!("Running {} p-median solver ({} districts, {} hexes, {} candidates)…",
        cfg.solver.algorithm, cfg.n_districts, problem.hexes.len(), problem.candidate_stations.len());

    let solver: Box<dyn Solver> = match cfg.solver.algorithm.as_str() {
        "greedy" => Box::new(GreedySolver),
        other => {
            eprintln!("unknown solver '{}' — supported: greedy", other);
            process::exit(1);
        }
    };

    let solution = solver.solve(&problem).unwrap_or_else(|e| {
        eprintln!("solver failed: {e}"); process::exit(1);
    });

    println!("Objective: {:.2} m·incident/min", solution.objective);

    let mean_load = solution.district_loads.iter().sum::<f64>() / cfg.n_districts as f64;
    let max_load  = solution.district_loads.iter().cloned().fold(f64::MIN, f64::max);
    if mean_load > 0.0 {
        println!("Workload ratio: {:.2}", max_load / mean_load);
    }

    if cfg.constraints.contiguity {
        println!("Contiguity: enforced");
    }

    // 9. Write hexes.json.
    let output_path = Path::new(&cfg.hex_output_path);
    optimizer::write_hexes_json(&problem.hexes, &solution, output_path)
        .unwrap_or_else(|e| {
            eprintln!("error writing {}: {e}", cfg.hex_output_path);
            process::exit(1);
        });
    println!("Written {} ({} entries)", cfg.hex_output_path, problem.hexes.len());

    // 10. Write districts.json.
    let districts_path = cfg.districts_output_path
        .as_deref()
        .unwrap_or("config/districts.json");
    optimizer::write_districts_json(&problem.candidate_stations, &solution, Path::new(districts_path))
        .unwrap_or_else(|e| {
            eprintln!("error writing {districts_path}: {e}");
            process::exit(1);
        });
    println!("Written {districts_path} ({} entries)", solution.station_indices.len());

    // 11. Build and persist the routing cache for the simulator.
    //
    // We build a single city-wide RoutingEngine from the full OSM graph with
    // anchors from ALL districts (hex nodes + station nodes). This gives
    // every district seamless cross-border routing — essential for mutual aid.
    // The sim loads one shared engine and assigns it to every district.
    let cache_path = cfg.routing_cache_output_path
        .as_deref()
        .unwrap_or("output/routing_cache.bin");

    println!("Building city-wide routing cache for {} districts…", cfg.n_districts);

    // Group hexes by their assigned district_id.
    let mut hexes_by_district: HashMap<u32, Vec<&H3Hex>> = HashMap::new();
    for (i, h) in problem.hexes.iter().enumerate() {
        let did = solution.assignments[i] as u32;
        hexes_by_district.entry(did).or_default().push(h);
    }

    // Collect ALL anchors across all districts: every hex node + every station node.
    let mut all_anchors: Vec<NodeId> = problem.hexes.iter()
        .map(|h| NodeId::new(h.nearest_osm_node))
        .collect();
    for district_id in 0..cfg.n_districts as u32 {
        let station_idx = solution.station_indices[district_id as usize];
        all_anchors.push(NodeId::new(problem.candidate_stations[station_idx].nearest_osm_node));
    }
    all_anchors.sort_by_key(|n| n.value());
    all_anchors.dedup();
    println!("  → {} unique anchor nodes across all districts", all_anchors.len());

    // Build one RoutingEngine on the full city OSM graph.
    let city_graph = osm.to_road_graph();
    println!("  → city graph: {} nodes, {} edges", city_graph.node_count(), city_graph.edge_count());
    let engine = RoutingEngine::from_graph(city_graph, &all_anchors);
    let city_snapshot = engine.to_snapshot();

    // Per-district hex → OSM node maps.
    let district_hex_nodes: Vec<(u32, Vec<(u64, u32)>)> = (0..cfg.n_districts as u32)
        .map(|district_id| {
            let hex_nodes: Vec<(u64, u32)> = hexes_by_district
                .get(&district_id)
                .map(|hs| hs.iter().map(|h| (h.index, h.nearest_osm_node)).collect())
                .unwrap_or_default();
            (district_id, hex_nodes)
        })
        .collect();

    routing_cache::save(cache_path, city_snapshot, district_hex_nodes);

    println!("Run `cargo run -- config/city.toml` to simulate the optimized layout.");
}
