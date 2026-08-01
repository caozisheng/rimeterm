//! Root of the data-only `rimeterm-zones` crate.
//!
//! Powers the `ZonesPane` in `rimeterm-tui`. Kept ratatui-free so `rimectl`
//! (or any other host) can reuse the same [`ZoneHandle`] / [`ZoneList`] +
//! coastline / coord tables without dragging a TUI stack in.

pub mod canvas;
pub mod coastline;
pub mod coords;
pub mod error;
pub mod handle;
pub mod locations;
pub mod projection;
pub mod solar;
pub mod watchlist;

pub use canvas::BrailleCanvas;
pub use coastline::COASTLINE;
pub use coords::ZONE_COORDS;
pub use error::ZonesError;
pub use handle::{
    TimezoneError, ZoneHandle, all_timezones, format_utc_offset, format_utc_offset_short,
    parse_zone,
};
pub use locations::{Placement, ZoneLocation, locate};
pub use projection::{
    LAT_MAX, clamp_lat, lat_to_norm, lon_to_norm, norm_to_lat, norm_to_lon, wrap_lon,
};
pub use solar::{SunPosition, is_night, subsolar, zenith_cos};
pub use watchlist::{ZoneEntry, ZoneList};
