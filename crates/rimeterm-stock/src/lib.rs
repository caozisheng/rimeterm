//! Stock market data types, persistence, market sessions, and akshare access.

mod client;
mod error;
mod format;
mod market;
mod model;
mod watchlist;

pub use client::StockClient;
pub use error::{Error, Result};
pub use format::{format_change_pct, format_compact, format_price};
pub use market::Market;
pub use model::{
    Candle, DetailBundle, Fundamentals, IndexRow, IntradayPoint, LiveDetail, NewsItem, QuoteRow,
    SearchResult, Snapshot,
};
pub use watchlist::{WatchEntry, WatchList};
