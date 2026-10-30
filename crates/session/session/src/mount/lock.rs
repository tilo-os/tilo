//! What the lock screen reads and steers beyond the password: the song that
//! plays and its transport. The screen's presentation and its shown state
//! live in its seat (`tilo_sheets::lock`); this is the session's side of
//! them, kept apart from the rest of the host so the lock can be read on its
//! own.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_service::Services;
use tilo_sheets::lock::{LockEffect, Media};

use crate::MEDIA;
use crate::panels;

/// What plays, as the lock screen shows it: the player the now-playing card
/// follows, and only what a locked screen may do to it — no queue, no
/// scrubber, no choice of player.
pub fn media(readings: &panels::Readings) -> Option<Media> {
    let readings = readings.borrow();
    let readings = readings.as_ref()?.current();
    let now = readings.now_playing.as_ref()?;
    Some(Media {
        title: now.title.clone(),
        subtitle: now.subtitle.clone(),
        playing: now.playing,
        can_toggle: now.can_toggle,
        can_previous: now.can_previous,
        can_next: now.can_next,
    })
}

/// Carry out what the screen's transport asked: play, pause, or the song
/// before or after this one. `false` when `asked` was not the transport's,
/// so the caller knows nothing was done.
///
/// A locked session withholds every published action but the screen's own,
/// so this cannot go through the door a media key uses: it is the session
/// asking the service that plays, straight. With nothing playing the screen
/// shows no transport, and nothing is asked of a player.
pub fn transport(
    asked: &LockEffect,
    readings: &panels::Readings,
    services: &Rc<RefCell<Services>>,
) -> bool {
    let action = match asked {
        LockEffect::PlayPause => "play-pause",
        LockEffect::NextTrack => "next",
        LockEffect::PreviousTrack => "previous",
        _ => return false,
    };
    if media(readings).is_none() {
        return true;
    }
    let asked = services
        .borrow_mut()
        .invoke(MEDIA, action, &serde_json::Map::new());
    match asked {
        Some(Ok(_)) => {}
        Some(Err(error)) => tracing::warn!(%error, "the lock screen's transport"),
        None => tracing::debug!("nothing publishes the player"),
    }
    true
}
