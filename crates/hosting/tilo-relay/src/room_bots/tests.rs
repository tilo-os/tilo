#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_moderation::{Case, JudgeError, Outcome, Verdict};

use super::*;
use crate::rooms::tests::{governor, restricted_until, view_read_relay};

const MALLORY: [u8; 32] = [7; 32];
const ADA: [u8; 32] = [8; 32];
/// The greeter's installation.
const GREETER: &str = "0b7e4c2a-9d1f-4e3b-8a6c-5f2d1e0c9b8a";

/// The root `governor()`'s device belongs to, which installs the room's bots.
fn owner_root() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[1; 32])
}

/// A host that decides what members and bots change against the surfaces
/// of the apps it runs, as a room host does.
async fn bot_host(
    state: &std::path::Path,
    key: &ed25519_dalek::SigningKey,
    cert: &DeviceCert,
) -> crate::Relay {
    // The native apps name their surfaces as the host first runs them.
    let _ = AppCatalogue::native();
    crate::surfaces::decide_receipt_from_registry("http://127.0.0.1:9");
    view_read_relay(state, key, cert).await
}

/// `bot`'s runner in `room`, the bot installed as a member first, as the
/// host does before it runs one.
async fn member_runner(
    rooms: &Rooms,
    room: &str,
    cert: &DeviceCert,
    bot: installed::InstalledBot,
    state: std::path::PathBuf,
) -> installed::BotRunner {
    let certifier = membership::HostRootCertifier::new(owner_root());
    membership::keep_member(rooms, room, cert, &certifier, &bot)
        .await
        .unwrap();
    installed::BotRunner::new(
        rooms.clone(),
        room.to_owned(),
        cert.clone(),
        certifier.device_key(&bot.installation),
        bot,
        std::collections::BTreeMap::new(),
        state,
    )
    .unwrap()
}

/// Restricts for half an hour and takes back what the person did.
struct FakeJudge;

impl Judge for FakeJudge {
    fn model(&self) -> &'static str {
        "fake-model"
    }

    async fn judge(&self, _case: &Case) -> Result<Verdict, JudgeError> {
        Ok(Verdict {
            outcome: Outcome::Restrict,
            minutes: Some(30),
            revert: true,
            reasoning: "They flooded the channel with the same link.".into(),
        })
    }
}

fn chat_of(bytes: &[u8]) -> chat::Chat {
    chat::Chat::read(&AutoCommit::load(bytes).unwrap()).unwrap()
}

fn texts_by(posts: &[chat::record::Message], person: [u8; 32]) -> Vec<String> {
    let person = hex::encode(person);
    posts
        .iter()
        .filter(|message| message.author.0 == person)
        .map(|message| message.text.clone())
        .collect()
}

/// The chat's first part of messages, as a record of the room.
fn first_part() -> String {
    chat::parts::part(0).as_str().to_owned()
}

/// The messages of the chat's first part, as the room holds it.
async fn posts(rooms: &Rooms, room: &str) -> Vec<chat::record::Message> {
    rooms
        .record_document(room, CHAT, &first_part())
        .await
        .unwrap()
        .map(|bytes| chat::messages::read_part(&AutoCommit::load(&bytes).unwrap()))
        .map(|part| part.messages.values().cloned().collect())
        .unwrap_or_default()
}

/// The message ids `page` holds in the parts beside its chat record.
fn message_ids(page: &Replica) -> Vec<String> {
    page.records_beside()
        .iter()
        .filter_map(|record| page.read_record(record, chat::messages::read_part))
        .flat_map(|part| {
            part.messages
                .ids()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Post `text` in `channel` of `room` as `author`, as their page does: a
/// view of the chat record and the parts of its messages, posting through
/// the chain into the head (starting the first part), its change carried
/// to the room on the part's own route. The new message's id.
async fn post_in(
    rooms: &Rooms,
    room: &str,
    cert: &DeviceCert,
    (author, channel, text): (&str, &str, &str),
) -> String {
    let catalogue = AppCatalogue::native();
    let chat = rooms
        .record_document(room, CHAT, "")
        .await
        .unwrap()
        .unwrap();
    let mut held = Vec::new();
    for (app, record) in rooms.record_routes(room).await.unwrap() {
        let Some(part) = ui::record::RecordId::parse(&record)
            .filter(|part| app == CHAT && chat::parts::part_index(part).is_some())
        else {
            continue;
        };
        let bytes = rooms
            .record_document(room, CHAT, &record)
            .await
            .unwrap()
            .unwrap();
        held.push((part, bytes));
    }
    // The page lives on this thread only; what it changed leaves with it.
    let (id, taken) = {
        let mut page = Replica::open(
            &catalogue,
            CHAT,
            room,
            author,
            ReplicaActor::Owner,
            Seed::Existing {
                record: ui::record::RecordId::package_primary(),
                changes: vec![chat],
            },
        )
        .unwrap();
        for (part, bytes) in held {
            page.apply_record(&part, &DocumentMutation::Snapshot(bytes))
                .unwrap();
        }
        let before = message_ids(&page);
        let arguments = serde_json::json!({ "channel": channel, "text": text });
        page.invoke(
            &tilo_actions::door::ActionAddress::new(
                ui::record::chain_resource_id(&chat::parts::chain()),
                "post",
            ),
            arguments.as_object().unwrap(),
            false,
        )
        .unwrap();
        let id = message_ids(&page)
            .into_iter()
            .find(|id| !before.contains(id))
            .expect("the post is in a part");
        (id, page.take_record_mutations())
    };
    for (record, mutation) in taken {
        match mutation {
            DocumentMutation::Snapshot(bytes) => rooms
                .author_room_record(room, cert, (CHAT, record.as_str()), |doc| {
                    tilo_automerge::ApplyIncoming::load_incoming(doc, &bytes)
                        .map(drop)
                        .map_err(|error| error.to_string())
                })
                .await
                .unwrap(),
            DocumentMutation::Incremental(bytes) => rooms
                .author_application(room, cert, &catalogue, CHAT, record.as_str(), |doc| {
                    tilo_automerge::ApplyIncoming::load_incoming(doc, &bytes)
                        .map(drop)
                        .map_err(|error| error.to_string())
                })
                .await
                .unwrap(),
        }
    }
    id
}

/// Ada and Mallory in `#general`, Mallory's three identical posts and
/// Ada's report of one.
fn flood(
    doc: &mut AutoCommit,
    ada: &str,
    mallory: &str,
) -> Result<String, tilo_actions::door::DoorError> {
    chat::demo::apply_as(
        doc,
        &hex::encode(governor().1.user_public_key),
        chat::Chat::CREATE_CHANNEL.with(chat::record::chat::CreateChannel {
            name: "general".into(),
            category: None,
            kind: None,
            topic: None,
        }),
    )?;
    chat::demo::introduce(doc, ada, "Ada", "AD")?;
    chat::demo::introduce(doc, mallory, "Mallory", "MA")?;
    let general = chat::projection::read(doc)
        .channel_rows()
        .into_iter()
        .find(|channel| channel.kind == chat::model::ChannelKind::Text)
        .expect("the chat has a text channel")
        .id;
    Ok(general)
}

/// Mallory's three identical posts in `general` beside Ada's, and Ada's
/// report of one.
async fn flood_posts(rooms: &Rooms, room: &str, cert: &DeviceCert, general: &str) {
    let (ada, mallory) = (hex::encode(ADA), hex::encode(MALLORY));
    post_in(rooms, room, cert, (&ada, general, "hello everyone")).await;
    let mut spam = String::new();
    for _ in 0..3 {
        spam = post_in(rooms, room, cert, (&mallory, general, "cheap links here")).await;
    }
    rooms
        .author_application(room, cert, &AppCatalogue::native(), CHAT, "", |doc| {
            chat::demo::apply_as(
                doc,
                &ada,
                chat::Chat::REPORT_MESSAGE.with(chat::record::chat::ReportMessage {
                    message: spam,
                    author: mallory,
                    reason: "spam".into(),
                }),
            )
            .map_err(|error| error.to_string())
        })
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the relay and its room stay live for the whole test"
)]
async fn a_report_filed_in_the_room_restricts_and_undoes_the_flood_as_the_bot() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = bot_host(scratch.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap().clone();
    let room = rooms.create("Plaza").await.unwrap().room;
    let catalogue = AppCatalogue::native();
    rooms
        .prepare_application(&room, &cert, &catalogue, CHAT, "")
        .await
        .unwrap();
    let (ada, mallory) = (hex::encode(ADA), hex::encode(MALLORY));
    let general = rooms
        .author_application(&room, &cert, &catalogue, CHAT, "", |doc| {
            flood(doc, &ada, &mallory).map_err(|error| error.to_string())
        })
        .await
        .unwrap();
    flood_posts(&rooms, &room, &cert, &general).await;

    let mut bot = ModerationBot::new(
        rooms.clone(),
        room.clone(),
        cert.clone(),
        ModerationAgent::new(FakeJudge),
    );
    bot.wake().await.unwrap();

    let posted = posts(&rooms, &room).await;
    assert!(
        texts_by(&posted, MALLORY).is_empty(),
        "the flood is taken back"
    );
    assert_eq!(texts_by(&posted, ADA), ["hello everyone"]);
    let after = chat_of(
        &rooms
            .record_document(&room, CHAT, "")
            .await
            .unwrap()
            .unwrap(),
    );
    let decision = after.moderation.decisions.iter().next().unwrap();
    assert_eq!(decision.person, mallory);
    assert_eq!(decision.outcome, Outcome::Restrict);
    assert_eq!(
        after.moderation.open_reports().count(),
        0,
        "the report is settled"
    );
    assert!(
        restricted_until(&rooms, &room, &cert, MALLORY)
            .await
            .is_some(),
        "Mallory is restricted through the room's governance"
    );
    // The bot's own changes file no report, so they do not wake it again.
    let cursor = bot.cursor.clone();
    bot.wake().await.unwrap();
    assert_eq!(bot.cursor, cursor);
}

/// A bot that answers every person's post in `channel`.
fn greeter(channel: &str) -> installed::InstalledBot {
    use tilo_automation::{ActorFilter, Binding, Selector, Shortcut, Step, StepKind, TriggerSpec};
    let trigger = TriggerSpec::RecordChanged {
        kind: "chat.messages".into(),
        actions: vec!["post".into()],
        fields: Vec::new(),
        by: ActorFilter::People,
    };
    let shortcut = Shortcut {
        id: "greeter".into(),
        name: "Greeter".into(),
        inputs: trigger.payload().to_vec(),
        steps: vec![Step::new(
            "answer",
            StepKind::Invoke {
                action: tilo_actions::ActionKey::new(CHAT, "chat.messages", "post"),
                resource: Selector::FirstOfKind("chat.messages".into()),
                args: [
                    ("channel".to_owned(), Binding::Literal(channel.into())),
                    ("text".to_owned(), Binding::Literal("welcome!".into())),
                ]
                .into_iter()
                .collect(),
                appearance: None,
            },
        )],
        output: None,
        grant: Vec::new(),
        triggers: vec![trigger],
        saved_by: None,
    };
    // Exactly what its steps and triggers need, as a room admits a bot.
    let shortcut = Shortcut {
        grant: tilo_automation::grant::derived_rules(
            &shortcut,
            &tilo_automation::ActionCatalog::new(Vec::new()),
        ),
        ..shortcut
    };
    installed::InstalledBot {
        installation: GREETER.into(),
        entry: "greeter".into(),
        name: "Greeter".into(),
        program: String::from_utf8(tilo_automation::program::canonical_bytes(&shortcut).unwrap())
            .unwrap(),
        inputs: serde_json::Map::new(),
        installed_at_ms: 1,
    }
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the relay and its room stay live for the whole test"
)]
async fn an_installed_bot_answers_a_post_in_the_room_once() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = bot_host(scratch.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap().clone();
    let room = rooms.create("Plaza").await.unwrap().room;
    let catalogue = AppCatalogue::native();
    rooms
        .prepare_application(&room, &cert, &catalogue, CHAT, "")
        .await
        .unwrap();
    let ada = hex::encode(ADA);
    let general = rooms
        .author_application(&room, &cert, &catalogue, CHAT, "", |doc| {
            chat::demo::apply_as(
                doc,
                &hex::encode(cert.user_public_key),
                chat::Chat::CREATE_CHANNEL.with(chat::record::chat::CreateChannel {
                    name: "general".into(),
                    category: None,
                    kind: None,
                    topic: None,
                }),
            )
            .map_err(|error| error.to_string())?;
            chat::demo::introduce(doc, &ada, "Ada", "AD").map_err(|error| error.to_string())?;
            Ok(chat::projection::read(doc)
                .channel_rows()
                .into_iter()
                .find(|channel| channel.kind == chat::model::ChannelKind::Text)
                .expect("the chat has a text channel")
                .id)
        })
        .await
        .unwrap();
    let mut runner = member_runner(
        &rooms,
        &room,
        &cert,
        greeter(general.as_str()),
        scratch.path().join("greeter.json"),
    )
    .await;
    // Installed now: what happened before is not answered.
    assert!(runner.step().await.unwrap().is_empty());
    assert_eq!(
        runner.room_input().await.unwrap()["members"],
        serde_json::json!([{"person": hex::encode(cert.user_public_key), "role": "owner"}]),
        "a program declaring `room` is told who is in it"
    );
    post_in(&rooms, &room, &cert, (&ada, &general, "hello")).await;
    let reports = runner.step().await.unwrap();
    assert_eq!(
        reports.len(),
        1,
        "{reports:?}, not run: {:?}",
        runner.dead_letters().collect::<Vec<_>>()
    );
    assert_eq!(
        reports[0].outcome,
        tilo_automation::RunOutcome::Completed,
        "{reports:?}"
    );
    // Its own post is a bot's, which the trigger does not count.
    assert!(runner.step().await.unwrap().is_empty());

    let posted = posts(&rooms, &room).await;
    let answers: Vec<_> = posted
        .iter()
        .filter(|message| message.text == "welcome!")
        .collect();
    assert_eq!(answers.len(), 1, "one answer to one post");
    assert_eq!(answers[0].author.0, format!("bot:{GREETER}"));
}

/// A room with the chat, Ada introduced, and `#general`'s id.
async fn chat_room(rooms: &Rooms, cert: &DeviceCert) -> (String, String) {
    let catalogue = AppCatalogue::native();
    let room = rooms.create("Plaza").await.unwrap().room;
    rooms
        .prepare_application(&room, cert, &catalogue, CHAT, "")
        .await
        .unwrap();
    let ada = hex::encode(ADA);
    let general = rooms
        .author_application(&room, cert, &catalogue, CHAT, "", |doc| {
            chat::demo::apply_as(
                doc,
                &hex::encode(cert.user_public_key),
                chat::Chat::CREATE_CHANNEL.with(chat::record::chat::CreateChannel {
                    name: "general".into(),
                    category: None,
                    kind: None,
                    topic: None,
                }),
            )
            .map_err(|error| error.to_string())?;
            chat::demo::introduce(doc, &ada, "Ada", "AD").map_err(|error| error.to_string())?;
            Ok(chat::projection::read(doc)
                .channel_rows()
                .into_iter()
                .find(|channel| channel.kind == chat::model::ChannelKind::Text)
                .expect("the chat has a text channel")
                .id)
        })
        .await
        .unwrap();
    (room, general)
}

async fn ada_says(rooms: &Rooms, room: &str, cert: &DeviceCert, channel: &str, text: &str) {
    let ada = hex::encode(ADA);
    post_in(rooms, room, cert, (&ada, channel, text)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restarted_runner_answers_what_was_posted_while_it_was_down() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = bot_host(scratch.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap().clone();
    let (room, general) = chat_room(&rooms, &cert).await;
    let state = scratch.path().join("greeter.json");
    let start = || {
        member_runner(
            &rooms,
            &room,
            &cert,
            greeter(general.as_str()),
            state.clone(),
        )
    };
    let mut runner = start().await;
    assert!(runner.step().await.unwrap().is_empty());
    drop(runner);

    ada_says(&rooms, &room, &cert, general.as_str(), "anyone here?").await;
    ada_says(&rooms, &room, &cert, general.as_str(), "hello?").await;

    let mut runner = start().await;
    let reports = runner.step().await.unwrap();
    assert_eq!(
        reports.len(),
        2,
        "both posts while down are answered: {reports:?}"
    );
    assert!(runner.step().await.unwrap().is_empty());
    drop(runner);
    // Restarting again repeats nothing.
    assert!(start().await.step().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the relay and its room stay live for the whole test"
)]
async fn a_run_that_fails_for_good_is_kept_as_a_dead_letter() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = bot_host(scratch.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap().clone();
    let (room, general) = chat_room(&rooms, &cert).await;
    let state = scratch.path().join("greeter.json");
    // Its answer goes to a channel the chat does not have.
    let start = || {
        member_runner(
            &rooms,
            &room,
            &cert,
            greeter("no-such-channel"),
            state.clone(),
        )
    };
    let mut runner = start().await;
    assert!(runner.step().await.unwrap().is_empty());
    ada_says(&rooms, &room, &cert, general.as_str(), "hello").await;
    let reports = runner.step().await.unwrap();
    assert!(
        matches!(reports[..], [ref report] if matches!(report.outcome, tilo_automation::RunOutcome::Failed { .. })),
        "{reports:?}"
    );
    drop(runner);

    let runner = start().await;
    let dead: Vec<_> = runner.dead_letters().collect();
    assert_eq!(dead.len(), 1, "kept across a restart");
    assert_eq!(dead[0].step.as_deref(), Some("answer"));
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the relay and its room stay live for the whole test"
)]
async fn the_host_installs_a_bot_as_a_member_and_renews_it_as_it_runs_out() {
    use membership::{DELEGATION_LIFETIME_NS, HostRootCertifier, Kept, keep_member};
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = bot_host(scratch.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap().clone();
    let (room, general) = chat_room(&rooms, &cert).await;
    let mut bot = greeter(general.as_str());
    bot.installation = "6f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6".into();

    // Installed with a delegation that is already running out.
    let short = HostRootCertifier::new(owner_root()).lasting(60 * 60 * 1_000_000_000);
    let (kept, _) = keep_member(&rooms, &room, &cert, &short, &bot)
        .await
        .unwrap();
    assert_eq!(kept, Kept::Installed);
    let row = rooms
        .installed_bot(&room, &bot.installation)
        .await
        .unwrap()
        .expect("the room records the bot");
    assert!(row.acting, "the bot acts in the room");
    assert_eq!(
        row.installation.installed_by,
        hex::encode(cert.user_public_key)
    );

    let certifier = HostRootCertifier::new(owner_root());
    let (kept, expires) = keep_member(&rooms, &room, &cert, &certifier, &bot)
        .await
        .unwrap();
    assert_eq!(kept, Kept::Renewed);
    let row = rooms
        .installed_bot(&room, &bot.installation)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.delegation_expires_at_ns, expires);
    assert!(expires > tilo_clock::unix_now_ns() + DELEGATION_LIFETIME_NS / 3 * 2);
    assert_eq!(
        keep_member(&rooms, &room, &cert, &certifier, &bot)
            .await
            .unwrap()
            .0,
        Kept::Current,
        "nothing is due on a fresh delegation"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the relay and its room stay live for the whole test"
)]
async fn a_bot_step_reads_through_a_records_registered_read() {
    use tilo_automation::{ActorFilter, Binding, Selector, Shortcut, Step, StepKind, TriggerSpec};
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = bot_host(scratch.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap().clone();
    let (room, general) = chat_room(&rooms, &cert).await;
    let trigger = TriggerSpec::RecordChanged {
        kind: "chat.messages".into(),
        actions: vec!["post".into()],
        fields: Vec::new(),
        by: ActorFilter::People,
    };
    let shortcut = Shortcut {
        id: "reader".into(),
        name: "Reader".into(),
        inputs: trigger.payload().to_vec(),
        steps: vec![Step::new(
            "digest",
            StepKind::Invoke {
                action: tilo_actions::ActionKey::new(CHAT, "chat.workspace", "read-room"),
                resource: Selector::FirstOfKind("chat.workspace".into()),
                args: std::collections::BTreeMap::new(),
                appearance: None,
            },
        )],
        output: None,
        grant: Vec::new(),
        triggers: vec![trigger],
        saved_by: None,
    };
    let shortcut = Shortcut {
        grant: tilo_automation::grant::derived_rules(
            &shortcut,
            &tilo_automation::ActionCatalog::new(Vec::new()),
        ),
        ..shortcut
    };
    let bot = installed::InstalledBot {
        installation: "3c9a7e10-5b2d-4f8e-a1c6-0d4e2b7f9a35".into(),
        entry: "reader".into(),
        name: "Reader".into(),
        program: String::from_utf8(tilo_automation::program::canonical_bytes(&shortcut).unwrap())
            .unwrap(),
        inputs: serde_json::Map::new(),
        installed_at_ms: 1,
    };
    let mut runner = member_runner(
        &rooms,
        &room,
        &cert,
        bot,
        scratch.path().join("reader.json"),
    )
    .await;
    assert!(runner.step().await.unwrap().is_empty());
    ada_says(&rooms, &room, &cert, &general, "what is here?").await;
    let reports = runner.step().await.unwrap();
    assert!(
        matches!(reports[..], [ref report] if report.outcome == tilo_automation::RunOutcome::Completed),
        "{reports:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the relay and its room stay live for the whole test"
)]
async fn a_bridge_posts_under_another_name_signed_by_its_own_device() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = bot_host(scratch.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap().clone();
    let (room, general) = chat_room(&rooms, &cert).await;
    let mut bridge = greeter(general.as_str());
    let mut shortcut = bridge.shortcut().unwrap();
    if let tilo_automation::StepKind::Invoke { appearance, .. } = &mut shortcut.steps[0].kind {
        *appearance = Some(tilo_automation::Binding::Literal(
            serde_json::json!({ "name": "Grace (Discord)" }),
        ));
    }
    bridge.program =
        String::from_utf8(tilo_automation::program::canonical_bytes(&shortcut).unwrap()).unwrap();
    let mut runner = member_runner(
        &rooms,
        &room,
        &cert,
        bridge,
        scratch.path().join("bridge.json"),
    )
    .await;
    assert!(runner.step().await.unwrap().is_empty());
    ada_says(&rooms, &room, &cert, general.as_str(), "hello").await;
    let reports = runner.step().await.unwrap();
    assert!(
        matches!(reports[..], [ref report] if report.outcome == tilo_automation::RunOutcome::Completed),
        "the room admits the bridge's post: {reports:?}"
    );
    let bytes = rooms
        .record_document(&room, CHAT, &first_part())
        .await
        .unwrap()
        .unwrap();
    let post = posts(&rooms, &room)
        .await
        .iter()
        .find(|message| message.text == "welcome!")
        .map(|message| message.author.0.clone())
        .expect("the bridge posted");
    assert_eq!(post, format!("bot:{GREETER}"), "still the bot's");
    let shown: Vec<_> = AutoCommit::load(&bytes)
        .unwrap()
        .get_changes(&[])
        .into_iter()
        .filter_map(|change| {
            change
                .message()
                .and_then(tilo_actions::ActionContext::from_change_message)
                .and_then(|context| context.invoker)
                .and_then(|invoker| invoker.appearance)
                .map(|appearance| appearance.name)
        })
        .collect();
    assert_eq!(shown, ["Grace (Discord)"], "the post shows the other name");
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the relay and its room stay live for the whole test"
)]
async fn a_refused_change_is_a_dead_letter_and_later_runs_still_land() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = bot_host(scratch.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap().clone();
    let (room, general) = chat_room(&rooms, &cert).await;
    let bot = greeter(general.as_str());
    let certifier = membership::HostRootCertifier::new(owner_root());
    // The room has it installed with no grant at all, so it refuses the
    // post this host's copy of the program lets the bot make.
    let mut granted_nothing = bot.clone();
    let mut program = bot.shortcut().unwrap();
    program.grant.clear();
    granted_nothing.program =
        String::from_utf8(tilo_automation::program::canonical_bytes(&program).unwrap()).unwrap();
    membership::keep_member(&rooms, &room, &cert, &certifier, &granted_nothing)
        .await
        .unwrap();
    let mut runner = installed::BotRunner::new(
        rooms.clone(),
        room.clone(),
        cert.clone(),
        certifier.device_key(&bot.installation),
        bot.clone(),
        std::collections::BTreeMap::new(),
        scratch.path().join("greeter.json"),
    )
    .unwrap();
    assert!(runner.step().await.unwrap().is_empty());
    ada_says(&rooms, &room, &cert, &general, "first").await;
    let reports = runner.step().await.unwrap();
    assert!(reports.is_empty(), "{reports:?}");
    let dead: Vec<_> = runner.dead_letters().collect();
    assert_eq!(dead.len(), 1, "the refusal is kept, not retried");
    assert!(
        dead[0]
            .reason
            .starts_with("the room refused the bot's change"),
        "{}",
        dead[0].reason
    );

    membership::keep_member(&rooms, &room, &cert, &certifier, &bot)
        .await
        .unwrap();
    ada_says(&rooms, &room, &cert, &general, "second").await;
    let reports = runner.step().await.unwrap();
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert_eq!(
        runner.dead_letters().count(),
        1,
        "{:?}",
        runner.dead_letters().collect::<Vec<_>>()
    );
    let answers = posts(&rooms, &room)
        .await
        .iter()
        .filter(|message| message.text == "welcome!")
        .count();
    assert_eq!(
        answers, 1,
        "the later answer lands; the refused one never does"
    );
}

#[test]
fn membership_changes_fire_a_bots_room_triggers_as_governance_verbs() {
    use crate::rooms::MemberMark;
    use tilo_automation::dispatch::{Armed, Event, Firer, dispatch};
    use tilo_automation::{ActorFilter, Shortcut, TriggerSpec};
    let mark = |role: &str, removed: bool, banned: bool| MemberMark {
        role: role.into(),
        removed,
        banned,
    };
    let before: std::collections::BTreeMap<String, MemberMark> = [
        ("ada".to_owned(), mark("editor", false, false)),
        ("bob".to_owned(), mark("editor", false, false)),
        ("cy".to_owned(), mark("viewer", false, false)),
        ("dee".to_owned(), mark("viewer", true, true)),
    ]
    .into_iter()
    .collect();
    let after = [
        ("ada".to_owned(), mark("moderator", false, false)),
        ("bob".to_owned(), mark("editor", true, true)),
        ("cy".to_owned(), mark("viewer", true, false)),
        ("dee".to_owned(), mark("viewer", true, false)),
        ("eve".to_owned(), mark("commenter", false, false)),
    ]
    .into_iter()
    .collect();
    let events = installed::governance_events("r1", "owner", 7, &before, &after);
    let seen: Vec<(String, String, Option<String>)> = events
        .iter()
        .map(|event| {
            let verb = tilo_automation::dispatch::verbs_of(&event.did)
                .pop()
                .unwrap();
            let person = event.arguments["person"].as_str().unwrap().to_owned();
            let role = event
                .arguments
                .get("role")
                .map(|role| role.as_str().unwrap().to_owned());
            (verb, person, role)
        })
        .collect();
    let row = |verb: &str, person: &str, role: Option<&str>| {
        (verb.to_owned(), person.to_owned(), role.map(str::to_owned))
    };
    assert_eq!(
        seen,
        [
            row("set-role", "ada", Some("moderator")),
            row("ban", "bob", None),
            row("remove-member", "cy", None),
            row("unban", "dee", None),
            row("add-member", "eve", Some("commenter")),
        ]
    );
    // The same membership seen again is no change.
    assert!(installed::governance_events("r1", "owner", 7, &after, &after).is_empty());

    // A program's room trigger fires on them, the person in the event.
    let trigger = TriggerSpec::RecordChanged {
        kind: "tilo.room".into(),
        actions: vec!["set-role".into(), "ban".into()],
        fields: Vec::new(),
        by: ActorFilter::Anyone,
    };
    let shortcut = Shortcut {
        id: "mirror".into(),
        name: "Mirror".into(),
        inputs: trigger.payload().to_vec(),
        steps: Vec::new(),
        output: None,
        grant: Vec::new(),
        triggers: vec![trigger],
        saved_by: None,
    };
    let armed = [Armed {
        firer: Firer::Bot { installation: "b1" },
        shortcut: &shortcut,
    }];
    let fired: Vec<_> = events
        .iter()
        .flat_map(|event| dispatch(&armed, Event::Record(event)).firings)
        .collect();
    assert_eq!(fired.len(), 2, "set-role and ban, not the others");
    assert_eq!(
        fired[1].inputs["event"].pointer("/arguments/person"),
        Some(&serde_json::json!("bob"))
    );
}

/// In a public room a node stewards, guests seal every chat post. A bot
/// (the Discord bridge, say) still answers in that chat: the room decides
/// its post as the bot's own, it reaches the chat sealed, and a guest
/// opens it.
#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the relay and its room stay live for the whole test"
)]
async fn a_bot_posts_into_a_sealed_chat_and_a_guest_reads_it() {
    use crate::rooms::sealed_records::tests::{SealingMember, governor_published, member_device};
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = crate::rooms::tests::governor_with(
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let relay = bot_host(scratch.path(), &key, &cert).await;
    let rooms = relay.rooms().unwrap().clone();
    let (room, general) = chat_room(&rooms, &cert).await;
    let (guest, guest_cert) = member_device(&rooms, &room, (&key, &cert), 3).await;
    let guest = SealingMember::join(&rooms, &room, (CHAT, ""), guest).await;
    // The chat's messages start in a part the guest's page starts sealed.
    let part = guest.part(&rooms, &room, &first_part()).await;
    let mut runner = member_runner(
        &rooms,
        &room,
        &cert,
        greeter(general.as_str()),
        scratch.path().join("greeter.json"),
    )
    .await;
    assert!(runner.step().await.unwrap().is_empty());

    // The guest's post leaves its page sealed, in the first part of the
    // chat's messages: those are sealed now.
    let started = {
        let chat = rooms
            .record_document(&room, CHAT, "")
            .await
            .unwrap()
            .unwrap();
        let mut page = Replica::open(
            &AppCatalogue::native(),
            CHAT,
            &room,
            &hex::encode(guest_cert.user_public_key),
            ReplicaActor::Owner,
            Seed::Existing {
                record: ui::record::RecordId::package_primary(),
                changes: vec![chat],
            },
        )
        .unwrap();
        let arguments = serde_json::json!({ "channel": general, "text": "hello" });
        page.invoke(
            &tilo_actions::door::ActionAddress::new(
                ui::record::chain_resource_id(&chat::parts::chain()),
                "post",
            ),
            arguments.as_object().unwrap(),
            false,
        )
        .unwrap();
        match page.take_record_mutations().pop() {
            Some((_, DocumentMutation::Snapshot(bytes))) => bytes,
            other => panic!("the page started the first part: {other:?}"),
        }
    };
    part.writes(&rooms, &room, |doc| {
        tilo_automerge::ApplyIncoming::load_incoming(doc, &started).unwrap();
    })
    .await;
    let reports = runner.step().await.unwrap();
    assert!(
        matches!(reports[..], [ref report] if report.outcome == tilo_automation::RunOutcome::Completed),
        "the room admits the bot's post to the sealed chat: {reports:?}"
    );

    // The guest opens what the room published, as its page does.
    let post = governor_published(&rooms, &room, &first_part(), &cert)
        .await
        .expect("the bot's post was published");
    assert!(post.encryption.is_some(), "never in the clear");
    part.take_in(&post);
    let _ = part.document();
    let posted = posts(&rooms, &room).await;
    let answers: Vec<_> = posted
        .iter()
        .filter(|message| message.text == "welcome!")
        .collect();
    assert_eq!(answers.len(), 1, "the guest reads the bot's answer");
    assert_eq!(answers[0].author.0, format!("bot:{GREETER}"));
    assert!(
        posted.iter().any(|message| message.text == "hello"),
        "beside Ada's post"
    );
}
