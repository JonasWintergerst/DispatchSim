use rayon::iter::IntoParallelRefIterator;

use crate::types::{SpawnProfileId, SimeType};
use crate::district::District;
use crate::event_log::{EventLog, Event};
use crate::clock::{SimClock, SimTime, TimeContext};
use crate::spawner::SpawnProfile;
use crate::config::CityConfig;

use std::collections::HashMap;
use rayon::iter::IntoParallelRefMutIterator;
use rayon::iter::ParallelIterator;

pub struct City {
    clock: SimClock,
    districts: Vec<District>,
    coordinator: CityCoordinator,
    event_log: EventLog,
    profiles: HashMap<SpawnProfileId, SpawnProfile>,
    sim_type: SimeType,
}

pub struct CityCoordinator;

impl City {
    pub fn tick(&mut self) {
        println!("{}", self.clock.elapsed_min);

        let profiles = &self.profiles;

        let time_context = TimeContext {
            hour: self.clock.hour_of_day(),
            day: self.clock.day_of_week(),
            season: self.clock.season(),
            current_time: SimTime { 0 : self.clock.elapsed_min },
        };
    
        let events: Vec<Event> = self.districts
            .par_iter_mut()
            .flat_map(|d| d.tick(time_context, profiles))  
            .collect();

        EventLog::print_events(events);
        self.clock.tick();
    }

    pub fn run(&mut self, ticks: u64) {
        self.clock.start_sim();
        loop {
            self.tick();
            if self.clock.elapsed_min >= ticks {
                break;
            }
        }
    }

    pub fn from_config(config: CityConfig, travel_matrix: Vec<Vec<f64>>) -> Self {
        let districts = config.districts
            .into_iter()
            .map(|d| District::from_config(&d, config.city.hex_radius))
            .collect();

        let id = SpawnProfileId::new();
        let mut profiles = HashMap::new();
        profiles.insert(id, SpawnProfile::urban(id));

        City {
            clock: SimClock::new(),
            districts,
            coordinator: CityCoordinator::new(),
            event_log: EventLog::new(),
            profiles: profiles,
            sim_type: config.city.sim_type,
        }
    }
}

impl CityCoordinator {
    pub fn new() -> Self { return CityCoordinator {}}
}