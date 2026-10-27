//! Calls, messages and email drafts: the app takes the number or the address
//! it holds and asks the session to open it.
//!
//! Contacts owns no dialler, composer or mailbox. It hands the value to the
//! desktop as a `tel:`, `sms:` or `mailto:` URI ([`crate::handoff`]) and
//! reports what the desktop answered: an app on this device opened it, or
//! nothing did. Silence is the one outcome it must not give — a verb that
//! does nothing leaves the person believing a call is being placed.

use std::sync::mpsc::TryRecvError;

use ui::portal::LaunchOutcome;

use super::ContactsApp;

/// What a person's number or address is handed over for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HandOff {
    Call,
    Message,
    Email,
}

impl HandOff {
    /// The verb in the person's words, for the refusal the window shows.
    fn verb(self) -> &'static str {
        match self {
            Self::Call => "call",
            Self::Message => "message",
            Self::Email => "email",
        }
    }

    /// `value` as the URI this verb opens, or `None` when it is not something
    /// that verb can reach.
    fn uri(self, value: &str) -> Option<String> {
        match self {
            Self::Call => crate::handoff::tel_uri(value),
            Self::Message => crate::handoff::sms_uri(value),
            Self::Email => crate::handoff::mailto_uri(value),
        }
    }

    /// Why the value cannot be used, in the person's words.
    fn refusal(self) -> &'static str {
        match self {
            Self::Call | Self::Message => "it has no dialable digits",
            Self::Email => "it is not an address a message can go to",
        }
    }
}

/// One verb waiting on the desktop's answer, and what it was asked to do in
/// the person's words ("call +44 20 7946 0018").
pub(super) type PendingHandOff = (ui::portal::LaunchChannel, String);

impl ContactsApp {
    /// Hand `value` to the desktop for `kind`
    /// ([`HandOff::uri`]), or say why it cannot be.
    ///
    /// The opener owns the answer, so it stays here until it comes; the
    /// window keeps working meanwhile. A refusal names the value it was
    /// asked for, so the person knows which number or address found no app.
    pub(crate) fn hand_off_value(&mut self, kind: HandOff, value: &str) {
        let Some(uri) = kind.uri(value) else {
            let refusal = kind.refusal();
            self.show_toast(format!("Couldn't {} {value}: {refusal}", kind.verb()));
            return;
        };
        let what = format!("{} {value}", kind.verb());
        self.hand_off.push((ui::portal::send_open_uri(&uri), what));
        self.dirty = true;
    }

    /// Open the person's website in the session's browser.
    pub(super) fn open_website(&mut self, url: String) {
        ui::widget::open_external_url(&url);
        self.opened_url = Some(url);
        self.dirty = true;
    }

    /// Take up every answer the desktop has given since the last frame.
    pub(super) fn drain_hand_off(&mut self) {
        let pending = std::mem::take(&mut self.hand_off);
        let mut open = Vec::with_capacity(pending.len());
        let mut refusals = Vec::new();
        for (rx, what) in pending {
            match rx.try_recv() {
                Ok(LaunchOutcome::Opened) => {}
                Ok(LaunchOutcome::Failed(reason)) => {
                    refusals.push(format!("Couldn't {what}: {reason}"));
                }
                Err(TryRecvError::Empty) => open.push((rx, what)),
                Err(TryRecvError::Disconnected) => refusals.push(format!(
                    "Couldn't {what}: the opener stopped without an answer"
                )),
            }
        }
        self.hand_off = open;
        for refusal in refusals {
            self.show_toast(refusal);
        }
    }
}
