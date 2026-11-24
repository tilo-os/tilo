//! The person's MCP server as this device's daemon publishes it to the apps attached to it:
//! every agent the server let in, the reach it was let in for and the sessions open now,
//! read as `mcp.server`.
//!
//! The reading is the workspace service's, which this daemon holds ([`Workspaces`]): the
//! same state that stops an agent or lets it go on ([`super::services`]), served to the apps
//! as a reading so a window lists the agents that act here instead of asking each one.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use tilo_lock::MutexRecover as _;
use tilo_workspace_service::apps::services::{Caller, PublishedResources, Reading, ResourceWatch};
use tokio::sync::{mpsc, oneshot, watch};

pub use tilo_actions::remote::mcp_server::RESOURCE as SERVER;

/// What the reading is made of: the workspace service's state, read now and watched for
/// changes. A function each keeps this testable without a host.
type Read = dyn Fn() -> Value + Send + Sync;
type Changes = dyn Fn() -> watch::Receiver<u64> + Send + Sync;

/// The MCP server, published to the apps this device serves.
pub struct Published {
    read: Arc<Read>,
    changes: Arc<Changes>,
    /// The watches this provider minted, by id, each with the task that ends when it does.
    watches: Mutex<HashMap<u64, tokio::task::AbortHandle>>,
    next_watch: AtomicU64,
}

impl Published {
    /// The server of the person this device's workspace service keeps, read where it keeps it.
    #[must_use]
    pub fn new(workspaces: &Arc<crate::device::workspaces::ServiceWorkspaces>) -> Self {
        let reading = Arc::clone(workspaces);
        let changing = Arc::clone(workspaces);
        Self {
            read: Arc::new(move || reading.copies().mcp_reading()),
            changes: Arc::new(move || changing.copies().mcp_changes()),
            watches: Mutex::default(),
            next_watch: AtomicU64::new(0),
        }
    }

    /// The provider over `read` and `changes`, which a test drives without a host.
    #[must_use]
    pub fn over(read: Arc<Read>, changes: Arc<Changes>) -> Self {
        Self {
            read,
            changes,
            watches: Mutex::default(),
            next_watch: AtomicU64::new(0),
        }
    }

    /// The reading now, with the revision it reads at.
    fn encoded(&self) -> Reading {
        let reading = encoded(&(self.read)());
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
            tracing::warn!(%error, "the MCP server's reading does not encode");
            None
        }
    }
}

impl PublishedResources for Published {
    fn snapshot(
        &self,
        _caller: &Caller,
        resource: &str,
    ) -> oneshot::Receiver<Result<Reading, String>> {
        let (answer, answered) = oneshot::channel();
        let sent = if resource == SERVER {
            Ok(self.encoded())
        } else {
            Ok((0, None))
        };
        let _ = answer.send(sent);
        answered
    }

    fn watch(
        &self,
        _caller: &Caller,
        resource: &str,
        sink: mpsc::UnboundedSender<runtime_collab::wire::Event>,
    ) -> Result<ResourceWatch, String> {
        if resource != SERVER {
            return Err(format!("this device publishes no `{resource}` resource"));
        }
        let (revision, reading) = self.encoded();
        if sink
            .send(runtime_collab::wire::Event::Reading {
                resource: SERVER.to_owned(),
                revision,
                reading,
            })
            .is_err()
        {
            return Err("the reader of the server went away".to_owned());
        }
        let mut changes = (self.changes)();
        let read = Arc::clone(&self.read);
        let task = tokio::spawn(async move {
            while changes.changed().await.is_ok() {
                let revision = *changes.borrow_and_update();
                let reading = encoded(&read());
                if sink
                    .send(runtime_collab::wire::Event::Reading {
                        resource: SERVER.to_owned(),
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

    /// A verb of the server is the action API's, not this reading's: the daemon's dispatch
    /// answers `stop` and `go-on` where it keeps what they change.
    fn invoke(
        &self,
        _caller: &Caller,
        resource: &str,
        action: &str,
        _arguments: Vec<u8>,
    ) -> oneshot::Receiver<runtime_collab::wire::Receipt> {
        let (answer, answered) = oneshot::channel();
        let receipt = if resource == SERVER {
            runtime_collab::wire::Receipt::Refused {
                code: "unknown_action".to_owned(),
                message: format!("`{action}` is asked of the server's action API, not its reading"),
            }
        } else {
            runtime_collab::wire::Receipt::Refused {
                code: "unknown_action".to_owned(),
                message: format!("this device publishes no `{resource}` resource"),
            }
        };
        let _ = answer.send(receipt);
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

    /// A provider over one value a test changes, and one revision it moves with.
    fn provider(reading: Arc<StdMutex<String>>, revision: u64) -> (Published, watch::Sender<u64>) {
        let (sender, _) = watch::channel(revision);
        let read = Arc::clone(&reading);
        let changing = sender.clone();
        (
            Published::over(
                Arc::new(move || {
                    serde_json::json!({ "agents": [*read
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)] })
                }),
                Arc::new(move || changing.subscribe()),
            ),
            sender,
        )
    }

    #[tokio::test]
    async fn the_server_reads_and_each_change_reaches_the_watch() {
        let reading = Arc::new(StdMutex::new("first".to_owned()));
        let (published, changed) = provider(Arc::clone(&reading), 1);

        let (revision, bytes) = published
            .snapshot(&caller(), SERVER)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(revision, 1);
        let value: Value = serde_json::from_slice(&bytes.unwrap()).unwrap();
        assert_eq!(value["agents"][0], "first");

        let (sink, mut events) = mpsc::unbounded_channel();
        let watch = published.watch(&caller(), SERVER, sink).unwrap();
        assert!(matches!(
            events.try_recv().unwrap(),
            runtime_collab::wire::Event::Reading { revision: 1, .. }
        ));

        *reading
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
        let value: Value = serde_json::from_slice(&reading.unwrap()).unwrap();
        assert_eq!(value["agents"][0], "second");

        published.unwatch(watch);
        assert!(
            published
                .watch(&caller(), "mcp.nothing", mpsc::unbounded_channel().0)
                .is_err(),
            "no other resource is this provider's"
        );
        let (_, none) = published
            .snapshot(&caller(), "mcp.nothing")
            .await
            .unwrap()
            .unwrap();
        assert!(none.is_none());
    }
}
