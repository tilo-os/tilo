//! The documents of the person's workspaces as the desktop reads and writes them: through the
//! device's daemon, which keeps the workspaces, on the shell link the daemon handed the desktop
//! when it started it ([`runtime_collab::wire::shell`]).
//!
//! The desktop keeps no copy of a workspace and none of its keys: a read is answered from
//! the daemon's copy, and a write is signed and sealed by the daemon as the device. A desktop the
//! daemon did not start has no link, and each of its asks is refused saying so.

#![cfg(unix)]

use std::sync::Arc;

use runtime_collab::ShellClient;
use runtime_collab::wire::shell::{DocumentWritten, ShellCall, ShellReply, ShellWrite};
use tilo_actions::remote::{
    InvokeRequest, ReceiptStatus, RemoteActionFrame, RemoteActionMessage, ResourceReadRequest,
};
use tilo_peer_state::owner::{AppWrite, WriteRefusal};
use tilo_peer_state::workspace_documents::{
    DeviceDocuments, DeviceWrites, DocumentRead, DocumentServing, DocumentWrite, DocumentWriting,
};

/// What a desktop the daemon did not start is told of each ask of its workspaces.
pub const NOT_STARTED_BY_THE_DAEMON: &str =
    "this desktop was not started by the device's daemon, which keeps the person's workspaces";

/// The shell link the daemon handed this process when it started it, or `handed`, the link it made
/// for a session a test runs in its own process, opened on `runtime`; or why there is none.
///
/// # Errors
/// The daemon did not start this process, or the descriptor it handed is no socket.
pub fn inherited_link(
    handed: Option<std::os::fd::OwnedFd>,
    runtime: &tokio::runtime::Handle,
) -> Result<ShellClient, String> {
    let link = match handed {
        Some(link) => {
            let link = std::os::unix::net::UnixStream::from(link);
            link.set_nonblocking(true).map(|()| link)
        }
        None => ShellClient::inherited().ok_or(NOT_STARTED_BY_THE_DAEMON)?,
    }
    .map_err(|error| format!("the link the device's daemon handed this desktop: {error}"))?;
    let _entered = runtime.enter();
    let link = tokio::net::UnixStream::from_std(link)
        .map_err(|error| format!("the link the device's daemon handed this desktop: {error}"))?;
    Ok(ShellClient::open(link).0)
}

/// The shell's connection to the device's daemon.
#[derive(Clone)]
pub struct DaemonShell {
    inner: Arc<Inner>,
}

struct Inner {
    /// The shell link, or why this desktop has none.
    link: Result<ShellClient, String>,
    /// What carries the connection's frames: a runtime with a worker of its own, never the thread
    /// a synchronous reader blocks.
    runtime: tokio::runtime::Handle,
}

impl std::fmt::Debug for DaemonShell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonShell")
            .field("linked", &self.inner.link.is_ok())
            .finish_non_exhaustive()
    }
}

impl DaemonShell {
    /// The shell's connection to the daemon over `link`, carried on `runtime`.
    #[must_use]
    pub fn new(link: Result<ShellClient, String>, runtime: tokio::runtime::Handle) -> Self {
        Self {
            inner: Arc::new(Inner { link, runtime }),
        }
    }

    /// The shell link, or why this desktop has none: what its apps' connections are vouched for
    /// on.
    pub fn link(&self) -> Result<ShellClient, String> {
        self.inner.link.clone()
    }

    /// Ask the daemon `call`.
    ///
    /// # Errors
    /// The desktop has no link, the daemon refused the call, or the link ended.
    pub async fn ask(&self, call: ShellCall) -> Result<ShellReply, String> {
        let inner = Arc::clone(&self.inner);
        self.inner
            .runtime
            .spawn(async move { inner.ask(call).await })
            .await
            .map_err(|_| "the daemon's connection stopped before it answered".to_owned())?
    }

    /// [`Self::ask`] from a thread that is not the runtime's: what a synchronous reader waits
    /// with.
    fn ask_blocking(&self, call: ShellCall) -> Result<ShellReply, String> {
        let (answered, answer) = std::sync::mpsc::sync_channel(1);
        let inner = Arc::clone(&self.inner);
        self.inner.runtime.spawn(async move {
            let _ = answered.send(inner.ask(call).await);
        });
        answer
            .recv()
            .map_err(|_| "the daemon's connection stopped before it answered".to_owned())?
    }

    /// What carries the connection's frames: where a follower of a read waits.
    #[must_use]
    pub fn runtime(&self) -> &tokio::runtime::Handle {
        &self.inner.runtime
    }

    /// Ask the device's one dispatch `message` as the person, in `workspace` where it is an
    /// action of one of its resources: the readings a read answers, or the value an action
    /// answered, as JSON.
    ///
    /// # Errors
    /// As [`Self::ask`], or the dispatch refused it, saying why.
    pub async fn dispatch(
        &self,
        workspace: Option<&str>,
        message: &RemoteActionMessage,
    ) -> Result<serde_json::Value, String> {
        let call = dispatched(workspace, message)?;
        answered(self.ask(call).await?)
    }

    /// [`Self::dispatch`] from a thread that is not the runtime's.
    ///
    /// # Errors
    /// As [`Self::dispatch`].
    pub fn dispatch_blocking(
        &self,
        workspace: Option<&str>,
        message: &RemoteActionMessage,
    ) -> Result<serde_json::Value, String> {
        let call = dispatched(workspace, message)?;
        answered(self.ask_blocking(call)?)
    }

    /// The readings of `read`, as the device reads them for the person.
    ///
    /// # Errors
    /// As [`Self::dispatch`].
    pub async fn read(&self, read: ResourceReadRequest) -> Result<serde_json::Value, String> {
        self.dispatch(None, &read_message(read)).await
    }

    /// [`Self::read`] from a thread that is not the runtime's.
    ///
    /// # Errors
    /// As [`Self::dispatch`].
    pub fn read_blocking(&self, read: ResourceReadRequest) -> Result<serde_json::Value, String> {
        self.dispatch_blocking(None, &read_message(read))
    }

    /// Run `request`, a typed action of a resource of `workspace`, as the person, from a thread
    /// that is not the runtime's: the value it answered.
    ///
    /// # Errors
    /// As [`Self::dispatch`]; a refusal says why.
    pub fn invoke_blocking(
        &self,
        workspace: &str,
        request: InvokeRequest,
    ) -> Result<serde_json::Value, String> {
        self.dispatch_blocking(Some(workspace), &RemoteActionMessage::Invoke(request))
    }

    /// The documents the device took a write to since this connection last asked, waited for
    /// until there is one; `None` when it took more than it kept told.
    ///
    /// # Errors
    /// As [`Self::ask`].
    pub async fn next_writes(&self) -> Result<Option<Vec<DocumentWritten>>, String> {
        match self.ask(ShellCall::Writes).await? {
            ShellReply::Writes(writes) => Ok(writes),
            other => Err(unexpected(&other)),
        }
    }
}

impl Inner {
    async fn ask(&self, call: ShellCall) -> Result<ShellReply, String> {
        let link = self.link.as_ref().map_err(Clone::clone)?;
        link.call(call).await.map_err(|error| match error {
            runtime_portal::NotAvailable::Refused(reason) => reason,
            error => format!("the device's daemon: {error}"),
        })
    }
}

/// `message` as the shell link carries it.
fn dispatched(workspace: Option<&str>, message: &RemoteActionMessage) -> Result<ShellCall, String> {
    Ok(ShellCall::Action {
        workspace: workspace.map(str::to_owned),
        frame: RemoteActionFrame::new(message.clone())
            .encode()
            .map_err(|error| format!("a call does not encode: {error}"))?,
        app: None,
    })
}

/// The JSON the dispatch answered with: the readings of a read, or the output of an action that
/// settled; or why it refused.
fn answered(reply: ShellReply) -> Result<serde_json::Value, String> {
    let ShellReply::Action(frame) = reply else {
        return Err(unexpected(&reply));
    };
    let message = RemoteActionFrame::decode(&frame)
        .map_err(|error| format!("the dispatch's answer does not read: {error}"))?
        .message;
    match message {
        RemoteActionMessage::ResourceReading { result, .. } => {
            serde_json::from_str(&result.map_err(|error| error.to_string())?)
                .map_err(|error| format!("the dispatch's answer does not read: {error}"))
        }
        RemoteActionMessage::Receipt(receipt) => match receipt.status {
            ReceiptStatus::Completed { output } => serde_json::to_value(&output)
                .map_err(|error| format!("the answer does not encode: {error}")),
            ReceiptStatus::Rejected { error } => Err(error.to_string()),
            ReceiptStatus::Failed { error } => Err(error.message),
            status => Err(format!("the action did not settle: {status:?}")),
        },
        other => Err(format!("the dispatch answered something else: {other:?}")),
    }
}

/// `read` as the dispatch takes it.
const fn read_message(read: ResourceReadRequest) -> RemoteActionMessage {
    RemoteActionMessage::ResourceRead {
        correlation: 0,
        on_behalf_of: None,
        read,
    }
}

/// A read of `app`'s `resource` of `workspace`, whole, as it stands now.
fn whole(workspace: &str, app: &str, resource: &str) -> ResourceReadRequest {
    ResourceReadRequest {
        workspace: workspace.to_owned(),
        app: app.to_owned(),
        record: resource.to_owned(),
        joined: false,
        after: None,
        wait_ms: None,
        document: true,
        listing: false,
    }
}

/// What one resource of `readings` is: its document whole, `Absent`, or `Pending` while it is on
/// its way.
///
/// # Errors
/// The readings name no resource, or its document does not read.
pub fn document_read(readings: &serde_json::Value) -> Result<DocumentRead, String> {
    let resource = readings["records"]
        .get(0)
        .ok_or("the read answered no resource")?;
    if resource["absent"].as_bool() == Some(true) {
        return Ok(DocumentRead::Absent);
    }
    if resource["arriving"].as_bool() == Some(true) {
        return Ok(DocumentRead::Pending);
    }
    let bytes = resource["document"]
        .as_str()
        .ok_or("the read answered no document")
        .and_then(|hex| hex::decode(hex).map_err(|_| "a document's bytes do not read"))?;
    let mut doc = automerge::AutoCommit::load(&bytes)
        .map_err(|error| format!("a document does not read: {error}"))?;
    let heads = doc.get_heads().iter().map(|head| head.0).collect();
    let revision = readings["revision"].as_str().unwrap_or_default();
    let revision = hex::decode(revision)
        .ok()
        .and_then(|revision| <[u8; 32]>::try_from(revision).ok())
        .unwrap_or_else(|| *blake3::hash(revision.as_bytes()).as_bytes());
    Ok(DocumentRead::Whole {
        revision,
        heads,
        bytes,
    })
}

fn unexpected(reply: &ShellReply) -> String {
    format!("the daemon answered something else: {reply:?}")
}

/// [`DeviceDocuments`] answered by the device's daemon.
#[derive(Clone, Debug)]
pub struct DaemonDocuments(pub DaemonShell);

impl DaemonDocuments {
    /// Whether the member whose device `certificate` names may read `app`'s `resource` of
    /// `workspace`, as the daemon decides it at the resource.
    ///
    /// # Errors
    /// The member may not, or the daemon did not answer.
    pub fn authorizes(
        &self,
        workspace: &str,
        app: &str,
        resource: &str,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        let certificate = serde_json::to_string(certificate)
            .map_err(|error| format!("a certificate does not encode: {error}"))?;
        match self.0.ask_blocking(ShellCall::Authorizes {
            workspace: workspace.to_owned(),
            app: app.to_owned(),
            resource: resource.to_owned(),
            certificate,
        })? {
            ShellReply::Authorized(allowed) => allowed,
            other => Err(unexpected(&other)),
        }
    }
}

impl DeviceDocuments for DaemonDocuments {
    fn read(
        &self,
        workspace: &str,
        app: &str,
        resource: &str,
        since: Option<&[[u8; 32]]>,
    ) -> Result<DocumentRead, String> {
        // A read answers the document whole, whatever the reader holds of it already.
        let _ = since;
        document_read(&self.0.read_blocking(whole(workspace, app, resource))?)
    }

    fn readable(&self, workspace: &str, app: &str, resource: &str) -> bool {
        let asked = self.0.ask_blocking(ShellCall::Readable {
            workspace: workspace.to_owned(),
            app: app.to_owned(),
            resource: resource.to_owned(),
        });
        answered_is(asked, resource)
    }

    fn listing(&self, workspace: &str) -> Result<Vec<(String, String)>, String> {
        let mut listing = whole(workspace, "", "");
        listing.listing = true;
        let listed = self.0.read_blocking(listing)?;
        Ok(listed["records"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|listed| {
                Some((
                    listed["app"].as_str()?.to_owned(),
                    listed["record"].as_str()?.to_owned(),
                ))
            })
            .collect())
    }

    fn holds(&self, workspace: &str, app: &str, resource: &str) -> bool {
        match self.read(workspace, app, resource, None) {
            Ok(read) => !matches!(read, DocumentRead::Absent),
            Err(error) => {
                tracing::warn!(resource, %error, "the daemon did not say");
                false
            }
        }
    }

    fn workspaces(&self) -> Vec<String> {
        match self.0.ask_blocking(ShellCall::Workspaces) {
            Ok(ShellReply::Workspaces(workspaces)) => workspaces,
            Ok(other) => {
                tracing::warn!(answer = %unexpected(&other), "the device's workspaces did not read");
                Vec::new()
            }
            Err(error) => {
                tracing::warn!(%error, "the device's workspaces did not read");
                Vec::new()
            }
        }
    }
}

/// A yes or no the daemon answered about `resource`; no where it could not say.
fn answered_is(asked: Result<ShellReply, String>, resource: &str) -> bool {
    match asked {
        Ok(ShellReply::Is(is)) => is,
        Ok(other) => {
            tracing::warn!(resource, answer = %unexpected(&other), "the daemon did not say");
            false
        }
        Err(error) => {
            tracing::warn!(resource, %error, "the daemon did not say");
            false
        }
    }
}

/// The device's one search, as the daemon answers it for the person on the shell link: the same
/// index, and the same reads decided per record, that `tilo do`, an agent's `find` and every other
/// caller of the daemon search. The daemon answers the link's person: a caller here is who the
/// link was handed to.
#[derive(Clone, Debug)]
pub struct DaemonSearch(pub DaemonShell);

impl DaemonSearch {
    /// Ask the daemon's search `request`, and read what it answered as `T`.
    fn asked<T: serde::de::DeserializeOwned + Send + 'static>(
        &self,
        request: InvokeRequest,
    ) -> tilo_actions::remote::Pending<Result<T, tilo_actions::remote::ActionCallError>> {
        let (answer, pending) = tilo_actions::remote::pending();
        let shell = self.0.clone();
        self.0.runtime().spawn(async move {
            let answered = shell
                .dispatch(None, &RemoteActionMessage::Invoke(request))
                .await
                .and_then(|output| {
                    let output: tilo_actions::ActionOutput = serde_json::from_value(output)
                        .map_err(|error| format!("the search's answer does not read: {error}"))?;
                    serde_json::from_value(output.value.unwrap_or_default())
                        .map_err(|error| format!("the search's answer does not read: {error}"))
                })
                .map_err(|reason| tilo_actions::remote::ActionCallError::Unavailable { reason });
            answer.complete(answered);
        });
        pending
    }
}

impl tilo_actions::remote::ResourceSearch for DaemonSearch {
    fn find(
        &self,
        caller: &tilo_actions::remote::Caller,
        needle: &str,
    ) -> tilo_actions::remote::Pending<
        Result<Vec<tilo_actions::remote::Found>, tilo_actions::remote::ActionCallError>,
    > {
        self.find_within(caller, needle, &tilo_actions::remote::Within::default())
    }

    fn find_within(
        &self,
        _caller: &tilo_actions::remote::Caller,
        needle: &str,
        within: &tilo_actions::remote::Within,
    ) -> tilo_actions::remote::Pending<
        Result<Vec<tilo_actions::remote::Found>, tilo_actions::remote::ActionCallError>,
    > {
        self.asked(tilo_actions::remote::search::Query::request(
            tilo_actions::InvocationId::new(),
            needle,
            within,
        ))
    }

    fn pictures(
        &self,
        _caller: &tilo_actions::remote::Caller,
    ) -> tilo_actions::remote::Pending<
        Result<Vec<tilo_actions::remote::search::Picture>, tilo_actions::remote::ActionCallError>,
    > {
        self.asked(tilo_actions::remote::search::pictures_request(
            tilo_actions::InvocationId::new(),
        ))
    }

    fn describe(
        &self,
        _caller: &tilo_actions::remote::Caller,
        description: tilo_actions::remote::search::Description,
    ) -> tilo_actions::remote::Pending<Result<(), tilo_actions::remote::ActionCallError>> {
        self.asked(tilo_actions::remote::search::describe_request(
            tilo_actions::InvocationId::new(),
            &description,
        ))
    }
}

/// [`DeviceWrites`] the device's daemon signs and seals as the device.
#[derive(Clone, Debug)]
pub struct DaemonWrites(pub DaemonShell);

impl DeviceWrites for DaemonWrites {
    fn write(
        &self,
        workspace: &str,
        changes: &[u8],
        writing: DocumentWriting<'_>,
    ) -> DocumentWrite {
        // The daemon seals with the keys of the document's group itself: `writing.sealed` is
        // what a service with the keys in this process would seal with, and the shell has
        // none.
        let call = ShellCall::Write {
            workspace: workspace.to_owned(),
            app: writing.app.to_owned(),
            document: writing.document.to_owned(),
            readers: writing.readers.map(str::to_owned),
            founding: writing.founding,
            snapshot: writing.snapshot,
            changes: changes.to_vec(),
        };
        let shell = self.0.clone();
        Box::pin(async move {
            match shell.ask(call).await.map_err(WriteRefusal::Refused)? {
                ShellReply::Written(Ok(write)) => app_write(write).map_err(WriteRefusal::Refused),
                ShellReply::Written(Err(refusal)) => {
                    Err(serde_json::from_str(&refusal).unwrap_or(WriteRefusal::Refused(refusal)))
                }
                other => Err(WriteRefusal::Refused(unexpected(&other))),
            }
        })
    }

    fn served(&self, workspace: &str, documents: &[(&str, &str)]) -> Option<DocumentServing> {
        let call = ShellCall::Served {
            workspace: workspace.to_owned(),
            documents: documents
                .iter()
                .map(|(app, document)| ((*app).to_owned(), (*document).to_owned()))
                .collect(),
        };
        // Whether the daemon has a serving device to ask is its answer; until it answered, this
        // waits as a wait on that device does.
        let shell = self.0.clone();
        Some(Box::pin(async move {
            match shell.ask(call).await? {
                ShellReply::Served(Some(served)) => served,
                // The daemon keeps the workspace itself: what it took is served.
                ShellReply::Served(None) => Ok(()),
                other => Err(unexpected(&other)),
            }
        }))
    }
}

/// The write the daemon answered, as the device's own writes answer.
fn app_write(write: ShellWrite) -> Result<AppWrite, String> {
    Ok(AppWrite {
        peer: write.peer,
        app_id: write.app_id,
        cluster_id: write.cluster_id,
        document_id: write.document_id,
        author: serde_json::from_str(&write.author)
            .map_err(|error| format!("a write's author does not read: {error}"))?,
        replayed: write.replayed,
        envelope: postcard::from_bytes(&write.envelope)
            .map_err(|error| format!("a write does not read: {error}"))?,
    })
}

/// The documents and writes of the device's daemon on the shell's `link`, carried on `runtime`.
#[must_use]
pub fn on_daemon(
    link: Result<ShellClient, String>,
    runtime: tokio::runtime::Handle,
) -> (Arc<dyn DeviceDocuments>, Arc<dyn DeviceWrites>, DaemonShell) {
    let shell = DaemonShell::new(link, runtime);
    (
        Arc::new(DaemonDocuments(shell.clone())),
        Arc::new(DaemonWrites(shell.clone())),
        shell,
    )
}

/// The receipts of the effects the desktop's own actions had, recorded by the device's daemon in
/// their workspaces as this device's: the desktop keeps no copy to record them in.
#[derive(Clone, Debug)]
pub struct DaemonReceipts(pub DaemonShell);

impl tilo_broker_core::workspace_commits::WorkspaceCommits for DaemonReceipts {
    fn projects_workspace(&self, workspace: tilo_desktop_model::WorkspaceId) -> bool {
        let workspace = workspace.to_string();
        let asked = self.0.ask_blocking(ShellCall::KeepsReceipts {
            workspace: workspace.clone(),
        });
        answered_is(asked, &workspace)
    }

    fn health(&self) -> Result<(), String> {
        self.0.link().map(drop)
    }

    /// An app's writes go to the daemon on the app's own connection there, never through the
    /// desktop.
    fn document(
        &self,
        _workspace: String,
        _app: String,
        _document: String,
        _readers: Option<String>,
        _envelope: collab_types::SignedOp,
        _resource_type: Option<String>,
    ) -> tilo_broker_core::workspace_commits::DocumentFuture {
        Box::pin(std::future::ready(Err(
            "an app writes its documents at the device's daemon, on its own connection there"
                .to_owned(),
        )))
    }

    fn effect(
        &self,
        workspace: String,
        redacted: Vec<u8>,
    ) -> tilo_broker_core::workspace_commits::EffectFuture {
        let shell = self.0.clone();
        Box::pin(async move {
            match shell
                .ask(ShellCall::Receipt {
                    workspace,
                    redacted,
                })
                .await?
            {
                ShellReply::Receipted(kept) => kept,
                other => Err(unexpected(&other)),
            }
        })
    }
}

/// Followers of the device's writes, told each document written as the daemon tells it.
#[derive(Clone)]
pub struct DaemonWritesFeed {
    told: tokio::sync::broadcast::Sender<DocumentWritten>,
}

/// How many writes a follower may fall behind by before it reads everything again.
const FOLLOWED_WRITES: usize = 256;

/// How long the feed waits before it asks a daemon that did not answer again.
const ASK_AGAIN: std::time::Duration = std::time::Duration::from_secs(1);

impl DaemonWritesFeed {
    /// Tell every follower of `shell`'s writes, for as long as the feed is followed.
    #[must_use]
    pub fn start(shell: &DaemonShell) -> Self {
        let (told, _) = tokio::sync::broadcast::channel(FOLLOWED_WRITES);
        let feed = Self { told: told.clone() };
        // A desktop with no link hears of no write: there is no daemon to tell it one.
        if shell.inner.link.is_err() {
            return feed;
        }
        let shell = shell.clone();
        let told = told.downgrade();
        shell.inner.runtime.clone().spawn(async move {
            loop {
                let answer = shell.next_writes().await;
                let Some(told) = told.upgrade() else {
                    return;
                };
                match answer {
                    Ok(Some(writes)) => {
                        for write in writes {
                            let _ = told.send(write);
                        }
                    }
                    Ok(None) => behind(&told),
                    // Whatever the daemon took while it did not answer is missed here too.
                    Err(error) => {
                        tracing::warn!(%error, "the device's writes are not followed now");
                        tokio::time::sleep(ASK_AGAIN).await;
                        behind(&told);
                    }
                }
            }
        });
        feed
    }

    /// Each document written from now on.
    #[must_use]
    pub fn follow(&self) -> tokio::sync::broadcast::Receiver<DocumentWritten> {
        self.told.subscribe()
    }
}

/// Every follower of `told` falls behind, and reads everything it follows again, as it does when
/// it missed writes on its own: the daemon took writes it did not tell.
fn behind(told: &tokio::sync::broadcast::Sender<DocumentWritten>) {
    for _ in 0..=FOLLOWED_WRITES {
        let _ = told.send(DocumentWritten {
            workspace: String::new(),
            app: String::new(),
            record: String::new(),
        });
    }
}

#[cfg(test)]
#[path = "daemon_documents_tests.rs"]
mod tests;
