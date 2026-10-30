//! The rows a room's governance keeps per device: its certificate and how
//! it was admitted.
use serde::{Deserialize, Serialize};
use tilo_user_identity::DeviceCert;

use super::GovernedDelegateRow;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct GovernedIdentity {
    pub(crate) device_cert: DeviceCert,
    #[serde(default)]
    pub(crate) revocations: Vec<tilo_user_identity::RevocationEntry>,
    #[serde(default)]
    pub(crate) admission: Option<RedemptionReceipt>,
    #[serde(default)]
    pub(crate) admissions: Vec<RedemptionReceipt>,
    /// Present when the device is a delegate of its person: an agent, or an
    /// installed bot ([`super::delegates`]). Absent from every other identity's
    /// bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) delegate: Option<GovernedDelegateRow>,
}
impl GovernedIdentity {
    pub(crate) fn has_admission(&self) -> bool {
        // The governor admits a bot device itself, so its installation is
        // its admission; an agent row is not, which keeps other people's
        // agents out.
        self.admission.is_some()
            || !self.admissions.is_empty()
            || self
                .delegate
                .as_ref()
                .is_some_and(|row| row.installation.is_some())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RedemptionReceipt {
    pub(crate) invite_id: [u8; 32],
    pub(crate) user_public_key: [u8; 32],
    pub(crate) device_public_key: [u8; 32],
    pub(crate) request_id: [u8; 32],
    pub(crate) role: crate::join::MemberRole,
    /// The signed invitation redeemed, in its text form
    /// ([`crate::join::encode_invitation_text`]): what lets every receiver
    /// check the admission itself, whoever wrote it. Kept once, in the
    /// room's per-invite ledger; an identity row's copy leaves it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) invitation: Option<String>,
}
impl RedemptionReceipt {
    /// The receipt as an identity row keeps it: without the invitation.
    pub(crate) fn on_row(&self) -> Self {
        Self {
            invitation: None,
            ..self.clone()
        }
    }
}
