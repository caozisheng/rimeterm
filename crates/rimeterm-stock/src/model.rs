use serde::{Deserialize, Serialize};

use crate::Market;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuoteRow {
    pub market: Market,
    pub symbol: String,
    pub name: String,
    pub last: Option<f64>,
    pub change_pct: Option<f64>,
    pub change_amount: Option<f64>,
    pub open: Option<f64>,
    pub high: Option<f64>,
    pub low: Option<f64>,
    pub prev_close: Option<f64>,
    pub volume: Option<f64>,
    pub amount: Option<f64>,
    pub pe: Option<f64>,
    pub pb: Option<f64>,
    pub market_cap: Option<f64>,
    pub as_of: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexRow {
    pub market: Market,
    pub code: String,
    pub name: String,
    pub close: f64,
    pub change_pct: f64,
    pub change_amount: f64,
    pub volume: f64,
    pub amount: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntradayPoint {
    pub time: String,
    pub price: f64,
    pub volume: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candle {
    pub date: String,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewsItem {
    pub published_at: String,
    pub title: String,
    pub summary: String,
    pub source: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fundamentals {
    pub pe: Option<f64>,
    pub pb: Option<f64>,
    pub market_cap: Option<f64>,
    pub currency: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetailBundle {
    pub quote: QuoteRow,
    pub intraday: Vec<IntradayPoint>,
    pub candles: Vec<Candle>,
    pub fundamentals: Option<Fundamentals>,
    pub news: Vec<NewsItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveDetail {
    pub quote: QuoteRow,
    pub intraday: Vec<IntradayPoint>,
}

impl DetailBundle {
    pub fn apply_live(&mut self, live: LiveDetail) {
        self.quote = live.quote;
        if !live.intraday.is_empty() {
            self.intraday = live.intraday;
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub rows: Vec<QuoteRow>,
    pub indices: Vec<IndexRow>,
    pub fetched_at_epoch_secs: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    pub market: Market,
    pub symbol: String,
    pub name: String,
    pub exchange: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_live_update_preserves_static_sections() {
        let mut detail = DetailBundle {
            quote: quote(1.0),
            intraday: vec![IntradayPoint {
                time: "09:30".into(),
                price: 1.0,
                volume: 1.0,
            }],
            candles: vec![Candle {
                date: "2026-08-01".into(),
                open: 1.0,
                high: 2.0,
                low: 0.5,
                close: 1.5,
                volume: 2.0,
            }],
            fundamentals: Some(Fundamentals {
                pe: Some(10.0),
                pb: Some(2.0),
                market_cap: Some(3.0),
                currency: "CNY".into(),
            }),
            news: vec![NewsItem {
                published_at: "now".into(),
                title: "headline".into(),
                summary: String::new(),
                source: "source".into(),
                url: None,
            }],
        };
        let static_sections = (
            detail.candles.clone(),
            detail.fundamentals.clone(),
            detail.news.clone(),
        );
        detail.apply_live(LiveDetail {
            quote: quote(2.0),
            intraday: vec![IntradayPoint {
                time: "09:31".into(),
                price: 2.0,
                volume: 2.0,
            }],
        });
        assert_eq!(
            (
                detail.quote.last,
                detail.intraday[0].price,
                detail.candles,
                detail.fundamentals,
                detail.news
            ),
            (
                Some(2.0),
                2.0,
                static_sections.0,
                static_sections.1,
                static_sections.2
            )
        );
    }

    fn quote(last: f64) -> QuoteRow {
        QuoteRow {
            market: Market::AShare,
            symbol: "600519".into(),
            name: "贵州茅台".into(),
            last: Some(last),
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
            error: None,
        }
    }
}
