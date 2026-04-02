// osm.rs
// Parse a Hamburg OSM PBF file into a RoadGraph.
// Assigns sequential internal NodeIds (u32) to all OSM nodes referenced by
// highway ways.  OSM node IDs (i64/u64) are not exposed outside this module.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use geo::{BoundingRect, Coord, LineString, Point, Polygon};
use osmpbf::{Element, ElementReader, RelMemberType};
use petgraph::graph::NodeIndex;
use rstar::{RTree, RTreeObject, AABB, PointDistance};

use crate::geo_utils::haversine_m;
use crate::routing::{Edge, Node, RoadGraph};
use crate::types::NodeId;

// ---------------------------------------------------------------------------
// Police station POI extracted from OSM.

#[derive(serde::Serialize, serde::Deserialize)]
pub struct PoliceStation {
    pub name: String,
    pub lat:  f64,
    pub lon:  f64,
}

// ---------------------------------------------------------------------------
// R-tree entry for O(log n) nearest-node queries.

#[derive(Clone)]
struct RTreeNode {
    id:  NodeId,
    lat: f64,
    lon: f64,
}

impl RTreeObject for RTreeNode {
    type Envelope = AABB<[f64; 2]>;
    fn envelope(&self) -> Self::Envelope {
        AABB::from_point([self.lon, self.lat])
    }
}

impl PointDistance for RTreeNode {
    fn distance_2(&self, point: &[f64; 2]) -> f64 {
        let dlat = self.lat - point[1];
        let dlon = self.lon - point[0];
        dlat * dlat + dlon * dlon
    }
}

// ---------------------------------------------------------------------------

pub struct OsmGraph {
    graph:  RoadGraph,
    rtree:  RTree<RTreeNode>,
    /// (NodeId, lat, lon) kept for bbox filtering in subgraph_for_bbox.
    node_positions: Vec<(NodeId, f64, f64)>,
}

impl OsmGraph {
    /// Parse the PBF at `path` and build a road graph over all highway ways.
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let reader = ElementReader::from_path(path)?;

        // Single pass: collect raw node positions + highway ways.
        let mut raw_nodes: HashMap<i64, (f64, f64)> = HashMap::new();
        let mut highway_ways: Vec<(String, Vec<i64>)> = Vec::new();

        reader.for_each(|element| match element {
            Element::Node(n) => {
                raw_nodes.insert(n.id(), (n.lat(), n.lon()));
            }
            Element::DenseNode(n) => {
                raw_nodes.insert(n.id(), (n.lat(), n.lon()));
            }
            Element::Way(w) => {
                let highway = w
                    .tags()
                    .find(|(k, _)| *k == "highway")
                    .map(|(_, v)| v.to_string());
                if let Some(hw) = highway {
                    let refs: Vec<i64> = w.refs().collect();
                    if refs.len() >= 2 {
                        highway_ways.push((hw, refs));
                    }
                }
            }
            Element::Relation(_) => {}
        })?;

        // Assign sequential NodeIds to OSM nodes referenced by highway ways.
        let relevant: HashSet<i64> = highway_ways
            .iter()
            .flat_map(|(_, refs)| refs.iter().copied())
            .collect();

        let mut graph = RoadGraph::new();
        let mut osm_to_nx: HashMap<i64, NodeIndex> = HashMap::with_capacity(relevant.len());
        let mut node_positions: Vec<(NodeId, f64, f64)> = Vec::with_capacity(relevant.len());
        let mut next_id: u32 = 0;

        let mut relevant: Vec<i64> = relevant.into_iter().collect();
        relevant.sort_unstable();
        for osm_id in &relevant {
            if let Some(&(lat, lon)) = raw_nodes.get(osm_id) {
                let node_id = NodeId::new(next_id);
                next_id += 1;
                let nx = graph.add_node(Node {
                    id:       node_id,
                    position: Point::new(lon, lat), // geo convention: (x=lon, y=lat)
                });
                osm_to_nx.insert(*osm_id, nx);
                node_positions.push((node_id, lat, lon));
            }
        }

        // Add directed edges in both directions for each way segment.
        for (highway, refs) in &highway_ways {
            let speed = speed_m_per_min(highway);
            for window in refs.windows(2) {
                let (a_osm, b_osm) = (window[0], window[1]);
                if let (Some(&a_nx), Some(&b_nx)) = (osm_to_nx.get(&a_osm), osm_to_nx.get(&b_osm)) {
                    if let (Some(&(a_lat, a_lon)), Some(&(b_lat, b_lon))) =
                        (raw_nodes.get(&a_osm), raw_nodes.get(&b_osm))
                    {
                        let dist_m   = haversine_m(a_lat, a_lon, b_lat, b_lon);
                        let time_min = ((dist_m / speed) as u32).max(1);
                        graph.add_edge(a_nx, b_nx, Edge { travel_time_min: time_min });
                        graph.add_edge(b_nx, a_nx, Edge { travel_time_min: time_min });
                    }
                }
            }
        }

        let rtree = RTree::bulk_load(
            node_positions.iter().map(|&(id, lat, lon)| RTreeNode { id, lat, lon }).collect()
        );

        Ok(Self { graph, rtree, node_positions })
    }

    /// Nearest road node to (lat, lon). O(log n) via R-tree.
    pub fn nearest_node(&self, lat: f64, lon: f64) -> NodeId {
        self.rtree
            .nearest_neighbor(&[lon, lat])
            .map(|n| n.id)
            .expect("OsmGraph has no nodes")
    }

    /// Return a `RoadGraph` containing only nodes inside the buffered bbox,
    /// preserving the same `NodeId` values as the parent graph.
    pub fn subgraph_for_bbox(
        &self,
        lat_min: f64,
        lat_max: f64,
        lon_min: f64,
        lon_max: f64,
        buffer_deg: f64,
    ) -> RoadGraph {
        let lb  = lat_min - buffer_deg;
        let lub = lat_max + buffer_deg;
        let lob = lon_min - buffer_deg;
        let loub = lon_max + buffer_deg;

        let in_bbox: HashSet<NodeId> = self
            .node_positions
            .iter()
            .filter(|(_, lat, lon)| *lat >= lb && *lat <= lub && *lon >= lob && *lon <= loub)
            .map(|(id, _, _)| *id)
            .collect();

        let mut sub = RoadGraph::new();
        let mut old_to_new: HashMap<NodeIndex, NodeIndex> = HashMap::new();

        for nx in self.graph.node_indices() {
            let node = &self.graph[nx];
            if in_bbox.contains(&node.id) {
                let new_nx = sub.add_node(Node { id: node.id, position: node.position });
                old_to_new.insert(nx, new_nx);
            }
        }

        for edge_idx in self.graph.edge_indices() {
            let (a, b) = self.graph.edge_endpoints(edge_idx).unwrap();
            if let (Some(&new_a), Some(&new_b)) = (old_to_new.get(&a), old_to_new.get(&b)) {
                let weight = self.graph[edge_idx].travel_time_min;
                sub.add_edge(new_a, new_b, Edge { travel_time_min: weight });
            }
        }

        sub
    }

    pub fn node_count(&self) -> usize { self.graph.node_count() }
    pub fn edge_count(&self) -> usize { self.graph.edge_count() }

    /// Returns the raw u32 node IDs of all nodes in the largest strongly-connected
    /// component of the road graph. Used to filter out hexes in disconnected enclaves
    /// (e.g. Neuwerk island) that have no road connection to the main network.
    pub fn main_component_node_ids(&self) -> HashSet<NodeId> {
        use petgraph::algo::kosaraju_scc;
        let sccs = kosaraju_scc(&self.graph);
        let largest = sccs.into_iter().max_by_key(|c| c.len()).unwrap_or_default();
        largest.into_iter().map(|nx| self.graph[nx].id).collect()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Approximate road speed in metres per minute by OSM highway classification.
fn speed_m_per_min(highway: &str) -> f64 {
    let km_h: f64 = match highway {
        "motorway" | "motorway_link" => 100.0,
        "trunk"    | "trunk_link"    =>  80.0,
        "primary"  | "primary_link"  =>  60.0,
        "secondary"| "secondary_link"=>  50.0,
        "tertiary" | "tertiary_link" | "unclassified" => 40.0,
        "residential" | "living_street"               => 30.0,
        _                                             => 20.0,
    };
    km_h * 1000.0 / 60.0
}

// haversine_m moved to geo_utils — re-exported here for callers within this module.

// ---------------------------------------------------------------------------
// Boundary extraction
// ---------------------------------------------------------------------------

/// Extract Hamburg's administrative boundary from the OSM PBF.
///
/// Looks for the relation with `boundary=administrative`, `admin_level=4`,
/// `name=Hamburg`. Assembles all outer-member ways into closed rings, then
/// returns the **largest polygon by bounding-box area** — this naturally
/// excludes the Neuwerk island group far out in the North Sea.
pub fn extract_admin_boundary(path: &Path) -> Result<Polygon<f64>, Box<dyn std::error::Error>> {
    // ----- Pass 1: find the Hamburg boundary relation → outer way IDs -----
    let mut outer_way_ids: Vec<i64> = Vec::new();

    {
        let reader = ElementReader::from_path(path)?;
        reader.for_each(|element| {
            if let Element::Relation(r) = element {
                let mut is_boundary  = false;
                let mut is_admin     = false;
                let mut is_hamburg   = false;
                let mut is_level_4   = false;

                for (k, v) in r.tags() {
                    match k {
                        "type"        if v == "boundary"       => is_boundary  = true,
                        "boundary"    if v == "administrative"  => is_admin     = true,
                        "admin_level" if v == "4"               => is_level_4   = true,
                        "name"        if v == "Hamburg"         => is_hamburg   = true,
                        _ => {}
                    }
                }

                if is_boundary && is_admin && is_level_4 && is_hamburg {
                    for member in r.members() {
                        if matches!(member.member_type, RelMemberType::Way)
                            && member.role().map(|r| r == "outer").unwrap_or(false)
                        {
                            outer_way_ids.push(member.member_id);
                        }
                    }
                }
            }
        })?;
    }

    if outer_way_ids.is_empty() {
        return Err("Hamburg admin_level=4 boundary relation not found in OSM file".into());
    }

    let outer_set: HashSet<i64> = outer_way_ids.iter().copied().collect();

    // ----- Pass 2: collect way node sequences for boundary ways -----
    let mut way_nodes: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut needed_node_ids: HashSet<i64>     = HashSet::new();

    {
        let reader = ElementReader::from_path(path)?;
        reader.for_each(|element| {
            if let Element::Way(w) = element {
                if outer_set.contains(&w.id()) {
                    let refs: Vec<i64> = w.refs().collect();
                    for &n in &refs {
                        needed_node_ids.insert(n);
                    }
                    way_nodes.insert(w.id(), refs);
                }
            }
        })?;
    }

    // ----- Pass 3: collect node coordinates for boundary nodes -----
    let mut node_coords: HashMap<i64, (f64, f64)> = HashMap::new();

    {
        let reader = ElementReader::from_path(path)?;
        reader.for_each(|element| match element {
            Element::Node(n) => {
                if needed_node_ids.contains(&n.id()) {
                    node_coords.insert(n.id(), (n.lat(), n.lon()));
                }
            }
            Element::DenseNode(n) => {
                if needed_node_ids.contains(&n.id()) {
                    node_coords.insert(n.id(), (n.lat(), n.lon()));
                }
            }
            _ => {}
        })?;
    }

    // ----- Assemble ways into closed rings -----
    let segs: Vec<Vec<i64>> = outer_way_ids
        .iter()
        .filter_map(|id| way_nodes.get(id).cloned())
        .collect();

    let rings = chain_ways_into_rings(segs);

    if rings.is_empty() {
        return Err("could not assemble any boundary ring from OSM ways".into());
    }

    // Convert each ring to a geo::Polygon and pick the largest.
    let polygons: Vec<Polygon<f64>> = rings
        .into_iter()
        .filter_map(|ring| {
            let coords: Vec<Coord<f64>> = ring
                .iter()
                .filter_map(|id| {
                    node_coords.get(id).map(|&(lat, lon)| Coord { x: lon, y: lat })
                })
                .collect();
            if coords.len() < 3 {
                return None;
            }
            Some(Polygon::new(LineString::from(coords), vec![]))
        })
        .collect();

    polygons
        .into_iter()
        .max_by(|a, b| {
            let size = |p: &Polygon<f64>| {
                p.bounding_rect()
                    .map(|r| (r.max().x - r.min().x) * (r.max().y - r.min().y))
                    .unwrap_or(0.0)
            };
            size(a).partial_cmp(&size(b)).unwrap_or(std::cmp::Ordering::Equal)
        })
        .ok_or_else(|| "no valid boundary polygons found".into())
}

/// Chain OSM way node sequences into closed rings.
///
/// Each way is a `Vec<i64>` of node IDs. Ways are joined by matching the last
/// node of one way with the first node of the next (flipping if needed).
/// Multiple disjoint rings (e.g. mainland + island) are returned separately.
fn chain_ways_into_rings(mut segs: Vec<Vec<i64>>) -> Vec<Vec<i64>> {
    if segs.is_empty() { return vec![]; }

    // endpoint_node → list of (seg_idx, at_start: bool)
    let mut endpoint_map: HashMap<i64, Vec<(usize, bool)>> = HashMap::new();
    for (i, seg) in segs.iter().enumerate() {
        if let Some(&first) = seg.first() {
            endpoint_map.entry(first).or_default().push((i, true));
        }
        if let Some(&last) = seg.last() {
            endpoint_map.entry(last).or_default().push((i, false));
        }
    }

    let n = segs.len();
    let mut used = vec![false; n];
    let mut rings: Vec<Vec<i64>> = Vec::new();
    let mut start = 0;

    while start < n {
        while start < n && used[start] { start += 1; }
        if start >= n { break; }

        used[start] = true;
        let mut ring: Vec<i64> = std::mem::take(&mut segs[start]);
        let ring_start = *ring.first().unwrap();

        loop {
            let current_end = *ring.last().unwrap();
            if current_end == ring_start && ring.len() > 1 { break; }

            let candidates = endpoint_map.get(&current_end).cloned().unwrap_or_default();
            let mut found = false;
            for (idx, at_start) in candidates {
                if used[idx] { continue; }
                used[idx] = true;
                found = true;
                let mut seg = std::mem::take(&mut segs[idx]);
                if !at_start { seg.reverse(); }
                ring.extend_from_slice(&seg[1..]);
                break;
            }
            if !found { break; }
        }

        rings.push(ring);
    }

    rings
}

// ---------------------------------------------------------------------------
// Police station extraction
// ---------------------------------------------------------------------------

/// Extract police station POIs (`amenity=police`) from the OSM PBF.
///
/// Handles both node-tagged stations and way-tagged building footprints
/// (using the position of the first node in the way as the location).
pub fn extract_police_stations(path: &Path) -> Result<Vec<PoliceStation>, Box<dyn std::error::Error>> {
    let mut stations: Vec<PoliceStation>    = Vec::new();
    let mut pending_ways: Vec<(String, i64)> = Vec::new(); // (name, first_node_ref)
    let mut needed_node_ids: HashSet<i64>   = HashSet::new();

    // ----- Pass 1: collect nodes and way refs with amenity=police -----
    {
        let reader = ElementReader::from_path(path)?;
        reader.for_each(|element| match element {
            Element::Node(n) => {
                let mut is_police = false;
                let mut name = String::new();
                for (k, v) in n.tags() {
                    match k {
                        "amenity" if v == "police" => is_police = true,
                        "name"                     => name = v.to_string(),
                        _                          => {}
                    }
                }
                if is_police {
                    stations.push(PoliceStation { name, lat: n.lat(), lon: n.lon() });
                }
            }
            Element::Way(w) => {
                let mut is_police = false;
                let mut name = String::new();
                for (k, v) in w.tags() {
                    match k {
                        "amenity" if v == "police" => is_police = true,
                        "name"                     => name = v.to_string(),
                        _                          => {}
                    }
                }
                if is_police {
                    if let Some(&first_ref) = w.refs().next().as_ref() {
                        needed_node_ids.insert(first_ref);
                        pending_ways.push((name, first_ref));
                    }
                }
            }
            _ => {}
        })?;
    }

    // ----- Pass 2 (only if there are way-tagged stations): resolve node positions -----
    if !pending_ways.is_empty() {
        let mut node_coords: HashMap<i64, (f64, f64)> = HashMap::new();

        let reader = ElementReader::from_path(path)?;
        reader.for_each(|element| match element {
            Element::Node(n) => {
                if needed_node_ids.contains(&n.id()) {
                    node_coords.insert(n.id(), (n.lat(), n.lon()));
                }
            }
            Element::DenseNode(n) => {
                if needed_node_ids.contains(&n.id()) {
                    node_coords.insert(n.id(), (n.lat(), n.lon()));
                }
            }
            _ => {}
        })?;

        for (name, node_id) in pending_ways {
            if let Some(&(lat, lon)) = node_coords.get(&node_id) {
                stations.push(PoliceStation { name, lat, lon });
            }
        }
    }

    stations.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(stations)
}
