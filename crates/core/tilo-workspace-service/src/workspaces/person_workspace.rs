//! Typed resources of the person's workspace.
//!
//! The person's workspace is an ordinary workspace whose only member is the
//! person, and each of their devices holds it
//! ([`person_workspace_id`](tilo_server_protocol::servers::person_workspace_id)).
//! What is theirs alone is a resource in it: their servers
//! ([`super::servers::SERVERS`]) and whatever else follows them from device to
//! device. Each is a [`tilo_resource::Resource`], changed only through its
//! typed actions, so every change has an author and a history, and it reaches
//! the person's other devices as any workspace resource does.
//!
//! Every device that writes a resource before the workspace's copy of it
//! reached it starts from the same genesis ([`genesis`]), so two first writes
//! merge into one resource rather than competing for its root.
//!
//! The device that makes a person's root founds the workspace ([`found`]). Its
//! one member is the person, so it admits every later device their root
//! certified as every workspace admits a member's devices: the device proves
//! it is the person's where the workspace is kept, and keeps it from then on.

use tilo_resource::{Invoke, Replica, Resource};
use tilo_server_protocol::servers::person_workspace_id;

use super::Workspaces;

pub mod accessory;
pub mod devices;
pub mod speaker;
pub mod wake;
pub mod wifi;

/// The name the person's workspace is made with.
pub const NAME: &str = "Servers";

/// Whether this device holds its person's workspace: the workspace names it
/// among its holders, whether it founded the workspace or was taken in.
pub async fn holds_person_workspace(workspaces: &Workspaces) -> bool {
    holds_person_workspace_of(workspaces, &workspaces.device_cert().user_public_key).await
}

/// Whether this device holds `person`'s workspace: the workspace names this device among its
/// holders.
pub async fn holds_person_workspace_of(workspaces: &Workspaces, person: &[u8; 32]) -> bool {
    let own = workspaces.device_cert();
    let Some(state) = workspaces.state(&person_workspace_id(person)).await else {
        return false;
    };
    state
        .governance()
        .keeping_devices()
        .iter()
        .any(|hint| hint.host.device_public_key == own.device_public_key)
}

/// Found this device's person's workspace here, unless this device holds it
/// already, and answer its id. Only one device founds it: one whose person's
/// list says another device of theirs keeps it opens it there as a member
/// instead.
///
/// # Errors
/// Another device of the person keeps the workspace, or it could not be made.
pub async fn found(workspaces: &Workspaces) -> Result<String, String> {
    let person = workspaces.device_cert().user_public_key;
    let id = person_workspace_id(&person);
    if holds_person_workspace_of(workspaces, &person).await {
        return Ok(id);
    }
    may_found(workspaces, &person).await?;
    workspaces.create_for(NAME, Some(&id), person).await?;
    workspaces.list_known_for(&person).await;
    Ok(id)
}

/// Whether this device may found `person`'s workspace: not when their list
/// says another of their devices keeps it, where this one opens it.
///
/// # Errors
/// Another device of the person keeps the workspace.
pub async fn may_found(workspaces: &Workspaces, person: &[u8; 32]) -> Result<(), String> {
    let id = person_workspace_id(person);
    if workspaces
        .kept_elsewhere(person)
        .await
        .iter()
        .any(|(workspace, _)| *workspace == id)
    {
        return Err(
            "your Servers workspace is kept on another of your devices; it reaches this one as \
             your other workspaces do"
                .into(),
        );
    }
    Ok(())
}

/// The first change of the resource `R` in `workspace`: made at time zero
/// under an actor derived from the resource's identity alone
/// ([`tilo_resource::genesis::actor`]), so every device makes the same change.
///
/// # Errors
/// The resource's initializer fails, which is a bug in it.
pub fn genesis<R: Resource>(workspace: &str) -> Result<automerge::AutoCommit, String> {
    let mut doc = automerge::AutoCommit::new().with_actor(tilo_resource::genesis::actor(
        "",
        R::KIND,
        workspace,
    ));
    R::initialize(&mut doc).map_err(|error| error.to_string())?;
    doc.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Genesis.change_message())
            .with_time(0),
    );
    Ok(doc)
}

/// The id of this device's person's workspace.
#[must_use]
pub fn workspace_id(workspaces: &Workspaces) -> String {
    person_workspace_id(&workspaces.device_cert().user_public_key)
}

/// Apply `action` to the resource `(app, name)` of this device's person's
/// workspace, as this device.
///
/// # Errors
/// This device holds no copy of the workspace, the action was refused, or the
/// resource could not be written.
pub async fn apply<R: Resource>(
    workspaces: &Workspaces,
    resource: (&str, &str),
    action: Invoke<R>,
) -> Result<(), String> {
    apply_on(workspaces, &workspace_id(workspaces), resource, action).await
}

/// [`apply`] on `workspace`, which is this device's person's workspace.
///
/// # Errors
/// As [`apply`].
pub async fn apply_on<R: Resource>(
    workspaces: &Workspaces,
    workspace: &str,
    (app, name): (&str, &str),
    action: Invoke<R>,
) -> Result<(), String> {
    if workspaces.is_member(workspace).await? {
        return apply_as_member(workspaces, workspace, (app, name), action).await;
    }
    let cert = workspaces.device_cert().clone();
    let start = Box::new(genesis::<R>(workspace)?);
    let applied = Box::pin(workspaces.author_workspace_document_from(
        (workspace, &cert),
        (app, name),
        Some(start),
        |doc| invoke(doc, &cert, app, action),
    ))
    .await
    .map_err(|error| error.to_string());
    if applied.is_ok() {
        workspaces.wrote(workspace, app, name);
    }
    applied
}

/// [`apply_on`] where this device holds `workspace` as a member copy another of the person's
/// devices keeps: the change is this device's own write, carried to the device keeping the
/// workspace and decided there as any member's write is.
///
/// # Errors
/// As [`apply`].
async fn apply_as_member<R: Resource>(
    workspaces: &Workspaces,
    workspace: &str,
    (app, name): (&str, &str),
    action: Invoke<R>,
) -> Result<(), String> {
    let cert = workspaces.device_cert().clone();
    let start = Box::new(genesis::<R>(workspace)?);
    edit_as_member(workspaces, workspace, (app, name), Some(start), |doc| {
        invoke(doc, &cert, app, action)
    })
    .await
}

/// [`apply_as_member`] for any edit a resource of the person's own workspace takes, typed or
/// not: `edit` changes `doc`, the record `(app, name)` as this device holds it, from `genesis`
/// where the workspace holds none yet, and the change is this device's own write, carried to
/// the device keeping the workspace.
///
/// # Errors
/// This device holds no copy of the workspace, the edit or the founding was refused, or the
/// write could not be kept.
pub(in crate::workspaces) async fn edit_as_member<F>(
    workspaces: &Workspaces,
    workspace: &str,
    (app, name): (&str, &str),
    genesis: Option<Box<automerge::AutoCommit>>,
    edit: F,
) -> Result<(), String>
where
    F: FnOnce(&mut automerge::AutoCommit) -> Result<(), String>,
{
    let (mut doc, heads) = match workspaces
        .resource_document(workspace, app, name)
        .await
        .map_err(|error| error.to_string())?
    {
        Some(bytes) => {
            let mut doc = automerge::AutoCommit::load(&bytes).map_err(|error| error.to_string())?;
            let heads = doc.get_heads();
            (doc, Some(heads))
        }
        None => (
            genesis.map_or_else(automerge::AutoCommit::new, |start| *start),
            None,
        ),
    };
    edit(&mut doc)?;
    if automerge::transaction::Transactable::pending_ops(&doc) != 0 {
        doc.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(tilo_actions::FrameworkChange::Native.change_message()),
        );
    }
    let founding = heads.is_none();
    if founding && !super::authored::founds_as_read(&doc, (app, name), workspace) {
        return Err(format!(
            "{app} {name} would be founded without the identity every reader of the \
             workspace's own records checks; it starts from its genesis"
        ));
    }
    let changes = match &heads {
        Some(heads) => doc.save_after(heads),
        None => doc.save(),
    };
    if changes.is_empty() {
        return Ok(());
    }
    // Boxed: committing a write can apply a change of the person's own workspace in turn.
    Box::pin(workspaces.own_write(
        workspace,
        (app, name),
        None,
        &changes,
        founding,
        founding,
        None,
    ))
    .await
    .map(drop)
    .map_err(|refusal| refusal.to_string())
}

/// Apply `action` to `doc`, a resource `R` of the app `app`, as `cert`'s person on
/// `cert`'s device.
///
/// # Errors
/// The action was refused.
pub(super) fn invoke<R: Resource>(
    doc: &mut automerge::AutoCommit,
    cert: &tilo_user_identity::DeviceCert,
    app: &str,
    action: Invoke<R>,
) -> Result<(), String> {
    let replica =
        Replica::<R>::from_doc(std::mem::take(doc)).as_invoker(&hex::encode(cert.user_public_key));
    // The record is the app's: every device decides the change against that app's surface, and
    // a change naming no app has one guessed from the kind's id, which the record's app does
    // not publish. This device's own actor is the genesis' already, and two devices continuing
    // it under that actor would make one sequence with two different changes.
    let mut replica = replica.in_app(app, &app_label::<R>(app));
    replica
        .doc_mut()
        .set_actor(automerge::ActorId::from(cert.device_public_key.to_vec()));
    let applied = replica
        .apply(action)
        .map(drop)
        .map_err(|error| error.to_string());
    std::mem::swap(doc, replica.doc_mut());
    applied
}

/// The name the app `app` shows in a record's history: the title `R`'s kind declares for it,
/// else the last segment of its id as a name.
fn app_label<R: Resource>(app: &str) -> String {
    if let Some(title) = tilo_resource::ResourceKind::of::<R>().app_title() {
        return title.to_owned();
    }
    let last = app.rsplit('.').next().unwrap_or(app);
    let mut chars = last.chars();
    chars
        .next()
        .map_or_else(String::new, |first| {
            first.to_uppercase().collect::<String>() + chars.as_str()
        })
}

/// The resource `(app, name)` of this device's person's workspace as this
/// device reads it: its initial state while the workspace has none.
///
/// # Errors
/// The resource could not be read.
pub async fn read<R: Resource>(
    workspaces: &Workspaces,
    resource: (&str, &str),
) -> Result<R::State, String> {
    read_on::<R>(workspaces, &workspace_id(workspaces), resource).await
}

/// [`read`] of `workspace`.
///
/// # Errors
/// As [`read`].
pub async fn read_on<R: Resource>(
    workspaces: &Workspaces,
    workspace: &str,
    (app, name): (&str, &str),
) -> Result<R::State, String> {
    let replica = match workspaces
        .resource_document(workspace, app, name)
        .await
        .map_err(|error| error.to_string())?
    {
        Some(bytes) => Replica::<R>::load(&bytes).map_err(|error| error.to_string())?,
        None => Replica::<R>::from_doc(genesis::<R>(workspace)?),
    };
    replica.state().map_err(|error| error.to_string())
}

/// The document `(app, name)` of this device's person's workspace, saved: `None`
/// while the workspace holds none of it yet.
///
/// A caller that keeps its own Automerge structure under a resource address
/// reads and edits it here rather than as a typed [`Resource`]: the workspace
/// still carries it to the person's other devices as any resource, and each
/// device's write has an author and a history.
///
/// # Errors
/// This device holds no copy of the workspace, or the document could not be read.
pub async fn document(
    workspaces: &Workspaces,
    resource: (&str, &str),
) -> Result<Option<Vec<u8>>, String> {
    document_on(workspaces, &workspace_id(workspaces), resource).await
}

/// [`document`] of `workspace`.
///
/// # Errors
/// As [`document`].
pub async fn document_on(
    workspaces: &Workspaces,
    workspace: &str,
    (app, name): (&str, &str),
) -> Result<Option<Vec<u8>>, String> {
    workspaces
        .resource_document(workspace, app, name)
        .await
        .map_err(|error| error.to_string())
}

/// Edit the document `(app, name)` of this device's person's workspace, as this
/// device: [`document`]'s counterpart.
///
/// # Errors
/// This device holds no copy of the workspace, the edit was refused, or the
/// document could not be written.
pub async fn edit_document<F>(
    workspaces: &Workspaces,
    resource: (&str, &str),
    edit: F,
) -> Result<(), String>
where
    F: FnOnce(&mut automerge::AutoCommit) -> Result<(), String>,
{
    edit_document_on(workspaces, &workspace_id(workspaces), resource, edit).await
}

/// [`edit_document`] on `workspace`, which is this device's person's workspace.
///
/// # Errors
/// As [`edit_document`].
pub async fn edit_document_on<F>(
    workspaces: &Workspaces,
    workspace: &str,
    (app, name): (&str, &str),
    edit: F,
) -> Result<(), String>
where
    F: FnOnce(&mut automerge::AutoCommit) -> Result<(), String>,
{
    if workspaces.is_member(workspace).await? {
        return edit_as_member(workspaces, workspace, (app, name), None, edit).await;
    }
    let cert = workspaces.device_cert().clone();
    let applied = workspaces
        .author_workspace_document_from((workspace, &cert), (app, name), None, edit)
        .await
        .map_err(|error| error.to_string());
    if applied.is_ok() {
        workspaces.wrote(workspace, app, name);
    }
    applied
}
