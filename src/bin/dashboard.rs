use eframe::egui;
use egui::{Color32, Pos2, Rect, Stroke, Vec2};
use serde::Deserialize;
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};

// ── Hex data ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct HexEntry {
    lat: f64,
    lon: f64,
    district_id: u32,
}

#[derive(Deserialize)]
struct StationEntry {
    name: String,
    lat: f64,
    lon: f64,
}

// ── Colour palette (16 visually distinct colours) ─────────────────────────────

const PALETTE: &[Color32] = &[
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

fn district_color(district_id: u32) -> Color32 {
    PALETTE[(district_id as usize).saturating_sub(1) % PALETTE.len()]
}

// ── Running process wrapper ───────────────────────────────────────────────────

struct RunningProcess {
    child: Child,
    /// Lines streamed from stdout by a background thread
    stdout_rx: Receiver<String>,
}

/// Spawn a command with piped stdout; a background thread forwards each line
/// through the returned channel so the UI thread never blocks.
fn spawn_with_live_stdout(mut cmd: Command) -> Result<RunningProcess, std::io::Error> {
    let mut child = cmd.stdout(Stdio::piped()).spawn()?;
    let stdout = child.stdout.take().expect("stdout was piped");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            if let Ok(l) = line {
                let _ = tx.send(l);
            }
        }
    });
    Ok(RunningProcess { child, stdout_rx: rx })
}

// ── Reports page types ────────────────────────────────────────────────────────

struct SavedReport {
    name: String,
    content: String,
}

#[derive(PartialEq)]
enum ActiveTab {
    Map,
    Reports,
}

// ── App state ─────────────────────────────────────────────────────────────────

struct DashboardApp {
    hexes: Vec<HexEntry>,
    stations: Vec<StationEntry>,
    lat_range: (f64, f64),
    lon_range: (f64, f64),
    /// Top-level status (idle / running / done)
    status: String,
    /// Latest relevant progress line from the running process
    live_output: String,
    process: Option<RunningProcess>,
    /// Raw text of the most recently generated report
    report_text: Option<String>,

    // ── Reports page ──────────────────────────────────────────────────────
    active_tab: ActiveTab,
    saved_reports: Vec<SavedReport>,
    report_name_input: String,
    /// Index of the report shown on the left half (or full view when no right)
    viewing: Option<usize>,
    /// Index of the report shown on the right half (side-by-side comparison)
    comparing: Option<usize>,
}

impl DashboardApp {
    fn new() -> Self {
        let hexes = load_hexes();
        let (lat_range, lon_range) = bounds(&hexes);
        let stations = load_stations();
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

    /// Drain the stdout channel and poll child exit status.
    fn poll_process(&mut self) {
        let Some(proc) = &mut self.process else { return };

        // Drain all pending lines; keep the last relevant one.
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
            Ok(None) => {} // still running
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
                // Suggest a default name for saving
                self.report_name_input = format!("Run {}", self.saved_reports.len() + 1);
                self.status = "Report ready.".into();
            }
            Err(e) => self.status = format!("Failed to run report: {e}"),
        }
    }

    fn save_current_report(&mut self) {
        let Some(content) = self.report_text.clone() else { return };
        let name = self.report_name_input.trim().to_owned();
        if name.is_empty() { return; }
        self.saved_reports.push(SavedReport { name, content });
        // Pre-fill next name
        self.report_name_input = format!("Run {}", self.saved_reports.len() + 1);
    }
}

/// Keep lines that carry per-iteration progress; skip noise / cargo build output.
fn is_progress_line(line: &str) -> bool {
    // Optimizer: "Station 3/12: hex 4521 selected — objective 1234.5"
    if line.starts_with("Station ") && line.contains("selected") {
        return true;
    }
    // Simulator: "event      10000 — sim time: day 5, 14:30"
    if line.starts_with("event ") && line.contains("sim time") {
        return true;
    }
    false
}

// ── egui update ───────────────────────────────────────────────────────────────

impl eframe::App for DashboardApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_process();
        if self.process.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }

        // ── Toolbar (always visible) ──────────────────────────────────────
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                // Tab switcher
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

        // ── Tab-specific content ──────────────────────────────────────────
        match self.active_tab {
            ActiveTab::Map => self.show_map_tab(ctx),
            ActiveTab::Reports => self.show_reports_tab(ctx),
        }
    }
}

impl DashboardApp {
    fn show_map_tab(&mut self, ctx: &egui::Context) {
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

    fn show_reports_tab(&mut self, ctx: &egui::Context) {
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
                                    // Report name as a clickable label (sets viewing)
                                    if ui.selectable_label(is_viewing, &report.name).clicked() {
                                        new_viewing = if is_viewing { None } else { Some(idx) };
                                    }
                                });
                                ui.horizontal(|ui| {
                                    // "B" button toggles comparison slot
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
            // Fixup indices
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
                    // Single report view
                    if let Some(report) = self.saved_reports.get(a) {
                        show_report_panel(ui, &report.name, &report.content);
                    }
                }
                (Some(a), Some(b)) => {
                    // Side-by-side comparison
                    let left_name    = self.saved_reports.get(a).map(|r| r.name.clone()).unwrap_or_default();
                    let left_content = self.saved_reports.get(a).map(|r| r.content.clone()).unwrap_or_default();
                    let right_name    = self.saved_reports.get(b).map(|r| r.name.clone()).unwrap_or_default();
                    let right_content = self.saved_reports.get(b).map(|r| r.content.clone()).unwrap_or_default();

                    // Split into two equal columns
                    let total_w = ui.available_width();
                    let col_w   = (total_w - 8.0) / 2.0; // 8px gap

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
}

/// Render a single named report in a scrollable monospace text area.
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

// ── Hex map rendering ─────────────────────────────────────────────────────────

fn draw_hex_map(
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

// ── Helpers ───────────────────────────────────────────────────────────────────

fn load_hexes() -> Vec<HexEntry> {
    let path = "config/hexes.json";
    let mut file = match std::fs::File::open(path) {
        Ok(f)  => f,
        Err(_) => return Vec::new(),
    };
    let mut buf = String::new();
    file.read_to_string(&mut buf).ok();
    serde_json::from_str(&buf).unwrap_or_default()
}

fn load_stations() -> Vec<StationEntry> {
    let path = "config/police_stations.json";
    let mut file = match std::fs::File::open(path) {
        Ok(f)  => f,
        Err(_) => return Vec::new(),
    };
    let mut buf = String::new();
    file.read_to_string(&mut buf).ok();
    serde_json::from_str(&buf).unwrap_or_default()
}

fn bounds(hexes: &[HexEntry]) -> ((f64, f64), (f64, f64)) {
    if hexes.is_empty() {
        return ((0.0, 1.0), (0.0, 1.0));
    }
    let lat_min = hexes.iter().map(|h| h.lat).fold(f64::MAX, f64::min);
    let lat_max = hexes.iter().map(|h| h.lat).fold(f64::MIN, f64::max);
    let lon_min = hexes.iter().map(|h| h.lon).fold(f64::MAX, f64::min);
    let lon_max = hexes.iter().map(|h| h.lon).fold(f64::MIN, f64::max);
    ((lat_min, lat_max), (lon_min, lon_max))
}

// ── Entry point ───────────────────────────────────────────────────────────────

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
