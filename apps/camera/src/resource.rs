//! Camera as a resource: its record ([`Record`]: the recent capture handoff
//! and the scan history) is the document; the shutter, devices and scanners
//! are live rows the surface publishes.
//!
//! Everything else is the surface's [`CameraEffect`] vocabulary: the screen,
//! mode and lens reveals and the capture preferences are offered on the
//! camera, while shutter, record, camera chooser, code-scanner import and
//! document-scanner effects ride the surface's
//! [`ResourceSurface::live_rows`]. Capture preferences are settings — the
//! person's [`CapturePreferences`] and this device's
//! [`CameraDevicePreferences`] — never document state.

use std::rc::Rc;

use ui::framework::{
    Command, EffectAnswer, EffectCx, EffectStatus, Fixture, LiveRow, ResourceApp, ResourceSurface,
    RunMode, SurfaceCx, ViewCx,
};
use ui::tilo_resource::Invoke;
use ui::widget::Element;

use crate::actions::CameraEffect;
use crate::app::CameraApp;
use crate::model::{
    CameraDevicePreferences, CaptureKind, CaptureMode, CapturePreferences, Facing, Timer,
};
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

/// Framework app: the camera widget is the surface over its [`Record`].
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
            cx.surface(app)
        } else {
            cx.surface(CameraApp::sample())
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

impl ResourceSurface<Record, CameraEffect> for CameraApp {
    /// The scan history and the recent capture, from the live record or a
    /// past version in History.
    fn present(&mut self, state: &Rc<Record>) {
        self.present_record(state);
    }

    /// A live window follows the person's and this device's capture
    /// preferences; a capture shows its scenario's; a past version the
    /// defaults.
    fn attach(&mut self, cx: SurfaceCx<Record>) {
        match cx.mode() {
            RunMode::Capture => {
                if let Some(clock) = cx.fixture::<ShotClock>() {
                    self.set_clock(clock.0);
                }
                if let Some(mode) = cx.fixture::<CaptureMode>() {
                    self.stage_mode(*mode);
                }
                if let Some(person) = cx.fixture::<CapturePreferences>() {
                    self.stage_preferences(*person, CameraDevicePreferences::default());
                }
            }
            RunMode::Live => self.follow_preferences(
                cx.settings::<CapturePreferences>(),
                cx.settings::<CameraDevicePreferences>(),
            ),
            RunMode::Past => {}
        }
        self.attach_dispatcher(cx.dispatcher());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scene(scenario);
    }

    fn effect(&mut self, effect: CameraEffect, cx: &mut EffectCx<Record, Self>) -> EffectAnswer {
        self.answer_effect(effect, cx)
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        crate::actions::live_rows(&self.camera_resource_ctx())
    }

    /// Switching between photo and video stops nothing: while a video
    /// records, the switch is unavailable.
    fn effects(&self) -> Vec<EffectStatus> {
        if !self.is_recording() {
            return Vec::new();
        }
        vec![EffectStatus::unavailable(
            CameraEffect::USE_MODE,
            "recording",
            "Stop recording to switch modes",
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

        let rows: Vec<_> = CameraApp::sample()
            .live_rows()
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

#[cfg(test)]
mod reach_tests {
    use ui::framework::ResourceApp as _;
    use ui::framework::testing::{TestApp, unreachable_actions};

    /// Every action the app publishes is reachable by address in a drive of
    /// each of its scenarios: none is advertised yet handled only by a
    /// click. The scenarios stage fixtures; the app someone opens acts on
    /// this machine (its files, its processes), so it is not walked.
    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action() {
        for scenario in super::CameraShell::scenarios() {
            let mut app = TestApp::<super::CameraShell>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(
                unreachable_actions(&mut app),
                Vec::<String>::new(),
                "{scenario}: published actions a drive cannot invoke"
            );
        }
    }
}
