//! Models-tab state projection over an owned models.dev snapshot.

use std::cmp::Ordering;

use rimeterm_models::{Model, Provider, ProvidersMap};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SortKey {
    Name,
    #[default]
    Release,
    Cost,
    Context,
}

impl SortKey {
    pub fn next(self) -> Self {
        match self {
            Self::Name => Self::Release,
            Self::Release => Self::Cost,
            Self::Cost => Self::Context,
            Self::Context => Self::Name,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Release => "date",
            Self::Cost => "cost",
            Self::Context => "ctx",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SortOrder {
    Ascending,
    Descending,
}

impl SortOrder {
    pub fn flip(self) -> Self {
        match self {
            Self::Ascending => Self::Descending,
            Self::Descending => Self::Ascending,
        }
    }

    pub fn arrow(self) -> &'static str {
        match self {
            Self::Ascending => "↑",
            Self::Descending => "↓",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Filters {
    pub reasoning: bool,
    pub tools: bool,
    pub open_weights: bool,
    pub free: bool,
}

impl Filters {
    pub fn labels(self) -> Vec<&'static str> {
        let mut labels = Vec::with_capacity(4);
        if self.reasoning {
            labels.push("R");
        }
        if self.tools {
            labels.push("T");
        }
        if self.open_weights {
            labels.push("Open");
        }
        if self.free {
            labels.push("Free");
        }
        labels
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub enum ProviderCategory {
    All,
    Origin,
    Cloud,
    #[default]
    Inference,
    Gateway,
    Tool,
}

impl ProviderCategory {
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Origin,
            Self::Origin => Self::Cloud,
            Self::Cloud => Self::Inference,
            Self::Inference => Self::Gateway,
            Self::Gateway => Self::Tool,
            Self::Tool => Self::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Origin => "Origin",
            Self::Cloud => "Cloud Platform",
            Self::Inference => "Inference",
            Self::Gateway => "Gateway",
            Self::Tool => "Dev Tool",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            Self::All => "Cat",
            Self::Origin => "Orig",
            Self::Cloud => "Cloud",
            Self::Inference => "Infra",
            Self::Gateway => "Gate",
            Self::Tool => "Tool",
        }
    }

    pub fn initial(self) -> char {
        match self {
            Self::All => 'A',
            Self::Origin => 'O',
            Self::Cloud => 'C',
            Self::Inference => 'I',
            Self::Gateway => 'G',
            Self::Tool => 'T',
        }
    }
}

pub fn provider_category(id: &str) -> ProviderCategory {
    match id {
        "anthropic"
        | "openai"
        | "google"
        | "deepseek"
        | "mistral"
        | "cohere"
        | "xai"
        | "llama"
        | "inception"
        | "upstage"
        | "zhipuai"
        | "minimax"
        | "moonshotai"
        | "xiaomi"
        | "alibaba"
        | "perplexity"
        | "bailing"
        | "nova"
        | "alibaba-cn"
        | "minimax-cn"
        | "minimax-coding-plan"
        | "moonshotai-cn"
        | "zai-coding-plan" => ProviderCategory::Origin,
        "amazon-bedrock"
        | "azure"
        | "azure-cognitive-services"
        | "google-vertex"
        | "google-vertex-anthropic"
        | "nvidia"
        | "ovhcloud"
        | "scaleway"
        | "vultr"
        | "sap-ai-core"
        | "cloudflare-workers-ai" => ProviderCategory::Cloud,
        "openrouter"
        | "helicone"
        | "requesty"
        | "302ai"
        | "aihubmix"
        | "cloudflare-ai-gateway"
        | "fastrouter"
        | "zenmux"
        | "submodel"
        | "vercel"
        | "nano-gpt"
        | "poe" => ProviderCategory::Gateway,
        "github-copilot" | "github-models" | "gitlab" | "v0" | "huggingface" | "lmstudio"
        | "ollama-cloud" | "wandb" | "morph" | "opencode" | "firmware" | "kimi-for-coding"
        | "modelscope" | "abacus" | "iflowcn" | "zai" => ProviderCategory::Tool,
        _ => ProviderCategory::Inference,
    }
}

#[derive(Clone, Debug)]
pub struct ProviderEntry {
    pub id: String,
    pub provider: Provider,
}

#[derive(Clone, Debug)]
pub struct ModelEntry {
    pub provider_id: String,
    pub provider_name: String,
    pub model: Model,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub providers: Vec<ProviderEntry>,
    pub provider_count: usize,
    pub model_count: usize,
    pub last_error: Option<String>,
}

impl Snapshot {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn from_providers(map: &ProvidersMap) -> Self {
        let mut providers: Vec<ProviderEntry> = map
            .iter()
            .map(|(id, provider)| ProviderEntry {
                id: id.clone(),
                provider: provider.clone(),
            })
            .collect();
        providers.sort_by(|a, b| a.id.cmp(&b.id));
        let model_count = providers
            .iter()
            .map(|entry| entry.provider.models.len())
            .sum();
        Self {
            provider_count: providers.len(),
            model_count,
            providers,
            last_error: None,
        }
    }

    pub fn provider(&self, id: &str) -> Option<&ProviderEntry> {
        self.providers.iter().find(|entry| entry.id == id)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderListItem {
    All { count: usize },
    CategoryHeader(ProviderCategory),
    Provider { id: String, count: usize },
}

#[derive(Clone, Debug)]
pub struct CatalogProjection {
    pub provider_items: Vec<ProviderListItem>,
    pub models: Vec<ModelEntry>,
}

impl CatalogProjection {
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        snapshot: &Snapshot,
        selected_provider: Option<&str>,
        filters: Filters,
        category_filter: ProviderCategory,
        group_by_category: bool,
        query: &str,
        sort_key: SortKey,
        sort_order: SortOrder,
    ) -> Self {
        let query = query.trim().to_ascii_lowercase();
        let matching = |provider_id: &str, model: &Model| {
            passes_filters(model, filters)
                && (query.is_empty()
                    || provider_id.to_ascii_lowercase().contains(&query)
                    || model.id.to_ascii_lowercase().contains(&query)
                    || model.name.to_ascii_lowercase().contains(&query)
                    || model
                        .family
                        .as_deref()
                        .is_some_and(|family| family.to_ascii_lowercase().contains(&query)))
        };

        let mut provider_rows: Vec<(ProviderCategory, &ProviderEntry, usize)> = snapshot
            .providers
            .iter()
            .filter_map(|entry| {
                let category = provider_category(&entry.id);
                if category_filter != ProviderCategory::All && category != category_filter {
                    return None;
                }
                let count = entry
                    .provider
                    .models
                    .values()
                    .filter(|model| matching(&entry.id, model))
                    .count();
                (count > 0).then_some((category, entry, count))
            })
            .collect();
        provider_rows.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.id.cmp(&b.1.id)));

        let all_count = provider_rows.iter().map(|(_, _, count)| count).sum();
        let mut provider_items = vec![ProviderListItem::All { count: all_count }];
        let mut previous_category = None;
        for (category, entry, count) in provider_rows {
            if group_by_category && previous_category != Some(category) {
                provider_items.push(ProviderListItem::CategoryHeader(category));
                previous_category = Some(category);
            }
            provider_items.push(ProviderListItem::Provider {
                id: entry.id.clone(),
                count,
            });
        }

        let active_provider = selected_provider.filter(|id| {
            provider_items.iter().any(
                |item| matches!(item, ProviderListItem::Provider { id: current, .. } if current == id),
            )
        });
        let mut models = Vec::with_capacity(all_count);
        for entry in &snapshot.providers {
            if active_provider.is_some_and(|id| id != entry.id) {
                continue;
            }
            if category_filter != ProviderCategory::All
                && provider_category(&entry.id) != category_filter
            {
                continue;
            }
            models.extend(
                entry
                    .provider
                    .models
                    .values()
                    .filter(|model| matching(&entry.id, model))
                    .cloned()
                    .map(|model| ModelEntry {
                        provider_id: entry.id.clone(),
                        provider_name: entry.provider.name.clone(),
                        model,
                    }),
            );
        }
        sort_models(&mut models, sort_key, sort_order);
        Self {
            provider_items,
            models,
        }
    }

    pub fn selectable_provider_index(&self, from: usize, forward: bool) -> usize {
        if self.provider_items.is_empty() {
            return 0;
        }
        let mut index = from.min(self.provider_items.len() - 1);
        while matches!(
            self.provider_items.get(index),
            Some(ProviderListItem::CategoryHeader(_))
        ) {
            if forward && index + 1 < self.provider_items.len() {
                index += 1;
            } else if index > 0 {
                index -= 1;
            } else {
                break;
            }
        }
        index
    }
}

fn passes_filters(model: &Model, filters: Filters) -> bool {
    (!filters.reasoning || model.reasoning)
        && (!filters.tools || model.tool_call)
        && (!filters.open_weights || model.open_weights)
        && (!filters.free || model.is_free())
}

fn sort_models(models: &mut [ModelEntry], key: SortKey, order: SortOrder) {
    models.sort_by(|a, b| {
        let cmp = match key {
            SortKey::Name => a
                .model
                .name
                .to_ascii_lowercase()
                .cmp(&b.model.name.to_ascii_lowercase()),
            SortKey::Release => option_cmp(
                a.model.release_date.as_deref(),
                b.model.release_date.as_deref(),
            ),
            SortKey::Cost => option_f64_cmp(a.model.input_cost(), b.model.input_cost()),
            SortKey::Context => option_cmp(a.model.context_tokens(), b.model.context_tokens()),
        }
        .then_with(|| a.provider_id.cmp(&b.provider_id))
        .then_with(|| a.model.id.cmp(&b.model.id));
        match order {
            SortOrder::Ascending => cmp,
            SortOrder::Descending => cmp.reverse(),
        }
    });
    // Missing values belong at the bottom in both directions.
    models.sort_by_key(|entry| match key {
        SortKey::Release => entry.model.release_date.is_none(),
        SortKey::Cost => entry.model.input_cost().is_none(),
        SortKey::Context => entry.model.context_tokens().is_none(),
        SortKey::Name => false,
    });
}

fn option_cmp<T: Ord>(a: Option<T>, b: Option<T>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn option_f64_cmp(a: Option<f64>, b: Option<f64>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

#[derive(Clone, Debug)]
pub enum ModelsRequest {
    Fetch { generation: u64 },
}

#[derive(Clone, Debug)]
pub enum ModelsResponse {
    Fetch {
        generation: u64,
        result: Result<Snapshot, String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rich_snapshot() -> Snapshot {
        let json = r#"{
          "openai":{"id":"openai","name":"OpenAI","models":{
            "gpt":{"id":"gpt","name":"GPT","tool_call":true,"cost":{"input":2,"output":8},"limit":{"context":128000}}
          }},
          "deepinfra":{"id":"deepinfra","name":"DeepInfra","models":{
            "reasoner":{"id":"reasoner","name":"Reasoner","reasoning":true,"open_weights":true,"cost":{"input":0,"output":0},"limit":{"context":200000}}
          }}
        }"#;
        Snapshot::from_providers(&serde_json::from_str(json).unwrap())
    }

    #[test]
    fn projection_filters_and_counts_across_providers() {
        let snapshot = rich_snapshot();
        let projection = CatalogProjection::build(
            &snapshot,
            None,
            Filters {
                reasoning: true,
                ..Filters::default()
            },
            ProviderCategory::All,
            false,
            "",
            SortKey::Release,
            SortOrder::Descending,
        );
        assert_eq!(projection.models.len(), 1);
        assert_eq!(projection.models[0].model.id, "reasoner");
        assert!(
            projection
                .provider_items
                .iter()
                .any(|item| matches!(item, ProviderListItem::Provider { count: 1, .. }))
        );
    }

    #[test]
    fn grouped_provider_headers_are_not_selectable() {
        let snapshot = rich_snapshot();
        let projection = CatalogProjection::build(
            &snapshot,
            None,
            Filters::default(),
            ProviderCategory::All,
            true,
            "",
            SortKey::Release,
            SortOrder::Descending,
        );
        let header = projection
            .provider_items
            .iter()
            .position(|item| matches!(item, ProviderListItem::CategoryHeader(_)))
            .unwrap();
        assert_ne!(projection.selectable_provider_index(header, true), header);
    }

    #[test]
    fn duplicate_model_ids_remain_distinct_by_provider() {
        let json = r#"{
          "a":{"id":"a","name":"A","models":{"shared":{"id":"shared","name":"A Shared"}}},
          "b":{"id":"b","name":"B","models":{"shared":{"id":"shared","name":"B Shared"}}}
        }"#;
        let snapshot = Snapshot::from_providers(&serde_json::from_str(json).unwrap());
        let projection = CatalogProjection::build(
            &snapshot,
            None,
            Filters::default(),
            ProviderCategory::All,
            false,
            "",
            SortKey::Name,
            SortOrder::Ascending,
        );
        let identities = projection
            .models
            .iter()
            .map(|entry| (entry.provider_id.as_str(), entry.model.id.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(identities, vec![("a", "shared"), ("b", "shared")]);
    }
    #[test]
    fn search_matches_provider_and_model_fields() {
        let snapshot = rich_snapshot();
        let projection = CatalogProjection::build(
            &snapshot,
            None,
            Filters::default(),
            ProviderCategory::All,
            false,
            "OPENAI",
            SortKey::Name,
            SortOrder::Ascending,
        );
        assert_eq!(projection.models.len(), 1);
        assert_eq!(projection.models[0].provider_id, "openai");
    }
}
