//! Explicit serialized owner scopes. These do not qualify native peer storage.
use super::*;
use crate::committed_authority::CommittedAuthority;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OwnerOperation {
    Enrollment { input_len: usize },
    Join { input_len: usize },
    MediaAdministration { input_len: usize },
    HostSigning { input_len: usize },
    WorkerCommit { input_len: usize },
    LegacyReceive { input_len: usize },
    Recovery { input_len: usize },
    Descriptor,
    Checkpoint,
    Flush,
    Export,
    DeliveryActivation,
}

/// Holds serialized custody across the synchronous Current/owner publication
/// and, if the caller needs it, subsequent asynchronous transport reconciliation.
/// Neither this token nor successful execution qualifies native allocations.
pub(crate) struct OwnerScope {
    retained: Arc<RetainedSerializedBytes>,
    generation: u64,
    operation: OwnerOperation,
    _scratch: ByteCredit,
    active: Arc<ActiveTurn>,
    used: bool,
    pending_input: Option<ByteCredit>,
}
// A panic can leave a volatile deferred queue behind while Current closes.
// Retain the input credit on that path; ordinary no-growth callbacks release it.
struct PendingInputCustody {
    retained: Arc<RetainedSerializedBytes>,
    credit: Option<ByteCredit>,
}
impl PendingInputCustody {
    fn finish(mut self, retained: bool) {
        if !retained {
            drop(self.credit.take());
        }
    }
}
impl Drop for PendingInputCustody {
    fn drop(&mut self) {
        if let Some(credit) = self.credit.take() {
            self.retained.push_pending(credit);
        }
    }
}
impl OwnerOperation {
    fn input_len(self) -> DataResult<usize> {
        let (length, limit) = match self {
            Self::Enrollment { input_len } => (input_len, 16 * 1024),
            Self::Join { input_len } | Self::MediaAdministration { input_len } => {
                (input_len, 256 * 1024)
            }
            Self::HostSigning { input_len } | Self::WorkerCommit { input_len } => {
                (input_len, 4 * 1024 * 1024)
            }
            Self::LegacyReceive { input_len } | Self::Recovery { input_len } => {
                (input_len, crate::store::MAX_AGGREGATE_BYTES)
            }
            _ => return Ok(0),
        };
        if length == 0 || length > limit {
            return Err(OwnerDataFailure::new(
                Failure::InvalidNativeInput,
                anyhow::anyhow!("owner operation input exceeds its protocol bound"),
            ));
        }
        Ok(length)
    }
}
impl OwnerScope {
    pub(crate) fn operation(&self) -> OwnerOperation {
        self.operation
    }
}
impl SessionOwner {
    pub(crate) fn prepare_owner_scope(
        &self,
        pool: &HostBytes,
        operation: OwnerOperation,
    ) -> DataResult<OwnerScope> {
        if !self.durability_healthy() {
            return Err(OwnerDataFailure::new(
                Failure::Unavailable,
                anyhow::anyhow!("owner requires explicit reopen"),
            ));
        }
        self.aggregate
            .require_writable()
            .map_err(|e| self.classify_data_error(e))?;
        let input = operation.input_len()?;
        let resources = self
            .resources
            .as_ref()
            .filter(|r| r.budget.same_pool(pool))
            .ok_or_else(|| {
                OwnerDataFailure::new(
                    Failure::Unavailable,
                    anyhow::anyhow!("owner scope requires attached original pool"),
                )
            })?;
        let current = schema::signed_size(
            self.aggregate
                .encoded_payload_len()
                .map_err(|e| self.classify_data_error(e))?,
        )
        .map_err(|e| self.classify_data_error(e))?;
        let unchanged = matches!(operation, OwnerOperation::Flush | OwnerOperation::Export);
        let aggregate = if unchanged {
            current
        } else {
            add(add(multiply(current, 2)?, multiply(input, 2)?)?, 512 * 1024)?
                .min(crate::store::MAX_AGGREGATE_BYTES)
        };
        self.store.preflight(aggregate).map_err(disk)?;
        let raw = common_raw_capacity(&self.aggregate).map_err(|e| self.classify_data_error(e))?;
        // Same conservative serialized simultaneous-copy contract as data turns.
        // This does not account a StatePeer clone or a serde/native decode.
        let transient = add(
            add(multiply(aggregate, 6)?, multiply(raw, 2)?)?,
            add(multiply(POLICY_BYTES, 3)?, multiply(input, 2)?)?,
        )?;
        let retained = if unchanged {
            0
        } else {
            add(
                add(multiply(aggregate, 2)?, raw)?,
                add(MAX_PUBLICATION_TAIL_BYTES, input)?,
            )?
        };
        let mut credit = pool
            .try_acquire_bulk(add(transient, retained)?)
            .ok_or_else(capacity)?;
        let mut retained_allowance = credit.split(retained).ok_or_else(capacity)?;
        let pending_input = if matches!(
            operation,
            OwnerOperation::LegacyReceive { .. } | OwnerOperation::Recovery { .. }
        ) {
            Some(retained_allowance.split(input).ok_or_else(capacity)?)
        } else {
            None
        };
        Ok(OwnerScope {
            retained: resources.retained.clone(),
            generation: self.committed_generation(),
            operation,
            _scratch: credit,
            active: Arc::new(ActiveTurn {
                cap: aggregate,
                retained_allowance: Mutex::new(retained_allowance),
                staged: Mutex::new(StagedCredits::default()),
            }),
            used: false,
            pending_input,
        })
    }

    fn enter_owner_scope(&self, scope: &mut OwnerScope) -> DataResult<ActiveReset> {
        if !self.durability_healthy() {
            return Err(OwnerDataFailure::new(
                Failure::Unavailable,
                anyhow::anyhow!("owner durability unavailable"),
            ));
        }
        let resources = self
            .resources
            .as_ref()
            .filter(|r| Arc::ptr_eq(&r.retained, &scope.retained))
            .ok_or_else(|| {
                OwnerDataFailure::new(
                    Failure::CurrentDenied,
                    anyhow::anyhow!("owner scope belongs to another owner"),
                )
            })?;
        if scope.used || scope.generation != self.committed_generation() {
            return Err(OwnerDataFailure::new(
                Failure::CurrentDenied,
                anyhow::anyhow!("owner scope used or generation changed"),
            ));
        }
        let active = resources.active.clone();
        {
            let mut slot = active.lock().map_err(|_| {
                OwnerDataFailure::new(
                    Failure::Unavailable,
                    anyhow::anyhow!("owner scope poisoned"),
                )
            })?;
            if slot.is_some() {
                return Err(OwnerDataFailure::new(
                    Failure::Unavailable,
                    anyhow::anyhow!("nested owner operation"),
                ));
            }
            *slot = Some(scope.active.clone());
        }
        scope.used = true;
        Ok(ActiveReset(active))
    }

    pub(crate) fn with_owner_scope<T>(
        &mut self,
        scope: &mut OwnerScope,
        current: &CommittedAuthority,
        callback: impl FnOnce(&mut Self) -> T,
    ) -> DataResult<T> {
        let _active = self.enter_owner_scope(scope)?;
        let pending = PendingInputCustody {
            retained: scope.retained.clone(),
            credit: scope.pending_input.take(),
        };
        let before = self.peer.pending_count();
        let result = current.with_owner(self, callback);
        pending.finish(self.peer.pending_count() > before);
        self.reconcile_pending_input_credits();
        result.map_err(|e| self.classify_data_error(e))
    }
    pub(crate) fn with_owner_view_scope<T>(
        &mut self,
        scope: &mut OwnerScope,
        current: &CommittedAuthority,
        callback: impl FnOnce(&CommittedView, &mut Self) -> T,
    ) -> DataResult<T> {
        let _active = self.enter_owner_scope(scope)?;
        let pending = PendingInputCustody {
            retained: scope.retained.clone(),
            credit: scope.pending_input.take(),
        };
        let before = self.peer.pending_count();
        let result = current.with_owner_view(self, callback);
        pending.finish(self.peer.pending_count() > before);
        self.reconcile_pending_input_credits();
        result.map_err(|e| self.classify_data_error(e))
    }

    /// Bind disk work to the actual owner root and workspace; this is not a
    /// session authorization or native resource qualification witness.
    pub(crate) fn staging_directory(
        &self,
        inventory: &crate::store::HostWorkInventory,
    ) -> DataResult<crate::store::StagingDirectory> {
        if !self.durability_healthy() {
            return Err(OwnerDataFailure::new(
                Failure::Unavailable,
                anyhow::anyhow!("owner requires reopen"),
            ));
        }
        if !self
            .resources
            .as_ref()
            .is_some_and(|r| inventory.same_pool(&r.budget))
        {
            return Err(OwnerDataFailure::new(
                Failure::Unavailable,
                anyhow::anyhow!("staging requires original attached host inventory"),
            ));
        }
        self.store
            .staging_directory(inventory, self.workspace_id)
            .map_err(|error| {
                OwnerDataFailure::new(
                    match error {
                        crate::store::HostWorkFailure::Capacity => {
                            Failure::Capacity(Capacity::Disk)
                        }
                        crate::store::HostWorkFailure::Conflict => Failure::CurrentDenied,
                        crate::store::HostWorkFailure::Unavailable => Failure::Unavailable,
                    },
                    error,
                )
            })
    }

    /// Early defense for direct legacy entry points after resource attachment.
    /// The installed host must reserve before caller-side context/decoding too.
    pub(crate) fn require_owner_resource_scope(&self) -> Result<()> {
        self.serialized_commit_cap().map(|_| ())
    }
}
#[cfg(test)]
#[path = "scopes/tests.rs"]
mod tests;
