use std::collections::HashMap;

use rand::Rng;
use rand_distr::{Distribution, Exp};

use crate::clock::SimTime;
use crate::types::{IncidentKind, SpawnProfileId};

pub struct SpawnProfile {
    pub base_lambda: f64,
    pub hour_multiplier: [f64; 24],
    pub weekday_multiplier: [f64; 7],
    pub season_multiplier: [f64; 4],
    pub incident_weights: Vec<(IncidentKind, f64)>,
}

impl SpawnProfile {
    pub fn new(
        base_lambda: f64,
        hour_multiplier: [f64; 24],
        weekday_multiplier: [f64; 7],
        season_multiplier: [f64; 4],
        incident_weights: Vec<(IncidentKind, f64)>,
    ) -> Self {
        SpawnProfile { base_lambda, hour_multiplier, weekday_multiplier, season_multiplier, incident_weights }
    }
}

/// Sample the time of the next incident spawn for a given spawn profile.
///
/// Uses an Exponential inter-arrival process whose rate (λ) is recomputed at
/// each hour boundary, so that time-of-day multipliers are applied correctly
/// even when the sampled gap straddles midnight or another hour change.
pub fn next_spawn_time(
    from: SimTime,
    profile_id: &SpawnProfileId,
    profiles: &HashMap<SpawnProfileId, SpawnProfile>,
    rng: &mut impl Rng,
) -> SimTime {
    let profile   = &profiles[profile_id];
    let lambda    = effective_lambda(from, profile);
    let candidate = from.0 + sample_inter_arrival(lambda, rng);
    let boundary  = next_hour_boundary(from);

    if candidate < boundary.0 {
        SimTime(candidate)
    } else {
        // Recalculate with the next hour's λ after crossing the boundary.
        next_spawn_time(boundary, profile_id, profiles, rng)
    }
}

/// Sample an inter-arrival duration in whole minutes from Exp(λ).
/// `lambda_per_hour` is converted to per-minute before sampling.
pub fn sample_inter_arrival(lambda_per_hour: f64, rng: &mut impl Rng) -> u64 {
    let lambda_per_min = lambda_per_hour / 60.0;
    let exp     = Exp::new(lambda_per_min).expect("lambda must be > 0");
    let minutes = exp.sample(rng);
    minutes.round().max(1.0) as u64
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn effective_lambda(time: SimTime, profile: &SpawnProfile) -> f64 {
    let hour   = (time.0 / 60 % 24) as usize;
    let day    = (time.0 / 1440 % 7) as usize;
    let season = (time.0 / 1440 % 365 / 91).min(3) as usize;

    profile.base_lambda
        * profile.hour_multiplier[hour]
        * profile.weekday_multiplier[day]
        * profile.season_multiplier[season]
}

fn next_hour_boundary(from: SimTime) -> SimTime {
    let mins_into_hour = from.0 % 60;
    SimTime(from.0 + (60 - mins_into_hour))
}
