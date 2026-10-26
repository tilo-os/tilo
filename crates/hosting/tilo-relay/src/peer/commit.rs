//! `Commit`: a delta whose author waits for its host order, with the
//! per-member change budget and the commit ledger.

use super::{
    AuthenticatedPeer, Envelope, PeerError, SessionCtx, SharedWorkspace, authorized_state_source,
    enqueue_state_inner, governor_notice, layout_refusal, resume_record, unix_now_ns, unpublished,
};

/// Spends `peer`'s budget for one change, unless the relay's own user sent
/// it: the host device and the bots a host-held root delegated must never
/// turn a burst into refusals.
pub(super) fn over_change_limit(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Option<crate::member_limits::Limited> {
    let limits = session.member_limits?;
    if peer.user_public_key == *session.own_user_public_key {
        return None;
    }
    limits
        .admit_change(&peer.user_public_key, std::time::Instant::now())
        .err()
}

/// Handle an inbound `Commit`: admit it exactly like a `Delta` (the reply
/// waits until the host persisted it), then tell the author its host order.
/// A commit the host cannot admit is refused to the author without closing
/// the session. A resent commit the host already admitted is answered with
/// its original order from the workspace's commit ledger and neither
/// persisted nor fanned out again.
pub(super) async fn handle_commit(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    env: Envelope,
) -> Result<Option<Envelope>, PeerError> {
    let admission = admit_commit(peer, session, subscribed, env).await?;
    match admission {
        CommitAdmission::Answered(reply) => Ok(reply),
        CommitAdmission::Admitted(admitted) => admitted.finish(session).await.map(Some),
    }
}

/// Handle `commits`, a run of `Commit`s the author sent back to back: admit
/// every one before waiting for the first, so the workspace's owner finds
/// them queued together and makes them durable under shared barriers
/// (`crate::persistence::Group`), then answer each in the order sent. A
/// copy of a commit in the same run must wait for the first to be recorded,
/// so a run never holds two with the same operation id
/// ([`commit_run_key`]).
#[expect(
    clippy::significant_drop_tightening,
    reason = "the admitted commits move into the answering loop one by one; the lint does not see the move"
)]
pub(super) async fn handle_commit_run(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    commits: Vec<Envelope>,
) -> Result<Vec<Option<Envelope>>, PeerError> {
    let mut admitted = Vec::with_capacity(commits.len());
    for env in commits {
        admitted.push(admit_commit(peer, session, subscribed, env).await?);
    }
    let mut replies = Vec::with_capacity(admitted.len());
    for admission in admitted {
        replies.push(match admission {
            CommitAdmission::Answered(reply) => reply,
            CommitAdmission::Admitted(admitted) => Some(admitted.finish(session).await?),
        });
    }
    Ok(replies)
}

/// What keys a commit within a run: its author and operation id.
pub(super) const fn commit_run_key(env: &Envelope) -> Option<([u8; 32], [u8; 16])> {
    match env {
        Envelope::Commit {
            author_device_public_key,
            op,
            ..
        } => Some((*author_device_public_key, *op)),
        _ => None,
    }
}

/// A `Commit` after its admission.
enum CommitAdmission {
    /// Answered without persisting: refused, or resent and found in the
    /// ledger.
    Answered(Option<Envelope>),
    /// Queued with the workspace's owner.
    Admitted(Box<AdmittedCommit>),
}

/// A commit queued with its workspace's owner, not yet durable.
struct AdmittedCommit {
    ws: SharedWorkspace,
    workspace_id: u64,
    op: [u8; 16],
    author: [u8; 32],
    digest: [u8; 32],
    authored: Envelope,
    admitted: super::admission::AdmittedState,
    observed: Option<crate::compaction::Observed>,
    admitting: Option<crate::compaction::Admitting>,
    /// Held until the commit is recorded: a copy of it (a resend after a
    /// reconnect) waits for this one, then is answered from the ledger.
    claim: crate::workspace::CommitClaim,
    committing: Option<std::time::Instant>,
}

/// Check, claim and queue `env`, a `Commit`, without waiting for it to be
/// durable.
async fn admit_commit(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    env: Envelope,
) -> Result<CommitAdmission, PeerError> {
    let Envelope::Commit {
        workspace_id,
        op,
        author_device_public_key,
        authored_at_ns,
        delta,
    } = env
    else {
        return Err(PeerError::Protocol(
            "handle_commit called with a non-Commit envelope".into(),
        ));
    };
    let refused = |reason: &str| {
        Ok(CommitAdmission::Answered(Some(Envelope::CommitRefused {
            workspace_id,
            op,
            reason: reason.into(),
        })))
    };
    if !subscribed.contains(&workspace_id) {
        return refused("commit sent before subscribing to workspace");
    }
    // The relay's own device takes part in its rooms' channels like any
    // other member: its self-connection commits as that device.
    let device =
        peer.role == crate::auth::PeerRole::Device || &peer.cert == session.own_device_cert;
    if !device || author_device_public_key != peer.device_public_key {
        return refused("author does not match authenticated device");
    }
    if let Some(limited) = over_change_limit(peer, session) {
        return refused(&limited.to_string());
    }
    let Some(ws) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
        return refused("workspace not bound on this relay");
    };
    let Some(source) = authorized_state_source(&ws, peer, session).await? else {
        let reason = unpublished(&*ws.lock().await, peer);
        return refused(reason);
    };
    let digest = *blake3::hash(&delta).as_bytes();
    let claiming = crate::timing::start();
    let claim = match claim(&ws, author_device_public_key, op).await {
        Ok(claim) => claim,
        Err(admitted) => {
            return Ok(CommitAdmission::Answered(Some(resent_commit(
                workspace_id,
                admitted,
                digest,
            ))));
        }
    };
    crate::timing::event("commit.claim_wait", claiming, 0);
    let committing = crate::timing::start();
    let observed = session
        .compactor
        .and_then(|compactor| compactor.observe(&delta));
    let admitting = session
        .compactor
        .zip(observed)
        .map(|(compactor, observed)| compactor.admitting(workspace_id, observed));
    let delta = Envelope::Delta {
        workspace_id,
        author_device_public_key,
        authored_at_ns,
        delta,
        signature: [0; 64],
    };
    if let Some(reason) = commit_refusal(&ws, author_device_public_key, &delta).await {
        return refused(&reason);
    }
    let authored = delta.clone();
    let admitted = Box::pin(enqueue_state_inner(
        &ws,
        delta,
        Some(source),
        peer.role,
        session.metrics.clone(),
        false,
    ))
    .await?;
    Ok(CommitAdmission::Admitted(Box::new(AdmittedCommit {
        ws,
        workspace_id,
        op,
        author: author_device_public_key,
        digest,
        authored,
        admitted,
        observed,
        admitting,
        claim,
        committing,
    })))
}

impl AdmittedCommit {
    /// Wait for the commit to be durable and published, record it, and
    /// answer its author with its host order.
    async fn finish(self: Box<Self>, session: &SessionCtx<'_>) -> Result<Envelope, PeerError> {
        let Self {
            ws,
            workspace_id,
            op,
            author,
            digest,
            authored,
            admitted,
            observed,
            admitting,
            claim,
            committing,
        } = *self;
        // What an author is told is committed never depends on work that can
        // still fail: wait for the change to be durable and published.
        crate::persistence::receipt(admitted.completion).await?;
        crate::timing::event("commit.durable", committing, 0);
        tell_governor(&ws, &authored).await;
        let durable = admitted.sequence.load(std::sync::atomic::Ordering::SeqCst);
        if let (Some(compactor), Some(observed)) = (session.compactor, observed) {
            compactor.note(&ws, workspace_id, observed);
        }
        drop(admitting);
        // The author holds what it committed; a resume never replays it to it.
        if let Some(position) = resume_record(session, workspace_id, [&authored]) {
            let _ = session.outbound.try_send(position);
        }
        let seq = remember_commit(
            &ws,
            crate::store::CommitRecord {
                author,
                op,
                seq: durable,
                digest,
            },
        )
        .await?;
        drop(claim);
        Ok(Envelope::Committed {
            workspace_id,
            op,
            seq,
        })
    }
}

/// Why the room refuses `delta`, a page's commit, if it does: decided as a
/// session's delta is ([`super::handle_delta`]), by what the change says it
/// does against its author's grants, then, for the room's shared desktop,
/// by what it does to what others made.
async fn commit_refusal(
    ws: &SharedWorkspace,
    author: [u8; 32],
    delta: &Envelope,
) -> Option<String> {
    let Envelope::Delta { delta: bytes, .. } = delta else {
        return None;
    };
    let (standings, payloads) = {
        let runtime = ws.lock().await;
        (runtime.standings.clone(), runtime.store.payloads().clone())
    };
    if let Some(reason) = crate::surfaces::refusal(&*payloads, bytes, standings.as_deref()) {
        tracing::warn!(%reason, device = %hex::encode(author), "refused a document change");
        return Some(reason);
    }
    layout_refusal(ws, author, delta).await
}

/// Hand `delta`, a member's change `ws` kept, to the room's governor, if a
/// governor follows the room. It follows every change the relay keeps, a
/// committed one as one sent alone (`handle_delta`): a page writes its
/// records as commits.
async fn tell_governor(ws: &SharedWorkspace, delta: &Envelope) {
    let Envelope::Delta {
        author_device_public_key,
        ..
    } = delta
    else {
        return;
    };
    let tap = ws.lock().await.governor_tap.clone();
    if let Some((tap, body)) = tap.zip(governor_notice(delta)) {
        super::state::tell_governor(&tap, *author_device_public_key, body).await;
    }
}

/// Claim `author`'s commit `op` for admission in `ws`
/// ([`crate::workspace::WorkspaceRuntime::claim_commit`]), first waiting for
/// a copy of it already being admitted. `Err` with its record when the
/// ledger holds it.
async fn claim(
    ws: &SharedWorkspace,
    author: [u8; 32],
    op: [u8; 16],
) -> Result<crate::workspace::CommitClaim, crate::store::CommitRecord> {
    loop {
        let runtime = ws.lock().await;
        let claimed = runtime
            .admitted_commit(&author, &op)
            .map_or_else(|| Ok(runtime.claim_commit(author, op)), Err);
        drop(runtime);
        let copy = match claimed {
            Err(admitted) => return Err(admitted),
            Ok(Ok(claim)) => return Ok(claim),
            Ok(Err(copy)) => copy,
        };
        drop(copy.lock().await);
    }
}

/// The answer to a resent commit the ledger already holds: its original
/// host order, or a refusal when the same operation id carries other content.
fn resent_commit(
    workspace_id: u64,
    admitted: crate::store::CommitRecord,
    digest: [u8; 32],
) -> Envelope {
    if admitted.digest != digest {
        return Envelope::CommitRefused {
            workspace_id,
            op: admitted.op,
            reason: "operation id already committed with different content".into(),
        };
    }
    tracing::debug!(
        workspace_id,
        seq = admitted.seq,
        "resent commit answered from the ledger"
    );
    Envelope::Committed {
        workspace_id,
        op: admitted.op,
        seq: admitted.seq,
    }
}

/// Record an admitted commit in the workspace's commit ledger and return the
/// host order it is acknowledged at: the durable sequence it was published
/// in (`record.seq`), or the next one when persistence assigned none. Every
/// member then receives commits in the order their authors were told.
async fn remember_commit(
    ws: &SharedWorkspace,
    mut record: crate::store::CommitRecord,
) -> Result<u64, PeerError> {
    let lane = ws.lock().await.commit_ledger_lane.clone();
    let _writing = lane.lock().await;
    let (ledger, store) = {
        let mut runtime = ws.lock().await;
        if record.seq == 0 {
            record.seq = runtime.next_seq(unix_now_ns());
        }
        (runtime.remember_commit(record), runtime.store.clone())
    };
    tokio::task::spawn_blocking(move || ledger.persist(&store))
        .await
        .map_err(|_| PeerError::Io(std::io::Error::other("commit ledger write panicked")))?;
    Ok(record.seq)
}
