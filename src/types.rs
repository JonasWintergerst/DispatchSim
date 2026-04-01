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
