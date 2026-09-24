//! Closed owner failures for route classification. Diagnostics never define wire codes.
use tilo_relay::delivery_v3::state::ResetReason;

pub(crate) type DataResult<T> = std::result::Result<T, OwnerDataFailure>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OwnerCapacityKind {
    Bytes,
    NativeQuota,
    Disk,
    Consumer,
    ReplaySize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OwnerDataFailureKind {
    CurrentDenied,
    UnsupportedPolicy,
    UnsupportedNativeFamily,
    WriterNotSelected,
    ExactIdConflict,
    NativeSequenceConflict,
    InvalidNativeInput,
    Reset(ResetReason),
    Capacity(OwnerCapacityKind),
    Unavailable,
    DurabilityUncertain,
}
#[derive(Debug)]
pub(crate) struct OwnerDataFailure {
    kind: OwnerDataFailureKind,
    _diagnostic: anyhow::Error,
}
impl OwnerDataFailure {
    pub(crate) fn kind(&self) -> OwnerDataFailureKind {
        self.kind
    }
    pub(in crate::owner) fn new(
        kind: OwnerDataFailureKind,
        diagnostic: impl Into<anyhow::Error>,
    ) -> Self {
        Self {
            kind,
            _diagnostic: diagnostic.into(),
        }
    }
}
impl std::fmt::Display for OwnerDataFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "owner data failure: {:?}", self.kind)
    }
}
impl std::error::Error for OwnerDataFailure {}

pub(in crate::owner) fn failure(
    kind: OwnerDataFailureKind,
    diagnostic: impl Into<anyhow::Error>,
) -> anyhow::Error {
    OwnerDataFailure::new(kind, diagnostic).into()
}
pub(in crate::owner) fn require(
    condition: bool,
    kind: OwnerDataFailureKind,
    diagnostic: &'static str,
) -> anyhow::Result<()> {
    if condition {
        Ok(())
    } else {
        Err(failure(kind, anyhow::anyhow!(diagnostic)))
    }
}
pub(in crate::owner) fn admission(error: qos_peer_state::AdmissionError) -> anyhow::Error {
    use qos_peer_state::AdmissionError;
    let kind = match &error {
        AdmissionError::WriterSelectionRequired => OwnerDataFailureKind::WriterNotSelected,
        AdmissionError::ConflictingSequence => OwnerDataFailureKind::NativeSequenceConflict,
        AdmissionError::ResourceLimit => {
            OwnerDataFailureKind::Capacity(OwnerCapacityKind::NativeQuota)
        }
        AdmissionError::Rejected(_) => OwnerDataFailureKind::InvalidNativeInput,
    };
    failure(kind, error)
}
