//! Async akshare bridge hosted on one background OS thread.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use rimeterm_stock::StockClient;

use crate::stock_model::{StockRequest, StockResponse};

pub struct StockWorker {
    request_tx: Sender<StockRequest>,
    response_rx: Receiver<StockResponse>,
}

impl StockWorker {
    pub fn spawn(proxy: Option<String>, tushare_token: Option<String>) -> Self {
        let (request_tx, request_rx) = mpsc::channel();
        let (response_tx, response_rx) = mpsc::channel();
        thread::Builder::new()
            .name("rimeterm-stock-worker".into())
            .spawn(move || run(request_rx, response_tx, proxy, tushare_token))
            .expect("spawn stock worker");
        Self {
            request_tx,
            response_rx,
        }
    }

    pub fn send(&self, request: StockRequest) {
        let _ = self.request_tx.send(request);
    }

    pub fn drain(&self) -> Vec<StockResponse> {
        let mut responses = Vec::new();
        while let Ok(response) = self.response_rx.try_recv() {
            responses.push(response);
        }
        responses
    }
}

fn run(
    request_rx: Receiver<StockRequest>,
    response_tx: Sender<StockResponse>,
    proxy: Option<String>,
    tushare_token: Option<String>,
) {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    let client = StockClient::new(proxy.as_deref(), tushare_token.as_deref());

    while let Ok(request) = request_rx.recv() {
        let response = match request {
            StockRequest::Refresh {
                generation,
                market,
                watchlist,
            } => StockResponse::Refresh {
                generation,
                result: runtime
                    .block_on(client.refresh(&watchlist, market))
                    .map_err(|error| error.to_string()),
            },
            StockRequest::Search {
                generation,
                market,
                query,
                limit,
            } => StockResponse::Search {
                generation,
                result: runtime
                    .block_on(client.search(market, &query, limit))
                    .map_err(|error| error.to_string()),
            },
            StockRequest::Detail { generation, entry } => StockResponse::Detail {
                generation,
                result: runtime
                    .block_on(client.detail(&entry))
                    .map_err(|error| error.to_string()),
            },
            StockRequest::LiveDetail { generation, entry } => StockResponse::LiveDetail {
                generation,
                result: runtime
                    .block_on(client.live_detail(&entry))
                    .map_err(|error| error.to_string()),
            },
        };
        if response_tx.send(response).is_err() {
            return;
        }
    }
}
