// Library crate root — all modules declared here so both the sim binary
// (src/main.rs) and the optimizer binary (src/bin/optimize.rs) can share them.

pub mod city;
pub mod clock;
pub mod config;
pub mod district;
pub mod event_log;
pub mod event_queue;
pub mod geo_utils;
pub mod hex;
pub mod incident;
pub mod optimizer;
pub mod osm;
pub mod report;
pub mod routing;
pub mod spawner;
pub mod station;
pub mod types;
pub mod unit;
