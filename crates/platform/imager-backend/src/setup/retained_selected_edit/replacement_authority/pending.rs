//! Constructor-private continuity from the actual reserved source capability.
//! Receipt decoding cannot construct either owner.
use super::*;
use crate::setup::retained_source_relay::selected_parent::SourceSelectedParent;
use runtime_fs::{
    EntryName,
    directory_backend::{
        PendingReplacementObservation, ReplacementObservation, WorkspacePendingReplacementBackend,
        WorkspaceReplacementPinBackend,
    },
    external_history::{CommittedReceipt, PreparedReceipt},
};
use std::sync::Weak;

pub struct PendingSelectedReplacement {
    source: Weak<SourceSelectedParent>,
    seed: Arc<RootSelectedReplacementSeed>,
    backend: Arc<dyn WorkspacePendingReplacementBackend>,
    observation: PendingReplacementObservation,
}
pub struct AcknowledgedSelectedReplacement {
    pending: Arc<PendingSelectedReplacement>,
    pin: Arc<dyn WorkspaceReplacementPinBackend>,
    observation: ReplacementObservation,
    prepared: PreparedReceipt,
    committed: CommittedReceipt,
}
impl RootSelectedReplacementSeed {
    pub(in crate::setup) fn reserve(
        self: &Arc<Self>,
        source: &Arc<SourceSelectedParent>,
        owned_temporary: &EntryName,
    ) -> io::Result<Arc<PendingSelectedReplacement>> {
        self.check()?;
        source.check_authority().map_err(io::Error::other)?;
        if !self.uses_input(source.input())
            || !Arc::ptr_eq(self, source.replacement_seed())
            || source.parent_identity() != self.record.parent
            || source
                .components()
                .iter()
                .map(|v| v.as_os_str().as_encoded_bytes().to_vec())
                .collect::<Vec<_>>()
                != self.record.components
        {
            return Err(refused());
        }
        // This source-private factory checks its genuinely completed CreateNew
        // capability; a name or caller implementation cannot mint reservation.
        let backend = source
            .prepare_owned_replacement(owned_temporary)
            .map_err(io::Error::other)?;
        backend.check().map_err(io::Error::other)?;
        let observation = backend.observe().map_err(io::Error::other)?;
        observation.validate().map_err(io::Error::other)?;
        if observation.purpose != runtime_fs::directory_backend::ReplacementPurpose::AtomicSave
            || observation.source_owner != self.record.source_owner
            || observation.expected_victim
                != source
                    .current_selected_identity()
                    .map_err(io::Error::other)?
            || observation.source[0] != self.record.source_root[0]
        {
            return Err(refused());
        }
        let result = Arc::new(PendingSelectedReplacement {
            source: Arc::downgrade(source),
            seed: self.clone(),
            backend,
            observation,
        });
        result.check_custody(source).map_err(io::Error::other)?;
        Ok(result)
    }
}
impl PendingSelectedReplacement {
    pub fn observation(&self) -> &PendingReplacementObservation {
        &self.observation
    }
    pub fn check_custody(&self, source: &SourceSelectedParent) -> Result<(), String> {
        self.seed.check().map_err(|e| e.to_string())?;
        let exact = self
            .source
            .upgrade()
            .ok_or_else(|| "Selected source retired".to_string())?;
        if !std::ptr::eq(Arc::as_ptr(&exact), source)
            || !self.seed.uses_input(source.input())
            || source.parent_identity() != self.seed.record.parent
        {
            return Err("Selected replacement source mismatch".into());
        }
        // Nonrecursive: the lineage gate itself calls this while holding state.
        source.check_authority()?;
        self.backend.check()?;
        if self.backend.observe()? != self.observation {
            return Err("Reserved source intent changed".into());
        }
        self.seed.check().map_err(|e| e.to_string())
    }
    pub fn backend(&self) -> &Arc<dyn WorkspacePendingReplacementBackend> {
        &self.backend
    }
    pub(super) fn seed(&self) -> &Arc<RootSelectedReplacementSeed> {
        &self.seed
    }
    pub(super) fn acknowledged(
        self: &Arc<Self>,
        prepared: PreparedReceipt,
        committed: CommittedReceipt,
    ) -> io::Result<AcknowledgedSelectedReplacement> {
        self.seed.check()?;
        self.seed
            .daemon
            .verify_replacement(&self.observation, &prepared, &committed)?;
        // ACK @5 is served ONLY by this exact submitted NativeJob after actual
        // signed sidecar/native ownership verification, not a DTO pin factory.
        let pin = self
            .backend
            .acknowledged_committed_pin()
            .map_err(io::Error::other)?;
        pin.check().map_err(io::Error::other)?;
        let observation = pin.observe().map_err(io::Error::other)?;
        if observation.operation != self.observation.operation
            || observation.source_owner != self.observation.source_owner
            || observation.source != self.observation.source
            || observation.victim != self.observation.expected_victim
            || observation.destination != self.observation.source
            || observation.capture
                != *uuid::Uuid::parse_str(&committed.capture_id)
                    .map_err(io::Error::other)?
                    .as_bytes()
            || observation.destination != committed.destination_identity
            || observation.victim != committed.victim_identity
            || observation.source != committed.source_identity
        {
            return Err(refused());
        }
        let proof = AcknowledgedSelectedReplacement {
            pending: self.clone(),
            pin,
            observation,
            prepared,
            committed,
        };
        let source = self.source.upgrade().ok_or_else(refused)?;
        proof.check_custody(&source).map_err(io::Error::other)?;
        Ok(proof)
    }
}
impl AcknowledgedSelectedReplacement {
    pub fn pending(&self) -> &Arc<PendingSelectedReplacement> {
        &self.pending
    }
    pub fn observation(&self) -> &ReplacementObservation {
        &self.observation
    }
    pub fn check_custody(&self, source: &SourceSelectedParent) -> Result<(), String> {
        self.pending.check_custody(source)?;
        self.pending
            .seed
            .daemon
            .verify_replacement(self.pending.observation(), &self.prepared, &self.committed)
            .map_err(|e| e.to_string())?;
        self.pin.check()?;
        if self.pin.observe()? != self.observation {
            return Err("Acknowledged replacement pin changed".into());
        }
        self.pending.check_custody(source)
    }
}

impl AcknowledgedSelectedReplacement {
    /// Only opaque independently checked ACK owners may request retirement.
    pub fn retire_before(&self, successor: &Self, source: &SourceSelectedParent) -> io::Result<()> {
        self.check_custody(source).map_err(io::Error::other)?;
        successor.check_custody(source).map_err(io::Error::other)?;
        if !Arc::ptr_eq(self.pending.seed(), successor.pending.seed())
            || self.observation.operation == successor.observation.operation
            || self.observation.source_owner != successor.observation.source_owner
        {
            return Err(refused());
        }
        self.pending
            .seed
            .daemon
            .retire_replacement(self, successor)?;
        successor.check_custody(source).map_err(io::Error::other)
    }
    /// Cache teardown only. Call AFTER daemon retirement and source RPC join;
    /// the current successor proof supplies authority checks, because retiring
    /// this exact old map entry intentionally invalidates old capability checks.
    pub fn retire_pin_controller_cache(&self) -> Result<(), String> {
        self.pin.retire_controller_cache()
    }
    pub fn retire_pending_controller_cache(&self) -> Result<(), String> {
        self.pending.backend.retire_controller_cache()
    }
    pub(super) fn verify_retirement_receipt(
        &self,
        daemon: &super::CertifiedNativeHistoryDaemon,
    ) -> io::Result<()> {
        if !std::ptr::eq(daemon, Arc::as_ptr(&self.pending.seed.daemon)) {
            return Err(refused());
        }
        daemon.verify_replacement(self.pending.observation(), &self.prepared, &self.committed)
    }
    pub(super) fn retirement_binding(&self) -> (&str, [u8; 32]) {
        (
            &self.committed.operation_id,
            self.committed.transaction_digest,
        )
    }
}
