//! Driving whatever is playing, over MPRIS.
//!
//! The transport keys (play/pause, next, previous) address *an
//! application*, not the machine, so they go over `org.mpris.MediaPlayer2`
//! on the session bus — the interface every real player and qOS's own
//! Music app (`apps/music/src/mpris.rs`) implements.
//!
//! Called from the compositor's input path, but a D-Bus round trip has no
//! natural bound (tens of seconds) and `zbus::block_on` can't run inside
//! the compositor's tokio runtime — so the bus work happens on a throwaway
//! `std::thread`, and the caller waits on a channel with a short deadline:
//! a healthy player answers in under a millisecond, an unhealthy one costs
//! one dropped OSD instead of a frozen desktop.

use std::sync::mpsc;
use std::time::Duration;

/// How long the caller waits for the player to answer before giving up.
const DEADLINE: Duration = Duration::from_millis(250);

/// The MPRIS well-known name prefix. Every player owns one.
const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";

/// A transport command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    PlayPause,
    Next,
    Previous,
}

impl Command {
    fn method(self) -> &'static str {
        match self {
            Command::PlayPause => "PlayPause",
            Command::Next => "Next",
            Command::Previous => "Previous",
        }
    }
}

/// So the OSD reports the resulting state, not the button pressed —
/// `PlayPause` is a toggle, and a wrong label would be worse than none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    NowPlaying,
    NowPaused,
    Skipped,
}

/// Send `cmd` to the active player. Returns `None` when there is no
/// player, the bus is unreachable, or the player didn't answer inside
/// [`DEADLINE`] — cases where the caller should show nothing rather than
/// claim something happened.
pub fn send(cmd: Command) -> Option<Outcome> {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("qos-mpris".to_string())
        .spawn(move || {
            let _ = tx.send(run(cmd)); // dropped if the caller already gave up
        });
    if let Err(e) = spawned {
        tracing::warn!(target: "qos::mpris", error = %e, "could not spawn mpris worker");
        return None;
    }
    if let Ok(outcome) = rx.recv_timeout(DEADLINE) {
        outcome
    } else {
        tracing::debug!(target: "qos::mpris", "no player answered within the deadline");
        None
    }
}

fn run(cmd: Command) -> Option<Outcome> {
    let conn = zbus::blocking::Connection::session()
        .map_err(|e| {
            tracing::debug!(target: "qos::mpris", error = %e, "no session bus");
        })
        .ok()?;

    let name = active_player(&conn)?;
    let proxy = zbus::blocking::Proxy::new(&conn, name.as_str(), MPRIS_PATH, PLAYER_IFACE).ok()?;

    // Read status *before* acting: reading after would race the player's
    // own (asynchronous) state change.
    let was_playing = matches!(playback_status(&proxy).as_deref(), Some("Playing"));

    proxy
        .call_method(cmd.method(), &())
        .map_err(|e| {
            tracing::debug!(target: "qos::mpris", error = %e, method = cmd.method(), "player rejected the command");
        })
        .ok()?;

    Some(match cmd {
        Command::PlayPause => {
            if was_playing {
                Outcome::NowPaused
            } else {
                Outcome::NowPlaying
            }
        }
        Command::Next | Command::Previous => Outcome::Skipped,
    })
}

/// A machine can have several players; the one reporting `Playing` wins,
/// else the first MPRIS name on the bus, so play/pause resumes whatever
/// was last open.
fn active_player(conn: &zbus::blocking::Connection) -> Option<String> {
    let dbus = zbus::blocking::fdo::DBusProxy::new(conn).ok()?;
    let names = dbus.list_names().ok()?;
    let players: Vec<String> = names
        .into_iter()
        .map(|n| n.as_str().to_string())
        .filter(|n| n.starts_with(MPRIS_PREFIX))
        .collect();
    if players.is_empty() {
        return None;
    }
    for name in &players {
        let Ok(proxy) = zbus::blocking::Proxy::new(conn, name.as_str(), MPRIS_PATH, PLAYER_IFACE)
        else {
            continue;
        };
        if matches!(playback_status(&proxy).as_deref(), Some("Playing")) {
            return Some(name.clone());
        }
    }
    players.into_iter().next()
}

fn playback_status(proxy: &zbus::blocking::Proxy<'_>) -> Option<String> {
    proxy.get_property::<String>("PlaybackStatus").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_map_to_the_spec_method_names() {
        // Wire names in the MPRIS spec, not ours to rename.
        assert_eq!(Command::PlayPause.method(), "PlayPause");
        assert_eq!(Command::Next.method(), "Next");
        assert_eq!(Command::Previous.method(), "Previous");
    }

    #[test]
    fn the_well_known_names_match_the_spec() {
        assert_eq!(MPRIS_PREFIX, "org.mpris.MediaPlayer2.");
        assert_eq!(MPRIS_PATH, "/org/mpris/MediaPlayer2");
        assert_eq!(PLAYER_IFACE, "org.mpris.MediaPlayer2.Player");
    }

    /// Must survive a machine with no session bus and no player — which
    /// is exactly what CI is.
    #[test]
    fn send_is_bounded_and_reports_nothing_when_there_is_no_player() {
        let started = std::time::Instant::now();
        let outcome = send(Command::PlayPause);
        let elapsed = started.elapsed();
        assert!(
            elapsed < DEADLINE * 4,
            "the input path must not wait on the bus: took {elapsed:?}"
        );
        // A dev machine with a player running legitimately returns Some; on
        // a machine with none (CI), `outcome` is `None` — either is correct,
        // the assertion above is the only hard requirement.
        let _ = outcome;
    }
}
