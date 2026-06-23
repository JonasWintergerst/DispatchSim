use egui::{Color32, Pos2, Rect, Stroke, Vec2};

use crate::data::{HexEntry, StationEntry};
use crate::palette::district_color;
use crate::{DashboardApp, HexOverlay};

impl DashboardApp {
    pub fn show_map_tab(&mut self, ctx: &egui::Context) {
        // Lazily upload the tile texture on first frame.
        if self.map_texture.is_none() {
            if let Some(tm) = &self.tile_map {
                let ci = egui::ColorImage::from_rgba_unmultiplied(
                    [tm.width as usize, tm.height as usize],
                    &tm.rgba,
                );
                self.map_texture =
                    Some(ctx.load_texture("tile_map", ci, egui::TextureOptions::LINEAR));
            }
        }

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

        let show_map = self.show_map;
        let hex_overlay = self.hex_overlay;
        let map_tex = self.map_texture.clone();
        let tile_map = self.tile_map.as_ref();

        // Capture tile bounds for the closure (avoids borrowing self).
        let tile_bounds = tile_map.map(|tm| (tm.lat_min, tm.lat_max, tm.lon_min, tm.lon_max));

        egui::CentralPanel::default().show(ctx, |ui| {
            let rect = ui.available_rect_before_wrap();
            let iso = if self.show_isochrones {
                Some(self.isochrone_minutes.as_slice())
            } else {
                None
            };
            draw_hex_map(
                ui,
                rect,
                &self.hexes,
                &self.stations,
                self.lat_range,
                self.lon_range,
                iso,
                hex_overlay,
                if show_map { map_tex.as_ref() } else { None },
                tile_bounds,
            );
        });
    }
}

fn isochrone_color(minutes: f32) -> Color32 {
    if minutes <= 5.0 {
        Color32::from_rgb(0, 200, 80)
    } else if minutes <= 10.0 {
        Color32::from_rgb(180, 220, 0)
    } else if minutes <= 15.0 {
        Color32::from_rgb(255, 140, 0)
    } else {
        Color32::from_rgb(220, 40, 40)
    }
}

#[allow(clippy::too_many_arguments)]
pub fn draw_hex_map(
    ui: &mut egui::Ui,
    rect: Rect,
    hexes: &[HexEntry],
    stations: &[StationEntry],
    (lat_min, lat_max): (f64, f64),
    (lon_min, lon_max): (f64, f64),
    isochrone_minutes: Option<&[f32]>,
    hex_overlay: HexOverlay,
    map_texture: Option<&egui::TextureHandle>,
    tile_bounds: Option<(f64, f64, f64, f64)>,
) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, Color32::from_rgb(15, 15, 25));

    let lat_span = (lat_max - lat_min).max(1e-9);
    let lon_span = (lon_max - lon_min).max(1e-9);

    let margin = 8.0_f32;
    let inner = rect.shrink(margin);

    // ── Map background ──────────────────────────────────────────────────
    if let (Some(tex), Some((t_lat_min, t_lat_max, t_lon_min, t_lon_max))) =
        (map_texture, tile_bounds)
    {
        // Convert tile geographic bounds to screen coordinates using the
        // same projection as the hex dots.
        let sx_min =
            inner.left() + ((t_lon_min - lon_min) / lon_span) as f32 * inner.width();
        let sx_max =
            inner.left() + ((t_lon_max - lon_min) / lon_span) as f32 * inner.width();
        let sy_min =
            inner.bottom() - ((t_lat_max - lat_min) / lat_span) as f32 * inner.height();
        let sy_max =
            inner.bottom() - ((t_lat_min - lat_min) / lat_span) as f32 * inner.height();

        let map_rect = Rect::from_min_max(Pos2::new(sx_min, sy_min), Pos2::new(sx_max, sy_max));

        // Clip to the visible area.
        let uv = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0));
        painter.image(tex.id(), map_rect, uv, Color32::WHITE);
    }

    // ── Hex dots ────────────────────────────────────────────────────────
    if hex_overlay != HexOverlay::Hidden {
        let dot_r = ((inner.width() / lon_span as f32) * 0.0015).clamp(1.5, 6.0);

        for (i, hex) in hexes.iter().enumerate() {
            let x = inner.left() + ((hex.lon - lon_min) / lon_span) as f32 * inner.width();
            let y =
                inner.bottom() - ((hex.lat - lat_min) / lat_span) as f32 * inner.height();
            let color = match isochrone_minutes {
                Some(iso) => isochrone_color(iso[i]),
                None => district_color(hex.district_id),
            };
            let pos = Pos2::new(x, y);
            match hex_overlay {
                HexOverlay::Filled => {
                    painter.circle_filled(pos, dot_r, color);
                }
                HexOverlay::Borders => {
                    painter.circle_stroke(pos, dot_r, Stroke::new(1.0, color));
                }
                HexOverlay::Hidden => unreachable!(),
            }
        }
    }

    // ── Station markers ─────────────────────────────────────────────────
    // Each opened station is labelled with its district number and tinted in
    // the district color, so it lines up with the hex colors and the legend.
    let dot_r = ((inner.width() / lon_span as f32) * 0.0015).clamp(1.5, 6.0);
    let marker_r = (dot_r + 5.0).max(8.0);
    for station in stations {
        let x = inner.left() + ((station.lon - lon_min) / lon_span) as f32 * inner.width();
        let y =
            inner.bottom() - ((station.lat - lat_min) / lat_span) as f32 * inner.height();
        let pos = Pos2::new(x, y);
        let color = district_color(station.district_id);
        painter.circle_filled(pos, marker_r + 1.5, Color32::BLACK);
        painter.circle_filled(pos, marker_r, color);
        painter.circle_stroke(pos, marker_r, Stroke::new(1.5, Color32::WHITE));
        // Pick black/white label for contrast against the district color.
        let lum = 0.299 * color.r() as f32 + 0.587 * color.g() as f32 + 0.114 * color.b() as f32;
        let text_col = if lum > 140.0 { Color32::BLACK } else { Color32::WHITE };
        painter.text(
            pos,
            egui::Align2::CENTER_CENTER,
            station.district_id.to_string(),
            egui::FontId::proportional(11.0),
            text_col,
        );
        let rect = Rect::from_center_size(pos, Vec2::splat((marker_r + 1.5) * 2.0));
        if ui.rect_contains_pointer(rect) {
            egui::show_tooltip_at_pointer(
                ui.ctx(),
                ui.layer_id(),
                egui::Id::new(&station.name),
                |ui| {
                    ui.label(format!("District {} — {}", station.district_id, station.name));
                },
            );
        }
    }

    // ── Legend ───────────────────────────────────────────────────────────
    if hex_overlay == HexOverlay::Hidden {
        return;
    }

    let legend_x = rect.right() - 110.0;
    let legend_y_start = rect.top() + 10.0;
    let row_h = 16.0;

    if isochrone_minutes.is_some() {
        let bands: &[(&str, Color32)] = &[
            ("≤ 5 min", Color32::from_rgb(0, 200, 80)),
            ("≤ 10 min", Color32::from_rgb(180, 220, 0)),
            ("≤ 15 min", Color32::from_rgb(255, 140, 0)),
            ("> 15 min", Color32::from_rgb(220, 40, 40)),
        ];
        painter.rect_filled(
            Rect::from_min_size(
                Pos2::new(legend_x - 4.0, legend_y_start - 4.0),
                Vec2::new(104.0, bands.len() as f32 * row_h + 8.0),
            ),
            4.0,
            Color32::from_rgba_premultiplied(0, 0, 0, 160),
        );
        for (i, &(label, color)) in bands.iter().enumerate() {
            let y = legend_y_start + i as f32 * row_h;
            painter.circle_filled(Pos2::new(legend_x + 6.0, y + 6.0), 5.0, color);
            painter.circle_stroke(
                Pos2::new(legend_x + 6.0, y + 6.0),
                5.0,
                Stroke::new(0.5, Color32::WHITE),
            );
            painter.text(
                Pos2::new(legend_x + 16.0, y),
                egui::Align2::LEFT_TOP,
                label,
                egui::FontId::proportional(11.0),
                Color32::WHITE,
            );
        }
    } else {
        let mut district_ids: Vec<u32> = hexes.iter().map(|h| h.district_id).collect();
        district_ids.sort_unstable();
        district_ids.dedup();

        painter.rect_filled(
            Rect::from_min_size(
                Pos2::new(legend_x - 4.0, legend_y_start - 4.0),
                Vec2::new(104.0, district_ids.len() as f32 * row_h + 8.0),
            ),
            4.0,
            Color32::from_rgba_premultiplied(0, 0, 0, 160),
        );
        for (i, &id) in district_ids.iter().enumerate() {
            let y = legend_y_start + i as f32 * row_h;
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
}
