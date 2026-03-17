use serde::{Serialize, Deserialize};

use crate::hex::HexCoord;


#[derive(Serialize, Deserialize)]
pub enum DistrictMsg {
    // upward
    RequestMutualAid { from: DistrictId, incident: IncidentId, unit_type: SimType},
    IncidentResolved { incident: IncidentId },
    UnitAvailable { unit: UnitId },

    //downward
    SendUnit { unit: UnitId, to_district: DistrictId, incident: IncidentId },
    UnitReturn { unit: UnitId },
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub enum SimType { Fire, Police, Ambulance }

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
pub enum UnitStatus { Idle, Dispatched, OnScene }

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
pub enum IncidentStatus { Resolved, Assigned, Open }

#[derive(Serialize, Deserialize)]
pub enum IncidentKind { Fire, MedicalEmergency, Crime, Accident }

#[derive(Serialize, Deserialize)]
pub enum Priority { A, B, C }

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum Season { Winter, Spring, Summer, Autumn }

#[derive(Serialize, Deserialize, Clone, Eq, Hash, PartialEq)]
pub struct SpawnProfileId(String);
impl SpawnProfileId {
    pub fn new(val: String) -> Self {
        Self(val)
    }
    
    pub fn value(&self) -> String {
        self.0.clone()
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub struct UnitId(u32);
impl UnitId {
    pub fn new(val: u32) -> Self {
        Self(val)
    }
    
    pub fn value(&self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IncidentId(String);
impl IncidentId {
    pub fn new(val: String) -> Self {
        Self(val)
    }
    
    pub fn value(&self) -> String {
        self.0.clone()
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Hash, PartialEq, Eq)]
pub struct NodeId(u32);
impl NodeId {
    pub fn new(val: u32) -> Self {
        Self(val)
    }
    
    pub fn value(&self) -> u32 {
        self.0
    }

    pub fn from_hex(coord: &HexCoord) -> Self {
        NodeId::new((coord.col * 1000 + coord.row * 10) as u32)
    }
}

#[derive(Serialize, Deserialize)]
pub struct HexId(u32);
impl HexId {
    pub fn new(val: u32) -> Self {
        Self(val)
    }
    
    pub fn value(&self) -> u32 {
        self.0
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]

pub struct DistrictId(u32);
impl DistrictId {
    pub fn new(val: u32) -> Self {
        Self(val)
    }
    
    pub fn value(&self) -> u32 {
        self.0
    }
}

#[derive(Serialize, Deserialize)]

pub struct StationId(u32);
impl StationId {
    pub fn new(val: u32) -> Self {
        Self(val)
    }
    
    pub fn value(&self) -> u32 {
        self.0
    }
}

#[derive(Serialize, Deserialize)]

pub struct StationName(pub String);
impl StationName {
    pub fn new(val: String) -> Self {
        Self(val)
    }
    
    pub fn value(&self) -> String {
        self.0.clone()
    }
}

#[derive(Serialize, Deserialize)]

pub struct UnitRequirements(pub u16);

#[derive(Serialize, Deserialize)]

pub struct Duration(u32);