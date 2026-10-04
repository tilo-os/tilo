//! Camera as a [`tilo_resource::Resource`]: one action vocabulary for capture
//! prefs, scan history and the recent-capture handoff; the shutter, devices
//! and scanners are live rows the surface publishes.
//!
//! Document mutations (mode, lens, settings toggles) live on this resource via
//! [`Resource::bound_actions`] / [`Resource::children`]. Screen reveals are
//! Views ([`Resource::local_operations`]). Shutter, record, device chooser,
//! code-scanner import and document-scanner Effects ride the surface's
//! [`ResourceSurface::live_rows`].

use std::rc::Rc;

use automerge::ROOT;
use ui::framework::{
    Command, Fixture, LiveRow, ResourceApp, ResourceSurface, RunMode, SurfaceCx, ViewCx,
};
use ui::tilo_actions::{ActionSpec, OperationKind, Resource as PublishedResource};
use ui::tilo_resource::{Doc, Invoke, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{
    CAMERA_ACTION_TYPE, CameraAction, bound_mode_facing_actions, capture_item_from_shared,
    publish_document_children, screen_view_specs,
};
use crate::app::CameraApp;
use crate::doc::{self, SharedCapture};
use crate::model::{CaptureKind, CaptureMode, Facing, ScanEntry, Settings, Timer};

/// Stable id for the published camera root.
pub const CAMERA_RESOURCE_ID: &str = "camera";

/// Shared camera document projection.
#[derive(Clone, Debug)]
pub struct CameraState {
    pub mode: CaptureMode,
    pub facing: Facing,
    pub settings: Settings,
    pub scans: Vec<ScanEntry>,
    pub recent: Option<SharedCapture>,
}

impl Default for CameraState {
    fn default() -> Self {
        Self {
            mode: CaptureMode::Photo,
            facing: Facing::Back,
            settings: Settings::default(),
            scans: Vec::new(),
            recent: None,
        }
    }
}

/// The camera resource.
pub struct Camera;

impl Resource for Camera {
    const KIND: &'static str = "camera";
    const LABEL: &'static str = "Camera";
    const ACTION_TYPE: &'static str = CAMERA_ACTION_TYPE;

    type State = CameraState;
    type Action = CameraAction;

    fn read(doc: &Doc) -> Result<CameraState, ModelError> {
        Ok(CameraState {
            mode: doc::read_mode(doc, &ROOT),
            facing: doc::read_facing(doc, &ROOT),
            settings: doc::read_settings(doc, &ROOT),
            scans: doc::read_scans(doc, &ROOT),
            recent: doc::read_recent_capture(doc, &ROOT),
        })
    }

    fn bound_actions(state: &CameraState) -> Vec<ActionSpec> {
        // Recording is live state the resource cannot see: mode stays
        // offered here and the surface refuses a change while recording.
        bound_mode_facing_actions(state.facing, false)
            .into_iter()
            .filter(|spec| spec.kind == OperationKind::Document)
            .collect()
    }

    fn children(state: &CameraState) -> Vec<PublishedResource> {
        let recent = state.recent.as_ref().map(capture_item_from_shared);
        publish_document_children(&state.settings, recent.as_ref(), &state.scans)
    }

    fn local_operations() -> Vec<ActionSpec> {
        screen_view_specs()
            .into_iter()
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }

    fn search(state: &CameraState) -> Vec<SearchEntry> {
        let mut entries = Vec::new();
        if let Some(recent) = &state.recent {
            entries.push(SearchEntry {
                anchor: Some(format!("camera.capture:{}", recent.id)),
                title: recent.label.clone(),
                text: "recent capture".into(),
                action: None,
            });
        }
        for scan in &state.scans {
            entries.push(SearchEntry {
                anchor: Some(format!("camera.scan:{}", scan.ts)),
                title: scan.raw.clone(),
                text: "scan".into(),
                action: None,
            });
        }
        entries
    }
}

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
pub(crate) fn sample_capture() -> CameraAction {
    CameraAction::RegisterCapture {
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

/// Framework app: the camera widget is the surface over [`Camera`].
pub struct CameraShell;

impl ResourceApp for CameraShell {
    type Resource = Camera;
    const ID: &'static str = "so.tilo.camera";
    const TITLE: &'static str = "Camera";
    const ICON: Option<&'static str> = Some("camera");

    /// A live window drives the cameras and scanners of this device and
    /// writes captures to its picture folder; a screenshot or a past version
    /// shows sample frames and reaches nothing.
    fn view(cx: &mut ViewCx<Camera>) -> Element {
        if cx.mode().is_live() {
            cx.surface(CameraApp::disk())
        } else {
            cx.surface(CameraApp::sample())
        }
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1280, 800)
    }

    fn fixture(fixture: &mut Fixture<Camera>) {
        fixture.provide(ShotClock(SHOT_NOW));
    }

    /// Every screenshot has a recent capture to hand off; the video and
    /// timer scenarios set the mode and the timer as a person would.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Camera>> {
        let mut actions = vec![sample_capture()];
        match name {
            Some("video") => actions.push(CameraAction::SetMode {
                mode: CaptureMode::Video,
            }),
            Some("timer") => actions.push(CameraAction::SetTimer { timer: Timer::S3 }),
            _ => {}
        }
        actions
            .iter()
            .filter_map(|action| Invoke::apply(action).ok())
            .collect()
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

impl ResourceSurface<Camera> for CameraApp {
    /// The surface reads the record in `sync`, from the document it is
    /// handed: the live one, or a past version in History.
    fn present(&mut self, _state: &Rc<CameraState>) {
        self.mark_dirty();
    }

    fn attach(&mut self, cx: SurfaceCx<Camera>) {
        if cx.mode() == RunMode::Capture
            && let Some(clock) = cx.fixture::<ShotClock>()
        {
            self.set_clock(clock.0);
        }
        self.attach_dispatcher(cx.dispatcher());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scene(scenario);
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        crate::actions::live_effect_extras(&self.camera_resource_ctx())
            .into_iter()
            .map(live_row)
            .collect()
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.live_rows().into_iter().find(|row| row.id() == id)
    }
}

/// A published effect root as a live row.
fn live_row(resource: PublishedResource) -> LiveRow {
    let row = resource.actions.into_iter().fold(
        LiveRow::new(resource.id.0, resource.kind, resource.label),
        LiveRow::operation,
    );
    resource
        .children
        .into_iter()
        .map(live_row)
        .fold(row, LiveRow::row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::prelude::TestApp;

    #[test]
    fn the_published_camera_owns_the_document_door_and_rows_carry_effects() {
        let state = CameraState {
            mode: CaptureMode::Video,
            ..CameraState::default()
        };
        let published = ui::tilo_resource::publish::<Camera>(CAMERA_RESOURCE_ID, "Camera", &state);
        assert_eq!(published.kind, "camera");
        for id in ["photo-mode", "video-mode", "lens-back", "lens-front"] {
            assert!(
                published.actions.iter().any(|action| action.id == id),
                "{id} must be on the framework resource"
            );
        }
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id.starts_with("open-")),
            "screen reveal Views stay on the published door"
        );
        let settings = published
            .children
            .iter()
            .find(|child| child.kind == "camera.settings")
            .expect("settings child on the published door");
        for id in ["grid", "hdr"] {
            assert!(
                settings.actions.iter().any(|action| action.id == id),
                "{id} must be on settings"
            );
        }

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
                .all(|action| action.kind != OperationKind::Document),
            "rows carry views and effects only"
        );
    }

    /// Every screenshot is a capture of sample frames at a fixed time, its
    /// recent capture and modes staged through actions.
    #[test]
    fn captures_stage_the_record_through_actions() {
        let mut viewfinder = TestApp::<CameraShell>::capture(None).unwrap();
        assert_eq!(viewfinder.harness().inspection().viewport, [1280, 800]);
        let recent = viewfinder
            .state()
            .recent
            .expect("a recent capture to hand off");
        assert_eq!(recent.label, "Golden hour");
        assert_eq!(viewfinder.state().mode, CaptureMode::Photo);

        let video = TestApp::<CameraShell>::capture(Some("video")).unwrap();
        assert_eq!(video.state().mode, CaptureMode::Video);

        let timer = TestApp::<CameraShell>::capture(Some("timer")).unwrap();
        assert_eq!(timer.state().settings.timer, Timer::S3);
    }
}
