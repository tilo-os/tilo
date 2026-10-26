//! The greeter's console: the same `tilo-control` action protocol the
//! installer answers, so a caller that cannot see the screen can tell that
//! a first boot reached sign-in, which system image it booted, and what the
//! greeter publishes — without OCR, SSH or a debug service.
//!
//! Authentication uses the ordinary greetd/PAM worker. Secret arguments are
//! transient input only: published resource values never contain credentials,
//! and a secret travels only inside the typed [`Operation`] that carries it
//! to [`GreeterApp::answer`], like every typed effect. Keyboard input
//! readiness remains separately observable.
//!
//! Every door ends in that one function: this console, the toolkit's typed
//! effect host (`ui::widget::resource_effects`) and the keyboard/pointer
//! handlers, which call the same `attempt_login` and choosers.

use automerge::AutoCommit;
use console_control::{Request, catalog, wire};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ui::tilo_actions::{
    ActionParam, ActionSpec, Availability, EncodedAction, OperationKind, ParamType, Resource,
};
use ui::tilo_resource::Effect as _;
use ui::tilo_resource::effect::{Answers, EffectDecl};
use zeroize::{Zeroize, Zeroizing};

use super::{AuthPhase, GreeterApp};
use tilo_lock::MutexRecover as _;
use ui::a11y::AccessibilityRequest;

/// `provider` in `tilo-control` terms.
pub const PROVIDER: &str = "greeter";

/// The operation type every greeter action travels under. It names the
/// greeter's typed [`Operation`] vocabulary; the toolkit's effect host and
/// this console both decode that vocabulary.
pub const OPERATION: &str = "greeter.console/v1";

/// Everything the greeter can be asked to do.
///
/// Not `Debug`: the sign-in and challenge variants carry a secret, and a
/// derived `Debug` would print it into any log that formats an operation.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Operation {
    ConfigureSessionDiagnostics {
        presentation: bool,
        gpu_stages: bool,
        gpu_timestamps: bool,
        gpu_dispatch_timestamps: bool,
    },
    RespondChallenge {
        challenge: String,
        response: String,
    },
    CancelAuthentication,
    SignIn {
        password: String,
    },
    Fingerprint,
    ChooseAccount {
        username: String,
    },
    SetUsername {
        username: String,
    },
    ChooseSession {
        session: String,
    },
    Suspend,
    Reboot,
    PowerOff,
    LargerText {
        enabled: bool,
    },
    HighContrast {
        enabled: bool,
    },
    ScreenReader {
        enabled: bool,
    },
}

impl ui::tilo_resource::Effect for Operation {
    const OPERATION: &'static str = OPERATION;

    #[expect(
        clippy::too_many_lines,
        reason = "one table entry per published effect keeps the vocabulary together"
    )]
    fn declarations() -> Vec<EffectDecl> {
        [
            (
                "configure-session-diagnostics",
                "Configure timing diagnostics for this sign-in",
                vec![
                    ActionParam::required(
                        "presentation",
                        ParamType::Boolean,
                        "Content-free timing for this sign-in",
                    ),
                    ActionParam::required(
                        "gpu_stages",
                        ParamType::Boolean,
                        "Content-free timing for this sign-in",
                    ),
                    ActionParam::required(
                        "gpu_timestamps",
                        ParamType::Boolean,
                        "Content-free timing for this sign-in",
                    ),
                    ActionParam::required(
                        "gpu_dispatch_timestamps",
                        ParamType::Boolean,
                        "Content-free timing for this sign-in",
                    ),
                ],
            ),
            (
                "respond-challenge",
                "Respond to authentication challenge",
                vec![
                    ActionParam::required(
                        "challenge",
                        ParamType::Text,
                        "Current authentication challenge identity",
                    ),
                    ActionParam::required(
                        "response",
                        ParamType::Secret,
                        "Private transient authentication response",
                    ),
                ],
            ),
            ("cancel-authentication", "Cancel sign-in", Vec::new()),
            (
                "sign-in",
                "Sign in",
                vec![ActionParam::required(
                    "password",
                    ParamType::Secret,
                    "Password; transient authentication input only",
                )],
            ),
            ("fingerprint", "Use fingerprint", Vec::new()),
            (
                "choose-account",
                "Accounts on this computer",
                vec![ActionParam::required(
                    "username",
                    ParamType::Text,
                    "Account on this computer",
                )],
            ),
            (
                "set-username",
                "Username",
                vec![ActionParam::required(
                    "username",
                    ParamType::Text,
                    "Username to authenticate",
                )],
            ),
            (
                "choose-session",
                "Session",
                vec![ActionParam::required(
                    "session",
                    ParamType::Text,
                    "Available session",
                )],
            ),
            ("suspend", "Suspend", Vec::new()),
            ("reboot", "Restart", Vec::new()),
            ("power-off", "Shut down", Vec::new()),
            (
                "larger-text",
                "Larger text",
                vec![ActionParam::required(
                    "enabled",
                    ParamType::Boolean,
                    "Turn this on or off",
                )],
            ),
            (
                "high-contrast",
                "High contrast",
                vec![ActionParam::required(
                    "enabled",
                    ParamType::Boolean,
                    "Turn this on or off",
                )],
            ),
            (
                "screen-reader",
                "Screen reader",
                vec![ActionParam::required(
                    "enabled",
                    ParamType::Boolean,
                    "Turn this on or off",
                )],
            ),
        ]
        .into_iter()
        .map(|(id, label, params)| greeter_declaration(id, label, params))
        .collect()
    }

    fn id(&self) -> &'static str {
        match self {
            Self::ConfigureSessionDiagnostics { .. } => "configure-session-diagnostics",
            Self::RespondChallenge { .. } => "respond-challenge",
            Self::CancelAuthentication => "cancel-authentication",
            Self::SignIn { .. } => "sign-in",
            Self::Fingerprint => "fingerprint",
            Self::ChooseAccount { .. } => "choose-account",
            Self::SetUsername { .. } => "set-username",
            Self::ChooseSession { .. } => "choose-session",
            Self::Suspend => "suspend",
            Self::Reboot => "reboot",
            Self::PowerOff => "power-off",
            Self::LargerText { .. } => "larger-text",
            Self::HighContrast { .. } => "high-contrast",
            Self::ScreenReader { .. } => "screen-reader",
        }
    }

    fn arguments(&self) -> serde_json::Map<String, Value> {
        let Value::Object(mut arguments) = serde_json::to_value(self).unwrap_or(Value::Null) else {
            return serde_json::Map::new();
        };
        arguments.remove("op");
        arguments.retain(|_, value| !value.is_null());
        arguments
    }

    fn decode(id: &str, arguments: &serde_json::Map<String, Value>) -> Result<Self, String> {
        let mut invocation = arguments.clone();
        invocation.insert("op".into(), Value::String(id.to_owned()));
        // Report the category only: serde's message can quote the offending
        // input, and that input may be a passcode or challenge response.
        serde_json::from_value(Value::Object(invocation)).map_err(|error| {
            let category = match error.classify() {
                serde_json::error::Category::Io => "unreadable payload",
                serde_json::error::Category::Syntax => "malformed payload",
                serde_json::error::Category::Data => "unexpected fields or values",
                serde_json::error::Category::Eof => "truncated payload",
            };
            format!("not a greeter operation: {category}")
        })
    }
}

/// One greeter effect declaration, as the console publishes it. The effect's
/// arguments are its published parameters; no greeter operation changes a
/// document.
fn greeter_declaration(
    id: &'static str,
    label: &'static str,
    params: Vec<ActionParam>,
) -> EffectDecl {
    EffectDecl {
        id,
        label,
        description: "",
        params,
        kind: OperationKind::Effect,
        row: false,
        answers: Answers::Nothing,
        risk: ui::tilo_actions::RiskLevel::Low,
        idempotent: false,
        action_kind: ui::tilo_actions::ActionKind::Outside,
        own_device: false,
        attended: false,
        risk_of: None,
        audience: ui::tilo_actions::Audience::Everyone,
        untrusted: false,
        opens: None,
        participant: false,
        edits: false,
        member: false,
        item: None,
        offload: None,
        tags: &[],
        outside_with: None,
    }
}

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

    /// One request line in, one reply line out. The greeter keeps no state in
    /// a document, so the caller's state is not read.
    pub fn answer_console(&mut self, line: &str, _state: &AutoCommit) -> String {
        match wire::parse(line) {
            Err(error) => wire::error("parse_error", &error.0),
            Ok(Request::Unsupported { op }) => wire::unsupported(&op),
            Ok(Request::StateDump) => wire::state(&self.console_state()),
            Ok(Request::Enumerate { query }) => {
                wire::answer(&catalog::enumerate(PROVIDER, &self.publish(), &query))
            }
            Ok(Request::Invoke { provider, id, args }) => {
                let admitted =
                    console_control::admit(PROVIDER, &self.publish(), &provider, &id, &args);
                match admitted {
                    Err(refusal) => wire::error(refusal.code, &refusal.message),
                    Ok(mut admitted) => {
                        let reply = match self.apply_encoded(&admitted.encoded) {
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

    /// Invoke one published action exactly as the console door does: resolve
    /// it in the current tree, refuse it when unavailable, enforce its
    /// confirmation contract, bind its arguments strictly, then apply it. The
    /// palette path and host-side tests use this instead of a second
    /// operation path.
    ///
    /// # Errors
    ///
    /// Returns the door's stable code and sentence when the action is not
    /// published, unavailable, unconfirmed or given arguments it cannot bind.
    pub fn invoke_published(
        &mut self,
        id: &str,
        args: &serde_json::Map<String, Value>,
    ) -> Result<Value, Refused> {
        let mut admitted = console_control::admit(PROVIDER, &self.publish(), PROVIDER, id, args)
            .map_err(|refusal| refused(refusal.code, refusal.message))?;
        let reply = self.apply_encoded(&admitted.encoded);
        admitted.encoded.payload.zeroize();
        reply
    }

    /// Decode and apply one encoded greeter operation. The console and the
    /// toolkit's typed effect host both end here, after their own resolution,
    /// availability and argument binding; the handlers still re-check the
    /// greeter's preconditions.
    ///
    /// # Errors
    ///
    /// Returns a stable code and the reason when the operation is malformed
    /// or cannot run now.
    pub(super) fn apply_encoded(&mut self, encoded: &EncodedAction) -> Result<Value, Refused> {
        if encoded.type_id != Operation::OPERATION {
            return Err(refused(
                "invalid_arguments",
                format!("not a greeter operation: {}", encoded.type_id),
            ));
        }
        let operation = Operation::from_payload(&encoded.payload)
            .map_err(|message| refused("invalid_arguments", message))?;
        self.apply(operation)
    }

    /// Apply one typed operation and report what the console shows now.
    pub fn apply(&mut self, operation: Operation) -> Result<Value, Refused> {
        match operation {
            Operation::ConfigureSessionDiagnostics {
                presentation,
                gpu_stages,
                gpu_timestamps,
                gpu_dispatch_timestamps,
            } => {
                if gpu_dispatch_timestamps && !gpu_timestamps {
                    return Err(refused(
                        "invalid_arguments",
                        "GPU dispatch timing requires GPU timestamps",
                    ));
                }
                self.next_sign_in_diagnostics = Some([
                    presentation,
                    gpu_stages,
                    gpu_timestamps,
                    gpu_dispatch_timestamps,
                ]);
            }

            Operation::RespondChallenge {
                challenge,
                response,
            } => return self.respond_console_challenge(&challenge, response),
            Operation::CancelAuthentication => return self.cancel_console_authentication(),
            Operation::SignIn { password } => {
                let mut secret = Zeroizing::new(password);
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
            Operation::Fingerprint => self.attempt_login(super::AuthMethod::Fingerprint),
            Operation::ChooseAccount { username } => {
                let Some(index) = self.users.iter().position(|user| user.username == username)
                else {
                    return Err(refused(
                        "invalid_arguments",
                        "Choose an account on this computer",
                    ));
                };
                self.choose_account(index);
            }
            Operation::SetUsername { username } => {
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
            Operation::ChooseSession { session } => {
                let Some(index) = self
                    .sessions
                    .iter()
                    .position(|candidate| candidate.name == session)
                else {
                    return Err(refused("invalid_arguments", "Choose an available session"));
                };
                self.selected_session = index;
                self.session_choice.set(session);
                self.dirty_structure = true;
            }
            Operation::Suspend => self.request_power(super::PowerAction::Suspend),
            Operation::Reboot => self.request_power(super::PowerAction::Reboot),
            Operation::PowerOff => self.request_power(super::PowerAction::PowerOff),
            Operation::LargerText { enabled } => {
                self.retune(AccessibilityRequest::TextScale(if enabled {
                    super::LARGER_TEXT_SCALE
                } else {
                    1.0
                }));
            }
            Operation::HighContrast { enabled } => {
                self.retune(AccessibilityRequest::HighContrast(enabled));
            }
            Operation::ScreenReader { enabled } => {
                self.retune(AccessibilityRequest::ScreenReader(enabled));
            }
        }
        Ok(self.accepted())
    }

    /// The same one-shot request the footer's toggle raises; the host applies
    /// it on the next frame.
    fn retune(&self, request: AccessibilityRequest) {
        self.requests.accessibility.set(Some(request));
    }

    fn respond_console_challenge(
        &mut self,
        challenge: &str,
        response: String,
    ) -> Result<Value, Refused> {
        let Some(id) = challenge.parse::<u64>().ok() else {
            return Err(refused(
                "invalid_arguments",
                "Choose the current authentication challenge",
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
    vec![
        action_with("respond-challenge", "greeter-auth-respond", |declaration| {
            if let Some(challenge) = challenge
                && let Some(param) = declaration
                    .params
                    .iter_mut()
                    .find(|param| param.name == "challenge")
            {
                param.choices = vec![challenge.id.to_string().into()];
            }
        }),
        action("cancel-authentication", "greeter-auth-cancel"),
    ]
}

fn diagnostics_action() -> ActionSpec {
    action(
        "configure-session-diagnostics",
        "greeter-session-diagnostics",
    )
}

impl GreeterApp {
    /// What the greeter publishes now: one resource whose actions are the
    /// typed [`Operation`] vocabulary. The console enumerates and admits
    /// against it, and the toolkit's effect host publishes the same tree.
    pub fn publish(&self) -> Vec<Resource> {
        let mut resource = Resource::new(PROVIDER, "greeter.sign-in", super::t("greeter-sign-in"));
        let mut specs = vec![
            diagnostics_action(),
            action("sign-in", "greeter-sign-in"),
            action_with("choose-session", "greeter-session", |declaration| {
                if let Some(param) = declaration
                    .params
                    .iter_mut()
                    .find(|param| param.name == "session")
                {
                    param.choices = self
                        .sessions
                        .iter()
                        .map(|session| session.name.clone().into())
                        .collect();
                }
            }),
        ];
        let challenge = self
            .auth_session
            .as_ref()
            .and_then(crate::auth::AuthSession::challenge);
        specs.extend(conversation_actions(challenge));
        if self.users.len() > 1 {
            specs.push(action_with(
                "choose-account",
                "greeter-accounts",
                |declaration| {
                    if let Some(param) = declaration
                        .params
                        .iter_mut()
                        .find(|param| param.name == "username")
                    {
                        param.choices = self
                            .users
                            .iter()
                            .map(|user| user.username.clone().into())
                            .collect();
                    }
                },
            ));
        }
        if self.login_user.is_none() {
            specs.push(action("set-username", "greeter-username"));
        }
        if self.fingerprint_available_for(&self.attempt_username()) {
            specs.push(action("fingerprint", "greeter-use-fingerprint"));
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

impl ui::widget::resource_effects::TypedEffects for GreeterApp {
    type Effect = Operation;

    fn published_resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        self.publish()
    }

    fn answer(&mut self, effect: Operation) -> Result<ui::tilo_actions::ActionOutput, String> {
        GreeterApp::apply(self, effect).map_err(|(code, message)| format!("{code}: {message}"))?;
        Ok(ui::tilo_actions::ActionOutput::default())
    }
}

/// The footer's power menu and accessibility toggles: what a person can do
/// from the sign-in screen before anyone is signed in.
fn screen_actions() -> Vec<ActionSpec> {
    [
        ("suspend", "greeter-suspend"),
        ("reboot", "greeter-restart"),
        ("power-off", "greeter-shut-down"),
        ("larger-text", "greeter-larger-text"),
        ("high-contrast", "greeter-high-contrast"),
        ("screen-reader", "greeter-screen-reader"),
    ]
    .into_iter()
    .map(|(id, label)| action(id, label))
    .collect()
}

/// One published action of the greeter's one typed vocabulary, with the
/// translation key its label comes from. The declaration supplies every
/// effect property and argument; a caller can adjust only what the live
/// screen knows (the values a choice may take).
fn action(id: &'static str, label: &'static str) -> ActionSpec {
    action_with(id, label, |_| {})
}

fn action_with(
    id: &'static str,
    label: &'static str,
    adjust: impl FnOnce(&mut EffectDecl),
) -> ActionSpec {
    // The ids are this file's own constants; the fallback keeps a typo
    // visible as an inert action instead of panicking a sign-in screen.
    let mut declaration =
        Operation::declaration(id).unwrap_or_else(|| greeter_declaration(id, label, Vec::new()));
    adjust(&mut declaration);
    let mut spec = declaration.spec(Operation::OPERATION);
    spec.label = super::t(label);
    spec
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
    fn every_published_action_decodes_to_the_typed_vocabulary_the_host_answers() {
        // The host refuses the whole surface when any effect's operation type
        // has no handler. The typed vocabulary is the only handler, so this
        // proves every action the greeter publishes in every phase is one an
        // effect host can answer, and that its bound payload decodes.
        let mut app = GreeterApp::for_shot(AuthScenario::PersonalSessions);
        let arguments = |id: &str| match id {
            "sign-in" => json!({"password": "hunter2"}),
            "respond-challenge" => json!({"challenge": "1", "response": "123456"}),
            "choose-session" => json!({"session": "GNOME"}),
            "choose-account" | "set-username" => json!({"username": "ada"}),
            "configure-session-diagnostics" => json!({
                "presentation": true,
                "gpu_stages": false,
                "gpu_timestamps": false,
                "gpu_dispatch_timestamps": false,
            }),
            "larger-text" | "high-contrast" | "screen-reader" => json!({"enabled": true}),
            _ => json!({}),
        };
        for phase in [AuthPhase::Idle, AuthPhase::Authenticating] {
            app.phase = phase;
            let resources = app.publish();
            let handlers: std::collections::HashSet<String> =
                std::iter::once(Operation::OPERATION.to_owned()).collect();
            let violations = ui::tilo_actions::validate_surface(
                &resources,
                &ui::tilo_actions::ActionRegistry::new(),
                &handlers,
            );
            assert!(violations.is_empty(), "{phase:?}: {violations:?}");
            for spec in &resources[0].actions {
                let action = spec.id.as_str();
                let args = arguments(action).as_object().cloned().unwrap_or_default();
                let encoded = spec
                    .build_json(&args)
                    .unwrap_or_else(|error| panic!("{action}: {error}"));
                let effect = Operation::from_payload(&encoded.payload)
                    .unwrap_or_else(|error| panic!("{action}: {error}"));
                let declaration = Operation::declaration(action)
                    .unwrap_or_else(|| panic!("{action} has no canonical declaration"));
                assert_eq!(declaration.id, effect.id(), "{action}");
                assert_eq!(declaration.kind, spec.kind, "{action}");
                assert_eq!(declaration.risk, spec.contract.risk, "{action}");
                assert_eq!(declaration.action_kind(), spec.contract.kind, "{action}");
                for param in spec.params() {
                    let declared = declaration
                        .params
                        .iter()
                        .find(|declared| declared.name == param.name)
                        .unwrap_or_else(|| {
                            panic!("{action} has undeclared parameter {}", param.name)
                        });
                    assert_eq!(declared.ty, param.ty, "{action} {}", param.name);
                    assert_eq!(declared.required, param.required, "{action} {}", param.name);
                }
            }
        }

        app.phase = AuthPhase::Idle;
        let reply = app
            .invoke_published(
                "choose-session@greeter",
                json!({"session": "GNOME"}).as_object().unwrap(),
            )
            .unwrap();
        assert_eq!(reply["status"], "accepted");
        assert_eq!(app.sessions[app.selected_session].name, "GNOME");
    }

    #[test]
    fn the_production_effect_host_answers_the_published_actions() {
        let console = console_control::ConsoleServer::new();
        let app =
            GreeterApp::for_shot(AuthScenario::PersonalSessions).with_console(console.clone());
        let mut driver = ui::headless::FrameHarness::new(
            ui::State::new(),
            Box::new(ui::widget::resource_effects::host(app)),
            (1280, 800),
        );
        let spec = ui::headless::parse_spec(
            r#"{"actions":[
              {"invoke":{"resource":"greeter","action":"choose-session","args":{"session":"GNOME"}}}
            ]}"#,
        )
        .unwrap();
        for action in &spec.actions {
            driver.perform(action).unwrap();
        }
        let answer = console.submit(r#"{"op":"state_dump"}"#);
        driver.deliver_external(CONSOLE_WAKE);
        let state: Value = serde_json::from_str(&answer.recv().unwrap()).unwrap();
        assert_eq!(state["state"]["session"], "GNOME");
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
        // The console wire and the published-action door are two entries to
        // one typed handler; both must reach the real greetd conversation.
        for door in ["console", "published"] {
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
            assert!(!app.publish().is_empty());
            return;
        };
        let _logs = tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .try_init();
        let mut app = GreeterApp::for_shot(AuthScenario::PersonalSessions);
        assert!(!app.keyboard_focus);
        let mut doc = AutoCommit::new();
        let resources = app.publish();
        let args = json!({"password":SECRET}).as_object().unwrap().clone();
        // The secret is a `Secret` parameter: the published catalogue never
        // carries it; it reaches the handler only inside the in-process
        // operation, as every typed effect's secret does.
        let catalogue = catalog::enumerate(PROVIDER, &resources, "");
        assert!(!catalogue.to_string().contains(SECRET));
        let reply = if door == "published" {
            app.invoke_published("sign-in@greeter", &args).map_or_else(
                |(code, message)| format!("{code}: {message}"),
                |value| value.to_string(),
            )
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
