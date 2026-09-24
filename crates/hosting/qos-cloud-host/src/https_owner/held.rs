//! Sole paid aggregate and native peer under the original Cloud Store lock.
//! This private component does not install a route or qualify a descriptor.
use super::{CloudDeliveryError, CloudStoreSource, Current};
use crate::{
    State, StoreCommitFailure,
    delivery::resources::{
        AggregateBytes, CodecBudget, DecodeLimits, HeldCloudState, MetadataChange, ResourceError,
        encode_state,
    },
};
use automerge::resource_limits::NativeReservation;
use qos_action_broker::peer_actions::PolicyTime;
use qos_peer_state::{BoundedPeerError, BoundedStatePeer, PeerWork};
use qos_user_identity::{DeviceCert, RevocationList};
use std::cell::Cell;

type Result<T> = std::result::Result<T, CloudDeliveryError>;

pub(super) struct HeldStoreTransaction {
    current: HeldCloudState,
    peer: BoundedStatePeer,
    lock: crate::storage::transaction::ExistingStoreLock,
    source: CloudStoreSource,
    time: PolicyTime,
    failed: Cell<Option<StoreCommitFailure>>,
    publication_error: Option<crate::storage::transaction::PublishFailure>,
    // Original path/identity metadata backing must die before its reservation.
    _metadata: NativeReservation,
}

impl HeldStoreTransaction {
    pub(super) fn open(
        source: &CloudStoreSource,
        time: &PolicyTime,
        work: &mut PeerWork<'_>,
        limits: DecodeLimits,
    ) -> Current<Result<Self>> {
        match Self::open_checked(source, time, work, limits) {
            Ok(held) => Current::Current(Ok(held)),
            Err(OpenFailure::Busy) => Current::Busy,
            Err(OpenFailure::Capacity) => Current::Current(Err(CloudDeliveryError::Capacity)),
            Err(OpenFailure::Denied) => source.deny(),
        }
    }

    #[cfg(unix)]
    fn open_checked(
        source: &CloudStoreSource,
        time: &PolicyTime,
        work: &mut PeerWork<'_>,
        limits: DecodeLimits,
    ) -> std::result::Result<Self, OpenFailure> {
        use crate::storage::transaction::{ExistingStoreLock, TypedLockFailure};
        use atomic_file::{FileIdentity, StorageDirectory};
        use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt};
        source.checked_time(time).map_err(|_| OpenFailure::Denied)?;
        // Prepay the retained PathBuf, the temporary OS path conversion, and
        // atomic-file's bounded names / typed I/O errors. No anyhow/backtrace
        // is constructed under this paid path. Atomic child names are fixed:
        // state.json/state.lock; stage names add at most 48 bytes. The 512-byte
        // error bound covers the nested PublicationError + std Custom boxes,
        // their fixed diagnostic strings and ExistingCommitError custody.
        let metadata_bytes = source
            .0
            .root
            .as_os_str()
            .len()
            .checked_mul(2)
            .and_then(|n| n.checked_add(1 + 2 * (10 + 48) + 128 + 512))
            .ok_or(OpenFailure::Capacity)?;
        let metadata = work.reserve_output(metadata_bytes).map_err(peer_failure)?;
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&source.0.root)
            .map_err(|_| OpenFailure::Denied)?;
        if FileIdentity::of(&directory).map_err(|_| OpenFailure::Denied)?
            != FileIdentity::of(&source.0.directory).map_err(|_| OpenFailure::Denied)?
        {
            return Err(OpenFailure::Denied);
        }
        let lock = ExistingStoreLock::acquire_typed(&source.0.root, directory, false).map_err(
            |error| match error {
                TypedLockFailure::Busy => OpenFailure::Busy,
                TypedLockFailure::Io(_) => OpenFailure::Denied,
            },
        )?;
        let storage = StorageDirectory::from_file(
            lock.directory()
                .try_clone()
                .map_err(|_| OpenFailure::Denied)?,
        )
        .map_err(|_| OpenFailure::Denied)?;
        let mut aggregate =
            crate::storage::Aggregate::open(&storage).map_err(|_| OpenFailure::Denied)?;
        let current = {
            let budget = CodecBudget::peer(work);
            let bytes = AggregateBytes::read_file(
                aggregate.checkpoint_file(),
                limits.maximum_bytes,
                &budget,
            )
            .map_err(resource_failure)?;
            let bytes = match aggregate.journal_file() {
                Some(journal) => bytes
                    .with_journal(journal, limits.maximum_bytes, &budget)
                    .map_err(resource_failure)?,
                None => bytes,
            };
            // Reconcile precisely the opened files/names before any state is
            // usable. A failed barrier never returns an admitted aggregate.
            aggregate
                .reconcile(&storage)
                .map_err(|_| OpenFailure::Denied)?;
            HeldCloudState::decode(&bytes, limits, &budget).map_err(resource_failure)?
        };
        source
            .checked_state(current.state(), lock.directory(), time)
            .map_err(|_| OpenFailure::Denied)?;
        current
            .validate_private(work)
            .map_err(|failure| match failure {
                crate::delivery::resources::PrivateStateFailure::Capacity => OpenFailure::Capacity,
                crate::delivery::resources::PrivateStateFailure::Denied => OpenFailure::Denied,
            })?;
        #[cfg(test)]
        tests::at_phase(tests::OpenPhase::AfterPrivate);
        // Recheck this exact configured owner after private work. Historical
        // certificates retained by native restore do not establish currentness.
        let now = source
            .checked_state(current.state(), lock.directory(), time)
            .map_err(|_| OpenFailure::Denied)?;
        let bound = current.state().bound.as_ref().ok_or(OpenFailure::Denied)?;
        let revoked = work
            .verified_revocations(&bound.revocations)
            .map_err(peer_failure)?;
        let roster = PaidRoster::active(&bound.roster, bound.root, revoked.list(), now, work)?;
        // Same mutable work and continuing normalized-input counter used by the
        // outer codec. No legacy StatePeer, second domain or extracted state.
        let peer = BoundedStatePeer::restore(
            &bound.peer,
            &current.state().room,
            &bound.authority,
            &roster.certificates,
            revoked.list(),
            crate::now_time(now),
            work,
        )
        .map_err(peer_failure)?;
        #[cfg(test)]
        tests::at_phase(tests::OpenPhase::AfterNative);
        current
            .validate_delivery(&peer, work)
            .map_err(|failure| match failure {
                crate::delivery::resources::DeliveryStateFailure::Capacity => OpenFailure::Capacity,
                crate::delivery::resources::DeliveryStateFailure::Denied => OpenFailure::Denied,
            })?;
        #[cfg(test)]
        tests::at_phase(tests::OpenPhase::AfterDelivery);
        source
            .checked_state(current.state(), lock.directory(), time)
            .map_err(|_| OpenFailure::Denied)?;
        Ok(Self {
            current,
            peer,
            lock,
            source: source.clone(),
            time: time.clone(),
            failed: Cell::new(None),
            publication_error: None,
            _metadata: metadata,
        })
    }

    #[cfg(not(unix))]
    fn open_checked(
        _: &CloudStoreSource,
        _: &PolicyTime,
        _: &mut PeerWork<'_>,
        _: DecodeLimits,
    ) -> std::result::Result<Self, OpenFailure> {
        Err(OpenFailure::Denied)
    }

    pub(super) fn state(&self) -> &State {
        self.current.state()
    }

    pub(super) fn commit_metadata(
        &mut self,
        change: MetadataChange<'_>,
        work: &mut PeerWork<'_>,
        limits: DecodeLimits,
    ) -> Result<bool> {
        use crate::storage::transaction::{PublishFailure, publish_bytes_typed};
        if let Some(failure) = self.failed.get() {
            return Err(CloudDeliveryError::OwnerUncertain(failure));
        }
        self.fresh()?;
        // Even metadata-only candidates must stay in the original peer domain.
        // A new budget or caller-supplied PeerWork cannot substitute that owner.
        if self.peer.read(work).is_err() {
            self.source.retire();
            return Err(CloudDeliveryError::Denied);
        }
        let next = self
            .state()
            .generation
            .checked_add(1)
            .ok_or(CloudDeliveryError::Capacity)?;
        let budget = CodecBudget::peer(work);
        let candidate = self
            .current
            .metadata_candidate(change, next, limits, &budget)
            .map_err(candidate_failure)?;
        let Some(candidate) = candidate else {
            self.fresh()?;
            return Ok(false);
        };
        let output = encode_state(candidate.state(), limits.maximum_bytes, &budget)
            .map_err(candidate_failure)?;
        self.fresh()?;
        if let Err(error) = publish_bytes_typed(
            self.lock.directory(),
            self.lock.root(),
            &self.failed,
            Some(&self.time),
            next,
            output.as_bytes(),
        ) {
            let result = match &error {
                PublishFailure::Latched(failure) | PublishFailure::Io { failure, .. } => {
                    CloudDeliveryError::OwnerUncertain(*failure)
                }
                PublishFailure::Clock(_) => CloudDeliveryError::Denied,
            };
            // Retain the exact original I/O error inside its paid owner. The
            // public refusal contains only the real atomic publication outcome.
            self.publication_error = Some(error);
            self.source.retire();
            return Err(result);
        }
        self.current = candidate;
        if self
            .source
            .checked_state(self.state(), self.lock.directory(), &self.time)
            .is_err()
        {
            self.source.retire();
            return Err(CloudDeliveryError::Denied);
        }
        Ok(true)
    }
    fn fresh(&self) -> Result<()> {
        if self.source.checked_time(&self.time).is_err() {
            self.source.retire();
            return Err(CloudDeliveryError::Denied);
        }
        Ok(())
    }
}

fn candidate_failure(error: ResourceError) -> CloudDeliveryError {
    match error {
        ResourceError::Capacity => CloudDeliveryError::Capacity,
        _ => CloudDeliveryError::InvalidInput,
    }
}

enum OpenFailure {
    Busy,
    Capacity,
    Denied,
}
fn resource_failure(error: ResourceError) -> OpenFailure {
    match error {
        ResourceError::Capacity => OpenFailure::Capacity,
        _ => OpenFailure::Denied,
    }
}
fn peer_failure(error: BoundedPeerError) -> OpenFailure {
    use automerge::resource_limits::NativeResourceError as Native;
    use qos_peer_state::resources::PeerResourceError;
    match error {
        BoundedPeerError::Resource(
            Native::NormalizedBytes
            | Native::Operations
            | Native::Collection
            | Native::ValueBytes
            | Native::WorkBytes,
        )
        | BoundedPeerError::Decode(PeerResourceError::Capacity) => OpenFailure::Capacity,
        _ => OpenFailure::Denied,
    }
}
struct PaidRoster {
    certificates: Vec<DeviceCert>,
    _reservation: NativeReservation,
}
impl PaidRoster {
    fn active(
        roster: &[DeviceCert],
        root: [u8; 32],
        revoked: &RevocationList,
        now: u64,
        work: &PeerWork<'_>,
    ) -> std::result::Result<Self, OpenFailure> {
        let mut count = 0usize;
        let mut bytes = 0usize;
        for cert in roster {
            // Keep the original Cloud historical signature/root checks even
            // when the original row will be excluded by expiry/revocation.
            qos_user_identity::verify_device_cert(cert).map_err(|_| OpenFailure::Denied)?;
            if cert.user_public_key != root {
                return Err(OpenFailure::Denied);
            }
            if !cert.is_expired_at(now) && !revoked.is_revoked(&cert.device_public_key) {
                count = count.checked_add(1).ok_or(OpenFailure::Capacity)?;
                bytes = bytes
                    .checked_add(size_of::<DeviceCert>())
                    .and_then(|n| n.checked_add(cert.device_label.len()))
                    .ok_or(OpenFailure::Capacity)?;
            }
        }
        let reservation = work.reserve_output(bytes).map_err(peer_failure)?;
        let mut certificates = Vec::new();
        certificates
            .try_reserve_exact(count)
            .map_err(|_| OpenFailure::Capacity)?;
        for cert in roster {
            if cert.is_expired_at(now) || revoked.is_revoked(&cert.device_public_key) {
                continue;
            }
            let mut device_label = String::new();
            device_label
                .try_reserve_exact(cert.device_label.len())
                .map_err(|_| OpenFailure::Capacity)?;
            device_label.push_str(&cert.device_label);
            certificates.push(DeviceCert {
                user_public_key: cert.user_public_key,
                device_public_key: cert.device_public_key,
                device_label,
                capabilities: cert.capabilities.clone(),
                issued_at_ns: cert.issued_at_ns,
                expires_at_ns: cert.expires_at_ns,
                signature: cert.signature,
            });
        }
        Ok(Self {
            certificates,
            _reservation: reservation,
        })
    }
}

#[cfg(test)]
mod tests;
