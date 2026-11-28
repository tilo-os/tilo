//! The record a window carries into another workspace.
//!
//! A workspace is where an app's records are kept: each is a resource of the workspace's one
//! store, so a window moved to another workspace runs as a process of that workspace, on that
//! workspace's records. What the person was looking at is the record the window shows, so the
//! move takes it along, through the store's own `move` action on the record, invoked as the
//! person: the destination founds it under an id minted for it there, holding what it holds now,
//! and the workspace it leaves no longer holds it. Each side is judged by whether the person may
//! write that kind of record there, before anything is written, so a move either side refuses
//! leaves both workspaces as they were.
//!
//! The window's saved presentation follows it, so it reopens where the person left it. That is
//! presentation, kept beside the record on this device, and is not part of the record.
//!
//! A record that belongs to its workspace — the document a workspace keeps for
//! an app, which every window there shows — stays where it is: one window of it
//! cannot take the workspace's own document to another workspace, so the
//! window's app opens the document of the workspace it comes to instead.
//!
//! Every step here waits on the device's daemon and therefore runs on a thread of its own: the
//! session's loop asks one and reads what it did on a later pass.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver};

use tilo_actions::remote::{InvokeRequest, RemoteActionTarget, record_path};
use tilo_session_client::daemon_documents::DaemonShell;
use ui::application::NativeProfile;
use ui::resource::{DocumentCheckout, DocumentId, DocumentPort, ResourceType};

use crate::device_workspaces::DeviceWorkspaces;

/// Why a step of a move could not be taken.
#[derive(Debug)]
pub(crate) enum Fault {
    /// The workspaces refused the move, or the daemon did not answer it.
    Refused(String),
    /// The window's saved presentation could not be read or kept.
    Profile(String),
}

/// One app's records in one workspace, and the directory the launcher points that app's data
/// home at there, which keeps the presentations of its windows on this device.
#[derive(Clone, Debug)]
pub(crate) struct Profile {
    /// The app's `XDG_DATA_HOME` in that workspace, as the launcher sets it.
    root: PathBuf,
    /// The workspace the records are kept in.
    workspace: String,
    /// The package that keeps its records there.
    app: String,
}

impl Profile {
    /// The records `app` keeps in `workspace`, run on this device.
    pub(crate) fn of(data_home: &Path, workspace: &str, app: &str) -> Self {
        let slug = launcher::appdir::workspace_slug(workspace);
        Self {
            root: launcher::appdir::host_app_dir(data_home, &slug, app).join(".local/share"),
            workspace: workspace.to_owned(),
            app: app.to_owned(),
        }
    }

    /// The saved presentations of the app's windows there.
    fn views(&self, port: &Rc<dyn DocumentPort>) -> Result<NativeProfile, Fault> {
        NativeProfile::open(
            &self.root,
            Some(&self.workspace),
            &self.app,
            Some(Rc::clone(port)),
        )
        .map_err(|error| Fault::Profile(error.to_string()))
    }
}

/// Move the record `id` of `from` to `to`'s workspace as `target`, through the store's `move`
/// action, as the person `daemon` answers.
pub(crate) fn move_record(
    daemon: &DaemonShell,
    from: &Profile,
    to: &Profile,
    id: &DocumentId,
    target: &DocumentId,
) -> Result<(), Fault> {
    across(
        daemon,
        tilo_resource_store::MOVE_VERB,
        (from, to),
        id,
        target,
    )
}

/// Copy the record `id` of `from` into `to`'s workspace as `target`, leaving it where it is,
/// through the store's copy, as the person `daemon` answers.
pub(crate) fn copy_record(
    daemon: &DaemonShell,
    from: &Profile,
    to: &Profile,
    id: &DocumentId,
    target: &DocumentId,
) -> Result<(), Fault> {
    across(
        daemon,
        tilo_resource_store::COPY_ACTION,
        (from, to),
        id,
        target,
    )
}

/// Take back the copy `id` a copy founded in `at`, while nobody has written it since.
pub(crate) fn withdraw_copy(
    daemon: &DaemonShell,
    at: &Profile,
    id: &DocumentId,
) -> Result<(), Fault> {
    invoke(
        daemon,
        at,
        id,
        tilo_resource_store::WITHDRAW_COPY_ACTION,
        serde_json::Map::new(),
    )
}

/// `action` of the record `id` of `from`, taking it to `to`'s workspace as `target`.
fn across(
    daemon: &DaemonShell,
    action: &str,
    (from, to): (&Profile, &Profile),
    id: &DocumentId,
    target: &DocumentId,
) -> Result<(), Fault> {
    let mut arguments = serde_json::Map::new();
    arguments.insert("to".to_owned(), to.workspace.clone().into());
    arguments.insert("target".to_owned(), target.as_str().into());
    invoke(daemon, from, id, action, arguments)
}

/// `action` of the record `id` the app of `at` keeps in its workspace, as the person `daemon`
/// answers.
fn invoke(
    daemon: &DaemonShell,
    at: &Profile,
    id: &DocumentId,
    action: &str,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> Result<(), Fault> {
    let request = InvokeRequest::plain(
        RemoteActionTarget {
            provider: at.app.clone(),
            address: tilo_actions::ActionAddress::new(record_path(&at.app, id.as_str()), action),
        },
        arguments,
    );
    daemon
        .invoke_blocking(&at.workspace, request)
        .map(drop)
        .map_err(Fault::Refused)
}

/// Copy the record `id` that the window of tab `tabs.0` shows in `source` into `destination`,
/// under an id minted for it there, which this answers, and give the window of tab `tabs.1` there
/// the presentation the first has, so it opens where the person was. A presentation that cannot
/// be kept takes the copy back, so a copy that fails leaves nothing behind.
pub(crate) fn copy(
    daemon: &DaemonShell,
    workspaces: &DeviceWorkspaces,
    (source, destination): (&Profile, &Profile),
    id: &DocumentId,
    (from_tab, to_tab): (u64, u64),
) -> Result<DocumentId, Fault> {
    let port = workspaces.port();
    let view = source
        .views(&port)?
        .saved_view(from_tab)
        .map_err(|error| Fault::Profile(error.to_string()))?
        .filter(|view| view.document_id == id.as_str());
    let target = DocumentId::minted();
    copy_record(daemon, source, destination, id, &target)?;
    let Some(view) = view else {
        return Ok(target);
    };
    let checkout = DocumentCheckout::new(
        target.clone(),
        ResourceType::new(view.resource_type.clone()),
    );
    let adopted = destination.views(&port).and_then(|views| {
        views
            .adopt_view(&view, to_tab, &checkout)
            .map_err(|error| Fault::Profile(error.to_string()))
    });
    match adopted {
        Ok(()) => Ok(target),
        Err(fault) => {
            if let Err(back) = withdraw_copy(daemon, destination, &target) {
                tracing::warn!(
                    ?back,
                    "a copy whose window could not follow stayed where it went"
                );
            }
            Err(fault)
        }
    }
}

/// Move the record `id` that the window of tab `tab` shows in `source` to `destination`, under
/// an id minted for it there, which this answers, and give the window there the presentation it
/// had: what it reopens to. A presentation that cannot be kept moves the record back, so a move
/// that fails leaves it where it was.
pub(crate) fn carry(
    daemon: &DaemonShell,
    workspaces: &DeviceWorkspaces,
    (source, destination): (&Profile, &Profile),
    id: &DocumentId,
    tab: u64,
) -> Result<DocumentId, Fault> {
    let port = workspaces.port();
    // The window reopens where the person left it, not at the top of the record.
    let view = source
        .views(&port)?
        .saved_view(tab)
        .map_err(|error| Fault::Profile(error.to_string()))?
        .filter(|view| view.document_id == id.as_str());
    let target = DocumentId::minted();
    move_record(daemon, source, destination, id, &target)?;
    let Some(view) = view else {
        return Ok(target);
    };
    let checkout = DocumentCheckout::new(
        target.clone(),
        ResourceType::new(view.resource_type.clone()),
    );
    let adopted = destination.views(&port).and_then(|views| {
        views
            .adopt_view(&view, tab, &checkout)
            .map_err(|error| Fault::Profile(error.to_string()))
    });
    match adopted {
        Ok(()) => Ok(target),
        Err(fault) => {
            if let Err(back) = move_record(daemon, destination, source, &target, id) {
                tracing::warn!(
                    ?back,
                    "a record whose window could not follow stayed where it went"
                );
            }
            Err(fault)
        }
    }
}

/// Take one step of a move off the loop's thread and hand back what it came to, for a later pass to
/// pick up: nothing on the loop waits on the daemon. `None` when this machine will not start the
/// thread.
pub(crate) fn off_loop<T, F>(step: F) -> Option<Receiver<T>>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (taken, answers) = mpsc::channel();
    std::thread::Builder::new()
        .name("tilo-window-transfer".to_owned())
        .spawn(move || {
            let _ = taken.send(step());
        })
        .ok()?;
    Some(answers)
}

/// Take the last word of a move off the loop's thread, which nothing waits for: `Err` when this
/// machine will not start the thread, so the last word is never taken and whoever asked is told.
pub(crate) fn off_loop_detached<F>(step: F) -> std::io::Result<()>
where
    F: FnOnce() + Send + 'static,
{
    std::thread::Builder::new()
        .name("tilo-window-transfer".to_owned())
        .spawn(step)
        .map(|_| ())
}
