//! The workspace's vault as this device's daemon publishes it to Passwords' own window, read as
//! `vault` ([`passwords_vault::RESOURCE`]).
//!
//! The reading is every entry the vault keeps on this device, secrets included, answered to
//! Passwords' process alone: another app's read or call is refused. The window shows it and keeps
//! nothing of it. Its verbs change the vault here as this device's person, and record no
//! secret: a secret arrives only as what the person typed on the system's card, which the host
//! running the window hands on ([`passwords_vault::KEEP_SECRET`]).
//!
//! A watch reads the vault again whenever this device changed it, and at least every few
//! seconds while it is followed, so an entry another device kept arrives while the window is
//! open; a reading that did not move is not sent again.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Map, Value};
use tilo_lock::MutexRecover as _;
use tilo_workspace_service::apps::services::{Caller, PublishedResources, Reading, ResourceWatch};
use tilo_workspace_service::workspaces::passwords_vault;
use tokio::sync::{mpsc, oneshot};

/// How often a followed vault is read again for what another device changed.
const AGAIN: Duration = Duration::from_secs(4);

/// The workspace's vault, published to Passwords' window.
pub(crate) struct Published {
    workspaces: Arc<crate::device::workspaces::ServiceWorkspaces>,
    watches: Mutex<HashMap<u64, tokio::task::AbortHandle>>,
    next_watch: AtomicU64,
}

impl Published {
    pub(crate) fn new(workspaces: &Arc<crate::device::workspaces::ServiceWorkspaces>) -> Self {
        Self {
            workspaces: Arc::clone(workspaces),
            watches: Mutex::default(),
            next_watch: AtomicU64::new(0),
        }
    }
}

/// `caller`'s reading of the vault, encoded, or why it has none.
async fn read(
    workspaces: &crate::device::workspaces::ServiceWorkspaces,
    caller: &Caller,
) -> Result<Option<Vec<u8>>, String> {
    let reading = workspaces
        .copies()
        .passwords_vault_reading(&caller.workspace, &caller.app_id)
        .await?;
    Ok(serde_json::to_vec(&reading).ok())
}

fn refused(code: &str, message: impl Into<String>) -> runtime_collab::wire::Receipt {
    runtime_collab::wire::Receipt::Refused {
        code: code.to_owned(),
        message: message.into(),
    }
}

impl PublishedResources for Published {
    fn snapshot(
        &self,
        caller: &Caller,
        resource: &str,
    ) -> oneshot::Receiver<Result<Reading, String>> {
        let (reply, answered) = oneshot::channel();
        if resource != passwords_vault::RESOURCE {
            let _ = reply.send(Ok((0, None)));
            return answered;
        }
        let workspaces = Arc::clone(&self.workspaces);
        let caller = caller.clone();
        tokio::spawn(async move {
            let revision = *workspaces.copies().vault_changes().borrow();
            let _ = reply.send(
                read(&workspaces, &caller)
                    .await
                    .map(|read| (revision, read)),
            );
        });
        answered
    }

    fn watch(
        &self,
        caller: &Caller,
        resource: &str,
        sink: mpsc::UnboundedSender<runtime_collab::wire::Event>,
    ) -> Result<ResourceWatch, String> {
        if resource != passwords_vault::RESOURCE {
            return Err(format!("this device publishes no `{resource}` resource"));
        }
        if caller.app_id != passwords_vault::PASSWORDS_APP {
            return Err(passwords_vault::ONLY_PASSWORDS.to_owned());
        }
        let workspaces = Arc::clone(&self.workspaces);
        let reader = caller.clone();
        let task = tokio::spawn(async move {
            let mut changes = workspaces.copies().vault_changes();
            let mut sent: Option<Option<Vec<u8>>> = None;
            let mut revision = 0u64;
            loop {
                match read(&workspaces, &reader).await {
                    Ok(reading) if sent.as_ref() != Some(&reading) => {
                        revision = revision.wrapping_add(1);
                        if sink
                            .send(runtime_collab::wire::Event::Reading {
                                resource: passwords_vault::RESOURCE.to_owned(),
                                revision,
                                reading: reading.clone(),
                            })
                            .is_err()
                        {
                            return;
                        }
                        sent = Some(reading);
                    }
                    Ok(_) => {}
                    Err(why) => tracing::debug!(%why, "the vault is not read for Passwords here"),
                }
                if sink.is_closed() {
                    return;
                }
                let _ = tokio::time::timeout(AGAIN, changes.changed()).await;
            }
        });
        let id = self.next_watch.fetch_add(1, Ordering::Relaxed);
        self.watches.lock_recover().insert(id, task.abort_handle());
        Ok(ResourceWatch::new(id))
    }

    fn unwatch(&self, watch: ResourceWatch) {
        if let Some(task) = self.watches.lock_recover().remove(&watch.id()) {
            task.abort();
        }
    }

    fn invoke(
        &self,
        caller: &Caller,
        resource: &str,
        action: &str,
        arguments: Vec<u8>,
    ) -> oneshot::Receiver<runtime_collab::wire::Receipt> {
        let (reply, answered) = oneshot::channel();
        if resource != passwords_vault::RESOURCE {
            let _ = reply.send(refused(
                "unknown_action",
                format!("this device publishes no `{resource}` resource"),
            ));
            return answered;
        }
        if caller.app_id != passwords_vault::PASSWORDS_APP {
            let _ = reply.send(refused("not_granted", passwords_vault::ONLY_PASSWORDS));
            return answered;
        }
        let Ok(arguments) = serde_json::from_slice::<Map<String, Value>>(&arguments) else {
            // Never quoted: what came may be a secret.
            let _ = reply.send(refused(
                "invalid_arguments",
                "the arguments are not a JSON object",
            ));
            return answered;
        };
        let workspaces = Arc::clone(&self.workspaces);
        let caller = caller.clone();
        let action = action.to_owned();
        tokio::spawn(async move {
            let receipt = match workspaces
                .copies()
                .passwords_vault_invoke(&caller.workspace, &caller.app_id, &action, arguments)
                .await
            {
                Ok(()) => runtime_collab::wire::Receipt::Done(
                    serde_json::to_vec(&tilo_actions::ActionOutput::default()).unwrap_or_default(),
                ),
                Err(message) => refused("refused", message),
            };
            let _ = reply.send(receipt);
        });
        answered
    }
}
