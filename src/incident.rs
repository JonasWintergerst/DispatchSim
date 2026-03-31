use crate::clock::SimTime;
use crate::types::{
    DistrictId, IncidentId, IncidentKind, IncidentStatus, NodeId, Priority, UnitId,
    UnitRequirements,
};

pub struct Incident {
    pub id: IncidentId,
    pub kind: IncidentKind,
    pub priority: Priority,
    pub location: NodeId,
    pub district: DistrictId,
    pub unit_required: UnitRequirements,
    pub spawned_at: SimTime,
    pub resolved_at: Option<SimTime>,
    pub units_assigned: Vec<UnitId>,
    pub status: IncidentStatus,
}

impl Incident {
    pub fn new(
        id: IncidentId,
        kind: IncidentKind,
        priority: Priority,
        location: NodeId,
        district: DistrictId,
        unit_required: UnitRequirements,
        spawned_at: SimTime,
    ) -> Self {
        Self {
            id,
            kind,
            priority,
            location,
            district,
            unit_required,
            spawned_at,
            resolved_at: None,
            units_assigned: vec![],
            status: IncidentStatus::Open,
        }
    }

    pub fn resolve(&mut self, time: SimTime) {
        self.resolved_at = Some(time);
        self.status = IncidentStatus::Resolved;
    }

    pub fn is_resolved(&self) -> bool {
        self.resolved_at.is_some()
    }
}
