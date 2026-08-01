use std::time::{SystemTime, UNIX_EPOCH};

use akshare::AkShareClient;
use chrono::Utc;
use futures::stream::{self, StreamExt};
use tracing::warn;

use crate::error::{Error, Result};
use crate::model::{
    Candle, DetailBundle, Fundamentals, IndexRow, IntradayPoint, LiveDetail, NewsItem, QuoteRow,
    SearchResult, Snapshot,
};
use crate::{Market, WatchEntry};

#[derive(Clone)]
pub struct StockClient {
    inner: AkShareClient,
}

impl StockClient {
    #[must_use]
    pub fn new(proxy: Option<&str>, tushare_token: Option<&str>) -> Self {
        let mut builder = AkShareClient::builder();
        if let Some(proxy) = proxy.filter(|value| !value.trim().is_empty()) {
            builder = builder.proxy(proxy);
        }
        if let Some(token) = tushare_token.filter(|value| !value.trim().is_empty()) {
            builder = builder.tushare_token(token);
        }
        Self {
            inner: builder.build(),
        }
    }

    pub async fn refresh(&self, entries: &[WatchEntry], market: Market) -> Result<Snapshot> {
        let rows = stream::iter(entries.iter().filter(|entry| entry.market == market))
            .map(|entry| async move {
                match self.quote(entry).await {
                    Ok(row) => row,
                    Err(error) => QuoteRow {
                        market: entry.market,
                        symbol: entry.symbol.clone(),
                        name: entry.name.clone(),
                        last: None,
                        change_pct: None,
                        change_amount: None,
                        open: None,
                        high: None,
                        low: None,
                        prev_close: None,
                        volume: None,
                        amount: None,
                        pe: None,
                        pb: None,
                        market_cap: None,
                        as_of: None,
                        error: Some(error.to_string()),
                    },
                }
            })
            .buffer_unordered(8)
            .collect()
            .await;
        let indices = match self.indices(market).await {
            Ok(indices) => indices,
            Err(error) => {
                warn!(market = %market, error = %error, "stock index refresh degraded");
                Vec::new()
            }
        };
        Ok(Snapshot {
            rows,
            indices,
            fetched_at_epoch_secs: epoch_seconds(),
        })
    }

    pub async fn search(
        &self,
        market: Market,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let results = match market {
            Market::AShare => {
                let mut results = self.inner.a_share_search(query, None, limit).await?;
                if results.len() < limit {
                    let spot = self.inner.stock_kc_a_spot().await?;
                    let fallback = search_spot_quotes(&spot, query, limit - results.len());
                    for item in fallback {
                        if !results.iter().any(|current| current.symbol == item.symbol) {
                            results.push(akshare::StockSearchResult {
                                symbol: item.symbol,
                                name: item.name,
                                market: "A股".into(),
                                exchange: item.exchange,
                            });
                        }
                    }
                }
                results
            }
            Market::HongKong => self.inner.hk_search(query, limit).await?,
            Market::Us => self.inner.us_search(query, limit).await?,
        };
        Ok(results
            .into_iter()
            .filter_map(|item| {
                market
                    .normalize_symbol(&item.symbol)
                    .ok()
                    .map(|symbol| SearchResult {
                        market,
                        symbol,
                        name: item.name,
                        exchange: item.exchange,
                    })
            })
            .take(limit)
            .collect())
    }

    pub async fn detail(&self, entry: &WatchEntry) -> Result<DetailBundle> {
        let (quote, candles) = futures::try_join!(self.quote(entry), self.candles(entry))?;
        let (intraday_result, fundamentals_result, news_result) = futures::join!(
            self.intraday(entry),
            self.fundamentals(entry),
            self.news(entry)
        );
        let mut intraday = degrade("intraday", entry, intraday_result, Vec::new());
        if intraday.is_empty() {
            intraday = candles
                .iter()
                .map(|candle| IntradayPoint {
                    time: candle.date.clone(),
                    price: candle.close,
                    volume: candle.volume,
                })
                .collect();
        }
        let fundamentals = degrade("fundamentals", entry, fundamentals_result, None);
        let news = degrade("news", entry, news_result, Vec::new());
        Ok(DetailBundle {
            quote,
            intraday,
            candles,
            fundamentals,
            news,
        })
    }

    pub async fn live_detail(&self, entry: &WatchEntry) -> Result<LiveDetail> {
        let (quote_result, intraday_result) =
            futures::join!(self.quote(entry), self.intraday(entry));
        let quote = quote_result?;
        let intraday = degrade("intraday", entry, intraday_result, Vec::new());
        Ok(LiveDetail { quote, intraday })
    }

    async fn quote(&self, entry: &WatchEntry) -> Result<QuoteRow> {
        let symbol = entry.market.normalize_symbol(&entry.symbol)?;
        let quote = match entry.market {
            Market::AShare => self.inner.a_share_quote(&symbol).await?,
            Market::HongKong => self.inner.hk_quote(&symbol).await?,
            Market::Us => self.inner.us_quote(&symbol).await?,
        };
        let previous = self.recent_candles(entry.market, &symbol, 2).await.ok();
        let prior_close = previous
            .as_ref()
            .and_then(|items| prior_close_from_history(items, quote.close));
        let change_amount = prior_close.map(|prior| quote.close - prior);
        let change_pct = prior_close
            .filter(|prior| *prior != 0.0)
            .map(|prior| (quote.close - prior) / prior * 100.0);
        Ok(QuoteRow {
            market: entry.market,
            symbol,
            name: entry.name.clone(),
            last: finite(quote.close),
            change_pct: change_pct.and_then(finite),
            change_amount: change_amount.and_then(finite),
            open: finite(quote.open),
            high: finite(quote.high),
            low: finite(quote.low),
            prev_close: prior_close.and_then(finite),
            volume: finite(quote.volume as f64),
            amount: None,
            pe: None,
            pb: None,
            market_cap: None,
            as_of: Some(quote.date),
            error: None,
        })
    }

    async fn candles(&self, entry: &WatchEntry) -> Result<Vec<Candle>> {
        let symbol = entry.market.normalize_symbol(&entry.symbol)?;
        let values = self.recent_candles(entry.market, &symbol, 120).await?;
        if values.is_empty() {
            return Err(Error::MissingCandles {
                market: entry.market.short_label(),
                symbol,
            });
        }
        Ok(values)
    }

    async fn recent_candles(
        &self,
        market: Market,
        symbol: &str,
        limit: usize,
    ) -> Result<Vec<Candle>> {
        let values = match market {
            Market::AShare => self.inner.a_share_candles(symbol, "qfq", limit).await?,
            Market::HongKong => self.inner.hk_candles(symbol, limit).await?,
            Market::Us => self.inner.us_candles(symbol, limit).await?,
        };
        Ok(keep_last(
            values
                .into_iter()
                .map(|item| Candle {
                    date: item.trade_date,
                    open: item.open,
                    high: item.high,
                    low: item.low,
                    close: item.close,
                    volume: item.volume as f64,
                })
                .collect(),
            limit,
        ))
    }

    async fn intraday(&self, entry: &WatchEntry) -> Result<Vec<IntradayPoint>> {
        let symbol = entry.market.normalize_symbol(&entry.symbol)?;
        let end = Utc::now().format("%Y%m%d").to_string();
        let start = end.clone();
        let values = match entry.market {
            Market::AShare => {
                self.inner
                    .stock_zh_a_hist_min(&symbol, "5", "qfq", &start, &end)
                    .await?
            }
            Market::HongKong => {
                self.inner
                    .stock_hk_hist_min(&symbol, "5", "qfq", &start, &end)
                    .await?
            }
            Market::Us => {
                self.inner
                    .stock_us_hist_min(&symbol, "5", "qfq", &start, &end)
                    .await?
            }
        };
        Ok(keep_last(
            values
                .into_iter()
                .map(|item| IntradayPoint {
                    time: item.trade_date,
                    price: item.close,
                    volume: item.volume,
                })
                .collect(),
            390,
        ))
    }

    async fn fundamentals(&self, entry: &WatchEntry) -> Result<Option<Fundamentals>> {
        let symbol = entry.market.normalize_symbol(&entry.symbol)?;
        match entry.market {
            Market::AShare => {
                let prefixed = if symbol.starts_with('6') {
                    format!("SH{symbol}")
                } else if symbol.starts_with('4') || symbol.starts_with('8') {
                    format!("BJ{symbol}")
                } else {
                    format!("SZ{symbol}")
                };
                let rows = self.inner.stock_zh_a_financial_indicator(&prefixed).await?;
                Ok(rows
                    .first()
                    .map(|row| fundamentals_from_json(row, entry.market, None)))
            }
            Market::HongKong => {
                let financial = self.inner.hk_financial(&symbol).await?;
                Ok(Some(Fundamentals {
                    pe: financial.pe_ttm.and_then(finite),
                    pb: financial.pb.and_then(finite),
                    market_cap: financial.market_cap_hkd.and_then(finite),
                    currency: entry.market.currency().to_string(),
                }))
            }
            Market::Us => {
                let rows = self.inner.stock_us_financial_indicator(&symbol).await?;
                let market_cap = self.inner.us_market_cap_from(&symbol).await?;
                Ok(rows
                    .first()
                    .map(|row| fundamentals_from_json(row, entry.market, market_cap))
                    .or_else(|| {
                        market_cap.map(|market_cap| Fundamentals {
                            pe: None,
                            pb: None,
                            market_cap: finite(market_cap),
                            currency: entry.market.currency().to_string(),
                        })
                    }))
            }
        }
    }

    async fn news(&self, entry: &WatchEntry) -> Result<Vec<NewsItem>> {
        let symbol = entry.market.normalize_symbol(&entry.symbol)?;
        let values = match entry.market {
            Market::AShare => self.inner.stock_news_em_by_name(&entry.name).await?,
            Market::HongKong => self.inner.stock_news_em_hk(&symbol).await?,
            Market::Us => self.inner.stock_news_em_us(&symbol).await?,
        };
        Ok(values
            .into_iter()
            .map(|item| NewsItem {
                published_at: item.publish_time,
                title: item.title,
                summary: item.content.unwrap_or_default(),
                source: item.source.unwrap_or_default(),
                url: item.url,
            })
            .collect())
    }

    async fn indices(&self, market: Market) -> Result<Vec<IndexRow>> {
        match market {
            Market::AShare => {
                let values = self.inner.index_stock_zh_spot_em("沪深重要指数").await?;
                Ok(values
                    .into_iter()
                    .filter(|item| {
                        matches!(
                            item.code.as_str(),
                            "000001" | "399001" | "399006" | "000300"
                        )
                    })
                    .map(|item| IndexRow {
                        market,
                        code: item.code,
                        name: item.name,
                        close: item.close,
                        change_pct: item.change_pct,
                        change_amount: item.change_amount,
                        volume: item.volume,
                        amount: item.amount,
                    })
                    .collect())
            }
            Market::HongKong => {
                let values = self.inner.index_hk_spot_em().await?;
                Ok(values
                    .into_iter()
                    .filter(|item| matches!(item.code.as_str(), "HSI" | "HSTECH" | "HSCEI"))
                    .map(|item| IndexRow {
                        market,
                        code: item.code,
                        name: item.name,
                        close: item.close,
                        change_pct: item.change_pct,
                        change_amount: item.change_amount,
                        volume: item.volume,
                        amount: item.amount,
                    })
                    .collect())
            }
            Market::Us => {
                let values = self.inner.index_global_spot().await?;
                Ok(values
                    .into_iter()
                    .filter(|item| matches!(item.code.as_str(), "DJIA" | "SPX" | "NDX"))
                    .map(|item| IndexRow {
                        market,
                        code: item.code,
                        name: item.name,
                        close: item.close,
                        change_pct: item.change_pct,
                        change_amount: item.change_amount,
                        volume: 0.0,
                        amount: 0.0,
                    })
                    .collect())
            }
        }
    }
}

fn degrade<T>(label: &str, entry: &WatchEntry, result: Result<T>, fallback: T) -> T {
    match result {
        Ok(value) => value,
        Err(error) => {
            warn!(market = %entry.market, symbol = %entry.symbol, fetch = label, %error, "optional stock detail fetch failed");
            fallback
        }
    }
}

fn finite(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

fn prior_close_from_history(candles: &[Candle], current: f64) -> Option<f64> {
    candles
        .iter()
        .rev()
        .find(|candle| candle.close.is_finite() && (candle.close - current).abs() > f64::EPSILON)
        .map(|candle| candle.close)
        .or_else(|| candles.iter().rev().nth(1).map(|candle| candle.close))
}

fn fundamentals_from_json(
    row: &serde_json::Value,
    market: Market,
    market_cap: Option<f64>,
) -> Fundamentals {
    Fundamentals {
        pe: json_number(row, &["PE_TTM", "PE", "PARENT_NETPROFIT_RATIO"]),
        pb: json_number(row, &["PB", "PB_MRQ", "BOOK_VALUE_RATIO"]),
        market_cap: market_cap
            .and_then(finite)
            .or_else(|| json_number(row, &["MARKET_CAP", "TOTAL_MARKET_CAP"])),
        currency: market.currency().to_string(),
    }
}

fn json_number(row: &serde_json::Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| {
        row.get(*key).and_then(|value| {
            value
                .as_f64()
                .or_else(|| value.as_str().and_then(|text| text.parse::<f64>().ok()))
                .and_then(finite)
        })
    })
}

fn search_spot_quotes(
    rows: &[akshare::stock::feature::SpotQuote],
    query: &str,
    limit: usize,
) -> Vec<SearchResult> {
    let query = query.trim().to_ascii_lowercase();
    rows.iter()
        .filter(|row| {
            row.code.to_ascii_lowercase().contains(&query)
                || row.name.to_ascii_lowercase().contains(&query)
        })
        .take(limit)
        .map(|row| SearchResult {
            market: Market::AShare,
            symbol: row.code.clone(),
            name: row.name.clone(),
            exchange: "SSE STAR".into(),
        })
        .collect()
}

fn keep_last<T>(mut values: Vec<T>, limit: usize) -> Vec<T> {
    if values.len() > limit {
        values.drain(..values.len() - limit);
    }
    values
}

fn epoch_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::{keep_last, search_spot_quotes};

    #[test]
    fn keep_last_caps_large_upstream_series() {
        assert_eq!(keep_last((0..10).collect(), 3), vec![7, 8, 9]);
    }

    #[test]
    fn keep_last_preserves_short_series() {
        assert_eq!(keep_last(vec![1, 2], 3), vec![1, 2]);
    }

    #[test]
    fn a_share_fallback_search_matches_star_market_code_and_name() {
        let rows = vec![akshare::stock::feature::SpotQuote {
            code: "688981".into(),
            name: "中芯国际".into(),
            latest_price: 0.0,
            change_pct: 0.0,
            change_amount: 0.0,
            volume: 0.0,
            amount: 0.0,
            amplitude_pct: 0.0,
            high: 0.0,
            low: 0.0,
            open: 0.0,
            prev_close: 0.0,
            volume_ratio: 0.0,
            turnover_rate: 0.0,
            pe_dynamic: 0.0,
            pb: 0.0,
            total_market_cap: 0.0,
            circulating_market_cap: 0.0,
            speed: 0.0,
            change_5min: 0.0,
            change_60d: 0.0,
            change_ytd: 0.0,
        }];
        assert_eq!(search_spot_quotes(&rows, "688981", 10)[0].symbol, "688981");
        assert_eq!(search_spot_quotes(&rows, "中芯", 10)[0].name, "中芯国际");
    }
}
