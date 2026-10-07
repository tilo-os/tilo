//! Native preparation stays on the UI owner; blocking verification uses the
//! existing bounded session pool. Only the owning loop publishes its outcome.
use super::{
    AccountState, Arc, NativeSignalAccount, NativeSignalHandles, Ordering, SendError,
    SignalAuthority, SignalBroker, SignalReceiver, assemble_native,
};
use crate::session_runtime::{PendingStart, Wake, runtime, start_blocking};
use std::{marker::PhantomData, rc::Rc, sync::atomic::AtomicBool};

const MAX_PENDING: usize = 4;
pub(crate) trait PendingAccounts {
    fn available(&self) -> bool;
    fn push(&self, activation: NativeSignalActivation) -> Result<(), SendError>;
    fn pump(&self);
}
#[derive(Default)]
struct Starts(std::cell::RefCell<Vec<NativeSignalActivation>>);
impl PendingAccounts for Starts {
    fn available(&self) -> bool {
        self.0.borrow().len() < MAX_PENDING
    }
    fn push(&self, activation: NativeSignalActivation) -> Result<(), SendError> {
        let mut starts = self.0.borrow_mut();
        if starts.len() >= MAX_PENDING {
            return Err(SendError::Unavailable);
        }
        starts.push(activation);
        drop(starts);
        Ok(())
    }
    fn pump(&self) {
        self.0
            .borrow_mut()
            .retain_mut(|start| start.poll().is_none());
    }
}
pub(crate) fn starts() -> Rc<dyn PendingAccounts> {
    Rc::new(Starts::default())
}

type Verified = Result<(SignalBroker, SignalReceiver), SendError>;

struct Lease {
    account: Arc<NativeSignalAccount>,
    cancelled: AtomicBool,
}
impl Lease {
    fn check(&self) -> Result<(), SendError> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(SendError::Unavailable);
        }
        self.account.gate.check()
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        // A cancelled in-flight verification remains reserved until its worker
        // releases this lease. A completed installation stays reserved as 2.
        let _ = self
            .account
            .activation
            .compare_exchange(1, 0, Ordering::AcqRel, Ordering::Acquire);
    }
}

/// UI-affine receipt for one frozen account activation. Dropping it cancels
/// publication, not another account, and never replays a provider operation.
pub struct NativeSignalActivation {
    pending: Option<PendingStart<Verified>>,
    lease: Option<Arc<Lease>>,
    interrupts: Option<[std::os::unix::net::UnixStream; 2]>,
    ui_owner: PhantomData<Rc<()>>,
}
impl NativeSignalActivation {
    /// Accept a verified result on the original owning loop. Preparing the
    /// worker, queueing it or verifying one stream never publishes Ready.
    /// The current original execution and credential grant are checked again.
    pub fn poll(&mut self) -> Option<Result<(), SendError>> {
        let outcome = self.pending.as_ref()?.poll()?;
        self.pending.take();
        self.interrupts.take();
        let lease = self.lease.take()?;
        let outcome = outcome
            .map_err(|_| SendError::Unavailable)
            .and_then(std::convert::identity)
            .and_then(|(broker, receiver)| {
                lease.check()?;
                lease.account.install_verified(&broker, receiver)
            });
        if outcome.is_ok() {
            lease.account.activation.store(2, Ordering::Release);
        } else {
            lease.account.activation_failed();
        }
        drop(lease);
        Some(outcome)
    }
}
impl Drop for NativeSignalActivation {
    fn drop(&mut self) {
        if let Some(lease) = self.lease.take() {
            lease.cancelled.store(true, Ordering::Release);
            let _ = lease
                .account
                .supervisor
                .unavailable(AccountState::Unavailable);
            if let Some(streams) = self.interrupts.take() {
                for stream in streams {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                }
            }
        }
        // Closing the receiver discards and drops any unpublished owned pair.
        self.pending.take();
    }
}
impl NativeSignalAccount {
    pub(super) fn activate_with_interrupts(
        self: &Arc<Self>,
        wake: Wake,
        interrupts: [std::os::unix::net::UnixStream; 2],
        assemble: impl FnOnce(Arc<dyn SignalAuthority>) -> Verified + Send + 'static,
    ) -> Result<NativeSignalActivation, SendError> {
        let mut pending = self.activate_on_worker(wake, assemble)?;
        pending.interrupts = Some(interrupts);
        Ok(pending)
    }
    fn activation_failed(&self) {
        let state = if self.gate.check().is_err() {
            AccountState::Locked
        } else {
            AccountState::Failed
        };
        let _ = self.supervisor.unavailable(state);
    }
    /// Verify the supplied owned assembly on the existing bounded session
    /// blocking pool; this returns without waiting for provider/storage I/O.
    /// There is at most one queued/in-flight activation for this account.
    /// # Errors
    /// Refuses a current activation, revoked permission or unavailable runtime.
    pub fn activate_on_worker(
        self: &Arc<Self>,
        wake: Wake,
        assemble: impl FnOnce(Arc<dyn SignalAuthority>) -> Verified + Send + 'static,
    ) -> Result<NativeSignalActivation, SendError> {
        self.activation
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| SendError::AlreadyReserved)?;
        let lease = Arc::new(Lease {
            account: self.clone(),
            cancelled: AtomicBool::new(false),
        });
        if let Err(error) = lease.check() {
            self.activation_failed();
            return Err(error);
        }
        let runtime = runtime().map_err(|_| {
            self.activation_failed();
            SendError::Unavailable
        })?;
        let worker = lease.clone();
        let pending = start_blocking(runtime.handle(), wake, move || {
            // This helper owns all half-assembled state; an unwound helper
            // cannot publish it or bring down the compositor's whole session.
            let outcome = tilo_rt::panic_boundary::isolate(|| {
                worker.check()?;
                let verified = assemble(worker.account.gate.clone())?;
                worker.check()?;
                Ok(verified)
            })
            .unwrap_or(Err(SendError::Unavailable));
            Ok(outcome)
        });
        Ok(NativeSignalActivation {
            pending: Some(pending),
            lease: Some(lease),
            interrupts: None,
            ui_owner: PhantomData,
        })
    }
    /// Production activation of separately owned send/receive handles.
    /// The native supervisor supplies custody; no ambient discovery or login.
    /// # Errors
    /// Refuses relative storage or another current activation.
    pub fn activate_native_on_worker(
        self: &Arc<Self>,
        wake: Wake,
        handles: NativeSignalHandles,
    ) -> Result<NativeSignalActivation, SendError> {
        if !handles.private_root.is_absolute() {
            return Err(SendError::InvalidRequest);
        }
        let interrupts = [
            handles
                .sender
                .try_clone()
                .map_err(|_| SendError::Unavailable)?,
            handles
                .receiver
                .try_clone()
                .map_err(|_| SendError::Unavailable)?,
        ];
        let account = self.clone();
        self.activate_with_interrupts(wake, interrupts, move |authority| {
            assemble_native(&account, handles, authority)
        })
    }
}
