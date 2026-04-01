// main.rs

mod city;
mod clock;
mod osm;
mod config;
mod district;
mod event_log;
mod event_queue;
mod hex;
mod incident;
mod report;
mod routing;
mod spawner;
mod station;
mod types;
mod unit;

use std::path::Path;
use std::process;
use std::time::Instant;

use config::LoadedConfig;
use city::City;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.get(1).map(String::as_str) == Some("report") {
        let db_path = args.get(2).map(String::as_str).unwrap_or("./output/dispatch_sim.db");
        report::print_report(db_path).unwrap_or_else(|e| {
            eprintln!("error reading database: {}", e);
            process::exit(1);
        });
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

    let city = City::from_config(&cfg);

    println!(
        "City ready — {} districts, {} total units",
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

    // Upper bound for progress — actual iterations will be far fewer
    println!("Starting sim — {} min simulated time", sim_end);
    let now = Instant::now();

    let mut tick = 0u64;
    loop {
        // Stop when heap is empty or clock has passed sim end
        if city.event_heap.is_empty() || city.clock.elapsed_min >= sim_end {
            break;
        }

        city.tick();
        tick += 1;

        if tick % log_every == 0 {
            println!(
                "  event {:>10} — sim time: day {}, {:02}:{:02}",
                tick,
                city.clock.elapsed_min / 1440,
                city.clock.hour_of_day(),
                city.clock.elapsed_min % 60,
            );
        }
    }

    city.flush();

    let time = now.elapsed().as_millis() as i32;
    println!("Sim complete in: {}ms — {} events processed.", time, tick);
}

// ---------------------------------------------------------------------------
// Config path resolution
// ---------------------------------------------------------------------------

/// Checks CLI args first, falls back to the default location.
fn resolve_config_path() -> std::path::PathBuf {
    if let Some(path) = std::env::args().nth(1) {
        return std::path::PathBuf::from(path);
    }

    // Default: look for config/city.toml next to the binary.
    let default = Path::new("config/city.toml");
    if default.exists() {
        return default.to_path_buf();
    }

    eprintln!("error: no config path provided and config/city.toml not found");
    eprintln!("usage: dispatch_sim [path/to/city.toml]");
    process::exit(1);
}