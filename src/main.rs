mod types;
mod clock;
mod event_log;
mod city;
mod district;
mod unit;
mod incident;
mod config;
mod spawner;
mod routing;
mod hex;
mod station;

use crate::city::City;
use crate::config::CityConfig;
use crate::routing::build_travel_matrix;

fn main() {
    let toml_str = std::fs::read_to_string("config/city.toml").unwrap();
    let config: CityConfig = toml::from_str(&toml_str).unwrap();

    let travel_matrix = build_travel_matrix(
        &config.districts,
        config.city.hex_radius,
    );

    let mut city = City::from_config(config, travel_matrix);
    city.run(100);
}