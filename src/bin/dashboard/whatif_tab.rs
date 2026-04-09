use egui::{Color32, RichText};
use std::process::{Command, Stdio};

use crate::DashboardApp;
use crate::process::spawn_with_live_stdout;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// A named variant: per-district unit allocation.
#[derive(Clone)]
pub struct WhatIfVariant {
    pub name: String,
    /// (district_id, district_name, unit_count)
    pub allocations: Vec<(u32, String, u32)>,
}

impl WhatIfVariant {
    pub fn total_units(&self) -> u32 {
        self.allocations.iter().map(|(_, _, c)| c).sum()
    }
}

/// Parsed result row from the whatif CLI output.
#[derive(Clone)]
pub struct WhatIfResult {
    pub rank: u32,
    pub name: String,
    pub units: u32,
    pub sla_a: f64,
    pub sla_b: f64,
    pub sla_c: f64,
    pub overall: f64,
}

// ---------------------------------------------------------------------------
// Tab rendering
// ---------------------------------------------------------------------------

impl DashboardApp {
    pub fn show_whatif_tab(&mut self, ctx: &egui::Context) {
        // Left panel: variant editor
        egui::SidePanel::left("whatif_editor")
            .min_width(320.0)
            .max_width(400.0)
            .show(ctx, |ui| {
                ui.heading("Unit Allocation Editor");
                ui.separator();

                let total: u32 = self.whatif_current_alloc.iter().map(|(_, _, c)| *c).sum();
                ui.label(format!("Total units: {total}"));
                ui.add_space(4.0);

                egui::ScrollArea::vertical()
                    .id_salt("whatif_alloc_scroll")
                    .show(ui, |ui| {
                        for (district_id, name, count) in self.whatif_current_alloc.iter_mut() {
                            ui.horizontal(|ui| {
                                ui.label(format!("{name} (D{district_id})"));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.small_button("+").clicked() && *count < 50 {
                                        *count += 1;
                                    }
                                    let mut val = *count as i32;
                                    ui.add(egui::DragValue::new(&mut val).range(1..=50).speed(0.1));
                                    *count = val.max(1) as u32;
                                    if ui.small_button("-").clicked() && *count > 1 {
                                        *count -= 1;
                                    }
                                });
                            });
                        }
                    });

                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Variant name:");
                    ui.text_edit_singleline(&mut self.whatif_variant_name);
                });
                if ui.button("Save as Variant").clicked() && !self.whatif_variant_name.trim().is_empty() {
                    let v = WhatIfVariant {
                        name: self.whatif_variant_name.trim().to_string(),
                        allocations: self.whatif_current_alloc.clone(),
                    };
                    self.whatif_variants.push(v);
                    self.whatif_variant_name = format!("Variant {}", self.whatif_variants.len() + 1);
                }

                if ui.button("Reset to Baseline").clicked() {
                    self.whatif_current_alloc = self.whatif_baseline.clone();
                }
            });

        // Right panel: variant list, run button, results
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("What-If Variants");
            ui.separator();

            // Variant list
            if self.whatif_variants.is_empty() {
                ui.weak("No variants saved yet. Adjust unit counts and click 'Save as Variant'.");
            } else {
                let mut delete_idx = None;
                for (idx, v) in self.whatif_variants.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("{}  ({} units)", v.name, v.total_units()))
                                .strong(),
                        );
                        if ui.small_button("Load").clicked() {
                            self.whatif_current_alloc = v.allocations.clone();
                        }
                        if ui.small_button("Delete").clicked() {
                            delete_idx = Some(idx);
                        }
                    });
                }
                if let Some(idx) = delete_idx {
                    self.whatif_variants.remove(idx);
                }
            }

            ui.add_space(8.0);
            ui.separator();

            // Quick-generate button
            ui.horizontal(|ui| {
                ui.label("Quick generate:");
                ui.label("delta ±");
                ui.add(egui::DragValue::new(&mut self.whatif_delta).range(1..=10).speed(0.1));
                ui.label("max variants:");
                ui.add(egui::DragValue::new(&mut self.whatif_max_variants).range(2..=50).speed(0.5));
            });

            // Sim duration override
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.whatif_short_sim, "Shorten sim to");
                ui.add_enabled(
                    self.whatif_short_sim,
                    egui::DragValue::new(&mut self.whatif_sim_duration)
                        .range(1440..=2_102_400)
                        .speed(1440.0)
                        .suffix(" min"),
                );
                let days = self.whatif_sim_duration as f64 / 1440.0;
                if self.whatif_short_sim {
                    ui.weak(format!("({days:.0} days)"));
                }
            });

            ui.add_space(4.0);

            let busy = self.process.is_some();
            ui.add_enabled_ui(!busy, |ui| {
                if ui.button("Run What-If Analysis").clicked() {
                    self.spawn_whatif();
                }
            });

            // ── Patrol strategy comparison ──────────────────────────────
            ui.add_space(8.0);
            ui.separator();
            egui::CollapsingHeader::new("Patrol Strategy Comparison")
                .default_open(true)
                .show(ui, |ui| {
                    if self.whatif_patrol_strategies.is_empty() {
                        ui.weak("No patrol_routes_*.json files in config/.");
                        ui.weak("Click a Generate button below to create one.");
                    } else {
                        for (label, path, enabled) in self.whatif_patrol_strategies.iter_mut() {
                            ui.horizontal(|ui| {
                                ui.checkbox(enabled, label.as_str());
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| { ui.weak(path.as_str()); },
                                );
                            });
                        }
                    }

                    ui.add_space(4.0);
                    ui.checkbox(&mut self.whatif_mutual_aid, "Include mutual-aid variants");

                    ui.add_space(4.0);
                    ui.add_enabled_ui(!busy, |ui| {
                        ui.horizontal(|ui| {
                            if ui.button("Generate (hotspot)").clicked() {
                                self.spawn_patrol_gen("hotspot");
                            }
                            if ui.button("Generate (border)").clicked() {
                                self.spawn_patrol_gen("border");
                            }
                            if ui.button("↻ Rescan").clicked() {
                                self.whatif_patrol_strategies = crate::scan_patrol_strategies();
                            }
                        });
                    });

                    ui.add_space(4.0);
                    let any_selected = self.whatif_patrol_strategies.iter().any(|(_, _, e)| *e);
                    ui.add_enabled_ui(!busy && any_selected, |ui| {
                        if ui.button("Run Patrol Comparison").clicked() {
                            self.spawn_whatif_patrol();
                        }
                    });
                    if !any_selected && !self.whatif_patrol_strategies.is_empty() {
                        ui.weak("Tick at least one strategy to enable.");
                    }
                });

            // Progress
            if self.whatif_running {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new(&self.whatif_progress)
                            .monospace()
                            .color(Color32::from_rgb(180, 220, 255)),
                    );
                });
            }

            // Results
            if !self.whatif_results.is_empty() {
                ui.add_space(12.0);
                ui.heading("Results — Ranked by Overall SLA");
                ui.separator();

                egui::Grid::new("whatif_results_grid")
                    .num_columns(7)
                    .striped(true)
                    .min_col_width(60.0)
                    .show(ui, |ui| {
                        ui.label(RichText::new("Rank").strong());
                        ui.label(RichText::new("Variant").strong());
                        ui.label(RichText::new("Units").strong());
                        ui.label(RichText::new("SLA-A %").strong());
                        ui.label(RichText::new("SLA-B %").strong());
                        ui.label(RichText::new("SLA-C %").strong());
                        ui.label(RichText::new("Overall %").strong());
                        ui.end_row();

                        for r in &self.whatif_results {
                            ui.label(format!("{}", r.rank));
                            ui.label(&r.name);
                            ui.label(format!("{}", r.units));

                            let color_a = sla_color(r.sla_a);
                            let color_b = sla_color(r.sla_b);
                            let color_c = sla_color(r.sla_c);
                            let color_o = sla_color(r.overall);

                            ui.label(RichText::new(format!("{:.1}%", r.sla_a)).color(color_a));
                            ui.label(RichText::new(format!("{:.1}%", r.sla_b)).color(color_b));
                            ui.label(RichText::new(format!("{:.1}%", r.sla_c)).color(color_c));
                            ui.label(RichText::new(format!("{:.1}%", r.overall)).color(color_o));
                            ui.end_row();
                        }
                    });
            }

            // Raw output (collapsible)
            if let Some(ref output) = self.whatif_raw_output {
                ui.add_space(8.0);
                egui::CollapsingHeader::new("Raw Output")
                    .default_open(false)
                    .show(ui, |ui| {
                        let mut text = output.as_str();
                        egui::ScrollArea::vertical()
                            .max_height(300.0)
                            .show(ui, |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut text)
                                        .font(egui::TextStyle::Monospace)
                                        .desired_width(f32::INFINITY),
                                );
                            });
                    });
            }
        });
    }

    pub fn spawn_whatif(&mut self) {
        if self.process.is_some() {
            self.status = "A process is already running.".into();
            return;
        }

        let mut cmd = Command::new("cargo");
        let mut args = vec![
            "run".to_string(),
            "--release".to_string(),
            "--bin".to_string(),
            "dispatch_sim".to_string(),
            "--".to_string(),
            "whatif".to_string(),
            "config/city.toml".to_string(),
            self.whatif_max_variants.to_string(),
            self.whatif_delta.to_string(),
        ];
        if self.whatif_short_sim {
            args.push(self.whatif_sim_duration.to_string());
        }
        cmd.args(&args);

        // Also capture stderr for cargo build output
        cmd.stderr(Stdio::piped());

        match spawn_with_live_stdout(cmd) {
            Ok(p) => {
                self.process = Some(p);
                self.status = "What-if analysis running…".into();
                self.whatif_running = true;
                self.whatif_progress = "Starting…".into();
                self.whatif_results.clear();
                self.whatif_raw_output = None;
                self.whatif_captured_output.clear();
            }
            Err(e) => self.status = format!("Failed to start what-if: {e}"),
        }
    }

    /// Spawn the patrol-strategy comparison: shells out to
    /// `dispatch_sim whatif-patrol …` with one `--strategy <label>` per ticked
    /// row and an optional `--no-aid` flag.
    pub fn spawn_whatif_patrol(&mut self) {
        if self.process.is_some() {
            self.status = "A process is already running.".into();
            return;
        }

        let mut cmd = Command::new("cargo");
        let mut args: Vec<String> = vec![
            "run".into(),
            "--release".into(),
            "--bin".into(),
            "dispatch_sim".into(),
            "--".into(),
            "whatif-patrol".into(),
            "config/city.toml".into(),
        ];
        for (label, _path, enabled) in &self.whatif_patrol_strategies {
            if *enabled {
                args.push("--strategy".into());
                args.push(label.clone());
            }
        }
        if !self.whatif_mutual_aid {
            args.push("--no-aid".into());
        }
        if self.whatif_short_sim {
            args.push(self.whatif_sim_duration.to_string());
        }
        cmd.args(&args);
        cmd.stderr(Stdio::piped());

        match spawn_with_live_stdout(cmd) {
            Ok(p) => {
                self.process = Some(p);
                self.status = "Patrol comparison running…".into();
                self.whatif_running = true;
                self.whatif_progress = "Starting…".into();
                self.whatif_results.clear();
                self.whatif_raw_output = None;
                self.whatif_captured_output.clear();
            }
            Err(e) => self.status = format!("Failed to start patrol comparison: {e}"),
        }
    }

    /// Spawn the offline patrol-route generator for one strategy. The
    /// strategy list is auto-rescanned when the process exits via
    /// `finalize_whatif`'s caller path — see `poll_process` in main.rs.
    pub fn spawn_patrol_gen(&mut self, strategy: &str) {
        if self.process.is_some() {
            self.status = "A process is already running.".into();
            return;
        }
        let mut cmd = Command::new("cargo");
        cmd.args([
            "run", "--release", "--bin", "patrol_gen", "--",
            "config/city.toml", strategy,
        ]);
        cmd.stderr(Stdio::piped());
        match spawn_with_live_stdout(cmd) {
            Ok(p) => {
                self.process = Some(p);
                self.status = format!("Generating patrol routes ({strategy})…");
            }
            Err(e) => self.status = format!("Failed to start patrol_gen: {e}"),
        }
    }

    /// Called from poll_process — accumulate whatif output lines.
    pub fn poll_whatif_output(&mut self, line: &str) {
        self.whatif_captured_output.push_str(line);
        self.whatif_captured_output.push('\n');

        // Update progress display
        if line.contains("…") || line.contains("...") || line.contains("done") {
            self.whatif_progress = line.trim().to_string();
        }
        if line.contains("Generated") && line.contains("variants") {
            self.whatif_progress = line.trim().to_string();
        }
    }

    /// Called when the whatif process finishes.
    pub fn finalize_whatif(&mut self) {
        self.whatif_running = false;
        self.whatif_progress.clear();
        self.whatif_raw_output = Some(self.whatif_captured_output.clone());
        self.whatif_results = parse_whatif_output(&self.whatif_captured_output);
        self.whatif_captured_output.clear();
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn sla_color(pct: f64) -> Color32 {
    if pct >= 90.0 {
        Color32::from_rgb(100, 220, 100)
    } else if pct >= 70.0 {
        Color32::from_rgb(220, 200, 80)
    } else {
        Color32::from_rgb(220, 80, 80)
    }
}

/// Parse the ranked comparison table from the CLI whatif output.
fn parse_whatif_output(output: &str) -> Vec<WhatIfResult> {
    let mut results = Vec::new();
    let mut in_table = false;

    for line in output.lines() {
        let trimmed = line.trim();

        // Detect start of table (after the header separator)
        if trimmed.starts_with("---") && in_table {
            continue;
        }
        if trimmed.contains("Rank") && trimmed.contains("Variant") && trimmed.contains("Overall") {
            in_table = true;
            continue;
        }
        if in_table && trimmed.starts_with("---") {
            continue;
        }
        if in_table && trimmed.starts_with("Detail") {
            break;
        }

        if in_table && !trimmed.is_empty() {
            if let Some(r) = parse_result_line(trimmed) {
                results.push(r);
            }
        }
    }

    results
}

fn parse_result_line(line: &str) -> Option<WhatIfResult> {
    // Format: "  1    Baseline                                   140      92.3%      98.1%      99.5%      96.6%"
    // We need to parse: rank, name (variable width), units, sla_a, sla_b, sla_c, overall
    // The percentages end with %
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 5 { return None; }

    let rank: u32 = parts[0].parse().ok()?;

    // Find the percentage values from the end
    let pct_values: Vec<f64> = parts.iter().rev()
        .take(4)
        .filter_map(|s| s.trim_end_matches('%').parse::<f64>().ok())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    if pct_values.len() != 4 { return None; }

    // Units is the value just before the 4 percentages
    let units_idx = parts.len() - 5;
    let units: u32 = parts[units_idx].parse().ok()?;

    // Name is everything between rank and units
    let name = parts[1..units_idx].join(" ");

    Some(WhatIfResult {
        rank,
        name,
        units,
        sla_a: pct_values[0],
        sla_b: pct_values[1],
        sla_c: pct_values[2],
        overall: pct_values[3],
    })
}
