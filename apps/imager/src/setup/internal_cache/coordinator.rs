//! Private retained download worker. No staging, UI, or support route is enabled.
//! Selection authority and all leased handles stay on this worker. Only bounded,
//! owned descriptions cross the control boundary; Ready is deliberately private.
use super::HeldReadSeek;
#[cfg(any(target_os = "linux", windows, all(test, unix)))]
use super::Session;
#[cfg(any(target_os = "linux", windows, all(test, unix)))]
use crate::setup::internal_download::PersistentOutcome;
use crate::{
    release::InternalRelease,
    setup::internal_download::{InternalTransport, Progress, SelectedManifest},
};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::thread::{self, JoinHandle};

#[cfg(all(target_os = "linux", not(test)))]
use super::linux as platform;
#[cfg(windows)]
use super::windows as platform;
#[cfg(test)]
#[cfg(unix)]
use tests::fixture as platform;

static NEXT_OPERATION: AtomicU64 = AtomicU64::new(1);

fn next_operation(counter: &AtomicU64) -> Result<u64, String> {
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            if n == 0 { None } else { n.checked_add(1) }
        })
        .map_err(|_| "Operation identifier exhausted".into())
}

/// Read-only cooperative stop view for a scoped consumer. The transfer latch
/// cannot be reset or retained beyond the borrowed consumption scope.
#[allow(
    dead_code,
    reason = "the live download flow does not consume verified cache entries yet"
)]
pub(in crate::setup) struct Cancellation<'a>(&'a AtomicBool);
impl Cancellation<'_> {
    /// Borrow the same owned worker cancellation flag for blocking native I/O.
    /// The flag remains read-only and cannot outlive this consumption scope.
    pub(in crate::setup) fn signal(&self) -> &AtomicBool {
        self.0
    }

    #[allow(
        dead_code,
        reason = "the live download flow does not consume verified cache entries yet"
    )]
    pub(in crate::setup) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::setup) struct Token {
    operation: u64,
    selection: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::setup) enum Phase {
    Authenticating,
    Acquiring,
    Preparing,
    Downloading,
    Ready,
    Consuming,
    Consumed,
    Paused,
    Cancelled,
    Failed,
    Released,
    Discarded,
}
#[derive(Clone, Debug)]
pub(in crate::setup) struct Status {
    pub token: Token,
    pub phase: Phase,
    /// Last positively acknowledged prefix per role (not bytes merely received).
    pub committed: [u64; 2],
    pub progress: Option<Progress>,
    pub finals: [bool; 2],
    /// Includes unknown residue; false is not a claim that no other cache exists.
    pub residue: bool,
    pub uncertain: bool,
    pub error: Option<String>,
}
impl Status {
    fn new(token: Token) -> Self {
        Self {
            token,
            phase: Phase::Authenticating,
            committed: [0; 2],
            progress: None,
            finals: [false; 2],
            residue: false,
            uncertain: false,
            error: None,
        }
    }
}
#[derive(Default)]
struct Flags {
    cancel: AtomicBool,
    pause: AtomicBool,
}
struct Request {
    token: Token,
    release: InternalRelease,
    flags: Arc<Flags>,
    discard: bool,
}
#[derive(Clone, Copy)]
enum Action {
    #[allow(
        dead_code,
        reason = "the live download flow releases rather than consumes verified cache entries"
    )]
    Consume,
    Release,
}
#[derive(Default)]
struct Mailbox {
    pending: Option<Request>,
    active: Option<(Token, Arc<Flags>)>,
    action: Option<Action>,
    status: Option<Status>,
    shutdown: bool,
}
#[derive(Default)]
struct Shared {
    mailbox: Mutex<Mailbox>,
    wake: Condvar,
}

/// One latest request, one Ready action and one coalesced status. Replacing a
/// request cancels the old operation out of band, even during blocked transport.
/// Dropping the only controller requests cancellation and joins the worker.
/// Network operations are bounded by transport timeouts; filesystem operations
/// and the scoped consumer remain cooperative and must return. Consumers receive
/// the cancellation latch and must check it during their own bounded work. The
/// callback grants read access only; it is not permission to stage or install.
pub(in crate::setup) struct Worker {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}
impl Worker {
    pub(in crate::setup) fn spawn(
        consumer: impl FnMut(
            &SelectedManifest,
            &Cancellation<'_>,
            &mut dyn HeldReadSeek,
            &mut dyn HeldReadSeek,
        ) -> Result<(), String>
        + Send
        + 'static,
    ) -> Result<Self, String> {
        imager_backend::cache::ensure_supported()?;
        Self::spawn_with(InternalTransport::new()?, consumer)
    }
    fn spawn_with(
        transport: InternalTransport,
        consumer: impl FnMut(
            &SelectedManifest,
            &Cancellation<'_>,
            &mut dyn HeldReadSeek,
            &mut dyn HeldReadSeek,
        ) -> Result<(), String>
        + Send
        + 'static,
    ) -> Result<Self, String> {
        Self::spawn_runner(move |shared| worker_loop(shared, transport, consumer))
    }
    fn spawn_runner(run: impl FnOnce(Arc<Shared>) + Send + 'static) -> Result<Self, String> {
        let shared = Arc::new(Shared::default());
        let worker = shared.clone();
        let thread = thread::Builder::new()
            .name("internal-cache".into())
            .spawn(move || run(worker))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }
    pub(in crate::setup) fn start(&self, release: InternalRelease) -> Result<Token, String> {
        self.submit(release, None)
    }
    /// A new authenticated cleanup operation and a NEW cancellation flag. The
    /// transfer's cancellation latch is never cleared. Verified finals remain.
    pub(in crate::setup) fn discard(
        &self,
        target: &Token,
        release: InternalRelease,
    ) -> Result<Token, String> {
        self.submit(release, Some(target))
    }
    fn submit(&self, release: InternalRelease, target: Option<&Token>) -> Result<Token, String> {
        let mut m = self.shared.mailbox.lock().unwrap();
        if m.shutdown {
            return Err("Worker is shut down".into());
        }
        if let Some(target) = target {
            if m.active.as_ref().is_none_or(|(t, _)| t != target)
                || target.selection != release.manifest_sha256()
            {
                return Err("Stale cleanup selection/operation".into());
            }
        }
        let operation = next_operation(&NEXT_OPERATION)?;
        let token = Token {
            operation,
            selection: release.manifest_sha256().into(),
        };
        if let Some((_, flags)) = &m.active {
            flags.cancel.store(true, Ordering::Release);
        }
        let flags = Arc::new(Flags::default());
        m.active = Some((token.clone(), flags.clone()));
        m.pending = Some(Request {
            token: token.clone(),
            release,
            flags,
            discard: target.is_some(),
        });
        m.action = None;
        m.status = Some(Status::new(token.clone()));
        self.shared.wake.notify_all();
        Ok(token)
    }
    pub(in crate::setup) fn pause(&self, token: &Token) -> Result<(), String> {
        self.stop(token, false)
    }
    pub(in crate::setup) fn cancel(&self, token: &Token) -> Result<(), String> {
        self.stop(token, true)
    }
    fn stop(&self, token: &Token, cancel: bool) -> Result<(), String> {
        let m = self.shared.mailbox.lock().unwrap();
        if m.status.as_ref().is_none_or(|s| terminal(s.phase)) {
            return Err("Operation is already terminal".into());
        }
        let (_, flags) = m
            .active
            .as_ref()
            .filter(|(t, _)| t == token)
            .ok_or("Stale stop operation")?;
        if cancel {
            flags.cancel.store(true, Ordering::Release);
        } else {
            flags.pause.store(true, Ordering::Release);
        }
        self.shared.wake.notify_all();
        Ok(())
    }
    #[allow(
        dead_code,
        reason = "reserved for the verified cache-to-installer handoff"
    )]
    pub(in crate::setup) fn consume(&self, token: &Token) -> Result<(), String> {
        self.action(token, Action::Consume)
    }
    pub(in crate::setup) fn release(&self, token: &Token) -> Result<(), String> {
        self.action(token, Action::Release)
    }
    fn action(&self, token: &Token, action: Action) -> Result<(), String> {
        let mut m = self.shared.mailbox.lock().unwrap();
        if m.shutdown
            || m.active.as_ref().is_none_or(|(t, f)| {
                t != token || f.cancel.load(Ordering::Acquire) || f.pause.load(Ordering::Acquire)
            })
            || m.status
                .as_ref()
                .is_none_or(|s| s.token != *token || s.phase != Phase::Ready)
            || m.action.is_some()
        {
            return Err("Stale or unavailable held Ready".into());
        }
        m.action = Some(action);
        self.shared.wake.notify_all();
        Ok(())
    }
    pub(in crate::setup) fn status(&self) -> Option<Status> {
        self.shared.mailbox.lock().unwrap().status.clone()
    }
    pub(in crate::setup) fn shutdown(&mut self) {
        {
            let mut m = self.shared.mailbox.lock().unwrap();
            m.shutdown = true;
            if let Some((_, flags)) = &m.active {
                flags.cancel.store(true, Ordering::Release);
            }
            m.pending = None;
            self.shared.wake.notify_all();
        }
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                if let Some(status) = &mut self.shared.mailbox.lock().unwrap().status {
                    status.phase = Phase::Failed;
                    status.error = Some("Download worker panicked; cache may be retained".into());
                    status.residue = true;
                    status.uncertain = true;
                }
            }
        }
        let mut m = self.shared.mailbox.lock().unwrap();
        if let Some(status) = &mut m.status {
            if !terminal(status.phase) {
                status.phase = Phase::Cancelled;
            }
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn terminal(phase: Phase) -> bool {
    matches!(
        phase,
        Phase::Consumed
            | Phase::Paused
            | Phase::Cancelled
            | Phase::Failed
            | Phase::Released
            | Phase::Discarded
    )
}
fn report(shared: &Shared, status: &Status) {
    let mut m = shared.mailbox.lock().unwrap();
    if m.active.as_ref().is_some_and(|(t, _)| *t == status.token) {
        m.status = Some(status.clone());
        shared.wake.notify_all();
    }
}
fn stop(flags: &Flags) -> Result<(), String> {
    if flags.cancel.load(Ordering::Acquire) {
        Err("Internal transfer cancelled".into())
    } else if flags.pause.load(Ordering::Acquire) {
        Err("Internal transfer paused".into())
    } else {
        Ok(())
    }
}
fn worker_loop(
    shared: Arc<Shared>,
    transport: InternalTransport,
    mut consumer: impl FnMut(
        &SelectedManifest,
        &Cancellation<'_>,
        &mut dyn HeldReadSeek,
        &mut dyn HeldReadSeek,
    ) -> Result<(), String>,
) {
    loop {
        let request = {
            let mut m = shared.mailbox.lock().unwrap();
            while m.pending.is_none() && !m.shutdown {
                m = shared.wake.wait(m).unwrap();
            }
            if m.shutdown {
                return;
            }
            m.pending.take().unwrap()
        };
        let mut status = Status::new(request.token.clone());
        // selected is external to every borrowed handle/Ready, on this stack.
        let result = (|| {
            stop(&request.flags)?;
            let selected = transport.fetch_manifest(&request.release, &request.flags.cancel)?;
            stop(&request.flags)?;
            run_selected(
                &selected,
                &request,
                &transport,
                &shared,
                &mut status,
                &mut consumer,
            )
        })();
        if request.flags.cancel.load(Ordering::Acquire) {
            status.phase = Phase::Cancelled;
        } else if result.is_err() {
            // Only an explicit stop produces Paused; a coincident I/O failure is
            // still failure, with its acknowledged prefix/uncertainty preserved.
            status.phase = if result
                .as_ref()
                .is_err_and(|e| e == "Internal transfer paused")
            {
                Phase::Paused
            } else {
                Phase::Failed
            };
        }
        if let Err(error) = result {
            status.error.get_or_insert(error);
        }
        // Scope has ended: no Session, component, Ready or root lease is live.
        let mut m = shared.mailbox.lock().unwrap();
        if let Some((token, flags)) = &m.active {
            if *token == status.token {
                // Same lock as stop admission: an accepted cancel wins even in
                // the last interval between scoped completion and publication.
                if flags.cancel.load(Ordering::Acquire) {
                    status.phase = Phase::Cancelled;
                }
                m.status = Some(status);
                shared.wake.notify_all();
            }
        }
    }
}

#[cfg(any(target_os = "linux", windows, all(test, unix)))]
struct Ready<'s> {
    selected: &'s SelectedManifest,
    finals: [platform::FinalHandle<'s>; 2],
}
#[cfg(any(target_os = "linux", windows, all(test, unix)))]
impl Ready<'_> {
    fn consume(
        &mut self,
        cancel: &AtomicBool,
        consumer: &mut impl FnMut(
            &SelectedManifest,
            &Cancellation<'_>,
            &mut dyn HeldReadSeek,
            &mut dyn HeldReadSeek,
        ) -> Result<(), String>,
    ) -> Result<(), String> {
        let [first, second] = &mut self.finals;
        first.with_verified_reader(cancel, |a| {
            second.with_verified_reader(cancel, |b| {
                consumer(self.selected, &Cancellation(cancel), a, b)
            })
        })
    }
}

#[cfg(not(any(target_os = "linux", windows, all(test, unix))))]
fn run_selected(
    _selected: &SelectedManifest,
    _request: &Request,
    _transport: &InternalTransport,
    _shared: &Shared,
    _status: &mut Status,
    _consumer: &mut impl FnMut(
        &SelectedManifest,
        &Cancellation<'_>,
        &mut dyn HeldReadSeek,
        &mut dyn HeldReadSeek,
    ) -> Result<(), String>,
) -> Result<(), String> {
    // Fail closed even if an internal caller bypasses spawn's preflight.
    imager_backend::cache::unsupported()
}

#[cfg(any(target_os = "linux", windows, all(test, unix)))]
fn run_selected<'s>(
    selected: &'s SelectedManifest,
    request: &Request,
    transport: &InternalTransport,
    shared: &Shared,
    status: &mut Status,
    consumer: &mut impl FnMut(
        &SelectedManifest,
        &Cancellation<'_>,
        &mut dyn HeldReadSeek,
        &mut dyn HeldReadSeek,
    ) -> Result<(), String>,
) -> Result<(), String> {
    let flags = &request.flags;
    status.phase = Phase::Acquiring;
    report(shared, status);
    stop(flags)?;
    // Bootstrap owns the one cooperative root lock and creation settlement.
    // An acquisition error can retain residue, even without a returned handle.
    status.residue = true;
    let components = platform::CacheRoot::acquire(selected)?
        .bootstrap(selected, &flags.cancel)
        .inspect_err(|_| status.uncertain = true)?;
    for (i, component) in components.iter().enumerate() {
        if let platform::Component::Final(final_handle) = component {
            status.finals[i] = true;
            status.residue |= final_handle.has_retained_residue();
        }
    }
    stop(flags)?;
    if request.discard {
        for component in components {
            stop(flags)?;
            if let platform::Component::Partial(partial) = component {
                partial
                    .discard(request.token.operation, &flags.cancel)
                    .inspect_err(|_| status.uncertain = true)?;
            }
        }
        stop(flags)?;
        status.phase = Phase::Discarded;
        status.residue = status.finals.iter().any(|b| *b);
        return Ok(());
    }
    let [first, second] = components;
    let (mut a, mut af) = split(first);
    let (mut b, mut bf) = split(second);
    status.phase = Phase::Preparing;
    report(shared, status);
    // Distinct locals keep both Sessions/prepared prefixes alive before either
    // transfer. Never carry a preparation receipt across pause/reacquisition.
    let (ap, ax) = a.as_mut().map(|p| (&mut p.provider, &mut p.file)).unzip();
    let (bp, bx) = b.as_mut().map(|p| (&mut p.provider, &mut p.file)).unzip();
    let mut sa = ap
        .map(|p| Session::new(selected, 0, request.token.operation, p))
        .transpose()?;
    let mut sb = bp
        .map(|p| Session::new(selected, 1, request.token.operation, p))
        .transpose()?;
    let pa = match (&mut sa, ax) {
        (Some(s), Some(file)) => Some(
            s.prepare(selected, file, &flags.cancel)
                // Preparation may normalize journals or trim/sync a tail before
                // failing. A string error cannot prove that nothing changed.
                .inspect_err(|_| status.uncertain = true)?,
        ),
        _ => None,
    };
    if let Some(p) = &pa {
        status.committed[0] = p.committed();
    }
    let pb = match (&mut sb, bx) {
        (Some(s), Some(file)) => Some(
            s.prepare(selected, file, &flags.cancel)
                // Preparation may normalize journals or trim/sync a tail before
                // failing. A string error cannot prove that nothing changed.
                .inspect_err(|_| status.uncertain = true)?,
        ),
        _ => None,
    };
    if let Some(p) = &pb {
        status.committed[1] = p.committed();
    }
    stop(flags)?;
    if let Some(prepared) = pa {
        transfer(transport, prepared, 0, flags, shared, status)?;
    }
    drop(sa);
    stop(flags)?;
    if let Some(partial) = a {
        af = Some(
            partial
                .publish(&flags.cancel)
                .inspect_err(|_| status.uncertain = true)?,
        );
        status.finals[0] = true;
    }
    stop(flags)?;
    if let Some(prepared) = pb {
        transfer(transport, prepared, 1, flags, shared, status)?;
    }
    drop(sb);
    stop(flags)?;
    if let Some(partial) = b {
        bf = Some(
            partial
                .publish(&flags.cancel)
                .inspect_err(|_| status.uncertain = true)?,
        );
        status.finals[1] = true;
    }
    stop(flags)?;
    let mut ready = Ready {
        selected,
        finals: [
            af.ok_or("Missing first held final")?,
            bf.ok_or("Missing second held final")?,
        ],
    };
    status.phase = Phase::Ready;
    status.residue = ready.finals.iter().any(|f| f.has_retained_residue());
    report(shared, status);
    let action = {
        let mut m = shared.mailbox.lock().unwrap();
        loop {
            stop(flags)?;
            if m.shutdown || m.active.as_ref().is_none_or(|(t, _)| *t != request.token) {
                return Err("Internal transfer cancelled".into());
            }
            if let Some(action) = m.action.take() {
                break action;
            }
            m = shared.wake.wait(m).unwrap();
        }
    };
    stop(flags)?;
    match action {
        Action::Release => status.phase = Phase::Released,
        Action::Consume => {
            status.phase = Phase::Consuming;
            report(shared, status);
            let consumed = ready.consume(&flags.cancel, &mut |selection, cancel, a, b| {
                let result = consumer(selection, cancel, a, b);
                if let Err(error) = &result {
                    status.error = Some(error.clone());
                }
                result
            });
            if let Err(error) = consumed {
                // Verification, cursor restoration and native guards can fail
                // alongside cancellation. Conservatively retain that uncertainty.
                status.uncertain = true;
                if let Some(callback_error) = &mut status.error {
                    if *callback_error != error {
                        callback_error.push_str(&format!("; held-reader boundary: {error}"));
                    }
                }
                return Err(error);
            }
            stop(flags)?;
            status.phase = Phase::Consumed;
        }
    }
    Ok(())
}
#[cfg(any(target_os = "linux", windows, all(test, unix)))]
fn split(
    component: platform::Component<'_>,
) -> (
    Option<platform::Partial<'_>>,
    Option<platform::FinalHandle<'_>>,
) {
    match component {
        platform::Component::Partial(p) => (Some(p), None),
        platform::Component::Final(f) => (None, Some(f)),
    }
}
#[cfg(any(target_os = "linux", windows, all(test, unix)))]
fn transfer(
    transport: &InternalTransport,
    prepared: super::PreparedPrefix<'_>,
    index: usize,
    flags: &Flags,
    shared: &Shared,
    status: &mut Status,
) -> Result<(), String> {
    status.phase = Phase::Downloading;
    let result = transport.transfer_persistent(prepared, &flags.cancel, &flags.pause, |progress| {
        if progress.operation == status.token.operation && progress.component == index {
            status.progress = Some(progress);
            report(shared, status);
        }
    });
    match result {
        Ok(PersistentOutcome::Complete(done)) => {
            status.committed[index] = done.byte_size();
            Ok(())
        }
        Ok(PersistentOutcome::Paused { committed }) => {
            status.committed[index] = committed;
            Err("Internal transfer paused".into())
        }
        Ok(PersistentOutcome::Cancelled {
            committed,
            error,
            uncertain,
        }) => {
            status.committed[index] = committed;
            status.uncertain |= uncertain;
            status.error = error;
            Err("Internal transfer cancelled".into())
        }
        Err(failure) => {
            status.committed[index] = failure.committed;
            status.uncertain |= failure.uncertain;
            Err(failure.message)
        }
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests;

#[cfg(all(test, not(any(target_os = "linux", windows))))]
#[test]
fn unsupported_host_refuses_worker_before_transport_or_consumer() {
    let result = Worker::spawn(|_, _, _, _| panic!("unsupported cache must not invoke consumer"));
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("unsupported cache must not start a worker"),
    };
    assert_eq!(Err::<(), _>(error), imager_backend::cache::unsupported());
}
