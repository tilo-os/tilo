//! Driving whatever is playing, over MPRIS.
//!
//! The transport keys (play/pause, next, previous) are the one part of
//! the function row that is not a property of the machine — they address
//! *an application*, and which application is a question only the session
//! bus can answer. `org.mpris.MediaPlayer2` is that answer: every player
//! worth the name owns a `org.mpris.MediaPlayer2.<something>` bus name
//! and implements `Player.PlayPause` / `.Next` / `.Previous` on it. qOS's
//! own Music app publishes one (`apps/music/src/mpris.rs`); so does
//! anything running under the launcher's `via-host` D-Bus proxy, which
//! explicitly allows the MPRIS interface through.
//!
//! # Why a worker thread with a deadline
//!
//! This is called from the compositor's input path, and a D-Bus round
//! trip to a *wedged* peer has no natural bound — the default reply
//! timeout is tens of seconds. A frame loop cannot wait on that. So the
//! bus work happens on a throwaway thread and the caller waits on a
//! channel with a short deadline: a healthy player answers in under a
//! millisecond, and an unhealthy one costs one dropped OSD instead of a
//! frozen desktop. The orphaned thread finishes on its own and exits.
//!
//! `zbus::block_on` also cannot run inside a tokio worker, and the
//! compositor has a runtime; a plain `std::thread` has no runtime context
//! attached, which is the second reason the work goes off-loop.

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

/// What happened, so the OSD can report the resulting state rather than
/// the button that was pressed. `PlayPause` is a toggle, and a slab that
/// said "Play" when it had in fact paused would be worse than none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    NowPlaying,
    NowPaused,
    Skipped,
}

/// Send `cmd` to the active player.
///
/// Returns `None` when there is no player on the bus, when the bus itself
/// is unreachable, or when the player did not answer inside
/// [`DEADLINE`] — all cases where the caller should show nothing rather
/// than claim something happened.
pub fn send(cmd: Command) -> Option<Outcome> {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("qos-mpris".to_string())
        .spawn(move || {
            // A send failure means the caller already gave up; the value
            // is simply dropped.
            let _ = tx.send(run(cmd));
        });
    if let Err(e) = spawned {
        tracing::warn!(target: "qos::mpris", error = %e, "could not spawn mpris worker");
        return None;
    }
    match rx.recv_timeout(DEADLINE) {
        Ok(outcome) => outcome,
        Err(_) => {
            tracing::debug!(target: "qos::mpris", "no player answered within the deadline");
            None
        }
    }
}

/// The bus work. Runs on the worker thread.
fn run(cmd: Command) -> Option<Outcome> {
    let conn = zbus::blocking::Connection::session()
        .map_err(|e| {
            tracing::debug!(target: "qos::mpris", error = %e, "no session bus");
        })
        .ok()?;

    let name = active_player(&conn)?;
    let proxy = zbus::blocking::Proxy::new(&conn, name.as_str(), MPRIS_PATH, PLAYER_IFACE)
        .ok()?;

    // Read the status *before* acting, so a toggle can report what it
    // toggled to. Reading afterwards would race the player's own state
    // change, which is asynchronous on its side.
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

/// Pick the player to drive.
///
/// A machine can have several — a browser tab, a music app, a video
/// player — and the one the user means is the one making sound. So a
/// player reporting `Playing` wins; failing that, the first MPRIS name on
/// the bus, which is the only defensible choice when everything is
/// paused and lets play/pause resume the last thing that was open.
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
        let Ok(proxy) =
            zbus::blocking::Proxy::new(conn, name.as_str(), MPRIS_PATH, PLAYER_IFACE)
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
        // These are wire names in the MPRIS spec, not ours to rename —
        // a typo here is a transport key that silently does nothing on
        // every player in existence.
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

    /// The compositor must survive a machine with no session bus and no
    /// player — which is exactly what CI is. `send` has to come back
    /// within its deadline and report nothing, not hang and not panic.
    #[test]
    fn send_is_bounded_and_reports_nothing_when_there_is_no_player() {
        let started = std::time::Instant::now();
        let outcome = send(Command::PlayPause);
        let elapsed = started.elapsed();
        assert!(
            elapsed < DEADLINE * 4,
            "the input path must not wait on the bus: took {elapsed:?}"
        );
        // The other half of this test's name, now actually checked. `run`
        // gives up on its first line when there is no session bus, so on
        // such a machine — CI, a headless build container — the outcome has
        // to be absence. A developer machine with a bus and a player
        // legitimately returns `Some`, which is why the claim is made
        // against the same precondition production turns on rather than
        // unconditionally. What stood here was `assert!(true)` inside an
        // `is_none()` branch: it held whether `send` reported absence, a
        // fabricated outcome, or nothing at all.
        if zbus::blocking::Connection::session().is_err() {
            assert!(
                outcome.is_none(),
                "there is no session bus, so no player can have been reached, \
                 yet send() reported an outcome"
            );
        }
    }
}
