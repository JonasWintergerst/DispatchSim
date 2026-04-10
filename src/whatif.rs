// whatif.rs
// What-if simulation framework: a `SimBatch` owns shared blueprint state
// (routing cache, pre-built per-district routing engines, base config) and
// runs a `Vec<Variant>` through one common event loop. The standard sim is
// just a `SimBatch` with a single identity variant; the whatif unit
// reallocation and patrol-strategy comparisons are SimBatches with many
// variants. There is exactly one event loop in the codebase, so progress
// reporting, mutual-aid handling, and shared-engine reuse all live in one
// place.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use rayon::prelude::*;
use rusqlite::Connection;

use crate::city::{self, City};
use crate::config::LoadedConfig;
use crate::routing::RoutingEngine;
use crate::routing_cache::{self, LoadedRoutingCache};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A single what-if variant: a label plus optional per-district unit counts
/// and optional patrol/mutual-aid overrides. `None` for any field means
/// "use the value from the base `LoadedConfig` as-is".
#[derive(Clone, Debug)]
pub struct Variant {
    pub name: String,
    /// When `Some`, overrides per-district `unit_count`. `None` leaves the
    /// counts from `cfg` untouched (used by the standard-sim batch of one).
    pub unit_counts: Option<Vec<(u32, u32)>>,
    /// When `Some`, overrides `cfg.city.patrol.routes_path` for this run.
    /// e.g. "config/patrol_routes_hotspot.json".
    pub patrol_routes_path: Option<String>,
    /// When `Some`, overrides `cfg.city.sim.mutual_aid_enabled`.
    pub mutual_aid: Option<bool>,
}

impl Variant {
    /// Identity variant — no overrides, runs `cfg` exactly as loaded.
    /// This is what the standard sim uses when wrapped in a `SimBatch`.
    pub fn identity(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            unit_counts: None,
            patrol_routes_path: None,
            mutual_aid: None,
        }
    }

    /// Convenience constructor for the unit-reallocation case.
    pub fn realloc(name: impl Into<String>, unit_counts: Vec<(u32, u32)>) -> Self {
        Self {
            name: name.into(),
            unit_counts: Some(unit_counts),
            patrol_routes_path: None,
            mutual_aid: None,
        }
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
// SimBatch — the canonical sim runner
// ---------------------------------------------------------------------------

/// A batch of one or more sim variants that share immutable blueprint state.
/// The standard sim is a SimBatch with a single `Variant::identity` entry;
/// the whatif runners build SimBatches with many variants.
///
/// All shared work (loading the routing cache, building per-district
/// `RoutingEngine`s) happens once during construction. Each variant in the
/// batch then constructs its own `City` inside a rayon worker, runs the
/// event loop, drains its SLA results, and drops the city.
pub struct SimBatch {
    cfg:                   LoadedConfig,
    cache:                 Arc<LoadedRoutingCache>,
    engine:                Arc<RoutingEngine>,
    variants:              Vec<Variant>,
    /// Output SQLite path. Use `{}` as a placeholder for the variant index.
    /// Single-variant batches may omit it.
    db_pattern:            String,
    /// Optional override for `cfg.city.sim.duration_minutes`.
    sim_duration_override: Option<u64>,
    /// Optional override for `cfg.city.sim.record_routes`. The standard sim
    /// honours the cfg value (None); whatif batches force false to avoid
    /// per-variant route logging overhead.
    record_routes_override: Option<bool>,
}

impl SimBatch {
    /// Build a SimBatch from a loaded config. Loads the routing cache and
    /// pre-builds per-district `RoutingEngine`s — both shared across every
    /// variant in this batch via `Arc`.
    ///
    /// The batch starts with a single `Variant::identity("Default")` and the
    /// standard `./output/dispatch_sim.db` output path; call `with_variants`
    /// and `with_db_pattern` to customise.
    pub fn from_config(cfg: LoadedConfig) -> Result<Self, String> {
        let cache_path = cfg.city.sim.routing_cache_path.as_deref().ok_or_else(|| {
            "sim.routing_cache_path is not set in city.toml — add e.g. \
             `routing_cache_path = \"output/routing_cache.bin\"` and run \
             `cargo run --bin optimize` to produce it".to_string()
        })?;
        let cache = Arc::new(routing_cache::load(cache_path).ok_or_else(|| {
            format!("routing cache not found at {cache_path} — run `cargo run --bin optimize` first")
        })?);
        let engine = Arc::new(city::build_routing_engine(&cache));

        Ok(Self {
            cfg,
            cache,
            engine,
            variants:               vec![Variant::identity("Default")],
            db_pattern:             "./output/dispatch_sim.db".to_string(),
            sim_duration_override:  None,
            record_routes_override: None,
        })
    }

    pub fn with_variants(mut self, variants: Vec<Variant>) -> Self {
        self.variants = variants;
        self
    }

    pub fn with_db_pattern(mut self, pat: impl Into<String>) -> Self {
        self.db_pattern = pat.into();
        self
    }

    pub fn with_duration(mut self, dur: u64) -> Self {
        self.sim_duration_override = Some(dur);
        self
    }

    pub fn with_record_routes(mut self, rr: bool) -> Self {
        self.record_routes_override = Some(rr);
        self
    }

    pub fn variants(&self) -> &[Variant] { &self.variants }

    /// Compute the per-variant DB path. If the pattern contains `{}` it is
    /// replaced with the variant index; otherwise the pattern is returned
    /// as-is (useful for single-variant batches).
    fn db_path_for(&self, idx: usize) -> String {
        if self.db_pattern.contains("{}") {
            self.db_pattern.replace("{}", &idx.to_string())
        } else {
            self.db_pattern.clone()
        }
    }

    /// Run every variant in parallel (rayon par_iter). Returns one result per
    /// variant, in input order. Per-variant progress and completion lines are
    /// printed to stdout in the same format as the legacy whatif runners.
    pub fn run(&self) -> Vec<Result<VariantResult, String>> {
        let n = self.variants.len();

        let mut indexed: Vec<(usize, Result<VariantResult, String>)> = self.variants
            .par_iter()
            .enumerate()
            .map(|(i, variant)| {
                let db_path = self.db_path_for(i);
                let started = Instant::now();
                let label   = variant.name.clone();

                let progress = move |elapsed: u64, sim_end: u64| {
                    let pct = if sim_end == 0 { 0.0 } else { (elapsed as f64 / sim_end as f64) * 100.0 };
                    println!("  [{:>3}/{n}] {:<40} sim {:>5.1}%", i + 1, label, pct);
                };

                let res = self.run_one(variant, &db_path, Some(&progress));

                match &res {
                    Ok(r)  => println!("  [{:>3}/{n}] {:<40} done ({}s) — SLA overall: {:.1}%",
                                       i + 1, variant.name, started.elapsed().as_secs(), r.overall_sla_pct()),
                    Err(e) => println!("  [{:>3}/{n}] {:<40} FAILED: {e}",
                                       i + 1, variant.name),
                }
                (i, res)
            })
            .collect();

        indexed.sort_by_key(|(i, _)| *i);
        indexed.into_iter().map(|(_, r)| r).collect()
    }

    /// Build the per-variant `LoadedConfig`, instantiate a `City`, run the
    /// event loop, drain SLA results.
    fn run_one(
        &self,
        variant:  &Variant,
        db_path:  &str,
        progress: Option<&(dyn Fn(u64, u64) + Sync)>,
    ) -> Result<VariantResult, String> {
        let cfg = self.materialise_variant_config(variant);

        let total_units = cfg.city.districts.iter().map(|d| d.unit_count).sum::<u32>();

        let mut city = City::from_config_with_engine(
            &cfg,
            db_path,
            Arc::clone(&self.cache),
            Arc::clone(&self.engine),
        );
        run_event_loop(&mut city, cfg.city.sim.duration_minutes, progress);

        extract_sla(db_path, &variant.name, total_units)
            .map_err(|e| format!("SLA extraction: {e}"))
    }

    /// Apply this batch's overrides plus the variant's overrides on top of a
    /// fresh clone of the base config.
    fn materialise_variant_config(&self, variant: &Variant) -> LoadedConfig {
        let mut cfg = self.cfg.clone();

        if let Some(counts) = &variant.unit_counts {
            for (district_id, unit_count) in counts {
                if let Some(d) = cfg.city.districts.iter_mut().find(|d| d.id == *district_id) {
                    d.unit_count = *unit_count;
                }
            }
        }

        if let Some(end) = self.sim_duration_override {
            cfg.city.sim.duration_minutes = end;
        }

        if let Some(rr) = self.record_routes_override {
            cfg.city.sim.record_routes = rr;
        }

        if let Some(routes_path) = &variant.patrol_routes_path {
            cfg.city.patrol = Some(crate::config::PatrolConfig {
                routes_path: Some(routes_path.clone()),
            });
        }
        if let Some(enabled) = variant.mutual_aid {
            cfg.city.sim.mutual_aid_enabled = Some(enabled);
        }

        cfg
    }
}

/// The single canonical event loop. Used by `SimBatch::run_one` and nothing
/// else. `progress` is invoked every ~10k ticks plus once at the end so the
/// consumer always sees a terminal update.
fn run_event_loop(
    city: &mut City,
    sim_end_min: u64,
    progress: Option<&(dyn Fn(u64, u64) + Sync)>,
) {
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
        unit_counts:        Some(baseline_counts.clone()),
        patrol_routes_path: None,
        mutual_aid:         Some(true),
    });
    for (label, path) in patrol_route_paths {
        out.push(Variant {
            name:               format!("Patrol: {label}"),
            unit_counts:        Some(baseline_counts.clone()),
            patrol_routes_path: Some(path.clone()),
            mutual_aid:         Some(false),
        });
        out.push(Variant {
            name:               format!("Patrol: {label} + aid"),
            unit_counts:        Some(baseline_counts.clone()),
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
