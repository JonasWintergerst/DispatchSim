use egui::Color32;
use std::process::{Command, Stdio};

mod data;
mod map_tab;
mod palette;
mod process;
mod reports_tab;

use data::{HexEntry, StationEntry};
use process::{is_progress_line, spawn_with_live_stdout, RunningProcess};
use reports_tab::SavedReport;

// ── Tab enum ─────────────────────────────────────────────────────────────────

#[derive(PartialEq)]
enum ActiveTab {
    Map,
    Reports,
}

// ── App state ────────────────────────────────────────────────────────────────

struct DashboardApp {
    hexes: Vec<HexEntry>,
    stations: Vec<StationEntry>,
    lat_range: (f64, f64),
    lon_range: (f64, f64),
    status: String,
    live_output: String,
    process: Option<RunningProcess>,
    report_text: Option<String>,

    // ── Reports page ─────────────────────────────────────────────────────
    active_tab: ActiveTab,
    saved_reports: Vec<SavedReport>,
    report_name_input: String,
    viewing: Option<usize>,
    comparing: Option<usize>,
}

impl DashboardApp {
    fn new() -> Self {
        let hexes = data::load_hexes();
        let (lat_range, lon_range) = data::bounds(&hexes);
        let stations = data::load_stations();
        Self {
            hexes,
            stations,
            lat_range,
            lon_range,
            status: "Ready.".into(),
            live_output: String::new(),
            process: None,
            report_text: None,
            active_tab: ActiveTab::Map,
            saved_reports: Vec::new(),
            report_name_input: "Run 1".into(),
            viewing: None,
            comparing: None,
        }
    }

    fn poll_process(&mut self) {
        let Some(proc) = &mut self.process else { return };

        while let Ok(line) = proc.stdout_rx.try_recv() {
            let trimmed = line.trim().to_owned();
            if is_progress_line(&trimmed) {
                self.live_output = trimmed;
            }
        }

        match proc.child.try_wait() {
            Ok(Some(exit)) => {
                self.status = if exit.success() {
                    "Done.".into()
                } else {
                    format!("Exited with status {exit}")
                };
                self.live_output.clear();
                self.process = None;
            }
            Ok(None) => {}
            Err(e) => {
                self.status = format!("Error polling process: {e}");
                self.live_output.clear();
                self.process = None;
            }
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
                ui.separator();

                let busy = self.process.is_some();
                ui.add_enabled_ui(!busy, |ui| {
                    if ui.button("⚙ Optimize").clicked() { self.spawn_optimize(); }
                    if ui.button("▶ Simulate").clicked() { self.spawn_simulate(); }
                });
                if ui.button("📋 Report").clicked() { self.run_report(); }

                ui.separator();
                ui.label(&self.status);

                if !self.live_output.is_empty() {
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
        }
    }
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
