//! Large parts of a recorded action, kept as content and named by hash.
//!
//! Every change records the action it ran ([`crate::ActionContext::action`])
//! so a receiver can replay it. An action whose payload is larger than the
//! history budget (a paste with images inline) still records it: each long
//! text or byte run of its payload is kept in the content store
//! (`tilo-content`) and the recorded payload names it by its BLAKE3 hash.
//! A receiver resolves those names from its content store before it
//! replays; until the content arrives the change is held, as a change whose
//! app surface has not arrived is.
//!
//! A reference is the JSON object `{"tilo.content/1": "<hash>", "as":
//! "text" | "bytes"}` in place of the string or byte array it stands for.

use serde_json::{Map, Value};
use tilo_content::ContentInputs;

use crate::{ActionError, EncodedAction};

/// The key a content reference is written under.
pub const CONTENT_REFERENCE_KEY: &str = "tilo.content/1";

/// The shortest text or byte run kept as content rather than inline.
pub const CONTENT_PART_THRESHOLD: usize = 16 * 1024;

const AS: &str = "as";
const TEXT: &str = "text";
const BYTES: &str = "bytes";

/// Content a recorded action names that is not here yet.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the recorded action names content {0} that is not here yet")]
pub struct MissingContent(pub String);

type ContentSink = Box<dyn Fn(ContentInputs)>;
type ContentRead = Box<dyn Fn(&str) -> Option<Vec<u8>>>;

thread_local! {
    static SINK: std::cell::RefCell<Option<ContentSink>> = const { std::cell::RefCell::new(None) };
    static READ: std::cell::RefCell<Option<ContentRead>> = const { std::cell::RefCell::new(None) };
}

/// Keep the content every change this thread commits names in `keep` (the
/// app's content store), and read the content a received change names from
/// `read`, so recorded actions replay wherever their content reaches. The
/// host sets both once per thread that runs actions.
pub fn keep_content_in(
    keep: impl Fn(ContentInputs) + 'static,
    read: impl Fn(&str) -> Option<Vec<u8>> + 'static,
) {
    SINK.with(|current| *current.borrow_mut() = Some(Box::new(keep)));
    READ.with(|current| *current.borrow_mut() = Some(Box::new(read)));
}

/// The content `hash` names, from this thread's reader.
pub(crate) fn read_here(hash: &str) -> Option<Vec<u8>> {
    READ.with(|read| read.borrow().as_ref().and_then(|read| read(hash)))
}

/// Hand the content `env`'s recorded action names to this thread's sink.
pub(crate) fn keep(env: &crate::ActionEnvelope) {
    let Ok(content) = env.recorded_content() else {
        return;
    };
    if content.is_empty() {
        return;
    }
    SINK.with(|sink| {
        if let Some(sink) = sink.borrow().as_ref() {
            sink(content);
        }
    });
}

impl EncodedAction {
    /// This action with every text or byte run of at least `threshold`
    /// bytes kept as content: the recorded form, and the content to keep.
    ///
    /// # Errors
    /// A payload that is not JSON, or that already uses the reserved key.
    pub fn with_content_parts(
        &self,
        threshold: usize,
    ) -> Result<(Self, ContentInputs), ActionError> {
        let mut value: Value =
            serde_json::from_slice(&self.payload).map_err(|source| ActionError::Decode {
                type_id: self.type_id.clone(),
                source,
            })?;
        let mut content = ContentInputs::new();
        externalize(&mut value, threshold, &mut content)?;
        let payload = serde_json::to_vec(&value).map_err(ActionError::Encode)?;
        Ok((
            Self {
                type_id: self.type_id.clone(),
                payload,
            },
            content,
        ))
    }

    /// The content this recorded action names, in payload order.
    #[must_use]
    pub fn content_parts(&self) -> Vec<String> {
        let mut hashes = Vec::new();
        if let Ok(value) = serde_json::from_slice::<Value>(&self.payload) {
            collect(&value, &mut hashes);
        }
        hashes
    }

    /// The action as it ran, with every content reference replaced by the
    /// content `read` returns for it, checked against its hash.
    ///
    /// # Errors
    /// The first content that `read` does not have (or returns other bytes
    /// for), or a payload that is not JSON.
    pub fn resolve_content(
        &self,
        read: &dyn Fn(&str) -> Option<Vec<u8>>,
    ) -> Result<Result<Self, MissingContent>, ActionError> {
        let mut value: Value =
            serde_json::from_slice(&self.payload).map_err(|source| ActionError::Decode {
                type_id: self.type_id.clone(),
                source,
            })?;
        if let Err(missing) = internalize(&mut value, read) {
            return Ok(Err(missing));
        }
        let payload = serde_json::to_vec(&value).map_err(ActionError::Encode)?;
        Ok(Ok(Self {
            type_id: self.type_id.clone(),
            payload,
        }))
    }
}

fn reference(hash: String, kind: &str) -> Value {
    let mut object = Map::new();
    object.insert(CONTENT_REFERENCE_KEY.into(), Value::String(hash));
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

fn externalize(
    value: &mut Value,
    threshold: usize,
    content: &mut ContentInputs,
) -> Result<(), ActionError> {
    match value {
        Value::String(text) if text.len() >= threshold => {
            let hash = content.add(std::mem::take(text).into_bytes());
            *value = reference(hash, TEXT);
        }
        Value::Array(items) if items.len() >= threshold => {
            if let Some(bytes) = byte_run(items) {
                let hash = content.add(bytes);
                *value = reference(hash, BYTES);
            } else {
                for item in items {
                    externalize(item, threshold, content)?;
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                externalize(item, threshold, content)?;
            }
        }
        Value::Object(object) => {
            if object.contains_key(CONTENT_REFERENCE_KEY) {
                return Err(ActionError::Refused(format!(
                    "an action payload may not use the key {CONTENT_REFERENCE_KEY}"
                )));
            }
            for item in object.values_mut() {
                externalize(item, threshold, content)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// The hash and kind of a content reference.
fn referenced(object: &Map<String, Value>) -> Option<(&str, &str)> {
    if object.len() != 2 {
        return None;
    }
    let hash = object.get(CONTENT_REFERENCE_KEY)?.as_str()?;
    let kind = object.get(AS)?.as_str()?;
    (tilo_content::valid_hash(hash) && (kind == TEXT || kind == BYTES)).then_some((hash, kind))
}

fn collect(value: &Value, hashes: &mut Vec<String>) {
    match value {
        Value::Object(object) => match referenced(object) {
            Some((hash, _)) => hashes.push(hash.to_owned()),
            None => object.values().for_each(|item| collect(item, hashes)),
        },
        Value::Array(items) => items.iter().for_each(|item| collect(item, hashes)),
        _ => {}
    }
}

fn internalize(
    value: &mut Value,
    read: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> Result<(), MissingContent> {
    match value {
        Value::Object(object) => {
            if let Some((hash, kind)) = referenced(object) {
                let missing = || MissingContent(hash.to_owned());
                let bytes = read(hash)
                    .filter(|bytes| tilo_content::verify(&hash.to_ascii_lowercase(), bytes))
                    .ok_or_else(missing)?;
                *value = if kind == TEXT {
                    Value::String(String::from_utf8(bytes).map_err(|_| missing())?)
                } else {
                    Value::Array(bytes.into_iter().map(Value::from).collect())
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

    /// Long text and byte runs leave the payload as references and come
    /// back exactly; short values stay inline; a missing part holds.
    #[test]
    fn large_parts_are_kept_as_content_and_resolve_back() {
        let image: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();
        let original = action(&serde_json::json!({
            "paste": { "text": "short", "body": "x".repeat(20_000), "image": image },
            "count": 3,
        }));
        let (recorded, content) = original.with_content_parts(CONTENT_PART_THRESHOLD).unwrap();
        assert_eq!(content.len(), 2);
        assert!(recorded.payload.len() < 1024, "the payload names its parts");
        let parts = recorded.content_parts();
        assert_eq!(parts.len(), 2);
        assert!(parts.iter().all(|hash| content.get(hash).is_some()));
        let resolved = recorded
            .resolve_content(&|hash| content.get(hash).map(<[u8]>::to_vec))
            .unwrap()
            .unwrap();
        let back: Value = serde_json::from_slice(&resolved.payload).unwrap();
        let want: Value = serde_json::from_slice(&original.payload).unwrap();
        assert_eq!(back, want);
        assert_eq!(
            recorded.resolve_content(&|_| None).unwrap(),
            Err(MissingContent(parts[0].clone()))
        );
        // Other bytes than the hash names are not the content.
        assert!(
            recorded
                .resolve_content(&|_| Some(vec![1]))
                .unwrap()
                .is_err()
        );
    }

    #[test]
    fn the_reserved_key_is_refused_in_a_payload() {
        let forged = action(&serde_json::json!({ CONTENT_REFERENCE_KEY: "x", "as": "text" }));
        assert!(matches!(
            forged.with_content_parts(CONTENT_PART_THRESHOLD),
            Err(ActionError::Refused(_))
        ));
    }
}
