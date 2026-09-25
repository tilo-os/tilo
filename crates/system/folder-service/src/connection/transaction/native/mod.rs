//! Native evidence stays private, move-only and process-local.
pub(in crate::connection) mod descriptor;
#[cfg(target_os = "linux")]
mod directory;
#[cfg(target_os = "linux")]
pub(in crate::connection) use directory::{DirectoryBaselineValue, VerifiedDirectoryComparison};
#[cfg(target_os = "linux")]
pub(in crate::connection) use linux::directory::{
    DirectoryBoundary, DirectoryContinuation, DirectoryOutcome, DirectoryPrepared,
    DirectoryUntouched, NativeDirectoryAdmission,
};
#[cfg(target_os = "linux")]
pub(super) mod linux;
#[cfg(any(target_os = "linux", test))]
mod witness;

#[cfg(target_os = "linux")]
pub(in crate::connection) fn namespace_nonce() -> std::io::Result<[u8; 16]> {
    linux::random_id()
}

/// Dedicated selected private-version execution. Endpoint guards are retained
/// for the observed proposal; the source bytes are owned by the store reader.
#[cfg(target_os = "linux")]
pub(in crate::connection) struct NativeRestore<'e> {
    published: bool,
    outcome: Option<Vec<u8>>,
    relocated: Option<(linux::RelocatedObject, linux::StableFileGuard)>,
    sealed: Option<SealedDisplacement>,
    epoch: linux::NativeEpoch<'e>,
    revision: u64,
    reader: crate::connection::store::VerifiedSelection<'e>,
    source: linux::ParentGuard,
    target: linux::ParentGuard,
    source_guard: Option<linux::StableFileGuard>,
    target_guard: Option<linux::StableFileGuard>,
    private: Option<linux::PrivateNamespace>,
    staged: Option<linux::SelectedStage>,
}

/// Owning current live-version capture; its snapshot is history, its guards are authority.
#[cfg(target_os = "linux")]
pub(in crate::connection) struct NativeConflictCapture<'e> {
    epoch: linux::NativeEpoch<'e>,
    revision: u64,
    snapshot: descriptor::ConflictSnapshot,
    parents: [linux::ParentGuard; 2],
    guards: Vec<Option<linux::StableFileGuard>>,
    namespaces: Vec<linux::CaptureNamespace>,
}

/// Default occupied restore accounts current versions without replacement.
#[cfg(target_os = "linux")]
pub(in crate::connection) struct NativeSelectedCollision<'e> {
    capture: NativeConflictCapture<'e>,
    selected: crate::connection::store::VerifiedSelection<'e>,
}

/// Inspection of already reached selected work after the stop fence.
#[cfg(target_os = "linux")]
pub(in crate::connection) struct NativeSelectedBoundary<'e> {
    proof: NativeRestore<'e>,
}

/// Untouched selected operation: current endpoint guards plus the paid pinned choice.
#[cfg(target_os = "linux")]
pub(in crate::connection) struct SelectedNeverBegun<'e> {
    native: NeverBegun<'e>,
    selected: crate::connection::store::VerifiedSelection<'e>,
}

/// Fresh proof that endpoint mutation has never begun. This also covers the
/// narrowly checked ordinary Verified-equality boundary before baseline commit.
/// Historical admission/Verified facts are only comparison targets; retained
/// native guards and paid rereads remain the live authority.
#[cfg(target_os = "linux")]
pub(in crate::connection) struct NeverBegun<'e> {
    epoch: linux::NativeEpoch<'e>,
    revision: u64,
    identity: descriptor::Admission,
    source: linux::ParentGuard,
    target: linux::ParentGuard,
    source_guard: Option<linux::StableFileGuard>,
    target_guard: Option<linux::StableFileGuard>,
    reservations: Vec<crate::connection::store::NativeNamespaceRecord>,
}

#[cfg(target_os = "linux")]
pub(in crate::connection) enum NativeSafeBoundary<'e> {
    #[expect(
        dead_code,
        reason = "retained no-mutation admission boundary; validation must continue to represent and verify it fail-closed"
    )]
    NeverBegun(NeverBegun<'e>),
    StagedOnly(OwnedBoundary<'e>),
    DisplacedAccounted(OwnedBoundary<'e>),
    PublishedAccounted(OwnedBoundary<'e>),
    #[expect(
        dead_code,
        reason = "retained indeterminate native boundary; validation must continue to reject it as recovery-required"
    )]
    Uncertain,
}
#[cfg(target_os = "linux")]
pub(in crate::connection) enum OwnedBoundary<'e> {
    Regular(RecoveredEvidence<'e>),
    Deletion(RecoveredDeletion<'e>),
}

/// Fresh endpoint guards reconstructed from exact registered clues, never a receipt wrapper.
#[cfg(target_os = "linux")]
pub(in crate::connection) struct RecoveredEvidence<'e> {
    epoch: linux::NativeEpoch<'e>,
    context_revision: u64,
    source: linux::StableFileGuard,
    target: linux::ParentGuard,
    namespace: linux::PrivateNamespace,
    staged: Option<linux::StagedFile>,
    published: Option<linux::NativePublication>,
    outcome: Option<Vec<u8>>,
    target_guard: Option<linux::StableFileGuard>,
    displaced: Option<(linux::RelocatedObject, linux::StableFileGuard)>,
}

#[cfg(target_os = "linux")]
pub(in crate::connection) struct RecoveredDeletion<'e> {
    epoch: linux::NativeEpoch<'e>,
    context_revision: u64,
    source: linux::ParentGuard,
    target: linux::ParentGuard,
    namespace: linux::PrivateNamespace,
    target_guard: Option<linux::StableFileGuard>,
    displaced: Option<(linux::RelocatedObject, linux::StableFileGuard)>,
    outcome: Option<Vec<u8>>,
}

#[cfg(target_os = "linux")]
pub(in crate::connection) struct NativeEquality<'e> {
    epoch: linux::NativeEpoch<'e>,
    source: linux::ParentGuard,
    target: linux::ParentGuard,
    source_guard: Option<linux::StableFileGuard>,
    target_guard: Option<linux::StableFileGuard>,
}

/// Consumed historical metadata capture; deliberately separate from live evidence.
#[cfg(target_os = "linux")]
pub(in crate::connection) struct NativeAdmission<'e> {
    record: crate::connection::store::ConnectionRecord,
    intent: crate::connection::store::OperationIntent,
    lifetime: std::sync::Weak<()>,
    endpoints: [&'e crate::connection::endpoint::PinnedEndpoint; 2],
    paths: [linux::AdmissionPath; 2],
    snapshot: descriptor::Admission,
    deadline: std::time::Instant,
    cancel: &'e std::sync::atomic::AtomicBool,
}

/// Read-only quarantine drain. It deliberately owns no `StepAdmission` and cannot
/// produce a publication seal; the enclosing boundary retains all native guards.
#[cfg(target_os = "linux")]
pub(in crate::connection) struct NativeDrainReader<'a, 'e> {
    boundary: NativeDrainBoundary<'a, 'e>,
    content: descriptor::Content,
}
#[cfg(target_os = "linux")]
enum NativeDrainBoundary<'a, 'e> {
    Ordinary(&'a mut NativeSafeBoundary<'e>),
    Selected(&'a mut NativeSelectedBoundary<'e>),
}

// These live proofs have no public constructors, serialization or cloning.
// Only the descendant Linux producer can fill their retained native evidence.
#[cfg(target_os = "linux")]
pub(in crate::connection) struct NativePrepared<'a, 'e> {
    epoch: &'a mut linux::NativeEpoch<'e>,
    namespace: &'a linux::PrivateNamespace,
    staged: Option<&'a linux::StagedFile>,
}
#[cfg(target_os = "linux")]
pub(in crate::connection) struct NativeRecoveryReader<'a, 'e> {
    selected_reader: Option<&'a mut crate::connection::store::VerifiedSelection<'e>>,
    epoch: &'a mut linux::NativeEpoch<'e>,
    namespace: &'a linux::PrivateNamespace,
    displaced: &'a linux::RelocatedObject,
    guard: &'a linux::StableFileGuard,
    admission: super::StepAdmission,
    content: descriptor::Content,
}
#[cfg(target_os = "linux")]
pub(in crate::connection) struct SealedDisplacement {
    displaced: linux::RelocatedObject,
    guard: linux::StableFileGuard,
    recovery: crate::connection::store::DurableAck<crate::connection::store::BoundRecovery>,
    lifetime: std::sync::Weak<()>,
}

#[cfg(target_os = "linux")]
pub(in crate::connection) struct NativeOutcome<'a, 'e> {
    reconstructed: bool,
    epoch: &'a mut linux::NativeEpoch<'e>,
    source: &'a linux::StableFileGuard,
    target: &'a linux::ParentGuard,
    namespace: &'a linux::PrivateNamespace,
    publication: linux::NativePublication,
    displaced: Option<SealedDisplacement>,
    descriptor: Vec<u8>,
}

#[cfg(target_os = "linux")]
pub(in crate::connection) struct NativeDeletion<'a, 'e> {
    reconstructed: bool,
    epoch: &'a mut linux::NativeEpoch<'e>,
    source: &'a linux::ParentGuard,
    target: &'a linux::ParentGuard,
    namespace: &'a linux::PrivateNamespace,
    displaced: SealedDisplacement,
    descriptor: Vec<u8>,
}

/// Original retained chains plus one immutable comparison stability window.
/// The durable receipt is deliberately separate from this nonserializable owner.
#[cfg(target_os = "linux")]
struct ExistingAncestorProof {
    admission: descriptor::Admission,
    parents: [linux::ParentGuard; 2],
    stability: Option<[Vec<linux::Stamp>; 2]>,
}
#[cfg(target_os = "linux")]
pub(in crate::connection) struct VerifiedComparison<'a> {
    existing_ancestors: Option<crate::connection::store::ExistingAncestorPatchV1>,
    evidence: &'a mut dyn CompletionEvidence,
    id: crate::connection::store::OperationId,
    revision: u64,
    fence: u64,
    lifetime: std::sync::Weak<()>,
    value: Option<crate::connection::model::EntryState>,
}
#[cfg(target_os = "linux")]
trait CompletionEvidence {
    #[cfg(test)]
    fn revoke_for_test(&mut self);
    fn fresh(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        live: &crate::connection::store::LiveOperation,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<Option<crate::connection::model::EntryState>, crate::connection::store::StoreError>;
    fn deadline(&self) -> std::time::Instant;
}

// Shared wire validation is deliberately std-only so its adversarial traces run
// on the development host as well as in the real Linux transport.
const FRAME_BYTES: usize = 40;
const MAX_FRAME: usize = 4096;
const MAX_GUARDS: u32 = 4;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Code {
    Hello = 1,
    Acquire = 2,
    Check = 3,
    Shutdown = 4,
    Acquired = 5,
    Healthy = 6,
    Revoked = 7,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Frame {
    epoch: [u8; 16],
    request: u64,
    guard: u32,
    code: Code,
}
impl Frame {
    fn encode(self) -> [u8; FRAME_BYTES] {
        let mut bytes = [0; FRAME_BYTES];
        bytes[..4].copy_from_slice(b"QLS1");
        bytes[4] = self.code as u8;
        bytes[8..24].copy_from_slice(&self.epoch);
        bytes[24..32].copy_from_slice(&self.request.to_le_bytes());
        bytes[32..36].copy_from_slice(&self.guard.to_le_bytes());
        bytes
    }
    fn decode(bytes: &[u8]) -> Result<Self, ()> {
        if bytes.len() != FRAME_BYTES
            || bytes.get(..4) != Some(b"QLS1")
            || bytes.get(5..8) != Some(&[0; 3])
            || bytes.get(36..40) != Some(&[0; 4])
        {
            return Err(());
        }
        let code = match bytes.get(4) {
            Some(1) => Code::Hello,
            Some(2) => Code::Acquire,
            Some(3) => Code::Check,
            Some(4) => Code::Shutdown,
            Some(5) => Code::Acquired,
            Some(6) => Code::Healthy,
            Some(7) => Code::Revoked,
            _ => return Err(()),
        };
        Ok(Self {
            epoch: bytes.get(8..24).ok_or(())?.try_into().map_err(|_| ())?,
            request: u64::from_le_bytes(bytes.get(24..32).ok_or(())?.try_into().map_err(|_| ())?),
            guard: u32::from_le_bytes(bytes.get(32..36).ok_or(())?.try_into().map_err(|_| ())?),
            code,
        })
    }
}
struct Admission {
    epoch: [u8; 16],
    next: u64,
    guards: u32,
    pending: Option<Frame>,
    revoked: bool,
}
impl Admission {
    fn new(epoch: [u8; 16]) -> Self {
        Self {
            epoch,
            next: 1,
            guards: 0,
            pending: None,
            revoked: false,
        }
    }
    fn revoke(&mut self) {
        self.revoked = true;
        self.pending = None;
    }
    fn request(&mut self, code: Code) -> Result<Frame, ()> {
        if self.revoked
            || self.pending.is_some()
            || self.next == u64::MAX
            || !matches!(code, Code::Acquire | Code::Check | Code::Shutdown)
            || (code == Code::Acquire && self.guards >= MAX_GUARDS)
        {
            self.revoke();
            return Err(());
        }
        let frame = Frame {
            epoch: self.epoch,
            request: self.next,
            guard: if code == Code::Acquire {
                self.guards + 1
            } else {
                0
            },
            code,
        };
        self.pending = Some(frame);
        self.next += 1;
        Ok(frame)
    }
    fn accept(&mut self, reply: Frame) -> Result<(), ()> {
        let valid = self.pending.is_some_and(|request| {
            !self.revoked
                && request.epoch == reply.epoch
                && request.request == reply.request
                && request.guard == reply.guard
                && reply.code
                    == if request.code == Code::Acquire {
                        Code::Acquired
                    } else {
                        Code::Healthy
                    }
        });
        if !valid {
            self.revoke();
            return Err(());
        }
        if reply.code == Code::Acquired {
            self.guards += 1;
        }
        self.pending = None;
        Ok(())
    }
}
// A receiver retains this reservation through asynchronous reaping. A stalled
// old child therefore prevents an unbounded series of new receivers/reap tasks.
struct ReceiverReservation(&'static std::sync::atomic::AtomicBool);
impl ReceiverReservation {
    fn acquire(slot: &'static std::sync::atomic::AtomicBool) -> Result<Self, ()> {
        slot.compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        )
        .map_err(|_| ())?;
        Ok(Self(slot))
    }
}
impl Drop for ReceiverReservation {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receiver_slot_remains_owned_until_cleanup_releases_it() {
        static SLOT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let retained = ReceiverReservation::acquire(&SLOT).unwrap();
        assert!(ReceiverReservation::acquire(&SLOT).is_err());
        drop(retained);
        assert!(ReceiverReservation::acquire(&SLOT).is_ok());
    }
    fn acquired(request: Frame) -> Frame {
        Frame {
            code: Code::Acquired,
            ..request
        }
    }
    #[test]
    fn wire_roundtrip_and_reject_noncanonical_frames() {
        let frame = Frame {
            epoch: [2; 16],
            request: 17,
            guard: 1,
            code: Code::Acquire,
        };
        assert_eq!(Frame::decode(&frame.encode()), Ok(frame));
        for index in [0, 5, 6, 7, 36, 37, 38, 39] {
            let mut bytes = frame.encode();
            bytes[index] ^= 1;
            assert!(Frame::decode(&bytes).is_err());
        }
        for len in [0, 39, 41, MAX_FRAME, MAX_FRAME + 1] {
            assert!(Frame::decode(&vec![0; len]).is_err());
        }
    }
    #[test]
    fn fifth_guard_and_concurrent_request_revoke_epoch() {
        let mut state = Admission::new([1; 16]);
        for _ in 0..MAX_GUARDS {
            let req = state.request(Code::Acquire).unwrap();
            state.accept(acquired(req)).unwrap();
        }
        assert!(state.request(Code::Acquire).is_err());
        assert!(state.request(Code::Check).is_err());
        let mut state = Admission::new([2; 16]);
        state.request(Code::Check).unwrap();
        assert!(state.request(Code::Check).is_err());
    }
    #[test]
    fn stale_epoch_wrong_guard_and_wrong_sequence_are_permanent() {
        for field in 0..3 {
            let mut state = Admission::new([1; 16]);
            let req = state.request(Code::Acquire).unwrap();
            let mut reply = acquired(req);
            match field {
                0 => reply.epoch[0] = 2,
                1 => reply.guard += 1,
                _ => reply.request += 1,
            }
            assert!(state.accept(reply).is_err());
            assert!(state.accept(acquired(req)).is_err());
            assert!(state.request(Code::Check).is_err());
        }
    }
    #[test]
    fn queued_success_cannot_undo_break_or_cancel() {
        let mut state = Admission::new([1; 16]);
        let req = state.request(Code::Acquire).unwrap();
        state.revoke();
        assert!(state.accept(acquired(req)).is_err());
        assert!(state.request(Code::Check).is_err());
    }
    #[test]
    fn sequence_exhaustion_refuses_instead_of_wrapping() {
        let mut state = Admission::new([1; 16]);
        state.next = u64::MAX;
        assert!(state.request(Code::Check).is_err());
    }
}

// Shared by native copying and Connected observation so neither can reset the
// same operation's aggregate read budget by opening another reader.
pub(in crate::connection) struct OperationReads {
    credit: std::sync::Mutex<ReadCredit>,
}
#[derive(Default)]
struct ReadCredit {
    sequence: u64,
    available: u64,
}
impl OperationReads {
    pub(in crate::connection) fn new() -> Self {
        Self::reopened(0)
    }
    pub(in crate::connection) fn reopened(sequence: u64) -> Self {
        Self {
            credit: std::sync::Mutex::new(ReadCredit {
                sequence,
                available: 0,
            }),
        }
    }
    /// Only the serialized store owner grants credit after `ReadReserve` fsync.
    pub(in crate::connection) fn grant(&self, sequence: u64, bytes: u64) -> Result<(), ()> {
        let mut credit = self.credit.lock().map_err(|_| ())?;
        if sequence != credit.sequence.checked_add(1).ok_or(())? {
            return Err(());
        }
        let available = credit
            .available
            .checked_add(bytes)
            .filter(|n| *n <= 1_073_741_824)
            .ok_or(())?;
        credit.sequence = sequence;
        credit.available = available;
        drop(credit);
        Ok(())
    }
    pub(in crate::connection) fn require_credit(&self, count: u64) -> Result<(), ()> {
        let credit = self.credit.lock().map_err(|_| ())?;
        if credit.available < count {
            return Err(());
        }
        drop(credit);
        Ok(())
    }
    pub(in crate::connection) fn charge(&self, count: u64) -> Result<(), ()> {
        let mut credit = self.credit.lock().map_err(|_| ())?;
        credit.available = credit.available.checked_sub(count).ok_or(())?;
        drop(credit);
        Ok(())
    }
    #[cfg(test)]
    fn funded_for_test() -> std::sync::Arc<Self> {
        let reads = std::sync::Arc::new(Self::new());
        reads.grant(1, 1_073_741_824).unwrap();
        reads
    }
}
struct ReadBudget {
    reads: std::sync::Arc<OperationReads>,
    deadline: std::time::Instant,
}
impl ReadBudget {
    #[cfg(test)]
    fn for_operation(reads: &std::sync::Arc<OperationReads>, deadline: std::time::Instant) -> Self {
        Self {
            reads: std::sync::Arc::clone(reads),
            deadline,
        }
    }
    fn charge(&self, count: u64) -> Result<(), ()> {
        if std::time::Instant::now() >= self.deadline {
            return Err(());
        }
        self.reads.charge(count)
    }
}
#[cfg(test)]
mod budget_tests {
    use super::*;
    #[test]
    fn granted_credit_is_single_issue_and_atomic_across_readers() {
        let reads = std::sync::Arc::new(OperationReads::new());
        let unit = 1024 * 1024;
        reads.grant(1, 16 * unit).unwrap();
        assert!(reads.grant(1, 16 * unit).is_err());
        assert!(reads.grant(3, unit).is_err());
        let count = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let reads = &reads;
                let count = &count;
                scope.spawn(move || {
                    for _ in 0..4 {
                        if reads.charge(unit).is_ok() {
                            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        }
                    }
                });
            }
        });
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 16);
        assert!(reads.charge(1).is_err());
        reads.grant(2, 1).unwrap();
        assert!(reads.charge(1).is_ok());
        assert!(reads.charge(u64::MAX).is_err());
    }
    #[test]
    fn unreserved_native_reads_have_no_spendable_credit() {
        let reads = std::sync::Arc::new(OperationReads::new());
        let budget = ReadBudget::for_operation(
            &reads,
            std::time::Instant::now() + std::time::Duration::from_secs(30),
        );
        assert!(budget.charge(0).is_ok());
        assert!(budget.charge(1).is_err());
    }
    #[test]
    fn new_epoch_keeps_the_same_operation_read_allowance() {
        let reads = OperationReads::funded_for_test();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let first = ReadBudget::for_operation(&reads, deadline);
        assert!(first.charge(1024 * 1024 * 1024).is_ok());
        drop(first);
        let resumed = ReadBudget::for_operation(&reads, deadline);
        assert!(resumed.charge(1).is_err());
    }
    #[test]
    fn aggregate_reads_cannot_reset_the_limit_or_outlive_the_deadline() {
        let budget = std::sync::Arc::new(ReadBudget {
            reads: OperationReads::funded_for_test(),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(60),
        });
        let second_reader = std::sync::Arc::clone(&budget);
        assert!(budget.charge(1024 * 1024 * 1024 - 1).is_ok());
        assert!(second_reader.charge(1).is_ok());
        assert!(budget.charge(1).is_err());
        assert!(second_reader.charge(u64::MAX).is_err());
        assert_eq!(budget.reads.credit.lock().unwrap().available, 0);
        let expired = ReadBudget {
            reads: std::sync::Arc::new(OperationReads::new()),
            deadline: std::time::Instant::now(),
        };
        assert!(expired.charge(0).is_err());
    }
}

#[cfg(target_os = "linux")]
pub(in crate::connection) struct NativeOwnedNames {
    directory_requests: Option<(
        crate::connection::store::OperationId,
        std::sync::Arc<std::sync::atomic::AtomicU16>,
    )>,
    records: Vec<crate::connection::store::NativeNamespaceRecord>,
    root: linux::ObjectId,
    endpoint: [u8; 64],
    budget: std::sync::Arc<ReadBudget>,
    epoch_lifetime: std::sync::Weak<()>,
    store_lifetime: std::sync::Weak<()>,
}

/// Per-observation ownership snapshots; never reused by a later scan.
#[cfg(target_os = "linux")]
pub(in crate::connection) struct NativeOwnedObservation<'a> {
    names: &'a NativeOwnedNames,
    snapshots: std::cell::RefCell<std::collections::BTreeMap<String, linux::OwnedSnapshot>>,
}
