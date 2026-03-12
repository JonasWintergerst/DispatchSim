

use crate::types::{DistrictId, IncidentId, IncidentKind, IncidentStatus, NodeId, Priority, UnitId, UnitRequirements};
use crate::clock::SimTime;


pub struct Incident {
    id: IncidentId,
    kind: IncidentKind, //Fire/MedicalEmergency/Crime/Accident/...
    priority: Priority,
    pub location: NodeId,
    district: DistrictId,
    unit_required: UnitRequirements,
    spawned_at: SimTime,
    resolved_at: Option<SimTime>,
    units_assigned: Vec<UnitId>,
    status: IncidentStatus,
}

impl Incident {
    pub fn new(
        id: IncidentId, 
        priority: Priority, 
        location: NodeId, 
        district: DistrictId, 
        unit_required: UnitRequirements,
        spawned_at: SimTime) -> Self {
            Self { 
                id, 
                kind: IncidentKind::Crime, 
                priority, 
                location, 
                district, 
                unit_required, 
                spawned_at, 
                resolved_at: None, 
                units_assigned: vec![], 
                status: IncidentStatus::Open }
        }

    pub fn is_resolved(&self) -> bool { true }
    pub fn resolve(&mut self, time: SimTime) {
        self.resolved_at = Some(time);
    }
    pub fn needs_more_units(&self) -> bool { true }
    pub fn get_status(&self) -> IncidentStatus { self.status }
    pub fn get_id(&self) -> IncidentId { self.id }
}