use egui::{Color32, Pos2, Rect, Stroke, Vec2};

use crate::data::{HexEntry, StationEntry};
use crate::palette::district_color;
use crate::DashboardApp;

impl DashboardApp {
    pub fn show_map_tab(&mut self, ctx: &egui::Context) {
        // Optional bottom panel — quick view of the latest report
        if let Some(text) = &self.report_text.clone() {
            egui::TopBottomPanel::bottom("report_panel")
                .resizable(true)
                .min_height(120.0)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.heading("Latest Report");
                        if ui.small_button("✖ Close").clicked() {
                            self.report_text = None;
                        }
                    });
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut text.as_str())
                                .font(egui::TextStyle::Monospace)
                                .desired_width(f32::INFINITY),
                        );
                    });
                });
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            let rect = ui.available_rect_before_wrap();
            draw_hex_map(ui, rect, &self.hexes, &self.stations, self.lat_range, self.lon_range);
        });
    }
}

pub fn draw_hex_map(
    ui: &mut egui::Ui,
    rect: Rect,
    hexes: &[HexEntry],
    stations: &[StationEntry],
    (lat_min, lat_max): (f64, f64),
    (lon_min, lon_max): (f64, f64),
) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, Color32::from_rgb(15, 15, 25));

    let lat_span = (lat_max - lat_min).max(1e-9);
    let lon_span = (lon_max - lon_min).max(1e-9);

    let margin = 8.0_f32;
    let inner = rect.shrink(margin);

    let dot_r = ((inner.width() / lon_span as f32) * 0.0015).clamp(1.5, 6.0);

    for hex in hexes {
        let x = inner.left() + ((hex.lon - lon_min) / lon_span) as f32 * inner.width();
        let y = inner.bottom() - ((hex.lat - lat_min) / lat_span) as f32 * inner.height();
        painter.circle_filled(Pos2::new(x, y), dot_r, district_color(hex.district_id));
    }

    // Station markers
    for station in stations {
        let x = inner.left() + ((station.lon - lon_min) / lon_span) as f32 * inner.width();
        let y = inner.bottom() - ((station.lat - lat_min) / lat_span) as f32 * inner.height();
        let pos = Pos2::new(x, y);
        painter.circle_filled(pos, dot_r + 4.0, Color32::BLACK);
        painter.circle_filled(pos, dot_r + 3.0, Color32::from_rgb(255, 215, 0));
        painter.text(
            pos,
            egui::Align2::CENTER_CENTER,
            "★",
            egui::FontId::proportional(10.0),
            Color32::BLACK,
        );
        // Tooltip on hover
        let rect = Rect::from_center_size(pos, Vec2::splat((dot_r + 4.0) * 2.0));
        if ui.rect_contains_pointer(rect) {
            egui::show_tooltip_at_pointer(ui.ctx(), ui.layer_id(), egui::Id::new(&station.name), |ui| {
                ui.label(&station.name);
            });
        }
    }

    // Legend
    let mut district_ids: Vec<u32> = hexes.iter().map(|h| h.district_id).collect();
    district_ids.sort_unstable();
    district_ids.dedup();

    let legend_x      = rect.right() - 110.0;
    let legend_y_start = rect.top() + 10.0;
    let row_h          = 16.0;

    painter.rect_filled(
        Rect::from_min_size(
            Pos2::new(legend_x - 4.0, legend_y_start - 4.0),
            Vec2::new(104.0, district_ids.len() as f32 * row_h + 8.0),
        ),
        4.0,
        Color32::from_rgba_premultiplied(0, 0, 0, 160),
    );

    for (i, &id) in district_ids.iter().enumerate() {
        let y     = legend_y_start + i as f32 * row_h;
        let color = district_color(id);
        painter.circle_filled(Pos2::new(legend_x + 6.0, y + 6.0), 5.0, color);
        painter.circle_stroke(
            Pos2::new(legend_x + 6.0, y + 6.0),
            5.0,
            Stroke::new(0.5, Color32::WHITE),
        );
        painter.text(
            Pos2::new(legend_x + 16.0, y),
            egui::Align2::LEFT_TOP,
            format!("District {id}"),
            egui::FontId::proportional(11.0),
            Color32::WHITE,
        );
    }
}
