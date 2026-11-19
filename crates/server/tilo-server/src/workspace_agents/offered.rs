//! The agent tools a turn on this server offers its model.
//!
//! The agent tools list `tilo_view`, which reads what a person's window shows for a path. A
//! turn this server carries out arrives on no connection, and its tools act on the workspace's
//! action service, where an app opens no window: nothing ever shows on that connection, so the
//! tool answers nothing but "no window shows …", and a step spent on it is lost. It is left out
//! of what the model is offered, and what the model is told says what reaches the same thing
//! instead ([`super::instructions`]).

use serde_json::Value;
use tilo_agent_turn::{Answer, Tools};
use tilo_mcp::McpServer;

use super::{ToolDefinition, WorkspaceActionProvider};

/// The tools a turn on this server is not offered: each reads a window of its person's on the
/// connection the turn's tools act on, and a turn here arrives on none.
pub(super) const NO_WINDOW: [&str; 1] = ["tilo_view"];

/// The agent tools of the turn's server, less those no window here can answer.
pub(super) struct Offered(McpServer<WorkspaceActionProvider>);

impl Offered {
    /// The tools of `server`, less those no window here can answer.
    pub(super) fn of(server: McpServer<WorkspaceActionProvider>) -> Self {
        Self(server)
    }

    /// The tools the model is offered of `server`: every one it lists but [`NO_WINDOW`].
    pub(super) async fn listed_of(
        server: &McpServer<WorkspaceActionProvider>,
    ) -> Result<Vec<ToolDefinition>, String> {
        Tools::list(server).await.map(|tools| {
            tools
                .into_iter()
                .filter(|tool| !NO_WINDOW.contains(&tool.name.as_str()))
                .collect()
        })
    }

    /// The server whose tools these are, which a document a turn answers with is read through.
    pub(super) fn server(&self) -> &McpServer<WorkspaceActionProvider> {
        &self.0
    }
}

impl Tools for Offered {
    async fn list(&self) -> Result<Vec<ToolDefinition>, String> {
        Self::listed_of(&self.0).await
    }

    async fn call(&self, name: &str, arguments: Value) -> Answer {
        Tools::call(&self.0, name, arguments).await
    }
}
