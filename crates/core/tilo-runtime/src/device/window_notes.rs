//! What each window this device runs shows (the record its app holds), published to the person's
//! desktop on its shell link: the desktop notes it beside the window, so the window's bar names
//! where it is and a place comes back on that record, and it keeps no broker of its own to read it
//! from.
//!
//! The rows are the broker's own ([`AppBrokerState::window_resources`]): what each local app
//! window holds, by the window instance the shell launched it for. Only the desktop reads them.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tilo_app_host::collab_broker::FanoutState;
use tilo_app_host::services::{Caller, PublishedResources, Reading, ResourceWatch};
use tilo_app_host::window_notes::{RESOURCE, WindowNotes};
use tilo_lock::MutexRecover as _;
use tokio::sync::{mpsc, oneshot};

/// One desktop following the windows, and the task that sends it each change.
struct Watch {
    id: ResourceWatch,
    task: tokio::task::JoinHandle<()>,
}

/// What the windows this device runs hold, as its daemon publishes them.
pub(super) struct Published {
    /// The fanout the broker serves on: the published rows are its broker's, which may be
    /// attached and detached as the daemon runs.
    fanout: std::sync::Weak<FanoutState>,
    watches: Arc<Mutex<Vec<Watch>>>,
    next_watch: AtomicU64,
}

/// Whether `caller` may read the rows: the person's desktop, which notes them beside its windows,
/// alone.
fn granted(caller: &Caller) -> bool {
    caller.app_id == tilo_desktop_model::commands::APP_ID
}

fn refused() -> String {
    "what the windows here hold is the desktop's to note".to_owned()
}

/// The rows' reading as the wire carries it: what the broker shows now, and the generation that
/// grows when it changes.
fn encoded(state: &tilo_app_host::collab_app_broker::AppBrokerState) -> Reading {
    (
        state.windows_generation(),
        Some(WindowNotes::of(&state.window_resources()).reading()),
    )
}

impl Published {
    /// The windows the broker on `fanout` serves, published from now on.
    pub(super) fn of(fanout: &Arc<FanoutState>) -> Arc<Self> {
        Arc::new(Self {
            fanout: Arc::downgrade(fanout),
            watches: Arc::default(),
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
        } else if !granted(caller) {
            Err(refused())
        } else {
            match self.fanout.upgrade().and_then(|fanout| fanout.app_broker()) {
                Some(state) => Ok(encoded(&state)),
                // The broker starts with the daemon; nothing holds a window until it serves.
                None => Ok((0, Some(WindowNotes::default().reading()))),
            }
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
        let state = self
            .fanout
            .upgrade()
            .and_then(|fanout| fanout.app_broker())
            .ok_or_else(|| "this device's broker is not serving its windows".to_owned())?;
        let id = ResourceWatch::new(self.next_watch.fetch_add(1, Ordering::Relaxed));
        let fanout = self.fanout.clone();
        let task = tokio::spawn(follow(fanout, state, sink));
        self.watches.lock_recover().push(Watch { id, task });
        Ok(id)
    }

    fn unwatch(&self, watch: ResourceWatch) {
        let removed = {
            let mut watches = self.watches.lock_recover();
            watches
                .iter()
                .position(|held| held.id == watch)
                .map(|at| watches.remove(at))
        };
        if let Some(held) = removed {
            held.task.abort();
        }
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
            message: format!(
                "`{resource}` is read, never changed: what a window shows is its app's"
            ),
        });
        answered
    }
}

/// Send each change of what the windows hold on `sink`, from `state` until the desktop stops
/// listening or the broker goes.
async fn follow(
    fanout: std::sync::Weak<FanoutState>,
    state: Arc<tilo_app_host::collab_app_broker::AppBrokerState>,
    sink: mpsc::UnboundedSender<runtime_collab::wire::Event>,
) {
    let mut state = state;
    loop {
        let mut changes = state.window_changes();
        if send(&state, &sink).is_err() {
            return;
        }
        // Held no longer, so a broker the daemon lets go of drops its last sender and this task
        // follows the next one it attaches.
        drop(state);
        while changes.changed().await.is_ok() {
            let Some(moved) = fanout.upgrade().and_then(|fanout| fanout.app_broker()) else {
                return;
            };
            if send(&moved, &sink).is_err() {
                return;
            }
        }
        match fanout.upgrade().and_then(|fanout| fanout.app_broker()) {
            Some(moved) => state = moved,
            None => return,
        }
    }
}

/// Send what `state`'s windows hold now.
fn send(
    state: &tilo_app_host::collab_app_broker::AppBrokerState,
    sink: &mpsc::UnboundedSender<runtime_collab::wire::Event>,
) -> Result<(), ()> {
    let (revision, reading) = encoded(state);
    sink.send(runtime_collab::wire::Event::Reading {
        resource: RESOURCE.to_owned(),
        revision,
        reading,
    })
    .map_err(|_| ())
}
