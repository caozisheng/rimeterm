//! Async akshare bridge hosted on one background OS thread.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use rimeterm_stock::{Market, StockClient};

use crate::stock_model::{StockRequest, StockResponse};

pub struct StockWorker {
    request_txs: [Sender<StockRequest>; 3],
    response_rx: Receiver<StockResponse>,
}

impl StockWorker {
    pub fn spawn(proxy: Option<String>, tushare_token: Option<String>) -> Self {
        let (response_tx, response_rx) = mpsc::channel();
        let (request_txs, jobs): (Vec<_>, Vec<_>) = [Market::AShare, Market::HongKong, Market::Us]
            .into_iter()
            .map(|market| {
                let (request_tx, request_rx) = mpsc::channel();
                (
                    request_tx,
                    (
                        market,
                        request_rx,
                        response_tx.clone(),
                        proxy.clone(),
                        tushare_token.clone(),
                    ),
                )
            })
            .unzip();
        thread::Builder::new()
            .name("rimeterm-stock-workers".into())
            .spawn(move || {
                run_concurrent(jobs, |(_, request_rx, response_tx, proxy, token)| {
                    run(request_rx, response_tx, proxy, token);
                });
            })
            .expect("spawn stock workers");
        Self {
            request_txs: request_txs
                .try_into()
                .unwrap_or_else(|_| unreachable!("three stock markets")),
            response_rx,
        }
    }

    pub fn send(&self, request: StockRequest) {
        let market = request_market(&request);
        let _ = self.request_txs[market_index(market)].send(request);
    }

    pub fn drain(&self) -> Vec<StockResponse> {
        let mut responses = Vec::new();
        while let Ok(response) = self.response_rx.try_recv() {
            responses.push(response);
        }
        responses
    }
}

fn request_market(request: &StockRequest) -> Market {
    match request {
        StockRequest::Refresh { market, .. } | StockRequest::Search { market, .. } => *market,
        StockRequest::Detail { entry, .. } | StockRequest::LiveDetail { entry, .. } => entry.market,
    }
}

const fn market_index(market: Market) -> usize {
    match market {
        Market::AShare => 0,
        Market::HongKong => 1,
        Market::Us => 2,
    }
}

fn run_concurrent<I, F>(jobs: I, run_job: F)
where
    I: IntoIterator,
    I::Item: Send,
    F: Fn(I::Item) + Sync,
{
    thread::scope(|scope| {
        for job in jobs {
            let run_job = &run_job;
            scope.spawn(move || run_job(job));
        }
    });
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use parking_lot::Mutex;

    use super::run_concurrent;

    #[test]
    fn run_concurrent_overlaps_independent_jobs() {
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let completed = Arc::new(Mutex::new(Vec::new()));
        run_concurrent(0..3, {
            let active = Arc::clone(&active);
            let peak = Arc::clone(&peak);
            let completed = Arc::clone(&completed);
            move |job| {
                let concurrent = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(concurrent, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(25));
                active.fetch_sub(1, Ordering::SeqCst);
                completed.lock().push(job);
            }
        });

        assert_eq!(
            (peak.load(Ordering::SeqCst), completed.lock().len()),
            (3, 3)
        );
    }
}
