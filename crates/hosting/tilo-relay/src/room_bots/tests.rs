#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]
#![expect(
    clippy::significant_drop_tightening,
    reason = "every test here keeps its relay and workspace alive to the end"
)]

use tilo_moderation::{Case, JudgeError, Outcome, Verdict};

use super::*;
use crate::workspaces::fixtures::{governor, restricted_until, test_host, test_host_taking_part};

const MALLORY: [u8; 32] = [7; 32];
const ADA: [u8; 32] = [8; 32];
/// The greeter's installation.
const GREETER: &str = "0b7e4c2a-9d1f-4e3b-8a6c-5f2d1e0c9b8a";

/// The root `governor()`'s device belongs to, which installs the workspace's bots.
pub(super) fn owner_root() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[1; 32])
}

/// A host that decides what members and bots change against the surfaces
/// of the apps it runs, as a workspace host does.
pub(super) async fn bot_host(
    state: &std::path::Path,
    key: &ed25519_dalek::SigningKey,
    cert: &DeviceCert,
) -> crate::workspaces::fixtures::TestHost {
    // The native apps name their surfaces as the host first runs them.
    let _ = AppCatalogue::native();
    crate::surfaces::decide_receipt(&state.join("surfaces"));
    test_host(state, key, cert).await
}

/// The same host run on a device that takes part in its workspaces' channels,
/// as a relay whose device owns workspaces does.
async fn bot_host_taking_part(
    state: &std::path::Path,
    key: &ed25519_dalek::SigningKey,
    cert: &DeviceCert,
) -> crate::workspaces::fixtures::TestHost {
    let _ = AppCatalogue::native();
    crate::surfaces::decide_receipt(&state.join("surfaces"));
    test_host_taking_part(state, key, cert).await
}

/// `bot`'s runner in `workspace`, the bot installed as a member first, as the
/// host does before it runs one.
async fn member_runner(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &DeviceCert,
    bot: installed::InstalledBot,
    state: std::path::PathBuf,
) -> installed::BotRunner {
    let certifier = membership::HostRootCertifier::new(owner_root());
    membership::keep_member(workspaces, workspace, cert, &certifier, &bot)
        .await
        .unwrap();
    installed::BotRunner::new(
        workspaces.clone(),
        workspace.to_owned(),
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

/// The chat's first part of messages, as a record of the workspace.
fn first_part() -> String {
    chat::parts::part(0).as_str().to_owned()
}

/// The messages of the chat's first part, as the workspace holds it.
async fn posts(workspaces: &Workspaces, workspace: &str) -> Vec<chat::record::Message> {
    workspaces
        .record_document(workspace, CHAT, &first_part())
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

/// Post `text` in `channel` of `workspace` as `author`, as their page does: a
/// view of the chat record and the parts of its messages, posting through
/// the chain into the head (starting the first part), its change carried
/// to the workspace on the part's own route. The new message's id.
async fn post_in(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &DeviceCert,
    (author, channel, text): (&str, &str, &str),
) -> String {
    post_with(workspaces, workspace, cert, (author, channel, text), None).await
}

/// [`post_in`] with `attachment`, a file another action put in the store.
async fn post_with(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &DeviceCert,
    (author, channel, text): (&str, &str, &str),
    attachment: Option<serde_json::Value>,
) -> String {
    let catalogue = AppCatalogue::native();
    let chat = workspaces
        .record_document(workspace, CHAT, "")
        .await
        .unwrap()
        .unwrap();
    let mut held = Vec::new();
    for (app, record) in workspaces.record_routes(workspace).await.unwrap() {
        let Some(part) = ui::record::RecordId::parse(&record)
            .filter(|part| app == CHAT && chat::parts::part_index(part).is_some())
        else {
            continue;
        };
        let bytes = workspaces
            .record_document(workspace, CHAT, &record)
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
            workspace,
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
        let mut arguments = serde_json::json!({ "channel": channel, "text": text });
        if let Some(attachment) = attachment {
            arguments["attachments"] = serde_json::json!([attachment]);
        }
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
            DocumentMutation::Snapshot(bytes) => workspaces
                .author_room_record(workspace, cert, (CHAT, record.as_str()), |doc| {
                    tilo_automerge::ApplyIncoming::load_incoming(doc, &bytes)
                        .map(drop)
                        .map_err(|error| error.to_string())
                })
                .await
                .unwrap(),
            DocumentMutation::Incremental(bytes) => workspaces
                .author_application(workspace, cert, &catalogue, CHAT, record.as_str(), |doc| {
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
            name: "general".parse().unwrap(),
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
async fn flood_posts(workspaces: &Workspaces, workspace: &str, cert: &DeviceCert, general: &str) {
    let (ada, mallory) = (hex::encode(ADA), hex::encode(MALLORY));
    post_in(
        workspaces,
        workspace,
        cert,
        (&ada, general, "hello everyone"),
    )
    .await;
    let mut spam = String::new();
    for _ in 0..3 {
        spam = post_in(
            workspaces,
            workspace,
            cert,
            (&mallory, general, "cheap links here"),
        )
        .await;
    }
    workspaces
        .author_application(workspace, cert, &AppCatalogue::native(), CHAT, "", |doc| {
            chat::demo::apply_as(
                doc,
                &ada,
                chat::Chat::REPORT_MESSAGE.with(chat::record::chat::ReportMessage {
                    message: spam,
                    author: mallory,
                    reason: "spam".parse().unwrap(),
                }),
            )
            .map_err(|error| error.to_string())
        })
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_report_filed_in_the_room_restricts_and_undoes_the_flood_as_the_bot() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = bot_host(scratch.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
    let workspace = workspaces.create("Plaza").await.unwrap().workspace;
    let catalogue = AppCatalogue::native();
    workspaces
        .prepare_application(&workspace, &cert, &catalogue, CHAT, "")
        .await
        .unwrap();
    let (ada, mallory) = (hex::encode(ADA), hex::encode(MALLORY));
    let general = workspaces
        .author_application(&workspace, &cert, &catalogue, CHAT, "", |doc| {
            flood(doc, &ada, &mallory).map_err(|error| error.to_string())
        })
        .await
        .unwrap();
    flood_posts(&workspaces, &workspace, &cert, &general).await;

    let mut bot = ModerationBot::new(
        workspaces.clone(),
        workspace.clone(),
        cert.clone(),
        ModerationAgent::new(FakeJudge),
    );
    bot.wake().await.unwrap();

    let posted = posts(&workspaces, &workspace).await;
    assert!(
        texts_by(&posted, MALLORY).is_empty(),
        "the flood is taken back"
    );
    assert_eq!(texts_by(&posted, ADA), ["hello everyone"]);
    let after = chat_of(
        &workspaces
            .record_document(&workspace, CHAT, "")
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
        restricted_until(&workspaces, &workspace, &cert, MALLORY)
            .await
            .is_some(),
        "Mallory is restricted through the workspace's governance"
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
        step_timeout_secs: None,
        output: None,
        grant: Vec::new(),
        triggers: vec![trigger],
        saved_by: None,
        saved_proof: None,
    };
    // Exactly what its steps and triggers need, as a workspace admits a bot.
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
        release: String::new(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_installed_bot_answers_a_post_in_the_room_once() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = bot_host(scratch.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
    let workspace = workspaces.create("Plaza").await.unwrap().workspace;
    let catalogue = AppCatalogue::native();
    workspaces
        .prepare_application(&workspace, &cert, &catalogue, CHAT, "")
        .await
        .unwrap();
    let ada = hex::encode(ADA);
    let general = workspaces
        .author_application(&workspace, &cert, &catalogue, CHAT, "", |doc| {
            chat::demo::apply_as(
                doc,
                &hex::encode(cert.user_public_key),
                chat::Chat::CREATE_CHANNEL.with(chat::record::chat::CreateChannel {
                    name: "general".parse().unwrap(),
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
        &workspaces,
        &workspace,
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
        "a program declaring `workspace` is told who is in it"
    );
    post_in(&workspaces, &workspace, &cert, (&ada, &general, "hello")).await;
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

    let posted = posts(&workspaces, &workspace).await;
    let answers: Vec<_> = posted
        .iter()
        .filter(|message| message.text == "welcome!")
        .collect();
    assert_eq!(answers.len(), 1, "one answer to one post");
    assert_eq!(answers[0].author.0, format!("bot:{GREETER}"));
}

/// A workspace with the chat, Ada introduced, and `#general`'s id.
async fn chat_room(workspaces: &Workspaces, cert: &DeviceCert) -> (String, String) {
    let catalogue = AppCatalogue::native();
    let workspace = workspaces.create("Plaza").await.unwrap().workspace;
    workspaces
        .prepare_application(&workspace, cert, &catalogue, CHAT, "")
        .await
        .unwrap();
    let ada = hex::encode(ADA);
    let general = workspaces
        .author_application(&workspace, cert, &catalogue, CHAT, "", |doc| {
            chat::demo::apply_as(
                doc,
                &hex::encode(cert.user_public_key),
                chat::Chat::CREATE_CHANNEL.with(chat::record::chat::CreateChannel {
                    name: "general".parse().unwrap(),
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
    (workspace, general)
}

async fn ada_says(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &DeviceCert,
    channel: &str,
    text: &str,
) {
    let ada = hex::encode(ADA);
    post_in(workspaces, workspace, cert, (&ada, channel, text)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restarted_runner_answers_what_was_posted_while_it_was_down() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = bot_host(scratch.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
    let (workspace, general) = chat_room(&workspaces, &cert).await;
    let state = scratch.path().join("greeter.json");
    let start = || {
        member_runner(
            &workspaces,
            &workspace,
            &cert,
            greeter(general.as_str()),
            state.clone(),
        )
    };
    let mut runner = start().await;
    assert!(runner.step().await.unwrap().is_empty());
    drop(runner);

    ada_says(
        &workspaces,
        &workspace,
        &cert,
        general.as_str(),
        "anyone here?",
    )
    .await;
    ada_says(&workspaces, &workspace, &cert, general.as_str(), "hello?").await;

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
async fn a_run_that_fails_for_good_is_kept_as_a_dead_letter() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = bot_host(scratch.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
    let (workspace, general) = chat_room(&workspaces, &cert).await;
    let state = scratch.path().join("greeter.json");
    // Its answer goes to a channel the chat does not have.
    let start = || {
        member_runner(
            &workspaces,
            &workspace,
            &cert,
            greeter("no-such-channel"),
            state.clone(),
        )
    };
    let mut runner = start().await;
    assert!(runner.step().await.unwrap().is_empty());
    ada_says(&workspaces, &workspace, &cert, general.as_str(), "hello").await;
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
async fn the_host_installs_a_bot_as_a_member_and_renews_it_as_it_runs_out() {
    use membership::{DELEGATION_LIFETIME_NS, HostRootCertifier, Kept, keep_member};
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = bot_host(scratch.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
    let (workspace, general) = chat_room(&workspaces, &cert).await;
    let mut bot = greeter(general.as_str());
    bot.installation = "6f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6".into();

    // Installed with a delegation that is already running out.
    let short = HostRootCertifier::new(owner_root()).lasting(60 * 60 * 1_000_000_000);
    let (kept, _) = keep_member(&workspaces, &workspace, &cert, &short, &bot)
        .await
        .unwrap();
    assert_eq!(kept, Kept::Installed);
    let row = workspaces
        .installed_bot(&workspace, &bot.installation)
        .await
        .unwrap()
        .expect("the workspace records the bot");
    assert!(row.acting, "the bot acts in the workspace");
    assert_eq!(
        row.installation.installed_by,
        hex::encode(cert.user_public_key)
    );

    let certifier = HostRootCertifier::new(owner_root());
    let (kept, expires) = keep_member(&workspaces, &workspace, &cert, &certifier, &bot)
        .await
        .unwrap();
    assert_eq!(kept, Kept::Renewed);
    let row = workspaces
        .installed_bot(&workspace, &bot.installation)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.delegation_expires_at_ns, expires);
    assert!(expires > tilo_clock::unix_now_ns() + DELEGATION_LIFETIME_NS / 3 * 2);
    assert_eq!(
        keep_member(&workspaces, &workspace, &cert, &certifier, &bot)
            .await
            .unwrap()
            .0,
        Kept::Current,
        "nothing is due on a fresh delegation"
    );
}

/// A bot's run reads the records it follows through the record door: Chat's
/// record of the workspace and its messages' chain, with what was just posted.
#[tokio::test(flavor = "multi_thread")]
async fn a_bot_step_reads_the_records_it_follows_through_the_record_door() {
    use tilo_automation::{ActorFilter, Shortcut, Step, StepKind, TriggerSpec};
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = bot_host(scratch.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
    let (workspace, general) = chat_room(&workspaces, &cert).await;
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
        steps: vec![
            Step::new(
                "chat",
                StepKind::Read {
                    app: CHAT.into(),
                    record: String::new(),
                    joined: false,
                },
            ),
            Step::new(
                "messages",
                StepKind::Read {
                    app: CHAT.into(),
                    record: chat::parts::chain().route().to_owned(),
                    joined: true,
                },
            ),
        ],
        step_timeout_secs: None,
        output: None,
        grant: Vec::new(),
        triggers: vec![trigger],
        saved_by: None,
        saved_proof: None,
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
        release: String::new(),
    };
    let mut runner = member_runner(
        &workspaces,
        &workspace,
        &cert,
        bot,
        scratch.path().join("reader.json"),
    )
    .await;
    assert!(runner.step().await.unwrap().is_empty());
    ada_says(&workspaces, &workspace, &cert, &general, "what is here?").await;
    let reports = runner.step().await.unwrap();
    let [report] = &reports[..] else {
        panic!("one run: {reports:?}");
    };
    assert_eq!(
        report.outcome,
        tilo_automation::RunOutcome::Completed,
        "{report:?}"
    );
    let read = |step: &str| {
        report
            .steps
            .iter()
            .find_map(|record| match &record.status {
                tilo_automation::StepStatus::Succeeded { output } if record.step == step => {
                    output.clone()
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("{step} read nothing: {report:?}"))
    };
    let chat = read("chat");
    assert!(
        chat["records"][0]["value"]["channels"]
            .as_object()
            .is_some_and(|channels| channels.contains_key(&general)),
        "{chat}"
    );
    let messages = read("messages").to_string();
    assert!(messages.contains("what is here?"), "{messages}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bridge_posts_under_another_name_signed_by_its_own_device() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = bot_host(scratch.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
    let (workspace, general) = chat_room(&workspaces, &cert).await;
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
        &workspaces,
        &workspace,
        &cert,
        bridge,
        scratch.path().join("bridge.json"),
    )
    .await;
    assert!(runner.step().await.unwrap().is_empty());
    ada_says(&workspaces, &workspace, &cert, general.as_str(), "hello").await;
    let reports = runner.step().await.unwrap();
    assert!(
        matches!(reports[..], [ref report] if report.outcome == tilo_automation::RunOutcome::Completed),
        "the workspace admits the bridge's post: {reports:?}"
    );
    let bytes = workspaces
        .record_document(&workspace, CHAT, &first_part())
        .await
        .unwrap()
        .unwrap();
    let post = posts(&workspaces, &workspace)
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
async fn a_refused_change_runs_again_then_is_a_dead_letter_and_later_runs_still_land() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = bot_host(scratch.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
    let (workspace, general) = chat_room(&workspaces, &cert).await;
    let bot = greeter(general.as_str());
    let certifier = membership::HostRootCertifier::new(owner_root());
    // The workspace has it installed with no grant at all, so it refuses the
    // post this host's copy of the program lets the bot make.
    let mut granted_nothing = bot.clone();
    let mut program = bot.shortcut().unwrap();
    program.grant.clear();
    granted_nothing.program =
        String::from_utf8(tilo_automation::program::canonical_bytes(&program).unwrap()).unwrap();
    membership::keep_member(&workspaces, &workspace, &cert, &certifier, &granted_nothing)
        .await
        .unwrap();
    let mut runner = installed::BotRunner::new(
        workspaces.clone(),
        workspace.clone(),
        cert.clone(),
        certifier.device_key(&bot.installation),
        bot.clone(),
        std::collections::BTreeMap::new(),
        scratch.path().join("greeter.json"),
    )
    .unwrap();
    assert!(runner.step().await.unwrap().is_empty());
    ada_says(&workspaces, &workspace, &cert, &general, "first").await;
    let reports = runner.step().await.unwrap();
    assert!(reports.is_empty(), "{reports:?}");
    assert_eq!(runner.dead_letters().count(), 0, "a refused run goes again");
    // Each run again meets the same refusal, until its attempts are spent.
    for _ in 0..40 {
        if runner.dead_letters().count() > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        runner.step().await.unwrap();
    }
    let dead: Vec<_> = runner.dead_letters().collect();
    assert_eq!(dead.len(), 1, "the refusal is kept once its runs are spent");
    assert!(
        dead[0]
            .reason
            .starts_with("the workspace refused the bot's change to so.tilo.chat/"),
        "{}",
        dead[0].reason
    );

    membership::keep_member(&workspaces, &workspace, &cert, &certifier, &bot)
        .await
        .unwrap();
    ada_says(&workspaces, &workspace, &cert, &general, "second").await;
    let reports = runner.step().await.unwrap();
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert_eq!(
        runner.dead_letters().count(),
        1,
        "{:?}",
        runner.dead_letters().collect::<Vec<_>>()
    );
    let answers = posts(&workspaces, &workspace)
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
    use crate::workspaces::MemberMark;
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

    // A program's workspace trigger fires on them, the person in the event.
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
        step_timeout_secs: None,
        output: None,
        grant: Vec::new(),
        triggers: vec![trigger],
        saved_by: None,
        saved_proof: None,
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

/// In a public workspace whose channels a node takes part in, guests seal every chat post. A bot
/// (the Discord bridge, say) still answers in that chat: the workspace decides
/// its post as the bot's own, it reaches the chat sealed, and a guest
/// opens it.
#[tokio::test(flavor = "multi_thread")]
async fn a_bot_posts_into_a_sealed_chat_and_a_guest_reads_it() {
    use crate::workspaces::sealed_fixtures::{SealingMember, governor_published, member_device};
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = crate::workspaces::fixtures::governor_with(
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let relay = bot_host_taking_part(scratch.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
    let (workspace, general) = chat_room(&workspaces, &cert).await;
    let (guest, guest_cert) = member_device(&workspaces, &workspace, (&key, &cert), 3).await;
    let guest = SealingMember::join(&workspaces, &workspace, (CHAT, ""), guest).await;
    // The chat's messages start in a part the guest's page starts sealed.
    let part = guest.part(&workspaces, &workspace, &first_part()).await;
    let mut runner = member_runner(
        &workspaces,
        &workspace,
        &cert,
        greeter(general.as_str()),
        scratch.path().join("greeter.json"),
    )
    .await;
    assert!(runner.step().await.unwrap().is_empty());

    // The guest's post leaves its page sealed, in the first part of the
    // chat's messages: those are sealed now.
    let started = {
        let chat = workspaces
            .record_document(&workspace, CHAT, "")
            .await
            .unwrap()
            .unwrap();
        let mut page = Replica::open(
            &AppCatalogue::native(),
            CHAT,
            &workspace,
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
    part.writes(&workspaces, &workspace, |doc| {
        tilo_automerge::ApplyIncoming::load_incoming(doc, &started).unwrap();
    })
    .await;
    let reports = runner.step().await.unwrap();
    assert!(
        matches!(reports[..], [ref report] if report.outcome == tilo_automation::RunOutcome::Completed),
        "the workspace admits the bot's post to the sealed chat: {reports:?}"
    );

    // The guest opens what the workspace published, as its page does.
    let post = governor_published(&workspaces, &workspace, &first_part(), &cert)
        .await
        .expect("the bot's post was published");
    assert!(post.encryption.is_some(), "never in the clear");
    part.take_in(&post);
    let _ = part.document();
    let posted = posts(&workspaces, &workspace).await;
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

/// A member's own record is theirs alone: sealed under a channel only their
/// devices hold, it is nobody else's to read, the governor's included. A bot
/// whose pass follows the workspace's records leaves it out and goes on
/// answering what it does hold, rather than stopping at it every pass.
#[tokio::test(flavor = "multi_thread")]
async fn a_bot_pass_goes_on_past_a_record_it_holds_no_keys_of() {
    use crate::workspaces::sealed_fixtures::{SealingMember, member_device, plaintext_record};
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = crate::workspaces::fixtures::governor_with(
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let relay = bot_host_taking_part(scratch.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
    let (workspace, general) = chat_room(&workspaces, &cert).await;
    let (guest_key, _guest_cert) = member_device(&workspaces, &workspace, (&key, &cert), 3).await;
    let guest = SealingMember::join(&workspaces, &workspace, (CHAT, ""), guest_key).await;
    // The guest's own drafts: a record of the chat only their devices read.
    // One of their devices wrote it before its channel existed, and what
    // another of them seals onto it now the workspace keeps but cannot open.
    let drafts_id =
        tilo_action_policy::member_record_id(&hex::encode(cert.user_public_key), "chat.drafts");
    let (other_key, _other_cert) = member_device(&workspaces, &workspace, (&key, &cert), 4).await;
    plaintext_record(
        &workspaces,
        &workspace,
        &other_key,
        (CHAT, &drafts_id),
        |doc| {
            use automerge::transaction::Transactable as _;
            doc.put(automerge::ROOT, "drafts", "half a message")
                .unwrap();
        },
    )
    .await;
    guest
        .apart(&workspaces, &workspace, &drafts_id)
        .await
        .writes(&workspaces, &workspace, |doc| {
            use automerge::transaction::Transactable as _;
            doc.put(automerge::ROOT, "drafts", "half a message, sealed")
                .unwrap();
        })
        .await;
    let unread = workspaces
        .record_document(&workspace, CHAT, &drafts_id)
        .await;
    assert!(
        unread.is_err(),
        "the workspace holds a record it cannot read: {unread:?}"
    );

    // A post in the chat still reaches the bot, which answers it.
    let mut runner = member_runner(
        &workspaces,
        &workspace,
        &cert,
        greeter(general.as_str()),
        scratch.path().join("greeter.json"),
    )
    .await;
    assert!(runner.step().await.unwrap().is_empty());
    ada_says(&workspaces, &workspace, &cert, &general, "hello").await;
    let reports = runner.step().await.unwrap();
    assert!(
        matches!(reports[..], [ref report] if report.outcome == tilo_automation::RunOutcome::Completed),
        "the pass goes on past the record it holds no keys of: {reports:?}"
    );
    assert!(
        posts(&workspaces, &workspace)
            .await
            .iter()
            .any(|message| message.text == "welcome!"),
        "the bot answered the post beside it"
    );
    assert!(runner.dead_letters().next().is_none(), "nothing buried");
}

/// The files the chat's first part references, as the workspace holds it.
async fn referenced_in(workspaces: &Workspaces, workspace: &str) -> Vec<attachments::Referenced> {
    attachments::referenced(
        &workspaces
            .record_document(workspace, CHAT, &first_part())
            .await
            .unwrap()
            .unwrap(),
    )
}

/// The file `bytes` as a message's attachment.
fn file(name: &str, bytes: &[u8]) -> serde_json::Value {
    serde_json::json!({
        "kind": "file",
        "name": name,
        "blob": blake3::hash(bytes).to_hex().to_string(),
        "size": bytes.len(),
    })
}

/// The one piece `device` answers `want` with when it keeps `bytes`, as a
/// member's device answers.
fn answer(want: &[u8], device: [u8; 32], bytes: &[u8]) -> Vec<u8> {
    let neighbour_wire::NeighbourMsg::RecordContentWant {
        requester,
        app_id,
        cluster_id,
        hash,
        ..
    } = postcard::from_bytes(want).unwrap()
    else {
        panic!("a request for content")
    };
    postcard::to_allocvec(&neighbour_wire::NeighbourMsg::RecordContentPiece {
        responder: tilo_peer_state::peer_handle(&device),
        requester,
        app_id,
        cluster_id,
        hash,
        total: u64::try_from(bytes.len()).unwrap(),
        offset: 0,
        bytes: bytes.to_vec(),
    })
    .unwrap()
}

/// What the workspace answers `device` asking it for `bytes`, assembled.
fn opened(
    workspaces: &Workspaces,
    workspace: &str,
    device: [u8; 32],
    bytes: &[u8],
) -> Option<Vec<u8>> {
    let want = postcard::to_allocvec(&neighbour_wire::NeighbourMsg::RecordContentWant {
        requester: tilo_peer_state::peer_handle(&device),
        app_id: CHAT.into(),
        cluster_id: workspace.into(),
        hash: blake3::hash(bytes).to_hex().to_string(),
        have: Vec::new(),
    })
    .unwrap();
    let mut whole = Vec::new();
    for frame in workspaces.content_answer(workspace, &want) {
        match postcard::from_bytes(&frame).unwrap() {
            neighbour_wire::NeighbourMsg::RecordContentPiece { bytes, .. } => whole.extend(bytes),
            neighbour_wire::NeighbourMsg::RecordContentLacks { .. } => return None,
            _ => panic!("a piece of content, or word that the host keeps none"),
        }
    }
    (!whole.is_empty()).then_some(whole)
}

/// Ada posts a file in the workspace's Chat. While her device is online it
/// answers the host's request for it; once she left, Bo opens the file
/// from the host's copy. Past Ada's quota her oldest file goes, and a file
/// over the cap is not asked for at all.
#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "one story: posting, leaving, the host's copy, the quota and the cap"
)]
async fn a_file_a_member_posted_stays_in_the_room_after_they_leave() {
    const BO: [u8; 32] = [9; 32];
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = bot_host(scratch.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
    let (workspace, general) = chat_room(&workspaces, &cert).await;
    let content = crate::content::stores(scratch.path());
    workspaces.serve_content(std::sync::Arc::new(crate::content::StoresContent(
        content.clone(),
    )));
    let blobs = content.get(&workspace).unwrap();
    let attachments_of = |limits| {
        attachments::WorkspaceAttachments::new(
            workspaces.clone(),
            workspace.clone(),
            cert.device_public_key,
            blobs.clone(),
            scratch.path().join("attachments.json"),
            limits,
        )
    };
    let keeper = attachments_of(attachments::AttachmentLimits {
        file_bytes: 64,
        member_bytes: 40,
        workspace_bytes: 1000,
    });
    let kept: std::sync::Arc<dyn crate::workspaces::ContentKeeper> = keeper.clone();
    workspaces.keep_content(&kept);
    let ada = hex::encode(ADA);

    let notes = b"the meeting notes, version one".to_vec();
    post_with(
        &workspaces,
        &workspace,
        &cert,
        (&ada, &general, "notes"),
        Some(file("notes.txt", &notes)),
    )
    .await;
    let asks = keeper.asks(&referenced_in(&workspaces, &workspace).await, 0);
    assert_eq!(
        asks.len(),
        1,
        "the host asks the workspace for the file it lacks"
    );
    assert!(
        keeper
            .asks(&referenced_in(&workspaces, &workspace).await, 1)
            .is_empty(),
        "and only once a while"
    );
    // Ada's device answers the host; then she leaves.
    workspaces.content_for_host(&workspace, ADA, &answer(&asks[0], ADA, &notes));
    assert_eq!(
        keeper.kept_for(&blake3::hash(&notes).to_hex()).as_deref(),
        Some(ada.as_str())
    );
    assert_eq!(
        opened(&workspaces, &workspace, BO, &notes),
        Some(notes.clone()),
        "Bo opens it from the host"
    );

    // A second file takes Ada past her quota: her oldest goes.
    let slides = b"the slides that came after them".to_vec();
    post_with(
        &workspaces,
        &workspace,
        &cert,
        (&ada, &general, "slides"),
        Some(file("slides.txt", &slides)),
    )
    .await;
    assert!(
        keeper
            .asks(&referenced_in(&workspaces, &workspace).await, 2)
            .is_empty(),
        "within the hour her quota has no workspace for it"
    );
    let an_hour_later = 3_600_000 + 2;
    let asks = keeper.asks(&referenced_in(&workspaces, &workspace).await, an_hour_later);
    assert_eq!(
        asks.len(),
        1,
        "an hour later only the new file is asked for"
    );
    workspaces.content_for_host(&workspace, ADA, &answer(&asks[0], ADA, &slides));
    assert_eq!(
        opened(&workspaces, &workspace, BO, &slides),
        Some(slides.clone())
    );
    assert_eq!(
        opened(&workspaces, &workspace, BO, &notes),
        None,
        "the oldest went past her quota"
    );

    // A file over the cap stays with its poster.
    let video = vec![7_u8; 65];
    post_with(
        &workspaces,
        &workspace,
        &cert,
        (&ada, &general, "a video"),
        Some(file("big.bin", &video)),
    )
    .await;
    assert!(
        keeper
            .asks(
                &referenced_in(&workspaces, &workspace).await,
                an_hour_later + 1
            )
            .is_empty()
    );

    // A restart keeps counting what it kept.
    drop(keeper);
    let again = attachments_of(attachments::AttachmentLimits::default());
    assert_eq!(
        again.kept_for(&blake3::hash(&slides).to_hex()).as_deref(),
        Some(ada.as_str())
    );
    assert_eq!(again.kept_for(&blake3::hash(&notes).to_hex()), None);
}

/// The Discord bridge as the host installs it, its program as shipped.
pub(super) fn discord_bridge() -> installed::InstalledBot {
    installed::InstalledBot {
        installation: "5b8d2278-de54-4bf8-a0b8-8ae4f0587eed".into(),
        entry: "discord-bridge".into(),
        name: "Discord Bridge".into(),
        program: include_str!("../../../../../apps/discord/bridge/program.json").to_owned(),
        inputs: serde_json::Map::new(),
        installed_at_ms: 1,
        release: String::new(),
    }
}

/// The workspace's Chat categories by name, each with its channels' names.
async fn chat_layout(workspaces: &Workspaces, workspace: &str) -> Vec<(String, Vec<String>)> {
    let record = workspace_record(&AppCatalogue::native(), CHAT);
    let saved = workspaces
        .record_document(workspace, CHAT, &record)
        .await
        .unwrap()
        .unwrap();
    let chat = chat_of(&saved);
    let named = |category: Option<&tilo_resource::Id>| {
        chat.channels
            .values()
            .filter(|channel| channel.is_channel())
            .filter(|channel| channel.category.as_ref() == category)
            .map(|channel| channel.name.clone())
            .collect::<Vec<_>>()
    };
    std::iter::once((String::new(), named(None)))
        .chain(
            chat.categories
                .iter()
                .map(|(id, category)| (category.name.clone(), named(Some(id)))),
        )
        .collect()
}

/// Connect `workspace` to the Discord server the bridge last saw as `goal`, and
/// `chat` holding `general` and `voice`.
async fn bridged_lounge(workspaces: &Workspaces, workspace: &str, cert: &DeviceCert) {
    use tilo_resource::Fields as _;
    let link = discord::DiscordLink {
        guild_id: "1000".into(),
        only_channel: None,
        name: "Fake".into(),
        mirror_channels: false,
        map_roles: false,
    };
    discord::connect(workspaces, workspace, cert, &link)
        .await
        .unwrap();
    let catalogue = AppCatalogue::native();
    // The server as the bridge last saw it: `goal`, and `chat` holding
    // `general` and `voice`.
    workspaces
        .author_application(
            workspace,
            cert,
            &catalogue,
            discord::APP,
            &workspace_record(&catalogue, discord::APP),
            |doc| {
                let current = ::discord::Guild::read_fields(doc, &automerge::ROOT)
                    .map_err(|error| error.to_string())?;
                let mut seen = current.clone();
                let channel = |id: &str, kind: &str, name: &str, parent: Option<&str>, at: i64| {
                    serde_json::json!({"id": id, "kind": kind, "name": name, "parent": parent, "position": at})
                };
                seen.observe(
                    "2000".into(),
                    serde_json::from_value(serde_json::json!([
                        channel("1103", "text", "goal", None, 0),
                        channel("1100", "category", "chat", None, 1),
                        channel("1101", "text", "general", Some("1100"), 2),
                        channel("1102", "voice", "voice", Some("1100"), 3),
                    ]))
                    .map_err(|error| error.to_string())?,
                    Vec::new(),
                );
                seen.write_changed_fields(
                    &current,
                    doc,
                    &automerge::ROOT,
                    &tilo_resource::Delta::default(),
                )
                .map_err(|error| error.to_string())
            },
        )
        .await
        .unwrap();
}

/// A bridge run the workspace refuses part of leaves the import half done: the
/// category it made stands, the moves after it were dropped. The run goes
/// again from the workspace's records and finishes the layout, the channels
/// under the server's category as in Discord.
#[tokio::test(flavor = "multi_thread")]
async fn a_bridge_run_the_room_refuses_part_of_finishes_the_import_when_it_goes_again() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = bot_host(scratch.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
    let workspace = workspaces.create("Lounge").await.unwrap().workspace;
    bridged_lounge(&workspaces, &workspace, &cert).await;
    let catalogue = AppCatalogue::native();
    let mut runner = member_runner(
        &workspaces,
        &workspace,
        &cert,
        discord_bridge(),
        scratch.path().join("bridge.json"),
    )
    .await;
    assert!(runner.step().await.unwrap().is_empty());

    // A change to the workspace's Chat has the bridge reconcile; the workspace
    // refuses the run's second change, mapping the category it just made.
    let owner = hex::encode(cert.user_public_key);
    workspaces
        .author_application(
            &workspace,
            &cert,
            &catalogue,
            CHAT,
            &workspace_record(&catalogue, CHAT),
            |doc| {
                chat::demo::apply_as(
                    doc,
                    &owner,
                    chat::Chat::CREATE_CHANNEL.with(chat::record::chat::CreateChannel {
                        name: "welcome".parse().unwrap(),
                        category: None,
                        kind: None,
                        topic: None,
                    }),
                )
                .map_err(|error| error.to_string())
            },
        )
        .await
        .unwrap();
    runner.refuse_change_after(1);
    runner.step().await.unwrap();
    let half = chat_layout(&workspaces, &workspace).await;
    assert!(
        half.iter()
            .any(|(name, channels)| name == "chat" && channels.is_empty()),
        "the category stands, empty: {half:?}"
    );

    // It goes again, from the workspace's records, and finishes.
    let done = |layout: &[(String, Vec<String>)]| {
        layout.iter().any(|(name, channels)| {
            name == "chat"
                && channels.contains(&"general".to_owned())
                && channels.contains(&"voice".to_owned())
        })
    };
    for _ in 0..40 {
        if done(&chat_layout(&workspaces, &workspace).await) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        runner.step().await.unwrap();
    }
    let layout = chat_layout(&workspaces, &workspace).await;
    assert!(done(&layout), "general and voice under chat: {layout:?}");
    assert_eq!(
        layout.iter().filter(|(name, _)| name == "chat").count(),
        1,
        "one chat category, the one the refused run made: {layout:?}"
    );
    assert!(
        layout[0].1.contains(&"goal".to_owned()),
        "goal stands apart, as in Discord: {layout:?}"
    );
}

/// The image ships the Discord bridge as a program file; installing from it
/// keeps the path as where the bot came from, and the program's own name as
/// its name, rather than naming the bot after the path.
#[tokio::test(flavor = "multi_thread")]
async fn a_bot_installed_from_a_program_file_is_named_for_itself_not_its_path() {
    let scratch = tempfile::tempdir().unwrap();
    let program = scratch.path().join("discord-bridge.json");
    std::fs::write(
        &program,
        include_str!("../../../../../apps/discord/bridge/program.json"),
    )
    .unwrap();
    let file = program.to_str().unwrap();
    let installed = installed::install(&scratch.path().join("bot"), file, serde_json::Map::new())
        .await
        .unwrap();
    assert_eq!(installed.entry, file, "the program file it came from");
    assert_eq!(installed.name, "Discord Bridge", "the program's own name");

    // Kept, and read back unchanged: a restart installs nothing new.
    let kept = installed::install(&scratch.path().join("bot"), file, serde_json::Map::new())
        .await
        .unwrap();
    assert_eq!(kept, installed);
}

/// A workspace installed from an older image keeps its bot when the host starts
/// on a newer one, and the bot runs the program the newer image ships: a
/// grant the older program lacked (reaching outside when the bridge settles
/// an outbox entry) reaches the workspace's row, so the workspace stops refusing the
/// bot's settles.
#[tokio::test(flavor = "multi_thread")]
async fn a_program_file_bot_runs_the_program_its_host_ships_now() {
    use membership::{HostRootCertifier, Kept, keep_member};
    let shipped = include_str!("../../../../../apps/discord/bridge/program.json");
    let settle = "resource:*/**/item:*/**/action:field-edit";
    // The bridge as an older image shipped it: settling read and undoable only.
    let mut older: serde_json::Value = serde_json::from_str(shipped).unwrap();
    let grant = older["grant"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|grant| grant["node"].as_str().unwrap().ends_with(settle))
        .expect("the bridge grants settling its outbox entries");
    grant["kinds"] = serde_json::json!(["read", "undoable"]);

    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = bot_host(scratch.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
    let (workspace, _) = chat_room(&workspaces, &cert).await;
    let file = scratch.path().join("discord-bridge.json");
    let entry = file.to_str().unwrap();
    let dir = scratch.path().join("bot");
    let certifier = HostRootCertifier::new(owner_root());

    std::fs::write(&file, serde_json::to_vec(&older).unwrap()).unwrap();
    let before = installed::install(&dir, entry, serde_json::Map::new())
        .await
        .unwrap();
    let (kept, _) = keep_member(&workspaces, &workspace, &cert, &certifier, &before)
        .await
        .unwrap();
    assert_eq!(kept, Kept::Installed);

    // The newer image ships the bridge as it is now.
    std::fs::write(&file, shipped).unwrap();
    let after = installed::install(&dir, entry, serde_json::Map::new())
        .await
        .unwrap();
    assert_eq!(after.installation, before.installation, "the same bot");
    assert_eq!(after.installed_at_ms, before.installed_at_ms);
    assert_ne!(after.program, before.program, "it runs the shipped program");
    let grants = &after.shortcut().unwrap().grant;
    assert!(
        grants
            .iter()
            .any(|rule| rule.node.to_string().ends_with(settle)
                && rule.kinds.contains(tilo_action_policy::ActionKind::Outside)),
        "the shipped bridge may reach outside as it settles: {grants:?}"
    );
    let (kept, _) = keep_member(&workspaces, &workspace, &cert, &certifier, &after)
        .await
        .unwrap();
    assert_eq!(
        kept,
        Kept::Updated,
        "the workspace's row follows the program"
    );
    let row = workspaces
        .installed_bot(&workspace, &after.installation)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.installation.program,
        tilo_automation::program::program_hash(after.program.as_bytes())
    );
    // Read back on the next start: nothing changes when the file did not.
    let again = installed::install(&dir, entry, serde_json::Map::new())
        .await
        .unwrap();
    assert_eq!(again, after);
}

/// Ada posts until the bot answers with `text`: a runner that just started
/// answers only what is posted after its first look.
async fn answered(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &DeviceCert,
    general: &str,
    text: &str,
) {
    tilo_test_env::within(format!("the bot answers {text}"), async {
        loop {
            ada_says(workspaces, workspace, cert, general, "hello").await;
            for _ in 0..20 {
                if posts(workspaces, workspace)
                    .await
                    .iter()
                    .any(|post| post.text == text)
                {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    })
    .await;
}

/// A developer pushes their bot's program into a running workspace, then a
/// changed one: the workspace's host runs each as soon as the workspace asks for it,
/// without restarting, and the second is the same bot (its installation and
/// what it read up to) running the new program under its new grants.
#[tokio::test(flavor = "multi_thread")]
async fn a_bot_program_pushed_into_a_running_room_replaces_the_one_it_runs() {
    use tilo_peer_state::workspace_membership::{BotRequest, WorkspaceMembershipCommand};
    const ENTRY: &str = "dev.ada.greeter";
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = bot_host(scratch.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
    let (workspace, general) = chat_room(&workspaces, &cert).await;
    let first = greeter(general.as_str()).program;
    let second = first.replace("welcome!", "welcome, again!");
    assert_ne!(first, second);
    let follower = requested::Follower {
        workspaces: workspaces.clone(),
        workspace: workspace.clone(),
        host: cert.clone(),
        root: owner_root(),
        state: scratch.path().join("host"),
        operator: std::collections::BTreeSet::new(),
        connectors: None,
    };
    let push = |version: &str, program: &str| {
        let command = WorkspaceMembershipCommand::RequestBot(BotRequest {
            entry: ENTRY.into(),
            version: version.into(),
            inputs: std::collections::BTreeMap::new(),
            program: Some(program.to_owned()),
        });
        let workspaces = workspaces.clone();
        let (workspace, cert) = (workspace.clone(), cert.clone());
        let id = *blake3::hash(program.as_bytes()).as_bytes();
        async move {
            workspaces
                .ask_for_bot(&workspace, &cert, id, command)
                .await
                .unwrap()
        }
    };
    let row_program = |installation: String| {
        let workspaces = workspaces.clone();
        let workspace = workspace.clone();
        async move {
            workspaces
                .installed_bot(&workspace, &installation)
                .await
                .unwrap()
                .expect("the bot is on the workspace's roster")
                .installation
                .program
        }
    };
    let welcomes = |workspaces: Workspaces, workspace: String| async move {
        posts(&workspaces, &workspace)
            .await
            .iter()
            .filter(|post| post.text == "welcome!")
            .count()
    };

    let mut known = requested::Followed::default();
    push("0.1.0-dev.1", &first).await;
    follower.follow_requests(&mut known).await;
    let (installation, _) = known.running(ENTRY).expect("the pushed bot runs");
    let installation = installation.to_owned();
    assert_eq!(
        row_program(installation.clone()).await,
        tilo_automation::program::program_hash(first.as_bytes())
    );
    answered(&workspaces, &workspace, &cert, general.as_str(), "welcome!").await;
    let first_answers = welcomes(workspaces.clone(), workspace.clone()).await;

    push("0.1.0-dev.2", &second).await;
    follower.follow_requests(&mut known).await;
    let (now, release) = known.running(ENTRY).expect("the bot still runs");
    assert_eq!(now, installation, "the same bot, not a new one");
    assert!(release.starts_with("0.1.0-dev.2#"), "{release}");
    assert_eq!(
        row_program(installation.clone()).await,
        tilo_automation::program::program_hash(second.as_bytes()),
        "the workspace's row names the program it runs now"
    );
    answered(
        &workspaces,
        &workspace,
        &cert,
        general.as_str(),
        "welcome, again!",
    )
    .await;
    assert_eq!(
        welcomes(workspaces.clone(), workspace.clone()).await,
        first_answers,
        "the first program answers nothing once replaced"
    );
}
