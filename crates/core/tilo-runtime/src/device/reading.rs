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

use futures_util::future::BoxFuture;
use serde_json::Value;
use tilo_lock::MutexRecover as _;
use tilo_workspace_service::apps::services::{Caller, PublishedResources, Reading, ResourceWatch};
use tokio::sync::{mpsc, oneshot, watch};

/// How a published reading is made: the value now, for the caller that reads it, and the
/// revision it moves with.
pub type Read = dyn Fn(&Caller) -> Value + Send + Sync;
/// How a published reading is made where its value is read from something that answers
/// asynchronously, such as a workspace's records: the value, for the caller that reads it.
pub type ReadLater = dyn Fn(Caller) -> BoxFuture<'static, Value> + Send + Sync;
/// Where a published reading says it changed.
pub type Changes = dyn Fn() -> watch::Receiver<u64> + Send + Sync;

/// How a [`Served`] makes its reading: now, or by asking something that answers asynchronously.
enum Source {
    Now(Arc<Read>),
    Later(Arc<ReadLater>),
}

/// One resource the daemon publishes itself, read now and watched for changes.
pub struct Served {
    resource: &'static str,
    source: Source,
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
            source: Source::Now(read),
            changes,
            watches: Mutex::default(),
            next_watch: AtomicU64::new(0),
        }
    }

    /// `resource`, read by `read` at the revision `changes` names, where reading it waits on
    /// something that answers asynchronously.
    #[must_use]
    pub fn later(resource: &'static str, read: Arc<ReadLater>, changes: Arc<Changes>) -> Self {
        Self {
            resource,
            source: Source::Later(read),
            changes,
            watches: Mutex::default(),
            next_watch: AtomicU64::new(0),
        }
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
        if resource != self.resource {
            let _ = answer.send(Ok((0, None)));
            return answered;
        }
        match &self.source {
            Source::Now(read) => {
                let reading = encoded(&read(caller));
                let revision = *((self.changes)()).borrow();
                let _ = answer.send(Ok((revision, reading)));
            }
            Source::Later(read) => {
                let read = Arc::clone(read);
                let reader = caller.clone();
                let changes = (self.changes)();
                tokio::spawn(async move {
                    let revision = *changes.borrow();
                    let reading = encoded(&read(reader).await);
                    let _ = answer.send(Ok((revision, reading)));
                });
            }
        }
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
        let id = self.next_watch.fetch_add(1, Ordering::Relaxed);
        let task = match &self.source {
            Source::Now(read) => {
                let reading = encoded(&read(caller));
                let revision = *((self.changes)()).borrow();
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
                let read = Arc::clone(read);
                let named = self.resource;
                let reader = caller.clone();
                tokio::spawn(async move {
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
                })
            }
            Source::Later(read) => {
                let mut changes = (self.changes)();
                let read = Arc::clone(read);
                let named = self.resource;
                let reader = caller.clone();
                tokio::spawn(async move {
                    let revision = *changes.borrow();
                    let reading = encoded(&read(reader.clone()).await);
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
                    while changes.changed().await.is_ok() {
                        let revision = *changes.borrow_and_update();
                        let reading = encoded(&read(reader.clone()).await);
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
                })
            }
        };
        self.watches.lock_recover().insert(id, task.abort_handle());
        Ok(ResourceWatch::new(id))
    }

    fn unwatch(&self, watch: ResourceWatch) {
        let task = self.watches.lock_recover().remove(&watch.id());
        if let Some(task) = task {
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use super::*;

    fn caller() -> Caller {
        Caller {
            app_id: "so.tilo.console".to_owned(),
            workspace: "home".to_owned(),
        }
    }

    /// A reading its daemon can only make by asking something that answers asynchronously: read
    /// now, and sent again each time the revision its changes name moves.
    #[tokio::test]
    async fn a_deferred_reading_reads_now_and_follows_its_changes() {
        let value = Arc::new(StdMutex::new("first".to_owned()));
        let (changed, source) = watch::channel(1_u64);
        let read = {
            let value = Arc::clone(&value);
            Arc::new(move |_caller: Caller| {
                let value = Arc::clone(&value);
                Box::pin(async move {
                    serde_json::json!({"agents": [*value
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)]})
                }) as BoxFuture<'static, Value>
            })
        };
        let change_source: Arc<Changes> = Arc::new(move || source.clone());
        let served = Served::later("server", read, change_source);

        let (revision, reading) = served.snapshot(&caller(), "server").await.unwrap().unwrap();
        assert_eq!(revision, 1);
        let value_read: Value = serde_json::from_slice(&reading.unwrap()).unwrap();
        assert_eq!(value_read["agents"][0], "first");

        let (sink, mut events) = mpsc::unbounded_channel();
        let watch = served.watch(&caller(), "server", sink).unwrap();
        let first = events.recv().await.expect("the reading now");
        assert!(matches!(
            first,
            runtime_collab::wire::Event::Reading { revision: 1, .. }
        ));

        *value
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = "second".to_owned();
        changed.send_replace(2);
        let sent = events.recv().await.expect("a changed reading");
        let runtime_collab::wire::Event::Reading {
            revision, reading, ..
        } = sent
        else {
            panic!("a watch takes readings");
        };
        assert_eq!(revision, 2);
        let value_read: Value = serde_json::from_slice(&reading.unwrap()).unwrap();
        assert_eq!(value_read["agents"][0], "second");
        served.unwatch(watch);
    }
}
