use crate::clock::SimTime;
use crate::types::{IncidentId, NodeId, SimType, UnitId, UnitStatus};

pub struct Unit {
    pub id: UnitId,
    pub unit_type: SimType,
    pub status: UnitStatus,
    position: NodeId,
    home: NodeId,
    pub route: Vec<NodeId>,
    pub assigned_incident: Option<IncidentId>,
    pub dispatch_time: Option<SimTime>,
    pub arrival_time: Option<SimTime>,
}

impl Unit {
    pub fn new(id: UnitId, unit_type: SimType, station: NodeId) -> Self {
        Self {
            id,
            unit_type,
            status: UnitStatus::Idle,
            position: station,
            home: station,
            route: Vec::new(),
            assigned_incident: None,
            dispatch_time: None,
            arrival_time: None,
        }
    }

    /// Begin travelling to an incident.
    pub fn dispatch(
        &mut self,
        route: Vec<NodeId>,
        time: SimTime,
        arrival_time: SimTime,
        incident_id: IncidentId,
    ) {
        self.status = UnitStatus::Dispatched;
        self.route = route;
        self.dispatch_time = Some(time);
        self.arrival_time = Some(arrival_time);
        self.assigned_incident = Some(incident_id);
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
