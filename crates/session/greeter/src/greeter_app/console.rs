//! The greeter's console: the same `tilo-control` action protocol the
//! installer answers, so a caller that cannot see the screen can tell that
//! a first boot reached sign-in, which system image it booted, and what the
//! greeter publishes — without OCR, SSH or a debug service.
//!
//! Authentication uses the ordinary greetd/PAM worker. Secret arguments are
//! transient input only: published resource values never contain credentials,
//! and a secret travels only inside the in-process encoded operation that
//! carries it to [`GreeterApp::apply_operation`], like every typed effect.
//! Keyboard input readiness remains separately observable.
//!
//! Every door ends in that one function: this console, the toolkit host's
//! action surface (`ResourceOperations::start_operation`) and the keyboard/pointer
//! handlers, which call the same `attempt_login` and choosers.

use automerge::AutoCommit;
use console_control::{Request, catalog, wire};
use serde_json::{Value, json};
use ui::tilo_actions::{
    ActionArgs, ActionParam, ActionSpec, Availability, EncodedAction, OperationKind, ParamType,
    Resource,
};
use zeroize::{Zeroize, Zeroizing};

use super::{AuthPhase, GreeterApp};
use tilo_lock::MutexRecover as _;
use ui::a11y::AccessibilityRequest;

/// `provider` in `tilo-control` terms.
pub const PROVIDER: &str = "greeter";

/// The operation type every greeter action travels under. The greeter
/// declares it as a locally handled type, so the host publishes its actions.
pub const OPERATION: &str = "greeter.console/v1";

/// Why an operation was refused: a stable code and a sentence.
pub(super) type Refused = (&'static str, String);

fn refused(code: &'static str, message: impl Into<String>) -> Refused {
    (code, message.into())
}

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
            Ok(Request::Invoke { provider, id, args }) => {
                let admitted = console_control::admit(
                    PROVIDER,
                    &ui::resources::resources(self, state),
                    &provider,
                    &id,
                    &args,
                );
                match admitted {
                    Err(refusal) => wire::error(refusal.code, &refusal.message),
                    Ok(mut admitted) => {
                        let reply = match self.apply_operation(&admitted.encoded) {
                            Ok(answer) => wire::answer(&answer),
                            Err((code, message)) => wire::error(code, &message),
                        };
                        admitted.encoded.payload.zeroize();
                        reply
                    }
                }
            }
        }
    }

    /// Apply one encoded greeter operation. The console and the toolkit
    /// host's action surface both end here, after their own resolution,
    /// availability and argument binding; the handlers still re-check the
    /// greeter's preconditions.
    ///
    /// # Errors
    ///
    /// Returns a stable code and the reason when the operation is malformed
    /// or cannot run now.
    pub(super) fn apply_operation(&mut self, encoded: &EncodedAction) -> Result<Value, Refused> {
        if encoded.type_id != OPERATION {
            return Err(refused(
                "invalid_arguments",
                format!("not a greeter operation: {}", encoded.type_id),
            ));
        }
        let Ok(Value::Object(mut invocation)) = serde_json::from_slice::<Value>(&encoded.payload)
        else {
            return Err(refused("invalid_arguments", "Malformed greeter operation"));
        };
        let Some(Value::String(action)) = invocation.remove("action") else {
            return Err(refused("invalid_arguments", "Malformed greeter operation"));
        };
        let mut args = match invocation.remove("args") {
            Some(Value::Object(args)) => args,
            None => serde_json::Map::new(),
            Some(_) => return Err(refused("invalid_arguments", "Malformed greeter operation")),
        };
        self.run_action(&action, &mut args)
    }

    fn run_action(
        &mut self,
        action: &str,
        args: &mut serde_json::Map<String, Value>,
    ) -> Result<Value, Refused> {
        match action {
            "configure-session-diagnostics" => {
                let mut flags = [false; 4];
                for (flag, (param, _)) in flags.iter_mut().zip(&super::SESSION_DIAGNOSTICS) {
                    let Some(enabled) = args.get(*param).and_then(Value::as_bool) else {
                        return Err(refused(
                            "invalid_arguments",
                            "Diagnostic options must be booleans",
                        ));
                    };
                    *flag = enabled;
                }
                if flags[3] && !flags[2] {
                    return Err(refused(
                        "invalid_arguments",
                        "GPU dispatch timing requires GPU timestamps",
                    ));
                }
                self.next_sign_in_diagnostics = Some(flags);
            }

            "respond-challenge" => return self.respond_console_challenge(args),
            "cancel-authentication" => return self.cancel_console_authentication(),
            "sign-in" => {
                let Some(Value::String(secret)) = args.remove("password") else {
                    return Err(refused("invalid_arguments", "A passcode is required"));
                };
                let mut secret = Zeroizing::new(secret);
                if secret.is_empty() || secret.len() > 4096 {
                    return Err(refused(
                        "invalid_arguments",
                        "The passcode is empty or exceeds its limit",
                    ));
                }
                {
                    let mut stored = self.password_buffer.lock_recover();
                    stored.zeroize();
                    *stored = std::mem::take(&mut *secret);
                }
                self.attempt_login(super::AuthMethod::Password);
            }
            "fingerprint" => self.attempt_login(super::AuthMethod::Fingerprint),
            "choose-account" => {
                let Some(username) = args.get("username").and_then(Value::as_str) else {
                    return Err(refused(
                        "invalid_arguments",
                        "Choose an account on this computer",
                    ));
                };
                let Some(index) = self.users.iter().position(|user| user.username == username)
                else {
                    return Err(refused(
                        "invalid_arguments",
                        "Choose an account on this computer",
                    ));
                };
                self.choose_account(index);
            }
            "set-username" => {
                let Some(username) = args.get("username").and_then(Value::as_str) else {
                    return Err(refused("invalid_arguments", "A username is required"));
                };
                if username.trim().is_empty() || username.len() > 256 {
                    return Err(refused(
                        "invalid_arguments",
                        "The username is empty or exceeds its limit",
                    ));
                }
                self.username.set(username.trim().to_owned());
                self.clear_password();
                self.dirty_structure = true;
            }
            "choose-session" => {
                let Some(name) = args.get("session").and_then(Value::as_str) else {
                    return Err(refused("invalid_arguments", "Choose an available session"));
                };
                let Some(index) = self
                    .sessions
                    .iter()
                    .position(|session| session.name == name)
                else {
                    return Err(refused("invalid_arguments", "Choose an available session"));
                };
                self.selected_session = index;
                self.session_choice.set(name.to_owned());
                self.dirty_structure = true;
            }
            _ => return self.run_screen_action(action, args),
        }
        Ok(self.accepted())
    }

    /// The footer's power menu and accessibility toggles.
    fn run_screen_action(
        &mut self,
        action: &str,
        args: &serde_json::Map<String, Value>,
    ) -> Result<Value, Refused> {
        match action {
            "suspend" => self.request_power(super::PowerAction::Suspend),
            "reboot" => self.request_power(super::PowerAction::Reboot),
            "power-off" => self.request_power(super::PowerAction::PowerOff),
            "larger-text" | "high-contrast" | "screen-reader" => {
                let Some(enabled) = args.get("enabled").and_then(Value::as_bool) else {
                    return Err(refused("invalid_arguments", "Choose on or off"));
                };
                // The same one-shot request the footer's toggle raises; the
                // host applies it on the next frame.
                self.requests.accessibility.set(Some(match action {
                    "larger-text" => AccessibilityRequest::TextScale(if enabled {
                        super::LARGER_TEXT_SCALE
                    } else {
                        1.0
                    }),
                    "high-contrast" => AccessibilityRequest::HighContrast(enabled),
                    _ => AccessibilityRequest::ScreenReader(enabled),
                }));
            }
            _ => return Err(refused("unknown_action", "This action is not published")),
        }
        Ok(self.accepted())
    }

    fn respond_console_challenge(
        &mut self,
        args: &mut serde_json::Map<String, Value>,
    ) -> Result<Value, Refused> {
        let Some(id) = args
            .get("challenge")
            .and_then(Value::as_str)
            .and_then(|id| id.parse::<u64>().ok())
        else {
            return Err(refused(
                "invalid_arguments",
                "Choose the current authentication challenge",
            ));
        };
        let Some(Value::String(response)) = args.remove("response") else {
            return Err(refused(
                "invalid_arguments",
                "An authentication response is required",
            ));
        };
        let response = Zeroizing::new(response);
        if response.len() > 4096 {
            return Err(refused(
                "invalid_arguments",
                "The authentication response exceeds its limit",
            ));
        }
        let Some(session) = &self.auth_session else {
            return Err(refused(
                "action_unavailable",
                "There is no authentication challenge to answer",
            ));
        };
        if let Err(reason) = session.respond(id, response) {
            return Err(refused("action_unavailable", reason));
        }
        self.clear_password();
        self.dirty_structure = true;
        Ok(self.accepted())
    }

    fn cancel_console_authentication(&mut self) -> Result<Value, Refused> {
        let Some(session) = &self.auth_session else {
            return Err(refused(
                "action_unavailable",
                "There is no authentication attempt to cancel",
            ));
        };
        if let Err(reason) = session.cancel() {
            return Err(refused("action_unavailable", reason));
        }
        self.clear_password();
        self.dirty_structure = true;
        Ok(self.accepted())
    }

    fn accepted(&self) -> Value {
        json!({"status":"accepted","state":self.console_state()})
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
            "session_diagnostics": self.next_sign_in_diagnostics.map(|flags| {
                super::SESSION_DIAGNOSTICS.iter().zip(flags).map(|((param, _), enabled)| {
                    ((*param).to_owned(), json!(enabled))
                }).collect::<serde_json::Map<String, Value>>()
            }),
            "status": if self.phase == AuthPhase::Authenticating { super::t("greeter-signing-in") } else { self.status_text() },
            "username": self.attempt_username(),
            "challenge": self.auth_session.as_ref().and_then(crate::auth::AuthSession::question).map(|(challenge, question)| json!({
                "id": challenge.id.to_string(),
                "response_kind": if challenge.secret { "secret" } else { "visible" },
                "question": question,
            })),
            "cancellation_requested": self.auth_session.as_ref().is_some_and(crate::auth::AuthSession::is_cancelling),
            "accounts": self.users.iter().map(|user| json!({"username":user.username,"display_name":user.display_name})).collect::<Vec<_>>(),
            "notice": match self.notice {
                super::Notice::None => Value::Null,
                super::Notice::Hint(key) => json!({ "kind": "hint", "key": key }),
                super::Notice::Error(key) => json!({ "kind": "error", "key": key }),
            },
            "accepts_input": self.phase == AuthPhase::Idle && self.keyboard_focus,
            "screen_reader": self.screen_reader,
            "image": { "composefs": console_control::booted_composefs() },
        })
    }
}

fn conversation_actions(challenge: Option<crate::auth::Challenge>) -> Vec<ActionSpec> {
    let mut specs = Vec::new();
    let mut response_param = ActionParam::required(
        "challenge",
        ParamType::Text,
        "Current authentication challenge identity",
    );
    if let Some(challenge) = challenge {
        response_param = response_param.with_choices([challenge.id.to_string()]);
    }
    specs.push(console_action(
        "respond-challenge",
        "greeter-auth-respond",
        vec![
            response_param,
            ActionParam::required(
                "response",
                ParamType::Secret,
                "Private transient authentication response",
            ),
        ],
    ));
    specs.push(console_action(
        "cancel-authentication",
        "greeter-auth-cancel",
        Vec::new(),
    ));
    specs
}

fn diagnostics_action() -> ActionSpec {
    console_action(
        "configure-session-diagnostics",
        "greeter-session-diagnostics",
        super::SESSION_DIAGNOSTICS
            .iter()
            .map(|(param, _)| {
                ActionParam::required(
                    param,
                    ParamType::Boolean,
                    "Content-free timing for this sign-in",
                )
            })
            .collect(),
    )
}

impl ui::resources::PublishesResources for GreeterApp {
    fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        let mut resource = Resource::new(PROVIDER, "greeter.sign-in", super::t("greeter-sign-in"));
        let mut specs = vec![
            diagnostics_action(),
            console_action(
                "sign-in",
                "greeter-sign-in",
                vec![ActionParam::required(
                    "password",
                    ParamType::Secret,
                    "Password; transient authentication input only",
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
        let challenge = self
            .auth_session
            .as_ref()
            .and_then(crate::auth::AuthSession::challenge);
        specs.extend(conversation_actions(challenge));
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
            if spec.id == "respond-challenge" {
                if self.phase != AuthPhase::Authenticating || challenge.is_none() {
                    spec.contract.availability = Availability::Unavailable {
                        code: "no_authentication_challenge".into(),
                        reason: "No authentication challenge is waiting for a response".into(),
                    };
                }
            } else if spec.id == "cancel-authentication" {
                if self.phase != AuthPhase::Authenticating
                    || self
                        .auth_session
                        .as_ref()
                        .is_none_or(|session| !session.can_cancel())
                {
                    spec.contract.availability = Availability::Unavailable {
                        code: "no_cancellable_authentication".into(),
                        reason: "No authentication attempt can be cancelled now".into(),
                    };
                }
            } else if self.phase != AuthPhase::Idle {
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
        // The footer's controls stay usable while an attempt runs, so their
        // actions are never gated on the authentication phase.
        for spec in screen_actions() {
            resource = resource.with_action(spec);
        }
        vec![resource]
    }
}

/// The footer's power menu and accessibility toggles: what a person can do
/// from the sign-in screen before anyone is signed in.
fn screen_actions() -> Vec<ActionSpec> {
    let mut specs: Vec<ActionSpec> = [
        ("suspend", "greeter-suspend"),
        ("reboot", "greeter-restart"),
        ("power-off", "greeter-shut-down"),
    ]
    .into_iter()
    .map(|(id, label)| console_action(id, label, Vec::new()))
    .collect();
    specs.extend(
        [
            ("larger-text", "greeter-larger-text"),
            ("high-contrast", "greeter-high-contrast"),
            ("screen-reader", "greeter-screen-reader"),
        ]
        .into_iter()
        .map(|(id, label)| {
            console_action(
                id,
                label,
                vec![ActionParam::required(
                    "enabled",
                    ParamType::Boolean,
                    "Turn this on or off",
                )],
            )
        }),
    );
    specs
}

fn console_action(id: &'static str, label: &'static str, params: Vec<ActionParam>) -> ActionSpec {
    ActionSpec::typed_operation(
        id,
        super::t(label),
        "",
        OperationKind::Effect,
        OPERATION,
        params,
        move |args| encode(id, args),
    )
}

/// The operation `action` with the arguments the door bound strictly against
/// its parameters. A secret argument rides along to the in-process handler
/// only; it is never published or logged (the host redacts `Secret` params).
fn encode(action: &str, args: &ActionArgs) -> EncodedAction {
    EncodedAction {
        type_id: OPERATION.into(),
        payload: serde_json::to_vec(&json!({"action": action, "args": args.to_json()}))
            .unwrap_or_default(),
    }
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
    fn the_host_publishes_every_greeter_action_it_can_run() {
        // The host refuses the whole surface when any effect's operation type
        // has no handler in the retained tree; validate against the handlers
        // the greeter actually declares, in every phase that changes it.
        let mut app = GreeterApp::for_shot(AuthScenario::PersonalSessions);
        for phase in [AuthPhase::Idle, AuthPhase::Authenticating] {
            app.phase = phase;
            let resources = ui::resources::resources(&app, &AutoCommit::new());
            let violations = ui::tilo_actions::validate_surface(
                &resources,
                &ui::tilo_actions::ActionRegistry::new(),
                &ui::widget::resource_operation_handlers(&app),
            );
            assert!(violations.is_empty(), "{phase:?}: {violations:?}");
            for action in [
                "sign-in",
                "respond-challenge",
                "choose-session",
                "cancel-authentication",
                "configure-session-diagnostics",
            ] {
                assert!(
                    ui::tilo_actions::door::resolve(
                        &resources,
                        &ui::tilo_actions::ActionAddress::new(PROVIDER, action),
                    )
                    .is_some(),
                    "{action} is not published"
                );
            }
        }

        app.phase = AuthPhase::Idle;
        let resources = ui::resources::resources(&app, &AutoCommit::new());
        let encoded = ui::tilo_actions::door::resolve(
            &resources,
            &ui::tilo_actions::ActionAddress::new(PROVIDER, "choose-session"),
        )
        .unwrap()
        .build_json(json!({"session":"GNOME"}).as_object().unwrap())
        .unwrap();
        let mut doc = AutoCommit::new();
        let started = ui::host_operation::start_operation(
            &mut app,
            &ui::host_operation::ResourceOperation::local(
                ui::tilo_actions::InvocationId::new(),
                OperationKind::Effect,
                &encoded.type_id,
                &encoded.payload,
            ),
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        )
        .unwrap();
        assert!(started.is_some());
        assert_eq!(app.sessions[app.selected_session].name, "GNOME");
    }

    #[test]
    fn power_and_accessibility_are_actions_even_while_signing_in() {
        // The footer's power menu and accessibility toggles stay usable while
        // an attempt runs; an agent or a screen-reader user reaches the same
        // controls without a pointer.
        let mut app = GreeterApp::for_shot(AuthScenario::PersonalSessions);
        app.phase = AuthPhase::Authenticating;
        let catalogue = ask(&mut app, r#"{"op":"actions_enumerate"}"#);
        let actions = catalogue["answer"]["actions"].as_array().unwrap();
        for id in [
            "suspend@greeter",
            "reboot@greeter",
            "power-off@greeter",
            "larger-text@greeter",
            "high-contrast@greeter",
            "screen-reader@greeter",
        ] {
            let action = actions
                .iter()
                .find(|action| action["id"] == id)
                .unwrap_or_else(|| panic!("{id} is not published"));
            assert_eq!(action["available"], true, "{id}");
        }

        for (id, expected) in [
            ("suspend@greeter", super::super::PowerAction::Suspend),
            ("reboot@greeter", super::super::PowerAction::Reboot),
            ("power-off@greeter", super::super::PowerAction::PowerOff),
        ] {
            app.last_power_action = None;
            let reply = ask(
                &mut app,
                &json!({"op":"actions_invoke","provider":PROVIDER,"id":id}).to_string(),
            );
            assert_eq!(reply["answer"]["status"], "accepted", "{id}: {reply}");
            assert_eq!(app.last_power_action, Some(expected), "{id}");
        }

        for (id, enabled, expected) in [
            (
                "high-contrast@greeter",
                true,
                AccessibilityRequest::HighContrast(true),
            ),
            (
                "larger-text@greeter",
                true,
                AccessibilityRequest::TextScale(super::super::LARGER_TEXT_SCALE),
            ),
            (
                "larger-text@greeter",
                false,
                AccessibilityRequest::TextScale(1.0),
            ),
            (
                "screen-reader@greeter",
                true,
                AccessibilityRequest::ScreenReader(true),
            ),
        ] {
            let reply = ask(
                &mut app,
                &json!({"op":"actions_invoke","provider":PROVIDER,"id":id,"args":{"enabled":enabled}})
                    .to_string(),
            );
            assert_eq!(reply["answer"]["status"], "accepted", "{id}: {reply}");
            assert_eq!(
                ui::widget::Widget::take_accessibility_request(&mut app),
                Some(expected),
                "{id}"
            );
        }
        assert_eq!(app.console_state()["screen_reader"], true);
        let refused = ask(
            &mut app,
            r#"{"op":"actions_invoke","provider":"greeter","id":"high-contrast@greeter"}"#,
        );
        assert_eq!(refused["err"]["code"], "invalid_arguments");
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

    fn start_worker(
        app: &mut GreeterApp,
        socket: &std::path::Path,
    ) -> std::sync::Arc<std::sync::Mutex<Vec<crate::auth::AuthUpdate>>> {
        let updates = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = std::sync::Arc::clone(&updates);
        app.begin_authenticating();
        app.auth_session = Some(crate::auth::spawn_auth_updates(
            socket.to_str().unwrap().to_owned(),
            app.attempt_username(),
            Zeroizing::new(SECRET.to_owned()),
            app.sessions.get(app.selected_session).unwrap().argv.clone(),
            GreeterApp::session_env(app.next_sign_in_diagnostics),
            std::sync::Arc::clone(&app.challenge_sequence),
            move |update| observed.lock().unwrap().push(update),
        ));
        updates
    }

    fn password_exchange(stream: &mut UnixStream) {
        stream
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        assert_eq!(
            read_request(stream),
            json!({"type":"create_session","username":"quinten"})
        );
        respond(
            stream,
            &json!({"type":"auth_message","auth_message_type":"secret","auth_message":"Password:"}),
        );
        assert_eq!(
            read_request(stream),
            json!({"type":"post_auth_message_response","response":SECRET})
        );
    }

    fn await_challenge(app: &GreeterApp) -> crate::auth::Challenge {
        tilo_test_env::wait_until("actual greetd challenge", || {
            app.auth_session
                .as_ref()
                .and_then(crate::auth::AuthSession::challenge)
                .is_some()
        });
        app.auth_session.as_ref().unwrap().challenge().unwrap()
    }

    fn apply_worker_updates(
        app: &mut GreeterApp,
        updates: &std::sync::Arc<std::sync::Mutex<Vec<crate::auth::AuthUpdate>>>,
        doc: &mut AutoCommit,
    ) {
        tilo_test_env::wait_until("actual authentication worker completion", || {
            updates
                .lock()
                .unwrap()
                .iter()
                .any(|update| matches!(update, crate::auth::AuthUpdate::Failed(_)))
        });
        let events = std::mem::take(&mut *updates.lock().unwrap());
        for event in events {
            assert!(!event.encode().contains(SECRET));
            ui::widget::Widget::on_external_event(
                app,
                &event.encode(),
                &mut ui::widget::Edits::lend(doc),
                &automerge::ROOT,
            );
        }
    }

    fn invoke(app: &mut GreeterApp, id: &str, args: &Value) -> Value {
        let line =
            json!({"op":"actions_invoke","provider":PROVIDER,"id":id,"args":args}).to_string();
        serde_json::from_str(&app.answer_console(&line, &AutoCommit::new())).unwrap()
    }

    #[test]
    fn declared_session_diagnostics_are_bounded_transient_and_secret_free() {
        let mut app = GreeterApp::for_shot(AuthScenario::PersonalSessions);
        assert!(app.next_sign_in_diagnostics.is_none());
        *app.password_buffer.lock().unwrap() = SECRET.into();
        let options = json!({"presentation":true,"gpu_stages":true,"gpu_timestamps":true,"gpu_dispatch_timestamps":false});
        let reply = invoke(&mut app, "configure-session-diagnostics@greeter", &options);
        assert_eq!(reply["answer"]["status"], "accepted");
        assert_eq!(app.console_state()["session_diagnostics"], options);
        assert!(!reply.to_string().contains(SECRET));
        assert!(!app.console_state().to_string().contains(SECRET));
        let env = GreeterApp::session_env(app.next_sign_in_diagnostics);
        assert_eq!(
            &env[3..],
            [
                "TILO_PROFILE_PRESENTATION=1",
                "TILO_PROFILE_GPU_STAGES=1",
                "VELLO_GPU_TIMESTAMPS=1"
            ]
        );
        let refused = invoke(
            &mut app,
            "configure-session-diagnostics@greeter",
            &json!({"presentation":true,"gpu_stages":true,"gpu_timestamps":false,"gpu_dispatch_timestamps":true}),
        );
        assert_eq!(refused["err"]["code"], "invalid_arguments");
        assert_eq!(app.console_state()["session_diagnostics"], options);
        let refused = invoke(
            &mut app,
            "configure-session-diagnostics@greeter",
            &json!({"presentation":true,"gpu_stages":true,"gpu_timestamps":true,"gpu_dispatch_timestamps":false,"environment":{"TILO_NATIVE_SCENARIO":SECRET}}),
        );
        assert_eq!(refused["err"]["code"], "invalid_arguments");
        assert!(!refused.to_string().contains(SECRET));
        let disabled = json!({"presentation":false,"gpu_stages":false,"gpu_timestamps":false,"gpu_dispatch_timestamps":false});
        let reply = invoke(&mut app, "configure-session-diagnostics@greeter", &disabled);
        assert_eq!(reply["answer"]["status"], "accepted");
        assert_eq!(
            GreeterApp::session_env(app.next_sign_in_diagnostics).len(),
            3
        );
        assert_eq!(app.console_state()["session_diagnostics"], disabled);
        app.phase = AuthPhase::Authenticating;
        let refused = invoke(&mut app, "configure-session-diagnostics@greeter", &options);
        assert_eq!(refused["err"]["code"], "action_unavailable");
        assert_eq!(app.console_state()["session_diagnostics"], disabled);
        assert!(
            GreeterApp::for_shot(AuthScenario::PersonalSessions)
                .next_sign_in_diagnostics
                .is_none()
        );
    }

    #[test]
    fn session_diagnostics_reach_the_ordinary_authenticated_greetd_start() {
        // An accepted session start exits the greeter process, as greetd
        // requires. Run that in a child: in this test binary the exit ended
        // every test still running or not yet started, and reported success.
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("greetd.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let daemon = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            password_exchange(&mut stream);
            respond(&mut stream, &json!({"type":"success"}));
            let request = read_request(&mut stream);
            assert_eq!(
                request,
                json!({
                    "type":"start_session", "cmd":["/usr/libexec/tilo-session"],
                    "env":["XDG_SESSION_TYPE=wayland","XDG_SESSION_DESKTOP=tilo","XDG_CURRENT_DESKTOP=tilo",
                        "TILO_PROFILE_PRESENTATION=1","TILO_PROFILE_GPU_STAGES=1","VELLO_GPU_TIMESTAMPS=1"]
                })
            );
            assert!(!request.to_string().contains(SECRET));
            respond(&mut stream, &json!({"type":"success"}));
        });
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "greeter_app::console::authentication_tests::session_diagnostics_start_child",
                "--nocapture",
            ])
            .env("TILO_GREETER_DIAGNOSTICS_START_FIXTURE", &socket)
            .output()
            .unwrap();
        daemon.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains(SECRET));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(SECRET));
    }

    #[test]
    fn session_diagnostics_start_child() {
        let Some(socket) = std::env::var_os("TILO_GREETER_DIAGNOSTICS_START_FIXTURE") else {
            return;
        };
        let mut app = GreeterApp::for_shot(AuthScenario::PersonalSessions);
        let configured = invoke(
            &mut app,
            "configure-session-diagnostics@greeter",
            &json!({
                "presentation":true,"gpu_stages":true,"gpu_timestamps":true,"gpu_dispatch_timestamps":false
            }),
        );
        assert_eq!(configured["answer"]["status"], "accepted");
        assert_eq!(app.selected_session, 0);
        assert_eq!(app.attempt_username(), "quinten");
        assert!(!app.console_state().to_string().contains(SECRET));
        let _updates = start_worker(&mut app, std::path::Path::new(&socket));
        // The worker exits this process once greetd starts the session; the
        // parent checks what greetd received.
        tilo_test_env::wait_until("the accepted session start exits the greeter", || false);
    }

    #[test]
    fn remote_challenges_use_actual_pam_prompts_and_reject_stale_response_owners() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("greetd.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let daemon = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            password_exchange(&mut stream);
            for (kind, expected) in [("secret", "one-time-code"), ("visible", "private-realm")] {
                respond(
                    &mut stream,
                    &json!({"type":"auth_message","auth_message_type":kind,"auth_message":if kind == "visible" { format!("Choose realm after {SECRET} one-time-code") } else { format!("One-time verification code after {SECRET}") }}),
                );
                assert_eq!(
                    read_request(&mut stream),
                    json!({"type":"post_auth_message_response","response":expected})
                );
            }
            respond(
                &mut stream,
                &json!({"type":"error","error_type":"auth_error","description":format!("pam_authenticate: {SECRET}: AUTH_ERR")}),
            );
            assert_eq!(read_request(&mut stream), json!({"type":"cancel_session"}));
            respond(&mut stream, &json!({"type":"success"}));
        });
        let mut app = GreeterApp::for_shot(AuthScenario::PersonalSessions);
        let updates = start_worker(&mut app, &socket);
        let first = await_challenge(&app);
        assert!(first.secret);
        let state = app.console_state();
        assert_eq!(state["challenge"]["id"], first.id.to_string());
        assert!(!state.to_string().contains(SECRET));
        assert!(
            state["challenge"]["question"]
                .as_str()
                .unwrap()
                .contains("One-time verification code")
        );
        let answer = invoke(
            &mut app,
            "respond-challenge@greeter",
            &json!({"challenge":first.id.to_string(),"response":"one-time-code"}),
        );
        assert_eq!(answer["answer"]["status"], "accepted");
        let second = await_challenge(&app);
        assert_ne!(first.id, second.id);
        assert!(!second.secret);
        let state = app.console_state();
        assert!(
            state["challenge"]["question"]
                .as_str()
                .unwrap()
                .contains("Choose realm")
        );
        assert!(!state.to_string().contains(SECRET));
        assert!(!state.to_string().contains("one-time-code"));
        let refusal = invoke(
            &mut app,
            "respond-challenge@greeter",
            &json!({"challenge":first.id.to_string(),"response":SECRET}),
        );
        assert_eq!(refusal["err"]["code"], "invalid_arguments");
        assert!(!refusal.to_string().contains(SECRET));
        let answer = invoke(
            &mut app,
            "respond-challenge@greeter",
            &json!({"challenge":second.id.to_string(),"response":"private-realm"}),
        );
        assert_eq!(answer["answer"]["status"], "accepted");
        let mut doc = AutoCommit::new();
        apply_worker_updates(&mut app, &updates, &mut doc);
        assert_eq!(app.phase, AuthPhase::Idle);
        assert_eq!(app.failures, 1);
        assert!(app.password_buffer.lock().unwrap().is_empty());
        assert!(!app.console_state().to_string().contains(SECRET));
        assert!(
            !doc.save()
                .windows(SECRET.len())
                .any(|bytes| bytes == SECRET.as_bytes())
        );
        assert_eq!(
            app.challenge_sequence
                .load(std::sync::atomic::Ordering::Relaxed),
            second.id + 1
        );
        daemon.join().unwrap();
    }

    #[test]
    fn remote_cancellation_uses_greetd_and_waits_for_worker_completion() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("greetd.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (ack_tx, ack_rx) = std::sync::mpsc::channel();
        let daemon = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            password_exchange(&mut stream);
            respond(
                &mut stream,
                &json!({"type":"auth_message","auth_message_type":"secret","auth_message":"Second factor:"}),
            );
            assert_eq!(read_request(&mut stream), json!({"type":"cancel_session"}));
            ack_rx.recv().unwrap();
            respond(&mut stream, &json!({"type":"success"}));
            let mut byte = [0];
            assert_eq!(
                stream.read(&mut byte).unwrap(),
                0,
                "cancellation must not start a session"
            );
        });
        let mut app = GreeterApp::for_shot(AuthScenario::PersonalSessions);
        let updates = start_worker(&mut app, &socket);
        let challenge = await_challenge(&app);
        let answer = invoke(&mut app, "cancel-authentication@greeter", &json!({}));
        assert_eq!(answer["answer"]["status"], "accepted");
        assert_eq!(
            app.phase,
            AuthPhase::Authenticating,
            "greetd has not yet acknowledged cancellation"
        );
        assert_eq!(app.console_state()["cancellation_requested"], true);
        assert!(app.console_state()["challenge"].is_null());
        let refusal = invoke(
            &mut app,
            "respond-challenge@greeter",
            &json!({"challenge":challenge.id.to_string(),"response":SECRET}),
        );
        assert_eq!(refusal["err"]["code"], "action_unavailable");
        assert!(!refusal.to_string().contains(SECRET));
        ack_tx.send(()).unwrap();
        let mut doc = AutoCommit::new();
        apply_worker_updates(&mut app, &updates, &mut doc);
        assert_eq!(app.phase, AuthPhase::Idle);
        assert_eq!(app.failures, 0);
        assert!(matches!(
            app.notice,
            super::super::Notice::Hint("greeter-auth-cancelled")
        ));
        assert!(app.auth_session.is_none());
        assert!(app.password_buffer.lock().unwrap().is_empty());
        daemon.join().unwrap();
    }

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
        // The console and the toolkit host's action surface are two doors to
        // one handler; both must reach the real greetd conversation.
        for door in ["console", "surface"] {
            remote_sign_in_through(door);
        }
    }

    fn remote_sign_in_through(door: &str) {
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
                .set_read_timeout(Some(tilo_test_env::hang_bound()))
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
            .env("TILO_GREETER_CONSOLE_AUTH_FIXTURE", door)
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
        let Some(door) = std::env::var_os("TILO_GREETER_CONSOLE_AUTH_FIXTURE") else {
            let app = GreeterApp::for_shot(AuthScenario::PersonalSessions);
            assert!(!ui::resources::resources(&app, &AutoCommit::new()).is_empty());
            return;
        };
        let _logs = tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .try_init();
        let mut app = GreeterApp::for_shot(AuthScenario::PersonalSessions);
        assert!(!app.keyboard_focus);
        let mut doc = AutoCommit::new();
        let resources = ui::resources::resources(&app, &doc);
        let args = json!({"password":SECRET}).as_object().unwrap().clone();
        // The secret is a `Secret` parameter: the published catalogue never
        // carries it; it reaches the handler only inside the in-process
        // operation, as every typed effect's secret does.
        let catalogue = catalog::enumerate(PROVIDER, &resources, "");
        assert!(!catalogue.to_string().contains(SECRET));
        let reply = if door == "surface" {
            let encoded = ui::tilo_actions::door::resolve(
                &resources,
                &ui::tilo_actions::ActionAddress::new(PROVIDER, "sign-in"),
            )
            .unwrap()
            .build_json(&args)
            .unwrap();
            let started = ui::host_operation::start_operation(
                &mut app,
                &ui::host_operation::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    OperationKind::Effect,
                    &encoded.type_id,
                    &encoded.payload,
                ),
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            )
            .unwrap()
            .and_then(ui::host_operation::ResourceOperationStart::completed)
            .unwrap();
            started.value.unwrap().to_string()
        } else {
            app.answer_console(
                &json!({"op":"actions_invoke","provider":PROVIDER,"id":"sign-in@greeter","args":args})
                    .to_string(),
                &doc,
            )
        };
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
