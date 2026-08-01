// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2025 Tom Larcher
// Ported from zonetimeline-tui @ v0.4.0
// (https://github.com/findyourexit/zonetimeline-tui).
// Projection swapped from Web-Mercator to Equirectangular by rimeterm
// (a purely linear lat→y map naturally gives the 2:1 landscape aspect
// that reads as a "world map"; Mercator's 1:1 aspect showed as a
// visual square in braille cells and looked wrong).

//! Equirectangular (plate carrée) projection helpers for the world map.
//!
//! A simple linear map: longitude/latitude in degrees → normalized
//! `[0, 1]` coordinates where x runs west→east and y runs north→south.
//! The natural aspect is **2:1** (360° wide × 180° tall), so a world
//! map in this projection reads as the familiar landscape rectangle.
//! No polar divergence — the poles sit at `y = 0` (north) and `y = 1`
//! (south) with no tan/ln explosion.

/// Latitude boundary in degrees. Equirectangular has no polar
/// divergence, so this is just the geographic edge of the globe.
pub const LAT_MAX: f64 = 90.0;

/// Clamp a latitude to the valid `[-90, 90]` range.
pub fn clamp_lat(lat: f64) -> f64 {
    lat.clamp(-LAT_MAX, LAT_MAX)
}

/// Normalize any longitude into the `[-180, 180)` half-open range.
pub fn wrap_lon(lon: f64) -> f64 {
    let wrapped = (lon + 180.0).rem_euclid(360.0) - 180.0;
    // f64 rem_euclid can round up to exactly 360.0 for tiny-negative inputs,
    // yielding wrapped == 180.0; fold that back into the half-open range.
    if wrapped >= 180.0 {
        wrapped - 360.0
    } else {
        wrapped
    }
}

/// Project a longitude to normalized x in `[0, 1)` (0 = 180°W; approaches 1
/// toward 180°E, which folds back to 0 at the antimeridian seam).
pub fn lon_to_norm(lon: f64) -> f64 {
    (wrap_lon(lon) + 180.0) / 360.0
}

/// Project a latitude to normalized y in `[0, 1]` (0 = north pole, 1 = south).
pub fn lat_to_norm(lat: f64) -> f64 {
    0.5 - clamp_lat(lat) / 180.0
}

/// Inverse of [`lon_to_norm`]: normalized x → longitude in degrees.
pub fn norm_to_lon(nx: f64) -> f64 {
    nx * 360.0 - 180.0
}

/// Inverse of [`lat_to_norm`]: normalized y → latitude in degrees.
pub fn norm_to_lat(ny: f64) -> f64 {
    (0.5 - ny) * 180.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
    }

    #[test]
    fn longitude_maps_to_full_width() {
        assert!(close(lon_to_norm(-180.0), 0.0, 1e-9));
        assert!(close(lon_to_norm(0.0), 0.5, 1e-9));
        assert!(close(lon_to_norm(179.999), 1.0, 1e-3));
    }

    #[test]
    fn equator_is_vertical_center() {
        assert!(close(lat_to_norm(0.0), 0.5, 1e-9));
    }

    #[test]
    fn latitude_is_linear_in_equirectangular() {
        // 45°N sits a quarter of the way down from the top.
        assert!(close(lat_to_norm(45.0), 0.25, 1e-9));
        // 45°S sits three-quarters of the way down.
        assert!(close(lat_to_norm(-45.0), 0.75, 1e-9));
    }

    #[test]
    fn poles_pin_to_the_edges() {
        assert!(close(lat_to_norm(90.0), 0.0, 1e-9));
        assert!(close(lat_to_norm(-90.0), 1.0, 1e-9));
        // Beyond the limit the projection stays clamped, not NaN.
        assert!(close(lat_to_norm(100.0), 0.0, 1e-9));
        assert!(close(lat_to_norm(-100.0), 1.0, 1e-9));
    }

    #[test]
    fn round_trips_within_the_valid_range() {
        for lat in [-80.0, -45.0, -10.0, 0.0, 23.4, 51.5, 80.0] {
            assert!(close(norm_to_lat(lat_to_norm(lat)), lat, 1e-9));
        }
        for lon in [-179.0, -73.0, 0.0, 55.5, 139.7] {
            assert!(close(norm_to_lon(lon_to_norm(lon)), lon, 1e-9));
        }
    }

    #[test]
    fn longitude_wrapping_is_stable() {
        assert!(close(wrap_lon(190.0), -170.0, 1e-9));
        assert!(close(wrap_lon(-190.0), 170.0, 1e-9));
        assert!(wrap_lon(180.0) < 180.0);
        assert!(wrap_lon(360.0).abs() < 1e-9);
    }
}
