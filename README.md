# dispatch_sim

A discrete-event simulation of emergency services dispatch, paired with a mathematical district optimizer. Model a city, co-optimize station locations and district boundaries, then evaluate the layout under realistic incident demand.

Built in Rust. Uses real OpenStreetMap road data and H3 geospatial indexing.

---

## What it does

**Optimizer** (`optimize` binary) solves a [p-median facility location](https://en.wikipedia.org/wiki/Facility_location_problem) problem over real H3 hex cells derived from a GeoJSON area boundary:

- Generates ~7 500 H3 cells at ~174 m resolution covering the configured area
- Snaps each cell centre to its nearest OSM road node (R-tree, O(log n))
- Runs a greedy p-median solver to co-optimize station positions and hex-to-district assignments, minimizing weighted travel time
- Enforces contiguity (each district is a single connected region) and workload balance constraints
- Writes `config/hexes.json` consumed by the simulator

**Simulator** (`dispatch_sim` binary) runs a parallel discrete-event simulation over the optimized layout:

- Units cycle through `Idle → Dispatched → OnScene → Returning → Idle`
- Priority dispatch with preemption (Priority A > B > C)
- Incident demand driven by per-hex Poisson processes with hour-of-day, day-of-week, and season multipliers calibrated to German/EU policing benchmarks (~300 calls/day for a city of 300 000)
- Districts process events in parallel via Rayon; the event heap is shared
- All events logged to a SQLite database for post-hoc analysis

---

## Architecture

```
src/
├── main.rs                  # dispatch_sim binary entry point
├── lib.rs                   # shared library root
├── bin/
│   └── optimize.rs          # optimize binary entry point
├── optimizer/
│   ├── mod.rs               # Solver trait, Problem/Solution types, hexes.json writer
│   ├── greedy.rs            # Greedy p-median solver with contiguity + workload repair
│   └── h3_grid.rs           # H3 cell generation from GeoJSON polygon (BFS)
├── city.rs                  # Event heap, districts, clock, SQLite log
├── district.rs              # Per-district event processing (runs in parallel)
├── event_queue.rs           # SimEvent enum + heap ordering
├── event_log.rs             # SQLite append log
├── routing.rs               # RoadGraph (petgraph), RoutingEngine, Dijkstra travel matrix
├── osm.rs                   # OSM PBF parser → RoadGraph + R-tree for nearest-node
├── spawner.rs               # Exponential inter-arrival + SpawnProfile scaling
├── hex.rs                   # Hex struct (H3 index, lat/lon, district, OSM node)
├── unit.rs                  # Unit state machine
├── incident.rs              # Incident record
├── station.rs               # Station struct
├── clock.rs                 # SimClock (elapsed minutes → hour/day/season)
├── config.rs                # TOML + JSON config loading
├── report.rs                # Terminal summary report from SQLite
├── geo_utils.rs             # Haversine distance
└── types.rs                 # Newtype IDs (UnitId, IncidentId, DistrictId, NodeId, …)
```

### Event flow (per district, per tick)

```
IncidentSpawn  →  dispatch best idle/returning/preemptable unit  →  UnitArrival
UnitArrival    →  unit OnScene, sample duration                  →  IncidentResolve
IncidentResolve→  unit returns or takes next pending incident    →  UnitReturn / UnitArrival
UnitReturn     →  unit Idle, drain pending queue
ShiftChange    →  log shift boundary, reschedule +480 min
```

Stale events (unit reassigned between scheduling and firing) are detected by a `dispatch_id` counter and silently dropped — no heap modification needed.

---

## Getting started

### Prerequisites

- Rust (stable, 2021 edition)
- Hamburg OSM data (for the optimizer): download `hamburg-latest.osm.pbf` from [Geofabrik](https://download.geofabrik.de/europe/germany/hamburg.html) and place it at `config/hamburg-latest.osm.pbf`

### Build

```bash
cargo build --release
```

### Run

```bash
# Step 1 — optimize district layout (requires OSM PBF)
cargo run --bin optimize -- config/optimize.toml

# Step 2 — simulate
cargo run --bin dispatch_sim -- config/city.toml

# Step 3 — report
cargo run --bin dispatch_sim -- report output/dispatch_sim.db
```

### Test

```bash
cargo test
```

---

## Configuration

### `config/optimize.toml`

```toml
n_districts     = 7
h3_resolution   = 9          # resolution 9 ≈ 174 m edge, ~7 500 hexes for Hamburg
area_geojson    = "config/hamburg.geojson"
osm_path        = "config/hamburg-latest.osm.pbf"
hex_output_path = "config/hexes.json"

[constraints]
contiguity         = true
max_workload_ratio = 1.5     # max district load / mean load

[objective]
travel_time_weight      = 1.0
workload_balance_weight = 0.2

[solver]
algorithm = "greedy"         # "greedy" | "simulated_annealing" (SA not yet implemented)
```

### `config/city.toml`

Defines districts (id, station, unit count), spawn profiles (λ, hour/weekday/season multipliers, incident type weights), and simulation parameters (duration, RNG seed, OSM path for routing).

The scenario is calibrated to a ~300 000-resident European city running a police-only dispatch model (~300 calls/day, 23 units across 7 districts, ~38% utilisation).

### `config/hexes.json`

Flat JSON array produced by the optimizer — one entry per H3 cell:

```json
[
  {
    "h3_index": 613195413336981503,
    "lat": 53.5503,
    "lon": 9.9936,
    "district_id": 2,
    "spawn_profile_id": "residential",
    "nearest_osm_node": 4821
  }
]
```

---

## Optimizer algorithm

The greedy p-median solver runs in O(p × n²) time:

1. **Distance matrix** — n × n haversine distances, computed in parallel (Rayon)
2. **Greedy station selection** — iteratively pick the candidate that maximally reduces weighted travel cost; ties broken by spawn-rate weighting
3. **Voronoi assignment** — each hex goes to its nearest station
4. **Contiguity repair** — BFS from each station; disconnected hexes are reassigned to the nearest adjacent district
5. **Workload balance repair** — border-swap iteration until max/mean load ratio ≤ configured limit

For resolution 9 (~7 500 hexes) the solver completes in a few seconds on a modern CPU.

---

## Output

The simulator writes a SQLite database to `./output/dispatch_sim.db` with one row per event (spawn, dispatch, arrival, resolve, return, shift change). The `report` subcommand prints a per-district summary:

- Total incidents dispatched
- Mean and 90th-percentile response time (minutes)
- Unit utilisation (%)
- Queued (unserved) incident count

---

## Current limitations / planned work

- **Solver**: only the greedy algorithm is implemented; simulated annealing is scaffolded but not yet written
- **Routing**: the simulator uses real OSM road times via Dijkstra per district; the optimizer uses haversine as a travel-time proxy — a network-distance objective would improve solution quality
- **Shift changes**: logged but crew rotation not yet modelled
- **Mutual aid**: inter-district dispatch is scaffolded (`DistrictMsg`) but not wired up
- **Patrol positions**: units return to station between calls; mid-patrol positioning not modelled

---

## Dependencies

| Crate | Purpose |
|-------|---------|
| `h3o` | H3 geospatial indexing |
| `petgraph` | Road graph (Dijkstra routing) |
| `osmpbf` | OSM PBF parser |
| `rstar` | R-tree for O(log n) nearest-node queries |
| `rayon` | Data parallelism (event processing, distance matrix) |
| `geo` | Polygon containment (H3 cell generation) |
| `rusqlite` | SQLite event log |
| `rand` / `rand_distr` | Exponential inter-arrival sampling |
| `serde` / `toml` / `serde_json` | Config and data serialisation |
