//! LIVE_V1 on the Cloud host: the shared host-core live service
//! (`tilo_workspace_host::live_service`) over the Cloud Store owner.
//!
//! * `POST /peer/v3/live/grant` is admitted inside a reserved Store turn with
//!   the original request proof. The grant is signed by the Store authority
//!   key, which is also the pinned workspace host of every data session.
//! * `/moq/live` is served by the shared live relay on the Cloud host's QUIC
//!   endpoint; permission observations share one Store turn per 50 ms across
//!   all live sessions.
use super::{CloudOwnerTurn, CloudStoreSource, CloudStoreTurn};
use anyhow::{Result, ensure};
use tilo_delivery_protocol::delivery_v3::http_proof::VerifiedRequest;
use tilo_workspace_host::live_service::{LiveService, LiveSigner};

pub(crate) use tilo_workspace_host::live_service::{InstalledLive, LiveForwarder};

/// Bind the shared live service to `source` under one of its own turns.
pub(crate) fn bind(
    source: CloudStoreSource,
    turn: &CloudStoreTurn,
    origin: &str,
) -> Result<LiveService<CloudStoreSource>> {
    ensure!(
        source.owns_turn(turn),
        "live authority needs this source's turn"
    );
    let host = turn.peer().authority().device_public_key;
    ensure!(
        turn.store.key().verifying_key().to_bytes() == host,
        "live signer is not the Store authority"
    );
    LiveService::bind(
        source,
        &CloudOwnerTurn::at(turn, turn.now_ns()),
        host,
        origin,
    )
}

/// Issue or renew one grant for an already admitted request inside this exact
/// reserved Store turn.
pub(crate) fn issue_in(
    live: &LiveService<CloudStoreSource>,
    source: &CloudStoreSource,
    turn: &CloudStoreTurn,
    request: &VerifiedRequest<'_>,
) -> Result<Vec<u8>> {
    ensure!(source.owns_turn(turn), "foreign Store turn");
    let key = turn.store.key();
    live.issue(
        &CloudOwnerTurn::at(turn, turn.now_ns()),
        LiveSigner {
            key: &key,
            certificate: turn.peer().authority(),
        },
        request,
    )
}
