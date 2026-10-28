//! The locked session's resource on the authenticated owner control socket.
//! This module never clears the lock: it only submits to the normal PAM worker.

use serde_json::json;
use tilo_actions::{
    ActionParam, ActionSpec, Availability, EncodedAction, OperationKind, ParamType, Resource,
};
use zeroize::Zeroizing;

use super::lock_controller::LockController;
use crate::shell::test_control::{Command, Response};

const PROVIDER: &str = "session-lock";

fn resources(locked: bool, lock: &LockController) -> Vec<Resource> {
    let mut action = ActionSpec::typed_operation(
        "unlock",
        crate::shell::i18n::t("lock-screen-unlock"),
        "",
        OperationKind::Effect,
        "session-lock.console/v1",
        vec![ActionParam::required(
            "password",
            ParamType::Secret,
            "Private transient session-owner password",
        )],
        |_| EncodedAction {
            type_id: "session-lock.console/v1".into(),
            payload: b"unlock".to_vec(),
        },
    );
    if !locked || lock.is_verifying() {
        action.type_mut().contract.availability = Availability::Unavailable {
            code: if locked {
                "authentication_busy"
            } else {
                "session_unlocked"
            }
            .into(),
            reason: if locked {
                "Password verification is already running"
            } else {
                "The session is already unlocked"
            }
            .into(),
        };
    }
    vec![
        Resource::new(
            PROVIDER,
            "session-lock.unlock",
            crate::shell::i18n::t("lock-screen-unlock"),
        )
        .with_action(action),
    ]
}

/// Called only after the production admin socket's device-owner/peer checks.
/// Agent-scoped and enrolled-device requests never enter this local surface.
pub(super) fn dispatch(
    command: &mut Command,
    locked: bool,
    lock: &mut LockController,
) -> Option<Response> {
    match command {
        Command::ActionsEnumerate { query, .. } if locked => Some(Response::Answer(
            console_control::enumerate(PROVIDER, &resources(locked, lock), query),
        )),
        // Answered at once, so there is nothing for `wait_ms` to wait for.
        Command::ActionsInvoke {
            provider,
            id,
            args,
            device: None,
            wait_ms: _,
        } if provider == PROVIDER => {
            let admitted =
                console_control::admit(PROVIDER, &resources(locked, lock), provider, id, args);
            // Remove private input even on refusal. Neither the encoded action
            // nor the action reply carries it, and no history action is created.
            let password = args
                .remove("password")
                .and_then(|value| match value {
                    serde_json::Value::String(password) => Some(password),
                    _ => None,
                })
                .map(Zeroizing::new);
            Some(match admitted {
                Err(refusal) => Response::Err {
                    code: refusal.code.into(),
                    message: refusal.message,
                },
                Ok(_) => match password {
                    Some(password) => match lock.submit_secret(password) {
                        Ok(()) => Response::Answer(
                            json!({"status":"accepted","authentication":"verifying"}),
                        ),
                        Err(reason) => Response::Err {
                            code: "action_unavailable".into(),
                            message: reason.into(),
                        },
                    },
                    None => Response::Err {
                        code: "invalid_arguments".into(),
                        message: crate::shell::i18n::t("lock-screen-enter-password"),
                    },
                },
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use crate::compositor::lock_auth::GreetdVerifier;
    use crate::compositor::lock_auth::UnavailableVerifier;
    #[cfg(target_os = "linux")]
    use crate::compositor::lock_controller::UnlockOutcome;
    use std::sync::Arc;

    fn invoke(password: &str) -> Command {
        Command::ActionsInvoke {
            provider: PROVIDER.into(),
            id: "unlock@session-lock".into(),
            args: json!({"password":password}).as_object().unwrap().clone(),
            device: None,
            wait_ms: None,
        }
    }

    #[test]
    fn unlock_resource_publishes_secret_and_never_encodes_it() {
        let lock = LockController::new(Arc::new(UnavailableVerifier), "owner");
        let resource = resources(true, &lock);
        let catalogue = console_control::enumerate(PROVIDER, &resource, "unlock");
        assert_eq!(catalogue["actions"][0]["id"], "unlock@session-lock");
        assert_eq!(catalogue["actions"][0]["params"][0]["name"], "password");
        let Command::ActionsInvoke { args, .. } = invoke("private-test-password") else {
            panic!("invoke must construct a semantic invocation")
        };
        let admitted =
            console_control::admit(PROVIDER, &resource, PROVIDER, "unlock@session-lock", &args)
                .unwrap();
        assert_eq!(admitted.encoded.payload, b"unlock");
        assert!(!catalogue.to_string().contains("private-test-password"));
    }

    #[test]
    fn unlock_rejects_invalid_busy_and_already_unlocked_without_secret_echoes() {
        let mut lock = LockController::new(Arc::new(UnavailableVerifier), "owner");
        let mut invalid = invoke("private-test-password");
        if let Command::ActionsInvoke { args, .. } = &mut invalid {
            args.insert("username".into(), json!("root"));
        }
        assert!(
            matches!(dispatch(&mut invalid,true,&mut lock),Some(Response::Err{code,..}) if code=="invalid_arguments")
        );
        assert!(!lock.is_verifying());
        let mut command = invoke("private-test-password");
        let response = dispatch(&mut command, false, &mut lock).unwrap();
        assert!(matches!(response,Response::Err{ref code,..} if code=="action_unavailable"));
        assert!(
            !serde_json::to_string(&response)
                .unwrap()
                .contains("private-test-password")
        );
        assert!(!lock.is_verifying());
        let mut command = invoke("private-test-password");
        assert!(matches!(
            dispatch(&mut command, true, &mut lock),
            Some(Response::Answer(_))
        ));
        assert_eq!(lock.buffer_len(), 0);
        assert!(lock.is_verifying());
        let mut duplicate = invoke("private-test-password");
        assert!(
            matches!(dispatch(&mut duplicate,true,&mut lock),Some(Response::Err{code,..}) if code=="action_unavailable")
        );
    }

    /// Linux only: the greetd verifier exists only beside the Linux greeter.
    #[cfg(target_os = "linux")]
    #[test]
    fn unlock_action_drives_normal_greetd_verification_and_denial_keeps_lock() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixListener;
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("greetd.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let daemon = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let read = |stream: &mut std::os::unix::net::UnixStream| {
                let mut len = [0; 4];
                stream.read_exact(&mut len).unwrap();
                let mut bytes = vec![0; usize::try_from(u32::from_ne_bytes(len)).unwrap()];
                stream.read_exact(&mut bytes).unwrap();
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()
            };
            let send = |stream: &mut std::os::unix::net::UnixStream, value: serde_json::Value| {
                let bytes = serde_json::to_vec(&value).unwrap();
                stream
                    .write_all(&(u32::try_from(bytes.len()).unwrap()).to_ne_bytes())
                    .unwrap();
                stream.write_all(&bytes).unwrap();
            };
            assert_eq!(
                read(&mut stream),
                json!({"type":"create_session","username":"owner"})
            );
            send(
                &mut stream,
                json!({"type":"auth_message","auth_message_type":"secret","auth_message":"Password:"}),
            );
            assert_eq!(
                read(&mut stream),
                json!({"type":"post_auth_message_response","response":"private-test-password"})
            );
            send(
                &mut stream,
                json!({"type":"error","error_type":"auth_error","description":"AUTH_ERR"}),
            );
            assert_eq!(read(&mut stream), json!({"type":"cancel_session"}));
            send(&mut stream, json!({"type":"success"}));
        });
        let mut lock = LockController::new(
            Arc::new(GreetdVerifier::new(socket.to_str().unwrap())),
            "owner",
        );
        let mut command = invoke("private-test-password");
        let response = dispatch(&mut command, true, &mut lock).unwrap();
        assert!(matches!(response, Response::Answer(_)));
        assert!(
            !serde_json::to_string(&response)
                .unwrap()
                .contains("private-test-password")
        );
        daemon.join().unwrap();
        let result = tilo_test_env::wait_for("the verify to resolve", || lock.poll_verification());
        assert_eq!(result, UnlockOutcome::StillLocked);
        assert_eq!(lock.buffer_len(), 0);
        assert!(lock.error().is_some());
    }
}
