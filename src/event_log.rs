use serde::{Deserialize, Serialize};
use rusqlite::{Connection, Result, params};

use crate::types::{DistrictId, IncidentId, UnitId};

#[derive(Serialize, Deserialize, Debug)]
pub enum EventKind {
    IncidentSpawned,
    UnitDispatched,
    UnitArrived,
    IncidentResolved,
    MutualAidRequested,
    UnitReturning,
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
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                sim_time    INTEGER NOT NULL,
                kind        TEXT    NOT NULL,
                district    INTEGER NOT NULL,
                unit        INTEGER,
                incident    TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_sim_time ON events (sim_time);
            CREATE INDEX IF NOT EXISTS idx_district ON events (district);
            CREATE INDEX IF NOT EXISTS idx_kind     ON events (kind);
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
                "INSERT INTO events (sim_time, kind, district, unit, incident)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;

            for e in events {
                stmt.execute(params![
                    e.sim_time as i64,
                    e.kind.as_str(),
                    e.district.value(),
                    e.unit.map(|u| u.value()),
                    e.incident.as_ref().map(|i| i.value()),
                ])?;
            }
        }

        tx.commit()
    }
}
