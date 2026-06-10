use egui::Color32;
use std::process::{Command, Stdio};

mod data;
mod map_tab;
mod palette;
mod process;
mod reports_tab;
mod tiles;
mod whatif_tab;

use data::{HexEntry, StationEntry};
use process::{spawn_with_live_stdout, ProcessKind, RunningProcess};
use reports_tab::SavedReport;
use whatif_tab::{WhatIfVariant, WhatIfResult};

// ── Tab enum ─────────────────────────────────────────────────────────────────

#[derive(PartialEq)]
enum ActiveTab {
    Map,
    Reports,
    WhatIf,
}

#[derive(PartialEq, Clone, Copy)]
enum HexOverlay {
    Filled,
    Borders,
    Hidden,
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
    /// Set when the user clicks Stop. After the process exits, the poll loop
    /// uses this to switch the status to "Stopped." and to auto-run the report
    /// against whatever the simulator already flushed to the SQLite DB.
    stop_requested: bool,
    report_text: Option<String>,

    // ── Map + hex overlay ────────────────────────────────────────────────
    show_map: bool,
    hex_overlay: HexOverlay,
    tile_map: Option<tiles::TileMap>,
    map_texture: Option<egui::TextureHandle>,

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
    /// Per-variant live progress bars (index = variant index).
    whatif_variant_progress: Vec<whatif_tab::VariantProgress>,
    whatif_captured_output: String,
    whatif_delta: u32,
    whatif_max_variants: usize,
    whatif_short_sim: bool,
    whatif_sim_duration: u64,

    // ── Patrol strategy comparison ───────────────────────────────────────
    /// (label, json_path, enabled) — discovered from config/patrol_routes_*.json
    whatif_patrol_strategies: Vec<(String, String, bool)>,
    /// When true, the patrol comparison includes the "+ aid" / "Mutual aid only" rows.
    whatif_mutual_aid: bool,
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
        let tile_map = tiles::fetch_tile_map(
            lat_range.0, lat_range.1, lon_range.0, lon_range.1,
        );
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
            stop_requested: false,
            report_text: None,
            show_map: tile_map.is_some(),
            hex_overlay: HexOverlay::Filled,
            tile_map,
            map_texture: None,
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
            whatif_variant_progress: Vec::new(),
            whatif_captured_output: String::new(),
            whatif_delta: 2,
            whatif_max_variants: 10,
            whatif_short_sim: true,
            whatif_sim_duration: 43_200, // 30 days
            whatif_patrol_strategies: scan_patrol_strategies(),
            whatif_mutual_aid: true,
        }
    }

    fn poll_process(&mut self) {
        let Some(proc) = &mut self.process else { return };
        let kind = proc.kind;

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

        // Route stdout lines to the right parser based on which subprocess
        // this is. Each kind has its own line format.
        for line in &lines {
            let trimmed = line.trim();
            match kind {
                ProcessKind::WhatIf | ProcessKind::WhatIfPatrol => {
                    self.poll_whatif_output(trimmed);
                }
                ProcessKind::Simulate => {
                    // SimBatch::run prints "  [  1/  1] <name>  sim  50.0%"
                    if let Some(pct) = parse_sim_progress(trimmed) {
                        self.sim_progress = pct;
                        self.live_output = trimmed.to_owned();
                    }
                }
                ProcessKind::Optimize => {
                    if trimmed.starts_with("Station ") && trimmed.contains("selected") {
                        self.live_output = trimmed.to_owned();
                    }
                }
                ProcessKind::PatrolGen => {
                    if !trimmed.is_empty() {
                        self.live_output = trimmed.to_owned();
                    }
                }
            }
        }

        if let Some(ok) = finished {
            let was_stopped = self.stop_requested;
            let was_whatif = matches!(kind, ProcessKind::WhatIf | ProcessKind::WhatIfPatrol);
            if was_whatif { self.finalize_whatif(); }
            self.status = if was_stopped {
                "Stopped.".into()
            } else if ok {
                "Done.".into()
            } else {
                "Process failed.".into()
            };
            self.live_output.clear();
            self.sim_progress = 0.0;
            self.process = None;
            self.stop_requested = false;
            if kind == ProcessKind::PatrolGen && ok {
                self.whatif_patrol_strategies = scan_patrol_strategies();
            }
            // After a stopped standard sim, surface whatever the simulator
            // committed to the SQLite DB so the user can see partial results.
            // Only the Simulate kind writes to output/dispatch_sim.db, so this
            // path must NOT fire for stopped optimize/patrol_gen/whatif runs.
            if was_stopped && kind == ProcessKind::Simulate {
                self.run_report();
                self.status = "Stopped — partial report ready.".into();
            }
        }
    }

    /// Kill the running child process tree. The next `poll_process` call will
    /// observe the exit and (for the standard sim) auto-generate a report from
    /// whatever the simulator already flushed to disk.
    fn stop_process(&mut self) {
        let Some(proc) = &mut self.process else { return };
        let pid = proc.child.id();
        #[cfg(windows)]
        {
            // `cargo run` spawns the sim binary as a grandchild; killing only
            // the direct child leaves the simulator orphaned. `taskkill /T`
            // walks the process tree.
            let _ = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        #[cfg(not(windows))]
        {
            let _ = pid; // silence unused-var on non-windows
            let _ = proc.child.kill();
        }
        self.stop_requested = true;
        self.status = "Stopping…".into();
    }

    fn spawn_optimize(&mut self) {
        if self.process.is_some() {
            self.status = "A process is already running.".into();
            return;
        }
        let mut cmd = Command::new("cargo");
        cmd.args(["run", "--release", "--bin", "optimize", "--", "config/optimize.toml"]);
        match spawn_with_live_stdout(cmd, ProcessKind::Optimize) {
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
        match spawn_with_live_stdout(cmd, ProcessKind::Simulate) {
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
                ui.add_enabled_ui(busy && !self.stop_requested, |ui| {
                    if ui.button("■ Stop").clicked() { self.stop_process(); }
                });
                if ui.button("📋 Report").clicked() { self.run_report(); }

                ui.separator();
                if self.tile_map.is_some() {
                    ui.toggle_value(&mut self.show_map, "🗺 Map");
                }
                egui::ComboBox::from_id_salt("hex_overlay")
                    .selected_text(match self.hex_overlay {
                        HexOverlay::Filled  => "Filled",
                        HexOverlay::Borders => "Borders",
                        HexOverlay::Hidden  => "Hidden",
                    })
                    .width(70.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.hex_overlay, HexOverlay::Filled,  "Filled");
                        ui.selectable_value(&mut self.hex_overlay, HexOverlay::Borders, "Borders");
                        ui.selectable_value(&mut self.hex_overlay, HexOverlay::Hidden,  "Hidden");
                    });
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

/// Scan `config/` for `patrol_routes_*.json` files. Returns
/// `(label, path, enabled)` triples sorted by label, all enabled by default.
pub fn scan_patrol_strategies() -> Vec<(String, String, bool)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("config") else { return out };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") { continue; }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        if let Some(label) = stem.strip_prefix("patrol_routes_") {
            out.push((label.to_string(), path.to_string_lossy().into_owned(), true));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

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
