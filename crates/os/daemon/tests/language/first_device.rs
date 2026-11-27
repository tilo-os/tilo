//! A computer's first start keeps the person's own records: this device is listed in their
//! devices record, and an agent's session opens on it, which certifies the agent's key there.
//!
//! Both are writes to `tilo.devices` from the start itself, so the store a device's service
//! builds has to declare that kind before either lands. A start that declared none refused both
//! with ``Undeclared("no type `tilo.devices` is declared")``, and the daemon went down with the
//! agent's session.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::Mutex;

use serde_json::json;

use crate::language::host;

/// The workspaces the daemon issued an invitation into, one entry per invitation, as it logs
/// each.
static INVITED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Keeps the workspace of every invitation the daemon issues in [`INVITED`].
struct Invitations;

/// The `message` and `workspace` fields of a traced event.
#[derive(Default)]
struct Issued {
    message: Option<String>,
    workspace: Option<String>,
}

impl tracing::field::Visit for Issued {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "message" => self.message = Some(format!("{value:?}")),
            "workspace" => self.workspace = Some(format!("{value:?}")),
            _ => {}
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "message" => self.message = Some(value.to_owned()),
            "workspace" => self.workspace = Some(value.to_owned()),
            _ => {}
        }
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Invitations {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        let mut issued = Issued::default();
        event.record(&mut issued);
        if issued.message.as_deref() == Some("workspace invitation issued") {
            INVITED
                .lock()
                .unwrap()
                .push(issued.workspace.unwrap_or_default());
        }
    }
}

/// How many invitations into `workspace` the daemon issued so far.
fn invitations_into(workspace: &str) -> usize {
    INVITED
        .lock()
        .unwrap()
        .iter()
        .filter(|invited| invited.as_str() == workspace)
        .count()
}

#[test]
fn a_first_start_lists_its_device_and_opens_an_agents_session() {
    host::run(|host| async move {
        let agent = host.agent();
        // The session answers as `tilo mcp` asks it to, so its agent's key is certified in the
        // person's devices record and the session serves.
        let listed = agent.ask("tools/list", json!({})).await;
        assert!(
            listed["result"]["tools"]
                .as_array()
                .is_some_and(|tools| !tools.is_empty()),
            "the agent's session lists its tools: {listed}"
        );
        // The device is in the record as the start keeps it, read back through the API a person's
        // key asks: the write the agent's session made is the one the start made.
        let key = host.key("first start");
        let workspace = host.workspace();
        let asked = format!("/v1/workspaces/{workspace}/resources?path=devices&limit=40");
        let (status, listed) = key.get(&asked).await;
        assert_eq!(status, 200, "the person's devices record reads: {listed}");
        assert!(
            listed["rows"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty()),
            "the person's devices record lists this device: {listed}"
        );
    });
}

/// Every `tilo mcp` connection is a run of the computer's one agent, under the one key this
/// device made it. That key enters the person's workspace by an invitation the first time; a
/// connection after that finds it listed there and comes in as it is. An invitation per
/// connection was a change to the workspace's member list on every connection, sent to each of
/// its devices: a program that reconnected every few seconds wrote one every few seconds, for as
/// long as it ran.
#[test]
fn an_agent_that_connects_again_is_invited_into_the_workspace_once() {
    host::run(|host| async move {
        host::also_trace(Box::new(Invitations));
        let workspace = host.workspace().to_owned();
        let first = host.agent();
        let listed = first.ask("tools/list", json!({})).await;
        assert!(
            listed["result"]["tools"].is_array(),
            "the agent's first session serves: {listed}"
        );
        let invited = invitations_into(&workspace);
        for connection in 1..=3 {
            let again = host.agent();
            let listed = again.ask("tools/list", json!({})).await;
            assert!(
                listed["result"]["tools"].is_array(),
                "the agent's connection {connection} after the first serves: {listed}"
            );
        }
        assert_eq!(
            invitations_into(&workspace),
            invited,
            "an agent whose key the workspace already lists is invited into it again on each \
             connection"
        );
    });
}
