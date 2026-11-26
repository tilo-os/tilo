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
pub mod connected;
pub mod device_answers;
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

/// How often [`when_held`] looks again while a change it did not hear of may be what admits this
/// device: a workspace's devices are read from what it holds as it is bound, which its governance
/// names no decision of.
const LOOK_AGAIN: std::time::Duration = std::time::Duration::from_millis(250);

/// Wait until this device holds its person's workspace: `false` when these workspaces stopped
/// first. The person's devices write into it before this device is taken in, and what they write
/// lands once it is ([`apply_when_held`]).
///
/// What admits this device is read again on each of the two moves that can change it — the
/// workspace taken into this device, and a governance decision on it — and every so often besides:
/// a workspace bound here is admitted to by the devices its governance names, which can be read
/// from the workspace after it is held without a decision anyone is told of.
pub async fn when_held(workspaces: &Workspaces) -> bool {
    use tokio::sync::broadcast::error::RecvError;
    // Subscribed before the first look, so a change between the look and the wait wakes it.
    let mut governance = workspaces.governance_changes();
    let mut holds = workspaces.holds_changed();
    loop {
        if holds_person_workspace(workspaces).await {
            return true;
        }
        let stopped = tokio::select! {
            changed = governance.recv() => matches!(changed, Err(RecvError::Closed)),
            changed = holds.changed() => changed.is_err(),
            () = tokio::time::sleep(LOOK_AGAIN) => false,
        };
        if stopped {
            return false;
        }
    }
}

/// [`apply`], once this device holds its person's workspace ([`when_held`]).
///
/// # Errors
/// As [`apply`], or these workspaces stopped before this device held it.
pub async fn apply_when_held<R: Resource>(
    workspaces: &Workspaces,
    resource: (&str, &str),
    action: Invoke<R>,
) -> Result<(), String> {
    if !when_held(workspaces).await {
        return Err(STOPPED.to_owned());
    }
    apply(workspaces, resource, action).await
}

/// Why a write waiting for the person's workspace was never made.
pub(crate) const STOPPED: &str = "this device stopped before it held your own workspace";

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
/// A person workspace's own record is named by its app and name, not founded
/// as a record with an id, so it stands on no chain of version steps and keeps
/// its first storage version: a type of a later one does not build here.
///
/// # Errors
/// The resource's initializer fails, which is a bug in it.
pub fn genesis<R: Resource>(workspace: &str) -> Result<automerge::AutoCommit, String> {
    const {
        assert!(
            R::VERSION == 1,
            "a person workspace's own record keeps its first storage version"
        );
    }
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

/// The document the record `record` of `R` in `workspace` starts as where this device's store
/// holds none of it, as that store founds a record: the change every copy of the kind derives in
/// the workspace, the containers of `R`'s type in it and nothing else.
///
/// # Errors
/// The founding cannot be made, or the document refused it.
pub fn founded<R: Resource>(
    workspace: &str,
    record: &str,
) -> Result<automerge::AutoCommit, String> {
    use tilo_automerge::ApplyIncoming as _;
    let containers =
        collab_types::document::founding::of_kind(&tilo_resource::surface::surface_of::<R>());
    let scope = collab_types::document::founding::Scope {
        workspace,
        kind: R::KIND,
        id: record,
    };
    let founding = collab_types::document::founding::structure(scope, &containers)
        .map_err(|error| error.to_string())?;
    let mut doc = automerge::AutoCommit::new();
    doc.apply_incoming([founding])
        .map_err(|error| error.to_string())?;
    Ok(doc)
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
    resource: (&str, &str),
    action: Invoke<R>,
) -> Result<(), String> {
    let cert = workspaces.device_cert().clone();
    let (app, name) = resource;
    // Boxed: committing a write can apply a change of the person's own workspace in turn.
    Box::pin(
        workspaces.author_record(workspace, (app, R::KIND, name), |doc| {
            invoke(doc, &cert, app, action)
        }),
    )
    .await
}

/// [`apply_on`] of several actions in one write of this device: the changes follow one another in
/// one document, so a record they found is founded once, at one author's first number. `optional`
/// names an action whose refusal keeps the others' write all the same, answered once that write
/// is kept.
///
/// # Errors
/// As [`apply`], and the refusal of the action `optional` names.
pub async fn apply_actions_on<R: Resource>(
    workspaces: &Workspaces,
    workspace: &str,
    resource: (&str, &str),
    actions: Vec<Invoke<R>>,
    optional: Option<usize>,
) -> Result<(), String> {
    let cert = workspaces.device_cert().clone();
    let (app, name) = resource;
    let mut refused = Ok(());
    // Boxed: committing a write can apply a change of the person's own workspace in turn.
    Box::pin(
        workspaces.author_record(workspace, (app, R::KIND, name), |doc| {
            let replica = Replica::<R>::from_doc(std::mem::take(doc))
                .as_invoker(&hex::encode(cert.user_public_key));
            let mut replica = replica.in_app(app, &app_label::<R>(app));
            replica
                .doc_mut()
                .set_actor(automerge::ActorId::from(cert.device_public_key.to_vec()));
            for (at, action) in actions.into_iter().enumerate() {
                if let Err(error) = replica
                    .apply(action)
                    .map(drop)
                    .map_err(|error| error.to_string())
                {
                    if Some(at) == optional {
                        refused = Err(error);
                        continue;
                    }
                    std::mem::swap(doc, replica.doc_mut());
                    return Err(error);
                }
            }
            std::mem::swap(doc, replica.doc_mut());
            Ok(())
        }),
    )
    .await?;
    refused
}

/// The kinds of the person's own records, of what a workspace's owner alone keeps of it, and of
/// the records the system keeps for apps, declared under the apps that keep them, each from its
/// type: every holder's store keeps them as the resources of those apps, founded and judged by
/// their types. What every service declares as it starts.
///
/// # Errors
/// A type's surface does not compile: the service cannot judge that kind's records, so it does
/// not start.
pub(crate) fn declare_kinds(surfaces: &collab_types::app_surface::Surfaces) -> Result<(), String> {
    use tilo_resource::surface::surface_of;
    declare(
        surfaces,
        devices::DEVICES.0,
        &[
            surface_of::<devices::Devices>(),
            surface_of::<device_answers::DeviceAnswers>(),
        ],
    )?;
    declare(
        surfaces,
        super::blocked::BLOCKED.0,
        &[surface_of::<super::blocked::Blocked>()],
    )?;
    declare(
        surfaces,
        connected::CONNECTED.0,
        &[surface_of::<connected::ConnectedAccounts>()],
    )?;
    declare(
        surfaces,
        super::region::REGION.0,
        &[surface_of::<super::region::Region>()],
    )?;
    declare(
        surfaces,
        super::servers::SERVERS.0,
        &[
            surface_of::<super::servers::Servers>(),
            surface_of::<super::servers::floor::Floor>(),
        ],
    )?;
    declare(
        surfaces,
        super::membership::OWNERSHIP.0,
        &[surface_of::<super::membership::Ownership>()],
    )?;
    declare(
        surfaces,
        tilo_user_identity::workspace_list::APP,
        &[surface_of::<
            tilo_user_identity::workspace_list::WorkspaceList,
        >()],
    )?;
    // The workspace records the system keeps for apps, whether or not the app is installed here.
    #[cfg(not(target_arch = "wasm32"))]
    {
        system::<super::vault_entries::WorkspaceVaultEntries>(surfaces, super::vault::APP);
        system::<tilo_workspace_emoji::WorkspaceEmoji>(surfaces, tilo_workspace_emoji::OWNER);
        system::<tilo_contact_directory::ContactDirectory>(surfaces, tilo_app_id::apps::CONTACTS);
        system::<crate::environment::VmEnvironment>(surfaces, crate::environment::VM_APP);
        // The content a host stopped keeping past a limit, kept in each workspace's store.
        super::keep_surface(
            surfaces,
            super::refused::APP,
            super::refused::surface().as_bytes(),
        )?;
        // The calls that wait for their person's yes, kept in each workspace's store.
        super::keep_surface(
            surfaces,
            tilo_action_services::workspace_invocations::PROVIDER,
            super::asked_kept::surface().as_bytes(),
        )?;
        // A workspace's shared desktop, each person's view of it, and each window's view.
        surfaces.declare_system(
            tilo_desktop_model::ownership::DESKTOP_APP,
            tilo_desktop_model::record::layout(),
        );
        surfaces.declare_system(
            tilo_desktop_model::ownership::DESKTOP_APP,
            tilo_desktop_model::record::person_view(),
        );
        surfaces.declare_system(
            tilo_desktop_model::ownership::DESKTOP_APP,
            tilo_desktop_model::record::window_view(),
        );
    }
    // A workspace's files are records of the store: its tree's own record and its chunks.
    #[cfg(all(feature = "files", not(target_arch = "wasm32")))]
    surfaces.declare_system(fs_protocol::tree::FILES_APP, fs_protocol::tree::declared());
    // The workspace's own record of its agents: its kinds and verbs, kept as an installed
    // package's surface is, so this device decides the changes that name them.
    #[cfg(feature = "agents")]
    super::keep_surface(
        surfaces,
        tilo_workspace_agent::OWNER,
        include_bytes!("../../workspace-surface.json"),
    )?;
    Ok(())
}

/// Declare `R`, a workspace record the system keeps for `app`, from its type.
#[cfg(not(target_arch = "wasm32"))]
fn system<R: Resource>(surfaces: &collab_types::app_surface::Surfaces, app: &str) {
    surfaces.declare_system(app, tilo_resource::surface::surface_of::<R>());
}

/// Declare `resources` as every kind `app` keeps, where this device's stores judge its records.
///
/// # Errors
/// A surface does not write as JSON, or as the surface file `app`'s records are named by.
fn declare(
    surfaces: &collab_types::app_surface::Surfaces,
    app: &str,
    resources: &[tilo_resource::surface::Surface],
) -> Result<(), String> {
    let file = collab_types::surface::canonical(&serde_json::json!({ "resources": resources }));
    super::keep_surface(surfaces, app, file.as_bytes())
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
    chars.next().map_or_else(String::new, |first| {
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
