use crate::clock::{SimTime, TimeContext, SimClock};
use crate::types::{IncidentId, IncidentKind, NodeId, Priority, SpawnProfileId, UnitRequirements, DistrictId};
use crate::incident::Incident;

use rand::Rng;
use rand::rngs::SmallRng;
use rand::SeedableRng;
use rand_distr::{Distribution, Poisson};


pub struct SpawnProfile {
    id: SpawnProfileId,
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
            .map(|_| {
                Incident::new(
                    IncidentId { 0 : 0 },
                    Priority::A,
                    node_id,
                    district_id,
                    UnitRequirements { 0 : 1 },
                    time_context.current_time,
                )
            })
            .collect()
    }

    pub fn urban(id: SpawnProfileId) -> Self {
        Self {
            id,
            // ~3 incidents/hour baseline for a busy urban district
            base_lambda: 3.0,

            // 24h multipliers — low at night, morning spike, big evening peak
            hour_multiplier: [
                0.4, 0.3, 0.3, 0.3, 0.4, 0.6,  // 00–05 late night / early morning
                0.8, 1.0, 1.2, 1.2, 1.1, 1.1,  // 06–11 morning ramp
                1.2, 1.2, 1.1, 1.1, 1.3, 1.5,  // 12–17 afternoon
                1.8, 2.0, 1.8, 1.5, 1.0, 0.6,  // 18–23 evening peak
            ],

            // Mon–Sun: weekdays steady, weekend nights spike
            weekday_multiplier: [
                1.0,  // Mon
                1.0,  // Tue
                1.0,  // Wed
                1.1,  // Thu — start of weekend creep
                1.3,  // Fri
                1.5,  // Sat
                1.2,  // Sun
            ],

            // Spring / Summer / Autumn / Winter
            season_multiplier: [
                1.1,  // Spring — more outdoor activity
                1.3,  // Summer — peak (heat, tourism, events)
                1.0,  // Autumn — baseline
                0.8,  // Winter — people stay indoors, fewer incidents
            ],

            incident_weights: vec![
                (IncidentKind::MedicalEmergency, 0.40),  // always the majority
                (IncidentKind::Crime,            0.25),
                (IncidentKind::Accident,         0.20),
                (IncidentKind::Fire,             0.10),
            ],
        }
    }
}