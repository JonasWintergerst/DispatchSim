use serde::{Serialize, Deserialize};

use crate::types::{EventKind, DistrictId, UnitId, IncidentId};


#[derive(Serialize, Deserialize)]
pub struct EventLog;




#[derive(Serialize, Deserialize)]
pub struct Event {
    pub sim_time: u64,
    pub kind: EventKind,
    pub district: DistrictId,
    pub unit: Option<UnitId>,
    pub incident: Option<IncidentId>,
}

impl EventLog {
    pub fn new() -> Self { EventLog }
    pub fn init() {}
    pub fn insert_batch(&self, events: &[Event]) {}

    pub fn print_events(events: Vec<Event>) {
        for event in events.iter() {
            println!(
                "Event {{ sim_time: {}, kind: {:?}, district: {:?}, unit: {:?}, incident: {:?} }}",
                event.sim_time,
                event.kind,
                event.district,
                event.unit,
                event.incident
            );
        }
    }
}