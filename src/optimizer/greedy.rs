// optimizer/greedy.rs
// Greedy p-median solver.
//
// Complexity: O(p × n²) — suitable for H3 resolution 9 (~7.5k hexes, ~20 s).
// For resolution 10 (~50k hexes) use the SimulatedAnnealing solver instead.
//
// Steps:
//   1. Precompute n×n haversine distance matrix (Rayon parallel).
//   2. Greedy station selection: pick p stations that maximally reduce the
//      weighted travel-time objective.
//   3. Voronoi assignment: assign each hex to its nearest station.
//   4. Contiguity repair: BFS from each station; reassign disconnected hexes.
//   5. Workload balance repair: iterative border-swap until max/mean ≤ limit.

use std::collections::{HashMap, HashSet, VecDeque};

use h3o::CellIndex;
use rayon::prelude::*;

use crate::geo_utils::haversine_m;
use super::{CandidateStation, H3Hex, OptimizerError, Problem, Solution};

// Precomputed H3 adjacency list: adj[i] = indices of hex neighbours within the hex set.
type Adj = Vec<Vec<usize>>;

pub struct GreedySolver;

impl super::Solver for GreedySolver {
    fn solve(&self, problem: &Problem) -> Result<Solution, OptimizerError> {
        let hexes = &problem.hexes;
        let n     = hexes.len();
        let p     = problem.n_districts;

        if n < p {
            return Err(OptimizerError(format!(
                "fewer hexes ({n}) than districts ({p})"
            )));
        }

        // Dispatch to candidate-based or hex-based path.
        if !problem.candidate_stations.is_empty() {
            return solve_candidate_based(
                hexes,
                &problem.candidate_stations,
                n, p,
                &problem.constraints,
                problem.distance_matrix.as_deref(),
                problem.adjacency_override.as_ref(),
            );
        }

        // --- Hex-based fallback (any hex can be a station) ---

        // 1. Distance matrix — flat row-major, dist[i*n + j] = haversine(i, j).
        println!("  Building {}×{} distance matrix…", n, n);
        let dist: Vec<f64> = (0..n)
            .into_par_iter()
            .flat_map(|i| {
                (0..n)
                    .map(|j| haversine_m(hexes[i].lat, hexes[i].lon, hexes[j].lat, hexes[j].lon))
                    .collect::<Vec<_>>()
            })
            .collect();

        let d = |i: usize, j: usize| dist[i * n + j];

        // 2. Greedy station selection.
        // cost[h] = spawn_rate[h] × dist to nearest open station (∞ initially).
        let mut cost: Vec<f64> = hexes.iter().map(|h| h.spawn_rate * f64::MAX / 2.0).collect();
        let mut station_indices: Vec<usize> = Vec::with_capacity(p);

        for round in 0..p {
            // For each candidate, compute the gain from opening it as station.
            let best = (0..n).into_par_iter().max_by(|&c1, &c2| {
                let g1 = gain_of(&cost, hexes, &dist, n, c1);
                let g2 = gain_of(&cost, hexes, &dist, n, c2);
                g1.partial_cmp(&g2).unwrap_or(std::cmp::Ordering::Equal)
            }).unwrap();

            station_indices.push(best);

            // Update cost: each hex's cost is its minimum to any open station.
            for h in 0..n {
                let new_cost = hexes[h].spawn_rate * d(h, best);
                if new_cost < cost[h] { cost[h] = new_cost; }
            }

            let total_obj: f64 = cost.iter().sum();
            println!("  Station {}/{}: hex {} selected — objective {:.1}", round + 1, p, best, total_obj);
        }

        // 3. Voronoi assignment (parallel: each hex independently picks nearest station).
        let mut assignments: Vec<usize> = (0..n)
            .into_par_iter()
            .map(|h| {
                station_indices.iter().copied().enumerate()
                    .min_by(|&(_, s1), &(_, s2)| d(h, s1).partial_cmp(&d(h, s2)).unwrap())
                    .map(|(district, _)| district)
                    .unwrap()
            })
            .collect();

        // Precompute adjacency list once; reused by both repair phases.
        let adj = match &problem.adjacency_override {
            Some(a) => a.clone(),
            None => {
                let cell_map = build_cell_map(hexes);
                build_adjacency(hexes, &cell_map)
            }
        };

        // In the hex-based path the station hex IS the anchor hex.
        let anchors = station_indices.clone();

        // 4. Contiguity repair.
        if problem.constraints.contiguity {
            repair_contiguity(&mut assignments, &adj, &anchors, p);
        }

        // 5. Workload balance repair.
        if let Some(ratio) = problem.constraints.max_workload_ratio {
            repair_workload(hexes, &mut assignments, &adj, &anchors, p, ratio);
        }

        // 6. Build solution.
        build_solution(hexes, assignments, station_indices, &dist, n, p)
    }
}

// ---------------------------------------------------------------------------
// Candidate-based p-median path
// ---------------------------------------------------------------------------

fn solve_candidate_based(
    hexes:              &[H3Hex],
    candidates:         &[CandidateStation],
    n:                  usize,
    p:                  usize,
    constraints:        &super::Constraints,
    distance_matrix:    Option<&[f64]>,
    adjacency_override: Option<&Vec<Vec<usize>>>,
) -> Result<Solution, super::OptimizerError> {
    let m = candidates.len();

    if m < p {
        return Err(super::OptimizerError(format!(
            "fewer candidate stations ({m}) than districts ({p})"
        )));
    }

    // 1. Build m×n distance matrix.
    //    Use precomputed road-network distances if available, otherwise haversine.
    let dist_cs: Vec<f64> = if let Some(dm) = distance_matrix {
        println!("  Using precomputed {}×{} road-distance matrix", m, n);
        dm.to_vec()
    } else {
        println!("  Building {}×{} candidate-to-hex distance matrix (haversine)…", m, n);
        (0..m)
            .into_par_iter()
            .flat_map(|c| {
                (0..n)
                    .map(|h| haversine_m(candidates[c].lat, candidates[c].lon, hexes[h].lat, hexes[h].lon))
                    .collect::<Vec<_>>()
            })
            .collect()
    };

    // 2. Greedy station selection over candidate set.
    let mut cost: Vec<f64> = hexes.iter().map(|h| h.spawn_rate * f64::MAX / 2.0).collect();
    let mut station_indices: Vec<usize> = Vec::with_capacity(p); // indices into candidates[]

    for round in 0..p {
        let best = (0..m).into_par_iter().max_by(|&c1, &c2| {
            let g1 = gain_of_candidate(&cost, hexes, &dist_cs, n, c1);
            let g2 = gain_of_candidate(&cost, hexes, &dist_cs, n, c2);
            g1.partial_cmp(&g2).unwrap_or(std::cmp::Ordering::Equal)
        }).unwrap();

        station_indices.push(best);

        for h in 0..n {
            let new_cost = hexes[h].spawn_rate * dist_cs[best * n + h];
            if new_cost < cost[h] { cost[h] = new_cost; }
        }

        let total_obj: f64 = cost.iter().sum();
        println!("  Station {}/{}: '{}' selected — objective {:.1}",
            round + 1, p, candidates[best].name, total_obj);
    }

    // 3. Voronoi assignment: each hex → nearest selected candidate.
    let mut assignments: Vec<usize> = (0..n)
        .into_par_iter()
        .map(|h| {
            station_indices.iter().copied().enumerate()
                .min_by(|&(_, c1), &(_, c2)| {
                    dist_cs[c1 * n + h].partial_cmp(&dist_cs[c2 * n + h]).unwrap()
                })
                .map(|(district, _)| district)
                .unwrap()
        })
        .collect();

    // Precompute adjacency list (use road-aware override if available).
    let adj = match adjacency_override {
        Some(a) => a.clone(),
        None => {
            let cell_map = build_cell_map(hexes);
            build_adjacency(hexes, &cell_map)
        }
    };

    // Anchor hex per district = hex in the district closest to its selected candidate station.
    let anchors = compute_anchor_hexes(hexes, &assignments, &station_indices, candidates, n, p);

    // 4. Contiguity repair.
    if constraints.contiguity {
        repair_contiguity(&mut assignments, &adj, &anchors, p);
    }

    // 5. Workload balance repair.
    if let Some(ratio) = constraints.max_workload_ratio {
        repair_workload(hexes, &mut assignments, &adj, &anchors, p, ratio);
    }

    // 6. Build solution.
    build_solution_candidate(hexes, assignments, station_indices, &dist_cs, n, p)
}

fn gain_of_candidate(
    cost:    &[f64],
    hexes:   &[H3Hex],
    dist_cs: &[f64],
    n:       usize,
    c:       usize,
) -> f64 {
    (0..n).map(|h| {
        let new_cost = hexes[h].spawn_rate * dist_cs[c * n + h];
        (cost[h] - new_cost).max(0.0)
    }).sum()
}

/// For each district, find the hex closest to its selected candidate station.
/// Used as the BFS anchor for contiguity / workload repair.
fn compute_anchor_hexes(
    hexes:           &[H3Hex],
    assigns:         &[usize],
    station_indices: &[usize],
    candidates:      &[CandidateStation],
    n:               usize,
    p:               usize,
) -> Vec<usize> {
    let mut anchors   = vec![0usize; p];
    let mut min_dists = vec![f64::MAX; p];

    for h in 0..n {
        let d  = assigns[h];
        let ci = station_indices[d];
        let dist = haversine_m(candidates[ci].lat, candidates[ci].lon, hexes[h].lat, hexes[h].lon);
        if dist < min_dists[d] {
            min_dists[d] = dist;
            anchors[d]   = h;
        }
    }
    anchors
}

fn build_solution_candidate(
    hexes:           &[H3Hex],
    assigns:         Vec<usize>,
    station_indices: Vec<usize>,
    dist_cs:         &[f64],
    n:               usize,
    p:               usize,
) -> Result<Solution, super::OptimizerError> {
    let objective: f64 = (0..n).map(|h| {
        hexes[h].spawn_rate * dist_cs[station_indices[assigns[h]] * n + h]
    }).sum();

    let loads = district_loads(hexes, &assigns, p);

    let mean = loads.iter().sum::<f64>() / p as f64;
    let max  = loads.iter().cloned().fold(f64::MIN, f64::max);
    println!(
        "  Workload ratio: {:.2}  (max {:.4}/min, mean {:.4}/min)",
        if mean > 0.0 { max / mean } else { 0.0 },
        max, mean
    );

    Ok(Solution {
        station_indices,
        assignments: assigns,
        objective,
        district_loads: loads,
    })
}

// ---------------------------------------------------------------------------
// Gain computation (hex-based fallback)
// ---------------------------------------------------------------------------

fn gain_of(
    cost:  &[f64],
    hexes: &[H3Hex],
    dist:  &[f64],
    n:     usize,
    c:     usize,
) -> f64 {
    (0..n).map(|h| {
        let new_cost = hexes[h].spawn_rate * dist[h * n + c];
        (cost[h] - new_cost).max(0.0)
    }).sum()
}

// ---------------------------------------------------------------------------
// Contiguity repair
// ---------------------------------------------------------------------------

fn repair_contiguity(
    assigns: &mut Vec<usize>,
    adj:     &Adj,
    anchors: &[usize],
    p:       usize,
) {
    let n = assigns.len();

    for d in 0..p {
        let start = anchors[d];

        // BFS from station hex within this district.
        let mut reachable: HashSet<usize> = HashSet::new();
        let mut queue:     VecDeque<usize> = VecDeque::new();
        reachable.insert(start);
        queue.push_back(start);

        while let Some(i) = queue.pop_front() {
            for &j in &adj[i] {
                if assigns[j] == d && reachable.insert(j) {
                    queue.push_back(j);
                }
            }
        }

        // Reassign disconnected hexes to an adjacent district.
        for i in 0..n {
            if assigns[i] == d && !reachable.contains(&i) {
                if let Some(new_d) = adj[i].iter().find(|&&j| assigns[j] != d).map(|&j| assigns[j]) {
                    assigns[i] = new_d;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Workload balance repair
// ---------------------------------------------------------------------------

fn repair_workload(
    hexes:   &[H3Hex],
    assigns: &mut Vec<usize>,
    adj:     &Adj,
    anchors: &[usize],
    p:       usize,
    limit:   f64,
) {
    let n = hexes.len();

    for _iter in 0..2_000 {
        let loads    = district_loads(hexes, assigns, p);
        let mean     = loads.iter().sum::<f64>() / p as f64;
        if mean == 0.0 { break; }
        let max_load = loads.iter().cloned().fold(f64::MIN, f64::max);
        if max_load / mean <= limit { break; }

        let over = loads.iter().copied().enumerate()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
            .map(|(d, _)| d).unwrap();
        let under = loads.iter().copied().enumerate()
            .min_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
            .map(|(d, _)| d).unwrap();
        if over == under { break; }

        // Find a border hex of `over` adjacent to `under` whose removal
        // keeps `over` connected.
        let mut swapped = false;
        for i in 0..n {
            if assigns[i] != over  { continue; }
            if i == anchors[over]  { continue; } // never move anchor hex

            let adj_to_under = adj[i].iter().any(|&j| assigns[j] == under);
            if !adj_to_under { continue; }

            if bfs_connected_without(assigns, adj, anchors[over], i, over) {
                assigns[i] = under;
                swapped = true;
                break;
            }
        }

        if !swapped { break; }
    }
}

// ---------------------------------------------------------------------------
// Build solution
// ---------------------------------------------------------------------------

fn build_solution(
    hexes:   &[H3Hex],
    assigns: Vec<usize>,
    stations: Vec<usize>,
    dist:    &[f64],
    n:       usize,
    p:       usize,
) -> Result<Solution, OptimizerError> {
    let objective: f64 = (0..n).map(|h| {
        hexes[h].spawn_rate * dist[h * n + stations[assigns[h]]]
    }).sum();

    let loads = district_loads(hexes, &assigns, p);

    let mean = loads.iter().sum::<f64>() / p as f64;
    let max  = loads.iter().cloned().fold(f64::MIN, f64::max);
    println!(
        "  Workload ratio: {:.2}  (max {:.4}/min, mean {:.4}/min)",
        if mean > 0.0 { max / mean } else { 0.0 },
        max, mean
    );

    Ok(Solution {
        station_indices: stations,
        assignments:     assigns,
        objective,
        district_loads:  loads,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn build_cell_map(hexes: &[H3Hex]) -> HashMap<u64, usize> {
    hexes.iter().enumerate().map(|(i, h)| (h.index, i)).collect()
}

fn build_adjacency(hexes: &[H3Hex], cell_map: &HashMap<u64, usize>) -> Adj {
    hexes.iter().enumerate().map(|(i, h)| {
        let Ok(cell) = CellIndex::try_from(h.index) else { return vec![]; };
        cell.grid_disk::<Vec<_>>(1).iter()
            .filter_map(|nbr| cell_map.get(&u64::from(*nbr)).copied())
            .filter(|&j| j != i)
            .collect()
    }).collect()
}

fn district_loads(hexes: &[H3Hex], assigns: &[usize], p: usize) -> Vec<f64> {
    let mut loads = vec![0.0f64; p];
    for (i, h) in hexes.iter().enumerate() {
        loads[assigns[i]] += h.spawn_rate;
    }
    loads
}

/// Returns true if district `district` is still connected after removing `excluded`.
fn bfs_connected_without(
    assigns:  &[usize],
    adj:      &Adj,
    start:    usize,
    excluded: usize,
    district: usize,
) -> bool {
    if start == excluded { return false; }

    let total = assigns.iter().filter(|&&d| d == district).count();
    let mut reachable: HashSet<usize> = HashSet::new();
    let mut queue:     VecDeque<usize> = VecDeque::new();
    reachable.insert(start);
    queue.push_back(start);

    while let Some(i) = queue.pop_front() {
        for &j in &adj[i] {
            if j != excluded && assigns[j] == district && reachable.insert(j) {
                queue.push_back(j);
            }
        }
    }

    reachable.len() == total - 1
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::optimizer::{Constraints, ObjectiveWeights, Problem, Solver};

    fn make_hex(index: u64, lat: f64, lon: f64, spawn_rate: f64) -> H3Hex {
        H3Hex { index, lat, lon, spawn_rate, profile_id: "residential".into(), nearest_osm_node: 0 }
    }

    fn unconstrained_problem(hexes: Vec<H3Hex>, p: usize) -> Problem {
        Problem {
            hexes,
            candidate_stations: vec![],
            n_districts: p,
            constraints: Constraints { contiguity: false, max_workload_ratio: None },
            objective:   ObjectiveWeights { travel_time: 1.0, workload_balance: 0.0 },
            distance_matrix: None,
            adjacency_override: None,
        }
    }

    #[test]
    fn greedy_two_clusters_two_stations() {
        // Two clearly separated clusters of 4 hexes each → p=2 stations should
        // place one station per cluster and objective should be low.
        let hexes = vec![
            make_hex(1, 53.50, 9.90, 1.0),
            make_hex(2, 53.51, 9.90, 1.0),
            make_hex(3, 53.50, 9.91, 1.0),
            make_hex(4, 53.51, 9.91, 1.0),
            make_hex(5, 53.60, 10.10, 1.0),
            make_hex(6, 53.61, 10.10, 1.0),
            make_hex(7, 53.60, 10.11, 1.0),
            make_hex(8, 53.61, 10.11, 1.0),
        ];
        let problem  = unconstrained_problem(hexes, 2);
        let solution = GreedySolver.solve(&problem).unwrap();

        // Each cluster should get its own district.
        let d0 = solution.assignments[0];
        assert!(solution.assignments[1..4].iter().all(|&d| d == d0),
            "first cluster should be in one district");
        let d1 = solution.assignments[4];
        assert!(solution.assignments[5..].iter().all(|&d| d == d1),
            "second cluster should be in one district");
        assert_ne!(d0, d1, "clusters should be in different districts");
    }

    #[test]
    fn more_stations_lower_or_equal_objective() {
        let hexes: Vec<H3Hex> = (0..9).map(|i| {
            make_hex(i as u64 + 1, 53.50 + (i / 3) as f64 * 0.01, 9.90 + (i % 3) as f64 * 0.01, 1.0)
        }).collect();

        let p2 = GreedySolver.solve(&unconstrained_problem(hexes.clone(), 2)).unwrap();
        let p3 = GreedySolver.solve(&unconstrained_problem(hexes,         3)).unwrap();

        assert!(
            p3.objective <= p2.objective + 1e-6,
            "p=3 objective ({}) should be ≤ p=2 objective ({})",
            p3.objective, p2.objective
        );
    }
}
