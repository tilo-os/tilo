//! The greeter's console: the same `tilo-control` action protocol the
//! installer answers, so a caller that cannot see the screen can tell that
//! a first boot reached sign-in, which system image it booted, and what the
//! greeter publishes — without OCR, SSH or a debug service.
//!
//! The greeter publishes no actions of its own yet; `actions list` answers
//! with its (empty) tree rather than pretending, and signing in stays on the
//! keyboard. `accepts_input` says when that keyboard reaches the form: the
//! process and its console come up before the compositor maps the window and
//! focuses it, and keys typed before then are lost. `state_dump` never
//! includes the passcode or any PAM prompt text beyond what the screen
//! already shows.

use automerge::AutoCommit;
use console_control::{Request, catalog, wire};
use serde_json::{Value, json};

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
            Ok(Request::Invoke { provider, id, args }) => {
                match console_control::admit(
                    PROVIDER,
                    &ui::resources::resources(self, state),
                    &provider,
                    &id,
                    &args,
                ) {
                    Err(refusal) => wire::error(refusal.code, &refusal.message),
                    Ok(_) => wire::error(
                        "action_failed",
                        "the greeter's console does not run actions; sign in at the keyboard",
                    ),
                }
            }
        }
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
            "status": self.status,
            "accepts_input": self.phase == AuthPhase::Idle && self.keyboard_focus,
            "image": { "composefs": console_control::booted_composefs() },
        })
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
    fn the_console_reports_sign_in_and_runs_nothing() {
        let mut app = GreeterApp::for_shot(AuthScenario::Personal);
        let state = ask(&mut app, r#"{"op":"state_dump"}"#);
        assert_eq!(state["state"]["app"], "greeter");
        assert_eq!(state["state"]["phase"], "sign-in");
        assert_eq!(state["state"]["accepts_input"], false);
        assert_eq!(
            ask(&mut app, r#"{"op":"actions_enumerate"}"#)["answer"]["count"],
            0
        );
        let invoked = ask(
            &mut app,
            r#"{"op":"actions_invoke","provider":"greeter","id":"sign-in@greeter"}"#,
        );
        assert_eq!(invoked["err"]["code"], "unknown_action");
        let typed = ask(&mut app, r#"{"op":"type","text":"hunter2"}"#);
        assert_eq!(typed["err"]["code"], "not_implemented");
    }

    #[test]
    fn sign_in_accepts_input_only_while_the_window_has_the_keyboard() {
        let mut app = GreeterApp::for_shot(AuthScenario::Personal);
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
