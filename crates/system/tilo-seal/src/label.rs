//! Domain-separation labels.
//!
//! Every sealed value is bound to exactly one [`Label`]: the HPKE `info`
//! string for public-key sealing, and part of the authenticated data for
//! at-rest envelopes. A ciphertext produced for one purpose therefore never
//! opens under another, even with the same key. The registry is closed on
//! purpose: a new use of sealing adds a variant here, where it is reviewed
//! next to every other use.

/// Registered purposes for sealed data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Label {
    /// A relay enrollment offer sealed to the soliciting device.
    RelayEnrollOffer,
    /// A relay enrollment request (bearer token) sealed to the issuer.
    RelayEnrollRequest,
    /// A relay enrollment response (certificate, configuration) sealed to
    /// the joining device.
    RelayEnrollResponse,
    /// A secret sealed to one device of the personal device fabric.
    DeviceFabricSecret,
    /// A password-protected portable identity backup.
    IdentityBackup,
    /// A delegation secret held by the invite-redemption authority.
    InviteRedemption,
    /// A sealed local secret vault (accounts, authenticator, ...).
    SecretVault,
    /// The desktop portal's per-application secret store.
    PortalSecretStore,
    /// The password manager's encrypted export.
    PasswordBackup,
    /// Persisted `OpenMLS` group state of one channel member.
    ChannelMlsState,
    /// A collaboration op or snapshot encrypted under a channel content key.
    ChannelContent,
    /// Channel content-key history handed to a joining member.
    ChannelKeyHistory,
    /// A symmetric key wrapped under a key-encryption key.
    KeyWrap,
    /// A Cloud host's signing secret, sealed to its machine.
    CloudHostSecret,
    /// A user root key wrapped in a browser (passkey PRF or recovery key).
    UserRootWrap,
    /// One app's secret in a machine's file-backed secret store.
    FileSecretStore,
    /// A disk-unlock sibling half held for a device, sealed to the person's
    /// secret-sync key.
    UnlockSiblingHalf,
    /// A sibling half released to one boot-time unlock request, sealed to
    /// that request's ephemeral key.
    UnlockRelease,
    /// The person's Secret Service store (`org.freedesktop.secrets`).
    SecretService,
}

impl Label {
    /// Every registered label, for uniqueness checks and documentation.
    pub const ALL: [Self; 19] = [
        Self::RelayEnrollOffer,
        Self::RelayEnrollRequest,
        Self::RelayEnrollResponse,
        Self::DeviceFabricSecret,
        Self::IdentityBackup,
        Self::InviteRedemption,
        Self::SecretVault,
        Self::PortalSecretStore,
        Self::PasswordBackup,
        Self::ChannelMlsState,
        Self::ChannelContent,
        Self::ChannelKeyHistory,
        Self::KeyWrap,
        Self::CloudHostSecret,
        Self::UserRootWrap,
        Self::FileSecretStore,
        Self::UnlockSiblingHalf,
        Self::UnlockRelease,
        Self::SecretService,
    ];

    /// The stable byte string bound into the cryptography. Changing one of
    /// these makes every existing ciphertext for that purpose unreadable.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RelayEnrollOffer => "tilo-seal/v1/relay-enroll-offer",
            Self::RelayEnrollRequest => "tilo-seal/v1/relay-enroll-request",
            Self::RelayEnrollResponse => "tilo-seal/v1/relay-enroll-response",
            Self::DeviceFabricSecret => "tilo-seal/v1/device-fabric-secret",
            Self::IdentityBackup => "tilo-seal/v1/identity-backup",
            Self::InviteRedemption => "tilo-seal/v1/invite-redemption",
            Self::SecretVault => "tilo-seal/v1/secret-vault",
            Self::PortalSecretStore => "tilo-seal/v1/portal-secret-store",
            Self::PasswordBackup => "tilo-seal/v1/password-backup",
            Self::ChannelMlsState => "tilo-seal/v1/channel-mls-state",
            Self::ChannelContent => "tilo-seal/v1/channel-content",
            Self::ChannelKeyHistory => "tilo-seal/v1/channel-key-history",
            Self::KeyWrap => "tilo-seal/v1/key-wrap",
            Self::CloudHostSecret => "tilo-seal/v1/cloud-host-secret",
            Self::UserRootWrap => "tilo-seal/v1/user-root-wrap",
            Self::FileSecretStore => "tilo-seal/v1/file-secret-store",
            Self::UnlockSiblingHalf => "tilo-seal/v1/unlock-sibling-half",
            Self::UnlockRelease => "tilo-seal/v1/unlock-release",
            Self::SecretService => "tilo-seal/v1/secret-service",
        }
    }

    pub(crate) const fn as_bytes(self) -> &'static [u8] {
        self.as_str().as_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::Label;

    #[test]
    fn labels_are_unique_and_prefix_free() {
        for (i, a) in Label::ALL.iter().enumerate() {
            for b in Label::ALL.iter().skip(i + 1) {
                assert_ne!(a, b);
                assert!(!a.as_str().starts_with(b.as_str()));
                assert!(!b.as_str().starts_with(a.as_str()));
            }
        }
    }
}
