//! Owned data model for `https://models.dev/api.json`.
//!
//! Port of upstream `modelsdev::data` (MIT, v0.14.0). Trimmed to only
//! the fields the ModelsPane actually renders — dropped `ReasoningOption`,
//! `CostTier`, `TierSpec`, `Modalities` maps beyond `input`/`output`, the
//! open-weights toggle, and the tui-piechart adornments. Re-syncing with
//! upstream stays trivial: field names are unchanged and `serde(default)`
//! everywhere absorbs whatever the API adds next.

use serde::Deserialize;
use std::collections::HashMap;

/// Top-level API shape: `{ "<provider_id>": Provider, … }`.
pub type ProvidersMap = HashMap<String, Provider>;

/// One provider (openai, anthropic, xai, groq, …) plus its models.
#[derive(Debug, Clone, Deserialize)]
pub struct Provider {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub doc: Option<String>,
    #[serde(default)]
    pub api: Option<String>,
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub models: HashMap<String, Model>,
}

/// One model entry. Only fields the pane currently reads.
#[derive(Debug, Clone, Deserialize)]
pub struct Model {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub tool_call: bool,
    #[serde(default)]
    pub attachment: bool,
    #[serde(default)]
    pub modalities: Option<Modalities>,
    #[serde(default)]
    pub cost: Option<Cost>,
    #[serde(default)]
    pub limit: Option<Limits>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub last_updated: Option<String>,
    #[serde(default)]
    pub open_weights: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Cost {
    #[serde(default)]
    pub input: Option<f64>,
    #[serde(default)]
    pub output: Option<f64>,
    #[serde(default)]
    pub cache_read: Option<f64>,
    #[serde(default)]
    pub cache_write: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Limits {
    #[serde(default)]
    pub context: Option<u64>,
    #[serde(default)]
    pub input: Option<u64>,
    #[serde(default)]
    pub output: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Modalities {
    #[serde(default)]
    pub input: Vec<String>,
    #[serde(default)]
    pub output: Vec<String>,
}

impl Model {
    /// True when the model emits text (or when no modalities are declared —
    /// upstream default). Image-only / embedding-only models return false.
    pub fn is_text_model(&self) -> bool {
        match &self.modalities {
            Some(m) => m.output.iter().any(|o| o == "text"),
            None => true,
        }
    }

    pub fn context_tokens(&self) -> Option<u64> {
        self.limit.as_ref().and_then(|l| l.context)
    }

    pub fn output_tokens(&self) -> Option<u64> {
        self.limit.as_ref().and_then(|l| l.output)
    }

    pub fn input_cost(&self) -> Option<f64> {
        self.cost.as_ref().and_then(|c| c.input)
    }

    pub fn output_cost(&self) -> Option<f64> {
        self.cost.as_ref().and_then(|c| c.output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_minimal_provider() {
        // Real shape from a stripped models.dev sample: provider + one model.
        let json = r#"{
            "openai": {
                "id": "openai",
                "name": "OpenAI",
                "env": ["OPENAI_API_KEY"],
                "models": {
                    "gpt-4o": {
                        "id": "gpt-4o",
                        "name": "GPT-4o",
                        "reasoning": false,
                        "tool_call": true,
                        "attachment": true,
                        "limit": {"context": 128000, "output": 16384},
                        "cost": {"input": 2.5, "output": 10.0}
                    }
                }
            }
        }"#;
        let map: ProvidersMap = serde_json::from_str(json).expect("parse");
        assert_eq!(map.len(), 1);
        let openai = &map["openai"];
        assert_eq!(openai.name, "OpenAI");
        assert_eq!(openai.models.len(), 1);
        let m = &openai.models["gpt-4o"];
        assert_eq!(m.context_tokens(), Some(128000));
        assert_eq!(m.input_cost(), Some(2.5));
        assert!(m.is_text_model());
    }

    #[test]
    fn deserialize_absent_fields_use_defaults() {
        // Bare-minimum shape: only id + name. Everything else optional.
        let json = r#"{
            "x": {"id": "x", "name": "X", "models": {
                "m": {"id": "m", "name": "M"}
            }}
        }"#;
        let map: ProvidersMap = serde_json::from_str(json).expect("parse");
        let m = &map["x"].models["m"];
        assert_eq!(m.context_tokens(), None);
        assert_eq!(m.input_cost(), None);
        assert!(!m.reasoning);
        assert!(m.is_text_model(), "no modalities → treated as text");
    }
}
