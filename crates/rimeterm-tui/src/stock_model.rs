//! Messages exchanged between [`crate::stock_pane::StockPane`] and its worker.

use rimeterm_stock::{DetailBundle, LiveDetail, Market, SearchResult, Snapshot, WatchEntry};

#[derive(Clone, Debug)]
pub enum StockRequest {
    Refresh {
        generation: u64,
        market: Market,
        watchlist: Vec<WatchEntry>,
    },
    Search {
        generation: u64,
        market: Market,
        query: String,
        limit: usize,
    },
    Detail {
        generation: u64,
        entry: WatchEntry,
    },
    LiveDetail {
        generation: u64,
        entry: WatchEntry,
    },
}

#[derive(Clone, Debug)]
pub enum StockResponse {
    Refresh {
        generation: u64,
        result: Result<Snapshot, String>,
    },
    Search {
        generation: u64,
        result: Result<Vec<SearchResult>, String>,
    },
    Detail {
        generation: u64,
        result: Result<DetailBundle, String>,
    },
    LiveDetail {
        generation: u64,
        result: Result<LiveDetail, String>,
    },
}
