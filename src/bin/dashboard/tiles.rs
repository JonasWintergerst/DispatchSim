use image::{GenericImage, RgbaImage};
use std::io::Read;
use std::path::Path;

const TILE_SIZE: u32 = 256;
const TILE_URL: &str = "https://tile.openstreetmap.org";
const CACHE_DIR: &str = "config/.tile_cache";
const ZOOM: u32 = 12;

/// Stitched tile map with geographic bounds.
pub struct TileMap {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Geographic bounds of the full stitched image (may be slightly larger
    /// than the requested bounds because tiles are discrete).
    pub lat_min: f64,
    pub lat_max: f64,
    pub lon_min: f64,
    pub lon_max: f64,
}

// ── Tile math (Web-Mercator) ────────────────────────────────────────────────

fn lon_to_tile_x(lon: f64, z: u32) -> f64 {
    ((lon + 180.0) / 360.0) * (1u64 << z) as f64
}

fn lat_to_tile_y(lat: f64, z: u32) -> f64 {
    let r = lat.to_radians();
    (1.0 - r.tan().asinh() / std::f64::consts::PI) / 2.0 * (1u64 << z) as f64
}

fn tile_x_to_lon(x: f64, z: u32) -> f64 {
    x / (1u64 << z) as f64 * 360.0 - 180.0
}

fn tile_y_to_lat(y: f64, z: u32) -> f64 {
    let n = std::f64::consts::PI * (1.0 - 2.0 * y / (1u64 << z) as f64);
    n.sinh().atan().to_degrees()
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Fetch OSM tiles covering the given bounds, stitch them, darken for the
/// dark UI theme, and return the result. Tiles are cached to disk under
/// `config/.tile_cache/` so subsequent runs are instant.
pub fn fetch_tile_map(
    lat_min: f64,
    lat_max: f64,
    lon_min: f64,
    lon_max: f64,
) -> Option<TileMap> {
    // Add a small padding so the map extends slightly beyond the hex bounds.
    let pad_lat = (lat_max - lat_min) * 0.05;
    let pad_lon = (lon_max - lon_min) * 0.05;
    let lat_lo = lat_min - pad_lat;
    let lat_hi = lat_max + pad_lat;
    let lon_lo = lon_min - pad_lon;
    let lon_hi = lon_max + pad_lon;

    let x_min = lon_to_tile_x(lon_lo, ZOOM).floor() as u32;
    let x_max = lon_to_tile_x(lon_hi, ZOOM).floor() as u32;
    let y_min = lat_to_tile_y(lat_hi, ZOOM).floor() as u32; // higher lat → smaller y
    let y_max = lat_to_tile_y(lat_lo, ZOOM).floor() as u32;

    let cols = x_max - x_min + 1;
    let rows = y_max - y_min + 1;

    let _ = std::fs::create_dir_all(CACHE_DIR);

    let mut canvas = RgbaImage::new(cols * TILE_SIZE, rows * TILE_SIZE);

    for ty in y_min..=y_max {
        for tx in x_min..=x_max {
            let tile_img = load_tile(tx, ty)?;
            let px = (tx - x_min) * TILE_SIZE;
            let py = (ty - y_min) * TILE_SIZE;
            canvas.copy_from(&tile_img, px, py).ok()?;
        }
    }

    darken(&mut canvas);

    Some(TileMap {
        width: canvas.width(),
        height: canvas.height(),
        rgba: canvas.into_raw(),
        lon_min: tile_x_to_lon(x_min as f64, ZOOM),
        lon_max: tile_x_to_lon((x_max + 1) as f64, ZOOM),
        lat_max: tile_y_to_lat(y_min as f64, ZOOM),
        lat_min: tile_y_to_lat((y_max + 1) as f64, ZOOM),
    })
}

// ── Internals ───────────────────────────────────────────────────────────────

fn load_tile(x: u32, y: u32) -> Option<RgbaImage> {
    let cache = format!("{CACHE_DIR}/{ZOOM}_{x}_{y}.png");

    if Path::new(&cache).exists() {
        return image::open(&cache).ok().map(|i| i.to_rgba8());
    }

    let url = format!("{TILE_URL}/{ZOOM}/{x}/{y}.png");
    let resp = ureq::get(&url)
        .set("User-Agent", "DispatchSim-Dashboard/0.1 (student project)")
        .call()
        .ok()?;

    let mut bytes = Vec::new();
    resp.into_reader().read_to_end(&mut bytes).ok()?;
    let _ = std::fs::write(&cache, &bytes);

    image::load_from_memory(&bytes).ok().map(|i| i.to_rgba8())
}

/// Darken tile colours so they read as a subtle background behind hex overlays.
fn darken(img: &mut RgbaImage) {
    for p in img.pixels_mut() {
        p[0] = (p[0] as f32 * 0.35) as u8;
        p[1] = (p[1] as f32 * 0.35) as u8;
        p[2] = (p[2] as f32 * 0.40) as u8; // slight cool tint
    }
}
