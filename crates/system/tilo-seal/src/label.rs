//! Domain-separation labels.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Label {
    RelayEnrollOffer,
    RelayEnrollRequest,
    /// A relay enrollment response (certificate, configuration) sealed to
    /// the joining device.
    RelayEnrollResponse,
    DeviceFabricSecret,
    IdentityBackup,
    MigrationPackage,
    InviteRedemption,
    SecretVault,
    PortalSecretStore,
    PasswordBackup,
    ChannelMlsState,
    ChannelContent,
    ChannelKeyHistory,
    KeyWrap,
    CloudHostSecret,
    UserRootWrap,
    FileSecretStore,
    /// A disk-unlock sibling half held for a device, sealed to the person's
    /// secret-sync key.
    UnlockSiblingHalf,
    /// A sibling half released to one boot-time unlock request, sealed to
    /// that request's ephemeral key.
    UnlockRelease,
    SecretService,
    /// The key a process proves to that store that it holds the device
    /// identity with, when the store cannot read its executable.
    StoreCallerProof,
    /// A probe asking which of the person's own devices are on the local
    /// network: the key sealed to each device asked, and the asking
    /// device's certificate under that key.
    LanDiscoveryProbe,
    LanDiscoveryReply,
    /// A person's user key wrapped under a key stretched from their
    /// passphrase, on a machine without a TPM.
    SignInPassphrase,
    /// A person's user key wrapped under a key derived from their recovery
    /// key.
    SignInRecovery,
    /// The TPM's dictionary-attack reset authorisation, wrapped under the
    /// admin's user key or the machine's recovery key.
    TpmLockoutAuth,
    /// The signed bytes of an invitation, sealed under a key that travels in
    /// the link's fragment: what a site holds in their place.
    SealedInvitation,
    /// Content a record references, sealed under the key its reference
    /// carries.
    RecordContent,
    /// One sender's media keys, sealed to the device that may publish with
    /// them and to no other.
    SenderMediaKeys,
}

impl Label {
    pub const ALL: [Self; 29] = [
        Self::RelayEnrollOffer,
        Self::RelayEnrollRequest,
        Self::RelayEnrollResponse,
        Self::DeviceFabricSecret,
        Self::IdentityBackup,
        Self::MigrationPackage,
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
        Self::StoreCallerProof,
        Self::LanDiscoveryProbe,
        Self::LanDiscoveryReply,
        Self::SignInPassphrase,
        Self::SignInRecovery,
        Self::TpmLockoutAuth,
        Self::SealedInvitation,
        Self::RecordContent,
        Self::SenderMediaKeys,
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
            Self::MigrationPackage => "tilo-seal/v1/migration-package",
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
            Self::StoreCallerProof => "tilo-seal/v1/store-caller-proof",
            Self::LanDiscoveryProbe => "tilo-seal/v1/lan-discovery-probe",
            Self::LanDiscoveryReply => "tilo-seal/v1/lan-discovery-reply",
            Self::SignInPassphrase => "tilo-seal/v1/sign-in-passphrase",
            Self::SignInRecovery => "tilo-seal/v1/sign-in-recovery",
            Self::TpmLockoutAuth => "tilo-seal/v1/tpm-lockout-auth",
            Self::SealedInvitation => "tilo-seal/v1/sealed-invitation",
            Self::RecordContent => "tilo-seal/v1/record-content",
            Self::SenderMediaKeys => "tilo-seal/v1/sender-media-keys",
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
    fn migration_cannot_be_opened_as_an_identity_backup() {
        use crate::{Context, SymmetricKey, envelope};
        let key = SymmetricKey::generate().unwrap();
        let bytes = envelope::seal(
            &key,
            Context::new(Label::MigrationPackage, b"package"),
            b"data",
        )
        .unwrap();
        assert_eq!(
            envelope::open(
                &key,
                Context::new(Label::MigrationPackage, b"package"),
                &bytes
            )
            .unwrap()
            .as_slice(),
            b"data"
        );
        assert!(
            envelope::open(
                &key,
                Context::new(Label::IdentityBackup, b"package"),
                &bytes
            )
            .is_err()
        );
        assert!(
            envelope::open(
                &key,
                Context::new(Label::MigrationPackage, b"different-package"),
                &bytes
            )
            .is_err()
        );
        assert_eq!(
            Label::MigrationPackage.as_str(),
            "tilo-seal/v1/migration-package"
        );
    }

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
