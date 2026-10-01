//! Durable revocation and atomic bearer-link redemption authority.
//!
//! The daemon is the single writer for one workspace authority file. A limited
//! bearer token is accepted only after its consumption count is durably
//! advanced, so two concurrent one-use attempts cannot both succeed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use collab_types::{
    Ed25519PublicKey, FileActor, FileInviteError, FileInviteSubject, FileInviteV1, FilePermission,
    bearer_secret_hash, canonical_bytes,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

const AUTHORITY_VERSION: u16 = 1;
pub const SHARE_PROJECTION_VERSION: u16 = 1;
pub const MAX_SHARES_PER_RECORD: usize = 32;
pub const MAX_SHARE_PROJECTION_BYTES: usize = 16 * 1024;
pub const MAX_SHARE_AUTHORITY_GRANTS: usize = 4_096;
pub const MAX_SHARE_AUTHORITY_FILE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorityState {
    version: u16,
    grants: BTreeMap<Uuid, GrantState>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantState {
    token_hash: [u8; 32],
    /// The signed grant is the authority's source of record/recipient truth.
    /// `None` reads authority files written before share projection existed;
    /// re-registering that token upgrades the row without changing its id.
    #[serde(default)]
    token: Option<FileInviteV1>,
    revoked: bool,
    redemptions: u32,
    max_redemptions: Option<u32>,
    /// Authenticated actors that consumed each numbered bearer use. This makes
    /// reconnect by the same certified device idempotent without allowing the
    /// bearer secret to admit a second actor.
    #[serde(default)]
    redemption_actors: Vec<FileActor>,
}

pub struct ShareAuthority {
    path: PathBuf,
    state: Mutex<AuthorityState>,
}

impl ShareAuthority {
    /// Open and fully validate a durable share authority.
    ///
    /// # Errors
    ///
    /// Returns an error for I/O, oversized/malformed/unsupported state, or any
    /// inconsistent persisted grant.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, ShareAuthorityError> {
        let path = path.into();
        let state = match std::fs::metadata(&path) {
            Ok(metadata) if metadata.len() > MAX_SHARE_AUTHORITY_FILE_BYTES => {
                return Err(ShareAuthorityError::StateTooLarge);
            }
            Ok(_) => {
                let bytes = std::fs::read(&path)?;
                let state: AuthorityState = serde_json::from_slice(&bytes)?;
                if state.version != AUTHORITY_VERSION {
                    return Err(ShareAuthorityError::UnsupportedVersion(state.version));
                }
                validate_state(&state)?;
                state
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => AuthorityState {
                version: AUTHORITY_VERSION,
                grants: BTreeMap::new(),
            },
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            path,
            state: Mutex::new(state),
        })
    }

    /// Admit a signed grant from an explicitly trusted workspace authority.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid/untrusted grants, id collisions, capacity
    /// limits, poisoned state, encoding, or durable persistence failure.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the writer lock intentionally spans admission checks, durable replacement, and in-memory publication"
    )]
    pub fn register(
        &self,
        token: &FileInviteV1,
        trusted_issuers: &[Ed25519PublicKey],
        now_ms: u64,
    ) -> Result<(), ShareAuthorityError> {
        token.verify_at(now_ms)?;
        if !trusted_issuers.contains(&token.issuer) {
            return Err(ShareAuthorityError::UntrustedIssuer);
        }
        let hash = token_hash(token)?;
        let mut current = self
            .state
            .lock()
            .map_err(|_| ShareAuthorityError::StatePoisoned)?;
        if let Some(existing) = current.grants.get(&token.id) {
            if existing.token_hash != hash {
                return Err(ShareAuthorityError::GrantIdCollision);
            }
            if existing.token.is_none() {
                let record_grants = current
                    .grants
                    .values()
                    .filter(|grant| {
                        grant
                            .token
                            .as_ref()
                            .is_some_and(|registered| registered.record_id == token.record_id)
                    })
                    .count();
                if record_grants >= MAX_SHARES_PER_RECORD {
                    return Err(ShareAuthorityError::TooManyRecordGrants);
                }
                let mut next = current.clone();
                next.grants
                    .get_mut(&token.id)
                    .ok_or(ShareAuthorityError::UnknownGrant)?
                    .token = Some(token.clone());
                persist(&self.path, &next)?;
                *current = next;
            }
            return Ok(());
        }
        if current.grants.len() >= MAX_SHARE_AUTHORITY_GRANTS {
            return Err(ShareAuthorityError::TooManyGrants);
        }
        let record_grants = current
            .grants
            .values()
            .filter(|grant| {
                grant
                    .token
                    .as_ref()
                    .is_some_and(|registered| registered.record_id == token.record_id)
            })
            .count();
        if record_grants >= MAX_SHARES_PER_RECORD {
            return Err(ShareAuthorityError::TooManyRecordGrants);
        }
        let mut next = current.clone();
        next.grants.insert(
            token.id,
            GrantState {
                token_hash: hash,
                token: Some(token.clone()),
                revoked: false,
                redemptions: 0,
                max_redemptions: token.max_redemptions,
                redemption_actors: Vec::new(),
            },
        );
        persist(&self.path, &next)?;
        *current = next;
        Ok(())
    }

    /// Revoke future access. Already-copied bytes cannot be recalled.
    ///
    /// # Errors
    ///
    /// Returns an error for poisoned state or durable persistence failure.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the writer lock intentionally spans revocation checks, durable replacement, and in-memory publication"
    )]
    pub fn revoke(&self, id: Uuid) -> Result<bool, ShareAuthorityError> {
        let mut current = self
            .state
            .lock()
            .map_err(|_| ShareAuthorityError::StatePoisoned)?;
        let Some(grant) = current.grants.get(&id) else {
            return Ok(false);
        };
        if grant.revoked {
            return Ok(false);
        }
        let mut next = current.clone();
        next.grants
            .get_mut(&id)
            .ok_or(ShareAuthorityError::UnknownGrant)?
            .revoked = true;
        persist(&self.path, &next)?;
        *current = next;
        Ok(true)
    }

    /// Recheck a named human's live grant for every record request. Device
    /// certification is deliberately a separate transport precondition; this
    /// method accepts the verified human root identity, never a display name.
    ///
    /// # Errors
    ///
    /// Returns an error when token/recipient/permission/live authority state
    /// fails validation or the state lock is poisoned.
    pub fn authorize_named(
        &self,
        token: &FileInviteV1,
        person: Ed25519PublicKey,
        required: FilePermission,
        now_ms: u64,
    ) -> Result<(), ShareAuthorityError> {
        token.verify_at(now_ms)?;
        if token.subject != FileInviteSubject::Person(person) {
            return Err(ShareAuthorityError::WrongRecipient);
        }
        if !token.permission.permits(required) {
            return Err(ShareAuthorityError::InsufficientPermission);
        }
        let hash = token_hash(token)?;
        let current = self
            .state
            .lock()
            .map_err(|_| ShareAuthorityError::StatePoisoned)?;
        let grant = current
            .grants
            .get(&token.id)
            .ok_or(ShareAuthorityError::UnknownGrant)?;
        if grant.token_hash != hash {
            return Err(ShareAuthorityError::GrantIdCollision);
        }
        if grant.revoked {
            return Err(ShareAuthorityError::Revoked);
        }
        drop(current);
        Ok(())
    }

    /// Verify and consume a bearer grant. Limited links require prior
    /// registration and are durably incremented before success is returned.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid secret/token/actor authority, exhausted or
    /// revoked grants, poisoned state, or durable persistence failure.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the writer lock serializes redemption checks with durable consumption and in-memory publication"
    )]
    pub fn redeem(
        &self,
        token: &FileInviteV1,
        secret: &[u8; 32],
        actor: FileActor,
        now_ms: u64,
    ) -> Result<Redemption, ShareAuthorityError> {
        token.verify_at(now_ms)?;
        let FileInviteSubject::Bearer { secret_hash } = token.subject else {
            return Err(ShareAuthorityError::NotBearer);
        };
        if bearer_secret_hash(secret) != secret_hash {
            return Err(ShareAuthorityError::WrongSecret);
        }
        let hash = token_hash(token)?;
        let mut current = self
            .state
            .lock()
            .map_err(|_| ShareAuthorityError::StatePoisoned)?;
        let grant = current
            .grants
            .get(&token.id)
            .ok_or(ShareAuthorityError::UnknownGrant)?;
        if grant.token_hash != hash {
            return Err(ShareAuthorityError::GrantIdCollision);
        }
        if grant.revoked {
            return Err(ShareAuthorityError::Revoked);
        }
        if let Some(index) = grant
            .redemption_actors
            .iter()
            .position(|redeemer| *redeemer == actor)
        {
            let redemption_number = u32::try_from(index)
                .ok()
                .and_then(|value| value.checked_add(1))
                .ok_or(ShareAuthorityError::InvalidState)?;
            return Ok(Redemption {
                grant_id: token.id,
                redemption_number,
                actor,
            });
        }
        if grant
            .max_redemptions
            .is_some_and(|max| grant.redemptions >= max)
        {
            return Err(ShareAuthorityError::Exhausted);
        }

        let mut next = current.clone();
        let count = {
            let next_grant = next
                .grants
                .get_mut(&token.id)
                .ok_or(ShareAuthorityError::UnknownGrant)?;
            next_grant.redemptions = next_grant.redemptions.saturating_add(1);
            next_grant.redemption_actors.push(actor);
            next_grant.redemptions
        };
        persist(&self.path, &next)?;
        *current = next;
        Ok(Redemption {
            grant_id: token.id,
            redemption_number: count,
            actor,
        })
    }

    /// Recheck a previously redeemed bearer session without consuming the
    /// link again. The opaque redemption receipt proves that this session was
    /// one of the authority's admitted uses; expiry, permission and live
    /// revocation are still evaluated on every request.
    pub(crate) fn authorize_redeemed(
        &self,
        token: &FileInviteV1,
        redemption: Redemption,
        required: FilePermission,
        now_ms: u64,
    ) -> Result<(), ShareAuthorityError> {
        token.verify_at(now_ms)?;
        if !matches!(token.subject, FileInviteSubject::Bearer { .. }) {
            return Err(ShareAuthorityError::NotBearer);
        }
        if !token.permission.permits(required) {
            return Err(ShareAuthorityError::InsufficientPermission);
        }
        if redemption.grant_id != token.id || redemption.redemption_number == 0 {
            return Err(ShareAuthorityError::InvalidRedemption);
        }
        let hash = token_hash(token)?;
        let current = self
            .state
            .lock()
            .map_err(|_| ShareAuthorityError::StatePoisoned)?;
        let grant = current
            .grants
            .get(&token.id)
            .ok_or(ShareAuthorityError::UnknownGrant)?;
        if grant.token_hash != hash {
            return Err(ShareAuthorityError::GrantIdCollision);
        }
        if grant.revoked {
            return Err(ShareAuthorityError::Revoked);
        }
        if redemption.redemption_number > grant.redemptions {
            return Err(ShareAuthorityError::InvalidRedemption);
        }
        let actor_index = usize::try_from(redemption.redemption_number.saturating_sub(1))
            .map_err(|_| ShareAuthorityError::InvalidRedemption)?;
        if grant.redemption_actors.get(actor_index) != Some(&redemption.actor) {
            return Err(ShareAuthorityError::InvalidRedemption);
        }
        drop(current);
        Ok(())
    }

    /// Return the complete bounded, redacted share state for one stable
    /// record. The signed tokens retained by the authority are the source of
    /// truth; bearer secrets and secret hashes are never projected.
    pub fn projection(
        &self,
        record_id: collab_types::FileRecordId,
        now_ms: u64,
    ) -> ShareProjectionV1 {
        let current = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let grants = current
            .grants
            .iter()
            .filter_map(|(id, state)| {
                let token = state.token.as_ref()?;
                if token.record_id != record_id {
                    return None;
                }
                let subject = match &token.subject {
                    FileInviteSubject::Person(person) => {
                        ShareSubjectProjection::Person { person: *person }
                    }
                    FileInviteSubject::Bearer { .. } => ShareSubjectProjection::BearerLink,
                };
                let expired = token
                    .expires_at_ms
                    .is_some_and(|deadline| now_ms >= deadline);
                let exhausted = state
                    .max_redemptions
                    .is_some_and(|max| state.redemptions >= max);
                let status = if state.revoked {
                    ShareGrantStatus::Revoked
                } else if expired {
                    ShareGrantStatus::Expired
                } else if exhausted {
                    ShareGrantStatus::Exhausted
                } else {
                    ShareGrantStatus::Active
                };
                Some(ShareGrantProjectionV1 {
                    grant_id: *id,
                    subject,
                    permission: token.permission,
                    expires_at_ms: token.expires_at_ms,
                    revoked: state.revoked,
                    expired,
                    used: state.redemptions > 0,
                    exhausted,
                    status,
                    redemptions: state.redemptions,
                    max_redemptions: state.max_redemptions,
                })
            })
            .collect();
        let projection = ShareProjectionV1 {
            schema_version: SHARE_PROJECTION_VERSION,
            record_id,
            observed_at_ms: now_ms,
            grants,
        };
        drop(current);
        projection
    }
}

/// Redacted, versioned share state safe to expose to local applications.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShareProjectionV1 {
    pub schema_version: u16,
    pub record_id: collab_types::FileRecordId,
    pub observed_at_ms: u64,
    pub grants: Vec<ShareGrantProjectionV1>,
}

impl ShareProjectionV1 {
    /// Serialize and validate this redacted projection for an xattr.
    ///
    /// # Errors
    ///
    /// Returns an error for inconsistent state, JSON failure, or a projection
    /// above the byte bound.
    pub fn to_xattr_bytes(&self) -> Result<Vec<u8>, ShareProjectionError> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > MAX_SHARE_PROJECTION_BYTES {
            return Err(ShareProjectionError::TooLarge);
        }
        Ok(bytes)
    }

    /// Parse and fully validate a redacted share projection xattr.
    ///
    /// # Errors
    ///
    /// Returns an error for oversized/malformed JSON, unsupported schema, or
    /// inconsistent/duplicate grant state.
    pub fn parse_xattr(bytes: &[u8]) -> Result<Self, ShareProjectionError> {
        if bytes.len() > MAX_SHARE_PROJECTION_BYTES {
            return Err(ShareProjectionError::TooLarge);
        }
        let projection: Self = serde_json::from_slice(bytes)?;
        projection.validate()?;
        Ok(projection)
    }

    fn validate(&self) -> Result<(), ShareProjectionError> {
        if self.schema_version != SHARE_PROJECTION_VERSION {
            return Err(ShareProjectionError::UnsupportedVersion(
                self.schema_version,
            ));
        }
        if self.grants.len() > MAX_SHARES_PER_RECORD {
            return Err(ShareProjectionError::TooManyGrants);
        }
        let mut ids = BTreeSet::new();
        for grant in &self.grants {
            if !ids.insert(grant.grant_id) {
                return Err(ShareProjectionError::InvalidGrantState);
            }
            let expired = grant
                .expires_at_ms
                .is_some_and(|deadline| self.observed_at_ms >= deadline);
            let used = grant.redemptions > 0;
            let exhausted = grant
                .max_redemptions
                .is_some_and(|max| grant.redemptions >= max);
            let status = if grant.revoked {
                ShareGrantStatus::Revoked
            } else if expired {
                ShareGrantStatus::Expired
            } else if exhausted {
                ShareGrantStatus::Exhausted
            } else {
                ShareGrantStatus::Active
            };
            if grant.expired != expired
                || grant.used != used
                || grant.exhausted != exhausted
                || grant.status != status
                || grant
                    .max_redemptions
                    .is_some_and(|max| max == 0 || grant.redemptions > max)
                || matches!(grant.subject, ShareSubjectProjection::Person { .. })
                    && grant.redemptions != 0
            {
                return Err(ShareProjectionError::InvalidGrantState);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "these booleans are explicit redundant fields in the versioned projection schema and are cross-validated on decode"
)]
pub struct ShareGrantProjectionV1 {
    pub grant_id: Uuid,
    pub subject: ShareSubjectProjection,
    pub permission: FilePermission,
    pub expires_at_ms: Option<u64>,
    pub revoked: bool,
    pub expired: bool,
    pub used: bool,
    pub exhausted: bool,
    pub status: ShareGrantStatus,
    pub redemptions: u32,
    pub max_redemptions: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ShareSubjectProjection {
    Person { person: Ed25519PublicKey },
    BearerLink,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShareGrantStatus {
    Active,
    Expired,
    Revoked,
    Exhausted,
}

#[derive(Debug, Error)]
pub enum ShareProjectionError {
    #[error("share projection exceeds the {MAX_SHARE_PROJECTION_BYTES}-byte bound")]
    TooLarge,
    #[error("share projection is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported share projection version {0}")]
    UnsupportedVersion(u16),
    #[error("share projection exceeds the {MAX_SHARES_PER_RECORD}-grant bound")]
    TooManyGrants,
    #[error("share projection contains inconsistent or duplicate grant state")]
    InvalidGrantState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Redemption {
    pub grant_id: Uuid,
    pub redemption_number: u32,
    pub(crate) actor: FileActor,
}

fn token_hash(token: &FileInviteV1) -> Result<[u8; 32], ShareAuthorityError> {
    let bytes = canonical_bytes(token)?;
    Ok(*blake3::hash(&bytes).as_bytes())
}

fn persist(path: &Path, state: &AuthorityState) -> Result<(), ShareAuthorityError> {
    let bytes = serde_json::to_vec(state)?;
    if bytes.len() as u64 > MAX_SHARE_AUTHORITY_FILE_BYTES {
        return Err(ShareAuthorityError::StateTooLarge);
    }
    tilo_files::native::atomic::write_creating_parent(path, &bytes)?;
    Ok(())
}

fn validate_state(state: &AuthorityState) -> Result<(), ShareAuthorityError> {
    if state.grants.len() > MAX_SHARE_AUTHORITY_GRANTS {
        return Err(ShareAuthorityError::TooManyGrants);
    }
    for (id, grant) in &state.grants {
        if grant.redemptions as usize > MAX_BEARER_REDEMPTIONS_FOR_STATE
            || grant
                .max_redemptions
                .is_some_and(|max| max == 0 || grant.redemptions > max)
            || (!grant.redemption_actors.is_empty()
                && grant.redemption_actors.len() != grant.redemptions as usize)
        {
            return Err(ShareAuthorityError::InvalidState);
        }
        if let Some(token) = &grant.token
            && (token.id != *id
                || token.max_redemptions != grant.max_redemptions
                || token.verify_at(0).is_err()
                || token_hash(token).map_err(|_| ShareAuthorityError::InvalidState)?
                    != grant.token_hash
                || matches!(token.subject, FileInviteSubject::Person(_))
                    && (grant.redemptions != 0 || !grant.redemption_actors.is_empty()))
        {
            return Err(ShareAuthorityError::InvalidState);
        }
    }
    Ok(())
}

const MAX_BEARER_REDEMPTIONS_FOR_STATE: usize = 1_000;

#[derive(Debug, Error)]
pub enum ShareAuthorityError {
    #[error("I/O failure: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid authority state: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported authority state version {0}")]
    UnsupportedVersion(u16),
    #[error("share authority state exceeds its byte bound")]
    StateTooLarge,
    #[error("share authority contains too many grants")]
    TooManyGrants,
    #[error("share authority state is internally inconsistent")]
    InvalidState,
    #[error("invite verification failed: {0}")]
    Invite(#[from] FileInviteError),
    #[error("canonical encoding failed: {0}")]
    Canonical(#[from] collab_types::CanonicalError),
    #[error("invite issuer is not a trusted workspace authority")]
    UntrustedIssuer,
    #[error("another token already uses this grant id")]
    GrantIdCollision,
    #[error("grant is not registered")]
    UnknownGrant,
    #[error("grant has been revoked")]
    Revoked,
    #[error("bearer link has no redemptions remaining")]
    Exhausted,
    #[error("invite is for a named recipient")]
    NotBearer,
    #[error("bearer secret did not match")]
    WrongSecret,
    #[error("grant belongs to another human identity")]
    WrongRecipient,
    #[error("grant does not include the requested permission")]
    InsufficientPermission,
    #[error("bearer redemption receipt was not issued for this grant")]
    InvalidRedemption,
    #[error("record already has the maximum number of share grants")]
    TooManyRecordGrants,
    #[error("share authority state lock is poisoned")]
    StatePoisoned,
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use collab_types::{
        FileInviteSpec, FileInviteSubject, FilePermission, FileRecordId, mint_bearer_file_invite,
        mint_file_invite,
    };
    use getrandom::{SysRng, rand_core::UnwrapErr};

    use super::*;

    fn actor(seed: u8) -> FileActor {
        let person = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[seed.saturating_add(1); 32]);
        FileActor {
            person: Ed25519PublicKey::from_bytes(person.verifying_key().to_bytes()),
            device: Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes()),
        }
    }

    fn limited_invite() -> (
        tempfile::TempDir,
        Ed25519PublicKey,
        collab_types::BearerFileInvite,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let mut rng = UnwrapErr(SysRng);
        let (issuer, issuer_public) = collab_types::generate_keypair();
        let invite = mint_bearer_file_invite(
            FileInviteSpec {
                workspace_id: Uuid::new_v4(),
                record_id: FileRecordId::new(),
                subject: FileInviteSubject::Bearer {
                    secret_hash: [0; 32],
                },
                permission: FilePermission::View,
                expires_at_ms: Some(10_000),
                max_redemptions: Some(1),
            },
            &issuer,
            &mut rng,
        )
        .unwrap();
        (dir, issuer_public, invite)
    }

    #[test]
    fn one_use_and_named_grants_are_exact_race_free_and_revocable() {
        // Phase `one_use_link_survives_restart_same_actor_reconnects_and_another_is_denied`.
        {
            let (dir, issuer, invite) = limited_invite();
            let path = dir.path().join("shares.json");
            let authority = ShareAuthority::open(&path).unwrap();
            let trusted = [issuer];
            authority.register(&invite.token, &trusted, 1).unwrap();
            authority
                .redeem(&invite.token, &invite.secret, actor(10), 2)
                .unwrap();
            drop(authority);

            let reopened = ShareAuthority::open(path).unwrap();
            assert_eq!(
                reopened
                    .redeem(&invite.token, &invite.secret, actor(10), 3)
                    .unwrap()
                    .redemption_number,
                1
            );
            assert!(matches!(
                reopened.redeem(&invite.token, &invite.secret, actor(20), 3),
                Err(ShareAuthorityError::Exhausted)
            ));
        }
        // Phase `concurrent_one_use_race_has_exactly_one_winner`.
        {
            let (dir, issuer, invite) = limited_invite();
            let authority = Arc::new(ShareAuthority::open(dir.path().join("shares.json")).unwrap());
            authority.register(&invite.token, &[issuer], 1).unwrap();
            let invite = Arc::new(invite);
            let barrier = Arc::new(Barrier::new(3));
            let mut threads = Vec::new();
            for seed in [30, 40] {
                let authority = authority.clone();
                let invite = invite.clone();
                let barrier = barrier.clone();
                threads.push(std::thread::spawn(move || {
                    barrier.wait();
                    authority.redeem(&invite.token, &invite.secret, actor(seed), 2)
                }));
            }
            barrier.wait();
            let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
            assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
            assert_eq!(
                results
                    .iter()
                    .filter(|r| matches!(r, Err(ShareAuthorityError::Exhausted)))
                    .count(),
                1
            );
        }
        // Phase `named_grants_bind_human_record_permission_and_live_revocation`.
        {
            let dir = tempfile::tempdir().unwrap();
            let (issuer_sk, issuer) = collab_types::generate_keypair();
            let (_, person) = collab_types::generate_keypair();
            let (_, stranger) = collab_types::generate_keypair();
            let token = collab_types::mint_file_invite(
                FileInviteSpec {
                    workspace_id: Uuid::new_v4(),
                    record_id: FileRecordId::new(),
                    subject: FileInviteSubject::Person(person),
                    permission: FilePermission::View,
                    expires_at_ms: Some(10_000),
                    max_redemptions: None,
                },
                &issuer_sk,
            )
            .unwrap();
            let authority = ShareAuthority::open(dir.path().join("shares.json")).unwrap();
            authority.register(&token, &[issuer], 1).unwrap();
            authority
                .authorize_named(&token, person, FilePermission::View, 2)
                .unwrap();
            assert!(matches!(
                authority.authorize_named(&token, stranger, FilePermission::View, 2),
                Err(ShareAuthorityError::WrongRecipient)
            ));
            assert!(matches!(
                authority.authorize_named(&token, person, FilePermission::Edit, 2),
                Err(ShareAuthorityError::InsufficientPermission)
            ));
            authority.revoke(token.id).unwrap();
            assert!(matches!(
                authority.authorize_named(&token, person, FilePermission::View, 3),
                Err(ShareAuthorityError::Revoked)
            ));
        }
    }

    #[test]
    fn untrusted_skewed_or_revoked_authority_fails_closed() {
        // Phase `release_evidence_forward_clock_skew_expires_grants_fail_closed`.
        {
            let (dir, issuer, invite) = limited_invite();
            let authority = ShareAuthority::open(dir.path().join("shares.json")).unwrap();
            authority.register(&invite.token, &[issuer], 9_999).unwrap();
            assert!(matches!(
                authority.redeem(&invite.token, &invite.secret, actor(10), 10_000),
                Err(ShareAuthorityError::Invite(_))
            ));
            let projection = authority.projection(invite.token.record_id, 10_000);
            assert_eq!(projection.grants.len(), 1);
            assert_eq!(projection.grants[0].status, ShareGrantStatus::Expired);
        }
        // Phase `revocation_and_untrusted_issuers_fail_closed`.
        {
            let (dir, issuer, invite) = limited_invite();
            let authority = ShareAuthority::open(dir.path().join("shares.json")).unwrap();
            assert!(matches!(
                authority.register(&invite.token, &[], 1),
                Err(ShareAuthorityError::UntrustedIssuer)
            ));
            authority.register(&invite.token, &[issuer], 1).unwrap();
            assert!(authority.revoke(invite.token.id).unwrap());
            assert!(matches!(
                authority.redeem(&invite.token, &invite.secret, actor(50), 2),
                Err(ShareAuthorityError::Revoked)
            ));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn authority_projection_is_redacted_bounded_and_validated_on_restart() {
        // Phase `redacted_projection_survives_restart_and_tracks_live_state`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("shares.json");
            let (issuer_sk, issuer) = collab_types::generate_keypair();
            let (_, person) = collab_types::generate_keypair();
            let workspace_id = Uuid::new_v4();
            let record_id = FileRecordId::new();
            let named = mint_file_invite(
                FileInviteSpec {
                    workspace_id,
                    record_id,
                    subject: FileInviteSubject::Person(person),
                    permission: FilePermission::Edit,
                    expires_at_ms: Some(500),
                    max_redemptions: None,
                },
                &issuer_sk,
            )
            .unwrap();
            let mut rng = UnwrapErr(SysRng);
            let bearer = mint_bearer_file_invite(
                FileInviteSpec {
                    workspace_id,
                    record_id,
                    subject: FileInviteSubject::Bearer {
                        secret_hash: [0; 32],
                    },
                    permission: FilePermission::View,
                    expires_at_ms: Some(300),
                    max_redemptions: Some(1),
                },
                &issuer_sk,
                &mut rng,
            )
            .unwrap();

            let authority = ShareAuthority::open(&path).unwrap();
            authority.register(&named, &[issuer], 1).unwrap();
            authority.register(&bearer.token, &[issuer], 1).unwrap();
            let initial = authority.projection(record_id, 10);
            assert_eq!(initial.grants.len(), 2);
            let named_projection = initial
                .grants
                .iter()
                .find(|grant| grant.grant_id == named.id)
                .unwrap();
            assert_eq!(
                named_projection.subject,
                ShareSubjectProjection::Person { person }
            );
            assert_eq!(named_projection.permission, FilePermission::Edit);
            assert_eq!(named_projection.status, ShareGrantStatus::Active);
            assert!(!named_projection.revoked);
            assert!(!named_projection.expired);
            assert!(!named_projection.used);

            let bytes = initial.to_xattr_bytes().unwrap();
            let json = String::from_utf8(bytes.clone()).unwrap();
            assert!(!json.contains("secret"));
            assert!(!json.contains(&hex::encode(bearer.secret)));
            assert_eq!(ShareProjectionV1::parse_xattr(&bytes).unwrap(), initial);

            authority
                .redeem(&bearer.token, &bearer.secret, actor(60), 20)
                .unwrap();
            authority.revoke(named.id).unwrap();
            drop(authority);

            let reopened = ShareAuthority::open(path).unwrap();
            let live = reopened.projection(record_id, 25);
            assert_eq!(live.grants.len(), 2);
            assert_eq!(
                live.grants
                    .iter()
                    .find(|grant| grant.grant_id == named.id)
                    .unwrap()
                    .status,
                ShareGrantStatus::Revoked
            );
            let link = live
                .grants
                .iter()
                .find(|grant| grant.grant_id == bearer.token.id)
                .unwrap();
            assert_eq!(link.subject, ShareSubjectProjection::BearerLink);
            assert_eq!(link.redemptions, 1);
            assert_eq!(link.max_redemptions, Some(1));
            assert!(link.used);
            assert!(link.exhausted);
            assert_eq!(link.status, ShareGrantStatus::Exhausted);

            let expired = reopened.projection(record_id, 300);
            assert_eq!(
                expired
                    .grants
                    .iter()
                    .find(|grant| grant.grant_id == bearer.token.id)
                    .unwrap()
                    .status,
                ShareGrantStatus::Expired
            );
        }
        // Phase `authority_enforces_projection_cardinality_bound`.
        {
            let dir = tempfile::tempdir().unwrap();
            let (issuer_sk, issuer) = collab_types::generate_keypair();
            let (_, person) = collab_types::generate_keypair();
            let workspace_id = Uuid::new_v4();
            let record_id = FileRecordId::new();
            let authority = ShareAuthority::open(dir.path().join("shares.json")).unwrap();
            for _ in 0..MAX_SHARES_PER_RECORD {
                let token = mint_file_invite(
                    FileInviteSpec {
                        workspace_id,
                        record_id,
                        subject: FileInviteSubject::Person(person),
                        permission: FilePermission::View,
                        expires_at_ms: None,
                        max_redemptions: None,
                    },
                    &issuer_sk,
                )
                .unwrap();
                authority.register(&token, &[issuer], 1).unwrap();
            }
            let overflow = mint_file_invite(
                FileInviteSpec {
                    workspace_id,
                    record_id,
                    subject: FileInviteSubject::Person(person),
                    permission: FilePermission::View,
                    expires_at_ms: None,
                    max_redemptions: None,
                },
                &issuer_sk,
            )
            .unwrap();
            assert!(matches!(
                authority.register(&overflow, &[issuer], 1),
                Err(ShareAuthorityError::TooManyRecordGrants)
            ));
            let projection = authority.projection(record_id, 2);
            assert_eq!(projection.grants.len(), MAX_SHARES_PER_RECORD);
            assert!(projection.to_xattr_bytes().unwrap().len() <= MAX_SHARE_PROJECTION_BYTES);
        }
        // Phase `restart_rejects_inconsistent_and_oversized_authority_state`.
        {
            let (dir, issuer, invite) = limited_invite();
            let path = dir.path().join("shares.json");
            let authority = ShareAuthority::open(&path).unwrap();
            authority.register(&invite.token, &[issuer], 1).unwrap();
            authority
                .redeem(&invite.token, &invite.secret, actor(70), 2)
                .unwrap();
            drop(authority);

            let mut value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            let row = value["grants"]
                .get_mut(invite.token.id.to_string())
                .unwrap();
            let duplicate = row["redemption_actors"][0].clone();
            row["redemption_actors"]
                .as_array_mut()
                .unwrap()
                .push(duplicate);
            std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(matches!(
                ShareAuthority::open(&path),
                Err(ShareAuthorityError::InvalidState)
            ));

            let oversized = dir.path().join("oversized.json");
            let file = std::fs::File::create(&oversized).unwrap();
            file.set_len(MAX_SHARE_AUTHORITY_FILE_BYTES + 1).unwrap();
            assert!(matches!(
                ShareAuthority::open(oversized),
                Err(ShareAuthorityError::StateTooLarge)
            ));
        }
    }
}
