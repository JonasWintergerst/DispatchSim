use egui::Color32;
use std::process::{Command, Stdio};

mod data;
mod map_tab;
mod palette;
mod process;
mod reports_tab;
mod whatif_tab;

use data::{HexEntry, StationEntry};
use process::{is_progress_line, spawn_with_live_stdout, RunningProcess};
use reports_tab::SavedReport;
use whatif_tab::{WhatIfVariant, WhatIfResult};

// ── Tab enum ─────────────────────────────────────────────────────────────────

#[derive(PartialEq)]
enum ActiveTab {
    Map,
    Reports,
    WhatIf,
}

// ── App state ────────────────────────────────────────────────────────────────

struct DashboardApp {
    hexes: Vec<HexEntry>,
    stations: Vec<StationEntry>,
    lat_range: (f64, f64),
    lon_range: (f64, f64),
    status: String,
    live_output: String,
    sim_progress: f32,
    process: Option<RunningProcess>,
    report_text: Option<String>,

    // ── Isochrone overlay ─────────────────────────────────────────────────
    /// Minimum haversine travel time in minutes to nearest station, one per hex.
    isochrone_minutes: Vec<f32>,
    show_isochrones: bool,

    // ── Reports page ─────────────────────────────────────────────────────
    active_tab: ActiveTab,
    saved_reports: Vec<SavedReport>,
    report_name_input: String,
    viewing: Option<usize>,
    comparing: Option<usize>,

    // ── What-If tab ──────────────────────────────────────────────────────
    /// Baseline allocation loaded from city.toml (district_id, name, unit_count)
    whatif_baseline: Vec<(u32, String, u32)>,
    /// Current allocation being edited
    whatif_current_alloc: Vec<(u32, String, u32)>,
    whatif_variant_name: String,
    whatif_variants: Vec<WhatIfVariant>,
    whatif_results: Vec<WhatIfResult>,
    whatif_raw_output: Option<String>,
    whatif_running: bool,
    whatif_progress: String,
    whatif_captured_output: String,
    whatif_delta: u32,
    whatif_max_variants: usize,
    whatif_short_sim: bool,
    whatif_sim_duration: u64,
}

impl DashboardApp {
    fn new() -> Self {
        let hexes = data::load_hexes();
        let (lat_range, lon_range) = data::bounds(&hexes);
        let stations = data::load_stations();
        let district_stations = data::load_district_stations();
        let isochrone_minutes: Vec<f32> = hexes
            .iter()
            .map(|h| data::min_travel_min(h.lat, h.lon, &district_stations))
            .collect();
        let baseline_alloc = data::load_district_allocations();
        let current_alloc = baseline_alloc.clone();
        Self {
            hexes,
            stations,
            lat_range,
            lon_range,
            status: "Ready.".into(),
            live_output: String::new(),
            sim_progress: 0.0,
            process: None,
            report_text: None,
            isochrone_minutes,
            show_isochrones: false,
            active_tab: ActiveTab::Map,
            saved_reports: Vec::new(),
            report_name_input: "Run 1".into(),
            viewing: None,
            comparing: None,
            whatif_baseline: baseline_alloc,
            whatif_current_alloc: current_alloc,
            whatif_variant_name: "Variant 1".into(),
            whatif_variants: Vec::new(),
            whatif_results: Vec::new(),
            whatif_raw_output: None,
            whatif_running: false,
            whatif_progress: String::new(),
            whatif_captured_output: String::new(),
            whatif_delta: 2,
            whatif_max_variants: 10,
            whatif_short_sim: true,
            whatif_sim_duration: 43_200, // 30 days
        }
    }

    fn poll_process(&mut self) {
        let Some(proc) = &mut self.process else { return };

        // Collect lines to avoid overlapping borrows on self.
        let mut lines = Vec::new();
        while let Ok(line) = proc.stdout_rx.try_recv() {
            lines.push(line);
        }

        let finished = match proc.child.try_wait() {
            Ok(Some(exit)) => Some(exit.success()),
            Ok(None) => None,
            Err(e) => {
                self.status = format!("Error polling process: {e}");
                self.live_output.clear();
                self.process = None;
                if self.whatif_running { self.finalize_whatif(); }
                return;
            }
        };

        // Process collected lines.
        for line in &lines {
            let trimmed = line.trim();
            if self.whatif_running {
                self.poll_whatif_output(trimmed);
            } else if is_progress_line(trimmed) {
                if let Some(pct) = parse_sim_progress(trimmed) {
                    self.sim_progress = pct;
                }
                self.live_output = trimmed.to_owned();
            }
        }

        if let Some(ok) = finished {
            if self.whatif_running { self.finalize_whatif(); }
            self.status = if ok { "Done.".into() } else { "Process failed.".into() };
            self.live_output.clear();
            self.sim_progress = 0.0;
            self.process = None;
        }
    }

    fn spawn_optimize(&mut self) {
        if self.process.is_some() {
            self.status = "A process is already running.".into();
            return;
        }
        let mut cmd = Command::new("cargo");
        cmd.args(["run", "--release", "--bin", "optimize", "--", "config/optimize.toml"]);
        match spawn_with_live_stdout(cmd) {
            Ok(p) => {
                self.process = Some(p);
                self.status = "Optimizer running…".into();
                self.live_output.clear();
            }
            Err(e) => self.status = format!("Failed to start optimizer: {e}"),
        }
    }

    fn spawn_simulate(&mut self) {
        if self.process.is_some() {
            self.status = "A process is already running.".into();
            return;
        }
        let mut cmd = Command::new("cargo");
        cmd.args(["run", "--release", "--bin", "dispatch_sim", "--", "config/city.toml"]);
        match spawn_with_live_stdout(cmd) {
            Ok(p) => {
                self.process = Some(p);
                self.status = "Simulation running…".into();
                self.live_output.clear();
            }
            Err(e) => self.status = format!("Failed to start simulation: {e}"),
        }
    }

    fn run_report(&mut self) {
        if self.process.is_some() {
            self.status = "A process is already running.".into();
            return;
        }
        self.status = "Generating report…".into();
        match Command::new("cargo")
            .args([
                "run", "--release", "--bin", "dispatch_sim",
                "--", "report", "output/dispatch_sim.db",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
        {
            Ok(out) => {
                let text = String::from_utf8_lossy(&out.stdout).into_owned();
                let err  = String::from_utf8_lossy(&out.stderr).into_owned();
                self.report_text = Some(if !text.trim().is_empty() { text }
                                        else if !err.trim().is_empty() { err }
                                        else { "(no output)".into() });
                self.report_name_input = format!("Run {}", self.saved_reports.len() + 1);
                self.status = "Report ready.".into();
            }
            Err(e) => self.status = format!("Failed to run report: {e}"),
        }
    }
}

// ── egui update ──────────────────────────────────────────────────────────────

impl eframe::App for DashboardApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_process();
        if self.process.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.active_tab, ActiveTab::Map,     "🗺 Map");
                ui.selectable_value(&mut self.active_tab, ActiveTab::Reports, "📊 Reports");
                ui.selectable_value(&mut self.active_tab, ActiveTab::WhatIf, "🔀 What-If");
                ui.separator();

                let busy = self.process.is_some();
                ui.add_enabled_ui(!busy, |ui| {
                    if ui.button("⚙ Optimize").clicked() { self.spawn_optimize(); }
                    if ui.button("▶ Simulate").clicked() { self.spawn_simulate(); }
                });
                if ui.button("📋 Report").clicked() { self.run_report(); }

                ui.separator();
                ui.toggle_value(&mut self.show_isochrones, "🌐 Isochrones");
                ui.separator();
                ui.label(&self.status);

                if self.sim_progress > 0.0 {
                    ui.separator();
                    ui.add(
                        egui::ProgressBar::new(self.sim_progress)
                            .desired_width(200.0)
                            .show_percentage(),
                    );
                } else if !self.live_output.is_empty() {
                    ui.separator();
                    ui.label(
                        egui::RichText::new(&self.live_output)
                            .monospace()
                            .color(Color32::from_rgb(180, 220, 255)),
                    );
                }
            });
            ui.add_space(4.0);
        });

        match self.active_tab {
            ActiveTab::Map => self.show_map_tab(ctx),
            ActiveTab::Reports => self.show_reports_tab(ctx),
            ActiveTab::WhatIf => self.show_whatif_tab(ctx),
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Parse a trailing `— 12.5%` from a sim progress line; returns 0.0–1.0.
fn parse_sim_progress(line: &str) -> Option<f32> {
    let pct_end = line.rfind('%')?;
    let before = &line[..pct_end];
    let num_start = before.rfind(|c: char| !c.is_ascii_digit() && c != '.')? + 1;
    before[num_start..].parse::<f32>().ok().map(|p| (p / 100.0).clamp(0.0, 1.0))
}

// ── Entry point ──────────────────────────────────────────────────────────────

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Dispatch Sim — Dashboard")
            .with_inner_size([1100.0, 750.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Dispatch Sim",
        options,
        Box::new(|_cc| Ok(Box::new(DashboardApp::new()))),
    )
}
