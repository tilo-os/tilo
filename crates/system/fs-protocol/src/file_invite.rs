//! Signed, record-scoped invitations for collaborative files.
//!
//! A bearer credential proves knowledge of a 256-bit secret; the signed token
//! stores only its domain-separated hash. Redemption limits are enforced by an
//! online authority, never by this self-contained token.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::files::FileRecordId;
use collab_types::canonical::CanonicalError;
use collab_types::crypto::{Ed25519PublicKey, Signature, VerifyError, sign_event, verify_event};

pub const FILE_INVITE_DOMAIN_TAG: &str = "tilo-file-invite-v1";
pub const FILE_INVITE_LINK_PREFIX: &str = "tilo-file://v1/";
pub const MAX_FILE_INVITE_TOKEN_BYTES: usize = 16 * 1024;
const BEARER_HASH_DOMAIN: &[u8] = b"tilo-file-bearer-v1\0";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum FilePermission {
    View,
    Edit,
}

impl FilePermission {
    pub fn permits(self, required: Self) -> bool {
        matches!((self, required), (Self::Edit, _) | (Self::View, Self::View))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum FileInviteSubject {
    /// A certified human root identity, not one of that person's device keys.
    Person(Ed25519PublicKey),
    /// Anyone presenting the secret whose hash is committed here.
    Bearer { secret_hash: [u8; 32] },
}

/// Immutable signed grant. Revocation and redemption counters live at the
/// authority so their current state can change without rewriting the token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileInviteV1 {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub record_id: FileRecordId,
    pub subject: FileInviteSubject,
    pub permission: FilePermission,
    pub expires_at_ms: Option<u64>,
    /// `Some(1)` requests a one-use link. A verifier must consult the online
    /// redemption authority; signature verification alone is insufficient.
    pub max_redemptions: Option<u32>,
    pub issuer: Ed25519PublicKey,
    pub signature: Signature,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileInviteSpec {
    pub workspace_id: Uuid,
    pub record_id: FileRecordId,
    pub subject: FileInviteSubject,
    pub permission: FilePermission,
    pub expires_at_ms: Option<u64>,
    pub max_redemptions: Option<u32>,
}

/// Secret-bearing value suitable for encoding into a copied link. Keep this
/// out of logs and persistent grant listings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BearerFileInvite {
    pub token: FileInviteV1,
    pub secret: [u8; 32],
}

impl BearerFileInvite {
    /// Encode the version-1 portable invite credential. The secret is placed
    /// after `#`, keeping it out of conventional request logs. V1 deliberately
    /// carries no network locator; an application needs an independently known
    /// workspace authority. The FS layer's endpoint-bound V2 share link should
    /// be used for a self-routing link copied between devices.
    ///
    /// # Errors
    ///
    /// Returns an error if canonical encoding fails or the token exceeds the wire bound.
    pub fn encode_link(&self) -> Result<String, FileInviteError> {
        let token = collab_types::canonical::canonical_bytes(&self.token)?;
        if token.len() > MAX_FILE_INVITE_TOKEN_BYTES {
            return Err(FileInviteError::TokenTooLarge);
        }
        Ok(format!(
            "{FILE_INVITE_LINK_PREFIX}{}#{}",
            hex::encode(token),
            hex::encode(self.secret)
        ))
    }

    /// Decode, authenticate, and expiry-check a copied link. A token carrying
    /// a redemption limit still needs online FS-side authority redemption;
    /// decoding never consumes it.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed, oversized, expired, wrongly signed, or
    /// secret-mismatched links.
    pub fn decode_link_at(link: &str, now_ms: u64) -> Result<Self, FileInviteError> {
        let rest = link
            .strip_prefix(FILE_INVITE_LINK_PREFIX)
            .ok_or(FileInviteError::InvalidLink)?;
        let (token_hex, secret_hex) = rest.split_once('#').ok_or(FileInviteError::InvalidLink)?;
        if token_hex.len() > MAX_FILE_INVITE_TOKEN_BYTES * 2 || secret_hex.len() != 64 {
            return Err(FileInviteError::InvalidLink);
        }
        let token_bytes = hex::decode(token_hex).map_err(|_| FileInviteError::InvalidLink)?;
        let secret_bytes = hex::decode(secret_hex).map_err(|_| FileInviteError::InvalidLink)?;
        let secret: [u8; 32] = secret_bytes
            .try_into()
            .map_err(|_| FileInviteError::InvalidLink)?;
        let token: FileInviteV1 = collab_types::canonical::from_canonical_bytes(&token_bytes)?;
        token.verify_at(now_ms)?;
        let FileInviteSubject::Bearer { secret_hash } = token.subject else {
            return Err(FileInviteError::NotBearer);
        };
        if bearer_secret_hash(&secret) != secret_hash {
            return Err(FileInviteError::WrongSecret);
        }
        Ok(Self { token, secret })
    }
}

#[derive(Serialize)]
struct FileInviteView<'a> {
    tag: &'a str,
    id: Uuid,
    workspace_id: Uuid,
    record_id: FileRecordId,
    subject: &'a FileInviteSubject,
    permission: FilePermission,
    expires_at_ms: Option<u64>,
    max_redemptions: Option<u32>,
    issuer: Ed25519PublicKey,
}

impl FileInviteV1 {
    fn view(&self) -> FileInviteView<'_> {
        FileInviteView {
            tag: FILE_INVITE_DOMAIN_TAG,
            id: self.id,
            workspace_id: self.workspace_id,
            record_id: self.record_id,
            subject: &self.subject,
            permission: self.permission,
            expires_at_ms: self.expires_at_ms,
            max_redemptions: self.max_redemptions,
            issuer: self.issuer,
        }
    }

    /// Verify the issuer signature, expiry, and redemption-limit shape.
    ///
    /// # Errors
    ///
    /// Returns an error when the signature is invalid, the invite is expired,
    /// or its redemption limit is zero.
    pub fn verify_at(&self, now_ms: u64) -> Result<(), FileInviteError> {
        verify_event(&self.view(), &self.signature, &self.issuer)
            .map_err(FileInviteError::BadSignature)?;
        if self
            .expires_at_ms
            .is_some_and(|deadline| now_ms >= deadline)
        {
            return Err(FileInviteError::Expired);
        }
        if self.max_redemptions == Some(0) {
            return Err(FileInviteError::InvalidRedemptionLimit);
        }
        Ok(())
    }

    /// Verify an unrestricted bearer invite and its presented secret.
    ///
    /// # Errors
    ///
    /// Returns an error when ordinary invite verification fails, the invite is
    /// not bearer-scoped, the secret mismatches, or online authority is needed.
    pub fn verify_bearer_at(&self, secret: &[u8; 32], now_ms: u64) -> Result<(), FileInviteError> {
        self.verify_at(now_ms)?;
        let FileInviteSubject::Bearer { secret_hash } = self.subject else {
            return Err(FileInviteError::NotBearer);
        };
        if bearer_secret_hash(secret) != secret_hash {
            return Err(FileInviteError::WrongSecret);
        }
        if self.max_redemptions.is_some() {
            return Err(FileInviteError::AuthorityRequired);
        }
        Ok(())
    }
}

/// Mint a signed invite for the supplied specification.
///
/// # Errors
///
/// Returns an error for an invalid redemption limit or failed canonical signing.
pub fn mint_file_invite(
    spec: FileInviteSpec,
    issuer: &ed25519_dalek::SigningKey,
) -> Result<FileInviteV1, FileInviteError> {
    mint_file_invite_inner(spec, issuer)
}

/// Generate a fresh bearer secret and mint its signed invite.
///
/// # Errors
///
/// Returns an error for an invalid redemption limit or failed canonical signing.
pub fn mint_bearer_file_invite(
    mut spec: FileInviteSpec,
    issuer: &ed25519_dalek::SigningKey,
    rng: &mut impl rand::CryptoRng,
) -> Result<BearerFileInvite, FileInviteError> {
    let mut secret = [0u8; 32];
    rng.fill_bytes(&mut secret);
    spec.subject = FileInviteSubject::Bearer {
        secret_hash: bearer_secret_hash(&secret),
    };
    let token = mint_file_invite_inner(spec, issuer)?;
    Ok(BearerFileInvite { token, secret })
}

fn mint_file_invite_inner(
    spec: FileInviteSpec,
    issuer_sk: &ed25519_dalek::SigningKey,
) -> Result<FileInviteV1, FileInviteError> {
    if spec.max_redemptions == Some(0) {
        return Err(FileInviteError::InvalidRedemptionLimit);
    }
    let issuer = Ed25519PublicKey::from_bytes(issuer_sk.verifying_key().to_bytes());
    let mut token = FileInviteV1 {
        id: Uuid::new_v4(),
        workspace_id: spec.workspace_id,
        record_id: spec.record_id,
        subject: spec.subject,
        permission: spec.permission,
        expires_at_ms: spec.expires_at_ms,
        max_redemptions: spec.max_redemptions,
        issuer,
        signature: Signature::from_bytes([0; 64]),
    };
    token.signature = sign_event(&token.view(), issuer_sk)?;
    Ok(token)
}

pub fn bearer_secret_hash(secret: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(BEARER_HASH_DOMAIN);
    hasher.update(secret);
    hasher.finalize().into()
}

#[derive(Debug, Error)]
pub enum FileInviteError {
    #[error("file invite signature did not verify: {0}")]
    BadSignature(#[source] VerifyError),
    #[error("file invite expired")]
    Expired,
    #[error("redemption limit must be at least one")]
    InvalidRedemptionLimit,
    #[error("this is a named-recipient invite, not a bearer invite")]
    NotBearer,
    #[error("bearer secret did not match")]
    WrongSecret,
    #[error("a redemption authority is required to enforce this link's use limit")]
    AuthorityRequired,
    #[error("canonicalisation failed: {0}")]
    Canonical(#[from] CanonicalError),
    #[error("file invite link is malformed")]
    InvalidLink,
    #[error("file invite token exceeds its wire bound")]
    TokenTooLarge,
}

#[cfg(test)]
mod tests {
    use super::*;
    use getrandom::{SysRng, rand_core::UnwrapErr};

    fn spec(subject: FileInviteSubject) -> FileInviteSpec {
        FileInviteSpec {
            workspace_id: Uuid::new_v4(),
            record_id: FileRecordId::new(),
            subject,
            permission: FilePermission::View,
            expires_at_ms: Some(2_000),
            max_redemptions: None,
        }
    }

    /// A named invite is record-scoped, signed and expires; a bearer secret is required and never
    /// stored in the token.
    #[test]
    fn a_named_invite_is_scoped_signed_and_expiring_and_a_bearer_secret_is_never_stored() {
        // Phase: named_invite_is_record_scoped_signed_and_expires.
        {
            let mut rng = UnwrapErr(SysRng);
            let issuer = ed25519_dalek::SigningKey::generate(&mut rng);
            let recipient = ed25519_dalek::SigningKey::generate(&mut rng);
            let token = mint_file_invite(
                spec(FileInviteSubject::Person(Ed25519PublicKey::from_bytes(
                    recipient.verifying_key().to_bytes(),
                ))),
                &issuer,
            )
            .unwrap();
            token.verify_at(1_999).unwrap();
            assert!(matches!(
                token.verify_at(2_000),
                Err(FileInviteError::Expired)
            ));

            let mut tampered = token;
            tampered.record_id = FileRecordId::new();
            assert!(matches!(
                tampered.verify_at(1_000),
                Err(FileInviteError::BadSignature(_))
            ));
        }

        // Phase: bearer_secret_is_required_and_never_stored_in_the_token.
        {
            let mut rng = UnwrapErr(SysRng);
            let issuer = ed25519_dalek::SigningKey::generate(&mut rng);
            let invite = mint_bearer_file_invite(
                spec(FileInviteSubject::Bearer {
                    secret_hash: [0; 32],
                }),
                &issuer,
                &mut rng,
            )
            .unwrap();
            invite
                .token
                .verify_bearer_at(&invite.secret, 1_000)
                .unwrap();
            assert!(matches!(
                invite.token.verify_bearer_at(&[9; 32], 1_000),
                Err(FileInviteError::WrongSecret)
            ));
            let encoded = collab_types::canonical::canonical_bytes(&invite.token).unwrap();
            assert!(!encoded.windows(32).any(|window| window == invite.secret));
        }
    }

    /// A limited link refuses offline verification; a copied link round-trips and authenticates
    /// before redemption.
    #[test]
    fn links_verify_only_as_their_kind_allows() {
        // Phase: limited_link_refuses_offline_verification.
        {
            let mut rng = UnwrapErr(SysRng);
            let issuer = ed25519_dalek::SigningKey::generate(&mut rng);
            let mut limited = spec(FileInviteSubject::Bearer {
                secret_hash: [0; 32],
            });
            limited.max_redemptions = Some(1);
            let invite = mint_bearer_file_invite(limited, &issuer, &mut rng).unwrap();
            assert!(matches!(
                invite.token.verify_bearer_at(&invite.secret, 1_000),
                Err(FileInviteError::AuthorityRequired)
            ));
        }

        // Phase: copied_link_round_trips_and_authenticates_before_redemption.
        {
            let mut rng = UnwrapErr(SysRng);
            let issuer = ed25519_dalek::SigningKey::generate(&mut rng);
            let mut limited = spec(FileInviteSubject::Bearer {
                secret_hash: [0; 32],
            });
            limited.max_redemptions = Some(1);
            let invite = mint_bearer_file_invite(limited, &issuer, &mut rng).unwrap();
            let link = invite.encode_link().unwrap();
            assert!(link.starts_with(FILE_INVITE_LINK_PREFIX));
            let decoded = BearerFileInvite::decode_link_at(&link, 1_000).unwrap();
            assert_eq!(decoded, invite);

            let mut wrong = link;
            let last = wrong.pop().unwrap();
            wrong.push(if last == '0' { '1' } else { '0' });
            assert!(matches!(
                BearerFileInvite::decode_link_at(&wrong, 1_000),
                Err(FileInviteError::WrongSecret)
            ));
        }
    }
}
