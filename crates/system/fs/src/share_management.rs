//! Authority-backed creation and revocation of collaborative-file shares.
//!
//! This service intentionally accepts cryptographic identities, not display
//! names. The caller must obtain the trusted issuer set and named recipient
//! root key from the identity layer. It does not prove that an issuer is a
//! workspace governor, certify ownership of the selected Iroh endpoint, or
//! test route liveness; those remain identity/governance and transport-layer
//! responsibilities. It does bind the issuer's signature to the endpoint and
//! durably register every grant before reporting creation success.

use std::collections::BTreeSet;
use std::sync::Arc;

use collab_types::Ed25519PublicKey;
use fs_protocol::{
    FileInviteError, FileInviteSpec, FileInviteSubject, FileInviteV1, FilePermission, FileRecordId,
    mint_bearer_file_invite, mint_file_invite,
};
use getrandom::{SysRng, rand_core::UnwrapErr};
use iroh_base::EndpointAddr;
use thiserror::Error;
use uuid::Uuid;

use crate::share_authority::{ShareAuthority, ShareAuthorityError};
use crate::share_link::{FileShareLinkError, FileShareLinkV2};

pub const MAX_TRUSTED_SHARE_ISSUERS: usize = 64;
pub const MAX_SHARE_LIFETIME_MS: u64 = 365 * 24 * 60 * 60 * 1_000;
pub const MAX_BEARER_REDEMPTIONS: u32 = 1_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NamedGrantRequest {
    pub workspace_id: Uuid,
    pub record_id: FileRecordId,
    /// Certified human root key. A device key must not be supplied here.
    pub recipient: Ed25519PublicKey,
    pub permission: FilePermission,
    /// `None` means the named grant remains valid until explicitly revoked.
    pub expires_at_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BearerLinkRequest {
    pub workspace_id: Uuid,
    pub record_id: FileRecordId,
    /// Exact routed endpoint signed into the V2 link.
    pub endpoint: EndpointAddr,
    pub permission: FilePermission,
    /// Bearer links must have a finite deadline.
    pub expires_at_ms: u64,
    /// Online authority consumption bound, from one through 1,000.
    pub max_redemptions: u32,
}

pub struct ShareManagement {
    authority: Arc<ShareAuthority>,
}

impl ShareManagement {
    #[must_use]
    pub fn new(authority: Arc<ShareAuthority>) -> Self {
        Self { authority }
    }

    /// Mint and durably register a named-human grant.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid scope/identity/issuer/expiry, invite
    /// signing failure, or durable authority registration failure.
    pub fn create_named(
        &self,
        request: NamedGrantRequest,
        issuer: &ed25519_dalek::SigningKey,
        trusted_issuers: &[Ed25519PublicKey],
        now_ms: u64,
    ) -> Result<FileInviteV1, ShareManagementError> {
        validate_scope(request.workspace_id, request.record_id)?;
        validate_public_key(request.recipient)?;
        validate_issuer(issuer, trusted_issuers)?;
        validate_expiry(request.expires_at_ms, now_ms, false)?;
        let token = mint_file_invite(
            FileInviteSpec {
                workspace_id: request.workspace_id,
                record_id: request.record_id,
                subject: FileInviteSubject::Person(request.recipient),
                permission: request.permission,
                expires_at_ms: request.expires_at_ms,
                max_redemptions: None,
            },
            issuer,
        )?;
        self.authority.register(&token, trusted_issuers, now_ms)?;
        Ok(token)
    }

    /// Mint an endpoint-bound V2 bearer link and durably register its inner
    /// grant before returning the only secret-bearing value to the caller.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid scope/issuer/expiry/redemption limits,
    /// invite/link construction failure, or authority persistence failure.
    pub fn create_bearer_v2(
        &self,
        request: BearerLinkRequest,
        issuer: &ed25519_dalek::SigningKey,
        trusted_issuers: &[Ed25519PublicKey],
        now_ms: u64,
    ) -> Result<FileShareLinkV2, ShareManagementError> {
        validate_scope(request.workspace_id, request.record_id)?;
        validate_issuer(issuer, trusted_issuers)?;
        validate_expiry(Some(request.expires_at_ms), now_ms, true)?;
        if request.max_redemptions == 0 || request.max_redemptions > MAX_BEARER_REDEMPTIONS {
            return Err(ShareManagementError::InvalidRedemptionLimit);
        }
        let mut rng = UnwrapErr(SysRng);
        let bearer = mint_bearer_file_invite(
            FileInviteSpec {
                workspace_id: request.workspace_id,
                record_id: request.record_id,
                subject: FileInviteSubject::Bearer {
                    secret_hash: [0; 32],
                },
                permission: request.permission,
                expires_at_ms: Some(request.expires_at_ms),
                max_redemptions: Some(request.max_redemptions),
            },
            issuer,
            &mut rng,
        )?;
        let link = FileShareLinkV2::bind_at(request.endpoint, bearer, issuer, now_ms)?;
        // Prove the complete secret-bearing URI fits its hard wire bound before
        // committing the authority row. A later encode cannot strand a grant.
        drop(link.encode_uri_at(now_ms)?);
        self.authority
            .register(&link.invite, trusted_issuers, now_ms)?;
        Ok(link)
    }

    /// Revoke a grant immediately. Already copied bytes cannot be recalled.
    ///
    /// # Errors
    ///
    /// Returns an error when durable authority state cannot be updated.
    pub fn revoke(&self, grant_id: Uuid) -> Result<bool, ShareManagementError> {
        Ok(self.authority.revoke(grant_id)?)
    }
}

fn validate_scope(workspace_id: Uuid, record_id: FileRecordId) -> Result<(), ShareManagementError> {
    if workspace_id.is_nil() || record_id.0.is_nil() {
        return Err(ShareManagementError::InvalidScope);
    }
    Ok(())
}

fn validate_public_key(key: Ed25519PublicKey) -> Result<(), ShareManagementError> {
    ed25519_dalek::VerifyingKey::from_bytes(key.as_bytes())
        .map(|_| ())
        .map_err(|_| ShareManagementError::InvalidIdentityKey)
}

fn validate_issuer(
    issuer: &ed25519_dalek::SigningKey,
    trusted_issuers: &[Ed25519PublicKey],
) -> Result<(), ShareManagementError> {
    if trusted_issuers.is_empty() || trusted_issuers.len() > MAX_TRUSTED_SHARE_ISSUERS {
        return Err(ShareManagementError::InvalidTrustedIssuerSet);
    }
    let mut unique = BTreeSet::new();
    for trusted in trusted_issuers {
        validate_public_key(*trusted)?;
        if !unique.insert(*trusted) {
            return Err(ShareManagementError::InvalidTrustedIssuerSet);
        }
    }
    let public = Ed25519PublicKey::from_bytes(issuer.verifying_key().to_bytes());
    if !unique.contains(&public) {
        return Err(ShareManagementError::UntrustedIssuer);
    }
    Ok(())
}

fn validate_expiry(
    expiry: Option<u64>,
    now_ms: u64,
    required: bool,
) -> Result<(), ShareManagementError> {
    let Some(expiry) = expiry else {
        return if required {
            Err(ShareManagementError::ExpiryRequired)
        } else {
            Ok(())
        };
    };
    if expiry <= now_ms {
        return Err(ShareManagementError::ExpiryNotFuture);
    }
    if expiry.saturating_sub(now_ms) > MAX_SHARE_LIFETIME_MS {
        return Err(ShareManagementError::ExpiryTooDistant);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum ShareManagementError {
    #[error("workspace and record ids must be non-nil")]
    InvalidScope,
    #[error("identity key is not a valid Ed25519 public key")]
    InvalidIdentityKey,
    #[error("trusted issuer set must be non-empty, unique, and bounded")]
    InvalidTrustedIssuerSet,
    #[error("the explicit issuer signing key is not trusted")]
    UntrustedIssuer,
    #[error("share expiry must be in the future")]
    ExpiryNotFuture,
    #[error("bearer links require a finite expiry")]
    ExpiryRequired,
    #[error("share expiry exceeds the maximum lifetime")]
    ExpiryTooDistant,
    #[error("bearer redemption limit is outside the supported bound")]
    InvalidRedemptionLimit,
    #[error("file invite creation failed: {0}")]
    Invite(#[from] FileInviteError),
    #[error("endpoint-bound link creation failed: {0}")]
    Link(#[from] FileShareLinkError),
    #[error("share authority update failed: {0}")]
    Authority(#[from] ShareAuthorityError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::share_authority::{ShareGrantStatus, ShareSubjectProjection};
    use iroh::SecretKey;

    const NOW: u64 = 10_000;

    struct Fixture {
        _dir: tempfile::TempDir,
        authority_path: std::path::PathBuf,
        authority: Arc<ShareAuthority>,
        service: ShareManagement,
        issuer: ed25519_dalek::SigningKey,
        issuer_public: Ed25519PublicKey,
        workspace_id: Uuid,
        record_id: FileRecordId,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let authority_path = dir.path().join("shares.json");
            let authority = Arc::new(ShareAuthority::open(&authority_path).unwrap());
            let issuer = ed25519_dalek::SigningKey::from_bytes(&[51; 32]);
            let issuer_public = Ed25519PublicKey::from_bytes(issuer.verifying_key().to_bytes());
            let service = ShareManagement::new(authority.clone());
            Self {
                _dir: dir,
                authority_path,
                authority,
                service,
                issuer,
                issuer_public,
                workspace_id: Uuid::from_u128(11),
                record_id: FileRecordId(Uuid::from_u128(12)),
            }
        }

        fn named(&self, recipient: Ed25519PublicKey) -> NamedGrantRequest {
            NamedGrantRequest {
                workspace_id: self.workspace_id,
                record_id: self.record_id,
                recipient,
                permission: FilePermission::Edit,
                expires_at_ms: Some(NOW + 1_000),
            }
        }

        fn bearer(&self) -> BearerLinkRequest {
            BearerLinkRequest {
                workspace_id: self.workspace_id,
                record_id: self.record_id,
                endpoint: EndpointAddr::new(SecretKey::from_bytes(&[61; 32]).public())
                    .with_ip_addr("127.0.0.1:4455".parse().unwrap()),
                permission: FilePermission::View,
                expires_at_ms: NOW + 1_000,
                max_redemptions: 1,
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn share_management_persists_exact_grants_and_bounds_every_input() {
        // Phase `creates_persists_restarts_and_revokes_exact_grants`.
        {
            let fixture = Fixture::new();
            let (_, recipient) = collab_types::generate_keypair();
            let named = fixture
                .service
                .create_named(
                    fixture.named(recipient),
                    &fixture.issuer,
                    &[fixture.issuer_public],
                    NOW,
                )
                .unwrap();
            let link = fixture
                .service
                .create_bearer_v2(
                    fixture.bearer(),
                    &fixture.issuer,
                    &[fixture.issuer_public],
                    NOW,
                )
                .unwrap();
            let uri = link.encode_uri_at(NOW).unwrap();
            let fragment = uri.split_once('#').unwrap().1.to_owned();
            assert!(!format!("{link:?}").contains(&fragment));
            assert_eq!(link.invite.workspace_id, fixture.workspace_id);
            assert_eq!(link.invite.record_id, fixture.record_id);
            assert_eq!(link.endpoint, fixture.bearer().endpoint);
            drop(fixture.service);
            drop(fixture.authority);

            let reopened = Arc::new(ShareAuthority::open(&fixture.authority_path).unwrap());
            let projection = reopened.projection(fixture.record_id, NOW + 1);
            assert_eq!(projection.grants.len(), 2);
            assert_eq!(
                projection
                    .grants
                    .iter()
                    .find(|grant| grant.grant_id == named.id)
                    .unwrap()
                    .subject,
                ShareSubjectProjection::Person { person: recipient }
            );
            let bearer = projection
                .grants
                .iter()
                .find(|grant| grant.grant_id == link.invite.id)
                .unwrap();
            assert_eq!(bearer.subject, ShareSubjectProjection::BearerLink);
            assert_eq!(bearer.max_redemptions, Some(1));

            let persisted =
                String::from_utf8(std::fs::read(&fixture.authority_path).unwrap()).unwrap();
            assert!(!persisted.contains(&fragment));
            let service = ShareManagement::new(reopened.clone());
            assert!(service.revoke(named.id).unwrap());
            assert_eq!(
                reopened
                    .projection(fixture.record_id, NOW + 2)
                    .grants
                    .iter()
                    .find(|grant| grant.grant_id == named.id)
                    .unwrap()
                    .status,
                ShareGrantStatus::Revoked
            );
        }
        // Phase `persistence_failure_returns_no_grant_and_does_not_mutate_authority`.
        {
            let fixture = Fixture::new();
            assert!(!fixture.authority_path.exists());
            std::fs::create_dir(&fixture.authority_path).unwrap();
            let (_, recipient) = collab_types::generate_keypair();
            assert!(matches!(
                fixture.service.create_named(
                    fixture.named(recipient),
                    &fixture.issuer,
                    &[fixture.issuer_public],
                    NOW
                ),
                Err(ShareManagementError::Authority(ShareAuthorityError::Io(_)))
            ));
            assert!(matches!(
                fixture.service.create_bearer_v2(
                    fixture.bearer(),
                    &fixture.issuer,
                    &[fixture.issuer_public],
                    NOW
                ),
                Err(ShareManagementError::Authority(ShareAuthorityError::Io(_)))
            ));
            assert!(
                fixture
                    .authority
                    .projection(fixture.record_id, NOW)
                    .grants
                    .is_empty()
            );
            std::fs::remove_dir(&fixture.authority_path).unwrap();
        }
        // Phase `trust_scope_expiry_redemption_and_endpoint_inputs_are_bounded`.
        {
            let fixture = Fixture::new();
            let (_, recipient) = collab_types::generate_keypair();
            assert!(matches!(
                fixture
                    .service
                    .create_named(fixture.named(recipient), &fixture.issuer, &[], NOW),
                Err(ShareManagementError::InvalidTrustedIssuerSet)
            ));
            let (_, stranger) = collab_types::generate_keypair();
            assert!(matches!(
                fixture.service.create_named(
                    fixture.named(recipient),
                    &fixture.issuer,
                    &[stranger],
                    NOW
                ),
                Err(ShareManagementError::UntrustedIssuer)
            ));

            let mut bearer = fixture.bearer();
            bearer.max_redemptions = MAX_BEARER_REDEMPTIONS + 1;
            assert!(matches!(
                fixture.service.create_bearer_v2(
                    bearer,
                    &fixture.issuer,
                    &[fixture.issuer_public],
                    NOW
                ),
                Err(ShareManagementError::InvalidRedemptionLimit)
            ));
            let mut bearer = fixture.bearer();
            bearer.expires_at_ms = NOW + MAX_SHARE_LIFETIME_MS + 1;
            assert!(matches!(
                fixture.service.create_bearer_v2(
                    bearer,
                    &fixture.issuer,
                    &[fixture.issuer_public],
                    NOW
                ),
                Err(ShareManagementError::ExpiryTooDistant)
            ));
            let mut bearer = fixture.bearer();
            bearer.endpoint = EndpointAddr::new(SecretKey::from_bytes(&[62; 32]).public());
            assert!(matches!(
                fixture.service.create_bearer_v2(
                    bearer,
                    &fixture.issuer,
                    &[fixture.issuer_public],
                    NOW
                ),
                Err(ShareManagementError::Link(
                    FileShareLinkError::EndpointHasNoRoute
                ))
            ));
        }
    }
}
