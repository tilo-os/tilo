//! The capture studio: the bar that takes a screenshot, records the screen
//! into a file or keeps the last half minute ready to clip, as an app the
//! session shows over the desktop.
//!
//! A recorder is fed the desktop's own frames — what the compositor draws,
//! read back from it — and encodes them once with the shared media stack,
//! in the most compatible codec this machine can run
//! ([`tilo_callstream::recording_encoder`]). While it records, the access
//! units go into a file ([`tilo_callstream::mp4_recording`]); always, they
//! also go into the replay buffer, which keeps the last half minute
//! encoded so "clip what just happened" needs no second encode and no
//! waiting.
//!
//! The replay buffer is bounded in frames and in bytes and always starts at
//! a keyframe: a clip that began in the middle of a group of pictures could
//! not be decoded.
//!
//! The app itself draws and asks, and the session captures: only the session
//! has the compositor, so the bar puts what the person chose on a [`Link`]
//! ([`Ask`]) and the session carries it out and hands back what came of it
//! ([`Scene`]). Its actions ([`CaptureEffect`]) reach the same asks through
//! the normal action door, so a shortcut, the palette and an agent take a
//! screenshot the way the bar does.

pub mod audio;
mod i18n;
mod recorder;

pub use recorder::{
    Frame, REPLAY_BYTES, REPLAY_FPS, REPLAY_FRAMES, REPLAY_SECONDS, Recorder, Settings, State,
    Unfinished, extension,
};

pub mod link;
pub mod resource;
pub mod surface;

pub use link::{
    Ask, Failed, Failure, Finished, Kept, Link, Recording, Refusal, Scene, Selection, Sent, Studio,
    Take,
};
pub use resource::{Capture, CaptureEffect, CaptureStudioApp};
pub use surface::StudioSurface;

/// The studio's package id.
pub const APP_ID: &str = "so.tilo.capture-studio";

/// The resource the studio's actions are published on, which a key that
/// takes a screenshot addresses.
pub const RESOURCE: &str = <Capture as ui::tilo_resource::Resource>::KIND;

/// The studio's toolkit definition, the one every host mounts, carrying its
/// committed surface.
#[must_use]
pub fn definition() -> &'static ui::prelude::ApplicationDefinition {
    ui::built_in_definition!(CaptureStudioApp)
}

#[cfg(test)]
mod tests {
    use tilo_sheets::Seat as _;
    use ui::vello::kurbo::Rect;
    use ui::widget::{CapturePreview, ScreenshotKept, ScreenshotSendDevice, Widget};

    use crate::link::{Ask, Finished, Kept, Recording, Scene, Sent, Studio};
    use crate::surface::StudioSurface;

    /// The screenshot the studio wrote, with the receipt that shows it.
    fn capture(path: std::path::PathBuf) -> Kept {
        Kept {
            kept: ScreenshotKept::SAVED,
            path,
            width: 2,
            height: 1,
            preview: CapturePreview {
                columns: 1,
                rows: 1,
                pixels: vec![[1, 2, 3, 255]],
            },
        }
    }

    /// Where the studio drew `name` this frame.
    fn rect_of(frame: &ui::headless::FrameHarness, name: &str) -> Rect {
        let rects = Widget::test_rects(frame.root::<StudioSurface>());
        match rects.into_iter().find(|(target, _)| target == name) {
            Some((_, rect)) => rect,
            None => panic!("nothing named {name} is on screen"),
        }
    }

    /// Whether the studio drew `name` at `at`.
    fn has(frame: &ui::headless::FrameHarness, at: ui::time::Instant, name: &str) -> bool {
        tilo_clock::testing::with_monotonic_time(at, || {
            Widget::test_rects(frame.root::<StudioSurface>())
                .into_iter()
                .any(|(target, _)| target == name)
        })
    }

    /// Where the studio drew `name` at `at`.
    fn rect_at(frame: &ui::headless::FrameHarness, at: ui::time::Instant, name: &str) -> Rect {
        tilo_clock::testing::with_monotonic_time(at, || rect_of(frame, name))
    }

    /// Press what the studio drew as `name` at `at`, where it drew it then.
    fn press(frame: &mut ui::headless::FrameHarness, at: ui::time::Instant, name: &str) {
        tilo_clock::testing::with_monotonic_time(at, || {
            let center = rect_of(frame, name).center();
            frame.click_at(center.x, center.y);
        });
    }

    /// A frame at `at`: the studio's animation is laid out where the test puts
    /// it, so a machine under load cannot move a picture between two frames.
    fn poll_at(frame: &mut ui::headless::FrameHarness, at: ui::time::Instant, what: &str) {
        let polled = tilo_clock::testing::with_monotonic_time(at, || frame.poll_frame());
        polled.unwrap_or_else(|error| panic!("{what}: {error:?}"));
    }

    /// A screenshot is offered to the device the person picks from its
    /// receipt, and the studio says what became of it.
    ///
    /// What the device receives is the session's to prove: the studio never
    /// touches the file.
    #[test]
    fn a_capture_can_be_sent_to_a_device() {
        let folder = tempfile::tempdir().expect("a folder");
        let path = folder.path().join("Screenshot 1.png");
        std::fs::write(&path, b"the screenshot").expect("a screenshot");
        let seat = Studio::new();
        let mut frame = ui::headless::FrameHarness::for_root(
            ui::state::State::new(),
            StudioSurface::new(Some(seat.clone())),
            (1280, 800),
            ui::theme::Theme::dark(),
        );
        let start = ui::time::now();
        let step = |frames: u64| start + ui::time::Duration::from_millis(frames * 250);
        seat.presentation().open(false);
        poll_at(&mut frame, step(1), "the bar comes up");
        let phone = ScreenshotSendDevice {
            id: "device-phone".into(),
            label: "Ada's phone".into(),
        };
        seat.link().publish(Scene {
            kept: Some(capture(path.clone())),
            devices: vec![phone.clone()],
            ..Scene::default()
        });
        poll_at(&mut frame, step(2), "the receipt shows");
        press(&mut frame, step(3), "capture.send-screenshot");
        poll_at(&mut frame, step(4), "the devices are offered");
        assert!(
            rect_at(&frame, step(4), "capture.send-to-device.device-phone").area() > 0.0,
            "the online device is offered by name"
        );
        press(&mut frame, step(5), "capture.send-to-device.device-phone");
        poll_at(&mut frame, step(6), "the pick is answered");
        assert_eq!(
            seat.link().take_asks(),
            [Ask::SendLastTo("device-phone".into())],
            "the session is asked for the last capture, by the picked device"
        );
        assert!(
            seat.presentation().is_open(),
            "the receipt stays up until the session says what became of the send"
        );
        seat.link().publish(Scene {
            kept: Some(capture(path)),
            devices: vec![phone],
            sent: Some(Sent::offered("Ada's phone".into())),
            ..Scene::default()
        });
        poll_at(&mut frame, step(7), "what became of it is said");
        assert!(
            rect_at(&frame, step(7), "capture.send-status").area() > 0.0,
            "the studio says where the screenshot went"
        );
        // Read, and then gone: the studio takes the receipt down once what it
        // says has been up long enough.
        poll_at(
            &mut frame,
            step(7) + ui::time::Duration::from_secs(10),
            "what became of it is read",
        );
        assert!(
            !seat.presentation().is_open(),
            "the receipt goes once what it says has been read"
        );
    }

    /// A screenshot is saved into the workspace's files from its receipt, and
    /// the studio says the name it landed under.
    ///
    /// What lands in the files is the session's to prove: the studio never
    /// touches them.
    #[test]
    fn a_capture_can_be_saved_into_the_workspace() {
        let folder = tempfile::tempdir().expect("a folder");
        let path = folder.path().join("Screenshot 1.png");
        std::fs::write(&path, b"the screenshot").expect("a screenshot");
        let seat = Studio::new();
        let mut frame = ui::headless::FrameHarness::for_root(
            ui::state::State::new(),
            StudioSurface::new(Some(seat.clone())),
            (1280, 800),
            ui::theme::Theme::dark(),
        );
        let start = ui::time::now();
        let step = |frames: u64| start + ui::time::Duration::from_millis(frames * 250);
        seat.presentation().open(false);
        poll_at(&mut frame, step(1), "the bar comes up");
        seat.link().publish(Scene {
            kept: Some(capture(path.clone())),
            ..Scene::default()
        });
        poll_at(&mut frame, step(2), "the receipt shows");
        press(&mut frame, step(3), "capture.send-screenshot");
        poll_at(&mut frame, step(4), "the picker is offered");
        assert!(
            rect_at(&frame, step(4), "capture.send-to-workspace").area() > 0.0,
            "the workspace's files are offered"
        );
        press(&mut frame, step(5), "capture.send-to-workspace");
        poll_at(&mut frame, step(6), "the pick is answered");
        assert_eq!(
            seat.link().take_asks(),
            [Ask::SendLastToWorkspace],
            "the session is asked to save the last capture"
        );
        assert!(
            seat.presentation().is_open(),
            "the receipt stays up until the session says what became of the save"
        );
        seat.link().publish(Scene {
            kept: Some(capture(path)),
            sent: Some(Sent::saved("Screenshot 1.png".into())),
            ..Scene::default()
        });
        poll_at(&mut frame, step(7), "what became of it is said");
        assert!(
            rect_at(&frame, step(7), "capture.send-status").area() > 0.0,
            "the studio names the file it was saved as"
        );
    }

    /// A recording that skipped frames says how many while it runs, and its
    /// receipt says what the file is missing and plays that file, never a
    /// screenshot taken since.
    #[test]
    fn a_recording_that_skipped_frames_says_so_and_plays_its_own_file() {
        let folder = tempfile::tempdir().expect("a folder");
        let recording = folder.path().join("Recording 1.mp4");
        std::fs::write(&recording, b"the recording").expect("a recording");
        let seat = Studio::new();
        let mut frame = ui::headless::FrameHarness::for_root(
            ui::state::State::new(),
            StudioSurface::new(Some(seat.clone())),
            (1280, 800),
            ui::theme::Theme::dark(),
        );
        let start = ui::time::now();
        let step = |frames: u64| start + ui::time::Duration::from_millis(frames * 250);
        seat.presentation().open(false);
        poll_at(&mut frame, step(1), "the bar comes up");
        seat.link().publish(Scene {
            recording: Some(Recording {
                path: recording.clone(),
                paused: false,
                skipped: 12,
            }),
            ..Scene::default()
        });
        poll_at(&mut frame, step(2), "the recording shows");
        assert!(
            has(&frame, step(2), "capture.recording-skipped"),
            "the bar says how many frames the recording skipped"
        );
        assert!(seat.link().take_asks().is_empty(), "nothing is asked yet");

        // The recording is finished: the bar gives way to a receipt that
        // says what the file is missing and plays it.
        seat.link().publish(Scene {
            finished: Some(Finished {
                path: Some(recording.clone()),
                skipped: 12,
                failure: None,
            }),
            ..Scene::default()
        });
        poll_at(&mut frame, step(3), "the receipt shows");
        assert!(
            !has(&frame, step(3), "capture.stop"),
            "the bar is down once the recording is finished"
        );
        press(&mut frame, step(4), "capture.play-recording");
        assert_eq!(
            seat.link().take_asks(),
            [Ask::Open(recording)],
            "the receipt plays the recording it is about"
        );
    }

    /// The bar goes down with the take, and the file comes only after the
    /// encoder finished it: what became of the recording is still said. A
    /// studio the shell took down stays down.
    #[test]
    fn a_finished_recording_comes_back_up_after_the_bar_put_itself_away() {
        let folder = tempfile::tempdir().expect("a folder");
        let recording = folder.path().join("Recording 1.mp4");
        std::fs::write(&recording, b"the recording").expect("a recording");
        let seat = Studio::new();
        let mut frame = ui::headless::FrameHarness::for_root(
            ui::state::State::new(),
            StudioSurface::new(Some(seat.clone())),
            (1280, 800),
            ui::theme::Theme::dark(),
        );
        let start = ui::time::now();
        let step = |frames: u64| start + ui::time::Duration::from_millis(frames * 250);
        seat.presentation().open(false);
        poll_at(&mut frame, step(1), "the bar comes up");
        seat.link().publish(Scene {
            recording: Some(Recording {
                path: recording.clone(),
                paused: false,
                skipped: 3,
            }),
            ..Scene::default()
        });
        poll_at(&mut frame, step(2), "the recording shows");
        assert!(
            has(&frame, step(2), "capture.stop"),
            "the bar shows the recording that runs"
        );

        // The person stops: the take is down at once, the encoder still has
        // the file to finish.
        seat.link().publish(Scene::default());
        poll_at(&mut frame, step(3), "the take is down");
        poll_at(&mut frame, step(4), "the bar put itself away");
        assert!(
            !seat.presentation().is_drawn(),
            "the studio is down while the file is finished"
        );

        // What the file is missing is still said: the receipt comes back up.
        seat.link().publish(Scene {
            finished: Some(Finished {
                path: Some(recording),
                skipped: 3,
                failure: None,
            }),
            ..Scene::default()
        });
        poll_at(&mut frame, step(5), "the receipt comes back");
        poll_at(&mut frame, step(6), "the receipt stays");
        assert!(
            seat.presentation().is_open(),
            "the shell shows the studio again"
        );
        assert!(
            has(&frame, step(6), "capture.play-recording"),
            "the receipt plays the recording it is about"
        );

        // The shell takes the studio down itself: that stays down.
        seat.presentation().close();
        poll_at(&mut frame, step(7), "the shell takes the studio down");
        poll_at(&mut frame, step(8), "the studio stays down");
        assert!(
            !has(&frame, step(8), "capture.play-recording"),
            "a studio the shell took down stays down"
        );
    }

    /// A recording the encoder could not finish says why, and keeps what it
    /// wrote: the receipt comes back up, shows the file, and does not offer
    /// to play a file that carries no trailer.
    #[test]
    fn a_recording_that_could_not_be_finished_says_so_and_shows_its_file() {
        let folder = tempfile::tempdir().expect("a folder");
        let recording = folder.path().join("Recording 1.mp4");
        std::fs::write(&recording, b"half a recording").expect("a recording");
        let seat = Studio::new();
        let mut frame = ui::headless::FrameHarness::for_root(
            ui::state::State::new(),
            StudioSurface::new(Some(seat.clone())),
            (1280, 800),
            ui::theme::Theme::dark(),
        );
        let start = ui::time::now();
        let step = |frames: u64| start + ui::time::Duration::from_millis(frames * 250);
        seat.presentation().open(false);
        poll_at(&mut frame, step(1), "the bar comes up");
        seat.link().publish(Scene {
            recording: Some(Recording {
                path: recording.clone(),
                paused: false,
                skipped: 0,
            }),
            ..Scene::default()
        });
        poll_at(&mut frame, step(2), "the recording shows");
        assert!(
            has(&frame, step(2), "capture.stop"),
            "the bar shows the recording that runs"
        );

        // The person stops, the bar puts itself away, and the encoder fails
        // to finish the file.
        seat.link().publish(Scene::default());
        poll_at(&mut frame, step(3), "the take is down");
        poll_at(&mut frame, step(4), "the bar put itself away");
        assert!(
            !seat.presentation().is_drawn(),
            "the studio is down while the file is finished"
        );
        seat.link().publish(Scene {
            finished: Some(Finished {
                path: Some(recording.clone()),
                skipped: 0,
                failure: Some("the muxer failed: Internal data stream error".to_owned()),
            }),
            ..Scene::default()
        });
        poll_at(&mut frame, step(5), "the failure comes back");
        poll_at(&mut frame, step(6), "the failure stays");
        assert!(
            seat.presentation().is_open(),
            "the shell shows the studio again, saying what became of the recording"
        );
        assert!(
            !has(&frame, step(6), "capture.play-recording"),
            "a file that was not finished is not played"
        );
        press(&mut frame, step(7), "capture.show-recording");
        assert_eq!(
            seat.link().take_asks(),
            [Ask::Reveal(recording)],
            "the receipt shows what was written in Files"
        );
    }

    /// A recording that kept no file says so too, and offers nothing to open.
    #[test]
    fn a_recording_that_kept_no_file_says_so_and_offers_nothing() {
        let seat = Studio::new();
        let mut frame = ui::headless::FrameHarness::for_root(
            ui::state::State::new(),
            StudioSurface::new(Some(seat.clone())),
            (1280, 800),
            ui::theme::Theme::dark(),
        );
        let start = ui::time::now();
        let step = |frames: u64| start + ui::time::Duration::from_millis(frames * 250);
        seat.presentation().open(false);
        poll_at(&mut frame, step(1), "the bar comes up");
        seat.link().publish(Scene {
            finished: Some(Finished {
                path: None,
                skipped: 0,
                failure: Some("no frame was written to the file".to_owned()),
            }),
            ..Scene::default()
        });
        poll_at(&mut frame, step(2), "the failure shows");
        assert!(
            seat.presentation().is_open(),
            "the studio says what became of the recording"
        );
        assert!(
            !has(&frame, step(2), "capture.show-recording")
                && !has(&frame, step(2), "capture.play-recording"),
            "there is no file to open"
        );
    }
}
