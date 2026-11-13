//! The content-free change waits a route follows (Nearby Send, speaker
//! groups, drives): each answer becomes an event naming nothing, and the app
//! asks for a fresh view instead of asking on a timer.

use runtime_collab::wire::Changes;

use super::{CollabEvent, InboundBridge};

/// The app whose routes follow the speaker groups: only the certified
/// Settings route is answered, so no other app spends a call on it.
const SPEAKER_APP_ID: &str = "so.tilo.settings";

/// The app whose routes follow the drive list and the app list: only the
/// certified Files route is answered, so no other app spends a call on it.
const DRIVES_APP_ID: &str = "so.tilo.file-explorer";

/// Start the waits `app_id`'s route on `channel` may follow. Every route
/// follows Nearby Send (the broker answers only Files); the speaker groups
/// and the drive list are followed only by the one app the broker answers.
pub(super) fn follow(
    app_id: &str,
    channel: &runtime_collab::ChannelHandle,
    inbound_tx: &InboundBridge,
) {
    let follow_one = |of, event, what| {
        follow_changes(channel.clone(), inbound_tx.clone(), of, event, what);
    };
    follow_one(
        Changes::NearbyTransfers,
        CollabEvent::NearbyTransferChanged,
        "Nearby Send",
    );
    if app_id == SPEAKER_APP_ID {
        follow_one(
            Changes::Speakers,
            CollabEvent::SpeakersChanged,
            "speaker groups",
        );
    }
    if app_id == DRIVES_APP_ID {
        follow_one(Changes::Drives, CollabEvent::DrivesChanged, "drives");
        follow_one(
            Changes::Applications,
            CollabEvent::ApplicationsChanged,
            "apps",
        );
    }
}

/// Follow what a snapshot of `of` would show on `channel` for as long as the
/// route lives, handing the app `event` after each change.
///
/// The first wait (from 0) answers at once, so an app that may follow `of`
/// learns its first view without asking on a timer, and learns it again after
/// the broker comes back. The call is pending without traffic while nothing
/// changes. A route the broker does not answer for `of` stops after its first
/// refusal.
fn follow_changes(
    channel: runtime_collab::ChannelHandle,
    inbound_tx: InboundBridge,
    of: Changes,
    event: CollabEvent,
    what: &'static str,
) {
    tokio::task::spawn_local(async move {
        let mut after = 0;
        loop {
            match channel.changed(of, after).await {
                Ok(change) if change != after => {
                    after = change;
                    if !inbound_tx.send_durable(event.clone()).await {
                        return;
                    }
                }
                Ok(_) => {
                    tracing::warn!("collab: the broker reported unchanged {what} as changed");
                    return;
                }
                Err(error) => {
                    tilo_failure::ignored!(
                        "a route that does not follow this change is unchanged by it",
                        tracing::debug!(%error, "collab: this route does not follow {what}")
                    );
                    return;
                }
            }
        }
    });
}
