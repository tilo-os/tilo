//! Live, record-scoped authorization for authenticated sharing sessions.
//!
//! A transport authentication result is deliberately not a durable access
//! decision. [`ShareSession`] binds it to a signed file invite, but rechecks
//! the device certificate, a pluggable live identity source, invite expiry,
//! permission, record scope and authority revocation for every operation.

use std::sync::Arc;

use collab_types::Ed25519PublicKey;
use fs_protocol::{
    BearerFileInvite, FileActor, FileInviteSubject, FileInviteV1, FilePermission, FileRecordId,
};
use thiserror::Error;
use tilo_clock::Clock;
use tilo_user_identity::verify_device_cert_at;
use uuid::Uuid;

use crate::clock::DaemonClock;
use crate::peer_auth::AuthenticatedFsPeer;
use crate::record_replication::{RecordAction, RecordAuthorizer};
use crate::share_authority::{Redemption, ShareAuthority, ShareAuthorityError};

/// Resolves whether a root-signed device certification is still current.
///
/// Production implementations should query the identity directory and its
/// current revocation set. This method is invoked for every record operation;
/// implementations must not cache an admission decision for the lifetime of
/// a connection.
pub trait LiveActorCertification: Send + Sync {
    fn certification_is_current(&self, peer: &AuthenticatedFsPeer, now_ns: u64) -> bool;
}

enum SessionGrant {
    Named,
    Bearer(Redemption),
}

/// An authenticated peer's live authority over exactly one stable record.
pub struct ShareSession {
    workspace_id: Uuid,
    record_id: FileRecordId,
    peer: AuthenticatedFsPeer,
    token: FileInviteV1,
    grant: SessionGrant,
    authority: Arc<ShareAuthority>,
    certification: Arc<dyn LiveActorCertification>,
    clock: Arc<dyn Clock>,
}

impl ShareSession {
    pub(crate) fn authenticated_peer(&self) -> &AuthenticatedFsPeer {
        &self.peer
    }

    /// Open a named-recipient session using the system clock.
    ///
    /// # Errors
    ///
    /// Returns an error when peer, workspace, invite, or live authorization
    /// validation fails.
    pub fn open_named(
        workspace_id: Uuid,
        peer: AuthenticatedFsPeer,
        token: FileInviteV1,
        authority: Arc<ShareAuthority>,
        certification: Arc<dyn LiveActorCertification>,
    ) -> Result<Self, ShareSessionError> {
        Self::open_named_with_clock(
            workspace_id,
            peer,
            token,
            authority,
            certification,
            Arc::new(DaemonClock),
        )
    }

    /// Open a named-recipient session with an explicit clock.
    ///
    /// # Errors
    ///
    /// Returns an error when peer, workspace, invite, or live authorization
    /// validation fails.
    pub fn open_named_with_clock(
        workspace_id: Uuid,
        peer: AuthenticatedFsPeer,
        token: FileInviteV1,
        authority: Arc<ShareAuthority>,
        certification: Arc<dyn LiveActorCertification>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, ShareSessionError> {
        if !matches!(token.subject, FileInviteSubject::Person(_)) {
            return Err(ShareSessionError::ExpectedNamedGrant);
        }
        let session = Self {
            workspace_id,
            record_id: token.record_id,
            peer,
            token,
            grant: SessionGrant::Named,
            authority,
            certification,
            clock,
        };
        session.authorize(&session.actor(), session.record_id, RecordAction::Read)?;
        Ok(session)
    }

    /// Redeem and open a bearer session using the system clock.
    ///
    /// # Errors
    ///
    /// Returns an error when peer validation, redemption, or live
    /// authorization fails.
    pub fn open_bearer(
        workspace_id: Uuid,
        peer: AuthenticatedFsPeer,
        invite: BearerFileInvite,
        authority: Arc<ShareAuthority>,
        certification: Arc<dyn LiveActorCertification>,
    ) -> Result<Self, ShareSessionError> {
        Self::open_bearer_with_clock(
            workspace_id,
            peer,
            invite,
            authority,
            certification,
            Arc::new(DaemonClock),
        )
    }

    /// Redeem and open a bearer session with an explicit clock. Redemption is
    /// performed online before the session exists; the secret is not retained.
    ///
    /// # Errors
    ///
    /// Returns an error when peer validation, redemption, or live
    /// authorization fails.
    pub fn open_bearer_with_clock(
        workspace_id: Uuid,
        peer: AuthenticatedFsPeer,
        invite: BearerFileInvite,
        authority: Arc<ShareAuthority>,
        certification: Arc<dyn LiveActorCertification>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, ShareSessionError> {
        let now_ms = clock.now_ms();
        validate_workspace(workspace_id, &invite.token)?;
        validate_peer(&peer, certification.as_ref(), now_ms)?;
        let actor = FileActor {
            person: Ed25519PublicKey::from_bytes(peer.user_public_key),
            device: Ed25519PublicKey::from_bytes(peer.device_public_key),
        };
        let redemption = authority.redeem(&invite.token, &invite.secret, actor, now_ms)?;
        let session = Self {
            workspace_id,
            record_id: invite.token.record_id,
            peer,
            token: invite.token,
            grant: SessionGrant::Bearer(redemption),
            authority,
            certification,
            clock,
        };
        session.authorize(&session.actor(), session.record_id, RecordAction::Read)?;
        Ok(session)
    }

    pub fn actor(&self) -> FileActor {
        FileActor {
            person: Ed25519PublicKey::from_bytes(self.peer.user_public_key),
            device: Ed25519PublicKey::from_bytes(self.peer.device_public_key),
        }
    }

    pub fn workspace_id(&self) -> Uuid {
        self.workspace_id
    }

    pub fn record_id(&self) -> FileRecordId {
        self.record_id
    }

    /// Stable signed grant identifier backing this session. The bearer secret
    /// is deliberately never exposed or retained here.
    pub fn grant_id(&self) -> Uuid {
        self.token.id
    }

    /// Rich authorization result used by protocol boundaries. This performs
    /// all live checks and is also the implementation behind
    /// [`RecordAuthorizer`].
    ///
    /// # Errors
    ///
    /// Returns an error when actor, record, peer certification, grant, or
    /// permission validation fails.
    pub fn authorize(
        &self,
        actor: &FileActor,
        record_id: FileRecordId,
        action: RecordAction,
    ) -> Result<(), ShareSessionError> {
        if *actor != self.actor() {
            return Err(ShareSessionError::WrongActor);
        }
        if record_id != self.record_id {
            return Err(ShareSessionError::WrongRecord);
        }
        let now_ms = self.clock.now_ms();
        validate_workspace(self.workspace_id, &self.token)?;
        validate_peer(&self.peer, self.certification.as_ref(), now_ms)?;
        let required = match action {
            RecordAction::Read => FilePermission::View,
            RecordAction::Write => FilePermission::Edit,
        };
        match self.grant {
            SessionGrant::Named => self.authority.authorize_named(
                &self.token,
                Ed25519PublicKey::from_bytes(self.peer.user_public_key),
                required,
                now_ms,
            )?,
            SessionGrant::Bearer(redemption) => {
                if redemption.actor != *actor {
                    return Err(ShareSessionError::WrongActor);
                }
                self.authority
                    .authorize_redeemed(&self.token, redemption, required, now_ms)?;
            }
        }
        Ok(())
    }
}

impl RecordAuthorizer for ShareSession {
    fn actor_is_certified(&self, actor: &FileActor) -> bool {
        if *actor != self.actor() {
            return false;
        }
        validate_peer(&self.peer, self.certification.as_ref(), self.clock.now_ms()).is_ok()
    }

    fn allows(&self, actor: &FileActor, record_id: FileRecordId, action: RecordAction) -> bool {
        self.authorize(actor, record_id, action).is_ok()
    }
}

fn validate_workspace(workspace_id: Uuid, token: &FileInviteV1) -> Result<(), ShareSessionError> {
    if token.workspace_id != workspace_id {
        return Err(ShareSessionError::WrongWorkspace);
    }
    Ok(())
}

fn validate_peer(
    peer: &AuthenticatedFsPeer,
    certification: &dyn LiveActorCertification,
    now_ms: u64,
) -> Result<(), ShareSessionError> {
    if peer.user_public_key != peer.cert.user_public_key
        || peer.device_public_key != peer.cert.device_public_key
    {
        return Err(ShareSessionError::PeerIdentityMismatch);
    }
    let current_time_ns = now_ms.saturating_mul(1_000_000);
    if peer.cert.issued_at_ns > current_time_ns
        || verify_device_cert_at(&peer.cert, current_time_ns).is_err()
    {
        return Err(ShareSessionError::UncertifiedActor);
    }
    if !certification.certification_is_current(peer, current_time_ns) {
        return Err(ShareSessionError::UncertifiedActor);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum ShareSessionError {
    #[error("invite belongs to another workspace")]
    WrongWorkspace,
    #[error("operation belongs to another stable record")]
    WrongRecord,
    #[error("operation actor does not match the authenticated peer")]
    WrongActor,
    #[error("authenticated peer fields do not match its device certificate")]
    PeerIdentityMismatch,
    #[error("peer device certification is invalid, expired, or currently revoked")]
    UncertifiedActor,
    #[error("a named-recipient invite is required")]
    ExpectedNamedGrant,
    #[error("share authority denied access: {0}")]
    Authority(#[from] ShareAuthorityError),
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use ed25519_dalek::SigningKey;
    use fs_protocol::{FileInviteSpec, mint_bearer_file_invite, mint_file_invite};
    use getrandom::{SysRng, rand_core::UnwrapErr};
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

    use tilo_clock::ManualClock;

    use super::*;

    const NOW_MS: u64 = 2_000;

    struct LiveCertification(AtomicBool);

    impl LiveActorCertification for LiveCertification {
        fn certification_is_current(&self, _peer: &AuthenticatedFsPeer, _now_ns: u64) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        issuer: SigningKey,
        issuer_public: Ed25519PublicKey,
        person: SigningKey,
        peer: AuthenticatedFsPeer,
        workspace: Uuid,
        record: FileRecordId,
        authority: Arc<ShareAuthority>,
        live: Arc<LiveCertification>,
        clock: Arc<ManualClock>,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let issuer = SigningKey::from_bytes(&[1; 32]);
            let person = SigningKey::from_bytes(&[2; 32]);
            let device = SigningKey::from_bytes(&[3; 32]);
            let mut cert = DeviceCert {
                user_public_key: person.verifying_key().to_bytes(),
                device_public_key: device.verifying_key().to_bytes(),
                device_label: "laptop".into(),
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: 1_000_000_000,
                expires_at_ns: Some(10_000_000_000),
                signature: [0; 64],
            };
            sign_device_cert(&mut cert, &person).unwrap();
            let peer = AuthenticatedFsPeer {
                user_public_key: cert.user_public_key,
                device_public_key: cert.device_public_key,
                device_label: cert.device_label.clone(),
                capabilities: cert.capabilities.clone(),
                cert,
            };
            let issuer_public = Ed25519PublicKey::from_bytes(issuer.verifying_key().to_bytes());
            let workspace = Uuid::new_v4();
            let record = FileRecordId::new();
            let authority = Arc::new(ShareAuthority::open(dir.path().join("shares.json")).unwrap());
            Self {
                _dir: dir,
                issuer,
                issuer_public,
                person,
                peer,
                workspace,
                record,
                authority,
                live: Arc::new(LiveCertification(AtomicBool::new(true))),
                clock: Arc::new(ManualClock::new(NOW_MS)),
            }
        }

        fn named_token(
            &self,
            person: Ed25519PublicKey,
            permission: FilePermission,
        ) -> FileInviteV1 {
            mint_file_invite(
                FileInviteSpec {
                    workspace_id: self.workspace,
                    record_id: self.record,
                    subject: FileInviteSubject::Person(person),
                    permission,
                    expires_at_ms: Some(5_000),
                    max_redemptions: None,
                },
                &self.issuer,
            )
            .unwrap()
        }

        fn register(&self, token: &FileInviteV1) {
            self.authority
                .register(token, &[self.issuer_public], NOW_MS)
                .unwrap();
        }

        fn open_named(&self, token: FileInviteV1) -> Result<ShareSession, ShareSessionError> {
            ShareSession::open_named_with_clock(
                self.workspace,
                self.peer.clone(),
                token,
                self.authority.clone(),
                self.live.clone(),
                self.clock.clone(),
            )
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn share_sessions_bind_subject_scope_and_keep_live_checks() {
        // Phase `named_session_rejects_wrong_person_and_device`.
        {
            let fixture = Fixture::new();
            let stranger = Ed25519PublicKey::from_bytes([99; 32]);
            let token = fixture.named_token(stranger, FilePermission::View);
            fixture.register(&token);
            assert!(matches!(
                fixture.open_named(token),
                Err(ShareSessionError::Authority(
                    ShareAuthorityError::WrongRecipient
                ))
            ));

            let token = fixture.named_token(
                Ed25519PublicKey::from_bytes(fixture.person.verifying_key().to_bytes()),
                FilePermission::View,
            );
            fixture.register(&token);
            let mut peer = fixture.peer.clone();
            peer.device_public_key = [44; 32];
            assert!(matches!(
                ShareSession::open_named_with_clock(
                    fixture.workspace,
                    peer,
                    token,
                    fixture.authority.clone(),
                    fixture.live.clone(),
                    fixture.clock,
                ),
                Err(ShareSessionError::PeerIdentityMismatch)
            ));
        }
        // Phase `view_cannot_write_and_record_scope_is_exact`.
        {
            let fixture = Fixture::new();
            let token = fixture.named_token(
                Ed25519PublicKey::from_bytes(fixture.person.verifying_key().to_bytes()),
                FilePermission::View,
            );
            fixture.register(&token);
            let session = fixture.open_named(token).unwrap();
            let actor = session.actor();
            assert!(
                session
                    .authorize(&actor, fixture.record, RecordAction::Read)
                    .is_ok()
            );
            assert!(matches!(
                session.authorize(&actor, fixture.record, RecordAction::Write),
                Err(ShareSessionError::Authority(
                    ShareAuthorityError::InsufficientPermission
                ))
            ));
            assert!(matches!(
                session.authorize(&actor, FileRecordId::new(), RecordAction::Read),
                Err(ShareSessionError::WrongRecord)
            ));
        }
        // Phase `grant_and_device_revocation_take_effect_after_session_creation`.
        {
            let fixture = Fixture::new();
            let token = fixture.named_token(
                Ed25519PublicKey::from_bytes(fixture.person.verifying_key().to_bytes()),
                FilePermission::Edit,
            );
            fixture.register(&token);
            let session = fixture.open_named(token.clone()).unwrap();
            let actor = session.actor();
            fixture.authority.revoke(token.id).unwrap();
            assert!(!session.allows(&actor, fixture.record, RecordAction::Read));

            let fixture = Fixture::new();
            let token = fixture.named_token(
                Ed25519PublicKey::from_bytes(fixture.person.verifying_key().to_bytes()),
                FilePermission::Edit,
            );
            fixture.register(&token);
            let session = fixture.open_named(token).unwrap();
            let actor = session.actor();
            fixture.live.0.store(false, Ordering::SeqCst);
            assert!(!session.actor_is_certified(&actor));
            assert!(!session.allows(&actor, fixture.record, RecordAction::Read));
        }
        // Phase `expiry_is_rechecked_after_session_creation`.
        {
            let fixture = Fixture::new();
            let token = fixture.named_token(
                Ed25519PublicKey::from_bytes(fixture.person.verifying_key().to_bytes()),
                FilePermission::View,
            );
            fixture.register(&token);
            let session = fixture.open_named(token).unwrap();
            let actor = session.actor();
            fixture.clock.set(5_000);
            assert!(!session.allows(&actor, fixture.record, RecordAction::Read));
        }
        // Phase `bearer_one_use_allows_same_actor_reconnect_and_keeps_live_checks`.
        {
            let fixture = Fixture::new();
            let mut rng = UnwrapErr(SysRng);
            let invite = mint_bearer_file_invite(
                FileInviteSpec {
                    workspace_id: fixture.workspace,
                    record_id: fixture.record,
                    subject: FileInviteSubject::Bearer {
                        secret_hash: [0; 32],
                    },
                    permission: FilePermission::View,
                    expires_at_ms: Some(5_000),
                    max_redemptions: Some(1),
                },
                &fixture.issuer,
                &mut rng,
            )
            .unwrap();
            fixture.register(&invite.token);
            fixture.live.0.store(false, Ordering::SeqCst);
            assert!(matches!(
                ShareSession::open_bearer_with_clock(
                    fixture.workspace,
                    fixture.peer.clone(),
                    invite.clone(),
                    fixture.authority.clone(),
                    fixture.live.clone(),
                    fixture.clock.clone(),
                ),
                Err(ShareSessionError::UncertifiedActor)
            ));
            fixture.live.0.store(true, Ordering::SeqCst);
            let session = ShareSession::open_bearer_with_clock(
                fixture.workspace,
                fixture.peer.clone(),
                invite.clone(),
                fixture.authority.clone(),
                fixture.live.clone(),
                fixture.clock.clone(),
            )
            .unwrap();
            ShareSession::open_bearer_with_clock(
                fixture.workspace,
                fixture.peer.clone(),
                invite,
                fixture.authority.clone(),
                fixture.live.clone(),
                fixture.clock.clone(),
            )
            .expect("the same certified actor can reconnect without another redemption");
            let actor = session.actor();
            assert!(session.allows(&actor, fixture.record, RecordAction::Read));
            fixture.authority.revoke(session.token.id).unwrap();
            assert!(!session.allows(&actor, fixture.record, RecordAction::Read));
        }
    }
}
