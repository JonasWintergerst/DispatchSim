use std::path::Path;
use std::process;
use std::time::Instant;

use dispatch_sim::config::LoadedConfig;
use dispatch_sim::whatif::{
    generate_patrol_variants, generate_reallocation_variants, print_comparison_table, SimBatch,
    Variant, VariantResult,
};

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

    run_standard(&args);
}

// ---------------------------------------------------------------------------
// Standard sim — a SimBatch with one identity variant
// ---------------------------------------------------------------------------

fn run_standard(args: &[String]) {
    let config_path = resolve_config_path(args);

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
    let batch = SimBatch::from_config(cfg)
        .unwrap_or_else(|e| { eprintln!("error: {e}"); process::exit(1); })
        .with_variants(vec![Variant::identity("Standard sim")])
        .with_db_pattern("./output/dispatch_sim.db");
    let setup_ms = setup_start.elapsed().as_millis();
    println!("Batch ready  [{setup_ms} ms setup]");

    let started = Instant::now();
    let results = batch.run();
    finalise(results, started);
}

// ---------------------------------------------------------------------------
// What-if unit reallocation
// ---------------------------------------------------------------------------

fn run_whatif(args: &[String]) {
    let config_path = args.get(2).map(String::as_str).unwrap_or("config/city.toml");
    let max_variants: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(10);
    let delta: u32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(2);
    let sim_duration: Option<u64> = args.get(5).and_then(|s| s.parse().ok());

    let cfg = LoadedConfig::load(Path::new(config_path)).unwrap_or_else(|e| {
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

    let variants = generate_reallocation_variants(&base_counts, delta, max_variants);
    let n_variants = variants.len();
    println!("\nGenerated {n_variants} variants. Running simulations in parallel…\n");

    let mut batch = SimBatch::from_config(cfg)
        .unwrap_or_else(|e| { eprintln!("error: {e}"); process::exit(1); })
        .with_variants(variants)
        .with_db_pattern("./output/whatif_{}.db")
        .with_record_routes(false);
    if let Some(d) = sim_duration {
        batch = batch.with_duration(d);
    }

    let started = Instant::now();
    let results = batch.run();
    finalise(results, started);
}

// ---------------------------------------------------------------------------
// What-if patrol strategy comparison
// ---------------------------------------------------------------------------

fn run_whatif_patrol(args: &[String]) {
    let config_path = args.get(2).map(String::as_str).unwrap_or("config/city.toml");

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

    let cfg = LoadedConfig::load(Path::new(config_path)).unwrap_or_else(|e| {
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

    let mut variants = generate_patrol_variants(&base_counts, &route_paths);
    if !include_aid {
        variants.retain(|v| {
            v.name != "Mutual aid only" && !v.name.ends_with(" + aid")
        });
    }
    let n_variants = variants.len();
    println!("\nGenerated {n_variants} variants. Running simulations in parallel…\n");

    let mut batch = SimBatch::from_config(cfg)
        .unwrap_or_else(|e| { eprintln!("error: {e}"); process::exit(1); })
        .with_variants(variants)
        .with_db_pattern("./output/whatif_patrol_{}.db")
        .with_record_routes(false);
    if let Some(d) = sim_duration {
        batch = batch.with_duration(d);
    }

    let started = Instant::now();
    let results = batch.run();
    finalise(results, started);
}

// ---------------------------------------------------------------------------
// Result handling — single-line summary for batches of 1, table otherwise
// ---------------------------------------------------------------------------

fn finalise(results: Vec<Result<VariantResult, String>>, started: Instant) {
    let mut ok: Vec<VariantResult> = results.into_iter().filter_map(|r| r.ok()).collect();
    let elapsed = started.elapsed().as_secs();

    if ok.len() == 1 {
        let r = &ok[0];
        println!(
            "\nSim complete in {}s — SLA overall: {:.1}%  (A: {:.1}%, B: {:.1}%, C: {:.1}%)",
            elapsed,
            r.overall_sla_pct(),
            r.sla_a.pct(),
            r.sla_b.pct(),
            r.sla_c.pct(),
        );
    } else {
        println!("\nBatch complete in {elapsed}s.");
        print_comparison_table(&mut ok);
    }
}

// ---------------------------------------------------------------------------
// Config path resolution
// ---------------------------------------------------------------------------

fn resolve_config_path(args: &[String]) -> std::path::PathBuf {
    if let Some(path) = args.get(1) {
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
