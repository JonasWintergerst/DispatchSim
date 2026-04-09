use std::path::Path;
use std::process;
use std::time::Instant;

use dispatch_sim::city::City;
use dispatch_sim::config::LoadedConfig;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.get(1).map(String::as_str) == Some("report") {
        let db_path = args.get(2).map(String::as_str).unwrap_or("./output/dispatch_sim.db");
        dispatch_sim::report::print_report(db_path).unwrap_or_else(|e| {
            eprintln!("error reading database: {}", e);
            process::exit(1);
        });
        return;
    }

    if args.get(1).map(String::as_str) == Some("heatmap") {
        let db_path  = args.get(2).map(String::as_str).unwrap_or("./output/dispatch_sim.db");
        let out_path = args.get(3).map(String::as_str).unwrap_or("./output/heatmap.geojson");
        dispatch_sim::report::export_heatmap(db_path, out_path).unwrap_or_else(|e| {
            eprintln!("error generating heatmap: {}", e);
            process::exit(1);
        });
        return;
    }

    if args.get(1).map(String::as_str) == Some("whatif") {
        run_whatif(&args);
        return;
    }

    if args.get(1).map(String::as_str) == Some("whatif-patrol") {
        run_whatif_patrol(&args);
        return;
    }

    let config_path = resolve_config_path();

    println!("Loading config from: {}", config_path.display());

    let cfg = LoadedConfig::load(&config_path).unwrap_or_else(|e| {
        eprintln!("error: {}", e);
        process::exit(1);
    });

    println!(
        "Config loaded — sim_type: {:?}, districts: {}, duration: {} min",
        cfg.city.sim.sim_type,
        cfg.city.districts.len(),
        cfg.city.sim.duration_minutes,
    );

    let setup_start = Instant::now();
    let city = City::from_config(&cfg);
    let setup_ms = setup_start.elapsed().as_millis();

    println!(
        "City ready — {} districts, {} total units  [{setup_ms} ms setup]",
        city.districts.len(),
        city.districts.iter().map(|d| d.units.len()).sum::<usize>(),
    );

    run(city, &cfg);
}

// ---------------------------------------------------------------------------
// Sim loop
// ---------------------------------------------------------------------------

fn run(mut city: City, cfg: &LoadedConfig) {
    let sim_end = cfg.city.sim.duration_minutes;
    let log_every = 10_000;

    println!("Starting sim — {} min simulated time", sim_end);
    let now = Instant::now();

    let mut tick = 0u64;
    loop {
        if city.event_heap.is_empty() || city.clock.elapsed_min >= sim_end {
            break;
        }

        city.tick();
        tick += 1;

        if tick % log_every == 0 {
            let pct = (city.clock.elapsed_min as f64 / sim_end as f64) * 100.0;
            println!(
                "  event {:>10} — sim time: day {}, {:02}:{:02} — {:.1}%",
                tick,
                city.clock.elapsed_min / 1440,
                city.clock.hour_of_day(),
                city.clock.elapsed_min % 60,
                pct,
            );
        }
    }

    city.flush();

    let time = now.elapsed().as_millis() as i32;
    println!("Sim complete in: {}ms — {} events processed.", time, tick);
}

// ---------------------------------------------------------------------------
// What-if unit reallocation
// ---------------------------------------------------------------------------

fn run_whatif(args: &[String]) {
    use std::sync::Arc;
    use rayon::prelude::*;
    use dispatch_sim::whatif::{generate_reallocation_variants, run_variant_with_cache, print_comparison_table};

    let config_path = args.get(2).map(String::as_str).unwrap_or("config/city.toml");
    let max_variants: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(10);
    let delta: u32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(2);
    // Optional: shorter sim duration for fast iteration (in minutes)
    let sim_duration: Option<u64> = args.get(5).and_then(|s| s.parse().ok());

    let cfg = dispatch_sim::config::LoadedConfig::load(Path::new(config_path)).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        process::exit(1);
    });

    let base_counts: Vec<(u32, String, u32)> = cfg.city.districts
        .iter()
        .map(|d| (d.id, d.name.clone(), d.unit_count))
        .collect();

    let total: u32 = base_counts.iter().map(|(_, _, c)| c).sum();
    println!("What-If Unit Reallocation");
    println!("  Config:       {config_path}");
    println!("  Districts:    {}", base_counts.len());
    println!("  Total units:  {total}");
    println!("  Delta:        ±{delta} units per transfer");
    println!("  Max variants: {max_variants}");
    if let Some(d) = sim_duration {
        println!("  Sim duration: {d} min (shortened)");
    }

    // Load the routing cache once and share it across all parallel workers.
    // Every variant deserialises its own `RoutingEngine` from the snapshots,
    // but the bincode decode of the cache file itself happens exactly once.
    let cache_path = cfg.city.sim.routing_cache_path.as_deref().unwrap_or_else(|| {
        eprintln!("error: sim.routing_cache_path is not set in {config_path}");
        process::exit(1);
    });
    let cache = Arc::new(
        dispatch_sim::routing_cache::load(cache_path).unwrap_or_else(|| {
            eprintln!("error: routing cache not found at {cache_path} — run `cargo run --bin optimize` first");
            process::exit(1);
        }),
    );

    let variants = generate_reallocation_variants(&base_counts, delta, max_variants);
    let n_variants = variants.len();
    println!("\nGenerated {n_variants} variants. Running simulations in parallel…\n");

    let config_p = Path::new(config_path);
    let batch_start = Instant::now();

    // Parallelise across variants. Each worker owns its own `City` and its own
    // SQLite DB (per-variant path), so there is no shared mutable state beyond
    // the read-only `Arc<LoadedRoutingCache>`. Determinism is preserved because
    // every variant re-seeds from `cfg.city.sim.rng_seed`, which is constant.
    let mut indexed: Vec<(usize, Result<dispatch_sim::whatif::VariantResult, String>)> = variants
        .par_iter()
        .enumerate()
        .map(|(i, variant)| {
            let db_path = format!("./output/whatif_{i}.db");
            let now = Instant::now();
            let res = run_variant_with_cache(
                config_p,
                variant,
                &db_path,
                sim_duration,
                Arc::clone(&cache),
            );
            match &res {
                Ok(r)  => println!("  [{:>3}/{n_variants}] {:<40} done ({}s) — SLA overall: {:.1}%",
                                   i + 1, variant.name, now.elapsed().as_secs(), r.overall_sla_pct()),
                Err(e) => println!("  [{:>3}/{n_variants}] {:<40} FAILED: {e}",
                                   i + 1, variant.name),
            }
            (i, res)
        })
        .collect();

    // Sort by variant index so output order is deterministic regardless of
    // the order rayon happened to complete them in.
    indexed.sort_by_key(|(i, _)| *i);
    let mut results: Vec<_> = indexed.into_iter().filter_map(|(_, r)| r.ok()).collect();

    println!("\nBatch complete in {}s.", batch_start.elapsed().as_secs());
    print_comparison_table(&mut results);
}

// ---------------------------------------------------------------------------
// What-if patrol strategy comparison
// ---------------------------------------------------------------------------

fn run_whatif_patrol(args: &[String]) {
    use std::sync::Arc;
    use rayon::prelude::*;
    use dispatch_sim::whatif::{generate_patrol_variants, run_variant_with_cache, print_comparison_table};

    let config_path = args.get(2).map(String::as_str).unwrap_or("config/city.toml");

    // Parse remaining args: --strategy <label> (repeatable), --no-aid, [duration_min]
    let mut strategy_filter: Vec<String> = Vec::new();
    let mut include_aid = true;
    let mut sim_duration: Option<u64> = None;
    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "--strategy" => {
                if let Some(label) = args.get(i + 1) {
                    strategy_filter.push(label.clone());
                    i += 2;
                } else {
                    eprintln!("error: --strategy requires a label");
                    process::exit(1);
                }
            }
            "--no-aid" => { include_aid = false; i += 1; }
            other => {
                if let Ok(dur) = other.parse::<u64>() {
                    sim_duration = Some(dur);
                }
                i += 1;
            }
        }
    }

    let cfg = dispatch_sim::config::LoadedConfig::load(Path::new(config_path)).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        process::exit(1);
    });

    let base_counts: Vec<(u32, String, u32)> = cfg.city.districts
        .iter()
        .map(|d| (d.id, d.name.clone(), d.unit_count))
        .collect();

    // Discover patrol_routes_*.json under config/
    let mut all_routes: Vec<(String, String)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir("config") {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") { continue; }
            let stem = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s,
                None => continue,
            };
            if let Some(label) = stem.strip_prefix("patrol_routes_") {
                all_routes.push((label.to_string(), path.to_string_lossy().into_owned()));
            }
        }
    }
    all_routes.sort_by(|a, b| a.0.cmp(&b.0));

    let route_paths: Vec<(String, String)> = if strategy_filter.is_empty() {
        all_routes
    } else {
        all_routes.into_iter()
            .filter(|(label, _)| strategy_filter.iter().any(|s| s == label))
            .collect()
    };

    if route_paths.is_empty() {
        eprintln!("error: no patrol_routes_*.json files found in config/ (or none matched the --strategy filter)");
        eprintln!("hint: run `cargo run --bin patrol_gen -- {config_path} hotspot` first");
        process::exit(1);
    }

    println!("What-If Patrol Strategy Comparison");
    println!("  Config:    {config_path}");
    println!("  Districts: {}", base_counts.len());
    println!("  Strategies: {}", route_paths.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>().join(", "));
    println!("  Mutual aid variants: {}", if include_aid { "yes" } else { "no" });
    if let Some(d) = sim_duration {
        println!("  Sim duration: {d} min (shortened)");
    }

    let cache_path = cfg.city.sim.routing_cache_path.as_deref().unwrap_or_else(|| {
        eprintln!("error: sim.routing_cache_path is not set in {config_path}");
        process::exit(1);
    });
    let cache = Arc::new(
        dispatch_sim::routing_cache::load(cache_path).unwrap_or_else(|| {
            eprintln!("error: routing cache not found at {cache_path} — run `cargo run --bin optimize` first");
            process::exit(1);
        }),
    );

    let mut variants = generate_patrol_variants(&base_counts, &route_paths);
    if !include_aid {
        variants.retain(|v| {
            v.name != "Mutual aid only" && !v.name.ends_with(" + aid")
        });
    }
    let n_variants = variants.len();
    println!("\nGenerated {n_variants} variants. Running simulations in parallel…\n");

    let config_p = Path::new(config_path);
    let batch_start = Instant::now();

    let mut indexed: Vec<(usize, Result<dispatch_sim::whatif::VariantResult, String>)> = variants
        .par_iter()
        .enumerate()
        .map(|(i, variant)| {
            let db_path = format!("./output/whatif_patrol_{i}.db");
            let now = Instant::now();
            let res = run_variant_with_cache(
                config_p,
                variant,
                &db_path,
                sim_duration,
                Arc::clone(&cache),
            );
            match &res {
                Ok(r)  => println!("  [{:>3}/{n_variants}] {:<40} done ({}s) — SLA overall: {:.1}%",
                                   i + 1, variant.name, now.elapsed().as_secs(), r.overall_sla_pct()),
                Err(e) => println!("  [{:>3}/{n_variants}] {:<40} FAILED: {e}",
                                   i + 1, variant.name),
            }
            (i, res)
        })
        .collect();

    indexed.sort_by_key(|(i, _)| *i);
    let mut results: Vec<_> = indexed.into_iter().filter_map(|(_, r)| r.ok()).collect();

    println!("\nBatch complete in {}s.", batch_start.elapsed().as_secs());
    print_comparison_table(&mut results);
}

// ---------------------------------------------------------------------------
// Config path resolution
// ---------------------------------------------------------------------------

fn resolve_config_path() -> std::path::PathBuf {
    if let Some(path) = std::env::args().nth(1) {
        return std::path::PathBuf::from(path);
    }

    let default = Path::new("config/city.toml");
    if default.exists() {
        return default.to_path_buf();
    }

    eprintln!("error: no config path provided and config/city.toml not found");
    eprintln!("usage: dispatch_sim [path/to/city.toml]");
    process::exit(1);
}
