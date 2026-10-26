//! `Scenario` — a typed action list driven against a `ShellDriver`.
//!
//! Each variant of [`Action`] corresponds to a high-level interaction
//! (click, drag, key, type, snapshot, etc.). The engine replays the
//! list step-by-step; the host driving it is responsible for wiring
//! `Snapshot` to its preferred sink (PNG file, JSON file, no-op).
//!
//! Designed for three callers:
//!   - **offscreen mode** — runs the scenario and emits a PNG per
//!     `Snapshot`. Render output goes via the offscreen renderer.
//!   - **headless mode** — runs the scenario and dumps chrome state
//!     JSON per `Snapshot`. No GPU is touched.
//!   - **window mode** — runs the scenario as a "pre-roll" before
//!     handing control to the live winit event loop.
//!
//! Built-in catalogue is the [`scenarios()`] table at the bottom: name,
//! description, and an action list. `--list-scenarios` and `--scenario NAME`
//! read that table. The apply engine above stays code; the scenes are data.

use std::cell::RefCell;
use std::path::Path;

use vello::kurbo::Point;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use shell::shell::chrome_state::{ChromeState, WsTab};
use shell::shell::quick_settings::QsLevel;
use shell::shell::state::StripId;
use ui::widget::GroupRow;

use super::driver::ShellDriver;

mod file_chooser;
pub mod film;
mod launcher;
mod motion_gallery;
mod motion_review;
mod one_place;
mod window_bar_reactions;
mod window_search;

/// One tab of a window seeded for the window-bar shots.
#[derive(Debug, Clone, Copy)]
pub struct BarTab {
    pub app_id: &'static str,
    /// What the app's client sets as its toplevel title.
    pub title: &'static str,
    pub surface_id: u64,
    /// The slots its app declares for the window's bar, if any.
    pub slots: Option<fn() -> ui::window_toolbar::WindowToolbar>,
}

/// One step in a scenario.
#[derive(Debug, Clone)]
pub enum Action {
    /// Click at raw screen coordinates (cursor-move + press + release).
    ClickAt(Point),
    /// Click on a named target. Names are resolved via
    /// [`ShellDriver::resolve_named_rect`]; if the rect is `Rect::ZERO`
    /// at the time of the click the engine logs a warning and skips
    /// the action.
    ClickNamed(&'static str),
    /// Press + drag + release through `steps` intermediate `CursorMoved`
    /// events. Used for tab reorders, wheel spins, etc.
    Drag {
        from: Point,
        to: Point,
        steps: u32,
    },
    /// Press + drag + release with the middle button. Used to prove that
    /// oversized capture surfaces can be navigated without drawing on them.
    MiddleDrag {
        from: Point,
        to: Point,
        steps: u32,
    },
    /// Synthesise a `WindowEvent::MouseWheel { LineDelta(dx, dy) }`.
    /// The host's wheel handlers gate on `last_cursor`, so the caller
    /// is expected to have dispatched a recent `MoveCursor` to align
    /// the cursor over the target widget first.
    MouseWheel {
        dx: f32,
        dy: f32,
    },
    /// Feed total signed travel from a shell-owned three-finger horizontal
    /// gesture into the active strip.
    StripPan(f64),
    /// Lift/cancel the synthetic three-finger stream and let elastic overpull
    /// settle to the nearest legal strip edge.
    EndStripPan,
    /// Move the cursor onto a named target's centre without pressing.
    /// Sibling of [`Action::ClickNamed`], for the affordances that are
    /// hover-driven — a tab pill opening its control cluster — where a
    /// literal coordinate would go stale the moment the row re-sizes.
    MoveCursorNamed(&'static str),
    /// Show every display's Identify card, named and numbered as a laptop
    /// panel with a desk monitor beside it would be.
    IdentifyDisplays,
    /// Move to a live group's empty tab-bar grab zone. Unlike raw screen
    /// coordinates this remains valid in compact and normal viewport sweeps.
    MoveCursorGroupGrab(usize),
    /// Move to 2 px inside a live group's leading (`leading: true`) or
    /// trailing edge, at mid-height: where a person grabs a window's edge.
    MoveCursorGroupEdge {
        group: usize,
        leading: bool,
    },
    /// Move the cursor `dx` px sideways from where it is, keeping its y, so a
    /// drag can be stepped without baking viewport geometry into a shot.
    MoveCursorBy(f64),
    /// Move an active whole-group drag beyond the named group's midpoint,
    /// resolving the after-slot without baking viewport geometry into a shot.
    MoveCursorGroupReorderTarget(usize),
    /// Synthesise a `CursorMoved` without a press/release. Use with
    /// `MousePress`/`MouseRelease` to compose drags that need mid-
    /// gesture snapshots — useful for the new tab-bar `hover_drop_idx`
    /// indicator which only paints between press and release.
    MoveCursor(Point),
    /// Move the pointer as the compositor does: the host notes it (the
    /// top-edge reveals read the path even over a client), then the move is
    /// dispatched.
    CompositorPointer(Point),
    /// Turn "hide the top bar until the pointer is near it" on or off.
    HideTopBar(bool),
    /// A secondary click on the named target: what a long-press by touch
    /// becomes.
    SecondaryClickNamed(&'static str),
    /// A finger dragged from one point to the other, as raw touch.
    TouchSwipe(Point, Point),
    /// A notification banner arrives: title and body.
    Notify(&'static str, &'static str),
    /// The focused window goes full screen, or leaves it.
    FullScreenFocused(bool),
    /// The key that asks for a fullscreen window's bar (Super) goes down.
    FullscreenBarKey,
    /// Synthesise a left-button `Pressed` at the current cursor.
    /// Caller owns the matching `MouseRelease`.
    MousePress,
    /// Synthesise a left-button `Released` at the current cursor.
    MouseRelease,
    /// Capture `frames` snapshots in a row, each separated by
    /// `gap_frames` of animation tick. Names them `prefix-001`,
    /// `prefix-002`, …. Useful for inspecting every frame of an
    /// animation (workspace switch, drag-out spawn, maximize
    /// transition) without re-driving the scenario for each.
    SnapshotSequence {
        prefix: &'static str,
        frames: u32,
        gap_frames: u32,
    },
    /// Synth a `KeyboardInput { Pressed }` for a logical key.
    Key(Key),
    /// Type a literal string (one `Key::Character` per char, skipping
    /// control). The caller is expected to have opened an input target
    /// (palette, name editor, etc.) first.
    Type(&'static str),
    /// Replace the active modifier set. Subsequent `Key(_)` actions see
    /// these modifiers in their `is_mod` check inside the host. The
    /// scenario engine resets modifiers after each `Snapshot` so chord
    /// state doesn't leak between named outputs.
    Modifiers(ModifiersState),
    /// Step animation `n` frames (~16 ms each).
    Wait(u32),
    /// Step frames until the active workspace's strips stop moving (no
    /// pan glide or snap, no slot or width spring), at most `max` frames.
    ///
    /// The frame clock is real time and a frame's length depends on how
    /// busy the machine is, so a fixed [`Action::Wait`] can end with a
    /// spring a pixel short of its target, or with a just-opened window
    /// still growing and the strip's content width moving under the next
    /// gesture. Reach for this before a capture that reads a resting strip.
    WaitStripSettled(u32),
    /// Step frames until the work a person would see finish a moment
    /// later has finished (a search index building, an icon arriving), as
    /// a scene-set capture does; what is still pending after the bound is
    /// stuck, and the next capture shows it.
    WaitSettled,
    /// Re-run layout without stepping the clock.
    ///
    /// A tab title's shaped width is recorded during `draw` and consumed
    /// by the *next* `apply_layout` — the layout pass holds no text
    /// renderer — so a title's first frame on screen is sized from a
    /// per-character estimate. A capture taken before that second pass
    /// therefore shows an estimate-sized row rather than the settled
    /// one. `Wait` paints but does not lay out again, so this is what a
    /// scenario reaches for before snapshotting a resting row.
    Relayout,
    /// Capture: render to PNG (offscreen mode) or dump state JSON
    /// (headless mode). Window mode treats this as a no-op (you can
    /// see it).
    Snapshot(&'static str),
    /// State-mutate shortcuts that bypass the event layer (handy for
    /// setup steps where dispatching real events would be brittle —
    /// e.g. opening the palette before typing a query).
    OpenPalette,
    /// Mark the capture studio as actively recording, so its persistent Stop
    /// state can be reviewed without starting a filesystem encoder in the
    /// deterministic visual harness.
    CaptureRecordingActive(bool),
    /// Pin the selected-pixels-to-PiP morph for deterministic visual review.
    /// PNG rendering can be slower than the live animation, so wall-clock
    /// snapshot sequences cannot faithfully expose its intermediate frames.
    CapturePipTransition(u16),
    /// End the recording through the same compositor-confirmed save receipt
    /// used after a successful file flush.
    CaptureRecordingFinished,
    CaptureReplayActive(bool),
    CaptureReplayBuffered(u8),
    CaptureReplaySaved,
    /// A screenshot finished: kept as a file, on the clipboard, or both.
    CaptureScreenshotFinished {
        saved: bool,
        copied: bool,
    },
    /// Pin the selected-pixels-to-screenshot-receipt morph for deterministic
    /// visual review under the software renderer.
    CaptureScreenshotTransition(u16),
    /// Pin the receipt-thumbnail-to-editor handoff for visual review.
    ScreenshotMarkupTransition(u16),
    /// Expose or hide real system-audio recording controls in deterministic
    /// visuals without probing the host `PipeWire` session.
    CaptureAudioAvailable(bool),
    /// Run queued capture intents through the production compositor executor.
    /// Opt-in avoids filesystem work in unrelated deterministic scenarios.
    CaptureExecutionEnabled(bool),
    /// The client of `surface_id` commits a new buffer, filled with `rgba`,
    /// at its window's size: a window that changes while the person looks.
    ClientCommit {
        surface_id: u64,
        rgba: [u8; 4],
    },
    /// The active Terminal window's session prints lines `from..=to`.
    TerminalPrints {
        from: u32,
        to: u32,
    },
    /// Seed real collaborative room changes, a second person's among them.
    SeedRoomHistory,
    /// Add two present collaborators and representative shares to the active
    /// workspace through the production capability path.
    SeedSharedWorkspace,
    /// Nobody else in any workspace: the launcher on a fresh device.
    SeedLauncherFresh,
    /// People in two workspaces, a call ringing, something shared with the
    /// person and notes they opened lately: the launcher's busy state.
    SeedLauncherBusy,
    /// Three strips in the shown workspace with people on two of them.
    SeedStripsWithPeople,
    /// Admit a named agent and expose one real foreign surface to its live
    /// session through the compositor-authoritative privacy snapshot.
    SeedAgentScreenExposure {
        surface_id: u64,
    },
    SeedInferredStatus(&'static str),
    /// Fill the active room with enough live participants to exercise the
    /// production People pager without weakening each row's action/intent UI.
    SeedCrowdedWorkspace,
    /// Three people and an agent in the active workspace, through the same
    /// presence the live shell folds: pointers over windows, one idle, one
    /// in a call.
    SeedPresenceRoom,
    /// Nobody else in the active workspace: only you.
    OnlyYou,
    /// A collaborator (by roster id) says `text` beside their pointer;
    /// `sent` once they pressed Enter.
    RemoteCursorChat {
        who: &'static str,
        text: &'static str,
        sent: bool,
    },
    /// A collaborator reacts with an emoji from their pointer.
    RemoteReaction {
        who: &'static str,
        emoji: &'static str,
    },
    /// Open this person's cursor chat with `text` typed.
    OpenCursorChat(&'static str),
    /// Open the app chooser an app asks for, to choose an app for `purpose`.
    OpenAppChooser(&'static str),
    /// Follow the collaborator with this roster id, or stop with `None`.
    FollowPeer(Option<&'static str>),
    /// Open someone's row in the roster.
    ExpandRosterRow(&'static str),
    /// The person's own input ended following.
    StopFollowingByInput,
    /// The collaborator with this roster id waved at this person.
    WaveFrom(&'static str),
    /// Arrive in the workspace on screen: the arrival line, telling what
    /// its people see when `public`.
    Arrive {
        public: bool,
    },
    /// Publish authenticated semantic work owned by one external app so the
    /// compositor-level collaboration fallback can be inspected in pixels.
    SeedForeignAppWork {
        app_id: &'static str,
        action: &'static str,
        resource: &'static str,
    },
    /// Seed the durable half of an invitation: the invited person has room
    /// access, but has not joined the workspace yet. This is intentionally
    /// distinct from presence so the product proof never calls an invite a
    /// live collaborator.
    SeedInvitePending,
    /// Resolve the pending invite into a live collaborator editing Notes.
    /// The driver retains the same identity and capability record rather
    /// than replacing the row with a screenshot-only fake.
    AdmitInvitedCollaborator,
    /// Apply a real forward revert to an earlier room-history entry.
    RevertRoomActivity,
    /// Move the named live collaborator to Files and focus that native app,
    /// proving presence follows the work rather than a shell-only avatar.
    ShowCollaboratorInFiles,
    /// Open a window, the way opening one actually happens: adopt a
    /// toplevel for `app_id` into the chrome's tree through the
    /// production `tab_binding::adopt_toplevel`, so the placement rule
    /// decides where it lands and the strip resync splices it in with
    /// the motion that goes with that.
    ///
    /// `surface_id` only has to be unique within a scenario.
    OpenWindow {
        app_id: &'static str,
        surface_id: u64,
    },
    /// Activate an app whose window has not mapped yet: the production
    /// launch holds its place from this frame, where its window arrives.
    HoldLaunchPlace {
        app_id: &'static str,
        label: &'static str,
    },
    /// The launch behind an [`Action::HoldLaunchPlace`] waits for a room
    /// still being joined, as a launch into one does.
    HoldLaunchForRoom(&'static str),
    /// The room [`Action::HoldLaunchForRoom`] waits for is ready.
    RoomReady,
    /// A peer's window whose app is still being fetched from the
    /// registry here, with no real install behind it.
    HoldInstallPlace {
        app_id: &'static str,
        label: &'static str,
    },
    /// The install [`Action::HoldInstallPlace`] stands for has fetched
    /// `received` of `total` bytes.
    InstallBytes {
        app_id: &'static str,
        received: u64,
        total: u64,
    },
    /// Let real time pass, then step one frame: for states keyed to how
    /// long something has taken, which frame steps alone do not advance.
    WaitWall(u64),
    /// Open a second tab of a **shell-hosted** app — Notes, Editor —
    /// into the active group.
    ///
    /// The sibling of [`Action::OpenWindow`], which adopts a foreign
    /// Wayland toplevel and so can never produce a hosted app. Both go
    /// the same production route: mutate the tree, bump the chrome
    /// revision, and let the strip resync splice the new tab into the
    /// live group — the path that gives a spliced tab its own document.
    AddHostedTab {
        app: &'static str,
        title: &'static str,
    },
    /// Open a **shell-hosted** app — Notes, Editor, Files, Terminal — as
    /// its own window, wherever the placement rule puts it.
    ///
    /// [`Action::AddHostedTab`] appends to the group that is already
    /// there, so a scenario built out of it can only ever end up with one
    /// window on screen. This one goes through
    /// `placement::placement_for` and
    /// `tab_binding::open_hosted_app`, so with a lone window selected the
    /// app lands in a *new group* beside it — which is what puts two
    /// apps, both drawing their real content, side by side in the strip.
    ///
    /// `app` names the app the way the tab pill does: "Notes", "Editor",
    /// "Files", "Terminal", "Browser", "Settings". Anything else becomes
    /// an `AppKind::Other`, which renders as a stub card.
    OpenHostedWindow {
        app: &'static str,
        title: &'static str,
    },
    /// Expose the real native Files/Editor catalogue to palette search.
    EnableNativeAppSearch,
    /// Sample the make-space slide at fixed points along its curve and
    /// capture each as `prefix-<ms>`.
    ///
    /// An offscreen frame costs far more wall time than the 420 ms curve
    /// lasts, so stepping frames only ever catches the start and the end.
    /// This asks the strip for a specific point on the curve instead —
    /// what gets rendered is the production `draw` at that point.
    SampleMakeSpace {
        prefix: &'static str,
        /// Milliseconds into the curve.
        at_ms: &'static [u64],
    },
    /// Sample the maximize/restore sibling fade at fixed points along
    /// `durations.fast` and capture each as `prefix-<ms>`.
    ///
    /// Same escape as [`Action::SampleMakeSpace`]: an offscreen frame
    /// costs more wall time than the 130 ms curve, so stepping frames
    /// only ever catches the endpoints.
    SampleSiblingFade {
        prefix: &'static str,
        /// Milliseconds into the fade.
        at_ms: &'static [u64],
    },
    /// Sample a tab pill's reveal curve at fixed points and capture each
    /// as `prefix-<ms>`.
    ///
    /// An offscreen frame costs more wall time than the curve lasts, so
    /// stepping frames only ever catches its endpoints — the pill is
    /// shut in one capture and wide open in the next. This asks the bar
    /// to hold the curve at a chosen point instead; what gets rendered
    /// is the production `apply_layout` there. Same escape
    /// [`Action::SampleMakeSpace`] gives the make-space slide.
    SampleTabReveal {
        prefix: &'static str,
        /// Milliseconds into the curve.
        at_ms: &'static [u64],
    },
    /// Put the selection on the `idx`-th group of the active strip, or
    /// clear it entirely with `None` — which is the state rule 3 (place
    /// at a seam) is about.
    SelectGroup(Option<usize>),
    /// Close the focused window, as its close button or `tab.close` does.
    CloseFocusedWindow,
    /// Seed the status tray with a named hardware state.
    ///
    /// The live poller is deliberately absent from a `--shots` render —
    /// a tray driven by the rendering machine's own battery would make
    /// every golden image a function of which laptop produced it. That
    /// leaves the interesting states (nearly flat, charging, muted, no
    /// network) unreachable by a screenshot, and those are exactly the
    /// ones whose styling needs looking at. This seeds the same
    /// `ChromeState::status` field the poller writes, so the tray takes
    /// the identical path to the identical pixels.
    ///
    /// Presets rather than a struct literal: the DSL stays readable in
    /// a scenario list, and a state worth capturing is worth naming.
    SeedStatus(&'static str),
    /// Raise the transient hardware-key readout, by preset name.
    ///
    /// The trigger is a media key, and a `--shots` render has no
    /// keyboard and no audio daemon to answer it — so the slab would
    /// never appear in a capture, which is precisely where its styling
    /// wants reviewing. This writes the same `ChromeState::osd` the key
    /// handler writes.
    SeedOsd(&'static str),
    /// Open quick settings on the Wi-Fi pane with a passphrase prompt
    /// up, by preset name (`empty`, `typed`, `rejected`).
    SeedWifiPrompt(&'static str),
    /// Raise the portal's file-chooser dialog, by mode (`open`,
    /// `open_multiple`, `save`).
    ///
    /// The trigger is a sandboxed app calling
    /// `org.freedesktop.impl.portal.FileChooser`, which a `--shots`
    /// render has no session bus to carry — so the dialog would never
    /// appear in a capture, which is exactly where its styling wants
    /// reviewing. This pushes a `FilePickerRequest` onto the same
    /// `FilePickerQueue` the D-Bus backend pushes to, and turns the same
    /// `pump_file_picker` the compositor turns every frame. The queue's
    /// downstream path is the same one used by the compositor.
    SeedFilePicker(&'static str),
    /// Raise the sheet asking how to open an NTFS drive, by preset
    /// (`windows`: the Windows partition beside Tilo; `stick`: an unknown
    /// external drive).
    ///
    /// The trigger is the drive worker hearing from the root mount service,
    /// which a `--shots` render has neither of. This queues the request the
    /// worker builds ([`shell::shell::drives::consent_request`]) on the same
    /// portal dialog queue the compositor pumps.
    SeedDriveConsent(&'static str),
    /// One window, alone in the active strip, whose tabs show the given
    /// apps with the titles their clients set and the slots their apps
    /// declared for the window's bar ([`ui::window_toolbar`]); `active` is
    /// the tab it shows and `width` its share of the display, in permille.
    SeedWindowBar {
        tabs: &'static [BarTab],
        active: usize,
        width: u32,
    },
    /// The app of the seeded window `surface_id` declares these slots now,
    /// as an app answers what the bar told it (suggestions for typed text).
    DeclareBarSlots {
        surface_id: u64,
        slots: fn() -> ui::window_toolbar::WindowToolbar,
    },
    /// Cut the `idx`-th group of the active strip down to one tab, so a
    /// scenario can set up the "lone window selected" shape rather than
    /// the four-tab group the default workspace boots with.
    LoneWindow(usize),
    /// Replace one live shell-hosted window with a long Notes document. This
    /// exercises complete semantic App capture through the same provider and
    /// shared card path used by the shell, without typing hundreds of
    /// deterministic key events into the visual harness.
    SeedLongNotes(usize),
    /// Merge changes Ada and her agent made into the long note
    /// [`Action::SeedLongNotes`] put in the `idx`-th group, as their replicas
    /// send them: the note's History then lists who did what, and when.
    SeedNoteHistory(usize),
    /// Resize every window in the active strip to `permille` of the
    /// display's usable width — what dragging the resize handles gets
    /// you.
    ///
    /// A capture of windows moving needs more than one of them on
    /// screen, and the default width is the whole display.
    SetWindowWidths(u32),
    SwitchWorkspace(usize),
    /// Add a strip named so to the active workspace and go to it, as the
    /// strip switcher's "New strip" does.
    CreateStrip(&'static str),
    /// Go to strip `n` (from 0) of the active workspace.
    FocusStrip(usize),
    PushQsLevel(QsLevel),
    /// Open the production host-session first-login chrome with an isolated,
    /// nonexistent identity path so screenshot tests cannot mutate user data.
    ShowHostDesktopOnboarding,
    SetOnboardingVisual(shell::onboarding_view::OnboardingVisualState),
    SetLockVisual(super::driver::LockVisualState),
    /// Turn the reduced-transparency setting on or off: floating chrome is
    /// then the opaque material instead of the blurred one.
    ReduceTransparency(bool),
    /// Create the person's "Research" workspace on the shown device, the way
    /// the laptop has it, and look at it.
    SeedOwnWorkspace,
    /// Split the "Outline" tab of the shown device's own "Research"
    /// workspace into a window of its own.
    SplitOwnOutline,
    /// Hold another device of the same person (a laptop with the
    /// "Research" workspace) in memory beside the shown one.
    SeedOtherOwnDevice,
    /// Bring the other device's workspace list and room content to the
    /// shown device through the real signed list and room document, then
    /// look at the workspace that arrived.
    ReceiveFromOtherOwnDevice,
    /// The other device splits its "Outline" tab into a window of its own.
    OtherOwnDeviceSplitsOutline,
    /// A player in another workspace and three people here, for the desk
    /// widgets.
    SeedDesk,
    /// Quick Settings open on its root page.
    OpenQuickSettings,
    /// A key reached the focused window.
    DeskTyping,
    /// Super alone: tapped, held, released.
    Super(shell::shell::input_policy::SuperGesture),
    /// Where the focused field's caret is.
    SetTextCaret(Option<vello::kurbo::Rect>),
    /// Move the pointer through `points`, one frame (~16 ms) apart: a
    /// trajectory, for pointer intent.
    PointerPath(Vec<Point>),
    /// Apply `cue` and capture `frames` frames `every_ms` apart on the
    /// toolkit's clock (`prefix-000`, `prefix-001`, …): every motion is
    /// sampled exactly where its production curve stands at each instant,
    /// however long an offscreen render takes. Needs the shell's
    /// `test-support` feature (the toolkit's clock is pinned per frame);
    /// without it the frames are skipped with a note.
    MotionFrames {
        prefix: &'static str,
        cue: MotionCue,
        frames: u32,
        every_ms: u64,
    },
    /// Record every frame of a motion at 60 Hz until it settles; see
    /// [`film::Film`]. Needs the shell's `test-support` feature.
    Film(film::Film),
    /// A shell-side cue or set-up step of the films; see [`film::Step`].
    Step(film::Step),
    /// Open `app` as a window of its own, `width` per mille of the display
    /// wide, as every app runs on a device: its surface shows the frames it
    /// draws in a frame of its own, and the window's bar reaches it only over
    /// its shell-ui connection. Needs the shell's `test-support` feature.
    OwnProcessWindow {
        app: OwnApp,
        width: u32,
    },
}

/// An app [`Action::OwnProcessWindow`] runs as a window of its own.
#[derive(Debug, Clone, Copy)]
pub enum OwnApp {
    /// Notes on the long note [`Action::SeedLongNotes`] writes, with
    /// changes Ada and her agent made to it.
    Notes,
    /// Files at the shell's starting place, with a few files in it.
    Files,
}

/// What starts the motion a [`Action::MotionFrames`] captures. Only a build
/// with `test-support` plays motion, so only it reads a cue.
#[derive(Debug, Clone)]
#[cfg_attr(
    not(feature = "test-support"),
    allow(
        dead_code,
        reason = "only MotionFrames reads a cue, and it needs the test-support clock"
    )
)]
pub enum MotionCue {
    /// Quick Settings opens, as a click on the tray opens it.
    OpenQuickSettings,
    /// Quick Settings closes.
    CloseQuickSettings,
    /// A Quick Settings section opens in place (or, with `None`, the open
    /// one closes), as a press on a tile's chevron does.
    QuickSettingsSection(Option<shell::shell::quick_settings::QsSection>),
    /// Click at this point.
    ClickAt(Point),
    /// The overview opens (`true`) or closes, as `source` asks: Super+Tab is
    /// the keyboard, "Overview" in the workspace menu a click.
    Overview {
        open: bool,
        source: ui::tokens::MotionSource,
    },
    /// Click the named target ([`ShellDriver::resolve_named_centre`]).
    ClickNamed(&'static str),
}

/// A named, ordered list of actions.
#[derive(Debug, Clone)]
pub struct Scenario {
    pub name: &'static str,
    pub description: &'static str,
    pub actions: Vec<Action>,
}

/// Trait for the per-mode `Snapshot` sink. Implementors decide how to
/// persist the captured state (PNG, JSON, no-op).
pub trait SnapshotSink {
    /// Called whenever the scenario emits a `Snapshot(name)`. The sink
    /// has full driver access so it can render, serialise, or both.
    fn capture(&mut self, driver: &mut ShellDriver, name: &str);
    /// Called with every frame an [`Action::Film`] painted; a sink that
    /// keeps no images ignores them.
    #[cfg_attr(
        not(feature = "test-support"),
        allow(
            dead_code,
            reason = "only a test-support build pins the clock a film records on"
        )
    )]
    fn film(&mut self, driver: &mut ShellDriver, prefix: &str, frames: Vec<film::FilmFrame>) {
        let _ = (driver, prefix, frames);
    }
}

/// Default no-op sink used by window mode (you watch the screen).
pub struct NullSink;
impl SnapshotSink for NullSink {
    fn capture(&mut self, _driver: &mut ShellDriver, _name: &str) {}
}

/// Run a scenario through `driver`, sending each `Snapshot` to `sink`.
/// Resets modifiers between snapshots so chord state doesn't leak.
pub fn run<S: SnapshotSink>(driver: &mut ShellDriver, scenario: &Scenario, sink: &mut S) {
    run_with(driver, scenario, sink, None);
}

/// Variant of [`run`] that, when `per_frame` is `Some(n)`, expands every
/// [`Action::Wait`] into `n` evenly-spaced intermediate `Snapshot`
/// captures. Output names are `<step-name>-frame<NN>` where `<step-name>`
/// is taken from the most recent named `Snapshot` action (so a single
/// `Wait` between two `Snapshot`s lands its frames under the upstream
/// snapshot's name).
///
/// The intent is to make animation curves inspectable per frame without
/// hand-authoring `SnapshotSequence` actions in every animated scenario.
/// When `per_frame` is `None` this collapses to the plain `run` path.
pub fn run_with<S: SnapshotSink>(
    driver: &mut ShellDriver,
    scenario: &Scenario,
    sink: &mut S,
    per_frame: Option<u32>,
) {
    // Track the most recent named Snapshot so per-frame intermediate
    // captures hang off it. Each `Snapshot(name)` rolls the index back
    // to 0; each per-frame capture increments it.
    let mut last_snapshot: Option<&'static str> = None;
    for action in &scenario.actions {
        match (action, per_frame) {
            (Action::Wait(n), Some(frames)) if frames > 0 => {
                // Split the wait into `frames` equal slices, snapshotting
                // after each slice. The animation timer is real wall-time
                // inside the host, so we still spend the full `n` ticks —
                // just punctuated with extra captures.
                let total = *n;
                let frames = frames.min(total.max(1));
                // `frames.max(1)` guarantees a non-zero divisor.
                #[allow(
                    clippy::arithmetic_side_effects,
                    reason = "frames.max(1) just above guarantees a non-zero divisor"
                )]
                let per_slice = total / frames.max(1);
                // `per_slice` is `total / frames`, so `per_slice * frames` never
                // exceeds `total` — `saturating_sub` documents that instead of a
                // bare `-` that clippy can't see is infallible here.
                let remainder = total.saturating_sub(per_slice.saturating_mul(frames));
                let prefix = last_snapshot.unwrap_or("frame");
                for i in 0..frames {
                    // Distribute the remainder over the first `remainder`
                    // slices so the total advance still equals `n`.
                    let extra = u32::from(i < remainder);
                    let advance = per_slice.saturating_add(extra);
                    if advance > 0 {
                        driver.step_frames(advance);
                    }
                    driver.relayout();
                    let name: &'static str =
                        Box::leak(format!("{prefix}-frame{i:02}").into_boxed_str());
                    sink.capture(driver, name);
                }
            }
            (Action::Snapshot(name), _) => {
                last_snapshot = Some(*name);
                apply(driver, action, sink);
            }
            _ => apply(driver, action, sink),
        }
    }
}

fn apply<S: SnapshotSink>(driver: &mut ShellDriver, action: &Action, sink: &mut S) {
    apply_pointer_and_capture(driver, action, sink);
    apply_open_window(driver, action);
    apply_add_hosted_tab(driver, action);
    apply_hosted_window(driver, action);
    apply_reveal_samples(driver, action, sink);
    apply_make_space_sample_and_widths(driver, action, sink);
    apply_seed_status_and_file_picker(driver, action);
    apply_seed_wifi_and_select_group(driver, action);
    apply_window_and_workspace_actions(driver, action);
    apply_seed_window_bar(driver, action);
    apply_desk(driver, action);
    apply_motion_frames(driver, action, sink);
    apply_own_process_window(driver, action);
    film::apply(driver, action, sink);
}

/// The long note [`Action::SeedLongNotes`] writes: more than a window shows.
fn long_note_body() -> String {
    (1..=52)
        .map(|line| {
            format!(
                "Complete capture {line:02} — the document continues beyond the physical display while the live Notes window stays exactly where it is."
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The window [`Action::OwnProcessWindow`] opens for Notes.
const OWN_NOTES: BarTab = BarTab {
    app_id: "so.tilo.notes",
    title: "Complete capture",
    surface_id: 9_301,
    slots: None,
};

/// The window [`Action::OwnProcessWindow`] opens for Files.
const OWN_FILES: BarTab = BarTab {
    app_id: "so.tilo.file-explorer",
    title: "Files",
    surface_id: 9_302,
    slots: None,
};

/// `Action::OwnProcessWindow` — split out of [`apply`] to keep that
/// dispatcher short.
fn apply_own_process_window(driver: &mut ShellDriver, action: &Action) {
    let Action::OwnProcessWindow { app, width } = action else {
        return;
    };
    let window: &'static [BarTab] = match app {
        OwnApp::Notes => &[OWN_NOTES],
        OwnApp::Files => &[OWN_FILES],
    };
    // The window is placed and adopted as a client's toplevel is.
    apply_seed_window_bar(
        driver,
        &Action::SeedWindowBar {
            tabs: window,
            active: 0,
            width: *width,
        },
    );
    #[cfg(not(feature = "test-support"))]
    {
        let _ = app;
        eprintln!(
            "scenario: a window of its own connects over shell-ui, which needs              `--features test-support`; it shows a plain frame"
        );
    }
    #[cfg(feature = "test-support")]
    if let Some(tab) = window.first()
        && let Err(error) = run_own_process_app(driver, *app, tab)
    {
        eprintln!("scenario: {app:?} did not run as a window of its own: {error}");
    }
}

/// Mount `app` for the tab `window` opened and run it behind its own
/// connection.
#[cfg(feature = "test-support")]
fn run_own_process_app(
    driver: &mut ShellDriver,
    app: OwnApp,
    window: &BarTab,
) -> Result<(), String> {
    use shell::shell::state::TileContent;
    let target = {
        let chrome = driver.chrome();
        let tree = chrome.tree();
        tree.desktop.active_workspace().and_then(|workspace| {
            workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .find(|tab| {
                    matches!(tab.tile, TileContent::Foreign { surface_id, .. } if surface_id == window.surface_id)
                })
                .map(|tab| (workspace.room().to_string(), tab.id))
        })
    };
    let (room, tab) = target.ok_or("its window did not open")?;
    let inputs = driver.chrome().hosted_inputs().clone();
    let component = match app {
        OwnApp::Notes => inputs.mount_note(&room, tab, "Complete capture.md", &long_note_body()),
        OwnApp::Files => {
            if let Some(directory) = inputs.files_directory() {
                for name in [
                    "Quarterly report.txt",
                    "Release notes.md",
                    "Recipes.md",
                    "Receipts 2026.csv",
                    "Travel plans.md",
                ] {
                    let _ = std::fs::write(directory.join(name), name);
                }
            }
            inputs.mount_files(&room, tab)
        }
    }
    .ok_or("this host mounts no apps")?
    .map_err(|error| error.to_string())?;
    let runtime = ui::App::from_component(
        component,
        ui::application::ComponentRunnerInputs::from_native_process(),
    )
    .map_err(|error| error.to_string())?;
    driver.add_own_process_app(runtime, window.app_id, window.surface_id)?;
    if matches!(app, OwnApp::Notes) {
        seed_note_history_of_tab(driver, &room, tab);
    }
    driver.relayout();
    Ok(())
}

/// `Action::MotionFrames` — split out of [`apply`] to keep that dispatcher
/// short.
fn apply_motion_frames<S: SnapshotSink>(driver: &mut ShellDriver, action: &Action, sink: &mut S) {
    let Action::MotionFrames {
        prefix,
        cue,
        frames,
        every_ms,
    } = action
    else {
        return;
    };
    #[cfg(not(feature = "test-support"))]
    {
        let _ = (driver, sink, prefix, cue, frames, every_ms);
        eprintln!(
            "scenario: MotionFrames pins the toolkit's clock, which needs \
             `--features test-support`; skipping"
        );
    }
    #[cfg(feature = "test-support")]
    {
        driver.relayout();
        let start = ui::time::now();
        let every = std::time::Duration::from_millis(*every_ms);
        for i in 0..*frames {
            let at = start.checked_add(every.saturating_mul(i)).unwrap_or(start);
            ui::time::testing::with_monotonic_time(at, || {
                if i == 0 {
                    apply_motion_cue(driver, cue);
                }
                // The apps answer what the cue asked (a toggle, a page) on
                // their update, as on a real frame.
                driver.pump_apps();
                driver.relayout();
                let name: &'static str = Box::leak(format!("{prefix}-{i:03}").into_boxed_str());
                sink.capture(driver, name);
            });
        }
    }
}

#[cfg(feature = "test-support")]
fn apply_motion_cue(driver: &mut ShellDriver, cue: &MotionCue) {
    match cue {
        MotionCue::OpenQuickSettings => driver.with_chrome(ShellDriver::seed_laptop_quick_settings),
        MotionCue::CloseQuickSettings => {
            driver.with_chrome(|st| st.overlays.quick_settings = false);
        }
        MotionCue::QuickSettingsSection(section) => {
            let section = *section;
            driver.with_chrome(|st| st.quick_settings.section = section);
        }
        MotionCue::ClickAt(point) => driver.click_at(*point),
        MotionCue::Overview { open, source } => {
            let open = *open;
            driver.with_chrome(|st| {
                st.overlays.overview = open;
                st.bump();
            });
            driver.host_mut().latch_overview_transition_from(*source);
        }
        MotionCue::ClickNamed(name) => match driver.resolve_named_centre(name) {
            Some(point) => driver.click_at(point),
            None => eprintln!("scenario: could not resolve named target '{name}'"),
        },
    }
}

/// The desk widgets and the Super surface.
fn apply_desk(driver: &mut ShellDriver, action: &Action) {
    match action {
        Action::SeedDesk => driver.seed_desk_for_shots(),
        Action::OpenQuickSettings => {
            driver.with_chrome(ShellDriver::seed_laptop_quick_settings);
        }
        Action::DeskTyping => driver.desk_typing_for_shots(),
        Action::Super(gesture) => driver.super_gesture_for_shots(*gesture),
        Action::SetTextCaret(caret) => driver.set_text_caret_for_shots(*caret),
        Action::PointerPath(points) => {
            for p in points {
                driver.move_cursor(*p);
                driver.step_frames(1);
            }
        }
        _ => {}
    }
}

/// Pointer, keyboard and plain-capture actions — split out of [`apply`]
/// to keep that dispatcher short. One arm per `Action` variant, matching
/// [`apply`]'s own dispatch; every other variant is a no-op here since
/// exactly one of the `apply_*` helpers owns it.
#[allow(
    clippy::too_many_lines,
    reason = "the deterministic scenario dispatcher keeps its action mapping explicit"
)]
fn apply_pointer_and_capture<S: SnapshotSink>(
    driver: &mut ShellDriver,
    action: &Action,
    sink: &mut S,
) {
    match action {
        Action::ClickAt(p) => driver.click_at(*p),
        Action::ClickNamed(name) => match driver.resolve_named_centre(name) {
            Some(p) => driver.click_at(p),
            None => eprintln!(
                "scenario: could not resolve named target '{name}' (rect not populated yet — paint first?)"
            ),
        },
        Action::Drag { from, to, steps } => driver.drag(*from, *to, *steps),
        Action::MiddleDrag { from, to, steps } => driver.middle_drag(*from, *to, *steps),
        Action::MouseWheel { dx, dy } => driver.mouse_wheel(*dx, *dy),
        Action::StripPan(travel) => {
            driver
                .host_mut()
                .update_active_strip_pan(*travel, true, std::time::Instant::now());
        }
        Action::EndStripPan => {
            driver
                .host_mut()
                .end_active_strip_pan(std::time::Instant::now(), false);
        }
        Action::Relayout => driver.relayout(),
        Action::MoveCursor(p) => driver.move_cursor(*p),
        Action::CompositorPointer(p) => {
            driver.host_mut().note_pointer(*p);
            driver.move_cursor(*p);
        }
        Action::HideTopBar(hide) => driver.host_mut().set_hide_top_bar_for_tests(*hide),
        Action::IdentifyDisplays => {
            let snapshot = identify_snapshot(driver.host().output_geometry());
            driver.host_mut().identify_displays_now(&snapshot);
        }
        Action::SecondaryClickNamed(name) => one_place::secondary_click_named(driver, name),
        Action::TouchSwipe(from, to) => one_place::touch_swipe(driver, *from, *to),
        Action::FullscreenBarKey => driver.host_mut().reveal_fullscreen_bar_by_key(),
        Action::Notify(title, body) => one_place::notify(driver, title, body),
        Action::FullScreenFocused(on) => one_place::full_screen_focused(driver, *on),
        Action::MoveCursorNamed(name) => match driver.resolve_named_centre(name) {
            Some(p) => driver.move_cursor(p),
            None => eprintln!(
                "scenario: could not resolve named target '{name}' (rect not populated yet — paint first?)"
            ),
        },
        Action::MoveCursorGroupGrab(index) => {
            if !driver.move_cursor_to_group_grab(*index) {
                eprintln!("scenario: could not resolve group {index}'s drag handle");
            }
        }
        Action::MoveCursorGroupEdge { group, leading } => {
            if !driver.move_cursor_to_group_edge(*group, *leading) {
                eprintln!("scenario: could not resolve group {group}'s edge");
            }
        }
        Action::MoveCursorBy(dx) => driver.move_cursor_by(*dx),
        Action::MoveCursorGroupReorderTarget(index) => {
            if !driver.move_cursor_to_group_reorder_target(*index) {
                eprintln!("scenario: could not resolve group {index}'s reorder target");
            }
        }
        Action::MousePress => driver.press(),
        Action::MouseRelease => driver.release(),
        Action::SnapshotSequence {
            prefix,
            frames,
            gap_frames,
        } => {
            for i in 1..=*frames {
                if *gap_frames > 0 {
                    driver.step_frames(*gap_frames);
                }
                driver.relayout();
                // Use a heap-allocated name leaked to 'static so the
                // `&'static str` `Snapshot` variant can carry it. The
                // process exits at the end of `shots`, so the leak is
                // bounded by scenario length and stays within tens of KB.
                let name: &'static str = Box::leak(format!("{prefix}-{i:03}").into_boxed_str());
                sink.capture(driver, name);
            }
        }
        Action::Key(k) => driver.press_key(k),
        Action::Type(s) => driver.type_str(s),
        Action::Modifiers(m) => driver.set_modifiers(*m),
        Action::Wait(n) => driver.step_frames(*n),
        Action::WaitStripSettled(max) => wait_strip_settled(driver, *max),
        Action::WaitSettled => {
            let _ = driver.paint_settled();
        }
        Action::Snapshot(name) => {
            // Always settle the layout pass before capturing — overlay
            // toggles since the last snapshot may have changed
            // structural sizes (overview reservation, modal scrim).
            driver.relayout();
            sink.capture(driver, name);
            // Reset modifiers between named outputs so chord state
            // doesn't bleed across snapshots.
            driver.set_modifiers(ModifiersState::empty());
        }
        Action::OpenPalette => {
            // Drive the default Super+Space binding through the host's keyboard branch instead
            // of `set_palette_visible(true)` directly — the hotkey path
            // calls `toggle_palette()` which also focuses the palette
            // input + clears the residual query (see ShellOverlayHost::
            // toggle_palette). Without that, subsequent `Type(_)`
            // actions would fall through because the palette ignores
            // characters when `!is_focused`.
            driver.set_modifiers(ModifiersState::SUPER);
            driver.press_key(&Key::Named(winit::keyboard::NamedKey::Space));
            driver.set_modifiers(ModifiersState::empty());
        }
        Action::CaptureRecordingActive(active) => {
            driver.set_capture_recording_active(*active);
        }
        Action::CapturePipTransition(permille) => {
            driver.set_capture_pip_transition_progress(*permille);
        }
        Action::CaptureRecordingFinished => {
            driver.set_capture_recording_finished();
        }
        Action::CaptureReplayActive(active) => {
            driver.set_capture_replay_active(
                *active,
                if *active {
                    ui::widget::AudioCaptureMode::SystemAndMicrophone
                } else {
                    ui::widget::AudioCaptureMode::None
                },
            );
        }
        Action::CaptureReplayBuffered(seconds) => {
            driver.set_capture_replay_buffered_seconds(*seconds);
        }
        Action::CaptureReplaySaved => {
            driver.set_capture_replay_saved();
        }
        Action::CaptureScreenshotFinished { saved, copied } => {
            driver.set_capture_screenshot_finished(ui::widget::ScreenshotKept {
                saved: *saved,
                copied: *copied,
            });
        }
        Action::CaptureScreenshotTransition(permille) => {
            driver.set_capture_screenshot_transition_progress(*permille);
        }
        Action::ScreenshotMarkupTransition(permille) => {
            driver.set_screenshot_markup_transition_progress(*permille);
        }
        Action::CaptureAudioAvailable(available) => {
            driver.set_capture_audio_available(*available);
        }
        Action::CaptureExecutionEnabled(enabled) => {
            driver.set_capture_execution_enabled(*enabled);
        }
        Action::TerminalPrints { from, to } => {
            let text = (*from..=*to).fold(String::new(), |mut text, line| {
                use std::fmt::Write as _;
                let _ = write!(text, "output line {line}\r\n");
                text
            });
            if let Err(error) = driver.feed_active_terminal(&text) {
                eprintln!("shots: terminal prints: {error}");
            }
        }
        Action::ClientCommit { surface_id, rgba } => {
            let store = shell::compositor::foreign::store();
            if let Some((w, h)) = store.viewport(*surface_id)
                && let Some(texels) = w
                    .checked_mul(h)
                    .filter(|texels| *texels > 0)
                    .and_then(|texels| usize::try_from(texels).ok())
            {
                store.publish_rgba(*surface_id, &rgba.repeat(texels), w, h);
            }
        }
        Action::ShowHostDesktopOnboarding => driver.show_host_desktop_onboarding(),
        Action::SetOnboardingVisual(state) => driver.set_onboarding_visual_state(*state),
        Action::SetLockVisual(state) => driver.set_lock_visual_state(*state),
        Action::ReduceTransparency(on) => driver.set_reduce_transparency(*on),
        Action::SeedRoomHistory => driver.seed_room_history_for_shots(),
        Action::SeedSharedWorkspace => driver.seed_shared_workspace_for_shots(),
        Action::SeedLauncherFresh => driver.seed_launcher_fresh_for_shots(),
        Action::SeedLauncherBusy => driver.seed_launcher_busy_for_shots(),
        Action::SeedStripsWithPeople => driver.seed_strips_with_people_for_shots(),
        Action::SeedAgentScreenExposure { surface_id } => {
            driver.seed_agent_screen_exposure_for_shots(*surface_id);
        }
        Action::SeedInferredStatus(text) => driver.seed_inferred_status_for_shots(text),
        Action::SeedCrowdedWorkspace => driver.seed_crowded_workspace_for_shots(),
        Action::SeedPresenceRoom => driver.seed_presence_room_for_shots(),
        Action::OnlyYou => driver.only_you_for_shots(),
        Action::RemoteCursorChat { who, text, sent } => {
            driver.remote_cursor_chat_for_shots(who, text, *sent);
        }
        Action::RemoteReaction { who, emoji } => driver.remote_reaction_for_shots(who, emoji),
        Action::OpenCursorChat(text) => driver.host_mut().open_cursor_chat_for_shots(text),
        Action::OpenAppChooser(purpose) => driver.host_mut().open_app_chooser_for_shots(purpose),
        Action::FollowPeer(id) => driver.host_mut().follow_for_shots(*id),
        Action::ExpandRosterRow(id) => driver.host_mut().expand_collaborator_for_shots(id),
        Action::StopFollowingByInput => driver.host_mut().stop_following_by_input_for_shots(),
        Action::WaveFrom(id) => driver.host_mut().wave_from_for_shots(id),
        Action::Arrive { public } => driver.host_mut().arrive_for_shots(*public),
        Action::SeedForeignAppWork {
            app_id,
            action,
            resource,
        } => driver.seed_foreign_app_work_for_shots(app_id, action, resource),
        Action::SeedInvitePending => driver.seed_invite_pending_for_shots(),
        Action::SeedOwnWorkspace => driver.seed_own_workspace_for_shots(),
        Action::SplitOwnOutline => driver.split_own_outline_for_shots(),
        Action::SeedOtherOwnDevice => driver.seed_other_own_device_for_shots(),
        Action::ReceiveFromOtherOwnDevice => driver.receive_from_other_own_device_for_shots(),
        Action::OtherOwnDeviceSplitsOutline => driver.other_own_device_splits_outline_for_shots(),
        Action::AdmitInvitedCollaborator => assert!(
            driver.admit_invited_collaborator_for_shots(),
            "scenario: collaboration journey cannot show live editing before Ada's pending invite is admitted"
        ),
        Action::RevertRoomActivity => assert!(
            driver.revert_room_activity_for_shots(),
            "scenario: collaboration journey cannot label a frame 'reverted' without a forward revert"
        ),
        Action::ShowCollaboratorInFiles => assert!(
            driver.show_collaborator_in_files_for_shots(),
            "scenario: collaboration journey cannot label a frame 'Ada in Files' unless Files was focused"
        ),
        _ => {}
    }
}

/// `Action::SeedNoteHistory`: an afternoon of work on the long note by Ada
/// and the agent working for her, merged as their replicas send it.
fn seed_note_history(driver: &ShellDriver, group_idx: usize) {
    let target = {
        let chrome = driver.chrome();
        let tree = chrome.tree();
        tree.desktop.active_workspace().and_then(|workspace| {
            let strip = workspace.strips.first()?;
            let group = strip.window_groups.get(group_idx)?;
            Some((workspace.room().to_string(), group.active_tab))
        })
    };
    let Some((room, tab)) = target else {
        eprintln!("scenario: no window to seed History in");
        return;
    };
    seed_note_history_of_tab(driver, &room, tab);
}

/// An afternoon of work by Ada and her agent on the note tab `tab` of
/// `room` shows, merged as their replicas send it.
fn seed_note_history_of_tab(driver: &ShellDriver, room: &str, tab: u64) {
    // The agent writes as its own device key, which the session verified as
    // delegated by Ada: its changes read as the agent's, for Ada.
    let agent_device = [0x5a_u8; 32];
    ui::history_source::set_room_agents(
        room,
        vec![ui::history_source::RosterAgent {
            device: agent_device,
            person: [0xad_u8; 32],
            person_name: "Ada".into(),
            agent_id: "research".into(),
            label: "Research".into(),
            color: [0x8b, 0x7c, 0xf6],
        }],
    );
    let now = ui::time::wall()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| i64::try_from(since.as_secs()).unwrap_or(0));
    let edits = [
        notes::scenario::PeerEdit {
            actor: b"Ada",
            purpose: "Added the open questions",
            paragraph: "Open questions: who owns the capture budget, and when do we review it?",
            at: now.saturating_sub(3 * 3600),
        },
        notes::scenario::PeerEdit {
            actor: b"Ada",
            purpose: "Added the next steps",
            paragraph: "Next steps: draft the review, then share it with the team on Thursday.",
            at: now.saturating_sub(3 * 3600 - 240),
        },
        notes::scenario::PeerEdit {
            actor: &agent_device,
            purpose: "Summarised the note",
            paragraph: "Summary: the capture continues past the display; two questions remain open.",
            at: now.saturating_sub(25 * 60),
        },
    ];
    let inputs = driver.chrome().hosted_inputs().clone();
    match inputs.merge_note_edits(room, tab, &edits) {
        Ok(merged) if merged == edits.len() => {}
        Ok(merged) => eprintln!("scenario: only {merged} of the note's changes merged"),
        Err(error) => eprintln!("scenario: the note's history was not seeded: {error}"),
    }
}

/// The room the launches of [`Action::HoldLaunchForRoom`] wait for.
fn shots_room() -> shell::shell::state::RoomId {
    shell::shell::state::RoomId::from(uuid::Uuid::from_u128(
        0x5107_0000_0000_4000_8000_0000_0000_0001,
    ))
}

/// `Action::OpenWindow` — split out of [`apply`] to keep that dispatcher short.
fn apply_open_window(driver: &mut ShellDriver, action: &Action) {
    match action {
        Action::HoldLaunchPlace { app_id, label } => {
            driver.with_chrome(|st: &mut ChromeState| {
                let placement = shell::shell::placement::placement_for_target(
                    &st.tree(),
                    st.strip_viewport(),
                    st.open_target,
                );
                shell::shell::tab_binding::hold_place_for_launch(
                    &mut st.tree_mut(),
                    app_id,
                    label,
                    placement,
                );
                st.bump();
            });
            return;
        }
        Action::HoldLaunchForRoom(app_id) => {
            driver.with_chrome(|st: &mut ChromeState| {
                st.tree_mut().joining_rooms.insert(shots_room());
                shell::shell::launcher::room_hold::hold_for_shots(
                    st.tree_handle(),
                    app_id,
                    shots_room(),
                );
                st.bump();
            });
            return;
        }
        Action::RoomReady => {
            driver.with_chrome(|st: &mut ChromeState| {
                st.tree_mut().joining_rooms.remove(&shots_room());
                st.bump();
            });
            return;
        }
        Action::HoldInstallPlace { app_id, label } => {
            driver.with_chrome(|st: &mut ChromeState| {
                shell::shell::peer_windows::show_install_under_way_for_shots(
                    &mut st.tree_mut(),
                    app_id,
                    label,
                );
                st.bump();
            });
            return;
        }
        Action::InstallBytes {
            app_id,
            received,
            total,
        } => {
            shell::shell::peer_windows::show_install_bytes_for_shots(app_id, *received, *total);
            driver.step_frames(1);
            return;
        }
        Action::WaitWall(ms) => {
            std::thread::sleep(std::time::Duration::from_millis(*ms));
            driver.step_frames(1);
            return;
        }
        _ => {}
    }
    if let Action::OpenWindow { app_id, surface_id } = action {
        let app_id = *app_id;
        let surface_id = *surface_id;
        driver.with_chrome(|st: &mut ChromeState| {
            {
                // Exactly what opening an app does: decide where the
                // window goes now, against the layout the user is
                // looking at, and leave it as a note for the adoption.
                let tree = st.tree();
                shell::shell::placement::note_placement_for_launch(
                    app_id,
                    &tree,
                    st.strip_viewport(),
                );
            }
            {
                let mut tree = st.tree_mut();
                shell::compositor::tab_binding::adopt_toplevel(
                    &mut tree,
                    surface_id,
                    Some(app_id.to_string()),
                );
            }
            st.bump();
        });
        // Stand in for the client. A window is not painted until it
        // has drawn something that fits the tab it is in, which is
        // what stops a launch putting an empty frame on screen — and
        // an adopted toplevel with no process behind it has drawn
        // nothing at all, so without this the placement scenarios
        // would depict where windows land by showing none of them.
        //
        // A layout pass first, because the size to draw at is the
        // tab's, and the tab does not have one until it has been
        // laid out. Which app each window is still reads off the tab
        // pill above it.
        driver.relayout();
        if let Some((w, h)) = shell::compositor::foreign::store().viewport(surface_id)
            && w > 0
            && h > 0
        {
            let px = (w as usize).saturating_mul(h as usize).saturating_mul(4);
            shell::compositor::foreign::store().publish_rgba(surface_id, &vec![0xF4; px], w, h);
        }
    }
}

/// `Action::SeedWindowBar` — split out of [`apply`] to keep that dispatcher
/// short. The windows are adopted the way a client's toplevel is, titled
/// the way its client titles them, gathered into one group as dragging
/// their bars together would, and stood in for with a plain frame.
#[allow(
    clippy::too_many_lines,
    reason = "one screenshot fixture staged top to bottom"
)]
fn apply_seed_window_bar(driver: &mut ShellDriver, action: &Action) {
    use shell::shell::state::TileContent;
    if let Action::DeclareBarSlots { surface_id, slots } = action {
        let tab = {
            let chrome = driver.chrome();
            let tree = chrome.tree();
            tree.desktop
                .workspaces
                .iter()
                .flat_map(|workspace| &workspace.strips)
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .find(|tab| {
                    matches!(&tab.tile, TileContent::Foreign { surface_id: id, .. } if id == surface_id)
                })
                .map(|tab| tab.id)
        };
        if let Some(tab) = tab {
            driver
                .host_mut()
                .declare_tab_toolbar_for_tests(tab, Some(slots()));
            driver.relayout();
        }
        return;
    }
    let Action::SeedWindowBar {
        tabs,
        active,
        width,
    } = action
    else {
        return;
    };
    let surface_of = |tile: &TileContent| match tile {
        TileContent::Foreign { surface_id, .. } => Some(*surface_id),
        _ => None,
    };
    for tab in *tabs {
        // Opened as any window is: placed, adopted, stood in for.
        apply_open_window(
            driver,
            &Action::OpenWindow {
                app_id: tab.app_id,
                surface_id: tab.surface_id,
            },
        );
        driver.with_chrome(|st: &mut ChromeState| {
            {
                let mut tree = st.tree_mut();
                shell::compositor::tab_binding::rename_tab_for_title(
                    &mut tree,
                    tab.surface_id,
                    tab.title,
                );
            }
            st.bump();
        });
    }
    let wanted: Vec<u64> = tabs.iter().map(|tab| tab.surface_id).collect();
    driver.with_chrome(|st: &mut ChromeState| {
        {
            let mut tree = st.tree_mut();
            let mut target = None;
            if let Some(workspace) = tree.desktop.active_workspace_mut() {
                for strip in &mut workspace.strips {
                    let mut gathered = Vec::new();
                    let mut home = None;
                    for (index, group) in strip.window_groups.iter_mut().enumerate() {
                        let mut kept = Vec::new();
                        for tab in group.tabs.drain(..) {
                            if surface_of(&tab.tile).is_some_and(|id| wanted.contains(&id)) {
                                home.get_or_insert(index);
                                gathered.push(tab);
                            } else {
                                kept.push(tab);
                            }
                        }
                        group.tabs = kept;
                    }
                    let Some(home) = home else {
                        continue;
                    };
                    gathered.sort_by_key(|tab| {
                        surface_of(&tab.tile)
                            .and_then(|id| wanted.iter().position(|w| *w == id))
                            .unwrap_or(usize::MAX)
                    });
                    // The first window's group holds them all, alone in the
                    // strip, so the shot shows this one window.
                    let only = strip.window_groups.swap_remove(home);
                    strip.window_groups = vec![only];
                    if let Some(group) = strip.window_groups.first_mut() {
                        group.active_tab = gathered
                            .get(*active)
                            .or_else(|| gathered.first())
                            .map_or(group.active_tab, |tab| tab.id);
                        group.tabs = gathered;
                        target = Some((strip.id, group.id));
                    }
                }
            }
            if let Some((strip, group)) = target {
                tree.desktop.set_active_group(strip, group);
            }
        }
        st.bump();
    });
    driver.relayout();
    apply_make_space_sample_and_widths(driver, &Action::SetWindowWidths(*width), &mut NullSink);
    driver.relayout();
    let ids: Vec<(u64, Option<ToolbarOf>)> = {
        let chrome = driver.chrome();
        let tree = chrome.tree();
        tabs.iter()
            .filter_map(|bar_tab| {
                tree.desktop
                    .workspaces
                    .iter()
                    .flat_map(|workspace| &workspace.strips)
                    .flat_map(|strip| &strip.window_groups)
                    .flat_map(|group| &group.tabs)
                    .find(|tab| surface_of(&tab.tile) == Some(bar_tab.surface_id))
                    .map(|tab| (tab.id, bar_tab.slots))
            })
            .collect()
    };
    for (tab, slots) in ids {
        driver
            .host_mut()
            .declare_tab_toolbar_for_tests(tab, slots.map(|slots| slots()));
    }
    driver.relayout();
    // The stand-in frames are drawn at the size the windows have now that
    // their bars show the apps' slots.
    for tab in *tabs {
        if let Some((w, h)) = shell::compositor::foreign::store().viewport(tab.surface_id)
            && w > 0
            && h > 0
        {
            let px = (w as usize).saturating_mul(h as usize).saturating_mul(4);
            shell::compositor::foreign::store().publish_rgba(tab.surface_id, &vec![0xF4; px], w, h);
        }
    }
    driver.relayout();
}

/// `Action::AddHostedTab` — split out of [`apply`] to keep that dispatcher short.
fn apply_add_hosted_tab(driver: &mut ShellDriver, action: &Action) {
    if let Action::AddHostedTab { app, title } = action {
        let app = *app;
        let title = *title;
        let mut added = None;
        driver.with_chrome(|st: &mut ChromeState| {
            let mut tree = st.tree_mut();
            // One past the highest tab id anywhere in the tree.
            // The production minter is crate-private, and a
            // scenario only needs an id no open tab already uses
            // — but it has to be *every* tab, not just this
            // group's: a tab id names the record its document
            // lives in, so reusing one from another workspace
            // would put two tabs on one document, which is the
            // thing this scenario exists to show cannot happen.
            let id = tree
                .desktop
                .workspaces
                .iter()
                .flat_map(|w| &w.strips)
                .flat_map(|s| &s.window_groups)
                .flat_map(|g| &g.tabs)
                .map(|t| t.id)
                .max()
                .unwrap_or(0)
                .saturating_add(1);
            let Some(ws) = tree.desktop.active_workspace_mut() else {
                return;
            };
            let Some(group) = ws
                .strips
                .first_mut()
                .and_then(|s| s.window_groups.first_mut())
            else {
                return;
            };
            group.tabs.push(shell::shell::state::Tab {
                id,
                title: title.to_string(),
                tile: shell::shell::state::TileContent::AppPlaceholder {
                    kind: shell::shell::state::AppKind::Other(app.to_string()),
                    label: title.to_string(),
                },
                app_title: None,
            });
            group.active_tab = id;
            added = Some(id);
            drop(tree);
            st.bump();
        });
        // Opening the tab makes its document, as the open-app action does.
        if let Some(added) = added {
            driver.open_tab_records(&[added]);
        }
    }
}

/// `Action::OpenHostedWindow` and `Action::EnableNativeAppSearch` — split
/// out of [`apply`] to keep that dispatcher short.
fn apply_hosted_window(driver: &mut ShellDriver, action: &Action) {
    match action {
        Action::OpenHostedWindow { app, title } => {
            let kind = app_kind_for(app);
            let title = *title;
            let mut opened = None;
            driver.with_chrome(|st: &mut ChromeState| {
                // Where the window goes is decided now, against the
                // layout on screen — the same rule a launch applies. A
                // hosted app has no client process to correlate a
                // pending-placement note against, so the answer is used
                // here rather than left for an adoption to pick up.
                let placement = {
                    let tree = st.tree();
                    shell::shell::placement::placement_for(
                        &tree,
                        tilo_desktop_model::commands::OpenIntent::Launch,
                        st.strip_viewport(),
                    )
                };
                let tab = {
                    let mut tree = st.tree_mut();
                    shell::compositor::tab_binding::open_hosted_app(
                        &mut tree, kind, title, placement,
                    )
                };
                st.bump();
                opened = Some(tab);
            });
            // The window's document, made by its opening as in a session.
            if let Some(tab) = opened {
                driver.open_tab_records(&[tab]);
            }
        }
        Action::EnableNativeAppSearch => {
            match shell::shell::hosted_inputs::HostedTileInputs::for_host_directory_privately(
                &std::env::temp_dir(),
            ) {
                Ok(inputs) => {
                    driver.with_chrome(|st| st.set_hosted_inputs_for_capture(inputs));
                }
                Err(error) => eprintln!("scenario: could not enable native app search: {error}"),
            }
        }
        _ => {}
    }
}

/// `Action::SampleTabReveal` — split out of [`apply`] to keep that
/// dispatcher short.
fn apply_reveal_samples<S: SnapshotSink>(driver: &mut ShellDriver, action: &Action, sink: &mut S) {
    if let Action::SampleTabReveal { prefix, at_ms } = action {
        // Freezing an animation curve at a chosen offset needs
        // `bar_mut_for_tests`, which is `cfg(any(test,
        // debug_assertions))` — the toolkit does not expose its
        // internals to a release build, and should not start doing
        // so for a capture tool. So this one scenario is
        // debug-only; every other scenario builds and runs in
        // release, which is what lets `shots` be built at all in
        // the configuration a session actually ships in.
        #[cfg(not(debug_assertions))]
        {
            let _ = (driver, sink, prefix, at_ms);
            eprintln!(
                "scenario: SampleTabReveal needs a debug build (it freezes an \
                 animation curve through a debug-only accessor); skipping"
            );
        }
        #[cfg(debug_assertions)]
        {
            // Lay out once so the hover that preceded this has latched a
            // curve, then freeze it before a render's worth of wall time
            // can retire it.
            driver.relayout();
            sample_held(
                driver,
                sink,
                prefix,
                at_ms,
                "tab reveal in flight",
                |row, at| {
                    row.groups.iter_mut().fold(false, |any, g| {
                        g.bar_mut_for_tests().hold_reveal_at(at) | any
                    })
                },
            );
        }
    }
}

/// [`Action::WaitStripSettled`]: step a frame at a time until no strip of
/// the active workspace is panning on its own and every window's slot has
/// reached its width. The windows' content may keep animating (a caret, a
/// spinner); it moves nothing the strip is measured by.
fn wait_strip_settled(driver: &mut ShellDriver, max: u32) {
    let strips: Vec<StripId> = {
        let st = driver.chrome();
        let tree = st.tree();
        tree.desktop
            .active_workspace()
            .map(|ws| ws.strips.iter().map(|s| s.id).collect())
            .unwrap_or_default()
    };
    for _ in 0..max {
        // One frame first: what the last action started (a focus reveal, a
        // resize) only begins moving on the frame that sees it.
        driver.step_frames(1);
        let moving = strips.iter().any(|sid| {
            driver
                .host_mut()
                .group_row_mut_for_tests(*sid)
                .is_some_and(|row| {
                    row.direct_pan_in_progress()
                        || row
                            .groups
                            .iter()
                            .any(|g| (g.dest_width_px() - g.layout_width_px()).abs() > 0.25)
                })
        });
        if !moving {
            return;
        }
    }
    eprintln!("scenario: the strip was still moving after {max} frames");
}

/// Capture `{prefix}-{ms}ms` at each of `at_ms`, with every strip's row
/// animation frozen there by `hold`, which reports whether it found one in
/// flight. Returns the strips it held, so the caller can release them.
fn sample_held<S: SnapshotSink>(
    driver: &mut ShellDriver,
    sink: &mut S,
    prefix: &str,
    at_ms: &[u64],
    what: &str,
    mut hold: impl FnMut(&mut GroupRow, std::time::Duration) -> bool,
) -> Vec<StripId> {
    let strips: Vec<_> = {
        let st = driver.chrome();
        let tree = st.tree();
        tree.desktop
            .active_workspace()
            .map(|ws| ws.strips.iter().map(|s| s.id).collect())
            .unwrap_or_default()
    };
    for ms in at_ms {
        let at = std::time::Duration::from_millis(*ms);
        let mut any = false;
        for sid in &strips {
            if let Some(row) = driver.host_mut().group_row_mut_for_tests(*sid) {
                any |= hold(row, at);
            }
        }
        if !any {
            eprintln!("scenario: no {what} to sample at {ms} ms");
        }
        driver.relayout();
        let name: &'static str = Box::leak(format!("{prefix}-{ms:03}ms").into_boxed_str());
        sink.capture(driver, name);
    }
    strips
}

/// `Action::SampleMakeSpace`, `Action::SampleSiblingFade` and
/// `Action::SetWindowWidths` — split out of [`apply`] to keep that
/// dispatcher short.
fn apply_make_space_sample_and_widths<S: SnapshotSink>(
    driver: &mut ShellDriver,
    action: &Action,
    sink: &mut S,
) {
    match action {
        Action::SampleSiblingFade { prefix, at_ms } => {
            driver.relayout();
            let strips = sample_held(driver, sink, prefix, at_ms, "sibling fade", |row, at| {
                row.hold_sibling_fade_at(at)
            });
            for sid in &strips {
                if let Some(row) = driver.host_mut().group_row_mut_for_tests(*sid) {
                    row.clear_sibling_fade_hold();
                }
            }
        }
        Action::SampleMakeSpace { prefix, at_ms } => {
            // Splice the new window into the widget tree, then lay out —
            // which is the pass that measures how far each pushed group
            // travels and starts the slide. Freeze it immediately, before
            // a render's worth of wall time can retire it.
            driver.step_frames(1);
            driver.relayout();
            sample_held(
                driver,
                sink,
                prefix,
                at_ms,
                "make-space slide in flight",
                GroupRow::hold_shift_at,
            );
        }
        Action::SetWindowWidths(permille) => {
            let permille = *permille;
            driver.with_chrome(|st: &mut ChromeState| {
                {
                    let mut tree = st.tree_mut();
                    if let Some(ws) = tree.desktop.active_workspace_mut() {
                        for strip in &mut ws.strips {
                            for g in &mut strip.window_groups {
                                g.width = shell::shell::state::WindowWidth::Fraction(permille);
                            }
                        }
                    }
                }
                st.bump();
            });
            // The persisted width is the durable one, but the strip resync
            // only splices whole groups — so push it onto the live widgets
            // too, which is what a resize gesture would have done.
            let strips: Vec<_> = {
                let st = driver.chrome();
                let tree = st.tree();
                tree.desktop
                    .active_workspace()
                    .map(|ws| ws.strips.iter().map(|s| s.id).collect())
                    .unwrap_or_default()
            };
            for sid in strips {
                if let Some(row) = driver.host_mut().group_row_mut_for_tests(sid) {
                    for g in &mut row.groups {
                        g.set_width(ui::widget::WindowWidth::Fraction(permille));
                    }
                }
            }
        }
        _ => {}
    }
}

/// `Action::SeedStatus`, `Action::SeedOsd` and `Action::SeedFilePicker` —
/// split out of [`apply`] to keep that dispatcher short.
fn apply_seed_status_and_file_picker(driver: &mut ShellDriver, action: &Action) {
    match action {
        #[cfg(target_os = "linux")]
        Action::SeedDriveConsent(preset) => seed_drive_consent(driver, preset),
        Action::SeedStatus(preset) => {
            let status = status_preset(preset);
            driver.with_chrome(|st: &mut ChromeState| {
                st.quick_settings.status = status.clone();
                st.bump();
            });
        }
        Action::SeedOsd(preset) => {
            let osd = osd_preset(preset);
            driver.with_chrome(|st: &mut ChromeState| {
                st.show_osd(osd);
            });
        }
        Action::SeedFilePicker(mode) => file_chooser::seed(driver, mode),
        _ => {}
    }
}

/// `Action::SeedDriveConsent`: the sheet the drive worker raises.
#[cfg(target_os = "linux")]
fn seed_drive_consent(driver: &mut ShellDriver, preset: &str) {
    use ntfs_mount::protocol::{Drive, Mode, Placement};
    let windows = preset == "windows";
    let drive = Drive {
        id: if windows { "nvme0n1p3:1" } else { "sdb1:7" }.into(),
        label: if windows { "Windows" } else { "" }.into(),
        size_bytes: if windows {
            475_000_000_000
        } else {
            64_000_000_000
        },
        placement: if windows {
            Placement::Internal
        } else {
            Placement::External
        },
        hot_added: !windows,
        remembered: None,
        needs_consent: true,
        preselect: if windows {
            Mode::Open
        } else {
            Mode::OpenSafely
        },
        can_remember: if windows {
            ntfs_mount::protocol::RememberOffer::AnyMode
        } else {
            ntfs_mount::protocol::RememberOffer::OpenSafely
        },
        mounted: None,
    };
    let queue = shell::shell::portal_dialog::PortalDialogQueue::default();
    driver.host_mut().set_portal_dialog_queue(queue.clone());
    let request = shell::shell::drives::consent_request(&drive, format!("shots-{preset}"));
    // Nobody answers a screenshot; the dropped receiver reads as "Not now".
    let _ = queue.enqueue_drive_consent(request);
    driver.host_mut().pump_portal_dialogs();
}

/// `Action::SeedWifiPrompt` and `Action::SelectGroup` — split out of
/// [`apply`] to keep that dispatcher short.
fn apply_seed_wifi_and_select_group(driver: &mut ShellDriver, action: &Action) {
    match action {
        Action::SeedWifiPrompt(preset) => {
            let preset = *preset;
            driver.with_chrome(|st: &mut ChromeState| {
                use shell::shell::quick_settings::{
                    QsLevel, QsSection, WifiNetwork, WifiPasswordPrompt,
                };
                st.overlays.quick_settings = true;
                st.quick_settings.nav_stack = vec![QsLevel::Root];
                st.quick_settings.section = Some(QsSection::Wifi);
                st.quick_settings.wifi_networks = vec![
                    WifiNetwork {
                        ssid: "Cafe WiFi".into(),
                        signal: 3,
                        secure: true,
                        security: os_actions::WifiSecurity::Wpa2,
                        connected: false,
                        known: false,
                    },
                    WifiNetwork {
                        ssid: "Airport Free".into(),
                        signal: 2,
                        secure: false,
                        security: os_actions::WifiSecurity::Open,
                        connected: false,
                        known: false,
                    },
                ];
                let mut p = WifiPasswordPrompt::new("Cafe WiFi");
                if preset == "rejected" {
                    p.error = Some("Couldn't join. Check the password.".into());
                }
                st.quick_settings.wifi_password = Some(p);
                st.bump();
            });
            match preset {
                "typed" => driver
                    .host_mut()
                    .seed_wifi_password_for_tests("hunter2pass", false),
                "rejected" => driver
                    .host_mut()
                    .seed_wifi_password_for_tests("wrongpass", true),
                _ => {}
            }
        }
        Action::SelectGroup(idx) => {
            let idx = *idx;
            driver.with_chrome(|st: &mut ChromeState| {
                {
                    let mut tree = st.tree_mut();
                    let Some(strip_id) = tree
                        .desktop
                        .active_workspace()
                        .and_then(|ws| ws.strips.first().map(|s| s.id))
                    else {
                        return;
                    };
                    match idx {
                        Some(i) => {
                            let gid = tree
                                .desktop
                                .active_workspace()
                                .and_then(|ws| ws.strips.first())
                                .and_then(|s| s.window_groups.get(i).map(|g| g.id));
                            if let Some(gid) = gid {
                                let active = tree.desktop.active;
                                tree.desktop.active_strip.insert(active, strip_id);
                                tree.desktop.set_active_group(strip_id, gid);
                            }
                        }
                        None => {
                            tree.desktop.active_group.remove(&strip_id);
                        }
                    }
                }
                st.bump();
            });
        }
        Action::CloseFocusedWindow => {
            driver.with_chrome(|st: &mut ChromeState| {
                if let Some(tab) = shell::shell::window_actions::active_tab_id(st) {
                    let _ = shell::shell::window_actions::close_tab(st, tab);
                }
                st.bump();
            });
        }
        _ => {}
    }
}

/// `Action::LoneWindow`, `Action::SwitchWorkspace` and `Action::PushQsLevel`,
/// split out of [`apply`] to keep that dispatcher short.
fn apply_window_and_workspace_actions(driver: &mut ShellDriver, action: &Action) {
    match action {
        Action::CreateStrip(name) => driver.with_chrome(|st: &mut ChromeState| {
            if let Err(error) = st.create_strip(name) {
                eprintln!("scenario: could not create strip {name}: {error}");
            }
        }),
        Action::FocusStrip(index) => driver.with_chrome(|st: &mut ChromeState| {
            st.focus_strip_index(*index);
            st.bump();
        }),
        Action::LoneWindow(idx) => {
            let idx = *idx;
            driver.with_chrome(|st: &mut ChromeState| {
                {
                    let mut tree = st.tree_mut();
                    if let Some(ws) = tree.desktop.active_workspace_mut()
                        && let Some(g) = ws
                            .strips
                            .first_mut()
                            .and_then(|s| s.window_groups.get_mut(idx))
                    {
                        g.tabs.truncate(1);
                        if let Some(t) = g.tabs.first() {
                            g.active_tab = t.id;
                        }
                    }
                }
                st.bump();
            });
        }
        Action::SeedLongNotes(group_idx) => {
            let target = {
                let chrome = driver.chrome();
                let tree = chrome.tree();
                tree.desktop.active_workspace().and_then(|workspace| {
                    let strip = workspace.strips.first()?;
                    let group = strip.window_groups.get(*group_idx)?;
                    Some((strip.id, workspace.room().to_string(), group.active_tab))
                })
            };
            let inputs = driver.chrome().hosted_inputs().clone();
            if let Some((strip_id, room, tab)) = target {
                let body = long_note_body();
                match inputs.mount_note(&room, tab, "Complete capture.md", &body) {
                    Some(Ok(component)) => {
                        match tilo_desktop::resolver::app_tile(component, None, |_| {}) {
                            Ok(app) => {
                                if let Some(host) = driver
                                    .host_mut()
                                    .group_at_for_tests(strip_id, *group_idx)
                                    .and_then(|group| group.active_host_mut())
                                {
                                    host.replace_content(ui::widget::TileContent::App(app));
                                }
                                driver.relayout();
                            }
                            Err(error) => eprintln!("shots: the long note did not mount: {error}"),
                        }
                    }
                    Some(Err(error)) => eprintln!("shots: the long note did not mount: {error}"),
                    None => eprintln!("shots: this host mounts no Notes"),
                }
            }
        }
        Action::SeedNoteHistory(group_idx) => seed_note_history(driver, *group_idx),
        Action::SwitchWorkspace(idx) => {
            driver.with_chrome(|st: &mut ChromeState| {
                let mut tree = st.tree_mut();
                if let Some(ws) = tree.desktop.workspaces.get(*idx) {
                    let id = ws.id;
                    tree.set_active(id);
                    drop(tree);
                    st.bump();
                }
            });
        }
        Action::PushQsLevel(level) => {
            let lvl = level.clone();
            driver.with_chrome(|st| {
                st.overlays.quick_settings = true;
                st.quick_settings.push_level(lvl);
            });
        }
        _ => {}
    }
}

/// The [`AppKind`](shell::shell::state::AppKind) behind an app name written the way a tab pill spells
/// it. Inverse of `AppKind::as_str`, so a scenario names apps the way the
/// screen does rather than the way the enum does.
///
/// Unknown names fall through to `AppKind::Other`, which is the same
/// thing the persisted tree carries for an app the shell has no built-in
/// widget for — it renders as a stub card rather than panicking.
fn app_kind_for(app: &str) -> shell::shell::state::AppKind {
    use shell::shell::state::AppKind;
    if app.eq_ignore_ascii_case("Terminal") {
        AppKind::Terminal
    } else if app.eq_ignore_ascii_case("Files") || app.eq_ignore_ascii_case("FileExplorer") {
        AppKind::FileExplorer
    } else if app.eq_ignore_ascii_case("Browser") {
        AppKind::Browser
    } else if app.eq_ignore_ascii_case("Settings") {
        AppKind::Settings
    } else {
        AppKind::Other(app.to_string())
    }
}

// =====================================================================
// JSON state dumping for headless mode
// =====================================================================

/// Serialise the chrome's externally-observable state into a compact
/// JSON object.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the shell fixture owns long-lived permits by design and lives for the whole run"
)]
pub fn chrome_state_json(driver: &ShellDriver) -> String {
    let st = driver.chrome();
    let host = driver.host();
    let guard = st.tree();
    let tree = &guard.desktop;
    let active = tree.active.raw();
    let active_idx = tree.workspaces.iter().position(|w| w.id == tree.active);
    let workspace_names: Vec<String> = tree.workspaces.iter().map(|w| w.name.clone()).collect();
    // Nothing past this point reads the tree; `st` (a separate guard) is
    // still needed below.
    drop(guard);
    let qs_level = match st.quick_settings.current_level() {
        QsLevel::Root => "root",
        QsLevel::BluetoothDetails(_) => "bluetooth-details",
        QsLevel::Notifications => "notifications",
    };
    let ws_tab = match st.ws_settings_tab {
        WsTab::General => "general",
        WsTab::Members => "members",
        WsTab::Agents => "agents",
        WsTab::Permissions => "permissions",
        WsTab::Apps => "apps",
        WsTab::Emoji => "emoji",
    };
    let palette_visible = host.palette_visible();
    let palette_query = host.palette_query();
    // `[[[group_id, [tab_id, ...]], ...], ...]`, for the drag-out tests.
    let groups_per_strip: Vec<Vec<serde_json::Value>> = host
        .groups_per_strip()
        .iter()
        .map(|(_strip, groups)| {
            groups
                .iter()
                .map(|(group, tabs)| serde_json::json!([group, tabs]))
                .collect()
        })
        .collect();
    serde_json::json!({
        "scenario_step": "",
        "active_workspace_id": active,
        "active_workspace_idx": active_idx.unwrap_or(0),
        "workspace_names": workspace_names,
        "overlay_overview": st.overlays.overview,
        "overlay_quick_settings": st.overlays.quick_settings,
        "overlay_ws_settings": st.overlays.ws_settings,
        "overlay_collaborators": st.overlays.collaborators,
        "overlay_room_activity": st.overlays.room_activity,
        "qs_level": qs_level,
        "qs_section": format!("{:?}", st.quick_settings.section),
        "ws_settings_tab": ws_tab,
        "palette_visible": palette_visible,
        "palette_query": palette_query,
        "revision": st.revision,
        "groups_per_strip": groups_per_strip,
        // Where each window is drawn and where the pointer is, so a resize
        // can be read off a capture to the pixel.
        "tab_tiles": host
            .tab_tiles()
            .iter()
            .map(|(tab, r)| serde_json::json!([tab, r.x0, r.y0, r.x1, r.y1]))
            .collect::<Vec<_>>(),
        "strip_scroll": host
            .strip_scroll_offsets()
            .iter()
            .map(|(strip, scroll, max)| serde_json::json!([strip, scroll, max]))
            .collect::<Vec<_>>(),
        "pointer": [driver.cursor().x, driver.cursor().y],
    })
    .to_string()
}

/// `SnapshotSink` that writes one `<name>.json` per snapshot under
/// `dir`. Used by headless mode.
pub struct JsonDirSink<'a> {
    pub dir: &'a Path,
    pub written: RefCell<Vec<String>>,
}

impl<'a> JsonDirSink<'a> {
    pub fn new(dir: &'a Path) -> Self {
        Self {
            dir,
            written: RefCell::new(Vec::new()),
        }
    }
}

impl SnapshotSink for JsonDirSink<'_> {
    fn capture(&mut self, driver: &mut ShellDriver, name: &str) {
        let path = self.dir.join(format!("{name}.json"));
        let json = chrome_state_json(driver);
        // `capture` has no `Result` to propagate through (the `SnapshotSink`
        // trait's callers are a fixed scenario-replay loop with nothing
        // useful to do with a per-snapshot error) — report and skip this
        // one file rather than aborting the whole capture run over it.
        if let Err(e) = std::fs::write(&path, json) {
            eprintln!("scenario: failed to write {}: {e}", path.display());
            return;
        }
        self.written.borrow_mut().push(path.display().to_string());
        println!("  wrote {}", path.display());
    }
}

// =====================================================================
// Built-in catalogue
//
// One table of named action lists. New scenes append a row; the apply
// engine above is the code that stays.

/// Frames to let the eased focus travel settle before a snapshot — the
/// snap curve is 200 ms of real wall time, and a frame sleeps 16 ms.
const PAN_SETTLE_FRAMES: u32 = 16;

/// Bound on [`Action::WaitStripSettled`]: about two seconds of frames, far
/// past any strip spring, so only a strip that never comes to rest reaches it.
const STRIP_SETTLE_MAX_FRAMES: u32 = 120;

/// Window width the strip is held at, in per-mille of the display's
/// usable width. Half a screen each puts two windows in view with a
/// third clipped at the edge.
const HALF_WIDTH_PERMILLE: u32 = 500;

/// Window width, per-mille of the display's usable width, that puts three
/// windows on screen at once. A third of the strip each is the shape a
/// demo takes when it wants several apps visible together, and it is the
/// shape a group's tab strip has the least room in.
const THIRD_WIDTH_PERMILLE: u32 = 333;

/// Points along the pill's 180 ms reveal curve worth looking at. Dense
/// at the start, where the ease-out does most of its travelling.
const REVEAL_SAMPLES: &[u64] = &[0, 20, 40, 60, 90, 130, 180];

/// Points along the make-space spring to render. Evenly spaced, ending
/// past its settle so the last one is the settled layout.
const MAKE_SPACE_SAMPLES: &[u64] = &[0, 60, 120, 180, 240, 300, 360, 420];

/// Points along the 130 ms sibling fade. Dense at the start, where
/// ease-out does most of its travelling.
const SIBLING_FADE_SAMPLES: &[u64] = &[0, 20, 40, 65, 100, 130];

/// Frames to let the motion finish before the settled capture.
const PLACEMENT_SETTLE_FRAMES: u32 = 8;

/// Resolve a [`Action::SeedStatus`] preset name into the reading the
/// poller would have published.
///
/// An unknown name yields the empty reading — which the tray draws as
/// "nothing known yet", the same as a machine whose first poll has not
/// landed. A typo therefore shows up as a visibly empty tray in the
/// capture rather than as a silently wrong one.
fn status_preset(name: &str) -> shell::shell::status::SystemStatus {
    use os_actions::{LinkKind, LinkStatus, VolumeState};
    use shell::shell::status::SystemStatus;
    use tilo_power_mgmt::{BatterySnapshot, BatteryState};

    let wifi = |signal: u8| LinkStatus {
        kind: LinkKind::Wifi,
        name: Some("Cafe WiFi".to_string()),
        signal,
    };
    let vol = |level: f32, muted: bool| VolumeState { level, muted };
    let batt = |percent: u8, state: BatteryState| BatterySnapshot {
        present: true,
        percent,
        state,
    };

    match name {
        "healthy" => SystemStatus {
            battery: Some(batt(72, BatteryState::Discharging)),
            battery_time: None,
            link: Some(wifi(4)),
            volume: Some(vol(0.45, false)),
            media: Vec::new(),
        },
        "charging" => SystemStatus {
            battery: Some(batt(38, BatteryState::Charging)),
            battery_time: None,
            link: Some(wifi(3)),
            volume: Some(vol(0.45, false)),
            media: Vec::new(),
        },
        "low" => SystemStatus {
            battery: Some(batt(14, BatteryState::Discharging)),
            battery_time: None,
            link: Some(wifi(2)),
            volume: Some(vol(0.20, false)),
            media: Vec::new(),
        },
        "critical" => SystemStatus {
            battery: Some(batt(3, BatteryState::Discharging)),
            battery_time: None,
            link: Some(wifi(1)),
            volume: Some(vol(0.20, false)),
            media: Vec::new(),
        },
        "muted" => SystemStatus {
            battery: Some(batt(88, BatteryState::Full)),
            battery_time: None,
            link: Some(wifi(4)),
            volume: Some(vol(0.6, true)),
            media: Vec::new(),
        },
        "offline" => SystemStatus {
            battery: Some(batt(64, BatteryState::Discharging)),
            battery_time: None,
            link: Some(LinkStatus::offline()),
            volume: Some(vol(0.45, false)),
            media: Vec::new(),
        },
        "wired" => SystemStatus {
            battery: None,
            battery_time: None,
            link: Some(LinkStatus {
                kind: LinkKind::Wired,
                name: Some("Wired connection 1".to_string()),
                signal: 0,
            }),
            volume: Some(vol(0.75, false)),
            media: Vec::new(),
        },
        _ => SystemStatus::default(),
    }
}

/// Resolve a [`Action::SeedOsd`] preset into the readout the media-key
/// handler would have raised.
fn osd_preset(name: &str) -> shell::shell::osd::Osd {
    use shell::shell::osd::{Osd, Transport};
    match name {
        "volume" => Osd::volume(0.65, false),
        "volume_low" => Osd::volume(0.10, false),
        "volume_muted" => Osd::volume(0.65, true),
        "brightness" => Osd::brightness(0.40),
        "keyboard_backlight" => Osd::keyboard_backlight(0.5),
        "keyboard_backlight_off" => Osd::keyboard_backlight(0.0),
        "mic_muted" => Osd::mic_mute(true),
        "no_backlight" => Osd::no_backlight(),
        "next_track" => Osd::transport(Transport::Next),
        _ => Osd::volume(0.5, false),
    }
}

/// The desk widgets through a working minute: at rest, typing, a pause, a
/// pass along the edge and a reach for it, the caret near a widget, and
/// the Super surface tapped, typed into and held. A pause is three real
/// seconds of frames.
fn desk_widgets_scenario() -> Scenario {
    use shell::shell::desk_widgets::GLANCE_AFTER_PAUSE;
    use shell::shell::input_policy::SuperGesture;
    // A pause of a few seconds is a few hundred 16 ms frames, and four more
    // so the pause has certainly passed.
    let pause_frames = u32::try_from(GLANCE_AFTER_PAUSE.as_millis() / 16)
        .unwrap_or(u32::MAX)
        .saturating_add(4);
    let along_edge: Vec<Point> = (0..30)
        .map(|k| Point::new(1590.0, 120.0 + 22.0 * f64::from(k)))
        .collect();
    let reach: Vec<Point> = [0.0, 0.45, 0.75, 0.92, 0.99, 1.0, 1.0]
        .into_iter()
        .map(|f| Point::new(800.0 + (1568.0 - 800.0) * f, 520.0 - 70.0 * f))
        .collect();
    Scenario {
        name: "desk_widgets",
        description: "Desk widgets: glance at rest, away while typing, back after a pause, transit vs reach at the edge, the caret rule, and the Super surface tapped and held.",
        actions: vec![
            Action::SeedDesk,
            Action::MoveCursor(Point::new(700.0, 420.0)),
            Action::Wait(20),
            Action::Relayout,
            Action::Snapshot("01-glance-at-rest"),
            Action::DeskTyping,
            Action::SnapshotSequence {
                prefix: "02-typing-away",
                frames: 10,
                gap_frames: 1,
            },
            Action::Wait(pause_frames.saturating_sub(12)),
            Action::SnapshotSequence {
                prefix: "03-pause-glance",
                frames: 14,
                gap_frames: 1,
            },
            Action::DeskTyping,
            Action::Wait(12),
            Action::PointerPath(along_edge),
            Action::Snapshot("04-transit-along-edge-stays-away"),
            Action::PointerPath(reach),
            Action::SnapshotSequence {
                prefix: "05-reach-glances",
                frames: 12,
                gap_frames: 1,
            },
            Action::MoveCursor(Point::new(700.0, 420.0)),
            Action::Wait(pause_frames),
            Action::SetTextCaret(Some(vello::kurbo::Rect::new(1440.0, 846.0, 1442.0, 866.0))),
            Action::Wait(16),
            Action::Snapshot("06-caret-near-music-keeps-it-away"),
            Action::SetTextCaret(None),
            Action::Wait(16),
            Action::Super(SuperGesture::Tap),
            Action::SnapshotSequence {
                prefix: "07-super-tap",
                frames: 12,
                gap_frames: 1,
            },
            Action::Snapshot("08-super-surface"),
            Action::Type("not"),
            Action::Wait(4),
            Action::Snapshot("09-super-typed"),
            Action::Key(Key::Named(NamedKey::Escape)),
            Action::Wait(20),
            Action::Snapshot("10-closed"),
            Action::Super(SuperGesture::Peek),
            Action::Wait(14),
            Action::Snapshot("11-super-peek-quiet-stage"),
            Action::Super(SuperGesture::PeekFull),
            Action::Wait(14),
            Action::Snapshot("11b-super-peek-grown"),
            Action::Super(SuperGesture::PeekReleased),
            Action::SnapshotSequence {
                prefix: "12-peek-released",
                frames: 10,
                gap_frames: 1,
            },
        ],
    }
}

/// Quick Settings appearing from the tray, opening Wi-Fi's networks in place
/// and closing them, and leaving: frame by frame at 90 Hz on the toolkit's
/// clock.
fn motion_quick_settings_scenario() -> Scenario {
    Scenario {
        name: "motion_quick_settings",
        description: "Quick Settings appears from the tray, opens Wi-Fi's networks in place and closes them, and leaves, at 90 Hz (needs --features test-support).",
        actions: vec![
            Action::MotionFrames {
                prefix: "qs-open",
                cue: MotionCue::OpenQuickSettings,
                frames: 26,
                every_ms: 11,
            },
            Action::MotionFrames {
                prefix: "qs-expand",
                cue: MotionCue::QuickSettingsSection(Some(
                    shell::shell::quick_settings::QsSection::Wifi,
                )),
                frames: 30,
                every_ms: 11,
            },
            Action::MotionFrames {
                prefix: "qs-collapse",
                cue: MotionCue::QuickSettingsSection(None),
                frames: 26,
                every_ms: 11,
            },
            Action::MotionFrames {
                prefix: "qs-close",
                cue: MotionCue::CloseQuickSettings,
                frames: 16,
                every_ms: 11,
            },
        ],
    }
}

/// The workspace overview opening and closing over a workspace of three
/// strips (one of them empty): every window flies from its place on its
/// strip into the overview's picture and back, frame by frame at 90 Hz on
/// the toolkit's clock. Super+Tab flies them at `fast`; "Overview" in the
/// workspace menu is a whole-view change at `slow`.
fn motion_overview_scenario() -> Scenario {
    use ui::tokens::MotionSource::{Keyboard, Pointer};
    Scenario {
        name: "motion_overview",
        description: "The overview opens and closes over three strips, by Super+Tab and by the workspace menu, at 90 Hz (needs --features test-support).",
        actions: vec![
            Action::OpenHostedWindow {
                app: "Terminal",
                title: "Terminal",
            },
            Action::CreateStrip("Research"),
            Action::OpenHostedWindow {
                app: "Files",
                title: "Files",
            },
            Action::OpenHostedWindow {
                app: "Editor",
                title: "Draft",
            },
            Action::CreateStrip("Later"),
            Action::FocusStrip(0),
            Action::Wait(12),
            Action::Snapshot("overview-desktop"),
            Action::MotionFrames {
                prefix: "overview-key-open",
                cue: MotionCue::Overview {
                    open: true,
                    source: Keyboard,
                },
                frames: 18,
                every_ms: 11,
            },
            Action::MotionFrames {
                prefix: "overview-key-close",
                cue: MotionCue::Overview {
                    open: false,
                    source: Keyboard,
                },
                frames: 18,
                every_ms: 11,
            },
            Action::MotionFrames {
                prefix: "overview-click-open",
                cue: MotionCue::Overview {
                    open: true,
                    source: Pointer,
                },
                frames: 44,
                every_ms: 11,
            },
            Action::MotionFrames {
                prefix: "overview-click-close",
                cue: MotionCue::Overview {
                    open: false,
                    source: Pointer,
                },
                frames: 30,
                every_ms: 11,
            },
        ],
    }
}

/// The Notes window's sidebar collapsing and expanding by a click on its
/// toggle: frame by frame at 90 Hz on the toolkit's clock.
fn motion_sidebar_scenario() -> Scenario {
    Scenario {
        name: "motion_sidebar",
        description: "A window's sidebar collapses and expands from its toggle, at 90 Hz (needs --features test-support).",
        actions: vec![
            // Focus the Notes window first, as a person's first click does.
            Action::ClickAt(Point::new(900.0, 700.0)),
            Action::Wait(4),
            Action::Snapshot("sidebar-rest"),
            Action::MotionFrames {
                prefix: "sidebar-collapse",
                // The Notes window's sidebar toggle, in its header.
                cue: MotionCue::ClickAt(Point::new(60.0, 96.0)),
                frames: 26,
                every_ms: 11,
            },
            Action::MotionFrames {
                prefix: "sidebar-expand",
                // The Notes window's sidebar toggle, in its header.
                cue: MotionCue::ClickAt(Point::new(60.0, 96.0)),
                frames: 26,
                every_ms: 11,
            },
        ],
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the shell screenshot catalogue is one data table of action lists; splitting it would only relocate rows"
)]
fn build_scenarios() -> Vec<Scenario> {
    vec![
        window_bar_browser_scenario(),
        window_bar_tabbed_scenario(),
        window_bar_files_scenario(),
        window_bar_terminal_scenario(),
        window_bar_writer_scenario(),
        window_bar_reactions::window_bar_reactions(),
        window_bar_overflow_scenario(),
        window_bar_suggestions_scenario(),
        desk_widgets_scenario(),
        motion_quick_settings_scenario(),
        motion_overview_scenario(),
        motion_sidebar_scenario(),
        Scenario {
            name: "personal_workspace_on_laptop",
            description: "The person's Research workspace as the laptop that made it shows it, before and after splitting off the outline.",
            actions: vec![
                Action::SeedOwnWorkspace,
                Action::Wait(20),
                Action::Relayout,
                Action::Snapshot("01-laptop-research"),
                Action::SplitOwnOutline,
                Action::Wait(120),
                Action::Relayout,
                Action::Snapshot("02-laptop-outline-split"),
            ],
        },
        Scenario {
            name: "personal_workspace_on_board",
            description: "The same Research workspace arriving on the person's Orange Pi from the laptop, and following the laptop's split.",
            actions: vec![
                Action::Snapshot("00-board-before"),
                Action::SeedOtherOwnDevice,
                Action::ReceiveFromOtherOwnDevice,
                Action::Wait(20),
                Action::Relayout,
                Action::Snapshot("01-board-research-arrived"),
                Action::OtherOwnDeviceSplitsOutline,
                Action::ReceiveFromOtherOwnDevice,
                Action::Wait(120),
                Action::Relayout,
                Action::Snapshot("02-board-outline-split"),
            ],
        },
        Scenario {
            name: "host_desktop_first_login",
            description: "Render the host-session identity setup exactly as the compositor shows it.",
            actions: vec![
                Action::ShowHostDesktopOnboarding,
                Action::Wait(4),
                Action::SetOnboardingVisual(shell::onboarding_view::OnboardingVisualState::Intro),
                Action::Relayout,
                Action::Snapshot("01-identity-introduction"),
                Action::SetOnboardingVisual(
                    shell::onboarding_view::OnboardingVisualState::Existing,
                ),
                Action::Relayout,
                Action::Snapshot("02-use-existing"),
                Action::SetOnboardingVisual(shell::onboarding_view::OnboardingVisualState::Nearby),
                Action::Relayout,
                Action::Snapshot("02a-nearby-waiting"),
                Action::SetOnboardingVisual(
                    shell::onboarding_view::OnboardingVisualState::NearbyCode,
                ),
                Action::Relayout,
                Action::Snapshot("02b-nearby-code"),
                Action::SetOnboardingVisual(shell::onboarding_view::OnboardingVisualState::Restore),
                Action::Relayout,
                Action::Snapshot("02c-restore-backup"),
                Action::SetOnboardingVisual(
                    shell::onboarding_view::OnboardingVisualState::RestoreError,
                ),
                Action::Relayout,
                Action::Snapshot("03-restore-error"),
                Action::SetOnboardingVisual(shell::onboarding_view::OnboardingVisualState::Protect),
                Action::Relayout,
                Action::Snapshot("04-protect-identity"),
                Action::SetOnboardingVisual(
                    shell::onboarding_view::OnboardingVisualState::CreateError,
                ),
                Action::Relayout,
                Action::Snapshot("05-create-error"),
                Action::SetOnboardingVisual(shell::onboarding_view::OnboardingVisualState::Backup),
                Action::Relayout,
                Action::Snapshot("06-encrypted-backup"),
                Action::SetOnboardingVisual(
                    shell::onboarding_view::OnboardingVisualState::BackupVerified,
                ),
                Action::Relayout,
                Action::Snapshot("07-backup-verified"),
                Action::SetOnboardingVisual(shell::onboarding_view::OnboardingVisualState::Ready),
                Action::Relayout,
                Action::Snapshot("08-ready"),
                Action::SetOnboardingVisual(
                    shell::onboarding_view::OnboardingVisualState::TourNavigate,
                ),
                Action::Relayout,
                Action::Snapshot("09-tour-navigate"),
                Action::SetOnboardingVisual(
                    shell::onboarding_view::OnboardingVisualState::TourOrganise,
                ),
                Action::Relayout,
                Action::Snapshot("10-tour-organise"),
                Action::SetOnboardingVisual(
                    shell::onboarding_view::OnboardingVisualState::TourCollaborate,
                ),
                Action::Relayout,
                Action::Snapshot("11-tour-collaborate"),
                Action::SetOnboardingVisual(
                    shell::onboarding_view::OnboardingVisualState::TourStart,
                ),
                Action::Relayout,
                Action::Snapshot("12-tour-start"),
            ],
        },
        Scenario {
            name: "now_playing_cards",
            description: "What plays, as Quick Settings and the lock screen show it.",
            actions: vec![
                Action::Step(film::Step::NowPlaying),
                Action::OpenQuickSettings,
                Action::Wait(30),
                Action::Relayout,
                Action::Snapshot("01-quick-settings"),
                Action::SetLockVisual(super::driver::LockVisualState::NowPlaying),
                Action::Snapshot("02-lock-screen"),
            ],
        },
        Scenario {
            name: "lock_screen_states",
            description: "Render settled production lock-screen states without locking the live session.",
            actions: vec![
                Action::SetLockVisual(super::driver::LockVisualState::Default),
                Action::Snapshot("01-default"),
                Action::SetLockVisual(super::driver::LockVisualState::Password),
                Action::Snapshot("02-password-entered"),
                Action::SetLockVisual(super::driver::LockVisualState::Verifying),
                Action::Snapshot("03-verifying"),
                Action::SetLockVisual(super::driver::LockVisualState::Error),
                Action::Snapshot("04-auth-error"),
                Action::SetLockVisual(super::driver::LockVisualState::Notifications),
                Action::Snapshot("05-notification-count"),
            ],
        },
        Scenario {
            name: "capture_studio",
            description: "The capture bar over the desktop: the output, the target, the shutter and the options, see-through.",
            actions: vec![
                Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
                Action::Key(Key::Character("s".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::Wait(12),
                Action::MoveCursor(Point::new(700.0, 300.0)),
                Action::Snapshot("capture-open"),
                Action::Drag {
                    from: Point::new(360.0, 220.0),
                    to: Point::new(980.0, 590.0),
                    steps: 8,
                },
                Action::Snapshot("capture-selection"),
                Action::MoveCursorNamed("capture.shutter"),
                Action::WaitWall(600),
                Action::Snapshot("capture-shutter-tooltip"),
                Action::ClickNamed("capture.options"),
                Action::WaitWall(300),
                Action::Wait(4),
                Action::Snapshot("capture-options"),
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::ClickNamed("capture.output.recording"),
                Action::Snapshot("capture-recording-selection"),
                Action::ClickNamed("capture.output.pip"),
                Action::Snapshot("capture-pip-selection"),
                Action::ClickNamed("capture.target.screen"),
                Action::Snapshot("capture-pip-screen"),
                Action::ClickNamed("capture.target.strip"),
                Action::Snapshot("capture-pip-strip"),
            ],
        },
        Scenario {
            name: "capture_window",
            description: "Screenshot of a window: only the picked window is outlined, the rest dimmed; moving focus the usual way picks the next one.",
            actions: vec![
                Action::LoneWindow(0),
                Action::SelectGroup(None),
                Action::OpenWindow {
                    app_id: "org.example.current",
                    surface_id: 5441,
                },
                Action::Wait(20),
                Action::SetWindowWidths(700),
                Action::SelectGroup(Some(0)),
                Action::Wait(24),
                Action::Relayout,
                Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
                Action::Key(Key::Character("s".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::Wait(12),
                Action::ClickNamed("capture.target.window"),
                Action::MoveCursor(Point::new(700.0, 300.0)),
                Action::Snapshot("window-picked"),
                Action::SelectGroup(Some(1)),
                Action::Wait(40),
                Action::Relayout,
                Action::MoveCursor(Point::new(700.0, 300.0)),
                Action::Snapshot("window-picked-after-focus-move"),
            ],
        },
        Scenario {
            name: "capture_frozen",
            description: "A screenshot freezes the windows when it opens: a client that draws meanwhile still shows what it showed, the strip still moves, and closing shows what it drew.",
            actions: vec![
                Action::LoneWindow(0),
                Action::SelectGroup(None),
                Action::OpenWindow {
                    app_id: "org.example.current",
                    surface_id: 5451,
                },
                Action::Wait(20),
                Action::SetWindowWidths(700),
                Action::SelectGroup(Some(0)),
                Action::Wait(24),
                Action::Relayout,
                Action::CaptureExecutionEnabled(true),
                Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
                Action::Key(Key::Character("s".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::Wait(12),
                Action::ClientCommit {
                    surface_id: 5451,
                    rgba: [0xD8, 0x3A, 0x4A, 0xFF],
                },
                Action::Wait(8),
                Action::MoveCursor(Point::new(700.0, 300.0)),
                Action::Snapshot("frozen-after-client-drew"),
                Action::ClickNamed("capture.target.window"),
                Action::SelectGroup(Some(1)),
                Action::Wait(40),
                Action::Relayout,
                Action::MoveCursor(Point::new(700.0, 300.0)),
                Action::Snapshot("frozen-window-picked-after-focus-move"),
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::Wait(8),
                Action::Snapshot("released-after-close"),
            ],
        },
        Scenario {
            name: "capture_frozen_terminal",
            description: "A screenshot opened while the terminal prints: the terminal holds still at the line it showed when the screenshot opened while it goes on printing, and closing shows what it printed meanwhile.",
            actions: vec![
                Action::OpenHostedWindow {
                    app: "Terminal",
                    title: "Terminal",
                },
                Action::Wait(24),
                Action::TerminalPrints { from: 1, to: 40 },
                Action::Wait(4),
                Action::CaptureExecutionEnabled(true),
                Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
                Action::Key(Key::Character("s".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::Wait(4),
                Action::Snapshot("terminal-held-as-opened"),
                Action::TerminalPrints { from: 41, to: 120 },
                Action::Wait(4),
                Action::Snapshot("terminal-still-held-while-printing"),
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::Wait(4),
                Action::Snapshot("terminal-released-after-close"),
            ],
        },
        Scenario {
            name: "capture_two_selections",
            description: "Shift and a new drag add a second selection; each carries its own remove; a plain drag replaces them.",
            actions: vec![
                Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
                Action::Key(Key::Character("s".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::Wait(12),
                Action::Drag {
                    from: Point::new(180.0, 170.0),
                    to: Point::new(500.0, 350.0),
                    steps: 7,
                },
                Action::Snapshot("selection-one"),
                Action::Modifiers(ModifiersState::SHIFT),
                Action::Drag {
                    from: Point::new(620.0, 400.0),
                    to: Point::new(980.0, 620.0),
                    steps: 7,
                },
                Action::Modifiers(ModifiersState::empty()),
                Action::MoveCursor(Point::new(1_200.0, 200.0)),
                Action::Snapshot("selection-two"),
                Action::MoveCursorNamed("capture.selection.0.remove"),
                Action::WaitWall(600),
                Action::Snapshot("selection-remove-hover"),
                Action::ClickNamed("capture.selection.0.remove"),
                Action::Snapshot("selection-removed"),
                Action::Drag {
                    from: Point::new(300.0, 500.0),
                    to: Point::new(520.0, 680.0),
                    steps: 7,
                },
                Action::Snapshot("selection-replaced"),
            ],
        },
        Scenario {
            name: "capture_selection_editing",
            description: "A screenshot selection is moved, resized from its handle, and nudged precisely without being redrawn.",
            actions: vec![
                Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
                Action::Key(Key::Character("s".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::Wait(12),
                Action::Drag {
                    from: Point::new(360.0, 220.0),
                    to: Point::new(980.0, 590.0),
                    steps: 8,
                },
                Action::Snapshot("selection-initial"),
                Action::MoveCursorNamed("capture.selection.0.move"),
                Action::MousePress,
                Action::MoveCursor(Point::new(760.0, 355.0)),
                Action::Snapshot("selection-moving"),
                Action::MouseRelease,
                Action::Snapshot("selection-moved"),
                Action::MoveCursorNamed("capture.selection.0.se"),
                Action::MousePress,
                Action::MoveCursor(Point::new(1_160.0, 690.0)),
                Action::Snapshot("selection-resizing"),
                Action::MouseRelease,
                Action::Snapshot("selection-resized"),
                Action::Modifiers(ModifiersState::SHIFT),
                Action::Key(Key::Named(NamedKey::ArrowLeft)),
                Action::Modifiers(ModifiersState::empty()),
                Action::Snapshot("selection-nudged-10px"),
            ],
        },
        Scenario {
            name: "capture_countdown",
            description: "A delayed screenshot counts down in the bar, which shrinks to the shutter and the seconds left.",
            actions: vec![
                Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
                Action::Key(Key::Character("s".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::Wait(12),
                Action::ClickNamed("capture.target.screen"),
                Action::ClickNamed("capture.options"),
                Action::WaitWall(300),
                Action::Wait(4),
                Action::ClickNamed("capture.option.delay-3"),
                Action::ClickNamed("capture.shutter"),
                Action::Snapshot("countdown-live"),
            ],
        },
        Scenario {
            name: "capture_screenshot_receipt",
            description: "Selected pixels collapse into a visual, non-modal screenshot receipt.",
            actions: vec![
                Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
                Action::Key(Key::Character("s".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::Wait(12),
                Action::Drag {
                    from: Point::new(360.0, 220.0),
                    to: Point::new(980.0, 590.0),
                    steps: 8,
                },
                Action::CaptureScreenshotFinished {
                    saved: true,
                    copied: true,
                },
                Action::CaptureScreenshotTransition(0),
                Action::Snapshot("receipt-enter-000"),
                Action::CaptureScreenshotTransition(500),
                Action::Snapshot("receipt-enter-500"),
                Action::CaptureScreenshotTransition(1_000),
                Action::Snapshot("receipt-enter-1000"),
                Action::MoveCursorNamed("capture.screenshot-thumbnail"),
                Action::Snapshot("receipt-thumbnail-hover"),
                Action::MoveCursorNamed("capture.edit-screenshot"),
                Action::Snapshot("receipt-edit-hover"),
                Action::ClickNamed("capture.dismiss-screenshot"),
                Action::Snapshot("receipt-dismissed"),
            ],
        },
        Scenario {
            name: "capture_screenshot_copied_receipt",
            description: "A copied screenshot settles into one uncrowded status line.",
            actions: vec![
                Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
                Action::Key(Key::Character("s".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::Wait(12),
                Action::Drag {
                    from: Point::new(180.0, 170.0),
                    to: Point::new(620.0, 540.0),
                    steps: 8,
                },
                Action::CaptureScreenshotFinished {
                    saved: false,
                    copied: true,
                },
                Action::CaptureScreenshotTransition(1_000),
                Action::Snapshot("receipt-copied"),
            ],
        },
        Scenario {
            name: "capture_every_target",
            description: "A screenshot of each target the capture tool offers, through the production executor: a selection inside a window, a selection across windows, a window, the screen and the strip.",
            actions: vec![
                Action::LoneWindow(0),
                Action::SelectGroup(None),
                Action::OpenWindow {
                    app_id: "org.example.current",
                    surface_id: 5451,
                },
                Action::Wait(20),
                Action::SelectGroup(None),
                Action::OpenWindow {
                    app_id: "org.example.next",
                    surface_id: 5452,
                },
                Action::Wait(20),
                Action::SetWindowWidths(700),
                Action::SelectGroup(Some(1)),
                Action::Wait(24),
                Action::Relayout,
                Action::CaptureExecutionEnabled(true),
                Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
                Action::Key(Key::Character("s".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::Wait(12),
                Action::Drag {
                    from: Point::new(120.0, 160.0),
                    to: Point::new(420.0, 420.0),
                    steps: 6,
                },
                Action::ClickNamed("capture.shutter"),
                Action::Wait(2),
                Action::CaptureScreenshotTransition(1_000),
                Action::Wait(2),
                Action::Snapshot("every-target-selection-in-window"),
                Action::ClickNamed("capture.dismiss-screenshot"),
                Action::Wait(2),
                Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
                Action::Key(Key::Character("s".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::Wait(12),
                Action::Drag {
                    from: Point::new(600.0, 160.0),
                    to: Point::new(1_100.0, 420.0),
                    steps: 6,
                },
                Action::ClickNamed("capture.shutter"),
                Action::Wait(2),
                Action::CaptureScreenshotTransition(1_000),
                Action::Wait(2),
                Action::Snapshot("every-target-screen-region"),
                Action::ClickNamed("capture.dismiss-screenshot"),
                Action::Wait(2),
                Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
                Action::Key(Key::Character("s".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::Wait(12),
                Action::ClickNamed("capture.target.window"),
                Action::ClickNamed("capture.shutter"),
                Action::Wait(2),
                Action::CaptureScreenshotTransition(1_000),
                Action::Wait(2),
                Action::Snapshot("every-target-window"),
                Action::ClickNamed("capture.dismiss-screenshot"),
                Action::Wait(2),
                Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
                Action::Key(Key::Character("s".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::Wait(12),
                Action::ClickNamed("capture.target.screen"),
                Action::ClickNamed("capture.shutter"),
                Action::Wait(2),
                Action::CaptureScreenshotTransition(1_000),
                Action::Wait(2),
                Action::Snapshot("every-target-screen"),
                Action::ClickNamed("capture.dismiss-screenshot"),
                Action::Wait(2),
                Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
                Action::Key(Key::Character("s".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::Wait(12),
                Action::ClickNamed("capture.target.strip"),
                Action::ClickNamed("capture.shutter"),
                Action::Wait(2),
                Action::CaptureScreenshotTransition(1_000),
                Action::Wait(2),
                Action::Snapshot("every-target-strip"),
                Action::ClickNamed("capture.dismiss-screenshot"),
                Action::Wait(2),
            ],
        },
        Scenario {
            name: "capture_screenshot_markup",
            description: "A real saved capture opens in the native exact-pixel markup workspace.",
            actions: vec![
                Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
                Action::Key(Key::Character("s".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::Wait(12),
                Action::Drag {
                    from: Point::new(320.0, 180.0),
                    to: Point::new(1_180.0, 650.0),
                    steps: 8,
                },
                Action::CaptureExecutionEnabled(true),
                Action::ClickNamed("capture.shutter"),
                Action::Wait(1),
                Action::CaptureScreenshotTransition(1_000),
                Action::ClickNamed("capture.edit-screenshot"),
                Action::Wait(1),
                Action::Relayout,
                Action::ScreenshotMarkupTransition(0),
                Action::Snapshot("markup-enter-000"),
                Action::ScreenshotMarkupTransition(250),
                Action::Snapshot("markup-enter-250"),
                Action::ScreenshotMarkupTransition(500),
                Action::Snapshot("markup-enter-500"),
                Action::ScreenshotMarkupTransition(750),
                Action::Snapshot("markup-enter-750"),
                Action::ScreenshotMarkupTransition(1_000),
                Action::Snapshot("markup-clean"),
                Action::MoveCursor(Point::new(500.0, 300.0)),
                Action::MouseWheel { dx: 0.0, dy: 4.0 },
                Action::Snapshot("markup-zoom"),
                Action::ClickNamed("markup.fit"),
                Action::Snapshot("markup-fit"),
                Action::Drag {
                    from: Point::new(300.0, 220.0),
                    to: Point::new(600.0, 360.0),
                    steps: 18,
                },
                Action::Snapshot("markup-pen"),
                Action::ClickNamed("markup.tool"),
                Action::Snapshot("markup-tool-picker"),
                Action::ClickNamed("markup.tool-highlighter"),
                Action::ClickNamed("markup.color-yellow"),
                Action::Drag {
                    from: Point::new(320.0, 400.0),
                    to: Point::new(620.0, 400.0),
                    steps: 18,
                },
                Action::Snapshot("markup-highlighter"),
                Action::ClickNamed("markup.undo"),
                Action::Snapshot("markup-undo"),
                Action::ClickNamed("markup.tool"),
                Action::ClickNamed("markup.tool-arrow"),
                Action::Drag {
                    from: Point::new(280.0, 390.0),
                    to: Point::new(520.0, 250.0),
                    steps: 18,
                },
                Action::Snapshot("markup-arrow"),
                Action::ClickNamed("markup.tool"),
                Action::ClickNamed("markup.tool-rectangle"),
                Action::ClickNamed("markup.color-blue"),
                Action::Drag {
                    from: Point::new(250.0, 270.0),
                    to: Point::new(470.0, 405.0),
                    steps: 18,
                },
                Action::Snapshot("markup-rectangle"),
                Action::ClickNamed("markup.tool"),
                Action::ClickNamed("markup.tool-redact"),
                Action::Drag {
                    from: Point::new(270.0, 318.0),
                    to: Point::new(550.0, 342.0),
                    steps: 12,
                },
                Action::Snapshot("markup-redact"),
                Action::MoveCursorNamed("markup.save"),
                Action::Snapshot("markup-save-hover"),
                Action::ClickNamed("markup.save"),
                Action::Wait(1),
                Action::Relayout,
                Action::Snapshot("markup-saved"),
            ],
        },
        Scenario {
            name: "pending_window",
            description: "An app asked for holds its place from the first frame: the ground \
                          alone, then the skeleton once its window misses the grace period; an \
                          app still being fetched from the registry says how much of it has \
                          arrived once the download reports it.",
            actions: vec![
                Action::LoneWindow(0),
                Action::SelectGroup(None),
                Action::HoldLaunchPlace {
                    app_id: "so.tilo.notes",
                    label: "Notes",
                },
                Action::Wait(1),
                Action::Relayout,
                Action::Snapshot("pending-first-frame"),
                Action::WaitWall(400),
                Action::Relayout,
                Action::Snapshot("pending-skeleton"),
                Action::SelectGroup(None),
                Action::HoldInstallPlace {
                    app_id: "so.tilo.slides",
                    label: "Slides",
                },
                Action::InstallBytes {
                    app_id: "so.tilo.slides",
                    received: 4_200_000,
                    total: 18_000_000,
                },
                Action::WaitWall(400),
                Action::Relayout,
                Action::Snapshot("pending-installing-progress"),
            ],
        },
        Scenario {
            name: "opening_card_room_wait",
            description: "An app asked for in a room still being joined holds its place, which \
                          says it comes from the room, then opens once the room is ready.",
            actions: vec![
                Action::LoneWindow(0),
                Action::SelectGroup(None),
                Action::HoldLaunchPlace {
                    app_id: "so.tilo.notes",
                    label: "Notes",
                },
                Action::HoldLaunchForRoom("so.tilo.notes"),
                Action::Wait(1),
                Action::Relayout,
                Action::Snapshot("room-wait"),
                Action::RoomReady,
                // The room's waiter starts the launch on its own thread.
                Action::WaitWall(200),
                Action::OpenWindow {
                    app_id: "so.tilo.notes",
                    surface_id: 5107,
                },
                Action::WaitSettled,
                Action::Relayout,
                Action::Snapshot("room-ready"),
            ],
        },
        Scenario {
            name: "capture_screenshot_markup_full_app",
            description: "A taller-than-screen window, picked by focusing it, opens in markup whole, then zooms and pans without altering its pixels.",
            actions: vec![
                Action::LoneWindow(0),
                Action::SelectGroup(None),
                Action::OpenWindow {
                    app_id: "org.example.current",
                    surface_id: 5421,
                },
                Action::Wait(20),
                Action::SelectGroup(None),
                Action::OpenWindow {
                    app_id: "org.example.next",
                    surface_id: 5422,
                },
                Action::Wait(20),
                Action::SetWindowWidths(1_000),
                Action::SelectGroup(Some(0)),
                Action::Wait(24),
                Action::Relayout,
                Action::SeedLongNotes(0),
                Action::Wait(2),
                Action::Relayout,
                Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
                Action::Key(Key::Character("s".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::Wait(12),
                Action::ClickNamed("capture.target.window"),
                Action::Snapshot("full-app-window-picked"),
                Action::CaptureExecutionEnabled(true),
                Action::ClickNamed("capture.shutter"),
                Action::Wait(1),
                Action::CaptureScreenshotTransition(1_000),
                Action::ClickNamed("capture.edit-screenshot"),
                Action::Wait(1),
                Action::Relayout,
                Action::ScreenshotMarkupTransition(1_000),
                Action::Snapshot("full-app-markup-fit"),
                Action::MoveCursor(Point::new(800.0, 430.0)),
                Action::MouseWheel { dx: 0.0, dy: 5.0 },
                Action::Snapshot("full-app-markup-zoom"),
                Action::MiddleDrag {
                    from: Point::new(800.0, 430.0),
                    to: Point::new(800.0, 650.0),
                    steps: 18,
                },
                Action::Snapshot("full-app-markup-pan"),
                Action::ClickNamed("markup.fit"),
                Action::Snapshot("full-app-markup-refit"),
            ],
        },
        Scenario {
            name: "capture_recording_options",
            description: "Recording setup: the rare settings, audio among them, live in the options popover.",
            actions: vec![
                Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
                Action::Key(Key::Character("s".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::CaptureAudioAvailable(true),
                Action::Wait(12),
                Action::ClickNamed("capture.output.recording"),
                Action::ClickNamed("capture.target.screen"),
                Action::Snapshot("record-screen-ready"),
                Action::ClickNamed("capture.options"),
                Action::WaitWall(300),
                Action::Wait(4),
                Action::Snapshot("record-options"),
                Action::ClickNamed("capture.option.microphone"),
                Action::ClickNamed("capture.options"),
                Action::WaitWall(300),
                Action::Wait(4),
                Action::Snapshot("record-options-microphone-on"),
            ],
        },
        Scenario {
            name: "capture_recording_active",
            description: "A recording in progress: the bar shrinks to the red stop shutter and the elapsed time, and the saved receipt offers to play it.",
            actions: vec![
                Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
                Action::Key(Key::Character("s".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::Wait(12),
                Action::ClickNamed("capture.output.recording"),
                Action::ClickNamed("capture.target.screen"),
                Action::ClickNamed("capture.shutter"),
                Action::Snapshot("recording-preparing"),
                Action::CaptureRecordingActive(true),
                Action::MoveCursor(Point::new(300.0, 300.0)),
                Action::Snapshot("recording-in-progress"),
                Action::MoveCursorNamed("capture.stop"),
                Action::WaitWall(600),
                Action::Snapshot("recording-stop-hover"),
                Action::MoveCursor(Point::new(300.0, 300.0)),
                Action::CaptureRecordingFinished,
                Action::Snapshot("recording-saved"),
                Action::MoveCursorNamed("capture.play-recording"),
                Action::Snapshot("recording-saved-play-hover"),
                Action::ClickNamed("capture.dismiss-recording"),
                Action::Snapshot("recording-receipt-dismissed"),
            ],
        },
        Scenario {
            name: "capture_recording_window",
            description: "Recording a window: the window is picked by focus and recorded wherever it goes.",
            actions: vec![
                Action::LoneWindow(0),
                Action::SelectGroup(None),
                Action::OpenWindow {
                    app_id: "org.example.current",
                    surface_id: 5401,
                },
                Action::Wait(20),
                Action::SelectGroup(None),
                Action::OpenWindow {
                    app_id: "org.example.next",
                    surface_id: 5402,
                },
                Action::Wait(20),
                Action::SetWindowWidths(1_000),
                Action::SelectGroup(Some(1)),
                Action::Wait(24),
                Action::Relayout,
                Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
                Action::Key(Key::Character("s".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::Wait(12),
                Action::ClickNamed("capture.output.recording"),
                Action::ClickNamed("capture.target.window"),
                Action::Snapshot("record-window-ready"),
                Action::ClickNamed("capture.shutter"),
                Action::CaptureRecordingActive(true),
                Action::Snapshot("record-window-in-progress"),
            ],
        },
        Scenario {
            name: "capture_replay_active",
            description: "Keeping the last 30 seconds: the bar offers Save and the stop shutter; a save offers to play it.",
            actions: vec![
                Action::CaptureReplayActive(true),
                Action::CaptureReplayBuffered(0),
                Action::Snapshot("replay-warming"),
                Action::CaptureReplayBuffered(30),
                Action::Snapshot("replay-ready"),
                Action::MoveCursorNamed("capture.save-replay"),
                Action::Snapshot("replay-save-hover"),
                Action::MoveCursor(Point::new(24.0, 180.0)),
                Action::CaptureReplaySaved,
                Action::Snapshot("replay-saved"),
                Action::MoveCursorNamed("capture.play-recording"),
                Action::Snapshot("replay-saved-play-hover"),
                Action::CaptureReplayActive(false),
                Action::Snapshot("replay-off"),
            ],
        },
        Scenario {
            name: "capture_replay_shortcut",
            description: "The global recording chord opens recording setup, where keeping the last 30 seconds is an option.",
            actions: vec![
                Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
                Action::Key(Key::Character("r".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::Wait(12),
                Action::Snapshot("replay-shortcut-setup"),
                Action::ClickNamed("capture.target.screen"),
                Action::ClickNamed("capture.options"),
                Action::WaitWall(300),
                Action::Wait(4),
                Action::MoveCursorNamed("capture.option.keep-recent"),
                Action::Snapshot("replay-shortcut-option"),
            ],
        },
        Scenario {
            name: "capture_pip_shortcut",
            description: "The global PiP chord lands in the capture bar with picture in picture chosen.",
            actions: vec![
                Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
                Action::Key(Key::Character("p".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::Wait(12),
                Action::MoveCursor(Point::new(500.0, 300.0)),
                Action::Snapshot("pip-shortcut-ready"),
            ],
        },
        Scenario {
            name: "capture_pip_region",
            description: "A selection inside a window peels into a persistent floating PiP of that part of that window.",
            actions: vec![
                Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
                Action::Key(Key::Character("s".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::Wait(12),
                Action::ClickNamed("capture.output.pip"),
                Action::Drag {
                    from: Point::new(330.0, 160.0),
                    to: Point::new(1_050.0, 620.0),
                    steps: 7,
                },
                Action::Snapshot("pip-region-ready"),
                Action::CaptureExecutionEnabled(true),
                Action::ClickNamed("capture.shutter"),
                Action::Wait(1),
                Action::CapturePipTransition(0),
                Action::Snapshot("pip-region-handoff-000"),
                Action::CapturePipTransition(500),
                Action::Snapshot("pip-region-handoff-500"),
                Action::CapturePipTransition(1_000),
                Action::Snapshot("pip-region-live"),
                Action::MoveCursor(Point::new(20.0, 320.0)),
                Action::Wait(90),
                Action::Snapshot("pip-region-clean"),
            ],
        },
        Scenario {
            name: "capture_pip_window",
            description: "A PiP of a window keeps showing that window wherever the strip scrolls.",
            actions: vec![
                Action::LoneWindow(0),
                Action::SelectGroup(None),
                Action::OpenWindow {
                    app_id: "org.example.current",
                    surface_id: 5431,
                },
                Action::Wait(20),
                Action::SelectGroup(None),
                Action::OpenWindow {
                    app_id: "org.example.next",
                    surface_id: 5432,
                },
                Action::Wait(20),
                Action::SetWindowWidths(1_000),
                Action::SelectGroup(Some(0)),
                Action::Wait(24),
                Action::Relayout,
                Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
                Action::Key(Key::Character("s".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::Wait(12),
                Action::ClickNamed("capture.output.pip"),
                Action::ClickNamed("capture.target.window"),
                Action::Snapshot("pip-window-ready"),
                Action::CaptureExecutionEnabled(true),
                Action::ClickNamed("capture.shutter"),
                Action::Wait(1),
                Action::CapturePipTransition(1_000),
                Action::MoveCursor(Point::new(20.0, 320.0)),
                Action::Wait(90),
                Action::Snapshot("pip-window-live"),
                Action::MoveCursorNamed("floating.chrome"),
                Action::Wait(12),
                Action::Snapshot("pip-window-hover"),
                Action::MoveCursor(Point::new(20.0, 320.0)),
                Action::Wait(90),
                Action::SelectGroup(Some(2)),
                Action::Wait(60),
                Action::Relayout,
                Action::Snapshot("pip-window-after-scroll"),
            ],
        },
        Scenario {
            name: "capture_pip_two_selections",
            description: "Two selections composed into one floating PiP, combined as a grid from the options.",
            actions: vec![
                Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
                Action::Key(Key::Character("s".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::Wait(12),
                Action::ClickNamed("capture.output.pip"),
                Action::Drag {
                    from: Point::new(250.0, 180.0),
                    to: Point::new(500.0, 330.0),
                    steps: 7,
                },
                Action::Modifiers(ModifiersState::SHIFT),
                Action::Drag {
                    from: Point::new(620.0, 360.0),
                    to: Point::new(900.0, 560.0),
                    steps: 7,
                },
                Action::Modifiers(ModifiersState::empty()),
                Action::ClickNamed("capture.options"),
                Action::WaitWall(300),
                Action::Wait(4),
                Action::ClickNamed("capture.option.combine-grid"),
                Action::Snapshot("pip-two-ready"),
                Action::CaptureExecutionEnabled(true),
                Action::ClickNamed("capture.shutter"),
                Action::Wait(1),
                Action::CapturePipTransition(0),
                Action::Snapshot("pip-two-handoff-000"),
                Action::CapturePipTransition(1_000),
                Action::Snapshot("pip-two-live"),
            ],
        },
        Scenario {
            name: "tab_drag_cancel",
            description: "Cancel a lifted tab with Escape before releasing the pointer.",
            actions: vec![
                Action::Snapshot("01-desktop"),
                Action::MoveCursorNamed("tab:Files"),
                Action::MousePress,
                Action::MoveCursor(Point::new(600.0, 300.0)),
                Action::Snapshot("02-tab-dragging"),
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::Snapshot("03-tab-cancelled"),
                Action::MouseRelease,
                Action::Snapshot("04-pointer-released"),
            ],
        },
        Scenario {
            name: "tab_menu_keyboard",
            description: "Open the focused tab menu with Super+F10, navigate and cancel.",
            actions: vec![
                Action::Snapshot("01-desktop"),
                Action::Modifiers(ModifiersState::SUPER),
                Action::Key(Key::Named(NamedKey::F10)),
                Action::Modifiers(ModifiersState::empty()),
                Action::Snapshot("02-tab-menu-focused"),
                Action::Key(Key::Named(NamedKey::ArrowDown)),
                Action::Snapshot("03-tab-menu-next"),
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::Snapshot("04-tab-menu-cancelled"),
            ],
        },
        // The app chooser an app asks for (Settings choosing an app to set
        // how it pastes): the palette over the installed apps.
        Scenario {
            name: "app_chooser",
            description: "Open the app chooser Settings asks for, then type to filter the apps.",
            actions: vec![
                Action::OpenAppChooser("set how it pastes"),
                Action::Wait(2),
                Action::Snapshot("01-app-chooser"),
                Action::Type("te"),
                Action::Wait(2),
                Action::Snapshot("02-app-chooser-filtered"),
            ],
        },
        launcher::fresh(),
        launcher::busy(),
        Scenario {
            name: "palette_typing",
            description: "Open the command palette, type 'h', clear it with Esc, then close it with Esc.",
            actions: vec![
                Action::Snapshot("01-desktop"),
                Action::OpenPalette,
                Action::Wait(2),
                Action::Snapshot("02-palette-open"),
                Action::Type("h"),
                Action::Snapshot("03-palette-typed-h"),
                // Esc clears a query first and closes only an empty field.
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::Wait(2),
                Action::Snapshot("04-palette-cleared"),
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::Wait(2),
                Action::Snapshot("05-palette-closed"),
            ],
        },
        // Apps of the manifest registry this host cannot run: on a Mac
        // Firewall is listed as "Tilo OS only" and Enter opens nothing, while
        // the desktop-only Terminal is not listed as unavailable.
        Scenario {
            name: "palette_unavailable_app",
            description: "Search 'firewall' (Tilo OS only), press Enter, then search 'terminal'.",
            actions: vec![
                Action::OpenPalette,
                Action::Wait(2),
                Action::Type("firewall"),
                Action::Wait(2),
                Action::Snapshot("01-unavailable-listed"),
                Action::Key(Key::Named(NamedKey::Enter)),
                Action::Wait(4),
                Action::Snapshot("02-after-enter"),
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::Wait(2),
                Action::OpenPalette,
                Action::Wait(2),
                Action::Type("terminal"),
                Action::Wait(2),
                Action::Snapshot("03-desktop-only-app"),
            ],
        },
        // The search palette's interaction: a long result list that scrolls
        // and offers the rest of a capped group, keyboard paging, a held
        // modifier's preview, an argument step with values to pick from,
        // and Escape clearing the query before it closes.
        Scenario {
            name: "palette_interaction",
            description: "Search 's', page and tab through it, hold Shift on an app, pick a value for 'screen off after', Escape out.",
            actions: vec![
                Action::OpenPalette,
                Action::Wait(2),
                Action::Type("s"),
                Action::Wait(2),
                Action::Snapshot("01-s"),
                Action::Type("ettings"),
                Action::Wait(2),
                Action::Snapshot("02-settings"),
                Action::Key(Key::Named(NamedKey::Tab)),
                Action::Snapshot("03-tab"),
                Action::Key(Key::Named(NamedKey::End)),
                Action::Snapshot("04-end"),
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::Type("editor"),
                Action::Wait(2),
                Action::Modifiers(ModifiersState::SHIFT),
                Action::Wait(1),
                Action::Snapshot("05-editor-shift-held"),
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::Type("screen off after"),
                Action::Wait(2),
                Action::Snapshot("06-screen-off-after"),
                Action::Key(Key::Named(NamedKey::ArrowDown)),
                Action::Key(Key::Named(NamedKey::Enter)),
                Action::Wait(2),
                Action::Snapshot("07-screen-off-after-values"),
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::Wait(2),
                Action::Snapshot("08-back-to-results"),
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::Wait(2),
                Action::Snapshot("09-query-cleared"),
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::Wait(2),
                Action::Snapshot("10-closed"),
            ],
        },
        Scenario {
            name: "palette_close_anim",
            description: "Open the palette, Escape, and capture the exit animation frames.",
            actions: vec![
                Action::OpenPalette,
                Action::Wait(3),
                Action::Snapshot("00-open"),
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::SnapshotSequence {
                    prefix: "close",
                    frames: 10,
                    gap_frames: 1,
                },
            ],
        },
        Scenario {
            name: "tab_drag",
            description: "Drag the second tab pill rightward across two pills.",
            actions: vec![
                Action::Snapshot("01-tabs-initial"),
                Action::Drag {
                    // Approximate centres of pills 1 and 3 inside the first
                    // window group's tab bar at viewport (1600, 900).
                    from: Point::new(360.0, 130.0),
                    to: Point::new(560.0, 130.0),
                    steps: 8,
                },
                // Wait long enough for the post-reorder layout spring
                // (`LAYOUT_ANIM_MS = 180 ms` in `quick-ui::widget::tabs`)
                // to settle. 12 frames × 16 ms = 192 ms covers it; anything
                // shorter snapshots pills mid-spring and renders adjacent
                // pills painting on top of each other.
                Action::Wait(12),
                Action::Snapshot("02-tabs-after-drag"),
            ],
        },
        Scenario {
            name: "tab_drag_out_to_new_group",
            description: "Drag a tab from a 4-tab group out past the group's right edge \
                      and assert two groups exist post-drop.",
            actions: vec![
                Action::Snapshot("01-before-drag"),
                Action::Drag {
                    // Pill centres inside the lone bootstrap Wide=800 group
                    // bar live around y=128 (topbar=48 + Column padding=24
                    // + workspace_view + strip_view chrome ≈ 56 → bar
                    // mid-y ≈ 128). Pill 1 (Files) sits at x≈424 with the
                    // Wide group laid out from x≈224 + 24 padding + 14 px
                    // leading icon + ~6 px gap. The integration test
                    // `driver_tab_drag_and_detach_scope_to_the_right_group` reads
                    // these via `WindowGroup::tab_rects()` for pixel-
                    // accurate hit; this scenario just needs coords close
                    // enough that the press lands inside the pill.
                    from: Point::new(424.0, 120.0),
                    to: Point::new(1124.0, 120.0),
                    steps: 16,
                },
                Action::Wait(4),
                Action::Snapshot("02-after-drag"),
            ],
        },
        Scenario {
            name: "tab_drag_hover_line_indicator",
            description: "Capture the minimalist white-line drop indicator that paints in the tab \
             bar while a drag is in flight over an inter-pill gap. Holds the drag \
             open (no release) so the line is visible in the snapshot.",
            actions: vec![
                Action::Snapshot("00-before"),
                Action::MoveCursorNamed("tab:Files"),
                Action::MousePress,
                Action::MoveCursorNamed("tab:Notes"),
                Action::Snapshot("01-lift-start"),
                Action::SnapshotSequence {
                    prefix: "02-held-lift",
                    frames: 5,
                    gap_frames: 2,
                },
                Action::MouseRelease,
                Action::Wait(12),
                Action::Snapshot("03-after-release"),
            ],
        },
        Scenario {
            name: "group_drag_visual",
            description: "Carry a complete window group over its neighbour, capturing the translucent handle and tucked app content throughout the lift.",
            actions: vec![
                Action::LoneWindow(0),
                Action::SelectGroup(Some(0)),
                Action::Wait(4),
                Action::SetWindowWidths(HALF_WIDTH_PERMILLE),
                Action::Wait(2),
                Action::OpenHostedWindow {
                    app: "Files",
                    title: "Files",
                },
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::SetWindowWidths(HALF_WIDTH_PERMILLE),
                Action::SelectGroup(Some(0)),
                Action::Wait(4),
                Action::Snapshot("00-before"),
                Action::MoveCursorGroupGrab(0),
                Action::MousePress,
                Action::MoveCursorGroupReorderTarget(1),
                Action::Snapshot("01-lift-start"),
                Action::SnapshotSequence {
                    prefix: "02-held-lift",
                    frames: 5,
                    gap_frames: 2,
                },
                Action::MouseRelease,
                Action::Wait(12),
                Action::Snapshot("03-after-release"),
            ],
        },
        Scenario {
            name: "workspace_switch",
            description: "Switch to workspace index 1, then back to 0, capturing \
             the first frame after each switch as well as the settled one.",
            actions: vec![
                Action::Snapshot("01-ws0"),
                Action::SwitchWorkspace(1),
                Action::SnapshotSequence {
                    prefix: "02-ws1-immediately",
                    frames: 2,
                    gap_frames: 0,
                },
                Action::Wait(1),
                Action::Snapshot("02-ws1"),
                Action::SwitchWorkspace(0),
                Action::SnapshotSequence {
                    prefix: "03-ws0-immediately",
                    frames: 2,
                    gap_frames: 0,
                },
                Action::Wait(1),
                Action::Snapshot("03-ws0-again"),
            ],
        },
        one_place::strip_switcher(),
        one_place::one_place(),
        one_place::fullscreen_bar(),
        Scenario {
            name: "strip_deck",
            description: "Four strips, each a full-height screen: the first fills the work \
             area, the wheel on the strip ribbon slides the deck to the second (every frame \
             of the slide recorded), Ctrl+Super+Down slides on to the third.",
            actions: vec![
                Action::SeedStripsWithPeople,
                Action::CreateStrip("Strip 4"),
                Action::FocusStrip(0),
                Action::MoveCursor(Point::new(800.0, 500.0)),
                Action::WaitWall(600),
                Action::Wait(4),
                Action::Snapshot("00-first-strip-fills"),
                Action::MoveCursorNamed("strip_switcher"),
                Action::Wait(2),
                Action::MouseWheel { dx: 0.0, dy: -1.0 },
                Action::SnapshotSequence {
                    prefix: "01-sliding-to-the-second",
                    frames: 10,
                    gap_frames: 1,
                },
                Action::WaitWall(500),
                Action::MoveCursor(Point::new(800.0, 500.0)),
                Action::Wait(4),
                Action::Snapshot("02-second-strip-fills"),
                Action::Modifiers(ModifiersState::CONTROL | ModifiersState::SUPER),
                Action::Key(Key::Named(NamedKey::ArrowDown)),
                Action::Modifiers(ModifiersState::empty()),
                Action::SnapshotSequence {
                    prefix: "03-sliding-to-the-third",
                    frames: 6,
                    gap_frames: 1,
                },
                Action::WaitWall(400),
                Action::Wait(4),
                Action::Snapshot("04-third-strip-fills"),
            ],
        },
        Scenario {
            name: "strip_switcher_scroll",
            description: "The wheel over the strip ribbon moving through three strips, \
             one per notch, down and back up, every few frames recorded.",
            actions: vec![
                Action::SeedStripsWithPeople,
                Action::MoveCursor(Point::new(800.0, 500.0)),
                Action::WaitWall(300),
                Action::MoveCursorNamed("strip_switcher"),
                Action::Wait(2),
                Action::Snapshot("00-start"),
                Action::MouseWheel { dx: 0.0, dy: -1.0 },
                Action::SnapshotSequence {
                    prefix: "01-down",
                    frames: 8,
                    gap_frames: 2,
                },
                Action::WaitWall(450),
                Action::MouseWheel { dx: 0.0, dy: -1.0 },
                Action::SnapshotSequence {
                    prefix: "02-down",
                    frames: 8,
                    gap_frames: 2,
                },
                Action::WaitWall(450),
                Action::MouseWheel { dx: 0.0, dy: 1.0 },
                Action::SnapshotSequence {
                    prefix: "03-up",
                    frames: 8,
                    gap_frames: 2,
                },
            ],
        },
        Scenario {
            name: "overview_transition",
            description: "Open the workspace overview with Super+Tab and leave with Esc, \
             capturing every frame of the crossfade both ways.",
            actions: vec![
                Action::MoveCursor(Point::new(900.0, 500.0)),
                Action::Snapshot("00-desktop"),
                Action::Modifiers(ModifiersState::SUPER),
                Action::Key(Key::Named(NamedKey::Tab)),
                Action::SnapshotSequence {
                    prefix: "01-enter",
                    frames: 26,
                    gap_frames: 1,
                },
                Action::Modifiers(ModifiersState::empty()),
                Action::Snapshot("02-overview"),
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::SnapshotSequence {
                    prefix: "03-leave",
                    frames: 26,
                    gap_frames: 1,
                },
                Action::Snapshot("04-desktop"),
            ],
        },
        Scenario {
            name: "shortcut_guide",
            description: "Open the responsive desktop navigation guide with F1.",
            actions: vec![
                Action::Wait(4),
                Action::Relayout,
                Action::Snapshot("01-desktop"),
                Action::Key(Key::Named(NamedKey::F1)),
                Action::Wait(2),
                Action::Relayout,
                Action::Snapshot("02-guide"),
            ],
        },
        Scenario {
            name: "overlay_edges",
            description: "Open every top-bar chip and hover its affordances so the popovers and tooltips are reviewed anchored at the top, left and right screen edges.",
            actions: vec![
                Action::Wait(4),
                Action::Relayout,
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::Wait(18),
                Action::Snapshot("00-desktop"),
                // Named targets, not coordinates: the scenario runs across
                // the viewport sweep, where every chip moves. Hover dwell
                // shows the tooltip of the tray pill against the right
                // screen edge, then the workspace name's against the left.
                Action::MoveCursorNamed("topbar_tray_pill"),
                Action::Wait(40),
                Action::Snapshot("01-tooltip-right-edge"),
                Action::MoveCursorNamed("topbar_workspace_pill"),
                Action::Wait(40),
                Action::Snapshot("02-tooltip-left-edge"),
                Action::ClickNamed("topbar_workspace_pill"),
                Action::Wait(4),
                Action::Snapshot("03-workspace-chip"),
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::ClickNamed("topbar_profile"),
                Action::Wait(4),
                Action::Snapshot("03b-profile-chip"),
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::ClickNamed("topbar_center_clock"),
                Action::Wait(4),
                Action::Snapshot("04-clock-chip"),
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::ClickNamed("topbar_tray_pill"),
                Action::Wait(4),
                Action::Snapshot("05-status-chip"),
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::SeedInvitePending,
                Action::Wait(3),
                Action::ClickNamed("topbar_avatar_stack"),
                Action::Wait(4),
                Action::Snapshot("06-people-with-pending-invite"),
            ],
        },
        Scenario {
            name: "solo_and_shared_shell_states",
            description: "Compare the quiet solo desktop with the same workspace after collaborators arrive, then open its roster.",
            actions: vec![
                Action::Wait(4),
                Action::Relayout,
                Action::Snapshot("01-solo-resting"),
                // A returning session opens Catch up when durable unseen work is
                // present. Dismiss it before exercising the ordinary live-roster
                // state in the remaining frames.
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::Wait(4),
                Action::SeedSharedWorkspace,
                Action::Wait(4),
                Action::Relayout,
                Action::Snapshot("02-shared-resting"),
                Action::ClickNamed("topbar_avatar_stack"),
                Action::Wait(4),
                Action::Snapshot("03-shared-roster"),
                Action::Key(Key::Named(NamedKey::Home)),
                Action::Snapshot("04-roster-keyboard-status-focus"),
            ],
        },
        Scenario {
            name: "agent_screen_exposure_disclosure",
            description: "Show trusted screen-sharing disclosure for a named agent with remote control.",
            actions: vec![
                Action::Wait(4),
                Action::Relayout,
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::Wait(18),
                Action::OpenWindow {
                    app_id: "org.libreoffice.LibreOffice",
                    surface_id: 9_201,
                },
                Action::SeedAgentScreenExposure { surface_id: 9_201 },
                Action::Wait(4),
                Action::Relayout,
                Action::Snapshot("01-agent-controls-screen"),
                Action::ClickNamed("topbar_sharing_indicator"),
                Action::Wait(3),
                Action::Snapshot("02-expanded-agent-disclosure"),
            ],
        },
        Scenario {
            name: "collaboration_product_proof",
            description: "Prove solo work, a pending invite, live editing, a forward revert, and cross-app presence.",
            actions: vec![
                Action::Wait(4),
                Action::Relayout,
                Action::Snapshot("01-solo-workspace"),
                Action::SeedInvitePending,
                Action::Wait(2),
                Action::Snapshot("02-invite-pending"),
                Action::AdmitInvitedCollaborator,
                Action::Wait(2),
                Action::Snapshot("03-ada-editing-notes"),
                Action::SeedRoomHistory,
                Action::RevertRoomActivity,
                Action::Wait(18),
                Action::Snapshot("04-reverted-forward"),
                Action::ShowCollaboratorInFiles,
                Action::Wait(2),
                Action::Snapshot("05-ada-in-files"),
            ],
        },
        Scenario {
            name: "presence_together",
            description: "Being together in a workspace: only you, three people and an agent at every scale, the roster with a row open, arriving in a public workspace, a wave, cursor chat, following and stopping, and the avatar menu with what people see.",
            actions: vec![
                Action::Wait(4),
                Action::Relayout,
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::Wait(18),
                Action::OnlyYou,
                Action::Wait(4),
                Action::Relayout,
                Action::Snapshot("01-only-you"),
                Action::ClickNamed("topbar_avatar_stack"),
                Action::Wait(4),
                Action::Snapshot("02-roster-only-you"),
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::Wait(2),
                Action::SeedPresenceRoom,
                Action::Wait(4),
                Action::Relayout,
                Action::Snapshot("03-three-people-and-an-agent"),
                Action::ClickNamed("topbar_avatar_stack"),
                Action::Wait(4),
                Action::Snapshot("04-roster"),
                Action::ExpandRosterRow("ada"),
                Action::Wait(24),
                Action::Snapshot("04b-roster-row-open"),
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::Wait(2),
                Action::Arrive { public: true },
                Action::Wait(3),
                Action::Snapshot("05-arriving-in-a-public-workspace"),
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::WaveFrom("ada"),
                Action::Wait(3),
                Action::Snapshot("06-a-wave-arriving"),
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::RemoteCursorChat {
                    who: "ada",
                    text: "Look at the second paragraph, it reads twice",
                    sent: true,
                },
                Action::RemoteReaction {
                    who: "ben",
                    emoji: "\u{1f389}",
                },
                Action::PointerPath(vec![Point::new(820.0, 300.0)]),
                Action::OpenCursorChat("On it, merging th"),
                Action::Wait(2),
                Action::Snapshot("07-cursor-chat"),
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::FollowPeer(Some("ada")),
                Action::Wait(6),
                Action::Snapshot("08-following"),
                Action::StopFollowingByInput,
                Action::Wait(1),
                Action::Snapshot("09-stopped-following"),
                Action::WaitWall(1_200),
                Action::ClickNamed("topbar_profile"),
                Action::Wait(4),
                Action::Snapshot("10-avatar-menu"),
            ],
        },
        Scenario {
            name: "crowded_people_roster",
            description: "Prove that a crowded live roster lists everyone with full activity and intent rows, scrolls under its pinned count, and remains keyboard navigable.",
            actions: vec![
                Action::Wait(4),
                Action::Relayout,
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::Wait(18),
                Action::SeedCrowdedWorkspace,
                Action::Wait(3),
                Action::Snapshot("01-everyone-listed"),
                Action::Key(Key::Named(NamedKey::Home)),
                Action::Key(Key::Named(NamedKey::End)),
                Action::Wait(2),
                Action::Snapshot("02-last-person-focused"),
            ],
        },
        Scenario {
            name: "foreign_app_collaboration_fallback",
            description: "Prove authenticated live work appears over its owning external app and never over the neighboring external app.",
            actions: vec![
                Action::Wait(4),
                Action::Relayout,
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::Wait(18),
                Action::OpenWindow {
                    app_id: "org.example.Sheets",
                    surface_id: 9_101,
                },
                Action::OpenWindow {
                    app_id: "org.example.Calendar",
                    surface_id: 9_102,
                },
                Action::Wait(5),
                Action::Relayout,
                Action::SeedForeignAppWork {
                    app_id: "org.example.Sheets",
                    action: "Reconciling",
                    resource: "Q3 forecast",
                },
                Action::Wait(2),
                Action::ClickNamed("tab:Sheets"),
                Action::Wait(2),
                Action::Snapshot("01-owning-app-shows-live-work"),
                Action::ClickNamed("tab:Calendar"),
                Action::Wait(2),
                Action::Snapshot("02-neighbor-app-does-not"),
            ],
        },
        Scenario {
            name: "collaboration_status_privacy",
            description: "Prove inline intent editing, manual publication, and the explicit hidden state with keyboard-only interaction.",
            actions: vec![
                Action::Wait(4),
                Action::Relayout,
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::Wait(18),
                Action::SeedSharedWorkspace,
                Action::SeedInferredStatus("Managing workspace access"),
                Action::ClickNamed("topbar_avatar_stack"),
                Action::Wait(3),
                Action::Key(Key::Named(NamedKey::Home)),
                Action::Key(Key::Named(NamedKey::Enter)),
                Action::Type("Closing the Q3 forecast"),
                Action::Snapshot("01-inline-editor"),
                Action::Key(Key::Named(NamedKey::Enter)),
                Action::Key(Key::Named(NamedKey::ArrowRight)),
                Action::Key(Key::Named(NamedKey::ArrowRight)),
                Action::Key(Key::Named(NamedKey::Enter)),
                Action::Snapshot("02-manual-status"),
                Action::Key(Key::Named(NamedKey::Home)),
                Action::Key(Key::Named(NamedKey::ArrowRight)),
                Action::Key(Key::Named(NamedKey::ArrowRight)),
                Action::Key(Key::Named(NamedKey::ArrowRight)),
                Action::Key(Key::Named(NamedKey::Enter)),
                Action::Snapshot("03-hidden-status"),
            ],
        },
        Scenario {
            name: "display_identify",
            description: "Settings' Identify: every display shows a large card \
             with its number in the accent and its name, as its canvas tile in \
             Settings does. Run with `--outputs docked-laptop`.",
            actions: vec![
                Action::Wait(2),
                Action::IdentifyDisplays,
                Action::Wait(12),
                Action::Snapshot("00-identify"),
            ],
        },
        Scenario {
            name: "top_bar_hidden_reveal",
            description: "Hide the top bar until the pointer is near it: the \
             window takes the bar's strip; a push to the top edge shows the bar \
             over the window in its docked place; heading back down hides it.",
            actions: vec![
                // The keyboard in a window, as in use: a bar holding the
                // keyboard stays shown.
                Action::ClickAt(Point::new(900.0, 500.0)),
                Action::Wait(2),
                Action::Snapshot("00-docked"),
                Action::HideTopBar(true),
                Action::Wait(4),
                Action::Snapshot("01-hidden"),
                Action::CompositorPointer(Point::new(900.0, 400.0)),
                Action::CompositorPointer(Point::new(900.0, 260.0)),
                Action::CompositorPointer(Point::new(900.0, 120.0)),
                Action::CompositorPointer(Point::new(900.0, 30.0)),
                Action::CompositorPointer(Point::new(900.0, 0.0)),
                Action::Wait(20),
                Action::Snapshot("02-reached-for"),
                Action::CompositorPointer(Point::new(1100.0, 10.0)),
                Action::CompositorPointer(Point::new(1400.0, 12.0)),
                Action::Wait(4),
                Action::Snapshot("03-along-the-bar"),
                Action::CompositorPointer(Point::new(1300.0, 60.0)),
                Action::CompositorPointer(Point::new(1100.0, 200.0)),
                Action::CompositorPointer(Point::new(900.0, 400.0)),
                Action::Wait(4),
                Action::Snapshot("04-hidden-again"),
            ],
        },
        Scenario {
            name: "strip_horizontal_scroll",
            description: "Swipe sideways over the strip and travel along it — one window \
             forward, then back — with the rail reading out where that is.",
            actions: vec![
                // Cursor over the active tile / strip area (well below the
                // topbar's persistent band and well clear of the leading
                // edge gutter).
                Action::MoveCursor(Point::new(900.0, 500.0)),
                Action::Snapshot("00-before"),
                // A sideways swipe steps focus one window along the strip —
                // the same operation Super+ArrowRight performs — and the
                // focus-follow pan brings it into view. A rightward wheel
                // (negative dx, winit's sign) travels forward, matching the
                // chord's direction.
                Action::MouseWheel { dx: -10.0, dy: 0.0 },
                Action::Wait(24),
                Action::Snapshot("01-after-pan"),
                // And back the way it came.
                Action::MouseWheel { dx: 10.0, dy: 0.0 },
                Action::Wait(24),
                Action::Snapshot("02-back-to-start"),
            ],
        },
        window_search::notes_search_in_place(),
        window_search::notes_search_options(),
        window_search::notes_replace_in_place(),
        window_search::own_process_notes_search(),
        Scenario {
            name: "own_process_notes_history",
            description: "Notes as a window of its own: the window bar's History control opens Notes' History panel over its shell-ui connection, with the changes Ada and her agent made.",
            actions: vec![
                Action::OwnProcessWindow {
                    app: OwnApp::Notes,
                    width: 700,
                },
                Action::Wait(4),
                Action::Relayout,
                Action::ClickNamed("window-history"),
                Action::Wait(30),
                Action::Relayout,
                Action::Snapshot("01-history-panel"),
            ],
        },
        window_search::own_process_files_search(),
        Scenario {
            name: "own_process_files_history",
            description: "Files as a window of its own: the window bar's History control opens its History panel over its shell-ui connection.",
            actions: vec![
                Action::OwnProcessWindow {
                    app: OwnApp::Files,
                    width: 700,
                },
                Action::Wait(4),
                Action::Relayout,
                Action::ClickNamed("window-history"),
                Action::Wait(30),
                Action::Relayout,
                Action::Snapshot("01-history-panel"),
            ],
        },
        Scenario {
            name: "own_process_notes_motion",
            description: "Mid-motion frames of Notes as a window of its own: the window bar's search expanding into its field, and History's panel sliding in.",
            actions: vec![
                Action::OwnProcessWindow {
                    app: OwnApp::Notes,
                    width: 700,
                },
                Action::Wait(4),
                Action::Relayout,
                Action::MotionFrames {
                    prefix: "search-open",
                    cue: MotionCue::ClickNamed("window-search-open"),
                    frames: 10,
                    every_ms: 30,
                },
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::Wait(24),
                Action::Relayout,
                Action::MotionFrames {
                    prefix: "history-open",
                    cue: MotionCue::ClickNamed("window-history"),
                    frames: 12,
                    every_ms: 30,
                },
            ],
        },
        Scenario {
            name: "notes_history_panel",
            description: "History as a side panel of the Notes window: changes by Ada and her agent grouped by session, filtered by person, and one previewed in place, read-only, with Restore and Restore just this change.",
            actions: vec![
                Action::SeedLongNotes(0),
                Action::Wait(2),
                Action::SeedNoteHistory(0),
                Action::Wait(4),
                Action::Relayout,
                Action::ClickNamed("window-history"),
                Action::Wait(30),
                Action::Snapshot("01-history-panel"),
                // Ada's latest change, in her session's band: its version
                // shows in place of the note, read-only.
                Action::ClickAt(Point::new(1400.0, 332.0)),
                Action::Wait(30),
                Action::Snapshot("02-entry-preview"),
            ],
        },
        Scenario {
            name: "notes_click_and_type",
            description: "Click into the Notes tile, type 'hi', snapshot — proves apps are interactive.",
            actions: vec![
                Action::Snapshot("01-pre-click"),
                // The Notes tile is centred horizontally with the body
                // starting ~150 px below the topbar. 660,500 lands inside
                // the body text area on the default 1600×900 viewport.
                Action::ClickAt(Point::new(660.0, 500.0)),
                Action::Wait(2),
                Action::Snapshot("02-post-click"),
                Action::Type("hi"),
                Action::Wait(2),
                Action::Snapshot("03-post-type"),
            ],
        },
        Scenario {
            name: "two_notes_tabs_keep_their_own_text",
            description: "Type in one Notes tab, open a second, type in it, switch back.",
            actions: vec![
                // Same body coordinate `notes_click_and_type` uses.
                Action::ClickAt(Point::new(660.0, 500.0)),
                Action::Wait(2),
                Action::Type("first tab writes alpha"),
                Action::Wait(2),
                Action::Snapshot("01-first-tab-typed"),
                Action::AddHostedTab {
                    app: "Notes",
                    title: "Notes",
                },
                Action::Wait(20),
                Action::Snapshot("02-second-tab-opened"),
                // Reach the new tab the way a person does — its pill — so
                // it is switched to rather than merely spliced in active.
                Action::ClickAt(Point::new(884.0, 103.0)),
                Action::Wait(4),
                Action::ClickAt(Point::new(660.0, 500.0)),
                Action::Wait(4),
                Action::Type("second tab writes beta"),
                Action::Wait(2),
                Action::Snapshot("03-second-tab-typed"),
                // Back to the first Notes pill, left-most in the bar.
                Action::ClickAt(Point::new(310.0, 103.0)),
                Action::Wait(4),
                Action::Snapshot("04-first-tab-still-alpha"),
            ],
        },
        Scenario {
            name: "cycle_all_tabs",
            description: "Click Notes → Files → Terminal → Browser tabs, snapshot each.",
            actions: vec![
                Action::Snapshot("01-notes-active"),
                // Files tab pill.
                Action::ClickAt(Point::new(443.0, 103.0)),
                Action::Wait(2),
                Action::Snapshot("02-files-active"),
                // Terminal tab pill.
                Action::ClickAt(Point::new(589.0, 103.0)),
                Action::Wait(2),
                Action::Snapshot("03-terminal-active"),
                // Browser tab pill.
                Action::ClickAt(Point::new(744.0, 103.0)),
                Action::Wait(2),
                Action::Snapshot("04-browser-active"),
            ],
        },
        Scenario {
            name: "floating_material",
            description: "Quick settings, then the palette, over a long note: floating chrome is the blurred material.",
            actions: vec![
                Action::SeedLongNotes(0),
                Action::Wait(2),
                Action::Relayout,
                Action::Snapshot("01-document"),
                Action::PushQsLevel(QsLevel::Root),
                Action::Wait(30),
                Action::Snapshot("02-quick-settings"),
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::Wait(30),
                Action::OpenPalette,
                Action::Wait(30),
                Action::Snapshot("03-palette"),
                // The same two surfaces under reduced transparency: opaque.
                Action::ReduceTransparency(true),
                Action::Snapshot("04-palette-opaque"),
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::Wait(30),
                Action::PushQsLevel(QsLevel::Root),
                Action::Wait(30),
                Action::Snapshot("05-quick-settings-opaque"),
            ],
        },
        Scenario {
            name: "tab_strip_material",
            description: "The window groups' docked tab strips: at rest, a hovered tab, then under reduced transparency.",
            actions: vec![
                Action::Wait(2),
                Action::Relayout,
                Action::Snapshot("01-strips"),
                Action::MoveCursorNamed("tab:Files"),
                Action::Wait(20),
                Action::Snapshot("02-hovered-tab"),
                Action::ReduceTransparency(true),
                Action::Wait(2),
                Action::Snapshot("03-solid"),
            ],
        },
        Scenario {
            name: "word_processor_tile",
            description: "Activate the Document tab so the light-chrome word processor is on screen.",
            actions: vec![
                Action::Snapshot("01-notes-active"),
                // The "Document" pill, in the group's opening layout.
                Action::ClickAt(Point::new(717.0, 104.0)),
                Action::Wait(2),
                Action::Snapshot("02-document-active"),
            ],
        },
        Scenario {
            name: "room_activity_from_palette",
            description: "Search 'workspace activity' in the palette and find the command that opens it.",
            actions: vec![
                Action::Snapshot("01-desktop"),
                Action::OpenPalette,
                Action::Wait(2),
                Action::Type("workspace activity"),
                Action::Wait(2),
                Action::Snapshot("02-palette-result"),
            ],
        },
        Scenario {
            name: "window_open_beside_selection",
            description: "Open a window beside the selected one; the neighbour to its right \
                      slides over to make room.",
            actions: vec![
                Action::LoneWindow(0),
                // A neighbour to the right, so there is something to move.
                Action::OpenWindow {
                    app_id: "org.example.right",
                    surface_id: 5001,
                },
                Action::Wait(20),
                // Windows at a third of the display each: a capture of windows
                // moving needs more than one of them on screen, and the
                // default width is the whole display.
                Action::SetWindowWidths(333),
                Action::SelectGroup(Some(0)),
                Action::Wait(4),
                Action::Snapshot("01-before"),
                Action::OpenWindow {
                    app_id: "org.example.new",
                    surface_id: 5002,
                },
                Action::SampleMakeSpace {
                    prefix: "02-making-room",
                    at_ms: MAKE_SPACE_SAMPLES,
                },
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::Snapshot("03-settled"),
            ],
        },
        Scenario {
            name: "window_maximize_restore_spring",
            description: "Super+M springs the focused group between its tiled rect and the \
                      work area; a second Super+M restores from the current position.",
            actions: vec![
                Action::LoneWindow(0),
                Action::OpenWindow {
                    app_id: "org.example.right",
                    surface_id: 6201,
                },
                Action::Wait(20),
                Action::SetWindowWidths(500),
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::SelectGroup(Some(0)),
                Action::Wait(4),
                Action::Snapshot("01-tiled"),
                Action::Modifiers(ModifiersState::SUPER),
                Action::Key(Key::Character("m".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::Snapshot("02-maximize-000"),
                Action::SnapshotSequence {
                    prefix: "02-maximize",
                    frames: 8,
                    gap_frames: 2,
                },
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::Snapshot("03-maximized"),
                Action::Modifiers(ModifiersState::SUPER),
                Action::Key(Key::Character("m".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::Snapshot("04-restore-000"),
                Action::SnapshotSequence {
                    prefix: "04-restore",
                    frames: 8,
                    gap_frames: 2,
                },
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::Snapshot("05-restored"),
            ],
        },
        Scenario {
            name: "window_maximize_sibling_fade",
            description: "Super+M covers neighbour windows at once (nothing of them paints \
                      over the maximized one), then Super+M again fades them back \
                      in on the fast curve.",
            actions: vec![
                Action::LoneWindow(0),
                Action::OpenWindow {
                    app_id: "org.example.right",
                    surface_id: 6301,
                },
                Action::Wait(20),
                Action::SetWindowWidths(500),
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::SelectGroup(Some(0)),
                Action::Wait(4),
                Action::Snapshot("01-tiled"),
                Action::Modifiers(ModifiersState::SUPER),
                Action::Key(Key::Character("m".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::SampleSiblingFade {
                    prefix: "02-maximize",
                    at_ms: SIBLING_FADE_SAMPLES,
                },
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::Snapshot("03-maximized"),
                Action::Modifiers(ModifiersState::SUPER),
                Action::Key(Key::Character("m".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::SampleSiblingFade {
                    prefix: "04-restore",
                    at_ms: SIBLING_FADE_SAMPLES,
                },
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::Snapshot("05-restored"),
            ],
        },
        Scenario {
            name: "window_width_spring",
            description: "An opening window grows its slot from zero, a committed resize \
                      springs to the new width, and Super+W collapses the last \
                      frame while the slot closes.",
            actions: vec![
                Action::LoneWindow(0),
                Action::OpenWindow {
                    app_id: "org.example.right",
                    surface_id: 6101,
                },
                Action::Wait(20),
                Action::SetWindowWidths(333),
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::SelectGroup(Some(0)),
                Action::Wait(4),
                Action::Snapshot("01-before-open"),
                Action::OpenWindow {
                    app_id: "org.example.new",
                    surface_id: 6102,
                },
                // Placement gives a new group the default fraction; retarget
                // the in-flight 0→width spring to a third so the slot stays
                // on screen next to its neighbours (and so a new input keeps
                // the current velocity).
                Action::SetWindowWidths(333),
                Action::Snapshot("02-open-000"),
                Action::SnapshotSequence {
                    prefix: "02-open",
                    frames: 8,
                    gap_frames: 2,
                },
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::Snapshot("03-open-settled"),
                Action::SetWindowWidths(500),
                Action::Snapshot("04-resize-000"),
                Action::SnapshotSequence {
                    prefix: "04-resize",
                    frames: 8,
                    gap_frames: 2,
                },
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::Snapshot("05-resize-settled"),
                Action::SelectGroup(Some(1)),
                Action::Wait(2),
                Action::Modifiers(ModifiersState::SUPER),
                Action::Key(Key::Character("w".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::SnapshotSequence {
                    prefix: "06-close",
                    frames: 8,
                    gap_frames: 2,
                },
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::Snapshot("07-close-settled"),
            ],
        },
        Scenario {
            name: "window_open_into_tabbed_group",
            description: "Open a window from inside the default four-tab group; it becomes \
                      the tab right of the selected one.",
            actions: vec![
                Action::SetWindowWidths(333),
                Action::SelectGroup(Some(0)),
                Action::Wait(4),
                Action::Snapshot("01-before"),
                Action::OpenWindow {
                    app_id: "org.example.tab",
                    surface_id: 5003,
                },
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::Snapshot("02-joined-the-group"),
            ],
        },
        Scenario {
            name: "window_open_at_visible_seam",
            description: "Three windows, no selection, a border on screen: the new window \
                      lands at that border.",
            actions: vec![
                Action::LoneWindow(0),
                Action::OpenWindow {
                    app_id: "org.example.second",
                    surface_id: 5011,
                },
                Action::Wait(20),
                Action::OpenWindow {
                    app_id: "org.example.third",
                    surface_id: 5012,
                },
                Action::Wait(20),
                Action::SetWindowWidths(333),
                Action::SelectGroup(None),
                Action::Wait(4),
                Action::Snapshot("01-before"),
                Action::OpenWindow {
                    app_id: "org.example.atseam",
                    surface_id: 5013,
                },
                Action::SampleMakeSpace {
                    prefix: "02-at-seam",
                    at_ms: MAKE_SPACE_SAMPLES,
                },
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::Snapshot("03-settled"),
            ],
        },
        Scenario {
            name: "window_open_at_next_seam_rightwards",
            description: "Three full-width windows, no selection, no border on screen: the \
                      new window takes the next border rightwards.",
            actions: vec![
                Action::LoneWindow(0),
                Action::OpenWindow {
                    app_id: "org.example.second",
                    surface_id: 5021,
                },
                Action::Wait(20),
                Action::OpenWindow {
                    app_id: "org.example.third",
                    surface_id: 5022,
                },
                Action::Wait(20),
                // Full width: the border after the window you are looking at
                // lands exactly on the screen's right edge, so none of the
                // space between windows is on screen.
                Action::SetWindowWidths(1000),
                Action::SelectGroup(None),
                Action::Wait(4),
                Action::Snapshot("01-before"),
                Action::OpenWindow {
                    app_id: "org.example.nextseam",
                    surface_id: 5023,
                },
                Action::SampleMakeSpace {
                    prefix: "02-next-seam",
                    at_ms: MAKE_SPACE_SAMPLES,
                },
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::Snapshot("03-settled"),
            ],
        },
        Scenario {
            name: "searched_app_opens_beside_focus",
            description: "Search for an app with a full strip; it opens as its own window right of the one you are in.",
            actions: vec![
                Action::LoneWindow(0),
                Action::SelectGroup(Some(0)),
                Action::OpenHostedWindow {
                    app: "Files",
                    title: "Files",
                },
                Action::SelectGroup(Some(1)),
                Action::OpenHostedWindow {
                    app: "Terminal",
                    title: "Terminal",
                },
                Action::SelectGroup(Some(2)),
                Action::OpenHostedWindow {
                    app: "Browser",
                    title: "Browser",
                },
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::SetWindowWidths(333),
                Action::SelectGroup(Some(0)),
                Action::Wait(8),
                Action::Snapshot("00-before-search"),
                Action::EnableNativeAppSearch,
                Action::OpenPalette,
                Action::Wait(2),
                Action::Type("org.tilo.Files"),
                Action::Wait(2),
                Action::Snapshot("01-search-result"),
                Action::Key(Key::Named(NamedKey::Enter)),
                Action::SampleMakeSpace {
                    prefix: "02-making-room",
                    at_ms: MAKE_SPACE_SAMPLES,
                },
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::SetWindowWidths(333),
                Action::Wait(8),
                Action::Snapshot("03-after-open"),
            ],
        },
        Scenario {
            name: "strip_edge_resize",
            description: "Notes, Terminal and Files side by side, Files focused. Hover and drag \
                      Files' left edge (the seam it shares with Terminal) left and back, then \
                      Shift+drag it, then drag its right edge: only Files changes width on a \
                      plain drag, its opposite edge holds still, and Shift trades width with \
                      Terminal alone.",
            actions: vec![
                Action::LoneWindow(0),
                Action::SelectGroup(Some(0)),
                Action::OpenHostedWindow {
                    app: "Terminal",
                    title: "Terminal",
                },
                Action::SelectGroup(Some(1)),
                Action::OpenHostedWindow {
                    app: "Files",
                    title: "Files",
                },
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::SetWindowWidths(300),
                Action::SelectGroup(Some(2)),
                Action::Wait(PLACEMENT_SETTLE_FRAMES * 3),
                Action::Relayout,
                Action::Snapshot("01-files-focused"),
                Action::MoveCursorGroupEdge {
                    group: 2,
                    leading: true,
                },
                Action::Wait(2),
                Action::Snapshot("02-hover-files-left-edge"),
                Action::MousePress,
                Action::MoveCursorBy(-40.0),
                Action::Wait(1),
                Action::Snapshot("03-drag-left-040"),
                Action::MoveCursorBy(-40.0),
                Action::Wait(1),
                Action::Snapshot("03-drag-left-080"),
                Action::MoveCursorBy(-40.0),
                Action::Wait(1),
                Action::Snapshot("03-drag-left-120"),
                Action::MoveCursorBy(200.0),
                Action::Wait(1),
                Action::Snapshot("03-drag-right-080"),
                Action::MouseRelease,
                Action::Wait(PLACEMENT_SETTLE_FRAMES * 3),
                Action::Snapshot("04-released"),
                Action::MoveCursorGroupEdge {
                    group: 2,
                    leading: true,
                },
                Action::Modifiers(ModifiersState::SHIFT),
                Action::MoveCursorBy(0.0),
                Action::Wait(2),
                Action::Snapshot("05-hover-shift"),
                Action::Modifiers(ModifiersState::SHIFT),
                Action::MousePress,
                Action::MoveCursorBy(-60.0),
                Action::Wait(1),
                Action::Snapshot("06-shift-drag-left-060"),
                Action::MoveCursorBy(-60.0),
                Action::Wait(1),
                Action::Snapshot("06-shift-drag-left-120"),
                Action::MouseRelease,
                Action::Modifiers(ModifiersState::empty()),
                Action::Wait(PLACEMENT_SETTLE_FRAMES * 3),
                Action::Snapshot("07-shift-released"),
                Action::MoveCursorGroupEdge {
                    group: 2,
                    leading: false,
                },
                Action::Wait(2),
                Action::Snapshot("08-hover-files-right-edge"),
                Action::MousePress,
                Action::MoveCursorBy(-60.0),
                Action::Wait(1),
                Action::Snapshot("09-drag-right-edge-in-060"),
                Action::MouseRelease,
                Action::Wait(PLACEMENT_SETTLE_FRAMES * 3),
                Action::Snapshot("10-released"),
            ],
        },
        Scenario {
            name: "proximity_open_close_paste",
            description: "Working in the first of three windows: open an app from search, close it again, \
                      open clipboard history while typing, and change the volume while typing on the bottom line.",
            actions: vec![
                Action::LoneWindow(0),
                Action::SelectGroup(Some(0)),
                Action::OpenHostedWindow {
                    app: "Files",
                    title: "Files",
                },
                Action::SelectGroup(Some(1)),
                Action::OpenHostedWindow {
                    app: "Terminal",
                    title: "Terminal",
                },
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::Modifiers(ModifiersState::SUPER),
                Action::Key(Key::Character("1".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::Wait(PLACEMENT_SETTLE_FRAMES * 3),
                Action::Snapshot("00-working-in-the-first-window"),
                Action::EnableNativeAppSearch,
                Action::OpenPalette,
                Action::Wait(2),
                Action::Type("Editor"),
                Action::Wait(2),
                Action::Snapshot("01-search-result"),
                Action::Key(Key::Named(NamedKey::Enter)),
                Action::Wait(PLACEMENT_SETTLE_FRAMES * 3),
                Action::Snapshot("02-opened-from-search"),
                Action::CloseFocusedWindow,
                Action::Wait(PLACEMENT_SETTLE_FRAMES * 3),
                Action::Snapshot("03-closed-it-again"),
                Action::SetTextCaret(Some(vello::kurbo::Rect::new(220.0, 420.0, 222.0, 440.0))),
                Action::Modifiers(ModifiersState::SUPER),
                Action::Key(Key::Character("v".into())),
                Action::Modifiers(ModifiersState::empty()),
                Action::Wait(16),
                Action::Snapshot("04-super-v-while-typing"),
                Action::Key(Key::Named(NamedKey::Escape)),
                Action::Wait(8),
                Action::SetTextCaret(Some(vello::kurbo::Rect::new(780.0, 820.0, 782.0, 840.0))),
                Action::SeedOsd("volume"),
                Action::Wait(16),
                Action::Snapshot("05-volume-while-typing-on-the-bottom-line"),
            ],
        },
        Scenario {
            name: "strip_scroll_across_apps",
            description: "Open Notes, Files, Terminal and Editor as four windows along the strip, \
                      then directly pan into the resisted trailing edge and back.",
            actions: vec![
                // The desktop boots with one four-tab group. Cut it to Notes so
                // the first window is a lone window — the shape the placement
                // rule turns into a column of its own for the next app.
                Action::LoneWindow(0),
                Action::SelectGroup(Some(0)),
                // One frame first: the resize reaches the live widgets through
                // the strip's `GroupRow`, which does not exist until a layout
                // pass has run, and the persisted width alone only shows up on
                // groups the resync splices in later.
                Action::Wait(4),
                // Half the usable width each, held there as the strip fills up:
                // two windows on screen at a time with a third clipped at the
                // edge is what makes both the arrivals and the pan legible. A
                // window opens at its own default width, so this is re-applied
                // after each one rather than set once.
                Action::SetWindowWidths(HALF_WIDTH_PERMILLE),
                Action::Wait(2),
                Action::Snapshot("01-one-window"),
                Action::OpenHostedWindow {
                    app: "Files",
                    title: "Files",
                },
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::SetWindowWidths(HALF_WIDTH_PERMILLE),
                Action::SelectGroup(Some(1)),
                Action::Wait(4),
                Action::Snapshot("02-two-windows"),
                Action::OpenHostedWindow {
                    app: "Terminal",
                    title: "Terminal",
                },
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::SetWindowWidths(HALF_WIDTH_PERMILLE),
                Action::SelectGroup(Some(2)),
                Action::Wait(4),
                Action::Snapshot("03-three-windows"),
                Action::OpenHostedWindow {
                    app: "Editor",
                    title: "Editor",
                },
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::SetWindowWidths(HALF_WIDTH_PERMILLE),
                Action::Wait(4),
                // The last window still grows into its half after the resize;
                // the pan's bounds follow the content width, so let it land
                // before anything is measured against them.
                Action::WaitStripSettled(STRIP_SETTLE_MAX_FRAMES),
                Action::Snapshot("04-four-windows"),
                // Back to the first window, which brings the strip home, its
                // gutter before the window — the starting position for the
                // travel. The reveal glides on the group-move spring, which
                // from the far end takes longer than a fixed wait to come
                // within a pixel.
                Action::SelectGroup(Some(0)),
                Action::Wait(PAN_SETTLE_FRAMES),
                Action::WaitStripSettled(STRIP_SETTLE_MAX_FRAMES),
                Action::Snapshot("05-strip-at-rest"),
                Action::StripPan(10_000.0),
                Action::Relayout,
                Action::Snapshot("06-held-past-trailing-edge"),
                Action::EndStripPan,
                Action::Wait(PAN_SETTLE_FRAMES),
                Action::WaitStripSettled(STRIP_SETTLE_MAX_FRAMES),
                Action::Snapshot("07-settled-trailing-room"),
                Action::StripPan(-10_000.0),
                Action::EndStripPan,
                Action::Wait(PAN_SETTLE_FRAMES),
                Action::WaitStripSettled(STRIP_SETTLE_MAX_FRAMES),
                Action::Snapshot("08-panned-back"),
            ],
        },
        Scenario {
            name: "narrow_groups_side_by_side",
            description: "Three windows at a third of the strip each, the first \
                      carrying more tabs than fit — the configuration where \
                      tab pills run out of room.",
            actions: vec![
                // Boot hands us one four-tab group; cut it to a lone window
                // so the next app lands in a column of its own rather than
                // as another tab.
                Action::LoneWindow(0),
                Action::SelectGroup(Some(0)),
                Action::Wait(4),
                Action::SetWindowWidths(THIRD_WIDTH_PERMILLE),
                Action::Wait(2),
                Action::OpenHostedWindow {
                    app: "Files",
                    title: "Files",
                },
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::SetWindowWidths(THIRD_WIDTH_PERMILLE),
                Action::SelectGroup(Some(1)),
                Action::Wait(4),
                Action::Snapshot("01-two-groups"),
                Action::OpenHostedWindow {
                    app: "Terminal",
                    title: "Terminal",
                },
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::SetWindowWidths(THIRD_WIDTH_PERMILLE),
                Action::SelectGroup(Some(2)),
                Action::Wait(4),
                Action::Snapshot("02-three-groups"),
                // Now crowd the first group. Three pills in a third-width
                // group is past what the row can show at natural width, so
                // this is the frame where the fit and the elision have to do
                // their job.
                Action::SelectGroup(Some(0)),
                Action::Wait(2),
                Action::AddHostedTab {
                    app: "Editor",
                    title: "Editor",
                },
                Action::Wait(4),
                Action::AddHostedTab {
                    app: "Notes",
                    title: "Shipping notes for the demo",
                },
                Action::Wait(6),
                Action::SetWindowWidths(THIRD_WIDTH_PERMILLE),
                Action::Wait(6),
                Action::Snapshot("03-crowded-narrow-group"),
            ],
        },
        Scenario {
            name: "tab_pill_reveal",
            description: "A four-tab group at rest, then one pill opening its \
                      control cluster and closing again.",
            actions: vec![
                Action::SelectGroup(Some(0)),
                Action::Wait(4),
                Action::Relayout,
                Action::Snapshot("01-idle"),
                Action::MoveCursorNamed("tab:Files"),
                Action::SampleTabReveal {
                    prefix: "02-opening",
                    at_ms: REVEAL_SAMPLES,
                },
                Action::Wait(12),
                Action::Snapshot("03-open"),
                // Off the bar entirely: the pill closes again, and its body
                // has to stay under the chips for the whole shrink. Park in
                // the middle of the window card, clear of the strip's edges
                // and the right-edge strip ribbon, so nothing else reacts
                // to the pointer and confuses what this is measuring.
                Action::MoveCursor(Point::new(800.0, 500.0)),
                Action::SampleTabReveal {
                    prefix: "04-closing",
                    at_ms: REVEAL_SAMPLES,
                },
                Action::Wait(12),
                Action::Snapshot("05-idle-again"),
            ],
        },
        Scenario {
            name: "tab_pill_reveal_narrow",
            description: "Three third-width groups, the first crowded; one of its \
                      pills opens its control cluster.",
            actions: vec![
                Action::LoneWindow(0),
                Action::SelectGroup(Some(0)),
                Action::Wait(4),
                Action::SetWindowWidths(THIRD_WIDTH_PERMILLE),
                Action::Wait(2),
                Action::OpenHostedWindow {
                    app: "Files",
                    title: "Files",
                },
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::SetWindowWidths(THIRD_WIDTH_PERMILLE),
                Action::SelectGroup(Some(1)),
                Action::Wait(4),
                Action::OpenHostedWindow {
                    app: "Terminal",
                    title: "Terminal",
                },
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::SetWindowWidths(THIRD_WIDTH_PERMILLE),
                Action::SelectGroup(Some(0)),
                Action::Wait(2),
                Action::AddHostedTab {
                    app: "Editor",
                    title: "Editor",
                },
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::AddHostedTab {
                    app: "Settings",
                    title: "Settings",
                },
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::AddHostedTab {
                    app: "Browser",
                    title: "Browser",
                },
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::AddHostedTab {
                    app: "Notes",
                    title: "Document",
                },
                Action::Wait(PLACEMENT_SETTLE_FRAMES),
                Action::SetWindowWidths(THIRD_WIDTH_PERMILLE),
                Action::Wait(4),
                Action::Relayout,
                Action::Snapshot("01-idle"),
                Action::MoveCursorNamed("tab:Editor"),
                Action::SampleTabReveal {
                    prefix: "02-opening",
                    at_ms: REVEAL_SAMPLES,
                },
                Action::Wait(12),
                Action::Snapshot("03-open"),
                Action::MoveCursor(Point::new(800.0, 500.0)),
                Action::SampleTabReveal {
                    prefix: "04-closing",
                    at_ms: REVEAL_SAMPLES,
                },
                Action::Wait(12),
                Action::Snapshot("05-idle-again"),
            ],
        },
        Scenario {
            name: "tab_pill_states",
            description: "One tab row carrying all three pill states at once: two \
                      idle, one active, one hovered.",
            actions: vec![
                Action::SelectGroup(Some(0)),
                Action::Wait(4),
                Action::Relayout,
                // Activate the third tab, then let the lift settle.
                Action::ClickNamed("tab:Terminal"),
                Action::Relayout,
                Action::Wait(24),
                // Pointer off the bar: active against three idle pills, with
                // no hover anywhere to blur the comparison.
                Action::MoveCursor(Point::new(800.0, 500.0)),
                Action::Relayout,
                Action::Wait(24),
                Action::Snapshot("01-active-vs-idle"),
                // Now hover the fourth pill so all three tiers are in frame.
                //
                // The `Relayout` has to come *before* the wait, not after it.
                // A pill's reveal curve is latched during layout and then read
                // off wall-clock elapsed, so relayouting last starts the
                // animation at the instant of capture and photographs the
                // hovered pill at zero — no body, no chips, only the brighter
                // title. Which is a picture of the frame the pointer arrived
                // on, not of a hovered tab.
                Action::MoveCursorNamed("tab:Document"),
                Action::Relayout,
                Action::Wait(24),
                Action::Snapshot("02-three-states"),
            ],
        },
        Scenario {
            name: "new_tab_from_the_plus_chip",
            description: "Click the tab strip's + chip; the palette opens aimed at that group, \
                      and its launch rows say they will open here.",
            actions: vec![
                Action::SelectGroup(Some(0)),
                Action::Wait(4),
                Action::Snapshot("01-tab-strip-with-plus"),
                // Resolved against the live widget tree, so this clicks the
                // rect the renderer actually painted.
                Action::ClickNamed("tabbar_new_tab"),
                Action::Wait(12),
                Action::Snapshot("02-palette-aimed-here"),
                Action::Type("term"),
                Action::Wait(8),
                Action::Snapshot("03-results-say-where-they-land"),
            ],
        },
        Scenario {
            name: "status_tray_states",
            description: "The tray across battery levels, charge state, mute and link kind.",
            actions: vec![
                Action::SeedStatus("healthy"),
                Action::Wait(3),
                Action::Snapshot("01-healthy"),
                Action::SeedStatus("charging"),
                Action::Wait(3),
                Action::Snapshot("02-charging"),
                Action::SeedStatus("low"),
                Action::Wait(3),
                Action::Snapshot("03-low"),
                Action::SeedStatus("critical"),
                Action::Wait(3),
                Action::Snapshot("04-critical"),
                Action::SeedStatus("muted"),
                Action::Wait(3),
                Action::Snapshot("05-muted"),
                Action::SeedStatus("offline"),
                Action::Wait(3),
                Action::Snapshot("06-offline"),
                Action::SeedStatus("wired"),
                Action::Wait(3),
                Action::Snapshot("07-wired-no-battery"),
            ],
        },
        Scenario {
            name: "media_key_osd",
            description: "The bottom-centre pill a volume / brightness / mute / backlight / transport key raises.",
            actions: vec![
                Action::SeedOsd("volume"),
                Action::Wait(14),
                Action::Snapshot("01-volume"),
                Action::SeedOsd("volume_low"),
                Action::Wait(14),
                Action::Snapshot("02-volume-low"),
                Action::SeedOsd("volume_muted"),
                Action::Wait(14),
                Action::Snapshot("03-muted"),
                Action::SeedOsd("brightness"),
                Action::Wait(14),
                Action::Snapshot("04-brightness"),
                Action::SeedOsd("mic_muted"),
                Action::Wait(14),
                Action::Snapshot("05-mic-muted"),
                Action::SeedOsd("next_track"),
                Action::Wait(14),
                Action::Snapshot("06-next-track"),
                Action::SeedOsd("keyboard_backlight"),
                Action::Wait(14),
                Action::Snapshot("07-keyboard-backlight"),
                Action::SeedOsd("no_backlight"),
                Action::Wait(14),
                Action::Snapshot("08-no-backlight"),
            ],
        },
        Scenario {
            name: "wifi_password_prompt",
            description: "Quick settings' Wi-Fi pane asking for a passphrase, empty / typed / rejected.",
            actions: vec![
                Action::SeedWifiPrompt("empty"),
                Action::Wait(8),
                Action::Snapshot("01-asking"),
                Action::SeedWifiPrompt("typed"),
                Action::Wait(4),
                Action::Snapshot("02-typed"),
                Action::SeedWifiPrompt("rejected"),
                Action::Wait(4),
                Action::Snapshot("03-rejected"),
            ],
        },
        Scenario {
            name: "drive_consent_sheet",
            description: "The sheet asking how to open an NTFS drive: the Windows partition and an unknown stick.",
            actions: vec![
                Action::SeedDriveConsent("windows"),
                Action::Wait(6),
                Action::Snapshot("01-windows"),
                Action::SeedDriveConsent("stick"),
                Action::Wait(4),
                Action::Snapshot("02-stick"),
            ],
        },
        Scenario {
            name: "file_chooser_dialog",
            description: "The portal file dialog: open, multi-select open, and save.",
            actions: vec![
                Action::SeedFilePicker("open"),
                Action::Wait(6),
                Action::Snapshot("01-open"),
                Action::SeedFilePicker("open_multiple"),
                Action::Wait(4),
                Action::Snapshot("02-open-multiple"),
                Action::SeedFilePicker("save"),
                Action::Wait(4),
                Action::Snapshot("03-save"),
            ],
        },
        Scenario {
            name: "file_chooser_window_sizes",
            description: "The file dialog is a system sheet: the same size and centred on the display whether a narrow or a wide window asks.",
            actions: vec![
                Action::SetWindowWidths(250),
                Action::SeedFilePicker("open"),
                Action::Wait(6),
                Action::Snapshot("01-small-window-open"),
                Action::SeedFilePicker("save"),
                Action::Wait(6),
                Action::Snapshot("02-small-window-save"),
                Action::SeedFilePicker("folder"),
                Action::Wait(6),
                Action::Snapshot("03-small-window-folder"),
                Action::SetWindowWidths(1_000),
                Action::SeedFilePicker("open"),
                Action::Wait(6),
                Action::Snapshot("04-large-window-open"),
            ],
        },
    ]
}

/// A stand-in favicon: a 16 px orange disc with a white centre, as PNG
/// bytes, the way the engine hands a page's icon over.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "pixel arithmetic on a 16 by 16 image"
)]
fn favicon_png() -> Vec<u8> {
    let mut rgba = Vec::with_capacity(16 * 16 * 4);
    for y in 0..16_i32 {
        for x in 0..16_i32 {
            let (dx, dy) = (x * 2 - 15, y * 2 - 15);
            let d = dx * dx + dy * dy;
            let pixel = if d <= 49 {
                [255, 255, 255, 255]
            } else if d <= 225 {
                [222, 88, 51, 255]
            } else {
                [0, 0, 0, 0]
            };
            rgba.extend_from_slice(&pixel);
        }
    }
    ui::png::encode_rgba8(16, 16, &rgba).unwrap_or_default()
}

/// The browser's slots, on duckduckgo.com: back (it came from a search),
/// the site as its editable location with its favicon, reload, bookmark
/// and the library.
fn browser_slots() -> ui::window_toolbar::WindowToolbar {
    use ui::window_toolbar::{
        ToolbarAction, ToolbarImage, ToolbarLocation, ToolbarNavigation, WindowToolbar,
    };
    let action = |id: &str, icon: &str, name: &str| ToolbarAction {
        id: id.into(),
        icon: icon.into(),
        name: name.into(),
        enabled: true,
        on: false,
        menu: false,
        items: Vec::new(),
        navigates: false,
        toggles: false,
        destructive: false,
        labelled: false,
    };
    WindowToolbar {
        sidebar: None,
        navigation: Some(ToolbarNavigation {
            back: true,
            forward: false,
        }),
        location: Some(ToolbarLocation {
            label: "duckduckgo.com".into(),
            value: "https://duckduckgo.com/?q=tilo+desktop".into(),
            placeholder: "Search or enter address".into(),
            icon: Some("lock".into()),
            editable: true,
            name: "Address and search".into(),
            go_name: "Go".into(),
            image: Some(ToolbarImage { png: favicon_png() }),
            ..ToolbarLocation::default()
        }),
        actions: vec![
            action("reload", "refresh", "Reload"),
            action("bookmark", "star", "Bookmark this page"),
            action("library", "history", "History and bookmarks"),
        ],
        ..WindowToolbar::default()
    }
}

/// Files in ~/Downloads, as Files declares it: the places list's toggle,
/// back and forward, the folder's path as a breadcrumb that edits as a path,
/// Up, the view and New folder in the row and the rest of the View options
/// in the overflow menu.
fn files_slots() -> ui::window_toolbar::WindowToolbar {
    use ui::window_toolbar::{
        LocationSegment, ToolbarAction, ToolbarLocation, ToolbarNavigation, WindowToolbar,
    };
    let action = |id: &str, icon: &str, name: &str| ToolbarAction {
        id: id.into(),
        icon: icon.into(),
        name: name.into(),
        enabled: true,
        on: false,
        menu: false,
        items: Vec::new(),
        navigates: false,
        toggles: false,
        destructive: false,
        labelled: false,
    };
    let segment = |label: &str, value: &str| LocationSegment {
        label: label.into(),
        value: value.into(),
    };
    WindowToolbar {
        sidebar: Some(true),
        navigation: Some(ToolbarNavigation {
            back: true,
            forward: false,
        }),
        location: Some(ToolbarLocation {
            label: "Downloads".into(),
            value: "~/Downloads".into(),
            placeholder: "Type a path, e.g. ~/Documents".into(),
            editable: true,
            name: "Location".into(),
            segments: vec![segment("Home", "~"), segment("Downloads", "~/Downloads")],
            ..ToolbarLocation::default()
        }),
        actions: vec![
            action("up", "arrow-up", "Up one folder"),
            action("view", "layout-grid", "Show as grid"),
            action("new-folder", "folder-plus", "New folder"),
            action("new-file", "file-plus", "New file…"),
            action("terminal", "square-terminal", "Open terminal here"),
            action("hidden", "eye", "Show hidden files"),
        ],
        ..WindowToolbar::default()
    }
}

/// The terminal's slots, as Terminal declares them on the device's own
/// shell: where the shell runs as the window's title, and the places the
/// target picker offers, the current one checked.
fn terminal_slots() -> ui::window_toolbar::WindowToolbar {
    use ui::window_toolbar::{LocationChoice, ToolbarLocation, WindowToolbar};
    let choice = |label: &str, value: &str, current: bool, separated: bool| LocationChoice {
        label: label.into(),
        value: value.into(),
        current,
        separated,
    };
    WindowToolbar {
        location: Some(ToolbarLocation {
            label: "Your shell on this device".into(),
            name: "Choose where this runs".into(),
            choices: vec![
                choice("Default", "open:default", false, false),
                choice("Work", "open:work", false, false),
                choice("Your shell on this device", "open:host", true, false),
                choice("linux virtual machine", "open:linux", false, false),
                choice("Manage environments…", "manage", false, true),
            ],
            ..ToolbarLocation::default()
        }),
        ..WindowToolbar::default()
    }
}

/// The browser's slots while "youtube.com" is typed into its address: the
/// omnibox's rows under the field, as Browser answers (the address, the
/// search with its engine, pages visited before with their addresses, the
/// engine's own searches).
fn browser_suggesting_slots() -> ui::window_toolbar::WindowToolbar {
    use ui::window_toolbar::LocationSuggestion;
    let row = |label: &str, detail: &str, value: &str, icon: &str| LocationSuggestion {
        label: label.into(),
        value: value.into(),
        icon: Some(icon.into()),
        detail: detail.into(),
    };
    let mut slots = browser_slots();
    if let Some(location) = slots.location.as_mut() {
        location.suggestions = vec![
            row("youtube.com", "", "https://youtube.com", "globe"),
            row(
                "youtube.com",
                "Search with DuckDuckGo",
                "https://duckduckgo.com/?q=youtube.com",
                "search",
            ),
            row(
                "YouTube",
                "youtube.com",
                "https://www.youtube.com/",
                "history",
            ),
            row(
                "Me at the zoo - YouTube",
                "youtube.com/watch?v=jNQXAC9IVRw",
                "https://www.youtube.com/watch?v=jNQXAC9IVRw",
                "history",
            ),
            row(
                "youtube music",
                "",
                "https://duckduckgo.com/?q=youtube+music",
                "search",
            ),
        ];
    }
    slots
}

const BAR_BROWSER: BarTab = BarTab {
    app_id: "so.tilo.browser",
    title: "DuckDuckGo",
    surface_id: 9101,
    slots: Some(browser_slots),
};
const BAR_FILES: BarTab = BarTab {
    app_id: "so.tilo.file-explorer",
    title: "Downloads",
    surface_id: 9102,
    slots: Some(files_slots),
};
const BAR_TERMINAL: BarTab = BarTab {
    app_id: "so.tilo.terminal",
    title: "Terminal",
    surface_id: 9104,
    slots: Some(terminal_slots),
};
/// A foreign app: no slots, its client's title is its location.
const BAR_WRITER: BarTab = BarTab {
    app_id: "libreoffice-writer",
    title: "Q3 report.odt - LibreOffice Writer",
    surface_id: 9103,
    slots: None,
};

fn window_bar_browser_scenario() -> Scenario {
    Scenario {
        name: "window-bar-browser",
        description: "The browser's toolbar as slots in its window's bar, untabbed: \
             back and forward, the site as its location, reload, bookmark and \
             the library, then the window's controls; the location edited, \
             with its go button; maximized.",
        actions: vec![
            Action::SeedWindowBar {
                tabs: &[BAR_BROWSER],
                active: 0,
                width: 700,
            },
            Action::Wait(30),
            Action::Relayout,
            Action::Snapshot("01-untabbed"),
            Action::ClickNamed("window_location"),
            Action::Wait(4),
            Action::Snapshot("02-location-edited"),
            Action::Type("example.org"),
            Action::Wait(4),
            Action::Snapshot("03-typed"),
            Action::Key(Key::Named(NamedKey::Escape)),
            Action::Wait(4),
            Action::Modifiers(ModifiersState::SUPER),
            Action::Key(Key::Character("m".into())),
            Action::Modifiers(ModifiersState::empty()),
            Action::Wait(PLACEMENT_SETTLE_FRAMES),
            Action::Relayout,
            Action::Snapshot("04-maximized"),
        ],
    }
}

fn window_bar_tabbed_scenario() -> Scenario {
    Scenario {
        name: "window-bar-tabbed",
        description: "Three tabs in one window: the tab row with the window's \
             controls, then the active tab's slots in a row of their own, \
             cross-fading as another tab is shown; a tab with no slots shows \
             no second row.",
        actions: vec![
            Action::SeedWindowBar {
                tabs: &[BAR_BROWSER, BAR_FILES, BAR_WRITER],
                active: 0,
                width: 700,
            },
            Action::Wait(30),
            Action::Relayout,
            Action::Snapshot("01-browser-active"),
            Action::ClickNamed("window_tab:1"),
            Action::Snapshot("02-to-files-000"),
            Action::SnapshotSequence {
                prefix: "02-to-files",
                frames: 4,
                gap_frames: 2,
            },
            Action::Wait(30),
            Action::Relayout,
            Action::Snapshot("03-files-active"),
            Action::ClickNamed("window_tab:2"),
            Action::Wait(30),
            Action::Relayout,
            Action::Snapshot("04-writer-active"),
        ],
    }
}

fn window_bar_files_scenario() -> Scenario {
    Scenario {
        name: "window-bar-files",
        description: "Files in Downloads, untabbed: back and forward and the \
             folder as its location in the window's bar.",
        actions: vec![
            Action::SeedWindowBar {
                tabs: &[BAR_FILES],
                active: 0,
                width: 700,
            },
            Action::Wait(30),
            Action::Relayout,
            Action::Snapshot("01-untabbed"),
        ],
    }
}

fn window_bar_terminal_scenario() -> Scenario {
    Scenario {
        name: "window-bar-terminal",
        description: "A terminal's window: where its shell runs is the \
             window's title in its bar, a picker with a chevron; a click opens \
             the system menu of the places it can run, the current one checked, \
             and Manage environments… after a separator.",
        actions: vec![
            Action::SeedWindowBar {
                tabs: &[BAR_TERMINAL],
                active: 0,
                width: 700,
            },
            Action::Wait(30),
            Action::Relayout,
            Action::Snapshot("01-title"),
            Action::ClickNamed("window_location"),
            Action::Wait(20),
            Action::Snapshot("02-picker"),
        ],
    }
}

fn window_bar_overflow_scenario() -> Scenario {
    Scenario {
        name: "window-bar-overflow",
        description: "Files in a narrow window: the actions that have no room \
             move into the bar's overflow button, whose menu lists them by \
             name; the buttons' tooltips name them too.",
        actions: vec![
            Action::SeedWindowBar {
                tabs: &[BAR_FILES],
                active: 0,
                width: 380,
            },
            Action::Wait(30),
            Action::Relayout,
            Action::Snapshot("01-narrow"),
            Action::ClickNamed("window_more"),
            Action::Wait(20),
            Action::Snapshot("02-overflow-menu"),
            Action::Key(Key::Named(NamedKey::Escape)),
            Action::Wait(10),
            Action::MoveCursorNamed("window_action:0"),
            Action::Wait(60),
            Action::Snapshot("03-tooltip"),
        ],
    }
}

fn window_bar_suggestions_scenario() -> Scenario {
    Scenario {
        name: "window-bar-suggestions",
        description: "The browser's address edited in the window's bar: what is \
             typed goes to the browser, whose suggestions the bar lists under \
             the field.",
        actions: vec![
            Action::SeedWindowBar {
                tabs: &[BAR_BROWSER],
                active: 0,
                width: 700,
            },
            Action::Wait(30),
            Action::Relayout,
            Action::ClickNamed("window_location"),
            Action::Wait(4),
            Action::Type("youtube.com"),
            Action::DeclareBarSlots {
                surface_id: 9101,
                slots: browser_suggesting_slots,
            },
            Action::Wait(20),
            Action::Snapshot("01-suggestions"),
            Action::Key(Key::Named(NamedKey::ArrowDown)),
            Action::Key(Key::Named(NamedKey::ArrowDown)),
            Action::Wait(10),
            Action::Snapshot("02-second-chosen"),
            Action::MoveCursorNamed("window_suggestion:3"),
            Action::Wait(10),
            Action::Snapshot("03-pointer-over-a-visited-page"),
        ],
    }
}

fn window_bar_writer_scenario() -> Scenario {
    Scenario {
        name: "window-bar-writer",
        description: "LibreOffice Writer, a foreign app with no slots: its \
             title without the app's name is its location.",
        actions: vec![
            Action::SeedWindowBar {
                tabs: &[BAR_WRITER],
                active: 0,
                width: 700,
            },
            Action::Wait(30),
            Action::Relayout,
            Action::Snapshot("01-untabbed"),
        ],
    }
}

/// The compositor snapshot a laptop panel with desk monitors would publish
/// for `outputs`: the first the built-in panel, the next a named monitor on
/// `DP-1`, any further ones bare ports.
fn identify_snapshot(
    outputs: &[shell::shell::overlay_host::OutputGeometry],
) -> settings_store::hardware::DisplayHardwareSnapshot {
    use settings_store::hardware::{
        DisplayHardwareSnapshot, DisplayOutputSnapshot, LiveOutputPlacement, MonitorIdentity,
        OutputDescriptor, OutputMode,
    };
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a display's logical position and size fit i32"
    )]
    fn round(value: f64) -> i32 {
        value.round() as i32
    }
    let mut snapshot = DisplayHardwareSnapshot::default();
    for (index, output) in outputs.iter().enumerate() {
        let connector = if index == 0 {
            "eDP-1".to_owned()
        } else {
            format!("DP-{index}")
        };
        let mode = OutputMode {
            width: u32::try_from(round(output.width * output.scale)).unwrap_or(1920),
            height: u32::try_from(round(output.height * output.scale)).unwrap_or(1080),
            refresh_mhz: 60_000,
        };
        snapshot.outputs.push(DisplayOutputSnapshot {
            descriptor: OutputDescriptor {
                connector: connector.clone(),
                connector_label: connector.clone(),
                modes: vec![mode],
                hdr: false,
                vrr: false,
                color_profiles: Vec::new(),
                monitor: (index == 1).then(|| MonitorIdentity {
                    make: "Dell".into(),
                    model: "DELL U2724DE".into(),
                    ..MonitorIdentity::default()
                }),
            },
            current_mode: mode,
            scale: output.scale,
            builtin: index == 0,
            sink_hdr: false,
            hdr_unavailable_reason: None,
            sink_vrr: false,
            vrr_unavailable_reason: None,
            unavailable_reason: None,
            color_profile_labels: std::collections::BTreeMap::new(),
        });
        snapshot.layout.push(LiveOutputPlacement {
            connector,
            x: round(output.x),
            y: round(output.y),
            width: u32::try_from(round(output.width)).unwrap_or(0),
            height: u32::try_from(round(output.height)).unwrap_or(0),
        });
    }
    snapshot
}

pub fn scenarios() -> &'static [Scenario] {
    use std::sync::LazyLock;
    static TABLE: LazyLock<Vec<Scenario>> = LazyLock::new(|| {
        [
            build_scenarios(),
            motion_review::all(),
            motion_gallery::all(),
        ]
        .concat()
    });
    &TABLE
}

pub fn catalogue() -> Vec<Scenario> {
    scenarios().to_vec()
}

pub fn lookup(name: &str) -> Option<Scenario> {
    scenarios().iter().find(|s| s.name == name).cloned()
}

// =====================================================================
// Tests
// =====================================================================

#[allow(
    unused_imports,
    reason = "The native WorkspaceExt methods are selected by host-specific workspace code and test configurations."
)]
use shell::shell::state::WorkspaceExt as _;

/// A window's own toolbar, as its app declares it.
type ToolbarOf = fn() -> ui::window_toolbar::WindowToolbar;

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
mod tests {
    use super::*;

    /// Counter sink for assertions: records each `(name, query)` pair
    /// without writing any files. Lets the unit test verify the
    /// scenario engine actually invokes the sink in order.
    struct CountingSink {
        seen: Vec<(String, String)>,
    }
    impl SnapshotSink for CountingSink {
        fn capture(&mut self, driver: &mut ShellDriver, name: &str) {
            self.seen
                .push((name.to_string(), driver.palette_query().to_string()));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn scenario_steps_drive_the_shell_and_snapshot_only_when_asked() {
        // Phase `scenario_palette_typing_lands_query`.
        {
            let mut d = ShellDriver::new((1600, 900));
            let mut sink = CountingSink { seen: Vec::new() };
            let scenario = lookup("palette_typing").expect("palette_typing in catalogue");
            run(&mut d, &scenario, &mut sink);
            // Locate the snapshot taken right after the `Type("h")` step.
            let typed = sink
                .seen
                .iter()
                .find(|(n, _)| n == "03-palette-typed-h")
                .expect("scenario should snapshot 03-palette-typed-h");
            assert_eq!(
                typed.1, "h",
                "after Type(\"h\") with palette open, the query should be 'h'"
            );
            let cleared = sink
                .seen
                .iter()
                .find(|(n, _)| n == "04-palette-cleared")
                .expect("scenario should snapshot 04-palette-cleared");
            assert_eq!(cleared.1, "", "the first Esc clears the query");
            // Final state: the second Esc closed the empty palette.
            assert!(
                !d.host().palette_visible(),
                "Esc should close the palette by the end of the scenario"
            );
        }
        // Phase `scenario_catalogue_lookup_round_trip`.
        {
            for s in catalogue() {
                let found = lookup(s.name).expect("named scenario should be in catalogue");
                assert_eq!(found.name, s.name);
            }
            assert!(lookup("does_not_exist").is_none());
        }
        // Phase `scenario_workspace_switch_moves_active_index`.
        {
            let mut d = ShellDriver::new((1600, 900));
            let initial = d.chrome().tree().desktop.active;
            let mut sink = NullSink;
            let scenario = lookup("workspace_switch").expect("workspace_switch in catalogue");
            run(&mut d, &scenario, &mut sink);
            // After the round-trip the active workspace should match the
            // initial one again.
            assert_eq!(d.chrome().tree().desktop.active, initial);
        }
        // Phase `scenario_post_animation_waits_outlast_their_animation`.
        // Regression: post-action waits in built-in scenarios must outlast
        // the longest in-flight animation, otherwise offscreen snapshots
        // capture mid-animation pixels (slide voids, layout-spring pill
        // overlap) instead of the settled state the scenario name implies.
        //
        // We only enforce the obvious cases here — the catalogue grows,
        // and not every wait gates a known animation — but the two we did
        // fix during the 2026-05-04 inspection sweep are pinned so a future
        // edit that shortens them re-introduces the visual artefacts.
        {
            // tab_drag releases a drag whose post-drop layout spring runs
            // for LAYOUT_ANIM_MS = 180 ms (see ui::widget::tabs).
            let td = lookup("tab_drag").expect("tab_drag in catalogue");
            let td_waits: Vec<u32> = td
                .actions
                .iter()
                .filter_map(|a| match a {
                    Action::Wait(n) => Some(*n),
                    _ => None,
                })
                .collect();
            assert!(!td_waits.is_empty(), "tab_drag must contain Wait actions");
            for n in &td_waits {
                assert!(
                    u64::from(*n) * 16 >= 180,
                    "tab_drag Wait({n}) is shorter than LAYOUT_ANIM_MS=180; snapshot will paint adjacent pills overlapping"
                );
            }
        }
        // Phase `chrome_state_json_includes_palette_query`.
        {
            let mut d = ShellDriver::new((1600, 900));
            d.host_mut().set_palette_visible(true);
            d.host_mut().set_palette_query("hello");
            let json = chrome_state_json(&d);
            assert!(
                json.contains("\"palette_query\":\"hello\""),
                "json should include palette_query: {json}"
            );
            assert!(json.contains("\"palette_visible\":true"));
        }
        // Phase `per_frame_flag_inserts_snapshots_between_waits`.
        // `run_with(per_frame = Some(n))` must insert `n` intermediate
        // snapshots between a named snapshot and the surrounding waits.
        // Pinned so a future edit that drops the wait expansion would
        // re-introduce the "single still frame for animated scenarios"
        // gap the harness was added to close.
        {
            // Synthetic minimal scenario: one named snapshot, one wait, one
            // closing snapshot. Drives no real animation so the test stays
            // fast (~step_frames sleeps for 16 ms × frames).
            let scenario = Scenario {
                name: "_per_frame_test",
                description: "",
                actions: vec![
                    Action::Snapshot("a"),
                    Action::Wait(4),
                    Action::Snapshot("b"),
                ],
            };
            let mut d = ShellDriver::new((128, 96));
            let mut sink = NameSink { names: Vec::new() };
            run_with(&mut d, &scenario, &mut sink, Some(4));
            // Expected: ["a", "a-frame00", "a-frame01", "a-frame02", "a-frame03", "b"].
            // The intermediate frames hang off the most-recent named snapshot
            // ("a"), so a regression that loses the upstream name would
            // surface as "frame-00" instead of "a-frame00".
            assert_eq!(
                sink.names,
                vec![
                    "a".to_string(),
                    "a-frame00".to_string(),
                    "a-frame01".to_string(),
                    "a-frame02".to_string(),
                    "a-frame03".to_string(),
                    "b".to_string(),
                ],
                "per-frame expansion must insert 4 intermediate snapshots between Wait(4) and Snapshot",
            );
        }
        // Phase `per_frame_none_is_identical_to_plain_run`.
        // When `per_frame` is `None`, `run_with` must behave exactly like
        // `run` — no extra snapshots inserted.
        {
            let scenario = Scenario {
                name: "_no_per_frame",
                description: "",
                actions: vec![
                    Action::Snapshot("first"),
                    Action::Wait(2),
                    Action::Snapshot("second"),
                ],
            };
            let mut d = ShellDriver::new((128, 96));
            let mut sink = NameSink { names: Vec::new() };
            run_with(&mut d, &scenario, &mut sink, None);
            assert_eq!(sink.names, vec!["first".to_string(), "second".to_string()],);
        }
    }

    #[test]
    fn collaboration_product_proof_keeps_states_distinct_and_backed_by_claims() {
        // Phase `collaboration_product_proof_keeps_invitation_presence_and_recovery_distinct`.
        {
            let scenario = lookup("collaboration_product_proof")
                .expect("collaboration_product_proof in catalogue");
            let snapshots: Vec<_> = scenario
                .actions
                .iter()
                .filter_map(|action| match action {
                    Action::Snapshot(name) => Some(*name),
                    _ => None,
                })
                .collect();
            assert_eq!(
                snapshots,
                vec![
                    "01-solo-workspace",
                    "02-invite-pending",
                    "03-ada-editing-notes",
                    "04-reverted-forward",
                    "05-ada-in-files",
                ],
                "the proof must retain every product state rather than skipping from invite to history"
            );
            assert!(
                scenario
                    .actions
                    .windows(2)
                    .any(|pair| matches!(pair, [Action::SeedInvitePending, Action::Wait(_)]))
            );
            assert!(
                scenario
                    .actions
                    .iter()
                    .any(|action| matches!(action, Action::RevertRoomActivity))
            );
        }
        // Phase `collaboration_product_proof_frames_are_backed_by_the_claimed_state`.
        {
            let mut driver = ShellDriver::new((1600, 900));
            let mut sink = CollaborationProofSink {
                pending_invite_is_durable: false,
                forward_revert_is_announced: false,
                ada_is_active_in_files: false,
            };
            run(
                &mut driver,
                &lookup("collaboration_product_proof")
                    .expect("collaboration_product_proof in catalogue"),
                &mut sink,
            );
            assert!(
                sink.pending_invite_is_durable,
                "the pending-invite frame must show both durable access and non-live presence"
            );
            assert!(
                sink.forward_revert_is_announced,
                "the reverted frame must be backed by a new forward recovery"
            );
            assert!(
                sink.ada_is_active_in_files,
                "the last frame must place the admitted collaborator in real Files content"
            );
        }
    }

    /// Snapshot names are a contract with the visual review suite. Check the
    /// actual state at each consequential frame as well, so a future fixture
    /// regression cannot quietly emit a plausible but false PNG.
    struct CollaborationProofSink {
        pending_invite_is_durable: bool,
        forward_revert_is_announced: bool,
        ada_is_active_in_files: bool,
    }

    impl SnapshotSink for CollaborationProofSink {
        #[allow(
            clippy::significant_drop_tightening,
            reason = "the borrowed workspace must stay alive while each named snapshot invariant is inspected"
        )]
        fn capture(&mut self, driver: &mut ShellDriver, name: &str) {
            let state = driver.chrome();
            let tree = state.tree();
            let Some(workspace) = tree.desktop.active_workspace() else {
                return;
            };
            match name {
                "02-invite-pending" => {
                    let ada = workspace
                        .collaborators
                        .iter()
                        .find(|person| person.id == "ada");
                    self.pending_invite_is_durable = ada.is_some_and(|person| {
                        person.invited
                            && person.active_tab.is_none()
                            && workspace.role_of("ada").is_some()
                    });
                }
                "04-reverted-forward" => {
                    self.forward_revert_is_announced = state.notifications().iter().any(|notice| {
                        notice.title == "Reverted a workspace change"
                            && notice.body.contains("Written forward")
                    });
                }
                "05-ada-in-files" => {
                    let files_tab = workspace
                        .strips
                        .iter()
                        .flat_map(|strip| &strip.window_groups)
                        .flat_map(|group| &group.tabs)
                        .find(|tab| tab.title == "Files")
                        .map(|tab| tab.id);
                    self.ada_is_active_in_files = files_tab.is_some_and(|files_tab| {
                        workspace.collaborators.iter().any(|person| {
                            person.id == "ada"
                                && !person.invited
                                && person.active_tab == Some(files_tab)
                        })
                    });
                }
                _ => {}
            }
        }
    }

    /// Sink that only records snapshot names — used to verify the
    /// per-frame expansion in `run_with` inserts the right number of
    /// intermediate captures between snapshots.
    struct NameSink {
        names: Vec<String>,
    }
    impl SnapshotSink for NameSink {
        fn capture(&mut self, _driver: &mut ShellDriver, name: &str) {
            self.names.push(name.to_string());
        }
    }

    /// Sink that records what the strip's scroll looks like at each
    /// named snapshot, straight off the live `GroupRow` the shell pans.
    struct StripScrollSink {
        seen: Vec<(String, f64, f64, f64, f64)>,
    }
    impl SnapshotSink for StripScrollSink {
        fn capture(&mut self, driver: &mut ShellDriver, name: &str) {
            let strip_id = {
                let st = driver.chrome();
                let tree = st.tree();
                tree.desktop
                    .active_workspace()
                    .and_then(|ws| ws.strips.first().map(|s| s.id))
            };
            let (scroll_x, max_scroll_x, pan_max_scroll_x, leading_inset) = strip_id
                .and_then(|sid| driver.host_mut().group_row_mut_for_tests(sid))
                .map_or((f64::NAN, f64::NAN, f64::NAN, f64::NAN), |row| {
                    (
                        row.scroll_x(),
                        row.max_scroll_x(),
                        row.pan_max_scroll_x(),
                        row.leading_inset(),
                    )
                });
            self.seen.push((
                name.to_string(),
                scroll_x,
                max_scroll_x,
                pan_max_scroll_x,
                leading_inset,
            ));
        }
    }

    /// The strip scenario has to end up with four *windows*, each one a
    /// live app — not four tabs of one window, and not stub cards.
    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "strip (borrowed from tree) is read all the way through the assertions \
                  at the end of this test; there's no earlier point to drop it without \
                  cloning the strip data out for no real gain"
    )]
    fn strip_scroll_scenario_opens_four_hosted_windows_side_by_side() {
        let mut d = ShellDriver::new((1600, 900));
        let mut sink = NullSink;
        run(
            &mut d,
            &lookup("strip_scroll_across_apps").expect("strip_scroll_across_apps in catalogue"),
            &mut sink,
        );

        let st = d.chrome();
        let tree = st.tree();
        let strip = &tree
            .desktop
            .active_workspace()
            .expect("active workspace")
            .strips[0];
        let kinds: Vec<&shell::shell::state::AppKind> = strip
            .window_groups
            .iter()
            .map(|g| {
                assert_eq!(
                    g.tabs.len(),
                    1,
                    "each app has to be its own window, not a tab of the one before it"
                );
                match &g.tabs[0].tile {
                    shell::shell::state::TileContent::AppPlaceholder { kind, .. } => kind,
                    other => panic!("expected a hosted app tile, got {other:?}"),
                }
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                &shell::shell::state::AppKind::Other("Notes".into()),
                &shell::shell::state::AppKind::FileExplorer,
                &shell::shell::state::AppKind::Terminal,
                &shell::shell::state::AppKind::Other("Editor".into()),
            ],
            "four distinct apps, in the order the scenario opens them"
        );
        for g in &strip.window_groups {
            assert_eq!(
                shell::shell::resolve_tile_shape_for_test(&g.tabs[0].tile),
                "App",
                "every window in the strip has to render a real app, not a stub card"
            );
        }
    }

    /// And it has to actually travel: the strip holds more than fits, direct
    /// manipulation pulls beyond the legal trailing rest with resistance,
    /// release settles there, and a reverse pan returns home. Read each state
    /// off the live `GroupRow` the shell pans.
    #[test]
    fn strip_scroll_scenario_pans_the_strip_and_back() {
        let mut d = ShellDriver::new((1600, 900));
        let mut sink = StripScrollSink { seen: Vec::new() };
        run(
            &mut d,
            &lookup("strip_scroll_across_apps").expect("strip_scroll_across_apps in catalogue"),
            &mut sink,
        );

        let at = |name: &str| -> (f64, f64, f64, f64) {
            let (_, sx, mx, pan_mx, inset) = sink
                .seen
                .iter()
                .find(|(n, ..)| n == name)
                .unwrap_or_else(|| panic!("scenario should snapshot {name}"));
            (*sx, *mx, *pan_mx, *inset)
        };
        let (rest_x, rest_max, rest_pan_max, rest_inset) = at("05-strip-at-rest");
        assert!(
            rest_max > 400.0,
            "four half-width windows must overflow the display by more than a third of \
             a screen, or there is nothing to pan across (max_scroll_x = {rest_max})"
        );
        // The strip's leading inset is the fixed edge gutter
        // (`STRIP_EDGE_GUTTER_PX`, 32 px, the same as the trailing one).
        assert!(
            (rest_inset - 32.0).abs() < 0.5,
            "the leading inset is the 32 px edge gutter (leading inset = {rest_inset})"
        );
        // Focusing the first window brings the strip home: the window's
        // edge is drawn just outside it, so stopping flush with the screen
        // edge would cut its border off.
        assert!(
            rest_x < 1.0,
            "the strip starts home, the gutter before its first window \
             (scroll_x = {rest_x}, leading inset = {rest_inset})"
        );
        assert!(
            rest_pan_max > rest_max,
            "direct panning must expose legal trailing canvas beyond the content-tight bound"
        );
        let (held_x, held_max, held_pan_max, _) = at("06-held-past-trailing-edge");
        assert!(
            held_x > held_pan_max,
            "the held gesture must visibly pull beyond its legal resting end \
             (scroll_x = {held_x}, pan_max_scroll_x = {held_pan_max})"
        );
        // `max_scroll_x` keeps the gutter after the last window too.
        let viewport = (held_pan_max - held_max + rest_inset) / 0.20;
        assert!(
            held_x < held_pan_max + viewport * 0.28,
            "rubber-band travel must stay below its 28%-viewport asymptote \
             (scroll_x = {held_x}, pan_max_scroll_x = {held_pan_max})"
        );
        let (settled_x, _, settled_pan_max, _) = at("07-settled-trailing-room");
        assert!(
            (settled_x - settled_pan_max).abs() < 1.0,
            "release must settle to the legal 20%-viewport trailing rest \
             (scroll_x = {settled_x}, pan_max_scroll_x = {settled_pan_max})"
        );
        let (back_x, ..) = at("08-panned-back");
        assert!(
            back_x < 1.0,
            "the reverse direct pan returns the strip home (scroll_x = {back_x})"
        );
    }
}
