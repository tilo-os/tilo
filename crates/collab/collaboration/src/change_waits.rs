//! The content-free change waits a route follows (Nearby Send, speaker
//! groups, drives): each answer becomes an event naming nothing, and the app
//! asks for a fresh view instead of asking on a timer.

use super::{CollabEvent, InboundBridge};

/// Start the waits `app_id`'s route on `channel` may follow. Every route
/// follows Nearby Send (the broker answers only Files); the speaker groups
/// and the drive list are followed only by the one app the broker answers.
pub(super) fn follow(
    app_id: &str,
    channel: &runtime_collab::ChannelHandle,
    inbound_tx: &InboundBridge,
) {
    follow_nearby_transfer(channel.clone(), inbound_tx.clone());
    if app_id == SPEAKER_APP_ID {
        follow_speakers(channel.clone(), inbound_tx.clone());
    }
    if app_id == DRIVES_APP_ID {
        follow_drives(channel.clone(), inbound_tx.clone());
        follow_applications(channel.clone(), inbound_tx.clone());
    }
}

/// Follow what a Nearby Send snapshot would show on `channel` for as long
/// as the route lives, handing the app a [`CollabEvent::NearbyTransferChanged`]
/// after each change. The first wait (from 0) answers at once, so an app
/// that may use Nearby Send learns its first view without asking on a
/// timer, and learns it again after the broker comes back. The call is
/// pending without traffic while nothing changes. The broker answers only
/// the certified Files route; every other route stops after its first
/// refusal.
fn follow_nearby_transfer(channel: runtime_collab::ChannelHandle, inbound_tx: InboundBridge) {
    tokio::task::spawn_local(async move {
        let mut after = 0;
        loop {
            match channel.nearby_transfer_changed(after).await {
                Ok(change) if change != after => {
                    after = change;
                    if !inbound_tx
                        .send_durable(CollabEvent::NearbyTransferChanged)
                        .await
                    {
                        return;
                    }
                }
                Ok(_) => {
                    tracing::warn!("collab: the broker reported unchanged Nearby Send as changed");
                    return;
                }
                Err(error) => {
                    tracing::debug!(%error, "collab: this route does not follow Nearby Send");
                    return;
                }
            }
        }
    });
}

/// The app whose routes follow the speaker groups: only the certified
/// Settings route is answered, so no other app spends a call on it.
const SPEAKER_APP_ID: &str = "so.tilo.settings";

/// Follow what a speaker snapshot would show for as long as the Settings
/// route lives, handing the app a [`CollabEvent::SpeakersChanged`] after each
/// change. The first wait (from 0) answers at once, so Settings learns its
/// first view without asking on a timer, and learns it again after the
/// broker comes back. The call is pending without traffic while nothing
/// changes; a host that cannot say when its speakers change ends it after
/// the first refusal.
fn follow_speakers(channel: runtime_collab::ChannelHandle, inbound_tx: InboundBridge) {
    tokio::task::spawn_local(async move {
        let mut after = 0;
        loop {
            match channel.speaker_changed(after).await {
                Ok(change) if change != after => {
                    after = change;
                    if !inbound_tx.send_durable(CollabEvent::SpeakersChanged).await {
                        return;
                    }
                }
                Ok(_) => {
                    tracing::warn!("collab: the broker reported unchanged speakers as changed");
                    return;
                }
                Err(error) => {
                    tracing::debug!(%error, "collab: this route does not follow speaker groups");
                    return;
                }
            }
        }
    });
}

/// The app whose routes follow the drive list and the app list: only the
/// certified Files route is answered, so no other app spends a call on it.
const DRIVES_APP_ID: &str = "so.tilo.file-explorer";

/// Follow the person's apps as Files lists them for as long as the Files
/// route lives, handing the app a [`CollabEvent::ApplicationsChanged`] after
/// each change. The first wait (from 0) answers at once, so Files learns its
/// first list without asking on a timer, and learns it again after the
/// broker comes back. The call is pending without traffic while nothing
/// changes; a host that cannot say when its apps change ends it after the
/// first refusal.
fn follow_applications(channel: runtime_collab::ChannelHandle, inbound_tx: InboundBridge) {
    tokio::task::spawn_local(async move {
        let mut after = 0;
        loop {
            match channel.applications_changed(after).await {
                Ok(change) if change != after => {
                    after = change;
                    if !inbound_tx
                        .send_durable(CollabEvent::ApplicationsChanged)
                        .await
                    {
                        return;
                    }
                }
                Ok(_) => {
                    tracing::warn!("collab: the broker reported unchanged apps as changed");
                    return;
                }
                Err(error) => {
                    tracing::debug!(%error, "collab: this route does not follow the app list");
                    return;
                }
            }
        }
    });
}

/// Follow the drives the shell lists for as long as the Files route lives,
/// handing the app a [`CollabEvent::DrivesChanged`] after each change. The
/// first wait (from 0) answers at once, so Files fetches its first list
/// without asking on a timer, and fetches it again after the broker comes
/// back. The call is pending without traffic while nothing changes; a host
/// that cannot say when its drives change ends it after the first refusal.
fn follow_drives(channel: runtime_collab::ChannelHandle, inbound_tx: InboundBridge) {
    tokio::task::spawn_local(async move {
        let mut after = 0;
        loop {
            match channel.drives_changed(after).await {
                Ok(change) if change != after => {
                    after = change;
                    if !inbound_tx.send_durable(CollabEvent::DrivesChanged).await {
                        return;
                    }
                }
                Ok(_) => {
                    tracing::warn!("collab: the broker reported unchanged drives as changed");
                    return;
                }
                Err(error) => {
                    tracing::debug!(%error, "collab: this route does not follow drives");
                    return;
                }
            }
        }
    });
}
