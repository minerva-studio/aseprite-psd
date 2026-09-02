use std::collections::{HashMap, HashSet};
use std::hash::Hash;

const MATCH_ASSIGNMENT_BONUS: i32 = 100;

#[derive(Clone, Copy)]
struct FlowEdge {
    to: usize,
    reverse: usize,
    capacity: u8,
    cost: i32,
}

/// Adds one residual edge pair to a deterministic min-cost flow graph.
fn add_edge(graph: &mut [Vec<FlowEdge>], from: usize, to: usize, capacity: u8, cost: i32) {
    let forward_reverse = graph[to].len();
    let backward_reverse = graph[from].len();
    graph[from].push(FlowEdge {
        to,
        reverse: forward_reverse,
        capacity,
        cost,
    });
    graph[to].push(FlowEdge {
        to: from,
        reverse: backward_reverse,
        capacity: 0,
        cost: -cost,
    });
}

/// Solves one maximum-weight assignment, optionally forbidding a selected edge.
fn solve_matching<ObservationKey, TrackKey, OccupiedKey, Occupancy>(
    observations: &[ObservationKey],
    candidates: &HashMap<ObservationKey, Vec<(TrackKey, u16)>>,
    occupancy: &Occupancy,
    forbidden: Option<(ObservationKey, TrackKey)>,
) -> (i32, Vec<(ObservationKey, TrackKey)>)
where
    ObservationKey: Copy + Eq + Hash,
    TrackKey: Copy + Eq + Hash,
    OccupiedKey: Copy + Eq + Hash,
    Occupancy: Fn(ObservationKey, TrackKey) -> OccupiedKey,
{
    let mut occupied_indices = HashMap::new();
    for observation in observations {
        if let Some(edges) = candidates.get(observation) {
            for (track, _) in edges {
                let occupied = occupancy(*observation, *track);
                let next = occupied_indices.len();
                occupied_indices.entry(occupied).or_insert(next);
            }
        }
    }

    let source = 0;
    let observation_start = 1;
    let occupied_start = observation_start + observations.len();
    let sink = occupied_start + occupied_indices.len();
    let mut graph = vec![Vec::new(); sink + 1];
    for (observation_index, observation) in observations.iter().enumerate() {
        let node = observation_start + observation_index;
        add_edge(&mut graph, source, node, 1, 0);
        add_edge(&mut graph, node, sink, 1, 0);
        if let Some(edges) = candidates.get(observation) {
            for (track, score) in edges {
                if forbidden == Some((*observation, *track)) {
                    continue;
                }
                let occupied = occupancy(*observation, *track);
                let occupied_node = occupied_start + occupied_indices[&occupied];
                add_edge(
                    &mut graph,
                    node,
                    occupied_node,
                    1,
                    -(MATCH_ASSIGNMENT_BONUS + i32::from(*score)),
                );
            }
        }
    }
    for index in 0..occupied_indices.len() {
        add_edge(&mut graph, occupied_start + index, sink, 1, 0);
    }

    let mut total_cost = 0;
    for _ in observations {
        let mut distance = vec![i32::MAX; graph.len()];
        let mut previous = vec![None; graph.len()];
        distance[source] = 0;
        for _ in 1..graph.len() {
            let mut changed = false;
            for from in 0..graph.len() {
                if distance[from] == i32::MAX {
                    continue;
                }
                for (edge_index, edge) in graph[from].iter().enumerate() {
                    if edge.capacity == 0 {
                        continue;
                    }
                    let candidate = distance[from] + edge.cost;
                    if candidate < distance[edge.to] {
                        distance[edge.to] = candidate;
                        previous[edge.to] = Some((from, edge_index));
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        if distance[sink] == i32::MAX {
            break;
        }
        total_cost += distance[sink];
        let mut node = sink;
        let mut path_length = 0;
        while node != source {
            path_length += 1;
            assert!(
                path_length <= graph.len(),
                "matching residual predecessor path contains a cycle"
            );
            let (from, edge_index) = previous[node].expect("flow path must reach the source");
            let reverse = graph[from][edge_index].reverse;
            graph[from][edge_index].capacity -= 1;
            graph[node][reverse].capacity += 1;
            node = from;
        }
    }

    let mut assignments = Vec::new();
    for (observation_index, observation) in observations.iter().enumerate() {
        let node = observation_start + observation_index;
        if let Some(edges) = candidates.get(observation) {
            for (track, _) in edges {
                if forbidden == Some((*observation, *track)) {
                    continue;
                }
                let occupied = occupancy(*observation, *track);
                let occupied_node = occupied_start + occupied_indices[&occupied];
                if graph[node]
                    .iter()
                    .any(|edge| edge.to == occupied_node && edge.capacity == 0 && edge.cost < 0)
                {
                    assignments.push((*observation, *track));
                    break;
                }
            }
        }
    }
    (-total_cost, assignments)
}

/// Finds the highest-scoring deterministic assignment and safely rejects tied edges.
pub(super) fn find_best_weighted_matching<ObservationKey, TrackKey, OccupiedKey>(
    observations: &[ObservationKey],
    candidates: &HashMap<ObservationKey, Vec<(TrackKey, u16)>>,
    occupancy: impl Fn(ObservationKey, TrackKey) -> OccupiedKey,
) -> (Vec<(ObservationKey, TrackKey)>, HashSet<ObservationKey>)
where
    ObservationKey: Copy + Eq + Hash,
    TrackKey: Copy + Eq + Hash,
    OccupiedKey: Copy + Eq + Hash,
{
    let (best_score, assignments) = solve_matching(observations, candidates, &occupancy, None);
    let mut tied_observations = HashSet::new();
    for (observation, track) in &assignments {
        let (alternative_score, _) = solve_matching(
            observations,
            candidates,
            &occupancy,
            Some((*observation, *track)),
        );
        if alternative_score == best_score {
            tied_observations.insert(*observation);
        }
    }
    (assignments, tied_observations)
}
