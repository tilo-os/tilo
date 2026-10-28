//! The window's side of the mail the person's devices hold for one another
//! ([`crate::copies`]): what it fetches is kept for them, and what they kept
//! and this device lacks is taken into the mailbox.

use ui::framework::PersonRecord;

use super::MailApp;
use crate::model::Message;
use crate::resource::read_mail;

impl MailApp {
    /// Keep the mail this window fetches for the person's other devices,
    /// and take what they kept, through their copies record. Where the
    /// person has no room of their own the record keeps no content, and
    /// nothing is shared.
    pub(crate) fn share_copies(&mut self, record: PersonRecord<crate::copies::MailCopies>) {
        if let Some(content) = record.content() {
            self.copies = Some(crate::copies::Copies::new(record, content));
        }
    }

    /// Take into the mailbox the mail the person's other devices kept for
    /// this one and this device lacks: from them, without asking a server,
    /// so it shows while its server cannot be reached and a refresh then
    /// asks the server only for what is newer. It is registered with the
    /// mailbox record and kept on this device like fetched mail.
    pub(super) fn take_peer_copies(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let Some(copies) = self.copies.as_mut() else {
            return;
        };
        if !copies.due() {
            return;
        }
        let mail = read_mail(doc.doc()).unwrap_or_default();
        let taken = {
            let st = self.shared.borrow();
            let accounts: Vec<String> = st
                .accounts
                .accounts
                .iter()
                .map(|account| account.config.email.clone())
                .collect();
            copies.take(&accounts, |id| {
                st.mailbox.get(id).is_some()
                    || mail
                        .messages
                        .get(&ui::tilo_resource::Id::from_raw(id.to_string()))
                        .is_some_and(|state| state.purged)
            })
        };
        if taken.is_empty() {
            return;
        }
        let baselines: Vec<crate::actions::IngestBaseline> = taken
            .iter()
            .filter_map(|message| kept_baseline(&mail, message))
            .collect();
        let mut st = self.shared.borrow_mut();
        let accounts: std::collections::BTreeSet<String> = taken
            .iter()
            .map(|message| message.account.clone())
            .collect();
        for message in taken {
            st.mailbox.insert(message);
        }
        if !baselines.is_empty() {
            st.ingest_baselines(doc, baselines);
        }
        for account in accounts {
            self.keep_on_device(&mut st, &account);
        }
        st.dirty = true;
    }
}

/// What registering `message`, fetched or kept, tells the mailbox record:
/// where its server holds it, and its flags where the record has none yet.
/// `None` for mail that is only here.
pub(super) fn kept_baseline(
    mail: &crate::resource::Mail,
    message: &Message,
) -> Option<crate::actions::IngestBaseline> {
    if message.account.is_empty() {
        return None;
    }
    let mut baseline = crate::actions::IngestBaseline::fetched(message, &message.account);
    if mail.knows(&message.id.to_string()) {
        baseline.read = None;
        baseline.starred = None;
        baseline.folder = None;
    }
    Some(baseline)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use automerge::AutoCommit;
    use tilo_content::ContentStore;

    use super::super::EngineHandle;
    use super::*;
    use crate::engine::{EngineEvent, EngineRequest, EngineRequestKind};
    use crate::model::Folder;
    use ui::widget::Widget as _;

    fn app() -> (MailApp, AutoCommit) {
        let mut app = MailApp::new_sample();
        let mut doc = crate::resource::TestMail::doc();
        app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        app.sync(&doc, &automerge::ROOT);
        (app, doc)
    }

    /// A message as `account`'s server sends it, and what reading it gives.
    fn sent_by_server(account: &str, uid: u32, at: i64, body: &str) -> (Message, Vec<u8>) {
        let raw = format!(
            "From: Ada <ada@x.example>\r\nTo: {account}\r\nSubject: Note {uid}\r\n\
Message-ID: <{uid}.kept@x.example>\r\n\r\n{body}\r\n"
        )
        .into_bytes();
        let mut message = crate::transport::parse_eml(&raw, at, Folder::Inbox).unwrap();
        message.account = account.to_owned();
        message.server = Some(crate::model::ServerPlace {
            folder: Folder::Inbox,
            uid,
            uid_validity: 7,
        });
        (message, raw)
    }

    /// One device's window with an engine the test answers for, and the
    /// person's copies record joined to a route of its own.
    fn device(
        record: PersonRecord<crate::copies::MailCopies>,
        lane: &Arc<ui::collaboration::record_content::ContentLane>,
    ) -> (
        MailApp,
        AutoCommit,
        std::sync::mpsc::Receiver<EngineRequest>,
        std::sync::mpsc::Sender<EngineEvent>,
    ) {
        let (mut app, doc) = app();
        let (outgoing, requests) = std::sync::mpsc::channel();
        let (incoming, incoming_rx) = std::sync::mpsc::channel();
        app.engine = Some(EngineHandle {
            tx: outgoing,
            rx: incoming_rx,
        });
        let content = ui::collaboration::record_content::RecordContent::store(
            ContentStore::memory(),
            Arc::clone(lane),
        );
        app.copies = Some(crate::copies::Copies::new(record, content));
        (app, doc, requests, incoming)
    }

    /// A second device of the person opens old mail its first device
    /// fetched while its own server cannot be reached: the message comes
    /// from the first device, by the address the person's copies record
    /// names, on the record's route, and the server is then asked only for
    /// what is newer than it.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "two devices and the route between them, step by step"
    )]
    fn a_device_takes_mail_its_other_device_fetched_while_its_server_is_unreachable() {
        use ui::collaboration::record_content::ContentLane;

        let person = "c7".repeat(32);
        let mut first_host =
            ui::framework::testing::TestApp::<crate::MailShell>::for_person(&person).unwrap();
        let mut second_host =
            ui::framework::testing::TestApp::<crate::MailShell>::for_person(&person).unwrap();
        let first_record = first_host.person::<crate::copies::MailCopies>().unwrap();
        let second_record = second_host.person::<crate::copies::MailCopies>().unwrap();
        // Each device's content of the record, and the route it asks on.
        let first_lane = ContentLane::of("so.tilo.mail", "copies-test-first-device");
        let second_lane = ContentLane::of("so.tilo.mail", "copies-test-second-device");
        let (mut first, mut first_doc, first_requests, first_events) =
            device(first_record.clone(), &first_lane);
        let (mut second, mut second_doc, second_requests, second_events) =
            device(second_record.clone(), &second_lane);
        let config = first.shared.borrow().accounts.active_config();
        let account = config.email.clone();
        assert_eq!(
            second.shared.borrow().accounts.active_config().email,
            account,
            "the person's accounts follow them"
        );

        // The first device fetches two messages from its server.
        let (old, old_raw) = sent_by_server(&account, 41, 1_000, "From last year.");
        let (newer, newer_raw) = sent_by_server(&account, 42, 2_000, "From last week.");
        let request = first
            .post_engine(EngineRequest::fetch(config.clone(), Folder::Inbox))
            .unwrap();
        first_requests.try_recv().unwrap();
        let mut synced = crate::sync::Synced {
            uid_validity: 7,
            reset: false,
            has_older: Some(false),
            ..Default::default()
        };
        let newer_address = tilo_content::hash_bytes(&newer_raw);
        synced.receive(vec![
            (old.clone(), old_raw.clone()),
            (newer.clone(), newer_raw),
        ]);
        first_events
            .send(EngineEvent::Fetched {
                request_id: request,
                account_id: account.clone(),
                folder: Folder::Inbox,
                synced,
            })
            .unwrap();
        first_events
            .send(EngineEvent::Completed {
                request_id: request,
                kind: EngineRequestKind::Fetch,
            })
            .unwrap();
        first.settle(
            &mut ui::widget::Edits::lend(&mut first_doc),
            &automerge::ROOT,
        );

        // The person's record names both, by the address of what the server
        // sent; the bytes went to the first device's host, which keeps them
        // with the record, and the window let go of its own copy.
        let old_address = tilo_content::hash_bytes(&old_raw);
        let kept = first_record.get();
        let named = kept
            .copies
            .get(&ui::tilo_resource::Id::from_raw(old.id.to_string()))
            .expect("the old message is kept for the person's devices");
        assert_eq!(named.content, old_address);
        assert_eq!(
            (named.folder, named.uid, named.uid_validity),
            (Folder::Inbox, 41, 7)
        );
        assert_eq!(kept.copies.len(), 2);
        let mut first_host_keeps: HashMap<String, Vec<u8>> =
            first_lane.take_requests().offers.into_iter().collect();
        assert_eq!(first_host_keeps.get(&old_address), Some(&old_raw));
        assert!(
            !first.copies.as_ref().unwrap().holds_for_test(&old_address),
            "the window holds no second copy of what its host keeps"
        );

        // The record reaches the second device; its server cannot be reached.
        second_record
            .record()
            .merge_peer_document(first_record.record().document().unwrap())
            .unwrap();
        let refresh = second
            .post_engine(EngineRequest::fetch(config.clone(), Folder::Inbox))
            .unwrap();
        second_requests.try_recv().unwrap();
        second_events
            .send(EngineEvent::Error {
                request_id: refresh,
                kind: EngineRequestKind::Fetch,
                context: "Fetch".into(),
                message: "the server cannot be reached".into(),
            })
            .unwrap();
        second_events
            .send(EngineEvent::Completed {
                request_id: refresh,
                kind: EngineRequestKind::Fetch,
            })
            .unwrap();
        second.settle(
            &mut ui::widget::Edits::lend(&mut second_doc),
            &automerge::ROOT,
        );
        assert!(
            second.shared.borrow().mailbox.get(old.id).is_none(),
            "nothing is here before it arrives"
        );

        // It asks the record's route, newest first, and the first device's
        // host answers with what it keeps.
        let wanted = second_lane.take_requests().wants;
        assert_eq!(
            wanted,
            [newer_address, old_address],
            "both messages are asked for, newest first"
        );
        for address in wanted {
            let bytes = first_host_keeps.remove(&address).unwrap();
            second_lane.arrive(address, bytes);
        }
        second.settle(
            &mut ui::widget::Edits::lend(&mut second_doc),
            &automerge::ROOT,
        );
        second.sync(&second_doc, &automerge::ROOT);

        let arrived = second
            .shared
            .borrow()
            .mailbox
            .get(old.id)
            .cloned()
            .expect("the old message arrived from the first device");
        assert_eq!(arrived.subject, "Note 41");
        assert!(
            arrived.body.contains("From last year."),
            "{:?}",
            arrived.body
        );
        assert_eq!(arrived.account, account);
        assert_eq!(
            arrived.server,
            Some(crate::model::ServerPlace {
                folder: Folder::Inbox,
                uid: 41,
                uid_validity: 7,
            })
        );
        assert!(second.shared.borrow().mailbox.get(newer.id).is_some());
        // The mailbox record knows where its server holds it, as for mail
        // fetched here.
        let mail = read_mail(&second_doc).unwrap();
        assert!(mail.knows(&old.id.to_string()));

        // Once its server answers again, it is asked only for what is newer.
        second
            .post_engine(EngineRequest::fetch(config, Folder::Inbox))
            .unwrap();
        assert!(matches!(
            second_requests.try_recv().unwrap(),
            EngineRequest::Fetch {
                want: crate::sync::Want::Latest {
                    known: Some(crate::sync::Window {
                        uid_validity: 7,
                        newest: 42,
                        oldest: 41,
                    })
                },
                ..
            }
        ));
    }
}
