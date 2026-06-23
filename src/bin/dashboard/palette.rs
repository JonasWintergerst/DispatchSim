use egui::Color32;

/// Number of distinct hues spread evenly around the color wheel before the
/// pattern repeats. Matches the configured district count; ids beyond it still
/// stay distinct via a small per-cycle hue offset.
const HUES: u32 = 24;

/// Distinct color for a district.
///
/// Hues are spread evenly around the wheel and the saturation/value is stepped
/// in three bands keyed off the id, so neighbouring districts differ in both
/// hue and brightness. The old fixed 16-color list wrapped and repeated once
/// districts > 16, and its `saturating_sub(1)` mapped districts 0 and 1 to the
/// same color; this generator avoids both. `district_id` is 0-based (as stored
/// in `hexes.json`).
pub fn district_color(district_id: u32) -> Color32 {
    let cycle = district_id / HUES;
    let hue = ((district_id % HUES) as f32 * (360.0 / HUES as f32) + cycle as f32 * 7.0)
        .rem_euclid(360.0);
    let (sat, val) = match district_id % 3 {
        0 => (0.90, 0.95),
        1 => (0.60, 0.95),
        _ => (0.90, 0.65),
    };
    let (r, g, b) = hsv_to_rgb(hue, sat, val);
    Color32::from_rgb(r, g, b)
}

/// HSV (h in [0,360), s/v in [0,1]) → 8-bit RGB.
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (
        ((r + m) * 255.0).round() as u8,
        ((g + m) * 255.0).round() as u8,
        ((b + m) * 255.0).round() as u8,
    )
}
