
use crate::clock::{SimClock, SimTime, TimeContext};
use crate::event_log::{Event, EventKind};
use crate::incident::Incident;
use crate::types::{DistrictId, DistrictMsg, IncidentStatus, NodeId, Priority, SpawnProfileId, StationId, StationName, UnitStatus};
use crate::routing::RoadGraph;
use crate::hex::Hex;
use crate::station::Station;
use crate::spawner::SpawnProfile;

use crate::routing::create_route;
use crate::config::DistrictConfig;
use crate::unit::Unit;

use std::collections::HashMap;



pub struct District {
    id: DistrictId,
    station: Station,
    pub units: Vec<Unit>,
    incidents: Vec<Incident>,
    local_graph: RoadGraph,
    pub hexes: Vec<Hex>,
    inbox: Vec<DistrictMsg>,
    outbox: Vec<DistrictMsg>,
}


impl District {
    pub fn tick(&mut self, time_context:TimeContext, profiles: &HashMap<SpawnProfileId, SpawnProfile>) -> Vec<Event> {
        // create empty Events vec
        let mut events: Vec<Event> = vec![];

        // Incident spawning
        events.extend(self.spawn_incidents(time_context, profiles));

        // Assign Units
        for incident in self.incidents.iter_mut() {
            if incident.get_status() == IncidentStatus::Open {

                if let Some(unit) = self.units.iter_mut()
                    .find(|u| u.status == UnitStatus::Idle)
                {
                    let route = create_route(unit.get_position(), incident.location);
                    Self::dispatch(unit, &incident, route);
                    events.push(Event {
                        sim_time: time_context.current_time.as_minutes(),
                        kind: EventKind::UnitDispatched,
                        district: self.id,
                        unit: Some(self.units[0].id),
                        incident: Some(incident.get_id()),
                    });
                }
            }
        }

        self.advance_routes();

        return events
    }

    fn dispatch_pending(&self) {}
    //fn drain_outbox() -> Vec<DistrictMsg> {}
    pub fn recive(&mut self, msg: DistrictMsg) {

    }

    fn spawn_incidents(&mut self, time_context:TimeContext, profiles: &HashMap<SpawnProfileId, SpawnProfile>) -> Vec<Event> {
        let mut events: Vec<Event> = vec![];
        for hex in self.hexes.iter_mut() {
            let profile = profiles.get(&hex.spawn_profile_id).expect("missing profile, for id");

            let new_incidents = profile.spawn(&time_context, hex.nearest_road_node, self.id);
            for i in new_incidents.iter() {
                events.push(Event {
                    sim_time: time_context.current_time.as_minutes(),
                    kind: EventKind::IncidentSpawned,
                    district: self.id,
                    unit: None,
                    incident: Some(i.get_id()),
                });
            }
            self.incidents.extend(new_incidents);
            
            

        }
        return events;
    }

    fn is_available(unit: &Unit) -> bool { return unit.get_status() == UnitStatus::Idle }

    fn dispatch(unit: &mut Unit, incident: &Incident, route: Vec<NodeId>) { 
        unit.assign(incident.get_id().clone());
        unit.route = route;
        unit.status = UnitStatus::Dispatched;
    }

    fn advance_routes(&mut self) {
        for unit in self.units.iter_mut() {
            if unit.get_status() == UnitStatus::Dispatched {
                unit.advance_route(); // TODO return node if it is on the incendent node need to impl reaktion
            }
        } 
    }


    pub fn new(district_id: DistrictId, station: Station, units: Vec<Unit>, hexes: Vec<Hex>) -> Self {
        District {
            id: district_id,
            hexes: hexes,
            station,
            local_graph: RoadGraph::new(),
            units: units,
            incidents: Vec::new(),
            inbox: Vec::new(),
            outbox: Vec::new(),
        }
    }
}
