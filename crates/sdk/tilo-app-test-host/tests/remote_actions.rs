//! An app's own client for the session's actions, served by a provider in this process: what it
//! reads of the catalogue, the invocations it follows to their end, the records it follows, and
//! the provider a guard enters for it. The calls are answered as the runner that starts an app
//! answers them, which the toolkit does not link.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use tilo_ui_base::arriving::Arrival;
use tilo_ui_base::widget::Collaborative as _;
use ui_core::remote_actions::testing::{self, FOLLOW_INTERVAL, FOLLOW_WAIT_MS, OBSERVE_INTERVAL};
use ui_core::remote_actions::{FollowedRead, InvocationProgress as _, RemoteActions};
use ui_core::tilo_actions;

use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, CatalogueResource,
    CatalogueRow, InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending,
    ReceiptStatus, RemoteActionMessage, RemoteActionTarget, ResourceReadRequest, WatchScope,
};
use tilo_actions::{ActionAddress, ActionOutput, InvocationId, InvocationUpdate};

fn row(id: &str) -> CatalogueRow {
    CatalogueRow {
        offered: None,
        provider: "7".into(),
        app_id: "so.tilo.example".into(),
        parent_resource: String::new(),
        resource: CatalogueResource {
            id: id.into(),
            kind: "example.item".into(),
            label: id.into(),
            presentation: None,
        },
        action: None,
        has_children: false,
        visibility: None,
        surface: None,
    }
}

fn target(action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: "7".into(),
        address: ActionAddress::new("example.item:1", action),
    }
}

/// Two catalogue pages; `done` completes at once, `slow` is still running for its first
/// [`RUNNING_LOOKS`] observes and done after, `forbidden` is refused. It cannot signal changes, so it
/// answers a waiting observe at once, as a provider from before waits does.
#[derive(Default)]
struct FakeHost {
    seen: Mutex<Vec<Caller>>,
    observed: std::sync::atomic::AtomicUsize,
}

impl ActionProvider for FakeHost {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        self.seen.lock().unwrap().push(caller.clone());
        let second = !query.cursor.is_empty();
        Pending::ready(Ok(CataloguePage {
            revision: "r1".into(),
            rows: vec![row(if second { "b" } else { "a" })],
            next_cursor: if second { Vec::new() } else { vec![1] },
            exhausted: second,
            ..CataloguePage::default()
        }))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let status = match request.target.address.action.as_str() {
            "done" => ReceiptStatus::Completed {
                output: ActionOutput {
                    value: Some(serde_json::json!({"ok": true})),
                    resources: Vec::new(),
                },
            },
            "slow" => ReceiptStatus::Accepted,
            _ => return Pending::ready(Err(ActionCallError::NotPermitted)),
        };
        Pending::ready(Ok(InvocationReceipt {
            invocation: request.invocation,
            status,
        }))
    }

    fn observe(&self, _caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        let running = self.observed.fetch_add(1, Ordering::SeqCst) < RUNNING_LOOKS;
        Some(InvocationState::new(
            *id,
            if running {
                InvocationUpdate::Accepted { invocation: *id }
            } else {
                InvocationUpdate::Succeeded {
                    invocation: *id,
                    output: ActionOutput::default(),
                }
            },
        ))
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), ActionCallError>> {
        tilo_actions::remote::Pending::ready(Ok(()))
    }
}

/// How often `slow` is seen still running: once by the provider's own look when the invoke asks to wait,
/// once by the client's first waiting observe.
const RUNNING_LOOKS: usize = 2;

fn client() -> (Arc<FakeHost>, RemoteActions) {
    tilo_app_test_host::answer_calls_in_process();
    let provider = Arc::new(FakeHost::default());
    let actions = RemoteActions::with_provider(
        Arc::clone(&provider) as Arc<dyn ActionProvider + Send + Sync>,
        Caller::local_app("so.tilo.example").in_workspace("workspace"),
    );
    (provider, actions)
}

/// A provider that reads one record, whose readings count its writes. While it holds reads, one that
/// names the revision it is at and waits is answered by the next [`Records::write`], as a provider
/// that waits for writes answers it; otherwise every read is answered at once.
#[derive(Default)]
struct Records {
    writes: Mutex<u64>,
    held: Mutex<Vec<(u64, tilo_actions::remote::Completer<RemoteActionMessage>)>>,
    asked: Mutex<Vec<ResourceReadRequest>>,
    holds: std::sync::atomic::AtomicBool,
    refuses: std::sync::atomic::AtomicBool,
}

impl Records {
    /// The copy of the record this provider reads, as it stands: a reading it publishes itself,
    /// naming the revision it stands at.
    fn copied(&self) -> tilo_actions::remote::ResourceCopy {
        let writes = *self.writes.lock().unwrap();
        tilo_actions::remote::ResourceCopy::reading(
            serde_json::json!({"writes": writes}),
            Some(format!("r{writes}")),
        )
    }

    /// The readings every reader of the record is given, from the copy the provider reads: an
    /// answer built here would test the shape against itself.
    fn readings(&self) -> Result<serde_json::Value, ActionCallError> {
        let asks = [tilo_actions::remote::ResourceAsk {
            address: tilo_actions::remote::ResourceAddress {
                workspace: "workspace".into(),
                app: "so.tilo.tasks".into(),
                record: String::new(),
            },
            joined: false,
        }];
        tilo_action_calls::readings_of(&asks, |_| Ok(self.copied())).and_then(|readings| {
            readings
                .readable()
                .map_err(|reason| ActionCallError::Refused { reason })
        })
    }

    /// The revision the readings name: what a follow asks to be answered past.
    fn revision(&self) -> Option<String> {
        self.readings().ok().and_then(|readings| {
            readings
                .get("revision")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
    }

    fn reading(&self, correlation: u64) -> RemoteActionMessage {
        let result = if self.refuses.load(Ordering::SeqCst) {
            Err(ActionCallError::Refused {
                reason: tilo_actions::remote::RESOURCE_READ_REFUSED.into(),
            })
        } else {
            self.readings().map(|readings| readings.to_string())
        };
        RemoteActionMessage::ResourceReading {
            correlation,
            result,
        }
    }

    /// A write lands: every read held for it is answered.
    fn write(&self) {
        let mut writes = self.writes.lock().unwrap();
        *writes = writes.saturating_add(1);
        drop(writes);
        let held = std::mem::take(&mut *self.held.lock().unwrap());
        for (correlation, answer) in held {
            answer.complete(self.reading(correlation));
        }
    }

    fn asked(&self) -> Vec<(Option<String>, Option<u32>)> {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .map(|read| (read.after.clone(), read.wait_ms))
            .collect()
    }
}

impl tilo_actions::remote::ResourceReads for Records {
    fn read<'a>(
        &'a self,
        _: &'a Caller,
        _: &'a tilo_actions::remote::ResourceAddress,
    ) -> tilo_actions::remote::ReadsAnswer<'a, tilo_actions::remote::ResourceCopy> {
        // A reading its provider publishes itself, naming the revision it stands at: the follow
        // shows it as it shows a document, and asks again when it moves.
        Box::pin(std::future::ready(Ok(self.copied())))
    }

    fn answer_read(
        self: Arc<Self>,
        _: &Caller,
        correlation: u64,
        read: ResourceReadRequest,
    ) -> tilo_actions::remote::ReadingAnswer {
        // A read that names the revision its reading would show, and waits, is held for a write.
        let waits = read.wait_ms.is_some() && read.after == self.revision();
        self.asked.lock().unwrap().push(read);
        if waits && self.holds.load(Ordering::SeqCst) && !self.refuses.load(Ordering::SeqCst) {
            let (answer, held) = tilo_actions::remote::pending();
            self.held.lock().unwrap().push((correlation, answer));
            return Box::pin(async move {
                held.await.unwrap_or(RemoteActionMessage::ResourceReading {
                    correlation,
                    result: Err(ActionCallError::Unanswered),
                })
            });
        }
        let reading = self.reading(correlation);
        Box::pin(std::future::ready(reading))
    }
}

/// A provider whose only service is its [`Records`].
struct RecordHost(Arc<Records>);

impl ActionProvider for RecordHost {
    fn query(
        &self,
        _: &Caller,
        _: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Ok(CataloguePage::default()))
    }

    fn invoke(
        &self,
        _: &Caller,
        _: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownAction))
    }

    fn observe(&self, _: &Caller, _: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(&self, _: &Caller, _: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Ok(()))
    }

    fn resource_reads(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceReads>> {
        Some(Arc::clone(&self.0) as Arc<dyn tilo_actions::remote::ResourceReads>)
    }
}

/// A reader of a provider's [`Records`], which hold reads when `holds`.
fn reader(holds: bool) -> (Arc<Records>, RemoteActions) {
    tilo_app_test_host::answer_calls_in_process();
    let records = Arc::new(Records::default());
    records.holds.store(holds, Ordering::SeqCst);
    let actions = RemoteActions::with_provider(
        Arc::new(RecordHost(Arc::clone(&records))) as Arc<dyn ActionProvider + Send + Sync>,
        Caller::local_person("ada").in_workspace("workspace"),
    );
    (records, actions)
}

/// Tasks' record of the workspace, as a window of another app names it.
fn tasks() -> ResourceReadRequest {
    ResourceReadRequest {
        workspace: "workspace".into(),
        app: "so.tilo.tasks".into(),
        record: String::new(),
        joined: false,
        // Neither is the follower's to use: it names what it shows and waits by itself.
        after: Some("stale".into()),
        wait_ms: Some(1),
        answer: tilo_actions::remote::ReadAnswer::Reading,
    }
}

/// How many writes the readings `followed` shows have seen.
fn writes_shown(followed: &FollowedRead) -> Option<u64> {
    followed.call().with(|arrival| {
        arrival
            .ready()
            .and_then(|readings| readings.pointer("/records/0/value/writes"))
            .and_then(serde_json::Value::as_u64)
    })
}

tilo_test_env::scenario! {
    fn a_catalogue_page_is_bounded_and_refused_when_it_cannot_be;

    phase a_single_page_catalogue_read_leaves_hierarchy_budget_with_its_caller;
    {
        let (provider, actions) = client();
        let answer = actions.catalogue_page(CatalogueQuery::default());
        let Arrival::Ready(page) = answer.get() else {
            panic!("the first page is answered: {:?}", answer.get());
        };
        assert_eq!(
            page.rows
                .iter()
                .map(|row| row.resource.id.as_str())
                .collect::<Vec<_>>(),
            ["a"]
        );
        assert_eq!(page.next_cursor, [1]);
        assert_eq!(provider.seen.lock().unwrap().len(), 1);
        assert!(!actions.is_busy());
    }
}

tilo_test_env::scenario! {
    fn an_invocation_is_followed_to_its_end_and_cancelled_honestly;

    phase an_invocation_reports_its_outcome_or_its_refusal;
    {
        let (_host, actions) = client();
        let done = actions.invoke(target("done"), InvokeArgument::None, None);
        assert!(matches!(
            done.get(),
            Arrival::Ready(InvocationUpdate::Succeeded { .. })
        ));
        let refused = actions.invoke(target("forbidden"), InvokeArgument::None, None);
        assert_eq!(
            refused.get(),
            Arrival::Failed(ActionCallError::NotPermitted)
        );
        assert!(refused.get().is_finished());
    }

    phase a_running_invocation_is_followed_until_it_ends;
    // Against a provider that answers a waiting observe at once with nothing new (one from before waits), the
    // client falls back to observing on its interval.
    {
        let clock = tilo_ui_base::background::Pumped::new();
        let _entered = clock.background().enter();
        let (_host, actions) = client();
        let slow = actions.invoke(target("slow"), InvokeArgument::None, None);
        assert!(matches!(
            slow.get(),
            Arrival::Ready(InvocationUpdate::Accepted { .. })
        ));
        assert!(!slow.get().is_finished());
        actions.poll();
        assert!(
            !slow.get().is_finished(),
            "not observed again before the interval"
        );
        clock.advance(OBSERVE_INTERVAL);
        actions.poll();
        assert!(matches!(
            slow.get(),
            Arrival::Ready(InvocationUpdate::Succeeded { .. })
        ));
        assert!(!actions.is_busy());
    }
}

tilo_test_env::scenario! {
    fn a_client_talks_through_the_session_or_the_host_entered_for_it;

    phase an_entered_host_serves_the_current_client_until_the_guard_drops;
    {
        tilo_app_test_host::answer_calls_in_process();
        let provider = Arc::new(FakeHost::default());
        {
            let _served = RemoteActions::enter(
                Arc::clone(&provider) as Arc<dyn ActionProvider + Send + Sync>,
                Caller::local_app("so.tilo.example").in_workspace("workspace"),
            );
            let answer = RemoteActions::current().catalogue_page(CatalogueQuery::default());
            assert!(matches!(answer.get(), Arrival::Ready(_)));
        }
        let mut after = RemoteActions::current();
        let _pending = after.catalogue_page(CatalogueQuery::default());
        assert_eq!(
            after.take_collab_requests().len(),
            1,
            "without a guard the client talks through the session"
        );
    }
}

tilo_test_env::scenario! {
    fn a_followed_record_shows_each_write_and_asks_nothing_between;

    phase a_followed_record_is_read_at_once_and_again_when_a_write_lands;
    {
        let (records, actions) = reader(true);
        let followed = actions.follow_resource(None, tasks());
        assert_eq!(writes_shown(&followed), Some(0));
        assert_eq!(
            records.asked(),
            [(None, None), (records.revision(), Some(FOLLOW_WAIT_MS))],
            "the first read is answered as the record is, the next names what is shown and waits"
        );
        assert!(
            !actions.is_busy(),
            "a read held for the next write is nothing the client is busy with"
        );
        let shown = followed.call().revision();
        actions.poll();
        assert_eq!(records.asked().len(), 2, "nothing is asked while nothing is written");
        assert_eq!(followed.call().revision(), shown, "and nothing is shown again");
        let shown_before = followed.call().revision();
        records.write();
        assert_eq!(writes_shown(&followed), Some(1));
        assert_ne!(
            followed.call().revision(),
            shown_before,
            "the revision the provider names is the one the UI follows"
        );
        records.write();
        assert_eq!(writes_shown(&followed), Some(2));
        assert_eq!(records.asked().len(), 4, "one read for each write");
    }

    phase a_dropped_follower_reads_nothing_more;
    {
        let (records, actions) = reader(true);
        let followed = actions.follow_resource(None, tasks());
        assert_eq!(records.asked().len(), 2);
        drop(followed);
        records.write();
        assert_eq!(records.asked().len(), 2, "the answer to the held read asks no other");
    }

    phase a_reader_the_host_refuses_is_told_and_shown_nothing_more;
    {
        let (records, actions) = reader(true);
        let followed = actions.follow_resource(None, tasks());
        assert_eq!(writes_shown(&followed), Some(0));
        records.refuses.store(true, Ordering::SeqCst);
        records.write();
        assert_eq!(
            followed.call().get(),
            Arrival::Failed(ActionCallError::Refused {
                reason: tilo_actions::remote::RESOURCE_READ_REFUSED.into(),
            }),
            "what was shown is no longer this reader's to see"
        );
        let asked = records.asked().len();
        records.refuses.store(false, Ordering::SeqCst);
        records.write();
        actions.poll();
        assert_eq!(records.asked().len(), asked, "a refused reader asks no more");
    }

    phase a_host_that_holds_no_read_is_read_again_on_the_interval;
    {
        let clock = tilo_ui_base::background::Pumped::new();
        let _entered = clock.background().enter();
        let (records, actions) = reader(false);
        let followed = actions.follow_resource(None, tasks());
        assert_eq!(writes_shown(&followed), Some(0));
        assert_eq!(
            records.asked().len(),
            2,
            "the waiting read came back at once with nothing new: this provider holds none"
        );
        {
            let mut writes = records.writes.lock().unwrap();
            *writes = writes.saturating_add(1);
        }
        actions.poll();
        assert_eq!(records.asked().len(), 2, "not asked again before the interval");
        clock.advance(FOLLOW_INTERVAL);
        actions.poll();
        assert_eq!(records.asked().last(), Some(&(None, None)));
        assert_eq!(writes_shown(&followed), Some(1));
        assert_eq!(records.asked().len(), 3, "one read an interval, never a loop");
    }

    phase a_record_its_host_cannot_read_says_why_once_and_is_asked_for_ever_more_rarely;
    {
        let clock = tilo_ui_base::background::Pumped::new();
        let _entered = clock.background().enter();
        // A provider that reads no records outside their apps.
        let (_host, actions) = client();
        let followed = actions.follow_resource(None, tasks());
        assert_eq!(
            followed.call().get(),
            Arrival::Failed(ActionCallError::Unavailable {
                reason: tilo_actions::remote::RESOURCE_READS_UNAVAILABLE.into(),
            })
        );
        assert!(!actions.is_busy(), "it waits for nothing: the record may never be read here");
        let told = followed.call().revision();
        clock.advance(FOLLOW_INTERVAL);
        actions.poll();
        clock.advance(FOLLOW_INTERVAL);
        actions.poll();
        assert_eq!(followed.call().revision(), told, "the same reason is not told again");
        assert!(
            testing::follow_retries(&actions)
                .into_iter()
                .all(|retry| retry == FOLLOW_INTERVAL.saturating_mul(4)),
            "each read it could not answer doubles the wait for the next"
        );
    }
}

tilo_test_env::scenario! {
    fn a_client_made_before_its_host_is_entered_is_that_hosts;

    phase what_it_asked_before_and_what_it_asks_under_the_guard_are_answered_by_that_host;
    {
        tilo_app_test_host::answer_calls_in_process();
        let provider = Arc::new(FakeHost::default());
        // The window a desktop opens with is built before the desktop serves its windows.
        let early = RemoteActions::current();
        let before = early.catalogue_page(CatalogueQuery::default());
        assert!(before.get().is_pending(), "nothing answers it yet");
        let _served = RemoteActions::enter(
            Arc::clone(&provider) as Arc<dyn ActionProvider + Send + Sync>,
            Caller::local_person("ada").in_workspace("workspace"),
        );
        let under = early.catalogue_page(CatalogueQuery::default());
        assert!(matches!(under.get(), Arrival::Ready(_)));
        assert!(
            matches!(before.get(), Arrival::Ready(_)),
            "what it asked before is that provider's to answer too"
        );
    }

    phase a_host_that_cannot_tell_when_the_workspace_changes_says_so;
    {
        let (_host, actions) = client();
        assert_eq!(
            actions.changed(WatchScope::Records).get(),
            Arrival::Failed(ActionCallError::Unavailable {
                reason: tilo_actions::remote::CHANGED_UNAVAILABLE.into(),
            })
        );
        assert!(!actions.is_busy());
    }
}
