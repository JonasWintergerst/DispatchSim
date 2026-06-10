// assign_profiles.rs
//
// Post-process config/hexes.json: assign each H3 cell a `spawn_profile_id`
// (residential / mixed / commercial) from real Hamburg land-use signal in the
// OSM PBF, instead of the optimizer's blanket "residential".
//
// Signal = density of commercial POIs (shops, food/nightlife amenities,
// offices, tourism) whose coordinates fall inside the cell, smoothed over the
// immediate H3 neighbour ring so zones come out contiguous rather than
// speckled. Cells are then split by smoothed score:
//   score == 0            -> residential   (no commercial activity)
//   0 < score < COMM      -> mixed
//   score >= COMM         -> commercial
//
// Usage:
//   cargo run --release --bin assign_profiles                        # stats only (dry run)
//   cargo run --release --bin assign_profiles -- write [MIXED] [COMM] # write hexes.json
//
//   score <  MIXED -> residential   score <  COMM -> mixed   else commercial
// MIXED defaults to 2.5, COMM to 18.0 (smoothed commercial-POI score),
// yielding a realistic Hamburg split (~65 % residential / 24 % mixed / 11 % commercial).

use std::collections::HashMap;
use std::path::Path;

use h3o::{CellIndex, LatLng, Resolution};
use osmpbf::{Element, ElementReader};

const HEXES_PATH: &str = "config/hexes.json";
const OSM_PATH: &str = "config/hamburg-260331.osm.pbf";

/// Commercial-activity weight for an OSM node's tags. 0.0 = not commercial.
fn commercial_weight<'a>(tags: impl Iterator<Item = (&'a str, &'a str)>) -> f64 {
    let mut w = 0.0;
    for (k, v) in tags {
        match k {
            "shop" if v != "no" => w += 1.0,
            "office" if v != "no" => w += 1.0,
            "amenity" => match v {
                "restaurant" | "bar" | "cafe" | "pub" | "fast_food" | "nightclub"
                | "food_court" | "biergarten" | "bank" | "cinema" | "theatre"
                | "marketplace" | "casino" | "bureau_de_change" | "pharmacy"
                | "fuel" | "car_rental" | "ice_cream" => w += 1.0,
                _ => {}
            },
            "tourism" => match v {
                "hotel" | "hostel" | "guest_house" | "motel" | "museum" | "gallery"
                | "attraction" => w += 0.5,
                _ => {}
            },
            _ => {}
        }
    }
    w
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let write = args.get(1).map(|s| s == "write").unwrap_or(false);
    let mixed_thresh: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(2.5);
    let comm_thresh: f64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(18.0);

    // 1. Load hexes.json (preserve every field; only mutate spawn_profile_id).
    let raw = std::fs::read_to_string(HEXES_PATH)?;
    let mut hexes: Vec<serde_json::Value> = serde_json::from_str(&raw)?;

    // h3_index -> position in `hexes`
    let mut pos: HashMap<u64, usize> = HashMap::with_capacity(hexes.len());
    for (i, h) in hexes.iter().enumerate() {
        let idx = h["h3_index"].as_u64().expect("h3_index missing");
        pos.insert(idx, i);
    }

    // 2. Tally commercial POIs per hex cell from the OSM PBF.
    let mut raw_score: HashMap<u64, f64> = HashMap::new();
    let reader = ElementReader::from_path(Path::new(OSM_PATH))?;
    let mut poi_count = 0u64;
    reader.for_each(|element| {
        let (lat, lon, w) = match element {
            Element::Node(n) => (n.lat(), n.lon(), commercial_weight(n.tags())),
            Element::DenseNode(n) => (n.lat(), n.lon(), commercial_weight(n.tags())),
            _ => return,
        };
        if w <= 0.0 {
            return;
        }
        let Ok(ll) = LatLng::new(lat, lon) else { return };
        let cell: u64 = ll.to_cell(Resolution::Nine).into();
        if pos.contains_key(&cell) {
            *raw_score.entry(cell).or_insert(0.0) += w;
            poi_count += 1;
        }
    })?;

    // 3. Smooth over the immediate neighbour ring (self + 0.5 * neighbours).
    let mut score: HashMap<u64, f64> = HashMap::with_capacity(hexes.len());
    for (&idx, _) in pos.iter() {
        let mut s = *raw_score.get(&idx).unwrap_or(&0.0);
        if let Ok(cell) = CellIndex::try_from(idx) {
            for nb in cell.grid_disk::<Vec<_>>(1) {
                let nb: u64 = nb.into();
                if nb != idx {
                    s += 0.5 * raw_score.get(&nb).copied().unwrap_or(0.0);
                }
            }
        }
        score.insert(idx, s);
    }

    // 4. Classify.
    let classify = |s: f64| -> &'static str {
        if s < mixed_thresh {
            "residential"
        } else if s < comm_thresh {
            "mixed"
        } else {
            "commercial"
        }
    };

    let mut counts: HashMap<&str, usize> = HashMap::new();
    for h in &hexes {
        let idx = h["h3_index"].as_u64().unwrap();
        *counts.entry(classify(score[&idx])).or_insert(0) += 1;
    }

    // Stats.
    let n = hexes.len();
    let mut sorted: Vec<f64> = score.values().copied().collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |p: f64| sorted[((n as f64 * p) as usize).min(n - 1)];
    println!("commercial POIs matched to hexes: {poi_count}");
    println!("smoothed score percentiles: p50={:.1} p75={:.1} p90={:.1} p95={:.1} p99={:.1} max={:.1}",
             pct(0.50), pct(0.75), pct(0.90), pct(0.95), pct(0.99), sorted[n - 1]);
    println!("thresholds MIXED={mixed_thresh:.1} COMM={comm_thresh:.1} → split:");
    for k in ["residential", "mixed", "commercial"] {
        let c = counts.get(k).copied().unwrap_or(0);
        println!("  {k:<12} {c:5}  ({:4.1}%)", 100.0 * c as f64 / n as f64);
    }

    // 5. Write back.
    if write {
        for h in &mut hexes {
            let idx = h["h3_index"].as_u64().unwrap();
            h["spawn_profile_id"] = serde_json::Value::String(classify(score[&idx]).to_string());
        }
        let out = serde_json::to_string(&hexes)?;
        std::fs::write(HEXES_PATH, out)?;
        println!("→ wrote {} hexes to {HEXES_PATH}", hexes.len());
    } else {
        println!("(dry run — pass `write` to update {HEXES_PATH})");
    }

    Ok(())
}
