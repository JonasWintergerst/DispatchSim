use crate::types::{ NodeId, StationId, StationName, SimeType, UnitId };

pub struct Station {
    pub id: StationId,
    pub name: StationName,
    pub station_type: SimeType,
    pub home_unit_ids: Vec<UnitId>,
}

impl Station {
    pub fn from_config(id: StationId ) -> Self {
        Station { 
            id,
            name: StationName{ 0: "test".to_string()},
            station_type: SimeType::Police, 
            home_unit_ids: Vec::new(),
        }
    }
}
