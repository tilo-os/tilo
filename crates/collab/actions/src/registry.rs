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
                let action: A = serde_json::from_slice(bytes).map_err(|e| ActionError::Decode {
                    type_id: tid.clone(),
                    source: e,
                })?;
                Ok(Box::new(action) as Box<dyn Action>)
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
        decoder(&encoded.payload)
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
