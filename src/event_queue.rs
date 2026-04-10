use std::cmp::Ordering;

use crate::clock::SimTime;
use crate::types::{DistrictId, HexId, IncidentId, UnitId};

pub enum SimEvent {
    IncidentSpawn   { time: SimTime, hex_id: HexId, district_id: DistrictId },
    UnitArrival     { time: SimTime, unit_id: UnitId, incident_id: IncidentId, district_id: DistrictId, dispatch_id: u32 },
    IncidentResolve { time: SimTime, incident_id: IncidentId, district_id: DistrictId },
    /// Unit has finished travelling and arrives at its home station.
    UnitReturn      { time: SimTime, unit_id: UnitId, district_id: DistrictId, dispatch_id: u32 },
    /// One full patrol cycle has elapsed; the handler restarts the loop if the
    /// unit is still patrolling. Stale-checked via `dispatch_id`.
    PatrolLoop      { time: SimTime, unit_id: UnitId, district_id: DistrictId, dispatch_id: u32 },
    /// Periodic shift boundary — logs the change and schedules the next one.
    ShiftChange     { time: SimTime, district_id: DistrictId },
    /// Periodic sweep: escalate long-waiting pending incidents and cancel stale ones.
    QueueEscalation { time: SimTime, district_id: DistrictId },
    /// Sentinel for event handlers that have no follow-on simulation event.
    /// Filtered out in City::tick and never pushed to the heap.
    NoOp,
}

impl SimEvent {
    pub fn time(&self) -> Option<SimTime> {
        match self {
            SimEvent::IncidentSpawn   { time, .. } => Some(*time),
            SimEvent::UnitArrival     { time, .. } => Some(*time),
            SimEvent::IncidentResolve { time, .. } => Some(*time),
            SimEvent::UnitReturn      { time, .. } => Some(*time),
            SimEvent::PatrolLoop      { time, .. } => Some(*time),
            SimEvent::ShiftChange     { time, .. } => Some(*time),
            SimEvent::QueueEscalation { time, .. } => Some(*time),
            SimEvent::NoOp            => None,
        }
    }

    pub fn district_id(&self) -> Option<DistrictId> {
        match self {
            SimEvent::IncidentSpawn   { district_id, .. } => Some(*district_id),
            SimEvent::UnitArrival     { district_id, .. } => Some(*district_id),
            SimEvent::IncidentResolve { district_id, .. } => Some(*district_id),
            SimEvent::UnitReturn      { district_id, .. } => Some(*district_id),
            SimEvent::PatrolLoop      { district_id, .. } => Some(*district_id),
            SimEvent::ShiftChange     { district_id, .. } => Some(*district_id),
            SimEvent::QueueEscalation { district_id, .. } => Some(*district_id),
            SimEvent::NoOp            => None,
        }
    }
}

impl Ord for SimEvent {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.time(), other.time()) {
            (Some(a), Some(b)) => a.cmp(&b),
            (None, None)       => Ordering::Equal,
            (Some(_), None)    => Ordering::Less,   // timed events sort before NoOp
            (None, Some(_))    => Ordering::Greater,
        }
    }
}

impl PartialOrd for SimEvent {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}

impl PartialEq for SimEvent {
    fn eq(&self, other: &Self) -> bool { self.cmp(other) == Ordering::Equal }
}

impl Eq for SimEvent {}
