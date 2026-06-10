// assign_profiles.rs
//
// Post-process config/hexes.json: assign each H3 cell a `spawn_profile_id`
// (residential / mixed / commercial) from real Hamburg land-use signal in the
// OSM PBF, instead of the optimizer's blanket "residential".
//
// The optimizer (`optimize` binary) now runs this same classification
// automatically, so this binary is only needed to re-tag an existing
// hexes.json or to experiment with the classification thresholds.
//
// Usage:
//   cargo run --release --bin assign_profiles                         # stats only (dry run)
//   cargo run --release --bin assign_profiles -- write [MIXED] [COMM] # write hexes.json
//
//   score <  MIXED -> residential   score <  COMM -> mixed   else commercial

use std::path::Path;

use dispatch_sim::profiles::{self, COMM_THRESH, MIXED_THRESH};

const HEXES_PATH: &str = "config/hexes.json";
const OSM_PATH: &str = "config/hamburg-260331.osm.pbf";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let write = args.get(1).map(|s| s == "write").unwrap_or(false);
    let mixed_thresh: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(MIXED_THRESH);
    let comm_thresh: f64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(COMM_THRESH);

    // 1. Load hexes.json (preserve every field; only mutate spawn_profile_id).
    let raw = std::fs::read_to_string(HEXES_PATH)?;
    let mut hexes: Vec<serde_json::Value> = serde_json::from_str(&raw)?;
    let indices: Vec<u64> = hexes
        .iter()
        .map(|h| h["h3_index"].as_u64().expect("h3_index missing"))
        .collect();

    // 2. Commercial-POI score per cell from the OSM PBF.
    let scores = profiles::commercial_scores(&indices, Path::new(OSM_PATH))?;

    // Stats.
    let n = indices.len();
    let mut sorted: Vec<f64> = indices.iter().map(|i| scores.get(i).copied().unwrap_or(0.0)).collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |p: f64| sorted[((n as f64 * p) as usize).min(n - 1)];
    let mut counts = [0usize; 3];
    for i in &indices {
        let s = scores.get(i).copied().unwrap_or(0.0);
        let slot = match profiles::classify(s, mixed_thresh, comm_thresh) {
            "residential" => 0,
            "mixed" => 1,
            _ => 2,
        };
        counts[slot] += 1;
    }
    println!("smoothed score percentiles: p50={:.1} p75={:.1} p90={:.1} p95={:.1} max={:.1}",
             pct(0.50), pct(0.75), pct(0.90), pct(0.95), sorted[n - 1]);
    println!("thresholds MIXED={mixed_thresh:.1} COMM={comm_thresh:.1} → split:");
    for (k, c) in ["residential", "mixed", "commercial"].iter().zip(counts) {
        println!("  {k:<12} {c:5}  ({:4.1}%)", 100.0 * c as f64 / n as f64);
    }

    // 3. Write back.
    if write {
        for h in &mut hexes {
            let idx = h["h3_index"].as_u64().unwrap();
            let s = scores.get(&idx).copied().unwrap_or(0.0);
            let p = profiles::classify(s, mixed_thresh, comm_thresh);
            h["spawn_profile_id"] = serde_json::Value::String(p.to_string());
        }
        std::fs::write(HEXES_PATH, serde_json::to_string(&hexes)?)?;
        println!("→ wrote {} hexes to {HEXES_PATH}", hexes.len());
    } else {
        println!("(dry run — pass `write` to update {HEXES_PATH})");
    }

    Ok(())
}
