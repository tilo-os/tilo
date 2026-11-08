//! The record a window carries into another workspace.
//!
//! A workspace is where an app's records are kept: the launcher gives each
//! (workspace, package) its own profile, so a window moved to another
//! workspace runs as a process of that workspace, with its own store. What the
//! person was looking at is the record the window shows, so the move takes it
//! along: the source document's whole history becomes a record of the
//! destination under a freshly minted id
//! ([`ui::record::transfer::rehome`]), the instance's presentation follows it,
//! and the source record is stamped retired naming the copy, so wherever it is
//! read it says where it went instead of looking live.
//!
//! A record that belongs to its workspace — the document a workspace keeps for
//! an app, which every window there shows — stays where it is: one window of it
//! cannot take the workspace's own document to another workspace, so the
//! window's app opens the document of the workspace it comes to instead.
//!
//! Every step here waits on the disk and therefore runs on a thread of its
//! own: the session's loop asks one and reads what it did on a later pass.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use automerge::{AutoCommit, ChangeHash};
use ui::application::NativeProfile;
use ui::record::transfer::{self, Durability, Origin, Retirement, TransferError};
use ui::record::{RecordCheckout, RecordId, RecordOpenError};

/// Why a step of a move could not be taken.
#[derive(Debug)]
pub(crate) enum Fault {
    /// The record itself: missing, retired, not what it claims to be, or the disk refused a read or
    /// a write.
    Transfer(TransferError),
    /// The app's profile could not be opened where the launcher keeps it.
    Profile(String),
    /// A write the move made has not reached the disk, so nothing else may read it yet.
    Unwritten,
}

/// One app's profile for one workspace: the directory the launcher points that app's data home at
/// there, and the workspace its records are scoped to.
#[derive(Clone, Debug)]
pub(crate) struct Profile {
    /// The app's `XDG_DATA_HOME` in that workspace, as the launcher sets it.
    root: PathBuf,
    /// The launcher's workspace identity, which the records are scoped to.
    workspace: String,
    /// The package that keeps its records there.
    app: String,
}

impl Profile {
    /// The profile `app` keeps its records of `workspace` in on this device.
    pub(crate) fn of(data_home: &Path, workspace: &str, app: &str) -> Self {
        let slug = launcher::appdir::workspace_slug(Some(workspace));
        Self {
            root: launcher::appdir::host_app_dir(data_home, &slug, app).join(".local/share"),
            workspace: workspace.to_owned(),
            app: app.to_owned(),
        }
    }

    fn open(&self) -> Result<NativeProfile, Fault> {
        NativeProfile::open(&self.root, Some(&self.workspace))
            .map_err(|error| Fault::Profile(error.to_string()))
    }
}

/// What a window's record became in the workspace the person carried it to.
pub(crate) struct Arrived {
    /// The record where the window showed it, as the destination's copy was made from it: what the
    /// retirement of the source names, and what the source is read through once its app let go.
    pub(crate) origin: Origin,
    /// The record it arrived as: a record of the destination, sharing the whole history of the one
    /// it left.
    pub(crate) target: RecordCheckout,
}

/// Read the record `id` that the window at `instance` shows in `source` , and make it a record of
/// `destination` under an id nothing there holds, with the presentation the instance saved. Nothing
/// in `source` changes.
pub(crate) fn take(
    source: &Profile,
    destination: &Profile,
    id: &RecordId,
    instance: u64,
) -> Result<Arrived, Fault> {
    let from = source.open()?;
    let doc = transfer::read_by_id(from.store(), &source.app, from.workspace_id(), id)
        .map_err(Fault::Transfer)?;
    let checkout = ui::record::identity(&doc).ok_or(Fault::Transfer(TransferError::Open(
        RecordOpenError::IdentityMismatch,
    )))?;
    let origin = Origin::Record(checkout);
    let to = destination.open()?;
    let mut arrived = transfer::rehome(
        &doc,
        &origin,
        from.workspace_id(),
        to.workspace_id(),
        &RecordId::minted(),
    )
    .map_err(Fault::Transfer)?;
    let (target, landing) = transfer::install(
        to.store(),
        &destination.app,
        to.workspace_id(),
        &mut arrived,
    )
    .map_err(Fault::Transfer)?;
    match landing.durability(to.store().as_ref()) {
        Durability::Durable => {}
        // A write that has not reached the disk is a record nothing else may read yet, and a failed
        // one never will: the move cannot go on.
        Durability::Pending | Durability::Failed(_) => return Err(Fault::Unwritten),
    }
    // The window reopens where the person left it, not at the top of the record it now shares.
    if let Some(view) = from
        .saved_view(instance)
        .map_err(|error| Fault::Profile(error.to_string()))?
    {
        to.adopt_view(&view, instance, &target)
            .map_err(|error| Fault::Profile(error.to_string()))?;
    }
    Ok(Arrived { origin, target })
}

/// `origin` 's document as its writer last made it durable, once that writer has let go; `None`
/// while an app still holds it.
/// `until` is the heads the app gave with its retirement: the document as it was just before it
/// stamped itself retired, which is everything the move still has to bring along, and which a
/// retired document can no longer be read as it stands.
pub(crate) fn released(
    source: &Profile,
    origin: &Origin,
    until: Option<&[ChangeHash]>,
) -> Result<Option<AutoCommit>, Fault> {
    let from = source.open()?;
    if let Some(heads) = until {
        return transfer::read_until(
            from.store(),
            &source.app,
            from.workspace_id(),
            origin,
            heads,
        )
        .map(Some)
        .map_err(Fault::Transfer);
    }
    if !transfer::writer_released(from.store(), &source.app, from.workspace_id(), origin)
        .map_err(Fault::Transfer)?
    {
        return Ok(None);
    }
    transfer::read(from.store(), &source.app, from.workspace_id(), origin)
        .map(Some)
        .map_err(Fault::Transfer)
}

/// What merging the source's late edits into the destination came to.
pub(crate) enum Merged {
    /// The destination already held every change the source has.
    Nothing,
    /// The changes arrived, on the disk.
    Written(usize),
    /// The destination's app holds the record open: it must let go before the merge can be written.
    Held,
}

/// Merge every change `source` holds that `target` lacks in `destination` .
pub(crate) fn merge(
    destination: &Profile,
    target: &RecordCheckout,
    source: &AutoCommit,
) -> Result<Merged, Fault> {
    let to = destination.open()?;
    let mut held = transfer::read(
        to.store(),
        &destination.app,
        to.workspace_id(),
        &Origin::Record(target.clone()),
    )
    .map_err(Fault::Transfer)?;
    let mut written = source.clone();
    if held.get_changes_added(&mut written).is_empty() {
        return Ok(Merged::Nothing);
    }
    match transfer::catch_up(
        to.store(),
        &destination.app,
        to.workspace_id(),
        target,
        source,
    ) {
        Ok((arriving, _)) => Ok(Merged::Written(arriving)),
        Err(TransferError::Busy) => Ok(Merged::Held),
        Err(error) => Err(Fault::Transfer(error)),
    }
}

/// Stamp `origin` in `profile` retired, saying why: that it moved to `why` 's workspace, or that it
/// never made it there. A document already retired is left as it is.
pub(crate) fn retire(profile: &Profile, origin: &Origin, why: &Retirement) -> Result<(), Fault> {
    let held = profile.open()?;
    transfer::retire(held.store(), &profile.app, held.workspace_id(), origin, why)
        .map(|_| ())
        .map_err(Fault::Transfer)
}

/// Take one step of a move off the loop's thread and hand back what it came to, for a later pass to
/// pick up: nothing on the loop waits on the disk. `None` when this machine will not start the
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
