//! A page speaking to a host as a browser does: one peer over the host's
//! HTTPS, the records' durable changes it received, and the host's answers.
//!
//! Every request a scenario makes goes through one vocabulary: the host's
//! agent supervisor, the workspace's records, and the workspace itself.

#![allow(
    dead_code,
    reason = "each test binary uses a different subset of these fixtures"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    clippy::future_not_send,
    clippy::cast_precision_loss,
    reason = "a fixture: what it cannot set up is the failure it reports"
)]

use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use tilo_session_client::{Cmd, Event, Events, Link, Outbox};

use tilo_actions::InvocationId;
use tilo_actions::api::ActionAddress;
use tilo_actions::remote::{
    ActionCallError, CataloguePage, CatalogueQuery, InvocationReceipt, InvokeArgument,
    InvokeRequest, ReceiptStatus, RemoteActionFrame, RemoteActionMessage, RemoteActionTarget,
};

use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_client::ServerAddress;
use tilo_session_actions::Heard;

use super::hosted::Hosted;

/// How long a page waits for its host to answer a barrier, or to hold what the page wrote. A
/// healthy host answers within a second even on a loaded runner; a write still not taken after
/// this is one the host never took, and every test of a shard waiting the whole shared hang bound
/// on one kept a runner busy for an hour.
pub const BARRIER_BOUND: Duration = Duration::from_secs(60);

/// When a wait for the host to take a page's write gives up: [`BARRIER_BOUND`] from now, or the
/// shared hang bound if that is shorter.
#[must_use]
pub fn write_deadline() -> Instant {
    Instant::now()
        .checked_add(tilo_test_env::hang_bound().min(BARRIER_BOUND))
        .expect("the bound fits in an Instant")
}
use crate::support::{Device, WORKSPACE, admitted_to, ready, start_peer};
use tilo_server_protocol::WorkspaceId;

/// A record as it stood when a write landed: its author, and the record saved.
pub type Checkpoint = ([u8; 32], Vec<u8>);

/// A page in a host's workspace.
pub struct Page {
    pub peer: Link,
    pub events: Events,
    pub host_key: [u8; 32],
    /// Each record of the workspace as it stood whenever someone else's write
    /// to it landed, in order, and as this page published it.
    pub documents: BTreeMap<DocumentRoute, Vec<Checkpoint>>,
    /// Who wrote what landed in a record this page cannot open yet.
    unopened: BTreeMap<DocumentRoute, [u8; 32]>,
    /// The host's answers, taken by [`Page::answer`].
    pub answers: Vec<RemoteActionMessage>,
    /// The correlation the page names its next catalogue request with.
    pub correlation: u64,
    pub workspace: WorkspaceId,
    pub cluster_id: String,
    /// Who is here, as the page's board heard them: its host pinned as the
    /// workspace's, so the runs the host speaks for are members too.
    pub board: tilo_live::presence::Board,
    /// The semantic work narrated here that the board admits, in order:
    /// the record it was on and the work.
    pub narrated: Vec<(
        tilo_server_protocol::live::LiveResource,
        tilo_actions::SemanticInteraction,
    )>,
    /// Told each time an event reaches the page's queue.
    arrived: std::sync::Arc<tokio::sync::Notify>,
}

impl Page {
    /// A page joining the workspace as `device`, under `host`'s host.
    pub async fn open(hosted: &Hosted, device: &Device, host: &Device) -> Self {
        Self::open_workspace(hosted, device, host, WORKSPACE).await
    }

    /// A page joining the workspace `cluster_id` names, asking its host to
    /// govern it and admit the page first.
    pub async fn open_workspace(
        hosted: &Hosted,
        device: &Device,
        host: &Device,
        cluster_id: &str,
    ) -> Self {
        Self::open_at(&hosted.server, device, host, cluster_id).await
    }

    /// A session of `device` with `server`, whose own device is `served_by`, in no workspace
    /// yet: what a page starts before it joins one, and what a device asks its server over
    /// for a workspace of its person's own.
    pub fn session(
        server: &tilo_server::daemon::Server,
        device: &Device,
        served_by: &Device,
    ) -> Link {
        start_peer(
            ServerAddress::at(server.node_addr()),
            device.cert.clone(),
            served_by.cert.clone(),
            device.key.clone(),
        )
    }

    /// [`Self::open_workspace`] on any host `server` runs, whatever serves
    /// its records.
    pub async fn open_at(
        server: &tilo_server::daemon::Server,
        device: &Device,
        host: &Device,
        cluster_id: &str,
    ) -> Self {
        let workspace = tilo_server_protocol::WorkspaceId::parse(cluster_id).unwrap();
        let peer = Self::session(server, device, host);
        let events = peer.events();
        if cluster_id == WORKSPACE {
            // The page asks its host to govern its person's workspace and
            // admit it, as every page does before joining one.
            peer.join_placed(cluster_id, "Workspace").unwrap();
        } else {
            ready(&[&peer]).await;
            let answer = peer
                .ask_workspaces(tilo_server_protocol::workspaces::WorkspacesRequest::Enter {
                    workspace: cluster_id.into(),
                })
                .unwrap();
            let entered = tilo_test_env::within("the node enters the governed fixture", answer)
                .await
                .unwrap()
                .unwrap();
            assert!(
                matches!(
                    entered,
                    tilo_server_protocol::workspaces::WorkspacesReply::Workspace(_)
                ),
                "the node enters the governed fixture: {entered:?}"
            );
            peer.send(Cmd::Join(workspace)).unwrap();
        }
        // The page acts as soon as it is open, and an action the host has not
        // subscribed the session to yet is refused and never answered.
        admitted_to(&peer, workspace).await;
        Self::joined((peer, events), device, host, cluster_id)
    }

    /// The page `peer` is, as `device`, once it joined the workspace
    /// `cluster_id` names under `host`'s host: `events` are its peer's,
    /// taken before it joined.
    pub fn joined(
        (peer, events): (Link, Events),
        device: &Device,
        host: &Device,
        cluster_id: &str,
    ) -> Self {
        let workspace = tilo_server_protocol::WorkspaceId::parse(cluster_id).unwrap();
        let mut board = tilo_live::presence::Board::new(cluster_id, device.cert.device_public_key);
        board.set_host(Some(host.cert.device_public_key));
        let arrived = std::sync::Arc::new(tokio::sync::Notify::new());
        let told = std::sync::Arc::clone(&arrived);
        peer.on_event(&events, std::sync::Arc::new(move || told.notify_one()));
        Self {
            arrived,
            board,
            narrated: Vec::new(),
            events,
            peer,
            host_key: host.cert.device_public_key,
            documents: BTreeMap::new(),
            unopened: BTreeMap::new(),
            answers: Vec::new(),
            correlation: 0,
            workspace,
            cluster_id: cluster_id.to_owned(),
        }
    }

    /// Take one event: a record that changed is kept as it stands then,
    /// everything else is the caller's to read.
    pub fn take(&mut self, event: Event) -> Option<Event> {
        if let Event::LiveSession {
            ws: workspace_id,
            from,
            track: tilo_live::live::Track::Presence,
            body,
            ..
        } = &event
            && *workspace_id == self.workspace
        {
            self.board
                .heard(*from, body, tilo_clock::unix_now_ms() as f64);
        }
        if let Event::Live {
            ws: workspace_id,
            from,
            frame,
        } = &event
            && *workspace_id == self.workspace
            && let Ok(tilo_server_protocol::live::LiveFrame::Ops(
                tilo_server_protocol::live::OpsFrame::Semantic { record, event },
            )) = tilo_server_protocol::live::LiveFrame::decode(frame)
        {
            let here = self.here();
            if let Some(work) = tilo_session_client::narration::admit(&here, *from, &event) {
                self.narrated.push((record, work));
            }
        }
        let Event::Landed { route, author } = event else {
            return Some(event);
        };
        let own = self.peer.engine().identity().device_cert.device_public_key;
        if author == Some(own) {
            // Kept when it was published.
            return None;
        }
        let read = self.read(&route);
        // Keys that arrived open what the writers before them wrote; keys that open nothing new
        // say nobody wrote anything.
        match (read, author.or_else(|| self.unopened.remove(&route))) {
            (Some(bytes), Some(author)) => {
                self.documents
                    .entry(route)
                    .or_default()
                    .push((author, bytes));
            }
            (_, Some(author)) => {
                self.unopened.insert(route, author);
            }
            (_, None) => {}
        }
        None
    }

    /// What this page's holder reads of `route`: the record whole, when it reads.
    fn read(&self, route: &DocumentRoute) -> Option<Vec<u8>> {
        crate::support::record_of(&self.peer, route)
    }

    /// The records whose writes reached this page before it could open them.
    pub fn unopened(&self) -> Vec<String> {
        self.unopened
            .keys()
            .map(|route| format!("{}/{}", route.app_id, route.document))
            .collect()
    }

    /// Everyone here now, as a surface draws them.
    pub fn here(&self) -> Vec<collab_types::presence::Shown> {
        let now = tilo_clock::unix_now_ms() as f64;
        self.board
            .here()
            .map(|member| tilo_live::presence::shown(member, now))
            .collect()
    }

    /// Take everything the page received.
    pub fn pump(&mut self) {
        for event in self.events.drain() {
            match self.take(event) {
                Some(Event::Remote(Heard::Action { from, frame, .. })) if from == self.host_key => {
                    self.answers
                        .push(RemoteActionFrame::decode(&frame).unwrap().message);
                }
                Some(Event::Remote(Heard::Undeliverable { refusal, .. })) => {
                    panic!("undeliverable: {refusal}")
                }
                Some(_) | None => {}
            }
        }
    }

    /// Follow the parts of `app`'s chain whose head is `head`, as a window
    /// showing them asks for them: a workspace sends a page a chain's parts
    /// once it asks for the chain's head.
    pub fn follow_chain(&self, app: &str, head: &str) {
        self.peer
            .send(Cmd::OpenDocumentAt(
                self.workspace,
                DocumentRoute {
                    app_id: app.into(),
                    cluster_id: self.cluster_id.clone(),
                    document: head.into(),
                },
            ))
            .unwrap();
    }

    /// The route of the workspace's shared layout.
    fn layout_route(&self) -> DocumentRoute {
        DocumentRoute {
            app_id: tilo_desktop_model::ownership::DESKTOP_APP.into(),
            cluster_id: self.cluster_id.clone(),
            document: tilo_desktop_model::ownership::DESKTOP_DOCUMENT.into(),
        }
    }

    /// Write the workspace's shared layout as a page first writes it: the
    /// workspace by its own id, one strip and no window yet, in the shape a
    /// page keeps its layout in ([`tilo_desktop_model::desktop_doc`]).
    pub fn put_layout(&mut self) {
        use tilo_desktop_model::{Layout, Strip, StripId, Workspace, WorkspaceId};
        let id = WorkspaceId::parse(&self.cluster_id).unwrap_or_else(|| WorkspaceId::from_u128(1));
        let mut workspace = Workspace::new(id, "Workspace");
        workspace.strips = vec![Strip::new(StripId(10), "Main")];
        let tree = Layout {
            active: id,
            workspaces: vec![workspace],
            ..Layout::default()
        };
        self.put_layout_tree(&tree);
    }

    /// Write `tree` as the workspace's shared layout, over the layout the page wrote: what a
    /// member's device writes when they open, close or move a window.
    pub fn put_layout_tree(&mut self, tree: &tilo_desktop_model::Layout) {
        let route = self.layout_route();
        // Over the layout as this page last published it, so a later write follows from it.
        let mut doc = self
            .documents
            .get(&route)
            .and_then(|kept| kept.last())
            .and_then(|(_, bytes)| automerge::AutoCommit::load(bytes).ok())
            .unwrap_or_default();
        let before = doc.get_heads();
        tilo_desktop_model::desktop_doc::write_shared(
            &mut doc,
            tree,
            &[],
            &[],
            &std::collections::BTreeMap::new(),
        );
        doc.commit();
        let bytes = doc.save();
        // The record whole the first time, what changed after.
        let mutation = if before.is_empty() {
            collab_types::DocumentMutation::Snapshot(bytes.clone())
        } else {
            collab_types::DocumentMutation::Incremental(doc.save_after(&before))
        };
        self.peer.write(route.clone(), &mutation).unwrap();
        // The host never echoes a device's own change back to it.
        self.documents
            .entry(route)
            .or_default()
            .push(([0; 32], bytes));
    }

    /// Open `app` in a window of the workspace's shared layout, as `person` opens it on this
    /// device: the change that places it, written into the layout this page holds.
    pub fn open_on_layout(&mut self, app: &str, title: &str, person: &str) {
        let route = self.layout_route();
        let bytes = self
            .read(&route)
            .expect("the page holds the workspace's layout");
        let doc = automerge::AutoCommit::load(&bytes).unwrap();
        let change = tilo_desktop_model::place::place_view(
            &doc,
            &tilo_desktop_model::place::Opener {
                person,
                opened_by: None,
            },
            &tilo_desktop_model::place::View {
                app_id: app,
                title,
                document: "",
                beside: None,
            },
        )
        .unwrap()
        .expect("the window is added");
        self.peer
            .write(
                route.clone(),
                &collab_types::DocumentMutation::Incremental(change.clone()),
            )
            .unwrap();
        self.documents
            .entry(route)
            .or_default()
            .push(([0; 32], change));
    }

    /// The workspace's shared layout as the page has received it.
    pub fn layout(&mut self) -> Option<tilo_desktop_model::ownership::SharedDesktop> {
        self.pump();
        let layout = self.layout_route();
        let bytes = self.read(&layout)?;
        tilo_desktop_model::shared_resource::read(&automerge::AutoCommit::load(&bytes).ok()?, None)
    }

    /// The views asked of the workspace's layout, as the page has received its document
    /// ([`tilo_desktop_model::asked`]).
    pub fn asked_views(&mut self) -> Vec<tilo_desktop_model::asked::AskedView> {
        self.pump();
        let layout = self.layout_route();
        let Some(bytes) = self.read(&layout) else {
            return Vec::new();
        };
        automerge::AutoCommit::load(&bytes).map_or_else(
            |_| Vec::new(),
            |doc| {
                tilo_desktop_model::asked::asked(&doc)
                    .into_iter()
                    .map(|(_, view)| view)
                    .collect()
            },
        )
    }

    /// Send `message` to the host.
    pub fn send(&self, message: RemoteActionMessage) {
        let frame = RemoteActionFrame::new(message).encode().unwrap();
        self.peer
            .send(Cmd::RemoteAction(self.workspace, self.host_key, frame))
            .unwrap();
    }

    /// Wait for the first answer `pick` accepts, removing it.
    pub async fn answer<T>(
        &mut self,
        what: &str,
        pick: impl Fn(&RemoteActionMessage) -> Option<T>,
    ) -> T {
        self.until(|page| {
            let index = page
                .answers
                .iter()
                .position(|answer| pick(answer).is_some())
                .ok_or_else(|| format!("no answer: {what}"))?;
            Ok(pick(&page.answers.remove(index)).unwrap())
        })
        .await
    }

    /// What `read` first answers of the page, read again each time an event reaches it rather
    /// than on a timer. `read` says what it still waits for, which the wait fails with once it
    /// outlasts the hang bound.
    pub async fn until<T>(&mut self, mut read: impl FnMut(&mut Self) -> Result<T, String>) -> T {
        let deadline = tokio::time::Instant::from_std(tilo_test_env::hang_deadline());
        loop {
            self.pump();
            let waiting = match read(self) {
                Ok(answer) => return answer,
                Err(waiting) => waiting,
            };
            // An event that reached the queue since the pump left its notice behind.
            let arrived = std::sync::Arc::clone(&self.arrived);
            if tokio::time::timeout_at(deadline, arrived.notified())
                .await
                .is_err()
            {
                panic!("{waiting}");
            }
        }
    }

    /// The host's catalogue of the workspace's records and their actions.
    pub async fn catalogue(
        &mut self,
        query: CatalogueQuery,
    ) -> Result<CataloguePage, ActionCallError> {
        self.correlation = self.correlation.checked_add(1).unwrap();
        let correlation = self.correlation;
        self.send(RemoteActionMessage::CatalogueRequest { correlation, query });
        self.answer("catalogue", |answer| match answer {
            RemoteActionMessage::CatalogueResponse {
                correlation: got,
                result,
            } if *got == correlation => Some(result.clone()),
            _ => None,
        })
        .await
    }

    /// The receipt of `invocation`.
    pub async fn receipt(&mut self, invocation: InvocationId) -> InvocationReceipt {
        self.answer("receipt", |answer| match answer {
            RemoteActionMessage::Receipt(receipt) if receipt.invocation == invocation => {
                Some(receipt.clone())
            }
            _ => None,
        })
        .await
    }

    /// Run `request`, answering its receipt.
    pub async fn invoke(&mut self, request: InvokeRequest) -> InvocationReceipt {
        let invocation = request.invocation;
        self.send(RemoteActionMessage::Invoke(request));
        self.receipt(invocation).await
    }

    /// Run `request` and follow it as a page does: the host answers accepted
    /// while a change's save is still landing, and the invocation is observed
    /// until it ends.
    pub async fn invoke_settled(&mut self, request: InvokeRequest) -> InvocationReceipt {
        let receipt = self.invoke(request).await;
        self.settle(receipt).await
    }

    /// Follow `receipt` while the host has it accepted: the invocation is
    /// observed until it ends.
    pub async fn settle(&mut self, mut receipt: InvocationReceipt) -> InvocationReceipt {
        let invocation = receipt.invocation;
        let deadline = tilo_test_env::hang_deadline();
        while matches!(receipt.status, ReceiptStatus::Accepted) {
            assert!(
                Instant::now() < deadline,
                "the invocation never settled: {receipt:?}"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
            self.send(RemoteActionMessage::observe(invocation));
            let state = self
                .answer("update", |answer| match answer {
                    RemoteActionMessage::Update {
                        invocation: got,
                        state,
                        ..
                    } if *got == invocation => Some(state.clone()),
                    _ => None,
                })
                .await;
            if let Some(state) = state {
                receipt = state.receipt();
            }
        }
        receipt
    }

    /// Call one action on the record `provider` serves, followed to its end: a
    /// session answers a page accepted while what the action changes is still
    /// landing, as it does for a person's own click.
    pub async fn invoke_on(
        &mut self,
        provider: &str,
        address: ActionAddress,
        argument: InvokeArgument,
    ) -> Result<tilo_actions::ActionOutput, String> {
        let invocation = InvocationId::new();
        self.send(RemoteActionMessage::Invoke(InvokeRequest {
            on_behalf_of: None,
            invocation,
            target: RemoteActionTarget {
                provider: provider.into(),
                address,
            },
            argument,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }));
        let answered = self.receipt(invocation).await;
        let receipt = self.settle(answered).await;
        match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output),
            other => Err(format!("{other:?}")),
        }
    }

    /// Wait until everything this page published is durable on the host.
    pub async fn durable(&self) {
        let receipt = self.peer.barrier().unwrap();
        let deadline = write_deadline();
        loop {
            match receipt.try_recv() {
                Ok(result) => return result.unwrap(),
                Err(tilo_session_client::TryRecvError::Empty) => {}
                Err(error) => panic!("the barrier ended: {error}"),
            }
            if Instant::now() >= deadline {
                // What the session still keeps, and what each write waits for, says why.
                let kept = match self.peer.kept_of(&[self.workspace]) {
                    Ok(kept) => tilo_test_env::within("the session says what it keeps", kept)
                        .await
                        .map_or_else(
                            |error| format!("nothing: {error:?}"),
                            |kept| format!("{kept:?}"),
                        ),
                    Err(error) => format!("nothing: {error:?}"),
                };
                panic!(
                    "the page's edits never became durable: the host did not answer its barrier \
                     within {BARRIER_BOUND:?}; the session keeps {kept}"
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// How many changes of `route` the page received.
    pub fn heard(&self, route: &DocumentRoute) -> usize {
        self.documents.get(route).map_or(0, Vec::len)
    }

    /// Wait until the page has received more than `past` changes of `route`.
    pub async fn until_heard(&mut self, route: &DocumentRoute, past: usize) {
        self.until(|page| {
            (page.heard(route) > past)
                .then_some(())
                .ok_or_else(|| format!("no change of {route:?} arrived"))
        })
        .await;
    }
}
