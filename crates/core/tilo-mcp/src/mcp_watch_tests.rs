//! What wakes a watch: every source it reports on is listened for before it is read, so a change
//! that arrives while the watch holds answers it at once instead of after a fixed re-read. Each
//! phase holds a watch far longer than the interval a watch used to look again in, and proves it
//! answered on the change itself.
use super::*;
use crate::agent::tests::{Notes, agent, answer, run};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, ChangeFeed, ChangePage,
    Completer, Here, InvocationReceipt, InvocationState, InvokeRequest, Pending, ResourceChanged,
    WatchScope, pending,
};

/// How long a watch in these tests holds: far longer than any interval a re-read would use, so an
/// answer arriving in a fraction of it came on a change and not from looking again.
const HELD: u64 = 20_000;

/// How long a test waits before making the change a watch is holding for.
const BEFORE: Duration = Duration::from_millis(50);

/// What a test's change arrives in under: a watch woken by it answers well inside this.
const PROMPTLY: Duration = Duration::from_secs(5);

/// Where a host's waiters for a change wait, and the test's handle on waking them.
#[derive(Clone, Default)]
struct Wakes(Arc<Mutex<Vec<Completer<()>>>>);

impl Wakes {
    /// The next change of what this stands for.
    fn woken(&self) -> Pending<()> {
        let (woken, waits) = pending();
        self.0.lock().unwrap().push(woken);
        waits
    }

    /// What changed: everyone waiting for it now.
    fn wake(&self) {
        for woken in self.0.lock().unwrap().drain(..) {
            woken.complete(());
        }
    }
}

/// A Notes workspace that tells a watch when its records and its roster changed, and whose feed of
/// the workspace's changes the test writes into: everything else is Notes' own answer.
#[derive(Clone)]
struct Watched {
    notes: Notes,
    records: Wakes,
    roster: Wakes,
    /// The changes the workspace's feed reports, oldest first.
    feed: Arc<Mutex<Vec<ResourceChanged>>>,
    /// Why the host refuses to tell when the workspace's records change, when it does.
    refuses_records: Option<String>,
    /// How many times a watch asked when the records change.
    records_asked: Arc<Mutex<usize>>,
}

impl Watched {
    fn new() -> Self {
        Self {
            notes: Notes::new(true),
            records: Wakes::default(),
            roster: Wakes::default(),
            feed: Arc::default(),
            refuses_records: None,
            records_asked: Arc::default(),
        }
    }

    /// A host that refuses to tell when the workspace's records change, saying `why`.
    fn refusing_records(why: &str) -> Self {
        Self {
            refuses_records: Some(why.to_owned()),
            ..Self::new()
        }
    }

    /// A record of the workspace changed, `change` as its history lists it: both the watched
    /// resource's history and the workspace's feed report it.
    fn wrote(&self, change: &str) {
        let entry = json!({"change": change, "sentence": "added one", "time_ms": 1});
        self.notes.changed(entry.clone());
        self.feed.lock().unwrap().push(ResourceChanged {
            app: "so.tilo.notes".into(),
            record: "notes.page:1".into(),
            entry,
        });
        self.records.wake();
    }

    /// Someone joined the workspace.
    fn was_joined_by(&self, name: &str) {
        self.notes.people(Some(vec![Here {
            who: format!("person:{name}"),
            name: name.to_owned(),
            kind: "person".into(),
            works_in: None,
            devices: Vec::new(),
        }]));
        self.roster.wake();
    }
}

impl ActionProvider for Watched {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        self.notes.query(caller, query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.notes.invoke(caller, request)
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        self.notes.observe(caller, id)
    }

    fn here(&self, caller: &Caller) -> Option<Vec<Here>> {
        self.notes.here(caller)
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        self.notes.cancel(caller, id)
    }

    fn resource_search(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceSearch>> {
        self.notes.resource_search()
    }

    fn change_feed(&self) -> Option<Arc<dyn ChangeFeed>> {
        Some(Arc::new(self.clone()))
    }

    /// What a watch of this workspace follows, listened for before it reads.
    fn changed(
        &self,
        _caller: &Caller,
        scope: WatchScope,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        Some(match scope {
            WatchScope::Records => {
                let mut asked = self.records_asked.lock().unwrap();
                *asked = asked.saturating_add(1);
                drop(asked);
                match &self.refuses_records {
                    Some(why) => Pending::ready(Err(ActionCallError::Unavailable {
                        reason: why.clone(),
                    })),
                    None => self.records.woken().map(Ok),
                }
            }
            WatchScope::Here => self.roster.woken().map(Ok),
        })
    }
}

impl ChangeFeed for Watched {
    fn since(
        &self,
        _caller: &Caller,
        after: Option<&str>,
    ) -> Pending<Result<ChangePage, ActionCallError>> {
        let changes = self.feed.lock().unwrap();
        // With no cursor a follower starts from now, as a feed answers it.
        let seen = after
            .and_then(|after| after.parse::<usize>().ok())
            .unwrap_or(changes.len())
            .min(changes.len());
        Pending::ready(Ok(ChangePage {
            changes: changes[seen..].to_vec(),
            cursor: changes.len().to_string(),
        }))
    }
}

/// A watch of `server`, running beside the test: what it answered, and when.
fn watching<H: ActionProvider + Send + Sync + 'static>(
    server: &Arc<McpServer<H>>,
    arguments: Value,
) -> tokio::task::JoinHandle<(Value, bool)> {
    let server = Arc::clone(server);
    tokio::spawn(async move { answer(&server, "tilo_watch", arguments).await })
}

/// The cursor a first watch answered with: what the test's watch reads on from.
async fn cursor_of<H: ActionProvider + Send + Sync + 'static>(
    server: &Arc<McpServer<H>>,
    arguments: Value,
) -> String {
    let (start, failed) = answer(server, "tilo_watch", arguments).await;
    assert!(!failed, "{start}");
    // A watch with no cursor starts now: nothing that happened yet is news, and who is here now
    // is said, so a page that opens draws them.
    assert!(
        start["events"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .all(|event| event["kind"] == "presence"),
        "a watch with no cursor starts now, saying who is here: {start}"
    );
    start["cursor"].as_str().unwrap_or_default().to_owned()
}

tilo_test_env::scenario! {
    fn a_watch_is_woken_by_each_source_it_reports_on;

    phase a_change_of_the_workspaces_records_answers_a_watch_of_it_mid_wait;
    // A watch of the whole workspace holds until a record of it changes, and the feed's own
    // change is what answers it.
    {
        let host = Watched::new();
        run(async {
            let server = Arc::new(agent(&host));
            let cursor = cursor_of(&server, json!({"wait_ms": 50})).await;
            let started = std::time::Instant::now();
            let watched = watching(&server, json!({"cursor": cursor, "wait_ms": HELD}));
            tokio::time::sleep(BEFORE).await;
            host.wrote("c1a2");
            let (watched, failed) = watched.await.unwrap();
            assert!(!failed, "{watched}");
            assert!(
                started.elapsed() < PROMPTLY,
                "the watch answered {} ms after it began, not on the change",
                started.elapsed().as_millis()
            );
            let change = watched["events"]
                .as_array()
                .unwrap()
                .iter()
                .find(|event| event["kind"] == "change")
                .unwrap_or_else(|| panic!("the change is told: {watched}"));
            assert_eq!(change["change"], "c1a2");
            assert_eq!(
                change["resources"],
                json!([{"path": "notes", "app": "so.tilo.notes", "record": "notes.page:1"}]),
                "the change names what it changed: {change}"
            );
        });
    }

    phase a_change_of_a_resources_history_answers_a_watch_of_it_mid_wait;
    {
        let host = Watched::new();
        run(async {
            let server = Arc::new(agent(&host));
            let cursor = cursor_of(
                &server,
                json!({"path": "notes/Todo", "wait_ms": 50}),
            )
            .await;
            let started = std::time::Instant::now();
            let watched = watching(
                &server,
                json!({"path": "notes/Todo", "cursor": cursor, "wait_ms": HELD}),
            );
            tokio::time::sleep(BEFORE).await;
            host.wrote("c3b4");
            let (watched, failed) = watched.await.unwrap();
            assert!(!failed, "{watched}");
            assert!(
                started.elapsed() < PROMPTLY,
                "the watch answered {} ms after it began, not on the change",
                started.elapsed().as_millis()
            );
            let change = watched["events"]
                .as_array()
                .unwrap()
                .iter()
                .find(|event| event["kind"] == "change")
                .unwrap_or_else(|| panic!("the change is told: {watched}"));
            assert_eq!(change["path"], "notes/Todo");
            assert_eq!(change["change"], "c3b4");
        });
    }

    phase a_run_of_this_session_answers_a_watch_mid_wait;
    // An invocation of the session starting is a run event, and the session's own run list says
    // so without asking the host again.
    {
        let host = Watched::new();
        run(async {
            let server = Arc::new(agent(&host));
            let cursor = cursor_of(&server, json!({"wait_ms": 50})).await;
            let started = std::time::Instant::now();
            let watched = watching(&server, json!({"cursor": cursor, "wait_ms": HELD}));
            tokio::time::sleep(BEFORE).await;
            server.watch_invocation_started(
                "3fa2c1d0-1111-4222-8333-444455556666",
                "notes/Todo/body#append-paragraph",
            );
            let (watched, failed) = watched.await.unwrap();
            assert!(!failed, "{watched}");
            assert!(
                started.elapsed() < PROMPTLY,
                "the watch answered {} ms after it began, not on the invocation",
                started.elapsed().as_millis()
            );
            let run = watched["events"]
                .as_array()
                .unwrap()
                .iter()
                .find(|event| event["kind"] == "invocation")
                .unwrap_or_else(|| panic!("the invocation is told: {watched}"));
            assert_eq!(run["phase"], "started");
            assert_eq!(run["path"], "notes/Todo/body#append-paragraph");
        });
    }

    phase someone_joining_answers_a_watch_mid_wait;
    // Who is in the workspace is the host's roster, and a change of it answers the watch without
    // reading again on a timer.
    {
        let host = Watched::new();
        run(async {
            let server = Arc::new(agent(&host));
            let cursor = cursor_of(&server, json!({"wait_ms": 50})).await;
            let started = std::time::Instant::now();
            let watched = watching(&server, json!({"cursor": cursor, "wait_ms": HELD}));
            tokio::time::sleep(BEFORE).await;
            host.was_joined_by("Bea");
            let (watched, failed) = watched.await.unwrap();
            assert!(!failed, "{watched}");
            assert!(
                started.elapsed() < PROMPTLY,
                "the watch answered {} ms after it began, not on the roster",
                started.elapsed().as_millis()
            );
            let here = watched["events"]
                .as_array()
                .unwrap()
                .iter()
                .find(|event| event["kind"] == "presence")
                .unwrap_or_else(|| panic!("who is here is told: {watched}"));
            assert!(
                here["here"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|member| member["name"] == "Bea"),
                "{here}"
            );
        });
    }

    phase a_source_the_host_refuses_to_follow_is_told_at_once_and_not_asked_again;
    // A host that refuses to tell when the workspace's records change says why, and the watch
    // answers with that at once rather than holding for a signal that never comes. Told once:
    // the next watch does not ask again, and holds to its own wait instead of asking in a loop.
    {
        let host = Watched::refusing_records("this session carries no records of the workspace");
        run(async {
            let server = Arc::new(agent(&host));
            let started = std::time::Instant::now();
            let (watched, failed) = answer(&server, "tilo_watch", json!({"wait_ms": HELD})).await;
            assert!(!failed, "{watched}");
            assert!(
                started.elapsed() < PROMPTLY,
                "the watch answered {} ms after it began, not on the refusal",
                started.elapsed().as_millis()
            );
            let unwatched = json!({
                "kind": "unwatched",
                "source": "records",
                "reason": "remote actions are unavailable: this session carries no records of the workspace",
            });
            let told = watched["events"]
                .as_array()
                .unwrap_or_else(|| panic!("the watch answers with events: {watched}"));
            assert!(
                told.contains(&unwatched),
                "the refusal is told with why: {watched}"
            );
            assert!(
                told.iter()
                    .all(|event| event == &unwatched || event["kind"] == "presence"),
                "a watch with no cursor says who is here and the refusal, nothing else: {watched}"
            );
            let cursor = watched["cursor"].as_str().unwrap_or_default().to_owned();
            let started = std::time::Instant::now();
            let (again, failed) =
                answer(&server, "tilo_watch", json!({"cursor": cursor, "wait_ms": 300})).await;
            assert!(!failed, "{again}");
            assert_eq!(again["events"], json!([]), "the refusal is told once");
            assert!(
                started.elapsed() >= Duration::from_millis(300),
                "a watch whose source was refused holds to its own wait"
            );
            assert_eq!(
                *host.records_asked.lock().unwrap(),
                1,
                "a refused source is not asked again"
            );
        });
    }

    phase a_watch_with_no_signal_of_a_source_holds_until_its_own_wait_ends;
    // A host that cannot tell when a source changed leaves the watch its deadline: nothing answers
    // it early, and it answers with what it has once its own wait is over.
    {
        let notes = Notes::new(true);
        run(async {
            let server = Arc::new(agent(&notes));
            let cursor = cursor_of(&server, json!({"wait_ms": 50})).await;
            let started = std::time::Instant::now();
            let (watched, failed) = answer(
                &server,
                "tilo_watch",
                json!({"path": "notes/Todo", "cursor": cursor, "wait_ms": 300}),
            )
            .await;
            assert!(!failed, "{watched}");
            assert_eq!(watched["events"], json!([]), "{watched}");
            assert!(
                started.elapsed() >= Duration::from_millis(250),
                "a watch nothing wakes holds its own {} ms, and answered after {} ms",
                300,
                started.elapsed().as_millis()
            );
        });
    }
}
