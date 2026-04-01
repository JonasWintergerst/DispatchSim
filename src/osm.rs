// osm.rs
// Parse a Hamburg OSM PBF file into a RoadGraph.
// Assigns sequential internal NodeIds (u32) to all OSM nodes referenced by
// highway ways.  OSM node IDs (i64/u64) are not exposed outside this module.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use geo::Point;
use osmpbf::{Element, ElementReader};
use petgraph::graph::NodeIndex;
use rstar::{RTree, RTreeObject, AABB, PointDistance};

use crate::geo_utils::haversine_m;
use crate::routing::{Edge, Node, RoadGraph};
use crate::types::NodeId;

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
