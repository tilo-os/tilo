//! The hosts the MCP tests drive: a stub app whose directory and invocations
//! the test states, and one that holds an invocation until the test says it
//! changed.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::assigning_clones,
    clippy::panic,
    reason = "test fixtures: a failure here is the test's own assertion"
)]

use crate::McpServer;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    io::{BufRead as _, Write as _},
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueProvider, CatalogueQuery,
    CatalogueResource, CatalogueRow, Completer, InvocationReceipt, InvocationState, InvokeRequest,
    Pending,
};

mod schema_rows {
    use tilo_resource::{Model, resource};

    #[derive(Model, Default, Debug, Clone, PartialEq)]
    pub struct Note {
        pub title: String,
    }

    #[resource(
        kind = "test.note",
        catalogue = "tests/locales",
        read_content = tilo_resource::read::fields
    )]
    impl Note {
        /// Rename the note.
        #[action]
        pub fn rename(&mut self, #[param("New title")] title: String) {
            self.title = title;
        }
    }
}

#[derive(Clone, Copy, Default)]
pub enum CatalogueAvailability {
    #[default]
    Ready,
    Hidden,
    NotPermitted,
    ExitDuringQuery,
}

#[derive(Clone, Default)]
pub struct StubHost {
    pub activation: bool,
    pub availability: CatalogueAvailability,
    pub exited: Arc<std::sync::atomic::AtomicBool>,
    pub record: Option<String>,
    pub workspace: Option<String>,
    pub invoked: Arc<StdMutex<Vec<InvokeRequest>>>,
    pub callers: Arc<StdMutex<Vec<Caller>>>,
    pub confirmation: tilo_actions::ConfirmationPolicy,
    /// Observations a started invocation stays accepted for before it
    /// succeeds.
    pub running_for: Arc<AtomicUsize>,
    /// What a finished invocation produced.
    pub output: Arc<StdMutex<tilo_actions::ActionOutput>>,
    /// The kind of operation the published action is.
    pub kind: Arc<StdMutex<tilo_actions::OperationKind>>,
    /// The tags of the published action's contract.
    pub tags: Arc<StdMutex<Vec<String>>>,
    /// Answer for the resource as published, whatever id the caller named:
    /// as the host does for the id a window bound a resource under, which
    /// names no resource it publishes.
    pub catalogue_only: bool,
}

impl StubHost {
    fn row(&self) -> CatalogueRow {
        let resource = tilo_resource::publish::<schema_rows::Note>(
            "test.note:1",
            "Note",
            &schema_rows::Note::default(),
        );
        let mut descriptor = tilo_actions::api::survey(&[resource])
            .into_iter()
            .find(|descriptor| descriptor.address.action == tilo_resource::APPLY)
            .unwrap();
        descriptor.contract.confirmation = self.confirmation.clone();
        descriptor.kind = *self.kind.lock().unwrap();
        descriptor.contract.tags = self.tags.lock().unwrap().clone();
        if self.activation {
            descriptor.address.action = collab_types::app_actions::ACTIVATE_WINDOW.into();
            descriptor.kind = tilo_actions::OperationKind::View;
            descriptor.contract = tilo_actions::ActionContract::view();
            descriptor.params.clear();
            descriptor.input = tilo_actions::api::InputSpec::None;
        }
        CatalogueRow {
            offered: None,
            provider: "71".into(),
            app_id: "so.tilo.test".into(),
            parent_resource: String::new(),
            resource: CatalogueResource {
                id: "test.note:1".into(),
                kind: "test.note".into(),
                label: "Note".into(),
                presentation: None,
            },
            action: Some(descriptor),
            has_children: false,
            visibility: None,
            surface: None,
        }
    }
}

impl ActionProvider for StubHost {
    fn query(
        &self,
        _caller: &Caller,
        mut query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        if matches!(self.availability, CatalogueAvailability::ExitDuringQuery)
            && query.provider.is_some()
        {
            self.exited.store(true, Ordering::SeqCst);
            return Pending::ready(Err(ActionCallError::Unavailable {
                reason: "owner exited".into(),
            }));
        }
        if matches!(self.availability, CatalogueAvailability::NotPermitted)
            && query.provider.is_some()
        {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        }
        let mut row = self.row();
        if let Some(provider) = query.provider.clone() {
            row.provider = provider;
        }
        if self.catalogue_only {
            // An id this host publishes nothing under (a bound one): the
            // page holds what it does publish, and answers no target.
            query.target = None;
        }
        let mut page = CataloguePage::from_rows("1".into(), vec![row], &query);
        page.providers = vec![CatalogueProvider {
            provider: "71".into(),
            app_id: "so.tilo.test".into(),
            workspace: self.workspace.clone().unwrap_or_else(|| "workspace".into()),
            document_id: self.record.clone().unwrap_or_default(),
            browsable: true,
        }];
        if matches!(self.availability, CatalogueAvailability::Hidden)
            || self.exited.load(Ordering::SeqCst)
        {
            page.providers.clear();
        }
        Pending::ready(Ok(page))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let invocation = request.invocation;
        // Only the person's approve of the call's asked record gets past a contract that asks
        // for a phrase.
        if matches!(
            self.confirmation,
            tilo_actions::ConfirmationPolicy::Phrase { .. }
        ) && request.approval().is_none()
        {
            return Pending::ready(Err(ActionCallError::confirmation_required()));
        }
        self.callers.lock().unwrap().push(caller.clone());
        self.invoked.lock().unwrap().push(request);
        Pending::ready(Ok(InvocationState::new(
            invocation,
            tilo_actions::InvocationUpdate::Accepted { invocation },
        )
        .receipt()))
    }

    fn observe(&self, _caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        let started = self
            .invoked
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.invocation == *id);
        let running = self
            .running_for
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                left.checked_sub(1)
            })
            .is_ok();
        // As the broker does: from the contract of the action it admitted.
        let untrusted = self
            .row()
            .action
            .is_some_and(|action| action.contract.carries_untrusted_content());
        Some(
            InvocationState::new(
                *id,
                if started && !running {
                    tilo_actions::InvocationUpdate::Succeeded {
                        invocation: *id,
                        output: self.output.lock().unwrap().clone(),
                    }
                } else {
                    tilo_actions::InvocationUpdate::Accepted { invocation: *id }
                },
            )
            .with_untrusted(untrusted),
        )
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Ok(()))
    }
}

pub fn run<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

/// One `tools/call` of `server`, answering with its `result`.
pub async fn tool<H: ActionProvider + Send + Sync + 'static>(
    server: &McpServer<H>,
    name: &str,
    arguments: Value,
) -> Value {
    let reply = server
        .handle(json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":name,"arguments":arguments}}))
        .await
        .unwrap();
    reply["result"].clone()
}

pub const PNG: &[u8] = b"\x89PNG\r\n\x1a\nthe-look";
pub const JPEG: &[u8] = b"\xff\xd8\xff\xe0the-next-look";

/// The content service as the stub app `so.tilo.test` reads it: a picture
/// it stored and something that is not an image.
pub struct StoredByTestApp;

impl crate::MediaReader for StoredByTestApp {
    fn read(&self, app_id: &str, hash: &str) -> std::io::Result<Option<Vec<u8>>> {
        Ok(match (app_id, hash) {
            ("so.tilo.test" | "so.tilo.notes", "look") => Some(PNG.to_vec()),
            ("so.tilo.test", "next-look") => Some(JPEG.to_vec()),
            ("so.tilo.test", "notes") => Some(b"plain text".to_vec()),
            _ => None,
        })
    }
}

/// A host whose invocations finish when the test says so, or when they are
/// cancelled, and which says when one changed, as the broker's hosts do:
/// only to the caller that may observe it.
#[derive(Clone, Default)]
pub struct HeldHost {
    pub stub: StubHost,
    pub owner: Arc<StdMutex<Option<Caller>>>,
    pub finished: Arc<StdMutex<HashSet<InvocationId>>>,
    pub cancelled: Arc<StdMutex<HashSet<InvocationId>>>,
    pub waiting: Arc<StdMutex<Vec<Completer<()>>>>,
    pub observes: Arc<AtomicUsize>,
    pub waits: Arc<AtomicUsize>,
    /// Every invocation finishes as soon as it starts.
    pub at_once: bool,
    /// How an invocation naming no revision is refused, if it is.
    pub without_revision: Option<WithoutRevision>,
    /// Take every invocation without ever answering its admission, as a
    /// broker whose admission journal waits on a slow disk.
    pub admission_unanswered: bool,
    /// What, naming a revision other than `revision`, is refused as a
    /// directory that moved on refuses it.
    pub stale: Option<Stale>,
    pub revision: Arc<StdMutex<String>>,
    pub unanswered: Arc<StdMutex<Vec<AdmissionCompleter>>>,
    pub queries: Arc<StdMutex<Vec<CatalogueQuery>>>,
}

/// An admission answer the host holds back.
type AdmissionCompleter = Completer<Result<InvocationReceipt, ActionCallError>>;

/// Why a host refuses an invocation that names no revision.
#[derive(Clone, Copy)]
pub enum WithoutRevision {
    /// As an app built before revisionless resolution does: as a moved
    /// revision.
    Stale,
    /// As the broker refuses a directory action that asks for confirmation
    /// without one.
    NeedsConfirmation,
}

/// What a host that moved past the revision a caller named refuses.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Stale {
    /// A read: [`CataloguePage::stale_cursor`] answers a resolve that named
    /// one, and the read itself refuses.
    Reads,
    /// An invocation, as a directory that moved on since the caller read it.
    Invokes,
}

/// A half-polled answer put back as a [`Pending`], for the few tests that
/// have to look at a page before answering it.
fn map_poll(
    poll: tilo_actions::remote::PendingPoll<Result<CataloguePage, ActionCallError>>,
) -> Pending<Result<CataloguePage, ActionCallError>> {
    match poll {
        tilo_actions::remote::PendingPoll::Ready(answer) => Pending::ready(answer),
        tilo_actions::remote::PendingPoll::Waiting => {
            Pending::ready(Err(ActionCallError::Unavailable {
                reason: "the host answered nothing".into(),
            }))
        }
        tilo_actions::remote::PendingPoll::Abandoned => {
            Pending::ready(Err(ActionCallError::Unavailable {
                reason: "the host gave up".into(),
            }))
        }
    }
}

impl HeldHost {
    /// Finish `id` and wake whoever waits on a change.
    pub fn finish(&self, id: InvocationId) {
        self.finished.lock().unwrap().insert(id);
        self.wake();
    }

    fn wake(&self) {
        for waiter in self.waiting.lock().unwrap().drain(..) {
            waiter.complete(());
        }
    }

    pub fn invoked(&self) -> Vec<InvocationId> {
        self.stub
            .invoked
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.invocation)
            .collect()
    }

    fn owns(&self, caller: &Caller, id: &InvocationId) -> bool {
        self.owner.lock().unwrap().as_ref() == Some(caller) && self.invoked().contains(id)
    }

    fn cancel_now(&self, caller: &Caller, id: &InvocationId) -> Result<(), ActionCallError> {
        if !self.owns(caller, id) {
            return Err(ActionCallError::UnknownInvocation);
        }
        self.cancelled.lock().unwrap().insert(*id);
        self.wake();
        Ok(())
    }
}

impl ActionProvider for HeldHost {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        self.queries.lock().unwrap().push(query.clone());
        let revision = self.revision.lock().unwrap().clone();
        let named = query.revision.clone();
        let moved = named.as_deref().is_some_and(|named| named != revision);
        if self.stale == Some(Stale::Reads) && moved {
            return Pending::ready(Err(ActionCallError::StaleRevision));
        }
        let mut answer = self.stub.query(caller, query);
        if !moved {
            return answer;
        }
        // The page itself is read from the host's own stub; only what a
        // resolve learns about the revision it named is the directory's.
        match answer.poll_ready() {
            tilo_actions::remote::PendingPoll::Ready(Ok(mut page)) => {
                page.stale_cursor = true;
                Pending::ready(Ok(page))
            }
            other => map_poll(other),
        }
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if request.revision.is_none() {
            match self.without_revision {
                Some(WithoutRevision::Stale) => {
                    return Pending::ready(Err(ActionCallError::StaleRevision));
                }
                Some(WithoutRevision::NeedsConfirmation) => {
                    return Pending::ready(Err(ActionCallError::InvalidRequest {
                        reason: tilo_actions::remote::CONFIRMED_ACTION_NEEDS_REVISION.into(),
                    }));
                }
                None => {}
            }
        }
        if self.stale == Some(Stale::Invokes)
            && request
                .revision
                .as_deref()
                .is_some_and(|revision| revision != self.revision.lock().unwrap().as_str())
        {
            return Pending::ready(Err(ActionCallError::StaleRevision));
        }
        self.owner.lock().unwrap().replace(caller.clone());
        let answer = self.stub.invoke(caller, request);
        if !self.admission_unanswered {
            return answer;
        }
        let (admission, unanswered) = tilo_actions::remote::pending();
        self.unanswered.lock().unwrap().push(admission);
        unanswered
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.observes.fetch_add(1, Ordering::SeqCst);
        if !self.owns(caller, id) {
            return None;
        }
        let update = if self.cancelled.lock().unwrap().contains(id) {
            tilo_actions::InvocationUpdate::Cancelled {
                invocation: *id,
                message: None,
            }
        } else if self.at_once || self.finished.lock().unwrap().contains(id) {
            tilo_actions::InvocationUpdate::Succeeded {
                invocation: *id,
                output: tilo_actions::ActionOutput::default(),
            }
        } else {
            tilo_actions::InvocationUpdate::Accepted { invocation: *id }
        };
        Some(InvocationState::new(*id, update))
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        if !self.owns(caller, id) {
            return None;
        }
        self.waits.fetch_add(1, Ordering::SeqCst);
        let (waiter, change) = tilo_actions::remote::pending();
        self.waiting.lock().unwrap().push(waiter);
        Some(change)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(self.cancel_now(caller, id))
    }
}

/// The arguments of a `tilo_do` of the stub note's action as `invocation`,
/// waiting long enough for its answer that only the test ends it.
pub fn long_invoke(invocation: InvocationId) -> Value {
    json!({
        "path": "test/Note#apply", "args": {},
        "invocation_id": invocation.0.to_string(), "wait_ms": 20_000
    })
}

/// How the invocation a `tilo_do` or `tilo_status` reply is about stands, as
/// its words say: `accepted` while it is pending, `cancelled`, or `succeeded`.
///
/// # Panics
/// The reply says none of these.
#[must_use]
pub fn state(reply: &Value) -> &'static str {
    let said = reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    if said.starts_with("pending ") {
        "accepted"
    } else if said == "cancelled" {
        "cancelled"
    } else if said == "ok" {
        "succeeded"
    } else {
        panic!("no state in {reply}")
    }
}

/// One MCP session over a newline-framed connection whose other end is
/// `client`, used as an agent overlaps its calls: two invokes wait at once,
/// a `tilo_status` and one that cancels answer while they wait — the cancel
/// stopping its invoke, which a cancel queued behind the invoke's wait
/// could not — and each reply comes back whole, on a line of its own, as
/// soon as it is ready rather than in the order asked. Closing the client's
/// side ends the connection.
pub fn overlapping_lines(host: &HeldHost, client: &tilo_socket::Stream) {
    client
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    let mut lines = std::io::BufReader::new(client.try_clone().unwrap()).lines();
    let mut reply = || -> Value {
        let line = lines.next().expect("a reply line").unwrap();
        serde_json::from_str(&line).expect("each line is one whole JSON reply")
    };
    let send = |id: u64, method: &str, params: Value| {
        let request = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        writeln!(&*client, "{request}").unwrap();
    };
    let tool = |name: &str, invocation: InvocationId| {
        let arguments = match name {
            "tilo_do" => long_invoke(invocation),
            "cancel" => json!({"invocation_id": invocation.0.to_string(), "cancel": true}),
            _ => json!({"invocation_id": invocation.0.to_string()}),
        };
        let name = if name == "cancel" {
            "tilo_status"
        } else {
            name
        };
        json!({"name": name, "arguments": arguments})
    };
    send(
        1,
        "initialize",
        json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"lines","version":"1"}}),
    );
    assert_eq!(reply()["id"], 1);
    let (cancelled, finished) = (InvocationId::new(), InvocationId::new());
    send(2, "tools/call", tool("tilo_do", cancelled));
    send(3, "tools/call", tool("tilo_do", finished));
    tilo_test_env::wait_until(
        "both invokes to reach the host while neither has finished",
        || host.invoked().len() == 2,
    );
    send(4, "tools/call", tool("tilo_status", cancelled));
    let status = reply();
    assert_eq!(
        status["id"], 4,
        "the status answers while both invokes wait"
    );
    assert_eq!(state(&status), "accepted", "{status}");
    send(5, "tools/call", tool("cancel", cancelled));
    let mut answered = std::collections::BTreeMap::new();
    while !answered.contains_key(&5) {
        let next = reply();
        answered.insert(next["id"].as_u64().unwrap(), next);
    }
    assert_eq!(state(&answered[&5]), "cancelled", "{}", answered[&5]);
    if !answered.contains_key(&2) {
        let next = reply();
        answered.insert(next["id"].as_u64().unwrap(), next);
    }
    assert_eq!(state(&answered[&2]), "cancelled", "{}", answered[&2]);
    host.finish(finished);
    let done = reply();
    assert_eq!(done["id"], 3, "{done}");
    assert_eq!(state(&done), "succeeded", "{done}");
    client.shutdown(std::net::Shutdown::Write).unwrap();
    assert!(lines.next().is_none(), "no reply is left over");
}
