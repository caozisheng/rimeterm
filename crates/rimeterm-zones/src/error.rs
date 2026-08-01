//! Error type surfaced by the data-crate layer.

use thiserror::Error;

use crate::handle::TimezoneError;

#[derive(Debug, Error)]
pub enum ZonesError {
    #[error("timezone: {0}")]
    Timezone(#[from] TimezoneError),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("toml decode: {0}")]
    TomlDecode(#[from] toml::de::Error),

    #[error("toml encode: {0}")]
    TomlEncode(#[from] toml::ser::Error),
}
