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

use tilo_resource::{Invoke, Replica, Resource};
use tilo_server_protocol::servers::person_workspace_id;

use super::Workspaces;

/// The first change of the resource `R` in `workspace`: made at time zero
/// under an actor derived from the resource's identity alone
/// ([`tilo_resource::genesis::actor`]), so every device makes the same change.
///
/// # Errors
/// The resource's initializer fails, which is a bug in it.
pub fn genesis<R: Resource>(workspace: &str) -> Result<automerge::AutoCommit, String> {
    let mut doc = automerge::AutoCommit::new()
        .with_actor(tilo_resource::genesis::actor("", R::KIND, workspace));
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
    person_workspace_id(&workspaces.serving_certificate().user_public_key)
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
    let cert = workspaces.serving_certificate().clone();
    let start = Box::new(genesis::<R>(workspace)?);
    let applied = Box::pin(workspaces.author_workspace_document_from(
        (workspace, &cert),
        (app, name),
        Some(start),
        |doc| {
            let mut replica = Replica::<R>::from_doc(std::mem::take(doc));
            // This device's own actor: the genesis is one actor's change
            // already, and two devices continuing it under that actor would
            // make one sequence with two different changes.
            replica
                .doc_mut()
                .set_actor(automerge::ActorId::from(cert.device_public_key.to_vec()));
            let applied = replica
                .apply(action)
                .map(drop)
                .map_err(|error| error.to_string());
            std::mem::swap(doc, replica.doc_mut());
            applied
        },
    ))
    .await
    .map_err(|error| error.to_string());
    if applied.is_ok() {
        workspaces.note_document_write(workspace, app, name);
    }
    applied
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
