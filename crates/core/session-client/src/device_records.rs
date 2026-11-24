//! What the desktop keeps of its sessions for the next one, as records of the device it runs on:
//! the workspace in front, the wallpapers, the notification centre, what could not be written to a
//! workspace yet, and the like. Each is one of the device's own records, which its daemon keeps
//! (`settings/<name>` in the person's own workspace): read through the one record API and written
//! with the device's `settings#set` action, both on the shell link the daemon handed the desktop.
//!
//! A write is handed to the daemon in the order the session made it, away from the thread that made
//! it; [`DeviceRecords::flush`] is what a session that ends waits for.

#![cfg(unix)]

use std::sync::Arc;

use serde_json::{Map, Value};
use tilo_actions::remote::{
    DEVICE_SETTINGS, InvokeArgument, InvokeRequest, ReadAnswer, RemoteActionTarget,
    ResourceReadRequest,
};
use tilo_actions::{ActionAddress, InvocationId};
use tokio::sync::mpsc;

use crate::daemon_documents::DaemonShell;

/// The action that writes one of the device's records.
const SET: &str = "set";

/// The device's records, as the desktop reads and writes them.
#[derive(Clone)]
pub struct DeviceRecords {
    keeper: Arc<Keeper>,
}

impl std::fmt::Debug for DeviceRecords {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceRecords").finish_non_exhaustive()
    }
}

enum Keeper {
    /// The device's daemon, in the person's own workspace, which its records are carried into.
    Daemon {
        shell: DaemonShell,
        workspace: String,
        writes: mpsc::UnboundedSender<Queued>,
    },
    /// Records held in this process alone, as the daemon would keep them: a test's.
    #[cfg(any(test, feature = "test-support"))]
    Held(std::sync::Mutex<std::collections::BTreeMap<String, Value>>),
}

/// What waits its turn to reach the daemon.
enum Queued {
    Set(String, Value),
    /// Answered once everything handed before it was answered.
    Flushed(std::sync::mpsc::SyncSender<()>),
}

impl DeviceRecords {
    /// The records of the device `shell` reaches, read and written in `workspace`, the person's
    /// own, which the daemon carries them into.
    #[must_use]
    pub fn on(shell: DaemonShell, workspace: String) -> Self {
        let (writes, mut queued) = mpsc::unbounded_channel();
        let (writer, space) = (shell.clone(), workspace.clone());
        shell.runtime().spawn(async move {
            while let Some(next) = queued.recv().await {
                match next {
                    Queued::Set(name, value) => {
                        let request = set_request(&name, value);
                        if let Err(error) = writer
                            .dispatch(
                                Some(&space),
                                &tilo_actions::remote::RemoteActionMessage::Invoke(request),
                            )
                            .await
                        {
                            tracing::warn!(%error, record = %name, "what the session keeps was not written");
                        }
                    }
                    Queued::Flushed(done) => {
                        let _ = done.send(());
                    }
                }
            }
        });
        Self {
            keeper: Arc::new(Keeper::Daemon {
                shell,
                workspace,
                writes,
            }),
        }
    }

    /// Records held in this process alone, as the device's daemon keeps them: what a test of a
    /// session that keeps something for the next one hands both sessions.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn held() -> Self {
        Self {
            keeper: Arc::new(Keeper::Held(std::sync::Mutex::default())),
        }
    }

    /// The record `name`.
    #[must_use]
    pub fn record(&self, name: &str) -> DeviceRecord {
        DeviceRecord {
            records: self.clone(),
            name: name.to_owned(),
        }
    }

    /// Wait until every write handed before this was answered: what a session that ends waits for.
    /// Called from a thread that is not the shell's runtime.
    pub fn flush(&self) {
        match &*self.keeper {
            Keeper::Daemon { writes, .. } => {
                let (done, flushed) = std::sync::mpsc::sync_channel(1);
                if writes.send(Queued::Flushed(done)).is_ok() {
                    let _ = flushed.recv();
                }
            }
            #[cfg(any(test, feature = "test-support"))]
            Keeper::Held(_) => {}
        }
    }
}

/// One record of the device's, by its name.
#[derive(Clone, Debug)]
pub struct DeviceRecord {
    records: DeviceRecords,
    name: String,
}

impl DeviceRecord {
    /// What the record holds, `None` when nothing was kept yet. Called from a thread that is not
    /// the shell's runtime.
    ///
    /// # Errors
    /// The daemon could not be asked, or refused the read.
    pub fn read(&self) -> Result<Option<Value>, String> {
        match &*self.records.keeper {
            Keeper::Daemon {
                shell, workspace, ..
            } => {
                let readings = shell.read_blocking(ResourceReadRequest {
                    workspace: workspace.clone(),
                    app: DEVICE_SETTINGS.to_owned(),
                    record: self.name.clone(),
                    joined: false,
                    after: None,
                    wait_ms: None,
                    answer: ReadAnswer::Reading,
                })?;
                let record = &readings["records"][0];
                Ok(record
                    .get("value")
                    .filter(|_| record["absent"].as_bool() != Some(true))
                    .filter(|value| !value.is_null())
                    .cloned())
            }
            #[cfg(any(test, feature = "test-support"))]
            Keeper::Held(held) => Ok(held
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&self.name)
                .cloned()),
        }
    }

    /// Keep `value` as the record, replacing what it held: handed to the daemon after every write
    /// before it, without waiting for it.
    pub fn set(&self, value: Value) {
        match &*self.records.keeper {
            Keeper::Daemon { writes, .. } => {
                if writes.send(Queued::Set(self.name.clone(), value)).is_err() {
                    tracing::warn!(record = %self.name, "the daemon's connection is gone; what the session keeps was not written");
                }
            }
            #[cfg(any(test, feature = "test-support"))]
            Keeper::Held(held) => {
                held.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(self.name.clone(), value);
            }
        }
    }

    /// [`Self::set`], answered once the daemon kept it.
    ///
    /// # Errors
    /// The daemon could not be asked, or did not keep it.
    pub fn set_now(&self, value: Value) -> Result<(), String> {
        match &*self.records.keeper {
            Keeper::Daemon {
                shell, workspace, ..
            } => {
                // After every write handed before it.
                self.records.flush();
                shell
                    .dispatch_blocking(
                        Some(workspace),
                        &tilo_actions::remote::RemoteActionMessage::Invoke(set_request(
                            &self.name, value,
                        )),
                    )
                    .map(|_| ())
            }
            #[cfg(any(test, feature = "test-support"))]
            Keeper::Held(_) => {
                self.set(value);
                Ok(())
            }
        }
    }
}

/// The request writing the record `name` as `value`.
fn set_request(name: &str, value: Value) -> InvokeRequest {
    let mut arguments = Map::new();
    arguments.insert("name".to_owned(), Value::String(name.to_owned()));
    arguments.insert("value".to_owned(), value);
    InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: tilo_device_identity::login::PROVIDER.to_owned(),
            address: ActionAddress::new(DEVICE_SETTINGS, SET),
        },
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}
