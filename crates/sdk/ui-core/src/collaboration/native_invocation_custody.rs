//! Native original-delivery checks stay on the authenticated shovel channel.
//! The owner thread forwards every check; no cached positive answer is authority.
use super::{InboundBridge, RouteSpec};
use crate::invocation_scope::{
    NativeInvocationReference, PublishedInvocationLease, PublishedInvocationRequest,
    RetainedInvocationDeclaration,
};
use std::sync::Arc;

const CHECK_QUEUE: usize = 8;
// Reuse the original native publication/delivery bound, not an unbounded RPC wait.
const CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

#[allow(
    clippy::future_not_send,
    reason = "native original checks use the authenticated Cap'n Proto handle on the shovel LocalSet"
)]
pub(super) async fn capture(
    channel: &runtime_collab::ChannelHandle,
    route: &RouteSpec,
    request: PublishedInvocationRequest,
    inbound: &InboundBridge,
    deadline: crate::time::Instant,
) -> Result<PublishedInvocationLease, String> {
    let mut reference = NativeInvocationReference {
        wire_id: request.wire_id,
        invocation: *request.invocation.0.as_bytes(),
        retained_bytes: 0,
    };
    let declaration = tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        channel.retained_invocation(reference.wire_id, &reference.invocation),
    )
    .await
    .map_err(|_| "original native invocation capture timed out")?
    .map_err(|error| error.to_string())?;
    // Bound queued custody too: decoded original inputs/spec, the lease's
    // immutable copies and its live checker all remain owned until retirement.
    reference.retained_bytes = declaration
        .len()
        .checked_mul(4)
        .ok_or("native custody size overflow")?;
    let declaration: RetainedInvocationDeclaration = serde_json::from_slice(&declaration)
        .map_err(|error| format!("original native invocation declaration: {error}"))?;
    if declaration.request != request {
        return Err("native delivery differs from broker-retained original inputs".into());
    }
    let original_spec = declaration.spec.clone();
    let (check_tx, mut check_rx) = tokio::sync::mpsc::channel::<(
        crate::time::Instant,
        std::sync::mpsc::SyncSender<Result<(), String>>,
    )>(CHECK_QUEUE);
    let channel = channel.clone();
    let current_reference = reference.clone();
    let state = inbound.state.clone();
    tokio::task::spawn_local(async move {
        while let Some((deadline, reply)) = check_rx.recv().await {
            let checked = async {
                if !state.is_healthy() {
                    return Err("native original transport retired".to_owned());
                }
                let bytes = channel
                    .retained_invocation(current_reference.wire_id, &current_reference.invocation)
                    .await
                    .map_err(|error| error.to_string())?;
                let current: RetainedInvocationDeclaration =
                    serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
                if current != declaration {
                    return Err("native original invocation declaration changed".into());
                }
                Ok(())
            };
            let result = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), checked)
                .await
                .unwrap_or_else(|_| Err("native original invocation check timed out".into()));
            let _ = reply.send(result);
        }
    });
    let healthy = inbound.state.clone();
    let check = Arc::new(move || {
        if !healthy.is_healthy() {
            return Err("native original transport retired".into());
        }
        let (reply, answer) = std::sync::mpsc::sync_channel(1);
        let deadline = crate::time::now() + CHECK_TIMEOUT;
        check_tx
            .try_send((deadline, reply))
            .map_err(|_| "native original check queue unavailable")?;
        answer
            .recv_timeout(deadline.saturating_duration_since(crate::time::now()))
            .map_err(|_| "native original check reply unavailable")?
    });
    PublishedInvocationLease::new(request, route.clone(), check)
        .with_dispatch_spec(original_spec)?
        .with_owner_token(Arc::new(reference))
}
