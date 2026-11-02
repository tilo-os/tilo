//! The panel's now playing, carried between quick settings and the audio
//! service.
//!
//! One process owns the players on the session bus and publishes them as
//! the `media` resource: what each plays, where its playhead is, how long
//! what it plays is, its cover, where its sound can go and what another of
//! the person's devices hands over. The session lends the panel what the
//! service reads ([`land`]) and hands the card's presses to the resource's
//! actions ([`follow`]), so the card is a view of the players and never a
//! second copy of them.
//!
//! Which speaker group this device plays its sound on is the session's own
//! to say, beside the players: it is the group this device has a session
//! with on the person's device fabric.

use serde_json::{Map, Value};
use tilo_actions::ActionAddress;
use tilo_quick_settings::QsEffect;
use tilo_quick_settings::device::SessionRequest;
use tilo_quick_settings::hosted::DevicesReading;

use crate::{Shared, devices};

/// The media resource the audio service publishes.
const MEDIA: &str = tilo_audio_service::media::MEDIA;

/// Land what the audio service reads of the players, if it publishes it and
/// this session has a device to show it on. Returns whether the person's
/// panel has that reading now.
pub(crate) fn land(shared: &Shared) -> bool {
    let Some(device) = &shared.device else {
        return false;
    };
    // The reading carries each player's cover, so it is taken in only when a
    // service published something, or the sound moved onto or off a speaker
    // group, since the last pass.
    let revision = shared.services.borrow().revision();
    let group = playing_on(shared);
    let fed = (revision, group.as_ref().map(|(group, _)| *group));
    if shared.media_fed.get() == Some(fed) {
        return false;
    }
    shared.media_fed.set(Some(fed));
    let Some(reading) = shared.services.borrow().read(MEDIA) else {
        return false;
    };
    match serde_json::from_value::<tilo_audio_service::media::Media>(reading) {
        Ok(media) => {
            device.media(media, group.and_then(|(_, name)| name));
            true
        }
        Err(error) => {
            tracing::warn!(%error, "the media reading did not parse");
            false
        }
    }
}

/// The speaker group this device plays its sound on, as its id and its
/// name, while it plays on one.
fn playing_on(shared: &Shared) -> Option<([u8; 16], Option<String>)> {
    let session = tilo_speaker_service::groups::outbound_playback_plans()
        .into_iter()
        .next()?;
    let fabric = shared.fabric.as_ref()?;
    let groups = fabric.as_ref().device_fabric();
    let name = groups
        .speaker_group(session.group)
        .map(|group| group.name.clone());
    Some((session.group.0, name))
}

/// Carry out what the person asked of what plays through the audio service.
/// Returns whether this was one; `false` leaves it to the other things the
/// session follows.
pub(crate) fn follow(shared: &Shared, request: &SessionRequest) -> bool {
    let SessionRequest::Effect(effect) = request else {
        return false;
    };
    let Some((action, arguments)) = media_action(shared, effect) else {
        return false;
    };
    let address = ActionAddress::new(MEDIA, action);
    match shared.doors.invoke(&address, &arguments) {
        Ok(_) => {
            // A hand-off continued here is taken: the reading the card shows
            // stops offering it.
            if matches!(effect, QsEffect::ResumePlayback { .. }) {
                shared.handoff.borrow_mut().take();
            }
            true
        }
        Err(error) => {
            // The service reads the players again after every control, so a
            // refusal is answered on screen by the reading, not by a
            // notification: the card's controls go back to how they were.
            tracing::debug!(%error, %action, "what plays did not change");
            true
        }
    }
}

/// The action of the `media` resource that carries out `effect`, with its
/// arguments; `None` for an effect that does not steer what plays.
fn media_action(shared: &Shared, effect: &QsEffect) -> Option<(&'static str, Map<String, Value>)> {
    let mut arguments = Map::new();
    let action = match effect {
        QsEffect::PlayPause => "play-pause",
        QsEffect::PreviousTrack => "previous",
        QsEffect::NextTrack => "next",
        QsEffect::Seek { fraction } => {
            arguments.insert("fraction".into(), Value::from(*fraction));
            "seek"
        }
        QsEffect::ResumePlayback { request } => {
            // The card names the hand-off the session publishes, so what
            // continuing it here names is that reading's own.
            let offer = published_offer(shared).filter(|offer| offer.request_id == *request)?;
            arguments.insert("request".into(), Value::from(offer.request_id.clone()));
            arguments.insert("app".into(), Value::from(offer.package_id.clone()));
            match &offer.source {
                media::handoff::HandoffSource::RemoteStream { url } => {
                    arguments.insert("url".into(), Value::from(url.clone()));
                }
                media::handoff::HandoffSource::LocalFile { path } => {
                    arguments.insert("path".into(), Value::from(path.to_string_lossy()));
                }
                // What the other device played off a workspace or an app's
                // own resource has to be brought over first: the card
                // refuses these itself.
                media::handoff::HandoffSource::Workspace { .. }
                | media::handoff::HandoffSource::AppResource { .. } => return None,
            }
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a playhead in a recording is far inside u64's milliseconds"
            )]
            let position_ms = offer.position_ms.max(0.0).round() as u64;
            arguments.insert("position_ms".into(), Value::from(position_ms));
            arguments.insert("playing".into(), Value::from(offer.playing));
            "resume"
        }
        _ => return None,
    };
    Some((action, arguments))
}

/// The hand-off another of the person's devices made this one, as the
/// session publishes it beside their devices: what the card offers to
/// continue here.
fn published_offer(shared: &Shared) -> Option<tilo_quick_settings::MediaOffer> {
    let published = shared.services.borrow().read(devices::RESOURCE)?;
    serde_json::from_value::<DevicesReading>(published)
        .ok()?
        .offer
}
