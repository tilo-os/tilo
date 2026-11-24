//! One resource the daemon publishes itself, served to the apps attached to it and to the
//! person's shell over the shell link: its reading, and each move of it delivered to the
//! followers that asked.
//!
//! A resource a daemon keeps in its own memory is not a record of a workspace, so it is not read
//! through the record reader: it is published ([`super::published::Published`]) with a provider
//! made here over two functions, one reading the value now and one subscribing to the revision
//! the value moves with. A daemon adds such a resource by publishing this provider, never by
//! growing a second channel.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use tilo_lock::MutexRecover as _;
use tilo_workspace_service::apps::services::{Caller, PublishedResources, Reading, ResourceWatch};
use tokio::sync::{mpsc, oneshot, watch};

/// How a published reading is made: the value now, for the caller that reads it, and the
/// revision it moves with.
pub type Read = dyn Fn(&Caller) -> Value + Send + Sync;
/// Where a published reading says it changed.
pub type Changes = dyn Fn() -> watch::Receiver<u64> + Send + Sync;

/// One resource the daemon publishes itself, read now and watched for changes.
pub struct Served {
    resource: &'static str,
    read: Arc<Read>,
    changes: Arc<Changes>,
    /// The watches this provider minted, by id, each with the task that ends when it does.
    watches: Mutex<HashMap<u64, tokio::task::AbortHandle>>,
    next_watch: AtomicU64,
}

impl Served {
    /// `resource`, read by `read` at the revision `changes` names.
    #[must_use]
    pub fn new(resource: &'static str, read: Arc<Read>, changes: Arc<Changes>) -> Self {
        Self {
            resource,
            read,
            changes,
            watches: Mutex::default(),
            next_watch: AtomicU64::new(0),
        }
    }

    /// The reading now, with the revision it reads at.
    fn encoded(&self, caller: &Caller) -> Reading {
        let reading = encoded(&(self.read)(caller));
        let changes = (self.changes)();
        let revision = *changes.borrow();
        (revision, reading)
    }
}

/// `reading` as the bytes an app reads, or nothing when it does not encode.
fn encoded(reading: &Value) -> Option<Vec<u8>> {
    match serde_json::to_vec(reading) {
        Ok(bytes) => Some(bytes),
        Err(error) => {
            tracing::warn!(%error, "a published reading does not encode");
            None
        }
    }
}

impl PublishedResources for Served {
    fn snapshot(
        &self,
        caller: &Caller,
        resource: &str,
    ) -> oneshot::Receiver<Result<Reading, String>> {
        let (answer, answered) = oneshot::channel();
        let sent = if resource == self.resource {
            Ok(self.encoded(caller))
        } else {
            Ok((0, None))
        };
        let _ = answer.send(sent);
        answered
    }

    fn watch(
        &self,
        caller: &Caller,
        resource: &str,
        sink: mpsc::UnboundedSender<runtime_collab::wire::Event>,
    ) -> Result<ResourceWatch, String> {
        if resource != self.resource {
            return Err(format!("this device publishes no `{resource}` resource"));
        }
        let (revision, reading) = self.encoded(caller);
        if sink
            .send(runtime_collab::wire::Event::Reading {
                resource: self.resource.to_owned(),
                revision,
                reading,
            })
            .is_err()
        {
            return Err("the reader of the resource went away".to_owned());
        }
        let mut changes = (self.changes)();
        let read = Arc::clone(&self.read);
        let named = self.resource;
        let reader = caller.clone();
        let task = tokio::spawn(async move {
            while changes.changed().await.is_ok() {
                let revision = *changes.borrow_and_update();
                let reading = encoded(&read(&reader));
                if sink
                    .send(runtime_collab::wire::Event::Reading {
                        resource: named.to_owned(),
                        revision,
                        reading,
                    })
                    .is_err()
                {
                    return;
                }
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

    /// What this resource publishes is read here alone; its actions are the dispatch's, so a
    /// published invoke reaches none of them.
    fn invoke(
        &self,
        _caller: &Caller,
        _resource: &str,
        action: &str,
        _arguments: Vec<u8>,
    ) -> oneshot::Receiver<runtime_collab::wire::Receipt> {
        let (answer, answered) = oneshot::channel();
        let _ = answer.send(runtime_collab::wire::Receipt::Refused {
            code: "unknown_action".to_owned(),
            message: format!("`{action}` is a typed action, not a change of the reading"),
        });
        answered
    }
}
