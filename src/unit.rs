use crate::clock::SimTime;
use crate::types::{IncidentId, NodeId, SimType, UnitId, UnitStatus};

pub struct Unit {
    pub id: UnitId,
    pub unit_type: SimType,
    pub status: UnitStatus,
    position: NodeId,
    home: NodeId,
    pub assigned_incident: Option<IncidentId>,
    pub dispatch_time: Option<SimTime>,
    pub arrival_time: Option<SimTime>,
    /// Monotonically increasing per unit. Carried on UnitArrival/UnitReturn events so
    /// stale events (from preempted dispatches) can be detected and discarded.
    pub dispatch_id: u32,
}

impl Unit {
    pub fn new(id: UnitId, unit_type: SimType, station: NodeId) -> Self {
        Self {
            id,
            unit_type,
            status: UnitStatus::Idle,
            position: station,
            home: station,
            assigned_incident: None,
            dispatch_time: None,
            arrival_time: None,
            dispatch_id: 0,
        }
    }

    /// Begin travelling to an incident. Returns the new dispatch_id, which must be
    /// embedded in the paired UnitArrival event for stale-event detection.
    pub fn dispatch(
        &mut self,
        time: SimTime,
        arrival_time: SimTime,
        incident_id: IncidentId,
    ) -> u32 {
        self.dispatch_id += 1;
        self.status = UnitStatus::Dispatched;
        self.dispatch_time = Some(time);
        self.arrival_time = Some(arrival_time);
        self.assigned_incident = Some(incident_id);
        self.dispatch_id
    }

    /// Begin returning to the home station after an incident resolves.
    /// Returns the new dispatch_id for the paired UnitReturn event.
    pub fn start_return(&mut self) -> u32 {
        self.dispatch_id += 1;
        self.status = UnitStatus::Returning;
        self.assigned_incident = None;
        self.dispatch_id
    }

    /// Mark the unit as on-scene and update its position to the incident location.
    pub fn arrive(&mut self, location: NodeId) {
        self.status = UnitStatus::OnScene;
        self.position = location;
    }

    /// Teleport position back to the home station (called when `UnitReturn` fires).
    /// Only takes effect if the unit has not been re-dispatched since the event was scheduled.
    pub fn return_to_station(&mut self) {
        self.position = self.home;
    }

    pub fn position(&self) -> NodeId { self.position }
    pub fn home(&self) -> NodeId { self.home }
}
