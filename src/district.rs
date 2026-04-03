use std::collections::{BinaryHeap, HashMap};

use rand::RngExt;
use rand::rngs::SmallRng;

use crate::clock::SimTime;
use crate::event_log::{Event, EventKind};
use crate::event_queue::SimEvent;
use crate::hex::Hex;
use crate::incident::Incident;
use crate::routing::RoutingEngine;
use crate::spawner::{SpawnProfile, next_spawn_time};
use crate::station::Station;
use crate::types::{
    BorderNode, DistrictId, HexId, IncidentId, IncidentKind, IncidentStatus, NodeId, Priority,
    SpawnProfileId, UnitId, UnitRequirements, UnitStatus,
};
use crate::unit::Unit;

/// Simulated minutes per shift (8 hours).
const SHIFT_MINUTES: u64 = 480;

/// Wrapper for incidents in the pending queue, ordered by priority rank (highest first).
#[derive(Eq, PartialEq)]
struct PendingIncident {
    rank: u8,
    id:   IncidentId,
}

impl Ord for PendingIncident {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.rank.cmp(&other.rank)
    }
}

impl PartialOrd for PendingIncident {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

pub struct District {
    pub id:           DistrictId,
    pub station:      Station,
    pub units:        Vec<Unit>,
    pub hexes:        Vec<Hex>,
    /// O(1) lookup: hex_id → (road_node, spawn_profile_id).
    hex_lookup:       HashMap<HexId, (NodeId, SpawnProfileId)>,
    /// Border hexes 8-adjacent to a different district; populated by City after construction.
    pub border_nodes: Vec<BorderNode>,
    /// Only active (non-resolved) incidents are kept here; resolved ones are removed immediately.
    incidents:        HashMap<IncidentId, Incident>,
    /// Incidents waiting for a unit, ordered by priority (highest first).
    pending_queue:    BinaryHeap<PendingIncident>,
    incident_counter: u32,
    rng:              SmallRng,
    routing:          RoutingEngine,
}

impl District {
    pub fn new(
        id:      DistrictId,
        station: Station,
        units:   Vec<Unit>,
        hexes:   Vec<Hex>,
        rng:     SmallRng,
        routing: RoutingEngine,
    ) -> Self {
        let hex_lookup = hexes.iter()
            .map(|h| (h.id, (h.node_id(), h.spawn_profile_id.clone())))
            .collect();
        District {
            id,
            station,
            units,
            hexes,
            hex_lookup,
            border_nodes: Vec::new(),
            incidents: HashMap::new(),
            pending_queue: BinaryHeap::new(),
            incident_counter: 0,
            rng,
            routing,
        }
    }

    pub fn process_events(
        &mut self,
        batch: &[SimEvent],
        profiles: &HashMap<SpawnProfileId, SpawnProfile>,
    ) -> Vec<(SimEvent, Event)> {
        let mut out = Vec::new();

        for ev in batch {
            match ev {
                SimEvent::IncidentSpawn { time, hex_id, .. } => {
                    self.handle_spawn(*time, *hex_id, profiles, &mut out);
                }
                SimEvent::UnitArrival { time, unit_id, incident_id, dispatch_id, .. } => {
                    self.handle_arrival(*time, *unit_id, incident_id, *dispatch_id, &mut out);
                }
                SimEvent::IncidentResolve { time, incident_id, .. } => {
                    self.handle_resolve(*time, incident_id, &mut out);
                }
                SimEvent::UnitReturn { time, unit_id, dispatch_id, .. } => {
                    self.handle_return(*time, *unit_id, *dispatch_id, &mut out);
                }
                SimEvent::ShiftChange { time, .. } => {
                    self.handle_shift_change(*time, &mut out);
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
        out: &mut Vec<(SimEvent, Event)>,
    ) {
        let (location, spawn_profile_id) = self.hex_lookup
            .get(&hex_id)
            .map(|(node, profile)| (*node, profile.clone()))
            .expect("hex_id not found in district");

        let profile  = &profiles[&spawn_profile_id];
        let kind     = sample_incident_kind(&profile.incident_weights, &mut self.rng);
        let priority = sample_priority(&mut self.rng);
        let new_rank = priority.rank();

        let incident_id = IncidentId::new(format!("{}-{}", self.id.value(), self.incident_counter));
        self.incident_counter += 1;

        self.incidents.insert(incident_id.clone(), Incident::new(
            incident_id.clone(), kind, priority, location, self.id, UnitRequirements(1), time,
        ));

        // Find a unit to dispatch: nearest idle → nearest returning → preempt lowest-priority dispatched.
        let dispatch_idx =
            if let Some(idx) = self.nearest_unit(location, UnitStatus::Idle) {
                Some(idx)
            } else if let Some(idx) = self.nearest_unit(location, UnitStatus::Returning) {
                Some(idx)
            } else if let Some((idx, old_id)) = self.find_preemptable(new_rank, location) {
                // Return the preempted incident to the pending queue.
                let old_rank = self.incidents.get(&old_id).map(|i| i.priority.rank()).unwrap_or(0);
                if let Some(inc) = self.incidents.get_mut(&old_id) {
                    inc.status = IncidentStatus::Open;
                }
                self.pending_queue.push(PendingIncident { rank: old_rank, id: old_id });
                Some(idx)
            } else {
                None
            };

        if let Some(idx) = dispatch_idx {
            let from        = self.units[idx].position();
            let tt          = self.routing.travel_time(from, location) as u64;
            let arrival     = SimTime(time.0 + tt);
            let unit_id     = self.units[idx].id;
            // Route is not used by the sim loop; pass empty Vec to avoid A* cost.
            let dispatch_id = self.units[idx].dispatch(Vec::new(), time, arrival, incident_id.clone());

            if let Some(inc) = self.incidents.get_mut(&incident_id) {
                inc.status = IncidentStatus::Assigned;
            }
            out.push((
                SimEvent::UnitArrival { time: arrival, unit_id, incident_id: incident_id.clone(), district_id: self.id, dispatch_id },
                Event { sim_time: time.0, kind: EventKind::UnitDispatched, district: self.id, unit: Some(unit_id), incident: Some(incident_id.clone()), priority: None, incident_kind: None },
            ));
        } else {
            self.pending_queue.push(PendingIncident { rank: new_rank, id: incident_id.clone() });
        }

        let next = next_spawn_time(time, &spawn_profile_id, profiles, &mut self.rng);
        out.push((
            SimEvent::IncidentSpawn { time: next, hex_id, district_id: self.id },
            Event {
                sim_time: time.0,
                kind: EventKind::IncidentSpawned,
                district: self.id,
                unit: None,
                incident: Some(incident_id),
                priority: Some(priority_str(priority)),
                incident_kind: Some(kind_str(kind)),
            },
        ));
    }

    fn handle_arrival(
        &mut self,
        time: SimTime,
        unit_id: UnitId,
        incident_id: &IncidentId,
        dispatch_id: u32,
        out: &mut Vec<(SimEvent, Event)>,
    ) {
        let Some(idx) = self.units.iter().position(|u| u.id == unit_id) else { return; };

        // Discard if the unit was reassigned after this event was scheduled.
        if self.units[idx].dispatch_id != dispatch_id { return; }

        let (incident_location, priority) = self.incidents.get(incident_id)
            .map(|i| (Some(i.location), Some(i.priority)))
            .unwrap_or((None, None));
        match incident_location {
            Some(loc) => self.units[idx].arrive(loc),
            None      => self.units[idx].status = UnitStatus::OnScene,
        }

        // On-scene duration by priority — Larson (1972), Chaiken (1978):
        //   P1 (immediate): mean ≈ 70 min  →  Uniform(45, 90)
        //   P2 (urgent):    mean ≈ 40 min  →  Uniform(25, 55)
        //   P3 (routine):   mean ≈ 25 min  →  Uniform(15, 35)
        let resolve_duration = match priority {
            Some(Priority::A) => self.rng.random_range(45u64..=90),
            Some(Priority::B) => self.rng.random_range(25u64..=55),
            _                 => self.rng.random_range(15u64..=35),
        };
        let resolve_time     = SimTime(time.0 + resolve_duration);

        out.push((
            SimEvent::IncidentResolve { time: resolve_time, incident_id: incident_id.clone(), district_id: self.id },
            Event { sim_time: time.0, kind: EventKind::UnitArrived, district: self.id, unit: Some(unit_id), incident: Some(incident_id.clone()), priority: None, incident_kind: None },
        ));
    }

    fn handle_resolve(
        &mut self,
        time: SimTime,
        incident_id: &IncidentId,
        out: &mut Vec<(SimEvent, Event)>,
    ) {
        // Remove resolved incident immediately to keep the HashMap small.
        self.incidents.remove(incident_id);

        let Some(unit_idx) = self.units.iter().position(|u| u.assigned_incident.as_ref() == Some(incident_id)) else {
            out.push((SimEvent::NoOp, Event { sim_time: time.0, kind: EventKind::IncidentResolved, district: self.id, unit: None, incident: Some(incident_id.clone()), priority: None, incident_kind: None }));
            return;
        };
        let unit_id = self.units[unit_idx].id;

        out.push((
            SimEvent::NoOp,
            Event { sim_time: time.0, kind: EventKind::IncidentResolved, district: self.id, unit: Some(unit_id), incident: Some(incident_id.clone()), priority: None, incident_kind: None },
        ));

        // Dispatch to a waiting incident before sending the unit home.
        if let Some(pending_id) = self.pop_best_pending() {
            let pending_loc = self.incidents[&pending_id].location;
            let from        = self.units[unit_idx].position();
            let tt          = self.routing.travel_time(from, pending_loc) as u64;
            let arrival     = SimTime(time.0 + tt);
            let dispatch_id = self.units[unit_idx].dispatch(Vec::new(), time, arrival, pending_id.clone());

            if let Some(inc) = self.incidents.get_mut(&pending_id) {
                inc.status = IncidentStatus::Assigned;
            }
            out.push((
                SimEvent::UnitArrival { time: arrival, unit_id, incident_id: pending_id.clone(), district_id: self.id, dispatch_id },
                Event { sim_time: time.0, kind: EventKind::UnitDispatched, district: self.id, unit: Some(unit_id), incident: Some(pending_id), priority: None, incident_kind: None },
            ));
        } else {
            // No pending work — return to station.
            let unit_pos    = self.units[unit_idx].position();
            let station_loc = self.station.location;
            if unit_pos == station_loc {
                self.units[unit_idx].status            = UnitStatus::Idle;
                self.units[unit_idx].assigned_incident = None;
            } else {
                let dispatch_id = self.units[unit_idx].start_return();
                let tt          = self.routing.travel_time(unit_pos, station_loc) as u64;
                let return_time = SimTime(time.0 + tt);
                out.push((
                    SimEvent::UnitReturn { time: return_time, unit_id, district_id: self.id, dispatch_id },
                    Event { sim_time: time.0, kind: EventKind::UnitReturning, district: self.id, unit: Some(unit_id), incident: None, priority: None, incident_kind: None },
                ));
            }
        }
    }

    fn handle_return(
        &mut self,
        time: SimTime,
        unit_id: UnitId,
        dispatch_id: u32,
        out: &mut Vec<(SimEvent, Event)>,
    ) {
        let Some(idx) = self.units.iter().position(|u| u.id == unit_id) else { return; };

        // Discard if the unit was reassigned while returning.
        if self.units[idx].dispatch_id != dispatch_id { return; }

        self.units[idx].return_to_station();
        self.units[idx].status            = UnitStatus::Idle;
        self.units[idx].assigned_incident = None;

        out.push((
            SimEvent::NoOp,
            Event { sim_time: time.0, kind: EventKind::UnitReturned, district: self.id, unit: Some(unit_id), incident: None, priority: None, incident_kind: None },
        ));

        // Immediately dispatch if something is waiting.
        if let Some(pending_id) = self.pop_best_pending() {
            let pending_loc = self.incidents[&pending_id].location;
            let from        = self.units[idx].position();
            let tt          = self.routing.travel_time(from, pending_loc) as u64;
            let arrival     = SimTime(time.0 + tt);
            let did         = self.units[idx].dispatch(Vec::new(), time, arrival, pending_id.clone());

            if let Some(inc) = self.incidents.get_mut(&pending_id) {
                inc.status = IncidentStatus::Assigned;
            }
            out.push((
                SimEvent::UnitArrival { time: arrival, unit_id, incident_id: pending_id.clone(), district_id: self.id, dispatch_id: did },
                Event { sim_time: time.0, kind: EventKind::UnitDispatched, district: self.id, unit: Some(unit_id), incident: Some(pending_id), priority: None, incident_kind: None },
            ));
        }
    }

    fn handle_shift_change(&mut self, time: SimTime, out: &mut Vec<(SimEvent, Event)>) {
        // Log the shift boundary and schedule the next one.
        // Future: rotate on/off-duty crew here.
        out.push((
            SimEvent::ShiftChange { time: SimTime(time.0 + SHIFT_MINUTES), district_id: self.id },
            Event { sim_time: time.0, kind: EventKind::ShiftStarted, district: self.id, unit: None, incident: None, priority: None, incident_kind: None },
        ));
    }

    // ── Helpers ───────────────────────────────────────────────────────────

    /// Return the index of the nearest unit with the given status to `location`.
    fn nearest_unit(&self, location: NodeId, status: UnitStatus) -> Option<usize> {
        self.units.iter()
            .enumerate()
            .filter(|(_, u)| u.status == status)
            .min_by_key(|(_, u)| self.routing.travel_time(u.position(), location))
            .map(|(idx, _)| idx)
    }

    /// Find a dispatched unit (if any) whose current incident has lower priority than
    /// `new_rank`. Returns the index of the best preemption target (lowest priority,
    /// nearest on tie) and the incident id it was assigned to.
    fn find_preemptable(&self, new_rank: u8, location: NodeId) -> Option<(usize, IncidentId)> {
        self.units.iter().enumerate()
            .filter(|(_, u)| u.status == UnitStatus::Dispatched)
            .filter_map(|(idx, u)| {
                let assigned = u.assigned_incident.as_ref()?;
                let inc = self.incidents.get(assigned)?;
                if inc.priority.rank() < new_rank {
                    let tt = self.routing.travel_time(u.position(), location);
                    Some((idx, assigned.clone(), inc.priority.rank(), tt))
                } else {
                    None
                }
            })
            .min_by(|a, b| a.2.cmp(&b.2).then(a.3.cmp(&b.3)))
            .map(|(idx, id, _, _)| (idx, id))
    }

    /// Remove and return the highest-priority incident from the pending queue.
    /// Skips stale entries whose incidents were already resolved and removed from the map.
    fn pop_best_pending(&mut self) -> Option<IncidentId> {
        while let Some(pending) = self.pending_queue.pop() {
            if self.incidents.contains_key(&pending.id) {
                return Some(pending.id);
            }
            // Stale entry — incident was resolved while queued; discard and continue.
        }
        None
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
    // P1=15%, P2=35%, P3=50% — APCO Project 33 / BJS LEMAS empirical mix
    match rng.random_range(0u32..100) {
        0..15  => Priority::A,
        15..50 => Priority::B,
        _      => Priority::C,
    }
}

fn priority_str(p: Priority) -> String {
    match p {
        Priority::A => "A".into(),
        Priority::B => "B".into(),
        Priority::C => "C".into(),
    }
}

fn kind_str(k: IncidentKind) -> String {
    match k {
        IncidentKind::Fire             => "Fire".into(),
        IncidentKind::MedicalEmergency => "MedicalEmergency".into(),
        IncidentKind::Crime            => "Crime".into(),
        IncidentKind::Accident         => "Accident".into(),
    }
}
