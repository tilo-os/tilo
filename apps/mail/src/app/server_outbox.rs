//! What a window on a device that holds the account carries out for the
//! mailbox: the messages in its outbox, the changes its servers still have to
//! be told, and the checks for new mail that a window reaching no server
//! asked.

use std::collections::HashMap;

use crate::actions::{MailCheck, Outgoing, ServerUpdate};
use crate::engine::{EngineRequest, EngineRequestId};

use super::{DeliveryDone, HeldSend, MailApp, SEND_UNDO_SECS, ServerUpdateDone};

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
    pub(super) fn flush_server_outbox(&mut self) {
        let deliveries = std::mem::take(&mut self.server_outbox.borrow_mut().deliveries);
        let due = ui::time::now() + std::time::Duration::from_secs(SEND_UNDO_SECS);
        for (outgoing, done) in deliveries {
            if self.sends_to_hold > 0 {
                self.sends_to_hold -= 1;
                self.held_sends.push(HeldSend {
                    outgoing,
                    done,
                    due,
                });
                continue;
            }
            self.deliver(outgoing, done);
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
