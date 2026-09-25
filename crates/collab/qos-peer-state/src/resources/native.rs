//! Native storage primitives for the private bounded peer backend.
//! Checked native allocation is separate from certified state admission.
use automerge::resource_limits::{
    NativeAllocationSet, NativeLimits, NativeOwned, NativeResourceError, NativeSaved,
    NativeWorkTurn,
};
use automerge::{AutoCommit, SaveOptions};

pub(crate) struct NativeBatch<'a> {
    allocations: &'a mut NativeAllocationSet,
    turn: &'a NativeWorkTurn,
    remaining_normalized: NormalizedRemaining<'a>,
}
enum NormalizedRemaining<'a> {
    Owned(usize),
    Continuing(&'a mut usize),
}
impl NormalizedRemaining<'_> {
    fn get(&mut self) -> &mut usize {
        match self {
            Self::Owned(value) => value,
            Self::Continuing(value) => value,
        }
    }
}
impl<'a> NativeBatch<'a> {
    pub(crate) fn new(
        allocations: &'a mut NativeAllocationSet,
        turn: &'a NativeWorkTurn,
        maximum_normalized: usize,
    ) -> Result<Self, NativeResourceError> {
        if maximum_normalized > 8 * 1024 * 1024 {
            return Err(NativeResourceError::NormalizedBytes);
        }
        Ok(Self {
            allocations,
            turn,
            remaining_normalized: NormalizedRemaining::Owned(maximum_normalized),
        })
    }
    /// Reborrows the embedding operation's one continuing counter. Releasing
    /// this batch never refunds work or resets the aggregate input ceiling.
    pub(crate) fn continuing(
        allocations: &'a mut NativeAllocationSet,
        turn: &'a NativeWorkTurn,
        remaining_normalized: &'a mut usize,
    ) -> Result<Self, NativeResourceError> {
        if *remaining_normalized > 8 * 1024 * 1024 {
            return Err(NativeResourceError::NormalizedBytes);
        }
        Ok(Self {
            allocations,
            turn,
            remaining_normalized: NormalizedRemaining::Continuing(remaining_normalized),
        })
    }
    pub(crate) fn load(&mut self, bytes: &[u8]) -> Result<NativeDocument, NativeResourceError> {
        // Apply the remaining aggregate ceiling BEFORE inspection/inflation.
        // Normalization uses this turn's exact ledger by construction; neither
        // caller-supplied alternate allowances nor per-document resets exist.
        let limits = NativeLimits {
            max_normalized_bytes: *self.remaining_normalized.get(),
            ..NativeLimits::default()
        };
        let normalized = self.turn.normalize_input(bytes, &limits)?;
        let remaining = self.remaining_normalized.get();
        *remaining = remaining
            .checked_sub(normalized.shape().normalized_bytes())
            .ok_or(NativeResourceError::NormalizedBytes)?;
        let document = normalized.reconstruct(self.allocations, self.turn)?;
        Ok(NativeDocument { document })
    }
}
/// No Clone, Deref, extraction or raw AutoCommit access. Custody is native.
pub(crate) struct NativeDocument {
    document: NativeOwned<AutoCommit>,
}
impl NativeDocument {
    pub(crate) fn read<'doc, 'turn>(
        &'doc self,
        turn: &'turn NativeWorkTurn,
    ) -> Result<automerge::resource_limits::NativeReadSession<'doc, 'turn>, NativeResourceError>
    {
        self.document.read(turn)
    }
    pub(crate) fn heads(
        &self,
        turn: &NativeWorkTurn,
    ) -> Result<automerge::resource_limits::NativeHashes, NativeResourceError> {
        self.document.base_heads_bounded(turn)
    }
    pub(crate) fn changes_since(
        &self,
        heads: &[automerge::ChangeHash],
        turn: &NativeWorkTurn,
    ) -> Result<automerge::resource_limits::NativeChanges, NativeResourceError> {
        self.document.changes_since_bounded(heads, turn)
    }
    pub(crate) fn has_missing_dependencies(&self) -> bool {
        self.document.has_missing_dependencies()
    }
    pub(crate) fn candidate(
        &self,
        allocations: &mut NativeAllocationSet,
        turn: &NativeWorkTurn,
    ) -> Result<Self, NativeResourceError> {
        Ok(Self {
            document: self.document.clone_bounded(allocations, turn)?,
        })
    }
    pub(crate) fn capture(
        &self,
        maximum_output_bytes: usize,
        turn: &NativeWorkTurn,
    ) -> Result<NativeCapture, NativeResourceError> {
        Ok(NativeCapture {
            saved: self.document.save_bounded_with_limit(
                turn,
                SaveOptions {
                    deflate: true,
                    retain_orphans: true,
                },
                maximum_output_bytes,
            )?,
        })
    }
}
pub(crate) struct NativeCapture {
    saved: NativeSaved,
}
impl NativeCapture {
    pub(crate) fn bytes(&self) -> &[u8] {
        self.saved.bytes()
    }
}
#[cfg(test)]
mod tests;
