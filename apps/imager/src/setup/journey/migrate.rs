//! Bringing a person's things over before installing. Someone new to Tilo makes their identity
//! here: this computer's daemon founds the person, and their own workspace with them, as
//! `tilo login --new-identity` has it do. Someone who already uses Tilo signs in with a ticket
//! from `tilo device add`. Either way the migration app then brings over what this computer
//! holds. Both are optional; installing works without them.

use std::sync::mpsc::{Receiver, TryRecvError};

use super::*;

/// The migration app, shown like any other app of the workspace.
const MIGRATIONS: &str = "tilo://open#/me/so.tilo.migrate";

/// Whether the person has used Tilo before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Arrival {
    /// First time: their identity is made here.
    New,
    /// They have a device of theirs that issues a ticket.
    Returning,
}

impl Arrival {
    fn of(choice: &str) -> Option<Self> {
        match choice {
            "new" => Some(Self::New),
            "returning" => Some(Self::Returning),
            _ => None,
        }
    }

    const fn key(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Returning => "returning",
        }
    }
}

#[derive(Default)]
pub(super) struct SignIn {
    /// Whether the person said they used Tilo before; nothing is asked until they say.
    pub arrival: Option<Arrival>,
    /// The ticket as typed; it reaches only the `tilo login` child process.
    pub ticket: String,
    /// The name a new identity goes by, as workspaces show it.
    pub name: String,
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

    /// The person says whether they used Tilo before (`new`, `returning`); false when `choice`
    /// is no answer to that.
    pub(super) fn choose(&mut self, choice: &str) -> bool {
        let Some(arrival) = Arrival::of(choice) else {
            return false;
        };
        self.arrival = Some(arrival);
        true
    }

    pub(super) fn set_ticket(&mut self, ticket: String) {
        if ticket != self.ticket && self.arrival == Some(Arrival::Returning) {
            self.signed_in = false;
        }
        self.ticket = ticket;
    }

    pub(super) fn set_name(&mut self, name: String) {
        self.name = name;
    }

    /// Whether what the chosen way in needs is entered.
    fn ready(&self) -> bool {
        match self.arrival {
            Some(Arrival::New) => !self.name.trim().is_empty(),
            Some(Arrival::Returning) => !self.ticket.trim().is_empty(),
            None => false,
        }
    }

    /// Make the person's identity here, or sign in with the ticket, through `tilo login` off the
    /// UI thread; [`Self::poll`] reads its answer.
    pub(super) fn start(&mut self) -> Result<(), &'static str> {
        if self.busy() {
            return Err("installer-migrate-busy");
        }
        let mut command = std::process::Command::new("tilo");
        command.arg("login");
        let starting = match self.arrival {
            None => return Err("installer-migrate-arrival-required"),
            Some(Arrival::New) => {
                let name = self.name.trim();
                if name.is_empty() {
                    return Err("installer-migrate-name-required");
                }
                command.args(["--new-identity", "--name", name]);
                "installer-migrate-creating"
            }
            Some(Arrival::Returning) => {
                let ticket = self.ticket.trim();
                if ticket.is_empty() {
                    return Err("installer-migrate-ticket-required");
                }
                command.arg(ticket);
                "installer-migrate-signing-in"
            }
        };
        command.stdin(std::process::Stdio::null());
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("installer-tilo-login".into())
            .spawn(move || {
                let result = command
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
        self.status = Some(t(starting));
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
                        self.status = Some(t(if self.arrival == Some(Arrival::New) {
                            "installer-migrate-created"
                        } else {
                            "installer-migrate-signed-in"
                        }));
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
            "arrival": self.arrival.map(Arrival::key),
            "signing_in": self.signing_in.is_some(),
            "signed_in": self.signed_in,
            "opening": self.opening.is_some(),
            "status": self.status,
            "failed": self.status_is_error,
        })
    }
}

impl InstallerPreview {
    /// Whether the person used Tilo before, what that way in needs, the sign-in and the way to
    /// the migration app.
    pub(super) fn migrate_content(&self) -> ui::widget::Column {
        let busy = self.sign_in.busy();
        let arrival = self.sign_in.arrival;
        let mut content = layout::related()
            .push(Box::new(
                Text::new(&t("installer-preview-identity-heading")).with_role(TextRole::Title),
            ))
            .push(Box::new(
                self.arrival_card(Arrival::New, arrival == Some(Arrival::New)),
            ))
            .push(Box::new(self.arrival_card(
                Arrival::Returning,
                arrival == Some(Arrival::Returning),
            )));
        let (field, sign_in) = match arrival {
            Some(Arrival::New) => (
                Some(self.field("name", &self.sign_in.name, TextInputKind::Name)),
                "installer-migrate-create",
            ),
            Some(Arrival::Returning) => (
                Some(self.field("ticket", &self.sign_in.ticket, TextInputKind::Code)),
                "installer-migrate-sign-in",
            ),
            None => (None, "installer-migrate-sign-in"),
        };
        if let Some(field) = field {
            content = content.push(Box::new(field));
        }
        let mut buttons = Row::new().with_rhythm(Rhythm::Related);
        if arrival.is_some() {
            buttons = buttons.push(Box::new(
                self.button(
                    "installer.migrate.sign-in",
                    sign_in,
                    InstallerPreviewEffect::SignInForMigration,
                )
                .with_enabled(self.live && !busy && self.sign_in.ready()),
            ));
        }
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

    /// The answer `arrival` to whether the person used Tilo before, as a card that chooses it.
    fn arrival_card(&self, arrival: Arrival, selected: bool) -> Card {
        let key = arrival.key();
        let title = t(&format!("installer-preview-{key}-title"));
        let description = t(&format!("installer-preview-{key}-body"));
        let icon = match arrival {
            Arrival::New => Icon::UserPlus,
            Arrival::Returning => Icon::MonitorSmartphone,
        };
        let copy = layout::related()
            .push(Box::new(Text::new(&title).with_role(TextRole::Title)))
            .push(Box::new(Text::new(&description)));
        let contents = Row::new()
            .with_rhythm(Rhythm::Group)
            .push(Box::new(
                IconWidget::new(icon)
                    .sized(ui::widget::IconSize::Large)
                    .accent(),
            ))
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_flex_grow()
                    .with_child(Box::new(copy)),
            ));
        let commands = self.commands.clone();
        Card::new(Box::new(contents))
            .with_accessible_name(format!("{title}. {description}"))
            .selected(selected)
            .on_activate(format!("installer.choice.{key}"), move || {
                commands
                    .borrow_mut()
                    .push(InstallerPreviewEffect::ChoosePreview {
                        choice: key.to_owned(),
                    });
            })
    }
}
