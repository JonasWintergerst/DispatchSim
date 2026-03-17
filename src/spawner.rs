use crate::clock::{SimTime, TimeContext, SimClock};
use crate::types::{IncidentId, IncidentKind, NodeId, Priority, SpawnProfileId, UnitRequirements, DistrictId};
use crate::incident::Incident;

use rand::Rng;
use rand::rngs::SmallRng;
use rand::SeedableRng;
use rand_distr::{Distribution, Poisson};


pub struct SpawnProfile {
    //id: SpawnProfileId,
    pub base_lambda: f64,
    pub hour_multiplier: [f64; 24],
    pub weekday_multiplier: [f64; 7],
    pub season_multiplier: [f64; 4],
    pub incident_weights: Vec<(IncidentKind, f64)>,
}

impl SpawnProfile {
    /// Compute the effective λ (incidents per minute) at the given clock state.
    /// base_lambda is per-hour, so we divide by 60 to get per-minute for Poisson draw.
    pub fn lambda_at(&self, time_context: &TimeContext) -> f64 {

        let season_idx = time_context.season as usize;

        let effective_lambda = self.base_lambda
            * self.hour_multiplier[time_context.hour as usize]
            * self.weekday_multiplier[time_context.day as usize]
            * self.season_multiplier[season_idx];

        // Convert from per-hour → per-minute for the tick Poisson draw
        effective_lambda / 60.0
    }

    /// Draw from Poisson(λ) to get incident count, then build each Incident.
    pub fn spawn(&self, time_context: &TimeContext, node_id: NodeId, district_id: DistrictId) -> Vec<Incident> {
        let mut rng = SmallRng::seed_from_u64(1);
        let lambda = self.lambda_at(time_context);

        // Guard: Poisson requires λ > 0
        if lambda <= 0.0 {
            return vec![];
        }

        let poisson = Poisson::new(lambda).expect("lambda must be > 0");
        let count: u64 = poisson.sample(&mut rng) as u64;

        
        (0..count)
        .map(|i| {
                let s = format!(
                    "{}-{}-{}-{}",
                    district_id.value(),
                    node_id.value(),
                    time_context.current_time.as_minutes(),
                    i
                );
                Incident::new(
                    IncidentId::new(s.clone()),
                    Priority::A,
                    node_id,
                    district_id,
                    UnitRequirements { 0 : 1 },
                    time_context.current_time,
                )
            })
            .collect()
    }

    pub fn new(
        base_lambda: f64,
        hour_multiplier: [f64; 24],
        weekday_multiplier: [f64; 7],
        season_multiplier: [f64; 4],
        incident_weights: Vec<(IncidentKind, f64)>,
    ) -> Self {
        SpawnProfile {
            base_lambda,
            hour_multiplier,
            weekday_multiplier,
            season_multiplier,
            incident_weights,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spawn() {
     
        let profile = test_spawn_profile_with_lambda(30.0);
        let district_id = DistrictId::new(1);
        let time_context = &TimeContext { hour: 12, day: 5, season: crate::types::Season::Autumn, current_time: SimTime(100) };
        
        let mut incidents: Vec<Incident> = Vec::new();
        
        
        let node_id = NodeId::new(1);
        incidents.extend(profile.spawn(time_context, node_id, district_id));
        
        assert!(!incidents.is_empty());
    }

    #[test]
    fn spawn_returns_empty_when_lambda_zero() {
        let profile = test_spawn_profile_with_lambda(0.0);
        let district_id = DistrictId::new(1);
        let node_id = NodeId::new(1);
        let time_context = &TimeContext { hour: 12, day: 5, season: crate::types::Season::Autumn, current_time: SimTime(100) };

        let incidents = profile.spawn(time_context, node_id, district_id);

        assert!(incidents.is_empty());
    }

    #[test]
    fn lambda_at_test() {
        let profile = test_spawn_profile_with_lambda(0.05);
        let time_context = &TimeContext { hour: 20, day: 6, season: crate::types::Season::Spring, current_time: SimTime(100) };
        let lambda = profile.lambda_at(time_context);

        assert!(lambda > 0.001);
    }

    fn test_spawn_profile_with_lambda(lambda: f64) -> SpawnProfile {
        //residential
        let hour_multiplier    = [0.4, 0.3, 0.3, 0.3, 0.4, 0.6, 0.8, 1.0, 1.0, 0.9, 0.9, 0.9, 0.9, 0.9, 0.9, 1.0, 1.1, 1.2, 1.3, 1.3, 1.2, 1.0, 0.8, 0.5];
        let weekday_multiplier = [1.0, 1.0, 1.0, 1.0, 1.0, 1.3, 1.3];
        let season_multiplier  = [1.0, 1.1, 1.0, 0.9];

        let test_profile = SpawnProfile::new(
            lambda,                     
            hour_multiplier,
            weekday_multiplier,
            season_multiplier,              
            vec![(IncidentKind::Crime, 1.0)], 
        );

        return test_profile
    }
}