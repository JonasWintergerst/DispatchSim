use crate::types::{NodeId, SimType, StationId, UnitId};

pub struct Station {
    pub id: StationId,
    pub name: String,
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
        Station { id, name, station_type, location, home_unit_ids }
    }
}
