//! Completed private staging remains inert until a fresh real owner turn admits it.
use super::*;
use crate::store::{HostWorkFailure, HostWorkInventory};
impl SessionOwner {
    pub(crate) fn prepare_staged_turn(
        &self,
        pool: &HostBytes,
        inventory: &HostWorkInventory,
        input_len: usize,
    ) -> DataResult<OwnerResourceTurn> {
        if !inventory.same_pool(pool)
            || !self
                .resources
                .as_ref()
                .is_some_and(|r| r.budget.same_pool(pool))
        {
            return Err(OwnerDataFailure::new(
                Failure::CurrentDenied,
                anyhow::anyhow!("staged input requires original owner and ingress pool"),
            ));
        }
        if input_len == 0 || input_len > 64 * 1024 * 1024 {
            return Err(OwnerDataFailure::new(
                Failure::InvalidNativeInput,
                anyhow::anyhow!("staged canonical length outside protocol bound"),
            ));
        }
        // Acquired before policy/context/file/native work; actual token custody
        // survives the synchronous commit and Current publication until caller drop.
        let large = inventory.try_large_decode().map_err(host_failure)?;
        self.prepare_data_turn_inner(
            pool,
            OwnerDataOperation::StateStaged { input_len },
            Some(large),
        )
    }
}
impl SessionOwner {
    /// Idle staging of one device in this owner's workspace, each with a fresh
    /// exclusive reservation so the caller can inspect and reclaim it.
    pub(crate) fn idle_upload_stages(
        &self,
        inventory: &HostWorkInventory,
        device: [u8; 32],
    ) -> Result<Vec<crate::store::StagingReservation>, HostWorkFailure> {
        let directory = self.store.staging_directory(inventory, self.workspace_id)?;
        directory
            .idle_stages(device)?
            .into_iter()
            .map(|identity| directory.reserve(identity))
            .collect()
    }
    /// Inert disk custody for one resumable upload in this owner's workspace.
    /// A reservation or completed prefix never implies admission or authority.
    pub(crate) fn reserve_upload_staging(
        &self,
        inventory: &HostWorkInventory,
        identity: crate::staging_manifest::StageIdentity,
    ) -> Result<crate::store::StagingReservation, HostWorkFailure> {
        if identity.workspace != self.workspace_id {
            return Err(HostWorkFailure::Conflict);
        }
        self.store.reserve_staging(inventory, identity)
    }
}
fn host_failure(error: HostWorkFailure) -> OwnerDataFailure {
    let kind = match error {
        HostWorkFailure::Capacity => Failure::Capacity(Capacity::NativeQuota),
        HostWorkFailure::Conflict => Failure::CurrentDenied,
        HostWorkFailure::Unavailable => Failure::Unavailable,
    };
    OwnerDataFailure::new(kind, error)
}

// Field order ensures every exit closes the original file before its lease and
// worker credit. Inert staging quota persists until explicit scoped cleanup.
struct StagedBody {
    file: std::fs::File,
    reservation: crate::store::StagingReservation,
    credit: ByteCredit,
}
impl SessionOwner {
    pub(crate) fn data_admit_staged(
        &mut self,
        turn: &mut OwnerResourceTurn,
        source: &VerifiedDeliverySource<'_>,
        completed: crate::delivery_http::upload::CompletedUpload,
        now: SystemTime,
    ) -> DataResult<(AdmissionOutcome, crate::store::StagingReservation)> {
        use std::io::{Read, Seek, SeekFrom};
        let (file, identity, reservation, credit) = completed.into_parts();
        let mut body = StagedBody {
            file,
            reservation,
            credit,
        };
        let input_len = usize::try_from(identity.total).map_err(|_| capacity())?;
        self.validate_data_turn(turn, Some(OwnerDataOperation::StateStaged { input_len }))?;
        if !turn
            ._large_decode
            .as_ref()
            .is_some_and(|lease| body.reservation.same_decode_inventory(lease))
            || !self
                .resources
                .as_ref()
                .is_some_and(|r| body.reservation.same_pool(&r.budget))
        {
            return Err(OwnerDataFailure::new(
                Failure::CurrentDenied,
                anyhow::anyhow!("staged file lacks original shared decode custody"),
            ));
        }
        self.validate_staged_delivery_source(source, &identity)
            .map_err(|e| self.classify_data_error(e))?;
        let now_ns: u64 = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|e| OwnerDataFailure::new(Failure::CurrentDenied, e))?
            .as_nanos()
            .try_into()
            .map_err(|_| capacity())?;
        body.reservation
            .validate_completed_file(&self.store, self.workspace_id, &body.file, now_ns)
            .map_err(host_failure)?;
        // The real turn already retained both input-copy peaks before this
        // allocation. This wire buffer is not a decoded native-state qualifier.
        let mut canonical = vec![0; input_len];
        if canonical.capacity() != input_len {
            return Err(capacity());
        }
        body.file
            .seek(SeekFrom::Start(0))
            .map_err(|e| OwnerDataFailure::new(Failure::Unavailable, e))?;
        body.file
            .read_exact(&mut canonical)
            .map_err(|e| OwnerDataFailure::new(Failure::InvalidNativeInput, e))?;
        if *blake3::hash(&canonical).as_bytes() != identity.digest {
            return Err(OwnerDataFailure::new(
                Failure::InvalidNativeInput,
                anyhow::anyhow!("staged canonical bytes changed after completion"),
            ));
        }
        body.reservation
            .validate_completed_file(&self.store, self.workspace_id, &body.file, now_ns)
            .map_err(host_failure)?;
        let outcome = self.with_data_operation(
            turn,
            OwnerDataOperation::StateStaged { input_len },
            |owner| {
                let credit = owner.split_retained_bytes(input_len)?;
                let before = owner.peer.pending_count();
                let result = owner.admit_staged_delivery(
                    source,
                    CompleteStateItem {
                        delivery_id: identity.delivery,
                        canonical: &canonical,
                    },
                    now,
                )?;
                if owner.peer.pending_count() > before {
                    if let Some(credit) = credit {
                        owner.store_pending_input_credit(credit);
                    }
                }
                Ok(result)
            },
        )?;
        drop(canonical);
        let StagedBody {
            file,
            reservation,
            credit,
        } = body;
        drop(file);
        drop(credit);
        Ok((outcome, reservation))
    }
}
