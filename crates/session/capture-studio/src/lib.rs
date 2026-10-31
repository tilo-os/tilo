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

mod i18n;
mod recorder;

pub use recorder::{
    Frame, REPLAY_BYTES, REPLAY_FPS, REPLAY_FRAMES, REPLAY_SECONDS, Recorder, Settings, State,
};

pub mod link;
pub mod resource;
pub mod surface;

pub use link::{Ask, Kept, Link, Recording, Refusal, Scene, Selection, Sent, Studio, Take};
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

    use crate::link::{Ask, Kept, Scene, Sent, Studio};
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
}
