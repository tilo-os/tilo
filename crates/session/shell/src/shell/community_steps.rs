//! The community workspace's steps on the action door: where the person
//! stands with the workspace a Tilo computer is in by default, joining it
//! again after they left it, and leaving it, for a script or an assistive
//! tool acting for the person. People do the same in "Join a workspace…"
//! and the workspace's own menu. The membership is the network's
//! ([`ShellNetwork::join_site`]); this door only reads it and asks.

use std::sync::Arc;

use tilo_actions::remote::community::{CommunityStatus, JOIN, LEAVE, RESOURCE, STATUS, STEPS};
use tilo_actions::remote::{Pending, pending};
use tilo_actions::{ActionParam, ParamType};

use super::network::ShellNetwork;

/// The application whose controls reach the steps.
const SETTINGS_APP_ID: &str = "so.tilo.settings";

const LEAVE_PARAMS: &[ActionParam] = &[ActionParam::required(
    "confirm",
    ParamType::Boolean,
    "Explicit confirmation that this computer leaves the community workspace.",
)];

/// The door over the session's network.
pub(crate) struct CommunityDoor(pub(crate) Arc<ShellNetwork>);

/// This session's door to the community steps over `network`.
pub(crate) fn broker_door(
    network: &Arc<ShellNetwork>,
) -> Arc<dyn tilo_action_broker::services::CommunityServices> {
    Arc::new(CommunityDoor(Arc::clone(network)))
}

fn contract(step: &str) -> tilo_actions::ActionContract {
    use tilo_actions::{
        ActionContract, ActionKind, ConfirmationPolicy, ExecutionMode, ResultSpec, RetryPolicy,
        RiskLevel,
    };
    let tags = vec![tilo_actions::LOCAL_DEVICE_ONLY.into()];
    let result = ResultSpec::Json {
        schema: serde_json::json!({ "type": "object" }),
    };
    match step {
        STATUS => ActionContract {
            tags,
            result,
            ..ActionContract::view()
        },
        JOIN => ActionContract {
            kind: ActionKind::Outside,
            risk: RiskLevel::Low,
            retry: RetryPolicy::Idempotent,
            execution: ExecutionMode::Background { cancellable: false },
            tags,
            result,
            ..ActionContract::default()
        },
        _ => ActionContract {
            kind: ActionKind::Outside,
            risk: RiskLevel::Low,
            confirmation: ConfirmationPolicy::Confirm,
            retry: RetryPolicy::Idempotent,
            tags,
            result,
            ..ActionContract::default()
        },
    }
}

fn descriptor(step: &str) -> tilo_actions::ActionDescriptor {
    use crate::shell::i18n::t;
    let (label, description) = match step {
        STATUS => ("community-status", "community-status-subtitle"),
        JOIN => ("community-join", "community-join-subtitle"),
        _ => ("community-leave", "community-leave-subtitle"),
    };
    tilo_actions::ActionDescriptor {
        semantic_identity: None,
        address: tilo_actions::ActionAddress::new(RESOURCE, step),
        resource_kind: RESOURCE.into(),
        resource_label: t("community-resource"),
        action_label: t(label),
        description: Some(t(description)),
        locus: tilo_actions::ExecutionLocus::default(),
        kind: if step == STATUS {
            tilo_actions::OperationKind::View
        } else {
            tilo_actions::OperationKind::Effect
        },
        contract: contract(step),
        input: tilo_actions::InputSpec::None,
        params: if step == LEAVE {
            LEAVE_PARAMS.to_vec()
        } else {
            Vec::new()
        },
        view: None,
    }
}

fn answer(
    value: Result<serde_json::Value, String>,
) -> tilo_action_broker::services::CommunityOutcome {
    value
        .map(|value| tilo_actions::ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        })
        .map_err(|message| {
            Box::new(tilo_actions::ActionFailure {
                code: "community_failed".into(),
                message,
                details: None,
                retryable: true,
            })
        })
}

impl CommunityDoor {
    fn status(&self) -> CommunityStatus {
        let Some(site) = super::network::configured_community_site() else {
            return CommunityStatus {
                site: None,
                joined: false,
                left: false,
            };
        };
        CommunityStatus {
            joined: self.0.holds_site(&site),
            left: self.0.left_site(&site),
            site: Some(site.to_string()),
        }
    }
}

impl tilo_action_broker::services::CommunityServices for CommunityDoor {
    fn app_id(&self) -> &str {
        SETTINGS_APP_ID
    }

    fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
        STEPS.into_iter().map(descriptor).collect()
    }

    fn start(
        &self,
        request: &tilo_action_broker::services::CommunityRequest<'_>,
    ) -> Result<Pending<tilo_action_broker::services::CommunityOutcome>, String> {
        // Membership is the person's own: an agent or an app acting for
        // nobody never changes it through here.
        request
            .actor
            .direct_person()
            .ok_or("only a person joins or leaves their community")?;
        if request.address.resource != RESOURCE {
            return Err("unknown community step".into());
        }
        let status =
            |door: &Self| serde_json::to_value(door.status()).map_err(|error| error.to_string());
        match request.address.action.as_str() {
            STATUS => Ok(Pending::ready(answer(status(self)))),
            LEAVE => {
                let site = super::network::configured_community_site()
                    .ok_or("this computer has no community workspace")?;
                self.0.leave_community(&site)?;
                Ok(Pending::ready(answer(status(self))))
            }
            JOIN => {
                let site = super::network::configured_community_site()
                    .ok_or("this computer has no community workspace")?;
                let (completer, answered) = pending();
                let network = Arc::clone(&self.0);
                std::thread::Builder::new()
                    .name("tilo-community-join".into())
                    .spawn(move || {
                        let joined = network
                            .rejoin_site(&site)
                            .map_err(|error| error.to_string())
                            .and_then(|_| {
                                serde_json::to_value(CommunityDoor(network).status())
                                    .map_err(|error| error.to_string())
                            });
                        completer.complete(answer(joined));
                    })
                    .map_err(|error| error.to_string())?;
                Ok(answered)
            }
            _ => Err("unknown community step".into()),
        }
    }
}
