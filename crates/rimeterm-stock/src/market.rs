use std::fmt;
use std::time::Duration;

use chrono::{DateTime, Datelike, NaiveTime, Utc, Weekday};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Equity market supported by the stock pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Market {
    AShare,
    HongKong,
    Us,
}

impl Market {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::AShare => "A股",
            Self::HongKong => "港股",
            Self::Us => "美股",
        }
    }

    #[must_use]
    pub const fn short_label(self) -> &'static str {
        match self {
            Self::AShare => "A",
            Self::HongKong => "HK",
            Self::Us => "US",
        }
    }

    #[must_use]
    pub const fn currency(self) -> &'static str {
        match self {
            Self::AShare => "CNY",
            Self::HongKong => "HKD",
            Self::Us => "USD",
        }
    }

    pub fn normalize_symbol(self, symbol: &str) -> Result<String> {
        let trimmed = symbol.trim();
        match self {
            Self::AShare => normalize_a_share(trimmed),
            Self::HongKong => normalize_hk(trimmed),
            Self::Us => normalize_us(trimmed),
        }
    }

    /// Reports whether `at` falls inside a regular weekday trading session.
    ///
    /// Exchange holiday calendars are not available from the data provider, so
    /// this deliberately uses weekday sessions as the deterministic fallback.
    #[must_use]
    pub fn is_open_at(self, at: DateTime<Utc>) -> bool {
        let (tz, sessions): (Tz, &[(NaiveTime, NaiveTime)]) = match self {
            Self::AShare => (chrono_tz::Asia::Shanghai, &A_SESSIONS),
            Self::HongKong => (chrono_tz::Asia::Hong_Kong, &HK_SESSIONS),
            Self::Us => (chrono_tz::America::New_York, &US_SESSIONS),
        };
        let local = at.with_timezone(&tz);
        if matches!(local.weekday(), Weekday::Sat | Weekday::Sun) {
            return false;
        }
        let time = local.time();
        sessions
            .iter()
            .any(|(start, end)| time >= *start && time < *end)
    }

    #[must_use]
    pub fn refresh_interval(self, at: DateTime<Utc>, open_hz: u16, closed_secs: u64) -> Duration {
        if self.is_open_at(at) {
            Duration::from_secs_f64(1.0 / f64::from(open_hz.max(1)))
        } else {
            Duration::from_secs(closed_secs.max(1))
        }
    }
}

impl fmt::Display for Market {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

const fn time(hour: u32, minute: u32) -> NaiveTime {
    match NaiveTime::from_hms_opt(hour, minute, 0) {
        Some(value) => value,
        None => NaiveTime::MIN,
    }
}

const A_SESSIONS: [(NaiveTime, NaiveTime); 2] =
    [(time(9, 30), time(11, 30)), (time(13, 0), time(15, 0))];
const HK_SESSIONS: [(NaiveTime, NaiveTime); 2] =
    [(time(9, 30), time(12, 0)), (time(13, 0), time(16, 0))];
const US_SESSIONS: [(NaiveTime, NaiveTime); 1] = [(time(9, 30), time(16, 0))];

fn normalize_a_share(symbol: &str) -> Result<String> {
    let upper = symbol.to_ascii_uppercase();
    let code = upper
        .strip_prefix("SH")
        .or_else(|| upper.strip_prefix("SZ"))
        .or_else(|| upper.strip_prefix("BJ"))
        .unwrap_or(&upper);
    let code = code
        .strip_suffix(".SH")
        .or_else(|| code.strip_suffix(".SZ"))
        .or_else(|| code.strip_suffix(".BJ"))
        .unwrap_or(code);
    if code.len() == 6 && code.bytes().all(|byte| byte.is_ascii_digit()) {
        Ok(code.to_string())
    } else {
        Err(invalid(Market::AShare, symbol, "expected a six-digit code"))
    }
}

fn normalize_hk(symbol: &str) -> Result<String> {
    let upper = symbol.to_ascii_uppercase();
    let code = upper.strip_suffix(".HK").unwrap_or(&upper);
    if code.is_empty() || code.len() > 5 || !code.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid(
            Market::HongKong,
            symbol,
            "expected one to five digits, optionally followed by .HK",
        ));
    }
    let value = code.parse::<u32>().map_err(|_| {
        invalid(
            Market::HongKong,
            symbol,
            "code is outside the supported range",
        )
    })?;
    Ok(format!("{value:05}"))
}

fn normalize_us(symbol: &str) -> Result<String> {
    let upper = symbol.trim().to_ascii_uppercase();
    let valid = !upper.is_empty()
        && upper.len() <= 15
        && upper
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
        && upper.bytes().any(|byte| byte.is_ascii_alphabetic());
    if valid {
        Ok(upper)
    } else {
        Err(invalid(
            Market::Us,
            symbol,
            "expected an alphanumeric ticker with optional dot or hyphen",
        ))
    }
}

fn invalid(market: Market, symbol: &str, reason: &'static str) -> Error {
    Error::InvalidSymbol {
        market: market.short_label(),
        symbol: symbol.to_string(),
        reason,
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::Market;

    #[test]
    fn normalizes_market_symbols() {
        assert_eq!(
            Market::AShare.normalize_symbol("sh600519").unwrap(),
            "600519"
        );
        assert_eq!(
            Market::HongKong.normalize_symbol("700.hk").unwrap(),
            "00700"
        );
        assert_eq!(Market::Us.normalize_symbol("brk.b").unwrap(), "BRK.B");
        assert!(Market::AShare.normalize_symbol("700").is_err());
    }

    #[test]
    fn asia_split_sessions_exclude_lunch_and_close_boundary() {
        let a_morning = Utc.with_ymd_and_hms(2026, 7, 6, 2, 0, 0).single().unwrap();
        let a_lunch = Utc.with_ymd_and_hms(2026, 7, 6, 4, 0, 0).single().unwrap();
        let hk_afternoon = Utc.with_ymd_and_hms(2026, 7, 6, 7, 0, 0).single().unwrap();
        assert!(Market::AShare.is_open_at(a_morning));
        assert!(!Market::AShare.is_open_at(a_lunch));
        assert!(Market::HongKong.is_open_at(hk_afternoon));
    }

    #[test]
    fn us_session_tracks_daylight_saving_time() {
        let winter_open = Utc.with_ymd_and_hms(2026, 1, 5, 15, 0, 0).single().unwrap();
        let summer_open = Utc.with_ymd_and_hms(2026, 7, 6, 14, 0, 0).single().unwrap();
        let summer_before = Utc.with_ymd_and_hms(2026, 7, 6, 13, 0, 0).single().unwrap();
        assert!(Market::Us.is_open_at(winter_open));
        assert!(Market::Us.is_open_at(summer_open));
        assert!(!Market::Us.is_open_at(summer_before));
    }

    #[test]
    fn weekends_are_closed() {
        let saturday = Utc.with_ymd_and_hms(2026, 7, 4, 15, 0, 0).single().unwrap();
        assert!(!Market::Us.is_open_at(saturday));
    }

    #[test]
    fn refresh_interval_clamps_zero_configuration() {
        let open = Utc.with_ymd_and_hms(2026, 7, 6, 14, 0, 0).single().unwrap();
        let closed = Utc.with_ymd_and_hms(2026, 7, 6, 22, 0, 0).single().unwrap();
        assert_eq!(Market::Us.refresh_interval(open, 0, 60).as_secs(), 1);
        assert_eq!(Market::Us.refresh_interval(closed, 2, 0).as_secs(), 1);
        assert_eq!(Market::Us.refresh_interval(open, 2, 60).as_millis(), 500);
    }
}
