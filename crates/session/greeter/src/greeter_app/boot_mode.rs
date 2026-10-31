//! The login screen as the boot's sign-in screen (docs/security/sign-in.md
//! §3.4): what tilo-boot-signin tells it, and the secret and "Can't sign
//! in" it answers with ([`crate::auth::boot`]).

use super::{
    AlignItems, AuthMethod, AuthPhase, BOOT_NOTE, Boot, BootLink, Button, ButtonRole, ControlSize,
    DeviceStatus, FingerprintAvailability, GreeterApp, LoginUser, Note, Notice, Rc, Stack,
    ToGreeter, Widget, boot, t, t_args,
};

impl GreeterApp {
    /// The boot sign-in screen, answering the supervisor at the other end of
    /// `link`. It offers no session choice and no fingerprint: the initramfs
    /// has neither, and the session after it starts for whoever signs in.
    pub fn for_boot(link: BootLink, screen_reader: bool) -> Self {
        let mut app = Self::with_sessions_login_user_and_fingerprint(
            Vec::new(),
            None,
            FingerprintAvailability::Unavailable,
        );
        app.screen_reader = screen_reader;
        app.boot = Some(Boot {
            link,
            people: Vec::new(),
            note: Note::None,
        });
        app.device = DeviceStatus::read();
        app
    }

    /// Show what the supervisor said ([`ToGreeter`]).
    pub fn boot_message(&mut self, message: ToGreeter) {
        if self.boot.is_none() {
            return;
        }
        match message {
            ToGreeter::Hello { .. } => {}
            ToGreeter::People {
                people,
                highlighted,
            } => {
                self.users = people
                    .iter()
                    .map(|person| LoginUser {
                        username: person.name.clone(),
                        display_name: person.display_name.clone(),
                    })
                    .collect();
                self.login_user = self
                    .users
                    .get(highlighted)
                    .or_else(|| self.users.first())
                    .cloned();
                if let Some(boot) = self.boot.as_mut() {
                    boot.people = people;
                }
                self.boot_ask(Note::None);
            }
            ToGreeter::Ask { person, note, .. } => {
                if let Some(user) = self.users.get(person) {
                    self.login_user = Some(user.clone());
                }
                self.boot_ask(note);
            }
            ToGreeter::Checking => {
                if self.phase != AuthPhase::Authenticating {
                    self.begin_authenticating();
                }
            }
            ToGreeter::Finish { unlocked } => {
                self.phase = if unlocked {
                    AuthPhase::Starting
                } else {
                    AuthPhase::Authenticating
                };
                self.notice = Notice::None;
                self.dirty_structure = true;
            }
        }
    }

    /// Ask for the chosen person's secret, with `note` under the field.
    pub(super) fn boot_ask(&mut self, note: Note) {
        if let Some(boot) = self.boot.as_mut() {
            boot.note = note;
        }
        self.phase = AuthPhase::Idle;
        self.prompt.clear();
        self.notice = if note == Note::None {
            Notice::None
        } else {
            Notice::Error(BOOT_NOTE)
        };
        self.clear_password();
        self.dirty_structure = true;
        self.focus_reset_pending = true;
    }

    /// The kind of secret the chosen person types at the boot sign-in.
    pub(super) fn boot_secret(&self) -> Option<boot::SecretKind> {
        let boot = self.boot.as_ref()?;
        let user = self.login_user.as_ref()?;
        boot.people
            .iter()
            .find(|person| person.name == user.username)
            .map(|person| person.secret)
    }

    /// Whether the chosen person's secret is a PIN: at boot as the
    /// supervisor lists them, after boot as `tilo-authd` keeps it.
    pub(super) fn secret_is_pin(&self) -> bool {
        let kind = if self.boot.is_some() {
            self.boot_secret()
        } else {
            self.login_user
                .as_ref()
                .and_then(|user| self.secret_kinds.get(&user.username).copied())
        };
        kind == Some(boot::SecretKind::Pin)
    }

    /// Send the typed secret to the supervisor, which checks it.
    pub(super) fn attempt_boot_sign_in(&mut self) {
        if self.phase != AuthPhase::Idle {
            return;
        }
        let Some(person) = self.login_user.as_ref().and_then(|user| {
            self.users
                .iter()
                .position(|listed| listed.username == user.username)
        }) else {
            return;
        };
        let Ok(secret) = self.take_auth_secret(AuthMethod::Password) else {
            self.set_hint(if self.boot_secret() == Some(boot::SecretKind::Pin) {
                "greeter-hint-pin"
            } else {
                "greeter-hint-passcode"
            });
            return;
        };
        self.begin_authenticating();
        let sent = self
            .boot
            .as_ref()
            .is_some_and(|boot| boot.link.sign_in(person, secret).is_ok());
        if !sent {
            self.phase = AuthPhase::Idle;
            self.show_error("greeter-error-service");
        }
    }

    /// "Can't sign in", under the field: at boot the way to the other ways
    /// of opening the disk, after boot the recovery form for a known
    /// account (sign-in.md §5.3).
    pub(super) fn build_cant_sign_in(
        &self,
        busy: bool,
        align: AlignItems,
    ) -> Option<Box<dyn Widget>> {
        if self.boot.is_none() && self.login_user.is_none() {
            return None;
        }
        let requests = Rc::clone(&self.requests);
        Some(Box::new(
            Stack::column(
                0.0,
                vec![Box::new(
                    Button::new("greeter.cant-sign-in", &t("greeter-cant-sign-in"))
                        .role(ButtonRole::Plain)
                        .control_size(ControlSize::Large)
                        .with_enabled(!busy)
                        .on_view(move || requests.cant_sign_in.set(true)),
                )],
            )
            .align(align),
        ))
    }
}

/// What the line under the field says for the supervisor's `note`.
pub(super) fn boot_note_text(note: Note) -> String {
    let pin = |secret| secret == boot::SecretKind::Pin;
    let count = |n: u32| [("count", n.into())];
    match note {
        Note::None => String::new(),
        Note::Wrong {
            secret,
            tries_left: None,
        } => t(if pin(secret) {
            "greeter-error-pin"
        } else {
            "greeter-error-passcode"
        }),
        Note::Wrong {
            secret,
            tries_left: Some(left),
        } => t_args(
            if pin(secret) {
                "greeter-error-pin-tries"
            } else {
                "greeter-error-passcode-tries"
            },
            &count(left),
        ),
        Note::Repeated {
            secret,
            tries_left: None,
        } => t(if pin(secret) {
            "greeter-error-pin-repeated"
        } else {
            "greeter-error-passcode-repeated"
        }),
        Note::Repeated {
            secret,
            tries_left: Some(left),
        } => t_args(
            if pin(secret) {
                "greeter-error-pin-repeated-tries"
            } else {
                "greeter-error-passcode-repeated-tries"
            },
            &count(left),
        ),
        Note::Wait { seconds } => {
            let minutes = seconds.div_ceil(60).max(1);
            let whole = |n: u64| u32::try_from(n).unwrap_or(u32::MAX);
            if minutes < 90 {
                t_args("greeter-error-wait-minutes", &count(whole(minutes)))
            } else {
                t_args(
                    "greeter-error-wait-hours",
                    &count(whole(minutes.div_ceil(60))),
                )
            }
        }
    }
}
