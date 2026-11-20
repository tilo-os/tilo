//! The window's side of the mail the workspace holds
//! ([`crate::copies`]): what it fetches is kept for them, and what they kept
//! and this device lacks is taken into the mailbox.

use ui::framework::WorkspaceResource;

use super::MailApp;
use crate::model::Message;

impl MailApp {
    pub(crate) fn open_peer_copies(&mut self) {
        if self.copies.is_none()
            && let Some(record) = self
                .copies_opener
                .as_ref()
                .and_then(|opener| opener.open(crate::copies::DOCUMENT))
            && let Some(content) = record.record().content()
        {
            self.share_copies(record, content);
        }
    }

    pub(crate) fn share_copies(
        &mut self,
        record: WorkspaceResource<crate::copies::MailCopies>,
        content: tilo_content::ContentStore,
    ) {
        self.shared.borrow_mut().workspace_copies = Some(record.clone());
        self.copies = Some(crate::copies::Copies::new(record, content));
    }

    pub(super) fn take_peer_copies(&mut self) {
        let Some(copies) = self.copies.as_mut() else {
            return;
        };
        if !copies.record().may_read() {
            copies.discard_pending();
            let mut state = self.shared.borrow_mut();
            let removed = std::mem::take(&mut state.workspace_messages);
            state.purge_workspace_messages(&removed);
            return;
        }
        if !copies.due() {
            return;
        }
        let record = copies.record().get();
        {
            let mut state = self.shared.borrow_mut();
            let removed = state
                .workspace_messages
                .iter()
                .copied()
                .filter(|&id| {
                    state
                        .mailbox
                        .get(id)
                        .is_none_or(|message| record.copy(&message.account, id).is_none())
                })
                .collect::<std::collections::HashSet<_>>();
            state.purge_workspace_messages(&removed);
        }
        let mail = self.shared.borrow().messages();
        let taken = {
            let st = self.shared.borrow();
            copies.take(|id| {
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
        let mut st = self.shared.borrow_mut();
        for message in taken {
            if st.mailbox.get(message.id).is_some() {
                continue;
            }
            st.workspace_messages.insert(message.id);
            st.mailbox.insert(message);
        }
        st.dirty = true;
    }
}

/// What registering `message`, fetched or kept, tells the mailbox resource.
pub(super) fn kept_baseline(
    mail: &crate::message_states::MessageStates,
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

    fn device(
        record: WorkspaceResource<crate::copies::MailCopies>,
        link: &Arc<ui::collaboration::document_content::ContentLink>,
    ) -> (
        MailApp,
        AutoCommit,
        std::sync::mpsc::Receiver<EngineRequest>,
        std::sync::mpsc::Sender<EngineEvent>,
    ) {
        let (mut app, doc) = app();
        let (outgoing, requests) = std::sync::mpsc::channel();
        let (incoming, incoming_rx) = std::sync::mpsc::channel();
        app.state().engine = Some(EngineHandle {
            tx: outgoing,
            rx: incoming_rx,
        });
        let content = ui::collaboration::document_content::DocumentContent::store(
            ContentStore::memory(),
            Arc::clone(link),
        );
        app.share_copies(record, content);
        (app, doc, requests, incoming)
    }

    #[test]
    fn importing_saved_mail_reads_the_previous_record_and_reseals_its_content() {
        use crate::copies::{DOCUMENT, MailCopies, NewCopy, kept};

        let person = "a9".repeat(32);
        let mut host =
            ui::framework::testing::TestApp::<crate::MailShell>::for_person(&person).unwrap();
        let previous = host.person::<MailCopies>().unwrap();
        let legacy_content = previous.content().unwrap();
        let current = host
            .shared_resource::<MailCopies>(ui::apps::MAIL, DOCUMENT)
            .unwrap();
        let account = "mail@x.example";
        let (message, bytes) = sent_by_server(account, 17, 500, "Saved on another device.");
        let reference = legacy_content.put(&bytes).unwrap();
        previous
            .invoke(&MailCopies::KEEP_COPIES.with(kept::KeepCopies {
                copies: vec![NewCopy {
                    id: message.id.to_string(),
                    account: account.into(),
                    content: reference.clone(),
                    minutes: 500,
                    folder: Folder::Inbox,
                    uid: 17,
                    uid_validity: 7,
                }],
            }))
            .unwrap();

        let import = crate::actions::MailEffect::ImportSavedCopies;
        let first = host.effect_confirmed(&import).unwrap().value.unwrap();
        assert_eq!(first["imported"], 1);
        let state = current.get();
        let copy = state.copy(account, message.id).unwrap();
        assert!(tilo_content::is_sealed(&copy.content));
        assert_ne!(copy.content, reference);
        let route = current.record().route().unwrap();
        let link = ui::collaboration::document_content::ContentLink::of(&route);
        let address = tilo_content::content_address(&copy.content).unwrap();
        let (_, ciphertext) = link
            .take_requests()
            .offers
            .into_iter()
            .find(|(offered, _)| offered == &address)
            .expect("the imported content was sent on the workspace route");
        assert!(!ciphertext.windows(24).any(|window| window == &bytes[..24]));
        link.arrive(address, ciphertext);
        assert_eq!(
            current.record().content().unwrap().read(&copy.content),
            Some(bytes)
        );
        assert_eq!(
            host.effect_confirmed(&import).unwrap().value.unwrap()["imported"],
            0
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "two devices and the route between them, step by step"
    )]
    fn a_member_reads_mail_another_member_fetched_and_loses_access_on_revocation() {
        use ui::collaboration::document_content::ContentLink;
        use ui::tilo_actions::policy::{
            Answer, Grant, Grantee, KindSet, MemberAccess, Role, TreePath, WorkspacePolicy,
        };

        let person = "c7".repeat(32);
        let mut first_host =
            ui::framework::testing::TestApp::<crate::MailShell>::for_person(&person).unwrap();
        let reader = "b9".repeat(32);
        let mut second_host =
            ui::framework::testing::TestApp::<crate::MailShell>::for_person(&reader).unwrap();
        let workspace = ui::framework::testing::TEST_WORKSPACE;
        let mut policy = WorkspacePolicy::new(workspace, &person);
        policy.members.insert(reader.clone(), Role::Viewer);
        policy.grants.push(Grant::new(
            "read-fetched-mail",
            Grantee::role(Role::Viewer),
            TreePath::workspace(workspace)
                .app(ui::apps::MAIL)
                .resource(crate::copies::DOCUMENT),
            KindSet::READ,
            Answer::Allow,
            Grantee::person(person.as_str()),
        ));
        second_host.set_permissions(ui::permissions::PermissionContext::from_access(
            MemberAccess::person(policy, reader.as_str()),
        ));
        let first_resource = first_host
            .shared_resource::<crate::copies::MailCopies>(ui::apps::MAIL, crate::copies::DOCUMENT)
            .unwrap();
        let second_resource = second_host
            .shared_resource::<crate::copies::MailCopies>(ui::apps::MAIL, crate::copies::DOCUMENT)
            .unwrap();
        assert!(second_resource.may_read());
        assert!(!second_resource.may("keep-copies"));
        let first_link = Arc::new(ContentLink::default());
        let second_link = Arc::new(ContentLink::default());
        let (mut first, mut first_doc, first_requests, first_events) =
            device(first_resource.clone(), &first_link);
        let (mut second, mut second_doc, second_requests, second_events) =
            device(second_resource.clone(), &second_link);
        let config = first.shared.borrow().accounts.active_config();
        let account = config.email.clone();
        assert_eq!(
            second.shared.borrow().accounts.active_config().email,
            account,
            "both simulated connections reach the same account"
        );

        let (old, old_raw) = sent_by_server(&account, 41, 1_000, "From last year.");
        let (newer, newer_raw) = sent_by_server(&account, 42, 2_000, "From last week.");
        let request = first
            .state()
            .post_engine(EngineRequest::fetch(config.clone(), Folder::Inbox))
            .unwrap();
        first_requests.try_recv().unwrap();
        let mut synced = crate::sync::Synced {
            uid_validity: 7,
            reset: false,
            has_older: Some(false),
            ..Default::default()
        };
        synced.receive(vec![
            (old.clone(), old_raw.clone()),
            (newer.clone(), newer_raw.clone()),
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

        // The workspace names both messages using sealed content references.
        // The host retains the ciphertext and the window releases its copy.
        let kept = first_resource.get();
        let named = |message: &Message| {
            kept.copy(&account, message.id)
                .expect("the workspace keeps the message")
                .clone()
        };
        let old_reference = named(&old).content;
        assert!(tilo_content::is_sealed(&old_reference), "{old_reference}");
        assert_eq!(
            (
                named(&old).folder,
                named(&old).uid,
                named(&old).uid_validity
            ),
            (Folder::Inbox, 41, 7)
        );
        assert_eq!(kept.copies.len(), 2);
        let old_address = tilo_content::content_address(&old_reference).unwrap();
        let newer_address = tilo_content::content_address(&named(&newer).content).unwrap();
        let mut first_host_keeps: HashMap<String, Vec<u8>> =
            first_link.take_requests().offers.into_iter().collect();
        for (raw, address) in [(&old_raw, &old_address), (&newer_raw, &newer_address)] {
            let held = first_host_keeps.get(address).expect("the host keeps it");
            assert!(
                !held.windows(24).any(|window| window == &raw[..24]),
                "the host keeps ciphertext"
            );
        }
        assert!(
            !first
                .copies
                .as_ref()
                .unwrap()
                .holds_for_test(&old_reference),
            "the window holds no second copy of what its host keeps"
        );

        second_resource
            .record()
            .merge_peer_document(first_resource.record().document().unwrap())
            .unwrap();
        let refresh = second
            .state()
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

        // It asks the resource's route, newest first, and the first device's
        // host answers with what it keeps.
        let wanted = second_link.take_requests().wants;
        assert_eq!(
            wanted,
            [newer_address, old_address],
            "both messages are asked for by address, newest first"
        );
        for address in wanted {
            let bytes = first_host_keeps.remove(&address).unwrap();
            second_link.arrive(address, bytes);
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
        // The workspace copy is visible without entering the previous mailbox record.
        let mail = crate::resource::TestMail::messages(&second.shared.borrow());
        assert!(!mail.knows(&old.id.to_string()));
        assert!(second.shared.borrow().workspace_messages.contains(&old.id));

        second
            .state()
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
        second_host.set_role(ui::permissions::Role::Viewer);
        assert!(!second.shared.borrow().may_read_message(old.id));
        second.take_peer_copies();
        assert!(second.shared.borrow().mailbox.get(old.id).is_none());
    }
}
