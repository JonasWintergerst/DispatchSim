// whatif.rs
// What-if unit reallocation: run N sim variants with different unit_count
// assignments across districts, collect SLA compliance per variant, and
// output a ranked comparison table.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::Connection;

use crate::city::City;
use crate::config::LoadedConfig;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A single what-if variant: a label plus per-district unit counts.
#[derive(Clone, Debug)]
pub struct Variant {
    pub name: String,
    /// district_id → unit_count
    pub unit_counts: Vec<(u32, u32)>,
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
pub fn run_variant(
    config_path: &Path,
    variant: &Variant,
    db_path: &str,
    sim_end: Option<u64>,
) -> Result<VariantResult, String> {
    // Load a fresh config and apply unit count overrides.
    let mut cfg = LoadedConfig::load(config_path)
        .map_err(|e| format!("config load: {e}"))?;

    let mut total_units = 0u32;
    for (district_id, unit_count) in &variant.unit_counts {
        if let Some(d) = cfg.city.districts.iter_mut().find(|d| d.id == *district_id) {
            d.unit_count = *unit_count;
        }
        total_units += unit_count;
    }

    // Optionally shorten the sim for faster iteration.
    if let Some(end) = sim_end {
        cfg.city.sim.duration_minutes = end;
    }

    // Disable route recording for speed.
    cfg.city.sim.record_routes = false;

    // Build and run the simulation.
    let mut city = City::from_config_with_db(&cfg, db_path);
    let sim_end_min = cfg.city.sim.duration_minutes;

    loop {
        if city.event_heap.is_empty() || city.clock.elapsed_min >= sim_end_min {
            break;
        }
        city.tick();
    }
    city.flush();

    // Extract SLA metrics from the output database.
    let result = extract_sla(db_path, &variant.name, total_units)
        .map_err(|e| format!("SLA extraction: {e}"))?;

    Ok(result)
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
    variants.push(Variant {
        name: "Baseline".into(),
        unit_counts: baseline.clone(),
    });

    // For each district pair, try transferring `delta` units in each direction
    let n = base_counts.len();
    for i in 0..n {
        for j in (i + 1)..n {
            if variants.len() >= max_variants { break; }

            let (id_i, name_i, count_i) = &base_counts[i];
            let (id_j, name_j, count_j) = &base_counts[j];

            // Transfer delta from i→j (if i has enough)
            if *count_i > delta {
                let mut v = baseline.clone();
                for (id, c) in v.iter_mut() {
                    if *id == *id_i { *c -= delta; }
                    if *id == *id_j { *c += delta; }
                }
                variants.push(Variant {
                    name: format!("{name_i} -{delta} → {name_j} +{delta}"),
                    unit_counts: v,
                });
            }

            if variants.len() >= max_variants { break; }

            // Transfer delta from j→i
            if *count_j > delta {
                let mut v = baseline.clone();
                for (id, c) in v.iter_mut() {
                    if *id == *id_j { *c -= delta; }
                    if *id == *id_i { *c += delta; }
                }
                variants.push(Variant {
                    name: format!("{name_j} -{delta} → {name_i} +{delta}"),
                    unit_counts: v,
                });
            }
        }
        if variants.len() >= max_variants { break; }
    }

    variants
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
