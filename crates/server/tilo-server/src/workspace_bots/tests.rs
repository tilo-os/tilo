#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_moderation::{Case, JudgeError, Outcome, Verdict};

use super::*;
use crate::workspaces::fixtures::{governor, test_host};
use automerge::AutoCommit;
use serde_json::Value;
use tilo_action_policy::{Answer, KindSet, TreePath};
use tilo_actions::MemberId;
use tilo_agent_session::AppCatalogue;
use tilo_agent_session::bot::host_steps::{
    HostStepError, HostSteps, Instruction, Judgement, Question,
};
use tilo_app_id::apps::CHAT;
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::agents::assistant;

use crate::tl_chat;

/// The kind tl Chat publishes `send` on, and the kind its posts name in the workspace's events.
const TEXT_CHANNEL_KIND: &str = "tl.so.tilo.chat.TextChannel";

/// The Discord bridge mirroring a guild into the workspace's Chat.
mod bridge;
/// A workspace's events through the host's own actions.
mod events;
mod initiative;
/// The host's own answers for the programs it runs for a person.
mod judging;
/// Reading tl Chat's messages.
mod messages;
/// A person's decision that the workspace has a moderation bot.
mod moderation_settings;
/// A person's event that waits on a feed or a page outside.
mod outside;
/// A person's event that waits on their mail.
mod sources;
/// A program over a tilolang app the server is built with.
mod tilolang;
/// A program's triggers at a time its person's calendars hold.
mod times;

/// `program`, carried out for `person`, with `assistant` taking what it
/// hands over and answering what it asks.
fn carried_out_for(
    program: installed::BotRunner,
    person: String,
    assistant: &std::sync::Arc<Assistant>,
) -> installed::BotRunner {
    program.runs_for(person, assistant.clone())
}

const MALLORY: [u8; 32] = [7; 32];
const ADA: [u8; 32] = [8; 32];
/// The greeter's installation.
const GREETER: &str = "0b7e4c2a-9d1f-4e3b-8a6c-5f2d1e0c9b8a";
/// The ticker's installation.
const TICKER: &str = "5e2c8a41-7b3d-4f69-a8c2-1d0e9b7a6f35";

/// The root `governor()`'s device belongs to, which installs the workspace's bots.
pub(super) fn owner_root() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[1; 32])
}

/// A server that decides what members and bots change against the surfaces of the apps it
/// serves, and takes part in its workspaces' channels, as a server that governs workspaces does.
pub(super) async fn bot_server(
    state: &std::path::Path,
    key: &ed25519_dalek::SigningKey,
    cert: &DeviceCert,
) -> crate::workspaces::fixtures::TestHost {
    // The native apps name their surfaces as the host first runs them.
    let _ = AppCatalogue::native();
    crate::surfaces::decide_receipt(&state.join("surfaces"));
    test_host(state, key, cert).await
}

/// A new member of `workspace` of their own person, admitted through its entry
/// with `role`, and their certificate.
pub(super) async fn member(
    workspaces: &Workspaces,
    owner: &DeviceCert,
    workspace: &str,
    seed: u8,
    role: tilo_server_protocol::workspaces::Role,
) -> DeviceCert {
    let (key, cert) = crate::workspaces::fixtures::device_cert(
        &ed25519_dalek::SigningKey::from_bytes(&[seed; 32]),
        "member",
        tilo_user_identity::DeviceCapabilities::member(),
    );
    crate::workspaces::fixtures::entry_join_as(
        workspaces,
        owner,
        workspace,
        (&key, &cert),
        seed,
        role,
    )
    .await;
    cert
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

/// Recommends a membership change and taking back what the person did.
struct FakeJudge(Outcome);

impl Judge for FakeJudge {
    fn model(&self) -> &'static str {
        "fake-model"
    }

    async fn judge(&self, case: &Case) -> Result<Verdict, JudgeError> {
        assert_eq!(case.activity.len(), 3, "the reported person's three posts");
        Ok(Verdict {
            outcome: self.0,
            minutes: (self.0 == Outcome::Restrict).then_some(30),
            revert: true,
            reasoning: "They flooded the channel with the same link.".into(),
        })
    }
}

pub(super) use messages::posts;

/// The step `step` that posts `text` in tl Chat's channel `target`.
fn saying(step: &str, target: tilo_automation::Selector, text: &str) -> tilo_automation::Step {
    use tilo_automation::{Binding, Step, StepKind};
    Step::new(
        step,
        StepKind::Invoke {
            action: tilo_actions::ActionKey::new(CHAT, TEXT_CHANNEL_KIND, "send"),
            resource: target,
            args: [("body".to_owned(), Binding::Literal(text.into()))]
                .into_iter()
                .collect(),
            appearance: None,
        },
    )
}

/// Introduce `person` to `workspace` as an editor, as a server admits a
/// member before their page writes: a member's change is decided as a
/// member's, never as the server's device alone.
async fn introduce(workspaces: &Workspaces, workspace: &str, cert: &DeviceCert, person: &str) {
    let Ok(bytes) = hex::decode(person) else {
        return;
    };
    let Ok(key) = <[u8; 32]>::try_from(bytes) else {
        return;
    };
    let Ok((_, members)) = workspaces.membership(workspace).await else {
        return;
    };
    if members.contains_key(person) {
        return;
    }
    workspaces
        .add_member(
            workspace,
            cert,
            crate::workspaces::AddMemberRequest {
                request_id: *blake3::hash(person.as_bytes()).as_bytes(),
                user_public_key: key,
                place: crate::workspaces::MemberPlace::Role(tilo_peer_state::join::Role::Editor),
                name: None,
            },
        )
        .await
        .unwrap();
}

/// Post `text` in `channel` of `workspace` as `author`, as their page does: through Chat's own
/// `send`, the change the person's, which the workspace decides.
async fn post_in(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &DeviceCert,
    (author, channel, text): (&str, &str, &str),
) {
    introduce(workspaces, workspace, cert, author).await;
    let (author, channel, text) = (author.to_owned(), channel.to_owned(), text.to_owned());
    workspaces
        .with_tl_record(
            workspace,
            &tl_chat::packages().unwrap(),
            tl_chat::viewer(),
            move |chat| {
                let who = tl_chat::member(MemberId::person(author));
                let to = tl_chat::resource(chat, "TextChannel", &channel)?;
                tl_chat::send(chat, &who, &to, &text, None).map(drop)
            },
        )
        .await
        .unwrap();
}

/// A bot that answers every person's post in the workspace's Chat.
fn greeter() -> installed::InstalledBot {
    greeter_into(tilo_automation::Selector::FirstOfKind(
        TEXT_CHANNEL_KIND.into(),
    ))
}

/// A bot that answers every person's post in the workspace's Chat by posting through the
/// channel `target` names.
fn greeter_into(target: tilo_automation::Selector) -> installed::InstalledBot {
    use tilo_automation::{ActorFilter, Shortcut, TriggerSpec};
    let trigger = TriggerSpec::ResourceChanged {
        kind: TEXT_CHANNEL_KIND.into(),
        actions: vec!["send".into()],
        fields: Vec::new(),
        by: ActorFilter::People,
    };
    let shortcut = Shortcut {
        id: "greeter".into(),
        name: "Greeter".into(),
        inputs: trigger.payload().to_vec(),
        steps: vec![saying("answer", target, "welcome!")],
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

/// A bot that posts `tick` in the workspace's Chat once every `seconds`, answering
/// fire times missed while its host was down as `catch_up` says.
fn ticker(seconds: u64, catch_up: tilo_automation::CatchUp) -> installed::InstalledBot {
    on_the_clock(tilo_automation::TriggerSpec::Schedule {
        schedule: tilo_automation::Schedule::Interval { seconds },
        catch_up,
    })
}

/// A bot that posts `tick` in the workspace's Chat once, at `at_ms`.
fn reminder(at_ms: i64) -> installed::InstalledBot {
    on_the_clock(tilo_automation::TriggerSpec::Once { at_ms })
}

/// A bot that posts `tick` in the workspace's Chat whenever `trigger`'s time comes.
fn on_the_clock(trigger: tilo_automation::TriggerSpec) -> installed::InstalledBot {
    use tilo_automation::Shortcut;
    let shortcut = Shortcut {
        id: "ticker".into(),
        name: "Ticker".into(),
        inputs: Vec::new(),
        steps: vec![saying(
            "announce",
            tilo_automation::Selector::FirstOfKind(TEXT_CHANNEL_KIND.into()),
            "tick",
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
        installation: TICKER.into(),
        entry: "ticker".into(),
        name: "Ticker".into(),
        program: String::from_utf8(tilo_automation::program::canonical_bytes(&shortcut).unwrap())
            .unwrap(),
        inputs: serde_json::Map::new(),
        installed_at_ms: 1,
        release: String::new(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_installed_bot_answers_a_post_in_the_workspace_once() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, general) = chat_workspace(&workspaces, &cert).await;
    let ada = hex::encode(ADA);
    let mut runner = member_runner(
        &workspaces,
        &workspace,
        &cert,
        greeter(),
        scratch.path().join("greeter.json"),
    )
    .await;
    // Installed now: what happened before is not answered.
    assert!(runner.step().await.unwrap().is_empty());
    let members = runner.workspace_input().await.unwrap()["members"].clone();
    assert_eq!(
        members
            .as_array()
            .unwrap()
            .iter()
            .map(|member| (member["member"].clone(), member["role"].clone()))
            .collect::<Vec<_>>(),
        [(
            serde_json::json!({"person": hex::encode(cert.user_public_key)}),
            serde_json::json!("owner")
        )],
        "a program declaring `workspace` is told who is in it"
    );
    post_in(&workspaces, &workspace, &cert, (&ada, &general, "hello")).await;
    let reports = runner.step().await.unwrap();
    assert_eq!(
        reports.len(),
        1,
        "{reports:?}, not invocation: {:?}",
        runner.dead_letters().collect::<Vec<_>>()
    );
    assert_eq!(
        reports[0].outcome,
        tilo_automation::InvocationOutcome::Completed,
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
    assert_eq!(
        answers[0].who,
        Some(tl_chat::member(MemberId::bot(GREETER)))
    );
}

/// A workspace with tl Chat's `#general`: the workspace, and the channel's id as Chat's record
/// names it.
async fn chat_workspace(workspaces: &Workspaces, cert: &DeviceCert) -> (String, String) {
    let workspace = workspaces.create("Plaza").await.unwrap().workspace;
    let owner = hex::encode(cert.user_public_key);
    let general = workspaces
        .with_tl_record(
            &workspace,
            &tl_chat::packages().unwrap(),
            tl_chat::viewer(),
            move |chat| {
                let who = tl_chat::member(MemberId::person(owner));
                tl_chat::new_channel(chat, &who, ("general", ""), None).map(|at| at.id_string())
            },
        )
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
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, general) = chat_workspace(&workspaces, &cert).await;
    let state = scratch.path().join("greeter.json");
    let start = || member_runner(&workspaces, &workspace, &cert, greeter(), state.clone());
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

/// A time just past an hourly fire time, so a minute-long schedule's next
/// time is a minute away.
const AFTER_THE_HOUR_MS: u64 = 3_600_001;
/// The minute-long schedule's first fire time.
const FIRST_TICK_MS: u64 = 3_660_000;
/// Its second.
const SECOND_TICK_MS: u64 = 3_720_000;

#[tokio::test(flavor = "multi_thread")]
async fn a_scheduled_bot_runs_at_its_time_once_and_again_at_the_next() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let manual = std::sync::Arc::new(tilo_clock::ManualClock::new(AFTER_THE_HOUR_MS));
    let mut runner = member_runner(
        &workspaces,
        &workspace,
        &cert,
        ticker(60, tilo_automation::CatchUp::Skip),
        scratch.path().join("ticker.json"),
    )
    .await;
    runner.use_clock(manual.clone());
    assert!(
        runner.step().await.unwrap().is_empty(),
        "its first time has not come"
    );
    manual.set(FIRST_TICK_MS - 1);
    assert!(
        runner.step().await.unwrap().is_empty(),
        "not a millisecond early"
    );
    manual.set(FIRST_TICK_MS);
    let reports = runner.step().await.unwrap();
    assert_eq!(
        reports.len(),
        1,
        "{reports:?}, not run: {:?}",
        runner.dead_letters().collect::<Vec<_>>()
    );
    assert_eq!(
        reports[0].outcome,
        tilo_automation::InvocationOutcome::Completed,
        "{reports:?}"
    );
    assert!(
        runner.step().await.unwrap().is_empty(),
        "one run per fire time"
    );
    let ticks = posts(&workspaces, &workspace)
        .await
        .into_iter()
        .filter(|message| message.text == "tick")
        .count();
    assert_eq!(ticks, 1, "the bot posted once");
    manual.set(SECOND_TICK_MS);
    assert_eq!(
        runner.step().await.unwrap().len(),
        1,
        "the next time runs again"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "a schedule's and a moment's missed fire time across one restart are one story"
)]
async fn a_restarted_host_fires_what_it_missed_once_whether_a_schedule_or_a_moment() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let manual = std::sync::Arc::new(tilo_clock::ManualClock::new(AFTER_THE_HOUR_MS));

    // A schedule that catches up, down across a quarter hour of its times:
    // they run once between them.
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let state = scratch.path().join("ticker.json");
    let mut runner = member_runner(
        &workspaces,
        &workspace,
        &cert,
        ticker(60, tilo_automation::CatchUp::RunOnce),
        state.clone(),
    )
    .await;
    runner.use_clock(manual.clone());
    assert!(runner.step().await.unwrap().is_empty());
    drop(runner);
    let back_ms = FIRST_TICK_MS + 15 * 60_000;
    manual.set(back_ms);
    let mut runner = member_runner(
        &workspaces,
        &workspace,
        &cert,
        ticker(60, tilo_automation::CatchUp::RunOnce),
        state,
    )
    .await;
    runner.use_clock(manual.clone());
    let reports = runner.step().await.unwrap();
    assert_eq!(
        reports.len(),
        1,
        "once for however many times it missed: {reports:?}"
    );
    assert!(runner.step().await.unwrap().is_empty());
    manual.set(back_ms + 60_000);
    assert_eq!(
        runner.step().await.unwrap().len(),
        1,
        "the next time comes as scheduled"
    );
    assert_eq!(
        ticks(&workspaces, &workspace).await,
        2,
        "the bot posted twice"
    );
    drop(runner);
    let manual = std::sync::Arc::new(tilo_clock::ManualClock::new(AFTER_THE_HOUR_MS));

    // A moment that passed while its host was down: it fires once, however
    // late, where a schedule that skips would let the time go.
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let state = scratch.path().join("reminder.json");
    let at_ms = i64::try_from(FIRST_TICK_MS).unwrap();
    let mut runner = member_runner(
        &workspaces,
        &workspace,
        &cert,
        reminder(at_ms),
        state.clone(),
    )
    .await;
    runner.use_clock(manual.clone());
    assert!(runner.step().await.unwrap().is_empty());
    drop(runner);
    let back_ms = FIRST_TICK_MS + 6 * 3_600_000;
    manual.set(back_ms);
    let mut runner = member_runner(
        &workspaces,
        &workspace,
        &cert,
        reminder(at_ms),
        state.clone(),
    )
    .await;
    runner.use_clock(manual.clone());
    let reports = runner.step().await.unwrap();
    assert_eq!(
        reports.len(),
        1,
        "{reports:?}, not run: {:?}",
        runner.dead_letters().collect::<Vec<_>>()
    );
    assert_eq!(
        reports[0].outcome,
        tilo_automation::InvocationOutcome::Completed,
        "{reports:?}"
    );
    assert!(runner.step().await.unwrap().is_empty());
    drop(runner);
    let mut runner = member_runner(&workspaces, &workspace, &cert, reminder(at_ms), state).await;
    runner.use_clock(manual.clone());
    assert!(
        runner.step().await.unwrap().is_empty(),
        "the next restart does not answer it again"
    );
    assert_eq!(
        ticks(&workspaces, &workspace).await,
        1,
        "the bot posted once"
    );

    // A moment that had passed before its bot was installed at all (the
    // host was down from the request on) fires in the first pass.
    let mut runner = member_runner(
        &workspaces,
        &workspace,
        &cert,
        reminder(at_ms + 1_000),
        scratch.path().join("installed-late.json"),
    )
    .await;
    runner.use_clock(manual.clone());
    assert_eq!(
        runner.step().await.unwrap().len(),
        1,
        "not run: {:?}",
        runner.dead_letters().collect::<Vec<_>>()
    );
    assert!(runner.step().await.unwrap().is_empty());
    assert_eq!(ticks(&workspaces, &workspace).await, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restarted_host_skips_the_times_it_missed() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let state = scratch.path().join("ticker.json");
    let manual = std::sync::Arc::new(tilo_clock::ManualClock::new(AFTER_THE_HOUR_MS));
    let mut runner = member_runner(
        &workspaces,
        &workspace,
        &cert,
        ticker(60, tilo_automation::CatchUp::Skip),
        state.clone(),
    )
    .await;
    runner.use_clock(manual.clone());
    assert!(runner.step().await.unwrap().is_empty());
    drop(runner);

    let back_ms = FIRST_TICK_MS + 15 * 60_000;
    let manual = std::sync::Arc::new(tilo_clock::ManualClock::new(back_ms));
    let mut runner = member_runner(
        &workspaces,
        &workspace,
        &cert,
        ticker(60, tilo_automation::CatchUp::Skip),
        state,
    )
    .await;
    runner.use_clock(manual.clone());
    assert!(
        runner.step().await.unwrap().is_empty(),
        "a time missed while down is let go"
    );
    manual.set(back_ms + 60_000);
    assert_eq!(
        runner.step().await.unwrap().len(),
        1,
        "the next time is the one that runs"
    );
}

/// How many times the bot on the clock posted.
async fn ticks(workspaces: &Workspaces, workspace: &str) -> usize {
    posts(workspaces, workspace)
        .await
        .into_iter()
        .filter(|message| message.text == "tick")
        .count()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_moment_fires_once_at_its_time_and_never_again() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let state = scratch.path().join("reminder.json");
    let at_ms = i64::try_from(FIRST_TICK_MS).unwrap();
    let manual = std::sync::Arc::new(tilo_clock::ManualClock::new(AFTER_THE_HOUR_MS));
    let mut runner = member_runner(
        &workspaces,
        &workspace,
        &cert,
        reminder(at_ms),
        state.clone(),
    )
    .await;
    runner.use_clock(manual.clone());
    assert!(
        runner.step().await.unwrap().is_empty(),
        "its moment has not come"
    );
    manual.set(FIRST_TICK_MS - 1);
    assert!(
        runner.step().await.unwrap().is_empty(),
        "not a millisecond early"
    );
    manual.set(FIRST_TICK_MS);
    let reports = runner.step().await.unwrap();
    assert_eq!(
        reports.len(),
        1,
        "{reports:?}, not run: {:?}",
        runner.dead_letters().collect::<Vec<_>>()
    );
    assert_eq!(
        reports[0].outcome,
        tilo_automation::InvocationOutcome::Completed,
        "{reports:?}"
    );
    assert!(
        runner.step().await.unwrap().is_empty(),
        "a moment comes once"
    );
    manual.set(FIRST_TICK_MS + 24 * 3_600_000);
    assert!(
        runner.step().await.unwrap().is_empty(),
        "and never again, however long the host runs"
    );
    drop(runner);

    // A restarted host reads that it answered the moment.
    let mut runner = member_runner(&workspaces, &workspace, &cert, reminder(at_ms), state).await;
    runner.use_clock(manual.clone());
    assert!(
        runner.step().await.unwrap().is_empty(),
        "a restart answers no moment twice"
    );
    assert_eq!(
        ticks(&workspaces, &workspace).await,
        1,
        "the bot posted once"
    );
}

/// A host whose assistant takes what programs hand it and whose model
/// judges what they ask, answering each as scripted, and with nothing once
/// a script is spent.
#[derive(Default)]
struct Assistant {
    taken: std::sync::Mutex<Vec<Instruction>>,
    answers: std::sync::Mutex<std::collections::VecDeque<Result<String, HostStepError>>>,
    judged: std::sync::Mutex<Vec<Judgement>>,
    judgements: std::sync::Mutex<std::collections::VecDeque<Result<Value, HostStepError>>>,
}

impl Assistant {
    fn answering(answers: Vec<Result<String, HostStepError>>) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            answers: std::sync::Mutex::new(answers.into()),
            ..Self::default()
        })
    }

    fn judging(judgements: Vec<Result<Value, HostStepError>>) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            judgements: std::sync::Mutex::new(judgements.into()),
            ..Self::default()
        })
    }

    fn taken(&self) -> Vec<Instruction> {
        self.taken.lock().unwrap().clone()
    }

    fn judged(&self) -> Vec<Judgement> {
        self.judged.lock().unwrap().clone()
    }
}

impl HostSteps for Assistant {
    fn instruct(&self, instruction: Instruction) -> Result<String, HostStepError> {
        self.taken.lock().unwrap().push(instruction);
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Ok(String::new()))
    }

    fn judge(&self, judgement: Judgement) -> Result<Value, HostStepError> {
        self.judged.lock().unwrap().push(judgement);
        self.judgements
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(Value::Null))
    }
}

/// A person's own program `steps`, started once at `at_ms` and told the
/// moment that fired it.
fn at_a_moment(at_ms: i64, steps: Vec<tilo_automation::Step>) -> installed::InstalledBot {
    own_program(tilo_automation::TriggerSpec::Once { at_ms }, steps)
}

/// A person's own program `steps`, started by `trigger`, and told what fired
/// it and its own note.
fn own_program(
    trigger: tilo_automation::TriggerSpec,
    steps: Vec<tilo_automation::Step>,
) -> installed::InstalledBot {
    use tilo_automation::Shortcut;
    let mut inputs = trigger.payload().to_vec();
    inputs.push(tilo_actions::params::ActionParam::optional(
        "note",
        tilo_actions::params::ParamType::Object,
        "The program's note",
    ));
    let shortcut = Shortcut {
        id: "reminder".into(),
        name: "Reminder".into(),
        inputs,
        steps,
        step_timeout_secs: None,
        output: None,
        grant: Vec::new(),
        triggers: vec![trigger],
        saved_by: None,
        saved_proof: None,
    };
    // Exactly what its steps need, as a workspace admits a bot.
    let shortcut = Shortcut {
        grant: tilo_automation::grant::derived_rules(
            &shortcut,
            &tilo_automation::ActionCatalog::new(Vec::new()),
        ),
        ..shortcut
    };
    installed::InstalledBot {
        installation: TICKER.into(),
        entry: "reminder".into(),
        name: "Reminder".into(),
        program: String::from_utf8(tilo_automation::program::canonical_bytes(&shortcut).unwrap())
            .unwrap(),
        inputs: serde_json::Map::new(),
        installed_at_ms: 1,
        release: String::new(),
    }
}

/// The step that hands `instruction` to the assistant.
fn tell(instruction: &str) -> tilo_automation::Step {
    tilo_automation::Step::new(
        "remind",
        tilo_automation::StepKind::Task {
            instruction: instruction.into(),
            allow: Vec::new(),
            output: None,
        },
    )
}

/// A person's own program: at `at_ms`, hand `instruction` to their
/// assistant.
fn reminding(at_ms: i64, instruction: &str) -> installed::InstalledBot {
    at_a_moment(at_ms, vec![tell(instruction)])
}

/// A person's own program: at `at_ms`, ask their model `question` about
/// the moment, and hand `instruction` to their assistant only on a yes.
fn judging(at_ms: i64, question: &str, instruction: &str) -> installed::InstalledBot {
    use tilo_automation::{Binding, Condition, Step, StepKind};
    at_a_moment(
        at_ms,
        vec![
            Step::new(
                "judge",
                StepKind::Ai {
                    prompt: question.into(),
                    output: Some(serde_json::json!({"type": "boolean"})),
                    input: Some(Binding::Template("{{input.event}}".into())),
                },
            ),
            Step::new(
                "when",
                StepKind::If {
                    cond: Condition::Truthy(Binding::step("judge", "")),
                    then: vec![tell(instruction)],
                    otherwise: Vec::new(),
                },
            ),
        ],
    )
}

/// A step on what the program itself keeps.
fn own_step(
    id: &str,
    action: &str,
    args: Vec<(&str, tilo_automation::Binding)>,
) -> tilo_automation::Step {
    use tilo_automation::{SHORTCUT_RESOURCE_KIND, SHORTCUTS_APP_ID, Selector, Step, StepKind};
    Step::new(
        id,
        StepKind::Invoke {
            action: tilo_actions::ActionKey::new(SHORTCUTS_APP_ID, SHORTCUT_RESOURCE_KIND, action),
            resource: Selector::FirstOfKind(SHORTCUT_RESOURCE_KIND.into()),
            args: args
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value))
                .collect(),
            appearance: None,
        },
    )
}

/// The steps `then`, taken when `cond` holds.
fn when(
    cond: tilo_automation::Condition,
    then: Vec<tilo_automation::Step>,
) -> tilo_automation::Step {
    tilo_automation::Step::new(
        "when",
        tilo_automation::StepKind::If {
            cond,
            then,
            otherwise: Vec::new(),
        },
    )
}

/// Every minute, in a program of a person's own.
fn every_minute() -> tilo_automation::TriggerSpec {
    tilo_automation::TriggerSpec::Schedule {
        schedule: tilo_automation::Schedule::Interval { seconds: 60 },
        catch_up: tilo_automation::CatchUp::Skip,
    }
}

/// The minute-long schedule's fire time number `tick`, from 1.
const fn tick_ms(tick: u64) -> u64 {
    3_600_000_u64.saturating_add(tick.saturating_mul(60_000))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_program_acts_on_every_tenth_firing_and_its_count_survives_a_restart() {
    use tilo_automation::{Binding, Condition};
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let state = scratch.path().join("counting.json");
    let ada = hex::encode(ADA);
    let assistant = Assistant::answering(vec![]);
    let manual = std::sync::Arc::new(tilo_clock::ManualClock::new(AFTER_THE_HOUR_MS));
    let program = || {
        own_program(
            every_minute(),
            vec![
                own_step(
                    "count",
                    "count",
                    vec![("every", Binding::Literal(serde_json::json!(10)))],
                ),
                when(
                    Condition::Truthy(Binding::step("count", "/reached")),
                    vec![tell("Sum up the last ten.")],
                ),
            ],
        )
    };
    let mut runner = member_runner(&workspaces, &workspace, &cert, program(), state.clone())
        .await
        .runs_for(ada.clone(), assistant.clone());
    runner.use_clock(manual.clone());
    assert!(runner.step().await.unwrap().is_empty());
    for tick in 1..=25_u64 {
        if tick == 5 || tick == 15 {
            // The host restarts between two firings: the count stands
            // where it stood.
            runner = member_runner(&workspaces, &workspace, &cert, program(), state.clone())
                .await
                .runs_for(ada.clone(), assistant.clone());
            runner.use_clock(manual.clone());
        }
        manual.set(tick_ms(tick));
        let reports = runner.step().await.unwrap();
        assert_eq!(
            reports.len(),
            1,
            "firing {tick}: {reports:?}, not run: {:?}",
            runner.dead_letters().collect::<Vec<_>>()
        );
        assert_eq!(
            reports[0].outcome,
            tilo_automation::InvocationOutcome::Completed,
            "firing {tick}: {reports:?}"
        );
        assert_eq!(
            u64::try_from(assistant.taken().len()).unwrap(),
            tick.div_euclid(10),
            "after {tick} firings"
        );
    }
    assert_eq!(runner.kept().read().counts().get(""), Some(&25));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_programs_note_is_read_by_every_run_and_changed_by_one_typed_change() {
    use tilo_agent_session::bot::host_steps::Keeps as _;
    use tilo_agent_session::bot::kept::NoteChange;
    use tilo_automation::{Binding, Condition};
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let state = scratch.path().join("noting.json");
    let ada = hex::encode(ADA);
    let assistant = Assistant::answering(vec![]);
    let manual = std::sync::Arc::new(tilo_clock::ManualClock::new(AFTER_THE_HOUR_MS));
    // Hand the work over once the note says how to write it, and keep the
    // time of this run for the next.
    let program = || {
        own_program(
            every_minute(),
            vec![
                when(
                    Condition::Truthy(Binding::Input("note/style".into())),
                    vec![tell("Write what is new since the last run.")],
                ),
                own_step(
                    "keep",
                    "keep",
                    vec![(
                        "kept",
                        Binding::Object(
                            [("last_ms".to_owned(), Binding::Input("event/due_ms".into()))]
                                .into_iter()
                                .collect(),
                        ),
                    )],
                ),
            ],
        )
    };
    let mut runner = member_runner(&workspaces, &workspace, &cert, program(), state.clone())
        .await
        .runs_for(ada.clone(), assistant.clone());
    runner.use_clock(manual.clone());
    assert!(runner.step().await.unwrap().is_empty());

    // No style yet: the run reads that in its note and hands nothing over,
    // and keeps where it stopped.
    manual.set(tick_ms(1));
    let reports = runner.step().await.unwrap();
    assert_eq!(
        reports[0].outcome,
        tilo_automation::InvocationOutcome::Completed,
        "{reports:?}, not run: {:?}",
        runner.dead_letters().collect::<Vec<_>>()
    );
    assert!(assistant.taken().is_empty());
    assert_eq!(
        runner.kept().read().note().kept,
        serde_json::json!({"last_ms": tick_ms(1)})
    );

    // The person's assistant writes how they want it, between two runs, by
    // the change a run's own step makes too. It is kept at once.
    runner
        .kept()
        .keep(NoteChange {
            style: Some("Three lines, no greeting.".into()),
            accepted: Some("Two new orders. One return. Nothing waits.".into()),
            ..NoteChange::default()
        })
        .unwrap();
    assert!(
        matches!(
            runner.kept().keep(NoteChange {
                guidance: Some("x".repeat(64 * 1024)),
                ..NoteChange::default()
            }),
            Err(HostStepError::Refused(_))
        ),
        "a part past its bound is refused"
    );
    drop(runner);

    // After a restart the next run reads all of it, and its instruction
    // carries the note.
    let mut runner = member_runner(&workspaces, &workspace, &cert, program(), state)
        .await
        .runs_for(ada, assistant.clone());
    runner.use_clock(manual.clone());
    manual.set(tick_ms(2));
    let reports = runner.step().await.unwrap();
    assert_eq!(
        reports[0].outcome,
        tilo_automation::InvocationOutcome::Completed,
        "{reports:?}"
    );
    let taken = assistant.taken();
    assert_eq!(taken.len(), 1, "{taken:?}");
    assert_eq!(taken[0].note["style"], "Three lines, no greeting.");
    assert_eq!(
        taken[0].note["accepted"],
        serde_json::json!(["Two new orders. One return. Nothing waits."])
    );
    assert_eq!(
        taken[0].note["kept"],
        serde_json::json!({"last_ms": tick_ms(1)}),
        "what the run before kept for this one"
    );
    assert_eq!(
        runner.kept().read().note().kept,
        serde_json::json!({"last_ms": tick_ms(2)})
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paused_program_fires_nothing_until_its_person_lets_it_go_on() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let state = scratch.path().join("pausing.json");
    let ada = hex::encode(ADA);
    let assistant = Assistant::answering(vec![]);
    let manual = std::sync::Arc::new(tilo_clock::ManualClock::new(AFTER_THE_HOUR_MS));
    let program = || own_program(every_minute(), vec![tell("Say what is new.")]);
    let mut runner = member_runner(&workspaces, &workspace, &cert, program(), state.clone())
        .await
        .runs_for(ada.clone(), assistant.clone());
    runner.use_clock(manual.clone());
    assert!(runner.step().await.unwrap().is_empty());
    manual.set(tick_ms(1));
    assert_eq!(runner.step().await.unwrap().len(), 1);
    assert_eq!(assistant.taken().len(), 1);

    runner.kept().pause(true).unwrap();
    for tick in 2..=3 {
        manual.set(tick_ms(tick));
        assert!(
            runner.step().await.unwrap().is_empty(),
            "paused at firing {tick}"
        );
    }
    drop(runner);
    // It stays paused across a restart.
    let mut runner = member_runner(&workspaces, &workspace, &cert, program(), state)
        .await
        .runs_for(ada, assistant.clone());
    runner.use_clock(manual.clone());
    manual.set(tick_ms(4));
    assert!(runner.step().await.unwrap().is_empty());
    assert_eq!(assistant.taken().len(), 1);

    runner.kept().pause(false).unwrap();
    assert!(
        runner.step().await.unwrap().is_empty(),
        "what came while it was paused is let go"
    );
    manual.set(tick_ms(5));
    assert_eq!(runner.step().await.unwrap().len(), 1, "it goes on");
    assert_eq!(assistant.taken().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_programs_work_may_send_unasked_only_what_its_person_permitted() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let state = scratch.path().join("permitted.json");
    let ada = hex::encode(ADA);
    let assistant = Assistant::answering(vec![]);
    let manual = std::sync::Arc::new(tilo_clock::ManualClock::new(AFTER_THE_HOUR_MS));
    let program = || own_program(every_minute(), vec![tell("Answer what came in.")]);
    let node = |action: &str| {
        tilo_automation::step_node(
            Some(workspace.as_str()),
            "so.tilo.mail",
            "mail.mailbox",
            &tilo_actions::ActionAddress::new("mail.mailbox:inbox", action),
        )
    };
    let send = tilo_automation::action_rule(
        "so.tilo.mail",
        "mail.mailbox",
        None,
        "send",
        KindSet::OUTSIDE,
        Answer::Allow,
    );
    let mut runner = member_runner(&workspaces, &workspace, &cert, program(), state.clone())
        .await
        .runs_for(ada.clone(), assistant.clone());
    runner.use_clock(manual.clone());
    assert!(runner.step().await.unwrap().is_empty());

    // Set up and nothing more: what its work would send still asks.
    manual.set(tick_ms(1));
    assert_eq!(runner.step().await.unwrap().len(), 1);
    assert!(!assistant.taken()[0].lets_through(&node("send")));

    // Its person says "always" for that one action. Nothing wider is kept.
    runner.kept().permit(send.clone()).unwrap();
    let whole_app = tilo_action_policy::Rule::new(
        TreePath::root()
            .any(tilo_action_policy::Level::Workspace)
            .app("so.tilo.mail"),
        KindSet::ALL,
        Answer::Allow,
    );
    assert!(matches!(
        runner.kept().permit(whole_app),
        Err(HostStepError::Refused(_))
    ));
    drop(runner);
    let mut runner = member_runner(&workspaces, &workspace, &cert, program(), state)
        .await
        .runs_for(ada, assistant.clone());
    runner.use_clock(manual.clone());
    manual.set(tick_ms(2));
    assert_eq!(runner.step().await.unwrap().len(), 1);
    let taken = assistant.taken();
    assert!(
        taken[1].lets_through(&node("send")),
        "the permission is kept across a restart and goes with the instruction"
    );
    assert!(!taken[1].lets_through(&node("delete")));

    // Taken back, it asks again.
    runner.kept().withdraw(&send).unwrap();
    manual.set(tick_ms(3));
    assert_eq!(runner.step().await.unwrap().len(), 1);
    assert!(!assistant.taken()[2].lets_through(&node("send")));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_program_asks_its_persons_model_and_acts_only_on_a_yes() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let at_ms = i64::try_from(FIRST_TICK_MS).unwrap();
    let ada = hex::encode(ADA);
    let assistant = Assistant::judging(vec![
        Ok(Value::Bool(true)),
        Ok(Value::Bool(false)),
        Err(HostStepError::Refused(
            "no model is linked for this kind of work".into(),
        )),
    ]);
    let manual = std::sync::Arc::new(tilo_clock::ManualClock::new(FIRST_TICK_MS));
    let question = "Is this the first working day of the month?";

    // Yes: the instruction is handed over.
    let mut runner = member_runner(
        &workspaces,
        &workspace,
        &cert,
        judging(at_ms, question, "Say that the report is due."),
        scratch.path().join("yes.json"),
    )
    .await
    .runs_for(ada.clone(), assistant.clone());
    runner.use_clock(manual.clone());
    let reports = runner.step().await.unwrap();
    assert_eq!(
        reports.len(),
        1,
        "{reports:?}, not run: {:?}",
        runner.dead_letters().collect::<Vec<_>>()
    );
    assert_eq!(
        reports[0].outcome,
        tilo_automation::InvocationOutcome::Completed,
        "{reports:?}"
    );
    let judged = assistant.judged();
    assert_eq!(judged.len(), 1, "{judged:?}");
    assert_eq!(judged[0].person, ada, "their own model is the one asked");
    assert_eq!(
        judged[0].question,
        Question::Prompt {
            prompt: question.into(),
            schema: Some(serde_json::json!({"type": "boolean"})),
        }
    );
    assert!(
        judged[0]
            .input
            .as_deref()
            .is_some_and(|input| input.contains(&at_ms.to_string())),
        "it judges what fired the run: {judged:?}"
    );
    assert_eq!(assistant.taken().len(), 1);
    drop(runner);

    // No: the run is done, and nothing is handed over.
    manual.set(FIRST_TICK_MS + 60_000);
    let mut runner = member_runner(
        &workspaces,
        &workspace,
        &cert,
        judging(at_ms + 60_000, question, "Say that the report is due."),
        scratch.path().join("no.json"),
    )
    .await
    .runs_for(ada.clone(), assistant.clone());
    runner.use_clock(manual.clone());
    let reports = runner.step().await.unwrap();
    assert_eq!(
        reports[0].outcome,
        tilo_automation::InvocationOutcome::Completed,
        "{reports:?}"
    );
    assert_eq!(assistant.judged().len(), 2);
    assert_eq!(assistant.taken().len(), 1, "a no hands nothing over");
    drop(runner);

    // No model to ask: the run stops at the judgement, with the reason
    // kept, and guesses nothing.
    manual.set(FIRST_TICK_MS + 120_000);
    let mut runner = member_runner(
        &workspaces,
        &workspace,
        &cert,
        judging(at_ms + 120_000, question, "Say that the report is due."),
        scratch.path().join("no-model.json"),
    )
    .await
    .runs_for(ada, assistant.clone());
    runner.use_clock(manual.clone());
    runner.step().await.unwrap();
    let dead: Vec<_> = runner.dead_letters().collect();
    assert_eq!(dead.len(), 1, "{dead:?}");
    assert_eq!(dead[0].step.as_deref(), Some("judge"));
    assert!(dead[0].reason.contains("no model is linked"), "{dead:?}");
    assert_eq!(assistant.taken().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reminder_hands_its_instruction_to_its_persons_assistant_once() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let state = scratch.path().join("reminder.json");
    let at_ms = i64::try_from(FIRST_TICK_MS).unwrap();
    let ada = hex::encode(ADA);
    let assistant = Assistant::answering(vec![]);
    let manual = std::sync::Arc::new(tilo_clock::ManualClock::new(AFTER_THE_HOUR_MS));
    let mut runner = member_runner(
        &workspaces,
        &workspace,
        &cert,
        reminding(at_ms, "Say that it is time to call home."),
        state.clone(),
    )
    .await
    .runs_for(ada.clone(), assistant.clone());
    runner.use_clock(manual.clone());
    assert!(runner.step().await.unwrap().is_empty());
    assert!(assistant.taken().is_empty(), "nothing before its moment");

    manual.set(FIRST_TICK_MS);
    let reports = runner.step().await.unwrap();
    assert_eq!(
        reports.len(),
        1,
        "{reports:?}, not run: {:?}",
        runner.dead_letters().collect::<Vec<_>>()
    );
    assert_eq!(
        reports[0].outcome,
        tilo_automation::InvocationOutcome::Completed,
        "{reports:?}"
    );
    let taken = assistant.taken();
    assert_eq!(taken.len(), 1, "{taken:?}");
    assert_eq!(taken[0].person, ada, "it runs for the person it is of");
    assert_eq!(taken[0].workspace, workspace);
    assert_eq!(taken[0].instruction, "Say that it is time to call home.");
    assert_eq!(taken[0].cause.installation, TICKER);
    assert!(
        taken[0]
            .cause
            .event
            .as_deref()
            .is_some_and(|event| event.starts_with("schedule")),
        "the moment that fired it is its cause: {taken:?}"
    );
    assert!(runner.step().await.unwrap().is_empty());
    drop(runner);

    // A restarted host hands nothing over a second time.
    manual.set(FIRST_TICK_MS + 3_600_000);
    let mut runner = member_runner(
        &workspaces,
        &workspace,
        &cert,
        reminding(at_ms, "Say that it is time to call home."),
        state,
    )
    .await
    .runs_for(ada, assistant.clone());
    runner.use_clock(manual.clone());
    assert!(runner.step().await.unwrap().is_empty());
    assert_eq!(assistant.taken().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_instruction_nothing_takes_is_handed_over_again_or_kept_as_a_dead_letter() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let at_ms = i64::try_from(FIRST_TICK_MS).unwrap();
    let ada = hex::encode(ADA);
    let assistant = Assistant::answering(vec![
        Err(HostStepError::Unavailable(
            "the assistant is starting".into(),
        )),
        Ok(String::new()),
        Err(HostStepError::Refused(
            "this person keeps no assistant".into(),
        )),
    ]);
    let failed = |reports: &[tilo_automation::InvocationReport]| {
        matches!(
            reports,
            [tilo_automation::InvocationReport {
                outcome: tilo_automation::InvocationOutcome::Failed { .. },
                ..
            }]
        )
    };

    // Nothing can take it yet: the run is tried again after its backoff,
    // and the step comes back as the same one.
    let manual = std::sync::Arc::new(tilo_clock::ManualClock::new(FIRST_TICK_MS));
    let mut runner = member_runner(
        &workspaces,
        &workspace,
        &cert,
        reminding(at_ms, "Say that it is time to call home."),
        scratch.path().join("reminder.json"),
    )
    .await
    .runs_for(ada.clone(), assistant.clone());
    runner.use_clock(manual.clone());
    let reports = runner.step().await.unwrap();
    assert!(failed(&reports), "{reports:?}");
    assert_eq!(runner.dead_letters().count(), 0, "it is tried again");
    assert!(
        runner.step().await.unwrap().is_empty(),
        "not before its backoff"
    );
    manual.set(FIRST_TICK_MS + 1_000);
    let reports = runner.step().await.unwrap();
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert_eq!(
        reports[0].outcome,
        tilo_automation::InvocationOutcome::Completed,
        "{reports:?}"
    );
    let taken = assistant.taken();
    assert_eq!(taken.len(), 2, "{taken:?}");
    assert_eq!(
        taken[0].cause.invocation, taken[1].cause.invocation,
        "the same step both times, so a host starts one turn for it"
    );
    drop(runner);

    // Refused for good: kept with the reason, where it can be read.
    manual.set(FIRST_TICK_MS + 60_000);
    let mut runner = member_runner(
        &workspaces,
        &workspace,
        &cert,
        reminding(at_ms + 60_000, "Say it again."),
        scratch.path().join("refused.json"),
    )
    .await
    .runs_for(ada, assistant.clone());
    runner.use_clock(manual.clone());
    let reports = runner.step().await.unwrap();
    assert!(failed(&reports), "{reports:?}");
    let dead: Vec<_> = runner.dead_letters().collect();
    assert_eq!(dead.len(), 1, "{dead:?}");
    assert_eq!(dead[0].step.as_deref(), Some("remind"));
    assert!(
        dead[0].reason.contains("this person keeps no assistant"),
        "{dead:?}"
    );
    drop(runner);

    // A bot that runs for nobody is offered no such step, as before.
    manual.set(FIRST_TICK_MS + 120_000);
    let mut runner = member_runner(
        &workspaces,
        &workspace,
        &cert,
        reminding(at_ms + 120_000, "Say hello."),
        scratch.path().join("nobody.json"),
    )
    .await;
    runner.use_clock(manual.clone());
    let reports = runner.step().await.unwrap();
    assert!(failed(&reports), "{reports:?}");
    assert_eq!(runner.dead_letters().count(), 1);
    assert_eq!(assistant.taken().len(), 3, "nobody's assistant was asked");

    // Until this host can start a turn of a person's assistant, its own
    // answer is that nothing takes the instruction yet.
    assert!(matches!(
        assistant::host_steps(workspaces.clone()).instruct(taken[0].clone()),
        Err(HostStepError::Unavailable(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_that_fails_for_good_is_kept_as_a_dead_letter() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, general) = chat_workspace(&workspaces, &cert).await;
    let state = scratch.path().join("greeter.json");
    // Its answer names a conversation the chat does not have.
    let start = || {
        member_runner(
            &workspaces,
            &workspace,
            &cert,
            greeter_into(tilo_automation::Selector::Fixed(
                "tl:so.tilo.chat:no-such-channel".into(),
            )),
            state.clone(),
        )
    };
    let mut runner = start().await;
    assert!(runner.step().await.unwrap().is_empty());
    ada_says(&workspaces, &workspace, &cert, general.as_str(), "hello").await;
    let reports = runner.step().await.unwrap();
    assert!(
        matches!(reports[..], [ref report] if matches!(report.outcome, tilo_automation::InvocationOutcome::Failed { .. })),
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
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let mut bot = greeter();
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

/// A bot's run reads the records it follows through the record API: Chat's
/// record of the workspace and its messages' chain, with what was just posted.
#[tokio::test(flavor = "multi_thread")]
async fn a_bot_step_reads_the_records_it_follows_through_the_record_api() {
    use tilo_automation::{ActorFilter, Shortcut, Step, StepKind, TriggerSpec};
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, general) = chat_workspace(&workspaces, &cert).await;
    let trigger = TriggerSpec::ResourceChanged {
        kind: TEXT_CHANNEL_KIND.into(),
        actions: vec!["send".into()],
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
                    record: tilo_workspace_service::tl_records::TREE.into(),
                    joined: false,
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
        panic!("one invocation: {reports:?}");
    };
    assert_eq!(
        report.outcome,
        tilo_automation::InvocationOutcome::Completed,
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
    // The reading is what every reader is answered: the record's value holds the app's
    // documents, each with its kind.
    let documents = &chat["records"][0]["value"]["records"];
    assert!(
        documents.as_array().is_some_and(|records| records
            .iter()
            .any(|record| record["kind"] == "tl.so.tilo.chat.Conversations")),
        "the read lists the workspace's conversations: {chat}"
    );
    let messages = read("messages").to_string();
    assert!(messages.contains("general"), "{messages}");
    assert!(messages.contains("what is here?"), "{messages}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bridge_relays_under_another_name_and_the_post_stays_the_bots() {
    use tilo_automation::{Binding, StepKind};
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, general) = chat_workspace(&workspaces, &cert).await;
    // The greeter relays its answer as written by Grace on Discord, granted what that takes.
    let mut bridge = greeter();
    let mut shortcut = bridge.shortcut().unwrap();
    if let StepKind::Invoke { action, args, .. } = &mut shortcut.steps[0].kind {
        *action = tilo_actions::ActionKey::new(CHAT, TEXT_CHANNEL_KIND, "relay");
        args.insert("by".to_owned(), Binding::Literal("Grace (Discord)".into()));
        args.insert("written_in".to_owned(), Binding::Literal("Discord".into()));
    }
    shortcut.grant = tilo_automation::grant::derived_rules(
        &shortcut,
        &tilo_automation::ActionCatalog::new(Vec::new()),
    );
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
        matches!(reports[..], [ref report] if report.outcome == tilo_automation::InvocationOutcome::Completed),
        "the workspace admits the bridge's post: {reports:?}"
    );
    let post = posts(&workspaces, &workspace)
        .await
        .into_iter()
        .find(|message| message.text == "welcome!")
        .expect("the bridge posted");
    assert_eq!(
        post.who,
        Some(tl_chat::member(MemberId::bot(GREETER))),
        "still the bot's"
    );
    assert_eq!(
        (post.by.as_str(), post.written_in.as_str()),
        ("Grace (Discord)", "Discord"),
        "the post shows the other name"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_change_runs_again_then_is_a_dead_letter_and_later_runs_still_land() {
    let scratch = tempfile::tempdir().unwrap();
    // The host holds the workspace's keys, as a host that governs one does: the
    // bot's install grants it nothing, so every record is sealed under keys of
    // its own, which only a key holder writes.
    let (key, cert) = crate::workspaces::fixtures::governor_with(
        tilo_user_identity::DeviceCapabilities::member(),
    );
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, general) = chat_workspace(&workspaces, &cert).await;
    let bot = greeter();
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
            .starts_with("the workspace refused the bot's change to so.tilo.chat:"),
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
fn membership_changes_fire_a_bots_workspace_triggers_as_governance_verbs() {
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
    let trigger = TriggerSpec::ResourceChanged {
        kind: "tilo.workspace".into(),
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
    use crate::workspaces::sealed_fixtures::{SealingMember, member_device};
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = crate::workspaces::fixtures::governor_with(
        tilo_user_identity::DeviceCapabilities::member(),
    );
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, general) = chat_workspace(&workspaces, &cert).await;
    let (guest_key, guest_cert) = member_device(&workspaces, &workspace, (&key, &cert), 3).await;
    let mut runner = member_runner(
        &workspaces,
        &workspace,
        &cert,
        greeter(),
        scratch.path().join("greeter.json"),
    )
    .await;
    assert!(runner.step().await.unwrap().is_empty());

    let before = workspaces.store_writes(&workspace, CHAT).await.unwrap();
    // The guest's post, as their page writes one: through Chat's own `send`, as the guest.
    let who = tl_chat::member(MemberId::person(hex::encode(guest_cert.user_public_key)));
    let in_general = general.clone();
    workspaces
        .with_tl_record(
            &workspace,
            &tl_chat::packages().unwrap(),
            tl_chat::viewer(),
            move |chat| {
                let to = tl_chat::resource(chat, "TextChannel", &in_general)?;
                tl_chat::send(chat, &who, &to, "hello", None).map(drop)
            },
        )
        .await
        .unwrap();
    // The guest takes part in the document Chat keeps the channel's messages in, under the keys
    // its members seal it with.
    let (document, hello) = workspaces
        .store_writes(&workspace, CHAT)
        .await
        .unwrap()
        .into_iter()
        .find(|(document, writes)| before.get(document).map_or(0, Vec::len) < writes.len())
        .expect("the guest's post was published");
    let guest = SealingMember::join(&workspaces, &workspace, (CHAT, &document), guest_key).await;
    let reports = runner.step().await.unwrap();
    assert!(
        matches!(reports[..], [ref report] if report.outcome == tilo_automation::InvocationOutcome::Completed),
        "the workspace admits the bot's post to the sealed chat: {reports:?}"
    );

    // The guest opens what the workspace published, as its page does, and holds what the
    // workspace does: the bot's answer with it.
    let post = workspaces
        .store_writes(&workspace, CHAT)
        .await
        .unwrap()
        .remove(&document)
        .and_then(|mut writes| writes.pop())
        .expect("the bot's post was published");
    assert!(
        !hello.contains(&post),
        "the bot's post is a write of its own"
    );
    assert!(post.encryption.is_some(), "never in the clear");
    guest.take_in(&post);
    let held = workspaces
        .resource_document(&workspace, CHAT, &document)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        guest.document().get_heads(),
        AutoCommit::load(&held).unwrap().get_heads(),
        "the guest holds the bot's answer"
    );
    let posted = posts(&workspaces, &workspace).await;
    let answers: Vec<_> = posted
        .iter()
        .filter(|message| message.text == "welcome!")
        .collect();
    assert_eq!(answers.len(), 1, "the guest reads the bot's answer");
    assert_eq!(
        answers[0].who,
        Some(tl_chat::member(MemberId::bot(GREETER)))
    );
    assert!(
        posted.iter().any(|message| message.text == "hello"),
        "beside the guest's post"
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
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
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
async fn a_bot_program_pushed_into_a_running_workspace_replaces_the_one_it_runs() {
    use tilo_peer_state::workspace_membership::{BotRequest, WorkspaceMembershipCommand};
    const ENTRY: &str = "dev.ada.greeter";
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, general) = chat_workspace(&workspaces, &cert).await;
    let first = greeter().program;
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

/// A host governs more than the workspace its operator started: a person's
/// own workspace here asks for a bot, and the bot runs in that one — beside
/// the workspace, answering the person who asks it — without the operator's
/// workspace ever running it.
#[tokio::test(flavor = "multi_thread")]
async fn a_bot_a_governed_workspace_asks_for_runs_beside_that_workspace() {
    use tilo_peer_state::workspace_membership::{BotRequest, WorkspaceMembershipCommand};
    const ENTRY: &str = "dev.ada.greeter";
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    // The workspace the operator started, and one of a person's own.
    let (started, _) = chat_workspace(&workspaces, &cert).await;
    let (annex, annex_general) = chat_workspace(&workspaces, &cert).await;
    assert_ne!(started, annex);

    let program = greeter().program;
    let request_id = *blake3::hash(program.as_bytes()).as_bytes();
    workspaces
        .ask_for_bot(
            &annex,
            &cert,
            request_id,
            WorkspaceMembershipCommand::RequestBot(BotRequest {
                entry: ENTRY.into(),
                version: "0.1.0-dev.1".into(),
                inputs: std::collections::BTreeMap::new(),
                program: Some(program),
            }),
        )
        .await
        .unwrap();

    // Every governed workspace is followed, and only the one that asked runs
    // the bot.
    let following = requested::Following {
        workspaces: workspaces.clone(),
        host: cert.clone(),
        root: owner_root(),
        state: scratch.path().join("host"),
        operator: std::collections::BTreeMap::from([(
            started.clone(),
            std::collections::BTreeSet::new(),
        )]),
        connectors: None,
    };
    let mut running = std::collections::BTreeMap::new();
    following.refresh(&mut running).await;
    assert_eq!(running.len(), 2, "each governed workspace has a follower");

    answered(
        &workspaces,
        &annex,
        &cert,
        annex_general.as_str(),
        "welcome!",
    )
    .await;
    assert!(
        posts(&workspaces, &started)
            .await
            .iter()
            .all(|post| post.text != "welcome!"),
        "the operator's workspace runs no bot it never asked for"
    );
    assert!(
        posts(&workspaces, &annex).await.iter().any(|post| post
            .who
            .as_ref()
            .and_then(|who| who.member_id())
            .is_some_and(MemberId::is_bot)),
        "the answer is the requested bot's"
    );

    // A workspace released is followed no more.
    workspaces.release(&annex).await.unwrap();
    following.refresh(&mut running).await;
    assert_eq!(running.len(), 1, "the released workspace is let go");
}
