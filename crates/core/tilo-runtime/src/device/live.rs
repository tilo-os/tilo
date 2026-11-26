//! What is live now in the workspaces this device keeps (a track playing, a timer running, a call),
//! published to the person's desktop on its shell link: the desktop shows it in the bar, quick
//! settings and on the lock screen, and keeps no workspace of its own to read it from.
//!
//! The rows are the workspaces' stores' own ([`LiveRows`]), as this device's person may read them.
//! Only the desktop reads them: they span every workspace, which no single app is shown.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tilo_app_host::services::{Caller, PublishedResources, Reading, ResourceWatch};
use tilo_lock::MutexRecover as _;
use tilo_workspace_service::live::{LiveRows, RESOURCE};
use tokio::sync::{mpsc, oneshot};

/// One desktop following the rows.
struct Watch {
    id: ResourceWatch,
    sink: mpsc::UnboundedSender<runtime_collab::wire::Event>,
}

/// The live rows of this device's workspaces, as its daemon publishes them.
pub(super) struct Published {
    rows: LiveRows,
    watches: Arc<Mutex<Vec<Watch>>>,
    next_watch: AtomicU64,
}

/// Whether `caller` may read the rows: the person's desktop, which shows them, alone.
fn granted(caller: &Caller) -> bool {
    caller.app_id == tilo_desktop_model::commands::APP_ID
}

fn refused() -> String {
    "what is live in every workspace is the desktop's to show".to_owned()
}

/// The rows' reading as the wire carries it.
fn encoded(rows: &LiveRows) -> Reading {
    (rows.revision(), Some(rows.reading()))
}

impl Published {
    /// The rows of the workspaces `workspaces` keeps, shown from now on.
    pub(super) fn of(workspaces: &tilo_workspace_service::workspaces::Workspaces) -> Arc<Self> {
        let rows = LiveRows::default();
        let watches: Arc<Mutex<Vec<Watch>>> = Arc::default();
        rows.wake_with({
            let (rows, watches) = (rows.clone(), Arc::downgrade(&watches));
            Arc::new(move || {
                let Some(watches) = watches.upgrade() else {
                    return;
                };
                let (revision, reading) = encoded(&rows);
                watches.lock_recover().retain(|watch| {
                    watch
                        .sink
                        .send(runtime_collab::wire::Event::Reading {
                            resource: RESOURCE.to_owned(),
                            revision,
                            reading: reading.clone(),
                        })
                        .is_ok()
                });
            })
        });
        let (shown, showing) = (workspaces.clone(), rows.clone());
        tilo_rt::app()
            .handle()
            .spawn(async move { shown.show_live(showing).await });
        Arc::new(Self {
            rows,
            watches,
            next_watch: AtomicU64::new(1),
        })
    }
}

impl PublishedResources for Published {
    fn snapshot(
        &self,
        caller: &Caller,
        resource: &str,
    ) -> oneshot::Receiver<Result<Reading, String>> {
        let (reply, answered) = oneshot::channel();
        let answer = if resource != RESOURCE {
            Ok((0, None))
        } else if granted(caller) {
            Ok(encoded(&self.rows))
        } else {
            Err(refused())
        };
        let _ = reply.send(answer);
        answered
    }

    fn watch(
        &self,
        caller: &Caller,
        resource: &str,
        sink: mpsc::UnboundedSender<runtime_collab::wire::Event>,
    ) -> Result<ResourceWatch, String> {
        if resource != RESOURCE {
            return Err(format!("this device publishes no `{resource}` resource"));
        }
        if !granted(caller) {
            return Err(refused());
        }
        let (revision, reading) = encoded(&self.rows);
        let _ = sink.send(runtime_collab::wire::Event::Reading {
            resource: RESOURCE.to_owned(),
            revision,
            reading,
        });
        let id = ResourceWatch::new(self.next_watch.fetch_add(1, Ordering::Relaxed));
        self.watches.lock_recover().push(Watch { id, sink });
        Ok(id)
    }

    fn unwatch(&self, watch: ResourceWatch) {
        self.watches.lock_recover().retain(|held| held.id != watch);
    }

    fn invoke(
        &self,
        _caller: &Caller,
        resource: &str,
        _action: &str,
        _arguments: Vec<u8>,
    ) -> oneshot::Receiver<runtime_collab::wire::Receipt> {
        let (reply, answered) = oneshot::channel();
        let _ = reply.send(runtime_collab::wire::Receipt::Refused {
            code: "unknown_action".into(),
            message: format!("`{resource}` is read, never changed: what is live is the records'"),
        });
        answered
    }
}
