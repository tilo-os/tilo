//! The record a window carries into another workspace.
//!
//! A workspace is where an app's records are kept: each is a document of the workspace's one
//! store, so a window moved to another workspace runs as a process of that workspace, on that
//! workspace's records. What the person was looking at is the record the window shows, so the
//! move takes it along: the source document is read from the workspace it is in, its whole
//! history becomes a record of the destination under a freshly minted id
//! ([`ui::resource::transfer::rehome`]) written there as its founding write, the instance's
//! presentation follows it, and the source record is stamped retired naming the copy
//! ([`ui::resource::transfer::stamp_retired`]), so wherever it is read it says where it went
//! instead of looking live.
//!
//! A record that belongs to its workspace — the document a workspace keeps for
//! an app, which every window there shows — stays where it is: one window of it
//! cannot take the workspace's own document to another workspace, so the
//! window's app opens the document of the workspace it comes to instead.
//!
//! Every step here waits on the workspaces' store and therefore runs on a thread of its own: the
//! session's loop asks one and reads what it did on a later pass.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use automerge::{AutoCommit, ChangeHash};
use ui::application::NativeProfile;
use ui::resource::transfer::{self, Origin, Retirement, TransferError};
use ui::resource::{
    DocumentAsk, DocumentCheckout, DocumentId, DocumentPort, DocumentRead, ResourceOpenError,
};

use crate::device_workspaces::DeviceWorkspaces;

/// How long a write of a move may take the workspace to answer.
const WRITE_WAIT: Duration = Duration::from_secs(10);

/// Why a step of a move could not be taken.
#[derive(Debug)]
pub(crate) enum Fault {
    /// The record itself: missing, retired, or not what it claims to be.
    Transfer(TransferError),
    /// The window's saved presentation could not be read or kept.
    Profile(String),
    /// The workspace did not answer a read or a write of the move, or refused it.
    Workspace(String),
}

/// One app's records in one workspace, as a move reads and writes them, and the directory the
/// launcher points that app's data home at there, which names the device its windows run on.
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
        let slug = launcher::appdir::workspace_slug(Some(workspace));
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

    /// The record `id` as the workspace holds it; `None` where it holds none.
    pub(crate) fn read(
        &self,
        port: &dyn DocumentPort,
        id: &str,
    ) -> Result<Option<AutoCommit>, Fault> {
        let ask = DocumentAsk {
            id: id.to_owned(),
            since: None,
        };
        let read = port
            .read(&self.app, &self.workspace, Some(vec![ask]))
            .map_err(Fault::Workspace)?
            .pop()
            .map(|(_, read)| read);
        match read {
            Some(DocumentRead::Whole { bytes, .. }) => AutoCommit::load(&bytes)
                .map(Some)
                .map_err(|error| Fault::Transfer(TransferError::Automerge(error.to_string()))),
            Some(DocumentRead::Absent) | None => Ok(None),
            Some(_) => Err(Fault::Workspace("the record is not here yet".to_owned())),
        }
    }

    /// Write `changes` to the record `id`, the whole document when `founding`, and wait for the
    /// workspace to take them.
    fn write(
        &self,
        port: &dyn DocumentPort,
        id: &str,
        changes: Vec<u8>,
        founding: bool,
    ) -> Result<(), Fault> {
        let mut written = port.write(&self.app, &self.workspace, id, changes, founding, None);
        match written(WRITE_WAIT) {
            Some(answer) => answer.map_err(Fault::Workspace),
            None => Err(Fault::Workspace(
                "the workspace did not answer in time".to_owned(),
            )),
        }
    }
}

/// What a window's record became in the workspace the person carried it to.
pub(crate) struct Arrived {
    /// The record where the window showed it, as the destination's copy was made from it: what the
    /// retirement of the source names.
    pub(crate) origin: Origin,
    /// The record it arrived as: a record of the destination, sharing the whole history of the one
    /// it left.
    pub(crate) target: DocumentCheckout,
}

/// Read the record `id` that the window at `instance` shows in `source` , and make it a record of
/// `destination` under an id nothing there holds, with the presentation the instance saved. Nothing
/// in `source` changes.
pub(crate) fn take(
    workspaces: &DeviceWorkspaces,
    source: &Profile,
    destination: &Profile,
    id: &DocumentId,
    instance: u64,
) -> Result<Arrived, Fault> {
    let port = workspaces.port();
    let doc = source
        .read(port.as_ref(), id.route())?
        .ok_or(Fault::Transfer(TransferError::Open(
            ResourceOpenError::MissingResource,
        )))?;
    let checkout = ui::resource::identity(&doc).ok_or(Fault::Transfer(TransferError::Open(
        ResourceOpenError::IdentityMismatch,
    )))?;
    let origin = Origin::Record(checkout);
    let minted = DocumentId::minted();
    let mut arrived = transfer::rehome(
        &doc,
        &origin,
        &source.workspace,
        &destination.workspace,
        &minted,
    )
    .map_err(Fault::Transfer)?;
    let target = DocumentCheckout::new(minted, origin.kind().clone());
    destination.write(port.as_ref(), target.id.route(), arrived.save(), true)?;
    // The window reopens where the person left it, not at the top of the record it now shares.
    if let Some(view) = source
        .views(&port)?
        .saved_view(instance)
        .map_err(|error| Fault::Profile(error.to_string()))?
    {
        destination
            .views(&port)?
            .adopt_view(&view, instance, &target)
            .map_err(|error| Fault::Profile(error.to_string()))?;
    }
    Ok(Arrived { origin, target })
}

/// `origin` 's document as the workspace holds it; `None` while the changes its app gave with its
/// retirement have not reached the workspace yet.
/// `until` is the heads the app gave with its retirement: the document as it was just before it
/// stamped itself retired, which is everything the move still has to bring along.
pub(crate) fn released(
    workspaces: &DeviceWorkspaces,
    source: &Profile,
    origin: &Origin,
    until: Option<&[ChangeHash]>,
) -> Result<Option<AutoCommit>, Fault> {
    let Origin::Record(checkout) = origin else {
        return Err(Fault::Transfer(TransferError::NotTransferable(
            "a workspace's own package document never leaves its workspace",
        )));
    };
    let port = workspaces.port();
    let Some(mut doc) = source.read(port.as_ref(), checkout.id.route())? else {
        return Err(Fault::Transfer(TransferError::Open(
            ResourceOpenError::MissingResource,
        )));
    };
    let heads = match until {
        Some(heads) => heads.to_vec(),
        // An app that said nothing is read as it stands, but never past a retirement it stamped
        // without saying where it stood: that change is the source's alone.
        None if transfer::is_retired(&doc) => doc
            .get_heads()
            .iter()
            .filter_map(|head| doc.get_change_by_hash(head))
            .flat_map(|change| change.deps().to_vec())
            .collect(),
        None => return Ok(Some(doc)),
    };
    if heads.iter().any(|head| !doc.has_change(head)) {
        return Ok(None);
    }
    Ok(Some(doc.fork_at(&heads).map_err(|error| {
        Fault::Transfer(TransferError::Automerge(error.to_string()))
    })?))
}

/// What merging the source's late edits into the destination came to.
pub(crate) enum Merged {
    /// The destination already held every change the source has.
    Nothing,
    /// The changes arrived, in the workspace.
    Written(usize),
}

/// Merge every change `source` holds that `target` lacks in `destination` : whatever window of
/// the destination holds the record open takes them as any change another device wrote.
pub(crate) fn merge(
    workspaces: &DeviceWorkspaces,
    destination: &Profile,
    target: &DocumentCheckout,
    source: &AutoCommit,
) -> Result<Merged, Fault> {
    let port = workspaces.port();
    let Some(mut held) = destination.read(port.as_ref(), target.id.route())? else {
        return Err(Fault::Transfer(TransferError::Open(
            ResourceOpenError::MissingResource,
        )));
    };
    let mut source = source.clone();
    let arriving = held.get_changes_added(&mut source).len();
    if arriving == 0 {
        return Ok(Merged::Nothing);
    }
    let before = held.get_heads();
    tilo_automerge::ApplyIncoming::merge_incoming(&mut held, &mut source)
        .map_err(|error| Fault::Transfer(TransferError::Automerge(error.to_string())))?;
    destination.write(
        port.as_ref(),
        target.id.route(),
        held.save_after(&before),
        false,
    )?;
    Ok(Merged::Written(arriving))
}

/// Stamp `origin` in `profile` retired, saying why: that it moved to `why` 's workspace, or that it
/// never made it there. A document already retired is left as it is.
pub(crate) fn retire(
    workspaces: &DeviceWorkspaces,
    profile: &Profile,
    origin: &Origin,
    why: &Retirement,
) -> Result<(), Fault> {
    let Origin::Record(checkout) = origin else {
        return Ok(());
    };
    let port = workspaces.port();
    let Some(mut doc) = profile.read(port.as_ref(), checkout.id.route())? else {
        return Ok(());
    };
    let before = doc.get_heads();
    transfer::stamp_retired(&mut doc, why).map_err(Fault::Transfer)?;
    if doc.get_heads() == before {
        return Ok(());
    }
    profile.write(
        port.as_ref(),
        checkout.id.route(),
        doc.save_after(&before),
        false,
    )
}

/// Take one step of a move off the loop's thread and hand back what it came to, for a later pass to
/// pick up: nothing on the loop waits on the workspaces' store. `None` when this machine will not
/// start the thread.
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

#[cfg(test)]
mod tests {
    use automerge::ReadDoc as _;
    use automerge::transaction::Transactable as _;
    use ui::resource::transfer::Successor;
    use ui::resource::{ResourceOwners, ResourceType};

    use super::*;

    const FROM: &str = "5e1f0000-0000-4000-8000-000000000001";
    const TO: &str = "5e1f0000-0000-4000-8000-000000000002";
    const APP: &str = "so.tilo.notes";

    /// The note `doc` holds.
    fn body(doc: &AutoCommit) -> Option<String> {
        match doc.get(automerge::ROOT, "body") {
            Ok(Some((automerge::Value::Scalar(value), _))) => value.as_str().map(str::to_owned),
            _ => None,
        }
    }

    /// The app writes `body` into its record `id` of `workspace`, as its window writes it there.
    fn write(workspaces: &DeviceWorkspaces, workspace: &str, id: &DocumentId, body: &str) {
        let records = ResourceOwners::writing(workspaces.port())
            .open(APP, workspace)
            .expect("the app's records of its workspace");
        let checkout = DocumentCheckout::new(id.clone(), ResourceType::new("notes.page"));
        if records.open_existing_resource_checked(&checkout).is_err() {
            records
                .create_resource(&checkout)
                .expect("the record opens");
        }
        records
            .with(id, |doc| {
                doc.put(automerge::ROOT, "body", body)
                    .expect("the note's body");
            })
            .expect("the record is open");
        records.flush().expect("the workspace takes it");
    }

    /// What `profile` holds of the record `id`.
    fn held(workspaces: &DeviceWorkspaces, profile: &Profile, id: &DocumentId) -> AutoCommit {
        profile
            .read(workspaces.port().as_ref(), id.route())
            .expect("the workspace reads")
            .expect("the workspace holds it")
    }

    /// A record a window shows goes to the workspace it is carried to under an id of its own there
    /// with its whole history, what its app wrote after the copy was made follows it, and the
    /// record it left says where it went; a copy nothing opened is retired as such.
    #[test]
    fn a_carried_record_arrives_with_its_history_and_its_late_changes() {
        let device = crate::daemon_fixture::TestDaemon::holding(&[FROM, TO]);
        let workspaces = device.device_workspaces();
        let home = tempfile::tempdir().expect("a data home");
        let (source, destination) = (
            Profile::of(home.path(), FROM, APP),
            Profile::of(home.path(), TO, APP),
        );
        let shown = DocumentId::minted();
        write(&workspaces, FROM, &shown, "first draft");

        let arrived = take(&workspaces, &source, &destination, &shown, 7).expect("it arrives");
        assert_ne!(
            arrived.target.id, shown,
            "under an id of the workspace it went to"
        );
        let mut copy = held(&workspaces, &destination, &arrived.target.id);
        assert_eq!(body(&copy).as_deref(), Some("first draft"));
        assert_eq!(ui::resource::workspace_scope(&copy).as_deref(), Some(TO));
        assert_eq!(transfer::provenance(&copy), vec![shown.clone()]);
        for head in held(&workspaces, &source, &shown).get_heads() {
            assert!(
                copy.has_change(&head),
                "the copy holds the record's history"
            );
        }

        // What the app writes before its window closes, and the heads it says it stood at.
        write(&workspaces, FROM, &shown, "first draft, last words");
        let mut last = held(&workspaces, &source, &shown);
        let until = last.get_heads();
        last.put(automerge::ROOT, "body", "never written")
            .expect("a change");
        assert!(
            released(
                &workspaces,
                &source,
                &arrived.origin,
                Some(&last.get_heads())
            )
            .expect("it reads")
            .is_none(),
            "a change the workspace does not hold yet is waited for"
        );
        let late = released(&workspaces, &source, &arrived.origin, Some(&until))
            .expect("it reads")
            .expect("what the app wrote is there");
        assert!(matches!(
            merge(&workspaces, &destination, &arrived.target, &late).expect("it merges"),
            Merged::Written(changes) if changes > 0
        ));
        assert!(matches!(
            merge(&workspaces, &destination, &arrived.target, &late).expect("it merges"),
            Merged::Nothing
        ));
        copy = held(&workspaces, &destination, &arrived.target.id);
        assert_eq!(body(&copy).as_deref(), Some("first draft, last words"));

        let successor = Successor {
            workspace: TO.to_owned(),
            document: arrived.target.id.as_str().to_owned(),
            by: "ada".to_owned(),
        };
        retire(
            &workspaces,
            &source,
            &arrived.origin,
            &Retirement::Moved(Some(successor.clone())),
        )
        .expect("the record it left is retired");
        assert_eq!(
            transfer::retirement(&held(&workspaces, &source, &shown)),
            Some(Retirement::Moved(Some(successor))),
            "the record it left names where it went"
        );
        assert_eq!(
            transfer::retirement(&held(&workspaces, &destination, &arrived.target.id)),
            None,
            "the copy is the live record"
        );

        retire(
            &workspaces,
            &destination,
            &Origin::Record(arrived.target.clone()),
            &Retirement::Abandoned,
        )
        .expect("a copy nothing opened is retired");
        assert_eq!(
            transfer::retirement(&held(&workspaces, &destination, &arrived.target.id)),
            Some(Retirement::Abandoned)
        );
    }
}
