//! Private retained download worker. No staging, UI, or support route is enabled.
//! Selection authority and all leased handles stay on this worker. Only bounded,
//! owned descriptions cross the control boundary; Ready is deliberately private.
use crate::cache::{HeldReadSeek, Session};
use crate::{
    catalog::InternalRelease,
    transport::{InternalTransport, PersistentOutcome, SelectedManifest},
};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};

use super::platform;
use crate::download::{Cancellation, NEXT_OPERATION, Phase, Status, Token, next_operation};
use crate::preference::PreferenceStore;
use crate::{PreferenceFailure, PreferenceNamespace, PreferencePublication, PreferenceState};
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
    saved_key: Option<Vec<u8>>,
}
#[derive(Clone, Copy)]
enum Action {
    #[cfg_attr(
        not(all(test, unix)),
        expect(
            dead_code,
            reason = "the public facade cannot construct this private action; crate tests prove protected staging fails closed"
        )
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
    preference: PreferenceState,
}
#[derive(Default)]
pub(super) struct Shared {
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
pub(crate) struct Worker {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}
impl Worker {
    pub(super) fn spawn_runner(
        run: impl FnOnce(Arc<Shared>) + Send + 'static,
    ) -> Result<Self, crate::error::Failure> {
        let shared = Arc::new(Shared::default());
        let worker = shared.clone();
        let thread = thread::Builder::new()
            .name("internal-cache".into())
            .spawn(move || run(worker))
            .map_err(|e| crate::error::Failure::from(e.to_string()))?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }
    pub(crate) fn start(&self, release: InternalRelease) -> Result<Token, crate::error::Failure> {
        self.submit(release, None, None)
    }
    pub(crate) fn start_with_preference(
        &self,
        release: InternalRelease,
        bytes: Vec<u8>,
    ) -> Result<Token, crate::error::Failure> {
        self.submit(release, None, Some(bytes))
    }
    pub(crate) fn preference(&self) -> PreferenceState {
        self.shared.mailbox.lock().unwrap().preference.clone()
    }
    /// A new authenticated cleanup operation and a NEW cancellation flag. The
    /// transfer's cancellation latch is never cleared. Verified finals remain.
    pub(crate) fn discard(
        &self,
        target: &Token,
        release: InternalRelease,
    ) -> Result<Token, crate::error::Failure> {
        self.submit(release, Some(target), None)
    }
    fn submit(
        &self,
        release: InternalRelease,
        target: Option<&Token>,
        saved_key: Option<Vec<u8>>,
    ) -> Result<Token, crate::error::Failure> {
        let mut m = self.shared.mailbox.lock().unwrap();
        if m.shutdown {
            return Err(crate::error::Failure::closed("Worker is shut down"));
        }
        if let Some(target) = target {
            if m.active.as_ref().is_none_or(|(t, _)| t != target)
                || target.selection != release.manifest_sha256()
            {
                return Err(crate::error::Failure::stale(
                    "Stale cleanup selection/operation",
                ));
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
            saved_key,
        });
        m.action = None;
        m.status = Some(Status::new(token.clone()));
        self.shared.wake.notify_all();
        Ok(token)
    }
    pub(crate) fn pause(&self, token: &Token) -> Result<(), crate::error::Failure> {
        self.stop(token, false)
    }
    pub(crate) fn cancel(&self, token: &Token) -> Result<(), crate::error::Failure> {
        self.stop(token, true)
    }
    fn stop(&self, token: &Token, cancel: bool) -> Result<(), crate::error::Failure> {
        let m = self.shared.mailbox.lock().unwrap();
        if m.shutdown {
            return Err(crate::error::Failure::closed("Worker is shut down"));
        }
        if m.status.as_ref().is_none_or(|s| terminal(s.phase)) {
            return Err(crate::error::Failure::stale(
                "Operation is already terminal",
            ));
        }
        let (_, flags) = m
            .active
            .as_ref()
            .filter(|(t, _)| t == token)
            .ok_or_else(|| crate::error::Failure::stale("Stale stop operation"))?;
        if cancel {
            flags.cancel.store(true, Ordering::Release);
        } else {
            flags.pause.store(true, Ordering::Release);
        }
        self.shared.wake.notify_all();
        Ok(())
    }
    #[cfg_attr(
        not(all(test, unix)),
        expect(
            dead_code,
            reason = "retained for crate security tests; no public consume operation may expose held finals"
        )
    )]
    pub(crate) fn consume(&self, token: &Token) -> Result<(), crate::error::Failure> {
        self.action(token, Action::Consume)
    }
    pub(crate) fn release(&self, token: &Token) -> Result<(), crate::error::Failure> {
        self.action(token, Action::Release)
    }
    fn action(&self, token: &Token, action: Action) -> Result<(), crate::error::Failure> {
        let mut m = self.shared.mailbox.lock().unwrap();
        if m.shutdown {
            return Err(crate::error::Failure::closed("Worker is shut down"));
        }
        if m.active.as_ref().is_none_or(|(t, f)| {
            t != token || f.cancel.load(Ordering::Acquire) || f.pause.load(Ordering::Acquire)
        }) || m
            .status
            .as_ref()
            .is_none_or(|s| s.token != *token || s.phase != Phase::Ready)
            || m.action.is_some()
        {
            return Err(crate::error::Failure::stale(
                "Stale or unavailable held Ready",
            ));
        }
        m.action = Some(action);
        self.shared.wake.notify_all();
        Ok(())
    }
    pub(crate) fn status(&self) -> Option<Status> {
        self.shared.mailbox.lock().unwrap().status.clone()
    }
    pub(crate) fn close(&mut self) -> Result<(), crate::error::Failure> {
        let mut panicked = false;
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
                panicked = true;
                if let Some(status) = &mut self.shared.mailbox.lock().unwrap().status {
                    status.phase = Phase::Failed;
                    status.failure_kind = Some(crate::ErrorKind::Failed);
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
        if panicked {
            Err(
                crate::error::Failure::from("Download worker panicked; cache may be retained")
                    .with_uncertainty(),
            )
        } else {
            Ok(())
        }
    }
    pub(crate) fn shutdown(&mut self) {
        let _ = self.close();
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn publish_preference_failure(shared: &Shared, failure: &PreferenceFailure) {
    shared.mailbox.lock().unwrap().preference = PreferenceState::Failed(failure.clone());
    shared.wake.notify_all();
}
fn preference_operation_error(failure: PreferenceFailure) -> crate::Error {
    let uncertain = failure.publication != PreferencePublication::NotPublished
        || matches!(
            failure.namespace,
            PreferenceNamespace::CreatedDurabilityUnknown
                | PreferenceNamespace::NamespaceUnknown
                | PreferenceNamespace::Unproven
        );
    let error = crate::Error::new(failure.kind, failure.message);
    if uncertain {
        error.with_uncertainty()
    } else {
        error
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
fn stop(flags: &Flags) -> Result<(), crate::error::Failure> {
    if flags.cancel.load(Ordering::Acquire) {
        Err("Internal transfer cancelled".into())
    } else if flags.pause.load(Ordering::Acquire) {
        Err("Internal transfer paused".into())
    } else {
        Ok(())
    }
}
pub(super) fn worker_loop(
    shared: Arc<Shared>,
    transport: InternalTransport,
    mut consumer: impl FnMut(
        &SelectedManifest,
        &Cancellation<'_>,
        &mut dyn HeldReadSeek,
        &mut dyn HeldReadSeek,
    ) -> Result<(), crate::error::Failure>,
    preference: PreferenceStore,
) {
    {
        let loaded = match preference.load() {
            Ok(bytes) => PreferenceState::Loaded(bytes),
            Err(failure) => PreferenceState::Failed(failure),
        };
        shared.mailbox.lock().unwrap().preference = loaded;
        shared.wake.notify_all();
    }
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
            if let Some(bytes) = request.saved_key.as_deref() {
                status.phase = Phase::SavingPreference;
                report(&shared, &status);
                let commit = preference.save(bytes).map_err(|failure| {
                    publish_preference_failure(&shared, &failure);
                    crate::error::Failure::from(preference_operation_error(failure))
                })?;
                status.preference_commit = Some(commit);
                report(&shared, &status);
                stop(&request.flags)?;
            }
            status.phase = Phase::Authenticating;
            report(&shared, &status);
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
            status.failure_kind.get_or_insert(error.kind());
            status.uncertain |= error.uncertain();
            status.error.get_or_insert_with(|| error.to_string());
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

struct Ready<'s> {
    selected: &'s SelectedManifest,
    finals: [platform::FinalHandle<'s>; 2],
}
impl Ready<'_> {
    fn consume(
        &mut self,
        cancel: &AtomicBool,
        consumer: &mut impl FnMut(
            &SelectedManifest,
            &Cancellation<'_>,
            &mut dyn HeldReadSeek,
            &mut dyn HeldReadSeek,
        ) -> Result<(), crate::error::Failure>,
    ) -> Result<(), crate::error::Failure> {
        let [first, second] = &mut self.finals;
        first.with_verified_reader(cancel, |a| {
            second.with_verified_reader(cancel, |b| {
                consumer(self.selected, &Cancellation::new(cancel), a, b)
            })
        })
    }
}

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
    ) -> Result<(), crate::error::Failure>,
) -> Result<(), crate::error::Failure> {
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
                    status.error = Some(error.to_string());
                }
                result
            });
            if let Err(error) = consumed {
                // Verification, cursor restoration and native guards can fail
                // alongside cancellation. Conservatively retain that uncertainty.
                status.uncertain = true;
                if let Some(callback_error) = &mut status.error {
                    if *callback_error != error.to_string() {
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
fn transfer(
    transport: &InternalTransport,
    prepared: crate::cache::PreparedPrefix<'_>,
    index: usize,
    flags: &Flags,
    shared: &Shared,
    status: &mut Status,
) -> Result<(), crate::error::Failure> {
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
            if let Some(error) = error {
                status.failure_kind = Some(error.kind());
                status.uncertain |= error.uncertain();
                status.error = Some(error.to_string());
            }
            Err("Internal transfer cancelled".into())
        }
        Err(failure) => {
            status.committed[index] = failure.committed;
            status.uncertain |= failure.uncertain;
            Err(failure.message)
        }
    }
}

#[cfg(all(test, unix))]
#[path = "coordinator/tests.rs"]
mod tests;
