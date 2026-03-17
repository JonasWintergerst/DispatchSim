

use crate::types::{UnitStatus, UnitId, SimType, IncidentId, NodeId};


pub struct Unit {
    pub id: UnitId,
    unit_type: SimType, // Fire/Police/Ambulance
    pub status: UnitStatus,
    position: NodeId,
    pub route: Vec<NodeId>,
    assigned_incident: Option<IncidentId>,
}

impl Unit {
    pub fn new(id: UnitId, unit_type: SimType, position: NodeId) -> Self {
        Self {
            id,
            unit_type,
            status: UnitStatus::Idle,
            position,
            route: Vec::new(),
            assigned_incident: None,
        }
    }
    pub fn assign(&mut self, id: IncidentId) {
        self.status = UnitStatus::Dispatched;
        self.assigned_incident = Some(id);
    }
    pub fn clear_assignment(&mut self) {
        self.status = UnitStatus::Idle;
        self.assigned_incident = None;
    }
    pub fn advance_route(&mut self) -> Option<NodeId> {

        self.position = self.route.pop().unwrap();

        //TODO if position = Incident position ...

        return Some(self.position)
    }
    pub fn get_status(&self) -> UnitStatus { self.status }
    pub fn get_position(&self) -> NodeId { self.position }
}