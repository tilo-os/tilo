//! Large parts of a recorded action, kept as content and named by reference.
//!
//! Every change records the action it ran ([`crate::ActionContext::action`])
//! so a receiver can replay it. An action whose payload is larger than the
//! history budget (a paste with images inline) still records it: each long
//! text or byte run of its payload is kept, sealed, in the content store of
//! the window whose code records the action ([`register`]), and the recorded
//! payload names it by the reference that store answered. Only the record's
//! readers hold the key a reference carries; whoever stores or carries the
//! part holds ciphertext. Where no store would keep a part, the action is
//! refused rather than recorded naming content nothing holds.
//!
//! A receiver resolves those references from its own content store before it
//! replays: the record's ([`crate::ActionRegistry`] keeps it), which asks the
//! record's route and holds the change until the part arrives, as it holds
//! one whose app surface has not.
//!
//! A reference is the JSON object `{"tilo.content/1": "<reference>", "as":
//! "text" | "bytes" | "json"}` in place of the string, byte array, or
//! array or object (a long list of small parts: a pasted book's
//! paragraphs) it stands for.

use serde_json::{Map, Value};
use tilo_content::ContentStore;
use tilo_wake::WakeHandle;

use crate::{ActionError, EncodedAction};

pub const CONTENT_REFERENCE_KEY: &str = "tilo.content/1";

/// The shortest text or byte run kept as content rather than inline.
pub const CONTENT_PART_THRESHOLD: usize = 16 * 1024;

const AS: &str = "as";
const TEXT: &str = "text";
const BYTES: &str = "bytes";
const JSON: &str = "json";

/// Content a recorded action names that is not here yet.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the recorded action names content {0} that is not here yet")]
pub struct MissingContent(pub String);

/// The parts of a recorded action kept as content: each part's bytes, by the
/// reference the recorded action names it by.
pub type ContentParts = Vec<(String, Vec<u8>)>;

thread_local! {
    /// The content stores of this process's windows, by the wake scope their
    /// code runs in.
    static WINDOW_STORES: std::cell::RefCell<Vec<(WakeHandle, ContentStore)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Keep `store` as the content store of the window whose wake scope `window`
/// names: where the large parts of the actions its code records are kept,
/// sealed, and where a recorded action's parts are read from, so recorded
/// actions replay wherever their content reaches. The host sets it once per
/// window, with the store it lent the window
/// ([`crate::ActionRegistry::connect_content`]); windows that closed are
/// forgotten.
pub fn register(window: &WakeHandle, store: ContentStore) {
    WINDOW_STORES.with(|stores| {
        let mut stores = stores.borrow_mut();
        stores.retain(|(handle, _)| handle.is_active() && !handle.same_open_scope(window));
        stores.push((window.clone(), store));
    });
}

/// The content store of the window whose code is running: the one registered
/// for the wake scope entered now. `None` outside a window, or for one whose
/// host lent no store.
#[must_use]
pub fn current() -> Option<ContentStore> {
    let here = WakeHandle::new();
    WINDOW_STORES.with(|stores| {
        stores
            .borrow()
            .iter()
            .find(|(handle, _)| handle.same_open_scope(&here))
            .map(|(_, store)| store.clone())
    })
}

/// The store this thread's window records its actions in: the entered scope's
/// ([`current`]), else the last one this thread registered for a window. A
/// window's callback runs on its own thread and may dispatch an action
/// outside the scope its toolkit enters for a pass; what it records is still
/// that window's.
pub(crate) fn recording_store() -> Option<ContentStore> {
    current().or_else(|| {
        WINDOW_STORES.with(|stores| stores.borrow().last().map(|(_, store)| store.clone()))
    })
}

/// Keep `bytes` in the content store of the window whose code records them,
/// answering the reference the recorded action names them by: sealed under a
/// key of their own when the store seals. Where no store keeps content, the
/// action is refused rather than recorded with a reference nothing holds.
fn put_here(bytes: &[u8]) -> Result<String, ActionError> {
    let Some(store) = recording_store() else {
        return Err(ActionError::Refused(format!(
            "a large part of the action ({} bytes) would be lost: no content store keeps content \
             where the action is recorded",
            bytes.len()
        )));
    };
    store.put(bytes).map_err(|error| {
        ActionError::Refused(format!(
            "a large part of the action could not be kept: {error}"
        ))
    })
}

/// Keep, in `store` — the content store of the record whose action `env`
/// applies, or the running window's when none is named — the parts `env`'s
/// recorded action names, sealed exactly as its writer sealed them: what a
/// device that applies a received action holds of it afterwards.
pub(crate) fn keep(env: &crate::ActionEnvelope, store: Option<&ContentStore>) {
    let Ok(content) = env.recorded_content() else {
        return;
    };
    let Some(store) = store.cloned().or_else(recording_store) else {
        return;
    };
    for (reference, bytes) in content {
        if store.has(&reference) {
            continue;
        }
        if let Err(error) = store.put_as(&reference, &bytes) {
            tracing::warn!(%error, "an action's content could not be kept");
        }
    }
}

impl EncodedAction {
    /// This action as its change records it: every text or byte run of at
    /// least `threshold` bytes, and every long list of small parts, kept as
    /// content in the store of the window whose code records it
    /// ([`register`]) and named by the reference it answered. Run once,
    /// where the action is invoked: a sealing store draws a fresh key for
    /// each part.
    ///
    /// # Errors
    ///
    /// [`ActionError::Refused`] when a part has nowhere to be kept: a
    /// reference nothing holds would lose it.
    pub(crate) fn with_content_parts(&self, threshold: usize) -> Result<Self, ActionError> {
        let mut value = self.payload_value()?;
        if holds_reserved(&value) {
            return Err(reserved());
        }
        // The payload's own shape stays inline; only its parts are kept.
        match &mut value {
            Value::Object(object) => {
                for item in object.values_mut() {
                    externalize(item, threshold)?;
                }
            }
            Value::Array(items) => {
                for item in items {
                    externalize(item, threshold)?;
                }
            }
            other => {
                externalize(other, threshold)?;
            }
        }
        let payload = serde_json::to_vec(&value).map_err(ActionError::Encode)?;
        Ok(Self {
            type_id: self.type_id.clone(),
            payload,
        })
    }

    /// The content this recorded action names, in payload order.
    #[must_use]
    pub fn content_parts(&self) -> Vec<String> {
        let mut references = Vec::new();
        if let Ok(value) = serde_json::from_slice::<Value>(&self.payload) {
            collect(&value, &mut references);
        }
        references
    }

    /// The parts this recorded action names, read from `action`, the action
    /// as it ran: `None` when this is not `action` with its large parts kept
    /// as content (another shape, or a part that is not what its reference
    /// names).
    #[must_use]
    pub fn recorded_parts_of(&self, action: &Self) -> Option<ContentParts> {
        if self.type_id != action.type_id {
            return None;
        }
        if self.payload == action.payload {
            return Some(ContentParts::new());
        }
        let recorded = self.payload_value().ok()?;
        let ran = action.payload_value().ok()?;
        let mut parts = ContentParts::new();
        pair(&recorded, &ran, &mut parts).then_some(())?;
        parts
            .iter()
            .all(|(reference, bytes)| tilo_content::names(reference, bytes))
            .then_some(parts)
    }

    /// The action as it ran, with every content reference replaced by the
    /// content `read` returns for it, checked against its reference.
    pub fn resolve_content(
        &self,
        read: &dyn Fn(&str) -> Option<Vec<u8>>,
    ) -> Result<Result<Self, MissingContent>, ActionError> {
        let mut value = self.payload_value()?;
        if let Err(missing) = internalize(&mut value, read) {
            return Ok(Err(missing));
        }
        let payload = serde_json::to_vec(&value).map_err(ActionError::Encode)?;
        Ok(Ok(Self {
            type_id: self.type_id.clone(),
            payload,
        }))
    }

    fn payload_value(&self) -> Result<Value, ActionError> {
        serde_json::from_slice(&self.payload).map_err(|source| ActionError::Decode {
            type_id: self.type_id.clone(),
            source,
        })
    }
}

fn reference(reference: String, kind: &str) -> Value {
    let mut object = Map::new();
    object.insert(CONTENT_REFERENCE_KEY.into(), Value::String(reference));
    object.insert(AS.into(), Value::String(kind.into()));
    Value::Object(object)
}

/// The bytes of an array that holds only bytes.
fn byte_run(items: &[Value]) -> Option<Vec<u8>> {
    items
        .iter()
        .map(|item| item.as_u64().and_then(|byte| u8::try_from(byte).ok()))
        .collect()
}

fn reserved() -> ActionError {
    ActionError::Refused(format!(
        "an action payload may not use the key {CONTENT_REFERENCE_KEY}"
    ))
}

fn holds_reserved(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            object.contains_key(CONTENT_REFERENCE_KEY) || object.values().any(holds_reserved)
        }
        Value::Array(items) => items.iter().any(holds_reserved),
        _ => false,
    }
}

/// Keep each text or byte run of `value` that is at least `threshold` bytes
/// as content, answering whether it kept one here or below.
fn externalize(value: &mut Value, threshold: usize) -> Result<bool, ActionError> {
    match value {
        Value::String(text) if text.len() >= threshold => {
            *value = reference(put_here(text.as_bytes())?, TEXT);
            return Ok(true);
        }
        Value::Array(items) => {
            if let Some(bytes) = byte_run(items).filter(|bytes| bytes.len() >= threshold) {
                *value = reference(put_here(&bytes)?, BYTES);
                return Ok(true);
            }
            let mut kept = false;
            for item in items {
                kept |= externalize(item, threshold)?;
            }
            if kept {
                return Ok(true);
            }
        }
        Value::Object(object) => {
            let mut kept = false;
            for item in object.values_mut() {
                kept |= externalize(item, threshold)?;
            }
            if kept {
                return Ok(true);
            }
        }
        _ => return Ok(false),
    }
    // A long list of small parts is long all the same: kept whole, as it is,
    // so its bytes follow from the action alone.
    let encoded = serde_json::to_vec(value).map_err(ActionError::Encode)?;
    if encoded.len() >= threshold {
        *value = reference(put_here(&encoded)?, JSON);
        return Ok(true);
    }
    Ok(false)
}

/// The reference and kind of a content reference.
fn referenced(object: &Map<String, Value>) -> Option<(&str, &str)> {
    if object.len() != 2 {
        return None;
    }
    let reference = object.get(CONTENT_REFERENCE_KEY)?.as_str()?;
    let kind = object.get(AS)?.as_str()?;
    (tilo_content::valid_reference(reference) && matches!(kind, TEXT | BYTES | JSON))
        .then_some((reference, kind))
}

fn collect(value: &Value, references: &mut Vec<String>) {
    match value {
        Value::Object(object) => match referenced(object) {
            Some((reference, _)) => references.push(reference.to_owned()),
            None => object.values().for_each(|item| collect(item, references)),
        },
        Value::Array(items) => items.iter().for_each(|item| collect(item, references)),
        _ => {}
    }
}

/// Collect into `parts` the bytes of `ran` each reference in `recorded`
/// stands for; whether the two are otherwise the same.
fn pair(recorded: &Value, ran: &Value, parts: &mut ContentParts) -> bool {
    match (recorded, ran) {
        (Value::Object(object), _) if referenced(object).is_some() => {
            let Some((reference, kind)) = referenced(object) else {
                return false;
            };
            let bytes = match (kind, ran) {
                (TEXT, Value::String(text)) => text.as_bytes().to_vec(),
                (BYTES, Value::Array(items)) => match byte_run(items) {
                    Some(bytes) => bytes,
                    None => return false,
                },
                (JSON, Value::Array(_) | Value::Object(_)) => match serde_json::to_vec(ran) {
                    Ok(bytes) => bytes,
                    Err(_) => return false,
                },
                _ => return false,
            };
            parts.push((reference.to_owned(), bytes));
            true
        }
        (Value::Object(recorded), Value::Object(ran)) => {
            recorded.len() == ran.len()
                && recorded
                    .iter()
                    .all(|(key, item)| ran.get(key).is_some_and(|other| pair(item, other, parts)))
        }
        (Value::Array(recorded), Value::Array(ran)) => {
            recorded.len() == ran.len()
                && recorded
                    .iter()
                    .zip(ran)
                    .all(|(item, other)| pair(item, other, parts))
        }
        _ => recorded == ran,
    }
}

fn internalize(
    value: &mut Value,
    read: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> Result<(), MissingContent> {
    match value {
        Value::Object(object) => {
            if let Some((named, kind)) = referenced(object) {
                let missing = || MissingContent(named.to_owned());
                let bytes = read(named)
                    .filter(|bytes| tilo_content::names(named, bytes))
                    .ok_or_else(missing)?;
                *value = match kind {
                    TEXT => Value::String(String::from_utf8(bytes).map_err(|_| missing())?),
                    JSON => {
                        let mut part: Value =
                            serde_json::from_slice(&bytes).map_err(|_| missing())?;
                        // A part an earlier version kept whole may hold
                        // parts kept on their own.
                        internalize(&mut part, read)?;
                        part
                    }
                    _ => Value::Array(bytes.into_iter().map(Value::from).collect()),
                };
                return Ok(());
            }
            for item in object.values_mut() {
                internalize(item, read)?;
            }
        }
        Value::Array(items) => {
            for item in items {
                internalize(item, read)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(payload: &Value) -> EncodedAction {
        EncodedAction {
            type_id: "note".into(),
            payload: serde_json::to_vec(payload).unwrap(),
        }
    }

    /// `action` as a window whose content store is `store` records it: its
    /// parts kept, sealed, in that store.
    fn recorded(action: &EncodedAction, store: &ContentStore) -> EncodedAction {
        let window = tilo_wake::WakeScope::new();
        register(&window.handle(), store.clone());
        let _in_the_window = window.enter();
        action.with_content_parts(CONTENT_PART_THRESHOLD).unwrap()
    }

    fn read(store: &ContentStore) -> impl Fn(&str) -> Option<Vec<u8>> + '_ {
        |reference| store.read(reference)
    }

    /// Long text and byte runs leave the payload as sealed references and
    /// come back exactly; short values stay inline; a missing part holds.
    #[test]
    fn large_parts_are_kept_sealed_and_resolve_back() {
        let image: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();
        let original = action(&serde_json::json!({
            "paste": { "text": "short", "body": "x".repeat(20_000), "image": image },
            "count": 3,
        }));
        let store = ContentStore::memory().sealing();
        let recorded = recorded(&original, &store);
        assert!(recorded.payload.len() < 1024, "the payload names its parts");
        let parts = recorded.content_parts();
        assert_eq!(parts.len(), 2);
        assert!(
            parts
                .iter()
                .all(|reference| tilo_content::is_sealed(reference))
        );
        assert!(
            !store.has(blake3::hash(&image).to_hex().as_ref()),
            "nothing kept in the clear"
        );
        let resolved = recorded.resolve_content(&read(&store)).unwrap().unwrap();
        let back: Value = serde_json::from_slice(&resolved.payload).unwrap();
        let want: Value = serde_json::from_slice(&original.payload).unwrap();
        assert_eq!(back, want);
        assert_eq!(
            recorded.resolve_content(&|_| None).unwrap(),
            Err(MissingContent(parts[0].clone()))
        );
        // Other bytes than the reference names are not the content.
        assert!(
            recorded
                .resolve_content(&|_| Some(vec![1]))
                .unwrap()
                .is_err()
        );
    }

    /// A device holding the action as it ran finds, from the recorded form
    /// alone, each part's bytes, and keeps them exactly as the writer
    /// sealed them; a recorded form that is not of that action is refused.
    #[test]
    fn the_action_as_it_ran_yields_its_parts_exactly() {
        let original = action(&serde_json::json!({ "body": "y".repeat(30_000), "at": 2 }));
        let writer = ContentStore::memory().sealing();
        let recorded = recorded(&original, &writer);
        let parts = recorded.recorded_parts_of(&original).unwrap();
        assert_eq!(parts.len(), 1);
        let elsewhere = ContentStore::memory();
        for (reference, bytes) in parts {
            elsewhere.put_as(&reference, &bytes).unwrap();
            assert_eq!(elsewhere.read(&reference), writer.read(&reference));
        }
        let other = action(&serde_json::json!({ "body": "z".repeat(30_000), "at": 2 }));
        assert!(recorded.recorded_parts_of(&other).is_none());
        let moved = action(&serde_json::json!({ "body": "y".repeat(30_000), "at": 3 }));
        assert!(recorded.recorded_parts_of(&moved).is_none());
        assert_eq!(
            original.recorded_parts_of(&original),
            Some(ContentParts::new())
        );
    }

    /// A long list of small parts (a pasted book's paragraphs) is kept
    /// whole and comes back exactly; the payload's own shape stays inline.
    #[test]
    fn a_long_list_of_small_parts_is_kept_whole() {
        let paragraphs: Vec<Value> = (0..4_000)
            .map(|n| serde_json::json!({ "kind": "paragraph", "text": format!("Paragraph {n}") }))
            .collect();
        let original = action(&serde_json::json!({ "paste": { "blocks": paragraphs }, "at": 1 }));
        let store = ContentStore::memory().sealing();
        let recorded = recorded(&original, &store);
        assert!(recorded.payload.len() < 1024, "the payload names its parts");
        let shape: Value = serde_json::from_slice(&recorded.payload).unwrap();
        assert_eq!(shape["at"], 1);
        assert_eq!(recorded.content_parts().len(), 1);
        let resolved = recorded.resolve_content(&read(&store)).unwrap().unwrap();
        let back: Value = serde_json::from_slice(&resolved.payload).unwrap();
        let want: Value = serde_json::from_slice(&original.payload).unwrap();
        assert_eq!(back, want);
        assert!(recorded.recorded_parts_of(&original).is_some());
    }

    #[test]
    fn the_reserved_key_is_refused_in_a_payload() {
        let forged =
            action(&serde_json::json!({ "nested": { CONTENT_REFERENCE_KEY: "x", "as": "text" } }));
        assert!(matches!(
            forged.with_content_parts(CONTENT_PART_THRESHOLD),
            Err(ActionError::Refused(_))
        ));
    }
}
