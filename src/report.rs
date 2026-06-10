// report.rs
// Queries dispatch_sim.db and prints a post-run analysis report to stdout.
// Invoked via:  dispatch_sim report [path/to/dispatch_sim.db]

use std::collections::HashMap;
use rusqlite::{Connection, Result};

pub fn print_report(db_path: &str) -> Result<()> {
    let conn = Connection::open(db_path)?;

    // Ensure the incident index exists on databases created before this was added.
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_incident ON events (incident);"
    )?;

    println!("\n=== dispatch_sim Analysis Report ===");
    println!("Database: {}\n", db_path);

    print_overview(&conn)?;
    print_sla_compliance(&conn)?;
    print_response_times(&conn)?;
    print_on_scene_duration(&conn)?;
    print_utilization(&conn)?;
    print_hourly_incidents(&conn)?;
    print_routing_resolution(&conn)?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Travel-time resolution breakdown
// ---------------------------------------------------------------------------

/// Show how each `travel_time` query was answered: the precomputed anchor↔anchor
/// matrix, exact A* (computed/cached), or haversine fallback. This makes the
/// routing model observable — a healthy run is ~100% matrix with ~0 haversine,
/// since every dispatch endpoint is a hex anchor.
fn print_routing_resolution(conn: &Connection) -> Result<()> {
    // The table is absent on DBs created before this instrumentation existed.
    let table_exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='routing_stats'",
        [], |r| r.get(0),
    )?;
    if table_exists == 0 {
        return Ok(());
    }

    let row = conn.query_row(
        "SELECT source_forward, source_reverse, exact_computed, exact_cached, haversine
         FROM routing_stats",
        [],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?, r.get::<_, i64>(4)?)),
    );
    let (fwd, rev, exact, cached, hav) = match row {
        Ok(t)  => t,
        Err(_) => return Ok(()), // table present but empty
    };

    let total = fwd + rev + exact + cached + hav;
    if total == 0 {
        return Ok(());
    }
    let pct = |x: i64| x as f64 / total as f64 * 100.0;

    // `rev` (the old symmetry-reverse tier) is always 0 with the full matrix;
    // fold it into the matrix total so older DBs still sum correctly.
    let matrix = fwd + rev;
    println!("\nTravel-Time Resolution  (how each routing query was answered)");
    println!("  {:<36} {:>14} {:>8}", "Tier", "N", "%");
    println!("  {:<36} {:>14} {:>7.1}%", "Precomputed matrix",               fmt_int(matrix), pct(matrix));
    println!("  {:<36} {:>14} {:>7.1}%", "Exact A* (computed)",               fmt_int(exact),  pct(exact));
    println!("  {:<36} {:>14} {:>7.1}%", "Exact A* (cached)",                 fmt_int(cached), pct(cached));
    println!("  {:<36} {:>14} {:>7.1}%", "Haversine fallback (disconnected)", fmt_int(hav),    pct(hav));
    println!("  {:<36} {:>14}", "Total", fmt_int(total));

    println!(
        "\n  Matrix: {:.1}%   Exact A*: {:.1}%   Haversine: {:.2}%",
        pct(matrix), pct(exact + cached), pct(hav),
    );
    if hav > 0 {
        println!("  ⚠ {} queries hit the haversine estimate (disconnected node pairs).", fmt_int(hav));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Overview
// ---------------------------------------------------------------------------

fn print_overview(conn: &Connection) -> Result<()> {
    let total_events: i64 =
        conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?;

    let sim_duration: Option<i64> =
        conn.query_row("SELECT MAX(sim_time) FROM events", [], |r| r.get(0))?;

    let spawned: i64 = conn.query_row(
        "SELECT COUNT(*) FROM events WHERE kind = 'IncidentSpawned'",
        [], |r| r.get(0),
    )?;

    let resolved: i64 = conn.query_row(
        "SELECT COUNT(*) FROM events WHERE kind = 'IncidentResolved'",
        [], |r| r.get(0),
    )?;

    let duration_min = sim_duration.unwrap_or(0);
    let duration_yr  = duration_min as f64 / (60.0 * 24.0 * 365.0);

    println!("Simulation");
    println!("  Duration:      {:>12} min  ({:.2} yr)", fmt_int(duration_min), duration_yr);
    println!("  Events logged: {:>12}", fmt_int(total_events));

    let escalated: i64 = conn.query_row(
        "SELECT COUNT(*) FROM events WHERE kind = 'IncidentEscalated'",
        [], |r| r.get(0),
    )?;

    let cancelled: i64 = conn.query_row(
        "SELECT COUNT(*) FROM events WHERE kind = 'IncidentCancelled'",
        [], |r| r.get(0),
    )?;

    let open    = spawned - resolved - cancelled;
    let open_pct = if spawned > 0 { open as f64 / spawned as f64 * 100.0 } else { 0.0 };
    let cancel_pct = if spawned > 0 { cancelled as f64 / spawned as f64 * 100.0 } else { 0.0 };
    println!("\nIncidents");
    println!("  Spawned:       {:>12}", fmt_int(spawned));
    println!("  Resolved:      {:>12}", fmt_int(resolved));
    println!("  Escalated:     {:>12}", fmt_int(escalated));
    println!("  Cancelled:     {:>12}  ({:.1}% self-resolved)", fmt_int(cancelled), cancel_pct);
    println!("  Open / queued: {:>12}  ({:.1}% unresolved)", fmt_int(open), open_pct);

    Ok(())
}

// ---------------------------------------------------------------------------
// SLA compliance  (response time vs. target by priority)
// ---------------------------------------------------------------------------

fn print_sla_compliance(conn: &Connection) -> Result<()> {
    // Silently add columns if this DB was created before priority logging was added.
    let _ = conn.execute_batch("ALTER TABLE events ADD COLUMN priority TEXT;");
    let _ = conn.execute_batch("ALTER TABLE events ADD COLUMN incident_kind TEXT;");

    let has_data: i64 = conn.query_row(
        "SELECT COUNT(*) FROM events WHERE kind='IncidentSpawned' AND priority IS NOT NULL",
        [], |r| r.get(0),
    )?;

    if has_data == 0 {
        println!("\nSLA Compliance — no priority data (re-run simulation)");
        return Ok(());
    }

    // SLA targets: Priority A ≤ 5 min, B ≤ 15 min, C ≤ 60 min
    // (German Einsatz 1/2/3 / APCO CAD standard)
    const TARGETS: &[(&str, i64)] = &[("A", 5), ("B", 15), ("C", 60)];

    let mut stmt = conn.prepare(
        "SELECT s.priority, a.district, (a.sim_time - s.sim_time) AS rt
         FROM events s
         JOIN events a ON s.incident = a.incident
         WHERE s.kind = 'IncidentSpawned'
           AND a.kind = 'UnitArrived'
           AND s.priority IS NOT NULL
         ORDER BY s.priority, a.district",
    )?;

    let rows: Vec<(String, i64, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .filter_map(|r| r.ok())
        .collect();

    if rows.is_empty() {
        println!("\nSLA Compliance — no dispatched-incident data");
        return Ok(());
    }

    // ── City-wide summary per priority ───────────────────────────────────
    println!("\nSLA Compliance  (first-unit response time vs. target)");
    println!("  {:<12} {:>8} {:>10} {:>10} {:>8} {:>8}",
             "Priority", "Target", "N", "Met SLA", "%", "Avg RT");

    for (prio, target) in TARGETS {
        let times: Vec<i64> = rows.iter()
            .filter(|(p, _, _)| p == prio)
            .map(|(_, _, rt)| *rt)
            .collect();
        if times.is_empty() { continue; }
        let n         = times.len() as i64;
        let met       = times.iter().filter(|&&rt| rt <= *target).count() as i64;
        let pct       = met as f64 / n as f64 * 100.0;
        let avg       = times.iter().sum::<i64>() as f64 / n as f64;
        println!("  {:<12} {:>7}m {:>10} {:>10} {:>7.1}% {:>8.1}",
                 format!("Priority {prio}"), target,
                 fmt_int(n), fmt_int(met), pct, avg);
    }

    // ── Per-district breakdown ───────────────────────────────────────────
    let mut by_district: std::collections::BTreeMap<i64, std::collections::HashMap<String, Vec<i64>>> =
        std::collections::BTreeMap::new();
    for (prio, district, rt) in &rows {
        by_district
            .entry(*district)
            .or_default()
            .entry(prio.clone())
            .or_default()
            .push(*rt);
    }

    println!("\n  Per-District Breakdown");
    println!("  {:<10} {:<6} {:>8} {:>8} {:>8} {:>8}",
             "District", "Prio", "N", "Met SLA", "%", "Avg RT");

    for (district, by_prio) in &by_district {
        for (prio, target) in TARGETS {
            let Some(times) = by_prio.get(*prio) else { continue };
            let n   = times.len() as i64;
            let met = times.iter().filter(|&&rt| rt <= *target).count() as i64;
            let pct = met as f64 / n as f64 * 100.0;
            let avg = times.iter().sum::<i64>() as f64 / n as f64;
            println!("  {:<10} {:<6} {:>8} {:>8} {:>7.1}% {:>8.1}",
                     district, prio, fmt_int(n), fmt_int(met), pct, avg);
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Response times (spawn → unit arrival)
// ---------------------------------------------------------------------------

fn print_response_times(conn: &Connection) -> Result<()> {
    // Load (district, response_minutes) for all resolved incidents.
    let mut stmt = conn.prepare(
        "SELECT a.district, (a.sim_time - s.sim_time) AS response_min
         FROM events s
         JOIN events a ON s.incident = a.incident
         WHERE s.kind = 'IncidentSpawned' AND a.kind = 'UnitArrived'
         ORDER BY a.district",
    )?;

    let rows: Vec<(i64, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    if rows.is_empty() {
        println!("\nResponse Times — no data");
        return Ok(());
    }

    println!("\nResponse Time  (spawn → unit arrival, minutes)");
    println!("  {:<10} {:>8} {:>8} {:>8} {:>8} {:>8}",
             "District", "N", "Avg", "P50", "P95", "Max");

    let mut by_district: std::collections::BTreeMap<i64, Vec<i64>> = std::collections::BTreeMap::new();
    for (district, rt) in &rows {
        by_district.entry(*district).or_default().push(*rt);
    }

    for (district, mut times) in by_district {
        times.sort_unstable();
        let n   = times.len();
        let avg = times.iter().sum::<i64>() as f64 / n as f64;
        let p50 = times[n / 2];
        let p95 = times[(n as f64 * 0.95) as usize];
        let max = times[n - 1];
        println!("  {:<10} {:>8} {:>8.1} {:>8} {:>8} {:>8}",
                 district, n, avg, p50, p95, max);
    }

    // City-wide totals
    let mut all: Vec<i64> = rows.iter().map(|(_, rt)| *rt).collect();
    all.sort_unstable();
    let n   = all.len();
    let avg = all.iter().sum::<i64>() as f64 / n as f64;
    let p50 = all[n / 2];
    let p95 = all[(n as f64 * 0.95) as usize];
    let max = all[n - 1];
    println!("  {:<10} {:>8} {:>8.1} {:>8} {:>8} {:>8}",
             "ALL", n, avg, p50, p95, max);

    Ok(())
}

// ---------------------------------------------------------------------------
// On-scene duration (unit arrival → resolved)
// ---------------------------------------------------------------------------

fn print_on_scene_duration(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT (r.sim_time - a.sim_time) AS scene_min
         FROM events a
         JOIN events r ON a.incident = r.incident
         WHERE a.kind = 'UnitArrived' AND r.kind = 'IncidentResolved'",
    )?;

    let mut times: Vec<i64> = stmt
        .query_map([], |r| r.get(0))?
        .filter_map(|r| r.ok())
        .collect();

    if times.is_empty() {
        println!("\nOn-Scene Duration — no data");
        return Ok(());
    }

    times.sort_unstable();
    let n   = times.len();
    let avg = times.iter().sum::<i64>() as f64 / n as f64;
    let p50 = times[n / 2];
    let p95 = times[(n as f64 * 0.95) as usize];
    let max = times[n - 1];

    println!("\nOn-Scene Duration  (unit arrival → resolved, minutes)");
    println!("  {:<8} {:>8} {:>8} {:>8} {:>8}",
             "N", "Avg", "P50", "P95", "Max");
    println!("  {:<8} {:>8.1} {:>8} {:>8} {:>8}",
             n, avg, p50, p95, max);

    Ok(())
}

// ---------------------------------------------------------------------------
// Unit utilization
// ---------------------------------------------------------------------------

fn print_utilization(conn: &Connection) -> Result<()> {
    // busy_minutes per unit = sum of (IncidentResolved.sim_time - UnitDispatched.sim_time)
    // for matching incident IDs, grouped by unit.
    let mut stmt = conn.prepare(
        "SELECT d.district, d.unit, SUM(r.sim_time - d.sim_time) AS busy_min
         FROM events d
         JOIN events r ON d.incident = r.incident
         WHERE d.kind = 'UnitDispatched' AND r.kind = 'IncidentResolved'
           AND d.unit IS NOT NULL
         GROUP BY d.district, d.unit
         ORDER BY d.district, d.unit",
    )?;

    let rows: Vec<(i64, i64, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .filter_map(|r| r.ok())
        .collect();

    let sim_duration: Option<i64> =
        conn.query_row("SELECT MAX(sim_time) FROM events", [], |r| r.get(0))?;

    let duration = sim_duration.unwrap_or(1).max(1);

    if rows.is_empty() {
        println!("\nUnit Utilization — no data");
        return Ok(());
    }

    println!("\nUnit Utilization  (busy time / sim duration)");
    println!("  {:<10} {:>6} {:>10} {:>10} {:>10}",
             "District", "Units", "Avg Busy%", "Min Busy%", "Max Busy%");

    let mut by_district: std::collections::BTreeMap<i64, Vec<f64>> = std::collections::BTreeMap::new();
    for (district, _unit, busy_min) in &rows {
        let pct = *busy_min as f64 / duration as f64 * 100.0;
        by_district.entry(*district).or_default().push(pct);
    }

    let mut total_pcts: Vec<f64> = Vec::new();
    for (district, pcts) in &by_district {
        let n   = pcts.len();
        let avg = pcts.iter().sum::<f64>() / n as f64;
        let min = pcts.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = pcts.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        total_pcts.extend(pcts);
        println!("  {:<10} {:>6} {:>10.1} {:>10.1} {:>10.1}",
                 district, n, avg, min, max);
    }

    let n   = total_pcts.len();
    let avg = total_pcts.iter().sum::<f64>() / n as f64;
    let min = total_pcts.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = total_pcts.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    println!("  {:<10} {:>6} {:>10.1} {:>10.1} {:>10.1}",
             "ALL", n, avg, min, max);

    Ok(())
}

// ---------------------------------------------------------------------------
// Incidents per hour of day
// ---------------------------------------------------------------------------

fn print_hourly_incidents(conn: &Connection) -> Result<()> {
    // Count spawns per hour of day across the full simulation.
    // Divide by the number of full days simulated to get a per-day rate.
    let mut stmt = conn.prepare(
        "SELECT (sim_time / 60) % 24 AS hour, COUNT(*) AS n
         FROM events
         WHERE kind = 'IncidentSpawned'
         GROUP BY hour
         ORDER BY hour",
    )?;

    let hourly: Vec<(i64, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    if hourly.is_empty() {
        println!("\nHourly Distribution — no data");
        return Ok(());
    }

    let sim_duration: Option<i64> =
        conn.query_row("SELECT MAX(sim_time) FROM events", [], |r| r.get(0))?;

    let days = (sim_duration.unwrap_or(0) as f64 / 1440.0).max(1.0);

    println!("\nIncidents by Hour of Day  (avg per simulated day)");
    println!("  {:<6} {:>8} {:>8}", "Hour", "Total", "Per Day");

    let mut counts = [0i64; 24];
    for (hour, n) in &hourly {
        if *hour >= 0 && *hour < 24 {
            counts[*hour as usize] = *n;
        }
    }

    for (hour, count) in counts.iter().enumerate() {
        let per_day = *count as f64 / days;
        println!("  {:02}:00  {:>8} {:>8.1}", hour, fmt_int(*count), per_day);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Route heatmap export
// ---------------------------------------------------------------------------

/// Read all dispatch routes from `db_path`, aggregate edge traversal counts,
/// and write a GeoJSON FeatureCollection to `output_path`.
///
/// Each feature is a LineString (one road segment) with a `count` property
/// indicating how many times that segment was traversed across all dispatches.
/// Suitable for loading in QGIS or any GeoJSON-capable tile renderer.
pub fn export_heatmap(db_path: &str, output_path: &str) -> Result<()> {
    let conn = Connection::open(db_path)?;

    // Ensure the table exists (may be missing on old DBs).
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS dispatch_routes (incident_id TEXT PRIMARY KEY, path TEXT NOT NULL);"
    )?;

    let route_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM dispatch_routes", [], |r| r.get(0))?;

    if route_count == 0 {
        eprintln!("heatmap: no routes in database — run the simulation first");
        return Ok(());
    }

    // Microdegree coordinate pair — used as a hashable, order-independent edge key.
    type Coord    = (i64, i64);
    type EdgeKey  = (Coord, Coord);

    let mut edge_counts: HashMap<EdgeKey, u64> = HashMap::new();

    let mut stmt = conn.prepare("SELECT path FROM dispatch_routes")?;
    let paths: Vec<String> = stmt
        .query_map([], |r| r.get(0))?
        .filter_map(|r| r.ok())
        .collect();

    for path_json in &paths {
        let coords: Vec<[f64; 2]> = match serde_json::from_str(path_json) {
            Ok(c) => c,
            Err(_) => continue,
        };
        for window in coords.windows(2) {
            let a = to_microdeg(window[0][0], window[0][1]);
            let b = to_microdeg(window[1][0], window[1][1]);
            let key: EdgeKey = if a <= b { (a, b) } else { (b, a) };
            *edge_counts.entry(key).or_insert(0) += 1;
        }
    }

    println!(
        "heatmap: {} routes → {} unique edges",
        fmt_int(route_count),
        fmt_int(edge_counts.len() as i64),
    );

    // Build GeoJSON manually — avoids adding a dependency.
    let mut features: Vec<String> = Vec::with_capacity(edge_counts.len());
    for ((a, b), count) in &edge_counts {
        let lon_a = a.0 as f64 / 1_000_000.0;
        let lat_a = a.1 as f64 / 1_000_000.0;
        let lon_b = b.0 as f64 / 1_000_000.0;
        let lat_b = b.1 as f64 / 1_000_000.0;
        features.push(format!(
            r#"{{"type":"Feature","geometry":{{"type":"LineString","coordinates":[[{lon_a},{lat_a}],[{lon_b},{lat_b}]]}},"properties":{{"count":{count}}}}}"#,
        ));
    }

    let geojson = format!(
        r#"{{"type":"FeatureCollection","features":[{}]}}"#,
        features.join(",")
    );

    std::fs::write(output_path, geojson)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

    println!("heatmap: written to {}", output_path);
    Ok(())
}

/// Convert a (lon, lat) float pair to integer microdegrees for use as a map key.
fn to_microdeg(lon: f64, lat: f64) -> (i64, i64) {
    ((lon * 1_000_000.0).round() as i64, (lat * 1_000_000.0).round() as i64)
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

fn fmt_int(n: i64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 { result.push(','); }
        result.push(ch);
    }
    result.chars().rev().collect()
}
