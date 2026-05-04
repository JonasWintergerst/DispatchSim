use rand::RngExt;
use serde::{Deserialize, Serialize};

use crate::hex::HexCoord;

// ---------------------------------------------------------------------------
// Inter-district messaging (Phase 2)
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
pub enum DistrictMsg {
    RequestMutualAid { from: DistrictId, incident: IncidentId, unit_type: SimType },
    IncidentResolved { incident: IncidentId },
    UnitAvailable { unit: UnitId },
    SendUnit { unit: UnitId, to_district: DistrictId, incident: IncidentId },
    UnitReturn { unit: UnitId },
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub enum SimType {
    Fire,
    Police,
    Ambulance,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
pub enum UnitStatus {
    Idle,
    Dispatched,
    OnScene,
    Returning,
    /// Cycling through a patrol route. Position is computed lazily from
    /// `Unit::patrol_started_at` and the route's cumulative segment times.
    Patrolling,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
pub enum IncidentStatus {
    Open,
    Assigned,
    Resolved,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
pub enum IncidentKind {
    Fire,
    MedicalEmergency,
    Crime,
    Accident,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
pub enum Priority {
    A,
    B,
    C,
}

impl Priority {
    /// Numeric rank for comparison: A=2 (highest), B=1, C=0 (lowest).
    pub fn rank(self) -> u8 {
        match self { Priority::A => 2, Priority::B => 1, Priority::C => 0 }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum Season {
    Winter,
    Spring,
    Summer,
    Autumn,
}

// ---------------------------------------------------------------------------
// ID newtypes
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Eq, Hash, PartialEq)]
pub struct SpawnProfileId(String);
impl SpawnProfileId {
    pub fn new(val: String) -> Self { Self(val) }
    pub fn value(&self) -> &str { &self.0 }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct UnitId(u32);
impl UnitId {
    pub fn new(val: u32) -> Self { Self(val) }
    pub fn value(self) -> u32 { self.0 }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct IncidentId(String);
impl IncidentId {
    pub fn new(val: String) -> Self { Self(val) }
    pub fn value(&self) -> &str { &self.0 }
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct NodeId(u32);
impl NodeId {
    pub fn new(val: u32) -> Self { Self(val) }
    pub fn value(self) -> u32 { self.0 }

    /// Encode (col, row) into a single u32: col in the high 16 bits, row in the low 16.
    /// Supports grids up to 65 535 wide and tall.
    pub fn from_hex(coord: &HexCoord) -> Self {
        let col = (coord.col as u32) & 0xFFFF;
        let row = (coord.row as u32) & 0xFFFF;
        NodeId::new((col << 16) | row)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Eq, PartialEq, PartialOrd, Ord, Hash, Debug)]
pub struct HexId(u32);
impl HexId {
    pub fn new(val: u32) -> Self { Self(val) }
    pub fn value(self) -> u32 { self.0 }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DistrictId(u32);
impl DistrictId {
    pub fn new(val: u32) -> Self { Self(val) }
    pub fn value(self) -> u32 { self.0 }
}

#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct StationId(u32);
impl StationId {
    pub fn new(val: u32) -> Self { Self(val) }
    pub fn value(self) -> u32 { self.0 }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PatrolRouteId(u32);
impl PatrolRouteId {
    pub fn new(val: u32) -> Self { Self(val) }
    pub fn value(self) -> u32 { self.0 }
}

/// Minimum number of units required to handle an incident.
#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct UnitRequirements(pub u8);

/// A hex on a district's boundary that is 8-adjacent to a hex in a different district.
/// Stored per district as the Phase 2 mutual-aid hook.
#[derive(Debug, Clone, Copy)]
pub struct BorderNode {
    pub node_id:            NodeId,
    pub neighbour_district: DistrictId,
}

/// Configuration for the queue-escalation sweep. Passed to districts so they
/// can bump priorities on long-waiting incidents and cancel stale ones.
#[derive(Clone, Copy)]
pub struct EscalationConfig {
    pub enabled:                  bool,
    pub interval_min:             u64,
    pub c_to_b_min:               u64,
    pub b_to_a_min:               u64,
    pub cancellation_threshold:   u64,
    pub cancellation_probability: f64,
}

/// On-scene service-time distribution for a single priority class. Params are
/// raw (Copy) so the enclosing `ServiceTimeConfig` stays cheap to pass; the
/// actual distribution object is rebuilt at each sample call.
#[derive(Clone, Copy, Debug)]
pub enum ServiceTimeDist {
    /// `rng.random_range(min..=max)` — current default, Larson/Chaiken-style.
    Uniform     { min: u64, max: u64 },
    /// Lognormal with log-space mean `mu` and log-space std `sigma`.
    /// Real-space median = exp(mu), mean = exp(mu + sigma²/2).
    Lognormal   { mu: f64, sigma: f64 },
    /// Exponential with mean duration `mean` (minutes). Memoryless baseline.
    Exponential { mean: f64 },
}

impl ServiceTimeDist {
    /// Sample an on-scene duration in whole minutes. Always >= 1 min.
    pub fn sample(&self, rng: &mut impl rand::Rng) -> u64 {
        use rand_distr::{Distribution, LogNormal, Exp};
        match *self {
            ServiceTimeDist::Uniform { min, max } => rng.random_range(min..=max),
            ServiceTimeDist::Lognormal { mu, sigma } => {
                let d = LogNormal::new(mu, sigma)
                    .expect("lognormal params must be finite and sigma > 0");
                d.sample(rng).round().max(1.0) as u64
            }
            ServiceTimeDist::Exponential { mean } => {
                let d = Exp::new(1.0 / mean).expect("exponential mean must be > 0");
                d.sample(rng).round().max(1.0) as u64
            }
        }
    }
}

/// Per-priority on-scene service-time distribution. Passed to districts so
/// `handle_arrival` can sample the right distribution for each incident.
#[derive(Clone, Copy, Debug)]
pub struct ServiceTimeConfig {
    pub a: ServiceTimeDist,
    pub b: ServiceTimeDist,
    pub c: ServiceTimeDist,
}

impl ServiceTimeConfig {
    /// Larson (1972) / Chaiken (1978) ranges — matches the pre-Phase-3 defaults.
    pub fn default_uniform() -> Self {
        Self {
            a: ServiceTimeDist::Uniform { min: 45, max: 90 },
            b: ServiceTimeDist::Uniform { min: 25, max: 55 },
            c: ServiceTimeDist::Uniform { min: 15, max: 35 },
        }
    }

    pub fn sample(&self, priority: Priority, rng: &mut impl rand::Rng) -> u64 {
        match priority {
            Priority::A => self.a.sample(rng),
            Priority::B => self.b.sample(rng),
            Priority::C => self.c.sample(rng),
        }
    }
}

/// A request from one district that has no available local unit. Emitted by
/// `District::process_events` and consumed by the City-level mutual-aid pass
/// in `City::tick`.
#[derive(Clone)]
pub struct MutualAidRequest {
    pub requesting_district: DistrictId,
    pub incident_id:         IncidentId,
    pub location:            NodeId,
    pub priority:            Priority,
    pub spawn_time:          crate::clock::SimTime,
}
