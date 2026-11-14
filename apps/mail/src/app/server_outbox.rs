//! What a window on a device that holds the account carries out for the
//! mailbox: the messages in its outbox, the changes its servers still have to
//! be told, and the checks for new mail that a window reaching no server
//! asked.

use std::collections::{HashMap, HashSet};

use crate::actions::{MailCheck, Outgoing, ServerUpdate};
use crate::engine::{EngineRequest, EngineRequestId};
use crate::i18n::CATALOG;
use crate::resource::{Mail, mailbox, read_mail};
use crate::state::Notice;

use super::{DeliveryDone, HeldSend, MailApp, ServerUpdateDone};

/// The moment, on this device's running clock, at which `outgoing` may leave,
/// when its wall-clock moment is `not_before` and the wall clock reads `wall`
/// now. A message with no moment goes at once.
fn leave_at(outgoing: &Outgoing, wall: i64, now: ui::time::Instant) -> ui::time::Instant {
    let wait = outgoing
        .not_before
        .map_or(0, |not_before| not_before.saturating_sub(wall));
    now + std::time::Duration::from_millis(u64::try_from(wait).unwrap_or(0))
}

/// Called once a check for new mail another window asked of this device
/// was fetched, or why it failed.
pub(crate) type CheckDone = Box<dyn FnOnce(Result<(), String>)>;

#[derive(Default)]
pub(crate) struct ServerOutbox {
    pub(super) queued: Vec<(ServerUpdate, ServerUpdateDone)>,
    pub(super) sent: HashMap<EngineRequestId, (ServerUpdate, ServerUpdateDone)>,
    pub(super) deliveries: Vec<(Outgoing, DeliveryDone)>,
    /// Checks for new mail another window asked, waiting for this device.
    pub(super) checks: Vec<(MailCheck, CheckDone)>,
    /// Those this device is fetching, by the request that fetches.
    pub(super) checking: HashMap<EngineRequestId, CheckDone>,
}

impl ServerOutbox {
    pub(crate) fn push(&mut self, update: ServerUpdate, done: ServerUpdateDone) {
        self.queued.push((update, done));
    }

    pub(crate) fn deliver(&mut self, outgoing: Outgoing, done: DeliveryDone) {
        self.deliveries.push((outgoing, done));
    }

    pub(crate) fn check(&mut self, check: MailCheck, done: CheckDone) {
        self.checks.push((check, done));
    }
}

impl MailApp {
    /// Deliver the sends whose undo window has passed, and keep those whose
    /// has not; a message taken back (its entry gone) is dropped, and the
    /// window that pressed Send gets it back in its composer.
    pub(super) fn release_held_sends(&mut self, doc: &ui::widget::Edits<'_>) {
        if self.held_sends.is_empty() {
            return;
        }
        let now = ui::time::now();
        let wall = ui::time::unix_now_ms_i64();
        let waiting: HashSet<String> = read_mail(doc.doc())
            .map(|mail| {
                mail.outbox
                    .pending()
                    .map(|(id, _)| id.as_str().to_owned())
                    .collect()
            })
            .unwrap_or_default();
        for mut held in std::mem::take(&mut self.held_sends) {
            if !waiting.contains(&held.outgoing.id) {
                // Taken back before any device delivered it: the window that
                // pressed Send gets the message back, another window only
                // stops waiting for it.
                if self.sent_here_id(&held.outgoing.id) {
                    self.reopen_unsent(&held.outgoing);
                }
            } else if held.due <= now && held.outgoing.may_leave_by(wall) {
                self.deliver(held.outgoing, held.done);
            } else {
                // Not yet: a wall clock this device reads behind the one that
                // pressed Send waits for the moment again, never spinning on
                // a deadline already gone.
                if held.due <= now {
                    held.due = leave_at(&held.outgoing, wall, now);
                }
                self.held_sends.push(held);
            }
        }
    }

    /// Undo on the window that pressed Send: take back every message it sent
    /// whose window is still open, wherever a device holds the account, and
    /// put it in the composer. A message a device delivered first is said to
    /// be already sent, never silently kept.
    pub(super) fn take_back_sends(&mut self, doc: &mut ui::widget::Edits<'_>) {
        if !std::mem::take(&mut self.unsend) {
            return;
        }
        let sent = std::mem::take(&mut self.sent_here);
        let delivered: HashSet<String> = read_mail(doc.doc())
            .map(|mail| {
                mail.outbox
                    .entries()
                    .filter(|(_, entry)| {
                        matches!(entry.settled, Some(ui::tilo_resource::Settled::Done(())))
                    })
                    .map(|(id, _)| id.as_str().to_owned())
                    .collect()
            })
            .unwrap_or_default();
        let mut too_late = false;
        for outgoing in sent {
            // A device delivered it before Undo reached the outbox: saying so
            // is the only honest answer. Everything else still waits and is
            // taken back.
            if delivered.contains(&outgoing.id) {
                too_late = true;
                continue;
            }
            self.shared.borrow().submit(
                doc,
                Mail::CANCEL_SEND.with(mailbox::CancelSend {
                    id: outgoing.id.clone(),
                }),
            );
            self.reopen_unsent(&outgoing);
        }
        if too_late {
            self.shared.borrow_mut().status =
                Some(Notice::info(CATALOG.t("mail-status-already-sent")));
        }
        self.dirty = true;
    }

    /// Whether this window sent the outbox message `id`.
    fn sent_here_id(&self, id: &str) -> bool {
        self.sent_here.iter().any(|outgoing| outgoing.id == id)
    }

    /// Forget sends whose undo window has closed or whose message left the
    /// outbox: the notice stops offering to take them back.
    pub(super) fn expire_sent_here(&mut self, doc: &ui::tilo_resource::Doc) {
        if self.sent_here.is_empty() {
            return;
        }
        let wall = ui::time::unix_now_ms_i64();
        let known: HashSet<String> = read_mail(doc)
            .map(|mail| {
                mail.outbox
                    .entries()
                    .map(|(id, _)| id.as_str().to_owned())
                    .collect()
            })
            .unwrap_or_default();
        self.sent_here
            .retain(|outgoing| known.contains(&outgoing.id) && !outgoing.may_leave_by(wall));
    }

    pub(super) fn flush_server_outbox(&mut self) {
        let deliveries = std::mem::take(&mut self.server_outbox.borrow_mut().deliveries);
        // The undo window is in the message, not in the window that pressed
        // Send: whichever device carries the message out, page or machine,
        // waits until the same moment, and Undo reaches it until then.
        let wall = ui::time::unix_now_ms_i64();
        let now = ui::time::now();
        for (outgoing, done) in deliveries {
            if outgoing.may_leave_by(wall) {
                self.deliver(outgoing, done);
            } else {
                self.held_sends.push(HeldSend {
                    due: leave_at(&outgoing, wall, now),
                    outgoing,
                    done,
                });
            }
        }
        let checks = std::mem::take(&mut self.server_outbox.borrow_mut().checks);
        for (check, done) in checks {
            let account = self
                .shared
                .borrow()
                .accounts
                .accounts
                .iter()
                .find(|account| account.config.email == check.account)
                .map(|account| account.config.clone());
            let Some(account) = account else {
                done(Err(format!(
                    "{} is not set up on this device",
                    check.account
                )));
                continue;
            };
            match self
                .state()
                .post_engine(EngineRequest::fetch(account, check.folder))
            {
                Some(request_id) => {
                    self.server_outbox
                        .borrow_mut()
                        .checking
                        .insert(request_id, done);
                }
                None => done(Err("the mail engine is not running".into())),
            }
        }
        let queued = std::mem::take(&mut self.server_outbox.borrow_mut().queued);
        for (update, done) in queued {
            let account = self
                .shared
                .borrow()
                .accounts
                .accounts
                .iter()
                .find(|account| account.config.email == update.account())
                .map(|account| account.config.clone());
            let Some(account) = account else {
                done(Err(format!(
                    "{} is not set up on this device",
                    update.account()
                )));
                continue;
            };
            let message = Box::new(update.locate());
            let request = match &update {
                ServerUpdate::Flags { .. } => EngineRequest::store_flag(account, message),
                ServerUpdate::Move { to, .. } => EngineRequest::move_message(account, message, *to),
                ServerUpdate::Remove { .. } => EngineRequest::purge(account, message),
                ServerUpdate::SaveDraft { .. } => {
                    let Some(draft) = update.saved_draft() else {
                        done(Err("not a draft to keep".into()));
                        continue;
                    };
                    EngineRequest::save_draft(account, Box::new(draft))
                }
            };
            let posted = self.state().post_engine(request);
            match posted {
                Some(request_id) => {
                    self.server_outbox
                        .borrow_mut()
                        .sent
                        .insert(request_id, (update, done));
                }
                None => done(Err("the mail engine is not running".into())),
            }
        }
    }
}
