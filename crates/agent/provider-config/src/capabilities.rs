//! What a hosted model can do: see pictures (`vision`) and call tools
//! (`tool_calling`).
//!
//! Three sources fill [`ModelDescriptor`]'s fields, in this order of
//! precedence:
//!
//! 1. The person, per model ([`CapabilitySource::Person`]). Nothing below
//!    replaces their answer.
//! 2. The provider's model list, where it says
//!    ([`from_listing`]): `OpenRouter`'s `architecture.input_modalities` and
//!    `supported_parameters`, Anthropic's `capabilities.image_input`,
//!    Gemini's `supportedGenerationMethods` and token limits, Mistral's
//!    `capabilities`, and the flags Moonshot, Requesty, Venice, DeepInfra,
//!    DeepSeek and xAI give.
//! 3. Tilo's table of known model families ([`family`]), for a list that
//!    says nothing (most OpenAI-compatible ones) and for a model named by
//!    hand.
//!
//! A model none of them knows keeps the defaults: no vision, and tool
//! calling assumed, so it is offered to agents and the provider's own
//! refusal names it.

use serde_json::Value;

use crate::{CapabilitySource, ModelDescriptor};

/// What one source says a model can do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub vision: Option<bool>,
    pub tool_calling: Option<bool>,
    pub context_window: Option<u32>,
}

impl Capabilities {
    const fn says_anything(self) -> bool {
        self.vision.is_some() || self.tool_calling.is_some()
    }
}

/// What a model-list `entry` says about its model, in whichever of the
/// known shapes it uses. Fields it does not carry stay `None`.
#[must_use]
pub fn from_listing(entry: &Value) -> Capabilities {
    let strings = |value: Option<&Value>| -> Option<Vec<String>> {
        value.and_then(Value::as_array).map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_ascii_lowercase)
                .collect()
        })
    };
    let flag = |object: Option<&Value>, keys: &[&str]| {
        keys.iter()
            .find_map(|key| object.and_then(|object| object.get(*key)))
            .and_then(|value| {
                // Anthropic nests `{ "supported": true }`; Mistral is flat.
                value
                    .as_bool()
                    .or_else(|| value.get("supported").and_then(Value::as_bool))
            })
    };
    let mut found = Capabilities::default();
    // Input modalities: OpenRouter and llama.cpp under `architecture`,
    // DeepSeek and xAI at the top.
    if let Some(modalities) = strings(
        entry
            .get("architecture")
            .and_then(|architecture| architecture.get("input_modalities")),
    )
    .or_else(|| strings(entry.get("input_modalities")))
    {
        found.vision = Some(modalities.iter().any(|modality| modality == "image"));
    }
    // OpenRouter: `supported_parameters`.
    if let Some(parameters) = strings(entry.get("supported_parameters")) {
        found.tool_calling = Some(parameters.iter().any(|parameter| parameter == "tools"));
    }
    // A capabilities object: Anthropic (`image_input`), Mistral (`vision`,
    // `function_calling`), Venice (`model_spec.capabilities`), LM Studio
    // (`trained_for_tool_use`); and flat flags: Moonshot
    // (`supports_image_in`), Requesty (`supports_vision`,
    // `supports_tool_calling`).
    let capabilities = entry.get("capabilities").or_else(|| {
        entry
            .get("model_spec")
            .and_then(|spec| spec.get("capabilities"))
    });
    if found.vision.is_none() {
        found.vision = flag(capabilities, &["image_input", "vision", "supportsVision"])
            .or_else(|| flag(Some(entry), &["supports_image_in", "supports_vision"]));
    }
    if found.tool_calling.is_none() {
        found.tool_calling = flag(
            capabilities,
            &[
                "tool_use",
                "function_calling",
                "supportsFunctionCalling",
                "trained_for_tool_use",
            ],
        )
        .or_else(|| flag(Some(entry), &["supports_tool_calling"]));
    }
    // DeepInfra tags its vision models.
    if found.vision.is_none()
        && let Some(tags) = strings(entry.get("metadata").and_then(|meta| meta.get("tags")))
    {
        found.vision = Some(tags.iter().any(|tag| tag == "vision"));
    }
    // Gemini's native list: a model that cannot generate content cannot
    // plan (an embedding model, say).
    if let Some(methods) = strings(entry.get("supportedGenerationMethods"))
        && !methods.iter().any(|method| method == "generatecontent")
    {
        found.tool_calling = Some(false);
    }
    let metadata = entry.get("metadata");
    found.context_window = [
        "context_length",
        "context_window",
        "max_context_length",
        "max_input_tokens",
        "inputTokenLimit",
        "context_size",
    ]
    .iter()
    .find_map(|key| {
        entry
            .get(*key)
            .or_else(|| metadata.and_then(|meta| meta.get(*key)))
            .and_then(Value::as_u64)
    })
    .and_then(|tokens| u32::try_from(tokens).ok());
    found
}

/// One row of the family table: models whose id contains `pattern`, or
/// starts with it after a `^` (after any `vendor/` or `models/` prefix,
/// case-folded).
struct Family {
    pattern: &'static str,
    vision: bool,
    tool_calling: bool,
}

const fn family_row(pattern: &'static str, vision: bool, tool_calling: bool) -> Family {
    Family {
        pattern,
        vision,
        tool_calling,
    }
}

/// Known model families, most specific first: the first row whose pattern
/// the id contains decides.
const FAMILIES: &[Family] = &[
    // Models that do not chat: never a planner.
    family_row("embed", false, false),
    family_row("rerank", false, false),
    family_row("moderation", false, false),
    family_row("whisper", false, false),
    family_row("transcribe", false, false),
    family_row("tts", false, false),
    family_row("dall-e", false, false),
    family_row("gpt-image", false, false),
    family_row("imagen", false, false),
    family_row("flux", false, false),
    family_row("stable-diffusion", false, false),
    // Perplexity's Sonar searches the web itself and takes no tools.
    family_row("sonar", false, false),
    // Reasoning models without tools.
    family_row("^o1-mini", false, false),
    family_row("^o1-preview", false, false),
    family_row("deepseek-r1-distill", false, false),
    // Vision variants of text families, before their families.
    family_row("-vl", true, true),
    family_row("vl-", true, true),
    family_row("qvq", true, false),
    family_row("vision", true, true),
    family_row("pixtral", true, true),
    family_row("glm-4.5v", true, true),
    family_row("glm-4.6v", true, true),
    family_row("glm-4v", true, true),
    family_row("llava", true, false),
    // Anthropic: every Claude 3 and later sees and calls tools.
    family_row("claude", true, true),
    // OpenAI.
    family_row("gpt-oss", false, true),
    family_row("gpt-3.5", false, true),
    family_row("gpt-4o", true, true),
    family_row("gpt-4.1", true, true),
    family_row("gpt-4-turbo", true, true),
    family_row("gpt-4", false, true),
    family_row("gpt-5", true, true),
    family_row("codex", true, true),
    family_row("^o1", true, true),
    family_row("^o3", true, true),
    family_row("^o4", true, true),
    // Google.
    family_row("gemma-3", true, false),
    family_row("gemma", false, false),
    family_row("gemini", true, true),
    // xAI.
    family_row("grok-4", true, true),
    family_row("grok-3", false, true),
    family_row("grok-code", false, true),
    family_row("grok", false, true),
    // Mistral.
    family_row("mistral-medium", true, true),
    family_row("mistral-small", true, true),
    family_row("mistral-large", false, true),
    family_row("magistral", false, true),
    family_row("devstral", false, true),
    family_row("codestral", false, true),
    family_row("ministral", false, true),
    // Meta.
    family_row("llama-4", true, true),
    family_row("llama4", true, true),
    family_row("llama-3.3", false, true),
    family_row("llama-3.1", false, true),
    // Open-weight families that call tools but see nothing.
    family_row("deepseek", false, true),
    family_row("qwen", false, true),
    family_row("qwq", false, true),
    family_row("kimi", false, true),
    family_row("moonshot", false, true),
    family_row("glm", false, true),
    family_row("minimax", false, true),
    family_row("command-a-vision", true, false),
    family_row("command-a", false, true),
    family_row("command-r", false, true),
    family_row("hermes", false, true),
    family_row("nemotron", false, true),
];

/// What Tilo's family table knows about model `id`, `None` for a family it
/// does not know.
#[must_use]
pub fn family(id: &str) -> Option<Capabilities> {
    let id = id.to_ascii_lowercase();
    let name = id.rsplit('/').next().unwrap_or(&id);
    FAMILIES
        .iter()
        .find(|family| match family.pattern.strip_prefix('^') {
            Some(prefix) => name.starts_with(prefix),
            None => name.contains(family.pattern),
        })
        .map(|family| Capabilities {
            vision: Some(family.vision),
            tool_calling: Some(family.tool_calling),
            context_window: None,
        })
}

/// Fill `model`'s capabilities from what its list `entry` says, and from
/// the family table for whatever it leaves unsaid. A person's answer stays.
pub fn apply_listing(model: &mut ModelDescriptor, entry: &Value) {
    let listed = from_listing(entry);
    if model.context_window.is_none() {
        model.context_window = listed.context_window;
    }
    if model.capabilities_set_by_person() {
        return;
    }
    let known = family(&model.id).unwrap_or_default();
    model.vision = listed.vision.or(known.vision).unwrap_or(false);
    model.tool_calling = listed.tool_calling.or(known.tool_calling).unwrap_or(true);
    model.capabilities_from = if listed.says_anything() {
        CapabilitySource::Provider
    } else if known.says_anything() {
        CapabilitySource::Family
    } else {
        CapabilitySource::Unknown
    };
}

/// Fill `model`'s capabilities from the family table alone: a model named
/// by hand. A person's answer stays.
pub fn apply_family(model: &mut ModelDescriptor) {
    apply_listing(model, &Value::Null);
}

/// Take what a fresh list says about a model the person already has,
/// keeping their own answer and their request limits: what a later "Test
/// connection" refreshes.
pub fn refresh(stored: &mut ModelDescriptor, listed: &ModelDescriptor) {
    if stored.context_window.is_none() {
        stored.context_window = listed.context_window;
    }
    if stored.capabilities_set_by_person() {
        return;
    }
    stored.vision = listed.vision;
    stored.tool_calling = listed.tool_calling;
    stored.capabilities_from = listed.capabilities_from;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn listed(id: &str, entry: &Value) -> ModelDescriptor {
        let mut model = ModelDescriptor::new(id);
        apply_listing(&mut model, entry);
        model
    }

    #[test]
    fn openrouter_modalities_and_parameters_decide() {
        let entry = json!({
            "id": "acme/seer",
            "context_length": 131_072,
            "architecture": {"input_modalities": ["text", "image"], "output_modalities": ["text"]},
            "supported_parameters": ["max_tokens", "tools", "tool_choice"]
        });
        let model = listed("acme/seer", &entry);
        assert!(model.vision && model.tool_calling);
        assert_eq!(model.context_window, Some(131_072));
        assert_eq!(model.capabilities_from, CapabilitySource::Provider);
        // The list outranks the family table: a Claude the list says is
        // text-only and tool-less is that.
        let entry = json!({
            "architecture": {"input_modalities": ["text"]},
            "supported_parameters": ["max_tokens"]
        });
        let model = listed("anthropic/claude-x", &entry);
        assert!(!model.vision && !model.tool_calling);
    }

    #[test]
    fn anthropic_and_mistral_capability_objects_decide() {
        let anthropic = json!({
            "id": "claude-x", "max_input_tokens": 200_000,
            "capabilities": {"image_input": {"supported": false}, "tool_use": {"supported": true}}
        });
        let model = listed("claude-x", &anthropic);
        assert!(!model.vision && model.tool_calling);
        assert_eq!(model.context_window, Some(200_000));
        let mistral = json!({
            "id": "pixtral-large", "max_context_length": 128_000,
            "capabilities": {"completion_chat": true, "function_calling": true, "vision": true}
        });
        let model = listed("pixtral-large", &mistral);
        assert!(model.vision && model.tool_calling);
    }

    #[test]
    fn a_list_that_says_nothing_falls_back_to_the_family_table() {
        let model = listed("gpt-4o-mini", &json!({"id": "gpt-4o-mini"}));
        assert!(model.vision && model.tool_calling);
        assert_eq!(model.capabilities_from, CapabilitySource::Family);
        let model = listed("text-embedding-3-small", &json!({}));
        assert!(!model.tool_calling);
        let model = listed("Qwen/Qwen2.5-VL-72B-Instruct", &json!({}));
        assert!(model.vision);
        let model = listed("deepseek-chat", &json!({}));
        assert!(!model.vision && model.tool_calling);
        let unknown = listed("house-model-7", &json!({}));
        assert!(!unknown.vision && unknown.tool_calling);
        assert_eq!(unknown.capabilities_from, CapabilitySource::Unknown);
    }

    #[test]
    fn the_persons_answer_outlasts_every_list() {
        let mut model = ModelDescriptor::new("claude-x");
        model.vision = false;
        model.tool_calling = false;
        model.capabilities_from = CapabilitySource::Person;
        let fresh = listed("claude-x", &json!({}));
        refresh(&mut model, &fresh);
        apply_family(&mut model);
        assert!(!model.vision && !model.tool_calling);
        assert!(model.capabilities_set_by_person());
    }

    #[test]
    fn gemini_lists_embedding_models_as_unable_to_plan() {
        let entry = json!({
            "name": "models/text-embedding-004",
            "inputTokenLimit": 2048,
            "supportedGenerationMethods": ["embedContent"]
        });
        let model = listed("models/text-embedding-004", &entry);
        assert!(!model.tool_calling);
        assert_eq!(model.context_window, Some(2048));
    }
}
