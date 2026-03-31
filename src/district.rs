use std::collections::{HashMap, VecDeque};

use rand::RngExt;
use rand::rngs::SmallRng;

use crate::clock::SimTime;
use crate::event_log::{Event, EventKind};
use crate::event_queue::SimEvent;
use crate::hex::Hex;
use crate::incident::Incident;
use crate::routing::{RoadGraph, TravelMatrix};
use crate::spawner::{SpawnProfile, next_spawn_time};
use crate::station::Station;
use crate::types::{
    DistrictId, HexId, IncidentId, IncidentKind, Priority, SpawnProfileId, UnitId,
    UnitRequirements, UnitStatus,
};
use crate::unit::Unit;

pub struct District {
    pub id: DistrictId,
    pub station: Station,
    pub units: Vec<Unit>,
    pub hexes: Vec<Hex>,
    incidents: Vec<Incident>,
    pending_queue: VecDeque<IncidentId>,
    road_graph: RoadGraph,  // Phase 2: populated from OSM data
    incident_counter: u32,
    rng: SmallRng,
}

impl District {
    pub fn new(id: DistrictId, station: Station, units: Vec<Unit>, hexes: Vec<Hex>, rng: SmallRng) -> Self {
        District {
            id,
            station,
            units,
            hexes,
            incidents: Vec::new(),
            pending_queue: VecDeque::new(),
            road_graph: RoadGraph::new(),
            incident_counter: 0,
            rng,
        }
    }

    pub fn process_events(
        &mut self,
        batch: &[SimEvent],
        profiles: &HashMap<SpawnProfileId, SpawnProfile>,
        travel: &TravelMatrix,
    ) -> Vec<(SimEvent, Event)> {
        let mut out = Vec::new();

        for ev in batch {
            match ev {
                SimEvent::IncidentSpawn { time, hex_id, .. } => {
                    self.handle_spawn(*time, *hex_id, profiles, travel, &mut out);
                }
                SimEvent::UnitArrival { time, unit_id, incident_id, .. } => {
                    self.handle_arrival(*time, *unit_id, incident_id, &mut out);
                }
                SimEvent::IncidentResolve { time, incident_id, .. } => {
                    self.handle_resolve(*time, incident_id, travel, &mut out);
                }
                SimEvent::UnitReturn { unit_id, .. } => {
                    self.handle_return(*unit_id);
                }
                SimEvent::NoOp => {}
            }
        }

        out
    }

    // ── Event handlers ────────────────────────────────────────────────────

    fn handle_spawn(
        &mut self,
        time: SimTime,
        hex_id: HexId,
        profiles: &HashMap<SpawnProfileId, SpawnProfile>,
        travel: &TravelMatrix,
        out: &mut Vec<(SimEvent, Event)>,
    ) {
        // Extract all hex data before any mutable borrow.
        let (location, spawn_profile_id) = self
            .hexes
            .iter()
            .find(|h| h.id == hex_id)
            .map(|h| (h.nearest_road_node, h.spawn_profile_id.clone()))
            .expect("hex_id not found in district");

        let profile  = &profiles[&spawn_profile_id];
        let kind     = sample_incident_kind(&profile.incident_weights, &mut self.rng);
        let priority = sample_priority(&mut self.rng);

        let incident_id = IncidentId::new(format!("{}-{}", self.id.value(), self.incident_counter));
        self.incident_counter += 1;

        let incident = Incident::new(
            incident_id.clone(),
            kind,
            priority,
            location,
            self.id,
            UnitRequirements(1),
            time,
        );
        self.incidents.push(incident);

        // Dispatch the first idle unit, or queue the incident if none available.
        if let Some(unit) = self.units.iter_mut().find(|u| u.status == UnitStatus::Idle) {
            let route        = travel.route_between(unit.position(), location);
            let travel_time  = travel.travel_time(unit.position(), location);
            let arrival_time = SimTime(time.0 + travel_time as u64);

            unit.dispatch(route, time, arrival_time, incident_id.clone());

            out.push((
                SimEvent::UnitArrival {
                    time: arrival_time,
                    unit_id: unit.id,
                    incident_id: incident_id.clone(),
                    district_id: self.id,
                },
                Event {
                    sim_time: time.0,
                    kind: EventKind::UnitDispatched,
                    district: self.id,
                    unit: Some(unit.id),
                    incident: Some(incident_id.clone()),
                },
            ));
        } else {
            // No idle unit — hold the incident until one frees up.
            self.pending_queue.push_back(incident_id.clone());
        }

        // Schedule the next spawn for this hex.
        let next = next_spawn_time(time, &spawn_profile_id, profiles, &mut self.rng);
        out.push((
            SimEvent::IncidentSpawn { time: next, hex_id, district_id: self.id },
            Event {
                sim_time: time.0,
                kind: EventKind::IncidentSpawned,
                district: self.id,
                unit: None,
                incident: Some(incident_id),
            },
        ));
    }

    fn handle_arrival(
        &mut self,
        time: SimTime,
        unit_id: UnitId,
        incident_id: &IncidentId,
        out: &mut Vec<(SimEvent, Event)>,
    ) {
        // Look up incident location before the mutable units borrow.
        let incident_location = self.incidents
            .iter()
            .find(|i| i.id == *incident_id)
            .map(|i| i.location);

        if let Some(unit) = self.units.iter_mut().find(|u| u.id == unit_id) {
            match incident_location {
                Some(loc) => unit.arrive(loc),
                None      => unit.status = UnitStatus::OnScene,
            }
        }

        let resolve_duration = self.rng.random_range(15u64..=90);
        let resolve_time     = SimTime(time.0 + resolve_duration);

        out.push((
            SimEvent::IncidentResolve {
                time: resolve_time,
                incident_id: incident_id.clone(),
                district_id: self.id,
            },
            Event {
                sim_time: time.0,
                kind: EventKind::UnitArrived,
                district: self.id,
                unit: Some(unit_id),
                incident: Some(incident_id.clone()),
            },
        ));
    }

    fn handle_resolve(
        &mut self,
        time: SimTime,
        incident_id: &IncidentId,
        travel: &TravelMatrix,
        out: &mut Vec<(SimEvent, Event)>,
    ) {
        // Step 1: resolve the incident.
        if let Some(inc) = self.incidents.iter_mut().find(|i| i.id == *incident_id) {
            inc.resolve(time);
        }

        // Step 2: free the assigned unit; extract id and current position.
        let freed = self.units.iter_mut()
            .find(|u| u.assigned_incident.as_ref() == Some(incident_id))
            .map(|u| {
                let id  = u.id;
                let pos = u.position();
                u.status            = UnitStatus::Idle;
                u.assigned_incident = None;
                (id, pos)
            });

        out.push((
            SimEvent::NoOp,
            Event {
                sim_time: time.0,
                kind:     EventKind::IncidentResolved,
                district: self.id,
                unit:     freed.map(|(id, _)| id),
                incident: Some(incident_id.clone()),
            },
        ));

        let (unit_id, unit_pos) = match freed {
            Some(f) => f,
            None    => return,
        };

        let station_loc = self.station.location;

        // Step 3: check the pending queue before sending the unit home.
        if let Some(pending_id) = self.pending_queue.pop_front() {
            // Find the pending incident's location. It should always exist and be unresolved.
            if let Some(pending_loc) = self.incidents
                .iter()
                .find(|i| i.id == pending_id && !i.is_resolved())
                .map(|i| i.location)
            {
                if let Some(unit) = self.units.iter_mut().find(|u| u.id == unit_id) {
                    let route        = travel.route_between(unit.position(), pending_loc);
                    let travel_time  = travel.travel_time(unit.position(), pending_loc) as u64;
                    let arrival_time = SimTime(time.0 + travel_time);

                    unit.dispatch(route, time, arrival_time, pending_id.clone());

                    out.push((
                        SimEvent::UnitArrival {
                            time:        arrival_time,
                            unit_id:     unit.id,
                            incident_id: pending_id.clone(),
                            district_id: self.id,
                        },
                        Event {
                            sim_time: time.0,
                            kind:     EventKind::UnitDispatched,
                            district: self.id,
                            unit:     Some(unit_id),
                            incident: Some(pending_id),
                        },
                    ));
                }
                return;
            }
            // Pending incident was already resolved (shouldn't happen in Phase 1) — fall through.
        }

        // Step 4: no pending incident — send unit back to station.
        if unit_pos != station_loc {
            let travel_time  = travel.travel_time(unit_pos, station_loc) as u64;
            let return_time  = SimTime(time.0 + travel_time);

            out.push((
                SimEvent::UnitReturn { time: return_time, unit_id, district_id: self.id },
                Event {
                    sim_time: time.0,
                    kind:     EventKind::UnitReturning,
                    district: self.id,
                    unit:     Some(unit_id),
                    incident: None,
                },
            ));
        }
    }

    fn handle_return(&mut self, unit_id: UnitId) {
        if let Some(unit) = self.units.iter_mut().find(|u| u.id == unit_id) {
            // Only update position if the unit has not been re-dispatched while in transit.
            if unit.status == UnitStatus::Idle && unit.assigned_incident.is_none() {
                unit.return_to_station();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Sampling helpers
// ---------------------------------------------------------------------------

fn sample_incident_kind(weights: &[(IncidentKind, f64)], rng: &mut impl RngExt) -> IncidentKind {
    let total: f64 = weights.iter().map(|(_, w)| w).sum();
    let mut roll   = rng.random::<f64>() * total;
    for &(kind, weight) in weights {
        roll -= weight;
        if roll <= 0.0 { return kind; }
    }
    weights.last().map(|&(k, _)| k).unwrap_or(IncidentKind::Fire)
}

fn sample_priority(rng: &mut impl RngExt) -> Priority {
    match rng.random_range(0u32..3) {
        0 => Priority::A,
        1 => Priority::B,
        _ => Priority::C,
    }
}
