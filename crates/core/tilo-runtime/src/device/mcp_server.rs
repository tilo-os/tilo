//! The person's MCP server as this device's daemon publishes it to the apps attached to it:
//! every agent the server let in, the reach it was let in for and the sessions open now,
//! read as `mcp.server`.
//!
//! The reading is the workspace service's, which this daemon holds ([`Workspaces`]): the
//! same state that stops an agent or lets it go on ([`super::services`]), served to the apps
//! as a reading so a window lists the agents that act here instead of asking each one.

use std::sync::Arc;

use super::reading::{Changes, Read, Served};

pub use tilo_action_services::mcp_server::RESOURCE as SERVER;

/// The MCP server, published to the apps this device serves, read where the workspaces keep it.
#[must_use]
pub fn published(workspaces: &Arc<crate::device::workspaces::ServiceWorkspaces>) -> Served {
    let reading = Arc::clone(workspaces);
    let changing = Arc::clone(workspaces);
    let read: Arc<Read> =
        Arc::new(move |_caller: &tilo_app_host::services::Caller| reading.copies().mcp_reading());
    let changes: Arc<Changes> = Arc::new(move || changing.copies().mcp_changes());
    Served::new(SERVER, read, changes)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use serde_json::Value;
    use tilo_app_host::services::{Caller, PublishedResources};
    use tokio::sync::{mpsc, watch};

    use super::*;

    fn caller() -> Caller {
        Caller {
            app_id: "so.tilo.console".to_owned(),
            workspace: "home".to_owned(),
            opened_for: None,
        }
    }

    /// A provider over one value a test changes, and one revision it moves with.
    fn provider(reading: &Arc<StdMutex<String>>, revision: u64) -> (Served, watch::Sender<u64>) {
        let (sender, _) = watch::channel(revision);
        let read = Arc::clone(reading);
        let changing = sender.clone();
        (
            Served::new(
                SERVER,
                Arc::new(move |_caller: &Caller| {
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
        let (published, changed) = provider(&reading, 1);

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
