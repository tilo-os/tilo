//! macOS: `MPNowPlayingInfoCenter` shows what is playing in Control Center
//! and on the lock screen, and `MPRemoteCommandCenter` delivers the media
//! keys and those controls' requests. Both are process-wide; the command
//! handlers run on the main queue and pass each request to the session's
//! handler.

use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSDictionary, NSNumber, NSString};
use objc2_media_player::{
    MPChangePlaybackPositionCommandEvent, MPMediaItemPropertyAlbumTitle, MPMediaItemPropertyArtist,
    MPMediaItemPropertyPlaybackDuration, MPMediaItemPropertyTitle, MPNowPlayingInfoCenter,
    MPNowPlayingInfoPropertyElapsedPlaybackTime, MPNowPlayingInfoPropertyPlaybackRate,
    MPNowPlayingPlaybackState, MPRemoteCommand, MPRemoteCommandCenter, MPRemoteCommandEvent,
    MPRemoteCommandHandlerStatus,
};

use super::{NowPlaying, NowPlayingAction, NowPlayingHandler, NowPlayingState, PlaybackStatus};

pub(crate) struct Session {
    center: Retained<MPNowPlayingInfoCenter>,
    /// Each command with the target its handler registered, removed on drop.
    targets: Vec<(Retained<MPRemoteCommand>, Retained<AnyObject>)>,
    playing: Arc<AtomicBool>,
}

type Handler = RcBlock<dyn Fn(NonNull<MPRemoteCommandEvent>) -> MPRemoteCommandHandlerStatus>;

impl Session {
    pub(crate) fn start(on_action: &NowPlayingHandler) -> Self {
        let playing = Arc::new(AtomicBool::new(false));
        let queue = |action: fn(&MPRemoteCommandEvent, bool) -> Option<NowPlayingAction>| {
            let on_action = on_action.clone();
            let playing = playing.clone();
            let handler: Handler = RcBlock::new(move |event: NonNull<MPRemoteCommandEvent>| {
                // SAFETY: the command center passes a valid event for the call.
                let event = unsafe { event.as_ref() };
                match action(event, playing.load(Ordering::Relaxed)) {
                    Some(action) => {
                        on_action(action);
                        MPRemoteCommandHandlerStatus::Success
                    }
                    None => MPRemoteCommandHandlerStatus::CommandFailed,
                }
            });
            handler
        };
        // SAFETY: the shared center and its commands are process-wide
        // objects; adding a block target is how they are observed.
        let targets = unsafe {
            let commands = MPRemoteCommandCenter::sharedCommandCenter();
            let wired: [(Retained<MPRemoteCommand>, Handler); 7] = [
                (
                    commands.playCommand(),
                    queue(|_, _| Some(NowPlayingAction::Play)),
                ),
                (
                    commands.pauseCommand(),
                    queue(|_, _| Some(NowPlayingAction::Pause)),
                ),
                (
                    commands.togglePlayPauseCommand(),
                    queue(|_, playing| {
                        Some(if playing {
                            NowPlayingAction::Pause
                        } else {
                            NowPlayingAction::Play
                        })
                    }),
                ),
                (
                    commands.stopCommand(),
                    queue(|_, _| Some(NowPlayingAction::Stop)),
                ),
                (
                    commands.nextTrackCommand(),
                    queue(|_, _| Some(NowPlayingAction::Next)),
                ),
                (
                    commands.previousTrackCommand(),
                    queue(|_, _| Some(NowPlayingAction::Previous)),
                ),
                (
                    Retained::into_super(commands.changePlaybackPositionCommand()),
                    queue(|event, _| {
                        let event = event.downcast_ref::<MPChangePlaybackPositionCommandEvent>()?;
                        let seconds = event.positionTime();
                        Duration::try_from_secs_f64(seconds)
                            .ok()
                            .map(NowPlayingAction::SeekTo)
                    }),
                ),
            ];
            wired
                .into_iter()
                .map(|(command, handler)| {
                    command.setEnabled(true);
                    let target = command.addTargetWithHandler(&handler);
                    (command, target)
                })
                .collect()
        };
        Self {
            // SAFETY: the process's default center.
            center: unsafe { MPNowPlayingInfoCenter::defaultCenter() },
            targets,
            playing,
        }
    }

    pub(crate) fn update(&mut self, now: &NowPlaying, state: &NowPlayingState) {
        let playing = state.status == PlaybackStatus::Playing;
        self.playing.store(playing, Ordering::Relaxed);
        let mut keys: Vec<&NSString> = Vec::new();
        let mut values: Vec<Retained<AnyObject>> = Vec::new();
        let mut text = |key: &'static NSString, value: &str| {
            if !value.is_empty() {
                keys.push(key);
                values.push(Retained::into_super(Retained::into_super(
                    NSString::from_str(value),
                )));
            }
        };
        // SAFETY: MediaPlayer's constant keys are valid static strings.
        unsafe {
            text(MPMediaItemPropertyTitle, &now.title);
            text(MPMediaItemPropertyArtist, &now.artist);
            text(MPMediaItemPropertyAlbumTitle, &now.album);
        }
        let mut number = |key: &'static NSString, value: f64| {
            keys.push(key);
            values.push(Retained::into_super(Retained::into_super(
                Retained::into_super(NSNumber::new_f64(value)),
            )));
        };
        // SAFETY: as above.
        unsafe {
            if let Some(duration) = state.duration {
                number(MPMediaItemPropertyPlaybackDuration, duration.as_secs_f64());
            }
            number(
                MPNowPlayingInfoPropertyElapsedPlaybackTime,
                state.position.as_secs_f64(),
            );
            number(
                MPNowPlayingInfoPropertyPlaybackRate,
                if playing { state.rate } else { 0.0 },
            );
        }
        let info = NSDictionary::from_retained_objects(&keys, &values);
        let status = match state.status {
            PlaybackStatus::Playing => MPNowPlayingPlaybackState::Playing,
            PlaybackStatus::Paused => MPNowPlayingPlaybackState::Paused,
            PlaybackStatus::Stopped => MPNowPlayingPlaybackState::Stopped,
        };
        // SAFETY: the dictionary holds only the documented keys and types.
        unsafe {
            self.center.setNowPlayingInfo(Some(&info));
            self.center.setPlaybackState(status);
        }
    }

    #[cfg(test)]
    fn published_title(&self) -> Option<String> {
        // SAFETY: reading back the dictionary this session set.
        let info = unsafe { self.center.nowPlayingInfo() }?;
        // SAFETY: as above.
        let title = info.objectForKey(unsafe { MPMediaItemPropertyTitle })?;
        Some(title.downcast::<NSString>().ok()?.to_string())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: removing the targets this session added, and clearing the
        // entry it published.
        unsafe {
            for (command, target) in &self.targets {
                command.removeTarget(Some(target));
            }
            self.center.setNowPlayingInfo(None);
            self.center
                .setPlaybackState(MPNowPlayingPlaybackState::Stopped);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_session_publishes_to_the_info_center_and_withdraws() {
        let mut session = Session::start(&(Arc::new(|_| {}) as NowPlayingHandler));
        session.update(
            &NowPlaying {
                title: "Aria".into(),
                artist: "Tilo".into(),
                album: String::new(),
                artwork_url: None,
            },
            &NowPlayingState {
                status: PlaybackStatus::Playing,
                position: Duration::from_secs(3),
                duration: Some(Duration::from_secs(180)),
                rate: 1.0,
            },
        );
        assert_eq!(session.published_title().as_deref(), Some("Aria"));
        let center = session.center.clone();
        drop(session);
        // SAFETY: reading the default center's entry.
        assert!(unsafe { center.nowPlayingInfo() }.is_none());
    }
}
