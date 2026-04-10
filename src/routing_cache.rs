// routing_cache.rs
// Binary on-disk routing cache shared by the optimizer (writer) and the
// simulator (reader). Each district carries its own RoutingEngine subgraph
// and its own hex → OSM-node mapping; the cache serialises both.
//
// The optimizer produces this file once after districting; the simulator
// loads it on every run and refuses to start without it.

use std::collections::HashMap;
use std::path::Path;

use crate::routing::RoutingSnapshot;

/// On-disk format for the routing cache file.
/// Contains a single city-wide graph snapshot shared by all districts,
/// plus per-district hex → OSM node mappings.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct RoutingCacheFile {
    /// Single city-wide road graph and precomputed anchor travel times.
    pub city_snapshot: RoutingSnapshot,
    /// Per-district hex → nearest OSM road node: `(district_id, Vec<(h3_index, node_id_raw)>)`.
    pub district_hex_nodes: Vec<(u32, Vec<(u64, u32)>)>,
}

/// In-memory representation of a loaded routing cache.
pub struct LoadedRoutingCache {
    /// City-wide routing snapshot (one graph for all districts).
    pub city_snapshot: RoutingSnapshot,
    /// district_id → (h3_index → nearest_road_node raw id)
    pub hex_nodes: HashMap<u32, HashMap<u64, u32>>,
}

/// Load a routing cache file from `path`. Returns `None` if the file does not
/// exist or cannot be decoded.
pub fn load(path: &str) -> Option<LoadedRoutingCache> {
    let data = std::fs::read(path).ok()?;
    let (file, _): (RoutingCacheFile, _) = bincode::serde::decode_from_slice(
        &data,
        bincode::config::standard(),
    ).ok()?;

    let n_districts = file.district_hex_nodes.len();
    println!("Loaded routing cache from: {} ({} districts, city-wide graph)", path, n_districts);

    let mut hex_nodes = HashMap::new();
    for (did, nodes) in file.district_hex_nodes {
        hex_nodes.insert(did, nodes.into_iter().collect());
    }
    Some(LoadedRoutingCache { city_snapshot: file.city_snapshot, hex_nodes })
}

/// Write a routing cache file to `path`. Creates parent directories as needed.
pub fn save(path: &str, city_snapshot: RoutingSnapshot, district_hex_nodes: Vec<(u32, Vec<(u64, u32)>)>) {
    let n_districts = district_hex_nodes.len();
    let file = RoutingCacheFile { city_snapshot, district_hex_nodes };

    let data = bincode::serde::encode_to_vec(&file, bincode::config::standard())
        .expect("failed to encode routing cache");

    if let Some(parent) = Path::new(path).parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(path, data).expect("failed to write routing cache");
    println!("Saved routing cache to: {} ({} districts, city-wide graph)", path, n_districts);
}
