# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
cargo build                   # Debug build
cargo build --release         # Optimized build
cargo run -- config/city.toml # Run simulation (default config path)
cargo test                    # Run all tests
cargo test <test_name>        # Run a single test by name
cargo clippy                  # Lint
cargo fmt                     # Format code
```

The simulation outputs a SQLite database to `./output/dispatch_sim.db`.

## Architecture

**dispatch_sim** is a discrete-event simulation of emergency services (Fire/Police/Ambulance) dispatch across a city divided into districts. Time is measured in simulated minutes and advances only when events are processed (no fixed-timestep loop).

### Event Loop

`City` owns a `BinaryHeap<Reverse<SimEvent>>` as the event queue. Each call to `City::tick()`:
1. Pops all events at the minimum timestamp
2. Groups them by `DistrictId`
3. Dispatches each district's event batch **in parallel** via Rayon (districts share no mutable state during processing)
4. Collects follow-on `SimEvent`s returned by districts and pushes them back to the heap
5. Logs `Event` records to SQLite via `EventLog`

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
- **ShiftChange** → log shift boundary, reschedule next `ShiftChange` at `now + 480 min`; hook for future crew-rotation logic

### Dispatch Priority & Preemption

Units are assigned using this precedence on every `IncidentSpawn`:
1. **Idle** unit — dispatched immediately
2. **Returning** unit — redirected en route back to station
3. **Preemption** — if new incident has higher priority than any currently-dispatched unit's incident, that unit is redirected; the preempted incident returns to `pending_queue`
4. **Queue** — incident added to `pending_queue` if no unit is available

`pending_queue` is a `Vec<IncidentId>`; `pop_best_pending()` always selects the highest-priority (`A > B > C`) open incident regardless of arrival order.

### Stale-Event Detection

Every `Unit` carries a `dispatch_id: u32` that increments on each `dispatch()` or `start_return()` call. `UnitArrival` and `UnitReturn` events embed the `dispatch_id` at the time of scheduling. When these events fire, the handler compares the event's `dispatch_id` against the unit's current value — a mismatch means the unit was reassigned and the event is silently discarded. This avoids the need to remove events from the heap.

### Configuration

- `config/city.toml` — simulation parameters, district definitions (station + unit counts), and spawn profiles (`residential`, `commercial`, `mixed`) with λ and multiplier arrays
- `config/hexes.json` — hex grid layout: one entry per cell with `col`, `row`, `district_id`, `spawn_profile_id`

### Key Design Decisions

- **Strong newtypes for all IDs** (`UnitId(u32)`, `IncidentId(String)`, etc.) — prevents accidental mixing at compile time
- **Districts are the unit of parallelism** — each district processes its events independently; inter-district coordination (mutual aid) is stubbed via `DistrictMsg` inbox/outbox but not yet implemented
- **`SimEvent` vs `Event`** — `SimEvent` drives future scheduling (heap); `Event` is an immutable log record written to SQLite

### Known Gaps / Planned Work

- `routing.rs` returns direct-hop routes with Chebyshev distance; will be replaced by OSM road graph + Dijkstra (petgraph already a dependency)
- `ShiftChange` logs boundaries but does not yet rotate crews or change unit availability
- Patrol routes are not yet modelled — open question on route generation and mid-patrol position
- `rstar` (R-tree) and `ratatui` (TUI) are dependencies imported but unused — planned for spatial indexing and visualization
- Inter-district mutual aid is scaffolded (`DistrictMsg` in `types.rs`) but not wired up
- `TravelMatrix` is an O(N²) precomputed HashMap; acceptable for Phase 1 but will be replaced once routing is graph-based
