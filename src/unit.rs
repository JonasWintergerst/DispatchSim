use std::sync::Arc;

use crate::clock::SimTime;
use crate::patrol::PatrolRoute;
use crate::types::{DistrictId, IncidentId, NodeId, SimType, UnitId, UnitStatus};

pub struct Unit {
    pub id: UnitId,
    pub unit_type: SimType,
    pub status: UnitStatus,
    position: NodeId,
    home: NodeId,
    pub assigned_incident: Option<IncidentId>,
    pub dispatch_time: Option<SimTime>,
    pub arrival_time: Option<SimTime>,
    /// Monotonically increasing per unit. Carried on UnitArrival/UnitReturn/PatrolLoop
    /// events so stale events (from preempted dispatches or interrupted patrols) can
    /// be detected and discarded.
    pub dispatch_id: u32,

    // ── Patrol state ─────────────────────────────────────────────────────
    /// The route this unit cycles when no incident is active. `None` means
    /// the unit returns to its station and waits there. Shared `Arc` so many
    /// units can use the same route without copying.
    pub patrol_route: Option<Arc<PatrolRoute>>,
    /// Sim time at which the current patrol loop started. Combined with the
    /// route's cumulative segment times, this lets `current_position` compute
    /// the unit's location lazily without per-tick events.
    pub patrol_started_at: Option<SimTime>,

    // ── Mutual aid state ─────────────────────────────────────────────────
    /// `Some(D)` while this unit is on loan to district D. The unit's home
    /// district is unchanged; only the incident it is currently serving lives
    /// in D. Cleared when the unit returns to its home station.
    pub loaned_to: Option<DistrictId>,
}

impl Unit {
    pub fn new(id: UnitId, unit_type: SimType, station: NodeId) -> Self {
        Self {
            id,
            unit_type,
            status: UnitStatus::Idle,
            position: station,
            home: station,
            assigned_incident: None,
            dispatch_time: None,
            arrival_time: None,
            dispatch_id: 0,
            patrol_route: None,
            patrol_started_at: None,
            loaned_to: None,
        }
    }

    /// Begin travelling to an incident. Returns the new dispatch_id, which must be
    /// embedded in the paired UnitArrival event for stale-event detection.
    ///
    /// Captures the unit's current position *before* clearing patrol state, so
    /// that callers who computed travel time from `current_position(now)` end
    /// up with a consistent starting point.
    pub fn dispatch(
        &mut self,
        time: SimTime,
        arrival_time: SimTime,
        incident_id: IncidentId,
    ) -> u32 {
        // If the unit was patrolling, snap its persistent `position` to the
        // lazy patrol position so subsequent calls to position() are correct.
        if self.status == UnitStatus::Patrolling {
            self.position = self.current_position(time);
            self.patrol_started_at = None;
        }
        // Any prior loan is voided by a fresh dispatch. Without this, a unit
        // that was previously dispatched on loan and is now being preempted by
        // a higher-priority *local* incident would still appear loaned, and
        // the resolve handler would route the synthetic cleanup back to the
        // wrong (foreign) district. `try_dispatch_pending` re-sets `loaned_to`
        // immediately afterwards if the new dispatch is itself a loan.
        self.loaned_to = None;
        self.dispatch_id += 1;
        self.status = UnitStatus::Dispatched;
        self.dispatch_time = Some(time);
        self.arrival_time = Some(arrival_time);
        self.assigned_incident = Some(incident_id);
        self.dispatch_id
    }

    /// Begin returning to the home station after an incident resolves.
    /// Returns the new dispatch_id for the paired UnitReturn event.
    pub fn start_return(&mut self) -> u32 {
        self.dispatch_id += 1;
        self.status = UnitStatus::Returning;
        self.assigned_incident = None;
        self.dispatch_id
    }

    /// Begin a patrol loop. Returns the new dispatch_id, which must be embedded
    /// in the paired PatrolLoop event for stale-event detection.
    pub fn start_patrol(&mut self, time: SimTime, route: Arc<PatrolRoute>) -> u32 {
        // Snap persistent position to the route's first waypoint so that any
        // subsequent dispatch starts from the loop's anchor.
        self.position = route.waypoints[0];
        self.dispatch_id += 1;
        self.status = UnitStatus::Patrolling;
        self.patrol_route = Some(route);
        self.patrol_started_at = Some(time);
        self.assigned_incident = None;
        self.dispatch_id
    }

    /// Mark the unit as on-scene and update its position to the incident location.
    pub fn arrive(&mut self, location: NodeId) {
        self.status = UnitStatus::OnScene;
        self.position = location;
    }

    /// Teleport position back to the home station (called when `UnitReturn` fires).
    /// Only takes effect if the unit has not been re-dispatched since the event was scheduled.
    pub fn return_to_station(&mut self) {
        self.position = self.home;
        self.loaned_to = None;
    }

    pub fn position(&self) -> NodeId { self.position }
    pub fn home(&self) -> NodeId { self.home }

    /// Live position. For all states except `Patrolling`, this is the unit's
    /// last-known persistent position. For patrolling units, it walks the
    /// loop's cumulative segment times to find the current waypoint —
    /// O(log N) and touches no shared state, so it's safe inside hot paths.
    pub fn current_position(&self, now: SimTime) -> NodeId {
        if self.status != UnitStatus::Patrolling {
            return self.position;
        }
        match (&self.patrol_route, self.patrol_started_at) {
            (Some(route), Some(started)) => route.position_at(started.0, now.0),
            _ => self.position,
        }
    }

    pub fn is_loaned(&self) -> bool { self.loaned_to.is_some() }
}
