//! Owned data model for the Native [`ModelsPane`].
//!
//! Follows the [`crate::agtop_model`] shape 1:1 so the two left-bottom
//! tabs stay conceptually parallel: a `Snapshot` holds every flattened
//! row (one row = one model), a `ModelView` is a filtered + sorted
//! projection recomputed per render, and a `Request` / `Response` pair
//! crosses the worker boundary with monotonic generations so late
//! replies never clobber a fresher one.
//!
//! [`ModelsPane`]: crate::models_pane::ModelsPane

use rimeterm_models::{Model, Provider, ProvidersMap};

/// Sort key for the models table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SortKey {
    Provider,
    Name,
    Context,
    InputCost,
    OutputCost,
    Release,
}

/// Ascending / descending toggle for [`SortKey`]. Same-key second press
/// flips the order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SortOrder {
    Ascending,
    Descending,
}

impl SortOrder {
    pub fn flip(self) -> Self {
        match self {
            SortOrder::Ascending => SortOrder::Descending,
            SortOrder::Descending => SortOrder::Ascending,
        }
    }
}

/// Flat row shown in the models table — one per (provider, model) pair.
/// Kept as owned strings so the row is independent of the fetched
/// `ProvidersMap`; a re-fetch swaps rows in without invalidating
/// borrows the pane still holds.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelRow {
    pub provider_id: String,
    pub provider_name: String,
    pub model_id: String,
    pub model_name: String,
    pub context: Option<u64>,
    pub output_limit: Option<u64>,
    pub input_cost: Option<f64>,
    pub output_cost: Option<f64>,
    pub reasoning: bool,
    pub tool_call: bool,
    pub attachment: bool,
    pub family: Option<String>,
    pub release_date: Option<String>,
    pub last_updated: Option<String>,
    pub is_text: bool,
    pub open_weights: bool,
}

impl ModelRow {
    fn from_pair(provider: &Provider, model: &Model) -> Self {
        Self {
            provider_id: provider.id.clone(),
            provider_name: provider.name.clone(),
            model_id: model.id.clone(),
            model_name: model.name.clone(),
            context: model.context_tokens(),
            output_limit: model.output_tokens(),
            input_cost: model.input_cost(),
            output_cost: model.output_cost(),
            reasoning: model.reasoning,
            tool_call: model.tool_call,
            attachment: model.attachment,
            family: model.family.clone(),
            release_date: model.release_date.clone(),
            last_updated: model.last_updated.clone(),
            is_text: model.is_text_model(),
            open_weights: model.open_weights,
        }
    }
}

/// Complete snapshot handed from worker → pane.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub rows: Vec<ModelRow>,
    /// Provider count in the source map — used in the title bar so
    /// the user sees `models · 87 providers · 4021 models`.
    pub provider_count: usize,
    /// Human-readable error from the last attempted fetch, if any.
    /// Kept alongside `rows` (not replacing it) so a failed refresh
    /// leaves the previously-fetched data on screen.
    pub last_error: Option<String>,
}

impl Snapshot {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Flatten a fetched `ProvidersMap` into a table of `ModelRow`s.
    /// Non-text models (image-gen, embeddings) are skipped — they
    /// don't have a meaningful "$/1M tokens" price and clutter the
    /// list.
    pub fn from_providers(map: &ProvidersMap) -> Self {
        let mut rows = Vec::with_capacity(map.len() * 8);
        for provider in map.values() {
            for model in provider.models.values() {
                if !model.is_text_model() {
                    continue;
                }
                rows.push(ModelRow::from_pair(provider, model));
            }
        }
        Self {
            rows,
            provider_count: map.len(),
            last_error: None,
        }
    }
}

/// Filtered + sorted derivation of [`Snapshot::rows`]. Recomputed each
/// render so cursor movement / sort flips are cheap; the full pass runs
/// in a few ms for ~4k rows.
#[derive(Clone, Debug)]
pub struct ModelView {
    pub rows: Vec<ModelRow>,
    pub filter: Option<String>,
    pub sort_key: SortKey,
    pub sort_order: SortOrder,
}

impl ModelView {
    pub fn from_snapshot(
        snapshot: &Snapshot,
        sort_key: SortKey,
        sort_order: SortOrder,
        filter: Option<&str>,
    ) -> Self {
        let filt = filter.map(|s| s.to_ascii_lowercase());
        let mut rows: Vec<ModelRow> = snapshot
            .rows
            .iter()
            .filter(|r| match &filt {
                None => true,
                Some(f) if f.is_empty() => true,
                Some(f) => row_matches(r, f),
            })
            .cloned()
            .collect();
        sort_rows(&mut rows, sort_key, sort_order);
        Self {
            rows,
            filter: filter.map(str::to_owned),
            sort_key,
            sort_order,
        }
    }
}

/// Case-insensitive substring match against every field that could
/// plausibly identify a row: provider id/name, model id/name/family.
fn row_matches(row: &ModelRow, needle_lower: &str) -> bool {
    let hit = |s: &str| s.to_ascii_lowercase().contains(needle_lower);
    hit(&row.provider_id)
        || hit(&row.provider_name)
        || hit(&row.model_id)
        || hit(&row.model_name)
        || row.family.as_deref().is_some_and(hit)
}

fn sort_rows(rows: &mut [ModelRow], key: SortKey, order: SortOrder) {
    // Stable so tie-breakers land in a predictable "insertion order"
    // shape. Missing values are pushed to the end for descending
    // numeric sorts and to the start for ascending ones — same
    // convention as upstream.
    rows.sort_by(|a, b| {
        let cmp = match key {
            SortKey::Provider => a
                .provider_name
                .to_ascii_lowercase()
                .cmp(&b.provider_name.to_ascii_lowercase())
                .then_with(|| {
                    a.model_name
                        .to_ascii_lowercase()
                        .cmp(&b.model_name.to_ascii_lowercase())
                }),
            SortKey::Name => a
                .model_name
                .to_ascii_lowercase()
                .cmp(&b.model_name.to_ascii_lowercase()),
            SortKey::Context => cmp_option_u64(a.context, b.context),
            SortKey::InputCost => cmp_option_f64(a.input_cost, b.input_cost),
            SortKey::OutputCost => cmp_option_f64(a.output_cost, b.output_cost),
            SortKey::Release => {
                cmp_option_str(a.release_date.as_deref(), b.release_date.as_deref())
            }
        };
        match order {
            SortOrder::Ascending => cmp,
            SortOrder::Descending => cmp.reverse(),
        }
    });
}

fn cmp_option_u64(a: Option<u64>, b: Option<u64>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.cmp(&b),
        // `None` always sorts LAST in ascending; the outer `.reverse()`
        // for descending mode flips it back to LAST too, which is what
        // upstream does. Matches "missing values shouldn't dominate the
        // top row for either sort direction".
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

fn cmp_option_f64(a: Option<f64>, b: Option<f64>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

fn cmp_option_str(a: Option<&str>, b: Option<&str>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.cmp(b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// Worker request. Fetch requests carry a generation so late replies
/// land harmlessly when a fresh request has already superseded them.
#[derive(Clone, Debug)]
pub enum ModelsRequest {
    Fetch { generation: u64 },
}

/// Worker response, one-to-one with a request.
#[derive(Clone, Debug)]
pub enum ModelsResponse {
    Fetch {
        generation: u64,
        /// `Ok` → snapshot ready. `Err` → human-readable error message
        /// the pane can drop into `Snapshot::last_error` while keeping
        /// any previously-fetched rows on screen.
        result: Result<Snapshot, String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimeterm_models::{Cost, Limits, Modalities};
    use std::collections::HashMap;

    fn sample() -> Snapshot {
        let mut providers: ProvidersMap = HashMap::new();
        let mut openai_models: HashMap<String, Model> = HashMap::new();
        openai_models.insert(
            "gpt-4o".into(),
            Model {
                id: "gpt-4o".into(),
                name: "GPT-4o".into(),
                family: None,
                reasoning: false,
                tool_call: true,
                attachment: true,
                modalities: None,
                cost: Some(Cost {
                    input: Some(2.5),
                    output: Some(10.0),
                    cache_read: None,
                    cache_write: None,
                }),
                limit: Some(Limits {
                    context: Some(128_000),
                    input: None,
                    output: Some(16_384),
                }),
                release_date: Some("2024-05-13".into()),
                last_updated: None,
                open_weights: false,
            },
        );
        providers.insert(
            "openai".into(),
            Provider {
                id: "openai".into(),
                name: "OpenAI".into(),
                doc: None,
                api: None,
                env: vec![],
                models: openai_models,
            },
        );
        let mut anthropic_models: HashMap<String, Model> = HashMap::new();
        anthropic_models.insert(
            "claude-opus-4".into(),
            Model {
                id: "claude-opus-4".into(),
                name: "Claude Opus 4".into(),
                family: None,
                reasoning: true,
                tool_call: true,
                attachment: true,
                modalities: Some(Modalities {
                    input: vec!["text".into()],
                    output: vec!["text".into()],
                }),
                cost: Some(Cost {
                    input: Some(15.0),
                    output: Some(75.0),
                    cache_read: None,
                    cache_write: None,
                }),
                limit: Some(Limits {
                    context: Some(200_000),
                    input: None,
                    output: Some(32_000),
                }),
                release_date: Some("2025-05-01".into()),
                last_updated: None,
                open_weights: false,
            },
        );
        providers.insert(
            "anthropic".into(),
            Provider {
                id: "anthropic".into(),
                name: "Anthropic".into(),
                doc: None,
                api: None,
                env: vec![],
                models: anthropic_models,
            },
        );
        Snapshot::from_providers(&providers)
    }

    #[test]
    fn snapshot_flattens_all_text_models() {
        let s = sample();
        assert_eq!(s.rows.len(), 2);
        assert_eq!(s.provider_count, 2);
    }

    #[test]
    fn filter_matches_case_insensitive_across_fields() {
        let s = sample();
        let v = ModelView::from_snapshot(&s, SortKey::Provider, SortOrder::Ascending, Some("OPUS"));
        assert_eq!(v.rows.len(), 1);
        assert_eq!(v.rows[0].model_id, "claude-opus-4");

        let v =
            ModelView::from_snapshot(&s, SortKey::Provider, SortOrder::Ascending, Some("openai"));
        assert_eq!(v.rows.len(), 1);
        assert_eq!(v.rows[0].provider_id, "openai");
    }

    #[test]
    fn sort_by_provider_ascending_alphabetizes() {
        let s = sample();
        let v = ModelView::from_snapshot(&s, SortKey::Provider, SortOrder::Ascending, None);
        assert_eq!(v.rows[0].provider_id, "anthropic");
        assert_eq!(v.rows[1].provider_id, "openai");
    }

    #[test]
    fn sort_by_input_cost_ascending_puts_cheaper_first() {
        let s = sample();
        let v = ModelView::from_snapshot(&s, SortKey::InputCost, SortOrder::Ascending, None);
        assert_eq!(v.rows[0].model_id, "gpt-4o", "$2.5 < $15");
    }

    #[test]
    fn sort_missing_values_land_after_present_ones() {
        let mut s = sample();
        s.rows.push(ModelRow {
            provider_id: "x".into(),
            provider_name: "X".into(),
            model_id: "unknown".into(),
            model_name: "Unknown".into(),
            context: None,
            output_limit: None,
            input_cost: None,
            output_cost: None,
            reasoning: false,
            tool_call: false,
            attachment: false,
            family: None,
            release_date: None,
            last_updated: None,
            is_text: true,
            open_weights: false,
        });
        let v = ModelView::from_snapshot(&s, SortKey::InputCost, SortOrder::Ascending, None);
        // Row with cost=None must land LAST regardless of order.
        assert_eq!(v.rows.last().unwrap().model_id, "unknown");
    }
}
