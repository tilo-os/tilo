//! Checking for new mail from a window that reaches no mail server: a
//! browser page.
//!
//! A page holds no sockets and no account secrets, so it never fetches. What
//! it shows is what the person's devices fetched (the mailbox record and the
//! mail kept for their devices). When it wants new mail it records a check
//! in the mailbox's `checks` outbox, and a live Mail on a device that holds
//! the account carries it out like a send or a flag change: it fetches, and
//! what it fetched reaches the page through the same records. When no such
//! device takes the check soon, the page says so beside the account's mail
//! rather than waiting quietly.

use std::time::Duration;

use crate::actions::MailCheck;
use crate::engine::EngineRequest;
use crate::model::Folder;
use crate::resource::{Mail, mailbox};
use crate::state::SyncCause;

use super::MailApp;

/// How long a check may wait for a device before the page says none took it.
pub(crate) const UNANSWERED: Duration = Duration::from_secs(20);

/// A check this window asked for, and since when it waits.
struct Asked {
    check: MailCheck,
    since: ui::time::Instant,
    /// The window said that no device took it.
    said: bool,
}

/// What a window that reaches no mail server asks of the devices that do.
#[derive(Default)]
pub(crate) struct DeviceChecks {
    /// This window asks devices: it is live and reaches no server itself.
    pub(crate) asks: bool,
    /// Checks the window wanted since it last recorded them.
    wanted: Vec<MailCheck>,
    /// Checks recorded and not yet answered.
    asked: Vec<Asked>,
    /// Accounts whose inbox this window already asked to check once.
    seeded: Vec<String>,
}

impl DeviceChecks {
    /// Take `request` as a check to ask for when it is a fetch: `true` when
    /// this window asks devices, so the request is answered that way.
    pub(crate) fn want(&mut self, request: &EngineRequest) -> bool {
        if !self.asks {
            return false;
        }
        if let EngineRequest::Fetch {
            account, folder, ..
        } = request
        {
            let check = MailCheck {
                account: account.email.clone(),
                folder: *folder,
            };
            if check.is_valid() && !self.wanted.contains(&check) {
                self.wanted.push(check);
            }
        }
        true
    }

    /// Each of `accounts` this window has not asked about yet: their inbox
    /// is checked once as the window opens, as a native window fetches it.
    pub(crate) fn seed(&mut self, accounts: &[String]) -> Vec<String> {
        if !self.asks {
            return Vec::new();
        }
        let fresh: Vec<String> = accounts
            .iter()
            .filter(|account| !account.is_empty() && !self.seeded.contains(account))
            .cloned()
            .collect();
        self.seeded.extend(fresh.iter().cloned());
        fresh
    }

    /// When the window next has to look at a check that still waits.
    pub(crate) fn deadline(&self) -> Option<ui::time::Instant> {
        self.asked
            .iter()
            .filter(|asked| !asked.said)
            .map(|asked| asked.since + UNANSWERED)
            .min()
    }
}

/// How a check stands in the mailbox.
#[derive(Debug, PartialEq, Eq)]
enum Standing {
    Waiting,
    Done,
    Failed(String),
    /// The mailbox no longer holds it (pruned, or taken back).
    Gone,
}

fn standing(mail: &Mail, check: &MailCheck) -> Standing {
    if mail.checks.pending().any(|(_, waiting)| waiting == check) {
        return Standing::Waiting;
    }
    // The newest settled check of the same folder says how it went.
    let mut found = Standing::Gone;
    for (_, entry) in mail.checks.entries() {
        if entry.value != *check {
            continue;
        }
        found = match &entry.settled {
            Some(ui::tilo_resource::Settled::Done(())) => Standing::Done,
            Some(ui::tilo_resource::Settled::Failed { reason, .. }) => {
                Standing::Failed(reason.clone())
            }
            None => Standing::Waiting,
        };
    }
    found
}

impl MailApp {
    /// Ask once for every account the window shows that it has not asked
    /// about yet.
    pub(super) fn seed_checks(&self) {
        let accounts: Vec<String> = self
            .shared
            .borrow()
            .accounts
            .accounts
            .iter()
            .map(|account| account.config.email.clone())
            .collect();
        let fresh = self.state().checks.seed(&accounts);
        let configs: Vec<_> = self
            .shared
            .borrow()
            .accounts
            .accounts
            .iter()
            .filter(|account| fresh.contains(&account.config.email))
            .map(|account| account.config.clone())
            .collect();
        for config in configs {
            self.state()
                .post_engine(EngineRequest::fetch(config, Folder::Inbox));
        }
    }

    /// Record the checks the window wanted, and say what became of those
    /// it asked: a device that fetched clears what the window said, one that
    /// failed says why, and a check no device took for a while says that.
    pub(super) fn ask_devices(&mut self, doc: &mut ui::widget::Edits<'_>) {
        if !self.state().checks.asks {
            return;
        }
        let wanted = std::mem::take(&mut self.state().checks.wanted);
        let now = ui::time::now();
        let Some(mail) = crate::resource::read_mail(doc.doc()) else {
            return;
        };
        for check in wanted {
            let waiting = mail.checks.pending().any(|(_, queued)| *queued == check);
            if !waiting {
                self.shared.borrow().submit(
                    doc,
                    Mail::CHECK_MAIL.with(mailbox::CheckMail {
                        account: check.account.clone(),
                        folder: check.folder,
                    }),
                );
            }
            let mut room = self.state();
            if !room.checks.asked.iter().any(|asked| asked.check == check) {
                room.checks.asked.push(Asked {
                    check,
                    since: now,
                    said: false,
                });
            }
        }
        let Some(mail) = crate::resource::read_mail(doc.doc()) else {
            return;
        };
        let asked = std::mem::take(&mut self.state().checks.asked);
        let mut still = Vec::new();
        for mut asked in asked {
            let label = {
                let st = self.shared.borrow();
                st.accounts
                    .label_for(&asked.check.account)
                    .unwrap_or_else(|| asked.check.account.clone())
            };
            match standing(&mail, &asked.check) {
                Standing::Waiting => {
                    if !asked.said && now >= asked.since + UNANSWERED {
                        asked.said = true;
                        self.shared
                            .borrow_mut()
                            .note_sync_failure(label, SyncCause::NoDevice);
                        self.dirty = true;
                    }
                    still.push(asked);
                }
                Standing::Done | Standing::Gone => {
                    let mut st = self.shared.borrow_mut();
                    if st
                        .sync_error
                        .as_ref()
                        .is_some_and(|failure| failure.account == label)
                    {
                        st.clear_sync_failure();
                    }
                    self.dirty = true;
                }
                Standing::Failed(reason) => {
                    self.shared
                        .borrow_mut()
                        .note_sync_failure(label, crate::notice::sync_cause(&reason));
                    self.dirty = true;
                }
            }
        }
        self.state().checks.asked = still;
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::{Standing, UNANSWERED, standing};
    use crate::actions::MailCheck;
    use crate::app::{EngineHandle, MailApp};
    use crate::engine::{EngineEvent, EngineRequest, EngineRequestKind};
    use crate::model::Folder;
    use crate::resource::{Mail, TestMail, read_mail};
    use crate::state::SyncCause;
    use automerge::AutoCommit;
    use ui::tilo_resource::{Id, Settled};
    use ui::widget::{Edits, Widget as _};

    fn check() -> MailCheck {
        MailCheck {
            account: "ada@example.org".into(),
            folder: Folder::Inbox,
        }
    }

    fn turn(app: &mut MailApp, doc: &mut AutoCommit) {
        app.settle(&mut Edits::lend(doc), &automerge::ROOT);
        app.sync(doc, &automerge::ROOT);
        app.settle(&mut Edits::lend(doc), &automerge::ROOT);
    }

    /// A window in a browser page: live, with the person's accounts, and no
    /// engine.
    fn page() -> (MailApp, AutoCommit) {
        let mut page = MailApp::new_sample();
        page.state().checks.asks = true;
        let mut doc = TestMail::doc();
        turn(&mut page, &mut doc);
        (page, doc)
    }

    fn waiting(doc: &AutoCommit) -> Vec<MailCheck> {
        read_mail(doc)
            .map(|mail| {
                mail.checks
                    .pending()
                    .map(|(_, check)| check.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    tilo_test_env::scenario! {
        fn a_check_reads_how_the_device_that_took_it_answered;

        phase a_check_waits_until_a_device_settles_it;
        {
            let mut mail = Mail::default();
            assert_eq!(standing(&mail, &check()), Standing::Gone);
            let id = mail.checks.push(Id::from_raw("one"), check());
            assert_eq!(standing(&mail, &check()), Standing::Waiting);
            mail.checks.settle(&id, Settled::Done(()));
            assert_eq!(standing(&mail, &check()), Standing::Done);
        }

        phase a_failed_check_says_why;
        {
            let mut mail = Mail::default();
            let id = mail.checks.push(Id::from_raw("one"), check());
            mail.checks.settle(
                &id,
                Settled::Failed {
                    reason: "fetch: connection refused".into(),
                    partial: (),
                },
            );
            assert_eq!(
                standing(&mail, &check()),
                Standing::Failed("fetch: connection refused".into())
            );
        }
    }

    tilo_test_env::scenario! {
        /// A page reaches no mail server: it asks a device that holds the
        /// account to check, says so when none does, and reads what one did.
        fn a_page_asks_a_device_that_holds_the_account_for_new_mail;

        phase a_page_asks_each_account_once_as_it_opens;
        {
            let (mut page, mut doc) = page();
            let accounts: Vec<String> = page
                .shared
                .borrow()
                .accounts
                .accounts
                .iter()
                .map(|account| account.config.email.clone())
                .collect();
            assert!(!accounts.is_empty(), "the sample has accounts");
            let asked = waiting(&doc);
            for account in &accounts {
                assert!(
                    asked.contains(&MailCheck {
                        account: account.clone(),
                        folder: Folder::Inbox,
                    }),
                    "{account}'s inbox is asked for: {asked:?}"
                );
            }
            // Checking again while one waits asks nothing more.
            let _ = page.state().refresh_active_folder();
            turn(&mut page, &mut doc);
            assert_eq!(waiting(&doc).len(), asked.len());
            assert!(page.shared.borrow().sync_error.is_none());
        }

        phase a_check_no_device_takes_is_said_beside_the_mail;
        {
            let (mut page, mut doc) = page();
            for asked in &mut page.state().checks.asked {
                asked.since -= UNANSWERED;
            }
            turn(&mut page, &mut doc);
            let said = page
                .shared
                .borrow()
                .sync_error
                .clone()
                .expect("the window says no device took the check");
            assert_eq!(said.cause, SyncCause::NoDevice);
            let text = crate::notice::sync_failure(&said);
            assert!(text.title.contains(&said.account), "{}", text.title);
            assert!(text.message.is_some(), "it says what the window shows meanwhile");
        }

        phase a_device_that_holds_the_account_fetches_and_settles_the_check;
        {
            let (mut page, mut doc) = page();
            let first = waiting(&doc)
                .into_iter()
                .next()
                .expect("the page asked for a check");
            for asked in &mut page.state().checks.asked {
                asked.since -= UNANSWERED;
            }
            turn(&mut page, &mut doc);
            assert!(page.shared.borrow().sync_error.is_some());

            // The device's window holds the account and an engine.
            let mut device = MailApp::new_sample();
            let (outgoing, outgoing_rx) = std::sync::mpsc::channel();
            let (incoming, incoming_rx) = std::sync::mpsc::channel();
            device.state().engine = Some(EngineHandle {
                tx: outgoing,
                rx: incoming_rx,
            });
            let outcome: Rc<RefCell<Option<Result<(), String>>>> = Rc::default();
            let seen = Rc::clone(&outcome);
            device.server_outbox().borrow_mut().check(
                first.clone(),
                Box::new(move |result| *seen.borrow_mut() = Some(result)),
            );
            device.flush_server_outbox();
            let request = outgoing_rx.try_recv().expect("the device fetches");
            let EngineRequest::Fetch {
                request_id,
                account,
                folder,
                ..
            } = request
            else {
                panic!("a fetch, not {request:?}");
            };
            assert_eq!((account.email.as_str(), folder), (first.account.as_str(), first.folder));
            assert!(outcome.borrow().is_none(), "settled only once fetched");
            incoming
                .send(EngineEvent::Completed {
                    request_id,
                    kind: EngineRequestKind::Fetch,
                })
                .unwrap();
            device.drain_engine(&mut Edits::lend(&mut doc));
            assert_eq!(*outcome.borrow(), Some(Ok(())));

            // The framework records the device's answer in the mailbox; the
            // page reads it and stops saying no device took it.
            let id = read_mail(&doc)
                .and_then(|mail| {
                    mail.checks
                        .pending()
                        .find(|(_, check)| **check == first)
                        .map(|(id, _)| id.clone())
                })
                .expect("the check waits");
            page.shared.borrow().submit(
                &mut Edits::lend(&mut doc),
                Mail::paths().checks.settle(&id, &Settled::Done(())),
            );
            turn(&mut page, &mut doc);
            let error = page.shared.borrow().sync_error.clone();
            assert!(
                error.as_ref().is_none_or(|failure| failure.cause != SyncCause::NoDevice
                    || !failure.account.contains(&first.account)),
                "{error:?}"
            );
        }

        phase a_device_without_the_account_says_so_rather_than_fetching;
        {
            let mut device = MailApp::new_sample();
            let (outgoing, outgoing_rx) = std::sync::mpsc::channel();
            let (_incoming, incoming_rx) = std::sync::mpsc::channel();
            device.state().engine = Some(EngineHandle {
                tx: outgoing,
                rx: incoming_rx,
            });
            let outcome: Rc<RefCell<Option<Result<(), String>>>> = Rc::default();
            let seen = Rc::clone(&outcome);
            device.server_outbox().borrow_mut().check(
                MailCheck {
                    account: "someone-else@example.test".into(),
                    folder: Folder::Inbox,
                },
                Box::new(move |result| *seen.borrow_mut() = Some(result)),
            );
            device.flush_server_outbox();
            assert!(outgoing_rx.try_recv().is_err(), "nothing is fetched");
            assert!(matches!(*outcome.borrow(), Some(Err(_))));
        }
    }
}
