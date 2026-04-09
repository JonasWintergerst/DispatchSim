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

/// One entry in the routing cache: `(district_id, snapshot, hex_node_assignments)`
/// where `hex_node_assignments` is `Vec<(h3_index, nearest_road_node_raw)>`.
pub type DistrictCacheEntry = (u32, RoutingSnapshot, Vec<(u64, u32)>);

/// On-disk format for the routing cache file.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct RoutingCacheFile {
    pub districts: Vec<DistrictCacheEntry>,
}

/// In-memory representation of a loaded routing cache.
pub struct LoadedRoutingCache {
    /// district_id → RoutingSnapshot
    pub snapshots: HashMap<u32, RoutingSnapshot>,
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

    println!("Loaded routing cache from: {} ({} districts)", path, file.districts.len());

    let mut snapshots = HashMap::new();
    let mut hex_nodes = HashMap::new();
    for (did, snap, nodes) in file.districts {
        snapshots.insert(did, snap);
        hex_nodes.insert(did, nodes.into_iter().collect());
    }
    Some(LoadedRoutingCache { snapshots, hex_nodes })
}

/// Write a routing cache file to `path`. Creates parent directories as needed.
pub fn save(path: &str, entries: Vec<DistrictCacheEntry>) {
    let count = entries.len();
    let file = RoutingCacheFile { districts: entries };

    let data = bincode::serde::encode_to_vec(&file, bincode::config::standard())
        .expect("failed to encode routing cache");

    if let Some(parent) = Path::new(path).parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(path, data).expect("failed to write routing cache");
    println!("Saved routing cache to: {} ({} districts)", path, count);
}
