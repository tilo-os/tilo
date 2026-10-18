//! Two replicas of one record — a person's and an agent's — converge through
//! the changes each takes and the other applies, with no transport between
//! them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

mod common;

use collab_types::DocumentMutation;
use common::catalogue;
use tilo_agent_participant::{Replica, Seed};

const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

#[test]
fn read_only_resource_views_filter_roles_and_never_join_the_document_door() {
    use ui::permissions::{PermissionContext, Role};
    let catalogue = catalogue();
    let app = "so.tilo.chat";
    let mut page = Replica::open(
        &catalogue,
        app,
        ROOM,
        "ada",
        tilo_agent_participant::ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let mut doc = page.read(Clone::clone).unwrap();
    chat::demo::apply_as(
        &mut doc,
        "ada",
        chat::Chat::CREATE_CATEGORY.with(chat::record::chat::CreateCategory {
            name: "Resource view fixture".into(),
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
    let viewer = PermissionContext::member_of(ROOM, "ada", Role::Viewer);
    let denied = PermissionContext::stranger("ada");
    let views = page.resource_views(&resource, "chat.workspace.categories", &viewer);
    assert!(
        !views.is_empty(),
        "the actual Chat resource declares a viewer navigation door"
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
    // The door offers changes, never views: the record's document actions
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
    let catalogue = catalogue();
    let app = "so.tilo.scratchpad";
    let mut page = Replica::open(
        &catalogue,
        app,
        ROOM,
        "ada",
        tilo_agent_participant::ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let snapshot = page
        .take_mutation()
        .expect("a new record is published whole");
    assert!(matches!(snapshot, DocumentMutation::Snapshot(_)));
    let mut agent = Replica::open(
        &catalogue,
        app,
        ROOM,
        "writer",
        tilo_agent_participant::ReplicaActor::Agent,
        Seed::Existing {
            record: page.record_id().clone(),
            changes: vec![bytes(snapshot)],
        },
    )
    .unwrap();
    // Whatever mounting wrote, the person's mount takes it in.
    if let Some(opened) = agent.take_mutation() {
        page.apply(&opened).unwrap();
    }
    let (note, arguments) = common::scratchpad_body("Drafted by the agent");
    agent.invoke(&note, &arguments, false).unwrap();
    let edit = agent
        .take_mutation()
        .expect("the edit is the agent's own change");
    page.apply(&edit).unwrap();
    assert!(
        body(&page).contains("Drafted by the agent"),
        "{}",
        body(&page)
    );
    assert!(agent.take_mutation().is_none(), "nothing is taken twice");

    // And back: the person's edit reaches the agent, and the agent never
    // republishes it as its own.
    let (note, arguments) = common::scratchpad_body("Ada's revision");
    page.invoke(&note, &arguments, false).unwrap();
    agent.apply(&page.take_mutation().unwrap()).unwrap();
    assert!(body(&agent).contains("Ada's revision"), "{}", body(&agent));
    assert!(
        agent.take_mutation().is_none(),
        "a received change is not the agent's"
    );
}

/// The record's identity alone, as a participant holds it before the
/// creator's content arrives: the shortest prefix of the creator's history
/// that names the record and its schema.
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
                    ["home", "id", "kind", "room"]
                        .iter()
                        .all(|key| prefix.get(&record, *key).unwrap().is_some())
                });
            (named && prefix.get(ROOT, "tilo:schema_version").unwrap().is_some())
                .then(|| prefix.save())
        })
        .expect("the creator's history names the record")
}

fn joined(
    catalogue: &tilo_agent_participant::AppCatalogue,
    page: &Replica,
    changes: Vec<u8>,
) -> Option<Replica> {
    Replica::open(
        catalogue,
        page.app_id(),
        ROOM,
        "writer",
        tilo_agent_participant::ReplicaActor::Agent,
        Seed::Existing {
            record: page.record_id().clone(),
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

/// Only the record's creator writes its initial content: a participant that
/// joins, before or after that content arrives, adds nothing of its own, so
/// nobody's later edits land in a copy the merge hides.
#[test]
fn only_the_creator_seeds_a_record() {
    let catalogue = catalogue();
    let mut failures = Vec::new();
    // The room's own record (Chat's) is not handed over as a copy.
    for definition in tilo_agent_participant::component_apps()
        .into_iter()
        .filter(|definition| !definition.keeps_room_record())
    {
        let app = definition.app_id;
        let mut page = Replica::open(
            &catalogue,
            app,
            ROOM,
            "ada",
            tilo_agent_participant::ReplicaActor::Owner,
            Seed::New,
        )
        .unwrap();
        let snapshot = page.take_mutation().expect("a new record is published");
        let created = content(&page);

        // Joined after the content was published.
        let mut after = joined(&catalogue, &page, bytes(snapshot)).expect("the record opens");
        if let Some(opened) = after.take_mutation() {
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
        if let Some(opened) = early.take_mutation() {
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
    let catalogue = catalogue();
    let app = "so.tilo.word-processor";
    let mut agent = Replica::open(
        &catalogue,
        app,
        ROOM,
        "writer",
        tilo_agent_participant::ReplicaActor::Agent,
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
        tilo_browser_peer::transport::read_presence_anchors(&anchor, &head).unwrap();
    let last = agent
        .read(|doc| {
            let state = <word_processor::model::Document as ui::tilo_resource::Resource>::read(doc)
                .unwrap();
            let body = state.text().unwrap().body();
            body.id_at(body.len() - 1).unwrap().to_owned()
        })
        .unwrap();
    // The paragraph it wrote, as the record's own item: the key every
    // collaborator's Writer draws a collaborator's caret under (as its
    // digest, when the key outgrows the live plane's anchor).
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

fn bot_standing(kinds: ui::permissions::policy::KindSet) -> ui::permissions::policy::Standing {
    use ui::permissions::policy::{Answer, Grant, RoomPolicy, Standing, Subject, TreePath};
    let bot = Subject::bot("b1");
    let grant = Grant::new(
        "bot:b1",
        bot.clone(),
        TreePath::room(ROOM),
        kinds,
        Answer::Allow,
        Subject::person("ada"),
    );
    Standing::person(RoomPolicy::new(ROOM, "ada"), "ada").bounded_by(bot, vec![grant])
}

/// Ada's new scratchpad, and the same record held by her bot `b1` granted
/// `kinds` in the room.
fn ada_and_her_bot(kinds: ui::permissions::policy::KindSet) -> (Replica, Replica) {
    let mut page = Replica::open(
        &catalogue(),
        "so.tilo.scratchpad",
        ROOM,
        "ada",
        tilo_agent_participant::ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let snapshot = page.take_mutation().unwrap();
    let bot = Replica::open(
        &catalogue(),
        page.app_id(),
        ROOM,
        "bot:b1",
        tilo_agent_participant::ReplicaActor::Bot {
            installation: "b1".into(),
            installer: "ada".into(),
            standing: bot_standing(kinds),
        },
        Seed::Existing {
            record: page.record_id().clone(),
            changes: vec![bytes(snapshot)],
        },
    )
    .unwrap();
    (page, bot)
}

#[test]
fn a_bot_replica_edits_within_its_grant_for_its_installer() {
    let (mut page, mut bot) = ada_and_her_bot(ui::permissions::policy::KindSet::READ_UNDOABLE);
    if let Some(opened) = bot.take_mutation() {
        page.apply(&opened).unwrap();
    }
    let (note, arguments) = common::scratchpad_body("Mirrored by the bot");
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
    page.apply(&bot.take_mutation().unwrap()).unwrap();
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
    assert_eq!(invoker.actor, "bot:b1");
    assert_eq!(invoker.kind, tilo_actions::ActorKind::Bot);
    assert_eq!(invoker.on_behalf_of.as_deref(), Some("ada"));
    assert_eq!(entry.caused_by.as_ref().unwrap().event, "e1");
    assert_eq!(
        tilo_agent_participant::bot::door::RunStep::of(&entry),
        Some(tilo_agent_participant::bot::door::RunStep {
            run: "r1".into(),
            step: "0".into()
        })
    );
}

#[test]
fn a_bot_without_a_write_grant_is_offered_only_reads() {
    let (_page, mut bot) = ada_and_her_bot(ui::permissions::policy::KindSet::READ);
    let (note, arguments) = common::scratchpad_body("Not allowed");
    assert!(
        bot.survey().iter().all(|entry| entry.address != note),
        "an edit outside the bot's grant is not offered"
    );
    assert!(bot.invoke(&note, &arguments, false).is_err());
    assert!(!body(&bot).contains("Not allowed"));
}

#[test]
fn a_bot_reads_a_channels_latest_post_made_after_it_opened_the_record() {
    let catalogue = catalogue();
    let mut page = Replica::open(
        &catalogue,
        "so.tilo.chat",
        ROOM,
        "ada",
        tilo_agent_participant::ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let mut doc = page.read(Clone::clone).unwrap();
    chat::demo::apply_as(
        &mut doc,
        "ada",
        chat::Chat::CREATE_CHANNEL.with(chat::record::chat::CreateChannel {
            name: "general".into(),
            category: None,
            kind: None,
            topic: None,
        }),
    )
    .unwrap();
    let projection = chat::projection::read(&doc);
    let channel = projection.channel_rows()[0].id.clone();
    page.apply(&DocumentMutation::Snapshot(doc.save())).unwrap();
    let mut bot = Replica::open(
        &catalogue,
        page.app_id(),
        ROOM,
        "bot:b1",
        tilo_agent_participant::ReplicaActor::Bot {
            installation: "b1".into(),
            installer: "ada".into(),
            standing: {
                use ui::permissions::policy::{
                    Answer, Grant, KindSet, Level, RoomPolicy, Standing, Subject, TreePath,
                };
                let bot = Subject::bot("b1");
                let read = Grant::new(
                    "bot:b1:0",
                    bot.clone(),
                    TreePath::root()
                        .any(Level::Room)
                        .any(Level::App)
                        .kind("chat.workspace".to_owned()),
                    KindSet::READ,
                    Answer::Allow,
                    Subject::person("ada"),
                );
                Standing::person(RoomPolicy::new(ROOM, "ada"), "ada").bounded_by(bot, vec![read])
            },
        },
        Seed::Existing {
            record: page.record_id().clone(),
            changes: vec![doc.save()],
        },
    )
    .unwrap();
    // Ada's page posts through the chain of the chat's messages, which
    // starts its first part; the bot holds that part beside the chat
    // record.
    let post = serde_json::json!({ "channel": channel, "text": "hello" });
    page.invoke(
        &tilo_actions::door::ActionAddress::new(
            ui::record::chain_resource_id(&chat::parts::chain()),
            "post",
        ),
        post.as_object().unwrap(),
        false,
    )
    .unwrap();
    let part = chat::parts::part(0);
    assert!(bot.keeps_beside(&part));
    let started = page.take_record_mutations();
    assert!(
        matches!(&started[..], [(record, DocumentMutation::Snapshot(_))] if *record == part),
        "the page started the first part"
    );
    for (record, mutation) in &started {
        bot.apply_record(record, mutation).unwrap();
    }
    let read = bot
        .survey()
        .into_iter()
        .find(|action| action.address.action == "read-room")
        .expect("a bot is offered Chat's read");
    // A post names its channel, so a bot finds the message among the
    // channel's latest.
    let arguments = serde_json::json!({ "channel": channel });
    let output = bot
        .invoke_with(
            None,
            None,
            &read.address,
            arguments.as_object().unwrap(),
            false,
        )
        .unwrap();
    let value = output.value.unwrap();
    assert_eq!(
        value.pointer("/messages/0/value/text"),
        Some(&serde_json::json!("hello")),
        "{value:#}"
    );
}

#[test]
fn a_bot_granted_posting_is_offered_the_chains_post_and_starts_a_part() {
    let catalogue = catalogue();
    let mut page = Replica::open(
        &catalogue,
        "so.tilo.chat",
        ROOM,
        "ada",
        tilo_agent_participant::ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let mut doc = page.read(Clone::clone).unwrap();
    chat::demo::apply_as(
        &mut doc,
        "ada",
        chat::Chat::CREATE_CHANNEL.with(chat::record::chat::CreateChannel {
            name: "general".into(),
            category: None,
            kind: None,
            topic: None,
        }),
    )
    .unwrap();
    let channel = chat::projection::read(&doc).channel_rows()[0].id.clone();
    page.apply(&DocumentMutation::Snapshot(doc.save())).unwrap();
    // What a program that posts derives: posting on the chat's messages.
    let standing = {
        use ui::permissions::policy::{
            Answer, Grant, KindSet, Level, RoomPolicy, Standing, Subject, TreePath,
        };
        let bot = Subject::bot("b1");
        let post = Grant::new(
            "bot:b1:0",
            bot.clone(),
            TreePath::root()
                .any(Level::Room)
                .app("so.tilo.chat".to_owned())
                .kind("chat.messages".to_owned())
                .any(Level::Resource)
                .rest()
                .action("post".to_owned()),
            KindSet::READ_UNDOABLE,
            Answer::Allow,
            Subject::person("ada"),
        );
        Standing::person(RoomPolicy::new(ROOM, "ada"), "ada").bounded_by(bot, vec![post])
    };
    let mut bot = Replica::open(
        &catalogue,
        page.app_id(),
        ROOM,
        "bot:b1",
        tilo_agent_participant::ReplicaActor::Bot {
            installation: "b1".into(),
            installer: "ada".into(),
            standing,
        },
        Seed::Existing {
            record: page.record_id().clone(),
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
    let started = bot.take_record_mutations();
    assert!(
        matches!(&started[..], [(record, DocumentMutation::Snapshot(_))] if *record == chat::parts::part(0)),
        "the bot started the first part"
    );
    for (record, mutation) in &started {
        page.apply_record(record, mutation).unwrap();
    }
    let posted: Vec<(String, String)> = page
        .read_record(&chat::parts::part(0), chat::messages::read_part)
        .unwrap()
        .messages
        .iter()
        .map(|(_, message)| (message.author.0.clone(), message.text.clone()))
        .collect();
    assert_eq!(posted, [("bot:b1".to_owned(), "welcome!".to_owned())]);
}
