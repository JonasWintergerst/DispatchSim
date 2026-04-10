// bench_dijkstra — benchmark Dijkstra-based routing operations on the real
// Hamburg OSM graph.
//
// Usage:
//   cargo run --release --bin bench_dijkstra -- config/hamburg-latest.osm.pbf config/hexes.json
//
// Benchmarks:
//   1. Single-source Dijkstra (full graph, one station)
//   2. Bounded single-source Dijkstra (max_cost=10, hex neighbourhood)
//   3. RoutingEngine::from_graph — all-anchors precomputation (the big one)

use std::collections::HashSet;
use std::path::Path;
use std::time::Instant;

use dispatch_sim::osm::OsmGraph;
use dispatch_sim::routing::RoutingEngine;
use dispatch_sim::types::NodeId;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: bench_dijkstra <osm.pbf> <hexes.json>");
        std::process::exit(1);
    }
    let osm_path = &args[1];
    let hexes_path = &args[2];

    // ── Load OSM graph ──────────────────────────────────────────────────
    println!("Loading OSM graph from: {osm_path}");
    let t0 = Instant::now();
    let osm = OsmGraph::load(Path::new(osm_path)).expect("failed to load OSM");
    let osm_load_ms = t0.elapsed().as_millis();
    println!("  → {} nodes, {} edges  ({osm_load_ms} ms)\n", osm.node_count(), osm.edge_count());

    // ── Load hexes.json to get anchor nodes ─────────────────────────────
    #[derive(serde::Deserialize)]
    struct HexEntry {
        nearest_osm_node: u32,
        #[allow(dead_code)]
        lat: f64,
        #[allow(dead_code)]
        lon: f64,
    }

    let raw = std::fs::read_to_string(hexes_path).expect("failed to read hexes.json");
    let hexes: Vec<HexEntry> = serde_json::from_str(&raw).expect("failed to parse hexes.json");
    let mut anchor_ids: Vec<NodeId> = hexes.iter().map(|h| NodeId::new(h.nearest_osm_node)).collect();
    anchor_ids.sort_by_key(|n| n.value());
    anchor_ids.dedup();
    println!("Anchors from hexes.json: {} unique nodes\n", anchor_ids.len());

    let hex_node_set: HashSet<NodeId> = anchor_ids.iter().copied().collect();

    // Pick a few representative nodes for single-source benchmarks.
    let sample_nodes: Vec<NodeId> = {
        let step = anchor_ids.len() / 5;
        (0..5).map(|i| anchor_ids[i * step]).collect()
    };

    // ── Benchmark 1: single-source Dijkstra (full graph) ───────────────
    println!("=== Benchmark 1: single_source_travel_times (full graph) ===");
    for (i, &node) in sample_nodes.iter().enumerate() {
        let t = Instant::now();
        let result = osm.single_source_travel_times(node);
        let ms = t.elapsed().as_millis();
        println!("  run {}: node={:>6}  → {} reachable nodes  ({ms} ms)",
            i + 1, node.value(), result.len());
    }
    println!();

    // ── Benchmark 2: bounded single-source Dijkstra (max_cost=10) ──────
    println!("=== Benchmark 2: bounded_single_source (max_cost=10 min) ===");
    for (i, &node) in sample_nodes.iter().enumerate() {
        let t = Instant::now();
        let result = osm.bounded_single_source(node, &hex_node_set, 10);
        let ms = t.elapsed().as_millis();
        println!("  run {}: node={:>6}  → {} targets hit  ({ms} ms)",
            i + 1, node.value(), result.len());
    }
    println!();

    // ── Benchmark 3: bounded single-source batch (all hex nodes) ───────
    println!("=== Benchmark 3: bounded Dijkstra batch ({} hex nodes, max_cost=10) ===", anchor_ids.len());
    {
        let t = Instant::now();
        let _results: Vec<_> = anchor_ids.iter().map(|&node| {
            osm.bounded_single_source(node, &hex_node_set, 10)
        }).collect();
        let ms = t.elapsed().as_millis();
        let secs = ms as f64 / 1000.0;
        println!("  sequential: {secs:.2} s  ({:.2} ms/node)\n",
            ms as f64 / anchor_ids.len() as f64);
    }
    {
        use rayon::prelude::*;
        let t = Instant::now();
        let _results: Vec<_> = anchor_ids.par_iter().map(|&node| {
            osm.bounded_single_source(node, &hex_node_set, 10)
        }).collect();
        let ms = t.elapsed().as_millis();
        let secs = ms as f64 / 1000.0;
        println!("  parallel:   {secs:.2} s  ({:.2} ms/node)\n",
            ms as f64 / anchor_ids.len() as f64);
    }

    // ── Benchmark 4: RoutingEngine::from_graph (the expensive one) ─────
    // Test with increasing anchor counts to show scaling.
    let anchor_counts = [10, 50, 100, 500, anchor_ids.len()];
    println!("=== Benchmark 4: RoutingEngine::from_graph (city-wide OSM) ===");
    println!("  Graph: {} nodes, {} edges", osm.node_count(), osm.edge_count());
    println!("  (runs Dijkstra from each anchor to all others)\n");

    for &count in &anchor_counts {
        let subset: Vec<NodeId> = anchor_ids.iter().copied().take(count).collect();
        let label = if count == anchor_ids.len() {
            format!("{count} (ALL)")
        } else {
            format!("{count}")
        };

        let graph = osm.to_road_graph();
        let t = Instant::now();
        let engine = RoutingEngine::from_graph(graph, &subset);
        let ms = t.elapsed().as_millis();
        let secs = ms as f64 / 1000.0;

        // Spot-check a travel time to make sure it worked.
        let tt = if subset.len() >= 2 {
            engine.travel_time(subset[0], subset[1])
        } else {
            0
        };
        println!("  {label:>8} anchors → {secs:>8.2} s  ({:.1} ms/anchor)  sample_tt={tt}",
            ms as f64 / count as f64);
    }
    println!();

    println!("Done.");
}
