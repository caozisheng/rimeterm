//! Model → USD pricing table for the [`AgtopPane`].
//!
//! Ported from upstream `agtop`'s `src/pricing.rs` (v2.4.24, MIT) with
//! two deliberate deviations to keep the compiled binary small:
//!
//! - We ship a hand-curated table (~40 entries covering the SKUs a
//!   coding-agent user is realistically running) rather than the
//!   auto-generated ~1,800-entry LiteLLM dump. If a model drifts off
//!   the list the cost line degrades to `unknown` — better than
//!   bloating rimeterm's binary with a 240 KiB `pricing_data.rs`
//!   we'd have to keep in sync forever.
//! - No TOML override file (yet). All rows are compile-time constants.
//!   A future `agtop.prices` config-file knob is a natural add.
//!
//! Cache-aware pricing (Anthropic prompt-caching) IS honoured — cache
//! reads at 0.10× input, cache writes at 1.25× input — so a long
//! Claude session that leans on prompt caching isn't overbilled by an
//! order of magnitude the way the naïve `input × rate` formula would.
//!
//! [`AgtopPane`]: crate::agtop_pane::AgtopPane

/// Per-model pricing entry. Every rate is USD per 1,000,000 tokens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelPrice {
    /// USD per 1M input tokens (standard, uncached).
    pub input_per_mtok: f64,
    /// USD per 1M output tokens.
    pub output_per_mtok: f64,
    /// Model's advertised max input-window size in tokens. Drives the
    /// per-agent context-fill bar in the detail popup.
    pub max_input_tokens: u64,
}

/// How `cost_usd` was computed. Surfaces in the detail popup so a
/// `$0.00` on an unknown model doesn't look like a `local` freebie.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CostBasis {
    /// Known per-token rate — cost is a real dollar estimate.
    Api,
    /// Local runtime (Ollama, llama.cpp, vLLM, LM Studio, HF endpoint).
    /// No API expenditure by design; cost is `$0`.
    Local,
    /// Model name was missing or didn't match anything. Cost is `$0`
    /// but callers should render it as `—` / `unknown`, not `$0`.
    Unknown,
}

impl CostBasis {
    pub fn label(self) -> &'static str {
        match self {
            CostBasis::Api => "api",
            CostBasis::Local => "local",
            CostBasis::Unknown => "unknown",
        }
    }
}

/// Local-runtime markers. Any model id containing one of these
/// substrings (case-insensitive) short-circuits cost to `$0` with
/// [`CostBasis::Local`]. Conservative on purpose — only well-known
/// on-device runtimes, not every open-weights model that might be
/// served by a paid endpoint.
const LOCAL_MARKERS: &[&str] = &[
    "ollama/",
    "ollama_chat/",
    "ollama:",
    "lmstudio/",
    "lm-studio/",
    "vllm/",
    "llama-cpp/",
    "llamacpp/",
    "localhost:",
    "127.0.0.1:",
    "huggingface/",
];

/// True if the model string identifies a local (no-API-cost) runtime.
pub fn is_local_model(model: &str) -> bool {
    if model.is_empty() {
        return false;
    }
    let lower = model.to_ascii_lowercase();
    LOCAL_MARKERS.iter().any(|m| lower.contains(m))
}

/// Compile-time price table. Ordered by vendor for reviewability;
/// runtime lookup goes through [`PriceTable`] which normalises access
/// and adds suffix-tolerant matching.
///
/// Prices as of 2026 Q1; sourced from Anthropic / OpenAI / Google
/// public docs. Keep this list to models a coding agent actually
/// invokes — no image gen, no whisper, no embeddings — so the table
/// stays scannable.
const CURATED: &[(&str, ModelPrice)] = &[
    // Anthropic — Claude 4 family (200K standard; -1m suffix promotes
    // to 1M via `PriceTable::context_limit`).
    ("claude-opus-4-7", pk(15.00, 75.00, 200_000)),
    ("claude-opus-4-1", pk(15.00, 75.00, 200_000)),
    ("claude-opus-4", pk(15.00, 75.00, 200_000)),
    ("claude-sonnet-4-7", pk(3.00, 15.00, 200_000)),
    ("claude-sonnet-4-6", pk(3.00, 15.00, 200_000)),
    ("claude-sonnet-4-5", pk(3.00, 15.00, 200_000)),
    ("claude-sonnet-4", pk(3.00, 15.00, 200_000)),
    ("claude-haiku-4-5", pk(0.80, 4.00, 200_000)),
    ("claude-haiku-4", pk(0.80, 4.00, 200_000)),
    // Anthropic — Claude 3.5 / 3 legacy.
    ("claude-3-5-sonnet", pk(3.00, 15.00, 200_000)),
    ("claude-3-5-haiku", pk(0.80, 4.00, 200_000)),
    ("claude-3-opus", pk(15.00, 75.00, 200_000)),
    ("claude-3-sonnet", pk(3.00, 15.00, 200_000)),
    ("claude-3-haiku", pk(0.25, 1.25, 200_000)),
    // OpenAI — GPT-5 / GPT-4 families.
    ("gpt-5", pk(1.25, 10.00, 256_000)),
    ("gpt-5-mini", pk(0.25, 2.00, 256_000)),
    ("gpt-5-nano", pk(0.05, 0.40, 256_000)),
    ("gpt-4o", pk(2.50, 10.00, 128_000)),
    ("gpt-4o-mini", pk(0.15, 0.60, 128_000)),
    ("gpt-4-turbo", pk(10.00, 30.00, 128_000)),
    ("gpt-4", pk(30.00, 60.00, 8_192)),
    ("gpt-3.5-turbo", pk(0.50, 1.50, 16_385)),
    // OpenAI — reasoning models.
    ("o1", pk(15.00, 60.00, 200_000)),
    ("o1-mini", pk(1.10, 4.40, 128_000)),
    ("o1-preview", pk(15.00, 60.00, 128_000)),
    ("o3", pk(2.00, 8.00, 200_000)),
    ("o3-mini", pk(1.10, 4.40, 200_000)),
    ("o4-mini", pk(1.10, 4.40, 200_000)),
    // Google — Gemini 2.x / 1.5.
    ("gemini-2.5-pro", pk(1.25, 10.00, 1_000_000)),
    ("gemini-2.5-flash", pk(0.075, 0.30, 1_000_000)),
    ("gemini-2.0-flash", pk(0.10, 0.40, 1_000_000)),
    ("gemini-2.0-flash-lite", pk(0.075, 0.30, 1_000_000)),
    ("gemini-1.5-pro", pk(1.25, 5.00, 2_000_000)),
    ("gemini-1.5-flash", pk(0.075, 0.30, 1_000_000)),
    // xAI — Grok family.
    ("grok-4", pk(3.00, 15.00, 256_000)),
    ("grok-3", pk(3.00, 15.00, 131_072)),
    ("grok-3-mini", pk(0.30, 0.50, 131_072)),
    // DeepSeek — hosted API SKUs (not the local variants).
    ("deepseek-chat", pk(0.27, 1.10, 128_000)),
    ("deepseek-reasoner", pk(0.55, 2.19, 128_000)),
    // Mistral — hosted flagship SKUs.
    ("mistral-large", pk(2.00, 6.00, 128_000)),
    ("mistral-medium", pk(2.70, 8.10, 32_000)),
];

/// `const fn` helper so the table literal reads at a glance.
const fn pk(input_per_mtok: f64, output_per_mtok: f64, max_input_tokens: u64) -> ModelPrice {
    ModelPrice {
        input_per_mtok,
        output_per_mtok,
        max_input_tokens,
    }
}

/// Runtime-facing wrapper: same shape as the upstream `PriceTable`
/// so future TOML-overlay work (`agtop --prices path.toml`, or
/// `[agtop.prices]` in the rimeterm config) drops in without touching
/// callers.
#[derive(Clone, Debug)]
pub struct PriceTable {
    /// Sorted by key so binary-search / linear iteration is
    /// deterministic. Small enough (<100 entries) that hashing would
    /// be pure overhead.
    entries: Vec<(&'static str, ModelPrice)>,
}

impl PriceTable {
    pub fn builtin() -> Self {
        let mut entries: Vec<(&'static str, ModelPrice)> = CURATED.to_vec();
        entries.sort_by_key(|(k, _)| *k);
        Self { entries }
    }

    /// Exact-then-suffix-tolerant lookup. Walks up to four `-`-separated
    /// suffixes off the right so a dated revision like
    /// `claude-sonnet-4-7-20260101` resolves to `claude-sonnet-4-7`,
    /// then `claude-sonnet-4`, then `claude-sonnet`, etc. Capped so a
    /// custom entry keyed on `claude` doesn't silently shadow every
    /// Claude SKU ever released.
    pub fn lookup(&self, model: &str) -> Option<ModelPrice> {
        if model.is_empty() {
            return None;
        }
        // Fast path — exact hit.
        if let Some(hit) = self.entries.iter().find(|(k, _)| *k == model) {
            return Some(hit.1);
        }
        // Suffix strip. Up to four trims; each trim halves the search
        // space so this is still cheap on the widest key.
        let mut s = model;
        for _ in 0..4 {
            let Some(i) = s.rfind('-') else { break };
            s = &s[..i];
            if let Some(hit) = self.entries.iter().find(|(k, _)| *k == s) {
                return Some(hit.1);
            }
        }
        None
    }

    /// Classify the model. Local always wins over exact match (a
    /// model literal like `ollama/llama3` is local even if a user
    /// pointed their `[models."ollama/llama3"]` entry at a real
    /// price — the `local` semantics are load-bearing for the
    /// display).
    pub fn cost_basis(&self, model: &str) -> CostBasis {
        if is_local_model(model) {
            return CostBasis::Local;
        }
        if model.is_empty() {
            return CostBasis::Unknown;
        }
        if self.lookup(model).is_some() {
            CostBasis::Api
        } else {
            CostBasis::Unknown
        }
    }

    /// Context-window limit for a model, in tokens. Prefer the table
    /// entry, then heuristics on the model id (`-1m`, `-1000k`, `-2m`
    /// long-context variants). Falls back to 200K — a conservative
    /// pick that matches Claude-family standard windows.
    pub fn context_limit(&self, model: &str) -> u64 {
        let lower = model.to_ascii_lowercase();
        if lower.contains("-1m") || lower.contains("1m-context") || lower.contains("-1000k") {
            return 1_000_000;
        }
        if lower.contains("-2m") {
            return 2_000_000;
        }
        self.lookup(model)
            .map(|p| p.max_input_tokens)
            .unwrap_or(200_000)
    }

    /// Total USD cost for a session bill, honouring Anthropic
    /// prompt-cache rates:
    /// - standard input at 1×
    /// - cache-read at 0.10×
    /// - cache-write at 1.25×
    /// - output at the model's output rate
    ///
    /// `in_tok` is the FULL input bucket (raw + cache_read +
    /// cache_write); the formula subtracts the cached portion before
    /// applying the standard rate so cache hits aren't overbilled.
    pub fn cost_with_cache(
        &self,
        model: &str,
        in_tok: u64,
        out_tok: u64,
        cache_read: u64,
        cache_write: u64,
    ) -> f64 {
        if is_local_model(model) {
            return 0.0;
        }
        let Some(p) = self.lookup(model) else {
            return 0.0;
        };
        let cached = cache_read.saturating_add(cache_write);
        let raw_input = in_tok.saturating_sub(cached);
        const M: f64 = 1_000_000.0;
        (raw_input as f64 / M) * p.input_per_mtok
            + (cache_read as f64 / M) * p.input_per_mtok * 0.10
            + (cache_write as f64 / M) * p.input_per_mtok * 1.25
            + (out_tok as f64 / M) * p.output_per_mtok
    }

    /// Number of entries in the table — surfaced by tests to catch a
    /// regression that would silently ship an empty table.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

impl Default for PriceTable {
    fn default() -> Self {
        Self::builtin()
    }
}

/// Format a USD cost for a table cell. Same buckets upstream `agtop`
/// uses so the visual rhythm carries over: `$0.04`, `$1.23`, `$42.1`,
/// `$1.2k`, `$1.2M`.
pub fn format_cost(usd: f64) -> String {
    if usd <= 0.0 {
        return "—".into();
    }
    if usd < 0.01 {
        return "<$0.01".into();
    }
    if usd < 10.0 {
        return format!("${usd:.2}");
    }
    if usd < 1000.0 {
        return format!("${usd:.1}");
    }
    if usd < 1_000_000.0 {
        return format!("${:.1}k", usd / 1000.0);
    }
    format!("${:.1}M", usd / 1_000_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_entries() {
        let t = PriceTable::builtin();
        // Sanity check: catches an accidentally-emptied CURATED slice
        // before it lands on main.
        assert!(t.len() >= 30, "curated table shrunk to {} entries", t.len());
    }

    #[test]
    fn exact_lookup_hits() {
        let t = PriceTable::builtin();
        let p = t.lookup("claude-sonnet-4-7").expect("known model");
        assert_eq!(p.input_per_mtok, 3.0);
        assert_eq!(p.output_per_mtok, 15.0);
        assert_eq!(p.max_input_tokens, 200_000);
    }

    #[test]
    fn suffix_tolerant_lookup_strips_date_revs() {
        let t = PriceTable::builtin();
        let p = t
            .lookup("claude-sonnet-4-7-20260101")
            .expect("dated revision");
        assert_eq!(p.input_per_mtok, 3.0);
    }

    #[test]
    fn suffix_lookup_capped_at_four_hops() {
        let t = PriceTable::builtin();
        // Four hops off the right of `a-b-c-d-e-f-g` should walk
        // down to `a-b-c` (kept as a-b-c-d-e-f-g → a-b-c-d-e-f →
        // …-e → …-d → …-c), missing the `claude` root — good.
        assert!(t.lookup("a-b-c-d-e-f-g").is_none());
    }

    #[test]
    fn cost_math_per_million() {
        let t = PriceTable::builtin();
        // 1M input at $3/Mtok = $3.
        let c = t.cost_with_cache("claude-sonnet-4-7", 1_000_000, 0, 0, 0);
        assert!((c - 3.0).abs() < 1e-6);
        // 1M output at $15/Mtok = $15.
        let c = t.cost_with_cache("claude-sonnet-4-7", 0, 1_000_000, 0, 0);
        assert!((c - 15.0).abs() < 1e-6);
    }

    #[test]
    fn cache_aware_pricing_matches_anthropic_rates() {
        let t = PriceTable::builtin();
        // 500K standard input + 500K cache_read on claude-sonnet-4-7:
        //   raw   = 500_000 → $1.50
        //   cache = 500_000 → $0.15 (0.10× input rate)
        //   total = $1.65
        let c = t.cost_with_cache("claude-sonnet-4-7", 1_000_000, 0, 500_000, 0);
        assert!((c - 1.65).abs() < 1e-6, "got {c}");
    }

    #[test]
    fn cache_write_at_125_percent() {
        let t = PriceTable::builtin();
        // 1M cache_write on claude-sonnet-4-7:
        //   raw   = 0
        //   write = 1M × $3 × 1.25 = $3.75
        let c = t.cost_with_cache("claude-sonnet-4-7", 1_000_000, 0, 0, 1_000_000);
        assert!((c - 3.75).abs() < 1e-6, "got {c}");
    }

    #[test]
    fn unknown_model_is_zero_cost() {
        let t = PriceTable::builtin();
        assert_eq!(t.cost_with_cache("totally-fake-model", 999, 999, 0, 0), 0.0);
    }

    #[test]
    fn local_model_short_circuits_to_zero() {
        let t = PriceTable::builtin();
        assert!(is_local_model("ollama/llama3"));
        assert!(is_local_model("Ollama:codellama"));
        assert!(is_local_model("vllm/mistral-7b"));
        assert_eq!(
            t.cost_with_cache("ollama/llama3", 5_000_000, 5_000_000, 0, 0),
            0.0
        );
        assert_eq!(t.cost_basis("ollama/llama3"), CostBasis::Local);
    }

    #[test]
    fn cost_basis_classifies_three_buckets() {
        let t = PriceTable::builtin();
        assert_eq!(t.cost_basis("claude-sonnet-4-7"), CostBasis::Api);
        assert_eq!(t.cost_basis("ollama/llama3"), CostBasis::Local);
        assert_eq!(t.cost_basis("totally-made-up"), CostBasis::Unknown);
        assert_eq!(t.cost_basis(""), CostBasis::Unknown);
    }

    #[test]
    fn context_limit_promotes_long_context_variants() {
        let t = PriceTable::builtin();
        assert_eq!(t.context_limit("claude-sonnet-4-7"), 200_000);
        assert_eq!(t.context_limit("claude-sonnet-4-7-1m"), 1_000_000);
        assert_eq!(t.context_limit("claude-sonnet-4-7-1000k"), 1_000_000);
        assert_eq!(t.context_limit("some-2m-context"), 2_000_000);
        // Fallback when nothing matches.
        assert_eq!(t.context_limit("totally-unknown-model"), 200_000);
    }

    #[test]
    fn format_cost_buckets() {
        assert_eq!(format_cost(0.0), "—");
        assert_eq!(format_cost(0.001), "<$0.01");
        assert_eq!(format_cost(0.04), "$0.04");
        assert_eq!(format_cost(1.23), "$1.23");
        assert_eq!(format_cost(42.1), "$42.1");
        assert_eq!(format_cost(1234.0), "$1.2k");
        assert_eq!(format_cost(1_500_000.0), "$1.5M");
    }
}
