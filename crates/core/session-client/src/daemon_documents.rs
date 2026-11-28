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
use runtime_collab::wire::shell::{DocumentWritten, ShellCall, ShellReply};
use tilo_actions::remote::{
    InvokeRequest, ReceiptStatus, RemoteActionFrame, RemoteActionMessage, ResourceReadRequest,
};
use tilo_peer_state::owner::WriteRefusal;
use tilo_peer_state::workspace_documents::{
    DeviceDocuments, DeviceWrites, DocumentRead, DocumentServing, DocumentWrite, DocumentWriting,
};

/// What a desktop the daemon did not start is told of each ask of its workspaces.
pub const NOT_STARTED_BY_THE_DAEMON: &str =
    "this desktop was not started by the device's daemon, which keeps the person's workspaces";

/// The shell link the daemon gave this process when it started it, or `handed`, the link it made
/// for a session a test runs in its own process, opened on `runtime`, and what resolves when that
/// link ends; or why there is none.
///
/// # Errors
/// The daemon did not start this process, or the descriptor it handed is no socket.
pub fn inherited_link(
    handed: Option<std::os::fd::OwnedFd>,
    runtime: &tokio::runtime::Handle,
) -> Result<(ShellClient, runtime_collab::ConnectionEnd), String> {
    let link = match handed {
        Some(link) => {
            let link = std::os::unix::net::UnixStream::from(link);
            link.set_nonblocking(true).map(|()| link)
        }
        None => return Err(NOT_STARTED_BY_THE_DAEMON.to_owned()),
    }
    .map_err(|error| format!("the link the device's daemon handed this desktop: {error}"))?;
    let _entered = runtime.enter();
    let link = tokio::net::UnixStream::from_std(link)
        .map_err(|error| format!("the link the device's daemon handed this desktop: {error}"))?;
    Ok(ShellClient::open(link))
}

/// What asking the daemon for a link with a one-use secret answered: the shell's link, or that
/// the secret admits a founding link, which is no shell's.
pub enum ShellLinkAnswer {
    /// The descriptor of the link the secret admits.
    Shell(std::os::fd::OwnedFd),
    /// The secret is the sign-in's founding one: this process may make or join this computer's
    /// person, and asks again for a person's link once it has.
    Founding,
}

/// Ask the daemon at `socket` for a link, handing `token` when the daemon started this session
/// with one: the same typed request a desktop makes when its link ended, made once to get the
/// first.
///
/// # Errors
/// The daemon did not answer, refused the token, or answered no link.
pub fn link_with(socket: &std::path::Path, token: Option<&str>) -> Result<ShellLinkAnswer, String> {
    use std::io::Write as _;
    let mut stream = tilo_socket::connect(socket)
        .map_err(|error| format!("this device's daemon at {}: {error}", socket.display()))?;
    // No read bound: the daemon answers a link once the person's services run, which a start
    // may take a while to reach, and the wait is the daemon's to give.
    let request = match token {
        Some(token) => serde_json::json!({"native": "shells#link", "token": token}),
        None => serde_json::json!({"native": "shells#link"}),
    }
    .to_string()
        + "\n";
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("this device's daemon at {}: {error}", socket.display()))?;
    let (bytes, descriptors) = runtime_portal::fd_transport::recv_with_fds(&stream)
        .map_err(|error| format!("this device's daemon at {}: {error}", socket.display()))?;
    let answer: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("this device's daemon answered unreadably: {error}"))?;
    if let Some(why) = answer.get("error").and_then(serde_json::Value::as_str) {
        return Err(format!(
            "this device's daemon refused the shell's link: {why}"
        ));
    }
    match descriptors.into_iter().next() {
        Some(link) => Ok(ShellLinkAnswer::Shell(link)),
        None if answer.get("founding").and_then(serde_json::Value::as_bool) == Some(true) => {
            Ok(ShellLinkAnswer::Founding)
        }
        None => Err(format!("this device's daemon answered no link: {answer}")),
    }
}

/// [`link_with`] with the one-use secret the daemon gave this session, answering the descriptor
/// of the shell's link.
///
/// # Errors
/// The daemon did not answer, refused the token, or answered no link — a founding link, which
/// is no shell's, among them.
pub fn link_from_token(
    socket: &std::path::Path,
    token: &str,
) -> Result<std::os::fd::OwnedFd, String> {
    match link_with(socket, Some(token))? {
        ShellLinkAnswer::Shell(link) => Ok(link),
        ShellLinkAnswer::Founding => Err(
            "this device's daemon answered the sign-in's founding link, not a shell's".to_owned(),
        ),
    }
}

/// How long a blocking ask may wait before it says where it waits from, and again each time it
/// waited as long once more.
const SLOW_ASK: std::time::Duration = std::time::Duration::from_secs(2);
/// How long a blocking ask waits for the daemon's answer before it fails, saying what it asked,
/// how long it waited, and whether the runtime that carries the link ever ran it: a daemon
/// answers a desktop's ask in milliseconds, so one that waits this long waits on something held.
pub(crate) const ASK_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);
/// Why a blocking ask of `asking` failed after waiting `waited`: the daemon never answered it,
/// or, where the runtime that carries the link never `ran` it, that runtime's thread is held and
/// the daemon was never asked.
fn unanswered(asking: &str, waited: std::time::Duration, ran: bool) -> String {
    let seconds = waited.as_secs();
    let why = if ran {
        format!("the device's daemon did not answer it in {seconds} s")
    } else {
        format!(
            "the thread that carries the device daemon's link ran nothing for {seconds} s, so the \
             daemon was never asked"
        )
    };
    tracing::error!(asking = %asking, waited_s = seconds, ran, "{why}");
    format!("{why}: {asking}")
}

/// What a blocking ask made on the thread that carries the daemon's link is told: it could
/// never be answered.
pub const ASKED_ON_THE_LINK: &str =
    "a blocking ask of the device's daemon was made on the thread that carries its link";

/// What a blocking read of the device's daemon made on the desktop session's loop is told: the
/// loop follows what it reads and takes the latest answer, and never waits for one.
pub const READ_ON_THE_LOOP: &str =
    "a blocking read of the device's daemon was made on the desktop session's loop";

std::thread_local! {
    /// Whether this thread is running a pass of the desktop session's loop.
    static ON_THE_LOOP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// This thread runs a pass of the desktop session's loop while it is held: a blocking read of the
/// daemon made meanwhile is refused ([`READ_ON_THE_LOOP`]). Every frame, every answer on the
/// session's control socket and every app's request waits for the pass, so a pass that waited on
/// a daemon that answers slowly would stop them all.
#[must_use = "the loop is marked only while the mark is held"]
pub struct OnTheLoop {
    was: bool,
    /// The mark is the thread's: it is never sent to another.
    _thread: std::marker::PhantomData<*const ()>,
}

impl OnTheLoop {
    /// Mark this thread as running a pass of the session's loop, until the mark is dropped.
    pub fn mark() -> Self {
        Self {
            was: ON_THE_LOOP.with(|on| on.replace(true)),
            _thread: std::marker::PhantomData,
        }
    }
}

impl Drop for OnTheLoop {
    fn drop(&mut self) {
        ON_THE_LOOP.with(|on| on.set(self.was));
    }
}

/// Whether this thread is running a pass of the desktop session's loop ([`OnTheLoop::mark`]): what
/// a caller asks before reading the daemon, so the loop follows a read instead of waiting for it.
#[must_use]
pub fn on_the_loop() -> bool {
    ON_THE_LOOP.with(std::cell::Cell::get)
}

/// Refuse a blocking read asked on the session's loop, saying what was asked and from where. A
/// build with debug assertions, every test's, stops there: such a read is a bug to move off the
/// loop, never a wait to take.
fn refused_on_the_loop(asking: &dyn Fn() -> String) -> Result<(), String> {
    if !ON_THE_LOOP.with(std::cell::Cell::get) {
        return Ok(());
    }
    let asking = asking();
    let from = std::backtrace::Backtrace::force_capture();
    tracing::error!(asking = %asking, from = %from, "{READ_ON_THE_LOOP}");
    debug_assert!(false, "{READ_ON_THE_LOOP}: {asking}\n{from}");
    Err(READ_ON_THE_LOOP.to_owned())
}

/// How much of what a waiting ask asked it says.
const ASKING_SHOWN: usize = 200;

/// The start of `call`, as a wait says what it asked: written no further than it is shown, so a
/// call that carries a document costs no more than one that does not.
fn shown(call: &ShellCall) -> String {
    use std::fmt::Write as _;
    /// Takes what is written up to [`ASKING_SHOWN`] bytes, and refuses the rest.
    struct Start(String);
    impl std::fmt::Write for Start {
        fn write_str(&mut self, written: &str) -> std::fmt::Result {
            let room = ASKING_SHOWN.saturating_sub(self.0.len());
            let mut end = written.len().min(room);
            while !written.is_char_boundary(end) {
                end = end.saturating_sub(1);
            }
            self.0.push_str(written.get(..end).unwrap_or_default());
            if end < written.len() {
                return Err(std::fmt::Error);
            }
            Ok(())
        }
    }
    let mut start = Start(String::new());
    // A call longer than what is shown stops the writing there, and says it goes on.
    if write!(start, "{call:?}").is_err() {
        start.0.push('…');
    }
    start.0
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
    /// The one thread that drives [`Self::runtime`] when it has only one: a blocking ask made on
    /// it waits for frames only it could carry.
    driver: Arc<std::sync::OnceLock<std::thread::ThreadId>>,
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
        let driver = Arc::new(std::sync::OnceLock::new());
        if runtime.runtime_flavor() == tokio::runtime::RuntimeFlavor::CurrentThread {
            let noted = Arc::clone(&driver);
            runtime.spawn(async move {
                let _ = noted.set(std::thread::current().id());
            });
        }
        Self {
            inner: Arc::new(Inner {
                link,
                runtime,
                driver,
            }),
        }
    }

    /// The shell link, or why this desktop has none: what its apps' connections are vouched for
    /// on. While the daemon is asked for a new link, this says so rather than waiting.
    pub fn link(&self) -> Result<ShellClient, String> {
        let link = self.inner.link.clone()?;
        match link.link_status() {
            runtime_collab::ShellLink::Linked => Ok(link),
            status => Err(reconnecting(&status)),
        }
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
    ///
    /// # Errors
    /// As [`Self::ask`].
    pub fn ask_blocking(&self, call: ShellCall) -> Result<ShellReply, String> {
        // What the call asks, for a wait that has to say so: its kind and the start of its fields.
        let asking = shown(&call);
        // The thread that carries the link's frames would wait for itself, and every other ask of
        // the desktop's after it: it is refused at once, saying where it was asked from.
        if self.inner.driver.get() == Some(&std::thread::current().id()) {
            tracing::error!(
                asking = %asking,
                from = %std::backtrace::Backtrace::force_capture(),
                "a blocking ask of the device's daemon was made on the thread that carries its link"
            );
            return Err(ASKED_ON_THE_LINK.to_owned());
        }
        let (answered, answer) = std::sync::mpsc::sync_channel(1);
        let inner = Arc::clone(&self.inner);
        // Whether the runtime that carries the link ran the ask at all: one that never did is
        // held by something else, and the daemon was never asked.
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let running = Arc::clone(&ran);
        self.inner.runtime.spawn(async move {
            running.store(true, std::sync::atomic::Ordering::Release);
            let _ = answered.send(inner.ask(call).await);
        });
        let asked = tilo_clock::now();
        // A thread that waits here waits on the daemon: one that waits long says what it asked
        // and where it waits from while it still waits, so a wait that never ends is found too,
        // and a wait on the desktop's own loop is moved off it.
        let mut said = false;
        let answered = loop {
            match answer.recv_timeout(SLOW_ASK) {
                Ok(answered) => break answered,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    let waited = tilo_clock::now().saturating_duration_since(asked);
                    if waited >= ASK_DEADLINE {
                        return Err(unanswered(
                            &asking,
                            waited,
                            ran.load(std::sync::atomic::Ordering::Acquire),
                        ));
                    }
                    tracing::warn!(
                        waited_ms = u64::try_from(waited.as_millis()).unwrap_or(u64::MAX),
                        thread = std::thread::current().name().unwrap_or("unnamed"),
                        asking = %asking,
                        from = %std::backtrace::Backtrace::force_capture(),
                        "a blocking ask of the device's daemon is still waiting"
                    );
                    said = true;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("the daemon's connection stopped before it answered".to_owned());
                }
            }
        };
        if said {
            let waited = tilo_clock::now().saturating_duration_since(asked);
            tracing::warn!(
                waited_ms = u64::try_from(waited.as_millis()).unwrap_or(u64::MAX),
                asking = %asking,
                "a blocking ask of the device's daemon was answered after a long wait"
            );
        }
        answered
    }

    /// What carries the connection's frames: where a follower of a read waits.
    #[must_use]
    pub fn runtime(&self) -> &tokio::runtime::Handle {
        &self.inner.runtime
    }

    /// Tell the daemon the person pressed in `window` (its tab id, as `device.Window.key` names
    /// it), at `at` in it where a pointer or a finger pressed, delivered with the input serial
    /// `serial` the compositor issued: a call an app's window makes that names it runs as the
    /// person, once, and a question it raises stands at `at`. Sent without waiting: a press the
    /// daemon hears late proves nothing, and the call it makes possible waits for it.
    pub fn press(
        &self,
        window: u64,
        serial: u32,
        at: Option<tilo_actions::remote::asked::PressPoint>,
    ) {
        let inner = Arc::clone(&self.inner);
        self.inner.runtime.spawn(async move {
            if let Err(error) = inner.ask(ShellCall::Press { window, serial, at }).await {
                tracing::debug!(%error, "the device's daemon was not told of the person's press");
            }
        });
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

    /// Run `request`, a typed action of a resource of `workspace`, as the person, saying where
    /// on this shell's screen they pressed it: what the daemon keeps as the question's
    /// `pressed` when the call waits for their yes.
    ///
    /// # Errors
    /// As [`Self::dispatch`]; a refusal says why.
    pub async fn invoke_pressed(
        &self,
        workspace: Option<&str>,
        request: InvokeRequest,
        pressed: runtime_collab::wire::shell::ShellPress,
    ) -> Result<serde_json::Value, String> {
        let call = dispatched_pressed(
            workspace,
            &RemoteActionMessage::Invoke(request),
            Some(pressed),
        )?;
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

    /// Run `request`, a typed action of a resource of `workspace`, as the person, and answer its
    /// receipt as it stands: completed, accepted to wait for their yes, or refused. What a call
    /// the dispatch keeps as an asked record answers with, where [`Self::dispatch`] waits for it
    /// to settle.
    ///
    /// # Errors
    /// As [`Self::ask`], or the dispatch answered no receipt.
    pub async fn invoke_receipt(
        &self,
        workspace: Option<&str>,
        request: InvokeRequest,
    ) -> Result<tilo_actions::remote::InvocationReceipt, String> {
        let call = dispatched(workspace, &RemoteActionMessage::Invoke(request))?;
        let ShellReply::Action(frame) = self.ask(call).await? else {
            return Err("the daemon answered something else".to_owned());
        };
        let message = RemoteActionFrame::decode(&frame)
            .map_err(|error| error.to_string())?
            .message;
        match message {
            RemoteActionMessage::Receipt(receipt) => Ok(receipt),
            other => Err(format!("the dispatch answered something else: {other:?}")),
        }
    }

    /// [`Self::invoke_receipt`] from a thread that is not the runtime's.
    ///
    /// # Errors
    /// As [`Self::invoke_receipt`].
    pub fn invoke_receipt_blocking(
        &self,
        workspace: Option<&str>,
        request: InvokeRequest,
    ) -> Result<tilo_actions::remote::InvocationReceipt, String> {
        let call = dispatched(workspace, &RemoteActionMessage::Invoke(request))?;
        let ShellReply::Action(frame) = self.ask_blocking(call)? else {
            return Err("the daemon answered something else".to_owned());
        };
        let message = RemoteActionFrame::decode(&frame)
            .map_err(|error| error.to_string())?
            .message;
        match message {
            RemoteActionMessage::Receipt(receipt) => Ok(receipt),
            other => Err(format!("the dispatch answered something else: {other:?}")),
        }
    }

    /// How the invocation `invocation` stands, as the daemon's dispatch keeps its record of it:
    /// the record asks the provider the call went to, so an action answered before it ran (the
    /// person's write to their own workspace before this device holds it) says here once it was
    /// made, or why it could not be. The answer waits up to `wait_ms` for the invocation to
    /// settle, and holds no thread meanwhile. `None` for an invocation the daemon keeps none of,
    /// or one whose caller this connection is not.
    ///
    /// # Errors
    /// As [`Self::ask`], or the dispatch answered something else.
    pub async fn observe(
        &self,
        workspace: Option<&str>,
        invocation: tilo_actions::InvocationId,
        wait_ms: Option<u32>,
    ) -> Result<Option<tilo_actions::remote::InvocationState>, String> {
        let message = RemoteActionMessage::Observe {
            correlation: 0,
            invocation,
            wait_ms,
            seen: None,
        };
        let call = dispatched(workspace, &message)?;
        let ShellReply::Action(frame) = self.ask(call).await? else {
            return Err("the daemon answered something else".to_owned());
        };
        let message = RemoteActionFrame::decode(&frame)
            .map_err(|error| error.to_string())?
            .message;
        match message {
            RemoteActionMessage::Update { state, .. } => Ok(state),
            other => Err(format!("the dispatch answered something else: {other:?}")),
        }
    }

    /// The readings of `read`, as the device reads them for the person.
    ///
    /// # Errors
    /// As [`Self::dispatch`].
    pub async fn read(&self, read: ResourceReadRequest) -> Result<serde_json::Value, String> {
        self.dispatch(None, &read_message(read)).await
    }

    /// What the device says the person may do of `ask`'s actions, in `workspace`, which keeps
    /// them: now, or, once `ask` names the verdicts shown and a wait, when the grants change or
    /// the wait passes.
    ///
    /// # Errors
    /// As [`Self::dispatch`].
    pub async fn shown(
        &self,
        workspace: &str,
        ask: tilo_actions::shown::ShownAsk,
    ) -> Result<Vec<tilo_actions::shown::ActionDecision>, String> {
        let message = RemoteActionMessage::ShownActionsRequest {
            correlation: 0,
            actions: ask.actions,
            after: ask.after,
            wait_ms: ask.wait_ms,
        };
        serde_json::from_value(self.dispatch(Some(workspace), &message).await?)
            .map_err(|error| format!("the device's verdicts do not read: {error}"))
    }

    /// [`Self::read`] from a thread that is not the runtime's.
    ///
    /// # Errors
    /// As [`Self::dispatch`]; one asked on the session's loop is refused ([`OnTheLoop`]).
    pub fn read_blocking(&self, read: ResourceReadRequest) -> Result<serde_json::Value, String> {
        refused_on_the_loop(&|| format!("{read:?}"))?;
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
        match link.call(call).await {
            Ok(reply) => Ok(reply),
            Err(runtime_portal::NotAvailable::Refused(reason)) => Err(reason),
            // The link ended under the ask: the session asks its daemon for another, and the ask
            // fails now saying so rather than waiting on a link that is gone.
            Err(runtime_portal::NotAvailable::ConnectionFailed(_)) => {
                Err(reconnecting(&link.link_status()))
            }
            Err(error) => Err(format!("the device's daemon: {error}")),
        }
    }
}

/// What an ask made while the daemon is asked for a new link is told: legible, and saying the
/// desktop reaches its daemon again by itself.
fn reconnecting(status: &runtime_collab::ShellLink) -> String {
    match status {
        runtime_collab::ShellLink::Linked => RECONNECTING.to_owned(),
        runtime_collab::ShellLink::Reconnecting => {
            "the device's daemon is reconnecting: this desktop reaches it again by itself"
                .to_owned()
        }
        runtime_collab::ShellLink::Unreachable { why } => {
            format!("the device's daemon is not answering: {why}; this desktop keeps asking")
        }
    }
}

/// What the shell says while it asks its daemon for a new link.
pub const RECONNECTING: &str = "the device's daemon is reconnecting";

/// `message` as the shell link carries it.
fn dispatched(workspace: Option<&str>, message: &RemoteActionMessage) -> Result<ShellCall, String> {
    dispatched_pressed(workspace, message, None)
}

/// [`dispatched`] with the window the person pressed the call in, where they did.
fn dispatched_pressed(
    workspace: Option<&str>,
    message: &RemoteActionMessage,
    pressed: Option<runtime_collab::wire::shell::ShellPress>,
) -> Result<ShellCall, String> {
    Ok(ShellCall::Action {
        workspace: workspace.map(str::to_owned),
        frame: RemoteActionFrame::new(message.clone())
            .encode()
            .map_err(|error| format!("a call does not encode: {error}"))?,
        caller: None,
        pressed,
    })
}

/// The JSON the dispatch answered with: the readings of a read, or the value an action that
/// settled answered; or why it refused. A receipt keeps an action's value inside its output,
/// and what [`DaemonShell::dispatch`] and the calls on it answer is the value itself.
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
        RemoteActionMessage::ShownActionsResponse { result, .. } => {
            serde_json::to_value(result.map_err(|error| error.to_string())?)
                .map_err(|error| format!("the dispatch's answer does not read: {error}"))
        }
        RemoteActionMessage::Receipt(receipt) => match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
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
#[must_use]
pub fn whole(workspace: &str, app: &str, resource: &str) -> ResourceReadRequest {
    ResourceReadRequest {
        workspace: workspace.to_owned(),
        app: app.to_owned(),
        record: resource.to_owned(),
        joined: false,
        after: None,
        wait_ms: None,
        answer: tilo_actions::remote::ReadAnswer::Document,
    }
}

/// What one resource of `readings` is: its document whole, `Absent`, or `Pending` while it is on
/// its way. A reading a provider publishes at the route (`{"value"}`) is no document: a reader of
/// documents holds nothing of it, and it reads as `Absent` until a document is written there.
///
/// # Errors
/// The readings name no resource, or its document does not read.
pub fn document_read(readings: &serde_json::Value) -> Result<DocumentRead, String> {
    let resource = readings["records"]
        .get(0)
        .ok_or("the read answered no resource")?;
    if resource["absent"].as_bool() == Some(true) || resource.get("value").is_some() {
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
        hint: None,
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

    fn listing(
        &self,
        workspace: &str,
    ) -> Result<Vec<tilo_actions::remote::ListedResource>, String> {
        // The daemon lists every resource the workspace publishes, and who is in it and the keys
        // it admitted are read from what it is, never stored: no document holds them, and a
        // reader that follows the documents would ask for one that is not there.
        let mut listed = tilo_actions::remote::ListedResource::read_listing(
            workspace,
            "",
            &self.0.read_blocking(listing(workspace))?,
        );
        listed.retain(|listed| {
            ![
                tilo_actions::remote::PRESENCE,
                tilo_actions::remote::KEY_GRANTS,
            ]
            .contains(&listed.address.app.as_str())
        });
        Ok(listed)
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

    fn member_changes(
        &self,
        workspace: &str,
    ) -> Result<Vec<tilo_peer_state::workspace_documents::MemberChange>, String> {
        match self.0.ask_blocking(ShellCall::MemberChanges {
            workspace: workspace.to_owned(),
        })? {
            ShellReply::MemberChanges(changes) => Ok(changes?
                .into_iter()
                .map(
                    |change| tilo_peer_state::workspace_documents::MemberChange {
                        change: change.change,
                        message: change.message,
                        time: change.time,
                    },
                )
                .collect()),
            other => Err(unexpected(&other)),
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

/// A read of every resource `workspace` holds, by its app and route.
#[must_use]
pub fn listing(workspace: &str) -> ResourceReadRequest {
    let mut listing = whole(workspace, "", "");
    listing.answer = tilo_actions::remote::ReadAnswer::Listing;
    listing
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
    /// Ask the daemon's search `request`, and read what it answered.
    fn asked(
        &self,
        request: InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<serde_json::Value, tilo_actions::remote::ActionCallError>,
    > {
        let (answer, pending) = tilo_actions::remote::pending();
        let shell = self.0.clone();
        self.0.runtime().spawn(async move {
            let answered = shell
                .dispatch(None, &RemoteActionMessage::Invoke(request))
                .await
                .map_err(|reason| tilo_actions::remote::ActionCallError::Unavailable { reason });
            answer.complete(answered);
        });
        pending
    }
}

impl tilo_workspace_records::search::Answers for DaemonSearch {
    fn find(
        &self,
        caller: &tilo_actions::remote::Caller,
        needle: &str,
    ) -> tilo_actions::remote::Pending<
        Result<Vec<tilo_workspace_records::search::Found>, tilo_actions::remote::ActionCallError>,
    > {
        self.find_within(
            caller,
            needle,
            &tilo_workspace_records::search::Within::default(),
        )
    }

    fn find_within(
        &self,
        _caller: &tilo_actions::remote::Caller,
        needle: &str,
        within: &tilo_workspace_records::search::Within,
    ) -> tilo_actions::remote::Pending<
        Result<Vec<tilo_workspace_records::search::Found>, tilo_actions::remote::ActionCallError>,
    > {
        use tilo_workspace_records::search::{Found, SearchAction, request};
        let within = (!within.is_everything()).then(|| within.clone());
        let request = request(&SearchAction::Find {
            needle: needle.to_owned(),
            within,
        });
        self.asked(request)
            .map(|answer| answer.and_then(read::<Vec<Found>>))
    }

    fn pictures(
        &self,
        _caller: &tilo_actions::remote::Caller,
    ) -> tilo_actions::remote::Pending<
        Result<Vec<tilo_workspace_records::search::Picture>, tilo_actions::remote::ActionCallError>,
    > {
        use tilo_workspace_records::search::{Picture, SearchAction, request};
        self.asked(request(&SearchAction::Pictures))
            .map(|answer| answer.and_then(read::<Vec<Picture>>))
    }

    fn describe(
        &self,
        _caller: &tilo_actions::remote::Caller,
        description: tilo_workspace_records::search::Description,
    ) -> tilo_actions::remote::Pending<Result<(), tilo_actions::remote::ActionCallError>> {
        use tilo_workspace_records::search::{SearchAction, request};
        self.asked(request(&SearchAction::Describe { description }))
            .map(|answer| answer.map(|_| ()))
    }
}

/// What the search answered `value` as `T`.
fn read<T: serde::de::DeserializeOwned>(
    value: serde_json::Value,
) -> Result<T, tilo_actions::remote::ActionCallError> {
    serde_json::from_value(value).map_err(|error| {
        tilo_actions::remote::ActionCallError::Unavailable {
            reason: format!("the search's answer does not read: {error}"),
        }
    })
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
        let call = ShellCall::Write {
            workspace: workspace.to_owned(),
            app: writing.app.to_owned(),
            kind: writing.kind.map(str::to_owned),
            record: writing.record.to_owned(),
            changes: changes.to_vec(),
        };
        let shell = self.0.clone();
        Box::pin(async move {
            match shell.ask(call).await.map_err(WriteRefusal::Refused)? {
                ShellReply::Written(Ok(())) => Ok(()),
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
    /// Held by every copy of the feed: while one is, a follower may still subscribe.
    kept: std::sync::Arc<()>,
}

/// How many writes a follower may fall behind by before it reads everything again.
const FOLLOWED_WRITES: usize = 256;

/// How long the feed waits before it asks a daemon that did not answer again.
const ASK_AGAIN: std::time::Duration = std::time::Duration::from_secs(1);

impl DaemonWritesFeed {
    /// Tell every follower of `shell`'s writes, for as long as the feed is followed: a follower
    /// is told each write whether or not anything still holds the feed it subscribed from.
    #[must_use]
    pub fn start(shell: &DaemonShell) -> Self {
        let (told, _) = tokio::sync::broadcast::channel(FOLLOWED_WRITES);
        let feed = Self {
            told: told.clone(),
            kept: std::sync::Arc::default(),
        };
        // A desktop with no link hears of no write: there is no daemon to tell it one.
        if shell.inner.link.is_err() {
            return feed;
        }
        let shell = shell.clone();
        let kept = std::sync::Arc::downgrade(&feed.kept);
        shell.inner.runtime.clone().spawn(async move {
            loop {
                let answer = shell.next_writes().await;
                // Nobody follows the writes any more, and nobody can start to.
                if told.receiver_count() == 0 && kept.strong_count() == 0 {
                    return;
                }
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
