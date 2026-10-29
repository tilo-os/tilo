//! Pausing whatever plays, over MPRIS, when a private output goes away.

use zbus::blocking::{Connection, Proxy, fdo::DBusProxy};

const PREFIX: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

/// Pause every player on the session bus that reports it is playing.
///
/// # Errors
///
/// When the session bus cannot be reached or its names cannot be listed. A
/// single player that does not answer is skipped.
pub fn pause_playing_players() -> zbus::Result<()> {
    let connection = Connection::session()?;
    let names = DBusProxy::new(&connection)?.list_names()?;
    for name in names
        .iter()
        .filter(|name| name.as_str().starts_with(PREFIX))
    {
        let Ok(player) = Proxy::new(&connection, name.as_str(), PATH, PLAYER) else {
            continue;
        };
        let playing = player
            .get_property::<String>("PlaybackStatus")
            .is_ok_and(|status| status == "Playing");
        if playing && let Err(error) = player.call_method("Pause", &()) {
            tracing::debug!(target: "tilo::audio_policy", %error, "player did not pause on unplug");
        }
    }
    Ok(())
}
