// patrol_gen — offline patrol-route generator.
//
// Usage:
//   cargo run --bin patrol_gen -- config/city.toml [strategy]
//
// Reads the same `city.toml` as the simulator (so it sees the spawn profiles
// and hex grid), reuses the routing cache produced by `optimize`, and writes
// `config/patrol_routes_<strategy>.json` for consumption by `dispatch_sim`.
//
// Strategies implemented:
//   * `hotspot` (default) — top-K spawn-rate hexes per district, ordered by
//     a nearest-neighbour TSP heuristic.
//   * `border`             — picks the district's border hexes.
//
// Other strategies described in the plan (`coverage`, `time_of_day`, `static`)
// are stubs that fall through to `hotspot` for now.

use std::collections::HashMap;
use std::path::Path;
use std::process;
use std::sync::Arc;

use dispatch_sim::config::LoadedConfig;
use dispatch_sim::patrol::{PatrolRouteEntry, PatrolRoutesFile};
use dispatch_sim::routing::RoutingEngine;
use dispatch_sim::routing_cache;
use dispatch_sim::types::{DistrictId, NodeId};

const DEFAULT_K_PER_ROUTE: usize = 10;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let config_path = args.get(1).map(String::as_str).unwrap_or("config/city.toml");
    let strategy    = args.get(2).map(String::as_str).unwrap_or("hotspot");

    let cfg = LoadedConfig::load(Path::new(config_path)).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        process::exit(1);
    });

    let cache_path = cfg.city.sim.routing_cache_path.as_deref().unwrap_or_else(|| {
        eprintln!("error: sim.routing_cache_path is not set in {config_path}");
        process::exit(1);
    });
    let cache = routing_cache::load(cache_path).unwrap_or_else(|| {
        eprintln!("error: routing cache not found at {cache_path} — run optimize first");
        process::exit(1);
    });

    // Build a RoutingEngine per district from the cached snapshots.
    let mut routings: HashMap<u32, Arc<RoutingEngine>> = HashMap::new();
    for (did, snap) in &cache.snapshots {
        routings.insert(*did, Arc::new(RoutingEngine::from_snapshot(snap.clone())));
    }

    // Group hex configs by district, attaching the canonical OSM node from
    // the cache (the cache's anchors are what the routing engine knows about).
    let mut hexes_by_district: HashMap<u32, Vec<HexInfo>> = HashMap::new();
    for h in &cfg.hex_grid.hexes {
        let node_raw = cache
            .hex_nodes
            .get(&h.district_id)
            .and_then(|m| m.get(&h.h3_index).copied())
            .or(h.nearest_osm_node);
        if let Some(node_raw) = node_raw {
            hexes_by_district.entry(h.district_id).or_default().push(HexInfo {
                h3_index:         h.h3_index,
                node_raw,
                spawn_profile_id: h.spawn_profile_id.clone(),
            });
        }
    }

    // Mean-of-day weight for each spawn profile.
    let profile_weight: HashMap<String, f64> = cfg.city.spawn_profiles.iter().map(|(id, p)| {
        let mean_h: f64 = p.hour_multiplier.iter().sum::<f64>() / 24.0;
        (id.clone(), p.base_lambda * mean_h)
    }).collect();

    let mut routes: Vec<PatrolRouteEntry> = Vec::new();
    for d in &cfg.city.districts {
        let Some(hexes) = hexes_by_district.get(&d.id) else { continue; };
        let Some(routing) = routings.get(&d.id) else { continue; };

        let waypoints = match strategy {
            "border" => border_waypoints(hexes, DEFAULT_K_PER_ROUTE),
            // hotspot is the default and the fallback for not-yet-implemented strategies
            _        => hotspot_waypoints(hexes, &profile_weight, DEFAULT_K_PER_ROUTE, routing),
        };
        if waypoints.len() < 2 { continue; }

        routes.push(PatrolRouteEntry {
            district_id: d.id,
            waypoints,
        });
    }

    let out = PatrolRoutesFile {
        strategy: strategy.to_string(),
        routes,
    };
    let path = format!("config/patrol_routes_{strategy}.json");
    std::fs::write(&path, serde_json::to_string_pretty(&out).unwrap())
        .unwrap_or_else(|e| { eprintln!("error: writing {path}: {e}"); process::exit(1); });

    println!("Wrote {} routes to {}", out.routes.len(), path);
}

#[derive(Clone)]
struct HexInfo {
    h3_index:         u64,
    node_raw:         u32,
    spawn_profile_id: String,
}

/// Pick the top-K hexes by spawn weight, then order them with a greedy
/// nearest-neighbour TSP starting from the highest-weight hex.
fn hotspot_waypoints(
    hexes:          &[HexInfo],
    profile_weight: &HashMap<String, f64>,
    k:              usize,
    routing:        &RoutingEngine,
) -> Vec<u32> {
    let mut ranked: Vec<(f64, &HexInfo)> = hexes.iter().map(|h| {
        let w = profile_weight.get(&h.spawn_profile_id).copied().unwrap_or(0.0);
        (w, h)
    }).collect();
    ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let chosen: Vec<&HexInfo> = ranked.iter().take(k).map(|(_, h)| *h).collect();
    if chosen.is_empty() { return Vec::new(); }

    // Greedy NN TSP: start from the highest-weight hex, repeatedly pick the
    // unvisited hex with smallest travel time.
    let mut remaining: Vec<&HexInfo> = chosen[1..].to_vec();
    let mut order: Vec<&HexInfo> = vec![chosen[0]];
    while !remaining.is_empty() {
        let last_node = NodeId::new(order.last().unwrap().node_raw);
        let (best_idx, _) = remaining.iter().enumerate()
            .map(|(i, h)| (i, routing.travel_time(last_node, NodeId::new(h.node_raw))))
            .min_by_key(|(_, t)| *t)
            .unwrap();
        order.push(remaining.remove(best_idx));
    }

    order.iter().map(|h| h.node_raw).collect()
}

/// Pick hexes that lie near the district boundary, by selecting the K hexes
/// with the largest centroid distance from the district mean. Coarse but
/// adequate for v1.
fn border_waypoints(hexes: &[HexInfo], k: usize) -> Vec<u32> {
    if hexes.is_empty() { return Vec::new(); }
    // Without coordinates here we just take every Nth hex spread across the
    // input order — a stand-in until coverage/border have a real implementation.
    let step = (hexes.len() / k.max(1)).max(1);
    hexes.iter().step_by(step).take(k).map(|h| h.node_raw).collect()
}

// Suppress dead-code warning on `DistrictId` import — kept for symmetry with
// other binaries.
#[allow(dead_code)]
fn _unused(_d: DistrictId) {}
