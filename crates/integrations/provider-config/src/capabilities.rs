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
    /// The model's own declared output limit: the most tokens one answer
    /// may take, reasoning and answer together.
    pub max_output_tokens: Option<u32>,
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
    // What the model itself declares one answer may take: OpenRouter's
    // `top_provider.max_completion_tokens`, Gemini's `outputTokenLimit`, and
    // the limits most OpenAI-compatible lists publish.
    found.max_output_tokens = [
        "max_completion_tokens",
        "max_output_tokens",
        "outputTokenLimit",
        "max_tokens",
    ]
    .iter()
    .find_map(|key| {
        entry
            .get(*key)
            .or_else(|| {
                entry
                    .get("top_provider")
                    .and_then(|provider| provider.get(*key))
            })
            .or_else(|| metadata.and_then(|meta| meta.get(*key)))
            .and_then(Value::as_u64)
    })
    .and_then(|tokens| u32::try_from(tokens).ok())
    .filter(|tokens| *tokens > 0);
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
            max_output_tokens: None,
        })
}

/// Fill `model`'s capabilities from what its list `entry` says, and from
/// the family table for whatever it leaves unsaid. A person's answer stays.
pub fn apply_listing(model: &mut ModelDescriptor, entry: &Value) {
    let listed = from_listing(entry);
    if model.context_window.is_none() {
        model.context_window = listed.context_window;
    }
    if model.max_output_tokens.is_none() {
        model.max_output_tokens = listed.max_output_tokens;
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
    if stored.max_output_tokens.is_none() {
        stored.max_output_tokens = listed.max_output_tokens;
    }
    if stored.capabilities_set_by_person() {
        return;
    }
    stored.vision = listed.vision;
    stored.tool_calling = listed.tool_calling;
    stored.capabilities_from = listed.capabilities_from;
}
