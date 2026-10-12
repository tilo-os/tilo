//! The canonical console action door, carried by the existing owner-authenticated RPC.

use console_control::{catalog, wire};
use serde_json::{Value, json};
use tilo_actions::{
    ActionParam, ActionSpec, ConfirmationPolicy, EncodedAction, OperationKind, ParamType, Resource,
};
use tilo_vm_protocol::{DeviceUnlockEnrol, Request};

use crate::{Daemon, DaemonError};

const PROVIDER: &str = "device-unlock";

fn resources() -> Vec<Resource> {
    let mut resource = Resource::new(PROVIDER, "device.encrypted-root", "Encrypted device");
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
    spec.contract.confirmation = ConfirmationPolicy::Confirm;
    spec.contract.kind = tilo_actions::ActionKind::Outside;
    resource.actions.push(spec);
    vec![resource]
}

impl Daemon {
    pub(crate) fn device_actions(&mut self, line: &str) -> Result<Value, DaemonError> {
        if line.len() > 65536 {
            return Ok(
                json!({"err":{"code":"invalid_arguments","message":"Device action arguments exceed their limit"}}),
            );
        }
        // A real current owner session is required even to publish its device surface.
        self.user_device_action_session()?;
        let request = wire::parse(line)
            .map_err(|_| DaemonError::InvalidParams("invalid device action request".into()))?;
        match request {
            wire::Request::Enumerate { query } => {
                Ok(json!({"answer":catalog::enumerate(PROVIDER, &resources(), &query)}))
            }
            wire::Request::StateDump => {
                Ok(json!({"state":{"provider":PROVIDER,"resource":"device.encrypted-root"}}))
            }
            wire::Request::Unsupported { .. } => Ok(
                json!({"err":{"code":"not_implemented","message":"This route serves semantic device actions only"}}),
            ),
            wire::Request::Invoke {
                provider,
                id,
                mut args,
            } => {
                if let Err(refusal) = catalog::admit(PROVIDER, &resources(), &provider, &id, &args)
                {
                    // Door details can include malformed argument values. Return the
                    // authoritative refusal code with a content-free human diagnostic.
                    return Ok(
                        json!({"err":{"code":refusal.code,"message":"Device action was refused; inspect its declared parameters and confirmation"}}),
                    );
                }
                args.remove(catalog::CONFIRM_ARG);
                let request: DeviceUnlockEnrol = serde_json::from_value(Value::Object(args))
                    .map_err(|_| {
                        DaemonError::InvalidParams("invalid private enrollment arguments".into())
                    })?;
                self.handle(Request::DeviceUnlockEnrol(request))
                    .map(|result| json!({"answer":result}))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_cli_request_reaches_the_declared_console_admission() {
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
            "--args-stdin",
        ])
        .unwrap();
        let Some(Command::Vm(VmCommand::Actions { command, .. })) = cli.command else {
            panic!("the maintained CLI did not select device actions");
        };
        let input = br#"{"passphrase":"private-fixture-answer","confirm":true}"#;
        let request = tilo_vm::device_actions::request(command, &input[..]).unwrap();
        let wire::Request::Invoke { provider, id, args } = wire::parse(&request.line).unwrap()
        else {
            panic!("the transport did not carry canonical invocation");
        };
        let admitted = catalog::admit(PROVIDER, &resources(), &provider, &id, &args).unwrap();
        assert_eq!(admitted.encoded.type_id, "device.unlock/v1");
        assert_eq!(admitted.encoded.payload, br#"{"action":"enroll"}"#);
        assert!(!format!("{request:?}").contains("private-fixture-answer"));
    }

    #[test]
    fn enrollment_is_a_real_secret_action_and_encoding_carries_no_credential() {
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
        let catalogue = catalog::enumerate(PROVIDER, &resources, "disk unlock");
        assert_eq!(catalogue["count"], 1);
        let args = json!({"passphrase":"private-fixture-answer","confirm":true})
            .as_object()
            .unwrap()
            .clone();
        let admitted = catalog::admit(
            PROVIDER,
            &resources,
            PROVIDER,
            "enroll@device-unlock",
            &args,
        )
        .unwrap();
        assert!(
            !String::from_utf8(admitted.encoded.payload)
                .unwrap()
                .contains("private-fixture-answer")
        );
        assert_eq!(catalogue["actions"][0]["params"][0]["ty"], "secret");
        let refused =
            json!({"passphrase":"private-fixture-answer","device":"/dev/attacker","confirm":true})
                .as_object()
                .unwrap()
                .clone();
        assert!(
            catalog::admit(
                PROVIDER,
                &resources,
                PROVIDER,
                "enroll@device-unlock",
                &refused
            )
            .is_err()
        );
    }
}
