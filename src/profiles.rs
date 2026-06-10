// profiles.rs
//
// Classify H3 hex cells into spawn profiles (residential / mixed / commercial)
// from real land-use signal in an OSM PBF: the density of commercial POIs
// (shops, food/nightlife amenities, offices, tourism) whose coordinates fall
// inside a cell, smoothed over the immediate H3 neighbour ring so zones come
// out contiguous rather than speckled.
//
// Shared by the `optimize` and `assign_profiles` binaries so a freshly
// optimised hexes.json always carries real profiles instead of the optimizer's
// blanket "residential" tag.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use h3o::{CellIndex, LatLng, Resolution};
use osmpbf::{Element, ElementReader};

/// Smoothed commercial-POI score thresholds: below MIXED a cell is
/// residential; below COMM it is mixed; at or above COMM it is commercial.
/// Defaults tuned to a realistic Hamburg split (~68 % / 21 % / 11 %).
pub const MIXED_THRESH: f64 = 2.5;
pub const COMM_THRESH: f64 = 18.0;

/// Commercial-activity weight for one OSM node's tags. 0.0 = not commercial.
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

/// Smoothed commercial-POI score per cell (self + 0.5 × neighbour ring),
/// computed from the OSM PBF at `osm_path`. Only cells present in `indices`
/// are scored. Resolution is taken from the first index (all hexes share one).
pub fn commercial_scores(
    indices: &[u64],
    osm_path: &Path,
) -> Result<HashMap<u64, f64>, Box<dyn std::error::Error>> {
    let want: HashSet<u64> = indices.iter().copied().collect();
    let resolution = indices
        .first()
        .and_then(|&i| CellIndex::try_from(i).ok())
        .map(|c| c.resolution())
        .unwrap_or(Resolution::Nine);

    // Tally raw commercial POIs per cell.
    let mut raw: HashMap<u64, f64> = HashMap::new();
    let reader = ElementReader::from_path(osm_path)?;
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
        let cell: u64 = ll.to_cell(resolution).into();
        if want.contains(&cell) {
            *raw.entry(cell).or_insert(0.0) += w;
        }
    })?;

    // Smooth over the immediate neighbour ring.
    let mut out = HashMap::with_capacity(indices.len());
    for &idx in indices {
        let mut s = raw.get(&idx).copied().unwrap_or(0.0);
        if let Ok(cell) = CellIndex::try_from(idx) {
            for nb in cell.grid_disk::<Vec<_>>(1) {
                let nb: u64 = nb.into();
                if nb != idx {
                    s += 0.5 * raw.get(&nb).copied().unwrap_or(0.0);
                }
            }
        }
        out.insert(idx, s);
    }
    Ok(out)
}

/// Map a smoothed commercial score to a spawn-profile id.
pub fn classify(score: f64, mixed_thresh: f64, comm_thresh: f64) -> &'static str {
    if score < mixed_thresh {
        "residential"
    } else if score < comm_thresh {
        "mixed"
    } else {
        "commercial"
    }
}

/// Classify each H3 cell in `indices` into a spawn-profile id using OSM
/// commercial-POI density with the default Hamburg thresholds.
pub fn classify_hexes(
    indices: &[u64],
    osm_path: &Path,
) -> Result<HashMap<u64, String>, Box<dyn std::error::Error>> {
    let scores = commercial_scores(indices, osm_path)?;
    Ok(indices
        .iter()
        .map(|&i| {
            let s = scores.get(&i).copied().unwrap_or(0.0);
            (i, classify(s, MIXED_THRESH, COMM_THRESH).to_string())
        })
        .collect())
}
