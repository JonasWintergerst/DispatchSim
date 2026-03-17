use crate::types::{ NodeId, StationId, StationName, SimType, UnitId };

pub struct Station {
    pub id: StationId,
    pub name: StationName,
    pub station_type: SimType,
    pub location: NodeId,
    pub home_unit_ids: Vec<UnitId>,
}

impl Station {
    pub fn new(
        id: StationId,
        name: String,
        station_type: SimType,
        location: NodeId,
        home_unit_ids: Vec<UnitId>,
    ) -> Self {
        Station { 
            id,
            name: StationName::new(name),
            station_type, 
            location,
            home_unit_ids,
        }
    }
}
