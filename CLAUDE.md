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

- **IncidentSpawn** → create `Incident`, find nearest idle `Unit`, call `TravelMatrix::route_between`, dispatch unit, emit `UnitArrival` at `now + travel_time`, schedule next spawn via exponential draw
- **UnitArrival** → set unit to `OnScene`, sample resolution duration, emit `IncidentResolve`
- **IncidentResolve** → mark incident `Resolved`, free unit back to `Idle`, emit `NoOp`

### Configuration

- `config/city.toml` — simulation parameters, district definitions (station + unit counts), and spawn profiles (`residential`, `commercial`, `mixed`) with λ and multiplier arrays
- `config/hexes.json` — hex grid layout: one entry per cell with `col`, `row`, `district_id`, `spawn_profile_id`

### Key Design Decisions

- **Strong newtypes for all IDs** (`UnitId(u32)`, `IncidentId(String)`, etc.) — prevents accidental mixing at compile time
- **Districts are the unit of parallelism** — each district processes its events independently; inter-district coordination (mutual aid) is stubbed via `DistrictMsg` inbox/outbox but not yet implemented
- **`SimEvent` vs `Event`** — `SimEvent` drives future scheduling (heap); `Event` is an immutable log record written to SQLite

### Known Phase 2 Gaps

- `routing.rs` returns direct-hop routes with zero intermediate nodes; will be replaced by OSM road graph + Dijkstra
- Units do not return to station after `IncidentResolve` (TODO in code)
- `rstar` (R-tree) and `ratatui` (TUI) are dependencies imported but unused — planned for spatial indexing and visualization respectively
- Inter-district mutual aid dispatch is scaffolded but not wired up
