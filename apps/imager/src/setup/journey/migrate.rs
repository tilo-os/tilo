//! Joining the person's workspace before installing: this computer signs in
//! with a ticket from `tilo device add`, and the migration app then brings over
//! what this computer holds. Both are optional; installing works without them.

use std::sync::mpsc::{Receiver, TryRecvError};

use super::*;

/// The migration app, shown like any other app of the workspace.
const MIGRATIONS: &str = "tilo://open#/me/so.tilo.migrate";

#[derive(Default)]
pub(super) struct SignIn {
    /// The ticket as typed; it reaches only the `tilo login` child process.
    pub ticket: String,
    signing_in: Option<Receiver<Result<(), String>>>,
    opening: Option<ui::portal::LaunchChannel>,
    signed_in: bool,
    /// What the last sign-in or opening answered, shown on the page.
    status: Option<String>,
    status_is_error: bool,
}

impl SignIn {
    pub(super) fn busy(&self) -> bool {
        self.signing_in.is_some() || self.opening.is_some()
    }

    pub(super) fn set_ticket(&mut self, ticket: String) {
        if ticket != self.ticket {
            self.signed_in = false;
        }
        self.ticket = ticket;
    }

    /// Run `tilo login <ticket>` off the UI thread; [`Self::poll`] reads its answer.
    pub(super) fn start(&mut self) -> Result<(), &'static str> {
        if self.busy() {
            return Err("installer-migrate-busy");
        }
        let ticket = self.ticket.trim().to_owned();
        if ticket.is_empty() {
            return Err("installer-migrate-ticket-required");
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("installer-tilo-login".into())
            .spawn(move || {
                let result = std::process::Command::new("tilo")
                    .arg("login")
                    .arg(&ticket)
                    .stdin(std::process::Stdio::null())
                    .output()
                    .map_err(|error| error.to_string())
                    .and_then(|output| {
                        if output.status.success() {
                            Ok(())
                        } else {
                            let reason = String::from_utf8_lossy(&output.stderr);
                            Err(reason.trim().chars().take(400).collect())
                        }
                    });
                let _ = sender.send(result);
            })
            .map_err(|_| "installer-migrate-sign-in-failed")?;
        self.signing_in = Some(receiver);
        self.status = Some(t("installer-migrate-signing-in"));
        self.status_is_error = false;
        Ok(())
    }

    pub(super) fn open_migrations(&mut self) -> Result<(), &'static str> {
        if self.opening.is_some() {
            return Err("installer-migrate-busy");
        }
        self.opening = Some(ui::portal::send_open_uri(MIGRATIONS));
        Ok(())
    }

    /// Take the answers of a finished sign-in or opening; true when one arrived.
    pub(super) fn poll(&mut self) -> bool {
        let mut changed = false;
        if let Some(receiver) = &self.signing_in {
            let result = match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err(String::new())),
            };
            if let Some(result) = result {
                self.signing_in = None;
                match result {
                    Ok(()) => {
                        self.signed_in = true;
                        self.status = Some(t("installer-migrate-signed-in"));
                        self.status_is_error = false;
                    }
                    Err(reason) if reason.is_empty() => {
                        self.status = Some(t("installer-migrate-sign-in-failed"));
                        self.status_is_error = true;
                    }
                    Err(reason) => {
                        self.status = Some(t_args(
                            "installer-migrate-sign-in-refused",
                            &[("reason", reason.into())],
                        ));
                        self.status_is_error = true;
                    }
                }
                changed = true;
            }
        }
        if let Some(opening) = &self.opening {
            let outcome = match opening.try_recv() {
                Ok(outcome) => Some(outcome),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(ui::portal::LaunchOutcome::Failed(String::new()))
                }
            };
            if let Some(outcome) = outcome {
                self.opening = None;
                if let ui::portal::LaunchOutcome::Failed(_) = outcome {
                    self.status = Some(t("installer-migrate-open-failed"));
                    self.status_is_error = true;
                }
                changed = true;
            }
        }
        changed
    }

    pub(super) fn projection(&self) -> serde_json::Value {
        serde_json::json!({
            "signing_in": self.signing_in.is_some(),
            "signed_in": self.signed_in,
            "opening": self.opening.is_some(),
            "status": self.status,
            "failed": self.status_is_error,
        })
    }
}

impl InstallerPreview {
    /// The ticket field, the sign-in and the way to the migration app.
    pub(super) fn migrate_content(&self) -> ui::widget::Column {
        let busy = self.sign_in.busy();
        let mut content = page::related().push(Box::new(self.field(
            "ticket",
            &self.sign_in.ticket,
            TextInputKind::Code,
        )));
        let mut buttons = Row::new().with_rhythm(Rhythm::Related).push(Box::new(
            self.button(
                "installer.migrate.sign-in",
                "installer-migrate-sign-in",
                InstallerPreviewEffect::SignInForMigration,
            )
            .with_enabled(self.live && !busy && !self.sign_in.ticket.trim().is_empty()),
        ));
        buttons = buttons.push(Box::new(
            self.button(
                "installer.migrate.open",
                "installer-migrate-open",
                InstallerPreviewEffect::OpenMigrations,
            )
            .role(ButtonRole::Plain)
            .with_enabled(self.live && self.sign_in.signed_in && !busy),
        ));
        content = content.push(Box::new(buttons));
        if let Some(status) = &self.sign_in.status {
            let mut text = Text::new(status)
                .with_id("installer.migrate.status")
                .with_live(if self.sign_in.status_is_error {
                    tilo_ui_base::a11y::Politeness::Assertive
                } else {
                    tilo_ui_base::a11y::Politeness::Polite
                });
            if !self.sign_in.status_is_error {
                text = text.with_role(TextRole::Caption);
            }
            content = content.push(Box::new(text));
        }
        content
    }
}
