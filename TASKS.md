# dispatch_sim — Task Tracker

Goal: a running, meaningful emergency dispatch simulation that produces analyzable SQLite output.

Tasks are grouped by phase. Phase 0 must be completed before the simulation will compile or run.

---

## Phase 0 — Compilation Blockers

- [x] **Fix Cargo.toml edition** — change `edition = "2024"` to `"2021"` (2024 is not a valid Rust edition)
- [x] **Fix `SimEvent` enum** — add `district_id: DistrictId` to `UnitArrival` and `IncidentResolve`; add `NoOp` variant. Without these, `city.rs` cannot batch events by district and `district.rs` won't compile.
- [x] **Implement missing `District` methods** — `create_incident()` (generate ID, sample priority/kind, look up hex position), `hex(HexId)` lookup, and `resolution_duration()` (sample service time from a distribution). All three are called in `district.rs` but not defined.
- [x] **Fix `Incident`** — add a getter for `id` (currently private, accessed directly in `district.rs`); fix `is_resolved()` (always returns `true`) and `needs_more_units()` (always returns `true`) to reflect actual state; sample `kind` from `incident_weights` config instead of hardcoding `Crime`.
- [x] **Create `config/hexes.json`** — define a spatially coherent hex grid for the 7 districts (Northwest, North Central, Northeast, Southwest, Central, Southeast, South Central) with `col`, `row`, `district_id`, and `spawn_profile_id` per cell. Required to load the simulation at all.
- [x] **Seed initial `IncidentSpawn` events** — the event heap starts empty so the simulation exits immediately. `City::from_config()` must push one `IncidentSpawn` per hex (or per district) at `t=0` or drawn from the spawn profile.

---

## Phase 1 — Core Simulation Correctness

- [x] **Implement unit return-to-station** — after `IncidentResolve`, units are freed to `Idle` but remain at the incident location. They should travel back to their home station so future dispatch travel times are realistic. Currently a `TODO` in `unit.rs`.
- [x] **Implement pending-incident queue** — when `IncidentSpawn` fires and no idle unit is available, the incident is silently dropped. A queue should hold unassigned incidents; when a unit becomes free it checks the queue and auto-dispatches.

---

## Phase 2 — Realism & Performance

- [x] **Phase 2a — OSM road graph + A\* routing** — `osm.rs` parses Hamburg `.pbf` into a `RoadGraph` (petgraph DiGraph) with rstar for nearest-node snapping; `routing.rs` `RoutingEngine` replaces `TravelMatrix` with Dijkstra precompute + lazy A\* cache via `RwLock<HashMap>`.
- [x] **Phase 2b — H3 spatial layer** — `optimizer/h3_grid.rs` generates H3 cells (`h3o`) over the Hamburg GeoJSON polygon; optimizer snaps each cell to the nearest OSM road node and assigns district/spawn-profile IDs; result written to `config/hexes.json`.
- [ ] **Nearest-unit spatial index** — replace the linear idle-unit scan in dispatch with an `rstar` R-tree for O(log n) nearest-idle-unit lookup. (`rstar` is already used in `osm.rs` for node snapping but not yet in `district.rs` dispatch.) Also enables cross-district searches for mutual aid.
- [ ] **Patrol routes for units** — define what a patrol route is (sequence of hex nodes) and how routes are assigned. Open questions: static per district or dynamic, how to model position mid-patrol at dispatch time. Likely approach: store route on unit state, compute position on-demand rather than via events.
- [ ] **Wire up inter-district mutual aid** — `DistrictMsg` enum and inbox/outbox channels are scaffolded in `types.rs` and `district.rs` but never used. When a district has no idle units it should broadcast `RequestMutualAid`; neighboring districts respond with `SendUnit` if they have a spare.

---


## Phase 3 — Observability & Output

- [x] **GUI dashboard** — `src/bin/dashboard.rs` implements a live `eframe`/`egui` window (not ratatui) showing a color-coded hex map with district boundaries, station markers, and a live sim log fed via a background thread. Launched as its own binary.
- [x] **Post-run analysis report** — `cargo run -- report [db_path]` queries `dispatch_sim.db` and prints: response time (avg/P50/P95/max) per district, on-scene duration, unit utilization %, and incidents per hour of day.

---

## Phase 5 — Government Demo Milestone

- [x] **Response time SLA compliance report** — `print_sla_compliance()` in `report.rs` outputs city-wide and per-district compliance % for Priority A (≤5 min), B (≤15 min), C (≤60 min). `priority` and `incident_kind` columns added to the events table and logged on every `IncidentSpawned` event. Dashboard gets a full **Reports page** (tab-switched) with save-named-report, single-view, and side-by-side comparison of any two saved runs.
- [x] **Isochrone coverage map** — for each station, compute all hexes reachable within N minutes using the existing `RoutingEngine`; render as a shaded overlay in the dashboard. Immediately shows coverage gaps and dead zones.
- [ ] **What-if unit reallocation** — run N sim variants with different `unit_count` assignments across districts, collect SLA compliance per variant, and output a ranked comparison table. Directly actionable given the optimizer is already in-place.
- [x] **Route heatmap** — store the full `Vec<NodeId>` path per dispatch in SQLite; aggregate edge traversal counts post-run; export as GeoJSON (edge → count) for rendering in QGIS or a tile-backed dashboard overlay.
