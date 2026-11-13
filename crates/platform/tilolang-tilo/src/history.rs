//! A workspace record's history, as every app's record has it (`collaboration::history_source`):
//! each change is a step of whoever made it, and Undo, Redo, History and the other history verbs
//! ([`verbs`]) are applied to the record as steps of their own.
//!
//! What here is the record's, wherever the record is kept: an app's instance in a tilolang host
//! (`instance::record`), and the session's own service over the workspace's records
//! ([`crate::records_service`]) publish the same verbs on an app's resource ([`with_verbs`]) and
//! apply them as the same recorded action ([`HostHistoryEdit`]), so a run is undone and listed
//! the same way whether or not an app's process holds the record.

use collaboration::history_source::authors::{
    HistoryAuthor, ResolveAuthor, UNKNOWN_AUTHOR_NAME, actor_handle, short_id, stable_hue,
};
use collaboration::history_source::verbs::{self, EDIT_TYPE, HistoryEdit};
use collaboration::history_source::words::HistoryWords;
use tilo_actions::{ActionContext, ActionRegistry, EncodedAction, ResourceSpec};

use crate::action::{ACTION_TYPE, DynAction};

/// `resource` (an app's own) with the history verbs of its record, those it does not offer
/// itself.
#[must_use]
pub fn with_verbs(mut resource: ResourceSpec) -> ResourceSpec {
    for spec in verbs::specs() {
        if !resource
            .actions
            .iter()
            .any(|existing| existing.id == spec.id)
        {
            resource = resource.with_action(spec);
        }
    }
    resource
}

/// What a tilolang host's history verbs say, without the toolkit: a step as the platform's
/// history sentence in `locale`, a person by the handle their changes carry. The UI half renders
/// History in the reader's own words.
#[derive(Clone, Debug)]
pub struct HostWords {
    locale: String,
}

impl HostWords {
    /// The words of `locale` (`en-US`, `de-DE`).
    #[must_use]
    pub fn new(locale: impl Into<String>) -> Self {
        Self {
            locale: locale.into(),
        }
    }
}

impl ResolveAuthor for HostWords {
    fn resolve(&self, actor_hex: &str) -> HistoryAuthor {
        let name = actor_handle(actor_hex).unwrap_or_else(|| UNKNOWN_AUTHOR_NAME.to_owned());
        HistoryAuthor::new(name, stable_hue(actor_hex), short_id(actor_hex))
    }
}

impl HistoryWords for HostWords {
    fn sentence(&self, entry: &ActionContext) -> String {
        tilo_history::Reader::new(&self.locale).sentence(entry)
    }
}

/// A history verb applied to a workspace record: the edit [`verbs`] applies, decoding the record's
/// recorded actions with the workspace's registry ([`register`]) and speaking the record's words.
#[derive(Debug)]
pub struct HostHistoryEdit {
    edit: HistoryEdit,
    words: HostWords,
}

impl HostHistoryEdit {
    /// The verb `edit`, speaking `words`.
    #[must_use]
    pub const fn new(edit: HistoryEdit, words: HostWords) -> Self {
        Self { edit, words }
    }

    /// The recorded form of the verb: what the record's step carries, replayed by any replica.
    ///
    /// # Errors
    /// The verb does not encode.
    pub fn encoded(&self) -> Result<EncodedAction, String> {
        Ok(EncodedAction {
            type_id: EDIT_TYPE.to_owned(),
            payload: serde_json::to_vec(&self.edit).map_err(|error| error.to_string())?,
        })
    }
}

impl<'de> serde::Deserialize<'de> for HostHistoryEdit {
    /// A history verb a peer recorded, replayed here: what it says is never shown, so it speaks
    /// the platform's source language.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self {
            edit: HistoryEdit::deserialize(deserializer)?,
            words: HostWords::new("en-US"),
        })
    }
}

impl tilo_actions::Action for HostHistoryEdit {
    fn change_kind(&self) -> tilo_actions::ChangeKind {
        self.edit.change_kind()
    }

    fn describe(&self, doc: &tilo_actions::Doc) -> Option<tilo_actions::Described> {
        Some(self.edit.described(doc))
    }

    fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), tilo_actions::ActionError> {
        self.edit.apply_registered(doc, register, &self.words)
    }
}

/// Register what a workspace record's changes record: the runtime's planned actions and the history
/// verbs.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<DynAction>(ACTION_TYPE);
    registry.register::<HostHistoryEdit>(EDIT_TYPE);
}
