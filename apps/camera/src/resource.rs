//! Camera as a resource: its record ([`Record`]: the recent capture handoff
//! and the scan history) is the document; the shutter, devices and scanners
//! are live rows the view publishes.
//!
//! Everything else is the window's [`CameraEffect`] vocabulary: the screen,
//! mode and lens reveals and the capture preferences are offered on the
//! camera, while shutter, record, camera chooser, code-scanner import and
//! document-scanner effects ride the live rows the view publishes. Capture
//! preferences are settings — the
//! person's [`CapturePreferences`] and this device's
//! [`CameraDevicePreferences`] — never document state.

use ui::framework::{Command, Fixture, ResourceApp, ViewCx};
use ui::tilo_resource::Invoke;
use ui::widget::Element;

use crate::app::CameraApp;
use crate::model::{CaptureKind, CaptureMode, CapturePreferences, Facing, Timer};
use crate::record::{Record, SharedCapture};

/// The clock a screenshot reads instead of the machine's, in Unix seconds,
/// so capture times and their relative labels come out the same every run.
#[derive(Clone, Copy, Debug)]
pub struct ShotClock(pub i64);

/// When every screenshot is taken.
const SHOT_NOW: i64 = 1_800_000_000;

/// The screenshots: the viewfinder (photo, video, a self-timer counting
/// down), the code scanner, the document scanner (empty, with a page, cropping
/// one), the settings (top and scrolled), and the viewfinder when the camera
/// is missing, refused or broken.
const SCENARIOS: &[&str] = &[
    "video",
    "timer",
    "scanner",
    "document",
    "document-page",
    "document-crop",
    "settings",
    "settings-scrolled",
    "no-camera",
    "camera-permission",
    "camera-error",
];

/// The recent capture every screenshot hands off: a photo taken two minutes
/// before the shot.
pub(crate) fn sample_capture() -> SharedCapture {
    SharedCapture {
        id: "c1".into(),
        media_id: format!("{}{:064x}", crate::media::MEDIA_PREFIX, 1),
        kind: CaptureKind::Photo,
        ts: SHOT_NOW - 120,
        w: 4_032,
        h: 3_024,
        dur_ms: 0,
        label: "Golden hour".into(),
        facing: Facing::Back,
    }
}

/// Framework app: the camera widget is the window over its [`Record`].
pub struct CameraShell;

impl ResourceApp for CameraShell {
    type Resource = Record;
    const ID: &'static str = "so.tilo.camera";
    const TITLE: &'static str = "Camera";
    const ICON: Option<&'static str> = Some("camera");

    /// A live window drives the cameras and scanners of this device and
    /// writes captures to its picture folder; a screenshot or a past version
    /// shows sample frames and reaches nothing.
    fn view(cx: &mut ViewCx<Record>) -> Element {
        if cx.mode().is_live() {
            let app = CameraApp::disk();
            let app = match cx.content() {
                Some(content) => app.with_content(content),
                None => app,
            };
            crate::app::window(cx, app)
        } else {
            crate::app::window(cx, CameraApp::sample())
        }
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    /// The video scenario films; the timer scenario counts down from a
    /// three-second self-timer the person chose.
    fn fixture(fixture: &mut Fixture<Record>) {
        fixture.provide(ShotClock(SHOT_NOW));
        match fixture.scenario() {
            Some("video") => fixture.provide(CaptureMode::Video),
            Some("timer") => fixture.provide(CapturePreferences {
                timer: Timer::S3,
                ..CapturePreferences::default()
            }),
            _ => {}
        }
    }

    /// Every screenshot has a recent capture to hand off.
    fn scenario(_name: Option<&str>) -> Vec<Invoke<Record>> {
        vec![Record::REGISTER_CAPTURE.with(sample_capture())]
    }

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::CATALOG)
    }

    fn commands() -> &'static [Command] {
        &[Command {
            option: "--emit-photos",
            run: emit_photos,
        }]
    }
}

/// `--emit-photos DIR`: write procedural photos and a lossless QR poster
/// into `DIR`, the library a headless test drives the camera against.
fn emit_photos(dir: &str) -> Result<(), String> {
    use crate::model::PhotoFormat;
    let dir = std::path::Path::new(dir);
    for seed in 0..5u8 {
        let frame = crate::frame::thumbnail(seed, 960, 640);
        crate::library::write_photo(dir, &frame, PhotoFormat::Jpeg)
            .map_err(|error| error.to_string())?;
        let _ = ui::rt::sleep_thread(std::time::Duration::from_millis(4));
    }
    let (qr, _) = crate::frame::scanner_frame(
        4,
        900,
        640,
        "WIFI:T:WPA2;S:Tilo Lounge;P:welcome-aboard;H:false;;",
    );
    crate::library::write_photo(dir, &qr, PhotoFormat::Png).map_err(|error| error.to_string())?;
    println!("camera: wrote sample photos to {}", dir.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::CameraEffect;
    use crate::model::CameraDevicePreferences;
    use ui::framework::LiveRow;
    use ui::prelude::TestApp;

    #[test]
    fn mode_lens_and_preferences_are_effects_and_rows_carry_effects() {
        let mut app = TestApp::<CameraShell>::new().unwrap();
        let surface = serde_json::to_string(&app.agent_surface()).unwrap();
        for id in [
            "use-mode",
            "use-lens",
            "set-grid",
            "set-hdr",
            "set-default-lens",
        ] {
            assert!(surface.contains(id), "{id} is published: {surface}");
        }
        let published =
            ui::tilo_resource::publish::<Record>("camera", "Camera", &Record::default());
        assert_eq!(published.kind, "camera");
        assert!(
            !published
                .children
                .iter()
                .any(|child| child.kind == "camera.settings"),
            "preferences are not document state"
        );

        let rows: Vec<_> = crate::actions::live_rows(&CameraApp::sample().camera_resource_ctx())
            .into_iter()
            .map(LiveRow::into_resource)
            .collect();
        for kind in [
            "camera.capture",
            "camera.scanner",
            "camera.document-scanner",
        ] {
            assert!(rows.iter().any(|row| row.kind == kind), "{kind} is a row");
        }
        assert!(
            rows.iter()
                .flat_map(|row| row.actions.iter())
                .all(|action| action.kind != ui::tilo_actions::OperationKind::Document),
            "rows carry views and effects only"
        );
    }

    /// A preference set through its effect is the person's or this device's
    /// setting, not an edit of the record.
    #[test]
    fn preferences_are_settings_of_the_person_and_the_device() {
        let mut app = TestApp::<CameraShell>::new().unwrap();
        let versions = app.versions().len();
        app.effect(&CameraEffect::SetGrid { on: false }).unwrap();
        app.effect(&CameraEffect::SetResolution {
            resolution: "4k".into(),
        })
        .unwrap();
        app.settle();
        assert!(!app.settings::<CapturePreferences>().get().grid);
        assert_eq!(
            app.settings::<CameraDevicePreferences>().get().resolution,
            crate::model::Resolution::R4K
        );
        assert_eq!(app.versions().len(), versions, "no edit of the record");
    }

    /// Every screenshot is a capture of sample frames at a fixed time, its
    /// recent capture staged through actions and its mode and timer through
    /// fixtures.
    #[test]
    fn captures_stage_the_record_and_the_preferences() {
        let mut viewfinder = TestApp::<CameraShell>::capture(None).unwrap();
        assert_eq!(viewfinder.harness().inspection().viewport, [1280, 800]);
        let recent = viewfinder
            .state()
            .recent
            .expect("a recent capture to hand off");
        assert_eq!(recent.label, "Golden hour");
        let mode = |app: &mut TestApp<CameraShell>| {
            app.with_surface(|camera: &mut CameraApp| camera.capture_mode())
                .unwrap()
        };
        assert_eq!(mode(&mut viewfinder), CaptureMode::Photo);

        let mut video = TestApp::<CameraShell>::capture(Some("video")).unwrap();
        assert_eq!(mode(&mut video), CaptureMode::Video);

        let mut timer = TestApp::<CameraShell>::capture(Some("timer")).unwrap();
        let chosen = timer
            .with_surface(|camera: &mut CameraApp| camera.capture_settings().timer)
            .unwrap();
        assert_eq!(chosen, Timer::S3);
    }
}

ui::reach_tests!(CameraShell);

#[cfg(test)]
mod agent_disclosure_tests {
    #[test]
    fn every_scenario_shows_an_agent_its_actions() {
        if let Err(failures) =
            ui::framework::testing::check_agents_are_shown_every_action::<super::CameraShell>()
        {
            panic!("{failures}");
        }
    }
}
