// optimize binary — district optimiser entry point.
//
// Usage:
//   cargo run --bin optimize -- config/optimize.toml
//
// Reads optimize.toml, generates H3 cells for the configured area, runs the
// p-median solver, and writes the result to hexes.json (path in optimize.toml).
// The simulator binary then reads that file.

use std::collections::HashMap;
use std::path::Path;
use std::process;

use h3o::Resolution;
use serde::Deserialize;

use dispatch_sim::config::SpawnProfileConfig;
use rayon::prelude::*;

use dispatch_sim::optimizer::{self, Constraints, H3Hex, ObjectiveWeights, Problem, Solver};
use dispatch_sim::optimizer::greedy::GreedySolver;
use dispatch_sim::optimizer::h3_grid;
use dispatch_sim::osm::OsmGraph;

// ---------------------------------------------------------------------------
// Optimizer config (optimize.toml)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct OptimizeConfig {
    n_districts:         usize,
    h3_resolution:       u8,
    area_geojson:        String,
    osm_path:            String,
    hex_output_path:     String,
    spawn_profiles_path: String,
    constraints:         ConstraintsConfig,
    objective:           ObjectiveConfig,
    solver:              SolverConfig,
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

    // 3. Generate H3 cells for the configured area.
    println!("Loading area polygon from: {}", cfg.area_geojson);
    let polygon = h3_grid::load_polygon(Path::new(&cfg.area_geojson)).unwrap_or_else(|e| {
        eprintln!("error loading GeoJSON: {e}"); process::exit(1);
    });

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

    // 4. Load OSM graph to snap hex centres to road nodes.
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

    // 6. Build problem.
    let problem = Problem {
        hexes,
        n_districts: cfg.n_districts,
        constraints: Constraints {
            contiguity:         cfg.constraints.contiguity,
            max_workload_ratio: cfg.constraints.max_workload_ratio,
        },
        objective: ObjectiveWeights {
            travel_time:      cfg.objective.travel_time_weight,
            workload_balance: cfg.objective.workload_balance_weight,
        },
    };

    // 7. Select and run solver.
    println!("Running {} p-median solver ({} districts, {} hexes)…",
        cfg.solver.algorithm, cfg.n_districts, problem.hexes.len());

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

    // Contiguity check: report whether any district is disconnected.
    if cfg.constraints.contiguity {
        println!("Contiguity: enforced");
    }

    // 8. Write hexes.json.
    let output_path = Path::new(&cfg.hex_output_path);
    optimizer::write_hexes_json(&problem.hexes, &solution, output_path)
        .unwrap_or_else(|e| {
            eprintln!("error writing {}: {e}", cfg.hex_output_path);
            process::exit(1);
        });

    println!("Written {} ({} entries)", cfg.hex_output_path, problem.hexes.len());
    println!("Run `cargo run -- config/city.toml` to simulate the optimized layout.");
}
