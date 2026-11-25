//! The workspace's agents, brought over from where builds before kept them.
//!
//! The list of a workspace's agents was once Chat's resource (`so.tilo.chat`,
//! kind `chat.agent`); it is the workspace's now ([`tilo_workspace_agent::OWNER`],
//! kind [`tilo_workspace_agent::KIND`]). The two are different documents, of
//! different apps under different ids, so no migration step inside one
//! document can carry the agents across. A workspace written before keeps them
//! in the old document, and nothing reads it any more.
//!
//! So the service that governs such a workspace copies them over once as it
//! opens it, before any member's session reaches it: every agent the old list
//! holds that the new one lacks goes into the new list as the governor's own
//! write, item for item with every field it held, through the governor's
//! normal authoring path, so members admit it as any write of the governor and
//! every device of the workspace takes the new list from there. Agents are
//! never taken out of the list (a retired one stays, marked so), so once the
//! new list holds each of them a later start finds nothing to copy, and the
//! old document is read for nothing else.
//!
//! A workspace with no list of agents yet under the workspace starts it with the
//! genesis every device makes of it ([`started`]), and the copy is made at time
//! zero by an actor derived from the heads both documents are at, so two
//! servers of one workspace that copy at once write the same changes, which
//! merge as one.
//!
//! History is not copied: the old document's changes name Chat's surface and
//! the old kind in their identity, and members decide every change of a
//! resource against its own app's surface, which no longer has that kind.

use automerge::transaction::{CommitOptions, Transactable as _};
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc as _, Value};
use collab_types::document::identity;
use collab_types::document::{DocumentId, ResourceType};
use tilo_resource::Resource as _;
use tilo_workspace_agent::WorkspaceAgents;

use super::Workspaces;

/// The app that kept the workspace's agents before the workspace did.
const FORMER_OWNER: &str = "so.tilo.chat";

/// The kind the workspace's agents were kept under before.
const FORMER_KIND: &str = "chat.agent";

/// The key the list of agents is kept under in both documents.
const AGENTS: &str = "agents";

impl Workspaces {
    /// Copy the agents every workspace governed here kept under Chat into
    /// the workspace's list of them. What could not be copied is logged and
    /// tried again when the service next opens.
    pub(super) async fn move_agent_rosters(&self) {
        let states: Vec<_> = self
            .inner
            .workspaces
            .lock()
            .await
            .values()
            .cloned()
            .collect();
        for state in states {
            let workspace = {
                let governed = state.lock().await;
                // A member's copy takes the list from the server that governs it.
                if governed.member.is_some() {
                    continue;
                }
                governed.workspace().to_owned()
            };
            if let Err(error) = Box::pin(self.move_agent_roster(&workspace)).await {
                tracing::error!(%workspace, %error, "the workspace's agents were not copied to the workspace's list of them");
            }
        }
    }

    /// [`Self::move_agent_rosters`] for `workspace`: how many agents it copied.
    pub(super) async fn move_agent_roster(&self, workspace: &str) -> Result<usize, String> {
        let former = collab_types::workspace_document_id(FORMER_KIND);
        let Some(bytes) = self
            .resource_document(workspace, FORMER_OWNER, &former)
            .await
            .map_err(|error| error.to_string())?
        else {
            return Ok(0);
        };
        let old = AutoCommit::load(&bytes).map_err(|error| error.to_string())?;
        if listed(&old).is_empty() {
            return Ok(0);
        }
        let list = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
        let governor = self.device_cert().clone();
        let home = workspace.to_owned();
        Box::pin(self.author_workspace_document(
            workspace,
            &governor,
            (tilo_workspace_agent::OWNER, &list),
            move |doc| copy_agents(&old, &home, doc),
        ))
        .await
        .map_err(|error| error.to_string())
    }

    /// Write `change` into the workspace's list of the agents of `workspace` as this host, onto the
    /// list as the workspace holds it. Where it holds none yet the list starts with the genesis
    /// every device makes of it ([`started`]): the identity every reader checks before it opens
    /// the list, the same change as the workspace that founds it elsewhere, so the two merge as
    /// one list.
    ///
    /// A verb `change` applies names the record's app (`Replica::in_app` with
    /// [`tilo_workspace_agent::OWNER`]): the record's receipt check reads the action as that
    /// app's, and a name guessed from its type is no app the record has, so every device would
    /// refuse the write.
    ///
    /// # Errors
    /// The genesis could not be made, or as [`Self::author_workspace_document_from`].
    pub async fn author_agents<R>(
        &self,
        workspace: &str,
        change: impl FnOnce(&mut AutoCommit) -> Result<R, String>,
    ) -> Result<R, String> {
        let list = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
        Box::pin(self.author_workspace_document_from(
            (workspace, self.device_cert()),
            (tilo_workspace_agent::OWNER, &list),
            // What is written onto it is this host's own change, never the genesis actor's.
            Some(Box::new(
                started(workspace)?.with_actor(automerge::ActorId::random()),
            )),
            change,
        ))
        .await
        .map_err(|error| error.to_string())
    }
}

/// The ids of the agents `doc` lists.
fn listed(doc: &AutoCommit) -> Vec<String> {
    match doc.get(ROOT, AGENTS) {
        Ok(Some((Value::Object(ObjType::Map), agents))) => doc.keys(&agents).collect(),
        _ => Vec::new(),
    }
}

/// Copy into `doc`, the workspace's list of the agents of `workspace` (empty
/// when the workspace holds none yet), every agent `old` lists that it lacks.
/// Returns how many it copied; it writes nothing when that is none.
///
/// # Errors
/// Either document does not read as a list of agents, or the copy could not be
/// written.
fn copy_agents(old: &AutoCommit, workspace: &str, doc: &mut AutoCommit) -> Result<usize, String> {
    let missing: Vec<String> = {
        let held = listed(doc);
        listed(old)
            .into_iter()
            .filter(|id| !held.contains(id))
            .collect()
    };
    if missing.is_empty() {
        return Ok(0);
    }
    if doc.get_heads().is_empty() {
        *doc = started(workspace)?;
    }
    let mut old_heads = old.clone().get_heads();
    old_heads.sort();
    let mut heads = doc.get_heads();
    heads.sort();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tilo.workspace.agent/from-chat.agent/v1");
    for head in old_heads.iter().chain(&heads) {
        hasher.update(&head.0);
    }
    let actor =
        automerge::ActorId::from(hasher.finalize().as_bytes().get(..16).unwrap_or_default());
    let writer = doc.get_actor().clone();
    doc.set_actor(actor);
    let copied = write_agents(old, doc, &missing).map_err(|error| error.to_string());
    if copied.is_ok() {
        // Committed here, at time zero, so every server that copies the same
        // agents onto the same list writes the same change.
        doc.commit_with(
            CommitOptions::default()
                .with_message(tilo_actions::FrameworkChange::Native.change_message())
                .with_time(0),
        );
    } else {
        doc.rollback();
    }
    doc.set_actor(writer);
    copied.and_then(|()| WorkspaceAgents::read(doc).map_err(|error| error.to_string()))?;
    Ok(missing.len())
}

/// Write each agent `ids` names, as `old` holds it, into `doc`'s list.
fn write_agents(
    old: &AutoCommit,
    doc: &mut AutoCommit,
    ids: &[String],
) -> Result<(), automerge::AutomergeError> {
    let Some((Value::Object(ObjType::Map), from)) = old.get(ROOT, AGENTS)? else {
        return Ok(());
    };
    let into = match doc.get(ROOT, AGENTS)? {
        Some((Value::Object(ObjType::Map), into)) => into,
        _ => doc.put_object(ROOT, AGENTS, ObjType::Map)?,
    };
    for id in ids {
        if let Some((value, item)) = old.get(&from, id.as_str())? {
            match value {
                Value::Scalar(scalar) => doc.put(&into, id.as_str(), scalar.into_owned())?,
                Value::Object(kind) => {
                    let copy = doc.put_object(&into, id.as_str(), kind)?;
                    copy_object(old, &item, doc, &copy)?;
                }
            }
        }
    }
    Ok(())
}

/// Copy everything `from` holds under `source` into `to`'s empty `target`, of
/// the same type.
fn copy_object(
    from: &AutoCommit,
    source: &ObjId,
    to: &mut AutoCommit,
    target: &ObjId,
) -> Result<(), automerge::AutomergeError> {
    match from.object_type(source)? {
        ObjType::Map | ObjType::Table => {
            for key in from.keys(source).collect::<Vec<_>>() {
                // Chat's lists were written under the old field name for the
                // person an agent acts for.
                let (from_key, into_key) = if key == "keeper" {
                    ("keeper", "acts_for")
                } else {
                    (key.as_str(), key.as_str())
                };
                match from.get(source, from_key)? {
                    Some((Value::Scalar(scalar), _)) => {
                        to.put(target, into_key, scalar.into_owned())?;
                    }
                    Some((Value::Object(kind), child)) => {
                        let copy = to.put_object(target, into_key, kind)?;
                        copy_object(from, &child, to, &copy)?;
                    }
                    None => {}
                }
            }
        }
        ObjType::List => {
            for index in 0..from.length(source) {
                match from.get(source, index)? {
                    Some((Value::Scalar(scalar), _)) => {
                        to.insert(target, index, scalar.into_owned())?;
                    }
                    Some((Value::Object(kind), child)) => {
                        let copy = to.insert_object(target, index, kind)?;
                        copy_object(from, &child, to, &copy)?;
                    }
                    None => {}
                }
            }
        }
        ObjType::Text => {
            let text = from.text(source)?;
            to.splice_text(target, 0, 0, &text)?;
        }
    }
    Ok(())
}

/// The workspace's list of the agents of `workspace` as it starts: the genesis
/// every device makes of it, so a device that later opens it finds the
/// document it would have made itself.
///
/// # Errors
/// The genesis could not be made.
fn started(workspace: &str) -> Result<AutoCommit, String> {
    let id = DocumentId::parse(&collab_types::workspace_document_id(
        tilo_workspace_agent::KIND,
    ))
    .ok_or("the list of agents has no document id")?;
    let kind = ResourceType::new(tilo_workspace_agent::KIND);
    let mut made = AutoCommit::new().with_actor(tilo_resource::genesis::actor(
        id.as_str(),
        kind.as_str(),
        workspace,
    ));
    if !identity::stamp_scoped(&mut made, &id, &kind, workspace, Some(workspace)) {
        return Err("the list of agents could not be named".to_owned());
    }
    WorkspaceAgents::initialize(&mut made).map_err(|error| error.to_string())?;
    let _ = identity::stamp_surface(&mut made);
    made.commit_with(
        CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Genesis.change_message())
            .with_time(0),
    );
    Ok(made)
}

#[cfg(test)]
#[path = "agent_roster_tests.rs"]
mod tests;
