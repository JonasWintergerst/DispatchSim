# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
cargo build                                          # Debug build
cargo build --release                                # Optimized build
cargo run --bin optimize -- config/optimize.toml     # Run district optimizer → writes config/hexes.json
cargo run --bin dispatch_sim -- config/city.toml     # Run simulation
cargo run --bin dispatch_sim -- report output/dispatch_sim.db  # Print summary report
cargo test                                           # Run all tests
cargo test <test_name>                               # Run a single test by name
cargo clippy                                         # Lint
cargo fmt                                            # Format code
```

The project has two binaries — always use `--bin`:
- **`optimize`** — p-median district optimizer; reads `config/optimize.toml`, writes `config/hexes.json`
- **`dispatch_sim`** — discrete-event simulator; reads `config/city.toml` and `config/hexes.json`

The simulation outputs a SQLite database to `./output/dispatch_sim.db`.

### Optimizer prerequisites

The optimizer requires two external files (not in the repo):

| File | Source |
|------|--------|
| `config/hamburg-latest.osm.pbf` | [Geofabrik Hamburg](https://download.geofabrik.de/europe/germany/hamburg.html) (~50 MB) |
| `config/hamburg.geojson` | Simplified boundary already in repo; replace with real boundary from [OSM Boundaries](https://osm-boundaries.com) if needed |

Typical workflow:
```bash
cargo run --bin optimize -- config/optimize.toml   # step 1: optimise
cargo run --bin dispatch_sim -- config/city.toml   # step 2: simulate
cargo run --bin dispatch_sim -- report output/dispatch_sim.db  # step 3: report
```

## Architecture

**dispatch_sim** is a discrete-event simulation of emergency services dispatch across a city divided into districts. Time is measured in simulated minutes and advances only when events are processed (no fixed-timestep loop).

> **Scope: police only.** This simulator models **police dispatch** exclusively. Every incident requires exactly one unit, `IncidentKind` variants are crime categories, and there is no multi-unit coordination logic, apparatus types, or fire/EMS-specific state. Do not propose changes shaped around fire/EMS dispatch (multi-unit response, apparatus mixing, BLS/ALS, etc.) — they don't apply here.

### Event Loop

`City` owns a `BinaryHeap<Reverse<SimEvent>>` as the event queue. Each call to `City::tick()`:
1. Pops all events at the minimum timestamp
2. Groups them by `DistrictId`
3. Dispatches each district's event batch sequentially (per-tick Rayon overhead dominates the work; parallelism happens at the whole-simulation level instead — see Key Design Decisions)
4. Collects follow-on `SimEvent`s and `MutualAidRequest`s returned by districts; mutual-aid requests are matched against neighbouring districts in a post-tick pass and lender units are dispatched cross-border
5. Pushes follow-on events back to the heap
6. Logs `Event` records to SQLite via `EventLog`

### Module Roles

| Module | Role |
|--------|------|
| `city.rs` | Owns the event heap, all districts, the clock, and the SQLite event log; drives the simulation loop |
| `district.rs` | Processes batches of events for one district; owns its `Unit`s, `Incident`s, `Station`, and `Hex`es |
| `event_queue.rs` | Defines `SimEvent` enum (`IncidentSpawn`, `UnitArrival`, `IncidentResolve`, `NoOp`) with `Ord` for heap ordering |
| `event_log.rs` | Appends `Event` rows to SQLite; events are separate from `SimEvent`s (logging vs. scheduling) |
| `spawner.rs` | Computes next incident spawn time via Exponential inter-arrival; `SpawnProfile` scales λ by hour/weekday/season |
| `routing.rs` | `TravelMatrix` maps `(NodeId, NodeId) → u32` minutes; currently Chebyshev distance on hex grid (Phase 2 will use petgraph + Dijkstra) |
| `clock.rs` | `SimClock` tracks elapsed minutes and exposes `hour_of_day()`, `day_of_week()`, `season()` for spawn scaling |
| `hex.rs` | `Hex` cells link spatial coordinates to a `DistrictId`, `SpawnProfileId`, and `NodeId` |
| `unit.rs` | `Unit` state machine: `Idle → Dispatched → OnScene → Idle` |
| `types.rs` | All newtype IDs (`UnitId`, `IncidentId`, `DistrictId`, `NodeId`, …) and shared enums |

### Event Processing Flow (per district)

- **IncidentSpawn** → create `Incident`, find a unit to dispatch (idle → returning → preempt lower-priority dispatched), emit `UnitArrival`; if no unit available, push to `pending_queue`; schedule next spawn via exponential draw
- **UnitArrival** → stale-check `dispatch_id`; if valid, set unit to `OnScene`, sample resolution duration, emit `IncidentResolve`
- **IncidentResolve** → mark incident `Resolved`; check `pending_queue` for waiting incidents — if found, dispatch unit directly from scene; otherwise emit `UnitReturn` and set unit to `Returning`
- **UnitReturn** → stale-check `dispatch_id`; if valid, move unit to home station, set `Idle`; check `pending_queue` and dispatch immediately if something is waiting
- **PatrolLoop** → stale-check `dispatch_id`; if the unit is still patrolling, schedule the next loop tick; the unit's actual position is computed lazily on demand from `patrol_started_at + route.cumulative_min` (no per-tick work)
- **ShiftChange** → log shift boundary, reschedule next `ShiftChange` at `now + 480 min`; hook for future crew-rotation logic

### Dispatch Priority & Preemption

Units are assigned using this precedence on every `IncidentSpawn`:
1. **Idle** unit — dispatched immediately
2. **Returning** unit — redirected en route back to station
3. **Preemption** — if new incident has higher priority than any currently-dispatched unit's incident, that unit is redirected; the preempted incident returns to `pending_queue`
4. **Queue** — incident added to `pending_queue` if no unit is available

`pending_queue` is a `Vec<IncidentId>`; `pop_best_pending()` always selects the highest-priority (`A > B > C`) open incident regardless of arrival order.

### Stale-Event Detection

Every `Unit` carries a `dispatch_id: u32` that increments on each `dispatch()`, `start_return()`, or `start_patrol()` call. `UnitArrival`, `UnitReturn`, and `PatrolLoop` events embed the `dispatch_id` at the time of scheduling. When these events fire, the handler compares the event's `dispatch_id` against the unit's current value — a mismatch means the unit was reassigned and the event is silently discarded. This avoids the need to remove events from the heap.

### Patrol Routes

Units configured as patrol units cycle through a closed loop of waypoints (`PatrolRoute` in `src/patrol.rs`). Routes are produced offline by the `patrol_gen` binary against a chosen `PatrolStrategy` (`hotspot`, `border`, etc.) and serialised to `config/patrol_routes_<strategy>.json`. The simulator loads them at startup via `patrol::load_routes`, compiling segment travel times against the routing cache.

Position-while-patrolling is **lazy**: the unit stores `patrol_started_at` and a shared `Arc<PatrolRoute>`, and `Unit::current_position(now)` walks `cumulative_min` to find the segment currently being traversed (O(log N), no shared state). There is no per-tick patrol update — only the periodic `PatrolLoop` event keeps the dispatch_id fresh.

When a patrolling unit is dispatched, `Unit::dispatch` snaps the persistent `position` to the lazy patrol position so travel-time computation starts from the right place.

### Mutual Aid

When a district cannot service an incident locally (no idle/returning unit and no preemptable lower-priority dispatch), it emits a `MutualAidRequest` alongside its events for the tick. After all districts process their batches, `City` runs a post-tick pass: for each request it scans neighbouring districts (within `mutual_aid_max_min` travel minutes), picks the closest idle or patrolling unit, and synthesises a cross-border dispatch on the lender's behalf via `try_accept_loan`. The lender's unit carries `loaned_to: Some(borrower_district)`, and on resolve a synthetic `IncidentResolve` is routed back to the original owner so its bookkeeping stays consistent. The lender unit returns to its own home station, not the borrower's. Lender selection is greedy by travel time and does not load-balance across candidate districts.

### Configuration

- `config/city.toml` — simulation parameters, district definitions (station + unit counts), and spawn profiles (`residential`, `commercial`, `mixed`) with λ and multiplier arrays
- `config/hexes.json` — flat JSON array produced by the optimizer; one entry per H3 cell with `h3_index`, `lat`, `lon`, `district_id`, `spawn_profile_id`, `nearest_osm_node`
- `config/optimize.toml` — optimizer parameters: `n_districts`, `h3_resolution`, area boundary, OSM path, solver algorithm, constraints

### Key Design Decisions

- **Strong newtypes for all IDs** (`UnitId(u32)`, `IncidentId(String)`, etc.) — prevents accidental mixing at compile time
- **Simulations are the unit of parallelism** — `City::tick` processes districts sequentially (they share no mutable state but per-tick Rayon overhead dominates). Throughput comes from running many whole simulations in parallel, one per thread, each reading from a shared `Arc<RoutingEngine>`. The what-if runner (`src/main.rs:run_whatif`) is the canonical example.
- **Routing is precomputed in the optimizer, not the sim** — the optimizer builds a single city-wide `RoutingEngine` from the full OSM graph (anchored on all hex nodes + station nodes across all districts) and serialises it to `output/routing_cache.bin` via `src/routing_cache.rs`. All districts share one `Arc<RoutingEngine>`, enabling seamless cross-district routing for mutual aid. The simulator refuses to start without this file; it does not load OSM itself.
- **`SimEvent` vs `Event`** — `SimEvent` drives future scheduling (heap); `Event` is an immutable log record written to SQLite

### Known Gaps / Planned Work

- `ShiftChange` logs boundaries but does not yet rotate crews or change unit availability
- `ratatui` (TUI) is a dependency planned for live visualization but not yet wired up
- Optimizer uses haversine distance proxy for the p-median objective; a road-network travel-time matrix would give more accurate results but requires full Dijkstra over the OSM graph
- Parallelization strategy (Design.txt §4): precompute routing once in the optimizer and parallelise across whole simulations (one thread per sim), not across districts within a sim. ✅ implemented — cache produced by `optimize`, consumed by `dispatch_sim` and `whatif` batch runner.
