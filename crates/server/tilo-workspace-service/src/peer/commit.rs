//! `Commit`: a delta whose author waits for its host order, and the commit
//! log.

use super::{
    AuthenticatedPeer, Envelope, PeerError, SessionCtx, SharedWorkspace, authorized_state_source,
    desktop_refusal, enqueue_state_inner, file_refusal_of, governor_notice, resume_document,
    unix_now_ns, unpublished,
};
use crate::workspace_access::WorkspaceOperation;
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::refusal::Refusal;

/// Whether `ws` keeps no more of `peer`'s writes: it is a person's own
/// workspace and keeps as much as they may
/// ([`crate::workspaces::Workspaces::storage_refusal`]). The server's own user
/// is never refused: the host's own writes keep the workspace whole.
pub(super) async fn over_storage(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    ws: &SharedWorkspace,
) -> bool {
    if peer.user_public_key == *session.own_user_public_key {
        return false;
    }
    let runtime = ws.lock().await;
    let Some(authority) = runtime.workspace_authority.as_ref() else {
        return false;
    };
    let Some(workspaces) = authority.workspaces() else {
        return false;
    };
    let workspace = authority.workspace().to_owned();
    drop(runtime);
    // Measuring walks the workspace's files.
    tokio::task::spawn_blocking(move || workspaces.storage_refusal(&workspace).is_some())
        .await
        .unwrap_or(false)
}

/// Handle an inbound `Commit`: admit it exactly like a `Delta` (the reply
/// waits until the host persisted it), then tell the author its host order.
/// A commit the host cannot admit is refused to the author without closing
/// the session. A resent commit the host already admitted is answered with
/// its original order from the workspace's commit log and neither
/// persisted nor fanned out again. A session's reader admits commits
/// itself and answers them as they become durable
/// ([`admit_commit`]).
pub(super) async fn handle_commit(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[WorkspaceId],
    env: Envelope,
) -> Result<Option<Envelope>, PeerError> {
    admit_commit(peer, session, subscribed, env)
        .await?
        .answer(session)
        .await
}

/// A `Commit` after its admission.
pub(super) enum CommitAdmission {
    /// Answered without persisting: refused, or resent and found in the
    /// log.
    Answered(Option<Envelope>),
    /// Queued with the workspace's owner.
    Admitted(Box<AdmittedCommit>),
}

/// A commit queued with its workspace's owner, not yet durable.
pub(super) struct AdmittedCommit {
    ws: SharedWorkspace,
    workspace_id: WorkspaceId,
    op: [u8; 16],
    author: [u8; 32],
    digest: [u8; 32],
    authored: Envelope,
    admitted: super::admission::AdmittedState,
    observed: Option<crate::compaction::Observed>,
    admitting: Option<crate::compaction::Admitting>,
    /// Held until the commit is recorded: a copy of it (a resend after a
    /// reconnect) waits for this one, then is answered from the log.
    claim: crate::workspace::CommitClaim,
    committing: Option<std::time::Instant>,
    /// Answered once the commit admitted here before this one was told to the governor.
    before: Option<tokio::sync::oneshot::Receiver<()>>,
    /// Dropped once this commit was told to the governor, or will never be.
    told: tokio::sync::oneshot::Sender<()>,
}

/// Why `delta` is not `peer`'s to commit ([`commit_refusal`]), or, a write of a member's own
/// record, not theirs: a host this workspace is attached to carries its members' own records as
/// they wrote them, and anyone else writes only their own.
async fn own_commit_refusal(
    ws: &SharedWorkspace,
    peer: &AuthenticatedPeer,
    author: [u8; 32],
    delta: &Envelope,
) -> Option<Refusal> {
    if let Some(refusal) = commit_refusal(ws, author, delta).await {
        return Some(refusal);
    }
    let Envelope::Delta { delta: bytes, .. } = delta else {
        return None;
    };
    if author != peer.device_public_key {
        return None;
    }
    let store = ws.lock().await.store.clone();
    crate::surfaces::own_refusal(&store, &peer.user_public_key, bytes)
}

/// Check, claim and queue `env`, a `Commit`, without waiting for it to be
/// durable: its answer ([`CommitAdmission::answer`]) waits for that. A
/// connection admits the commits it is sent one after another, in the order
/// sent, so a workspace's owner keeps them in that order, and answers each
/// once it is durable while it goes on with what came after it.
pub(super) async fn admit_commit(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[WorkspaceId],
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
    let refused = |refusal: Refusal| {
        Ok(CommitAdmission::Answered(Some(Envelope::CommitRefused {
            workspace_id,
            op,
            refusal,
        })))
    };
    // Not subscribed or not open here yet: it goes through once ready.
    let open = session.workspaces.lock().await.get(&workspace_id).cloned();
    let Some(ws) = open.filter(|_| subscribed.contains(&workspace_id)) else {
        return refused(Refusal::NotReady);
    };
    // A commit carries its session's own device's change: a host this
    // workspace is attached to carries its members' instead.
    if author_device_public_key != peer.device_public_key
        && !ws.lock().await.attached(&peer.device_public_key)
    {
        return refused(Refusal::Invalid(
            "author does not match authenticated device".into(),
        ));
    }
    if over_storage(peer, session, &ws).await {
        return refused(Refusal::Full);
    }
    let channel = crate::peer::admission::channel_of(&delta);
    let Some(source) =
        authorized_state_source(&ws, peer, session, WorkspaceOperation::Publish, channel).await?
    else {
        return refused(unpublished());
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
    if let Some(refusal) = own_commit_refusal(&ws, peer, author_device_public_key, &delta).await {
        return refused(refusal);
    }
    let authored = delta.clone();
    let admitted = Box::pin(enqueue_state_inner(
        &ws,
        delta,
        Some(source),
        session.metrics.clone(),
        false,
        false,
    ))
    .await?;
    let (before, told) = ws.lock().await.next_governor_turn();
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
        before,
        told,
    })))
}

impl CommitAdmission {
    /// What the commit's author is told: its order once it is durable,
    /// or the answer it was given at admission.
    pub(super) async fn answer(
        self,
        session: &SessionCtx<'_>,
    ) -> Result<Option<Envelope>, PeerError> {
        match self {
            Self::Answered(reply) => Ok(reply),
            Self::Admitted(admitted) => admitted.finish(session).await.map(Some),
        }
    }
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
            before,
            told,
        } = *self;
        // What an author is told is committed never depends on work that can
        // still fail: wait for the change to be durable and published.
        crate::persistence::receipt(admitted.completion).await?;
        crate::timing::event("commit.durable", committing, 0);
        // Commits become durable together and finish in any order, but the
        // governor's copy takes an author's writes only in the order they were
        // written: one told after a later one would be passed over for good.
        // And the copy decides the author's next writes, a compaction of these
        // included, so the author is told this one is committed only once the
        // copy holds it.
        if let Some(before) = before {
            let _ = before.await;
        }
        tell_governor(&ws, &authored).await;
        drop(told);
        let durable = admitted.sequence.load(std::sync::atomic::Ordering::SeqCst);
        if let (Some(compactor), Some(observed)) = (session.compactor, observed) {
            compactor.note(&ws, workspace_id, observed);
        }
        drop(admitting);
        // The author holds what it committed; a resume never replays it to it.
        if let Some(position) = resume_document(session, workspace_id, [&authored]) {
            let _ = session.outbound.try_send(position);
        }
        let seq = remember_commit(
            &ws,
            crate::store::CommitEntry {
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

/// Why the workspace refuses `delta`, a page's commit, if it does: decided as a
/// session's delta is ([`super::handle_delta`]), by what the change says it
/// does against its author's grants, then, for the workspace's shared desktop,
/// by what it does to what others made, then by what the workspace's own copy
/// of the record takes in.
async fn commit_refusal(
    ws: &SharedWorkspace,
    author: [u8; 32],
    delta: &Envelope,
) -> Option<Refusal> {
    let Envelope::Delta { delta: bytes, .. } = delta else {
        return None;
    };
    let payloads = ws.lock().await.store.payloads().clone();
    let named = crate::surfaces::governance_named(&*payloads, bytes);
    let decision = Box::pin(crate::workspace::access_deciding(
        ws,
        author,
        &named,
        |device_access, payloads| {
            crate::surfaces::refusal(payloads, bytes, device_access).is_some()
                || crate::surfaces::role_refusal(bytes, device_access, author).is_some()
        },
    ))
    .await;
    let device_access = decision.device_access;
    if let Some(refusal) =
        crate::surfaces::refusal(&*decision.payloads, bytes, device_access.as_deref())
    {
        let refusal = super::state::behind_governance(decision.governance_behind, refusal);
        tracing::warn!(%refusal, device = %hex::encode(author), "refused a document change");
        file_refusal_of(ws, delta, &refusal.to_string()).await;
        return Some(refusal);
    }
    if let Some(refusal) = desktop_refusal(ws, author, delta).await {
        return Some(refusal);
    }
    if let Some(refusal) = crate::surfaces::role_refusal(bytes, device_access.as_deref(), author) {
        let refusal = super::state::behind_governance(decision.governance_behind, refusal);
        tracing::warn!(%refusal, device = %hex::encode(author), "refused a change a viewer's role writes nothing of");
        file_refusal_of(ws, delta, &refusal.to_string()).await;
        return Some(refusal);
    }
    super::governor_refusal(ws, author, delta).await
}

/// Hand `delta`, a member's change `ws` kept, to the workspace's governor, if a
/// governor follows the workspace, and wait until its copy took the change in. It
/// follows every change the server keeps, a committed one as one sent alone
/// (`handle_delta`): a page writes its records as commits.
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
        let taken = super::state::tell_governor(ws, &tap, *author_device_public_key, body).await;
        let _ = taken.await;
    }
}

/// Claim `author`'s commit `op` for admission in `ws`
/// ([`crate::workspace::WorkspaceRuntime::claim_commit`]), first waiting for
/// a copy of it already being admitted. `Err` with its record when the
/// log holds it.
async fn claim(
    ws: &SharedWorkspace,
    author: [u8; 32],
    op: [u8; 16],
) -> Result<crate::workspace::CommitClaim, crate::store::CommitEntry> {
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

/// The answer to a resent commit the log already holds: its original
/// host order, or a refusal when the same operation id carries other content.
fn resent_commit(
    workspace_id: WorkspaceId,
    admitted: crate::store::CommitEntry,
    digest: [u8; 32],
) -> Envelope {
    if admitted.digest != digest {
        return Envelope::CommitRefused {
            workspace_id,
            op: admitted.op,
            refusal: Refusal::Conflict,
        };
    }
    tracing::debug!(
        workspace_id = %workspace_id,
        seq = admitted.seq,
        "resent commit answered from the log"
    );
    Envelope::Committed {
        workspace_id,
        op: admitted.op,
        seq: admitted.seq,
    }
}

/// Record an admitted commit in the workspace's commit log and return the
/// host order it is acknowledged at: the durable sequence it was published
/// in (`record.seq`), or the next one when persistence assigned none. Every
/// member then receives commits in the order their authors were told.
async fn remember_commit(
    ws: &SharedWorkspace,
    mut record: crate::store::CommitEntry,
) -> Result<u64, PeerError> {
    let queue = ws.lock().await.commit_log_queue.clone();
    let _writing = queue.lock().await;
    let (log, store) = {
        let mut runtime = ws.lock().await;
        if record.seq == 0 {
            record.seq = runtime.next_seq(unix_now_ns());
        }
        (runtime.remember_commit(record), runtime.store.clone())
    };
    tokio::task::spawn_blocking(move || log.persist(&store))
        .await
        .map_err(|_| PeerError::Io(std::io::Error::other("commit log write panicked")))?;
    Ok(record.seq)
}
