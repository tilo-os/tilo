//! The tools an agent whose turn is worked in a process of its own (the Agent app) works through,
//! taken here for it: two steps of its agent session under
//! [`agent_tools::PROVIDER`](tilo_actions::remote::agent_tools::PROVIDER), listing the workspace's
//! MCP tools and taking one call of them.
//!
//! The broker admits each step as one of the agent's session and hands it the session's own API;
//! every call is made through that API ([`tilo_mcp::agent_tools`]), so it is the agent's, decided
//! on its grants, asked of its person first where the workspace says so, and named in History as
//! the session.

use tilo_actions::remote::Pending;
use tilo_actions::remote::agent_tools as wire;
use tilo_broker_core::steps::{HostStepOutcome, HostStepRequest, HostStepServices};

/// The agent sessions' tools, as this session takes them.
#[derive(Default)]
pub(crate) struct AgentToolSteps(tilo_mcp::agent_tools::AgentTools);

/// Step `action`, described for the broker's catalogue.
fn descriptor(action: &str, label: &str, detail: &str) -> tilo_actions::ActionDescriptor {
    use tilo_session_core::desktop::i18n::t;
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

impl HostStepServices for AgentToolSteps {
    fn provider(&self) -> &'static str {
        wire::PROVIDER
    }

    fn app_id(&self) -> &'static str {
        wire::APP
    }

    fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
        vec![
            descriptor(wire::LIST, "agent-tools-list", "agent-tools-list-detail"),
            descriptor(wire::CALL, "agent-tools-call", "agent-tools-call-detail"),
        ]
    }

    fn for_agent_sessions(&self) -> bool {
        true
    }

    fn listed(&self) -> bool {
        false
    }

    fn start(&self, request: &HostStepRequest<'_>) -> Result<Pending<HostStepOutcome>, String> {
        if request.address.resource != wire::RESOURCE {
            return Err("unknown step of an agent's tools".into());
        }
        let actor = request.actor;
        if !actor.is_agent() {
            return Err("an agent's tools are its own".into());
        }
        let session = actor
            .session()
            .ok_or("an agent calls its tools as a step of its session")?;
        let provider = request
            .provider
            .clone()
            .ok_or("an agent calls its tools only through its session's API")?;
        self.0.start(&tilo_mcp::agent_tools::Step {
            workspace: request.workspace,
            agent: actor.member().id(),
            session,
            provider,
            action: &request.address.action,
            argument: request.argument,
        })
    }
}
