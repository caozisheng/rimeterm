// The audit functions and `ColorOps` trait are used from `#[cfg(test)]`
// blocks (Ship 1 invariants in `mod.rs::tests`, Ship 2 derivation tests
// in `tokens.rs::tests`) and exposed as public API for future per-theme
// derivations or user-customizable themes. Suppress the dev/release
// dead-code warnings — they're real API, just not exercised yet by the
// shipped binary.
#![cfg_attr(not(test), allow(dead_code))]

//! WCAG contrast utilities and a small color-arithmetic primitive.
//!
//! Two responsibilities:
//!
//! 1. **WCAG 2.1 contrast checks** ([`contrast_ratio`], [`luminance_delta`])
//!    used by the Ship 1 audit and the Ship 2 derivation invariants.
//! 2. **Color arithmetic** ([`ColorOps::lighten`], [`ColorOps::darken`])
//!    used by per-theme constructors to derive interaction colors from a
//!    small set of base hues. Linear-RGB blend toward white/black —
//!    correct *direction* and good-enough magnitude for terminal display
//!    where the terminal applies its own gamma curve anyway. Hand-rolled
//!    in <30 lines rather than pulling the `palette` crate; if a future
//!    need arises (perceptual-uniform CIE Lab, accessibility autocorrect)
//!    that's the trigger to revisit.

use ratatui::style::Color;

/// Convert a ratatui [`Color`] to approximate sRGB (0-255). Returns
/// `None` for indexed/reset/named colours whose on-screen RGB is
/// terminal-defined and so can't be evaluated objectively.
pub(crate) fn color_to_srgb(c: Color) -> Option<(u8, u8, u8)> {
    match c {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        _ => None,
    }
}

/// WCAG relative luminance of an sRGB triple (per WCAG 2.1).
pub(crate) fn relative_luminance((r, g, b): (u8, u8, u8)) -> f64 {
    fn channel(c: u8) -> f64 {
        let s = f64::from(c) / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
}

/// WCAG contrast ratio between two sRGB colours (per WCAG 2.1 SC 1.4.3).
/// Returns `None` if either input is a non-RGB ratatui colour.
pub(crate) fn contrast_ratio(fg: Color, bg: Color) -> Option<f64> {
    let l1 = relative_luminance(color_to_srgb(fg)?);
    let l2 = relative_luminance(color_to_srgb(bg)?);
    let (light, dark) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
    Some((light + 0.05) / (dark + 0.05))
}

/// Absolute WCAG-luminance distance between two colors, scaled to a
/// 0-100 range for human-readable test failures. Returns `None` if
/// either color is non-RGB.
///
/// Used by derivation invariants ("derived selection must be at least
/// N units away from its surface base") where a contrast *ratio* is
/// the wrong metric — both surfaces are bright, the ratio is near 1,
/// but a small luminance shift is still perceptually distinct.
pub(crate) fn luminance_delta(a: Color, b: Color) -> Option<f64> {
    let la = relative_luminance(color_to_srgb(a)?);
    let lb = relative_luminance(color_to_srgb(b)?);
    Some((la - lb).abs() * 100.0)
}

/// Color arithmetic: blend toward white/black, luminance-aware mode
/// detection. Operates on RGB-defined colours; named colours pass
/// through unchanged so callers can apply ops uniformly without
/// branching on color kind.
pub(crate) trait ColorOps {
    /// Blend toward black by `factor` (0.0 = unchanged, 1.0 = black).
    /// Linear-RGB so each step matches the WCAG luminance scale used
    /// by [`contrast_ratio`].
    fn darken(self, factor: f64) -> Self;

    /// Blend toward white by `factor` (0.0 = unchanged, 1.0 = white).
    fn lighten(self, factor: f64) -> Self;

    /// True when the color's WCAG luminance is above 0.5 (i.e. the
    /// surface is "light" and selection highlights should darken,
    /// not lighten). Returns `false` for non-RGB colors.
    fn is_light(&self) -> bool;
}

impl ColorOps for Color {
    fn darken(self, factor: f64) -> Self {
        let Some((r, g, b)) = color_to_srgb(self) else {
            return self;
        };
        let f = factor.clamp(0.0, 1.0);
        Color::Rgb(blend(r, 0, f), blend(g, 0, f), blend(b, 0, f))
    }

    fn lighten(self, factor: f64) -> Self {
        let Some((r, g, b)) = color_to_srgb(self) else {
            return self;
        };
        let f = factor.clamp(0.0, 1.0);
        Color::Rgb(blend(r, 255, f), blend(g, 255, f), blend(b, 255, f))
    }

    fn is_light(&self) -> bool {
        color_to_srgb(*self)
            .map(|rgb| relative_luminance(rgb) > 0.5)
            .unwrap_or(false)
    }
}

/// Linearly interpolate between two channel values in sRGB space.
fn blend(from: u8, to: u8, factor: f64) -> u8 {
    let from = f64::from(from);
    let to = f64::from(to);
    (from + (to - from) * factor).round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sanity-check the contrast formula against published WCAG examples.
    #[test]
    fn contrast_ratio_matches_known_values() {
        // White on black is the maximum possible: 21:1.
        let r = contrast_ratio(Color::Rgb(255, 255, 255), Color::Rgb(0, 0, 0)).unwrap();
        assert!((r - 21.0).abs() < 0.01, "white/black: {r:.4}");
        // Same colour on itself is the minimum: 1:1.
        let r = contrast_ratio(Color::Rgb(128, 128, 128), Color::Rgb(128, 128, 128)).unwrap();
        assert!((r - 1.0).abs() < 0.01, "self/self: {r:.4}");
        // Non-RGB inputs return None (caller skips silently).
        assert_eq!(contrast_ratio(Color::Cyan, Color::Black), None);
    }

    #[test]
    fn darken_endpoints_and_identity() {
        let c = Color::Rgb(100, 150, 200);
        assert_eq!(c.darken(0.0), c, "factor=0 is identity");
        assert_eq!(c.darken(1.0), Color::Rgb(0, 0, 0), "factor=1 is black");
        assert_eq!(c.darken(-0.5), c, "negative factor clamps to 0");
        assert_eq!(c.darken(1.5), Color::Rgb(0, 0, 0), "factor>1 clamps to 1");
        // Named colors pass through unchanged so callers can apply ops uniformly.
        assert_eq!(Color::Cyan.darken(0.5), Color::Cyan);
    }

    #[test]
    fn lighten_endpoints_and_identity() {
        let c = Color::Rgb(100, 150, 200);
        assert_eq!(c.lighten(0.0), c, "factor=0 is identity");
        assert_eq!(
            c.lighten(1.0),
            Color::Rgb(255, 255, 255),
            "factor=1 is white"
        );
        assert_eq!(Color::Yellow.lighten(0.5), Color::Yellow);
    }

    /// 50% darken should land halfway between original and black on
    /// each channel. Verifies the linear blend math; tolerates ±1 due
    /// to integer rounding.
    #[test]
    fn darken_50_percent_is_midway() {
        let c = Color::Rgb(200, 100, 50);
        let d = c.darken(0.5);
        let Color::Rgb(r, g, b) = d else {
            panic!("expected RGB")
        };
        assert!((i16::from(r) - 100).abs() <= 1, "r={r}");
        assert!((i16::from(g) - 50).abs() <= 1, "g={g}");
        assert!((i16::from(b) - 25).abs() <= 1, "b={b}");
    }

    #[test]
    fn is_light_classifies_correctly() {
        assert!(Color::Rgb(255, 255, 255).is_light(), "white is light");
        assert!(!Color::Rgb(0, 0, 0).is_light(), "black is dark");
        // Solarized base3 — the "lightest" surface.
        assert!(
            Color::Rgb(253, 246, 227).is_light(),
            "solarized base3 is light"
        );
        // Solarized base03 — the "darkest" surface.
        assert!(
            !Color::Rgb(0, 43, 54).is_light(),
            "solarized base03 is dark"
        );
        // Non-RGB returns false (caller treats as dark).
        assert!(!Color::Cyan.is_light());
    }

    #[test]
    fn luminance_delta_zero_for_same_color() {
        let c = Color::Rgb(128, 128, 128);
        assert_eq!(luminance_delta(c, c), Some(0.0));
    }
}
