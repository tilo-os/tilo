//! Two replicas of one record — a person's and an agent's — converge through
//! the changes each takes and the other applies, with no transport between
//! them.

use crate::common::catalogue;
use chat::projection::ChatProjection as _;
use collab_types::DocumentMutation;
use tilo_actions::MemberId;
use tilo_agent_session::{Replica, ReplicaActor, Seed};

const WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

#[test]
fn a_record_kept_at_an_older_storage_version_opens_and_is_brought_up() {
    use ui::tilo_resource::evolution::{VERSION_KEY, stored_version};
    // Sheets keeps its workbook at a version it migrates up to.
    let app = "so.tilo.sheets";
    let mut page = crate::common::open(
        app,
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let Some(DocumentMutation::Snapshot(snapshot)) = page.take_mutation().unwrap() else {
        panic!("a new record is published whole");
    };
    let mut kept = automerge::AutoCommit::load(&snapshot).unwrap();
    let current = stored_version(&kept);
    assert!(current > 1, "Sheets has changed its storage");
    automerge::transaction::Transactable::put(
        &mut kept,
        automerge::ROOT,
        VERSION_KEY,
        automerge::ScalarValue::Uint(u64::from(current - 1)),
    )
    .unwrap();

    // A host, a bot or an agent opens what the workspace kept: it is brought up
    // to the app's version as a window brings it up.
    let mut held = crate::common::open(
        app,
        &MemberId::person("host"),
        ReplicaActor::Owner,
        Seed::Existing {
            record: page.document_id().clone(),
            changes: vec![kept.save()],
        },
    )
    .expect("an older record opens");
    assert_eq!(held.read(stored_version), Some(current));
    // The workspace still keeps the older document; the host brings the replica
    // up to it before each run, and the replica stays at its version.
    held.apply(&DocumentMutation::Snapshot(kept.save()))
        .expect("the workspace's older document merges into the brought-up replica");
    assert_eq!(held.read(stored_version), Some(current));
    // The migration is sent with the replica's changes, so the workspace's copy
    // is brought up too.
    let Some(DocumentMutation::Incremental(migration)) = held.take_mutation().unwrap() else {
        panic!("the migration is sent");
    };
    let mut workspace = kept.clone();
    tilo_automerge::ApplyIncoming::load_incoming(&mut workspace, &migration).unwrap();
    assert_eq!(stored_version(&workspace), current);
}

#[test]
fn read_only_resource_views_filter_roles_and_never_join_the_document_api() {
    use ui::permissions::{PermissionContext, Role};
    let app = "so.tilo.chat";
    let mut page = crate::common::open(
        app,
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let mut doc = page.read(Clone::clone).unwrap();
    chat::demo::apply_as(
        &mut doc,
        "ada",
        chat::Chat::CREATE_CATEGORY.with(chat::chat::chat::CreateCategory {
            name: "Resource view fixture".parse().unwrap(),
        }),
    )
    .unwrap();
    let category = chat::projection::read(&doc)
        .category_rows()
        .into_iter()
        .find(|category| category.name == "Resource view fixture")
        .unwrap()
        .id;
    page.apply(&DocumentMutation::Snapshot(doc.save())).unwrap();
    let resource = chat::projection::item_id("categories", &category);
    let before = body(&page);
    let viewer = PermissionContext::member_of(WORKSPACE, &MemberId::person("ada"), Role::Viewer);
    let denied = PermissionContext::stranger(&MemberId::person("ada"));
    let views = page.resource_views(&resource, "chat.workspace.categories", &viewer);
    assert!(
        !views.is_empty(),
        "the actual Chat resource declares a viewer navigation API"
    );
    assert!(
        views
            .iter()
            .all(|view| view.kind == tilo_actions::OperationKind::View)
    );
    assert!(
        page.resource_views(&resource, "chat.workspace.categories", &denied)
            .is_empty()
    );
    assert!(
        page.resource_views("missing", "chat.workspace.categories", &viewer)
            .is_empty()
    );
    assert!(
        page.resource_views(&resource, "wrong.kind", &viewer)
            .is_empty()
    );
    // The action API offers changes, never views: the record's document actions
    // and those its chat's chain routes to the part they change.
    assert!(
        page.survey()
            .iter()
            .all(|action| action.kind != tilo_actions::OperationKind::View)
    );
    assert_eq!(
        body(&page),
        before,
        "metadata discovery writes no CRDT changes"
    );
}

fn body(app: &Replica) -> String {
    app.read(|doc| serde_json::to_string(&automerge::AutoSerde::from(doc)).unwrap())
        .unwrap()
}

fn bytes(mutation: DocumentMutation) -> Vec<u8> {
    match mutation {
        DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => bytes,
    }
}

#[test]
fn an_agent_mount_and_a_person_mount_converge() {
    let app = "so.tilo.scratchpad";
    let mut page = crate::common::open(
        app,
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let snapshot = page
        .take_mutation()
        .unwrap()
        .expect("a new record is published whole");
    assert!(matches!(snapshot, DocumentMutation::Snapshot(_)));
    let mut agent = crate::common::open(
        app,
        &MemberId::agent("writer"),
        ReplicaActor::Agent {
            person: MemberId::person("writer"),
            name: String::new(),
        },
        Seed::Existing {
            record: page.document_id().clone(),
            changes: vec![bytes(snapshot)],
        },
    )
    .unwrap();
    // Whatever mounting wrote, the person's mount takes it in.
    if let Some(opened) = agent.take_mutation().unwrap() {
        page.apply(&opened).unwrap();
    }
    let (note, arguments) = crate::common::scratchpad_body("Drafted by the agent");
    agent.invoke(&note, &arguments, false).unwrap();
    let edit = agent
        .take_mutation()
        .unwrap()
        .expect("the edit is the agent's own change");
    page.apply(&edit).unwrap();
    assert!(
        body(&page).contains("Drafted by the agent"),
        "{}",
        body(&page)
    );
    assert!(
        agent.take_mutation().unwrap().is_none(),
        "nothing is taken twice"
    );

    // And back: the person's edit reaches the agent, and the agent never
    // republishes it as its own.
    let (note, arguments) = crate::common::scratchpad_body("Ada's revision");
    page.invoke(&note, &arguments, false).unwrap();
    agent
        .apply(&page.take_mutation().unwrap().unwrap())
        .unwrap();
    assert!(body(&agent).contains("Ada's revision"), "{}", body(&agent));
    assert!(
        agent.take_mutation().unwrap().is_none(),
        "a received change is not the agent's"
    );
}

/// The record's identity alone, as a member holds it before the
/// creator's content arrives: the shortest prefix of the creator's history
/// that names the record.
fn identity_only(page: &Replica) -> Vec<u8> {
    use automerge::{ROOT, ReadDoc as _};
    let changes = page
        .read(|doc| {
            doc.clone()
                .get_changes(&[])
                .into_iter()
                .map(|change| change.raw_bytes().to_vec())
                .collect::<Vec<_>>()
        })
        .unwrap();
    let mut prefix = automerge::AutoCommit::new();
    changes
        .into_iter()
        .find_map(|change| {
            tilo_automerge::ApplyIncoming::load_incoming(&mut prefix, &change).unwrap();
            let named = prefix
                .get(ROOT, "tilo:record")
                .unwrap()
                .is_some_and(|(_, record)| {
                    ["home", "id", "kind", "workspace"]
                        .iter()
                        .all(|key| prefix.get(&record, *key).unwrap().is_some())
                });
            named.then(|| prefix.save())
        })
        .expect("the creator's history names the record")
}

fn joined(
    catalogue: &tilo_agent_session::AppCatalogue,
    page: &Replica,
    changes: Vec<u8>,
) -> Option<Replica> {
    Replica::open(
        catalogue,
        page.app_id(),
        WORKSPACE,
        &MemberId::agent("writer"),
        ReplicaActor::Agent {
            person: MemberId::person("writer"),
            name: String::new(),
        },
        Seed::Existing {
            record: page.document_id().clone(),
            changes: vec![changes],
        },
    )
    .ok()
}

/// Every app's content, without the record identity both sides write.
fn content(app: &Replica) -> serde_json::Value {
    let mut body: serde_json::Value = serde_json::from_str(&body(app)).unwrap();
    body.as_object_mut().unwrap().remove("tilo:record");
    body
}

/// Only the record's creator writes its initial content: a member that
/// joins, before or after that content arrives, adds nothing of its own, so
/// nobody's later edits land in a copy the merge hides.
#[test]
fn only_the_creator_seeds_a_record() {
    let catalogue = catalogue();
    let mut failures = Vec::new();
    // The workspace's own record (Chat's) is not handed over as a copy.
    for definition in tilo_agent_session::component_apps()
        .into_iter()
        .filter(|definition| !definition.keeps_workspace_resource())
    {
        let app = definition.app_id();
        let mut page = crate::common::open(
            app,
            &MemberId::person("ada"),
            ReplicaActor::Owner,
            Seed::New,
        )
        .unwrap();
        let snapshot = page
            .take_mutation()
            .unwrap()
            .expect("a new record is published");
        let created = content(&page);

        // Joined after the content was published.
        let mut after = joined(&catalogue, &page, bytes(snapshot)).expect("the record opens");
        if let Some(opened) = after.take_mutation().unwrap() {
            page.apply(&opened).unwrap();
        }
        if content(&page) != created {
            failures.push(format!(
                "{app}: a joined mount changed the record: {}",
                body(&page)
            ));
        }

        // Joined before it: the identity alone, then the creator's document.
        // An app may also refuse to launch until the record is initialized
        // (Notes validates it): that writes nothing either.
        let Some(mut early) = joined(&catalogue, &page, identity_only(&page)) else {
            continue;
        };
        let whole = page.read(|doc| doc.clone().save()).unwrap();
        early.apply(&DocumentMutation::Snapshot(whole)).unwrap();
        if let Some(opened) = early.take_mutation().unwrap() {
            page.apply(&opened).unwrap();
        }
        if content(&page) != created {
            failures.push(format!(
                "{app}: an early joined mount changed the record: {}",
                body(&page)
            ));
        }
        if content(&early) != content(&page) {
            failures.push(format!(
                "{app}: the joined mount sees {} and the creator {}",
                body(&early),
                body(&page)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn an_agents_presence_is_where_its_last_operation_left_it() {
    let app = "so.tilo.word-processor";
    let mut agent = crate::common::open(
        app,
        &MemberId::agent("writer"),
        ReplicaActor::Agent {
            person: MemberId::person("writer"),
            name: String::new(),
        },
        Seed::New,
    )
    .unwrap();
    assert!(
        agent.presence().is_none(),
        "nothing done, nowhere in particular"
    );
    let append = agent
        .survey()
        .into_iter()
        .find(|operation| operation.address.action == "append-paragraph")
        .expect("the document publishes append-paragraph");
    let mut arguments = serde_json::Map::new();
    arguments.insert("text".into(), "Héllo".into());
    agent.invoke(&append.address, &arguments, false).unwrap();

    let (anchor, head) = agent
        .presence()
        .expect("the append leaves the agent somewhere");
    let (key, start, end) =
        tilo_server_protocol::live::read_presence_anchors(&anchor, &head).unwrap();
    let last = agent
        .read(|doc| {
            let state = <word_processor::model::Document as ui::tilo_resource::Resource>::read(doc)
                .unwrap();
            let body = state.text().unwrap().body();
            body.id_at(body.len() - 1).unwrap().to_owned()
        })
        .unwrap();
    // The paragraph it wrote, as the record's own item: the key every
    // member's Writer draws a member's caret under (as its
    // digest, when the key outgrows the live session's anchor).
    let written = ui::presence::PresenceKey::Resource {
        app: app.to_owned(),
        id: last,
        kind: "word-processor.document".to_owned(),
    };
    assert!(
        key == written || Some(&key) == written.digest_form().as_ref(),
        "{key:?}"
    );
    assert_eq!((start, end), (5, 5), "a caret after its five characters");
}

/// An agent has nobody beside it to confirm a removal: one Undo takes back
/// runs at once, so it can take back a clip it placed by mistake.
#[test]
fn an_agent_removes_a_clip_it_placed_without_being_asked() {
    let mut agent = crate::common::open(
        "so.tilo.video-editor",
        &MemberId::agent("editor"),
        ReplicaActor::Agent {
            person: MemberId::person("editor"),
            name: String::new(),
        },
        Seed::New,
    )
    .unwrap();
    let mut run = |action: &str, arguments: serde_json::Value| {
        let address = agent
            .survey()
            .into_iter()
            .find(|operation| operation.address.action == action)
            .unwrap_or_else(|| panic!("the project publishes {action}"))
            .address;
        let arguments = arguments.as_object().cloned().unwrap();
        agent
            .invoke(&address, &arguments, false)
            .unwrap_or_else(|error| panic!("{action}: {error}"))
            .value
            .unwrap_or_default()
    };
    let media = run("add-title", serde_json::json!({ "text": "HARBOUR DAYS" }));
    let clip = run(
        "add-clip",
        serde_json::json!({ "media": media["media"], "track": "V1", "at": 0, "in": 0, "out": 2 }),
    );
    let clip = clip["clip"].as_str().unwrap().to_owned();
    run("remove-clip", serde_json::json!({ "clip": clip }));
    assert!(!body(&agent).contains(&clip), "the clip is gone");
}

fn bot_access(kinds: ui::permissions::policy::KindSet) -> ui::permissions::policy::MemberAccess {
    use ui::permissions::policy::{
        Actor, Answer, Grant, Grantee, MemberAccess, MemberId, TreePath, WorkspacePolicy,
    };
    let bot = Grantee::bot("b1");
    let grant = Grant::new(
        "bot:b1",
        bot,
        TreePath::workspace(WORKSPACE),
        kinds,
        Answer::Allow,
        Grantee::person("ada"),
    );
    MemberAccess::person(WorkspacePolicy::new(WORKSPACE, "ada"), "ada")
        .acting_as(Actor::Member(MemberId::bot("b1")), vec![grant])
}

/// Ada's new scratchpad, and the same record held by her bot `b1` granted
/// `kinds` in the workspace.
fn ada_and_her_bot(kinds: ui::permissions::policy::KindSet) -> (Replica, Replica) {
    let mut page = crate::common::open(
        "so.tilo.scratchpad",
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let snapshot = page.take_mutation().unwrap().unwrap();
    let bot = crate::common::open(
        page.app_id(),
        &MemberId::bot("b1"),
        ReplicaActor::Bot {
            installation: "b1".into(),
            installer: "ada".into(),
            access: bot_access(kinds),
        },
        Seed::Existing {
            record: page.document_id().clone(),
            changes: vec![bytes(snapshot)],
        },
    )
    .unwrap();
    (page, bot)
}

#[test]
fn a_bot_replica_edits_within_its_grant_for_its_installer() {
    let (mut page, mut bot) = ada_and_her_bot(ui::permissions::policy::KindSet::READ_UNDOABLE);
    if let Some(opened) = bot.take_mutation().unwrap() {
        page.apply(&opened).unwrap();
    }
    let (note, arguments) = crate::common::scratchpad_body("Mirrored by the bot");
    let context =
        tilo_actions::ActionContext::default().with_caused_by(Some(tilo_actions::context::Cause {
            installation: "b1".into(),
            event: "e1".into(),
            depth: 1,
        }));
    let context =
        context
            .with_group("r1", "Mirror", "shortcut")
            .with_facet(tilo_actions::ActionFacet::new(
                tilo_automation::STEP_FACET,
                "0",
            ));
    bot.invoke_with(None, Some(context), &note, &arguments, false)
        .unwrap();
    page.apply(&bot.take_mutation().unwrap().unwrap()).unwrap();
    assert!(
        body(&page).contains("Mirrored by the bot"),
        "{}",
        body(&page)
    );
    // History names the bot for its installer, the run's cause and the run
    // step, which is how a restarted run finds the step it took.
    let entry = page
        .read(|doc| {
            doc.clone()
                .get_changes(&[])
                .iter()
                .filter_map(|change| {
                    tilo_actions::ActionContext::from_change_message(change.message()?)
                })
                .find(|context| context.caused_by.is_some())
        })
        .flatten()
        .expect("the bot's change records its entry");
    let invoker = entry.invoker.clone().unwrap();
    assert_eq!(
        invoker.acting.by,
        tilo_actions::Actor::Member(MemberId::bot("b1"))
    );
    assert!(invoker.acting.is_bot());
    assert_eq!(invoker.acting.acted_for_person(), Some("ada"));
    assert_eq!(entry.caused_by.as_ref().unwrap().event, "e1");
    assert_eq!(
        tilo_workspace_service::agents::programs::api::AutomationStep::of(&entry),
        Some(
            tilo_workspace_service::agents::programs::api::AutomationStep {
                invocation: "r1".into(),
                step: "0".into()
            }
        )
    );
}

#[test]
fn a_bot_without_a_write_grant_is_offered_only_reads() {
    let (_page, mut bot) = ada_and_her_bot(ui::permissions::policy::KindSet::READ);
    let (note, arguments) = crate::common::scratchpad_body("Not allowed");
    assert!(
        bot.survey().iter().all(|entry| entry.address != note),
        "an edit outside the bot's grant is not offered"
    );
    assert!(bot.invoke(&note, &arguments, false).is_err());
    assert!(!body(&bot).contains("Not allowed"));
}

#[test]
fn a_bot_reads_a_post_made_after_it_opened_the_record_through_the_record_api() {
    let mut page = crate::common::open(
        "so.tilo.chat",
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let mut doc = page.read(Clone::clone).unwrap();
    crate::common::add_channel(&mut doc, "ada", "general", None);
    let projection = chat::projection::read(&doc);
    let channel = projection.channel_rows()[0].id.clone();
    page.apply(&DocumentMutation::Snapshot(doc.save())).unwrap();
    let mut bot = crate::common::open(
        page.app_id(),
        &MemberId::bot("b1"),
        ReplicaActor::Bot {
            installation: "b1".into(),
            installer: "ada".into(),
            access: {
                use ui::permissions::policy::{
                    Actor, Answer, Grant, Grantee, KindSet, Level, MemberAccess, MemberId,
                    TreePath, WorkspacePolicy,
                };
                let bot = Grantee::bot("b1");
                let read = Grant::new(
                    "bot:b1:0",
                    bot,
                    TreePath::root()
                        .any(Level::Workspace)
                        .any(Level::App)
                        .kind("chat.workspace".to_owned()),
                    KindSet::READ,
                    Answer::Allow,
                    Grantee::person("ada"),
                );
                MemberAccess::person(WorkspacePolicy::new(WORKSPACE, "ada"), "ada")
                    .acting_as(Actor::Member(MemberId::bot("b1")), vec![read])
            },
        },
        Seed::Existing {
            record: page.document_id().clone(),
            changes: vec![doc.save()],
        },
    )
    .unwrap();
    // Ada's page posts through the chain of the chat's messages, which
    // starts its first part; the bot holds that part beside the chat
    // record.
    let post = serde_json::json!({ "channel": channel, "text": "hello" });
    page.invoke(
        &tilo_actions::api::ActionAddress::new(
            ui::resource::chain_resource_id(&chat::parts::chain()),
            "post",
        ),
        post.as_object().unwrap(),
        false,
    )
    .unwrap();
    let part = chat::parts::part(0);
    assert!(bot.keeps_beside(&part));
    let started = page.take_document_mutations().unwrap();
    assert!(
        matches!(&started[..], [(record, DocumentMutation::Snapshot(_))] if *record == part),
        "the page started the first part"
    );
    for (record, mutation) in &started {
        bot.apply_document(record, mutation).unwrap();
    }
    // The bot's host reads what the bot holds through the record API: the
    // chain joined, or its head, the newest part alone.
    let held = tilo_actions::remote::HeldCopies(
        std::iter::once(bot.document_id().clone())
            .chain(bot.resources_beside())
            .filter_map(|record| {
                let bytes = bot.read_resource(&record, |doc| doc.clone().save())?;
                Some((record.route().to_owned(), bytes))
            })
            .collect(),
    );
    let chain = chat::parts::chain().route().to_owned();
    for (record, joined) in [
        (chain.clone(), true),
        (tilo_actions::document_sync::chain_head(&chain), false),
    ] {
        let ask = tilo_actions::remote::ResourceAsk {
            address: tilo_actions::remote::ResourceAddress {
                app: "so.tilo.chat".into(),
                record,
            },
            joined,
        };
        let read = held
            .readable(&tilo_actions::remote::Caller::local_agent("bot:b1"), &[ask])
            .unwrap();
        let records = read["records"].as_array().unwrap();
        assert_eq!(records.len(), 1, "{read:#}");
        assert_eq!(records[0]["record"], part.route(), "{read:#}");
        let messages = records[0]["value"]["messages"].as_object().unwrap();
        assert!(
            messages
                .values()
                .any(|message| message["v"]["text"] == "hello"),
            "{read:#}"
        );
    }
}

#[test]
fn a_bot_granted_posting_is_offered_the_chains_post_and_starts_a_part() {
    let mut page = crate::common::open(
        "so.tilo.chat",
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let mut doc = page.read(Clone::clone).unwrap();
    crate::common::add_channel(&mut doc, "ada", "general", None);
    let channel = chat::projection::read(&doc).channel_rows()[0].id.clone();
    page.apply(&DocumentMutation::Snapshot(doc.save())).unwrap();
    // What a program that posts derives: posting on the chat's messages.
    let access = {
        use ui::permissions::policy::{
            Actor, Answer, Grant, Grantee, KindSet, Level, MemberAccess, MemberId, TreePath,
            WorkspacePolicy,
        };
        let bot = Grantee::bot("b1");
        let post = Grant::new(
            "bot:b1:0",
            bot,
            TreePath::root()
                .any(Level::Workspace)
                .app("so.tilo.chat".to_owned())
                .kind("chat.messages".to_owned())
                .any(Level::Resource)
                .rest()
                .action("post".to_owned()),
            KindSet::READ_UNDOABLE,
            Answer::Allow,
            Grantee::person("ada"),
        );
        MemberAccess::person(WorkspacePolicy::new(WORKSPACE, "ada"), "ada")
            .acting_as(Actor::Member(MemberId::bot("b1")), vec![post])
    };
    let mut bot = crate::common::open(
        page.app_id(),
        &MemberId::bot("b1"),
        ReplicaActor::Bot {
            installation: "b1".into(),
            installer: "ada".into(),
            access,
        },
        Seed::Existing {
            record: page.document_id().clone(),
            changes: vec![doc.save()],
        },
    )
    .unwrap();
    let offered = bot.survey();
    let post = offered
        .iter()
        .find(|action| action.address.action == "post" && action.resource_kind == "chat.messages")
        .unwrap_or_else(|| {
            panic!(
                "the chain's post is offered: {:?}",
                offered
                    .iter()
                    .map(|action| (&action.address, &action.resource_kind))
                    .collect::<Vec<_>>()
            )
        })
        .address
        .clone();
    let arguments = serde_json::json!({ "channel": channel, "text": "welcome!" });
    bot.invoke(&post, arguments.as_object().unwrap(), false)
        .unwrap();
    let started = bot.take_document_mutations().unwrap();
    assert!(
        matches!(&started[..], [(record, DocumentMutation::Snapshot(_))] if *record == chat::parts::part(0)),
        "the bot started the first part"
    );
    for (record, mutation) in &started {
        page.apply_document(record, mutation).unwrap();
    }
    let posted: Vec<(ui::tilo_resource::MemberId, String)> = page
        .read_resource(&chat::parts::part(0), chat::messages::read_part)
        .unwrap()
        .messages
        .iter()
        .map(|(_, message)| (message.author.clone(), message.text.clone()))
        .collect();
    assert_eq!(
        posted,
        [(
            ui::tilo_resource::MemberId::bot("b1"),
            "welcome!".to_owned()
        )]
    );
}

/// An agent acting for a person writes as the agent (its own actor is
/// the change's author) and the replica hands that change back to be saved:
/// what it wrote as the agent is its own, as what it writes as the person is.
#[test]
fn an_agent_acting_for_a_person_hands_back_what_it_wrote_as_the_agent() {
    let mut made = crate::common::open(
        "so.tilo.sheets",
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let Some(DocumentMutation::Snapshot(genesis)) = made.take_mutation().unwrap() else {
        panic!("a new record is published whole");
    };
    let access = ui::permissions::PermissionContext::member_of(
        WORKSPACE,
        &MemberId::person("ada"),
        ui::permissions::Role::Owner,
    )
    .access()
    .clone();
    let mut agent = crate::common::open(
        "so.tilo.sheets",
        &MemberId::person("ada"),
        ReplicaActor::Member {
            access: access
                .as_acting(
                    tilo_action_policy::Acting::handed_agent("Claude Code", "ada").in_session("s1"),
                )
                .unwrap(),
            sandbox: Vec::new(),
            unix_time: 1_900_000_000,
            agent_name: None,
            device: None,
        },
        Seed::Existing {
            record: made.document_id().clone(),
            changes: vec![genesis.clone()],
        },
    )
    .unwrap();
    let _ = agent.take_mutation();
    let new_sheet = agent
        .survey()
        .into_iter()
        .find(|descriptor| descriptor.address.action == "new-sheet")
        .expect("the workbook offers the agent a new sheet")
        .address;
    let arguments = serde_json::json!({"name": "Launch plan"});
    agent
        .invoke(&new_sheet, arguments.as_object().unwrap(), false)
        .unwrap();
    let Some(DocumentMutation::Incremental(wrote)) = agent.take_mutation().unwrap() else {
        panic!("the agent's sheet is handed back to be saved");
    };
    let mut doc = automerge::AutoCommit::new();
    tilo_automerge::ApplyIncoming::load_incoming(&mut doc, &genesis).unwrap();
    let before = doc.get_heads();
    tilo_automerge::ApplyIncoming::load_incoming(&mut doc, &wrote).unwrap();
    assert!(
        doc.get_changes(&before).iter().any(|change| {
            ui::history_source::authors::actor_handle(&change.actor_id().to_hex_string()).as_deref()
                == Some("agent:Claude Code")
        }),
        "written as the agent"
    );
    // Its session is on the change, so History names the session's writer beside the person.
    assert!(
        doc.get_changes(&before).iter().any(|change| {
            change
                .message()
                .and_then(tilo_actions::ActionContext::from_change_message)
                .and_then(|context| context.invoker)
                .is_some_and(|invoker| {
                    invoker
                        .acting
                        .agent_session
                        .as_ref()
                        .map(tilo_action_policy::AgentSessionId::as_str)
                        == Some("s1")
                        && invoker.acting.acted_for_person() == Some("ada")
                        // The delegation its person handed it, which the replica decided it
                        // by: every receiver decides the change by the same.
                        && invoker.acting.handed
                })
        }),
        "the agent's session and delegation are on its change"
    );
}

/// The agent the person invited, acting for them in two replicas of one
/// record: each opened from what the workspace kept, as a host opens one per
/// turn and again after a restart or a refused write. Each replica writes
/// under a writer of its own, so the workspace takes both writes: two
/// changes under one writer and sequence would leave the record unable to
/// hold both, and whoever read it whole after would refuse it.
#[test]
fn two_replicas_of_one_agent_never_write_one_sequence_twice() {
    let mut made = crate::common::open(
        "so.tilo.sheets",
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let Some(DocumentMutation::Snapshot(genesis)) = made.take_mutation().unwrap() else {
        panic!("a new record is published whole");
    };
    let access = ui::permissions::PermissionContext::member_of(
        WORKSPACE,
        &MemberId::person("ada"),
        ui::permissions::Role::Owner,
    )
    .access()
    .clone();
    let mut wrote = Vec::new();
    for title in ["Launch plan", "Retro"] {
        let mut agent = crate::common::open(
            "so.tilo.sheets",
            &MemberId::person("ada"),
            ReplicaActor::Member {
                access: access
                    .clone()
                    .as_acting(tilo_action_policy::Acting::handed_agent(
                        "Claude Code",
                        "ada",
                    ))
                    .unwrap(),
                sandbox: Vec::new(),
                unix_time: 1_900_000_000,
                agent_name: None,
                device: None,
            },
            Seed::Existing {
                record: made.document_id().clone(),
                changes: vec![genesis.clone()],
            },
        )
        .unwrap();
        let _ = agent.take_mutation();
        let new_sheet = agent
            .survey()
            .into_iter()
            .find(|descriptor| descriptor.address.action == "new-sheet")
            .expect("the workbook offers the agent a new sheet")
            .address;
        let arguments = serde_json::json!({ "name": title });
        agent
            .invoke(&new_sheet, arguments.as_object().unwrap(), false)
            .unwrap();
        let Some(DocumentMutation::Incremental(changes)) = agent.take_mutation().unwrap() else {
            panic!("the agent's sheet is handed back to be saved");
        };
        wrote.push(changes);
    }
    let mut doc = automerge::AutoCommit::new();
    tilo_automerge::ApplyIncoming::load_incoming(&mut doc, &genesis).unwrap();
    for changes in &wrote {
        tilo_automerge::ApplyIncoming::load_incoming(&mut doc, changes)
            .expect("the workspace takes each replica's write");
    }
    let changes = doc.get_changes(&[]);
    let sequences: std::collections::BTreeSet<_> = changes
        .iter()
        .map(|change| (change.actor_id().to_bytes().to_vec(), change.seq()))
        .collect();
    assert_eq!(
        sequences.len(),
        changes.len(),
        "one change per writer and sequence"
    );
}
