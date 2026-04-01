// Shared geographic utilities used by both the simulator (osm.rs) and the
// optimizer (optimizer/greedy.rs, optimizer/h3_grid.rs).

/// Haversine distance in metres between two (lat, lon) points in degrees.
pub fn haversine_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const R: f64 = 6_371_000.0;
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());
    R * c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn haversine_same_point_is_zero() {
        assert_eq!(haversine_m(53.55, 9.99, 53.55, 9.99), 0.0);
    }

    #[test]
    fn haversine_known_distance() {
        // Hamburg city hall → 1° north ≈ 111_320 m
        let d = haversine_m(53.55, 9.99, 54.55, 9.99);
        assert!((d - 111_320.0).abs() < 500.0, "expected ~111km, got {d}");
    }
}
