//! The greeter's console: the same `tilo-control` action protocol the
//! installer answers, so a caller that cannot see the screen can tell that
//! a first boot reached sign-in, which system image it booted, and what the
//! greeter publishes — without OCR, SSH or a debug service.
//!
//! Authentication uses the ordinary greetd/PAM worker. Secret arguments are
//! transient input only: neither published resource values nor encoded operations
//! contain credentials. Keyboard input readiness remains separately observable.

use automerge::AutoCommit;
use console_control::{Request, catalog, wire};
use serde_json::{Value, json};
use ui::tilo_actions::{
    ActionParam, ActionSpec, Availability, EncodedAction, OperationKind, ParamType, Resource,
};
use zeroize::{Zeroize, Zeroizing};

use super::{AuthPhase, GreeterApp};

/// `provider` in `tilo-control` terms.
pub const PROVIDER: &str = "greeter";

/// The external-event payload that tells the greeter console requests are
/// queued.
pub const CONSOLE_WAKE: &str = "\u{1}greeter-console";

impl GreeterApp {
    /// Attach the console request queue; `main` starts its endpoints.
    #[must_use]
    pub fn with_console(mut self, console: console_control::ConsoleServer) -> Self {
        self.console = Some(console);
        self
    }

    pub(super) fn serve_console(&mut self, state: &AutoCommit) {
        let Some(console) = self.console.clone() else {
            return;
        };
        for pending in console.take_pending() {
            let reply = self.answer_console(pending.line(), state);
            pending.respond(reply);
        }
    }

    /// One request line in, one reply line out.
    pub fn answer_console(&mut self, line: &str, state: &AutoCommit) -> String {
        match wire::parse(line) {
            Err(error) => wire::error("parse_error", &error.0),
            Ok(Request::Unsupported { op }) => wire::unsupported(&op),
            Ok(Request::StateDump) => wire::state(&self.console_state()),
            Ok(Request::Enumerate { query }) => wire::answer(&catalog::enumerate(
                PROVIDER,
                &ui::resources::resources(self, state),
                &query,
            )),
            Ok(Request::Invoke {
                provider,
                id,
                mut args,
            }) => {
                match console_control::admit(
                    PROVIDER,
                    &ui::resources::resources(self, state),
                    &provider,
                    &id,
                    &args,
                ) {
                    Err(refusal) => wire::error(refusal.code, &refusal.message),
                    Ok(admitted) => self.invoke_console(&admitted.address.action, &mut args),
                }
            }
        }
    }

    fn invoke_console(
        &mut self,
        action: &str,
        args: &mut serde_json::Map<String, Value>,
    ) -> String {
        match action {
            "sign-in" => {
                let Some(Value::String(secret)) = args.remove("password") else {
                    return wire::error("invalid_arguments", "A passcode is required");
                };
                let mut secret = Zeroizing::new(secret);
                if secret.is_empty() || secret.len() > 4096 {
                    return wire::error(
                        "invalid_arguments",
                        "The passcode is empty or exceeds its limit",
                    );
                }
                {
                    let mut stored = self
                        .password_buffer
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    stored.zeroize();
                    *stored = std::mem::take(&mut *secret);
                }
                self.attempt_login(super::AuthMethod::Password);
            }
            "fingerprint" => self.attempt_login(super::AuthMethod::Fingerprint),
            "choose-account" => {
                let Some(username) = args.get("username").and_then(Value::as_str) else {
                    return wire::error("invalid_arguments", "Choose an account on this computer");
                };
                let Some(index) = self.users.iter().position(|user| user.username == username)
                else {
                    return wire::error("invalid_arguments", "Choose an account on this computer");
                };
                self.choose_account(index);
            }
            "set-username" => {
                let Some(username) = args.get("username").and_then(Value::as_str) else {
                    return wire::error("invalid_arguments", "A username is required");
                };
                if username.trim().is_empty() || username.len() > 256 {
                    return wire::error(
                        "invalid_arguments",
                        "The username is empty or exceeds its limit",
                    );
                }
                self.username.set(username.trim().to_owned());
                self.clear_password();
                self.dirty_structure = true;
            }
            "choose-session" => {
                let Some(name) = args.get("session").and_then(Value::as_str) else {
                    return wire::error("invalid_arguments", "Choose an available session");
                };
                let Some(index) = self
                    .sessions
                    .iter()
                    .position(|session| session.name == name)
                else {
                    return wire::error("invalid_arguments", "Choose an available session");
                };
                self.selected_session = index;
                self.session_choice.set(name.to_owned());
                self.dirty_structure = true;
            }
            _ => return wire::error("unknown_action", "This action is not published"),
        }
        wire::answer(&json!({"status":"accepted","state":self.console_state()}))
    }

    /// What the greeter shows, as data.
    pub fn console_state(&self) -> Value {
        json!({
            "app": PROVIDER,
            "phase": match self.phase {
                AuthPhase::Idle => "sign-in",
                AuthPhase::Authenticating => "authenticating",
                AuthPhase::Starting => "starting-session",
            },
            "account": self.login_user.as_ref().map(|user| json!({
                "username": user.username,
                "display_name": user.display_name,
            })),
            "session": self.sessions.get(self.selected_session).map(|session| session.name.clone()),
            "sessions": self.sessions.iter().map(|session| session.name.clone()).collect::<Vec<_>>(),
            "session_choice": self.offers_session_choice(),
            "status": if self.phase == AuthPhase::Authenticating { super::t("greeter-signing-in") } else { self.status_text() },
            "username": self.attempt_username(),
            "accounts": self.users.iter().map(|user| json!({"username":user.username,"display_name":user.display_name})).collect::<Vec<_>>(),
            "notice": match self.notice {
                super::Notice::None => Value::Null,
                super::Notice::Hint(key) => json!({ "kind": "hint", "key": key }),
                super::Notice::Error(key) => json!({ "kind": "error", "key": key }),
            },
            "accepts_input": self.phase == AuthPhase::Idle && self.keyboard_focus,
            "image": { "composefs": console_control::booted_composefs() },
        })
    }
}

impl ui::resources::PublishesResources for GreeterApp {
    fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        let mut resource = Resource::new(PROVIDER, "greeter.sign-in", super::t("greeter-sign-in"));
        let mut specs = vec![
            console_action(
                "sign-in",
                "greeter-sign-in",
                vec![ActionParam::required(
                    "password",
                    ParamType::Secret,
                    "Passcode; transient authentication input only",
                )],
            ),
            console_action(
                "choose-session",
                "greeter-session",
                vec![
                    ActionParam::required("session", ParamType::Text, "Available session")
                        .with_choices(self.sessions.iter().map(|session| session.name.clone())),
                ],
            ),
        ];
        if self.users.len() > 1 {
            specs.push(console_action(
                "choose-account",
                "greeter-accounts",
                vec![
                    ActionParam::required("username", ParamType::Text, "Account on this computer")
                        .with_choices(self.users.iter().map(|user| user.username.clone())),
                ],
            ));
        }
        if self.login_user.is_none() {
            specs.push(console_action(
                "set-username",
                "greeter-username",
                vec![ActionParam::required(
                    "username",
                    ParamType::Text,
                    "Username to authenticate",
                )],
            ));
        }
        if self.fingerprint_available_for(&self.attempt_username()) {
            specs.push(console_action(
                "fingerprint",
                "greeter-use-fingerprint",
                Vec::new(),
            ));
        }
        for mut spec in specs {
            if self.phase != AuthPhase::Idle {
                spec.contract.availability = Availability::Unavailable {
                    code: "authentication_busy".into(),
                    reason: "An authentication attempt is already running".into(),
                };
            } else if spec.id == "sign-in"
                && (self.attempt_username().is_empty()
                    || self.sessions.get(self.selected_session).is_none())
            {
                spec.contract.availability = Availability::Unavailable {
                    code: "sign_in_not_ready".into(),
                    reason: "Choose an account and an available session first".into(),
                };
            }
            resource = resource.with_action(spec);
        }
        vec![resource]
    }
}

fn console_action(id: &'static str, label: &'static str, params: Vec<ActionParam>) -> ActionSpec {
    ActionSpec::typed_operation(
        id,
        super::t(label),
        "",
        OperationKind::Effect,
        "greeter.console/v1",
        params,
        move |_| EncodedAction {
            type_id: "greeter.console/v1".into(),
            // The credential is deliberately absent from the encoded operation.
            payload: json!({"action":id}).to_string().into_bytes(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::greeter_app::AuthScenario;

    fn ask(app: &mut GreeterApp, line: &str) -> Value {
        serde_json::from_str(&app.answer_console(line, &AutoCommit::new())).unwrap()
    }

    #[test]
    fn the_console_publishes_sign_in_without_synthetic_input() {
        let mut app = GreeterApp::for_shot(AuthScenario::PersonalSessions);
        let state = ask(&mut app, r#"{"op":"state_dump"}"#);
        assert_eq!(state["state"]["app"], "greeter");
        assert_eq!(state["state"]["phase"], "sign-in");
        assert_eq!(state["state"]["accepts_input"], false);
        let catalogue = ask(&mut app, r#"{"op":"actions_enumerate"}"#);
        assert!(
            catalogue["answer"]["actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|action| action["id"] == "sign-in@greeter")
        );
        let invoked = ask(
            &mut app,
            r#"{"op":"actions_invoke","provider":"greeter","id":"sign-in@greeter"}"#,
        );
        assert_eq!(invoked["err"]["code"], "invalid_arguments");
        let typed = ask(&mut app, r#"{"op":"type","text":"hunter2"}"#);
        assert_eq!(typed["err"]["code"], "not_implemented");
    }

    #[test]
    fn sign_in_accepts_input_only_while_the_window_has_the_keyboard() {
        let mut app = GreeterApp::for_shot(AuthScenario::PersonalSessions);
        app.keyboard_focus = true;
        assert_eq!(
            ask(&mut app, r#"{"op":"state_dump"}"#)["state"]["accepts_input"],
            true
        );
        app.phase = AuthPhase::Authenticating;
        assert_eq!(
            ask(&mut app, r#"{"op":"state_dump"}"#)["state"]["accepts_input"],
            false
        );
        app.phase = AuthPhase::Idle;
        app.keyboard_focus = false;
        assert_eq!(
            ask(&mut app, r#"{"op":"state_dump"}"#)["state"]["accepts_input"],
            false
        );
    }
}

#[cfg(test)]
mod authentication_tests {
    use super::*;
    use crate::greeter_app::AuthScenario;
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};

    const SECRET: &str = "console-test-secret-never-published";

    fn read_request(stream: &mut UnixStream) -> Value {
        let mut len = [0; 4];
        stream.read_exact(&mut len).unwrap();
        let len = u32::from_le_bytes(len) as usize;
        assert!(len < 8192);
        let mut bytes = vec![0; len];
        stream.read_exact(&mut bytes).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn respond(stream: &mut UnixStream, value: &Value) {
        let bytes = serde_json::to_vec(value).unwrap();
        stream
            .write_all(&u32::try_from(bytes.len()).unwrap().to_le_bytes())
            .unwrap();
        stream.write_all(&bytes).unwrap();
    }

    #[test]
    fn remote_sign_in_uses_the_real_greetd_exchange_and_does_not_disclose_secrets() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("greetd.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            let mut accepted = None;
            tilo_test_env::wait_until(
                "greeter authentication socket connection",
                || match listener.accept() {
                    Ok((stream, _)) => {
                        accepted = Some(stream);
                        true
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => false,
                    Err(error) => panic!("authentication fixture accept failed: {error}"),
                },
            );
            let mut stream = accepted.unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            assert_eq!(
                read_request(&mut stream),
                json!({"type":"create_session","username":"quinten"})
            );
            respond(
                &mut stream,
                &json!({"type":"auth_message","auth_message_type":"secret","auth_message":"Password:"}),
            );
            assert_eq!(
                read_request(&mut stream),
                json!({"type":"post_auth_message_response","response":SECRET})
            );
            // Deliberately hostile backend detail must never enter output/logs.
            respond(
                &mut stream,
                &json!({"type":"error","error_type":"auth_error","description":format!("pam_authenticate: {SECRET}: AUTH_ERR")}),
            );
            assert_eq!(read_request(&mut stream), json!({"type":"cancel_session"}));
            respond(&mut stream, &json!({"type":"success"}));
        });
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "greeter_app::console::authentication_tests::remote_sign_in_fixture_child",
                "--nocapture",
            ])
            .env("TILO_GREETER_CONSOLE_AUTH_FIXTURE", "1")
            .env("GREETD_SOCK", &socket)
            .output()
            .unwrap();
        server.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains(SECRET));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(SECRET));
    }

    #[test]
    fn remote_sign_in_fixture_child() {
        if std::env::var_os("TILO_GREETER_CONSOLE_AUTH_FIXTURE").is_none() {
            let app = GreeterApp::for_shot(AuthScenario::PersonalSessions);
            assert!(!ui::resources::resources(&app, &AutoCommit::new()).is_empty());
            return;
        }
        let _logs = tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .try_init();
        let mut app = GreeterApp::for_shot(AuthScenario::PersonalSessions);
        assert!(!app.keyboard_focus);
        let mut doc = AutoCommit::new();
        let resources = ui::resources::resources(&app, &doc);
        let args = json!({"password":SECRET}).as_object().unwrap().clone();
        let admitted =
            catalog::admit(PROVIDER, &resources, PROVIDER, "sign-in@greeter", &args).unwrap();
        assert!(!String::from_utf8_lossy(&admitted.encoded.payload).contains(SECRET));
        let reply = app.answer_console(
            &json!({"op":"actions_invoke","provider":PROVIDER,"id":"sign-in@greeter","args":args})
                .to_string(),
            &doc,
        );
        assert_eq!(app.phase, AuthPhase::Idle);
        assert_eq!(app.failures, 1);
        assert!(app.password_buffer.lock().unwrap().is_empty());
        assert!(!reply.contains(SECRET));
        assert!(!app.console_state().to_string().contains(SECRET));
        assert!(!String::from_utf8_lossy(&doc.save()).contains(SECRET));
        assert!(matches!(app.notice, super::super::Notice::Error(_)));
    }

    #[test]
    fn remote_sign_in_binds_secret_and_preserves_account_session_authority() {
        let mut app = GreeterApp::for_shot(AuthScenario::PersonalSessions);
        let doc = AutoCommit::new();
        let request = |id: &str, args: Value| {
            json!({"op":"actions_invoke","provider":PROVIDER,"id":id,"args":args}).to_string()
        };
        assert!(
            app.answer_console(
                &request("choose-session@greeter", json!({"session":"GNOME"})),
                &doc
            )
            .contains("accepted")
        );
        assert_eq!(
            app.sessions[app.selected_session].argv,
            vec!["/usr/bin/gnome-session"]
        );
        let refused: Value = serde_json::from_str(&app.answer_console(
            &request("choose-session@greeter", json!({"session":"/bin/attacker"})),
            &doc,
        ))
        .unwrap();
        assert_eq!(refused["err"]["code"], "invalid_arguments");
        let refused: Value = serde_json::from_str(&app.answer_console(
            &request(
                "sign-in@greeter",
                json!({"password":SECRET,"username":"root"}),
            ),
            &doc,
        ))
        .unwrap();
        assert_eq!(refused["err"]["code"], "invalid_arguments");
        app.phase = AuthPhase::Authenticating;
        let refused: Value = serde_json::from_str(&app.answer_console(
            &request("sign-in@greeter", json!({"password":SECRET})),
            &doc,
        ))
        .unwrap();
        assert_eq!(refused["err"]["code"], "action_unavailable");
        assert!(!refused.to_string().contains(SECRET));
        assert!(app.password_buffer.lock().unwrap().is_empty());
    }
}
