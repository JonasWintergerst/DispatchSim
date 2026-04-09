// whatif.rs
// What-if unit reallocation: run N sim variants with different unit_count
// assignments across districts, collect SLA compliance per variant, and
// output a ranked comparison table.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use rusqlite::Connection;

use crate::city::City;
use crate::config::LoadedConfig;
use crate::routing_cache::LoadedRoutingCache;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A single what-if variant: a label plus per-district unit counts and
/// optional patrol/mutual-aid overrides.
#[derive(Clone, Debug)]
pub struct Variant {
    pub name: String,
    /// district_id → unit_count
    pub unit_counts: Vec<(u32, u32)>,
    /// When `Some`, overrides `cfg.city.patrol.routes_path` for this run.
    /// e.g. "config/patrol_routes_hotspot.json".
    pub patrol_routes_path: Option<String>,
    /// When `Some`, overrides `cfg.city.sim.mutual_aid_enabled`.
    pub mutual_aid: Option<bool>,
}

impl Variant {
    /// Convenience constructor for the legacy unit-reallocation case.
    pub fn realloc(name: impl Into<String>, unit_counts: Vec<(u32, u32)>) -> Self {
        Self { name: name.into(), unit_counts, patrol_routes_path: None, mutual_aid: None }
    }
}

/// SLA result for one priority level.
#[derive(Clone, Debug, Default)]
pub struct SlaBucket {
    pub total: u64,
    pub met: u64,
    pub avg_rt: f64,
}

impl SlaBucket {
    pub fn pct(&self) -> f64 {
        if self.total == 0 { 0.0 } else { self.met as f64 / self.total as f64 * 100.0 }
    }
}

/// Aggregated results for one variant run.
#[derive(Clone, Debug)]
pub struct VariantResult {
    pub name: String,
    pub total_units: u32,
    pub sla_a: SlaBucket,
    pub sla_b: SlaBucket,
    pub sla_c: SlaBucket,
    /// Per-district SLA A compliance % (district_id → pct)
    pub district_sla_a: BTreeMap<u32, f64>,
}

impl VariantResult {
    /// Overall weighted SLA compliance across all priorities.
    pub fn overall_sla_pct(&self) -> f64 {
        let total = self.sla_a.total + self.sla_b.total + self.sla_c.total;
        if total == 0 { return 0.0; }
        let met = self.sla_a.met + self.sla_b.met + self.sla_c.met;
        met as f64 / total as f64 * 100.0
    }
}

// ---------------------------------------------------------------------------
// Run a single variant
// ---------------------------------------------------------------------------

/// Run a simulation variant with the given unit counts and return SLA results.
/// `config_path` is the path to city.toml.
/// `db_path` is the output SQLite path (will be overwritten).
///
/// This single-shot entry point reloads the routing cache from disk every call.
/// When running many variants in a batch, prefer `run_variant_with_cache` to
/// share one `Arc<LoadedRoutingCache>` across all workers.
pub fn run_variant(
    config_path: &Path,
    variant: &Variant,
    db_path: &str,
    sim_end: Option<u64>,
) -> Result<VariantResult, String> {
    let cfg = load_variant_config(config_path, variant, sim_end)?;
    let total_units = variant.unit_counts.iter().map(|(_, c)| c).sum::<u32>();

    let mut city = City::from_config_with_db(&cfg, db_path);
    run_sim(&mut city, cfg.city.sim.duration_minutes, None);

    extract_sla(db_path, &variant.name, total_units)
        .map_err(|e| format!("SLA extraction: {e}"))
}

/// Run a variant using a preloaded routing cache. Used by the parallel
/// whatif runner — every worker clones the same `Arc` so the routing graph
/// and anchor travel-time table are built exactly once per batch.
///
/// If `progress` is `Some`, the closure is invoked periodically during the
/// sim loop with `(elapsed_min, sim_end_min)`. The whatif runner uses this
/// to print per-variant progress lines analogous to the standard sim's
/// every-10k-events log line.
pub fn run_variant_with_cache(
    config_path: &Path,
    variant:     &Variant,
    db_path:     &str,
    sim_end:     Option<u64>,
    cache:       Arc<LoadedRoutingCache>,
    progress:    Option<&(dyn Fn(u64, u64) + Sync)>,
) -> Result<VariantResult, String> {
    let cfg = load_variant_config(config_path, variant, sim_end)?;
    let total_units = variant.unit_counts.iter().map(|(_, c)| c).sum::<u32>();

    let mut city = City::from_config_with_routing(&cfg, db_path, cache);
    run_sim(&mut city, cfg.city.sim.duration_minutes, progress);

    extract_sla(db_path, &variant.name, total_units)
        .map_err(|e| format!("SLA extraction: {e}"))
}

fn load_variant_config(
    config_path: &Path,
    variant: &Variant,
    sim_end: Option<u64>,
) -> Result<LoadedConfig, String> {
    let mut cfg = LoadedConfig::load(config_path)
        .map_err(|e| format!("config load: {e}"))?;

    for (district_id, unit_count) in &variant.unit_counts {
        if let Some(d) = cfg.city.districts.iter_mut().find(|d| d.id == *district_id) {
            d.unit_count = *unit_count;
        }
    }

    if let Some(end) = sim_end {
        cfg.city.sim.duration_minutes = end;
    }
    cfg.city.sim.record_routes = false;

    if let Some(routes_path) = &variant.patrol_routes_path {
        cfg.city.patrol = Some(crate::config::PatrolConfig {
            routes_path: Some(routes_path.clone()),
        });
    }
    if let Some(enabled) = variant.mutual_aid {
        cfg.city.sim.mutual_aid_enabled = Some(enabled);
    }

    Ok(cfg)
}

fn run_sim(
    city: &mut City,
    sim_end_min: u64,
    progress: Option<&(dyn Fn(u64, u64) + Sync)>,
) {
    // Mirror the standard sim's progress cadence: a callback every ~10k ticks.
    // 10k events is a good interval — frequent enough to feel live, infrequent
    // enough that the println cost is negligible relative to the work.
    const PROGRESS_EVERY: u64 = 10_000;
    let mut tick: u64 = 0;
    loop {
        if city.event_heap.is_empty() || city.clock.elapsed_min >= sim_end_min {
            break;
        }
        city.tick();
        tick += 1;
        if let Some(cb) = progress {
            if tick % PROGRESS_EVERY == 0 {
                cb(city.clock.elapsed_min, sim_end_min);
            }
        }
    }
    if let Some(cb) = progress {
        // Final 100% tick so the consumer always sees a terminal update.
        cb(sim_end_min.min(city.clock.elapsed_min), sim_end_min);
    }
    city.flush();
}

// ---------------------------------------------------------------------------
// SLA extraction
// ---------------------------------------------------------------------------

fn extract_sla(db_path: &str, name: &str, total_units: u32) -> rusqlite::Result<VariantResult> {
    let conn = Connection::open(db_path)?;

    let mut stmt = conn.prepare(
        "SELECT s.priority, a.district, (a.sim_time - s.sim_time) AS rt
         FROM events s
         JOIN events a ON s.incident = a.incident
         WHERE s.kind = 'IncidentSpawned'
           AND a.kind = 'UnitArrived'
           AND s.priority IS NOT NULL",
    )?;

    let rows: Vec<(String, i64, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .filter_map(|r| r.ok())
        .collect();

    let mut sla_a = SlaBucket::default();
    let mut sla_b = SlaBucket::default();
    let mut sla_c = SlaBucket::default();

    // Per-district Priority A tracking
    let mut district_a_total: BTreeMap<u32, u64> = BTreeMap::new();
    let mut district_a_met: BTreeMap<u32, u64> = BTreeMap::new();

    for (prio, district, rt) in &rows {
        let (bucket, target) = match prio.as_str() {
            "A" => (&mut sla_a, 5i64),
            "B" => (&mut sla_b, 15i64),
            "C" => (&mut sla_c, 60i64),
            _ => continue,
        };
        bucket.total += 1;
        if *rt <= target { bucket.met += 1; }
        bucket.avg_rt += *rt as f64;

        if prio == "A" {
            *district_a_total.entry(*district as u32).or_insert(0) += 1;
            if *rt <= 5 {
                *district_a_met.entry(*district as u32).or_insert(0) += 1;
            }
        }
    }

    if sla_a.total > 0 { sla_a.avg_rt /= sla_a.total as f64; }
    if sla_b.total > 0 { sla_b.avg_rt /= sla_b.total as f64; }
    if sla_c.total > 0 { sla_c.avg_rt /= sla_c.total as f64; }

    let district_sla_a: BTreeMap<u32, f64> = district_a_total.iter().map(|(&did, &total)| {
        let met = district_a_met.get(&did).copied().unwrap_or(0);
        let pct = if total == 0 { 0.0 } else { met as f64 / total as f64 * 100.0 };
        (did, pct)
    }).collect();

    Ok(VariantResult {
        name: name.to_string(),
        total_units,
        sla_a,
        sla_b,
        sla_c,
        district_sla_a,
    })
}

// ---------------------------------------------------------------------------
// Variant generation helpers
// ---------------------------------------------------------------------------

/// Generate variants by redistributing units: move ±delta units between
/// each pair of districts, keeping total constant.
pub fn generate_reallocation_variants(
    base_counts: &[(u32, String, u32)], // (district_id, name, unit_count)
    delta: u32,
    max_variants: usize,
) -> Vec<Variant> {
    let mut variants = Vec::new();

    // Baseline variant
    let baseline: Vec<(u32, u32)> = base_counts.iter().map(|(id, _, c)| (*id, *c)).collect();
    variants.push(Variant::realloc("Baseline", baseline.clone()));

    // For each district pair, try transferring `delta` units in each direction
    let n = base_counts.len();
    for i in 0..n {
        for j in (i + 1)..n {
            if variants.len() >= max_variants { break; }

            let (id_i, name_i, count_i) = &base_counts[i];
            let (id_j, name_j, count_j) = &base_counts[j];

            if *count_i > delta {
                let mut v = baseline.clone();
                for (id, c) in v.iter_mut() {
                    if *id == *id_i { *c -= delta; }
                    if *id == *id_j { *c += delta; }
                }
                variants.push(Variant::realloc(
                    format!("{name_i} -{delta} → {name_j} +{delta}"),
                    v,
                ));
            }

            if variants.len() >= max_variants { break; }

            if *count_j > delta {
                let mut v = baseline.clone();
                for (id, c) in v.iter_mut() {
                    if *id == *id_j { *c -= delta; }
                    if *id == *id_i { *c += delta; }
                }
                variants.push(Variant::realloc(
                    format!("{name_j} -{delta} → {name_i} +{delta}"),
                    v,
                ));
            }
        }
        if variants.len() >= max_variants { break; }
    }

    variants
}

// ---------------------------------------------------------------------------
// Patrol / mutual-aid variant generation
// ---------------------------------------------------------------------------

/// Generate variants that exercise the patrol + mutual-aid features. The
/// baseline is the supplied unit counts with no patrols and aid disabled;
/// each subsequent variant turns on a single combination so the resulting
/// table makes the contribution of each feature obvious.
pub fn generate_patrol_variants(
    base_counts:        &[(u32, String, u32)],
    patrol_route_paths: &[(String, String)],   // (label, json path)
) -> Vec<Variant> {
    let baseline_counts: Vec<(u32, u32)> =
        base_counts.iter().map(|(id, _, c)| (*id, *c)).collect();

    let mut out = Vec::new();
    out.push(Variant::realloc("Baseline (no patrol, no aid)", baseline_counts.clone()));
    out.push(Variant {
        name:               "Mutual aid only".to_string(),
        unit_counts:        baseline_counts.clone(),
        patrol_routes_path: None,
        mutual_aid:         Some(true),
    });
    for (label, path) in patrol_route_paths {
        out.push(Variant {
            name:               format!("Patrol: {label}"),
            unit_counts:        baseline_counts.clone(),
            patrol_routes_path: Some(path.clone()),
            mutual_aid:         Some(false),
        });
        out.push(Variant {
            name:               format!("Patrol: {label} + aid"),
            unit_counts:        baseline_counts.clone(),
            patrol_routes_path: Some(path.clone()),
            mutual_aid:         Some(true),
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Print comparison table
// ---------------------------------------------------------------------------

pub fn print_comparison_table(results: &mut [VariantResult]) {
    // Sort by overall SLA descending
    results.sort_by(|a, b| b.overall_sla_pct().partial_cmp(&a.overall_sla_pct()).unwrap());

    println!("\n=== What-If Unit Reallocation — Comparison ===\n");
    println!(
        "  {:<4} {:<40} {:>6} {:>10} {:>10} {:>10} {:>10}",
        "Rank", "Variant", "Units", "SLA-A %", "SLA-B %", "SLA-C %", "Overall %"
    );
    println!("  {}", "-".repeat(94));

    for (rank, r) in results.iter().enumerate() {
        println!(
            "  {:<4} {:<40} {:>6} {:>9.1}% {:>9.1}% {:>9.1}% {:>9.1}%",
            rank + 1,
            r.name,
            r.total_units,
            r.sla_a.pct(),
            r.sla_b.pct(),
            r.sla_c.pct(),
            r.overall_sla_pct(),
        );
    }

    println!("\n  Detail — Priority A avg response time (minutes):");
    println!(
        "  {:<4} {:<40} {:>10}",
        "Rank", "Variant", "Avg RT (A)"
    );
    println!("  {}", "-".repeat(58));
    for (rank, r) in results.iter().enumerate() {
        println!(
            "  {:<4} {:<40} {:>10.1}",
            rank + 1,
            r.name,
            r.sla_a.avg_rt,
        );
    }
}
