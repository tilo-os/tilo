//! "Can't sign in" after boot: the recovery key, and the new secret it
//! ends with (docs/security/sign-in.md §4.5, §5.3).
//!
//! The form takes the passcode form's place. The recovery key and the new
//! secret, typed twice, are held as the passcode is: in process-local
//! buffers, zeroed after every attempt and when the form closes. tilo-authd
//! checks the key and seals the new secret, which can take seconds, so the
//! request runs on a worker thread and its answer is taken on the next pass.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};

use super::{
    AlignItems, AuthPhase, Button, ButtonRole, ControlSize, FieldRole, GreeterApp, JustifyContent,
    Notice, Presentation, Rc, SecretKind, SecretPrompt, Stack, Text, TextRole, Theme, UserEvent,
    Widget, boot, t,
};
use crate::handoff::{SET_SECRET_BOUND, SetSecret, set_secret_with_recovery};
use tilo_lock::MutexRecover as _;
use zeroize::{Zeroize, Zeroizing};

/// Wakes the screen when tilo-authd has answered.
pub(super) const RECOVERY_WAKE: &str = "\u{1}greeter-recovery";

/// The recovery form while it is open.
pub(super) struct Recovery {
    pub(super) key: Arc<Mutex<String>>,
    pub(super) new: Arc<Mutex<String>>,
    pub(super) confirm: Arc<Mutex<String>>,
    pub(super) notice: Notice,
    /// The answer to the request in flight, and the kind of secret it sets.
    pending: Option<(Receiver<SetSecret>, boot::SecretKind)>,
}

impl Recovery {
    fn new() -> Self {
        Self {
            key: Arc::new(Mutex::new(String::new())),
            new: Arc::new(Mutex::new(String::new())),
            confirm: Arc::new(Mutex::new(String::new())),
            notice: Notice::None,
            pending: None,
        }
    }

    fn wipe(&self) {
        for buffer in [&self.key, &self.new, &self.confirm] {
            buffer.lock_recover().zeroize();
        }
    }

    pub(super) fn is_pending(&self) -> bool {
        self.pending.is_some()
    }
}

impl Drop for Recovery {
    fn drop(&mut self) {
        self.wipe();
    }
}

/// The ids of the form's fields, in order.
pub(super) const RECOVERY_FIELDS: [&str; 3] = [
    "greeter.recovery-key",
    "greeter.new-secret",
    "greeter.confirm-secret",
];

impl GreeterApp {
    /// "Can't sign in" after boot: show the recovery form in place of the
    /// passcode form, its key field first.
    pub(super) fn open_recovery(&mut self) {
        if self.recovery.is_some() || self.login_user.is_none() || self.phase != AuthPhase::Idle {
            return;
        }
        self.clear_password();
        self.notice = Notice::None;
        self.recovery = Some(Recovery::new());
        self.dirty_structure = true;
        self.focus_reset_pending = true;
    }

    /// Back to the passcode form, saying `notice` under its field.
    fn close_recovery(&mut self, notice: Notice) {
        // Dropping the form zeroes its buffers.
        self.recovery = None;
        self.notice = notice;
        self.dirty_structure = true;
        self.focus_reset_pending = true;
    }

    /// Cancel: back to the passcode form, unless a change is in flight.
    pub(super) fn cancel_recovery(&mut self) {
        if self.recovery.as_ref().is_some_and(|r| !r.is_pending()) {
            self.close_recovery(Notice::None);
        }
    }

    fn set_recovery_notice(&mut self, notice: Notice) {
        if let Some(recovery) = self.recovery.as_mut()
            && recovery.notice != notice
        {
            recovery.notice = notice;
            self.dirty_structure = true;
        }
    }

    /// Check the entries and ask tilo-authd to set the new secret.
    pub(super) fn submit_recovery(&mut self) {
        let Some(recovery) = self.recovery.as_ref() else {
            return;
        };
        if recovery.is_pending() {
            return;
        }
        let Some(user) = self.login_user.as_ref().map(|user| user.username.clone()) else {
            return;
        };
        let key = Zeroizing::new(std::mem::take(&mut *recovery.key.lock_recover()));
        let new = Zeroizing::new(std::mem::take(&mut *recovery.new.lock_recover()));
        let confirm = Zeroizing::new(std::mem::take(&mut *recovery.confirm.lock_recover()));
        if key.trim().is_empty() {
            self.set_recovery_notice(Notice::Hint("greeter-recovery-hint-key"));
            self.refill(&key, &new, &confirm);
            return;
        }
        if new.is_empty() {
            self.set_recovery_notice(Notice::Hint("greeter-recovery-hint-new"));
            self.refill(&key, &new, &confirm);
            return;
        }
        if *new != *confirm {
            // The key stays; the two entries are typed again.
            self.set_recovery_notice(Notice::Error("greeter-recovery-mismatch"));
            self.refill(&key, "", "");
            self.focus_reset_pending = true;
            return;
        }
        drop(confirm);
        let kind = if new.bytes().all(|b| b.is_ascii_digit()) {
            boot::SecretKind::Pin
        } else {
            boot::SecretKind::Password
        };
        let (sender, answer) = mpsc::channel();
        let socket = self.auth_socket.clone();
        let proxy = self.proxy.get().cloned();
        let spawned = std::thread::Builder::new()
            .name("greeter-recovery".into())
            .spawn(move || {
                let outcome =
                    set_secret_with_recovery(&socket, &user, &key, &new, SET_SECRET_BOUND);
                drop((key, new));
                let _ = sender.send(outcome);
                if let Some(proxy) = proxy {
                    let _ = proxy.send_event(UserEvent::External(RECOVERY_WAKE.to_owned()));
                }
            });
        let Some(recovery) = self.recovery.as_mut() else {
            return;
        };
        if spawned.is_ok() {
            recovery.pending = Some((answer, kind));
            recovery.notice = Notice::None;
        } else {
            recovery.notice = Notice::Error("greeter-recovery-failed");
        }
        self.dirty_structure = true;
        self.focus_reset_pending = true;
    }

    /// Put back what was typed after a note about the form itself.
    fn refill(&self, key: &str, new: &str, confirm: &str) {
        if let Some(recovery) = &self.recovery {
            for (buffer, value) in [
                (&recovery.key, key),
                (&recovery.new, new),
                (&recovery.confirm, confirm),
            ] {
                let mut stored = buffer.lock_recover();
                stored.zeroize();
                stored.push_str(value);
            }
        }
    }

    /// Take tilo-authd's answer, when it has come.
    pub(super) fn take_recovery_result(&mut self) {
        let Some((answer, kind)) = self.recovery.as_ref().and_then(|r| r.pending.as_ref()) else {
            return;
        };
        let kind = *kind;
        let outcome = match answer.try_recv() {
            Ok(outcome) => outcome,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => SetSecret::Failed,
        };
        if let Some(recovery) = self.recovery.as_mut() {
            recovery.pending = None;
            recovery.wipe();
        }
        match outcome {
            SetSecret::Done => {
                if let Some(user) = &self.login_user {
                    self.secret_kinds.insert(user.username.clone(), kind);
                }
                self.failures = 0;
                self.close_recovery(Notice::Hint(if kind == boot::SecretKind::Pin {
                    "greeter-recovery-done-pin"
                } else {
                    "greeter-recovery-done-password"
                }));
            }
            refused => {
                self.set_recovery_notice(Notice::Error(match refused {
                    SetSecret::Wrong => "greeter-recovery-wrong",
                    SetSecret::WeakSecret => "greeter-recovery-weak",
                    SetSecret::Done | SetSecret::Failed => "greeter-recovery-failed",
                }));
                self.dirty_structure = true;
                self.focus_reset_pending = true;
            }
        }
    }

    /// The recovery key, the new secret twice, one line saying what the
    /// attempt is doing or why it was refused, and the actions.
    pub(super) fn build_recovery(
        &self,
        recovery: &Recovery,
        theme: &Theme,
        align: AlignItems,
    ) -> Box<dyn Widget> {
        let s = &theme.tokens.spacing;
        let pending = recovery.is_pending();
        let prompt = |id: &str, label: &str, buffer: &Arc<Mutex<String>>| {
            SecretPrompt::new(SecretKind::Secret)
                .with_presentation(Presentation::DialogBody)
                .with_id(id)
                .with_label(FieldRole::Secret, t(label))
                .with_secret_buffer(Arc::clone(buffer))
        };
        let [key_id, new_id, confirm_id] = RECOVERY_FIELDS;
        let line = recovery.notice.key().map(t);
        let confirm = prompt(confirm_id, "greeter-recovery-confirm", &recovery.confirm);
        let confirm = match recovery.notice {
            _ if pending => confirm.with_busy(Some(t("greeter-recovery-saving"))),
            Notice::Error(_) => confirm.with_error(line),
            Notice::Hint(_) | Notice::None => confirm.with_notice(line),
        };
        let requests = Rc::clone(&self.requests);
        let cancel = Rc::clone(&self.requests);
        let actions = Stack::row(
            s.s2,
            vec![
                Box::new(
                    Button::new("greeter.recovery-cancel", &t("greeter-recovery-cancel"))
                        .role(ButtonRole::Plain)
                        .control_size(ControlSize::Regular)
                        .with_enabled(!pending)
                        .on_view(move || cancel.cancel_recovery.set(true)),
                ),
                Box::new(
                    Button::new("greeter.recovery-save", &t("greeter-recovery-save"))
                        .role(ButtonRole::Primary)
                        .control_size(ControlSize::Regular)
                        .on_effect(move || requests.recover.set(true)),
                ),
            ],
        )
        .justify(JustifyContent::End);
        Box::new(
            Stack::column(
                s.s3,
                vec![
                    Box::new(
                        Stack::column(
                            0.0,
                            vec![Box::new(
                                Text::new(&t("greeter-recovery-intro"))
                                    .with_id("greeter.recovery-intro")
                                    .with_role(TextRole::Body)
                                    .with_color(theme.tokens.colors.text_dim),
                            )],
                        )
                        .align(align),
                    ),
                    Box::new(prompt(key_id, "greeter-recovery-key", &recovery.key)),
                    Box::new(prompt(new_id, "greeter-recovery-new", &recovery.new)),
                    Box::new(confirm),
                    Box::new(actions),
                ],
            )
            .max_width(super::FORM_WIDTH)
            .align(AlignItems::Stretch),
        )
    }
}
