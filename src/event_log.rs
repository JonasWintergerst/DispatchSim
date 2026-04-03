use serde::{Deserialize, Serialize};
use rusqlite::{Connection, Result, params};

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
                incident_kind TEXT
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
        ")?;

        Ok(Self { conn })
    }

    /// Flush a batch of events in a single transaction.
    /// Call this once per tick rather than per-event.
    pub fn insert_batch(&mut self, events: &[Event]) -> Result<()> {
        if events.is_empty() { return Ok(()); }

        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO events (sim_time, kind, district, unit, incident, priority, incident_kind)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
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
