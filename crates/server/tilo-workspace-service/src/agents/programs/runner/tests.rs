#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use chat::projection::ChatProjection as _;
use tilo_app_id::apps::CHAT;

use super::*;
use crate::agents::assistant;
use crate::agents::programs::host_steps::{
    HostStepError, HostSteps, Instruction, Judgement, Question,
};
use crate::workspaces::fixtures::{governor, test_host};
use serde_json::Value;
use tilo_action_policy::{Answer, KindSet, TreePath};
use tilo_actions::MemberId;

/// A workspace's events through the host's own actions.
mod events;
mod initiative;
/// The host's own answers for the programs it runs for a person.
mod judging;
/// A person's event that waits on a feed or a page outside.
mod outside;
/// A program's triggers at a time its person's calendars hold.
mod times;

/// `program`, carried out for `person`, with `assistant` taking what it
/// hands over and answering what it asks.
fn carried_out_for(
    program: ProgramRunner,
    person: String,
    assistant: &std::sync::Arc<Assistant>,
) -> ProgramRunner {
    program.runs_for(person, assistant.clone())
}

const MALLORY: [u8; 32] = [7; 32];
const ADA: [u8; 32] = [8; 32];
/// The greeter's id.
const GREETER: &str = "0b7e4c2a-9d1f-4e3b-8a6c-5f2d1e0c9b8a";
/// The ticker's id.
const TICKER: &str = "5e2c8a41-7b3d-4f69-a8c2-1d0e9b7a6f35";

/// A server that decides what members and events change against the surfaces
/// of the apps it serves, as a workspace server does.
pub(super) async fn event_server(
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
/// as a server whose device owns workspaces does.
pub(super) async fn event_server_taking_part(
    state: &std::path::Path,
    key: &ed25519_dalek::SigningKey,
    cert: &DeviceCert,
) -> crate::workspaces::fixtures::TestHost {
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

/// `program`'s runner in `workspace`, its progress kept in `state`.
fn program_runner(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &DeviceCert,
    program: Program,
    state: std::path::PathBuf,
) -> ProgramRunner {
    ProgramRunner::new(
        workspaces.clone(),
        workspace.to_owned(),
        cert.clone(),
        program,
        state,
    )
    .unwrap()
}

pub(super) use crate::workspaces::fixtures::chat::{chat_workspace, post_in, posts};

/// A program that reads the chat on every person's post.
fn greeter() -> Program {
    use tilo_automation::{ActorFilter, Shortcut, Step, StepKind, TriggerSpec};
    let trigger = TriggerSpec::ResourceChanged {
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
            StepKind::Read {
                app: CHAT.into(),
                record: String::new(),
                joined: false,
            },
        )],
        step_timeout_secs: None,
        output: None,
        grant: Vec::new(),
        triggers: vec![trigger],
        saved_by: None,
        saved_proof: None,
    };
    // Exactly what its steps and triggers need.
    let shortcut = Shortcut {
        grant: tilo_automation::grant::derived_rules(
            &shortcut,
            &tilo_automation::ActionCatalog::new(Vec::new()),
        ),
        ..shortcut
    };
    program_of(GREETER, &shortcut)
}

/// A program that reads the chat once every `seconds`, answering fire times
/// missed while its server was down as `catch_up` says.
fn ticker(seconds: u64, catch_up: tilo_automation::CatchUp) -> Program {
    on_the_clock(tilo_automation::TriggerSpec::Schedule {
        schedule: tilo_automation::Schedule::Interval { seconds },
        catch_up,
    })
}

/// A program that reads the chat once, at `at_ms`.
fn reminder(at_ms: i64) -> Program {
    on_the_clock(tilo_automation::TriggerSpec::Once { at_ms })
}

/// A program that reads the chat whenever `trigger`'s time comes.
fn on_the_clock(trigger: tilo_automation::TriggerSpec) -> Program {
    use tilo_automation::{Shortcut, Step, StepKind};
    let shortcut = Shortcut {
        id: "ticker".into(),
        name: "Ticker".into(),
        inputs: Vec::new(),
        steps: vec![Step::new(
            "answer",
            StepKind::Read {
                app: CHAT.into(),
                record: String::new(),
                joined: false,
            },
        )],
        step_timeout_secs: None,
        output: None,
        grant: Vec::new(),
        triggers: vec![trigger],
        saved_by: None,
        saved_proof: None,
    };
    // Exactly what its steps and triggers need.
    let shortcut = Shortcut {
        grant: tilo_automation::grant::derived_rules(
            &shortcut,
            &tilo_automation::ActionCatalog::new(Vec::new()),
        ),
        ..shortcut
    };
    program_of(TICKER, &shortcut)
}

/// The event `id` running `shortcut`.
fn program_of(id: &str, shortcut: &tilo_automation::Shortcut) -> Program {
    Program {
        id: id.into(),
        program: String::from_utf8(tilo_automation::program::canonical_bytes(shortcut).unwrap())
            .unwrap(),
        inputs: serde_json::Map::new(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_event_answers_a_post_in_the_workspace_once() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = event_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
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
                chat::Chat::CREATE_CHANNEL.with(chat::chat::chat::CreateChannel {
                    name: "general".parse().unwrap(),
                    category: None,
                    kind: None,
                    topic: None,
                }),
            )
            .map_err(|error| error.to_string())?;
            Ok(chat::projection::read(doc)
                .channel_rows()
                .into_iter()
                .find(|channel| channel.kind == chat::model::ChannelKind::Text)
                .expect("the chat has a text channel")
                .id)
        })
        .await
        .unwrap();
    let mut runner = program_runner(
        &workspaces,
        &workspace,
        &cert,
        greeter(),
        scratch.path().join("greeter.json"),
    );
    // Set up now: what happened before is not answered.
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
    assert!(
        runner.step().await.unwrap().is_empty(),
        "one answer to one post"
    );
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
    let server = event_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, general) = chat_workspace(&workspaces, &cert).await;
    let state = scratch.path().join("greeter.json");
    let start = || program_runner(&workspaces, &workspace, &cert, greeter(), state.clone());
    let mut runner = start();
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

    let mut runner = start();
    let reports = runner.step().await.unwrap();
    assert_eq!(
        reports.len(),
        2,
        "both posts while down are answered: {reports:?}"
    );
    assert!(runner.step().await.unwrap().is_empty());
    drop(runner);
    // Restarting again repeats nothing.
    assert!(start().step().await.unwrap().is_empty());
}

/// A time just past an hourly fire time, so a minute-long schedule's next
/// time is a minute away.
const AFTER_THE_HOUR_MS: u64 = 3_600_001;
/// The minute-long schedule's first fire time.
const FIRST_TICK_MS: u64 = 3_660_000;
/// Its second.
const SECOND_TICK_MS: u64 = 3_720_000;

#[tokio::test(flavor = "multi_thread")]
async fn a_scheduled_event_runs_at_its_time_once_and_again_at_the_next() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = event_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let manual = std::sync::Arc::new(tilo_clock::ManualClock::new(AFTER_THE_HOUR_MS));
    let mut runner = program_runner(
        &workspaces,
        &workspace,
        &cert,
        ticker(60, tilo_automation::CatchUp::Skip),
        scratch.path().join("ticker.json"),
    );
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
    let server = event_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let manual = std::sync::Arc::new(tilo_clock::ManualClock::new(AFTER_THE_HOUR_MS));

    // A schedule that catches up, down across a quarter hour of its times:
    // they run once between them.
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let state = scratch.path().join("ticker.json");
    let mut runner = program_runner(
        &workspaces,
        &workspace,
        &cert,
        ticker(60, tilo_automation::CatchUp::RunOnce),
        state.clone(),
    );
    runner.use_clock(manual.clone());
    assert!(runner.step().await.unwrap().is_empty());
    drop(runner);
    let back_ms = FIRST_TICK_MS + 15 * 60_000;
    manual.set(back_ms);
    let mut runner = program_runner(
        &workspaces,
        &workspace,
        &cert,
        ticker(60, tilo_automation::CatchUp::RunOnce),
        state,
    );
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
    drop(runner);
    let manual = std::sync::Arc::new(tilo_clock::ManualClock::new(AFTER_THE_HOUR_MS));

    // A moment that passed while its host was down: it fires once, however
    // late, where a schedule that skips would let the time go.
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let state = scratch.path().join("reminder.json");
    let at_ms = i64::try_from(FIRST_TICK_MS).unwrap();
    let mut runner = program_runner(
        &workspaces,
        &workspace,
        &cert,
        reminder(at_ms),
        state.clone(),
    );
    runner.use_clock(manual.clone());
    assert!(runner.step().await.unwrap().is_empty());
    drop(runner);
    let back_ms = FIRST_TICK_MS + 6 * 3_600_000;
    manual.set(back_ms);
    let mut runner = program_runner(
        &workspaces,
        &workspace,
        &cert,
        reminder(at_ms),
        state.clone(),
    );
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
    let mut runner = program_runner(&workspaces, &workspace, &cert, reminder(at_ms), state);
    runner.use_clock(manual.clone());
    assert!(
        runner.step().await.unwrap().is_empty(),
        "the next restart does not answer it again"
    );

    // A moment that had passed before its event was set up at all (the
    // server was down from the request on) fires in the first pass.
    let mut runner = program_runner(
        &workspaces,
        &workspace,
        &cert,
        reminder(at_ms + 1_000),
        scratch.path().join("set-up-late.json"),
    );
    runner.use_clock(manual.clone());
    assert_eq!(
        runner.step().await.unwrap().len(),
        1,
        "not run: {:?}",
        runner.dead_letters().collect::<Vec<_>>()
    );
    assert!(runner.step().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restarted_host_skips_the_times_it_missed() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = event_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let state = scratch.path().join("ticker.json");
    let manual = std::sync::Arc::new(tilo_clock::ManualClock::new(AFTER_THE_HOUR_MS));
    let mut runner = program_runner(
        &workspaces,
        &workspace,
        &cert,
        ticker(60, tilo_automation::CatchUp::Skip),
        state.clone(),
    );
    runner.use_clock(manual.clone());
    assert!(runner.step().await.unwrap().is_empty());
    drop(runner);

    let back_ms = FIRST_TICK_MS + 15 * 60_000;
    let manual = std::sync::Arc::new(tilo_clock::ManualClock::new(back_ms));
    let mut runner = program_runner(
        &workspaces,
        &workspace,
        &cert,
        ticker(60, tilo_automation::CatchUp::Skip),
        state,
    );
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

#[tokio::test(flavor = "multi_thread")]
async fn a_moment_fires_once_at_its_time_and_never_again() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = event_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let state = scratch.path().join("reminder.json");
    let at_ms = i64::try_from(FIRST_TICK_MS).unwrap();
    let manual = std::sync::Arc::new(tilo_clock::ManualClock::new(AFTER_THE_HOUR_MS));
    let mut runner = program_runner(
        &workspaces,
        &workspace,
        &cert,
        reminder(at_ms),
        state.clone(),
    );
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
    let mut runner = program_runner(&workspaces, &workspace, &cert, reminder(at_ms), state);
    runner.use_clock(manual.clone());
    assert!(
        runner.step().await.unwrap().is_empty(),
        "a restart answers no moment twice"
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
fn at_a_moment(at_ms: i64, steps: Vec<tilo_automation::Step>) -> Program {
    own_program(tilo_automation::TriggerSpec::Once { at_ms }, steps)
}

/// A person's own program `steps`, started by `trigger`, and told what fired
/// it and its own note.
fn own_program(
    trigger: tilo_automation::TriggerSpec,
    steps: Vec<tilo_automation::Step>,
) -> Program {
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
    // Exactly what its steps need.
    let shortcut = Shortcut {
        grant: tilo_automation::grant::derived_rules(
            &shortcut,
            &tilo_automation::ActionCatalog::new(Vec::new()),
        ),
        ..shortcut
    };
    program_of(TICKER, &shortcut)
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
fn reminding(at_ms: i64, instruction: &str) -> Program {
    at_a_moment(at_ms, vec![tell(instruction)])
}

/// A person's own program: at `at_ms`, ask their model `question` about
/// the moment, and hand `instruction` to their assistant only on a yes.
fn judging(at_ms: i64, question: &str, instruction: &str) -> Program {
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
    let server = event_server(scratch.path(), &key, &cert).await;
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
    let mut runner = program_runner(&workspaces, &workspace, &cert, program(), state.clone())
        .runs_for(ada.clone(), assistant.clone());
    runner.use_clock(manual.clone());
    assert!(runner.step().await.unwrap().is_empty());
    for tick in 1..=25_u64 {
        if tick == 5 || tick == 15 {
            // The host restarts between two firings: the count stands
            // where it stood.
            runner = program_runner(&workspaces, &workspace, &cert, program(), state.clone())
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
    use crate::agents::programs::host_steps::Keeps as _;
    use crate::agents::programs::kept::NoteChange;
    use tilo_automation::{Binding, Condition};
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = event_server(scratch.path(), &key, &cert).await;
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
    let mut runner = program_runner(&workspaces, &workspace, &cert, program(), state.clone())
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
    let mut runner = program_runner(&workspaces, &workspace, &cert, program(), state)
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
    let server = event_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let state = scratch.path().join("pausing.json");
    let ada = hex::encode(ADA);
    let assistant = Assistant::answering(vec![]);
    let manual = std::sync::Arc::new(tilo_clock::ManualClock::new(AFTER_THE_HOUR_MS));
    let program = || own_program(every_minute(), vec![tell("Say what is new.")]);
    let mut runner = program_runner(&workspaces, &workspace, &cert, program(), state.clone())
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
    let mut runner = program_runner(&workspaces, &workspace, &cert, program(), state)
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
    let server = event_server(scratch.path(), &key, &cert).await;
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
    let mut runner = program_runner(&workspaces, &workspace, &cert, program(), state.clone())
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
    let mut runner = program_runner(&workspaces, &workspace, &cert, program(), state)
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
    let server = event_server(scratch.path(), &key, &cert).await;
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
    let mut runner = program_runner(
        &workspaces,
        &workspace,
        &cert,
        judging(at_ms, question, "Say that the report is due."),
        scratch.path().join("yes.json"),
    )
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
    let mut runner = program_runner(
        &workspaces,
        &workspace,
        &cert,
        judging(at_ms + 60_000, question, "Say that the report is due."),
        scratch.path().join("no.json"),
    )
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
    let mut runner = program_runner(
        &workspaces,
        &workspace,
        &cert,
        judging(at_ms + 120_000, question, "Say that the report is due."),
        scratch.path().join("no-model.json"),
    )
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
    let server = event_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let state = scratch.path().join("reminder.json");
    let at_ms = i64::try_from(FIRST_TICK_MS).unwrap();
    let ada = hex::encode(ADA);
    let assistant = Assistant::answering(vec![]);
    let manual = std::sync::Arc::new(tilo_clock::ManualClock::new(AFTER_THE_HOUR_MS));
    let mut runner = program_runner(
        &workspaces,
        &workspace,
        &cert,
        reminding(at_ms, "Say that it is time to call home."),
        state.clone(),
    )
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
    let mut runner = program_runner(
        &workspaces,
        &workspace,
        &cert,
        reminding(at_ms, "Say that it is time to call home."),
        state,
    )
    .runs_for(ada, assistant.clone());
    runner.use_clock(manual.clone());
    assert!(runner.step().await.unwrap().is_empty());
    assert_eq!(assistant.taken().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_instruction_nothing_takes_is_handed_over_again_or_kept_as_a_dead_letter() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = event_server(scratch.path(), &key, &cert).await;
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
    let mut runner = program_runner(
        &workspaces,
        &workspace,
        &cert,
        reminding(at_ms, "Say that it is time to call home."),
        scratch.path().join("reminder.json"),
    )
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
    let mut runner = program_runner(
        &workspaces,
        &workspace,
        &cert,
        reminding(at_ms + 60_000, "Say it again."),
        scratch.path().join("refused.json"),
    )
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

    // A program that runs for nobody is offered no such step, as before.
    manual.set(FIRST_TICK_MS + 120_000);
    let mut runner = program_runner(
        &workspaces,
        &workspace,
        &cert,
        reminding(at_ms + 120_000, "Say hello."),
        scratch.path().join("nobody.json"),
    );
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
    let server = event_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, general) = chat_workspace(&workspaces, &cert).await;
    let state = scratch.path().join("greeter.json");
    // It hands its answer to an assistant, and nobody it runs for keeps one.
    let answering = || {
        own_program(
            tilo_automation::TriggerSpec::ResourceChanged {
                kind: "chat.messages".into(),
                actions: vec!["post".into()],
                fields: Vec::new(),
                by: tilo_automation::ActorFilter::People,
            },
            vec![tell("Say welcome.")],
        )
    };
    let start = || program_runner(&workspaces, &workspace, &cert, answering(), state.clone());
    let mut runner = start();
    assert!(runner.step().await.unwrap().is_empty());
    ada_says(&workspaces, &workspace, &cert, general.as_str(), "hello").await;
    let reports = runner.step().await.unwrap();
    assert!(
        matches!(reports[..], [ref report] if matches!(report.outcome, tilo_automation::InvocationOutcome::Failed { .. })),
        "{reports:?}"
    );
    drop(runner);

    let runner = start();
    let dead: Vec<_> = runner.dead_letters().collect();
    assert_eq!(dead.len(), 1, "kept across a restart");
    assert_eq!(dead[0].step.as_deref(), Some("remind"));
}

/// An event changes no record: a run whose program posts in the chat is
/// refused, and nothing of it lands in the workspace.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_that_changes_a_record_is_refused_and_lands_nothing() {
    use tilo_automation::{ActorFilter, Binding, Selector, Shortcut, Step, StepKind, TriggerSpec};
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = event_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, general) = chat_workspace(&workspaces, &cert).await;
    let trigger = TriggerSpec::ResourceChanged {
        kind: "chat.messages".into(),
        actions: vec!["post".into()],
        fields: Vec::new(),
        by: ActorFilter::People,
    };
    let shortcut = Shortcut {
        id: "poster".into(),
        name: "Poster".into(),
        inputs: trigger.payload().to_vec(),
        steps: vec![Step::new(
            "answer",
            StepKind::Invoke {
                action: tilo_actions::ActionKey::new(CHAT, "chat.messages", "post"),
                resource: Selector::FirstOfKind("chat.messages".into()),
                args: [
                    (
                        "channel".to_owned(),
                        Binding::Literal(general.clone().into()),
                    ),
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
    let shortcut = Shortcut {
        grant: tilo_automation::grant::derived_rules(
            &shortcut,
            &tilo_automation::ActionCatalog::new(Vec::new()),
        ),
        ..shortcut
    };
    let mut runner = program_runner(
        &workspaces,
        &workspace,
        &cert,
        program_of(GREETER, &shortcut),
        scratch.path().join("poster.json"),
    );
    assert!(runner.step().await.unwrap().is_empty());
    ada_says(&workspaces, &workspace, &cert, general.as_str(), "hello").await;
    assert!(
        runner.step().await.unwrap().is_empty(),
        "a refused run reports nothing"
    );
    assert!(
        !posts(&workspaces, &workspace)
            .await
            .iter()
            .any(|post| post.text == "welcome!"),
        "nothing it changed reached the chat"
    );
}

/// An event's run reads the records it follows through the record API: Chat's
/// record of the workspace and its messages' chain, with what was just posted.
#[tokio::test(flavor = "multi_thread")]
async fn an_event_step_reads_the_records_it_follows_through_the_record_api() {
    use tilo_automation::{ActorFilter, Shortcut, Step, StepKind, TriggerSpec};
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = event_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, general) = chat_workspace(&workspaces, &cert).await;
    let trigger = TriggerSpec::ResourceChanged {
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
    let mut runner = program_runner(
        &workspaces,
        &workspace,
        &cert,
        program_of("3c9a7e10-5b2d-4f8e-a1c6-0d4e2b7f9a35", &shortcut),
        scratch.path().join("reader.json"),
    );
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
    assert!(
        chat["records"][0]["value"]["channels"]
            .as_object()
            .is_some_and(|channels| channels.contains_key(&general)),
        "{chat}"
    );
    let messages = read("messages").to_string();
    assert!(messages.contains("what is here?"), "{messages}");
}

#[test]
fn membership_changes_fire_a_programs_workspace_triggers() {
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
    let events = governance_events("r1", "owner", 7, &before, &after);
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
    assert!(governance_events("r1", "owner", 7, &after, &after).is_empty());

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
