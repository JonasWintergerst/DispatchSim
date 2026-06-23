use serde::{Deserialize, Serialize};
use rusqlite::{Connection, Result, params};

use crate::routing::RouteStats;
use crate::types::{DistrictId, IncidentId, UnitId};

// ---------------------------------------------------------------------------
// Route record
// ---------------------------------------------------------------------------

/// A single dispatch route to be persisted in `dispatch_routes`.
/// `path_json` is a JSON array of `[lon, lat]` pairs representing the full
/// road path from the unit's position to the incident location.
pub struct RouteRecord {
    pub incident_id: IncidentId,
    pub path_json:   String,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum EventKind {
    IncidentSpawned,
    UnitDispatched,
    UnitArrived,
    IncidentResolved,
    MutualAidRequested,
    UnitReturning,
    UnitReturned,
    ShiftStarted,
    PatrolStarted,
    IncidentEscalated,
    IncidentCancelled,
}

impl EventKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EventKind::IncidentSpawned    => "IncidentSpawned",
            EventKind::UnitDispatched     => "UnitDispatched",
            EventKind::UnitArrived        => "UnitArrived",
            EventKind::IncidentResolved   => "IncidentResolved",
            EventKind::MutualAidRequested => "MutualAidRequested",
            EventKind::UnitReturning      => "UnitReturning",
            EventKind::UnitReturned       => "UnitReturned",
            EventKind::ShiftStarted       => "ShiftStarted",
            EventKind::PatrolStarted      => "PatrolStarted",
            EventKind::IncidentEscalated  => "IncidentEscalated",
            EventKind::IncidentCancelled  => "IncidentCancelled",
        }
    }
}

/// Which dispatch path produced a `UnitDispatched` event. `Idle`, `Patrolling`,
/// `Returning`, and `Preempt` are the four branches of the spawn-time dispatch
/// rule (closest available unit, then preempt a lower-priority dispatch).
/// `MutualAid` is a cross-border loan; `Queued` is a freed unit sent straight
/// to a previously-queued incident.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchSource {
    Idle,
    Patrolling,
    Returning,
    Preempt,
    MutualAid,
    Queued,
}

impl DispatchSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            DispatchSource::Idle       => "Idle",
            DispatchSource::Patrolling => "Patrolling",
            DispatchSource::Returning  => "Returning",
            DispatchSource::Preempt    => "Preempt",
            DispatchSource::MutualAid  => "MutualAid",
            DispatchSource::Queued     => "Queued",
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Event {
    pub sim_time: u64,
    pub kind: EventKind,
    pub district: DistrictId,
    pub unit: Option<UnitId>,
    pub incident: Option<IncidentId>,
    /// "A", "B", or "C" — populated only for IncidentSpawned events.
    pub priority: Option<String>,
    /// "Fire", "MedicalEmergency", "Crime", "Accident" — populated only for IncidentSpawned.
    pub incident_kind: Option<String>,
    /// Which dispatch path was taken — populated only for UnitDispatched events.
    pub dispatch_source: Option<DispatchSource>,
}

pub struct EventLog {
    conn: Connection,
}

impl EventLog {
    /// Open a fresh database file, removing any existing file at that path.
    pub fn open(path: &str) -> Result<Self> {
        let _ = std::fs::remove_file(path); // ignore error if file doesn't exist
        let conn = Connection::open(path)?;

        conn.execute_batch("
            PRAGMA journal_mode = WAL;

            CREATE TABLE IF NOT EXISTS events (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                sim_time      INTEGER NOT NULL,
                kind          TEXT    NOT NULL,
                district      INTEGER NOT NULL,
                unit          INTEGER,
                incident      TEXT,
                priority      TEXT,
                incident_kind TEXT,
                dispatch_source TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_sim_time  ON events (sim_time);
            CREATE INDEX IF NOT EXISTS idx_district  ON events (district);
            CREATE INDEX IF NOT EXISTS idx_kind      ON events (kind);
            CREATE INDEX IF NOT EXISTS idx_incident  ON events (incident);
            CREATE INDEX IF NOT EXISTS idx_priority  ON events (priority);

            CREATE TABLE IF NOT EXISTS dispatch_routes (
                incident_id TEXT PRIMARY KEY,
                path        TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS routing_stats (
                source_forward INTEGER NOT NULL,
                source_reverse INTEGER NOT NULL,
                exact_computed INTEGER NOT NULL,
                exact_cached   INTEGER NOT NULL,
                haversine      INTEGER NOT NULL
            );
        ")?;

        Ok(Self { conn })
    }

    /// Persist the travel-time resolution counters (single-row table; the
    /// previous row, if any, is replaced).
    pub fn write_routing_stats(&mut self, s: &RouteStats) -> Result<()> {
        self.conn.execute("DELETE FROM routing_stats", [])?;
        self.conn.execute(
            "INSERT INTO routing_stats
               (source_forward, source_reverse, exact_computed, exact_cached, haversine)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                s.source_forward as i64,
                s.source_reverse as i64,
                s.exact_computed as i64,
                s.exact_cached   as i64,
                s.haversine      as i64,
            ],
        )?;
        Ok(())
    }

    /// Flush a batch of events in a single transaction.
    /// Call this once per tick rather than per-event.
    pub fn insert_batch(&mut self, events: &[Event]) -> Result<()> {
        if events.is_empty() { return Ok(()); }

        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO events (sim_time, kind, district, unit, incident, priority, incident_kind, dispatch_source)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;

            for e in events {
                stmt.execute(params![
                    e.sim_time as i64,
                    e.kind.as_str(),
                    e.district.value(),
                    e.unit.map(|u| u.value()),
                    e.incident.as_ref().map(|i| i.value()),
                    e.priority.as_deref(),
                    e.incident_kind.as_deref(),
                    e.dispatch_source.as_ref().map(|s| s.as_str()),
                ])?;
            }
        }

        tx.commit()
    }

    /// Flush a batch of dispatch routes in a single transaction.
    pub fn insert_routes_batch(&mut self, routes: &[RouteRecord]) -> Result<()> {
        if routes.is_empty() { return Ok(()); }

        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR IGNORE INTO dispatch_routes (incident_id, path) VALUES (?1, ?2)",
            )?;

            for r in routes {
                stmt.execute(params![r.incident_id.value(), r.path_json])?;
            }
        }

        tx.commit()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db_path(tag: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        std::env::temp_dir()
            .join(format!("dispatch_sim_{tag}_{nanos}.db"))
            .to_string_lossy().into_owned()
    }

    fn cleanup(p: &str) {
        let _ = std::fs::remove_file(p);
        let _ = std::fs::remove_file(format!("{p}-wal"));
        let _ = std::fs::remove_file(format!("{p}-shm"));
    }

    /// `write_routing_stats` persists the counters and keeps exactly one row
    /// across repeated writes (so the report reads the latest snapshot, not a
    /// growing history).
    #[test]
    fn routing_stats_round_trip_and_replace() {
        let p = temp_db_path("routing_stats");
        cleanup(&p);

        let mut log = EventLog::open(&p).expect("open db");
        let stats = RouteStats {
            source_forward: 10, source_reverse: 5,
            exact_computed: 3, exact_cached: 2, haversine: 1,
        };
        log.write_routing_stats(&stats).expect("write stats");

        let conn = Connection::open(&p).expect("reopen");
        let read = |c: &Connection| c.query_row(
            "SELECT source_forward, source_reverse, exact_computed, exact_cached, haversine
             FROM routing_stats",
            [],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?, r.get::<_, i64>(4)?)),
        ).unwrap();
        assert_eq!(read(&conn), (10, 5, 3, 2, 1));

        // A second write replaces the row rather than appending.
        log.write_routing_stats(&stats).expect("rewrite");
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM routing_stats", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(count, 1, "write_routing_stats must keep a single row");

        drop(conn);
        cleanup(&p);
    }
}
