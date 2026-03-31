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

- [ ] **Phase 2a — OSM road graph + A\* routing** — replace `TravelMatrix` (Chebyshev hex-distance) in `routing.rs` with a real Hamburg road graph loaded from an OSM `.pbf` file (Geofabrik). Nodes = OSM intersections (`NodeId` becomes OSM node ID u64), edges = road segments with travel time (segment length / road type speed). Parse with `osmpbf` crate; build a `petgraph::DiGraph`; route with A\* using straight-line distance as heuristic. Add `OsmRoutingEngine` with lazy per-query route caching (`RwLock<HashMap>`). Keep the synthetic hex grid as spatial unit for now.
- [ ] **Phase 2b — H3 spatial layer** — replace the synthetic `hexes.json` grid with H3 cells (`h3o` crate) over Hamburg at an appropriate resolution (~1 km cells = resolution 7–8). Each H3 cell gets a district ID and spawn profile. Each cell stores `nearest_road_node: NodeId` (snapped to nearest OSM intersection). Districts map to Hamburg Stadtteile polygons or custom bounding polygons loaded from GeoJSON.
- [ ] **Nearest-unit spatial index** — replace the linear idle-unit scan in dispatch with an `rstar` R-tree (already a dependency, currently unused) for O(log n) nearest-idle-unit lookup. Also enables cross-district searches for mutual aid.
- [ ] **Patrol routes for units** — define what a patrol route is (sequence of hex nodes) and how routes are assigned. Open questions: static per district or dynamic, how to model position mid-patrol at dispatch time. Likely approach: store route on unit state, compute position on-demand rather than via events.
- [ ] **Wire up inter-district mutual aid** — `DistrictMsg` enum and inbox/outbox channels are scaffolded in `types.rs` and `district.rs` but never used. When a district has no idle units it should broadcast `RequestMutualAid`; neighboring districts respond with `SendUnit` if they have a spare.

---

## Phase 4 — Optimization Integration

- [ ] **Programmatic config API** — expose a `SimConfig::builder()` or plain struct API so an outer optimizer can construct district layouts, unit counts, and spawn profiles without a TOML file. `City::from_config` stays for CLI use; add `City::from_sim_config(SimConfig)` as a second entry point that accepts the struct directly. Enables running the sim in a tight eval loop without file I/O.

---

## Phase 3 — Observability & Output

- [ ] **ratatui TUI dashboard** — `ratatui` is already a dependency. Live terminal view showing: sim time, events/sec, per-district unit status (idle/dispatched/on-scene), incident queue depth, and a color-coded hex-grid map. Headless by default, toggled via CLI flag.
- [x] **Post-run analysis report** — `cargo run -- report [db_path]` queries `dispatch_sim.db` and prints: response time (avg/P50/P95/max) per district, on-scene duration, unit utilization %, and incidents per hour of day.
