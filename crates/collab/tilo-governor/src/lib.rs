//! The governor's side of the workspace-join exchange, once for every host
//! that governs rooms: a relay, a cloud host or a person's own device.
//!
//! What differs between them is only where a room lives and how a change
//! to it becomes durable and reaches its members: a [`GovernorStore`].
//! Everything a governor decides (which invitation a device redeems, what
//! a challenge binds, who is admitted, which certificate a member holds)
//! is decided here, against the room's [`JoinLedger`] and its committed
//! governance, so a room is joined the same way whoever hosts it.

use ed25519_dalek::SigningKey;
use std::future::Future;
use std::time::{Duration, SystemTime};
use tilo_delivery_protocol::workspace_join::WorkspaceJoinService;
use tilo_peer_state::StatePeer;
use tilo_peer_state::join::{
    JoinBootstrap, JoinChallengeResponse, JoinCompleteRequest, JoinInspection,
    JoinTransportRequest, JoinTransportResponse, MembershipChallengeResponse,
    MembershipCompleteRequest,
};
use tilo_peer_state::join_ledger::{
    Governed, JoinError, JoinLedger, Redeeming, invitation_room, request_digest,
};

/// One room as a governor holds it while it decides: locked, so nothing
/// else changes it meanwhile.
pub trait RoomTxn {
    /// The room's id.
    fn room(&self) -> &str;
    /// The room as committed now, as its join ledger reads it.
    fn governed(&self) -> Governed<'_>;
    /// The room's join ledger as it stands, staged changes included.
    fn ledger(&self) -> &JoinLedger;
    /// Stage `ledger` as the room's join ledger. The store makes it durable
    /// before the answer it belongs to is given.
    fn stage_ledger(&mut self, ledger: JoinLedger);
    /// Commit `peer` as the room's next governance, signed at `now`, and,
    /// against the room as then committed, `ledger`'s answer as its join
    /// ledger: durable when this returns, and sent to the room's members
    /// before the answer it belongs to is given. Nothing changes when
    /// `ledger` refuses.
    ///
    /// # Errors
    /// `ledger` refuses, or the change cannot be committed.
    fn commit_governance(
        &mut self,
        peer: StatePeer,
        now: u64,
        ledger: &mut dyn FnMut(&Governed<'_>) -> Result<JoinLedger, JoinError>,
    ) -> Result<(), JoinError>;
}

/// Where a governor's rooms live: the one thing a relay and a device
/// governing rooms do differently.
pub trait GovernorStore: Send + Sync {
    /// The key the governor signs its rooms' governance with.
    fn key(&self) -> &SigningKey;

    /// Fresh randomness for a challenge.
    ///
    /// # Errors
    /// There is no randomness to give.
    fn nonce(&self) -> Result<[u8; 32], JoinError>;

    /// Run `decide` on `room`, locked, then make what it staged durable
    /// and send what it committed, and only then answer what `decide`
    /// answered.
    ///
    /// # Errors
    /// The room is not governed here, `decide` refuses, or its change
    /// cannot be made durable.
    fn with_room<T: Send>(
        &self,
        room: &str,
        decide: impl FnOnce(&mut dyn RoomTxn) -> Result<T, JoinError> + Send,
    ) -> impl Future<Output = Result<T, JoinError>> + Send;
}

/// The governor of the rooms in `S`: answers the workspace-join exchange.
#[derive(Clone, Debug)]
pub struct Governor<S>(pub S);

/// `now`, in nanoseconds since the Unix epoch, as a clock reading.
fn at(now: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_nanos(now))
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// The room a join request is about, read without trusting it: the
/// ledger then verifies it against what that room issued.
///
/// # Errors
/// The request carries an invitation that names no room.
pub fn requested_room(request: &JoinTransportRequest, now: u64) -> Result<String, JoinError> {
    Ok(match request {
        JoinTransportRequest::Inspect { signed_invitation } => {
            invitation_room(signed_invitation, now)?
        }
        JoinTransportRequest::Challenge(request) => {
            invitation_room(&request.signed_invitation, now)?
        }
        JoinTransportRequest::Complete(request) => request.proof.challenge.room.clone(),
        JoinTransportRequest::MembershipChallenge(request) => request.room.clone(),
        JoinTransportRequest::MembershipComplete(request) => request.proof.challenge.room.clone(),
    })
}

/// Admit the verified `joins` to `room` in one governance change signed
/// with `key`. Each is checked on its own and refused alone; everyone
/// admitted is then committed, recorded in the room's ledger and sent,
/// together. Answers each join in order.
///
/// # Errors
/// The change cannot be committed or sent.
pub fn admit(
    room: &mut dyn RoomTxn,
    joins: &[Redeeming<'_>],
    key: &SigningKey,
    now: u64,
) -> Result<Vec<Result<JoinBootstrap, JoinError>>, JoinError> {
    let mut candidate = room.governed().peer.clone();
    let refusals = room.ledger().admit(&mut candidate, joins, key, now);
    for (join, refused) in joins.iter().zip(&refusals) {
        if let Some(error) = refused {
            tracing::warn!(
                room = %room.room(),
                invite_id = %hex::encode(join.redemption.join().invite_id()),
                person = %hex::encode(join.redemption.certificate().user_public_key),
                outcome = "replay_rejected",
                reason = %error,
                "workspace invitation redemption rejected"
            );
        }
    }
    if refusals.iter().all(Option::is_some) {
        return Ok(refusals.into_iter().flatten().map(Err).collect());
    }
    let mut delivered = Vec::new();
    let ledger = room.ledger().clone();
    room.commit_governance(candidate, now, &mut |governed| {
        let mut completed = ledger.clone();
        delivered = completed.complete(governed, joins, &refusals)?;
        Ok(completed)
    })?;
    for bootstrap in delivered.iter().flatten() {
        tracing::info!(
            room = %bootstrap.room,
            invite_id = %hex::encode(bootstrap.invite_id),
            person = %hex::encode(bootstrap.device_cert.user_public_key),
            device = %hex::encode(bootstrap.device_cert.device_public_key),
            role = ?bootstrap.role,
            outcome = "redeemed",
            "workspace invitation redemption committed"
        );
    }
    Ok(delivered)
}

impl<S: GovernorStore> Governor<S> {
    /// Answer one workspace-join request from the device at
    /// `remote_endpoint`, its authenticated transport key.
    pub async fn answer(
        &self,
        remote_endpoint: [u8; 32],
        request: JoinTransportRequest,
        now: u64,
    ) -> JoinTransportResponse {
        self.decide(remote_endpoint, request, now)
            .await
            .unwrap_or_else(|error| JoinTransportResponse::Error {
                code: error.code(),
                message: error.to_string(),
            })
    }

    /// Decide one workspace-join request from the device at
    /// `remote_endpoint`: its answer, or why it is refused.
    ///
    /// # Errors
    /// The request is refused, or its change cannot be made durable.
    pub async fn decide(
        &self,
        remote_endpoint: [u8; 32],
        request: JoinTransportRequest,
        now: u64,
    ) -> Result<JoinTransportResponse, JoinError> {
        let room = requested_room(&request, now)?;
        let store = &self.0;
        Ok(match request {
            JoinTransportRequest::Inspect { signed_invitation } => {
                store
                    .with_room(&room, |room| {
                        let join =
                            room.ledger()
                                .inspect(&room.governed(), &signed_invitation, now)?;
                        Ok(JoinTransportResponse::Inspection(JoinInspection {
                            invite_id: join.invite_id(),
                            grant: join.grant().clone(),
                        }))
                    })
                    .await?
            }
            JoinTransportRequest::Challenge(request) => {
                let nonce = store.nonce()?;
                store
                    .with_room(&room, |room| {
                        let mut ledger = room.ledger().clone();
                        let challenge = ledger.challenge(
                            &room.governed(),
                            remote_endpoint,
                            request,
                            nonce,
                            now,
                        )?;
                        room.stage_ledger(ledger);
                        Ok(JoinTransportResponse::Challenge(JoinChallengeResponse {
                            challenge,
                        }))
                    })
                    .await?
            }
            JoinTransportRequest::Complete(request) => {
                let key = store.key().clone();
                store
                    .with_room(&room, |room| {
                        complete(room, remote_endpoint, &request, &key, now)
                            .map(JoinTransportResponse::Complete)
                    })
                    .await?
            }
            JoinTransportRequest::MembershipChallenge(request) => {
                let nonce = store.nonce()?;
                store
                    .with_room(&room, |room| {
                        let mut ledger = room.ledger().clone();
                        let challenge = ledger.membership_challenge(
                            &room.governed(),
                            remote_endpoint,
                            request,
                            nonce,
                            now,
                        )?;
                        room.stage_ledger(ledger);
                        Ok(JoinTransportResponse::MembershipChallenge(
                            MembershipChallengeResponse { challenge },
                        ))
                    })
                    .await?
            }
            JoinTransportRequest::MembershipComplete(request) => {
                let key = store.key().clone();
                store
                    .with_room(&room, |room| {
                        membership_complete(room, remote_endpoint, &request, &key, now)
                            .map(JoinTransportResponse::MembershipComplete)
                    })
                    .await?
            }
        })
    }
}

/// Admit the device at `remote_endpoint` that proved `request`, or answer
/// again what its earlier, identical request was answered.
fn complete(
    room: &mut dyn RoomTxn,
    remote_endpoint: [u8; 32],
    request: &JoinCompleteRequest,
    key: &SigningKey,
    now: u64,
) -> Result<JoinBootstrap, JoinError> {
    let request_id = request.proof.challenge.request_id;
    let proof_digest = request_digest(&request.proof)?;
    if let Some(prior) = room.ledger().completed(
        &room.governed(),
        remote_endpoint,
        request_id,
        proof_digest,
        now,
    )? {
        return Ok(prior);
    }
    let redemption =
        room.ledger()
            .redeem(&room.governed(), remote_endpoint, &request.proof, now)?;
    let joins = [Redeeming {
        remote_endpoint,
        request_id,
        proof_digest,
        redemption: &redemption,
        name: None,
    }];
    admit(room, &joins, key, now)?
        .pop()
        .unwrap_or_else(|| Err(JoinError::new("join bootstrap was not built")))
}

/// The room's current state for a member that proved its membership. A
/// member proving with its renewed certificate first has the room hold
/// that certificate in place of the one it replaces, committed and sent
/// before the answer.
fn membership_complete(
    room: &mut dyn RoomTxn,
    remote_endpoint: [u8; 32],
    request: &MembershipCompleteRequest,
    key: &SigningKey,
    now: u64,
) -> Result<JoinBootstrap, JoinError> {
    let renewal =
        room.ledger()
            .membership_renewal(&room.governed(), remote_endpoint, request, now)?;
    if let Some(renewed) = renewal {
        let mut candidate = room.governed().peer.clone();
        candidate
            .governance_mut()
            .renew_member_certificate(&renewed, key, at(now))?;
        let ledger = room.ledger().clone();
        room.commit_governance(candidate, now, &mut |_| Ok(ledger.clone()))?;
        tracing::info!(
            room = %room.room(),
            person = %hex::encode(renewed.user_public_key),
            device = %hex::encode(renewed.device_public_key),
            outcome = "renewed",
            "member device certificate renewed"
        );
    }
    room.ledger()
        .membership_complete(&room.governed(), remote_endpoint, request, now)
}

impl<S> WorkspaceJoinService for Governor<S>
where
    S: GovernorStore + Clone + std::fmt::Debug + 'static,
{
    async fn handle(
        &self,
        remote_endpoint: [u8; 32],
        request: JoinTransportRequest,
    ) -> JoinTransportResponse {
        self.answer(remote_endpoint, request, tilo_clock::unix_now_ns())
            .await
    }
}
