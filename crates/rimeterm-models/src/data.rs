//! Owned data model for `https://models.dev/api.json`.
//!
//! Port of the permissive subset used by `reyamira/models` v0.14.0. Optional
//! fields preserve distinctions between absent and explicitly reported values.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Top-level API shape: `{ "<provider_id>": Provider, ... }`.
pub type ProvidersMap = HashMap<String, Provider>;

/// One provider plus its models.
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

/// One model entry from models.dev.
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
    pub temperature: bool,
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
    pub knowledge: Option<String>,
    #[serde(default)]
    pub open_weights: bool,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub structured_output: Option<bool>,
    #[serde(default)]
    pub reasoning_options: Vec<ReasoningOption>,
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
    #[serde(default)]
    pub reasoning: Option<f64>,
    #[serde(default)]
    pub input_audio: Option<f64>,
    #[serde(default)]
    pub output_audio: Option<f64>,
    #[serde(default)]
    pub tiers: Vec<CostTier>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReasoningOption {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<Option<String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CostTier {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<TierSpec>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TierSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
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

    pub fn is_free(&self) -> bool {
        self.cost
            .as_ref()
            .is_none_or(|c| c.input.unwrap_or(0.0) == 0.0 && c.output.unwrap_or(0.0) == 0.0)
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

    #[test]
    fn deserialize_rich_model_fields_permissively() {
        let json = r#"{
            "openai": {"id":"openai","name":"OpenAI","models":{
                "o3": {
                    "id":"o3","name":"o3","description":"Reasoning model",
                    "temperature":true,"structured_output":true,"status":"deprecated",
                    "knowledge":"2025-06","reasoning_options":[
                        {"type":"effort","values":["low",null,"high"]}
                    ],
                    "cost": {
                        "input":2.0,"output":8.0,"reasoning":9.0,
                        "input_audio":1.0,"output_audio":2.0,
                        "tiers":[{"input":4.0,"output":12.0,"tier":{"type":"context","size":200000}}]
                    }
                }
            }}
        }"#;
        let map: ProvidersMap = serde_json::from_str(json).expect("parse rich model");
        let model = &map["openai"].models["o3"];
        assert_eq!(model.description.as_deref(), Some("Reasoning model"));
        assert!(model.temperature);
        assert_eq!(model.structured_output, Some(true));
        assert_eq!(model.status.as_deref(), Some("deprecated"));
        assert_eq!(model.knowledge.as_deref(), Some("2025-06"));
        assert_eq!(model.reasoning_options.len(), 1);
        let cost = model.cost.as_ref().unwrap();
        assert_eq!(cost.reasoning, Some(9.0));
        assert_eq!(cost.input_audio, Some(1.0));
        assert_eq!(cost.output_audio, Some(2.0));
        assert_eq!(cost.tiers[0].tier.as_ref().unwrap().size, Some(200_000));
    }

    #[test]
    fn parse_current_sdk_shape_with_context_tier_and_interleaved_fields() {
        let json = r#"{
          "anthropic":{"id":"anthropic","name":"Anthropic","env":[],"models":{
            "claude-sonnet":{"id":"claude-sonnet","name":"Claude Sonnet","description":"Model","attachment":true,"reasoning":true,"reasoning_options":[{"type":"toggle"},{"type":"effort","values":["low","max"]}],"tool_call":true,"interleaved":{"field":"reasoning_content"},"structured_output":true,"temperature":true,"release_date":"2026-02-17","last_updated":"2026-03-13","modalities":{"input":["text","image","pdf"],"output":["text"]},"open_weights":false,"limit":{"context":1000000,"output":64000},"cost":{"input":3,"output":15,"tiers":[{"input":6,"output":22.5,"tier":{"type":"context","size":200000}}],"context_over_200k":{"input":6,"output":22.5}}}
          }}
        }"#;
        let providers: ProvidersMap = serde_json::from_str(json).expect("current SDK shape");
        assert_eq!(providers["anthropic"].models.len(), 1);
    }
}
