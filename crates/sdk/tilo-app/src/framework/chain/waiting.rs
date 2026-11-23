//! The actions that wait for the part they change to arrive, and the list of
//! them the app's windows share: a device back from away writes into the part
//! once the workspace sends it, after what waited before, and shows meanwhile
//! what it is sending.

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use tilo_actions::{ActionError, ActionInvoker};
use ui_core::automerge::transaction::Transactable as _;
use ui_core::automerge::{ObjType, ROOT, ReadDoc as _, Value};

use super::{ChainCore, ChainState, PART_ARRIVING, Write, answer};

/// Workspace writes held by the device where they were requested.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct QueuedWrites {
    #[serde(default)]
    pub(super) workspaces: BTreeMap<String, BTreeMap<String, Vec<QueuedWrite>>>,
    /// The entries of each workspace's and chain's record of waiting writes
    /// on the person's devices that moved into [`Self::workspaces`], so none
    /// moves twice.
    #[serde(default)]
    pub(super) taken: BTreeMap<String, BTreeMap<String, BTreeSet<String>>>,
}

impl settings_model::SettingsSchema for QueuedWrites {
    const SCOPE: settings_model::Scope = settings_model::Scope::DeviceLocal;
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct QueuedWrite {
    pub(super) id: String,
    pub(super) action: serde_json::Value,
    pub(super) invoker: ActionInvoker,
}

/// Actions that wait for the part they change to arrive, as the list of
/// them keeps them: `id` names their entry there.
pub(super) struct Unsent {
    id: String,
    /// What waits: one action, or every member of a group invoked as one.
    actions: Vec<Box<dyn Any>>,
    /// Whether they were invoked as one group, written as one step.
    group: bool,
    invoker: ActionInvoker,
}

impl ChainCore {
    /// Keep `action`, an action's JSON, invoked by `invoker`, with the
    /// waiting actions, after the others. Its entry there.
    pub(super) fn keep_unsent(
        &self,
        action: &[u8],
        invoker: &ActionInvoker,
    ) -> Result<String, ActionError> {
        let action: serde_json::Value =
            serde_json::from_slice(action).map_err(ActionError::Encode)?;
        self.keep_unsent_value(action, invoker)
    }

    /// Keep `members`, the actions of one group invoked as one, with the
    /// waiting actions, after the others, as the one step they are. Its entry
    /// there.
    pub(super) fn keep_unsent_group(
        &self,
        members: &[Box<dyn Any>],
        invoker: &ActionInvoker,
    ) -> Result<String, ActionError> {
        let mut actions = Vec::with_capacity(members.len());
        for member in members {
            let encoded = (self.part.to_json)(&**member)?;
            actions.push(
                serde_json::from_slice::<serde_json::Value>(&encoded)
                    .map_err(ActionError::Encode)?,
            );
        }
        self.keep_unsent_value(serde_json::Value::Array(actions), invoker)
    }

    /// Keep `action` — one action, or a group's list of members — with the
    /// waiting actions, after the others. Its entry there.
    fn keep_unsent_value(
        &self,
        action: serde_json::Value,
        invoker: &ActionInvoker,
    ) -> Result<String, ActionError> {
        if let Some(settings) = &self.workspace.device_queue {
            let id = format!("local:{}", tilo_resource::action::fresh_uuid());
            settings
                .update(|queue| {
                    queue
                        .workspaces
                        .entry(self.workspace_key.clone())
                        .or_default()
                        .entry(self.chain.as_str().to_owned())
                        .or_default()
                        .push(QueuedWrite {
                            id: id.clone(),
                            action,
                            invoker: invoker.clone(),
                        });
                })
                .map_err(|error| ActionError::Refused(error.to_string()))?;
            return Ok(id);
        }
        // A chain with no device queue (an import's, a tool's, a test's) keeps
        // the waiting write in the app's record of them, as every chain did
        // before the queue was kept on the device.
        if !self.open_unsent(true) {
            return Err(ActionError::Refused(PART_ARRIVING.into()));
        }
        let entry = serde_json::to_string(&(action, invoker)).map_err(ActionError::Encode)?;
        let chain = self.chain.as_str().to_owned();
        self.edit_unsent(move |doc| {
            let list = match doc.get(ROOT, chain.as_str())? {
                Some((Value::Object(ObjType::List), list)) => list,
                _ => doc.put_object(ROOT, chain.as_str(), ObjType::List)?,
            };
            doc.insert(&list, doc.length(&list), entry)?;
            let kept = doc.get(&list, doc.length(&list).saturating_sub(1))?;
            Ok(kept.map(|(_, id)| id.to_string()).unwrap_or_default())
        })
        .map_err(ActionError::Refused)
    }
}

impl ChainState {
    /// Apply the actions that wait for their part, in order, while their part
    /// is here: what is still on its way waits on, and the ones after it too.
    /// Each leaves the list of them once written or refused.
    pub(super) fn apply_pending(&self) {
        // Writing one refreshes the chain, which comes back here.
        if self.draining.replace(true) {
            return;
        }
        for entry in self.unsent() {
            let written = if entry.group {
                match self.try_apply_group(&entry.actions, &entry.invoker) {
                    Ok(Write::Arriving) => break,
                    Ok(Write::Done(written, _)) => Ok(written.last()),
                    Err(error) => Err(error),
                }
            } else {
                let Some(action) = entry.actions.first() else {
                    self.core.forget_unsent(&entry.id);
                    continue;
                };
                match self.try_apply(&**action, &entry.invoker) {
                    Ok(Write::Arriving) => break,
                    Ok(Write::Done(written, _)) => Ok(written.last()),
                    Err(error) => Err(error),
                }
            };
            self.core.forget_unsent(&entry.id);
            answer(self.part().kind, &entry.id, written);
        }
        self.draining.set(false);
        if self.follow_unsent() {
            self.core.moved();
        }
    }

    pub(super) fn read_unwritten(&self, unsent: &[Unsent]) -> Option<Rc<dyn Any>> {
        if unsent.is_empty() {
            return None;
        }
        let part = self.part();
        let mut doc = ui_core::automerge::AutoCommit::new();
        (part.initialize)(&mut doc).ok()?;
        let base = (self.base)();
        // As held, without a refresh: this is read while one runs.
        let parts = (part.typed)(&self.held());
        for entry in unsent {
            let members: Vec<&dyn Any> = entry.actions.iter().map(|action| &**action).collect();
            let Ok(prepared) = (part.prepare)(&members, &*base, &*parts, &entry.invoker) else {
                continue;
            };
            for action in &prepared {
                let Ok((action, encoded)) = (part.written)(&**action) else {
                    continue;
                };
                self.core
                    .show_unsent(&mut doc, action, encoded, &entry.invoker);
            }
        }
        (part.state)(&doc)
    }

    /// The actions waiting for a part of this chain, oldest first.
    pub(super) fn unsent(&self) -> Vec<Unsent> {
        let part = self.part();
        self.core
            .unsent()
            .into_iter()
            .filter_map(|(id, action, invoker)| {
                let (actions, group) = match action {
                    serde_json::Value::Array(members) => {
                        let mut actions = Vec::with_capacity(members.len());
                        for member in members {
                            match (part.from_json)(member) {
                                Ok(action) => actions.push(action),
                                Err(error) => {
                                    tracing::warn!(kind = part.kind, %error, "a waiting group member does not read");
                                    return None;
                                }
                            }
                        }
                        (actions, true)
                    }
                    action => {
                        let action = (part.from_json)(action)
                            .map_err(|error| {
                                tracing::warn!(kind = part.kind, %error, "a waiting write does not read");
                            })
                            .ok()?;
                        (vec![action], false)
                    }
                };
                Some(Unsent {
                    id,
                    actions,
                    group,
                    invoker,
                })
            })
            .collect()
    }
}
