//! Domain-separation labels.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Label {
    ServerEnrollOffer,
    ServerEnrollRequest,
    /// A server enrollment response (certificate, configuration) sealed to
    /// the joining device.
    ServerEnrollResponse,
    DeviceFabricSecret,
    IdentityBackup,
    MigrationPackage,
    InviteRedemption,
    SecretVault,
    PasswordBackup,
    GroupMlsState,
    GroupContent,
    GroupKeyHistory,
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
    /// The key a process proves to the person's session that it holds the
    /// device identity with, when the session cannot look at the process.
    SessionCallerProof,
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
    /// Content a record references, sealed under the key its reference
    /// carries.
    DocumentContent,
    /// One sender's media keys, sealed to the device that may publish with
    /// them and to no other.
    SenderMediaKeys,
    /// A text a person's vault released for one site's page, sealed to the
    /// device that holds that page's window and to no other.
    ReleasedPageText,
    /// The private half of a person's rollback floor key, sealed to their
    /// secret-sync key.
    RollbackFloorKey,
}

impl Label {
    pub const ALL: [Self; 30] = [
        Self::ServerEnrollOffer,
        Self::ServerEnrollRequest,
        Self::ServerEnrollResponse,
        Self::DeviceFabricSecret,
        Self::IdentityBackup,
        Self::MigrationPackage,
        Self::InviteRedemption,
        Self::SecretVault,
        Self::PasswordBackup,
        Self::GroupMlsState,
        Self::GroupContent,
        Self::GroupKeyHistory,
        Self::KeyWrap,
        Self::CloudHostSecret,
        Self::UserRootWrap,
        Self::FileSecretStore,
        Self::UnlockSiblingHalf,
        Self::UnlockRelease,
        Self::SecretService,
        Self::StoreCallerProof,
        Self::SessionCallerProof,
        Self::LanDiscoveryProbe,
        Self::LanDiscoveryReply,
        Self::SignInPassphrase,
        Self::SignInRecovery,
        Self::TpmLockoutAuth,
        Self::DocumentContent,
        Self::SenderMediaKeys,
        Self::ReleasedPageText,
        Self::RollbackFloorKey,
    ];

    /// The stable byte string bound into the cryptography. Changing one of
    /// these makes every existing ciphertext for that purpose unreadable.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ServerEnrollOffer => "tilo-seal/v1/server-enroll-offer",
            Self::ServerEnrollRequest => "tilo-seal/v1/server-enroll-request",
            Self::ServerEnrollResponse => "tilo-seal/v1/server-enroll-response",
            Self::DeviceFabricSecret => "tilo-seal/v1/device-fabric-secret",
            Self::IdentityBackup => "tilo-seal/v1/identity-backup",
            Self::MigrationPackage => "tilo-seal/v1/migration-package",
            Self::InviteRedemption => "tilo-seal/v1/invite-redemption",
            Self::SecretVault => "tilo-seal/v1/secret-vault",
            Self::PasswordBackup => "tilo-seal/v1/password-backup",
            Self::GroupMlsState => "tilo-seal/v1/channel-mls-state",
            Self::GroupContent => "tilo-seal/v1/channel-content",
            Self::GroupKeyHistory => "tilo-seal/v1/channel-key-history",
            Self::KeyWrap => "tilo-seal/v1/key-wrap",
            Self::CloudHostSecret => "tilo-seal/v1/cloud-host-secret",
            Self::UserRootWrap => "tilo-seal/v1/user-root-wrap",
            Self::FileSecretStore => "tilo-seal/v1/file-secret-store",
            Self::UnlockSiblingHalf => "tilo-seal/v1/unlock-sibling-half",
            Self::UnlockRelease => "tilo-seal/v1/unlock-release",
            Self::SecretService => "tilo-seal/v1/secret-service",
            Self::StoreCallerProof => "tilo-seal/v1/store-caller-proof",
            Self::SessionCallerProof => "tilo-seal/v1/session-caller-proof",
            Self::LanDiscoveryProbe => "tilo-seal/v1/lan-discovery-probe",
            Self::LanDiscoveryReply => "tilo-seal/v1/lan-discovery-reply",
            Self::SignInPassphrase => "tilo-seal/v1/sign-in-passphrase",
            Self::SignInRecovery => "tilo-seal/v1/sign-in-recovery",
            Self::TpmLockoutAuth => "tilo-seal/v1/tpm-lockout-auth",
            Self::DocumentContent => "tilo-seal/v1/record-content",
            Self::SenderMediaKeys => "tilo-seal/v1/sender-media-keys",
            Self::ReleasedPageText => "tilo-seal/v1/released-page-text",
            Self::RollbackFloorKey => "tilo-seal/v1/rollback-floor-key",
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
