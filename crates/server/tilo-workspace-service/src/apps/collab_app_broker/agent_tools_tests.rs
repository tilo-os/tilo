//! An agent whose turn is worked in its own process calls the workspace's tools as steps of its
//! agent session ([`tilo_actions::remote::agent_tools`]): the session takes the MCP tools for it through the
//! session's own API, so an action a call takes is the agent's, in the session's group, exactly as
//! the agent's own invocation is.

use super::agent_connection::{admit, answered, connection};
use super::*;
use serde_json::json;
use tilo_actions::remote::agent_tools as wire;
use tilo_actions::remote::{ActionProvider as _, InvokeRequest, RemoteActionTarget};
use tilo_actions::{ActionAddress, InvocationId};

/// What the agent session's name is in History.
const NAME: &str = "Claude Code #1";
/// The agent session [`admit`] lets in.
const SESSION: &str = "so.tilo.session/agent-session/one";

/// The agent tools, as a device's session serves them.
#[derive(Default)]
struct Tools(tilo_mcp::agent_tools::AgentTools);

impl crate::apps::services::HostStepServices for Tools {
    fn provider(&self) -> &'static str {
        wire::PROVIDER
    }
    fn app_id(&self) -> &'static str {
        wire::APP
    }
    fn for_agent_sessions(&self) -> bool {
        true
    }
    fn listed(&self) -> bool {
        false
    }
    fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
        [wire::LIST, wire::CALL]
            .into_iter()
            .map(|action| tilo_actions::ActionDescriptor {
                did: None,
                semantic_identity: None,
                address: ActionAddress::new(wire::RESOURCE, action),
                resource_kind: wire::RESOURCE.into(),
                resource_label: "Agent tools".into(),
                action_label: action.into(),
                keywords: Vec::new(),
                icon: None,
                description: None,
                state: tilo_actions::ActionState::default(),
                locus: tilo_actions::ExecutionLocus::default(),
                kind: tilo_actions::OperationKind::View,
                contract: tilo_actions::ActionContract::view(),
                input: tilo_actions::InputSpec::None,
                params: Vec::new(),
                view: None,
            })
            .collect()
    }
    fn start(
        &self,
        request: &crate::apps::services::HostStepRequest<'_>,
    ) -> Result<tilo_actions::remote::Pending<crate::apps::services::HostStepOutcome>, String> {
        let session = request.actor.session().ok_or("no session")?;
        self.0.start(&tilo_mcp::agent_tools::Step {
            workspace: request.workspace,
            agent: request.actor.member().id(),
            session,
            provider: request.provider.clone().ok_or("no session API")?,
            action: &request.address.action,
            argument: request.argument,
        })
    }
}

/// A desktop whose one step lists its windows, keeping who took it and in which work.
#[derive(Default)]
struct Desktop {
    taken: Mutex<Vec<(tilo_actions::MemberId, Option<String>)>>,
}

impl crate::apps::services::HostStepServices for Desktop {
    fn provider(&self) -> &'static str {
        "test-desktop"
    }
    fn app_id(&self) -> &'static str {
        "so.tilo.desktop"
    }
    fn for_agent_sessions(&self) -> bool {
        true
    }
    fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
        vec![tilo_actions::ActionDescriptor {
            did: None,
            semantic_identity: None,
            address: ActionAddress::new("desktop", "list-windows"),
            resource_kind: "desktop".into(),
            resource_label: "Desktop".into(),
            action_label: "List windows".into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            state: tilo_actions::ActionState::default(),
            locus: tilo_actions::ExecutionLocus::default(),
            kind: tilo_actions::OperationKind::View,
            contract: tilo_actions::ActionContract::view(),
            input: tilo_actions::InputSpec::None,
            params: Vec::new(),
            view: None,
        }]
    }
    fn start(
        &self,
        request: &crate::apps::services::HostStepRequest<'_>,
    ) -> Result<tilo_actions::remote::Pending<crate::apps::services::HostStepOutcome>, String> {
        self.taken.lock().unwrap().push((
            request.actor.member().clone(),
            request.group.map(|group| group.id.clone()),
        ));
        Ok(tilo_actions::remote::Pending::ready(Ok(
            tilo_actions::ActionOutput::default(),
        )))
    }
}

/// Step `action` of the agent's tools with `argument`, as invocation `id`.
fn step(id: u128, action: &str, argument: &serde_json::Value) -> InvokeRequest {
    InvokeRequest {
        invocation: InvocationId(uuid::Uuid::from_u128(id)),
        target: RemoteActionTarget {
            provider: wire::PROVIDER.into(),
            address: ActionAddress::new(wire::RESOURCE, action),
        },
        argument: wire::argument(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        appearance: None,
        wait_ms: None,
        signed: None,
    }
}

/// What step `request` made once it ended, the broker taking its turn meanwhile.
async fn ended(f: &Fixture, api: &AgentConnection, request: InvokeRequest) -> serde_json::Value {
    let caller = connection();
    let id = request.invocation;
    let receipt = answered(f, api.invoke(&caller, request))
        .await
        .expect("answered")
        .expect("the step is taken");
    if let tilo_actions::remote::ReceiptStatus::Completed { output } = receipt.status {
        return output.value.expect("a value");
    }
    tilo_test_env::within("the step ends", async {
        loop {
            f.state.take_turn(&f.directories);
            if let Some(state) = api.observe(&caller, &id) {
                match state.update {
                    tilo_actions::InvocationUpdate::Succeeded { output, .. } => {
                        break output.value.expect("a value");
                    }
                    update if update.is_terminal() => panic!("the step ended {update:?}"),
                    _ => {}
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
}

/// The agent lists its tools and calls one that takes the desktop's step: the step is the agent's,
/// in its session's work, as the broker hands it to whoever takes it.
#[tokio::test(flavor = "current_thread")]
async fn an_agents_tool_call_runs_as_the_agent_in_its_session() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let desktop = Arc::new(Desktop::default());
            let f = Fixture::with_services(None, vec![Arc::new(Tools::default()), desktop.clone()]);
            let agent = admit(&f);
            let api = f
                .state
                .agent_connection(&agent.presentation(), NAME)
                .unwrap();

            let listed: wire::ToolList =
                serde_json::from_value(ended(&f, &api, step(61, wire::LIST, &json!({}))).await)
                    .unwrap();
            let names: Vec<&str> = listed.tools.iter().map(|tool| tool.name.as_str()).collect();
            for tool in ["tilo_ls", "tilo_read", "tilo_do", "tilo_find"] {
                assert!(names.contains(&tool), "{names:?}");
            }

            let call = |id, name: &str, arguments| {
                step(
                    id,
                    wire::CALL,
                    &json!({"name": name, "arguments": arguments}),
                )
            };
            // The desktop's step is listed to the agent's session; its own tools are not.
            let ls: wire::ToolAnswer = serde_json::from_value(
                ended(&f, &api, call(62, "tilo_ls", json!({"path": "desktop"}))).await,
            )
            .unwrap();
            assert!(
                !ls.failed && ls.text.contains("#list-windows"),
                "{}",
                ls.text
            );
            assert!(
                !ls.text.contains(wire::CALL),
                "the agent's tools are taken, never listed: {}",
                ls.text
            );

            let done: wire::ToolAnswer = serde_json::from_value(
                ended(
                    &f,
                    &api,
                    call(63, "tilo_do", json!({"path": "desktop#list-windows"})),
                )
                .await,
            )
            .unwrap();
            assert!(!done.failed, "{}", done.text);
            assert_eq!(
                desktop.taken.lock().unwrap().as_slice(),
                [(
                    tilo_actions::MemberId::agent(agent.delegation.agent_id.clone()),
                    Some(SESSION.to_owned())
                )],
                "the call is the agent's, a step of its session"
            );
            drop(f);
        })
        .await;
}
