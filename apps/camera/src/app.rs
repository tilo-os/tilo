//! Root widget for the Camera app.
//!
//! A canvas-heavy app: rather than composing dozens of taffy nodes, the
//! root occupies a single layout node and paints every screen (Camera,
//! Scanner, Gallery, Settings) straight into the Vello scene, hit-testing
//! its own controls.
//!
//! The photo/video library is real files on disk (see [`crate::library`]):
//! the gallery reflects whatever is actually there, the shutter writes a
//! PNG, delete removes the file. The Automerge document (see [`crate::doc`])
//! holds only the collaborative metadata — favorites, scan history, and
//! capture preferences. A live viewfinder needs a camera; when none is
//! present the camera/scanner screens say so honestly rather than faking a
//! feed. The hermetic `--shots` / test path uses a self-contained sample
//! source so screenshots and tests never touch the disk or a device.

#[cfg(test)]
use qos_actions::EncodedAction;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use automerge::{AutoCommit, ChangeHash, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect};
use vello::peniko::Color;

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Image, ImageFit, Toggle, ToggleFace, Widget};
use winit::event::WindowEvent;

use crate::doc;
use crate::frame::{self, FrameBuf};
use crate::library;
use crate::model::{
    Capture, CaptureKind, CaptureMode, CodeKind, Facing, GalleryFilter, PhotoFormat, Resolution,
    ScanEntry, ScannedCode, Screen, Settings, Timer, classify,
};
use crate::paint as p;

/// Uniform inset for floating controls from the window edge. The camera is an
/// immersive, full-bleed app: the viewfinder fills the window edge-to-edge and
/// every control floats over it at this margin — there is no navigation rail
/// or sidebar (the OS owns window chrome). Gallery and Settings are reached
/// from the viewfinder as a push and an overlay sheet, not as rail items.
const EDGE: f64 = 18.0;
/// Payload composited into the sample scanner poster (hermetic path only).
const SCAN_PAYLOAD: &str = "WIFI:T:WPA2;S:qOS Lounge;P:welcome-aboard;H:false;;";
/// Single decoded-size budget for gallery thumbnails/previews (one decode
/// per item, reused across the card, detail preview, and camera library
/// button).
const THUMB: (u32, u32) = (512, 342);
/// Below this content width the side panels fold away for a single-column
/// layout.
const WIDE_MIN: f64 = 900.0;

fn white() -> Color {
    Color::from_rgb8(255, 255, 255)
}
fn shrink(r: Rect, d: f64) -> Rect {
    Rect::new(r.x0 + d, r.y0 + d, r.x1 - d, r.y1 - d)
}

/// Where the library's pixels live.
enum Storage {
    /// Live: real files under this directory.
    Disk(PathBuf),
    /// Hermetic (screenshots/tests): in-memory procedural samples.
    Sample,
}

/// An in-progress video recording: the encoder accumulating frames plus the
/// epoch second it started (for the on-screen elapsed timer).
struct RecState {
    rec: crate::video::Recorder,
    started: i64,
}

#[derive(Clone, Debug)]
enum Action {
    Nav(Screen),
    /// Dismiss an overlay screen (Gallery / Settings) back to the viewfinder
    /// it was opened from.
    Back,
    Shutter,
    SetMode(CaptureMode),
    CycleFacing,
    SetFilter(GalleryFilter),
    SelectCapture(String),
    CloseDetail,
    ToggleFavSelected,
    DeleteSelected,
    DismissScan,
    ScanAction(usize),
    /// Decode a QR/barcode out of a library photo (used when no camera feed
    /// is available, so the scanner stays useful offline).
    ScanLibraryItem(String),
    ShareSelected,
    CycleResolution,
    CycleFormat,
    SetTimer(Timer),
    ToggleGrid,
    ToggleLevel,
    ToggleMirror,
    ToggleGeotag,
    ToggleHdr,
    CycleDefaultFacing,
}

pub struct CameraApp {
    rect: Rect,
    node: Option<NodeId>,

    storage: Storage,
    initial_screen: Screen,

    // CRDT-backed caches (refreshed every sync)
    screen: Screen,
    mode: CaptureMode,
    facing: Facing,
    settings: Settings,
    favorites: HashSet<String>,
    scans: Vec<ScanEntry>,

    // Library (from disk scan or the sample set)
    captures: Vec<Capture>,
    lib_dirty: bool,

    // ephemeral UI state
    /// The viewfinder screen an overlay (Gallery / Settings) was opened from,
    /// so its back button returns there rather than always to Camera.
    return_to: Screen,
    filter: GalleryFilter,
    selected_capture: Option<String>,
    scan_dismissed: bool,
    scanned: Option<ScannedCode>,
    scan_gen: (u32, u32),
    toast: Option<String>,
    last_mouse: Point,
    gallery_scroll: f64,
    settings_scroll: f64,

    // In-progress video recording (None unless the shutter is armed in video
    // mode). The encoder is fed real frames as they arrive.
    recording: Option<RecState>,

    // Live camera frame (populated by the live-capture worker / tests).
    latest_frame: Option<FrameBuf>,
    frame_gen: u64,
    #[cfg(feature = "live-capture")]
    camera: Option<crate::capture::Capture>,
    /// The `(device, width, height)` the live pipeline is currently open on,
    /// so device/resolution changes trigger a re-open.
    #[cfg(feature = "live-capture")]
    cam_config: Option<(String, u32, u32)>,

    // render caches
    cam_view: Option<(u32, u32, u8, Image)>,
    live_view: Option<(u64, Image)>,
    scan_view: Option<(u32, u32, Image)>,
    thumbs: HashMap<String, Image>,

    regions: Vec<(Rect, Action)>,
    dirty: bool,
    initialized: bool,

    /// Framework-owned action dispatch: brackets the undo boundary, encodes and
    /// dispatches each action, and remembers the last one so a test can prove a
    /// button and its palette `ActionSpec` build the same `EncodedAction`.
    actions: ui::ActionHost,
}

impl Default for CameraApp {
    fn default() -> Self {
        Self::disk()
    }
}

impl CameraApp {
    /// The live app: library backed by real files on disk. Opens on the
    /// viewfinder — the camera's home — with the gallery a tap away.
    pub fn disk() -> Self {
        Self::with(Storage::Disk(library::resolve_dir()), Screen::Camera)
    }

    /// The hermetic app used by `--shots` and tests: self-contained sample
    /// content, no disk or device access.
    pub fn sample() -> Self {
        Self::with(Storage::Sample, Screen::Camera)
    }

    fn with(storage: Storage, initial_screen: Screen) -> Self {
        Self {
            rect: Rect::ZERO,
            node: None,
            storage,
            initial_screen,
            screen: initial_screen,
            mode: CaptureMode::Photo,
            facing: Facing::Back,
            settings: Settings::default(),
            favorites: HashSet::new(),
            scans: Vec::new(),
            captures: Vec::new(),
            lib_dirty: true,
            return_to: Screen::Camera,
            filter: GalleryFilter::All,
            selected_capture: None,
            scan_dismissed: false,
            scanned: None,
            scan_gen: (0, 0),
            toast: None,
            last_mouse: Point::ZERO,
            gallery_scroll: 0.0,
            settings_scroll: 0.0,
            recording: None,
            latest_frame: None,
            frame_gen: 0,
            #[cfg(feature = "live-capture")]
            camera: None,
            #[cfg(feature = "live-capture")]
            cam_config: None,
            cam_view: None,
            live_view: None,
            scan_view: None,
            thumbs: HashMap::new(),
            regions: Vec::new(),
            dirty: true,
            initialized: false,
            actions: ui::ActionHost::new(),
        }
    }

    /// Hand a freshly captured RGBA frame to the app (live-capture worker or
    /// tests). This is what the viewfinder shows, what the shutter saves, and
    /// — while recording — what the video encoder is fed.
    pub fn push_camera_frame(&mut self, frame: FrameBuf) {
        if let Some(state) = &mut self.recording {
            state.rec.push(&frame);
        }
        self.latest_frame = Some(frame);
        self.frame_gen = self.frame_gen.wrapping_add(1);
        self.dirty = true;
    }

    /// True while a video recording is in progress.
    fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    /// Begin a recording. Honest about missing hardware: with no live feed the
    /// disk/live source can't record, so it says so instead of faking a clip.
    fn start_recording(&mut self) {
        if !self.feed_active() {
            self.toast = Some("Connect a camera to record".into());
            return;
        }
        let mut rec = crate::video::Recorder::new(crate::video::FPS);
        // Seed with the frame already on screen so a clip is never empty.
        if let Some(fb) = &self.latest_frame {
            rec.push(fb);
        }
        self.recording = Some(RecState {
            rec,
            started: now_secs(),
        });
        self.dirty = true;
    }

    /// Finish the recording: mux the frames into a real MJPEG/AVI clip and
    /// save it (disk) or append an in-memory sample entry (hermetic).
    fn stop_recording(&mut self, _doc: &mut AutoCommit, _root: &ObjId) {
        let Some(state) = self.recording.take() else {
            return;
        };
        let dur = state.rec.duration_ms();
        let Some(bytes) = state.rec.finish() else {
            self.toast = Some("Nothing recorded".into());
            return;
        };
        match &self.storage {
            Storage::Disk(dir) => {
                match crate::video::write_clip(dir, &bytes, &library::timestamp_name()) {
                    Ok(name) => {
                        self.lib_dirty = true;
                        self.selected_capture = Some(name);
                        self.toast = Some(format!("Video saved · {}", fmt_dur(dur)));
                    }
                    Err(e) => self.toast = Some(format!("Save failed: {e}")),
                }
            }
            Storage::Sample => {
                // Hermetic path (no disk): register the real clip's length as a
                // gallery entry so the recording round-trips visibly.
                let id = format!("VID_{}.avi", now_secs());
                let cap = Capture {
                    id: id.clone(),
                    kind: CaptureKind::Video,
                    ts: now_secs(),
                    w: self.latest_frame.as_ref().map_or(0, |f| f.w),
                    h: self.latest_frame.as_ref().map_or(0, |f| f.h),
                    dur_ms: dur,
                    label: "New clip".into(),
                    favorite: false,
                    facing: self.facing,
                    scene: Self::viewfinder_seed(self.facing),
                    path: None,
                };
                self.captures.insert(0, cap);
                self.selected_capture = Some(id);
                self.toast = Some(format!("Video saved · {}", fmt_dur(dur)));
            }
        }
    }

    /// Whether a live viewfinder feed is currently available.
    fn feed_active(&self) -> bool {
        match self.storage {
            Storage::Sample => true,
            Storage::Disk(_) => self.latest_frame.is_some(),
        }
    }

    fn viewfinder_seed(facing: Facing) -> u8 {
        match facing {
            Facing::Back => 0,
            Facing::Front => 3,
            Facing::Wide => 2,
        }
    }

    fn push_region(&mut self, rect: Rect, action: Action) {
        self.regions.push((rect, action));
    }

    fn reload_library(&mut self) {
        self.captures = match &self.storage {
            Storage::Disk(dir) => library::scan(dir, &self.favorites),
            Storage::Sample => sample_captures(),
        };
        self.lib_dirty = false;
    }

    /// Dispatch one Camera [`CameraAction`](crate::actions::CameraAction) — the
    /// single mutation path shared with the Ctrl+K palette. The action is applied
    /// through the qOS action executor (a pinned actor + committed change, so it
    /// replays byte-identically on peers), exactly as the palette does. The doc
    /// heads captured just before the first action of the event are stashed so the
    /// host records **one** undo boundary bracketing it
    /// ([`ui::widget::Widget::take_committed_action`]).
    fn perform_action(&mut self, doc: &mut AutoCommit, action: crate::actions::CameraAction) {
        self.actions
            .dispatch(doc, crate::actions::CAMERA_ACTION_TYPE, &action);
    }

    /// The context Ctrl+K needs to build Camera's resource tree: the gallery
    /// shots currently in the library cache, with their favorite state. Derived
    /// entirely from state the app already holds; nothing is fabricated.
    pub(crate) fn camera_resource_ctx(&self) -> crate::actions::CameraResourceCtx {
        let items = self
            .captures
            .iter()
            .map(|c| crate::actions::GalleryItem {
                id: c.id.clone(),
                label: c.label.clone(),
                favorite: c.favorite,
                is_video: c.is_video(),
            })
            .collect();
        crate::actions::CameraResourceCtx { items }
    }

    fn apply(&mut self, action: Action, doc: &mut AutoCommit, root: &ObjId) {
        self.dirty = true;
        // Any interaction clears a transient confirmation; the action below
        // sets a fresh one if it has something to report.
        self.toast = None;
        match action {
            Action::Nav(s) => {
                // Remember which viewfinder an overlay was summoned from so its
                // back button returns there.
                if matches!(s, Screen::Gallery | Screen::Settings)
                    && matches!(self.screen, Screen::Camera | Screen::Scanner)
                {
                    self.return_to = self.screen;
                }
                self.screen = s;
                doc::write_screen(doc, root, s);
            }
            Action::Back => {
                self.screen = self.return_to;
                doc::write_screen(doc, root, self.return_to);
            }
            Action::Shutter => match self.mode {
                CaptureMode::Photo => self.capture(doc, root),
                CaptureMode::Video => {
                    if self.is_recording() {
                        self.stop_recording(doc, root);
                    } else {
                        self.start_recording();
                    }
                }
            },
            Action::SetMode(m) => {
                // Switching mode mid-record would strand the clip; ignore.
                if !self.is_recording() {
                    self.mode = m;
                    doc::write_mode(doc, root, m);
                    // Photo/Video also bring the viewfinder forward when chosen
                    // from the scanner's shared switcher.
                    if self.screen != Screen::Camera {
                        self.screen = Screen::Camera;
                        doc::write_screen(doc, root, Screen::Camera);
                    }
                }
            }
            Action::CycleFacing => {
                self.facing = self.facing.next();
                doc::write_facing(doc, root, self.facing);
                self.cam_view = None;
            }
            Action::SetFilter(f) => {
                self.filter = f;
                self.gallery_scroll = 0.0;
            }
            Action::SelectCapture(id) => self.selected_capture = Some(id),
            Action::CloseDetail => self.selected_capture = None,
            Action::ToggleFavSelected => {
                if let Some(id) = self.selected_capture.clone() {
                    // Flip the cached state for immediate feedback, then route
                    // the doc write through the shared action so the heart, a
                    // delete's cleanup, and Ctrl+K all dispatch one thing.
                    let mut toggled = None;
                    if let Some(c) = self.captures.iter_mut().find(|c| c.id == id) {
                        c.favorite = !c.favorite;
                        toggled = Some(c.favorite);
                    }
                    if let Some(on) = toggled {
                        if on {
                            self.favorites.insert(id.clone());
                        } else {
                            self.favorites.remove(&id);
                        }
                        self.perform_action(
                            doc,
                            crate::actions::CameraAction::SetFavorite { id: id.clone(), on },
                        );
                    }
                }
            }
            Action::DeleteSelected => {
                if let Some(id) = self.selected_capture.clone() {
                    if let Storage::Disk(dir) = &self.storage {
                        let _ = library::delete(dir, &id);
                    }
                    // The pixels are device I/O (removed above / reconciled via
                    // FUSE); the shot's only collaborative trace is its favorite
                    // flag, cleared through the same action the heart uses — but
                    // only when it was actually starred, so an unstarred delete
                    // touches the document not at all.
                    if self.favorites.contains(&id) {
                        self.perform_action(
                            doc,
                            crate::actions::CameraAction::SetFavorite {
                                id: id.clone(),
                                on: false,
                            },
                        );
                    }
                    self.captures.retain(|c| c.id != id);
                    self.thumbs.remove(&id);
                    self.favorites.remove(&id);
                    self.selected_capture = None;
                    self.toast = Some("Deleted".into());
                }
            }
            Action::DismissScan => self.scan_dismissed = true,
            Action::ShareSelected => {
                if let Some(cap) = self
                    .selected_capture
                    .clone()
                    .and_then(|id| self.captures.iter().find(|c| c.id == id).cloned())
                {
                    match &cap.path {
                        Some(p) => {
                            ui::clipboard_copy(&p.to_string_lossy());
                            self.toast = Some("Path copied to clipboard".into());
                        }
                        None => self.toast = Some("Saved in your library".into()),
                    }
                }
            }
            Action::ScanAction(i) => {
                if let Some(s) = self.scanned.clone() {
                    // The chosen action (open / connect / copy) is external; the
                    // scan-history row-insert is the collaborative action.
                    self.perform_scan_action(&s, i);
                    self.perform_action(
                        doc,
                        crate::actions::CameraAction::RecordScan {
                            raw: s.raw.clone(),
                            ts: now_secs(),
                        },
                    );
                    self.scans = doc::read_scans(doc, root);
                }
            }
            Action::ScanLibraryItem(id) => self.scan_library_item(&id, doc, root),
            Action::CycleResolution => {
                self.settings.resolution = cycle(&Resolution::ALL, self.settings.resolution);
                doc::write_settings(doc, root, &self.settings);
            }
            Action::CycleFormat => {
                self.settings.photo_format = cycle(&PhotoFormat::ALL, self.settings.photo_format);
                doc::write_settings(doc, root, &self.settings);
            }
            Action::SetTimer(tm) => {
                self.settings.timer = tm;
                doc::write_settings(doc, root, &self.settings);
            }
            Action::ToggleGrid => {
                self.settings.grid = !self.settings.grid;
                doc::write_settings(doc, root, &self.settings);
            }
            Action::ToggleLevel => {
                self.settings.level = !self.settings.level;
                doc::write_settings(doc, root, &self.settings);
            }
            Action::ToggleMirror => {
                self.settings.mirror_front = !self.settings.mirror_front;
                doc::write_settings(doc, root, &self.settings);
            }
            Action::ToggleGeotag => {
                self.settings.geotag = !self.settings.geotag;
                doc::write_settings(doc, root, &self.settings);
            }
            Action::ToggleHdr => {
                self.settings.hdr = !self.settings.hdr;
                doc::write_settings(doc, root, &self.settings);
            }
            Action::CycleDefaultFacing => {
                self.settings.default_facing = self.settings.default_facing.next();
                doc::write_settings(doc, root, &self.settings);
            }
        }
    }

    /// Take a photo: write the current frame to disk (live) or append an
    /// in-memory sample (hermetic).
    fn capture(&mut self, _doc: &mut AutoCommit, _root: &ObjId) {
        match &self.storage {
            Storage::Sample => {
                let id = format!(
                    "IMG_{}.{}",
                    now_secs(),
                    self.settings.photo_format.extension()
                );
                let cap = Capture {
                    id: id.clone(),
                    kind: CaptureKind::Photo,
                    ts: now_secs(),
                    w: self.settings.resolution.dims().0,
                    h: self.settings.resolution.dims().1,
                    dur_ms: 0,
                    label: "New capture".into(),
                    favorite: false,
                    facing: self.facing,
                    scene: Self::viewfinder_seed(self.facing),
                    path: None,
                };
                self.captures.insert(0, cap);
                self.selected_capture = Some(id);
                self.toast = Some("Photo saved".into());
            }
            Storage::Disk(dir) => {
                let Some(frame) = self.latest_frame.clone() else {
                    self.toast = Some("Connect a camera to capture".into());
                    return;
                };
                match library::write_photo(dir, &frame, self.settings.photo_format) {
                    Ok(name) => {
                        self.lib_dirty = true;
                        self.selected_capture = Some(name);
                        self.toast = Some("Photo saved".into());
                    }
                    Err(e) => self.toast = Some(format!("Save failed: {e}")),
                }
            }
        }
    }

    /// Decode a code out of a library photo by id. Real work: the photo is
    /// decoded to luma and run through the QR detector. Used by the scanner
    /// when there is no live camera feed, so a saved QR (a screenshot, a
    /// shared poster) can still be acted on.
    fn scan_library_item(&mut self, id: &str, doc: &mut AutoCommit, root: &ObjId) {
        self.scan_dismissed = false;
        let path = self
            .captures
            .iter()
            .find(|c| c.id == id)
            .and_then(|c| c.path.clone());
        let Some(path) = path else {
            self.toast = Some("No code found in that photo".into());
            return;
        };
        let Some((luma, w, h)) = library::load_luma(&path, 1024) else {
            self.toast = Some("Couldn't read that photo".into());
            return;
        };
        if let Some(s) = crate::scan::decode_and_classify(&luma, w, h, |p| p) {
            self.scanned = Some(s.clone());
            // Decoding is device I/O; recording it into the shared history
            // is the collaborative action.
            self.perform_action(
                doc,
                crate::actions::CameraAction::RecordScan {
                    raw: s.raw.clone(),
                    ts: now_secs(),
                },
            );
            self.scans = doc::read_scans(doc, root);
            self.toast = Some(format!("{} found", s.kind.title()));
        } else {
            self.scanned = None;
            self.toast = Some("No code found in that photo".into());
        }
    }

    /// Carry out the chosen action for a scanned code. Openable schemes are
    /// handed to the OS default handler; everything is also copied to the
    /// clipboard so the action does something real on any host.
    fn perform_scan_action(&mut self, s: &ScannedCode, i: usize) {
        let actions = s.kind.actions();
        let label = actions.get(i).copied().unwrap_or("Copy");
        match label {
            "Open link" | "Open map" | "Compose" | "Call" => {
                open_external(&s.raw);
                ui::clipboard_copy(&s.raw);
                self.toast = Some(format!("{label} · copied to clipboard"));
            }
            "Connect" | "Copy password" => {
                if let CodeKind::Wifi { password, .. } = &s.kind {
                    ui::clipboard_copy(password);
                    self.toast = Some("Wi-Fi password copied".into());
                } else {
                    ui::clipboard_copy(&s.raw);
                    self.toast = Some("Copied to clipboard".into());
                }
            }
            _ => {
                ui::clipboard_copy(&s.raw);
                self.toast = Some("Copied to clipboard".into());
            }
        }
    }

    fn hit(&mut self, pos: Point, doc: &mut AutoCommit, root: &ObjId) -> bool {
        let found = self
            .regions
            .iter()
            .rev()
            .find(|(r, _)| r.contains(pos))
            .map(|(_, a)| a.clone());
        if let Some(action) = found {
            self.apply(action, doc, root);
            true
        } else {
            false
        }
    }

    /// Captures passing the active gallery filter.
    fn filtered(&self) -> Vec<Capture> {
        self.captures
            .iter()
            .filter(|c| self.filter.matches(c))
            .cloned()
            .collect()
    }

    #[cfg(feature = "live-capture")]
    fn poll_camera(&mut self) {
        // Only the live/disk storage drives a real capture device. The sample
        // storage backs `--shots`, the test suite and any headless render; it
        // synthesizes its viewfinder frame, so it must never enumerate or open
        // /dev/video*. The `live-capture` feature compiles the pipeline in; this
        // runtime check is what decides whether it actually runs — keeping the
        // hermetic path device-free even in a default (feature-on) build.
        if !matches!(self.storage, Storage::Disk(_)) {
            return;
        }

        // Pick a device for the selected lens: enumerate /dev/video* and index
        // by facing (rear/front/wide → 0/1/2), clamped to what's present.
        let devices = crate::capture::list_devices();
        let idx = match self.facing {
            Facing::Back => 0,
            Facing::Front => 1,
            Facing::Wide => 2,
        };
        let device = devices.get(idx).or_else(|| devices.first()).cloned();
        let (rw, rh) = capture_request_dims(self.settings.resolution);
        let want = device.map(|d| (d, rw, rh));

        // (Re)open when the desired device or resolution changes. Recording
        // holds the current pipeline so a clip isn't cut by a settings poke.
        if want != self.cam_config && !self.is_recording() {
            self.camera = None;
            if let Some((d, w, h)) = &want {
                // Try the requested size, then fall back to a broadly-supported
                // mode so the feed stays alive on modest webcams.
                self.camera = crate::capture::Capture::open(d, *w, *h)
                    .or_else(|_| crate::capture::Capture::open(d, 1280, 720))
                    .ok();
            }
            self.cam_config = want;
        }

        if let Some(cam) = &self.camera {
            let mut latest = None;
            while let Ok(f) = cam.frames.try_recv() {
                latest = Some(f);
            }
            if let Some(f) = latest {
                self.push_camera_frame(FrameBuf {
                    rgba: f.rgba,
                    w: f.width,
                    h: f.height,
                });
            }
        }
    }
}

fn cycle<T: Copy + PartialEq>(all: &[T], cur: T) -> T {
    let i = all.iter().position(|x| *x == cur).unwrap_or(0);
    all[(i + 1) % all.len()]
}

/// The frame size to request from a webcam for a chosen resolution preset.
/// Sensor-still presets (12 MP) exceed what a UVC video stream delivers, so
/// they're capped at a 4K video request; the pipeline negotiates the nearest
/// supported mode and the actual frame size is taken from the buffer.
#[cfg(feature = "live-capture")]
fn capture_request_dims(res: Resolution) -> (u32, u32) {
    match res {
        Resolution::R720p => (1280, 720),
        Resolution::R1080p => (1920, 1080),
        Resolution::R4K | Resolution::R12MP => (3840, 2160),
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn ago(ts: i64, now: i64) -> String {
    let d = (now - ts).max(0);
    if d < 60 {
        "just now".into()
    } else if d < 3600 {
        format!("{}m ago", d / 60)
    } else if d < 86_400 {
        format!("{}h ago", d / 3600)
    } else {
        format!("{}d ago", d / 86_400)
    }
}

fn fmt_dur(ms: i64) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Best-effort hand-off to the system default handler for a URI. Never
/// blocks and ignores failures — the clipboard copy is the guaranteed path.
fn open_external(uri: &str) {
    let _ = std::process::Command::new("xdg-open")
        .arg(uri)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn frame_image_fill(buf: FrameBuf) -> Image {
    let (w, h) = (buf.w, buf.h);
    Image::from_rgba(Arc::from(buf.rgba), w, h).with_fit(ImageFit::Fill)
}

fn frame_image_cover(buf: FrameBuf) -> Image {
    let (w, h) = (buf.w, buf.h);
    Image::from_rgba(Arc::from(buf.rgba), w, h).with_fit(ImageFit::Cover)
}

/// Self-contained sample library for the hermetic screenshot/test path.
/// Uses procedural scenes (`path = None`) so nothing is read from disk.
fn sample_captures() -> Vec<Capture> {
    let now = now_secs();
    let mk = |id: &str,
              kind: CaptureKind,
              off: i64,
              scene: u8,
              fav: bool,
              facing: Facing,
              label: &str,
              dur: i64| Capture {
        id: id.into(),
        kind,
        ts: now - off,
        w: 4032,
        h: 3024,
        dur_ms: dur,
        label: label.into(),
        favorite: fav,
        facing,
        scene,
        path: None,
    };
    use CaptureKind::{Photo, Video};
    use Facing::{Back, Front, Wide};
    vec![
        mk("c1", Photo, 120, 1, true, Back, "Golden hour", 0),
        mk("c2", Photo, 1100, 2, false, Back, "City lights", 0),
        mk("c3", Video, 4200, 0, false, Back, "Coast drive", 7400),
        mk("c4", Photo, 9000, 3, true, Front, "Portrait", 0),
        mk("c5", Photo, 26000, 4, false, Wide, "Forest trail", 0),
        mk("c6", Photo, 90000, 5, false, Back, "Desert dunes", 0),
        mk("c7", Video, 180000, 2, false, Back, "Night market", 12300),
        mk("c8", Photo, 360000, 0, true, Back, "Mountain ridge", 0),
        mk("c9", Photo, 520000, 4, false, Wide, "Meadow", 0),
    ]
}

impl Widget for CameraApp {
    /// Publish the open gallery + its shots so the host's Ctrl+K action palette
    /// scopes to them. Delegates to the
    /// [`ResourceProvider`](qos_actions::ResourceProvider) impl in
    /// [`crate::actions`].
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        <Self as qos_actions::ResourceProvider>::resources(self, doc)
    }

    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its metadata action, so it records one undo boundary for it (see
    /// [`Self::perform_action`]). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<ChangeHash>> {
        self.actions.take_committed()
    }

    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if !self.initialized {
            doc::init_defaults(state, path, self.initial_screen);
            self.initialized = true;
        }
        self.screen = doc::read_screen(state, path);
        // Dev/screenshot hook: force a screen without driving the nav.
        if let Ok(s) = std::env::var("CAMERA_SCREEN")
            && let Some(sc) = Screen::from_str(&s)
        {
            self.screen = sc;
        }
        // Don't let a persisted mode flip mid-record.
        if self.recording.is_none() {
            self.mode = doc::read_mode(state, path);
        }
        self.facing = doc::read_facing(state, path);
        self.settings = doc::read_settings(state, path);
        self.favorites = doc::read_favorites(state, path);
        self.scans = doc::read_scans(state, path);

        #[cfg(feature = "live-capture")]
        self.poll_camera();

        // Hermetic path has no frame clock: while recording, synthesize a
        // procedural frame each tick so a real multi-frame clip accumulates.
        if self.recording.is_some() && matches!(self.storage, Storage::Sample) {
            let n = self.recording.as_ref().map_or(0, |s| s.rec.frame_count());
            let seed = Self::viewfinder_seed(self.facing).wrapping_add((n as u8) & 3);
            let fb = frame::viewfinder(seed, 480, 270);
            if let Some(state) = &mut self.recording {
                state.rec.push(&fb);
            }
            self.dirty = true;
        }

        if self.lib_dirty {
            self.reload_library();
        }
        // Reflect favorite changes onto disk-backed captures without a full
        // rescan.
        if matches!(self.storage, Storage::Disk(_)) {
            for c in &mut self.captures {
                c.favorite = self.favorites.contains(&c.id);
            }
        }
        // Drop a stale selection.
        if let Some(id) = self.selected_capture.clone()
            && !self.captures.iter().any(|c| c.id == id)
        {
            self.selected_capture = None;
        }
        // Screenshot hook: preselect an item so the detail view can be
        // captured headless. No effect on normal runtime (env is unset).
        if self.selected_capture.is_none() && std::env::var_os("CAMERA_SELECT").is_some() {
            self.selected_capture = self.captures.first().map(|c| c.id.clone());
        }
        // Screenshot hook: decode the first library photo that holds a code so
        // the offline scanner's result can be captured headless. No runtime
        // effect (env is unset outside the harness).
        if self.scanned.is_none() && std::env::var_os("CAMERA_SCAN_FIRST").is_some() {
            let paths: Vec<PathBuf> = self
                .captures
                .iter()
                .filter(|c| !c.is_video())
                .filter_map(|c| c.path.clone())
                .collect();
            for p in paths {
                if let Some((luma, w, h)) = library::load_luma(&p, 1024)
                    && let Some(s) = crate::scan::decode_and_classify(&luma, w, h, |q| q)
                {
                    self.scanned = Some(s);
                    self.scan_dismissed = false;
                    break;
                }
            }
        }
        // Screenshot hook: pin a scroll position (clamped at draw time) so a
        // scrolled state can be captured headless. No runtime effect.
        if let Ok(v) = std::env::var("CAMERA_SCROLL")
            && let Ok(px) = v.parse::<f64>()
        {
            self.gallery_scroll = px;
            self.settings_scroll = px;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.regions.clear();
        p::fill_rect(scene, t, self.rect, theme.tokens.colors.surfaces.base);
        // An immersive camera: every screen is full-bleed. The viewfinder
        // (Camera/Scanner) and the gallery fill the window edge-to-edge with
        // controls floating over them; Settings is an overlay sheet. There is
        // no navigation rail — secondary screens are reached from the
        // viewfinder and dismissed back to it.
        let full = self.rect;
        match self.screen {
            Screen::Camera => self.draw_camera(scene, t, tr, theme, full),
            Screen::Scanner => self.draw_scanner(scene, t, tr, theme, full),
            Screen::Gallery => self.draw_gallery(scene, t, tr, theme, full),
            Screen::Settings => self.draw_settings(scene, t, tr, theme, full),
        }
        self.draw_toast(scene, t, tr, theme);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta};
        use winit::keyboard::{Key, NamedKey};
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.hit(self.last_mouse, state, path) {
                    *handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * 48.0,
                    MouseScrollDelta::PixelDelta(pos) => pos.y / scale_factor,
                };
                // Wheel up (dy > 0) scrolls toward the top of the content.
                match self.screen {
                    Screen::Gallery => {
                        self.gallery_scroll = (self.gallery_scroll - dy).max(0.0);
                        self.dirty = true;
                        *handled = true;
                    }
                    Screen::Settings => {
                        self.settings_scroll = (self.settings_scroll - dy).max(0.0);
                        self.dirty = true;
                        *handled = true;
                    }
                    _ => {}
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                let act = match logical_key {
                    Key::Character(c) => match c.as_str() {
                        "1" => Some(Action::Nav(Screen::Camera)),
                        "2" => Some(Action::Nav(Screen::Scanner)),
                        "3" => Some(Action::Nav(Screen::Gallery)),
                        "4" => Some(Action::Nav(Screen::Settings)),
                        "g" | "G" => Some(Action::ToggleGrid),
                        "f" | "F" => Some(Action::CycleFacing),
                        " " if matches!(self.screen, Screen::Camera) => Some(Action::Shutter),
                        _ => None,
                    },
                    Key::Named(NamedKey::Space) if matches!(self.screen, Screen::Camera) => {
                        Some(Action::Shutter)
                    }
                    Key::Named(NamedKey::Escape) if matches!(self.screen, Screen::Scanner) => {
                        Some(Action::DismissScan)
                    }
                    Key::Named(NamedKey::Escape) if matches!(self.screen, Screen::Gallery) => {
                        Some(Action::CloseDetail)
                    }
                    Key::Named(NamedKey::Escape) if matches!(self.screen, Screen::Settings) => {
                        Some(Action::Back)
                    }
                    _ => None,
                };
                if let Some(a) = act {
                    self.apply(a, state, path);
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "CameraApp"
    }
    fn test_id(&self) -> Option<&str> {
        Some("camera_app")
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn needs_redraw(&self) -> bool {
        // Keep animating while recording so the elapsed timer advances and the
        // hermetic frame-synth keeps feeding the encoder.
        self.dirty || self.recording.is_some()
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &ui::permissions::PermissionContext,
    ) {
        self.draw(scene, t, tr, theme, presence);
    }
}

// ====================================================================
// Shared chrome + control helpers
// ====================================================================

impl CameraApp {
    /// Horizontal centre of the window — the shutter, mode switcher, toasts and
    /// recording badge all centre here.
    fn stage_center_x(&self) -> f64 {
        self.rect.center().x
    }

    /// A round floating glass button (used for the settings gear, the lens
    /// switch and back buttons over the viewfinder). Returns its hit rect.
    fn glass_button(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        theme: &Theme,
        cx: f64,
        cy: f64,
        r: f64,
        icon: &str,
        action: Action,
    ) -> Rect {
        let c = &theme.tokens.colors;
        p::circle(scene, t, cx, cy, r, p::alpha(c.neutral[0], 0.5));
        p::circle_stroke(scene, t, cx, cy, r, c.border_white_10, 1.0);
        let ir = r * 0.52;
        p::icon(
            scene,
            t,
            icon,
            Rect::new(cx - ir, cy - ir, cx + ir, cy + ir),
            white(),
            1.9,
        );
        let hit = Rect::new(cx - r, cy - r, cx + r, cy + r);
        self.push_region(hit, action);
        hit
    }

    /// The Photo / Video / Scan mode switcher that floats near the bottom of
    /// the viewfinder on both the Camera and Scanner screens. Photo and Video
    /// are capture sub-modes on the Camera screen; Scan switches to the
    /// scanner. This is how the viewfinder screens navigate between each other
    /// now that there is no rail.
    fn draw_mode_switcher(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        center_x: f64,
        top_y: f64,
    ) {
        let seg_w = 240.0;
        let seg = Rect::new(
            center_x - seg_w / 2.0,
            top_y,
            center_x + seg_w / 2.0,
            top_y + 34.0,
        );
        let on_cam = matches!(self.screen, Screen::Camera);
        let items: Vec<(String, bool, Action)> = vec![
            (
                "Photo".to_string(),
                on_cam && self.mode == CaptureMode::Photo,
                Action::SetMode(CaptureMode::Photo),
            ),
            (
                "Video".to_string(),
                on_cam && self.mode == CaptureMode::Video,
                Action::SetMode(CaptureMode::Video),
            ),
            (
                "Scan".to_string(),
                matches!(self.screen, Screen::Scanner),
                Action::Nav(Screen::Scanner),
            ),
        ];
        self.segmented(scene, t, tr, theme, seg, items);
    }

    fn segmented(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        items: Vec<(String, bool, Action)>,
    ) {
        let c = &theme.tokens.colors;
        p::fill_round(
            scene,
            t,
            rect,
            rect.height() / 2.0,
            p::alpha(c.neutral[2], 0.92),
        );
        p::stroke_round(
            scene,
            t,
            rect,
            rect.height() / 2.0,
            p::alpha(c.border_standard, 0.5),
            1.0,
        );
        let n = items.len().max(1);
        let seg_w = rect.width() / n as f64;
        for (i, (label, selected, action)) in items.into_iter().enumerate() {
            let seg = Rect::new(
                rect.x0 + i as f64 * seg_w,
                rect.y0,
                rect.x0 + (i as f64 + 1.0) * seg_w,
                rect.y1,
            );
            if selected {
                p::fill_round(
                    scene,
                    t,
                    shrink(seg, 3.0),
                    (seg.height() - 6.0) / 2.0,
                    theme.accent_primary,
                );
            }
            let col = if selected {
                white()
            } else {
                p::alpha(c.text_primary, 0.78)
            };
            p::text_in(scene, tr, seg, &label, 13.0, col);
            self.push_region(seg, action);
        }
    }

    fn toggle_switch(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        theme: &Theme,
        rect: Rect,
        on: bool,
        action: Action,
    ) {
        // Shared switch face on the settings sheet (solid chrome, not over-media):
        // themed track/knob/focus plus hover feedback, replacing the hand-painted
        // pill. The existing region hit-test still fires the toggle action.
        let hovered = rect.contains(self.last_mouse);
        Toggle::paint(scene, t, theme, &ToggleFace::new(rect, on).hovered(hovered));
        self.push_region(rect, action);
    }

    /// A centred "status" card for sparse states (no camera, empty library):
    /// a bounded, softly-raised panel holding an icon, title and optional
    /// subtitle. Anchoring the message in a real panel — capped in width and
    /// centred in `area` — keeps it from reading as a lone cluster stranded in
    /// a wide, empty window. Returns the icon's rect so callers can decorate it
    /// (e.g. a "no camera" slash).
    fn draw_empty_state(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        icon: &str,
        title: &str,
        subtitle: Option<&str>,
        glass: bool,
    ) -> Rect {
        let c = &theme.tokens.colors;
        // Bounded card, centred in `area`: wide enough to read as a panel,
        // capped so it never stretches edge-to-edge on ultra-wide windows.
        let pw = 360.0_f64.min((area.width() - 48.0).max(240.0));
        let base_h: f64 = if subtitle.is_some() { 208.0 } else { 172.0 };
        let ph = base_h.min((area.height() - 32.0).max(140.0));
        let cx = area.center().x;
        let cy = area.center().y;
        let panel = Rect::new(cx - pw / 2.0, cy - ph / 2.0, cx + pw / 2.0, cy + ph / 2.0);

        // Glass over the near-black viewfinder; a softly-raised surface over the
        // content background. Colours come from the theme in both cases.
        let (fill, title_col, sub_col, icon_col) = if glass {
            (
                p::alpha(c.neutral[1], 0.55),
                p::alpha(white(), 0.92),
                p::alpha(white(), 0.55),
                p::alpha(white(), 0.5),
            )
        } else {
            (
                p::alpha(c.surfaces.raised, 0.55),
                c.text_primary,
                c.text_dim,
                p::alpha(c.text_dim, 0.85),
            )
        };
        p::fill_round(scene, t, panel, 22.0, fill);
        p::stroke_round(scene, t, panel, 22.0, c.border_white_10, 1.0);

        let icx = panel.center().x;
        let icy = panel.y0 + 56.0;
        let icon_r = Rect::new(icx - 24.0, icy - 24.0, icx + 24.0, icy + 24.0);
        p::icon(scene, t, icon, icon_r, icon_col, 2.0);
        let ty = panel.y0 + 116.0;
        p::text_center(scene, tr, icx, ty, title, 16.0, title_col);
        if let Some(sub) = subtitle {
            p::text_center(scene, tr, icx, ty + 24.0, sub, 12.5, sub_col);
        }
        icon_r
    }

    /// A full-bleed "camera unavailable" placeholder for the camera screen
    /// when no live feed is present. Near-black so it reads as "camera off",
    /// honest rather than faking a feed; the message sits in a centred card so
    /// it stays anchored at wide widths.
    fn draw_no_feed(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        subtitle: &str,
    ) {
        let c = &theme.tokens.colors;
        p::fill_rect(scene, t, rect, c.neutral[0]);
        // Centre the message in the whole window — the viewfinder is full-bleed.
        let ic = self.draw_empty_state(
            scene,
            t,
            tr,
            theme,
            rect,
            "camera",
            "No camera connected",
            Some(subtitle),
            true,
        );
        // Slash across the icon to read as "no camera".
        p::line(
            scene,
            t,
            ic.x0 + 3.0,
            ic.y0 + 3.0,
            ic.x1 - 3.0,
            ic.y1 - 3.0,
            p::alpha(white(), 0.5),
            2.2,
        );
    }

    fn draw_toast(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let Some(msg) = self.toast.clone() else {
            return;
        };
        let c = &theme.tokens.colors;
        let tw = f64::from(p::text_w(tr, &msg, 13.0));
        let w = tw + 56.0;
        let cx = self.stage_center_x();
        let r = Rect::new(
            cx - w / 2.0,
            self.rect.y1 - 64.0,
            cx + w / 2.0,
            self.rect.y1 - 28.0,
        );
        p::fill_round(scene, t, r, 18.0, p::alpha(c.neutral[1], 0.96));
        p::stroke_round(scene, t, r, 18.0, p::alpha(theme.accent_primary, 0.5), 1.0);
        p::icon(
            scene,
            t,
            "check",
            Rect::new(
                r.x0 + 14.0,
                r.center().y - 8.0,
                r.x0 + 30.0,
                r.center().y + 8.0,
            ),
            theme.status_success,
            2.0,
        );
        p::text(
            scene,
            tr,
            r.x0 + 36.0,
            r.center().y - 8.0,
            &msg,
            13.0,
            c.text_primary,
        );
    }

    /// A slim, non-interactive scroll indicator pinned to the right edge of
    /// `area`. Only drawn when the content overflows.
    fn draw_scrollbar(
        &self,
        scene: &mut Scene,
        t: Affine,
        theme: &Theme,
        area: Rect,
        scroll: f64,
        max_scroll: f64,
        view_h: f64,
        content_h: f64,
    ) {
        if max_scroll <= 0.0 || content_h <= 0.0 {
            return;
        }
        let c = &theme.tokens.colors;
        let track = Rect::new(area.x1 - 6.0, area.y0 + 6.0, area.x1 - 3.0, area.y1 - 6.0);
        let vh = track.height();
        let thumb_h = (vh * (view_h / content_h)).clamp(28.0, vh);
        let frac = (scroll / max_scroll).clamp(0.0, 1.0);
        let ty = track.y0 + (vh - thumb_h) * frac;
        p::fill_round(
            scene,
            t,
            Rect::new(track.x0, ty, track.x1, ty + thumb_h),
            1.5,
            p::alpha(c.text_dim, 0.55),
        );
    }
}

// ====================================================================
// Camera screen
// ====================================================================

impl CameraApp {
    fn draw_camera(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
    ) {
        let has_feed = self.feed_active();
        let recording = self.is_recording();

        // 1. Edge-to-edge viewfinder — the live preview bleeds to every window
        // edge; every control floats over it rather than living in a panel.
        if has_feed {
            let vw = full.width().max(2.0) as u32;
            let vh = full.height().max(2.0) as u32;
            match &self.storage {
                Storage::Sample => {
                    let seed = Self::viewfinder_seed(self.facing);
                    let need = self
                        .cam_view
                        .as_ref()
                        .is_none_or(|(w, h, s, _)| (*w, *h, *s) != (vw, vh, seed));
                    if need {
                        self.cam_view = Some((
                            vw,
                            vh,
                            seed,
                            frame_image_fill(frame::viewfinder(seed, vw, vh)),
                        ));
                    }
                    if let Some((_, _, _, img)) = self.cam_view.as_mut() {
                        img.paint_at(scene, t, full);
                    }
                }
                Storage::Disk(_) => {
                    let cur_gen = self.frame_gen;
                    if self.live_view.as_ref().is_none_or(|(g, _)| *g != cur_gen)
                        && let Some(fb) = self.latest_frame.clone()
                    {
                        self.live_view = Some((cur_gen, frame_image_cover(fb)));
                    }
                    if let Some((_, img)) = self.live_view.as_mut() {
                        img.paint_at(scene, t, full);
                    }
                }
            }
            self.draw_viewfinder_overlays(scene, t, theme, full);
            // Legibility washes: a soft dark fade top and bottom so white
            // controls stay readable over any scene — no opaque panels.
            let blk = Color::from_rgb8(0, 0, 0);
            p::vscrim(
                scene,
                t,
                Rect::new(full.x0, full.y0, full.x1, full.y0 + 104.0),
                blk,
                0.42,
                0.0,
            );
            p::vscrim(
                scene,
                t,
                Rect::new(full.x0, full.y1 - 200.0, full.x1, full.y1),
                blk,
                0.0,
                0.6,
            );
            self.draw_camera_top_controls(scene, t, tr, theme, full);
            if recording {
                self.draw_rec_indicator(scene, t, tr, full);
            }
        } else {
            self.draw_no_feed(
                scene,
                t,
                tr,
                theme,
                full,
                "Connect a camera to start the viewfinder",
            );
        }

        // Settings gear, top-right — opens Settings as an overlay sheet. Drawn
        // whether or not a feed is present so Settings is always reachable.
        self.glass_button(
            scene,
            t,
            theme,
            full.x1 - EDGE - 16.0,
            full.y0 + 35.0,
            16.0,
            "settings",
            Action::Nav(Screen::Settings),
        );

        // 2. Bottom capture cluster (always drawn; disabled without a feed).
        self.draw_capture_cluster(scene, t, tr, theme, full, has_feed, recording);
    }

    fn draw_viewfinder_overlays(&self, scene: &mut Scene, t: Affine, theme: &Theme, vf: Rect) {
        // Rule-of-thirds grid.
        if self.settings.grid {
            let gc = p::alpha(white(), 0.22);
            for k in 1..3 {
                let x = vf.x0 + vf.width() * f64::from(k) / 3.0;
                p::line(scene, t, x, vf.y0, x, vf.y1, gc, 1.0);
                let y = vf.y0 + vf.height() * f64::from(k) / 3.0;
                p::line(scene, t, vf.x0, y, vf.x1, y, gc, 1.0);
            }
        }
        // Level horizon indicator (only when enabled — no permanent reticle
        // cluttering the centre of the frame).
        if self.settings.level {
            let y = vf.center().y;
            p::line(
                scene,
                t,
                vf.center().x - 60.0,
                y,
                vf.center().x + 60.0,
                y,
                p::alpha(theme.status_success, 0.9),
                2.0,
            );
            p::line(
                scene,
                t,
                vf.center().x - 92.0,
                y,
                vf.center().x - 70.0,
                y,
                p::alpha(white(), 0.55),
                2.0,
            );
            p::line(
                scene,
                t,
                vf.center().x + 70.0,
                y,
                vf.center().x + 92.0,
                y,
                p::alpha(white(), 0.55),
                2.0,
            );
        }
    }

    /// A floating glass control pill (leading icon optional). `active` fills it
    /// with the accent; otherwise it's dark glass with a hairline border.
    /// Returns the pill's rect so the caller can register a hit region.
    fn ctrl_pill(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        x: f64,
        y: f64,
        icon: Option<&str>,
        label: &str,
        active: bool,
    ) -> Rect {
        let c = &theme.tokens.colors;
        let pad = 11.0;
        let icon_w = if icon.is_some() { 17.0 } else { 0.0 };
        let tw = if label.is_empty() {
            0.0
        } else {
            f64::from(p::text_w(tr, label, 12.0))
        };
        let gap = if icon.is_some() && !label.is_empty() {
            5.0
        } else {
            0.0
        };
        let w = pad * 2.0 + icon_w + gap + tw;
        let h = 30.0;
        let r = Rect::new(x, y, x + w, y + h);
        if active {
            p::fill_round(scene, t, r, h / 2.0, theme.accent_primary);
        } else {
            p::fill_round(scene, t, r, h / 2.0, p::alpha(c.neutral[0], 0.5));
            p::stroke_round(scene, t, r, h / 2.0, c.border_white_10, 1.0);
        }
        let fg = if active {
            white()
        } else {
            p::alpha(white(), 0.92)
        };
        let mut cx = x + pad;
        if let Some(ic) = icon {
            p::icon(
                scene,
                t,
                ic,
                Rect::new(cx, r.center().y - 8.5, cx + 17.0, r.center().y + 8.5),
                fg,
                1.7,
            );
            cx += icon_w + gap;
        }
        if !label.is_empty() {
            p::text(scene, tr, cx, r.center().y - 8.0, label, 12.0, fg);
        }
        r
    }

    /// The quick-control row floating along the top of the viewfinder:
    /// resolution, HDR, self-timer and grid — each tappable, so the settings
    /// most-used while shooting are one tap away rather than buried.
    fn draw_camera_top_controls(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
    ) {
        let mut x = full.x0 + EDGE;
        let y = full.y0 + 20.0;
        let gap = 9.0;

        let r = self.ctrl_pill(
            scene,
            t,
            tr,
            theme,
            x,
            y,
            None,
            self.settings.resolution.label(),
            false,
        );
        self.push_region(r, Action::CycleResolution);
        x = r.x1 + gap;

        let r = self.ctrl_pill(scene, t, tr, theme, x, y, None, "HDR", self.settings.hdr);
        self.push_region(r, Action::ToggleHdr);
        x = r.x1 + gap;

        let next_timer = cycle(&Timer::ALL, self.settings.timer);
        let tlabel = if self.settings.timer == Timer::Off {
            "Timer".to_string()
        } else {
            self.settings.timer.label().to_string()
        };
        let r = self.ctrl_pill(
            scene,
            t,
            tr,
            theme,
            x,
            y,
            Some("timer"),
            &tlabel,
            self.settings.timer != Timer::Off,
        );
        self.push_region(r, Action::SetTimer(next_timer));
        x = r.x1 + gap;

        let r = self.ctrl_pill(
            scene,
            t,
            tr,
            theme,
            x,
            y,
            Some("grid"),
            "",
            self.settings.grid,
        );
        self.push_region(r, Action::ToggleGrid);
    }

    /// The bottom capture cluster floating over the viewfinder: a gallery
    /// thumbnail peek, the shutter, a lens switch, and — when idle — the
    /// Photo/Video/Scan mode switcher above the shutter.
    fn draw_capture_cluster(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
        has_feed: bool,
        recording: bool,
    ) {
        let c = &theme.tokens.colors;
        let scx = self.stage_center_x();
        let cy = full.y1 - 64.0;
        let danger = c.semantic.danger;

        // Photo / Video / Scan switcher above the shutter — hidden while
        // recording (mode is locked; the stop-square shutter + REC badge carry
        // the state).
        if !recording {
            self.draw_mode_switcher(scene, t, tr, theme, scx, full.y1 - 132.0);
        }

        // Gallery thumbnail peek, bottom-left. Tap → Gallery.
        let ts = 30.0;
        let thumb = Rect::new(
            full.x0 + EDGE + 8.0,
            cy - ts,
            full.x0 + EDGE + 8.0 + ts * 2.0,
            cy + ts,
        );
        if let Some(cap) = self.captures.first().cloned() {
            self.ensure_thumb(&cap);
            p::push_clip_round(scene, t, thumb, 14.0);
            if let Some(img) = self.thumbs.get_mut(&cap.id) {
                img.paint_at(scene, t, thumb);
            }
            p::pop(scene);
            p::stroke_round(scene, t, thumb, 14.0, p::alpha(white(), 0.65), 1.5);
            let n = self.captures.len();
            let badge = Rect::new(
                thumb.x1 - 15.0,
                thumb.y0 - 9.0,
                thumb.x1 + 9.0,
                thumb.y0 + 15.0,
            );
            p::fill_round(scene, t, badge, 12.0, theme.accent_primary);
            p::text_in(scene, tr, badge, &n.to_string(), 11.0, white());
        } else {
            p::fill_round(scene, t, thumb, 14.0, p::alpha(c.neutral[0], 0.5));
            p::stroke_round(scene, t, thumb, 14.0, c.border_white_10, 1.5);
            p::icon(
                scene,
                t,
                "gallery",
                shrink(thumb, 16.0),
                p::alpha(white(), 0.7),
                1.6,
            );
        }
        self.push_region(thumb, Action::Nav(Screen::Gallery));

        // Shutter (centre). Photo: white disc. Video: red dot → red stop-square
        // while recording.
        let outer = 34.0;
        let ring = if recording {
            danger
        } else if has_feed {
            p::alpha(white(), 0.95)
        } else {
            p::alpha(white(), 0.4)
        };
        p::circle_stroke(scene, t, scx, cy, outer, ring, 4.5);
        match self.mode {
            CaptureMode::Photo => {
                let core = if has_feed {
                    white()
                } else {
                    p::alpha(white(), 0.4)
                };
                p::circle(scene, t, scx, cy, outer - 8.0, core);
            }
            CaptureMode::Video if recording => {
                let s = 14.0;
                p::fill_round(
                    scene,
                    t,
                    Rect::new(scx - s, cy - s, scx + s, cy + s),
                    5.0,
                    danger,
                );
            }
            CaptureMode::Video => {
                let core = if has_feed {
                    danger
                } else {
                    p::alpha(danger, 0.4)
                };
                p::circle(scene, t, scx, cy, outer - 10.0, core);
            }
        }
        self.push_region(
            Rect::new(scx - outer, cy - outer, scx + outer, cy + outer),
            Action::Shutter,
        );

        // Lens switch (bottom-right) — cycles rear / front / ultra-wide.
        let lx = full.x1 - 60.0;
        p::circle(scene, t, lx, cy, 26.0, p::alpha(c.neutral[0], 0.5));
        p::circle_stroke(scene, t, lx, cy, 26.0, c.border_white_10, 1.0);
        p::icon(
            scene,
            t,
            "switch",
            Rect::new(lx - 13.0, cy - 13.0, lx + 13.0, cy + 13.0),
            white(),
            2.0,
        );
        p::text_center(
            scene,
            tr,
            lx,
            cy + 34.0,
            self.facing.label(),
            10.0,
            p::alpha(white(), 0.78),
        );
        self.push_region(
            Rect::new(lx - 26.0, cy - 26.0, lx + 26.0, cy + 26.0),
            Action::CycleFacing,
        );
    }

    /// A red "● REC m:ss" badge floating at the top-centre of the stage while
    /// a clip is recording.
    fn draw_rec_indicator(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, full: Rect) {
        let scx = self.stage_center_x();
        let started = self.recording.as_ref().map_or(0, |s| s.started);
        let elapsed = (now_secs() - started).max(0);
        let label = format!("REC  {}", fmt_dur(elapsed * 1000));
        let red = Color::from_rgb8(240, 60, 60);
        let tw = f64::from(p::text_w(tr, &label, 12.5));
        let w = 20.0 + 12.0 + tw + 14.0;
        let pill = Rect::new(scx - w / 2.0, full.y0 + 18.0, scx + w / 2.0, full.y0 + 46.0);
        p::fill_round(
            scene,
            t,
            pill,
            14.0,
            p::alpha(Color::from_rgb8(0, 0, 0), 0.55),
        );
        p::stroke_round(scene, t, pill, 14.0, p::alpha(red, 0.55), 1.0);
        p::circle(scene, t, pill.x0 + 17.0, pill.center().y, 5.0, red);
        p::text(
            scene,
            tr,
            pill.x0 + 29.0,
            pill.center().y - 8.0,
            &label,
            12.5,
            white(),
        );
    }

    fn ensure_thumb(&mut self, cap: &Capture) {
        if self.thumbs.contains_key(&cap.id) {
            return;
        }
        let (w, h) = THUMB;
        let img = match &cap.path {
            Some(p) => match library::load_thumb(p, w, h) {
                Some((rgba, tw, th)) => {
                    Image::from_rgba(Arc::from(rgba), tw, th).with_fit(ImageFit::Cover)
                }
                None => frame_image_cover(frame::thumbnail(cap.scene, w, h)),
            },
            None => frame_image_cover(frame::thumbnail(cap.scene, w, h)),
        };
        self.thumbs.insert(cap.id.clone(), img);
    }
}

// ====================================================================
// Scanner screen
// ====================================================================

impl CameraApp {
    fn draw_scanner(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
    ) {
        let live = self.feed_active();
        let wide = full.width() >= WIDE_MIN;
        let panel_w = if wide { 300.0 } else { 0.0 };
        let panel_gap = if wide { 14.0 } else { 0.0 };
        // The scan stage: the full width minus the optional recent-scans panel.
        // The scan window + result card centre here.
        let stage = Rect::new(
            full.x0,
            full.y0,
            full.x1 - EDGE - panel_w - panel_gap,
            full.y1,
        );

        if live {
            self.draw_scanner_feed(scene, t, tr, theme, full, stage);
        } else {
            self.draw_scanner_offline(scene, t, tr, theme, full, stage);
        }

        // Floating title, top-left over the feed.
        let title = if live {
            "Scan a code"
        } else {
            "Scan from a photo"
        };
        let subtitle = if live {
            "Point at any QR or barcode — detection is automatic"
        } else {
            "No camera — pick a saved photo to scan a code from it"
        };
        self.draw_glass_header(
            scene,
            t,
            tr,
            theme,
            full.x0 + EDGE,
            full.y0 + 18.0,
            title,
            subtitle,
        );

        // Settings gear, top-right.
        self.glass_button(
            scene,
            t,
            theme,
            full.x1 - EDGE - 16.0,
            full.y0 + 34.0,
            16.0,
            "settings",
            Action::Nav(Screen::Settings),
        );

        // Recent scans, floating glass panel (wide only) — below the gear and
        // clear of the bottom mode switcher.
        if wide {
            let panel = Rect::new(
                full.x1 - EDGE - panel_w,
                full.y0 + 62.0,
                full.x1 - EDGE,
                full.y1 - 74.0,
            );
            self.draw_scan_panel(scene, t, tr, theme, panel);
        }

        // Bottom-centre mode switcher: the scanner is a peer of Photo/Video, so
        // you switch back to the camera the same way you got here. Centred on
        // the feed (clear of the recent-scans panel) to align with the scan
        // window and result card.
        self.draw_mode_switcher(scene, t, tr, theme, full.center().x, full.y1 - 56.0);
    }

    /// A compact floating glass header (title + subtitle) used as an overlay
    /// label on the scanner's edge-to-edge feed.
    fn draw_glass_header(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        x: f64,
        y: f64,
        title: &str,
        subtitle: &str,
    ) {
        let c = &theme.tokens.colors;
        let tw = f64::from(p::text_w(tr, title, 15.0));
        let sw = f64::from(p::text_w(tr, subtitle, 11.5));
        let w = tw.max(sw) + 32.0;
        let r = Rect::new(x, y, x + w, y + 52.0);
        p::fill_round(scene, t, r, 16.0, p::alpha(c.neutral[0], 0.55));
        p::stroke_round(scene, t, r, 16.0, c.border_white_10, 1.0);
        p::text(scene, tr, x + 16.0, y + 12.0, title, 15.0, white());
        p::text(
            scene,
            tr,
            x + 16.0,
            y + 32.0,
            subtitle,
            11.5,
            p::alpha(white(), 0.7),
        );
    }

    fn draw_scanner_feed(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
        stage: Rect,
    ) {
        let vw = full.width().max(2.0) as u32;
        let vh = full.height().max(2.0) as u32;

        // Produce the viewfinder image + a real decode (keyed on the full size).
        match &self.storage {
            Storage::Sample => {
                if self
                    .scan_view
                    .as_ref()
                    .is_none_or(|(w, h, _)| (*w, *h) != (vw, vh))
                {
                    let (buf, scanned) = frame::scanner_frame(4, vw, vh, SCAN_PAYLOAD);
                    self.scan_view = Some((vw, vh, frame_image_fill(buf)));
                    self.scanned = scanned;
                    self.scan_gen = (vw, vh);
                }
            }
            Storage::Disk(_) => {
                let cur_gen = self.frame_gen;
                if self.live_view.as_ref().is_none_or(|(g, _)| *g != cur_gen)
                    && let Some(fb) = self.latest_frame.clone()
                {
                    let luma = frame::luma(&fb);
                    let (fw, fh) = (fb.w, fb.h);
                    self.scanned =
                        crate::scan::decode_and_classify(&luma, fw as usize, fh as usize, |p| p);
                    self.scan_gen = (fw, fh);
                    self.live_view = Some((cur_gen, frame_image_cover(fb)));
                }
            }
        }

        // Edge-to-edge feed.
        match &self.storage {
            Storage::Sample => {
                if let Some((_, _, img)) = self.scan_view.as_mut() {
                    img.paint_at(scene, t, full);
                }
            }
            Storage::Disk(_) => {
                if let Some((_, img)) = self.live_view.as_mut() {
                    img.paint_at(scene, t, full);
                }
            }
        }

        // Darken everything but a scan window centred on the frame (where the
        // subject — and the hermetic sample QR — actually is). Sized to sit
        // clear of the rail and the recent-scans panel.
        let side = (stage.width().min(stage.height()) * 0.62).clamp(180.0, 460.0);
        // Frame the subject at the centre of the feed. The window side is
        // clamped to the (panel-excluded) stage, so it stays clear of the
        // recent-scans panel on wide layouts.
        let wc = Point::new(full.center().x, full.center().y);
        let win = Rect::new(
            wc.x - side / 2.0,
            wc.y - side / 2.0,
            wc.x + side / 2.0,
            wc.y + side / 2.0,
        );
        let dark = p::alpha(Color::from_rgb8(0, 0, 0), 0.5);
        p::fill_rect(scene, t, Rect::new(full.x0, full.y0, full.x1, win.y0), dark);
        p::fill_rect(scene, t, Rect::new(full.x0, win.y1, full.x1, full.y1), dark);
        p::fill_rect(scene, t, Rect::new(full.x0, win.y0, win.x0, win.y1), dark);
        p::fill_rect(scene, t, Rect::new(win.x1, win.y0, full.x1, win.y1), dark);
        let blen = 28.0;
        let acc = theme.accent_primary;
        for &(sx, sy) in &[(-1.0_f64, -1.0_f64), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
            let ox = if sx < 0.0 { win.x0 } else { win.x1 };
            let oy = if sy < 0.0 { win.y0 } else { win.y1 };
            p::line(scene, t, ox, oy, ox - sx * blen, oy, acc, 3.0);
            p::line(scene, t, ox, oy, ox, oy - sy * blen, acc, 3.0);
        }
        p::line(
            scene,
            t,
            win.x0 + 8.0,
            win.center().y,
            win.x1 - 8.0,
            win.center().y,
            p::alpha(acc, 0.7),
            2.0,
        );

        // Detected bounding box (decode corners → full-frame coords).
        if let Some(s) = self.scanned.clone() {
            let (gw, gh) = self.scan_gen;
            let sx = full.width() / f64::from(gw.max(1));
            let sy = full.height() / f64::from(gh.max(1));
            let pt = |(x, y): (f32, f32)| {
                Point::new(full.x0 + f64::from(x) * sx, full.y0 + f64::from(y) * sy)
            };
            let mut bez = BezPath::new();
            let pts: Vec<Point> = s.corners.iter().map(|c| pt(*c)).collect();
            bez.move_to(pts[0]);
            for q in &pts[1..] {
                bez.line_to(*q);
            }
            bez.close_path();
            let green = theme.status_success;
            scene.fill(
                vello::peniko::Fill::NonZero,
                t,
                p::alpha(green, 0.16),
                None,
                &bez,
            );
            scene.stroke(&vello::kurbo::Stroke::new(3.0), t, green, None, &bez);
            for q in &pts {
                p::circle(scene, t, q.x, q.y, 4.0, green);
            }
            let cxy = s.center();
            let tag = detected_label(&s.symbology);
            let tw = f64::from(p::text_w(tr, &tag, 11.0)) + 18.0;
            let tagx = (full.x0 + f64::from(cxy.0) * sx) - tw / 2.0;
            let top = pts.iter().map(|q| q.y).fold(f64::MAX, f64::min);
            let tagr = Rect::new(tagx, top - 26.0, tagx + tw, top - 6.0);
            p::fill_round(scene, t, tagr, 10.0, green);
            p::text_in(scene, tr, tagr, &tag, 11.0, white());
        }

        // Floating result card, centred on the frame under the scan window.
        if let Some(s) = self.scanned.clone()
            && !self.scan_dismissed
        {
            self.draw_scan_card(scene, t, tr, theme, full, &s);
        }
    }

    /// No live camera: let the user scan a code out of a photo already in
    /// the library. Real decode — tapping a thumbnail runs the QR detector
    /// over the file's pixels.
    fn draw_scanner_offline(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
        stage: Rect,
    ) {
        let c = &theme.tokens.colors;
        // Full-bleed "camera off" backdrop; the photo picker floats over it.
        p::fill_rect(scene, t, full, c.neutral[0]);

        let photos: Vec<Capture> = self
            .captures
            .iter()
            .filter(|c| !c.is_video() && c.path.is_some())
            .cloned()
            .collect();

        let grid = Rect::new(
            stage.x0 + EDGE,
            full.y0 + 86.0,
            stage.x1 - 8.0,
            full.y1 - 76.0,
        );

        if photos.is_empty() {
            let cx = grid.center().x;
            let cy = grid.center().y;
            let ic = Rect::new(cx - 24.0, cy - 58.0, cx + 24.0, cy - 10.0);
            p::icon(scene, t, "scan", ic, p::alpha(white(), 0.5), 2.0);
            p::text_center(
                scene,
                tr,
                cx,
                cy + 6.0,
                "Nothing to scan yet",
                16.0,
                p::alpha(white(), 0.9),
            );
            p::text_center(
                scene,
                tr,
                cx,
                cy + 30.0,
                "Save a photo of a QR or barcode and it can be scanned here",
                12.5,
                p::alpha(white(), 0.55),
            );
            return;
        }

        let target_w = 156.0;
        let gap = 12.0;
        // Shared reflow math so this grid lines up exactly like every other
        // card grid in the OS (one canonical formula, not a per-app copy).
        let cols = ui::widget::card_columns(grid.width(), target_w, gap);
        let cell_w = ui::widget::card_column_width(grid.width(), cols, gap);
        let card_h = (cell_w * 0.72).clamp(94.0, 150.0);
        p::push_clip_rect(scene, t, grid);
        for (i, cap) in photos.iter().enumerate() {
            let col = i % cols;
            let row = i / cols;
            let x0 = grid.x0 + col as f64 * (cell_w + gap);
            let y0 = grid.y0 + row as f64 * (card_h + gap);
            if y0 > grid.y1 {
                break;
            }
            let card = Rect::new(x0, y0, x0 + cell_w, y0 + card_h);
            self.ensure_thumb(cap);
            p::push_clip_round(scene, t, card, 14.0);
            if let Some(img) = self.thumbs.get_mut(&cap.id) {
                img.paint_at(scene, t, card);
            }
            p::pop(scene);
            p::stroke_round(scene, t, card, 14.0, c.border_white_10, 1.0);
            self.push_region(card, Action::ScanLibraryItem(cap.id.clone()));
        }
        p::pop(scene);

        if let Some(s) = self.scanned.clone()
            && !self.scan_dismissed
        {
            self.draw_scan_card(scene, t, tr, theme, stage, &s);
        }
    }

    fn draw_scan_card(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        stage: Rect,
        s: &ScannedCode,
    ) {
        let c = &theme.tokens.colors;
        let cw = stage.width().min(560.0) - 36.0;
        let ch = 122.0;
        // Sit above the bottom mode switcher rather than under it.
        let bottom = stage.y1 - 76.0;
        let card = Rect::new(
            stage.center().x - cw / 2.0,
            bottom - ch,
            stage.center().x + cw / 2.0,
            bottom,
        );
        p::fill_round(scene, t, card, 18.0, p::alpha(c.neutral[0], 0.92));
        p::stroke_round(scene, t, card, 18.0, c.border_white_10, 1.0);

        let icon_name = code_icon(&s.kind);
        let badge = Rect::new(
            card.x0 + 16.0,
            card.y0 + 16.0,
            card.x0 + 60.0,
            card.y0 + 60.0,
        );
        p::fill_round(scene, t, badge, 13.0, p::alpha(theme.accent_primary, 0.20));
        p::icon(
            scene,
            t,
            icon_name,
            shrink(badge, 11.0),
            theme.accent_primary,
            2.0,
        );

        let tx = badge.x1 + 14.0;
        p::text(scene, tr, tx, card.y0 + 16.0, s.kind.title(), 15.0, white());
        let summ = s.kind.summary(&s.raw);
        let summ = ellipsize(tr, &summ, 13.0, card.x1 - tx - 40.0);
        p::text(
            scene,
            tr,
            tx,
            card.y0 + 39.0,
            &summ,
            13.0,
            p::alpha(white(), 0.7),
        );

        let xr = Rect::new(
            card.x1 - 34.0,
            card.y0 + 12.0,
            card.x1 - 12.0,
            card.y0 + 34.0,
        );
        p::icon(scene, t, "close", xr, p::alpha(white(), 0.6), 1.8);
        self.push_region(xr, Action::DismissScan);

        let actions = s.kind.actions();
        let mut bx = card.x0 + 16.0;
        let by = card.y1 - 42.0;
        for (i, label) in actions.iter().enumerate() {
            let tw = f64::from(p::text_w(tr, label, 13.0));
            let bw = tw + 30.0;
            let br = Rect::new(bx, by, bx + bw, by + 30.0);
            if i == 0 {
                p::fill_round(scene, t, br, 15.0, theme.accent_primary);
                p::text_in(scene, tr, br, label, 13.0, white());
            } else {
                p::fill_round(scene, t, br, 15.0, c.surface_white_10);
                p::text_in(scene, tr, br, label, 13.0, p::alpha(white(), 0.92));
            }
            self.push_region(br, Action::ScanAction(i));
            bx += bw + 10.0;
        }
    }

    fn draw_scan_panel(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
    ) {
        let c = &theme.tokens.colors;
        p::fill_round(scene, t, panel, 20.0, p::alpha(c.neutral[0], 0.55));
        p::stroke_round(scene, t, panel, 20.0, c.border_white_10, 1.0);
        let px = panel.x0 + 18.0;
        p::text(
            scene,
            tr,
            px,
            panel.y0 + 20.0,
            "Recent scans",
            14.0,
            white(),
        );

        if self.scans.is_empty() {
            let cx = panel.center().x;
            let cy = panel.y0 + 120.0;
            p::icon(
                scene,
                t,
                "scan",
                Rect::new(cx - 18.0, cy - 18.0, cx + 18.0, cy + 18.0),
                p::alpha(white(), 0.45),
                1.8,
            );
            p::text_center(
                scene,
                tr,
                cx,
                cy + 28.0,
                "No codes scanned yet",
                13.0,
                p::alpha(white(), 0.7),
            );
            p::text_center(
                scene,
                tr,
                cx,
                cy + 48.0,
                "Scanned codes show up here",
                11.5,
                p::alpha(white(), 0.5),
            );
            return;
        }

        let now = now_secs();
        let mut y = panel.y0 + 50.0;
        for entry in self.scans.clone().iter().take(8) {
            if y + 44.0 > panel.y1 - 12.0 {
                break;
            }
            let kind = classify(&entry.raw);
            let row = Rect::new(panel.x0 + 8.0, y, panel.x1 - 8.0, y + 44.0);
            p::icon(
                scene,
                t,
                code_icon(&kind),
                Rect::new(
                    row.x0 + 8.0,
                    row.center().y - 9.0,
                    row.x0 + 26.0,
                    row.center().y + 9.0,
                ),
                p::alpha(c.accent[6], 0.95),
                1.8,
            );
            let summ = ellipsize(tr, &kind.summary(&entry.raw), 13.0, row.width() - 44.0);
            p::text(
                scene,
                tr,
                row.x0 + 36.0,
                row.center().y - 14.0,
                &summ,
                13.0,
                white(),
            );
            p::text(
                scene,
                tr,
                row.x0 + 36.0,
                row.center().y + 2.0,
                &ago(entry.ts, now),
                11.0,
                p::alpha(white(), 0.6),
            );
            y += 46.0;
        }
    }
}

/// The badge shown over a live-detected code in the scanner viewfinder, e.g.
/// `"QR Code detected"`. A single space between the symbology and the verb.
fn detected_label(symbology: &str) -> String {
    format!("{symbology} detected")
}

fn code_icon(kind: &CodeKind) -> &'static str {
    match kind {
        CodeKind::Url => "link",
        CodeKind::Wifi { .. } => "wifi",
        CodeKind::Contact { .. } => "person",
        CodeKind::Email => "copy",
        CodeKind::Phone => "phone",
        CodeKind::Geo => "map",
        CodeKind::Text => "search",
    }
}

// ====================================================================
// Gallery screen
// ====================================================================

impl CameraApp {
    fn draw_gallery(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
    ) {
        let c = &theme.tokens.colors;
        let pad = 24.0;
        let now = now_secs();
        let wide = content.width() >= WIDE_MIN;

        // Back button, top-left — returns to the viewfinder the gallery was
        // opened from (there is no rail to navigate from).
        let bb_cx = content.x0 + pad + 16.0;
        let bb_cy = content.y0 + 30.0;
        p::circle(scene, t, bb_cx, bb_cy, 16.0, c.surfaces.raised);
        p::circle_stroke(
            scene,
            t,
            bb_cx,
            bb_cy,
            16.0,
            p::alpha(c.border_standard, 0.5),
            1.0,
        );
        p::icon(
            scene,
            t,
            "back",
            Rect::new(bb_cx - 8.0, bb_cy - 8.0, bb_cx + 8.0, bb_cy + 8.0),
            c.text_primary,
            1.9,
        );
        self.push_region(
            Rect::new(bb_cx - 16.0, bb_cy - 16.0, bb_cx + 16.0, bb_cy + 16.0),
            Action::Back,
        );

        // Header, beside the back button.
        let hx = bb_cx + 16.0 + 16.0;
        p::text(
            scene,
            tr,
            hx,
            content.y0 + 16.0,
            "Library",
            20.0,
            c.text_primary,
        );
        let count = match self.captures.len() {
            0 => "Empty".to_string(),
            1 => "1 item".to_string(),
            n => format!("{n} items"),
        };
        p::text(scene, tr, hx, content.y0 + 44.0, &count, 12.5, c.text_dim);

        let seg_w = 320.0_f64
            .min(content.width() - 2.0 * pad - 120.0)
            .max(200.0);
        let seg = Rect::new(
            content.x1 - pad - seg_w,
            content.y0 + 16.0,
            content.x1 - pad,
            content.y0 + 48.0,
        );
        let items: Vec<(String, bool, Action)> = GalleryFilter::ALL
            .iter()
            .map(|f| {
                (
                    f.label().to_string(),
                    *f == self.filter,
                    Action::SetFilter(*f),
                )
            })
            .collect();
        self.segmented(scene, t, tr, theme, seg, items);

        let filtered = self.filtered();

        // Empty state — anchored in a centred card so it never reads as a lone
        // cluster stranded in a wide, empty window.
        if filtered.is_empty() {
            let area = Rect::new(
                content.x0 + pad,
                content.y0 + 64.0,
                content.x1 - pad,
                content.y1 - pad,
            );
            let msg = match self.filter {
                GalleryFilter::All => "No photos yet",
                GalleryFilter::Photos => "No photos",
                GalleryFilter::Videos => "No videos",
                GalleryFilter::Favorites => "No favorites yet",
            };
            let sub = matches!(self.filter, GalleryFilter::All)
                .then_some("Take a photo and it'll show up here");
            self.draw_empty_state(scene, t, tr, theme, area, "gallery", msg, sub, false);
            return;
        }

        // Selection: on wide, always show a detail panel (default to first);
        // on narrow, only when the user explicitly picked an item (overlay).
        let sel_id = self
            .selected_capture
            .clone()
            .filter(|id| filtered.iter().any(|c| &c.id == id))
            .or_else(|| {
                if wide {
                    filtered.first().map(|c| c.id.clone())
                } else {
                    None
                }
            });

        let detail_w = if wide && sel_id.is_some() { 332.0 } else { 0.0 };
        let detail_gap = if detail_w > 0.0 { 18.0 } else { 0.0 };
        let grid = Rect::new(
            content.x0 + pad,
            content.y0 + 64.0,
            content.x1 - pad - detail_w - detail_gap,
            content.y1 - pad,
        );

        // Grid layout. Shared reflow math (one canonical formula) so this roll
        // aligns exactly like every other card grid in the OS.
        let target_w = 196.0;
        let gap = 14.0;
        let cols = ui::widget::card_columns(grid.width(), target_w, gap);
        let cell_w = ui::widget::card_column_width(grid.width(), cols, gap);
        let card_h = (cell_w * 0.76).clamp(120.0, 168.0);

        // Vertical scroll over the full roll — clamped to the content extent
        // so the last row is always reachable.
        let rows = filtered.len().div_ceil(cols);
        let content_h = rows as f64 * card_h + rows.saturating_sub(1) as f64 * gap;
        let max_scroll = (content_h - grid.height()).max(0.0);
        self.gallery_scroll = self.gallery_scroll.clamp(0.0, max_scroll);
        let off = self.gallery_scroll;

        p::push_clip_rect(scene, t, grid);
        for (i, cap) in filtered.iter().enumerate() {
            let col = i % cols;
            let row = i / cols;
            let x0 = grid.x0 + col as f64 * (cell_w + gap);
            let y0 = grid.y0 + row as f64 * (card_h + gap) - off;
            // Cull rows scrolled out of view.
            if y0 + card_h < grid.y0 || y0 > grid.y1 {
                continue;
            }
            let card = Rect::new(x0, y0, x0 + cell_w, y0 + card_h);
            let selected = sel_id.as_deref() == Some(cap.id.as_str()) && wide;
            self.draw_gallery_card(scene, t, tr, theme, card, cap, now, selected);
        }
        p::pop(scene);
        if max_scroll > 0.0 {
            self.draw_scrollbar(
                scene,
                t,
                theme,
                grid,
                off,
                max_scroll,
                grid.height(),
                content_h,
            );
        }

        // Detail — side panel (wide) or full overlay (narrow).
        if let Some(id) = sel_id
            && let Some(cap) = filtered.iter().find(|c| c.id == id).cloned()
        {
            if wide {
                let panel = Rect::new(
                    grid.x1 + detail_gap,
                    content.y0 + 64.0,
                    content.x1 - pad,
                    content.y1 - pad,
                );
                self.draw_gallery_detail(scene, t, tr, theme, panel, &cap, now, false);
            } else {
                let panel = Rect::new(
                    content.x0 + pad,
                    content.y0 + 64.0,
                    content.x1 - pad,
                    content.y1 - pad,
                );
                // Scrim behind the overlay — tapping it dismisses.
                p::fill_rect(scene, t, content, p::alpha(Color::from_rgb8(0, 0, 0), 0.4));
                self.push_region(content, Action::CloseDetail);
                self.draw_gallery_detail(scene, t, tr, theme, panel, &cap, now, true);
            }
        }
    }

    fn draw_gallery_card(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        cap: &Capture,
        now: i64,
        selected: bool,
    ) {
        let c = &theme.tokens.colors;
        self.ensure_thumb(cap);
        p::push_clip_round(scene, t, card, 14.0);
        if let Some(img) = self.thumbs.get_mut(&cap.id) {
            img.paint_at(scene, t, card);
        }
        let strip = Rect::new(card.x0, card.y1 - 44.0, card.x1, card.y1);
        p::fill_rect(scene, t, strip, p::alpha(Color::from_rgb8(0, 0, 0), 0.45));
        let label = ellipsize(tr, &cap.label, 12.5, card.width() - 20.0);
        p::text(
            scene,
            tr,
            card.x0 + 10.0,
            card.y1 - 34.0,
            &label,
            12.5,
            white(),
        );
        p::text(
            scene,
            tr,
            card.x0 + 10.0,
            card.y1 - 18.0,
            &ago(cap.ts, now),
            10.5,
            p::alpha(white(), 0.8),
        );
        p::pop(scene);

        if cap.is_video() {
            let pl = Rect::new(card.x0 + 8.0, card.y0 + 8.0, card.x0 + 30.0, card.y0 + 30.0);
            p::circle(
                scene,
                t,
                pl.center().x,
                pl.center().y,
                11.0,
                p::alpha(Color::from_rgb8(0, 0, 0), 0.5),
            );
            p::icon(scene, t, "play", shrink(pl, 5.0), white(), 1.5);
            if cap.dur_ms > 0 {
                let dur = fmt_dur(cap.dur_ms);
                let dw = f64::from(p::text_w(tr, &dur, 11.0)) + 14.0;
                let dr = Rect::new(
                    card.x1 - dw - 8.0,
                    card.y0 + 8.0,
                    card.x1 - 8.0,
                    card.y0 + 30.0,
                );
                p::fill_round(scene, t, dr, 8.0, p::alpha(Color::from_rgb8(0, 0, 0), 0.5));
                p::text_in(scene, tr, dr, &dur, 11.0, white());
            }
        } else if cap.favorite {
            let st = Rect::new(card.x1 - 30.0, card.y0 + 8.0, card.x1 - 8.0, card.y0 + 30.0);
            p::icon(
                scene,
                t,
                "star",
                st,
                theme.tokens.colors.semantic.warning,
                1.5,
            );
        }

        let ring = if selected {
            theme.accent_primary
        } else {
            p::alpha(c.border_standard, 0.0)
        };
        p::stroke_round(scene, t, card, 14.0, ring, 2.5);
        self.push_region(card, Action::SelectCapture(cap.id.clone()));
    }

    fn draw_gallery_detail(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        cap: &Capture,
        now: i64,
        overlay: bool,
    ) {
        let c = &theme.tokens.colors;
        p::fill_round(scene, t, panel, 18.0, c.surfaces.raised);
        p::stroke_round(scene, t, panel, 18.0, p::alpha(c.border_standard, 0.5), 1.0);

        // Close button on the overlay variant.
        if overlay {
            let xr = Rect::new(
                panel.x1 - 40.0,
                panel.y0 + 14.0,
                panel.x1 - 16.0,
                panel.y0 + 38.0,
            );
            p::fill_round(scene, t, xr, 12.0, p::alpha(c.neutral[2], 0.9));
            p::icon(scene, t, "close", shrink(xr, 6.0), c.text_primary, 1.8);
            self.push_region(xr, Action::CloseDetail);
        }

        let preview_h = if overlay {
            (panel.height() * 0.42).clamp(150.0, 320.0)
        } else {
            188.0
        };
        let preview = Rect::new(
            panel.x0 + 16.0,
            panel.y0 + 16.0,
            panel.x1 - 16.0,
            panel.y0 + 16.0 + preview_h,
        );
        self.ensure_thumb(cap);
        p::push_clip_round(scene, t, preview, 14.0);
        if let Some(img) = self.thumbs.get_mut(&cap.id) {
            img.paint_at(scene, t, preview);
        }
        if cap.is_video() {
            p::circle(
                scene,
                t,
                preview.center().x,
                preview.center().y,
                22.0,
                p::alpha(Color::from_rgb8(0, 0, 0), 0.45),
            );
            p::icon(
                scene,
                t,
                "play",
                Rect::new(
                    preview.center().x - 11.0,
                    preview.center().y - 11.0,
                    preview.center().x + 11.0,
                    preview.center().y + 11.0,
                ),
                white(),
                2.0,
            );
        }
        p::pop(scene);

        let mut y = preview.y1 + 16.0;
        let title = ellipsize(tr, &cap.label, 17.0, panel.width() - 36.0);
        p::text(scene, tr, panel.x0 + 18.0, y, &title, 17.0, c.text_primary);
        y += 24.0;
        p::text(
            scene,
            tr,
            panel.x0 + 18.0,
            y,
            &ago(cap.ts, now),
            12.0,
            c.text_dim,
        );
        y += 28.0;

        let mut rows = vec![(
            "Type",
            if cap.is_video() {
                "Video".to_string()
            } else {
                "Photo".to_string()
            },
        )];
        if cap.w > 0 && cap.h > 0 {
            rows.push(("Resolution", format!("{} × {}", cap.w, cap.h)));
        }
        rows.push(("Format", format_label(cap)));
        if let Some(size) = cap
            .path
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
        {
            rows.push(("Size", human_size(size)));
        } else if cap.w > 0 && cap.h > 0 {
            rows.push((
                "Size",
                format!(
                    "{:.1} MB",
                    (f64::from(cap.w) * f64::from(cap.h) * 1.5) / 1_000_000.0
                ),
            ));
        }
        for (k, v) in rows {
            p::text(scene, tr, panel.x0 + 18.0, y, k, 12.5, c.text_dim);
            p::text_right(scene, tr, panel.x1 - 18.0, y, &v, 12.5, c.text_primary);
            y += 26.0;
        }

        // Action buttons.
        let by = panel.y1 - 52.0;
        let bw = (panel.width() - 36.0 - 20.0) / 3.0;
        let labels = [
            ("star", "Favorite", Action::ToggleFavSelected),
            ("share", "Share", Action::ShareSelected),
            ("trash", "Delete", Action::DeleteSelected),
        ];
        for (i, (icon, label, action)) in labels.into_iter().enumerate() {
            let x0 = panel.x0 + 18.0 + i as f64 * (bw + 10.0);
            let br = Rect::new(x0, by, x0 + bw, by + 38.0);
            let danger = icon == "trash";
            let on = icon == "star" && cap.favorite;
            let col = if danger {
                theme.tokens.colors.semantic.danger
            } else if on {
                theme.tokens.colors.semantic.warning
            } else {
                c.text_primary
            };
            p::fill_round(scene, t, br, 12.0, p::alpha(c.neutral[2], 0.9));
            let lw = f64::from(p::text_w(tr, label, 11.5));
            let group_w = 16.0 + 6.0 + lw;
            let ix = br.center().x - group_w / 2.0;
            p::icon(
                scene,
                t,
                icon,
                Rect::new(ix, br.center().y - 8.0, ix + 16.0, br.center().y + 8.0),
                col,
                1.8,
            );
            p::text(scene, tr, ix + 22.0, br.center().y - 7.0, label, 11.5, col);
            self.push_region(br, action);
        }
    }
}

fn format_label(cap: &Capture) -> String {
    if let Some(ext) = cap
        .path
        .as_ref()
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
    {
        return ext.to_uppercase();
    }
    if cap.is_video() {
        "MP4".into()
    } else {
        "PNG".into()
    }
}

fn human_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1_000_000.0 {
        format!("{:.1} MB", b / 1_000_000.0)
    } else if b >= 1_000.0 {
        format!("{:.0} KB", b / 1_000.0)
    } else {
        format!("{bytes} B")
    }
}

// ====================================================================
// Settings screen
// ====================================================================

impl CameraApp {
    fn draw_settings(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
    ) {
        let c = &theme.tokens.colors;

        // Camera settings are an overlay sheet, not a full desktop pane. A
        // dim scrim backs it; tapping the scrim dismisses back to the
        // viewfinder.
        p::fill_rect(scene, t, full, p::alpha(Color::from_rgb8(0, 0, 0), 0.45));
        self.push_region(full, Action::Back);

        // Centred sheet — wide enough that grouped rows use the full width and
        // stay aligned, capped so it reads as a sheet rather than a stranded
        // narrow column in an empty window.
        let total = Self::settings_total_height();
        let sw = 620.0_f64.min(full.width() - 96.0);
        let sh = (total + 24.0).min(full.height() - 72.0);
        let sx = full.center().x - sw / 2.0;
        let sy = full.center().y - sh / 2.0;
        let sheet = Rect::new(sx, sy, sx + sw, sy + sh);
        // Base-surface sheet so the raised grouped cards read distinctly on it.
        p::fill_round(scene, t, sheet, 22.0, c.surfaces.base);
        p::stroke_round(scene, t, sheet, 22.0, p::alpha(c.border_standard, 0.6), 1.0);
        // Absorb taps on the sheet's own padding so only the scrim dismisses.
        self.push_region(sheet, Action::Nav(Screen::Settings));

        let inner = 22.0;
        let x0 = sheet.x0 + inner;
        let colw = sheet.width() - inner * 2.0;
        let top_gap = 22.0;

        // Full painted height stays reachable via scroll on short sheets.
        let max_scroll = (total - sh).max(0.0);
        self.settings_scroll = self.settings_scroll.clamp(0.0, max_scroll);
        let off = self.settings_scroll;

        p::push_clip_round(scene, t, sheet, 22.0);
        let mut y = sheet.y0 + top_gap - off;
        p::text(scene, tr, x0, y, "Settings", 22.0, c.text_primary);
        y += 44.0;

        // Capture group.
        y = self.group_title(scene, tr, theme, x0, y, "Capture");
        let cap_rows = 4;
        let cap_rect = Rect::new(x0, y, x0 + colw, y + f64::from(cap_rows) * 52.0);
        p::fill_round(scene, t, cap_rect, 14.0, c.surfaces.raised);
        p::stroke_round(
            scene,
            t,
            cap_rect,
            14.0,
            p::alpha(c.border_standard, 0.5),
            1.0,
        );
        let mut ry = y;
        ry = self.value_row(
            scene,
            t,
            tr,
            theme,
            x0,
            ry,
            colw,
            "Resolution",
            self.settings.resolution.label(),
            Action::CycleResolution,
        );
        self.row_sep(scene, t, theme, x0, ry, colw);
        ry = self.value_row(
            scene,
            t,
            tr,
            theme,
            x0,
            ry,
            colw,
            "Photo format",
            self.settings.photo_format.label(),
            Action::CycleFormat,
        );
        self.row_sep(scene, t, theme, x0, ry, colw);
        ry = self.toggle_row(
            scene,
            t,
            tr,
            theme,
            x0,
            ry,
            colw,
            "HDR",
            self.settings.hdr,
            Action::ToggleHdr,
        );
        self.row_sep(scene, t, theme, x0, ry, colw);
        ry = self.timer_row(scene, t, tr, theme, x0, ry, colw);
        y = ry + 20.0;

        // Composition group.
        y = self.group_title(scene, tr, theme, x0, y, "Composition");
        let comp_rect = Rect::new(x0, y, x0 + colw, y + 3.0 * 52.0);
        p::fill_round(scene, t, comp_rect, 14.0, c.surfaces.raised);
        p::stroke_round(
            scene,
            t,
            comp_rect,
            14.0,
            p::alpha(c.border_standard, 0.5),
            1.0,
        );
        ry = y;
        ry = self.toggle_row(
            scene,
            t,
            tr,
            theme,
            x0,
            ry,
            colw,
            "Grid (rule of thirds)",
            self.settings.grid,
            Action::ToggleGrid,
        );
        self.row_sep(scene, t, theme, x0, ry, colw);
        ry = self.toggle_row(
            scene,
            t,
            tr,
            theme,
            x0,
            ry,
            colw,
            "Level guide",
            self.settings.level,
            Action::ToggleLevel,
        );
        self.row_sep(scene, t, theme, x0, ry, colw);
        ry = self.toggle_row(
            scene,
            t,
            tr,
            theme,
            x0,
            ry,
            colw,
            "Mirror front camera",
            self.settings.mirror_front,
            Action::ToggleMirror,
        );
        y = ry + 20.0;

        y = self.group_title(scene, tr, theme, x0, y, "Privacy & camera");
        let pc_rect = Rect::new(x0, y, x0 + colw, y + 2.0 * 52.0);
        p::fill_round(scene, t, pc_rect, 14.0, c.surfaces.raised);
        p::stroke_round(
            scene,
            t,
            pc_rect,
            14.0,
            p::alpha(c.border_standard, 0.5),
            1.0,
        );
        ry = y;
        ry = self.toggle_row(
            scene,
            t,
            tr,
            theme,
            x0,
            ry,
            colw,
            "Location tags",
            self.settings.geotag,
            Action::ToggleGeotag,
        );
        self.row_sep(scene, t, theme, x0, ry, colw);
        let _ = self.value_row(
            scene,
            t,
            tr,
            theme,
            x0,
            ry,
            colw,
            "Default camera",
            self.settings.default_facing.label(),
            Action::CycleDefaultFacing,
        );

        p::pop(scene);

        // Close button, top-right of the sheet — fixed above the scroll.
        let xr = Rect::new(
            sheet.x1 - 44.0,
            sheet.y0 + 14.0,
            sheet.x1 - 16.0,
            sheet.y0 + 42.0,
        );
        p::fill_round(scene, t, xr, 12.0, p::alpha(c.neutral[2], 0.9));
        p::icon(scene, t, "close", shrink(xr, 8.0), c.text_primary, 1.8);
        self.push_region(xr, Action::Back);

        if max_scroll > 0.0 {
            self.draw_scrollbar(scene, t, theme, sheet, off, max_scroll, sh, total);
        }
    }

    /// Full painted height of the settings sheet, independent of window size.
    /// Mirrors the group layout in [`Self::draw_settings`]; the two must stay
    /// in step so scroll always reaches the last row.
    fn settings_total_height() -> f64 {
        let row = 52.0;
        let header = 24.0;
        let group_gap = 20.0;
        let body = 44.0                                 // title → first group
            + header + 4.0 * row + group_gap            // Capture (4 rows)
            + header + 3.0 * row + group_gap            // Composition (3 rows)
            + header + 2.0 * row; // Privacy & camera (2 rows)
        22.0 + body + 24.0 // top gap + body + bottom breathing room
    }

    fn group_title(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        x: f64,
        y: f64,
        label: &str,
    ) -> f64 {
        p::text(
            scene,
            tr,
            x + 4.0,
            y,
            &label.to_uppercase(),
            11.0,
            p::alpha(theme.tokens.colors.text_dim, 0.9),
        );
        y + 24.0
    }

    fn row_sep(&self, scene: &mut Scene, t: Affine, theme: &Theme, x: f64, y: f64, w: f64) {
        p::line(
            scene,
            t,
            x + 16.0,
            y,
            x + w - 16.0,
            y,
            p::alpha(theme.tokens.colors.border_standard, 0.35),
            1.0,
        );
    }

    fn toggle_row(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        x: f64,
        y: f64,
        w: f64,
        label: &str,
        on: bool,
        action: Action,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let row = Rect::new(x, y, x + w, y + 52.0);
        p::text(
            scene,
            tr,
            x + 18.0,
            row.center().y - 8.0,
            label,
            14.0,
            c.text_primary,
        );
        let sw = Rect::new(
            row.x1 - 18.0 - 46.0,
            row.center().y - 13.0,
            row.x1 - 18.0,
            row.center().y + 13.0,
        );
        self.toggle_switch(scene, t, theme, sw, on, action);
        y + 52.0
    }

    fn value_row(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        x: f64,
        y: f64,
        w: f64,
        label: &str,
        value: &str,
        action: Action,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let row = Rect::new(x, y, x + w, y + 52.0);
        p::text(
            scene,
            tr,
            x + 18.0,
            row.center().y - 8.0,
            label,
            14.0,
            c.text_primary,
        );
        let chev = Rect::new(
            row.x1 - 30.0,
            row.center().y - 9.0,
            row.x1 - 16.0,
            row.center().y + 9.0,
        );
        p::icon(scene, t, "chevron", chev, p::alpha(c.text_dim, 0.9), 1.8);
        p::text_right(
            scene,
            tr,
            chev.x0 - 8.0,
            row.center().y - 8.0,
            value,
            13.5,
            theme.accent_primary,
        );
        self.push_region(row, action);
        y + 52.0
    }

    fn timer_row(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        x: f64,
        y: f64,
        w: f64,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let row = Rect::new(x, y, x + w, y + 52.0);
        p::text(
            scene,
            tr,
            x + 18.0,
            row.center().y - 8.0,
            "Self-timer",
            14.0,
            c.text_primary,
        );
        let seg = Rect::new(
            row.x1 - 18.0 - 168.0,
            row.center().y - 15.0,
            row.x1 - 18.0,
            row.center().y + 15.0,
        );
        let items: Vec<(String, bool, Action)> = Timer::ALL
            .iter()
            .map(|tm| {
                (
                    tm.label().to_string(),
                    *tm == self.settings.timer,
                    Action::SetTimer(*tm),
                )
            })
            .collect();
        self.segmented(scene, t, tr, theme, seg, items);
        y + 52.0
    }
}

/// Truncate `s` with an ellipsis so it fits in `max_w` pixels at `size`.
fn ellipsize(tr: &mut TextRenderer, s: &str, size: f32, max_w: f64) -> String {
    if f64::from(p::text_w(tr, s, size)) <= max_w {
        return s.to_string();
    }
    let mut out = String::new();
    for ch in s.chars() {
        let trial = format!("{out}{ch}…");
        if f64::from(p::text_w(tr, &trial, size)) > max_w {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

/// Test-only accessors that let sibling modules (`crate::actions` tests) drive
/// the *real* affordance path and observe its result, without exposing internal
/// fields to the rest of the crate. Each helper routes through the same
/// production code an on-screen control does.
#[cfg(test)]
impl CameraApp {
    /// Select a gallery shot by id, exactly as clicking its card does.
    pub(crate) fn test_select(&mut self, id: &str) {
        self.apply(
            Action::SelectCapture(id.to_string()),
            &mut AutoCommit::new(),
            &automerge::ROOT,
        );
    }

    /// Fire the favorite affordance on the current selection through the real
    /// handler (which dispatches the shared action).
    pub(crate) fn test_toggle_favorite(&mut self, doc: &mut AutoCommit) {
        self.apply(Action::ToggleFavSelected, doc, &automerge::ROOT);
    }

    /// The `EncodedAction` the most recent affordance dispatched.
    pub(crate) fn test_last_dispatched(&self) -> Option<EncodedAction> {
        self.actions.last_dispatched().cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_app() -> (CameraApp, AutoCommit, ObjId) {
        let mut app = CameraApp::sample();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        (app, doc, root)
    }

    #[test]
    fn detected_label_has_single_space() {
        let tag = detected_label("QR Code");
        assert_eq!(tag, "QR Code detected");
        assert!(
            !tag.contains("  "),
            "detected label must not contain a double space: {tag:?}"
        );
    }

    #[test]
    fn sample_source_populates_without_touching_disk() {
        let (app, _doc, _root) = sample_app();
        assert_eq!(app.screen, Screen::Camera);
        assert_eq!(app.captures.len(), 9);
        assert!(app.captures.iter().all(|c| c.path.is_none()));
    }

    #[test]
    fn disk_source_starts_empty_and_seeds_no_content() {
        // Point at a directory that does not exist → an empty library, never
        // fabricated samples.
        let dir = std::env::temp_dir().join("camera-empty-lib-xyz-does-not-exist");
        let mut app = CameraApp::with(Storage::Disk(dir), Screen::Gallery);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        assert!(app.captures.is_empty());
        assert!(app.scans.is_empty());
    }

    #[test]
    fn nav_action_switches_screen_in_doc() {
        let (mut app, mut doc, root) = sample_app();
        app.apply(Action::Nav(Screen::Gallery), &mut doc, &root);
        assert_eq!(app.screen, Screen::Gallery);
        assert_eq!(doc::read_screen(&doc, &root), Screen::Gallery);
    }

    #[test]
    fn sample_shutter_appends_capture() {
        let (mut app, mut doc, root) = sample_app();
        let before = app.captures.len();
        app.apply(Action::Shutter, &mut doc, &root);
        assert_eq!(app.captures.len(), before + 1);
        assert!(app.toast.is_some());
    }

    #[test]
    fn disk_shutter_writes_a_real_jpeg_via_handle_event() {
        use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
        use winit::keyboard::{Key, NamedKey, PhysicalKey};

        let dir = std::env::temp_dir().join(format!("camera-shutter-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut app = CameraApp::with(Storage::Disk(dir.clone()), Screen::Camera);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        assert_eq!(app.screen, Screen::Camera);
        assert_eq!(
            app.settings.photo_format,
            PhotoFormat::Jpeg,
            "default format is JPEG"
        );
        // Simulate a camera frame arriving.
        app.push_camera_frame(frame::viewfinder(1, 64, 48));

        // Press Space through the real event path. `KeyEvent` carries a
        // non-exhaustive field, so build it via the zeroed-struct idiom used
        // across the toolkit's own tests.
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(NamedKey::Space);
        ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        let event = WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        };
        let mods: Modifiers = Default::default();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            &event,
            1.0,
            &mods,
            &mut doc,
            &root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        assert!(handled, "space should be handled on the camera screen");

        // A real JPEG landed on disk and a rescan finds it.
        let files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "jpg"))
            .collect();
        assert_eq!(files.len(), 1, "shutter wrote exactly one JPEG");
        let bytes = std::fs::read(files[0].path()).unwrap();
        assert_eq!(&bytes[0..2], &[0xFF, 0xD8], "genuine JPEG (SOI marker)");
        app.sync(&mut doc, &root);
        assert_eq!(app.captures.len(), 1);
        assert!(app.captures[0].path.is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn disk_shutter_without_frame_is_honest() {
        let dir = std::env::temp_dir().join(format!("camera-noframe-test-{}", std::process::id()));
        let mut app = CameraApp::with(Storage::Disk(dir.clone()), Screen::Camera);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        assert!(!app.feed_active());
        app.apply(Action::Shutter, &mut doc, &root);
        // Nothing saved; the toast tells the truth.
        assert!(app.captures.is_empty());
        assert_eq!(app.toast.as_deref(), Some("Connect a camera to capture"));
        assert!(
            !dir.exists(),
            "no directory created when there is nothing to save"
        );
    }

    #[test]
    fn set_mode_persists_and_locks_while_recording() {
        let (mut app, mut doc, root) = sample_app();
        assert_eq!(app.mode, CaptureMode::Photo);
        app.apply(Action::SetMode(CaptureMode::Video), &mut doc, &root);
        assert_eq!(app.mode, CaptureMode::Video);
        assert_eq!(doc::read_mode(&doc, &root), CaptureMode::Video);

        // Shutter in video mode starts recording; mode can't be flipped while
        // a clip is in progress.
        app.apply(Action::Shutter, &mut doc, &root);
        assert!(app.is_recording());
        app.apply(Action::SetMode(CaptureMode::Photo), &mut doc, &root);
        assert_eq!(app.mode, CaptureMode::Video, "mode locked while recording");
    }

    #[test]
    fn sample_video_record_round_trips_into_gallery() {
        let (mut app, mut doc, root) = sample_app();
        app.apply(Action::SetMode(CaptureMode::Video), &mut doc, &root);
        let before = app.captures.len();

        // Start; the hermetic frame-synth runs on each sync tick.
        app.apply(Action::Shutter, &mut doc, &root);
        assert!(app.is_recording());
        for _ in 0..6 {
            app.sync(&mut doc, &root);
        }
        assert!(app.recording.as_ref().unwrap().rec.frame_count() >= 6);

        // Stop → a real clip length lands as a new gallery video.
        app.apply(Action::Shutter, &mut doc, &root);
        assert!(!app.is_recording());
        assert_eq!(app.captures.len(), before + 1);
        assert!(app.captures[0].is_video());
        assert!(app.captures[0].dur_ms > 0, "clip has a real duration");
        assert!(
            app.toast
                .as_deref()
                .unwrap_or("")
                .starts_with("Video saved")
        );
    }

    #[test]
    fn disk_video_recording_writes_a_real_avi() {
        let dir = std::env::temp_dir().join(format!("camera-vidrec-test-{}", std::process::id()));
        let mut app = CameraApp::with(Storage::Disk(dir.clone()), Screen::Camera);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        app.apply(Action::SetMode(CaptureMode::Video), &mut doc, &root);

        // A frame arrives, then record: start seeds with it, further frames are
        // fed straight into the encoder.
        app.push_camera_frame(frame::viewfinder(0, 128, 96));
        assert!(app.feed_active());
        app.apply(Action::Shutter, &mut doc, &root); // start
        assert!(app.is_recording());
        for i in 1..6u8 {
            app.push_camera_frame(frame::viewfinder(i % 6, 128, 96));
        }
        app.apply(Action::Shutter, &mut doc, &root); // stop → write clip
        assert!(!app.is_recording());

        // Exactly one real AVI on disk; it's a RIFF/AVI container that probes.
        let avis: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "avi"))
            .collect();
        assert_eq!(avis.len(), 1, "recording wrote exactly one AVI");
        let bytes = std::fs::read(avis[0].path()).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"AVI ");
        let (w, h, dur) = crate::video::probe_avi(&avis[0].path()).expect("probe recorded clip");
        assert_eq!((w, h), (128, 96));
        assert!(dur > 0);

        // The gallery rescans it as a video with dimensions + duration.
        app.sync(&mut doc, &root);
        assert_eq!(app.captures.len(), 1);
        assert!(app.captures[0].is_video());
        assert_eq!((app.captures[0].w, app.captures[0].h), (128, 96));
        assert!(app.captures[0].dur_ms > 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn disk_video_record_without_feed_is_honest() {
        let dir =
            std::env::temp_dir().join(format!("camera-vidrec-noframe-{}", std::process::id()));
        let mut app = CameraApp::with(Storage::Disk(dir.clone()), Screen::Camera);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        app.apply(Action::SetMode(CaptureMode::Video), &mut doc, &root);
        assert!(!app.feed_active());
        app.apply(Action::Shutter, &mut doc, &root);
        assert!(!app.is_recording(), "no feed → recording never starts");
        assert_eq!(app.toast.as_deref(), Some("Connect a camera to record"));
        assert!(!dir.exists());
    }

    #[test]
    fn cycle_facing_and_persist() {
        let (mut app, mut doc, root) = sample_app();
        assert_eq!(app.facing, Facing::Back);
        app.apply(Action::CycleFacing, &mut doc, &root);
        assert_eq!(app.facing, Facing::Front);
        assert_eq!(doc::read_facing(&doc, &root), Facing::Front);
    }

    #[test]
    fn toggle_grid_persists_to_settings() {
        let (mut app, mut doc, root) = sample_app();
        let before = app.settings.grid;
        app.apply(Action::ToggleGrid, &mut doc, &root);
        assert_eq!(app.settings.grid, !before);
        assert_eq!(doc::read_settings(&doc, &root).grid, !before);
    }

    #[test]
    fn scan_action_records_history() {
        let mut app = CameraApp::sample();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root); // decodes the sample poster → scanned set
        // Force the scanner to decode by rendering once would need a GPU; the
        // sample scanner frame is produced lazily on draw, so seed a scanned
        // code directly to exercise the action path.
        app.scanned = Some(ScannedCode {
            raw: SCAN_PAYLOAD.to_string(),
            kind: classify(SCAN_PAYLOAD),
            symbology: "QR Code".into(),
            corners: [(0.0, 0.0); 4],
        });
        app.apply(Action::ScanAction(0), &mut doc, &root);
        let scans = doc::read_scans(&doc, &root);
        assert_eq!(scans.len(), 1);
        assert_eq!(scans[0].raw, SCAN_PAYLOAD);
        assert_eq!(app.scans.len(), 1);
    }

    #[test]
    fn wifi_scan_parses_and_primary_action_copies_password() {
        // The Wi-Fi payload is parsed into its fields and the primary action
        // ("Connect") copies the password — the parse+copy path, end to end.
        let payload = "WIFI:T:WPA2;S:qOS Lounge;P:welcome-aboard;H:false;;";
        let kind = classify(payload);
        match &kind {
            CodeKind::Wifi {
                ssid,
                auth,
                password,
                hidden,
            } => {
                assert_eq!(ssid, "qOS Lounge");
                assert_eq!(auth, "WPA2");
                assert_eq!(password, "welcome-aboard");
                assert!(!hidden);
                assert_eq!(kind.actions()[0], "Connect");
            }
            other => panic!("expected wifi, got {other:?}"),
        }
        let mut app = CameraApp::sample();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        app.scanned = Some(ScannedCode {
            raw: payload.to_string(),
            kind,
            symbology: "QR Code".into(),
            corners: [(0.0, 0.0); 4],
        });
        app.apply(Action::ScanAction(0), &mut doc, &root); // "Connect"
        assert_eq!(app.toast.as_deref(), Some("Wi-Fi password copied"));
    }

    #[test]
    fn resolution_cycles() {
        let (mut app, mut doc, root) = sample_app();
        assert_eq!(app.settings.resolution, Resolution::R12MP);
        app.apply(Action::CycleResolution, &mut doc, &root);
        assert_eq!(app.settings.resolution, Resolution::R4K);
    }

    /// Run the full five-pass frame (sync → layout → draw) for `app` at a
    /// given viewport, on the caller's shared doc. CPU-only; no GPU needed
    /// (rasterization is skipped — we only exercise layout + scene build).
    fn render_at(app: &mut CameraApp, doc: &mut AutoCommit, root: &ObjId, w: f32, h: f32) {
        let theme = Theme::dark();
        let presence = PresenceCtx::empty();
        let perms = ui::permissions::PermissionContext::permissive();
        let mut tr = TextRenderer::new();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        app.sync(doc, root);
        let child = app.build_layout(&mut taffy, &theme, &mut node_map);
        let rootn = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(w),
                        height: Dimension::length(h),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        {
            let mm = node_map.clone();
            let trr = &mut tr;
            taffy
                .compute_layout_with_measure(
                    rootn,
                    taffy::Size {
                        width: taffy::AvailableSpace::Definite(w),
                        height: taffy::AvailableSpace::Definite(h),
                    },
                    |known, avail, id, _, _| {
                        if let Some(&p) = mm.get(&id) {
                            unsafe { (*p).measure(known, avail, trr) }
                        } else {
                            taffy::Size::ZERO
                        }
                    },
                )
                .unwrap();
        }
        app.apply_layout(&taffy, child, Point::ORIGIN);
        let mut scene = Scene::new();
        app.draw_with_presence(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &presence,
            &perms,
        );
    }

    #[test]
    fn resize_reflows_the_same_instance() {
        // The breakpoint must be re-derived from the current width every frame:
        // one instance rendered wide THEN narrow must reflow, not stay frozen
        // at the first frame's layout.
        let mut app = CameraApp::with(Storage::Sample, Screen::Scanner);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;

        render_at(&mut app, &mut doc, &root, 1280.0, 720.0);
        assert_eq!(app.screen, Screen::Scanner);
        let wide_w = app.scan_view.as_ref().expect("wide viewfinder").0;
        assert!(app.rect.width() >= 1279.0);

        render_at(&mut app, &mut doc, &root, 560.0, 720.0);
        let narrow_w = app.scan_view.as_ref().expect("narrow viewfinder").0;
        assert!(app.rect.width() <= 561.0, "rect tracked the resize");

        // The viewfinder image was rebuilt for the narrower width — proof the
        // layout followed the resize rather than caching the first frame.
        assert!(
            narrow_w < wide_w,
            "viewfinder reflowed {wide_w} -> {narrow_w}"
        );
    }

    #[test]
    fn lens_label_never_reads_as_navigation() {
        // The lens selector caption must not be the word "Back".
        assert_eq!(Facing::Back.label(), "Rear");
        assert_ne!(Facing::Back.label(), "Back");
    }

    fn wheel(dy: f32) -> WindowEvent {
        WindowEvent::MouseWheel {
            device_id: unsafe { std::mem::zeroed() },
            delta: winit::event::MouseScrollDelta::LineDelta(0.0, dy),
            phase: winit::event::TouchPhase::Moved,
        }
    }

    fn drive(app: &mut CameraApp, ev: &WindowEvent, doc: &mut AutoCommit, root: &ObjId) -> bool {
        let mods: winit::event::Modifiers = Default::default();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            ev,
            1.0,
            &mods,
            doc,
            root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        handled
    }

    #[test]
    fn wheel_scrolls_gallery_and_clamps_at_top() {
        let (mut app, mut doc, root) = sample_app();
        app.apply(Action::Nav(Screen::Gallery), &mut doc, &root);
        assert_eq!(app.gallery_scroll, 0.0);
        // Wheel down moves the roll; wheel far up clamps back to the top.
        assert!(drive(&mut app, &wheel(-3.0), &mut doc, &root));
        assert!(app.gallery_scroll > 0.0);
        assert!(drive(&mut app, &wheel(100.0), &mut doc, &root));
        assert_eq!(app.gallery_scroll, 0.0);
    }

    #[test]
    fn wheel_scrolls_settings() {
        let (mut app, mut doc, root) = sample_app();
        app.apply(Action::Nav(Screen::Settings), &mut doc, &root);
        assert!(drive(&mut app, &wheel(-2.0), &mut doc, &root));
        assert!(app.settings_scroll > 0.0);
    }

    #[test]
    fn settings_sheet_needs_scroll_on_short_windows() {
        // On a short window the whole sheet must remain reachable: the max
        // scroll equals how far the content overflows the viewport.
        let total = CameraApp::settings_total_height();
        assert!(
            (total - 670.0).abs() < 1.0,
            "settings height ~670, got {total}"
        );
        let short = 560.0;
        assert!(total - short > 100.0, "last rows are below a 560px fold");
    }

    #[test]
    fn scan_library_item_decodes_a_real_qr_from_disk() {
        // Rasterize a real QR to a PNG on disk, then prove the scanner decodes
        // it through the production code path — no camera involved.
        let payload = "https://qos.example/scanned-from-a-saved-photo";
        let m = crate::scan::encode(payload).expect("encode");
        let (scale, quiet) = (8usize, 4usize);
        let dim = (m.size + quiet * 2) * scale;
        let mut rgba = vec![255u8; dim * dim * 4];
        for my in 0..m.size {
            for mx in 0..m.size {
                if !m.is_dark(mx, my) {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        let px = (mx + quiet) * scale + dx;
                        let py = (my + quiet) * scale + dy;
                        let i = (py * dim + px) * 4;
                        rgba[i] = 17;
                        rgba[i + 1] = 17;
                        rgba[i + 2] = 17;
                    }
                }
            }
        }
        let fb = FrameBuf {
            rgba,
            w: dim as u32,
            h: dim as u32,
        };

        let dir = std::env::temp_dir().join(format!("camera-scan-lib-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Lossless PNG so the QR survives the write for a clean decode.
        library::write_photo(&dir, &fb, PhotoFormat::Png).unwrap();

        let mut app = CameraApp::with(Storage::Disk(dir.clone()), Screen::Scanner);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        assert!(!app.feed_active(), "no camera in the test env");
        assert_eq!(app.captures.len(), 1, "the QR photo is in the library");

        let id = app.captures[0].id.clone();
        app.scan_library_item(&id, &mut doc, &root);
        let scanned = app.scanned.clone().expect("the saved QR decodes");
        assert_eq!(scanned.raw, payload);
        // The decode is recorded to history like a live scan.
        assert_eq!(app.scans.len(), 1);
        assert_eq!(app.scans[0].raw, payload);
        std::fs::remove_dir_all(&dir).ok();
    }
}
