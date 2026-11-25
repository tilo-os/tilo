//! A trigger fires a saved shortcut once workspace-wide: the device whose turn it is starts it
//! and writes where it got to in the workspace, and no device starts it again.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use automerge::transaction::{CommitOptions, Transactable as _};
use serde_json::json;
use tilo_automation::{
    Binding, CatchUp, InvocationMode, InvocationOutcome, InvocationReport, Schedule, Selector,
    Step, StepEntry, StepFailure, StepKind, StepOutcome, StepPath, StepStatus,
};
use tilo_peer_state::workspace_membership::{SettingsChange, WorkspaceMembershipCommand};

use super::*;
use crate::workspaces::fixtures::{Started, governor, on_issue_change, save_signed, test_host};

tilo_test_env::scenario! {
    fn a_committed_change_is_a_record_event_only_with_its_entry;

    phase a_committed_verb_becomes_a_record_event_with_its_actor_and_facet;
    {
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, "seed", 1).unwrap();
        doc.commit();
        let before = doc.get_heads();
        let context = ActionContext::for_app("so.tilo.chat", "Chat")
            .with_invoker(tilo_actions::ActionInvoker::person("bob"))
            .with_described(tilo_actions::Described::new(tilo_actions::Did::verb(
                tilo_actions::ActionKey::new("so.tilo.chat", "chat.workspace", "post"),
                [],
            )))
            .with_facet(tilo_actions::ActionFacet::new(
                tilo_actions::INCOMING_MESSAGE_FACET,
                "true",
            ));
        doc.put(automerge::ROOT, "text", "hi").unwrap();
        let hash = doc
            .commit_with(CommitOptions::default().with_message(context.change_message().unwrap()))
            .unwrap();
        let record = ("workspace-1", "rec-1", "chat.workspace");
        let events = record_events(record, &doc, Some(&before), 0);
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.change, hash.to_string());
        assert_eq!(event.actor, tilo_actions::Acting::person("bob"));
        assert!(event.incoming_message);
        assert!(
            record_events(record, &doc, Some(&doc.committed_heads()), 0).is_empty(),
            "nothing new after the last look"
        );
        assert_eq!(
            record_events(record, &doc, None, 0).len(),
            1,
            "a record's first look reports the changes made since it was watched"
        );
        assert!(
            record_events(record, &doc, None, i64::MAX).is_empty(),
            "and none made before"
        );
    }

    phase a_change_without_an_entry_is_no_event;
    {
        let mut doc = AutoCommit::new();
        let before = doc.get_heads();
        doc.put(automerge::ROOT, "x", 1).unwrap();
        doc.commit();
        assert!(record_events(("r", "rec", "k"), &doc, Some(&before), 0).is_empty());
    }
}

/// A shortcut saved with a moment that has come fires on the device that saved it, once: the
/// progress it wrote in the workspace stops every later look, its own included, from firing it
/// again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_moment_that_came_fires_its_shortcut_once_workspace_wide() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let host = test_host(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Home").await.unwrap().workspace;
    let mut shortcut: Shortcut = serde_json::from_value(json!({
        "id": "tidy",
        "name": "Tidy",
        "steps": [],
        "triggers": [{"trigger": "once", "at_ms": tilo_clock::unix_now_ms_i64() - 1_000}],
    }))
    .unwrap();
    tilo_peer_state::workspace::sign_saved_shortcut(&workspace, &mut shortcut, &cert, &key)
        .unwrap();
    workspaces
        .commit_own_membership(
            &workspace,
            WorkspaceMembershipCommand::Settings(SettingsChange::Shortcuts(
                tilo_automation::ShortcutChange::Save(Box::new(shortcut)),
            )),
            None,
        )
        .await
        .unwrap();

    let started = Arc::new(Started::default());
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    runner.pass(Vec::new()).await;
    runner.pass(Vec::new()).await;
    let firings = started.0.lock().unwrap().clone();
    assert_eq!(firings.len(), 1, "{firings:?}");
    assert_eq!(firings[0].shortcut, "tidy");
    assert_eq!(firings[0].saver, hex::encode(cert.user_public_key));
    let mut again = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    again.arm().await;
    again.pass(Vec::new()).await;
    assert_eq!(
        started.0.lock().unwrap().len(),
        1,
        "a device that starts over reads the progress in the workspace and fires nothing again"
    );
    assert!(again.waiting().is_empty());
    host.shutdown().await;
}

/// A shortcut whose trigger is an event the Discord connector reports in one channel.
fn on_connector_event() -> Shortcut {
    serde_json::from_value(json!({
        "id": "answer",
        "name": "Answer",
        "steps": [],
        "inputs": [{
            "name": "event",
            "ty": "object",
            "required": true,
            "description": "What the connector reported",
        }],
        "triggers": [{
            "trigger": "connector",
            "app_id": "so.tilo.discord",
            "source": "message-created",
            "filter": {"channel": "42"},
        }],
    }))
    .unwrap()
}

/// What a test's connector says it reported, as the runner reads it.
#[derive(Default)]
struct Reports(Mutex<std::collections::BTreeMap<String, Vec<ReportedEvent>>>);

impl Reports {
    fn report(&self, app: &str, event: ReportedEvent) {
        self.0
            .lock()
            .unwrap()
            .entry(app.to_owned())
            .or_default()
            .push(event);
    }
}

impl ConnectorEvents for Reports {
    fn since(&self, app: &str, cursor: u64) -> Vec<ReportedEvent> {
        self.0
            .lock()
            .unwrap()
            .get(app)
            .map(|events| {
                events
                    .iter()
                    .filter(|event| event.seq > cursor)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// A connector's event fires the shortcut watching its app, source and fields, on the device
/// whose connector reported it, and only once: an event of another source or holding another
/// value in a filtered field fires nothing, and looking again at the same log fires no second run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connector_event_fires_the_shortcut_watching_it_once() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let host = test_host(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Discord").await.unwrap().workspace;
    save_signed(workspaces, &workspace, on_connector_event(), (&key, &cert)).await;

    let reports = Arc::new(Reports::default());
    let started = Arc::new(Started::default());
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    )
    .watching_connectors(Arc::clone(&reports) as Arc<dyn ConnectorEvents>);
    runner.arm().await;

    reports.report(
        "so.tilo.discord",
        ReportedEvent {
            seq: 1,
            source: "message-created".into(),
            id: "1188".into(),
            at_ms: 5,
            payload: json!({"channel": "42", "content": "yo"})
                .as_object()
                .unwrap()
                .clone(),
        },
    );
    runner.pass(Vec::new()).await;
    runner.pass(Vec::new()).await;
    let firings = started.0.lock().unwrap().clone();
    assert_eq!(firings.len(), 1, "{firings:?}");
    assert_eq!(firings[0].shortcut, "answer");
    assert_eq!(firings[0].inputs["event"]["payload"]["content"], "yo");

    reports.report(
        "so.tilo.discord",
        ReportedEvent {
            seq: 2,
            source: "message-edited".into(),
            id: "1189".into(),
            at_ms: 6,
            payload: json!({"channel": "42", "content": "edited"})
                .as_object()
                .unwrap()
                .clone(),
        },
    );
    reports.report(
        "so.tilo.discord",
        ReportedEvent {
            seq: 3,
            source: "message-created".into(),
            id: "1190".into(),
            at_ms: 7,
            payload: json!({"channel": "7", "content": "elsewhere"})
                .as_object()
                .unwrap()
                .clone(),
        },
    );
    reports.report(
        "so.tilo.other",
        ReportedEvent {
            seq: 1,
            source: "message-created".into(),
            id: "1".into(),
            at_ms: 8,
            payload: json!({"channel": "42"}).as_object().unwrap().clone(),
        },
    );
    runner.pass(Vec::new()).await;
    runner.pass(Vec::new()).await;
    assert_eq!(
        started.0.lock().unwrap().len(),
        1,
        "a source, a channel or an app the trigger does not name fires nothing"
    );
    host.shutdown().await;
}

/// Only the shortcuts the device's own person saved arm here: one another person's device saved
/// in the same workspace fires nothing on this device.
#[test]
fn a_shortcut_saved_by_someone_elses_device_arms_nothing_here() {
    let (key, cert) = governor();
    let id = tilo_desktop_model::WorkspaceId::new();
    let workspace = id.to_string();
    let workspace = workspace.as_str();
    let mut shortcut = on_issue_change();
    tilo_peer_state::workspace::sign_saved_shortcut(workspace, &mut shortcut, &cert, &key).unwrap();
    let mut described = tilo_desktop_model::Workspace::new(id, "Home");
    described
        .shortcuts
        .push(tilo_automation::SavedShortcut::encode(&shortcut));
    let saver = hex::encode(cert.user_public_key);
    let now = tilo_clock::unix_now_ns();
    let none = tilo_user_identity::RevocationList::default;
    assert_eq!(
        armed(
            std::slice::from_ref(&described),
            |_| Some(none()),
            |_| saver.clone(),
            false,
            now
        )
        .len(),
        1,
        "the saver's own device arms it"
    );
    assert!(
        armed(
            &[described],
            |_| Some(none()),
            |_| "someone else".to_owned(),
            false,
            now
        )
        .is_empty(),
        "another person's device arms nothing"
    );
}

/// Where a schedule got to is the workspace's, not the device's: the device's service restarts,
/// reads the fire time it wrote before, and fires the shortcut no second time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_schedules_fire_time_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let keys = Arc::new(crate::workspaces::fixtures::TestGroupKeys::default());
    let (key, cert) = governor();
    let host =
        crate::workspaces::fixtures::test_host_holding(dir.path(), &key, &cert, Arc::clone(&keys))
            .await;
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Home").await.unwrap().workspace;
    let mut shortcut: Shortcut = serde_json::from_value(json!({
        "id": "tidy",
        "name": "Tidy",
        "steps": [],
        "triggers": [{"trigger": "once", "at_ms": tilo_clock::unix_now_ms_i64() - 1_000}],
    }))
    .unwrap();
    let mark = tilo_peer_state::trigger_mark(&shortcut.triggers[0]);
    tilo_peer_state::workspace::sign_saved_shortcut(&workspace, &mut shortcut, &cert, &key)
        .unwrap();
    workspaces
        .commit_own_membership(
            &workspace,
            WorkspaceMembershipCommand::Settings(SettingsChange::Shortcuts(
                tilo_automation::ShortcutChange::Save(Box::new(shortcut)),
            )),
            None,
        )
        .await
        .unwrap();
    let started = Arc::new(Started::default());
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    runner.pass(Vec::new()).await;
    runner.pass(Vec::new()).await;
    assert_eq!(
        started.0.lock().unwrap().len(),
        1,
        "the moment came: it fired"
    );
    let fired = workspaces
        .schedule_answered(&workspace, "tidy", 0, &mark)
        .await
        .unwrap();
    assert!(fired.is_some(), "its fire time is written in the workspace");
    drop(runner);
    host.shutdown().await;

    let host = crate::workspaces::fixtures::test_host_holding(dir.path(), &key, &cert, keys).await;
    let workspaces = host.workspaces();
    assert_eq!(
        workspaces
            .schedule_answered(&workspace, "tidy", 0, &mark)
            .await
            .unwrap(),
        fired,
        "the restarted service reads the fire time it wrote"
    );
    let mut again = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    again.arm().await;
    again.pass(Vec::new()).await;
    assert_eq!(
        started.0.lock().unwrap().len(),
        1,
        "the shortcut does not fire again after the restart"
    );
    host.shutdown().await;
}

/// The turn is told the files its cause carries: their names and the address the file-read
/// tool reads them at, so the agent reads what the person sent.
#[cfg(feature = "agents")]
#[test]
fn a_cause_tells_the_files_its_item_carries() {
    use crate::agents::triggered;

    let event: DocumentEvent = serde_json::from_value(json!({
        "workspace": "w",
        "record": "general",
        "app": "so.tilo.chat",
        "kind": "tl.so.tilo.chat.Message",
        "change": "c0ffee",
        "actor": {"by": {"member": {"person": "ada"}}},
        "did": {"did": "verb", "action": {"app": "so.tilo.chat", "kind": "tl.so.tilo.chat.Message", "action": "send"}},
        "item": {"kind": "tl.so.tilo.chat.Message", "id": "m-1", "label": ""},
        "after": {
            "body": {"blocks": [{"text": "read this", "marks": []}]},
            "$held": [{
                "$kind": "tl.so.tilo.chat.Attached",
                "name": "plan.txt",
                "media": "text/plain",
                "size": 12,
                "blob": "abc123",
            }],
        },
    }))
    .unwrap();
    let said = triggered::said(&event);
    assert!(said.contains("read this"), "{said}");
    assert!(
        said.contains("tilo.content:abc123") && said.contains("files#read-text"),
        "the turn is told where to read the file: {said}"
    );
}

/// A wall clock this thread follows, so the runner reads the time the test pins; the real clock
/// is back once the test ends.
struct Pinned(tilo_clock::testing::SharedWall);

impl Pinned {
    #[must_use]
    fn at(now_ms: u64) -> Self {
        let wall = tilo_clock::testing::SharedWall::default();
        wall.follow();
        let pinned = Self(wall);
        pinned.set(now_ms);
        pinned
    }

    fn set(&self, now_ms: u64) {
        let Some(now) = UNIX_EPOCH.checked_add(Duration::from_millis(now_ms)) else {
            unreachable!("a near time");
        };
        self.0.pin(now);
    }
}

impl Drop for Pinned {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// The next fire time of a minute-long schedule after `now_ms`.
fn next_tick(now_ms: u64) -> u64 {
    now_ms
        .div_euclid(60_000)
        .saturating_add(1)
        .saturating_mul(60_000)
}

fn every_minute(catch_up: CatchUp) -> TriggerSpec {
    TriggerSpec::Schedule {
        schedule: Schedule::Interval { seconds: 60 },
        catch_up,
    }
}

fn at_a_moment(at_ms: i64) -> TriggerSpec {
    TriggerSpec::Once { at_ms }
}

/// A shortcut `id` with one trigger and its steps.
fn shortcut(id: &str, trigger: TriggerSpec, steps: Vec<Step>) -> Shortcut {
    Shortcut {
        id: id.into(),
        name: id.into(),
        inputs: trigger.payload().to_vec(),
        steps,
        step_timeout_secs: None,
        output: None,
        grant: Vec::new(),
        triggers: vec![trigger],
        saved_by: None,
        saved_proof: None,
        paused: false,
    }
}

/// The step that takes the library's count of a run, reaching every `every` runs.
fn count_step(every: u64) -> Step {
    Step::new(
        "count",
        StepKind::Invoke {
            action: tilo_actions::ActionKey::new(
                tilo_automation::SHORTCUTS_APP_ID,
                tilo_automation::LIBRARY_RESOURCE_KIND,
                tilo_automation::COUNT_ACTION,
            ),
            resource: Selector::FirstOfKind(tilo_automation::LIBRARY_RESOURCE_KIND.into()),
            args: BTreeMap::from([("every".to_owned(), Binding::Literal(json!(every)))]),
            appearance: None,
        },
    )
}

/// Let the runner look twice: the first look takes a due fire time's turn and starts its run,
/// the second reads how that run ended and keeps the firing done.
async fn run_once(runner: &mut Runner) {
    runner.pass(Vec::new()).await;
    runner.pass(Vec::new()).await;
}

/// What the firings of shortcut `id` in `workspace` were kept as.
async fn ended_rows(workspaces: &Workspaces, workspace: &str, id: &str) -> Vec<Ended> {
    workspaces
        .leases_done_under(workspace, &crate::apps::automation::firings_of(id))
        .await
        .unwrap()
        .into_values()
        .map(|answer| serde_json::from_value(answer).unwrap())
        .collect()
}

/// How a run a test's broker started ended.
#[derive(Clone)]
enum Ending {
    Ran,
    Failed {
        step: &'static str,
        failure: StepFailure,
    },
}

/// `firing`'s run `run` as the run engine reports it, taking `counted_step` when one is named.
fn ended_report(
    firing: &Firing,
    run: AutomationInvocationId,
    ending: &Ending,
    counted_step: Option<&str>,
) -> InvocationReport {
    let path = StepPath::root().child(0);
    let mut steps = Vec::new();
    if let Some(step) = counted_step {
        steps.push(StepEntry {
            path: path.clone(),
            step: step.to_owned(),
            invocation: None,
            attempts: Vec::new(),
            degraded: StepOutcome::None,
            status: StepStatus::Succeeded { output: None },
        });
    }
    let outcome = match ending {
        Ending::Ran => InvocationOutcome::Completed,
        Ending::Failed { step, failure } => {
            steps.push(StepEntry {
                path: path.clone(),
                step: (*step).to_owned(),
                invocation: None,
                attempts: Vec::new(),
                degraded: StepOutcome::None,
                status: StepStatus::Failed {
                    failure: failure.clone(),
                    attempt: 1,
                    recovered: tilo_automation::Recovered::Stopped,
                },
            });
            InvocationOutcome::Failed {
                path,
                failure: failure.clone(),
            }
        }
    };
    InvocationReport {
        invocation: run,
        shortcut: firing.shortcut.clone(),
        version: None,
        trigger: firing.trigger.kind().into(),
        mode: InvocationMode::Unattended,
        steps,
        output: None,
        degraded: Vec::new(),
        outcome,
    }
}

/// A broker that answers each start and each run's end as a test scripts it.
#[derive(Default)]
struct Scripted {
    /// What each start answers, in order; `Ok` once the script is spent.
    starts: Mutex<VecDeque<Result<(), ActionCallError>>>,
    /// How each run ends, in order; ran once the script is spent.
    endings: Mutex<VecDeque<Ending>>,
    /// The ending of every run it started, by run.
    runs: Mutex<BTreeMap<AutomationInvocationId, Ending>>,
    /// Every firing it started, in order.
    started: Mutex<Vec<Firing>>,
}

impl Scripted {
    fn starting(starts: Vec<Result<(), ActionCallError>>, endings: Vec<Ending>) -> Arc<Self> {
        Arc::new(Self {
            starts: Mutex::new(starts.into()),
            endings: Mutex::new(endings.into()),
            ..Self::default()
        })
    }

    fn started(&self) -> Vec<Firing> {
        self.started.lock().unwrap().clone()
    }
}

impl StartRun for Scripted {
    fn start(&self, firing: &Firing, run: AutomationInvocationId) -> Result<u64, ActionCallError> {
        self.starts.lock().unwrap().pop_front().unwrap_or(Ok(()))?;
        let ending = self
            .endings
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ending::Ran);
        self.runs.lock().unwrap().insert(run, ending);
        let mut started = self.started.lock().unwrap();
        started.push(firing.clone());
        Ok(u64::try_from(started.len()).unwrap())
    }

    fn ended(&self, firing: &Firing, run: AutomationInvocationId) -> Option<InvocationReport> {
        let ending = self.runs.lock().unwrap().get(&run).cloned()?;
        Some(ended_report(firing, run, &ending, None))
    }
}

/// A broker standing in for a counted shortcut's run: it reads how many of the shortcut's runs
/// counted in the workspace, as the library's count step does, and keeps what each run reached.
struct Counting {
    /// The count step's multiple.
    every: u64,
    /// The step its runs take for the count.
    step: String,
    /// The counted runs the workspace held when the last run started, as the test read them.
    counted: AtomicU64,
    /// Every run it started: its firing, the count it reached, and whether that was a multiple.
    started: Mutex<Vec<(Firing, u64, bool)>>,
}

impl Counting {
    fn reaching(every: u64, step: &str) -> Arc<Self> {
        Arc::new(Self {
            every,
            step: step.to_owned(),
            counted: AtomicU64::new(0),
            started: Mutex::new(Vec::new()),
        })
    }

    /// The counted runs the workspace held before the next pass.
    fn seeing(&self, counted: u64) {
        self.counted.store(counted, Ordering::SeqCst);
    }

    fn counts_reached(&self) -> Vec<u64> {
        self.started
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, _, reached)| *reached)
            .map(|(_, count, _)| *count)
            .collect()
    }
}

impl StartRun for Counting {
    fn start(&self, firing: &Firing, _: AutomationInvocationId) -> Result<u64, ActionCallError> {
        let count = self.counted.load(Ordering::SeqCst).saturating_add(1);
        let mut started = self.started.lock().unwrap();
        started.push((firing.clone(), count, count.is_multiple_of(self.every)));
        Ok(u64::try_from(started.len()).unwrap())
    }

    fn ended(&self, firing: &Firing, run: AutomationInvocationId) -> Option<InvocationReport> {
        Some(ended_report(firing, run, &Ending::Ran, Some(&self.step)))
    }
}

/// A schedule saved in the workspace fires at its first time and again at the next, once per
/// time, on the device that saved it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scheduled_shortcut_runs_at_its_time_and_again_at_the_next() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let host = test_host(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Clock").await.unwrap().workspace;
    let trigger = every_minute(CatchUp::Skip);
    save_signed(
        workspaces,
        &workspace,
        shortcut("ticker", trigger.clone(), Vec::new()),
        (&key, &cert),
    )
    .await;
    let now = tilo_clock::unix_now_ms();
    let first = next_tick(now);
    let clock = Pinned::at(now);
    let started = Arc::new(Started::default());
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    runner.pass(Vec::new()).await;
    assert!(
        started.0.lock().unwrap().is_empty(),
        "its first time has not come"
    );
    clock.set(first - 1);
    runner.pass(Vec::new()).await;
    assert!(
        started.0.lock().unwrap().is_empty(),
        "not a millisecond early"
    );
    clock.set(first);
    run_once(&mut runner).await;
    let firings = started.0.lock().unwrap().clone();
    assert_eq!(firings.len(), 1, "{firings:?}");
    assert_eq!(firings[0].shortcut, "ticker");
    assert_eq!(firings[0].trigger, trigger);
    assert_eq!(
        ended_rows(workspaces, &workspace, "ticker").await.len(),
        1,
        "its one run is kept done"
    );
    runner.pass(Vec::new()).await;
    assert_eq!(started.0.lock().unwrap().len(), 1, "one run per fire time");
    clock.set(first + 60_000);
    run_once(&mut runner).await;
    assert_eq!(
        started.0.lock().unwrap().len(),
        2,
        "the next time runs again"
    );
    host.shutdown().await;
}

/// A host that starts again fires what it missed once, whether a schedule that catches up or a
/// moment that passed while it was down, and nothing twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario, end to end, reads as one test"
)]
async fn a_restarted_host_fires_what_it_missed_once_whether_a_schedule_or_a_moment() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let host = test_host(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Catch up").await.unwrap().workspace;
    let now = tilo_clock::unix_now_ms();
    let first = next_tick(now);
    let clock = Pinned::at(now);

    // A schedule that catches up, down across a quarter hour of its times: they run once
    // between them.
    save_signed(
        workspaces,
        &workspace,
        shortcut("ticker", every_minute(CatchUp::RunOnce), Vec::new()),
        (&key, &cert),
    )
    .await;
    let started = Arc::new(Started::default());
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    runner.pass(Vec::new()).await;
    assert!(started.0.lock().unwrap().is_empty());
    drop(runner);
    let back = first + 15 * 60_000;
    clock.set(back);
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    run_once(&mut runner).await;
    assert_eq!(
        started.0.lock().unwrap().len(),
        1,
        "once for however many times it missed"
    );
    runner.pass(Vec::new()).await;
    assert_eq!(started.0.lock().unwrap().len(), 1);
    clock.set(back + 60_000);
    run_once(&mut runner).await;
    assert_eq!(
        started.0.lock().unwrap().len(),
        2,
        "the next time comes as scheduled"
    );
    drop(runner);

    // A moment that passed while its host was down fires once, however late.
    let at_ms = i64::try_from(first).unwrap();
    save_signed(
        workspaces,
        &workspace,
        shortcut("reminder", at_a_moment(at_ms), Vec::new()),
        (&key, &cert),
    )
    .await;
    let started = Arc::new(Started::default());
    clock.set(back);
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    run_once(&mut runner).await;
    assert_eq!(
        started.0.lock().unwrap().len(),
        1,
        "its moment, however late"
    );
    runner.pass(Vec::new()).await;
    assert_eq!(started.0.lock().unwrap().len(), 1);
    drop(runner);
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    runner.pass(Vec::new()).await;
    assert_eq!(
        started.0.lock().unwrap().len(),
        1,
        "the next restart does not answer it again"
    );
    drop(runner);

    // A moment that had passed before its shortcut was set up at all fires in the first pass.
    save_signed(
        workspaces,
        &workspace,
        shortcut("late", at_a_moment(at_ms.saturating_add(1_000)), Vec::new()),
        (&key, &cert),
    )
    .await;
    let started = Arc::new(Started::default());
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    run_once(&mut runner).await;
    assert_eq!(
        started.0.lock().unwrap().len(),
        1,
        "a moment seen first after it passed fires at once"
    );
    runner.pass(Vec::new()).await;
    assert_eq!(started.0.lock().unwrap().len(), 1);
    host.shutdown().await;
}

/// A schedule that skips its missed times lets a quarter hour of them go, and runs the next.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restarted_host_skips_the_times_it_missed() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let host = test_host(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Skip").await.unwrap().workspace;
    save_signed(
        workspaces,
        &workspace,
        shortcut("ticker", every_minute(CatchUp::Skip), Vec::new()),
        (&key, &cert),
    )
    .await;
    let now = tilo_clock::unix_now_ms();
    let first = next_tick(now);
    let clock = Pinned::at(now);
    let started = Arc::new(Started::default());
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    runner.pass(Vec::new()).await;
    assert!(started.0.lock().unwrap().is_empty());
    drop(runner);

    let back = first + 15 * 60_000;
    clock.set(back);
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    run_once(&mut runner).await;
    assert!(
        started.0.lock().unwrap().is_empty(),
        "a time missed while down is let go"
    );
    clock.set(back + 60_000);
    run_once(&mut runner).await;
    assert_eq!(
        started.0.lock().unwrap().len(),
        1,
        "the next time is the one that runs"
    );
    host.shutdown().await;
}

/// A moment fires once at its time, never a millisecond early, never again however long the host
/// runs, and no restart answers it twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_moment_fires_once_at_its_time_and_never_again() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let host = test_host(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Reminder").await.unwrap().workspace;
    let now = tilo_clock::unix_now_ms();
    let first = next_tick(now);
    let at_ms = i64::try_from(first).unwrap();
    save_signed(
        workspaces,
        &workspace,
        shortcut("reminder", at_a_moment(at_ms), Vec::new()),
        (&key, &cert),
    )
    .await;
    let clock = Pinned::at(now);
    let started = Arc::new(Started::default());
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    runner.pass(Vec::new()).await;
    assert!(
        started.0.lock().unwrap().is_empty(),
        "its moment has not come"
    );
    clock.set(first - 1);
    runner.pass(Vec::new()).await;
    assert!(
        started.0.lock().unwrap().is_empty(),
        "not a millisecond early"
    );
    clock.set(first);
    run_once(&mut runner).await;
    assert_eq!(started.0.lock().unwrap().len(), 1, "its moment, once");
    runner.pass(Vec::new()).await;
    assert_eq!(started.0.lock().unwrap().len(), 1, "a moment comes once");
    clock.set(first + 24 * 3_600_000);
    run_once(&mut runner).await;
    assert_eq!(
        started.0.lock().unwrap().len(),
        1,
        "and never again, however long the host runs"
    );
    drop(runner);

    // A restarted host reads that it answered the moment.
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    run_once(&mut runner).await;
    assert_eq!(
        started.0.lock().unwrap().len(),
        1,
        "a restart answers no moment twice"
    );
    host.shutdown().await;
}

/// A paused shortcut fires nothing and its times pass unanswered, across a restart too; resumed,
/// it runs the next time and not the ones it let go.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_paused_shortcut_fires_nothing_until_it_is_resumed() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let host = test_host(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Pausing").await.unwrap().workspace;
    let mut paused = shortcut("ticker", every_minute(CatchUp::Skip), Vec::new());
    paused.paused = true;
    save_signed(workspaces, &workspace, paused, (&key, &cert)).await;
    let now = tilo_clock::unix_now_ms();
    let first = next_tick(now);
    let clock = Pinned::at(now);
    let started = Arc::new(Started::default());
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    runner.pass(Vec::new()).await;
    for tick in 1..=2_u64 {
        clock.set(first + (tick - 1) * 60_000);
        run_once(&mut runner).await;
        assert!(
            started.0.lock().unwrap().is_empty(),
            "paused at its time {tick}"
        );
    }
    drop(runner);
    // It stays paused across a restart.
    clock.set(first + 2 * 60_000);
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    run_once(&mut runner).await;
    assert!(started.0.lock().unwrap().is_empty(), "still paused");
    assert!(
        ended_rows(workspaces, &workspace, "ticker")
            .await
            .is_empty()
    );

    save_signed(
        workspaces,
        &workspace,
        shortcut("ticker", every_minute(CatchUp::Skip), Vec::new()),
        (&key, &cert),
    )
    .await;
    runner.arm().await;
    // The times that came while it was paused were answered there: resuming runs the next one.
    run_once(&mut runner).await;
    assert!(
        started.0.lock().unwrap().is_empty(),
        "what came while it was paused is let go"
    );
    clock.set(first + 3 * 60_000);
    run_once(&mut runner).await;
    assert_eq!(started.0.lock().unwrap().len(), 1, "it goes on");
    assert_eq!(
        ended_rows(workspaces, &workspace, "ticker").await.len(),
        1,
        "one run since it went on"
    );
    host.shutdown().await;
}

/// A counted shortcut's runs are kept in the workspace as counted, one per fire time, so its
/// count carries on after a restart and reaches its multiple at the tenth and twentieth run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_counted_shortcut_keeps_its_count_in_the_workspace_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let host = test_host(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Counting").await.unwrap().workspace;
    let counted = shortcut("counted", every_minute(CatchUp::Skip), vec![count_step(10)]);
    assert!(
        crate::apps::automation::counts(&counted, "count"),
        "the run's count step is the library's"
    );
    save_signed(workspaces, &workspace, counted, (&key, &cert)).await;
    let now = tilo_clock::unix_now_ms();
    let first = next_tick(now);
    let clock = Pinned::at(now);
    let broker = Counting::reaching(10, "count");
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&broker) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    // The first look keeps where the schedule stands; the times then come one a minute.
    runner.pass(Vec::new()).await;
    for tick in 1..=25_u64 {
        if tick == 5 || tick == 15 {
            // The host restarts between two firings: the count stands where it stood.
            runner = Runner::new(
                workspaces.clone(),
                Arc::clone(&broker) as Arc<dyn StartRun>,
                Order::WORKSPACE,
            );
            runner.arm().await;
        }
        clock.set(first + (tick - 1) * 60_000);
        let counted_runs = ended_rows(workspaces, &workspace, "counted").await.len();
        broker.seeing(u64::try_from(counted_runs).unwrap());
        run_once(&mut runner).await;
        let rows = ended_rows(workspaces, &workspace, "counted").await;
        assert_eq!(
            rows.len(),
            usize::try_from(tick).unwrap(),
            "after firing {tick}"
        );
        assert!(
            rows.iter().all(|row| row.counted
                && row.ended == tilo_automation::dispatch::retry::Ended::Ran),
            "firing {tick}: {rows:?}"
        );
    }
    assert_eq!(
        broker.counts_reached(),
        [10, 20],
        "the count kept in the workspace reaches its multiple on the tenth and twentieth run"
    );
    host.shutdown().await;
}

/// A run whose broker cannot start it yet is tried again after its backoff, as one firing; a run
/// that is refused for good is kept done as a dead letter with its step and the words it failed
/// with.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_that_cannot_start_is_tried_again_and_a_refusal_is_kept_as_a_dead_letter() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let host = test_host(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Retrying").await.unwrap().workspace;
    let now = tilo_clock::unix_now_ms();
    let clock = Pinned::at(now);
    let at_ms = i64::try_from(now.saturating_sub(1_000)).unwrap();
    save_signed(
        workspaces,
        &workspace,
        shortcut("reminder", at_a_moment(at_ms), Vec::new()),
        (&key, &cert),
    )
    .await;
    let broker = Scripted::starting(
        vec![Err(ActionCallError::Unavailable {
            reason: "the broker is starting".into(),
        })],
        Vec::new(),
    );
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&broker) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    runner.pass(Vec::new()).await;
    assert!(
        broker.started().is_empty(),
        "nothing could take it yet: it is tried again"
    );
    assert!(
        ended_rows(workspaces, &workspace, "reminder")
            .await
            .is_empty(),
        "it is not kept as done while it may go again"
    );
    runner.pass(Vec::new()).await;
    assert!(broker.started().is_empty(), "not before its backoff");
    clock.set(now + 1_000);
    runner.pass(Vec::new()).await;
    assert_eq!(broker.started().len(), 1, "the same firing, started again");
    runner.pass(Vec::new()).await;
    let rows = ended_rows(workspaces, &workspace, "reminder").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        rows[0].ended,
        tilo_automation::dispatch::retry::Ended::Ran,
        "{rows:?}"
    );
    drop(runner);

    // Refused for good: kept with the reason, where it can be read.
    save_signed(
        workspaces,
        &workspace,
        shortcut("refused", at_a_moment(at_ms), Vec::new()),
        (&key, &cert),
    )
    .await;
    let broker = Scripted::starting(
        vec![Err(ActionCallError::Refused {
            reason: "this person keeps no assistant".into(),
        })],
        Vec::new(),
    );
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&broker) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    runner.pass(Vec::new()).await;
    let rows = ended_rows(workspaces, &workspace, "refused").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        rows[0].ended,
        tilo_automation::dispatch::retry::Ended::Refused
    );
    assert_eq!(rows[0].step, None);
    assert!(
        rows[0]
            .why
            .as_deref()
            .is_some_and(|why| why.contains("this person keeps no assistant")),
        "{rows:?}"
    );
    host.shutdown().await;
}

/// A run that fails for good at one of its steps is kept done as a dead letter naming that step
/// and the words it failed with.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_that_fails_for_good_is_kept_as_a_dead_letter() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let host = test_host(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Failing").await.unwrap().workspace;
    let now = tilo_clock::unix_now_ms();
    let _clock = Pinned::at(now);
    let at_ms = i64::try_from(now.saturating_sub(1_000)).unwrap();
    save_signed(
        workspaces,
        &workspace,
        shortcut("greeter", at_a_moment(at_ms), Vec::new()),
        (&key, &cert),
    )
    .await;
    let broker = Scripted::starting(
        Vec::new(),
        vec![Ending::Failed {
            step: "remind",
            failure: StepFailure::Host {
                error: Box::new(ActionCallError::Refused {
                    reason: "this person keeps no assistant".into(),
                }),
            },
        }],
    );
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&broker) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    runner.pass(Vec::new()).await;
    assert_eq!(broker.started().len(), 1);
    runner.pass(Vec::new()).await;
    let rows = ended_rows(workspaces, &workspace, "greeter").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        rows[0].ended,
        tilo_automation::dispatch::retry::Ended::Refused
    );
    assert_eq!(rows[0].step.as_deref(), Some("remind"));
    assert!(
        rows[0]
            .why
            .as_deref()
            .is_some_and(|why| why.contains("this person keeps no assistant")),
        "{rows:?}"
    );
    assert!(!rows[0].counted);
    host.shutdown().await;
}

/// The grant `root`'s person certified for `agent` in `workspace`, from `issued` and lasting as
/// long as an agent's grant may, with the key it signs with.
fn agent_grant(
    root: &ed25519_dalek::SigningKey,
    seed: u8,
    agent: &str,
    workspace: &str,
    issued_ns: u64,
) -> (tilo_user_identity::DeviceCert, ed25519_dalek::SigningKey) {
    let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let workspace = *uuid::Uuid::parse_str(workspace).unwrap().as_bytes();
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "An agent's grant".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::agent_grant(
            tilo_user_identity::AgentScope::agent_in(workspace, agent),
        ),
        issued_at_ns: issued_ns,
        expires_at_ns: Some(
            issued_ns.saturating_add(tilo_user_identity::MAX_AGENT_GRANT_LIFETIME_NS),
        ),
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, root).unwrap();
    (cert, key)
}

/// A shortcut an agent saved with its own key arms, and its runs act as that agent for its
/// person; one a device saved acts as the person.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shortcut_saved_by_an_agent_acts_as_that_agent_and_one_saved_by_a_person_as_the_person() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let host = test_host(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Agent").await.unwrap().workspace;
    let now = tilo_clock::unix_now_ms();
    let clock = Pinned::at(now);
    let person = hex::encode(cert.user_public_key);
    let at_ms = i64::try_from(now.saturating_sub(1_000)).unwrap();

    save_signed(
        workspaces,
        &workspace,
        shortcut("mine", at_a_moment(at_ms), Vec::new()),
        (&key, &cert),
    )
    .await;
    let (grant, grant_key) = agent_grant(
        &ed25519_dalek::SigningKey::from_bytes(&[1; 32]),
        41,
        "scout",
        &workspace,
        now.saturating_mul(1_000_000).saturating_sub(1),
    );
    let mut theirs = shortcut("theirs", at_a_moment(at_ms), Vec::new());
    tilo_peer_state::workspace::sign_saved_shortcut_as_agent(
        &workspace,
        &mut theirs,
        "scout",
        &grant,
        &grant_key,
    )
    .unwrap();
    workspaces
        .commit_own_membership(
            &workspace,
            WorkspaceMembershipCommand::Settings(SettingsChange::Shortcuts(
                tilo_automation::ShortcutChange::Save(Box::new(theirs)),
            )),
            None,
        )
        .await
        .unwrap();

    let started = Arc::new(Started::default());
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    runner.pass(Vec::new()).await;
    let firings = started.0.lock().unwrap().clone();
    assert_eq!(
        firings.len(),
        1,
        "the saver's own device starts its shortcut now: {firings:?}"
    );
    assert_eq!(firings[0].shortcut, "mine");
    assert_eq!(firings[0].agent, None);
    assert_eq!(
        firings[0].caller(),
        Caller::local_person(person.clone()).in_workspace(&workspace),
        "a device's shortcut acts as its person"
    );
    assert!(
        ended_rows(workspaces, &workspace, "theirs")
            .await
            .is_empty(),
        "another device answers the agent's shortcut after its wait"
    );

    // The agent's device is not this one: this device waits its grace, then starts it, and its
    // run acts as the agent for its person.
    clock.set(now + 30_000);
    runner.pass(Vec::new()).await;
    let firings = started.0.lock().unwrap().clone();
    assert_eq!(firings.len(), 2, "{firings:?}");
    assert_eq!(firings[1].shortcut, "theirs");
    assert_eq!(firings[1].saver, person);
    assert_eq!(
        firings[1].agent,
        Some(("scout".to_owned(), grant.device_public_key))
    );
    assert_eq!(
        firings[1].caller(),
        Caller::delegated_agent("scout", cert.user_public_key, grant.device_public_key)
            .in_workspace(&workspace),
        "the agent's shortcut acts as that agent for its person"
    );
    host.shutdown().await;
}

/// Set the pinned clock to `at_ms` and let the runner look once.
#[cfg(feature = "agents")]
async fn pass_at_ms(clock: &Pinned, runner: &mut Runner, at_ms: u64) {
    clock.set(at_ms);
    runner.pass(Vec::new()).await;
}

/// What the firings so far bound to the `event` input.
#[cfg(feature = "agents")]
fn fired_events(started: &Started) -> Vec<Value> {
    started
        .0
        .lock()
        .unwrap()
        .iter()
        .map(|firing| firing.inputs[tilo_automation::EVENT_INPUT].clone())
        .collect()
}

/// When a source test's clock starts, ms since the epoch.
#[cfg(feature = "agents")]
const SOURCE_START: u64 = 1_800_000_000_000;
/// How often a source test's trigger looks, in seconds.
#[cfg(feature = "agents")]
const SOURCE_EVERY: u64 = 900;

/// What a test's source answers a look with.
#[cfg(feature = "agents")]
#[derive(Clone)]
struct Answer {
    status: u16,
    kind: &'static str,
    body: String,
}

/// A source on this machine that a test starts: it answers every look with what the test last
/// had it say, and counts the looks.
#[cfg(feature = "agents")]
struct Source {
    responder: test_http::Responder,
    answer: Arc<Mutex<Answer>>,
    looks: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(feature = "agents")]
impl Source {
    /// A source that answers `body` as the media type `kind`.
    fn saying(kind: &'static str, body: &str) -> Self {
        let answer = Arc::new(Mutex::new(Answer {
            status: 200,
            kind,
            body: body.to_owned(),
        }));
        let looks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (said, counted) = (Arc::clone(&answer), Arc::clone(&looks));
        let responder = test_http::Responder::handle(move |_request| {
            counted.fetch_add(1, Ordering::SeqCst);
            let answer = said.lock().unwrap().clone();
            Some(
                test_http::Reply::status(answer.status)
                    .header("Content-Type", answer.kind)
                    .body(answer.body),
            )
        });
        Self {
            responder,
            answer,
            looks,
        }
    }

    /// Its address.
    fn at(&self) -> String {
        format!("{}/watched", self.responder.url())
    }

    /// From now on it says `body`.
    fn says(&self, body: &str) {
        let mut answer = self.answer.lock().unwrap();
        answer.status = 200;
        answer.body = body.to_owned();
    }

    /// From now on it answers every look with `status` and nothing else.
    fn fails_with(&self, status: u16) {
        let mut answer = self.answer.lock().unwrap();
        answer.status = status;
        answer.body = String::new();
    }

    /// How many looks reached it.
    fn looks(&self) -> usize {
        self.looks.load(Ordering::SeqCst)
    }
}

/// An Atom feed that lists `entries`, the first of them first.
#[cfg(feature = "agents")]
fn feed(entries: &[(&str, &str)]) -> String {
    use std::fmt::Write as _;
    let mut listed = String::new();
    for (id, title) in entries {
        let _ = write!(
            listed,
            "<entry><id>urn:example:{id}</id><title>{title}</title>\
             <link href=\"https://example.org/posts/{id}\"/>\
             <updated>2026-11-17T10:00:00Z</updated>\
             <summary>What &lt;b&gt;{title}&lt;/b&gt; is about.</summary></entry>"
        );
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <feed xmlns=\"http://www.w3.org/2005/Atom\"><title>News</title>\
         <id>urn:example:feed</id><updated>2026-11-17T10:00:00Z</updated>{listed}</feed>"
    )
}

/// A page that says what the small plan costs, made at `made` and marked up with `class`.
#[cfg(feature = "agents")]
fn prices(small: u32, made: &str, class: &str) -> String {
    format!(
        "<!doctype html><html><head><title>Prices</title><script>var t = '{made}';</script></head>\
         <body><main class=\"{class}\"><h1>Prices</h1>\
         <p class=\"{class}\">The small plan costs {small} euros a month for one person.</p>\
         <p>The large plan costs 40 euros a month for a whole team.</p>\
         <p>This page was made at {made}.</p></main></body></html>"
    )
}

/// A shortcut `id` that fires on something new in the source of `kind` at `at`, looked at every
/// [`SOURCE_EVERY`] seconds.
#[cfg(feature = "agents")]
fn on_a_source(id: &str, kind: &str, at: &str) -> Shortcut {
    shortcut(
        id,
        TriggerSpec::SourceChanged {
            kind: kind.to_owned(),
            at: at.to_owned(),
            every_secs: Some(SOURCE_EVERY),
            filter: serde_json::Map::new(),
        },
        Vec::new(),
    )
}

/// A runner armed over `workspaces`, its looks at sources outside reaching a server on this
/// machine when `fixtures` says so.
#[cfg(feature = "agents")]
async fn source_runner(workspaces: &Workspaces, started: &Arc<Started>, fixtures: bool) -> Runner {
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    if fixtures {
        runner.reach_fixtures();
    }
    runner.arm().await;
    runner
}

/// A feed that did not change fires nothing and asks nobody anything; an entry it lists anew
/// fires its shortcut once, with what it says as data; and a runner that starts over carries on
/// from what the workspace kept, so it fires nothing it fired before.
#[cfg(feature = "agents")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, end to end, reads as one test"
)]
async fn a_feed_that_did_not_change_fires_nothing_and_a_new_entry_fires_its_shortcut_once() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let host = test_host(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Feeds").await.unwrap().workspace;
    let source = Source::saying(
        "application/atom+xml",
        &feed(&[("2", "Second post"), ("1", "First post")]),
    );
    save_signed(
        workspaces,
        &workspace,
        on_a_source("reader", "tilo.web.feed", &source.at()),
        (&key, &cert),
    )
    .await;
    let clock = Pinned::at(SOURCE_START);
    let started = Arc::new(Started::default());
    let mut runner = source_runner(workspaces, &started, true).await;
    let every_ms = SOURCE_EVERY * 1000;

    // The first look takes in what the feed lists, and none of it is new.
    pass_at_ms(&clock, &mut runner, SOURCE_START).await;
    assert_eq!(source.looks(), 1);
    // Before the next look is due nothing is asked of the source at all.
    pass_at_ms(&clock, &mut runner, SOURCE_START + 60_000).await;
    assert_eq!(
        source.looks(),
        1,
        "it is looked at no more often than it says"
    );
    // Looked at again and again while it lists the same: nothing fires.
    pass_at_ms(&clock, &mut runner, SOURCE_START + every_ms).await;
    pass_at_ms(&clock, &mut runner, SOURCE_START + 2 * every_ms).await;
    assert_eq!(source.looks(), 3);
    assert!(fired_events(&started).is_empty(), "no change, no firing");

    // The feed lists a new entry: its shortcut fires once, with that entry as data.
    source.says(&feed(&[
        ("3", "Third post"),
        ("2", "Second post"),
        ("1", "First post"),
    ]));
    pass_at_ms(&clock, &mut runner, SOURCE_START + 3 * every_ms).await;
    let events = fired_events(&started);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["kind"], "tilo.web.feed");
    assert_eq!(events[0]["item"], "urn:example:3");
    let facts = &events[0]["facts"];
    for said in [
        "Third post",
        "https://example.org/posts/3",
        "What Third post is about.",
    ] {
        let held = serde_json::to_string(facts).unwrap();
        assert!(held.contains(said), "{said}: {held}");
    }
    for old in ["Second post", "First post"] {
        let held = serde_json::to_string(facts).unwrap();
        assert!(
            !held.contains(old),
            "what it listed before is not new: {held}"
        );
    }

    // The same list again: the change is fired once.
    pass_at_ms(&clock, &mut runner, SOURCE_START + 4 * every_ms).await;
    assert_eq!(source.looks(), 5);
    assert_eq!(fired_events(&started).len(), 1);

    // A runner that starts over carries on from what the workspace kept of its looks: it looks
    // no sooner than the last look said, and nothing it fired before fires again.
    drop(runner);
    let mut runner = source_runner(workspaces, &started, true).await;
    pass_at_ms(&clock, &mut runner, SOURCE_START + 4 * every_ms + 60_000).await;
    assert_eq!(
        source.looks(),
        5,
        "a restart looks no sooner than the last look said"
    );
    pass_at_ms(&clock, &mut runner, SOURCE_START + 5 * every_ms).await;
    assert_eq!(source.looks(), 6);
    assert_eq!(fired_events(&started).len(), 1, "nothing is fired twice");
    // What the feed lists anew after the restart fires, and only that.
    source.says(&feed(&[
        ("4", "Fourth post"),
        ("3", "Third post"),
        ("2", "Second post"),
        ("1", "First post"),
    ]));
    pass_at_ms(&clock, &mut runner, SOURCE_START + 6 * every_ms).await;
    let events = fired_events(&started);
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(events[1]["item"], "urn:example:4");
    host.shutdown().await;
}

/// A page fires its shortcut when the text a person reads there changed, and not for its own
/// clock or its markup.
#[cfg(feature = "agents")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_page_fires_its_shortcut_when_the_text_read_changed_and_not_for_its_clock() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let host = test_host(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Pages").await.unwrap().workspace;
    let source = Source::saying("text/html", &prices(10, "10:42:07, 5 minutes ago", "a"));
    save_signed(
        workspaces,
        &workspace,
        on_a_source("prices", "tilo.web.page", &source.at()),
        (&key, &cert),
    )
    .await;
    let clock = Pinned::at(SOURCE_START);
    let started = Arc::new(Started::default());
    let mut runner = source_runner(workspaces, &started, true).await;
    let every_ms = SOURCE_EVERY * 1000;
    pass_at_ms(&clock, &mut runner, SOURCE_START).await;

    // Its markup and its clock changed; what a person reads there did not.
    source.says(&prices(10, "11:15:40, 38 minutes ago", "b wide"));
    pass_at_ms(&clock, &mut runner, SOURCE_START + every_ms).await;
    assert_eq!(source.looks(), 2);
    assert!(fired_events(&started).is_empty());

    // A price changed: the new line is handed over, the lines that stayed are not.
    source.says(&prices(12, "11:31:02, 2 minutes ago", "b wide"));
    pass_at_ms(&clock, &mut runner, SOURCE_START + 2 * every_ms).await;
    let events = fired_events(&started);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["kind"], "tilo.web.page");
    let added = events[0]["facts"]["added"].as_str().unwrap().to_owned();
    assert!(
        added.contains("The small plan costs 12 euros a month for one person."),
        "{added}"
    );
    for stayed in ["The large plan costs", "costs 10 euros"] {
        assert!(!added.contains(stayed), "{stayed}: {added}");
    }

    // A change is handed over once.
    pass_at_ms(&clock, &mut runner, SOURCE_START + 3 * every_ms).await;
    assert_eq!(fired_events(&started).len(), 1);
    host.shutdown().await;
}

/// A source that fails is left alone for longer after each failure, and fires its shortcut once
/// when it has come to count as gone.
#[cfg(feature = "agents")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, end to end, reads as one test"
)]
async fn a_source_that_fails_is_left_alone_for_longer_and_fires_once_it_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let host = test_host(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Gone").await.unwrap().workspace;
    let source = Source::saying("application/atom+xml", &feed(&[("1", "First post")]));
    save_signed(
        workspaces,
        &workspace,
        on_a_source("watch", "tilo.web.feed", &source.at()),
        (&key, &cert),
    )
    .await;
    let clock = Pinned::at(SOURCE_START);
    let started = Arc::new(Started::default());
    let mut runner = source_runner(workspaces, &started, true).await;
    let every_ms = SOURCE_EVERY * 1000;

    pass_at_ms(&clock, &mut runner, SOURCE_START).await;
    assert_eq!(source.looks(), 1);

    // The source is in trouble. After each look that fails it is left alone twice as long: a
    // look a minute before the next one is due asks nothing.
    source.fails_with(503);
    let mut due = SOURCE_START.saturating_add(every_ms);
    let mut gap = every_ms;
    for failed in 1..=6_usize {
        pass_at_ms(&clock, &mut runner, due.saturating_sub(60_000)).await;
        assert_eq!(source.looks(), failed, "left alone until its look is due");
        pass_at_ms(&clock, &mut runner, due).await;
        assert_eq!(source.looks(), failed + 1);
        assert_eq!(
            fired_events(&started).len(),
            usize::from(failed == 6),
            "after {failed} failed looks"
        );
        gap = gap.saturating_mul(2);
        due = due.saturating_add(if failed == 6 { 86_400 * 1_000 } else { gap });
    }

    // The sixth failure in a row: its shortcut fires once, told that the source is gone and why.
    let events = fired_events(&started);
    assert_eq!(events.len(), 1, "{events:?}");
    let facts = &events[0]["facts"];
    assert_eq!(facts["gone"], true);
    assert!(
        facts["why"].as_str().unwrap().contains("HTTP 503"),
        "{facts}"
    );

    // Gone, it is looked at once a day, and nothing fires again.
    pass_at_ms(&clock, &mut runner, due - 3_600_000).await;
    assert_eq!(source.looks(), 7);
    pass_at_ms(&clock, &mut runner, due).await;
    assert_eq!(source.looks(), 8);
    assert_eq!(fired_events(&started).len(), 1, "fired once");

    // It answers again and lists a new entry: that entry fires the shortcut.
    source.says(&feed(&[("2", "Second post"), ("1", "First post")]));
    pass_at_ms(&clock, &mut runner, due + 86_400_000).await;
    let events = fired_events(&started);
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(events[1]["item"], "urn:example:2");
    host.shutdown().await;
}

/// A source on this machine is refused by the fetch as it ships: nothing is asked of it, and its
/// shortcut fires once, told that the source is gone.
#[cfg(feature = "agents")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_source_on_this_machine_is_refused_when_the_fetch_ships() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let host = test_host(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Local").await.unwrap().workspace;
    let source = Source::saying("text/html", &prices(10, "10:42", "a"));
    save_signed(
        workspaces,
        &workspace,
        on_a_source("local", "tilo.web.page", &source.at()),
        (&key, &cert),
    )
    .await;
    let clock = Pinned::at(SOURCE_START);
    let started = Arc::new(Started::default());
    let mut runner = source_runner(workspaces, &started, false).await;

    pass_at_ms(&clock, &mut runner, SOURCE_START).await;
    pass_at_ms(&clock, &mut runner, SOURCE_START + SOURCE_EVERY * 1000).await;
    pass_at_ms(&clock, &mut runner, SOURCE_START + 7 * 86_400_000).await;
    assert_eq!(
        source.looks(),
        0,
        "this machine's own network is never looked at"
    );
    let events = fired_events(&started);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["facts"]["gone"], true);
    assert!(
        events[0]["facts"]["why"]
            .as_str()
            .unwrap()
            .contains("local network"),
        "{events:?}"
    );
    host.shutdown().await;
}

/// A look at a source goes to the source itself: a proxy this machine's environment names is
/// sent nothing of it. Such a proxy would resolve a name itself, behind the lookup the fetch's
/// client judges, and so reach what that lookup refuses.
#[cfg(feature = "agents")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_look_at_a_source_sends_nothing_to_a_proxy_the_environment_names() {
    use tilo_test_env::ScopedEnv;
    if !tilo_test_env::in_own_process_here(
        module_path!(),
        "a_look_at_a_source_sends_nothing_to_a_proxy_the_environment_names",
    ) {
        return;
    }
    let proxy = test_http::Responder::handle(|_| {
        Some(
            test_http::Reply::status(200)
                .header("Content-Type", "text/plain")
                .body("the proxy's answer"),
        )
    });
    let source = Source::saying("text/html", &prices(10, "10:42", "a"));
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let host = test_host(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Proxy").await.unwrap().workspace;
    save_signed(
        workspaces,
        &workspace,
        on_a_source("page", "tilo.web.page", &source.at()),
        (&key, &cert),
    )
    .await;
    // The environment names the proxy only while the look runs: the host's endpoint asks its
    // relay where it is seen from as it opens, and that request is no look at a source.
    let mut environment = ScopedEnv::new();
    for name in [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        environment.set(name, proxy.url());
    }
    // Nothing exempts an address from that proxy, and nothing has a name of it ignored.
    for name in ["NO_PROXY", "no_proxy", "REQUEST_METHOD"] {
        environment.remove(name);
    }
    let clock = Pinned::at(SOURCE_START);
    let started = Arc::new(Started::default());
    let mut runner = source_runner(workspaces, &started, true).await;
    pass_at_ms(&clock, &mut runner, SOURCE_START).await;

    let sent = proxy.drain_requests();
    assert!(
        sent.is_empty(),
        "the proxy the environment names was sent {} request(s) of a look: {:?}",
        sent.len(),
        sent.iter()
            .map(|request| (&request.method, request.target.as_str()))
            .collect::<Vec<_>>()
    );
    assert_eq!(source.looks(), 1, "the source itself was asked");
    host.shutdown().await;
}

/// The workspace's own emoji, as Chat's record holds them in the workspace's store: a `party`.
async fn found_party_emoji(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &tilo_user_identity::DeviceCert,
    png: &[u8],
) {
    let record = collab_types::workspace_document_id(tilo_workspace_emoji::KIND);
    workspaces
        .author_record(
            workspace,
            (
                tilo_workspace_emoji::OWNER,
                tilo_workspace_emoji::KIND,
                &record,
            ),
            |doc| {
                let emoji = doc
                    .put_object(automerge::ROOT, "emoji", automerge::ObjType::Map)
                    .map_err(|error| error.to_string())?;
                let party = doc
                    .put_object(&emoji, "party", automerge::ObjType::Map)
                    .map_err(|error| error.to_string())?;
                doc.put(&party, "image", automerge::ScalarValue::Bytes(png.to_vec()))
                    .map_err(|error| error.to_string())?;
                let added_by = serde_json::to_string(&tilo_action_policy::MemberId::person(
                    hex::encode(cert.user_public_key),
                ))
                .map_err(|error| error.to_string())?;
                doc.put(&party, "added_by", added_by)
                    .map_err(|error| error.to_string())?;
                doc.put(&party, "added_at", 1_i64)
                    .map_err(|error| error.to_string())
            },
        )
        .await
        .unwrap();
}

/// The inputs a shortcut declares that its host binds are filled on every firing: the
/// workspace it runs in with each member's role and the public address of their picture, the
/// workspace's own emoji, and what fired it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_fills_the_workspace_emoji_and_context_inputs_a_shortcut_declares() {
    struct ChatProfile {
        root: [u8; 32],
        photo: Vec<u8>,
    }
    impl crate::workspaces::PersonNames for ChatProfile {
        fn name_of(&self, person: &[u8; 32]) -> Option<String> {
            (person == &self.root).then(|| "Ada".to_owned())
        }
        fn keep_name(&self, _: &[u8; 32], _: &str) -> Result<(), String> {
            Ok(())
        }
        fn photo_of(&self, person: &[u8; 32]) -> Option<Vec<u8>> {
            (person == &self.root).then(|| self.photo.clone())
        }
    }

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n....";

    let dir = tempfile::tempdir().unwrap();
    let keys = Arc::new(crate::workspaces::fixtures::TestGroupKeys::default());
    let (key, cert) = governor();
    let host = crate::workspaces::fixtures::test_host_holding(dir.path(), &key, &cert, keys).await;
    let workspaces = host.workspaces();
    workspaces.set_member_device(Arc::new(crate::workspaces::MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    let workspace = workspaces.create("Lounge").await.unwrap().workspace;
    // This host serves its people's pictures at its site, and keeps one for the governor.
    workspaces.advertises_at("https://eu1.tilo.sh");
    workspaces.person_names(Arc::new(ChatProfile {
        root: cert.user_public_key,
        photo: PNG.to_vec(),
    }));
    // The workspace's own emoji, as Chat's record holds them: founded from the same genesis
    // every reader of the workspace's records checks.
    found_party_emoji(workspaces, &workspace, &cert, PNG).await;

    let shortcut: Shortcut = serde_json::from_value(json!({
        "id": "bridge",
        "name": "Bridge",
        "steps": [],
        "inputs": [
            {"name": "workspace", "ty": "object", "description": "The workspace"},
            {"name": "emoji", "ty": "object", "description": "The workspace's emoji"},
            {"name": "context", "ty": "object", "description": "What fired it"}
        ],
        "triggers": [{
            "trigger": "resource_changed",
            "kind": "tasks.task",
            "actions": [],
            "fields": [],
        }],
    }))
    .unwrap();
    save_signed(workspaces, &workspace, shortcut, (&key, &cert)).await;

    let started = Arc::new(Started::default());
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;

    // What a change of one of the workspace's records fires: the dispatcher reads who made it
    // and what they did into the firing's context.
    let event = DocumentEvent {
        workspace: workspace.clone(),
        record: "record".into(),
        app: "so.tilo.tasks".into(),
        kind: "tasks.task".into(),
        change: "c1".into(),
        actor: tilo_actions::Acting::person(hex::encode(cert.user_public_key)),
        did: tilo_actions::Did::verb(
            tilo_actions::ActionKey::new("so.tilo.tasks", "tasks.task", "add"),
            [],
        ),
        arguments: serde_json::Map::default(),
        item: None,
        before: None,
        after: None,
        caused_by: None,
        origin: None,
        incoming_message: false,
        housekeeping: false,
        shortcut: None,
        invocation: None,
    };
    let waiting = dispatched(
        &runner.armed,
        Event::Record(&event),
        tilo_clock::unix_now_ms_i64(),
    );
    assert_eq!(waiting.len(), 1, "the shortcut fires on the change");
    let inputs = runner.host_inputs(&waiting[0].firing).await;
    assert_bound_inputs(&inputs, cert.user_public_key, &workspace, PNG);
    host.shutdown().await;
}

/// What the firing's `inputs` hold of the workspace, its emoji and what fired it, as this host
/// binds them for the person `who` in `workspace`.
fn assert_bound_inputs(
    inputs: &serde_json::Map<String, serde_json::Value>,
    who: [u8; 32],
    workspace: &str,
    png: &[u8],
) {
    let me = inputs["workspace"]["members"]
        .as_array()
        .unwrap_or_else(|| panic!("the workspace input: {inputs:?}"))
        .iter()
        .find(|row| row["member"]["person"] == json!(hex::encode(who)))
        .unwrap_or_else(|| panic!("the governor's row: {inputs:?}"));
    assert_eq!(me["name"], "Ada");
    assert_eq!(me["role"], "owner");
    let avatar = me["avatar"].as_str().unwrap_or_default();
    assert!(
        avatar.starts_with("https://eu1.tilo.sh/avatars/"),
        "the picture they set is served at a public address: {avatar}"
    );

    let emoji = inputs["emoji"]["emoji"]
        .as_array()
        .unwrap_or_else(|| panic!("the emoji input: {inputs:?}"));
    assert_eq!(emoji[0]["name"], "party");
    assert_eq!(emoji[0]["picture"], blake3::hash(png).to_hex().to_string());

    let context = &inputs["context"];
    assert_eq!(context["trigger"], "resource_changed");
    assert_eq!(context["workspace"], workspace);
    assert!(
        context["who"].to_string().contains(&hex::encode(who)),
        "who made the change: {}",
        context["who"]
    );
    assert!(
        context["what"].is_object(),
        "what they did: {}",
        context["what"]
    );
}
