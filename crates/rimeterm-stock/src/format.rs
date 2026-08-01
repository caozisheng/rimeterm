/// Formats a price with a compact, precision-preserving number of decimals.
#[must_use]
pub fn format_price(value: Option<f64>) -> String {
    match value.filter(|number| number.is_finite()) {
        None => "--".to_string(),
        Some(number) if number.abs() >= 100.0 => format!("{number:.2}"),
        Some(number) if number.abs() >= 1.0 => format!("{number:.3}"),
        Some(number) => format!("{number:.4}"),
    }
}

/// Formats percentage change with an explicit sign and percent suffix.
#[must_use]
pub fn format_change_pct(value: Option<f64>) -> String {
    value
        .filter(|number| number.is_finite())
        .map_or_else(|| "--".to_string(), |number| format!("{number:+.2}%"))
}

/// Formats a large scalar with K/M/B/T suffixes.
#[must_use]
pub fn format_compact(value: Option<f64>) -> String {
    let Some(number) = value.filter(|number| number.is_finite()) else {
        return "--".to_string();
    };
    let absolute = number.abs();
    let (scaled, suffix) = if absolute >= 1_000_000_000_000.0 {
        (number / 1_000_000_000_000.0, "T")
    } else if absolute >= 1_000_000_000.0 {
        (number / 1_000_000_000.0, "B")
    } else if absolute >= 1_000_000.0 {
        (number / 1_000_000.0, "M")
    } else if absolute >= 1_000.0 {
        (number / 1_000.0, "K")
    } else {
        return format!("{number:.0}");
    };
    format!("{scaled:.2}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::{format_change_pct, format_compact, format_price};

    #[test]
    fn formats_missing_and_non_finite_as_placeholder() {
        assert_eq!(format_price(None), "--");
        assert_eq!(format_change_pct(Some(f64::NAN)), "--");
        assert_eq!(format_compact(Some(f64::INFINITY)), "--");
    }

    #[test]
    fn formats_prices_by_magnitude() {
        assert_eq!(format_price(Some(1688.0)), "1688.00");
        assert_eq!(format_price(Some(7.1256)), "7.126");
        assert_eq!(format_price(Some(0.12345)), "0.1235");
    }

    #[test]
    fn formats_signed_percentage() {
        assert_eq!(format_change_pct(Some(1.25)), "+1.25%");
        assert_eq!(format_change_pct(Some(-0.5)), "-0.50%");
    }

    #[test]
    fn formats_compact_values() {
        assert_eq!(format_compact(Some(999.0)), "999");
        assert_eq!(format_compact(Some(12_500.0)), "12.50K");
        assert_eq!(format_compact(Some(-2_000_000.0)), "-2.00M");
        assert_eq!(format_compact(Some(3_250_000_000.0)), "3.25B");
    }
}
