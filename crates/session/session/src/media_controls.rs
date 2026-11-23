//! The system's media controls over every Playback the apps here play.
//!
//! A tilolang app never publishes a player itself: it writes its Playback resource, and the host
//! of its windows says what that Playback holds ([`runtime_portal::process_link::MediaView`]).
//! One board here decides which of them a bare media key reaches — the one a person last touched
//! — publishes each to the desktop and turns what the desktop asks into the Playback's own
//! action, run by the host that plays it. Music, Video, Tube and any other app that plays reach
//! the keys and the now-playing widget the same way.

use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, TryRecvError};

use runtime_portal::process_link::{MediaCommand, MediaView};
use tilo_runtime::device::processes::MediaChange;
use tilo_media_controls::{Command, MediaControls, PlayerView, Status};
use tilo_service::Wake;

/// The controls over the apps this session runs, and what each of them last said it plays.
pub(crate) struct Serving {
    controls: MediaControls,
    /// What the apps' hosts said since the last pass.
    changes: Receiver<MediaChange>,
    /// What plays now, by the Playback's id.
    playing: BTreeMap<String, PlayerView>,
}

impl Serving {
    /// Show the Playbacks the session's apps play: what a control asks runs through `apps`'
    /// processes as the Playback's own action. Nothing is served where the session starts no
    /// app or has no runtime to serve the desktop's controls on.
    pub(crate) fn start(apps: &crate::apps::Apps, wake: Wake) -> Option<Self> {
        let processes = apps.processes()?;
        let runtime = tilo_rt::try_app().ok()?;
        let (sends, changes) = std::sync::mpsc::channel();
        // What the hosts say comes on their own threads: it is queued for the loop, which is
        // woken to take it.
        let sends = std::sync::Mutex::new(sends);
        processes.on_media(std::sync::Arc::new(move |change| {
            if let Ok(sends) = sends.lock() {
                let _ = sends.send(change);
            }
            wake();
        }));
        let controls = MediaControls::start(runtime.handle(), {
            std::sync::Arc::new(move |id: &str, command: Command| {
                let _ = processes.media_command(id, said(command));
            })
        });
        Some(Self {
            controls,
            changes,
            playing: BTreeMap::new(),
        })
    }

    /// Show what the apps said since the last pass: a Playback that moved is published again,
    /// and one that stopped leaves the desktop's controls.
    pub(crate) fn follow(&mut self) {
        let mut moved = false;
        loop {
            match self.changes.try_recv() {
                Ok(MediaChange::Playing(view)) => {
                    self.playing.insert(view.id.clone(), player(view));
                    moved = true;
                }
                Ok(MediaChange::Stopped(id)) => moved |= self.playing.remove(&id).is_some(),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        if moved {
            self.controls
                .update(self.playing.values().cloned().collect());
        }
    }
}

/// One Playback as the desktop's controls show it.
fn player(view: MediaView) -> PlayerView {
    PlayerView {
        id: view.id,
        identity: view.identity,
        title: view.title,
        artist: view.artist,
        album: String::new(),
        art_url: view.art_url,
        status: if view.playing {
            Status::Playing
        } else {
            Status::Paused
        },
        position_us: view.position_us,
        length_us: view.length_us,
        volume: view.volume,
        rate: view.rate,
        can_next: view.can_next,
        can_prev: view.can_prev,
        can_seek: view.length_us > 0,
        can_play: true,
        can_pause: true,
    }
}

/// The control the desktop asks, as the host that plays it is told.
fn said(command: Command) -> MediaCommand {
    match command {
        Command::Play => MediaCommand::Play,
        Command::Pause => MediaCommand::Pause,
        Command::Toggle => MediaCommand::Toggle,
        Command::Stop => MediaCommand::Stop,
        Command::Next => MediaCommand::Next,
        Command::Previous => MediaCommand::Previous,
        Command::SeekBy(micros) => MediaCommand::SeekBy(micros),
        Command::SeekTo(micros) => MediaCommand::SeekTo(micros),
        Command::SetVolume(level) => MediaCommand::SetVolume(level),
        Command::Mute => MediaCommand::Mute,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A playing Playback as the host that plays it says it.
    fn view() -> MediaView {
        MediaView {
            id: "playback-1".to_owned(),
            identity: "Video".to_owned(),
            title: "Undersea cables".to_owned(),
            artist: "Veritasium".to_owned(),
            art_url: String::new(),
            playing: true,
            position_us: 12_000_000,
            length_us: 60_000_000,
            volume: 0.5,
            rate: 1.0,
            can_next: true,
            can_prev: true,
        }
    }

    #[test]
    fn a_playback_a_host_says_is_one_the_controls_show() {
        let shown = player(view());
        assert_eq!(shown.id, "playback-1");
        assert_eq!(shown.title, "Undersea cables");
        assert_eq!(shown.status, Status::Playing);
        assert_eq!(shown.position_us, 12_000_000);
        assert!(shown.can_seek, "a Playback with a length can be seeked");
        let mut paused = view();
        paused.playing = false;
        assert_eq!(player(paused).status, Status::Paused);
    }

    #[test]
    fn every_control_the_desktop_asks_is_the_playback_action_it_names() {
        assert_eq!(said(Command::Toggle), MediaCommand::Toggle);
        assert_eq!(said(Command::Next), MediaCommand::Next);
        assert_eq!(said(Command::SeekBy(-15_000_000)), MediaCommand::SeekBy(-15_000_000));
        assert_eq!(said(Command::SetVolume(0.25)), MediaCommand::SetVolume(0.25));
    }
}
