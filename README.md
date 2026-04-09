# dispatch_sim

A discrete-event simulation of emergency services dispatch, paired with a mathematical district optimizer. Model a city, optimize station-to-district assignments using real OSM police station data, then evaluate the layout under realistic incident demand.

Built in Rust. Uses real OpenStreetMap road data and H3 geospatial indexing.

---

## What it does

**Optimizer** (`optimize` binary) solves a [p-median facility location](https://en.wikipedia.org/wiki/Facility_location_problem) problem over real H3 hex cells, using real police stations extracted from OSM as the candidate facility set:

- Extracts real police station locations from the OSM PBF and writes them to `config/police_stations.json`
- Generates ~7 500 H3 cells at ~174 m resolution covering Hamburg
- Snaps each cell centre and each candidate station to its nearest OSM road node (R-tree, O(log n))
- Runs a greedy p-median solver to select p stations from the candidate set and assign hexes to the nearest selected station, minimizing weighted travel time
- Enforces contiguity (each district is a single connected region) and workload balance constraints
- Writes `config/hexes.json` (hex → district assignment) and `config/districts.json` (district → selected station)

**Dashboard** (`dashboard` binary) provides a live GUI for the full workflow:

- Color-coded hex map showing district boundaries and station markers
- One-click buttons to run the optimizer, simulator, and report generator
- Save named reports and compare any two side-by-side

**Simulator** (`dispatch_sim` binary) runs a parallel discrete-event simulation over the optimized layout:

- Reads station locations from `config/districts.json` — units home to real, OSM-snapped station nodes
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
│   ├── optimize.rs          # optimize binary entry point
│   └── dashboard/           # dashboard GUI binary
│       ├── main.rs           # app state, toolbar, tab dispatch, entry point
│       ├── map_tab.rs        # hex map rendering, station markers, legend
│       ├── reports_tab.rs    # report list, save/compare, side-by-side view
│       ├── process.rs        # child-process spawning and stdout streaming
│       ├── data.rs           # hex/station JSON loading
│       └── palette.rs        # 16-color district palette
├── optimizer/
│   ├── mod.rs               # Solver trait, Problem/Solution/CandidateStation types, JSON writers
│   ├── greedy.rs            # Greedy p-median solver (candidate-based + hex fallback)
│   └── h3_grid.rs           # H3 cell generation from polygon (BFS)
├── city.rs                  # Event heap, districts, clock, SQLite log
├── district.rs              # Per-district event processing (runs in parallel)
├── event_queue.rs           # SimEvent enum + heap ordering
├── event_log.rs             # SQLite append log
├── routing.rs               # RoadGraph (petgraph), RoutingEngine, Dijkstra travel matrix
├── osm.rs                   # OSM PBF parser → RoadGraph + R-tree + police station extraction
├── spawner.rs               # Exponential inter-arrival + SpawnProfile scaling
├── hex.rs                   # Hex struct (H3 index, lat/lon, district, OSM node)
├── unit.rs                  # Unit state machine
├── incident.rs              # Incident record
├── station.rs               # Station struct
├── clock.rs                 # SimClock (elapsed minutes → hour/day/season)
├── config.rs                # TOML + JSON config loading (city.toml, hexes.json, districts.json)
├── report.rs                # Terminal summary report from SQLite
├── geo_utils.rs             # Haversine distance
└── types.rs                 # Newtype IDs (UnitId, IncidentId, DistrictId, NodeId, …)
```

### Data flow

```
OSM PBF
  ├─→ extract_police_stations()  →  config/police_stations.json  (candidate set)
  ├─→ extract_admin_boundary()   →  Hamburg polygon
  └─→ OsmGraph (road network)

optimizer (p-median):
  demand:     H3 hexes (lat/lon, spawn_rate)
  facilities: candidate stations snapped to OSM nodes
  output:     config/hexes.json      (hex → district_id)
              config/districts.json  (district_id → selected station + OSM node)

simulator:
  reads hexes.json        (incident spawning, district assignment)
  reads districts.json    (station location → unit home node)
```

### Event flow (per district, per tick)

```
IncidentSpawn   →  dispatch best idle/returning/preemptable unit  →  UnitArrival
UnitArrival     →  unit OnScene, sample duration                  →  IncidentResolve
IncidentResolve →  unit returns or takes next pending incident    →  UnitReturn / UnitArrival
UnitReturn      →  unit Idle, drain pending queue
ShiftChange     →  log shift boundary, reschedule +480 min
```

Stale events (unit reassigned between scheduling and firing) are detected by a `dispatch_id` counter and silently dropped — no heap modification needed.

---

## Performance

Measured on AMD Ryzen 5 2600 (6 cores, 3.4 GHz), 16 GB RAM, Windows 11 — release build, 24 districts, Hamburg OSM (757 k road nodes, 1.7 M edges), 7 500 H3 cells at resolution 9, 4 simulated years.

| Phase | Time |
|-------|------|
| Optimizer (OSM load, p-median, adjacency, repair) | **14.2 s** |
| Simulator setup (routing cache load) | **0.8 s** |
| 4-year simulation (~1.63 M events) | **93.8 s** (~17.4 k events/s) |
| Peak memory | **~220 MB** |

```bash
cargo build --release
cargo run --release --bin optimize   -- config/optimize.toml
cargo run --release --bin dispatch_sim -- config/city.toml
```

---

## Getting started

### Prerequisites

- Rust (stable, 2021 edition)
- Hamburg OSM data: download `hamburg-latest.osm.pbf` from [Geofabrik](https://download.geofabrik.de/europe/germany/hamburg.html) and place it at `config/hamburg-latest.osm.pbf`

### Build

```bash
cargo build --release
```

### Run

```bash
# Step 1 — optimize district layout (requires OSM PBF)
#   Writes config/police_stations.json, config/hexes.json, config/districts.json
cargo run --bin optimize -- config/optimize.toml

# Step 2 — simulate
cargo run --bin dispatch_sim -- config/city.toml

# Step 3 — report
cargo run --bin dispatch_sim -- report output/dispatch_sim.db

# Or use the dashboard GUI (runs all steps via buttons)
cargo run --bin dashboard
```

### Test

```bash
cargo test
```

---

## Configuration

### `config/optimize.toml`

```toml
n_districts             = 24
h3_resolution           = 9          # resolution 9 ≈ 174 m edge, ~7 500 hexes for Hamburg
osm_path                = "config/hamburg-latest.osm.pbf"
hex_output_path         = "config/hexes.json"

# Station candidates for the p-median solver.
# Defaults to police_stations.json (extracted from OSM by the optimizer itself).
# Supply a custom file with additional candidate locations to expand the search space.
station_candidates_path = "config/police_stations.json"

# Output path for the district → station mapping consumed by the simulator.
districts_output_path   = "config/districts.json"

[constraints]
contiguity         = true
max_workload_ratio = 1.5     # max district load / mean load

[objective]
travel_time_weight      = 1.0
workload_balance_weight = 0.2

[solver]
algorithm = "greedy"
```

### `config/city.toml`

Defines districts (id, name, unit count), spawn profiles (λ, hour/weekday/season multipliers, incident type weights), and simulation parameters (duration, RNG seed, OSM path for routing).

Station names and locations are **not** defined here — they come from `config/districts.json` written by the optimizer.

```toml
hex_grid_path  = "config/hexes.json"
districts_path = "config/districts.json"

[[districts]]
id         = 0
name       = "PK 11"
unit_count = 3
```

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

### `config/districts.json`

Produced by the optimizer — one entry per district, mapping it to the selected station:

```json
[
  {
    "district_id": 0,
    "station_name": "Polizeikommissariat 14",
    "station_lat": 53.5547,
    "station_lon": 9.9845,
    "station_osm_node": 12345
  }
]
```

### `config/police_stations.json`

Extracted from the OSM PBF by the optimizer on each run. Used as the default candidate set for the p-median solver. Can be replaced with a custom file (see `station_candidates_path` in `optimize.toml`) to add or change candidate locations.

---

## Optimizer algorithm

The greedy p-median solver selects p stations from the candidate set in O(p × m × n) time (m candidates, n hexes):

1. **Distance matrix** — m × n haversine distances from each candidate to each hex, computed in parallel (Rayon)
2. **Greedy station selection** — iteratively pick the candidate that maximally reduces total weighted travel cost
3. **Voronoi assignment** — each hex goes to its nearest selected candidate
4. **Contiguity repair** — BFS from each station's anchor hex; disconnected hexes are reassigned to the nearest adjacent district
5. **Workload balance repair** — border-swap iteration until max/mean load ratio ≤ configured limit

For resolution 9 (~7 500 hexes, ~24 candidates) the solver completes in a few seconds on a modern CPU.

A fallback hex-based path (any hex can be a station) is used when `candidate_stations` is empty, preserving backward compatibility.

---

## Output

The simulator writes a SQLite database to `./output/dispatch_sim.db` with one row per event (spawn, dispatch, arrival, resolve, return, shift change).

### Report

```bash
cargo run --release --bin dispatch_sim -- report output/dispatch_sim.db
```

Prints a terminal summary covering:

- **Simulation overview** — duration, total events logged
- **Incidents** — spawned, resolved, open/queued count
- **SLA compliance** — city-wide and per-district compliance % for Priority A (≤5 min), B (≤15 min), C (≤60 min)
- **Response time per district** — N, mean, P50, P95, max (spawn → unit arrival, minutes)
- **On-scene duration** — mean, P50, P95, max (arrival → resolved)
- **Unit utilisation per district** — average, min, and max busy % per unit
- **Incidents by hour of day** — total and per-day average across the simulation

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
| `eframe` / `egui` | Dashboard GUI (GPU-accelerated immediate-mode) |
| `serde` / `toml` / `serde_json` | Config and data serialisation |
