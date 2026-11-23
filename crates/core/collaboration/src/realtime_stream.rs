//! A channel's realtime events, as the endpoint takes them in.

use super::{CollabEvent, InboundBridge};

#[allow(
    clippy::future_not_send,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
pub(super) async fn spawn_realtime_stream(
    channel: &runtime_collab::ChannelHandle,
    inbound_tx: &InboundBridge,
) {
    let stream = match channel.subscribe_realtime().await {
        Ok(stream) => stream,
        Err(error) => {
            tracing::warn!(%error, "collab: realtime events unavailable on this channel");
            return;
        }
    };
    let tx = inbound_tx.clone();
    tokio::task::spawn_local(async move {
        loop {
            let bytes = match stream.read().await {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(%error, "collab: realtime event stream ended");
                    break;
                }
            };
            if bytes.len() > collab_types::realtime::MAX_REALTIME_WIRE_BYTES {
                tracing::warn!(
                    len = bytes.len(),
                    "collab: refused an oversized realtime event"
                );
                continue;
            }
            let event = match serde_json::from_slice::<collab_types::realtime::RealtimeEvent>(
                &bytes,
            ) {
                Ok(event) => event,
                Err(error) => {
                    tracing::warn!(%error, "collab: refused a realtime event that does not decode");
                    continue;
                }
            };
            if !event.valid() {
                tracing::warn!(
                    "collab: an invalid realtime event ends the channel's realtime stream"
                );
                break;
            }
            // The receiver is gone: the bridge, and with it whoever
            // listened, has shut down.
            if !tx.send_durable(CollabEvent::Realtime(event)).await {
                break;
            }
        }
    });
}
