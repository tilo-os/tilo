//! The broker's door to home-relay hosting: how Settings, as the person
//! using it, lists the workspaces this device governs with whether their
//! home relay hosts each too, and chooses it for one
//! ([`super::network::ShellNetwork::home_relay_hosting`]).
//!
//! Only a person takes these steps, and only for a workspace they may
//! grant in: having the relay host a room hands it the room. Each step
//! reaches this device's host, and choosing reaches the relay, so both run
//! on a worker thread and answer once done.

use std::sync::Arc;

use tilo_actions::remote::home_relay_hosting::{
    HomeRelayHosting, PROVIDER, RESOURCE, SET, STEPS, WORKSPACES,
};
use tilo_actions::remote::{Pending, pending};
use tilo_actions::{ActionArgs, ActionParam, ParamType};

use super::network::ShellNetwork;
use super::state::{RoomId, WeakWorkspaceTreeHandle};

/// The application whose Devices & Sync controls reach the steps.
const SETTINGS_APP_ID: &str = "so.tilo.settings";

const SET_PARAMS: &[ActionParam] = &[
    ActionParam::required("room", ParamType::Text, "The workspace's room id."),
    ActionParam::required(
        "on",
        ParamType::Boolean,
        "Whether the home relay hosts the workspace as well.",
    ),
];

/// What `step` takes.
fn params(step: &str) -> &'static [ActionParam] {
    if step == SET { SET_PARAMS } else { &[] }
}

/// What `step` may do and answers. Listing reads; choosing hands the room to
/// the person's own relay or takes it back, asked as a switch is: the
/// person flipping it is the request.
fn contract(step: &str) -> tilo_actions::ActionContract {
    use tilo_actions::{ActionContract, ActionKind, ResultSpec, RetryPolicy, RiskLevel};
    let json = |schema: serde_json::Value| ResultSpec::Json { schema };
    if step == WORKSPACES {
        ActionContract {
            result: json(serde_json::json!({ "type": "object" })),
            ..ActionContract::view()
        }
    } else {
        ActionContract {
            kind: ActionKind::Outside,
            risk: RiskLevel::Low,
            retry: RetryPolicy::Idempotent,
            execution: tilo_actions::ExecutionMode::Background { cancellable: false },
            result: json(serde_json::json!({ "type": "object" })),
            ..ActionContract::default()
        }
    }
}

fn descriptor(step: &str) -> tilo_actions::ActionDescriptor {
    use crate::shell::i18n::t;
    let (label, description) = if step == WORKSPACES {
        (
            "search-action-home-relay-workspaces",
            "search-action-home-relay-workspaces-subtitle",
        )
    } else {
        (
            "search-action-home-relay-set",
            "search-action-home-relay-set-subtitle",
        )
    };
    tilo_actions::ActionDescriptor {
        semantic_identity: None,
        address: tilo_actions::ActionAddress::new(RESOURCE, step),
        resource_kind: RESOURCE.into(),
        resource_label: t("search-action-home-relay-resource"),
        action_label: t(label),
        description: Some(t(description)),
        locus: tilo_actions::ExecutionLocus::default(),
        kind: if step == WORKSPACES {
            tilo_actions::OperationKind::View
        } else {
            tilo_actions::OperationKind::Effect
        },
        contract: contract(step),
        input: tilo_actions::InputSpec::None,
        params: params(step).to_vec(),
        view: None,
    }
}

/// `argument`, in its slot form, bound to `params`.
fn bind_slot(params: &[ActionParam], argument: Option<&str>) -> Result<ActionArgs, String> {
    let supplied = match argument {
        None | Some("") => serde_json::Map::new(),
        Some(slot) => {
            let json = slot
                .strip_prefix(tilo_actions::params::ARGUMENTS_JSON_PREFIX)
                .ok_or("home relay steps take JSON arguments")?;
            serde_json::from_str(json).map_err(|error| error.to_string())?
        }
    };
    ActionArgs::bind_json(params, &supplied).map_err(|error| error.to_string())
}

fn outcome(
    answer: Result<serde_json::Value, String>,
) -> tilo_action_broker::services::HostStepOutcome {
    answer
        .map(|value| tilo_actions::ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        })
        .map_err(|message| {
            Box::new(tilo_actions::ActionFailure {
                code: "home_relay_hosting_failed".into(),
                message,
                details: None,
                retryable: false,
            })
        })
}

/// The door over this session's workspaces and network.
pub(crate) struct BrokerDoor {
    tree: WeakWorkspaceTreeHandle,
    network: Arc<ShellNetwork>,
}

/// This session's door to home-relay hosting.
pub(crate) fn broker_door(
    tree: &super::state::WorkspaceTreeHandle,
    network: Arc<ShellNetwork>,
) -> Arc<dyn tilo_action_broker::services::HostStepServices> {
    Arc::new(BrokerDoor {
        tree: tree.downgrade(),
        network,
    })
}

/// Whether `person` may grant in `room`'s workspace here: handing the room
/// to another host is theirs to decide.
fn may_choose(tree: &WeakWorkspaceTreeHandle, room: &str, person: &str) -> bool {
    tree.upgrade().is_some_and(|tree| {
        tree.read()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room().to_string() == room)
            .is_some_and(|workspace| {
                workspace.may_govern_at(
                    person,
                    tilo_action_policy::roles::governance::GRANT,
                    tilo_clock::unix_now(),
                )
            })
    })
}

/// `hosting`, without the workspaces `person` may not choose for.
fn chosen_by(
    tree: &WeakWorkspaceTreeHandle,
    person: &str,
    mut hosting: HomeRelayHosting,
) -> HomeRelayHosting {
    hosting
        .workspaces
        .retain(|workspace| may_choose(tree, &workspace.room, person));
    hosting
}

impl tilo_action_broker::services::HostStepServices for BrokerDoor {
    fn provider(&self) -> &'static str {
        PROVIDER
    }

    fn app_id(&self) -> &str {
        SETTINGS_APP_ID
    }

    fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
        STEPS.into_iter().map(descriptor).collect()
    }

    fn start(
        &self,
        request: &tilo_action_broker::services::HostStepRequest<'_>,
    ) -> Result<Pending<tilo_action_broker::services::HostStepOutcome>, String> {
        let person = request
            .actor
            .direct_person()
            .ok_or("only a person chooses where their workspaces are hosted")?
            .to_owned();
        let step = STEPS
            .into_iter()
            .find(|step| request.address.resource == RESOURCE && request.address.action == *step)
            .ok_or("unknown home relay step")?;
        let args = bind_slot(params(step), request.argument)?;
        let set = if step == SET {
            let room = args.text("room").unwrap_or_default().to_owned();
            if !may_choose(&self.tree, &room, &person) {
                return Err(format!(
                    "{person} may not choose where this workspace is hosted"
                ));
            }
            let room = RoomId::parse(&room).ok_or("not a workspace's room")?;
            Some((room, args.boolean("on").unwrap_or(false)))
        } else {
            None
        };
        let (completer, answer) = pending();
        let network = Arc::clone(&self.network);
        let tree = self.tree.clone();
        std::thread::Builder::new()
            .name("tilo-home-relay".into())
            .spawn(move || {
                let answered = match set {
                    Some((room, on)) => network
                        .set_home_relay_hosting(room, on)
                        .map(|()| serde_json::json!({ "room": room.to_string(), "on_relay": on })),
                    None => network.home_relay_hosting().and_then(|hosting| {
                        serde_json::to_value(chosen_by(&tree, &person, hosting))
                            .map_err(|error| error.to_string())
                    }),
                };
                completer.complete(outcome(answered));
            })
            .map_err(|error| format!("could not ask this device's host: {error}"))?;
        Ok(answer)
    }
}
