//! The tools an agent whose turn is worked in a process of its own (the Agent app) works through,
//! taken by this device's daemon for it: two steps of its agent session under
//! [`agent_tools::PROVIDER`](tilo_action_services::agent_tools::PROVIDER), listing the workspace's
//! MCP tools and taking one call of them.
//!
//! The dispatch admits each step as one of the agent's session and hands it the session's own API;
//! every call is made through that API ([`tilo_mcp::agent_tools`]), so it is the agent's, decided
//! on its grants, asked of its person first where the workspace says so, and named in History as
//! the session.

use std::sync::Arc;

use tilo_action_services::agent_tools as wire;
use tilo_actions::remote::Admits;
use tilo_actions::remote::Pending;
use tilo_actions::remote::admitted::top_level;
use tilo_actions::remote::{Admitted, Caller, InvokeRequest, Outcome};
use tilo_workspace_service::workspaces::providers::{Providers, Reach};

/// The agent sessions' tools, as this device takes them.
#[derive(Default)]
pub(crate) struct AgentToolSteps(tilo_mcp::agent_tools::AgentTools);

impl AgentToolSteps {
    /// Take the agent sessions' tool calls from now on, admitted by the tools' app, taken only
    /// inside an agent session and never listed: an agent's app asks for them, not its model.
    pub(super) fn register(self: Arc<Self>, providers: &Providers) {
        providers.provide(
            wire::PROVIDER,
            wire::APP,
            Reach::AgentSessions { listed: false },
            self,
        );
    }

    /// Every tool this provider offers, described.
    fn descriptors() -> Vec<tilo_actions::ActionDescriptor> {
        vec![
            descriptor(wire::LIST, "agent-tools-list", "agent-tools-list-detail"),
            descriptor(wire::CALL, "agent-tools-call", "agent-tools-call-detail"),
        ]
    }
}

/// Step `action`, described for the broker's catalogue.
fn descriptor(action: &str, label: &str, detail: &str) -> tilo_actions::ActionDescriptor {
    use crate::i18n::t;
    tilo_actions::ActionDescriptor {
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: tilo_actions::ActionAddress::new(wire::RESOURCE, action),
        resource_kind: wire::RESOURCE.into(),
        resource_label: t("agent-tools-resource"),
        action_label: t(label),
        keywords: Vec::new(),
        icon: None,
        description: Some(t(detail)),
        locus: tilo_actions::ExecutionLocus::default(),
        // The step itself only reads: every change a call makes is its own invocation, decided
        // where it lands.
        kind: tilo_actions::OperationKind::View,
        contract: tilo_actions::ActionContract {
            result: tilo_actions::ResultSpec::Json {
                schema: serde_json::json!({ "type": "object" }),
            },
            ..tilo_actions::ActionContract::view()
        },
        input: tilo_actions::InputSpec::None,
        params: Vec::new(),
        view: None,
    }
}

impl Admits for AgentToolSteps {
    const PROVIDER: &'static str = wire::PROVIDER;
    const APP: &'static str = wire::APP;

    fn rows(&self) -> Vec<tilo_actions::api::PlacedAction> {
        top_level(Self::descriptors())
    }

    /// Carry out an invocation the device's dispatch admitted for `caller`.
    fn start(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        admitted: &Admitted,
    ) -> Result<Pending<Outcome>, String> {
        let address = &request.target.address;
        if address.resource != wire::RESOURCE {
            return Err("unknown step of an agent's tools".into());
        }
        let acting = caller.acting();
        let agent = acting.agent_id().ok_or("an agent's tools are its own")?;
        let person = acting
            .acted_for_person()
            .ok_or("an agent's tools work for its person")?;
        let session = acting
            .agent_session
            .as_ref()
            .map(tilo_member_types::AgentSessionId::as_str)
            .ok_or("an agent calls its tools as a step of its session")?;
        let provider = admitted
            .through
            .clone()
            .ok_or("an agent calls its tools only through its session's API")?;
        let workspace = caller
            .workspace()
            .ok_or("an agent's tools are called in its workspace")?;
        let argument = request.argument.to_slot();
        self.0.start(&tilo_mcp::agent_tools::Step {
            workspace,
            agent,
            person,
            session,
            provider,
            action: &address.action,
            argument: argument.as_deref(),
        })
    }
}
