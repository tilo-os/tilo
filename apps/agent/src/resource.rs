//! Agent session resource type for the one resource/action/view model.
//!
//! The agent binary is a CLI `kind = "service"` participant — it has no window
//! and does not call [`ui::framework::run`]. This module names the session
//! resource; launching stays in `main` via clap.

use serde::{Deserialize, Serialize};
use ui::framework::{ResourceApp, ViewCx};
use ui::tilo_actions::{Action, ActionError, ActionRegistry};
use ui::tilo_resource::{Doc, ModelError, Resource};
use ui::widget::Element;

/// No collaborative document mutations from the agent binary itself — it
/// applies other apps' document actions through the broker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EmptyDocAction {}

impl Action for EmptyDocAction {
    fn apply(&self, _doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
        Ok(())
    }
}

/// Opaque session projection (the agent does not own a CRDT of its own).
#[derive(Clone, Debug, Default)]
pub struct AgentState;

/// The agent session resource.
pub struct Agent;

impl Resource for Agent {
    /// No prior record kind existed for the headless agent; this is the
    /// stable id for a future hosted surface.
    const KIND: &'static str = "agent.session";
    const LABEL: &'static str = "Agent";
    const ACTION_TYPE: &'static str = "agent.action";

    type State = AgentState;
    type Action = EmptyDocAction;

    fn register(registry: &mut ActionRegistry) {
        registry.register::<EmptyDocAction>(Self::ACTION_TYPE);
    }

    fn read(_doc: &Doc) -> Result<AgentState, ModelError> {
        Ok(AgentState)
    }
}

/// Framework shell type for catalogue consistency. The CLI `main` does not
/// host it — see the crate docs and `tilo-app.toml` (`kind = "service"`).
pub struct AgentShell;

impl ResourceApp for AgentShell {
    type Resource = Agent;
    const ID: &'static str = "so.tilo.agent";
    const TITLE: &'static str = "Agent";
    const ICON: Option<&'static str> = Some("agent");

    fn view(cx: &mut ViewCx<Agent>) -> Element {
        let _ = cx;
        Box::new(ui::widget::Column::new())
    }
}
