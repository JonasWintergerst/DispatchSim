// optimizer/h3_grid.rs
// Generate H3 cells covering a GeoJSON polygon at a given resolution.
// Uses BFS from the polygon centroid to enumerate all cells whose centers
// fall inside the polygon. No h3o "geo" feature required.

use std::collections::{HashSet, VecDeque};

use geo::{BoundingRect, Contains, Coord, LineString, Point, Polygon};
use h3o::{CellIndex, LatLng, Resolution};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Parse a GeoJSON file that contains a Feature (or FeatureCollection) with a
/// Polygon geometry. Returns a `geo::Polygon<f64>` with (x=lon, y=lat) coords.
pub fn load_polygon(path: &std::path::Path) -> Result<Polygon<f64>, Box<dyn std::error::Error>> {
    let raw = std::fs::read_to_string(path)?;
    let v: serde_json::Value = serde_json::from_str(&raw)?;

    // Support both Feature and FeatureCollection (use first feature).
    let geometry = match v["type"].as_str() {
        Some("Feature")           => v["geometry"].clone(),
        Some("FeatureCollection") => v["features"][0]["geometry"].clone(),
        _                         => v["geometry"].clone(), // bare geometry
    };

    let ring = geometry["coordinates"][0]
        .as_array()
        .ok_or("expected polygon coordinates[0] to be an array")?;

    let coords: Vec<Coord<f64>> = ring.iter()
        .filter_map(|c| {
            let lon = c.get(0)?.as_f64()?;
            let lat = c.get(1)?.as_f64()?;
            Some(Coord { x: lon, y: lat })
        })
        .collect();

    if coords.len() < 3 {
        return Err("polygon must have at least 3 coordinate pairs".into());
    }

    Ok(Polygon::new(LineString::from(coords), vec![]))
}

/// Enumerate every H3 cell at `resolution` whose geographic centre is inside
/// `polygon`.  Returns `(CellIndex, lat_deg, lon_deg)` for each cell found.
///
/// Algorithm: BFS from the polygon's centroid cell.  Neighbours are queued
/// whenever they lie within the polygon's bounding box (with a small buffer),
/// so that concave regions are covered even if intermediate cells are outside.
pub fn cells_for_polygon(
    polygon: &Polygon<f64>,
    resolution: Resolution,
) -> Vec<(CellIndex, f64, f64)> {
    let bbox = polygon.bounding_rect().expect("non-empty polygon");

    let centroid_lon = (bbox.min().x + bbox.max().x) / 2.0;
    let centroid_lat = (bbox.min().y + bbox.max().y) / 2.0;

    let start_cell = LatLng::new(centroid_lat, centroid_lon)
        .expect("centroid has valid lat/lon")
        .to_cell(resolution);

    let mut seen: HashSet<CellIndex> = HashSet::new();
    let mut queue: VecDeque<CellIndex> = VecDeque::new();
    let mut result: Vec<(CellIndex, f64, f64)> = Vec::new();

    seen.insert(start_cell);
    queue.push_back(start_cell);

    // Buffer in degrees so we explore cells whose centres are just outside the
    // bounding box but whose area overlaps the polygon boundary.
    const BUF: f64 = 0.02;

    while let Some(cell) = queue.pop_front() {
        let center = h3o::LatLng::from(cell);
        let lat    = center.lat();
        let lon    = center.lng();

        // Prune cells outside the buffered bounding box.
        if lat < bbox.min().y - BUF || lat > bbox.max().y + BUF
            || lon < bbox.min().x - BUF || lon > bbox.max().x + BUF
        {
            continue;
        }

        if polygon.contains(&Point::new(lon, lat)) {
            result.push((cell, lat, lon));
        }

        // Always expand neighbours to handle concave regions.
        let neighbors: Vec<CellIndex> = cell.grid_disk::<Vec<_>>(1);
        for nbr in neighbors {
            if seen.insert(nbr) {
                queue.push_back(nbr);
            }
        }
    }

    result
}
