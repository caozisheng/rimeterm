//! Compact display helpers for the ModelsPane.
//!
//! Ported from upstream `modelsdev::formatting`, trimmed to the four
//! helpers the pane's flat table actually calls. Rendering (colors,
//! layout) stays in `rimeterm_tui` — this module only hands back
//! `String`s so the crate has no ratatui dependency.

/// Em-dash used to render "missing value" in the model table. Matches
/// upstream `modelsdev::formatting::EM_DASH` so a shared visual language
/// carries across.
pub const EM_DASH: &str = "\u{2014}";

/// Compact token count: `128000` → `128K`, `2_000_000` → `2.0M`,
/// `750` → `750`. Ported from `modelsdev::formatting::format_tokens`.
pub fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        let millions = n as f64 / 1_000_000.0;
        if millions >= 10.0 {
            format!("{millions:.0}M")
        } else {
            format!("{millions:.1}M")
        }
    } else if n >= 1_000 {
        let thousands = n as f64 / 1_000.0;
        format!("{thousands:.0}K")
    } else {
        format!("{n}")
    }
}

/// Compact price display used in list columns — mirrors upstream
/// `Model::cost_short`. Renders `None` as an em-dash so the column is
/// always the same width.
pub fn format_cost_short(value: Option<f64>) -> String {
    match value {
        Some(v) if v >= 100.0 => format!("${v:.0}"),
        Some(v) if v >= 1.0 => format!("${v:.1}"),
        Some(v) if v >= 0.01 => format!("${v:.2}"),
        Some(v) => format!("${v:.3}"),
        None => EM_DASH.to_string(),
    }
}

/// Human "input/output cost per million tokens" pair for the detail
/// row, e.g. `"$2.5 / $10.0"`. Missing sides render as em-dash.
pub fn format_cost_pair(input: Option<f64>, output: Option<f64>) -> String {
    format!(
        "{} / {}",
        format_cost_short(input),
        format_cost_short(output)
    )
}

/// Format a context / limit value or em-dash when unknown.
pub fn format_context(v: Option<u64>) -> String {
    v.map(format_tokens).unwrap_or_else(|| EM_DASH.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_bins_match_upstream_shape() {
        assert_eq!(format_tokens(500), "500");
        assert_eq!(format_tokens(1_500), "2K"); // rounds to nearest K
        assert_eq!(format_tokens(128_000), "128K");
        assert_eq!(format_tokens(2_000_000), "2.0M");
        assert_eq!(format_tokens(15_000_000), "15M");
    }

    #[test]
    fn cost_short_thresholds() {
        assert_eq!(format_cost_short(Some(0.001)), "$0.001");
        assert_eq!(format_cost_short(Some(0.5)), "$0.50");
        assert_eq!(format_cost_short(Some(2.5)), "$2.5");
        assert_eq!(format_cost_short(Some(150.0)), "$150");
        assert_eq!(format_cost_short(None), EM_DASH);
    }

    #[test]
    fn cost_pair_renders_both_sides() {
        assert_eq!(format_cost_pair(Some(2.5), Some(10.0)), "$2.5 / $10.0");
        assert_eq!(
            format_cost_pair(None, Some(1.0)),
            format!("{EM_DASH} / $1.0")
        );
    }

    #[test]
    fn context_missing_renders_em_dash() {
        assert_eq!(format_context(None), EM_DASH);
        assert_eq!(format_context(Some(64_000)), "64K");
    }
}
