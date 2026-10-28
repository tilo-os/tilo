//! The [`ActionRegistry`]: `type_id` → decoder, so a replaying peer can
//! turn an [`EncodedAction`] back into a live `Box<dyn Action>` without
//! knowing the concrete type at the call site. An app registers each
//! action type once; the registry is the same seam the OS-wide
//! global-search backend plugs into to enumerate action *types* across apps.

use std::collections::HashMap;

use serde::de::DeserializeOwned;

use crate::action::{Action, ActionError, EncodedAction};

type Decoder = Box<dyn Fn(&[u8]) -> Result<Box<dyn Action>, ActionError> + Send + Sync>;
type ContentReader = Box<dyn Fn(&str) -> Option<Vec<u8>> + Send + Sync>;

/// Decodes one action type's payload ([`ActionRegistry::register_decoder`]).
pub type DecodePayload = fn(&[u8]) -> Result<Box<dyn Action>, serde_json::Error>;

/// Maps a stable `type_id` to the decoder that reconstructs its action
/// from a payload — cheap to share behind an `Arc`/`&` at dispatch time.
#[derive(Default)]
pub struct ActionRegistry {
    decoders: HashMap<String, Decoder>,
    /// Where the large parts of a recorded action are read from by hash
    /// ([`crate::content`]): the content store of the device replaying.
    content: Option<ContentReader>,
}

impl ActionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry holding what `register` adds, such as an app's action set.
    #[must_use]
    pub fn with(register: impl FnOnce(&mut Self)) -> Self {
        let mut registry = Self::new();
        register(&mut registry);
        registry
    }

    /// Register `A` under `type_id`; a later [`decode`](Self::decode) of an
    /// [`EncodedAction`] carrying this `type_id` deserializes the payload
    /// into an `A` and boxes it as `dyn Action`.
    pub fn register<A>(&mut self, type_id: impl Into<String>)
    where
        A: Action + DeserializeOwned + 'static,
    {
        let type_id = type_id.into();
        let tid = type_id.clone();
        self.decoders.insert(
            type_id,
            Box::new(move |bytes: &[u8]| {
                let action: A = decode_json(bytes).map_err(|e| ActionError::Decode {
                    type_id: tid.clone(),
                    source: e,
                })?;
                Ok(Box::new(action) as Box<dyn Action>)
            }),
        );
    }

    /// Register `decode` under `type_id`, for an action type that decodes
    /// its payload itself, such as one generic type whose payload one
    /// non-generic function decodes for every instance.
    pub fn register_decoder(&mut self, type_id: impl Into<String>, decode: DecodePayload) {
        let type_id = type_id.into();
        let tid = type_id.clone();
        self.decoders.insert(
            type_id,
            Box::new(move |bytes: &[u8]| {
                decode(bytes).map_err(|e| ActionError::Decode {
                    type_id: tid.clone(),
                    source: e,
                })
            }),
        );
    }

    /// Reconstruct a live action from its encoded form.
    ///
    /// # Errors
    ///
    /// Returns [`ActionError::UnknownType`] if no decoder is registered
    /// for `encoded.type_id`, or [`ActionError::Decode`] if the payload
    /// fails to deserialize into the registered type.
    pub fn decode(&self, encoded: &EncodedAction) -> Result<Box<dyn Action>, ActionError> {
        let decoder = self
            .decoders
            .get(&encoded.type_id)
            .ok_or_else(|| ActionError::UnknownType(encoded.type_id.clone()))?;
        decoder(&crate::group_form::readable(
            &encoded.type_id,
            &encoded.payload,
        ))
    }

    /// Read the large parts of recorded actions from `read` (the device's
    /// content store), so a change whose action names content replays.
    /// Without one, the thread's reader ([`crate::keep_content_in`]) is used.
    pub fn set_content(&mut self, read: impl Fn(&str) -> Option<Vec<u8>> + Send + Sync + 'static) {
        self.content = Some(Box::new(read));
    }

    /// The recorded action `recorded` as it ran, its content parts read
    /// from this registry's content reader.
    ///
    /// # Errors
    /// [`MissingContent`](crate::MissingContent) for content not here yet
    /// (inside `Ok`), or a payload that is not JSON.
    pub fn resolve(
        &self,
        recorded: &EncodedAction,
    ) -> Result<Result<EncodedAction, crate::MissingContent>, ActionError> {
        if recorded.content_parts().is_empty() {
            return Ok(Ok(recorded.clone()));
        }
        let here = crate::content::read_here;
        let read: &dyn Fn(&str) -> Option<Vec<u8>> = match &self.content {
            Some(read) => read.as_ref(),
            None => &here,
        };
        recorded.resolve_content(read)
    }

    /// Whether a decoder is registered for `type_id`.
    pub fn contains(&self, type_id: &str) -> bool {
        self.decoders.contains_key(type_id)
    }

    /// Every registered action type id (the general-scope enumeration seam).
    pub fn type_ids(&self) -> Vec<&str> {
        self.decoders.keys().map(String::as_str).collect()
    }
}

/// A `T` from its JSON bytes, through a JSON value: one byte parser for
/// every action type, compiled here, rather than one per type and crate.
///
/// # Errors
/// Why the bytes are not a `T`.
#[inline]
pub fn decode_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, serde_json::Error> {
    decode_payload("", bytes)
}

/// A `T` from the payload of an action of type `type_id`, like
/// [`decode_json`]: a group written with what its members share once
/// ([`crate::group_form`]) reads as the list of them.
pub fn decode_payload<T: DeserializeOwned>(
    type_id: &str,
    payload: &[u8],
) -> Result<T, serde_json::Error> {
    T::deserialize(payload_value(type_id, payload)?)
}

/// The JSON value of the payload of an action of type `type_id`, a group's
/// as the list of its members; [`decode_json`] passes no type, so its bytes
/// read as they are. One function for both keeps every app to one import.
#[inline(never)]
fn payload_value(type_id: &str, payload: &[u8]) -> Result<serde_json::Value, serde_json::Error> {
    match serde_json::from_slice(payload)? {
        serde_json::Value::Object(shared) if crate::group_form::is_group(type_id) => {
            crate::group_form::members_of(shared)
                .map(serde_json::Value::Array)
                .map_err(<serde_json::Error as serde::de::Error>::custom)
        }
        value => Ok(value),
    }
}
