//! The device's own actions, answered as action frames on the owner-authenticated RPC.

use serde_json::{Map, Value, json};
use tilo_actions::remote::{
    ActionCallError, CataloguePage, InvocationReceipt, InvokeArgument, InvokeRequest,
    ReceiptStatus, RemoteActionFrame, RemoteActionMessage,
};
use tilo_actions::{
    ActionOutput, ActionParam, ActionSpec, ConfirmationPolicy, EncodedAction, OperationKind,
    ParamType, ResourceSpec,
};
use tilo_vm_protocol::{DeviceUnlockEnrol, Request};

use crate::{Daemon, DaemonError};

const PROVIDER: &str = "device-unlock";

fn resources() -> Vec<ResourceSpec> {
    let mut resource = ResourceSpec::new(PROVIDER, "device.encrypted-root", "Encrypted device");
    let mut spec = ActionSpec::typed_operation(
        "enroll",
        "Enroll sibling disk unlock",
        "",
        OperationKind::Effect,
        "device.unlock/v1",
        vec![
            ActionParam::required(
                "passphrase",
                ParamType::Secret,
                "Existing disk passphrase; private transient input",
            ),
            ActionParam::optional(
                "networks",
                ParamType::Array,
                "Up to eight saved network names for early boot",
            ),
            ActionParam::optional(
                "allow_unsealed",
                ParamType::Boolean,
                "Explicitly allow readable device-factor and network material without a TPM",
            ),
        ],
        |_| EncodedAction {
            type_id: "device.unlock/v1".into(),
            payload: br#"{"action":"enroll"}"#.to_vec(),
        },
    );
    spec.type_mut().contract.confirmation = ConfirmationPolicy::Confirm;
    spec.type_mut().contract.kind = tilo_actions::ActionKind::Outside;
    resource.actions.push(spec);
    vec![resource]
}

impl Daemon {
    /// The frame that answers the frame `line`.
    pub(crate) fn device_actions(&mut self, line: &str) -> Result<Value, DaemonError> {
        if line.len() > 65536 {
            return Ok(
                json!({"err":{"code":"invalid_arguments","message":"Device action arguments exceed their limit"}}),
            );
        }
        // A real current owner session is required even to publish its device surface.
        self.user_device_action_session()?;
        let message = RemoteActionFrame::decode(line.as_bytes())
            .map_err(|_| DaemonError::InvalidParams("invalid device action frame".into()))?
            .message;
        let answer = match message {
            RemoteActionMessage::CatalogueRequest { correlation, query } => {
                RemoteActionMessage::CatalogueResponse {
                    correlation,
                    result: Ok(CataloguePage::published(PROVIDER, &resources(), &query)),
                }
            }
            RemoteActionMessage::Invoke(request) => {
                let invocation = request.invocation;
                let status = match admit(&request) {
                    Err(error) => ReceiptStatus::Rejected { error },
                    Ok(enrol) => {
                        let value = self.handle(Request::DeviceUnlockEnrol(enrol))?;
                        ReceiptStatus::Completed {
                            output: ActionOutput {
                                value: Some(value),
                                resources: Vec::new(),
                            },
                        }
                    }
                };
                RemoteActionMessage::Receipt(InvocationReceipt { invocation, status })
            }
            _ => {
                return Ok(
                    json!({"err":{"code":"not_implemented","message":"This route lists and invokes device actions only"}}),
                );
            }
        };
        serde_json::to_value(RemoteActionFrame::new(answer))
            .map_err(|error| DaemonError::InvalidParams(error.to_string()))
    }
}

/// `request` admitted against the device's actions, and the enrollment it asks.
fn admit(request: &InvokeRequest) -> Result<DeviceUnlockEnrol, ActionCallError> {
    if request.target.provider != PROVIDER {
        return Err(ActionCallError::UnknownAction);
    }
    let arguments = match &request.argument {
        InvokeArgument::Json(arguments) => arguments.clone(),
        InvokeArgument::None | InvokeArgument::Text(_) => Map::new(),
    };
    // A refusal's details can include malformed argument values: the code goes back, with a
    // content-free diagnostic.
    let refused = |error: &tilo_actions::ApiError| match ActionCallError::from_api(error) {
        ActionCallError::InvalidRequest { .. } => ActionCallError::InvalidRequest {
            reason: "Device action was refused; inspect its declared parameters".into(),
        },
        other => other,
    };
    tilo_actions::api::admit(
        &resources(),
        &request.target.address,
        &arguments,
        request.confirmation.as_ref(),
    )
    .map_err(|error| refused(&error))?;
    serde_json::from_value(Value::Object(arguments)).map_err(|_| ActionCallError::InvalidRequest {
        reason: "invalid private enrollment arguments".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::remote::{CatalogueQuery, Confirmation};

    fn frame(line: &str) -> RemoteActionMessage {
        RemoteActionFrame::decode(line.as_bytes()).unwrap().message
    }

    tilo_test_env::scenario! {
        fn device_unlock_enrollment_is_admitted_without_carrying_its_secret;

        phase canonical_cli_request_reaches_the_declared_admission;
        {
            use clap::Parser as _;
            use tilo_vm::cli::{Cli, Command, VmCommand};

            let cli = Cli::try_parse_from([
                "tilo-vm",
                "vm",
                "actions",
                "--device",
                "fixture",
                "invoke",
                PROVIDER,
                "enroll@device-unlock",
                "--confirm",
                "--args-stdin",
            ])
            .unwrap();
            let Some(Command::Vm(VmCommand::Actions { command, .. })) = cli.command else {
                panic!("the maintained CLI did not select device actions");
            };
            let input = br#"{"passphrase":"private-fixture-answer"}"#;
            let request = tilo_vm::device_actions::request(command, &input[..]).unwrap();
            let RemoteActionMessage::Invoke(invoke) = frame(&request.line) else {
                panic!("the transport did not carry an invocation");
            };
            assert_eq!(invoke.confirmation, Some(Confirmation::Confirmed));
            let enrol = admit(&invoke).unwrap();
            assert_eq!(enrol.passphrase, "private-fixture-answer");
            assert!(!format!("{request:?}").contains("private-fixture-answer"));
        }

        phase the_unlock_secret_is_a_real_secret_action_and_a_refusal_carries_no_credential;
        {
            let resources = resources();
            assert_eq!(
                resources
                    .first()
                    .unwrap()
                    .actions
                    .first()
                    .unwrap()
                    .contract
                    .kind,
                tilo_actions::ActionKind::Outside
            );
            let query = CatalogueQuery {
                needle: "disk unlock".into(),
                ..CatalogueQuery::default()
            };
            let page = CataloguePage::published(PROVIDER, &resources, &query);
            assert_eq!(page.rows.len(), 1);
            let enroll = page.rows[0].action.as_ref().unwrap();
            assert_eq!(enroll.params[0].ty, ParamType::Secret);
            let invoke = |args: Value, confirmation: Option<Confirmation>| {
                let mut request = InvokeRequest::plain(
                    tilo_actions::remote::RemoteActionTarget {
                        provider: PROVIDER.into(),
                        address: tilo_actions::ActionAddress::new(PROVIDER, "enroll"),
                    },
                    args.as_object().unwrap().clone(),
                );
                request.confirmation = confirmation;
                admit(&request)
            };
            assert!(matches!(
                invoke(json!({"passphrase":"private-fixture-answer"}), None),
                Err(ActionCallError::ConfirmationRequired { .. })
            ));
            let refused = invoke(
                json!({"passphrase":"private-fixture-answer","device":"/dev/attacker"}),
                Some(Confirmation::Confirmed),
            )
            .unwrap_err();
            assert!(!refused.to_string().contains("private-fixture-answer"));
            assert!(
                invoke(json!({"passphrase":"private-fixture-answer"}), Some(Confirmation::Confirmed))
                    .is_ok()
            );
        }
    }
}
