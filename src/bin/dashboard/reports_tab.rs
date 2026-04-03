use egui::{Color32, Vec2};

use crate::DashboardApp;

pub struct SavedReport {
    pub name: String,
    pub content: String,
}

impl DashboardApp {
    pub fn show_reports_tab(&mut self, ctx: &egui::Context) {
        // ── Save bar ─────────────────────────────────────────────────────
        egui::TopBottomPanel::top("save_bar").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Save current report:");
                let name_edit = egui::TextEdit::singleline(&mut self.report_name_input)
                    .desired_width(180.0);
                ui.add(name_edit);
                let can_save = self.report_text.is_some()
                    && !self.report_name_input.trim().is_empty();
                if ui.add_enabled(can_save, egui::Button::new("💾 Save")).clicked() {
                    self.save_current_report();
                }
                if self.report_text.is_none() {
                    ui.label(
                        egui::RichText::new("(no report loaded — click 📋 Report first)")
                            .weak(),
                    );
                }
            });
            ui.add_space(4.0);
        });

        // ── Report list sidebar ───────────────────────────────────────────
        let mut delete_idx: Option<usize> = None;
        let mut new_viewing  = self.viewing;
        let mut new_comparing = self.comparing;

        egui::SidePanel::left("report_list_panel")
            .min_width(200.0)
            .max_width(300.0)
            .show(ctx, |ui| {
                ui.heading(format!("Saved Reports ({})", self.saved_reports.len()));
                ui.separator();

                if self.saved_reports.is_empty() {
                    ui.weak("No saved reports yet.");
                    return;
                }

                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (idx, report) in self.saved_reports.iter().enumerate() {
                        let is_viewing   = self.viewing   == Some(idx);
                        let is_comparing = self.comparing == Some(idx);

                        // Highlight selected rows
                        let bg = if is_viewing && is_comparing {
                            Color32::from_rgba_premultiplied(100, 80, 180, 80)
                        } else if is_viewing {
                            Color32::from_rgba_premultiplied(60, 120, 60, 80)
                        } else if is_comparing {
                            Color32::from_rgba_premultiplied(140, 80, 40, 80)
                        } else {
                            Color32::TRANSPARENT
                        };

                        egui::Frame::default()
                            .fill(bg)
                            .inner_margin(egui::Margin::symmetric(4, 2))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    if ui.selectable_label(is_viewing, &report.name).clicked() {
                                        new_viewing = if is_viewing { None } else { Some(idx) };
                                    }
                                });
                                ui.horizontal(|ui| {
                                    let compare_label = if is_comparing { "◀ B" } else { "B ▶" };
                                    if ui.small_button(compare_label).on_hover_text(
                                        if is_comparing { "Remove from comparison" }
                                        else { "Show side-by-side with A" }
                                    ).clicked() {
                                        new_comparing = if is_comparing { None } else { Some(idx) };
                                    }
                                    if ui.small_button("✖").on_hover_text("Delete").clicked() {
                                        delete_idx = Some(idx);
                                    }
                                });
                            });
                        ui.separator();
                    }
                });
            });

        // Apply list mutations after the borrow on self ends
        self.viewing   = new_viewing;
        self.comparing = new_comparing;

        if let Some(idx) = delete_idx {
            self.saved_reports.remove(idx);
            if self.viewing   == Some(idx) { self.viewing   = None; }
            else if let Some(v) = self.viewing   { if v > idx { self.viewing   = Some(v - 1); } }
            if self.comparing == Some(idx) { self.comparing = None; }
            else if let Some(c) = self.comparing { if c > idx { self.comparing = Some(c - 1); } }
        }

        // ── Central report view ───────────────────────────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
            match (self.viewing, self.comparing) {
                (None, None) => {
                    ui.centered_and_justified(|ui| {
                        ui.weak("Select a report from the list to view it.\nClick 'B ▶' on a second report to compare side by side.");
                    });
                }
                (Some(a), None) | (None, Some(a)) => {
                    if let Some(report) = self.saved_reports.get(a) {
                        show_report_panel(ui, &report.name, &report.content);
                    }
                }
                (Some(a), Some(b)) => {
                    let left_name    = self.saved_reports.get(a).map(|r| r.name.clone()).unwrap_or_default();
                    let left_content = self.saved_reports.get(a).map(|r| r.content.clone()).unwrap_or_default();
                    let right_name    = self.saved_reports.get(b).map(|r| r.name.clone()).unwrap_or_default();
                    let right_content = self.saved_reports.get(b).map(|r| r.content.clone()).unwrap_or_default();

                    let total_w = ui.available_width();
                    let col_w   = (total_w - 8.0) / 2.0;

                    ui.horizontal_top(|ui| {
                        ui.allocate_ui(Vec2::new(col_w, ui.available_height()), |ui| {
                            show_report_panel(ui, &left_name, &left_content);
                        });
                        ui.add_space(8.0);
                        ui.allocate_ui(Vec2::new(col_w, ui.available_height()), |ui| {
                            show_report_panel(ui, &right_name, &right_content);
                        });
                    });
                }
            }
        });
    }

    pub fn save_current_report(&mut self) {
        let Some(content) = self.report_text.clone() else { return };
        let name = self.report_name_input.trim().to_owned();
        if name.is_empty() { return; }
        self.saved_reports.push(SavedReport { name, content });
        self.report_name_input = format!("Run {}", self.saved_reports.len() + 1);
    }
}

fn show_report_panel(ui: &mut egui::Ui, name: &str, content: &str) {
    ui.heading(name);
    ui.separator();
    egui::ScrollArea::vertical()
        .id_salt(name)
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut content.as_ref() as &mut &str)
                    .font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY),
            );
        });
}
