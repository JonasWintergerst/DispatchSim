// Patrol routes — phase 2 patrol modelling.
//
// A `PatrolRoute` is a closed loop of NodeIds with precomputed travel times
// between consecutive waypoints. The simulator never builds these at run time;
// they are produced offline by the `patrol_gen` binary and loaded from JSON.
//
// Position-while-patrolling is computed lazily via `Unit::current_position`,
// not by ticking — this is the "event-based lazy evaluation" pattern from
// Design.txt §3.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::routing::RoutingEngine;
use crate::types::{DistrictId, NodeId, PatrolRouteId};

// ---------------------------------------------------------------------------
// Strategy enum (string-tagged so it round-trips through TOML / what-if)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatrolStrategy {
    /// No patrols — units idle at their station.
    None,
    /// Loop through the highest-spawn-rate hexes in the district.
    Hotspot,
    /// Hover near border nodes for cross-district responsiveness.
    Border,
    /// Loop chosen to minimise the maximum travel time from any patrol point
    /// to any hex in the district.
    Coverage,
    /// User-supplied list of NodeIds, no generation.
    Static,
}

impl PatrolStrategy {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "none"     => Some(Self::None),
            "hotspot"  => Some(Self::Hotspot),
            "border"   => Some(Self::Border),
            "coverage" => Some(Self::Coverage),
            "static"   => Some(Self::Static),
            _          => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None     => "none",
            Self::Hotspot  => "hotspot",
            Self::Border   => "border",
            Self::Coverage => "coverage",
            Self::Static   => "static",
        }
    }
}

// ---------------------------------------------------------------------------
// PatrolRoute
// ---------------------------------------------------------------------------

/// A closed-loop patrol route. The last waypoint should equal the first so
/// the loop is well-defined; `from_waypoints` does not enforce this but the
/// generator does.
///
/// `cumulative_min[i]` is the elapsed time at the moment the unit *reaches*
/// `waypoints[i]` measured from the start of the loop. `cumulative_min[0] = 0`
/// and `cumulative_min.last() == total_min`.
#[derive(Debug, Clone)]
pub struct PatrolRoute {
    pub id:                    PatrolRouteId,
    pub waypoints:             Vec<NodeId>,
    pub segment_durations_min: Vec<u32>,   // len = waypoints.len() - 1
    pub cumulative_min:        Vec<u32>,   // len = waypoints.len()
    pub total_min:             u32,
}

impl PatrolRoute {
    /// Build a route from a sequence of waypoints, computing segment durations
    /// from the routing engine. The route is closed automatically: if the last
    /// waypoint differs from the first, a closing segment is appended.
    ///
    /// Returns `None` if `waypoints` is empty — a route with no points cannot
    /// be patrolled. A single-waypoint route is allowed (degenerate, the unit
    /// just stands at that point).
    pub fn from_waypoints(
        id:        PatrolRouteId,
        mut waypoints: Vec<NodeId>,
        routing:   &RoutingEngine,
    ) -> Option<Self> {
        if waypoints.is_empty() {
            return None;
        }
        if waypoints.len() < 2 {
            // Degenerate route: a single point. Total = 0, no segments.
            return Some(Self {
                id,
                waypoints,
                segment_durations_min: Vec::new(),
                cumulative_min:        vec![0],
                total_min:             0,
            });
        }

        if waypoints.first() != waypoints.last() {
            let first = waypoints[0];
            waypoints.push(first);
        }

        let mut segment_durations_min = Vec::with_capacity(waypoints.len() - 1);
        let mut cumulative_min        = Vec::with_capacity(waypoints.len());
        cumulative_min.push(0);

        let mut acc: u32 = 0;
        for w in waypoints.windows(2) {
            // travel_time returns u32 minutes; floor at 1 so loops always advance.
            let tt = routing.travel_time(w[0], w[1]).max(1);
            segment_durations_min.push(tt);
            acc = acc.saturating_add(tt);
            cumulative_min.push(acc);
        }

        Some(Self {
            id,
            waypoints,
            segment_durations_min,
            cumulative_min,
            total_min: acc.max(1),
        })
    }

    /// Position of a unit that started this loop at `started_at` and is now at
    /// `now`. Snaps to the *origin* waypoint of the segment currently being
    /// traversed (NodeIds are discrete, so we don't interpolate).
    pub fn position_at(&self, started_at_min: u64, now_min: u64) -> NodeId {
        debug_assert!(!self.waypoints.is_empty(), "PatrolRoute invariant: waypoints non-empty (enforced by from_waypoints)");
        if self.total_min == 0 || self.waypoints.len() == 1 {
            return self.waypoints[0];
        }
        let elapsed = now_min.saturating_sub(started_at_min);
        let offset  = (elapsed % self.total_min as u64) as u32;

        // partition_point: first index whose cumulative time is > offset.
        // The segment currently being traversed is the one *before* that
        // index, so we subtract 1.
        let idx = self.cumulative_min
            .partition_point(|&t| t <= offset)
            .saturating_sub(1)
            .min(self.waypoints.len() - 1);
        self.waypoints[idx]
    }
}

// ---------------------------------------------------------------------------
// On-disk schema for `config/patrol_routes_*.json`
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatrolRoutesFile {
    pub strategy: String,
    pub routes:   Vec<PatrolRouteEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatrolRouteEntry {
    pub district_id: u32,
    /// OSM node IDs as raw u32, in visit order. The closing edge back to the
    /// first waypoint is implicit; `from_waypoints` adds it.
    pub waypoints:   Vec<u32>,
}

/// Per-district set of compiled patrol routes, ready for the simulator.
pub type PatrolRouteSet = HashMap<DistrictId, Vec<Arc<PatrolRoute>>>;

/// Load patrol routes from JSON, compile them against a per-district routing
/// engine lookup, and return a map keyed by `DistrictId`. Districts that have
/// no entry in the file get an empty Vec; missing districts are simply absent
/// (callers should treat that as "no patrols").
pub fn load_routes(
    path:    &Path,
    routings: &HashMap<DistrictId, Arc<RoutingEngine>>,
) -> Result<PatrolRouteSet, String> {
    let raw = fs::read_to_string(path)
        .map_err(|e| format!("could not read patrol routes file '{}': {}", path.display(), e))?;
    let file: PatrolRoutesFile = serde_json::from_str(&raw)
        .map_err(|e| format!("patrol routes JSON parse error: {}", e))?;

    let mut out: PatrolRouteSet = HashMap::new();
    let mut next_id: u32 = 0;
    for (entry_idx, entry) in file.routes.into_iter().enumerate() {
        let did = DistrictId::new(entry.district_id);
        let Some(routing) = routings.get(&did) else {
            // No matching district — silently skip.
            continue;
        };
        if entry.waypoints.len() < 2 {
            eprintln!(
                "warning: patrol route #{} (district {}) has {} waypoint(s); need >= 2, skipping",
                entry_idx, entry.district_id, entry.waypoints.len()
            );
            continue;
        }
        let waypoints: Vec<NodeId> = entry.waypoints.iter().copied().map(NodeId::new).collect();
        let Some(route) = PatrolRoute::from_waypoints(
            PatrolRouteId::new(next_id),
            waypoints,
            routing,
        ) else {
            eprintln!(
                "warning: patrol route #{} (district {}) failed to compile, skipping",
                entry_idx, entry.district_id
            );
            continue;
        };
        next_id += 1;
        out.entry(did).or_default().push(Arc::new(route));
    }

    Ok(out)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a 3-waypoint route directly without a routing engine, by
    /// hand-constructing the struct. Used to test the position math in
    /// isolation from routing.
    fn handmade_route(durations: Vec<u32>, ids: Vec<u32>) -> PatrolRoute {
        assert_eq!(durations.len() + 1, ids.len(), "durations vs waypoint count mismatch");
        let mut cumulative = Vec::with_capacity(ids.len());
        cumulative.push(0u32);
        let mut acc = 0u32;
        for &d in &durations { acc += d; cumulative.push(acc); }
        PatrolRoute {
            id: PatrolRouteId::new(0),
            waypoints: ids.into_iter().map(NodeId::new).collect(),
            segment_durations_min: durations,
            cumulative_min: cumulative,
            total_min: acc,
        }
    }

    #[test]
    fn position_at_start_is_first_waypoint() {
        let r = handmade_route(vec![5, 5, 5], vec![10, 20, 30, 10]);
        assert_eq!(r.position_at(0, 0), NodeId::new(10));
    }

    #[test]
    fn position_mid_segment() {
        let r = handmade_route(vec![5, 5, 5], vec![10, 20, 30, 10]);
        // 3 minutes into the first segment → still on waypoint 10.
        assert_eq!(r.position_at(0, 3), NodeId::new(10));
        // Exactly at segment boundary: snaps to the new waypoint.
        assert_eq!(r.position_at(0, 5), NodeId::new(20));
        // Mid second segment.
        assert_eq!(r.position_at(0, 7), NodeId::new(20));
    }

    #[test]
    fn position_wraps_at_loop_end() {
        let r = handmade_route(vec![5, 5, 5], vec![10, 20, 30, 10]);
        // total = 15. After 16 minutes we are 1 min into the next loop → first waypoint.
        assert_eq!(r.position_at(0, 16), NodeId::new(10));
        // After 21 minutes (one full loop + 6 min) → mid second segment.
        assert_eq!(r.position_at(0, 21), NodeId::new(20));
    }

    #[test]
    fn position_with_started_at_offset() {
        let r = handmade_route(vec![5, 5, 5], vec![10, 20, 30, 10]);
        // Started at t=100, query at t=107 → 7 minutes elapsed → mid second seg.
        assert_eq!(r.position_at(100, 107), NodeId::new(20));
    }
}
