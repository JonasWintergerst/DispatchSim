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
    use dispatch_sim::whatif::{generate_reallocation_variants, run_variant, print_comparison_table};

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

    let variants = generate_reallocation_variants(&base_counts, delta, max_variants);
    println!("\nGenerated {} variants. Running simulations…\n", variants.len());

    let config_p = Path::new(config_path);
    let mut results = Vec::new();

    for (i, variant) in variants.iter().enumerate() {
        let db_path = format!("./output/whatif_{i}.db");
        print!("  [{}/{}] {:<40} … ", i + 1, variants.len(), variant.name);

        let now = Instant::now();
        match run_variant(config_p, variant, &db_path, sim_duration) {
            Ok(r) => {
                let elapsed = now.elapsed().as_secs();
                println!("done ({elapsed}s) — SLA overall: {:.1}%", r.overall_sla_pct());
                results.push(r);
            }
            Err(e) => {
                println!("FAILED: {e}");
            }
        }
    }

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
