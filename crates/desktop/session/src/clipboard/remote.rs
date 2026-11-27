//! The clipboard's way to the person's other devices: this session shares what was copied here
//! with its device daemon, and reads what their other devices copied from it.
//!
//! The daemon carries the copies ([`tilo_runtime::device::clipboard`]): it hands what this session
//! shares to each of the person's other devices over the one action service it serves, and keeps
//! what theirs share with it. The session tells it about a copy with the resource's [`SHARE`]
//! action, and follows the resource's reading for the copies another device offers.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget, ResourceReadRequest,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_app_host::clipboard_continuity::Reading;
use tilo_lock::MutexRecover as _;
use tilo_runtime::device::clipboard as wire;
use tilo_session_client::daemon_documents::{DaemonShell, whole};

/// How long a follow waits before it asks a daemon that did not answer again.
const ASK_AGAIN: std::time::Duration = std::time::Duration::from_secs(1);

/// One copy as this session shares it: its format, its bytes and when it was copied.
pub(crate) struct Shared {
    pub(crate) mime: String,
    pub(crate) bytes: Vec<u8>,
    pub(crate) created_unix_ms: u64,
    pub(crate) pinned: bool,
}

/// This session's link to its device daemon for the clipboard: the resource it follows, and the
/// action it shares a copy with.
pub(crate) struct Remote {
    daemon: DaemonShell,
    /// The person's own workspace, where the record of their devices is kept: the workspace the
    /// daemon answers the copies in.
    workspace: String,
    /// The copies the daemon last read from the person's other devices.
    latest: Arc<Mutex<Reading>>,
    follow: tokio::task::AbortHandle,
}

impl Drop for Remote {
    fn drop(&mut self) {
        self.follow.abort();
    }
}

impl Remote {
    /// Follow the copies the person's other devices offer, as the device's daemon keeps them, and
    /// wake `wake` each time they move. `person` is the person this session runs for.
    pub(crate) fn attach(daemon: &DaemonShell, person: [u8; 32], wake: tilo_service::Wake) -> Self {
        let workspace = collab_types::person_workspace(&person).to_string();
        let latest = Arc::new(Mutex::new(Reading::default()));
        let follow = {
            let (daemon, latest, wake, at) =
                (daemon.clone(), Arc::clone(&latest), wake, workspace.clone());
            let runtime = daemon.runtime().clone();
            runtime.spawn(async move {
                let mut after: Option<String> = None;
                loop {
                    let mut read: ResourceReadRequest = whole(&at, "session", wire::RESOURCE);
                    read.wait_ms = after.is_some().then_some(tilo_actions::remote::MAX_REMOTE_WAIT_MS);
                    read.after.clone_from(&after);
                    match daemon.read(read).await {
                        Ok(answered) => {
                            after = answered
                                .get("revision")
                                .and_then(Value::as_str)
                                .map(str::to_owned);
                            if let Some(reading) = reading_of(&answered) {
                                let moved = *latest.lock_recover() != reading;
                                *latest.lock_recover() = reading;
                                if moved {
                                    wake();
                                }
                            }
                        }
                        Err(error) => {
                            tilo_failure::ignored!(
                                "the daemon did not answer; the same read is asked again a moment later",
                                tracing::debug!(%error, "the copies on the person's other devices did not read")
                            );
                            after = None;
                            tokio::time::sleep(ASK_AGAIN).await;
                        }
                    }
                }
            })
        };
        Self {
            daemon: daemon.clone(),
            workspace,
            latest,
            follow: follow.abort_handle(),
        }
    }

    /// The copies the person's other devices offered, as of the last reading.
    pub(crate) fn reading(&self) -> Reading {
        self.latest.lock_recover().clone()
    }

    /// Share `copies` with the person's other devices, through the daemon: on a thread of its own,
    /// since the daemon answers off this loop. `unshared` says whether a copy still has to be
    /// offered, and is put back when the daemon could not be asked, so the next pass asks again.
    pub(crate) fn share(
        &self,
        copies: Vec<Shared>,
        unshared: Arc<AtomicBool>,
        wake: tilo_service::Wake,
    ) {
        let copies: Vec<Value> = copies
            .into_iter()
            .map(|copy| {
                serde_json::json!({
                    "mime": copy.mime,
                    "bytes": hex::encode(copy.bytes),
                    "created_unix_ms": copy.created_unix_ms,
                    "pinned": copy.pinned,
                })
            })
            .collect();
        let daemon = self.daemon.clone();
        let workspace = self.workspace.clone();
        std::thread::spawn(move || {
            let request = request(wire::SHARE, serde_json::json!({"copies": copies}));
            match daemon.invoke_receipt_blocking(Some(&workspace), request) {
                Ok(receipt) if receipt.status.is_terminal() => {
                    tilo_failure::ignored!(
                        "a share the daemon refused keeps nothing of the copy, and the next copy offers it again",
                        if matches!(
                            receipt.status,
                            ReceiptStatus::Rejected { .. } | ReceiptStatus::Failed { .. }
                        ) {
                            tracing::debug!(status = ?receipt.status, "the clipboard was not shared");
                        }
                    );
                }
                Ok(_) => {}
                // The daemon could not be asked: the copy is offered again on the next pass.
                Err(error) => {
                    tilo_failure::ignored!(
                        "the copy is offered again on the next pass, once the daemon is reached",
                        tracing::debug!(%error, "the clipboard share did not reach the daemon")
                    );
                    unshared.store(true, Ordering::Relaxed);
                }
            }
            wake();
        });
    }
}

/// The reading `answered` carries, or `None` while the daemon holds none.
fn reading_of(answered: &Value) -> Option<Reading> {
    let value = answered["records"]
        .as_array()
        .and_then(|records| records.first())
        .map_or(Value::Null, |record| record["value"].clone());
    if value.is_null() {
        Some(Reading::default())
    } else {
        tilo_failure::ignored!(
            "the next reading says the copies again, and this one is taken as none",
            serde_json::from_value(value)
                .map_err(|error| {
                    tracing::debug!(%error, "the copies on the person's other devices do not read");
                })
                .ok()
        )
    }
}

/// The daemon's `action` of clipboard continuity with `arguments`, as the person's own call.
fn request(action: &str, arguments: Value) -> InvokeRequest {
    let argument = match arguments {
        Value::Object(fields) => InvokeArgument::Json(fields),
        _ => InvokeArgument::None,
    };
    InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: wire::PROVIDER.to_owned(),
            address: ActionAddress::new(wire::RESOURCE, action),
        },
        argument,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}
