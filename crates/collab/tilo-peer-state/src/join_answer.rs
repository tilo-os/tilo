//! Answering the workspace-join exchange for a workspace this device holds.
//!
//! What differs between holders is only where a workspace lives and how a change
//! to it becomes durable and reaches its members: a [`JoinWorkspaces`].
//! Everything decided (which invitation a device redeems, what a challenge
//! binds, who is admitted, which certificate a member holds) is decided
//! here, against the workspace's [`JoinLedger`] and its committed governance, so
//! a workspace is joined the same way whichever device answers.

use crate::StatePeer;
use crate::join::{
    JoinBootstrap, JoinChallengeResponse, JoinCompleteRequest, JoinInspection,
    JoinTransportRequest, JoinTransportResponse, MembershipChallengeResponse,
    MembershipCompleteRequest,
};
use crate::join::WorkspaceId;
use crate::join_ledger::{
    Governed, JoinError, JoinLedger, Redeeming, invitation_workspace, request_digest,
};
use crate::owner::write::{SIGNED_RUNS, Signatures, UNSIGNED};
use collab_types::signer::DeviceSigner;
use std::future::Future;
use std::time::{Duration, SystemTime};
use tilo_delivery_protocol::workspace_join::WorkspaceJoinService;
use tilo_user_identity::DeviceCert;

/// One workspace as its holder keeps it while it decides: locked, so nothing
/// else changes it meanwhile.
pub trait WorkspaceTxn {
    /// The workspace's id.
    fn workspace(&self) -> &str;
    /// The workspace as committed now, as its join ledger reads it.
    fn governed(&self) -> Governed<'_>;
    /// The workspace's join ledger as it stands, staged changes included.
    fn ledger(&self) -> &JoinLedger;
    /// Stage `ledger` as the workspace's join ledger. The store makes it durable
    /// before the answer it belongs to is given.
    fn stage_ledger(&mut self, ledger: JoinLedger);
    /// Commit `peer` as the workspace's next governance, signed at `now`, and,
    /// against the workspace as then committed, `ledger`'s answer as its join
    /// ledger: durable when this returns, and sent to the workspace's members
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

    /// [`Self::commit_governance`] of a change that only admits people
    /// through their invitations ([`admit`]), each perhaps named as they
    /// chose, and changes nothing else. A store may publish it as an
    /// addition to earlier admissions rather than starting the workspace over;
    /// by default it is committed like any other change.
    ///
    /// # Errors
    /// As [`Self::commit_governance`].
    fn commit_admission(
        &mut self,
        peer: StatePeer,
        now: u64,
        ledger: &mut dyn FnMut(&Governed<'_>) -> Result<JoinLedger, JoinError>,
    ) -> Result<(), JoinError> {
        self.commit_governance(peer, now, ledger)
    }
}

/// Where the workspaces a join is answered for live.
pub trait JoinWorkspaces: Send + Sync {
    /// This device's custody of the key it signs the workspaces' governance with.
    fn signer(&self) -> &dyn DeviceSigner;

    /// Fresh randomness for a challenge.
    fn nonce(&self) -> Result<[u8; 32], JoinError>;

    /// Run `decide` on `workspace`, locked, then make what it staged durable
    /// and send what it committed, and only then answer what `decide`
    /// answered.
    ///
    /// # Errors
    /// The workspace is not governed here, `decide` refuses, or its change
    /// cannot be made durable.
    fn with_workspace<T: Send>(
        &self,
        workspace: &str,
        decide: impl FnOnce(&mut dyn WorkspaceTxn) -> Result<T, JoinError> + Send,
    ) -> impl Future<Output = Result<T, JoinError>> + Send;
}

/// The workspace-join exchange answered for the workspaces in `S`.
#[derive(Clone, Debug)]
pub struct JoinAnswer<S>(pub S);

/// `now`, in nanoseconds since the Unix epoch, as a clock reading.
fn at(now: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_nanos(now))
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// The workspace a join request is about, read without trusting it: the
/// ledger then verifies it against what that workspace issued.
pub fn requested_workspace(
    request: &JoinTransportRequest,
    now: u64,
) -> Result<WorkspaceId, JoinError> {
    Ok(match request {
        JoinTransportRequest::Inspect { signed_invitation } => {
            invitation_workspace(signed_invitation, now)?
        }
        JoinTransportRequest::Challenge(request) => {
            invitation_workspace(&request.signed_invitation, now)?
        }
        JoinTransportRequest::Complete(request) => request.proof.challenge.workspace_id,
        JoinTransportRequest::MembershipChallenge(request) => request.workspace_id,
        JoinTransportRequest::MembershipComplete(request) => request.proof.challenge.workspace_id,
    })
}

/// Admit the verified `joins` to `workspace` in one governance change signed
/// with `key`. Each is checked on its own and refused alone; everyone
/// admitted is then committed and recorded in the workspace's ledger,
/// together. Answers each join in order, with its admission: what the
/// bootstrap says of the device, without the workspace's state, which
/// [`crate::join_ledger::Governed::filled`] adds for a device it is sent to.
pub fn admit(
    workspace: &mut dyn WorkspaceTxn,
    joins: &[Redeeming<'_>],
    key: &dyn DeviceSigner,
    now: u64,
) -> Result<Vec<Result<JoinBootstrap, JoinError>>, JoinError> {
    admit_signed(workspace, joins, key, &|| Ok(()), now)
}

/// [`admit`], committing nothing unless `settled`: the turn signed all it wrote.
fn admit_signed(
    workspace: &mut dyn WorkspaceTxn,
    joins: &[Redeeming<'_>],
    key: &dyn DeviceSigner,
    settled: &dyn Fn() -> Result<(), String>,
    now: u64,
) -> Result<Vec<Result<JoinBootstrap, JoinError>>, JoinError> {
    let mut candidate = workspace.governed().peer.clone();
    let refusals = workspace.ledger().admit(&mut candidate, joins, key, now);
    settled().map_err(JoinError::new)?;
    for (join, refused) in joins.iter().zip(&refusals) {
        if let Some(error) = refused {
            tracing::warn!(
                workspace = %workspace.workspace(),
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
    let ledger = workspace.ledger().clone();
    workspace.commit_admission(candidate, now, &mut |governed| {
        let mut completed = ledger.clone();
        delivered = completed.complete(governed, joins, &refusals)?;
        Ok(completed)
    })?;
    for bootstrap in delivered.iter().flatten() {
        tracing::info!(
            workspace = %bootstrap.workspace_id,
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

impl<S: JoinWorkspaces> JoinAnswer<S> {
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
    pub async fn decide(
        &self,
        remote_endpoint: [u8; 32],
        request: JoinTransportRequest,
        now: u64,
    ) -> Result<JoinTransportResponse, JoinError> {
        let workspace = requested_workspace(&request, now)?.to_string();
        let store = &self.0;
        Ok(match request {
            JoinTransportRequest::Inspect { signed_invitation } => {
                store
                    .with_workspace(&workspace, |workspace| {
                        let join = workspace.ledger().inspect(
                            &workspace.governed(),
                            &signed_invitation,
                            now,
                        )?;
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
                    .with_workspace(&workspace, |workspace| {
                        let mut ledger = workspace.ledger().clone();
                        let challenge = ledger.challenge(
                            &workspace.governed(),
                            remote_endpoint,
                            request,
                            nonce,
                            now,
                        )?;
                        workspace.stage_ledger(ledger);
                        Ok(JoinTransportResponse::Challenge(JoinChallengeResponse {
                            challenge,
                        }))
                    })
                    .await?
            }
            JoinTransportRequest::Complete(request) => {
                self.signed(&workspace, |workspace, signed| {
                    complete(workspace, remote_endpoint, &request, signed, now)
                        .map(JoinTransportResponse::Complete)
                })
                .await?
            }
            JoinTransportRequest::MembershipChallenge(request) => {
                let nonce = store.nonce()?;
                self.signed(&workspace, |workspace, signed| {
                    // A further device of someone the workspace has in it
                    // needs no invitation: the workspace admits it as that
                    // member's device, and it proves the membership it was
                    // admitted as.
                    admit_member_device(workspace, &request.device_cert, signed, now)?;
                    let mut ledger = workspace.ledger().clone();
                    let challenge = ledger.membership_challenge(
                        &workspace.governed(),
                        remote_endpoint,
                        request.clone(),
                        nonce,
                        now,
                    )?;
                    workspace.stage_ledger(ledger);
                    Ok(JoinTransportResponse::MembershipChallenge(
                        MembershipChallengeResponse { challenge },
                    ))
                })
                .await?
            }
            JoinTransportRequest::MembershipComplete(request) => {
                self.signed(&workspace, |workspace, signed| {
                    membership_complete(workspace, remote_endpoint, &request, signed, now)
                        .map(JoinTransportResponse::MembershipComplete)
                })
                .await?
            }
        })
    }
}

impl<S: JoinWorkspaces> JoinAnswer<S> {
    /// Run `decide`, a change of `workspace` that signs as it goes, in one
    /// signed turn of this device's signer: a run that asked for a signature
    /// the signer has not given yet commits nothing, the signer signs what it
    /// asked for outside the workspace's lock, and `decide` runs again.
    async fn signed<T: Send>(
        &self,
        workspace: &str,
        mut decide: impl FnMut(&mut dyn WorkspaceTxn, &Signatures) -> Result<T, JoinError> + Send,
    ) -> Result<T, JoinError> {
        let signer = self.0.signer();
        let signatures = Signatures::of(signer);
        for _ in 0..SIGNED_RUNS {
            let ran = self
                .0
                .with_workspace(workspace, |held| decide(held, &signatures))
                .await;
            if !signatures
                .sign_wanted(signer)
                .await
                .map_err(JoinError::new)?
            {
                return ran;
            }
        }
        Err(JoinError::new(UNSIGNED))
    }
}

/// Admit the device at `remote_endpoint` that proved `request`, or answer
/// again what its earlier, identical request was answered.
fn complete(
    workspace: &mut dyn WorkspaceTxn,
    remote_endpoint: [u8; 32],
    request: &JoinCompleteRequest,
    signed: &Signatures,
    now: u64,
) -> Result<JoinBootstrap, JoinError> {
    let request_id = request.proof.challenge.request_id;
    let proof_digest = request_digest(&request.proof)?;
    let prior = {
        let governed = workspace.governed();
        workspace
            .ledger()
            .completed(&governed, remote_endpoint, request_id, proof_digest, now)?
    };
    if let Some(prior) = prior {
        return Ok(prior);
    }
    let redemption =
        workspace
            .ledger()
            .redeem(&workspace.governed(), remote_endpoint, &request.proof, now)?;
    let joins = [Redeeming {
        remote_endpoint,
        request_id,
        proof_digest,
        redemption: &redemption,
        name: None,
    }];
    let admitted = admit_signed(workspace, &joins, signed, &|| signed.settled(), now)?
        .pop()
        .unwrap_or_else(|| Err(JoinError::new("join bootstrap was not built")))?;
    // The device is sent the workspace as the admission committed it.
    workspace.governed().filled(admitted)
}

/// Admit `device` to the workspace before it is challenged, when the workspace
/// has never admitted it: a device certified by the root of a person the
/// workspace has in it is that person's device and needs no invitation, so it
/// asks once and is let in. A device the workspace already holds, and a
/// renewal of one it holds, is left to the exchange that answers for it.
fn admit_member_device(
    workspace: &mut dyn WorkspaceTxn,
    device: &DeviceCert,
    signed: &Signatures,
    now: u64,
) -> Result<(), JoinError> {
    let held = workspace
        .governed()
        .peer
        .certificates()
        .iter()
        .any(|cert| cert.device_public_key == device.device_public_key);
    if held {
        return Ok(());
    }
    let mut candidate = workspace.governed().peer.clone();
    candidate
        .governance_mut()
        .admit_member_device(device, signed, at(now))
        .map_err(JoinError::new)?;
    signed.settled().map_err(JoinError::new)?;
    let ledger = workspace.ledger().clone();
    workspace.commit_governance(candidate, now, &mut |_| Ok(ledger.clone()))?;
    tracing::info!(
        workspace = %workspace.workspace(),
        person = %hex::encode(device.user_public_key),
        device = %hex::encode(device.device_public_key),
        outcome = "member device admitted",
        "a member's device was admitted without an invitation"
    );
    Ok(())
}

/// The workspace's current state for a member that proved its membership. A
/// member proving with its renewed certificate first has the workspace hold
/// that certificate in place of the one it replaces, committed and sent
/// before the answer.
fn membership_complete(
    workspace: &mut dyn WorkspaceTxn,
    remote_endpoint: [u8; 32],
    request: &MembershipCompleteRequest,
    signed: &Signatures,
    now: u64,
) -> Result<JoinBootstrap, JoinError> {
    let renewal = workspace.ledger().membership_renewal(
        &workspace.governed(),
        remote_endpoint,
        request,
        now,
    )?;
    if let Some(renewed) = renewal {
        let mut candidate = workspace.governed().peer.clone();
        candidate
            .governance_mut()
            .renew_member_certificate(&renewed, signed, at(now))?;
        signed.settled().map_err(JoinError::new)?;
        let ledger = workspace.ledger().clone();
        workspace.commit_governance(candidate, now, &mut |_| Ok(ledger.clone()))?;
        tracing::info!(
            workspace = %workspace.workspace(),
            person = %hex::encode(renewed.user_public_key),
            device = %hex::encode(renewed.device_public_key),
            outcome = "renewed",
            "member device certificate renewed"
        );
    }
    workspace
        .ledger()
        .membership_complete(&workspace.governed(), remote_endpoint, request, now)
}

impl<S> WorkspaceJoinService for JoinAnswer<S>
where
    S: JoinWorkspaces + Clone + std::fmt::Debug + 'static,
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
