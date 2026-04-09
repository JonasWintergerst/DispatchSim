use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;

use rand::RngExt;
use rand::rngs::SmallRng;

use crate::clock::SimTime;
use crate::event_log::{Event, EventKind, RouteRecord};
use crate::event_queue::SimEvent;
use crate::hex::Hex;
use crate::incident::Incident;
use crate::patrol::PatrolRoute;
use crate::routing::RoutingEngine;
use crate::spawner::{SpawnProfile, next_spawn_time};
use crate::station::Station;
use crate::types::{
    BorderNode, DistrictId, HexId, IncidentId, IncidentKind, IncidentStatus, MutualAidRequest,
    NodeId, Priority, SpawnProfileId, UnitId, UnitRequirements, UnitStatus,
};
use crate::unit::Unit;

/// Simulated minutes per shift (8 hours).
const SHIFT_MINUTES: u64 = 480;

/// Output type for `process_events` — a sim event to enqueue, a log event, and an
/// optional route record to persist (only present on `UnitDispatched` events).
type Out = Vec<(SimEvent, Event, Option<RouteRecord>)>;

/// Full output of one `process_events` call. Mutual-aid requests are returned
/// alongside the event/log stream so the City can do a single post-processing
/// pass after every district has ticked.
pub struct ProcessOutput {
    pub events:       Out,
    pub aid_requests: Vec<MutualAidRequest>,
}

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
    /// Includes both locally-spawned and loaned-in (foreign) incidents.
    incidents:        HashMap<IncidentId, Incident>,
    /// Incidents waiting for a unit, ordered by priority (highest first).
    pending_queue:    BinaryHeap<PendingIncident>,
    incident_counter: u32,
    rng:              SmallRng,
    /// Shared read-mostly routing engine. Multiple parallel sim variants can
    /// share the same `Arc` without cloning the underlying graph or anchor
    /// travel-time table — `RoutingEngine` is `Sync` via an internal `RwLock`
    /// on its lazy A* cache.
    pub(crate) routing: Arc<RoutingEngine>,
    record_routes:    bool,
    /// Patrol routes available in this district. Assigned to units at city
    /// construction time; multiple units can share an `Arc<PatrolRoute>`.
    pub patrol_routes: Vec<Arc<PatrolRoute>>,
}

impl District {
    pub fn new(
        id:      DistrictId,
        station: Station,
        units:   Vec<Unit>,
        hexes:   Vec<Hex>,
        rng:     SmallRng,
        routing: Arc<RoutingEngine>,
        record_routes: bool,
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
            record_routes,
            patrol_routes: Vec::new(),
        }
    }

    pub fn process_events(
        &mut self,
        batch: &[SimEvent],
        profiles: &HashMap<SpawnProfileId, SpawnProfile>,
    ) -> ProcessOutput {
        let mut out = ProcessOutput { events: Vec::new(), aid_requests: Vec::new() };

        for ev in batch {
            match ev {
                SimEvent::IncidentSpawn { time, hex_id, .. } => {
                    self.handle_spawn(*time, *hex_id, profiles, &mut out);
                }
                SimEvent::UnitArrival { time, unit_id, incident_id, dispatch_id, .. } => {
                    self.handle_arrival(*time, *unit_id, incident_id, *dispatch_id, &mut out.events);
                }
                SimEvent::IncidentResolve { time, incident_id, .. } => {
                    self.handle_resolve(*time, incident_id, &mut out.events);
                }
                SimEvent::UnitReturn { time, unit_id, dispatch_id, .. } => {
                    self.handle_return(*time, *unit_id, *dispatch_id, &mut out.events);
                }
                SimEvent::PatrolLoop { time, unit_id, dispatch_id, .. } => {
                    self.handle_patrol_loop(*time, *unit_id, *dispatch_id, &mut out.events);
                }
                SimEvent::ShiftChange { time, .. } => {
                    self.handle_shift_change(*time, &mut out.events);
                }
                SimEvent::NoOp => {}
            }
        }

        out
    }

    // ── Mutual-aid surface (called by City) ────────────────────────────────

    /// Try to dispatch one of this district's free units to a foreign incident.
    /// Inserts the loaned-in incident into the local `incidents` map and runs
    /// the standard dispatch path. Returns `true` on successful loan.
    ///
    /// Called by `City::tick`'s mutual-aid pass; the request originated in
    /// `loaning_from` and is being served by `self`.
    pub fn try_accept_loan(
        &mut self,
        time:         SimTime,
        loaning_from: DistrictId,
        request:      &MutualAidRequest,
        out:          &mut Out,
    ) -> bool {
        // Synthesise an Incident record locally so handle_arrival/handle_resolve
        // see it in self.incidents like any other call.
        let inc = Incident::new(
            request.incident_id.clone(),
            // Kind/required units don't affect the dispatch path; use defaults.
            IncidentKind::Crime,
            request.priority,
            request.location,
            loaning_from,
            UnitRequirements(1),
            request.spawn_time,
        );
        self.incidents.insert(request.incident_id.clone(), inc);

        let success = self.try_dispatch_pending(request.incident_id.clone(), time, out, /*loaned*/ true);

        if success {
            // Log the mutual-aid handoff so the SQLite log records it for the
            // what-if comparison table.
            out.push((
                SimEvent::NoOp,
                Event {
                    sim_time:      time.0,
                    kind:          EventKind::MutualAidRequested,
                    district:      loaning_from,
                    unit:          None,
                    incident:      Some(request.incident_id.clone()),
                    priority:      None,
                    incident_kind: None,
                },
                None,
            ));
        } else {
            // Roll back the speculative insert if no unit ended up taking it.
            self.incidents.remove(&request.incident_id);
        }
        success
    }

    /// Remove a pending incident from this district. Called when the City has
    /// successfully loaned the incident out. We mark the incident as Assigned
    /// (not Open) so `pop_best_pending` skips it; the actual `Incident` row
    /// stays in `self.incidents` until the foreign district reports completion
    /// via a synthetic `IncidentResolve`.
    pub fn mark_loaned_out(&mut self, incident_id: &IncidentId) {
        if let Some(inc) = self.incidents.get_mut(incident_id) {
            inc.status = IncidentStatus::Assigned;
        }
    }

    // ── Event handlers ────────────────────────────────────────────────────

    fn handle_spawn(
        &mut self,
        time: SimTime,
        hex_id: HexId,
        profiles: &HashMap<SpawnProfileId, SpawnProfile>,
        out: &mut ProcessOutput,
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

        // Schedule the next spawn first (independent of dispatch outcome).
        let next = next_spawn_time(time, &spawn_profile_id, profiles, &mut self.rng);
        out.events.push((
            SimEvent::IncidentSpawn { time: next, hex_id, district_id: self.id },
            Event {
                sim_time: time.0,
                kind: EventKind::IncidentSpawned,
                district: self.id,
                unit: None,
                incident: Some(incident_id.clone()),
                priority: Some(priority_str(priority)),
                incident_kind: Some(kind_str(kind)),
            },
            None,
        ));

        // Try to dispatch a local unit. The dispatch helper handles
        // Idle / Patrolling / Returning / preemption precedence.
        let dispatched = self.try_dispatch_pending(incident_id.clone(), time, &mut out.events, /*loaned*/ false);

        if !dispatched {
            // Push to local pending queue AND emit a mutual-aid request so the
            // City can try a neighbour district. The pending entry is the
            // fallback if no neighbour can help.
            self.pending_queue.push(PendingIncident { rank: new_rank, id: incident_id.clone() });
            out.aid_requests.push(MutualAidRequest {
                requesting_district: self.id,
                incident_id,
                location,
                priority,
                spawn_time: time,
            });
        }
    }

    /// Core dispatch logic: try to assign the best available unit to the given
    /// (already-inserted) incident. Returns true on success. Used by
    /// `handle_spawn` for normal dispatches and by `try_accept_loan` for
    /// mutual-aid loans.
    fn try_dispatch_pending(
        &mut self,
        incident_id: IncidentId,
        time: SimTime,
        out: &mut Out,
        loaned: bool,
    ) -> bool {
        let (location, new_rank) = match self.incidents.get(&incident_id) {
            Some(inc) => (inc.location, inc.priority.rank()),
            None      => return false,
        };

        // Precedence: Idle → Patrolling → Returning → preempt-lower-priority.
        let dispatch_idx = if let Some(idx) = self.nearest_unit(location, UnitStatus::Idle, time) {
            Some(idx)
        } else if let Some(idx) = self.nearest_unit(location, UnitStatus::Patrolling, time) {
            Some(idx)
        } else if let Some(idx) = self.nearest_unit(location, UnitStatus::Returning, time) {
            Some(idx)
        } else if let Some((idx, old_id)) = self.find_preemptable(new_rank, location, time) {
            let old_rank = self.incidents.get(&old_id).map(|i| i.priority.rank()).unwrap_or(0);
            if let Some(inc) = self.incidents.get_mut(&old_id) {
                inc.status = IncidentStatus::Open;
            }
            self.pending_queue.push(PendingIncident { rank: old_rank, id: old_id });
            Some(idx)
        } else {
            None
        };

        let Some(idx) = dispatch_idx else { return false; };

        let from        = self.units[idx].current_position(time);
        let tt          = self.routing.travel_time(from, location) as u64;
        let arrival     = SimTime(time.0 + tt);
        let unit_id     = self.units[idx].id;
        let route = if self.record_routes {
            let path = self.routing.route_between(from, location);
            Some(build_route_record(&incident_id, &path, &self.routing))
        } else { None };
        let dispatch_id = self.units[idx].dispatch(time, arrival, incident_id.clone());
        if loaned {
            // The unit is being lent to another district; remember which one
            // for instrumentation. The unit physically still belongs to its
            // home district and will return to its own station after resolve.
            self.units[idx].loaned_to = self.incidents.get(&incident_id).map(|i| i.district);
        }

        if let Some(inc) = self.incidents.get_mut(&incident_id) {
            inc.status = IncidentStatus::Assigned;
        }
        out.push((
            SimEvent::UnitArrival { time: arrival, unit_id, incident_id: incident_id.clone(), district_id: self.id, dispatch_id },
            Event { sim_time: time.0, kind: EventKind::UnitDispatched, district: self.id, unit: Some(unit_id), incident: Some(incident_id), priority: None, incident_kind: None },
            route,
        ));
        true
    }

    fn handle_arrival(
        &mut self,
        time: SimTime,
        unit_id: UnitId,
        incident_id: &IncidentId,
        dispatch_id: u32,
        out: &mut Out,
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
            None,
        ));
    }

    fn handle_resolve(
        &mut self,
        time: SimTime,
        incident_id: &IncidentId,
        out: &mut Out,
    ) {
        // Remove resolved incident immediately to keep the HashMap small.
        let removed = self.incidents.remove(incident_id);

        let Some(unit_idx) = self.units.iter().position(|u| u.assigned_incident.as_ref() == Some(incident_id)) else {
            // No matching unit in this district. Either: (a) the incident was
            // resolved by a loaned-out unit and the foreign district just sent
            // us a synthetic IncidentResolve to clean up — that's the path we
            // hit when `removed.is_some()` and we have no local unit; (b) the
            // unit was reassigned. Either way, just log and exit.
            out.push((SimEvent::NoOp, Event { sim_time: time.0, kind: EventKind::IncidentResolved, district: self.id, unit: None, incident: Some(incident_id.clone()), priority: None, incident_kind: None }, None));
            return;
        };
        let unit_id = self.units[unit_idx].id;
        let was_loaned = self.units[unit_idx].is_loaned();
        let original_owner = self.units[unit_idx].loaned_to;

        out.push((
            SimEvent::NoOp,
            Event { sim_time: time.0, kind: EventKind::IncidentResolved, district: self.id, unit: Some(unit_id), incident: Some(incident_id.clone()), priority: None, incident_kind: None },
            None,
        ));

        // If this was a loaned incident, notify the original owner so they can
        // clean up their pending bookkeeping. The synthetic IncidentResolve
        // will hit the early-return branch above on the owner's side.
        if was_loaned {
            if let Some(owner) = original_owner {
                out.push((
                    SimEvent::IncidentResolve {
                        time, incident_id: incident_id.clone(), district_id: owner,
                    },
                    // Don't double-log the resolve; the local one above is enough.
                    // This tuple element is required by Out, so emit a NoOp-ish
                    // event that distinguishes itself in the SQLite log.
                    Event { sim_time: time.0, kind: EventKind::IncidentResolved, district: owner, unit: None, incident: Some(incident_id.clone()), priority: None, incident_kind: None },
                    None,
                ));
            }
        }
        let _ = removed; // silence unused

        // After a loan completes, the unit clears `loaned_to` on return to home.
        // We don't dispatch a loaned unit to a *local* pending incident here;
        // it goes home (or back to patrol) first.
        if was_loaned {
            self.send_unit_home_or_patrol(unit_idx, time, unit_id, out);
            return;
        }

        // Dispatch to a waiting incident before sending the unit home.
        if let Some(pending_id) = self.pop_best_pending() {
            let pending_loc = self.incidents[&pending_id].location;
            let from        = self.units[unit_idx].current_position(time);
            let tt          = self.routing.travel_time(from, pending_loc) as u64;
            let arrival     = SimTime(time.0 + tt);
            let route = if self.record_routes {
                let path = self.routing.route_between(from, pending_loc);
                Some(build_route_record(&pending_id, &path, &self.routing))
            } else { None };
            let dispatch_id = self.units[unit_idx].dispatch(time, arrival, pending_id.clone());

            if let Some(inc) = self.incidents.get_mut(&pending_id) {
                inc.status = IncidentStatus::Assigned;
            }
            out.push((
                SimEvent::UnitArrival { time: arrival, unit_id, incident_id: pending_id.clone(), district_id: self.id, dispatch_id },
                Event { sim_time: time.0, kind: EventKind::UnitDispatched, district: self.id, unit: Some(unit_id), incident: Some(pending_id), priority: None, incident_kind: None },
                route,
            ));
        } else {
            self.send_unit_home_or_patrol(unit_idx, time, unit_id, out);
        }
    }

    /// After a unit becomes free with no pending work, decide whether it
    /// should resume patrolling, return to its home station, or go directly
    /// idle (if already at the station). Centralised so handle_resolve and
    /// handle_return share the logic.
    fn send_unit_home_or_patrol(
        &mut self,
        unit_idx: usize,
        time:     SimTime,
        unit_id:  UnitId,
        out:      &mut Out,
    ) {
        let unit_pos    = self.units[unit_idx].current_position(time);
        let station_loc = self.station.location;
        let has_patrol  = self.units[unit_idx].patrol_route.is_some();

        if unit_pos == station_loc {
            // Already at the station — start patrolling immediately if assigned,
            // otherwise sit idle.
            self.units[unit_idx].status            = UnitStatus::Idle;
            self.units[unit_idx].assigned_incident = None;
            if has_patrol {
                self.start_patrol_now(unit_idx, time, unit_id, out);
            }
        } else {
            // Need to travel back to the station. Schedule a normal UnitReturn;
            // patrolling (if any) will start when the unit arrives home.
            let dispatch_id = self.units[unit_idx].start_return();
            let tt          = self.routing.travel_time(unit_pos, station_loc) as u64;
            let return_time = SimTime(time.0 + tt);
            out.push((
                SimEvent::UnitReturn { time: return_time, unit_id, district_id: self.id, dispatch_id },
                Event { sim_time: time.0, kind: EventKind::UnitReturning, district: self.id, unit: Some(unit_id), incident: None, priority: None, incident_kind: None },
                None,
            ));
        }
    }

    fn handle_return(
        &mut self,
        time: SimTime,
        unit_id: UnitId,
        dispatch_id: u32,
        out: &mut Out,
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
            None,
        ));

        // Immediately dispatch if something is waiting locally.
        if let Some(pending_id) = self.pop_best_pending() {
            let pending_loc = self.incidents[&pending_id].location;
            let from        = self.units[idx].current_position(time);
            let tt          = self.routing.travel_time(from, pending_loc) as u64;
            let arrival     = SimTime(time.0 + tt);
            let route = if self.record_routes {
                let path = self.routing.route_between(from, pending_loc);
                Some(build_route_record(&pending_id, &path, &self.routing))
            } else { None };
            let did         = self.units[idx].dispatch(time, arrival, pending_id.clone());

            if let Some(inc) = self.incidents.get_mut(&pending_id) {
                inc.status = IncidentStatus::Assigned;
            }
            out.push((
                SimEvent::UnitArrival { time: arrival, unit_id, incident_id: pending_id.clone(), district_id: self.id, dispatch_id: did },
                Event { sim_time: time.0, kind: EventKind::UnitDispatched, district: self.id, unit: Some(unit_id), incident: Some(pending_id), priority: None, incident_kind: None },
                route,
            ));
            return;
        }

        // No pending work. Start patrolling if this unit has a route assigned.
        if self.units[idx].patrol_route.is_some() {
            self.start_patrol_now(idx, time, unit_id, out);
        }
    }

    fn handle_patrol_loop(
        &mut self,
        time: SimTime,
        unit_id: UnitId,
        dispatch_id: u32,
        out: &mut Out,
    ) {
        let Some(idx) = self.units.iter().position(|u| u.id == unit_id) else { return; };

        // Stale check: if the unit was redirected to an incident, the in-flight
        // PatrolLoop event is no longer valid.
        if self.units[idx].dispatch_id != dispatch_id { return; }

        // Only renew the loop if the unit is currently Idle (initial start) or
        // Patrolling (cycling). Any other state means we hit a benign race we
        // can safely drop.
        let s = self.units[idx].status;
        if s != UnitStatus::Idle && s != UnitStatus::Patrolling { return; }

        if self.units[idx].patrol_route.is_none() { return; }
        self.start_patrol_now(idx, time, unit_id, out);
    }

    /// Helper: put a unit into patrol state and schedule the next loop event.
    /// Used by both `start_patrol_now`-style transitions and `handle_patrol_loop`.
    fn start_patrol_now(
        &mut self,
        unit_idx: usize,
        time:     SimTime,
        unit_id:  UnitId,
        out:      &mut Out,
    ) {
        let Some(route) = self.units[unit_idx].patrol_route.clone() else { return; };
        let total = route.total_min.max(1) as u64;
        let did = self.units[unit_idx].start_patrol(time, route);
        let next = SimTime(time.0 + total);
        out.push((
            SimEvent::PatrolLoop { time: next, unit_id, district_id: self.id, dispatch_id: did },
            Event { sim_time: time.0, kind: EventKind::PatrolStarted, district: self.id, unit: Some(unit_id), incident: None, priority: None, incident_kind: None },
            None,
        ));
    }

    fn handle_shift_change(&mut self, time: SimTime, out: &mut Out) {
        // Log the shift boundary and schedule the next one.
        // Future: rotate on/off-duty crew here.
        out.push((
            SimEvent::ShiftChange { time: SimTime(time.0 + SHIFT_MINUTES), district_id: self.id },
            Event { sim_time: time.0, kind: EventKind::ShiftStarted, district: self.id, unit: None, incident: None, priority: None, incident_kind: None },
            None,
        ));
    }

    // ── Helpers ───────────────────────────────────────────────────────────

    /// Return the index of the nearest unit with the given status to `location`.
    /// `now` is needed to compute the live position of patrolling units.
    fn nearest_unit(&self, location: NodeId, status: UnitStatus, now: SimTime) -> Option<usize> {
        self.units.iter()
            .enumerate()
            .filter(|(_, u)| u.status == status)
            .min_by_key(|(_, u)| self.routing.travel_time(u.current_position(now), location))
            .map(|(idx, _)| idx)
    }

    /// Find a dispatched unit (if any) whose current incident has lower priority than
    /// `new_rank`. Returns the index of the best preemption target (lowest priority,
    /// nearest on tie) and the incident id it was assigned to.
    fn find_preemptable(&self, new_rank: u8, location: NodeId, now: SimTime) -> Option<(usize, IncidentId)> {
        self.units.iter().enumerate()
            .filter(|(_, u)| u.status == UnitStatus::Dispatched)
            .filter_map(|(idx, u)| {
                let assigned = u.assigned_incident.as_ref()?;
                let inc = self.incidents.get(assigned)?;
                if inc.priority.rank() < new_rank {
                    let tt = self.routing.travel_time(u.current_position(now), location);
                    Some((idx, assigned.clone(), inc.priority.rank(), tt))
                } else {
                    None
                }
            })
            .min_by(|a, b| a.2.cmp(&b.2).then(a.3.cmp(&b.3)))
            .map(|(idx, id, _, _)| (idx, id))
    }

    /// Remove and return the highest-priority incident from the pending queue.
    /// Skips entries whose incident is missing OR no longer Open (e.g. because
    /// it was loaned out via mutual aid).
    fn pop_best_pending(&mut self) -> Option<IncidentId> {
        while let Some(pending) = self.pending_queue.pop() {
            if let Some(inc) = self.incidents.get(&pending.id) {
                if inc.status == IncidentStatus::Open {
                    return Some(pending.id);
                }
            }
            // Stale or already-loaned-out entry — discard and continue.
        }
        None
    }

    /// Used by City to seed initial PatrolLoop events for patrol-enabled units.
    pub fn initial_patrol_events(&self) -> Vec<SimEvent> {
        self.units.iter().filter_map(|u| {
            if u.patrol_route.is_some() {
                Some(SimEvent::PatrolLoop {
                    time:        SimTime(0),
                    unit_id:     u.id,
                    district_id: self.id,
                    dispatch_id: u.dispatch_id,
                })
            } else {
                None
            }
        }).collect()
    }
}

// ---------------------------------------------------------------------------
// Route helpers
// ---------------------------------------------------------------------------

/// Serialise a route (sequence of NodeIds) as a JSON array of `[lon, lat]` pairs.
/// Nodes without a position in the graph are silently skipped.
fn build_route_record(incident_id: &IncidentId, path: &[NodeId], routing: &RoutingEngine) -> RouteRecord {
    let coords: Vec<[f64; 2]> = path.iter()
        .filter_map(|&id| routing.node_position(id))
        .map(|(lon, lat)| [lon, lat])
        .collect();
    let path_json = serde_json::to_string(&coords).unwrap_or_default();
    RouteRecord { incident_id: incident_id.clone(), path_json }
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
