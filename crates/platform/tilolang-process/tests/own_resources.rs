//! Desktop `TiloLang` Mail uses one mailbox per workspace and applies workspace roles.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a test: a failure is a panic"
)]

use std::fmt::Write as _;
use std::rc::Rc;
use std::sync::Arc;

use collaboration::{CollabEvent, CollabRequest, CollaborationManager, RouteSpec};
use tilo_action_policy::{
    Acting, Answer, Grant, Grantee, KindSet, MemberAccess, Role, WorkspacePolicy,
};
use tilo_render_protocol::channel::Channel;
use tilo_render_protocol::patch::unix::ChannelTransport;
use tilo_render_protocol::patch::{self, Level, ToUi, Transport as _};
use tilolang_process::TilolangProcess;
use tilolang_process::hosted::{Hosted, Spec};
use tilolang_process::phrases::Phrases;
use tilolang_process::testing::{Broker, Bus, Inbox, Sent};
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::instance::document::InstanceDocument;

const MAIL: &str = "so.tilo.lang.mail";
const MAIL_SOURCE: &str = include_str!("../../../../apps/tl/mail/app.tl");
const MAIL_IDS: &str = include_str!("../../../../apps/tl/mail/app.ids");

/// Two members, by the hex of their user root keys.
const ADA: &str = "abababababababababababababababababababababababababababababababab";
const BO: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";

/// What only Ada's mail says: in her conversations' list and search, and on the API, which
/// names a conversation by its newest words.
const ADAS: [&str; 3] = ["Launch plan", "Grace Hopper", "lease renewal"];
/// Subjects her inbox's messages are published under: the launch thread's replies are its own
/// messages, and the lease's is its one, so the API offers Ada's messages and not only the
/// conversations they are in.
const ON_HER_MAIL: [&str; 2] = [
    "Re: Launch plan for the autumn release",
    "Your lease renewal",
];

fn compiled(dir: &str, source: &str, ids: &str) -> Bundle {
    let lock = tilolang::IdsLock::parse(ids).unwrap();
    let compiled =
        tilolang::compile_package(source, &lock, &[]).unwrap_or_else(|d| panic!("{dir}: {d:#?}"));
    Bundle::new(&compiled, [])
}

fn mail() -> Bundle {
    compiled("mail", MAIL_SOURCE, MAIL_IDS)
}

/// The window, from the far end of the patch connection: the root's tree as the instance sent
/// it, and everything else it sent (the rows of its lists, the conversation opened beside).
struct Window {
    transport: ChannelTransport,
    root: Option<patch::Node>,
    sent: String,
}

impl Window {
    fn take(&mut self) {
        while let Some(batch) = self.transport.recv().unwrap() {
            for message in patch::decode_batch::<ToUi>(&batch).unwrap() {
                let _ = writeln!(self.sent, "{message:?}");
                match message {
                    ToUi::Open {
                        level: Level::Root,
                        tree,
                        ..
                    } => self.root = Some(tree),
                    ToUi::Patch {
                        level: Level::Root,
                        patches,
                    } => {
                        let root = self.root.as_mut().expect("a patch after the open");
                        for change in patches {
                            change.apply(root).expect("the patch applies");
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    /// Everything the window was sent to show, as text.
    fn shows(&self) -> String {
        assert!(self.root.is_some(), "the window shows its root");
        self.sent.clone()
    }
}

/// One member running Mail in their tilolang host.
struct Member {
    host: TilolangProcess,
    window: Window,
    inbox: Inbox,
    sent: Sent,
    route: RouteSpec,
}

fn wake() -> tilolang_tilo::background::Waker {
    Arc::new(|| {})
}

fn policy(workspace: uuid::Uuid) -> WorkspacePolicy {
    let mut policy = WorkspacePolicy::new(workspace.to_string(), ADA);
    policy.members.insert(BO.to_owned(), Role::Viewer);
    policy
}

fn member(bus: &Rc<Bus>, workspace: uuid::Uuid, person: &str, bundle: &Arc<Bundle>) -> Member {
    member_with_archive(bus, workspace, person, bundle, None)
}

fn member_with_archive(
    bus: &Rc<Bus>,
    workspace: uuid::Uuid,
    person: &str,
    bundle: &Arc<Bundle>,
    archive: Option<automerge::AutoCommit>,
) -> Member {
    let route = tilolang_process::route_of(&bundle.meta, workspace, person);
    let broker = Broker::on(bus, route.clone(), person);
    let (inbox, sent) = (broker.inbox(), broker.sent());
    let mut manager = CollaborationManager::with_peer_transport(broker);
    assert!(manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: route.document.clone(),
        resource_type: None,
        handle: Some(person.to_owned()),
    }));
    let _ = manager.poll();
    let record =
        InstanceDocument::new(automerge::AutoCommit::new(), manager, route.clone(), person);
    let mut host = TilolangProcess::new(None, wake());
    let kept = host.kept(MAIL).unwrap();
    if let Some(mut archive) = archive {
        let earlier = tilolang_tilo::member_document_of(MAIL, person).unwrap();
        let document = collab_types::document::DocumentId::parse(&earlier).unwrap();
        collaboration::document::save::resource_document(
            &kept.documents,
            MAIL,
            &workspace.to_string(),
            &document.storage_key(),
        )
        .unwrap()
        .save(&mut archive)
        .unwrap();
    }
    let (ours, theirs) = Channel::pair().unwrap();
    let watched = ours.as_fd().try_clone_to_owned().unwrap();
    let mut policy = policy(workspace);
    policy.grants.push(Grant::new(
        "read-mail",
        Grantee::role(Role::Viewer),
        policy.node().app(MAIL),
        KindSet::READ,
        Answer::Allow,
        Grantee::person(ADA),
    ));
    let hosted = Hosted::with_document(
        Spec::new(MAIL, workspace.to_string(), 1, person)
            .access(MemberAccess::new(policy, Acting::person(person))),
        record,
        Arc::clone(bundle),
        &kept,
        Box::new(ChannelTransport::new(ours).unwrap()),
        &Rc::new(Phrases::new("en-US")),
        host.formats(),
        wake(),
    );
    host.add(hosted, Some(watched));
    Member {
        host,
        window: Window {
            transport: ChannelTransport::new(theirs).unwrap(),
            root: None,
            sent: String::new(),
        },
        inbox,
        sent,
        route,
    }
}

impl Member {
    fn hosted(&self) -> &Hosted {
        self.host.instances().next().unwrap()
    }

    /// Frames until the host owes nothing: no instance being opened, and no window with a frame
    /// coming — a change its instance has not sent, or work it reads off the frame. Nothing is
    /// settled by letting time pass; a host that owes nothing draws nothing more.
    fn settle(&mut self) {
        let began = std::time::Instant::now();
        loop {
            let changed = self.host.turn();
            self.window.take();
            if !changed && !self.host.owes_work() {
                return;
            }
            assert!(
                began.elapsed() < std::time::Duration::from_secs(120),
                "the host owes work after {}s: {} opening, {} running",
                began.elapsed().as_secs(),
                self.host.opening(),
                self.host.instances().count()
            );
            if !changed {
                // Nothing moved: what the host owes is work off its frame, which wakes it.
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }

    /// What search finds in the member's Mail: every hit its indexes publish, as text.
    fn found(&self) -> String {
        let host = self.hosted().instance().host().unwrap();
        host.wait_index();
        format!("{:?}", tilolang_tilo::search::every_hit(&host))
    }

    /// What the member's instance publishes on the API (each resource with its actions), as
    /// text.
    fn published(&self) -> String {
        format!("{:?}", self.hosted().published())
    }

    /// The answer the member's instance sent to the API's invocation `id`.
    fn answer(&self, id: u64) -> String {
        self.sent
            .borrow()
            .iter()
            .find_map(|request| match request {
                CollabRequest::UpdateInvocation {
                    id: answered,
                    update,
                } if *answered == id => Some(format!("{update:?}")),
                _ => None,
            })
            .unwrap_or_else(|| panic!("invocation {id} was answered"))
    }
}

/// None of `ada`'s mail reaches `bo`: not his document, his window, his search or his api.
#[test]
fn workspace_members_share_mail_and_the_viewer_cannot_archive_it() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let bundle = Arc::new(mail());
    assert!(!bundle.meta.own_resources);
    let workspace = uuid::Uuid::new_v4();
    let bus = Rc::new(Bus::default());
    let mut ada = member(&bus, workspace, ADA, &bundle);
    let mut bo = member(&bus, workspace, BO, &bundle);

    assert_eq!(ada.route, RouteSpec::app(MAIL, workspace));
    assert_eq!(bo.route, ada.route);
    assert!(collab_types::validator::requires_independent_document_keys(
        MAIL, ""
    ));
    assert!(collab_types::validator::requires_independent_document_keys(
        MAIL,
        &tilo_actions::document_sync::document_chunk("", 1),
    ));
    let old = tilolang_tilo::member_document_of(MAIL, ADA).unwrap();
    assert!(!tilolang_tilo::reaches(MAIL, Some(&old), Some(BO)));

    // Ada's mail arrives: an inbox a week in.
    ada.settle();
    bo.settle();
    ada.host
        .instances_mut()
        .next()
        .unwrap()
        .stage("a busy inbox");
    ada.settle();
    bo.settle();
    for what in ADAS {
        assert!(ada.window.shows().contains(what), "Ada's Mail shows {what}");
        assert!(ada.found().contains(what), "Ada's search finds {what}");
    }
    for what in ON_HER_MAIL {
        assert!(
            ada.published().contains(what),
            "Ada's api offers {what}: {}",
            ada.published()
        );
    }

    for what in ADAS {
        assert!(bo.window.shows().contains(what), "Bo's Mail shows {what}");
        assert!(bo.found().contains(what), "Bo's search finds {what}");
    }
    assert_eq!(
        bo.hosted().record().doc().get_heads(),
        ada.hosted().record().doc().get_heads(),
        "both members have the same mailbox history"
    );
    assert!(
        bo.hosted()
            .published()
            .iter()
            .all(|action| action.action_label != "Archive"
                || !action.contract.availability.is_available()),
        "the viewer's command palette cannot archive a message"
    );

    // View access lets Bo read the mailbox. Ada can archive the conversation.
    let archive = ada
        .hosted()
        .published()
        .iter()
        .find(|action| action.action_label == "Archive")
        .expect("Ada's conversations offer Archive")
        .address
        .clone();
    let heads = ada.hosted().record().doc().get_heads();
    let invoke = |id: u64, actor: &str| CollabEvent::Invocation {
        id,
        invocation: tilo_actions::InvocationId::new(),
        resource: archive.resource.clone(),
        action: archive.action.clone(),
        argument: None,
        actor: tilo_actions::MemberId::person(actor).into(),
        context: None,
    };
    ada.inbox.borrow_mut().push_back(invoke(1, BO));
    ada.settle();
    assert!(
        ada.answer(1).contains("the action is not allowed"),
        "Bo is refused: {}",
        ada.answer(1)
    );
    assert_eq!(
        ada.hosted().record().doc().get_heads(),
        heads,
        "Ada's mail is as it was"
    );
    ada.inbox.borrow_mut().push_back(invoke(2, ADA));
    ada.settle();
    assert!(
        ada.answer(2).contains("Succeeded"),
        "Ada archives the conversation: {}",
        ada.answer(2)
    );
    let revoked = MemberAccess::new(policy(workspace), Acting::person(BO));
    assert!(bo.host.set_access(MAIL, Some(&revoked)));
    assert_eq!(bo.host.instances().count(), 0);
    assert_eq!(bo.host.take_ended(), [1]);
}

fn previous_mail(workspace: uuid::Uuid) -> automerge::AutoCommit {
    let launch = "    let launch = a.threads.insert(";
    let prior_source = MAIL_SOURCE.replacen(
        launch,
        &format!("    a.signature = \"Regards, Sam\"\n{launch}"),
        1,
    );
    assert_ne!(
        prior_source, MAIL_SOURCE,
        "the archive fixture has a signature"
    );
    let prior_bundle = Arc::new(compiled("archived mail", &prior_source, MAIL_IDS));
    let mut earlier = member(&Rc::new(Bus::default()), workspace, ADA, &prior_bundle);
    earlier.settle();
    earlier
        .host
        .instances_mut()
        .next()
        .unwrap()
        .stage("a busy inbox");
    earlier.settle();
    mark_previous_conversation(&mut earlier);
    earlier
        .host
        .instances_mut()
        .next()
        .unwrap()
        .stage("a reply being written");
    earlier.settle();
    let schedule = earlier
        .hosted()
        .published()
        .iter()
        .find(|action| action.address.action == "send_later")
        .expect("the draft can be scheduled")
        .address
        .clone();
    let at = serde_json::json!({ "at": "2099-12-31T12:00:00Z" });
    earlier
        .inbox
        .borrow_mut()
        .push_back(CollabEvent::Invocation {
            id: 9,
            invocation: tilo_actions::InvocationId::new(),
            resource: schedule.resource,
            action: schedule.action,
            argument: Some(format!("{}{at}", tilo_actions::ARGUMENTS_JSON_PREFIX)),
            actor: tilo_actions::MemberId::person(ADA).into(),
            context: None,
        });
    earlier.settle();
    assert!(
        earlier.answer(9).contains("Succeeded"),
        "{}",
        earlier.answer(9)
    );
    assert_eq!(
        earlier
            .hosted()
            .instance()
            .host()
            .unwrap()
            .runtime()
            .pending_outbox()
            .len(),
        1,
        "the earlier copy has an unsent job"
    );
    earlier.hosted().record().doc()
}

fn mark_previous_conversation(earlier: &mut Member) {
    for (id, action, arguments) in [
        (
            6,
            "label",
            serde_json::json!({ "name": "Release planning", "on": true }),
        ),
        (7, "mute", serde_json::json!({ "muted": true })),
        (
            8,
            "snooze",
            serde_json::json!({ "until": "2099-12-31T12:00:00Z" }),
        ),
    ] {
        let address = earlier
            .hosted()
            .published()
            .iter()
            .find(|offered| {
                offered.address.action == action
                    && offered
                        .resource_label
                        .contains("Launch plan for the autumn release")
            })
            .unwrap_or_else(|| panic!("the old conversation offers {action}"))
            .address
            .clone();
        earlier
            .inbox
            .borrow_mut()
            .push_back(CollabEvent::Invocation {
                id,
                invocation: tilo_actions::InvocationId::new(),
                resource: address.resource,
                action: address.action,
                argument: Some(format!(
                    "{}{arguments}",
                    tilo_actions::ARGUMENTS_JSON_PREFIX
                )),
                actor: tilo_actions::MemberId::person(ADA).into(),
                context: None,
            });
        earlier.settle();
        assert!(
            earlier.answer(id).contains("Succeeded"),
            "{}",
            earlier.answer(id)
        );
    }
}

fn draft_subjects(member: &Member) -> Vec<String> {
    use tilolang_tilo::runtime::Value;

    let host = member.hosted().instance().host().unwrap();
    let root = host.root_of(MAIL).unwrap();
    let Value::List(drafts) = host.runtime().read(&root, "drafts").unwrap() else {
        panic!("the mailbox holds drafts");
    };
    drafts
        .into_iter()
        .map(|item| {
            let Value::Ref(draft) = item else {
                panic!("a draft item")
            };
            let text = host.runtime().read(&draft.target, "subject").unwrap();
            assert!(matches!(
                host.runtime().read(&draft.target, "sent"),
                Ok(Value::Bool(false))
            ));
            text.as_text().unwrap().to_owned()
        })
        .collect::<Vec<_>>()
}

fn assert_recovered_mail(member: &Member) {
    use tilolang_tilo::runtime::{ResourceRef, Value};

    let host = member.hosted().instance().host().unwrap();
    let root = host.root_of(MAIL).unwrap();
    let read = |item: &ResourceRef, field: &str| host.runtime().read(item, field).unwrap();
    let Value::List(threads) = read(&root, "recovered_mail") else {
        panic!("the workspace holds recovered conversations");
    };
    assert_eq!(threads.len(), 8);
    let conversation = threads
        .into_iter()
        .find_map(|item| {
            let Value::Ref(thread) = item else {
                panic!("a recovered conversation");
            };
            (read(&thread.target, "subject").as_text()
                == Some("Launch plan for the autumn release"))
            .then_some(thread.target)
        })
        .expect("the archive contains the launch conversation");
    assert!(matches!(read(&conversation, "muted"), Value::Bool(true)));
    assert_eq!(
        read(&conversation, "snoozed_until").as_text(),
        Some("2099-12-31T12:00:00Z")
    );
    assert!(matches!(read(&conversation, "labels"), Value::List(values)
        if values.iter().any(|name| name.as_text() == Some("Release planning"))));
    let Value::List(messages) = read(&conversation, "messages") else {
        panic!("the recovered conversation holds messages");
    };
    assert_eq!(messages.len(), 3);
    let Value::Ref(message) = &messages[0] else {
        panic!("a recovered message");
    };
    assert!(
        read(&message.target, "body")
            .as_text()
            .unwrap()
            .contains("The beta goes out on the fifth")
    );
    assert_eq!(
        read(&message.target, "received").as_text(),
        Some("2026-06-30T08:20:00Z")
    );
    assert_eq!(read(&message.target, "key").as_text(), Some(""));
    assert!(matches!(read(&root, "accounts"), Value::List(values) if values.is_empty()));
    assert!(member.window.shows().contains("Launch plan"));
    assert!(member.found().contains("Launch plan"));
}

#[test]
fn an_archive_owner_recovers_mail_into_the_shared_mailbox_once() {
    use tilolang_tilo::runtime::Value;

    let bundle = Arc::new(mail());
    let workspace = uuid::Uuid::new_v4();
    let old_draft = previous_mail(workspace);

    let bus = Rc::new(Bus::default());
    let mut ada = member_with_archive(&bus, workspace, ADA, &bundle, Some(old_draft));
    let mut bo = member(&bus, workspace, BO, &bundle);
    ada.settle();
    bo.settle();
    let recover = ada
        .hosted()
        .published()
        .iter()
        .find(|action| action.address.action == "recover_previous")
        .expect("the mailbox offers recovery")
        .address
        .clone();
    let invoke = |id, actor: &str| CollabEvent::Invocation {
        id,
        invocation: tilo_actions::InvocationId::new(),
        resource: recover.resource.clone(),
        action: recover.action.clone(),
        argument: None,
        actor: tilo_actions::MemberId::person(actor).into(),
        context: None,
    };

    ada.inbox.borrow_mut().push_back(invoke(10, BO));
    ada.settle();
    assert!(ada.answer(10).contains("permission_denied"));
    assert!(draft_subjects(&ada).is_empty());
    assert!(!ada.found().contains("Launch plan"));
    for id in [11, 12] {
        ada.inbox.borrow_mut().push_back(invoke(id, ADA));
        ada.settle();
        bo.settle();
        assert!(ada.answer(id).contains("Succeeded"), "{}", ada.answer(id));
        let expected = ["Re: Launch plan for the autumn release"];
        assert_eq!(draft_subjects(&ada), expected);
        assert_eq!(draft_subjects(&bo), expected);
        assert_recovered_mail(&ada);
        assert_recovered_mail(&bo);
        assert!(
            ada.hosted()
                .instance()
                .host()
                .unwrap()
                .runtime()
                .pending_outbox()
                .is_empty()
        );
    }

    ada.host
        .instances_mut()
        .next()
        .unwrap()
        .stage("a mailbox on its own");
    ada.settle();
    ada.inbox.borrow_mut().push_back(invoke(13, ADA));
    ada.settle();
    bo.settle();
    assert!(ada.answer(13).contains("Succeeded"), "{}", ada.answer(13));
    for member in [&ada, &bo] {
        let host = member.hosted().instance().host().unwrap();
        let root = host.root_of(MAIL).unwrap();
        let Value::List(accounts) = host.runtime().read(&root, "accounts").unwrap() else {
            panic!("the account is part of the shared mailbox");
        };
        let [Value::Ref(account)] = accounts.as_slice() else {
            panic!("the workspace has its configured account");
        };
        assert_eq!(
            host.runtime()
                .read(&account.target, "signature")
                .unwrap()
                .as_text(),
            Some("Regards, Sam")
        );
    }
}
