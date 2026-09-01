use std::collections::{HashMap, HashSet};
use std::hash::Hash;

const FAMILY_MATCH_ASSIGNMENT_BONUS: u32 = 100;
const MAX_FAMILY_MATCHING_STATES: usize = 100_000;

struct WeightedMatchingSearch<'a, ObservationKey, TrackKey, OccupiedKey, Occupancy> {
    observations: &'a [ObservationKey],
    candidates: &'a HashMap<ObservationKey, Vec<(TrackKey, u16)>>,
    occupancy: Occupancy,
    used: HashSet<OccupiedKey>,
    current: Vec<(ObservationKey, TrackKey)>,
    best_score: Option<u32>,
    solutions: Vec<Vec<(ObservationKey, TrackKey)>>,
    states: usize,
}

impl<ObservationKey, TrackKey, OccupiedKey, Occupancy>
    WeightedMatchingSearch<'_, ObservationKey, TrackKey, OccupiedKey, Occupancy>
where
    ObservationKey: Copy + Eq + Hash,
    TrackKey: Copy + Eq,
    OccupiedKey: Copy + Eq + Hash,
    Occupancy: Fn(ObservationKey, TrackKey) -> OccupiedKey,
{
    fn visit(&mut self, position: usize, score: u32) {
        if self.states >= MAX_FAMILY_MATCHING_STATES {
            return;
        }
        self.states += 1;
        if position == self.observations.len() {
            match self.best_score {
                Some(best) if score > best => {
                    self.best_score = Some(score);
                    self.solutions.clear();
                    self.solutions.push(self.current.clone());
                }
                Some(best)
                    if score == best
                        && self.solutions.len() < 2
                        && !self
                            .solutions
                            .iter()
                            .any(|solution| solution == &self.current) =>
                {
                    self.solutions.push(self.current.clone());
                }
                None => {
                    self.best_score = Some(score);
                    self.solutions.push(self.current.clone());
                }
                _ => {}
            }
            return;
        }

        let observation = self.observations[position];
        if let Some(edges) = self.candidates.get(&observation) {
            for &(track, edge_score) in edges {
                let occupied = (self.occupancy)(observation, track);
                if self.used.insert(occupied) {
                    self.current.push((observation, track));
                    self.visit(
                        position + 1,
                        score + FAMILY_MATCH_ASSIGNMENT_BONUS + u32::from(edge_score),
                    );
                    self.current.pop();
                    self.used.remove(&occupied);
                }
            }
        }
        self.visit(position + 1, score);
    }
}

/// Finds the highest-scoring deterministic assignment and its tied observations.
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
    let mut search = WeightedMatchingSearch {
        observations,
        candidates,
        occupancy,
        used: HashSet::new(),
        current: Vec::new(),
        best_score: None,
        solutions: Vec::new(),
        states: 0,
    };
    search.visit(0, 0);
    let assignments = search.solutions.first().cloned().unwrap_or_default();
    let mut tied_observations = HashSet::new();
    if search.solutions.len() > 1 {
        for observation in observations {
            let variants = search
                .solutions
                .iter()
                .map(|solution| {
                    solution
                        .iter()
                        .find(|(candidate, _)| candidate == observation)
                        .map(|(_, track)| *track)
                })
                .collect::<HashSet<_>>();
            if variants.len() > 1 {
                tied_observations.insert(*observation);
            }
        }
    }
    (assignments, tied_observations)
}
