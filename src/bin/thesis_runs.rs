// thesis_runs.rs
// Reproduces the seven thesis experiment runs in one CRN-paired batch:
//   §6.5 unit reallocation:        scen_base, scen_n1, scen_n2, scen_n3
//   §6.6 service-time sensitivity: svc_uni, svc_logn, svc_exp
//
// All variants share the base seed from config/city.toml (rng_seed = 42), so
// they consume identical arrival/priority streams — the CRN pairing reported
// in the thesis. Output DBs are written to ./output/<variant>.db; KPIs are
// extracted with thesis/extract_kpis.py (pass `=<fleet>`: 89 everywhere
// except scen_n3 = 91).
//
// Usage: cargo run --release --bin thesis_runs [duration_minutes]
//        (default 2 102 400 min = 4 simulated years)

use std::path::Path;
use std::process;
use std::time::Instant;

use dispatch_sim::config::{LoadedConfig, ServiceTimeDistRaw, ServiceTimeRaw};
use dispatch_sim::whatif::{SimBatch, Variant};

const FOUR_YEARS_MIN: u64 = 4 * 365 * 24 * 60; // 2_102_400

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let duration: u64 = args
        .get(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(FOUR_YEARS_MIN);

    let cfg = LoadedConfig::load(Path::new("config/city.toml")).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        process::exit(1);
    });

    // Service-time families for §6.6, per-priority mean matched to the
    // Larson/Chaiken uniform means (67.5 / 40 / 25 min).
    let sigma = 0.4_f64;
    let (mean_a, mean_b, mean_c) = (67.5_f64, 40.0_f64, 25.0_f64);

    let st_uniform = ServiceTimeRaw {
        a: Some(ServiceTimeDistRaw::Uniform { min: 45, max: 90 }),
        b: Some(ServiceTimeDistRaw::Uniform { min: 25, max: 55 }),
        c: Some(ServiceTimeDistRaw::Uniform { min: 15, max: 35 }),
    };
    let st_lognormal = ServiceTimeRaw {
        a: Some(ServiceTimeDistRaw::Lognormal { mu: mean_a.ln() - sigma * sigma / 2.0, sigma }),
        b: Some(ServiceTimeDistRaw::Lognormal { mu: mean_b.ln() - sigma * sigma / 2.0, sigma }),
        c: Some(ServiceTimeDistRaw::Lognormal { mu: mean_c.ln() - sigma * sigma / 2.0, sigma }),
    };
    let st_exponential = ServiceTimeRaw {
        a: Some(ServiceTimeDistRaw::Exponential { mean: mean_a }),
        b: Some(ServiceTimeDistRaw::Exponential { mean: mean_b }),
        c: Some(ServiceTimeDistRaw::Exponential { mean: mean_c }),
    };

    // District ids: 3 = PK 14 (bottleneck), 4 = PK 15 (donor), 17 = PK 34,
    // 7 = PK 21. Reallocations match the original 14-day thesis runs.
    //
    // The §6.6 service-time variants run on the *stable* n1 allocation: under
    // the baseline allocation PK 14 saturates and its queue backlog dominates
    // every response-time KPI, masking the service-time shape effect entirely.
    let n1_alloc = vec![(3, 4), (4, 4)];
    let variants = vec![
        Variant::identity("scen_base"),
        Variant::realloc("scen_n1 (PK 15 -2 -> PK 14 +2)", n1_alloc.clone()),
        Variant::realloc("scen_n2 (PK 15 -2 -> PK 34 +2)", vec![(4, 4), (17, 6)]),
        Variant::realloc("scen_n3 (PK 14 +1, PK 21 +1)", vec![(3, 3), (7, 3)]),
        Variant::realloc("svc_uni (n1 alloc)", n1_alloc.clone()).with_service_time(st_uniform),
        Variant::realloc("svc_logn (n1 alloc)", n1_alloc.clone()).with_service_time(st_lognormal),
        Variant::realloc("svc_exp (n1 alloc)", n1_alloc).with_service_time(st_exponential),
    ];

    println!("Thesis experiment batch — 7 variants, {duration} min each ({:.1} days)",
             duration as f64 / 1440.0);

    let batch = SimBatch::from_config(cfg)
        .unwrap_or_else(|e| { eprintln!("error: {e}"); process::exit(1); })
        .with_variants(variants)
        .with_db_pattern("./output/thesis_{}.db")
        .with_duration(duration)
        .with_record_routes(false);

    let started = Instant::now();
    let results = batch.run();
    let failed = results.iter().filter(|r| r.is_err()).count();
    println!("\nBatch complete in {}s ({failed} failed).", started.elapsed().as_secs());
    println!("DB index map: 0=scen_base 1=scen_n1 2=scen_n2 3=scen_n3 4=svc_uni 5=svc_logn 6=svc_exp");
}
