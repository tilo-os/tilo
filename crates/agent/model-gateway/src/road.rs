//! The road a request takes to its model, and the provider's cache along it.
//!
//! A provider may take fields of its own beside the request. One that serves
//! a model from several places is told by which measure to choose among
//! them; one that finds its cache by a key is given the same key for every
//! call of a conversation; a model that caches only what is marked has its
//! instruction marked. Each field is one its provider documents, added in
//! this one place, and none names another model: the checked model id goes
//! as it is.
//!
//! A person switches any of them off as they switch off any field, in the
//! profile's `omit_params`, and what a caller asked of the road itself
//! stays as it asked it. A provider that refuses what was added is asked
//! once more without it and is not sent it again ([`refuses`],
//! [`remember`]).

use std::collections::BTreeSet;

use agent_provider_config::presets::Preset;
use tilo_lock::MutexRecover as _;

use super::*;

/// A provider asked something of its own, told by its endpoint's name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Spoken {
    /// Serves one model from several places: `provider` chooses among them,
    /// `session_id` keeps a conversation where its cache is, and a model
    /// that caches only what is marked is sent the marks.
    OpenRouter,
    /// Finds its prompt cache by `prompt_cache_key`.
    OpenAi,
}

/// The name an endpoint is reached under, in lower case.
fn named(endpoint: &str) -> Option<String> {
    reqwest::Url::parse(endpoint.trim())
        .ok()?
        .domain()
        .map(str::to_ascii_lowercase)
}

/// The provider `profile` speaks to, where it is one asked something of its
/// own: the one whose ready-made profile names the same endpoint.
fn spoken(profile: &ProviderProfile) -> Option<Spoken> {
    if profile.protocol != ProviderProtocol::OpenAiCompatible {
        return None;
    }
    let at = named(&profile.endpoint)?;
    [
        (Preset::OPENROUTER, Spoken::OpenRouter),
        (Preset::OPENAI, Spoken::OpenAi),
    ]
    .into_iter()
    .find(|(preset, _)| named(preset.endpoint()).as_deref() == Some(at.as_str()))
    .map(|(_, spoken)| spoken)
}

/// A key that is the same for every call of one conversation and tells
/// nothing of it. It is made of the instruction a turn opens with, which
/// names its agent and its conversation and stays the same from call to
/// call and from turn to turn. A request that opens with none has no key.
fn conversation_key(object: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    let words = |content: &serde_json::Value| match content {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    let instruction = if let Some(instructions) = object.get("instructions") {
        words(instructions)
    } else {
        let first = object.get("messages")?.as_array()?.first()?;
        let role = first.get("role")?.as_str()?;
        if !matches!(role, "system" | "developer") {
            return None;
        }
        words(first.get("content")?)
    };
    (!instruction.trim().is_empty()).then(|| {
        crate::openai::idempotency_key(&[b"conversation".as_slice(), instruction.as_bytes()])
    })
}

/// Whether `model`, as a provider of many vendors' models names it, caches
/// only what a request marks.
fn caches_what_is_marked(model: &str) -> bool {
    model.starts_with("anthropic/")
}

/// Add to `object`, a request already shaped for `profile`, what its
/// provider is asked beside it for `model`.
pub(super) fn beside(
    profile: &ProviderProfile,
    model: &str,
    object: &mut serde_json::Map<String, serde_json::Value>,
) {
    let Some(spoken) = spoken(profile) else {
        return;
    };
    let wanted = |object: &serde_json::Map<String, serde_json::Value>, field: &str| {
        !object.contains_key(field) && !profile.omit_params.iter().any(|param| param == field)
    };
    let key = conversation_key(object);
    match spoken {
        Spoken::OpenRouter => {
            // The place that answers first, of those that serve this model.
            if wanted(object, "provider") {
                object.insert("provider".into(), serde_json::json!({ "sort": "latency" }));
            }
            if let Some(key) = key
                && wanted(object, "session_id")
            {
                object.insert("session_id".into(), key.into());
            }
            if caches_what_is_marked(model) && wanted(object, "cache_control") {
                // The conversation as it grows, and the instruction with the
                // tools before it, which the next turn opens with again.
                object.insert(
                    "cache_control".into(),
                    serde_json::json!({ "type": "ephemeral" }),
                );
                if let Some(instruction) = object
                    .get_mut("messages")
                    .and_then(serde_json::Value::as_array_mut)
                    .and_then(|messages| messages.first_mut())
                    .filter(|first| {
                        first.get("role").and_then(serde_json::Value::as_str) == Some("system")
                    })
                    .and_then(|first| first.get_mut("content"))
                {
                    mark_last_cacheable(instruction);
                }
            }
        }
        Spoken::OpenAi => {
            if let Some(key) = key
                && wanted(object, "prompt_cache_key")
            {
                object.insert("prompt_cache_key".into(), key.into());
            }
        }
    }
}

/// The fields `asked` holds at its top beyond `plain`, by name.
pub(super) fn added(plain: &serde_json::Value, asked: &serde_json::Value) -> Vec<String> {
    let (Some(plain), Some(asked)) = (plain.as_object(), asked.as_object()) else {
        return Vec::new();
    };
    asked
        .keys()
        .filter(|field| !plain.contains_key(field.as_str()))
        .cloned()
        .collect()
}

/// The roads whose provider refused what was added, by endpoint and model:
/// they are not sent it again while this process lives.
static REFUSED: Mutex<BTreeSet<(String, String)>> = Mutex::new(BTreeSet::new());

/// Whether `profile`'s provider refused before what is added for `model`.
pub(super) fn refused(profile: &ProviderProfile, model: &str) -> bool {
    REFUSED
        .lock_recover()
        .contains(&(profile.endpoint.clone(), model.to_owned()))
}

/// Keep that `profile`'s provider refuses what is added for `model`.
pub(super) fn remember(profile: &ProviderProfile, model: &str) {
    REFUSED
        .lock_recover()
        .insert((profile.endpoint.clone(), model.to_owned()));
}

/// Whether `failure` is a provider's refusal of the request as it was
/// written, the answer a field it does not know gets. It did no work for
/// such a request, so asking again without the field repeats nothing.
pub(super) fn refuses(failure: &ProviderFailure) -> bool {
    matches!(failure.status, Some(400 | 422))
}

/// Mark the last part of an instruction in words for its provider's cache,
/// turning plain words into one part. Nothing where the last such part is
/// marked already.
fn mark_last_cacheable(content: &mut serde_json::Value) {
    let marked = serde_json::json!({ "type": "ephemeral" });
    match content {
        serde_json::Value::String(text) if !text.trim().is_empty() => {
            let text = std::mem::take(text);
            *content = serde_json::json!([{
                "type": "text",
                "text": text,
                "cache_control": marked,
            }]);
        }
        serde_json::Value::Array(parts) => {
            let last = parts.iter_mut().rev().find(|part| {
                part.get("type").and_then(serde_json::Value::as_str) == Some("text")
                    && part
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|text| !text.trim().is_empty())
            });
            if let Some(part) = last.and_then(serde_json::Value::as_object_mut) {
                part.entry("cache_control").or_insert(marked);
            }
        }
        _ => {}
    }
}
