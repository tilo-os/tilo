//! The workspaces this device's daemon holds, as the session reads and writes a record of theirs
//! itself: the record a window carries to another workspace when it moves there. A store opened
//! over them ([`DeviceWorkspaces::port`]) reads each record from the device's workspace service and
//! writes what changed through it, signed as this device and sealed by the daemon where the
//! workspace's documents are sealed for its members; the daemon tells this device's apps of it
//! as of a member's. No copy is kept beside the workspace's.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use tilo_desktop_model::WorkspaceId;
use ui::resource::{DocumentAsk, DocumentPort, DocumentRead};

use crate::engine_write::{Writing, Written};
use crate::workspace_view::WorkspaceView;

/// The workspaces' copy the session reads and writes, once it holds one: what a store reaches
/// them through ([`Self::port`]), on any thread.
#[derive(Clone)]
pub(crate) struct DeviceWorkspaces {
    /// How the person's workspaces stand on the device's daemon, which holds them.
    view: Arc<WorkspaceView>,
    /// Where this computer's own workspace service keeps what it keeps of its workspaces.
    service_root: Option<std::path::PathBuf>,
    /// The documents of the workspaces this device's service holds.
    reads: Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
    /// What this device's service writes into a workspace's documents: where a write of the
    /// session's reaches the workspace, signed, sealed and kept as this device's.
    writes: Arc<dyn tilo_peer_state::workspace_documents::DeviceWrites>,
    /// The stream of record changes, told what the session wrote once the workspace took it.
    changes: crate::platform::records::Changes,
}

/// The session's records' way to the workspaces' copy: this device's service's own reads and
/// writes of it. Until the session holds the copy, every record is on its way and every write
/// waits for it.
struct EnginePort {
    /// The workspaces' copy once the session holds it, shared with the writes waiting for it.
    attached: Rc<RefCell<Option<Rc<DeviceWorkspaces>>>>,
    wake: tilo_service::Wake,
}

impl EnginePort {
    fn attached(&self) -> Option<Rc<DeviceWorkspaces>> {
        self.attached.borrow().clone()
    }
}

/// What the service holds of one document, as a store reads it.
fn wire_read(read: tilo_peer_state::workspace_documents::DocumentRead) -> DocumentRead {
    use tilo_peer_state::workspace_documents::DocumentRead as Held;
    match read {
        Held::Whole {
            revision,
            heads,
            bytes,
            hint,
        } => DocumentRead::Whole {
            revision,
            heads,
            bytes,
            hint,
        },
        Held::Changes {
            revision,
            heads,
            bytes,
        } => DocumentRead::Changes {
            revision,
            heads,
            bytes,
        },
        Held::Unchanged(revision) => DocumentRead::Unchanged(revision),
        Held::Absent => DocumentRead::Absent,
        Held::Pending => DocumentRead::Pending,
    }
}

impl DocumentPort for EnginePort {
    fn read(
        &self,
        app: &str,
        workspace: &str,
        reads: Option<Vec<DocumentAsk>>,
    ) -> Result<Vec<(String, DocumentRead)>, String> {
        let Some(attached) = self.attached() else {
            // Nothing reads before the session holds the workspaces: each record is on its way.
            return Ok(reads
                .unwrap_or_default()
                .into_iter()
                .map(|ask| (ask.id, DocumentRead::Pending))
                .collect());
        };
        let Some(reads) = reads else {
            return Ok(attached
                .reads
                .listing(workspace)?
                .into_iter()
                .filter(|(held, _)| held == app)
                .map(|(_, record)| (record, DocumentRead::Held))
                .collect());
        };
        reads
            .into_iter()
            .map(|ask| {
                let since: Option<Vec<[u8; 32]>> = ask.since;
                let read = attached
                    .reads
                    .read(workspace, app, &ask.id, since.as_deref())?;
                Ok((ask.id, wire_read(read)))
            })
            .collect()
    }

    fn write(
        &self,
        app: &str,
        workspace: &str,
        id: &str,
        changes: Vec<u8>,
        founding: bool,
        readers: Option<&str>,
    ) -> ui::resource::Written {
        let readers = readers.map(str::to_owned);
        let Some(room) = WorkspaceId::parse(workspace) else {
            let mut refused = Some(Err(format!("{workspace} names no workspace")));
            return Box::new(move |_| refused.take());
        };
        let key: DocumentKey = (workspace.to_owned(), app.to_owned(), id.to_owned());
        let wake = Arc::clone(&self.wake);
        let held = Rc::clone(&self.attached);
        let mut writing: Option<Writing> = None;
        Box::new(move |patience| {
            // A write made before the session holds the workspaces waits for them: what the
            // session keeps as it starts reaches the workspace once it is there.
            let attached = held.borrow().clone()?;
            if writing.is_none() {
                // A workspace the daemon does not hold yet (one still being founded) takes the
                // write once it does. The daemon seals it: for the readers it names, when it
                // names some.
                if !attached.view.holds(room) {
                    return None;
                }
                let (writes, key, changes, readers) = (
                    Arc::clone(&attached.writes),
                    key.clone(),
                    changes.clone(),
                    readers.clone(),
                );
                writing = Some(Writing::write(Arc::clone(&wake), async move {
                    writes
                        .write(
                            &key.0,
                            &changes,
                            tilo_peer_state::workspace_documents::DocumentWriting {
                                app: &key.1,
                                document: &key.2,
                                readers: readers.as_deref(),
                                founding,
                                snapshot: founding,
                                sealed: None,
                            },
                        )
                        .await
                }));
            }
            let written = writing.as_mut()?.within(patience)?;
            Some(attached.written(&key, written))
        })
    }
}

impl DeviceWorkspaces {
    /// The way a store on this thread reads and writes these workspaces' records.
    pub(crate) fn port(&self) -> Rc<dyn DocumentPort> {
        Rc::new(EnginePort {
            attached: Rc::new(RefCell::new(Some(Rc::new(self.clone())))),
            wake: Arc::new(|| {}),
        })
    }

    /// Take what the write of `key` came to: the roles that follow records are told of a write the
    /// workspace took. This device's apps hear it from the daemon, as they hear a member's.
    fn written(
        &self,
        (workspace, app, record): &DocumentKey,
        written: Written,
    ) -> Result<(), String> {
        match written {
            Ok(_) => {
                self.changes.note(workspace, app, record, false);
                Ok(())
            }
            // A change the workspace's rules refuse is told to whoever wrote it (the write
            // answered with the typed refusal) and filed where the person's activity reads it.
            Err(error) => Err(error.to_string()),
        }
    }
}

/// A record, by workspace, app and wire record.
type DocumentKey = (String, String, String);

/// A document a service of the session keeps in a workspace: one other threads write too.

impl DeviceWorkspaces {
    /// The workspaces `view` says the daemon holds, read through `reads` and written through
    /// `writes`; a write the workspace took is noted on `changes`. `service_root` is where this computer's own workspace service keeps its
    /// workspaces, where it runs one.
    pub(crate) fn new(
        view: Arc<WorkspaceView>,
        service_root: Option<std::path::PathBuf>,
        reads: Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
        writes: Arc<dyn tilo_peer_state::workspace_documents::DeviceWrites>,
        changes: crate::platform::records::Changes,
    ) -> Self {
        Self {
            view,
            service_root,
            reads,
            writes,
            changes,
        }
    }

    /// Where this computer's own workspace service keeps its workspaces, where it runs one.
    pub(crate) fn service_root(&self) -> Option<std::path::PathBuf> {
        self.service_root.clone()
    }

    /// The documents of the workspaces the device's service holds.
    pub(crate) fn reads(&self) -> &Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments> {
        &self.reads
    }
}
