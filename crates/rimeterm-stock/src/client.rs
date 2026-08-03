use std::time::{SystemTime, UNIX_EPOCH};

use akshare::AkShareClient;
use chrono::Utc;
use encoding_rs::GBK;
use futures::stream::{self, StreamExt};
use tracing::warn;

use crate::error::{Error, Result};
use crate::model::{
    Candle, DetailBundle, Fundamentals, IndexRow, IntradayPoint, LiveDetail, NewsItem, QuoteRow,
    SearchResult, Snapshot,
};
use crate::{Market, WatchEntry};

#[derive(Debug)]
struct UsSpot {
    code: String,
    latest_price: f64,
    change_pct: f64,
    change_amount: f64,
    volume: f64,
    amount: f64,
    high: f64,
    low: f64,
    open: f64,
    prev_close: f64,
    pe_dynamic: f64,
    pb: f64,
    total_market_cap: f64,
}

#[derive(Clone)]
pub struct StockClient {
    inner: AkShareClient,
    http: reqwest::Client,
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
        Self::from_inner(builder.build(), proxy)
    }

    fn from_inner(inner: AkShareClient, proxy: Option<&str>) -> Self {
        let mut builder = reqwest::Client::builder()
            .http1_only()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30));
        if let Some(proxy) = proxy
            .filter(|value| !value.trim().is_empty())
            .and_then(|value| reqwest::Proxy::all(value).ok())
        {
            builder = builder.proxy(proxy);
        }
        Self {
            inner,
            http: builder.build().unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    pub async fn refresh(&self, entries: &[WatchEntry], market: Market) -> Result<Snapshot> {
        let mut rows = if market == Market::Us {
            self.refresh_us(entries).await?
        } else {
            stream::iter(entries.iter().filter(|entry| entry.market == market))
                .map(|entry| async move {
                    match self.quote(entry).await {
                        Ok(row) => row,
                        Err(error) => quote_error_row(entry, error.to_string()),
                    }
                })
                .buffer_unordered(8)
                .collect::<Vec<QuoteRow>>()
                .await
        };
        // Lock display order: sort watchlist quotes by symbol (Code) so rows
        // do not shuffle each refresh due to concurrent request completion.
        rows.sort_by(|a, b| {
            a.symbol
                .to_ascii_uppercase()
                .cmp(&b.symbol.to_ascii_uppercase())
        });
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

    async fn refresh_us(&self, entries: &[WatchEntry]) -> Result<Vec<QuoteRow>> {
        let entries = entries
            .iter()
            .filter(|entry| entry.market == Market::Us)
            .collect::<Vec<_>>();
        if entries.is_empty() {
            return Ok(Vec::new());
        }

        let symbols = entries
            .iter()
            .map(|entry| entry.market.normalize_symbol(&entry.symbol))
            .collect::<Result<Vec<_>>>()?;
        let spots = self.us_spots(&symbols).await?;
        Ok(entries
            .into_iter()
            .zip(symbols)
            .map(|(entry, symbol)| {
                spots
                    .iter()
                    .find(|spot| spot.code.eq_ignore_ascii_case(&symbol))
                    .map_or_else(
                        || quote_error_row(entry, "realtime quote unavailable".into()),
                        |spot| us_quote_row(entry, spot),
                    )
            })
            .collect())
    }

    async fn us_spots(&self, symbols: &[String]) -> Result<Vec<UsSpot>> {
        let list = symbols
            .iter()
            .map(|symbol| format!("gb_{}", symbol.to_ascii_lowercase()))
            .collect::<Vec<_>>()
            .join(",");
        let bytes = self
            .http
            .get(format!("https://hq.sinajs.cn/list={list}"))
            .header("Referer", "https://finance.sina.com.cn")
            .send()
            .await
            .map_err(akshare::Error::from)?
            .error_for_status()
            .map_err(akshare::Error::from)?
            .bytes()
            .await
            .map_err(akshare::Error::from)?;
        let (body, _, _) = GBK.decode(&bytes);
        Ok(body.lines().filter_map(parse_sina_us_spot).collect())
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

fn parse_sina_us_spot(line: &str) -> Option<UsSpot> {
    let (prefix, quoted) = line.split_once('=')?;
    let code = prefix
        .trim()
        .strip_prefix("var hq_str_gb_")?
        .to_ascii_uppercase();
    let fields = quoted
        .trim()
        .trim_matches(';')
        .trim_matches('"')
        .split(',')
        .collect::<Vec<_>>();
    if fields.len() < 36 {
        return None;
    }
    Some(UsSpot {
        code,
        latest_price: fields[1].parse().ok()?,
        change_pct: fields[2].parse().unwrap_or_default(),
        change_amount: fields[4].parse().unwrap_or_default(),
        volume: fields[10].parse().unwrap_or_default(),
        amount: fields[35].parse().unwrap_or_default(),
        high: fields[6].parse().unwrap_or_default(),
        low: fields[7].parse().unwrap_or_default(),
        open: fields[5].parse().unwrap_or_default(),
        prev_close: fields[26].parse().unwrap_or_default(),
        pe_dynamic: fields[14].parse().unwrap_or_default(),
        pb: fields[16].parse().unwrap_or_default(),
        total_market_cap: fields[12].parse().unwrap_or_default(),
    })
}

fn us_quote_row(entry: &WatchEntry, spot: &impl UsQuote) -> QuoteRow {
    QuoteRow {
        market: Market::Us,
        symbol: spot.code().to_ascii_uppercase(),
        name: entry.name.clone(),
        last: finite(spot.latest_price()),
        change_pct: finite(spot.change_pct()),
        change_amount: finite(spot.change_amount()),
        open: finite(spot.open()),
        high: finite(spot.high()),
        low: finite(spot.low()),
        prev_close: finite(spot.prev_close()),
        volume: finite(spot.volume()),
        amount: finite(spot.amount()),
        pe: finite(spot.pe_dynamic()),
        pb: finite(spot.pb()),
        market_cap: finite(spot.total_market_cap()),
        as_of: None,
        error: None,
    }
}

trait UsQuote {
    fn code(&self) -> &str;
    fn latest_price(&self) -> f64;
    fn change_pct(&self) -> f64;
    fn change_amount(&self) -> f64;
    fn volume(&self) -> f64;
    fn amount(&self) -> f64;
    fn high(&self) -> f64;
    fn low(&self) -> f64;
    fn open(&self) -> f64;
    fn prev_close(&self) -> f64;
    fn pe_dynamic(&self) -> f64;
    fn pb(&self) -> f64;
    fn total_market_cap(&self) -> f64;
}

impl UsQuote for UsSpot {
    fn code(&self) -> &str {
        &self.code
    }
    fn latest_price(&self) -> f64 {
        self.latest_price
    }
    fn change_pct(&self) -> f64 {
        self.change_pct
    }
    fn change_amount(&self) -> f64 {
        self.change_amount
    }
    fn volume(&self) -> f64 {
        self.volume
    }
    fn amount(&self) -> f64 {
        self.amount
    }
    fn high(&self) -> f64 {
        self.high
    }
    fn low(&self) -> f64 {
        self.low
    }
    fn open(&self) -> f64 {
        self.open
    }
    fn prev_close(&self) -> f64 {
        self.prev_close
    }
    fn pe_dynamic(&self) -> f64 {
        self.pe_dynamic
    }
    fn pb(&self) -> f64 {
        self.pb
    }
    fn total_market_cap(&self) -> f64 {
        self.total_market_cap
    }
}

impl UsQuote for akshare::stock::feature::SpotQuote {
    fn code(&self) -> &str {
        &self.code
    }
    fn latest_price(&self) -> f64 {
        self.latest_price
    }
    fn change_pct(&self) -> f64 {
        self.change_pct
    }
    fn change_amount(&self) -> f64 {
        self.change_amount
    }
    fn volume(&self) -> f64 {
        self.volume
    }
    fn amount(&self) -> f64 {
        self.amount
    }
    fn high(&self) -> f64 {
        self.high
    }
    fn low(&self) -> f64 {
        self.low
    }
    fn open(&self) -> f64 {
        self.open
    }
    fn prev_close(&self) -> f64 {
        self.prev_close
    }
    fn pe_dynamic(&self) -> f64 {
        self.pe_dynamic
    }
    fn pb(&self) -> f64 {
        self.pb
    }
    fn total_market_cap(&self) -> f64 {
        self.total_market_cap
    }
}

fn quote_error_row(entry: &WatchEntry, error: String) -> QuoteRow {
    QuoteRow {
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
        error: Some(error),
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
    use super::{StockClient, keep_last, parse_sina_us_spot, search_spot_quotes, us_quote_row};

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

    #[test]
    fn us_spot_quote_maps_realtime_fields_without_daily_candles() {
        let entry = crate::WatchEntry {
            market: crate::Market::Us,
            symbol: "AAPL".into(),
            name: "Apple Inc.".into(),
        };
        let spot = akshare::stock::feature::SpotQuote {
            code: "AAPL".into(),
            name: "苹果".into(),
            latest_price: 304.88,
            change_pct: -1.3,
            change_amount: -4.03,
            volume: 29_798_441.0,
            amount: 9_115_710_976.0,
            amplitude_pct: 2.99,
            high: 311.8,
            low: 302.56,
            open: 309.67,
            prev_close: 308.91,
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
        };

        let row = us_quote_row(&entry, &spot);

        assert_eq!(
            (
                row.last,
                row.change_pct,
                row.change_amount,
                row.open,
                row.high,
                row.low,
                row.prev_close,
                row.volume,
                row.amount,
            ),
            (
                Some(304.88),
                Some(-1.3),
                Some(-4.03),
                Some(309.67),
                Some(311.8),
                Some(302.56),
                Some(308.91),
                Some(29_798_441.0),
                Some(9_115_710_976.0),
            )
        );
    }

    #[test]
    fn sina_us_spot_parser_reads_live_price_fields() {
        let spot = parse_sina_us_spot(
            "var hq_str_gb_aapl=\"Apple,306.2350,-0.87,2026-08-04 00:47:24,-2.6750,309.5800,311.8000,302.5600,344.5700,200.6250,35140199,58675387,4495878907900,8.30,36.900000,0.87,0.00,0.00,0.00,14681140000,63,0.0000,0.00,0.00,,Aug 03 12:47PM EDT,308.9100,0,1,2026,10749201107.0000,0.0000,0.0000,0.0000,0.0000,308.9100\";",
        )
        .expect("valid Sina quote");

        assert_eq!(
            (
                spot.code.as_str(),
                spot.latest_price,
                spot.open,
                spot.high,
                spot.low,
                spot.prev_close
            ),
            ("AAPL", 306.235, 309.58, 311.8, 302.56, 308.91)
        );
    }

    #[tokio::test]
    #[ignore = "live Sina smoke test"]
    async fn live_us_refresh_returns_realtime_aapl_quote() {
        let client = StockClient::new(None, None);
        let entries = [crate::WatchEntry {
            market: crate::Market::Us,
            symbol: "AAPL".into(),
            name: "Apple Inc.".into(),
        }];

        let snapshot = client
            .refresh(&entries, crate::Market::Us)
            .await
            .expect("live US refresh");

        assert!(snapshot.rows[0].last.is_some(), "{snapshot:?}");
    }
}
