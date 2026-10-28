//! What a locked session sees of media and may do to it: the one player the
//! lock card follows, the change that repaints it, and the single transport
//! command it can send. All three answer the same question — which player is
//! the lock card talking to — so they belong together, apart from the run loop
//! that only calls them. A locked session reads no other player, and nothing
//! but the lock card sends transport, which is why media is only ever this
//! narrow here.

use std::time::Duration;

/// How often a locked session re-reads its players, so the lock card follows
/// a track change or a pause made elsewhere.
pub(super) const LOCK_MEDIA_REFRESH: Duration = Duration::from_secs(1);

/// The player the lock screen shows: the one the now-playing card would,
/// among players that are playing or paused. A stopped player has nothing
/// to control from a locked screen.
pub(super) fn lock_now_playing() -> Option<crate::shell::now_playing::PlayerSnapshot> {
    use crate::shell::now_playing::{MprisSource, NowPlayingSource, PlaybackStatus, active_index};
    let source = MprisSource;
    source.refresh();
    let players: Vec<_> = source
        .players()
        .into_iter()
        .filter(|player| player.status != PlaybackStatus::Stopped)
        .collect();
    let index = active_index(&players, None)?;
    players.into_iter().nth(index)
}

/// Whether the lock card must repaint: anything it shows changed. The
/// playhead is not shown on the lock screen, so its movement is not a change.
pub(super) fn lock_media_changed(
    before: Option<&crate::shell::now_playing::PlayerSnapshot>,
    after: Option<&crate::shell::now_playing::PlayerSnapshot>,
) -> bool {
    match (before, after) {
        (None, None) => false,
        (Some(before), Some(after)) => {
            before.bus_name != after.bus_name
                || before.status != after.status
                || before.title != after.title
                || before.artist != after.artist
                || before.artwork != after.artwork
                || before.can_play != after.can_play
                || before.can_pause != after.can_pause
                || before.can_go_next != after.can_go_next
                || before.can_go_previous != after.can_go_previous
        }
        _ => true,
    }
}

/// Send one lock-screen transport command to exactly the player the lock
/// card shows. Transport is all a locked session may do to media.
pub(super) fn send_lock_transport(
    player: Option<&crate::shell::now_playing::PlayerSnapshot>,
    transport: crate::shell::now_playing_panel::Transport,
) -> bool {
    use crate::shell::now_playing::{MprisSource, NowPlayingSource};
    let Some(player) = player.filter(|player| transport.enabled(player)) else {
        return false;
    };
    MprisSource.send(&player.bus_name, transport.command())
}
