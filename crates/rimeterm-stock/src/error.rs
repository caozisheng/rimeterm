use std::io;
use std::path::PathBuf;

/// Errors produced by stock data, symbol validation, or watchlist persistence.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid {market} symbol `{symbol}`: {reason}")]
    InvalidSymbol {
        market: &'static str,
        symbol: String,
        reason: &'static str,
    },

    #[error("akshare request failed: {0}")]
    Akshare(#[from] akshare::Error),

    #[error("failed to read watchlist `{path}`: {source}")]
    ReadWatchlist { path: PathBuf, source: io::Error },

    #[error("failed to parse watchlist `{path}`: {source}")]
    ParseWatchlist {
        path: PathBuf,
        source: toml::de::Error,
    },

    #[error("failed to serialize watchlist: {0}")]
    SerializeWatchlist(#[from] toml::ser::Error),

    #[error("failed to persist watchlist `{path}`: {source}")]
    WriteWatchlist { path: PathBuf, source: io::Error },

    #[error("no quote data returned for {market} symbol `{symbol}`")]
    MissingQuote {
        market: &'static str,
        symbol: String,
    },

    #[error("no candle data returned for {market} symbol `{symbol}`")]
    MissingCandles {
        market: &'static str,
        symbol: String,
    },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
