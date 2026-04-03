# dispatch_sim — Feature Ideas

Brainstormed ideas for a production-ready government-facing application.
Highest-priority items are tracked in TASKS.md.

---

## Visualization & Situational Awareness

- **Route heatmap** — store the full `Vec<NodeId>` path per dispatch in SQLite, aggregate edge traversal counts post-run, render as colored road overlay on a real tile map (export to GeoJSON for QGIS or render in-app via a tile backend)
- **Live unit tracker** — interpolate unit position along its current route based on elapsed time; show moving dots on the map during simulation playback
- **Isochrone map** — for each station, shade all hexes reachable within N minutes; instantly shows coverage gaps and dead zones
- **Coverage gap animation** — replay a sim run as a time-lapse: color hexes red when the nearest available unit is >8 min away, green when <4 min

---

## Analytics & Reporting

- **Response time SLA compliance** — government contracts typically have targets (e.g. "Priority A: 8 min in 90% of cases"); track and report compliance rate per district, per time-of-day, per season
- **Unit utilization heatmap by hour** — which districts are understaffed on Monday mornings vs. Saturday nights; drives shift planning
- **Incident clustering** — identify recurring hotspots (H3 cells with disproportionate incident rates); informs patrol placement and station siting
- **Counterfactual "what-if" reports** — "if we add one unit to district 4, how does P95 response time change?"; run N sim variants, diff the outputs

---

## Optimizer-Driven Decision Support

- **Station placement optimizer** — given a budget (N stations, M units), find the placement minimizing average response time; expose results as a ranked list with trade-off curves
- **Unit reallocation advisor** — after a sim run, flag districts where units sat idle >70% of the time and suggest reallocation to understaffed neighbors
- **Shift scheduling optimizer** — model time-varying demand (SpawnProfile already captures this) and recommend how many units per district per shift to meet SLA targets
- **Mutual aid cost estimator** — when district A borrows a unit from district B, B's own response times degrade; quantify this trade-off

---

## Realism & Operational Fidelity

- **Multi-unit incidents** — some incidents (structure fires, major accidents) require 2–3 units; model staged arrival and resolution
- **Crew fatigue / shift handover** — units accumulate hours; force rotation after 12h shifts; model the gap during handover
- **Traffic-aware travel times** — scale OSM edge weights by hour-of-day (rush hour = slower); SpawnProfile multipliers could drive this

---

## Government / Institutional Features

- **Audit log & reproducibility** — every run is fully reproducible from seed + config; export a signed run manifest (config hash + seed + version) for regulatory accountability
- **Scenario library** — save named scenarios ("Hamburg 2025 baseline", "post-stadium-event surge") and compare them side-by-side in the dashboard
- **Budget constraint modeling** — input: annual budget; output: optimal fleet size and station layout within that budget, with SLA compliance curves
- **Export to standard formats** — GeoJSON for GIS teams, CSV for Excel-wielding administrators, PDF report generation for council presentations
- **Role-based access** (if this becomes a web app) — planners see full optimizer controls; analysts see reports; executives see summary dashboard only
