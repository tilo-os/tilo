//! [`DynAction`]: a tilolang action as a replayable tilo action.
//!
//! At the origin a tilolang action runs in wasm as a planner; its writes are recorded as
//! [`DynEdit`]s. The recorded action is plain data whose `apply` replays those edits without
//! wasm, so a peer that never loaded the module still verifies and applies the change
//! (implementation design 7.3). It is committed through the framework executor, so the change
//! carries the normal `ActionContext`: invoker, group, encoded action. Undo and history work as
//! for any app.

use automerge::transaction::Transactable;
use automerge::{ObjType, ROOT, ReadDoc, Value as AmValue};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use tilo_actions::{
    Action, ActionError, ActionFacet, ActionKey, ActionRegistry, Described, Did, Doc,
    EncodedAction, Scalar,
};
use tilo_resource::{ActionCx, Edit, Fields, Model, ModelError, Slot};

use crate::record::DynRecord;

/// The registry type id every [`DynAction`] is encoded under.
pub const ACTION_TYPE: &str = "tl.action";

/// The key of the map in the room record that holds every tilolang resource.
pub const RECORDS_KEY: &str = "tl";

/// The facet naming the authorship chain of a change (spec 4.3, point 10).
pub const AUTHORSHIP_FACET: &str = "tl.via";

/// The action id of a bound text input changing its field.
pub const EDIT_TEXT_ACTION: &str = "edit-text";

/// The action id of a bound checkbox changing its field.
pub const EDIT_FLAG_ACTION: &str = "edit-flag";

/// Who is behind the code that planned an action.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Authorship {
    /// The person, agent task or app the code ran for.
    pub invoker: String,
    /// The content hashes of the code authors, outermost first: the module that wrote the
    /// expression that invoked the action, then the modules it passed through.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chain: Vec<String>,
    /// The module hash that declares the action.
    pub module: String,
}

impl Authorship {
    /// The facets that carry this chain in the change's `ActionContext`, so history can say
    /// "you, via Alex's view".
    #[must_use]
    pub fn facets(&self) -> Vec<ActionFacet> {
        self.chain
            .iter()
            .map(|module| ActionFacet::new(AUTHORSHIP_FACET, module.as_str()))
            .collect()
    }
}

/// One write of a planned action, addressed to a resource of the action's app.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum DynEdit {
    /// A new resource: `record` is its JSON form ([`DynRecord::to_json`]), `$type` included.
    Create { id: String, record: Json },
    /// A field edit below the resource `id`; the edit's path starts at a field of the record.
    Edit { id: String, edit: Edit<DynRecord> },
    /// Remove the resource.
    Remove { id: String },
    /// An edit of a resource of another app than the action's: the host's own `Invocation`
    /// records, which an action's follow-up changes write beside the package's resources.
    In { app: String, edit: Box<DynEdit> },
}

impl DynEdit {
    /// The same edit, addressed to the resources of `app`.
    #[must_use]
    pub fn in_app(self, app: &str) -> Self {
        Self::In {
            app: app.to_owned(),
            edit: Box::new(self),
        }
    }
}

/// A tilolang action as recorded: what was invoked, by whom, and exactly what it wrote.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DynAction {
    pub app: String,
    /// The resource kind the action is published on (`tl.<app>.<Type>`).
    pub kind: String,
    pub action_id: String,
    /// The invocation arguments, as plain JSON, for history and agents.
    pub args: Json,
    pub authorship: Authorship,
    pub edits: Vec<DynEdit>,
}

fn refused(error: &ModelError) -> ActionError {
    ActionError::Refused(error.to_string())
}

/// The map under `key` of `parent`, made when absent.
fn ensure_map(
    doc: &mut Doc,
    parent: &automerge::ObjId,
    key: &str,
) -> Result<automerge::ObjId, ActionError> {
    if let Some((AmValue::Object(ObjType::Map), id)) = doc.get(parent, key)? {
        return Ok(id);
    }
    Ok(doc.put_object(parent, key, ObjType::Map)?)
}

impl DynAction {
    /// Encode under [`ACTION_TYPE`], the framework's action encoding.
    ///
    /// # Errors
    /// The action does not serialize.
    pub fn encode(&self) -> Result<EncodedAction, ActionError> {
        EncodedAction::encode(ACTION_TYPE, self)
    }

    /// The resource ids this action writes, in order of first mention.
    #[must_use]
    pub fn resources(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = Vec::new();
        for edit in &self.edits {
            let mut edit = edit;
            while let DynEdit::In { edit: inner, .. } = edit {
                edit = inner;
            }
            let (DynEdit::Create { id, .. } | DynEdit::Edit { id, .. } | DynEdit::Remove { id }) =
                edit
            else {
                continue;
            };
            if !ids.contains(&id.as_str()) {
                ids.push(id.as_str());
            }
        }
        ids
    }
}

/// Apply `edits` to the resources of `app` in `doc`, creating the maps they live in.
///
/// `seed` pins the ids a minting edit would draw: edits carry their own ids, so it only has to
/// be the same on every replica.
///
/// # Errors
/// An edit names a resource that is not there, or does not fit its field.
pub fn apply_edits(
    doc: &mut Doc,
    app: &str,
    edits: &[DynEdit],
    seed: &[u8],
) -> Result<(), ActionError> {
    if edits.is_empty() {
        return Ok(());
    }
    let mut cx = ActionCx::for_write(doc, seed);
    let records = ensure_map(doc, &ROOT, RECORDS_KEY)?;
    for edit in edits {
        apply_one(doc, &records, app, edit, &mut cx)?;
    }
    Ok(())
}

fn apply_one(
    doc: &mut Doc,
    records: &automerge::ObjId,
    app: &str,
    edit: &DynEdit,
    cx: &mut ActionCx,
) -> Result<(), ActionError> {
    if let DynEdit::In { app, edit } = edit {
        return apply_one(doc, records, app, edit, cx);
    }
    let app = ensure_map(doc, records, app)?;
    match edit {
        DynEdit::Create { id, record } => {
            let record = DynRecord::from_json(record).map_err(|error| refused(&error))?;
            record
                .store(doc, Slot::Key(&app, id))
                .map_err(|error| refused(&error))?;
        }
        DynEdit::Edit { id, edit } => {
            let Some((AmValue::Object(ObjType::Map), obj)) = doc.get(&app, id.as_str())? else {
                return Err(ActionError::Refused(format!("there is no resource `{id}`")));
            };
            DynRecord::edit_fields(doc, &obj, edit.path(), edit.op(), cx)
                .map_err(|error| refused(&error))?;
        }
        DynEdit::Remove { id } => {
            if doc.get(&app, id.as_str())?.is_some() {
                doc.delete(&app, id.as_str())?;
            }
        }
        DynEdit::In { .. } => {}
    }
    Ok(())
}

impl Action for DynAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let payload =
            serde_json::to_vec(self).map_err(|error| ActionError::Refused(error.to_string()))?;
        apply_edits(doc, &self.app, &self.edits, &payload)
    }

    fn describe(&self, _doc: &Doc) -> Option<Described> {
        let args = self
            .args
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(name, value)| Some((name.clone(), Scalar::from_json(value)?)));
        let key = ActionKey::new(
            self.app.as_str(),
            self.kind.as_str(),
            self.action_id.as_str(),
        );
        let described = Described::new(Did::verb(key, args));
        Some(match self.resources().first() {
            Some(id) => described.on(*id, *id),
            None => described,
        })
    }
}

/// Register the decoder of [`DynAction`], so a replaying peer reconstructs it with no wasm.
pub fn register_actions(registry: &mut ActionRegistry) {
    registry.register::<DynAction>(ACTION_TYPE);
}

/// The records map of `app` in `doc`, when anything was written there.
#[must_use]
pub fn app_records(doc: &Doc, app: &str) -> Option<automerge::ObjId> {
    let (AmValue::Object(ObjType::Map), records) = doc.get(&ROOT, RECORDS_KEY).ok()?? else {
        return None;
    };
    match doc.get(&records, app).ok()?? {
        (AmValue::Object(ObjType::Map), id) => Some(id),
        _ => None,
    }
}

/// Read the resource `id` of `app` from `doc`.
///
/// # Errors
/// The resource is absent, or its type is not registered.
pub fn read_resource(doc: &Doc, app: &str, id: &str) -> Result<DynRecord, ModelError> {
    let Some(records) = app_records(doc, app) else {
        return Err(ModelError::Missing {
            path: app.to_owned(),
        });
    };
    match doc.get(&records, id)? {
        Some((AmValue::Object(ObjType::Map), obj)) => DynRecord::read_fields(doc, &obj),
        _ => Err(ModelError::Missing {
            path: id.to_owned(),
        }),
    }
}

/// The ids of every resource of `app` in `doc`, in key order.
#[must_use]
pub fn resource_ids(doc: &Doc, app: &str) -> Vec<String> {
    app_records(doc, app)
        .map(|records| doc.keys(&records).collect())
        .unwrap_or_default()
}
