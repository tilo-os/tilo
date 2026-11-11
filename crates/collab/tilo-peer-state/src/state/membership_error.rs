//! Why a device holds no current authority in the workspace, as the refusal
//! its caller reads.
use super::backend;
use super::delegates;

/// Why a device holds no current authority in the workspace.
///
/// Each kind is a different thing to do about it: a stranger's device is
/// unknown ([`Self::UnknownDevice`]), a removed member's is known and
/// admitted but no longer a member ([`Self::NoMembership`]), and a device the
/// workspace revoked must stop, not retry ([`Self::Revoked`]).
#[derive(Clone, Debug, thiserror::Error)]
pub enum MembershipError {
    /// The workspace holds no certificate for the device.
    #[error("certified identity missing")]
    UnknownDevice,
    /// The device's certificate does not verify, or has run out.
    #[error(transparent)]
    Certificate(tilo_user_identity::DeviceCertError),
    /// The workspace's root has revoked the device.
    #[error("device revoked by workspace")]
    Revoked,
    /// The workspace's governance is not here to decide against.
    #[error("workspace governance missing")]
    GovernanceMissing,
    /// The governance document carries no row for the device.
    #[error("device is absent from governed roster")]
    Absent,
    /// The row the workspace holds is another certificate than the device's.
    #[error("governed certificate mismatch")]
    CertificateMismatch,
    /// The revocations in the governance document are not this workspace's root's.
    #[error("revocation root mismatch")]
    RevocationRoot,
    /// A revocation entry does not verify.
    #[error(transparent)]
    Revocation(tilo_user_identity::RevocationError),
    /// The device's delegation is withdrawn, expired or names another.
    #[error(transparent)]
    Delegate(delegates::DelegateInactive),
    /// The device's person is a stranger here, or the workspace blocks them.
    #[error("person has no workspace membership")]
    NoMembership,
    /// The workspace's rows for this person's access do not decode, so
    /// whether they are a member cannot be told: not the same as being none,
    /// and the person is owed the difference.
    #[error("this person's access rows do not read back")]
    UnreadableAccess,
    /// The governance document's row for the device does not decode.
    #[error("the governed identity row does not decode")]
    MalformedIdentity(#[source] std::sync::Arc<crate::state::RowError>),
}
impl From<backend::CertificateLookupError> for MembershipError {
    fn from(error: backend::CertificateLookupError) -> Self {
        match error {
            backend::CertificateLookupError::Missing => Self::UnknownDevice,
            backend::CertificateLookupError::Validity(error) => Self::Certificate(error),
            backend::CertificateLookupError::Revoked => Self::Revoked,
        }
    }
}
