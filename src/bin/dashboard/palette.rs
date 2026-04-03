use egui::Color32;

pub const PALETTE: &[Color32] = &[
    Color32::from_rgb(228, 26, 28),
    Color32::from_rgb(55, 126, 184),
    Color32::from_rgb(77, 175, 74),
    Color32::from_rgb(152, 78, 163),
    Color32::from_rgb(255, 127, 0),
    Color32::from_rgb(166, 86, 40),
    Color32::from_rgb(247, 129, 191),
    Color32::from_rgb(153, 153, 153),
    Color32::from_rgb(255, 255, 51),
    Color32::from_rgb(0, 190, 190),
    Color32::from_rgb(190, 0, 190),
    Color32::from_rgb(0, 128, 0),
    Color32::from_rgb(210, 105, 30),
    Color32::from_rgb(70, 130, 180),
    Color32::from_rgb(255, 69, 0),
    Color32::from_rgb(34, 139, 34),
];

pub fn district_color(district_id: u32) -> Color32 {
    PALETTE[(district_id as usize).saturating_sub(1) % PALETTE.len()]
}
