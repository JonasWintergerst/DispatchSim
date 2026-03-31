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

- [ ] **Graph-based routing** — replace the Chebyshev hex-distance stub in `routing.rs` (`create_route()` returns `[]`, `route_travel_time()` returns `0`) with a proper hex-adjacency graph + Dijkstra using `petgraph` (already a dependency).
- [ ] **Nearest-unit spatial index** — replace the linear idle-unit scan in dispatch with an `rstar` R-tree (already a dependency, currently unused) for O(log n) nearest-idle-unit lookup. Also enables cross-district searches for mutual aid.
- [ ] **Wire up inter-district mutual aid** — `DistrictMsg` enum and inbox/outbox channels are scaffolded in `types.rs` and `district.rs` but never used. When a district has no idle units it should broadcast `RequestMutualAid`; neighboring districts respond with `SendUnit` if they have a spare.

---

## Phase 3 — Observability & Output

- [ ] **ratatui TUI dashboard** — `ratatui` is already a dependency. Live terminal view showing: sim time, events/sec, per-district unit status (idle/dispatched/on-scene), incident queue depth, and a color-coded hex-grid map. Headless by default, toggled via CLI flag.
- [ ] **Post-run analysis report** — add a CLI subcommand (or auto-print) that queries `dispatch_sim.db` for: average response time by district/type/priority, unit utilization rates, incidents-per-hour histogram, queue wait times. Validates that outputs are sensible.
