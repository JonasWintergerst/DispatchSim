use serde::{Serialize, Deserialize};


#[derive(Serialize, Deserialize)]
pub enum DistrictMsg {
    // upward
    RequestMutualAid { from: DistrictId, incident: IncidentId, unit_type: SimeType},
    IncidentResolved { incident: IncidentId },
    UnitAvailable { unit: UnitId },

    //downward
    SendUnit { unit: UnitId, to_district: DistrictId, incident: IncidentId },
    UnitReturn { unit: UnitId },
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub enum SimeType { Fire, Police, Ambulance }

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
pub enum UnitStatus { Idle, Dispatched, OnScene }

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
pub enum IncidentStatus { Resolved, Assigned, Open }

#[derive(Serialize, Deserialize)]
pub enum IncidentKind { Fire, MedicalEmergency, Crime, Accident }

#[derive(Serialize, Deserialize)]
pub enum Priority { A, B, C }

#[derive(Serialize, Deserialize, Debug)]
pub enum EventKind { Else, SpawnedIncident, AssignedUnit, ResolvedIncident }

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum Season { Winter, Spring, Summer, Autumn }

#[derive(Serialize, Deserialize, Clone, Copy, Eq, Hash, PartialEq)]
pub struct SpawnProfileId(u8);
impl SpawnProfileId {
    pub fn new() -> Self { SpawnProfileId(1) }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub struct UnitId(u32);

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub struct IncidentId(pub u32);

#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct NodeId(u32);

#[derive(Serialize, Deserialize)]
pub struct HexId(u32);

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]

pub struct DistrictId(pub u32);

#[derive(Serialize, Deserialize)]

pub struct StationId(pub u32);

#[derive(Serialize, Deserialize)]

pub struct StationName(pub String);

#[derive(Serialize, Deserialize)]

pub struct UnitRequirements(pub u16);

#[derive(Serialize, Deserialize)]

pub struct Duration(u32);