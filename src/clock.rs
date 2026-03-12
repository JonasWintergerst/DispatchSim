use serde::{Serialize, Deserialize};


use crate::types::Season;

#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct SimClock {
    pub elapsed_min: u64,
}

impl SimClock {
    pub fn hour_of_day(&self) -> u32 { (self.elapsed_min / 60 % 24) as u32 }
    pub fn day_of_week(&self) -> u32 { (self.elapsed_min / 1440 % 7) as u32 }
    pub fn is_night(&self) -> bool { let h = self.hour_of_day(); h >= 22 || h < 6 }
    pub fn season(&self) -> Season {
        let day_of_year = (self.elapsed_min / 1440 % 365) as u32;
        match day_of_year {
            0..=89   => Season::Winter,
            90..=180 => Season::Spring,
            181..=273 => Season::Summer,
            _         => Season::Autumn,
        }
    }

    pub fn tick(&mut self) { self.elapsed_min += 1 }
    pub fn start_sim(&mut self) { self.elapsed_min = 0}
    pub fn new() -> Self { SimClock { elapsed_min: 0 }}
}

#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct SimTime(pub u64);

impl SimTime {
    pub fn as_minutes(&self) -> u64 { self.0 }

    pub fn duration_since(&self, earlier: SimTime) -> u64 {
        self.0.saturating_sub(earlier.0)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct TimeContext {
    pub hour: u32,
    pub day: u32,
    pub season: Season,
    pub current_time: SimTime,
}