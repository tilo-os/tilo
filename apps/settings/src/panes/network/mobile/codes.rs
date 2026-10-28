//! The SIM's codes, asked with the window's password prompt
//! (`crate::secrets`): its PIN to unlock it or to switch the PIN lock, its
//! PUK and a new PIN once it is blocked, and the current and a new PIN to
//! change it. The section draws no code field of its own.
//!
//! The answer starts the same change the effect starts with the codes
//! given. While it runs the prompt says so; it closes when the change
//! lands, or says the code was wrong and asks again. A SIM that a wrong PIN
//! blocks is asked for its PUK next.

use ui::icons::Icon;
use ui::widget::{FieldSpec, PromptOutcome, SecretAnswer, SecretKind, SecretPrompt};

use super::{Begun, MobileChange, MobileModel, SimLock};
use crate::i18n::t;
use os_actions::{ModemChange, ModemInfo};

/// What the section asks its prompts under.
const KEY: &str = "mobile";

/// What the open prompt asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CodeAsk {
    /// The PIN of the locked SIM `sim`.
    Unlock(Option<String>),
    /// The PUK of the blocked SIM `sim`, and a new PIN.
    Unblock(Option<String>),
    /// The PIN, to start (`on`) or stop asking for it at power-on.
    PinLock { sim: Option<String>, on: bool },
    /// The current PIN and a new one.
    ChangePin(Option<String>),
}

/// The open prompt, and whether a change it started is running.
pub(super) struct Asking {
    pub(super) what: CodeAsk,
    busy: bool,
}

impl MobileModel {
    fn ask(&mut self, what: CodeAsk, prompt: SecretPrompt) {
        self.cx.secrets().ask(
            KEY,
            prompt
                .with_icon(Icon::Lock)
                .with_id("settings.network.mobile.prompt"),
        );
        self.asking = Some(Asking { what, busy: false });
        self.changed = true;
    }

    /// What the open prompt asks for.
    pub(super) fn asked(&self) -> Option<&CodeAsk> {
        self.asking.as_ref().map(|asking| &asking.what)
    }

    /// Ask for the PIN of the locked SIM `sim`.
    pub(super) fn ask_unlock(&mut self, sim: Option<String>, modem: Option<&ModemInfo>) {
        let prompt = SecretPrompt::new(SecretKind::SimPin {
            attempts_left: modem.and_then(|modem| modem.retries.pin),
        })
        .with_title(t("net-unlock-sim"))
        .with_resource(t("net-mobile-pin-explain"))
        .with_primary_label(t("net-unlock-sim"));
        self.ask(CodeAsk::Unlock(sim), prompt);
    }

    /// Ask for the PUK of the blocked SIM `sim` and a new PIN.
    pub(super) fn ask_unblock(&mut self, sim: Option<String>, modem: Option<&ModemInfo>) {
        let prompt = SecretPrompt::new(SecretKind::SimPuk {
            attempts_left: modem.and_then(|modem| modem.retries.puk),
        })
        .with_title(t("net-mobile-unblock"))
        .with_resource(t("net-mobile-puk-explain"))
        .with_primary_label(t("net-mobile-unblock"));
        self.ask(CodeAsk::Unblock(sim), prompt);
    }

    /// Ask for the PIN to start or stop asking for it at power-on.
    pub(super) fn ask_pin_lock(
        &mut self,
        sim: Option<String>,
        on: bool,
        modem: Option<&ModemInfo>,
    ) {
        let prompt = SecretPrompt::new(SecretKind::SimPin {
            attempts_left: modem.and_then(|modem| modem.retries.pin),
        })
        .with_title(t("net-mobile-pin-lock"))
        .with_resource(t(if on {
            "net-mobile-pin-lock-on-explain"
        } else {
            "net-mobile-pin-lock-off-explain"
        }))
        .with_primary_label(t(if on {
            "net-mobile-pin-lock-turn-on"
        } else {
            "net-mobile-pin-lock-turn-off"
        }));
        self.ask(CodeAsk::PinLock { sim, on }, prompt);
    }

    /// Ask for the current PIN and a new one.
    pub(super) fn ask_change_pin(&mut self, sim: Option<String>) {
        let prompt = SecretPrompt::with_fields(vec![
            FieldSpec::secret(t("net-mobile-current-pin")),
            FieldSpec::secret(t("net-mobile-new-pin")),
        ])
        .with_title(t("net-mobile-change-pin"))
        .with_resource(t("net-mobile-change-pin-explain"))
        .with_primary_label(t("net-mobile-change-pin"));
        self.ask(CodeAsk::ChangePin(sim), prompt);
    }

    /// Close the prompt.
    pub(super) fn close_codes(&mut self) {
        if self.asking.take().is_some() {
            self.cx.secrets().close(KEY);
            self.changed = true;
        }
    }

    /// A change the prompt started landed: done, it closes; refused, it
    /// says why and asks again.
    pub(super) fn settle_codes(&mut self, landed: &Result<(), String>) {
        let Some(asking) = self.asking.as_mut().filter(|asking| asking.busy) else {
            return;
        };
        match landed {
            Ok(()) => self.close_codes(),
            Err(why) => {
                asking.busy = false;
                self.cx.secrets().reject(KEY, why.clone());
            }
        }
    }

    /// A SIM a wrong PIN blocked asks for its PUK next; one that unlocked
    /// otherwise asks nothing.
    pub(super) fn follow_lock(&mut self) {
        let Some(asking) = self.asking.as_ref().filter(|asking| !asking.busy) else {
            return;
        };
        let lock = self.modem().map(|modem| modem.lock);
        match (&asking.what, lock) {
            (CodeAsk::Unlock(sim), Some(SimLock::Puk)) => {
                let sim = sim.clone();
                let modem = self.modem().cloned();
                self.ask_unblock(sim, modem.as_ref());
            }
            (CodeAsk::Unlock(_) | CodeAsk::Unblock(_), Some(SimLock::None)) => {
                self.close_codes();
            }
            _ => {}
        }
    }

    /// Take in what the person answered, and start what it asks for.
    pub(super) fn poll_codes(&mut self) {
        let Some(outcome) = self.cx.secrets().take_answer(KEY) else {
            return;
        };
        match outcome {
            PromptOutcome::Cancel => {
                if self.asking.take().is_some() {
                    self.changed = true;
                }
            }
            PromptOutcome::Submit(answer) => match self.answered(answer) {
                Ok(Begun::Work(work)) => {
                    if let Some(asking) = &mut self.asking {
                        asking.busy = true;
                    }
                    self.cx.secrets().busy(KEY, Some(t("net-mobile-checking")));
                    self.start_job(work);
                }
                Ok(Begun::Done) => {}
                Err(why) => self.cx.secrets().reject(KEY, why),
            },
        }
    }

    /// The change `answer` asks for.
    fn answered(&mut self, answer: SecretAnswer) -> Result<Begun, String> {
        let Some(what) = self.asked().cloned() else {
            return Ok(Begun::Done);
        };
        let change = match what {
            CodeAsk::Unlock(sim) => {
                let (_, sim) = self.sim(sim.as_deref(), |modem| modem.lock == SimLock::Pin)?;
                ModemChange::SendPin {
                    sim,
                    pin: answer.secret,
                }
            }
            CodeAsk::Unblock(sim) => {
                let (_, sim) = self.sim(sim.as_deref(), |modem| modem.lock == SimLock::Puk)?;
                ModemChange::SendPuk {
                    sim,
                    puk: answer.secret,
                    new_pin: answer.new_pin.unwrap_or_default(),
                }
            }
            CodeAsk::PinLock { sim, on } => {
                let (_, sim) = self.sim(sim.as_deref(), ModemInfo::usable)?;
                ModemChange::SetPinLock {
                    sim,
                    pin: answer.secret,
                    enabled: on,
                }
            }
            CodeAsk::ChangePin(sim) => {
                let (_, sim) = self.sim(sim.as_deref(), ModemInfo::usable)?;
                let mut values = answer.values.into_iter();
                ModemChange::ChangePin {
                    sim,
                    old_pin: values.next().unwrap_or_default(),
                    new_pin: values.next().unwrap_or_default(),
                }
            }
        };
        Ok(self.change(MobileChange::Modem(change)))
    }
}
