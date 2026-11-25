//! What the session's roles hear of the workspaces' records changing.
//!
//! The device's daemon reads each record a write lands in into the device's one search index;
//! the session reads none. What it keeps is the one stream the roles that follow records read,
//! such as the person's mentions ([`crate::platform::records::Changes`]): an app's write
//! committed here, or a peer's change taken in, is noted there, and what this device already
//! held is noted once as replayed when the channels that open it settle.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;

/// The notes of the workspaces' records changing, over the documents this device holds.
pub(crate) struct RecordChanges {
    reads: Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
    changes: crate::platform::records::Changes,
    /// The workspaces this device held when they were last noted whole ([`Self::sweep`]).
    swept: Mutex<BTreeSet<String>>,
}

impl RecordChanges {
    /// Note what `changes` follows of the workspaces this device holds, as `view` says the daemon
    /// published them: a change of one the device does not hold is no news to any role, and is
    /// noted when the device holds it. `reads` lists what each holds when it is swept.
    pub(crate) fn start(
        reads: &Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
        view: &Arc<crate::workspace_view::WorkspaceView>,
        changes: crate::platform::records::Changes,
    ) -> Arc<Self> {
        changes.holding(Arc::new({
            let view = Arc::clone(view);
            move |workspace: &str| {
                tilo_desktop_model::WorkspaceId::parse(workspace).is_some_and(|id| view.holds(id))
            }
        }));
        Arc::new(Self {
            reads: Arc::clone(reads),
            changes,
            swept: Mutex::new(BTreeSet::new()),
        })
    }

    /// Note every record each workspace this device holds keeps, as replayed: once per workspace.
    pub(crate) fn sweep(&self) {
        for workspace in self.reads.workspaces() {
            if self.swept.lock_recover().contains(&workspace) {
                continue;
            }
            match self.reads.listing(&workspace) {
                Ok(records) => {
                    self.swept.lock_recover().insert(workspace.clone());
                    for listed in records {
                        let (app, record) = (listed.address.app, listed.address.record);
                        self.changes.note(&workspace, &app, &record, true);
                    }
                }
                Err(error) => tracing::warn!(
                    %error,
                    %workspace,
                    "the records the session follows are not listed"
                ),
            }
        }
    }

    /// `record` of `app` in `workspace` changed: the device's daemon took a write of it.
    pub(crate) fn changed(&self, workspace: &str, app: &str, record: &str) {
        self.changes.note(workspace, app, record, false);
    }
}
