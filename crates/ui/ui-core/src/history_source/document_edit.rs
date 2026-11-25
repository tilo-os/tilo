//! The store's history verbs on a document no app's types read: a person's record, a routed
//! record, the host's own document. An app's resource takes the same verbs through its own
//! types, so a restore there is checked against what the app reads; here the document is taken
//! as it is stored.

use super::ThreadWords;
use super::verbs::{EDIT_TYPE, HistoryEdit, edit_from_payload};

/// A registry for a document no app's types read: it decodes none of their actions.
const fn decodes_no_app_action(_: &mut tilo_actions::ActionRegistry) {}

/// One history verb, as the executor runs it on a document without app types.
#[derive(Debug)]
pub struct DocumentHistoryEdit {
    edit: HistoryEdit,
    /// What fills the registry the document's recorded actions are decoded with.
    register: fn(&mut tilo_actions::ActionRegistry),
}

impl DocumentHistoryEdit {
    #[must_use]
    pub const fn new(edit: HistoryEdit) -> Self {
        Self::decoded_with(edit, decodes_no_app_action)
    }

    /// `edit`, taking back what the document recorded as the actions `register` teaches a
    /// registry to decode: a resource's own, for a document of that resource.
    #[must_use]
    pub const fn decoded_with(
        edit: HistoryEdit,
        register: fn(&mut tilo_actions::ActionRegistry),
    ) -> Self {
        Self { edit, register }
    }

    /// Teach `registry` to decode the history verbs a document records.
    pub fn register(registry: &mut tilo_actions::ActionRegistry) {
        registry.register::<Self>(EDIT_TYPE);
    }

    /// The edit as its invocation carries it.
    ///
    /// # Panics
    /// Never: a history edit is plain data.
    #[must_use]
    #[expect(clippy::expect_used, reason = "a history edit is plain data")]
    pub fn encoded(&self) -> tilo_actions::EncodedAction {
        tilo_actions::EncodedAction {
            type_id: EDIT_TYPE.to_owned(),
            payload: serde_json::to_vec(&self.edit).expect("a history edit serializes"),
        }
    }
}

impl<'de> serde::Deserialize<'de> for DocumentHistoryEdit {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        let payload = serde_json::to_vec(&value).map_err(serde::de::Error::custom)?;
        Ok(Self::new(
            edit_from_payload(&payload).map_err(serde::de::Error::custom)?,
        ))
    }
}

impl tilo_actions::Action for DocumentHistoryEdit {
    fn change_kind(&self) -> tilo_actions::ChangeKind {
        self.edit.change_kind()
    }

    fn describe(&self, doc: &tilo_actions::Doc) -> Option<tilo_actions::Described> {
        Some(self.edit.described(doc))
    }

    fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), tilo_actions::ActionError> {
        self.edit.apply_registered(doc, self.register, &ThreadWords)
    }

    fn steps(&self, doc: &tilo_actions::Doc) -> Option<Vec<tilo_actions::ActionStep>> {
        self.edit
            .steps(doc)?
            .into_iter()
            .map(|edit| {
                let payload = serde_json::to_vec(&edit).ok()?;
                let action: Box<dyn tilo_actions::Action> =
                    Box::new(Self::decoded_with(edit, self.register));
                Some((action, payload))
            })
            .collect()
    }
}
