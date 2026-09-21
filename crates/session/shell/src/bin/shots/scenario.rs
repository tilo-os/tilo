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
//! Built-in catalogue lives at the bottom; new scenarios go there as
//! `pub fn` constructors so `--list-scenarios` and `--scenario NAME`
//! pick them up automatically.

use std::cell::RefCell;
use std::path::Path;

use vello::kurbo::Point;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use shell::shell::chrome_state::{ChromeState, WsTab};
use shell::shell::quick_settings::QsLevel;

use super::driver::ShellDriver;

/// One step in a scenario.
///
/// `ClickNamed`, `Modifiers`, `SetWsSettings` and `PushQsLevel` are
/// currently unconstructed: `run_action` implements all four, but no
/// scenario in `catalogue()` reaches for them yet. They are the DSL's
/// surface, not dead branches — deleting them would take
/// `resolve_named_rect`-based click targeting out of reach of the next
/// scenario that needs it.
#[derive(Debug, Clone)]
#[allow(
    dead_code,
    reason = "the screenshot scenario DSL intentionally keeps actions available before every variant has a catalogue caller"
)]
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
    /// Move the cursor onto a named target's centre without pressing.
    /// Sibling of [`Action::ClickNamed`], for the affordances that are
    /// hover-driven — a tab pill opening its control cluster — where a
    /// literal coordinate would go stale the moment the row re-sizes.
    MoveCursorNamed(&'static str),
    /// Synthesise a `CursorMoved` without a press/release. Use with
    /// `MousePress`/`MouseRelease` to compose drags that need mid-
    /// gesture snapshots — useful for the new tab-bar `hover_drop_idx`
    /// indicator which only paints between press and release.
    MoveCursor(Point),
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
    Type(String),
    /// Replace the active modifier set. Subsequent `Key(_)` actions see
    /// these modifiers in their `is_mod` check inside the host. The
    /// scenario engine resets modifiers after each `Snapshot` so chord
    /// state doesn't leak between named outputs.
    Modifiers(ModifiersState),
    /// Step animation `n` frames (~16 ms each).
    Wait(u32),
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
    /// Pin the launch-to-top-bar recorder transition for visual review. The
    /// live shell uses elapsed time; shots use permille so every phase remains
    /// inspectable even under a slow software renderer.
    CaptureRecordingTransition(u16),
    /// Pin the selected-pixels-to-PiP morph for deterministic visual review.
    /// PNG rendering can be slower than the live animation, so wall-clock
    /// snapshot sequences cannot faithfully expose its intermediate frames.
    CapturePipTransition(u16),
    /// Toggle the visual recorder session between live and paused without
    /// starting an encoder, for deterministic indicator review.
    CaptureRecordingPaused(bool),
    /// Pin pause/resume label and icon morphs under slow software rendering.
    CaptureRecordingStateTransition(u16),
    /// Transition the active pill through the same compositor-confirmed save
    /// receipt used after a successful file flush.
    CaptureRecordingFinished,
    /// Pin the saved receipt entrance under slow software rendering.
    CaptureRecordingReceiptTransition(u16),
    CaptureReplayActive(bool),
    CaptureReplayBuffered(u8),
    CaptureReplaySaved,
    CaptureScreenshotFinished {
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
    /// Seed real collaborative room changes and open one of the two history
    /// surfaces. `explore = false` captures outcome-first Catch up;
    /// `true` captures the full three-panel browser.
    SeedRoomActivity {
        explore: bool,
    },
    /// Seed two context-grouped sessions without writing named ranges. Used
    /// to prove the first human seam correction materializes shared bounds.
    SeedAutomaticWorkSessions,
    ExpandAutomaticWorkSession,
    CorrectAutomaticWorkSession,
    /// Switch the seeded causal history into workspace replay.
    ShowRoomActivityReplay,
    /// Narrow the open activity surface to Ada's real authored changes so a
    /// collaboration proof cannot accidentally show only the local author.
    FilterRoomActivityToAda,
    /// Add two present collaborators and representative shares to the active
    /// workspace through the production capability path.
    SeedSharedWorkspace,
    /// Admit a named agent and expose one real foreign surface to its live
    /// session through the compositor-authoritative privacy snapshot.
    SeedAgentScreenExposure {
        surface_id: u64,
    },
    SeedInferredStatus(&'static str),
    /// Fill the active room with enough live participants to exercise the
    /// production People pager without weakening each row's action/intent UI.
    SeedCrowdedWorkspace,
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
    /// Select a real timeline row so provenance and comparison are exercised.
    SelectRoomActivityEntry(usize),
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
    /// `placement::placement_for_new_window` and
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
    /// Press on the workspace wheel's rim and drag it `dy` px vertically
    /// in `steps` increments, WITHOUT releasing.
    ///
    /// Self-locating: the press lands on the disc's own laid-out rect,
    /// so the scenario does not carry coordinates that go stale when the
    /// sidebar's width changes with the viewport. Leaving the button
    /// down is the point — the indicator stepping across workspaces is a
    /// mid-drag state, and a scenario that released could never capture
    /// it.
    DragWheelRim {
        dy: f64,
        steps: u32,
    },
    /// Sample the workspace wheel's settle at fixed points along its
    /// curve and capture each as `prefix-<ms>`.
    ///
    /// The escape [`Action::SampleTabReveal`] gives, pointed at the
    /// wheel: a release's settle lasts about half a second and one
    /// offscreen render costs more wall time than that, so stepping
    /// frames catches the wheel where it was let go and then already
    /// parked. This asks the disc to hold the production spring at a
    /// chosen millisecond instead, and what renders is the production
    /// `draw` there.
    SampleWheelSettle {
        prefix: &'static str,
        /// Milliseconds into the settle.
        at_ms: &'static [u64],
    },
    /// Put the selection on the `idx`-th group of the active strip, or
    /// clear it entirely with `None` — which is the state rule 3 (place
    /// at a seam) is about.
    SelectGroup(Option<usize>),
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
    /// Cut the `idx`-th group of the active strip down to one tab, so a
    /// scenario can set up the "lone window selected" shape rather than
    /// the four-tab group the default workspace boots with.
    LoneWindow(usize),
    /// Replace one live shell-hosted window with a long Notes document. This
    /// exercises complete semantic App capture through the same provider and
    /// shared card path used by the shell, without typing hundreds of
    /// deterministic key events into the visual harness.
    SeedLongNotes(usize),
    /// Resize every window in the active strip to `permille` of the
    /// display's usable width — what dragging the resize handles gets
    /// you.
    ///
    /// A capture of windows moving needs more than one of them on
    /// screen, and the default width is the whole display.
    SetWindowWidths(u16),
    SwitchWorkspace(usize),
    SetWsSettings(WsTab),
    PushQsLevel(QsLevel),
    /// Open the production host-session first-login chrome with an isolated,
    /// nonexistent identity path so screenshot tests cannot mutate user data.
    ShowHostDesktopOnboarding,
    SetOnboardingVisual(shell::onboarding_view::OnboardingVisualState),
    SetLockVisual(super::driver::LockVisualState),
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
    apply_hosted_window_and_wheel_drag(driver, action);
    apply_wheel_and_reveal_samples(driver, action, sink);
    apply_make_space_sample_and_widths(driver, action, sink);
    apply_seed_status_and_file_picker(driver, action);
    apply_seed_wifi_and_select_group(driver, action);
    apply_window_and_workspace_actions(driver, action);
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
        Action::Relayout => driver.relayout(),
        Action::MoveCursor(p) => driver.move_cursor(*p),
        Action::MoveCursorNamed(name) => match driver.resolve_named_centre(name) {
            Some(p) => driver.move_cursor(p),
            None => eprintln!(
                "scenario: could not resolve named target '{name}' (rect not populated yet — paint first?)"
            ),
        },
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
            // Drive the default Super+K binding through the host's keyboard branch instead
            // of `set_palette_visible(true)` directly — the hotkey path
            // calls `toggle_palette()` which also focuses the palette
            // input + clears the residual query (see ShellOverlayHost::
            // toggle_palette). Without that, subsequent `Type(_)`
            // actions would fall through because the palette ignores
            // characters when `!is_focused`.
            driver.set_modifiers(ModifiersState::SUPER);
            driver.press_key(&Key::Character(winit::keyboard::SmolStr::new("k")));
            driver.set_modifiers(ModifiersState::empty());
        }
        Action::CaptureRecordingActive(active) => {
            driver.set_capture_recording_active(*active);
        }
        Action::CaptureRecordingTransition(permille) => {
            driver.set_capture_recording_transition_progress(*permille);
        }
        Action::CapturePipTransition(permille) => {
            driver.set_capture_pip_transition_progress(*permille);
        }
        Action::CaptureRecordingPaused(paused) => {
            driver.set_capture_recording_paused(*paused);
        }
        Action::CaptureRecordingStateTransition(permille) => {
            driver.set_capture_recording_state_transition_progress(*permille);
        }
        Action::CaptureRecordingFinished => {
            driver.set_capture_recording_finished();
        }
        Action::CaptureRecordingReceiptTransition(permille) => {
            driver.set_capture_recording_receipt_transition_progress(*permille);
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
        Action::CaptureScreenshotFinished { copied } => {
            driver.set_capture_screenshot_finished(*copied);
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
        Action::ShowHostDesktopOnboarding => driver.show_host_desktop_onboarding(),
        Action::SetOnboardingVisual(state) => driver.set_onboarding_visual_state(*state),
        Action::SetLockVisual(state) => driver.set_lock_visual_state(*state),
        Action::SeedRoomActivity { explore } => {
            driver.seed_room_activity_for_shots(*explore);
        }
        Action::SeedAutomaticWorkSessions => driver.seed_automatic_work_sessions_for_shots(),
        Action::ExpandAutomaticWorkSession => assert!(
            driver.expand_automatic_work_session_for_shots(),
            "scenario: automatic work session must be available to expand"
        ),
        Action::CorrectAutomaticWorkSession => assert!(
            driver.correct_automatic_work_session_for_shots(),
            "scenario: automatic work-session seam correction must succeed"
        ),
        Action::ShowRoomActivityReplay => driver.show_room_activity_replay_for_shots(),
        Action::FilterRoomActivityToAda => assert!(
            driver.filter_room_activity_to_ada_for_shots(),
            "scenario: attributed history requires a real change authored by Ada"
        ),
        Action::SeedSharedWorkspace => driver.seed_shared_workspace_for_shots(),
        Action::SeedAgentScreenExposure { surface_id } => {
            driver.seed_agent_screen_exposure_for_shots(*surface_id);
        }
        Action::SeedInferredStatus(text) => driver.seed_inferred_status_for_shots(text),
        Action::SeedCrowdedWorkspace => driver.seed_crowded_workspace_for_shots(),
        Action::SeedForeignAppWork {
            app_id,
            action,
            resource,
        } => driver.seed_foreign_app_work_for_shots(app_id, action, resource),
        Action::SeedInvitePending => driver.seed_invite_pending_for_shots(),
        Action::AdmitInvitedCollaborator => assert!(
            driver.admit_invited_collaborator_for_shots(),
            "scenario: collaboration journey cannot show live editing before Ada's pending invite is admitted"
        ),
        Action::SelectRoomActivityEntry(index) => {
            driver.select_room_activity_entry(*index);
        }
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

/// `Action::OpenWindow` — split out of [`apply`] to keep that dispatcher short.
fn apply_open_window(driver: &mut ShellDriver, action: &Action) {
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

/// `Action::AddHostedTab` — split out of [`apply`] to keep that dispatcher short.
fn apply_add_hosted_tab(driver: &mut ShellDriver, action: &Action) {
    if let Action::AddHostedTab { app, title } = action {
        let app = *app;
        let title = *title;
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
                .workspaces
                .iter()
                .flat_map(|w| &w.strips)
                .flat_map(|s| &s.window_groups)
                .flat_map(|g| &g.tabs)
                .map(|t| t.id)
                .max()
                .unwrap_or(0)
                .saturating_add(1);
            let Some(ws) = tree.active_workspace_mut() else {
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
            });
            group.active_tab = id;
            drop(tree);
            st.bump();
        });
    }
}

/// `Action::OpenHostedWindow` and `Action::DragWheelRim` — split out of
/// [`apply`] to keep that dispatcher short.
fn apply_hosted_window_and_wheel_drag(driver: &mut ShellDriver, action: &Action) {
    match action {
        Action::OpenHostedWindow { app, title } => {
            let kind = app_kind_for(app);
            let title = *title;
            driver.with_chrome(|st: &mut ChromeState| {
                // Where the window goes is decided now, against the
                // layout on screen — the same rule a launch applies. A
                // hosted app has no client process to correlate a
                // pending-placement note against, so the answer is used
                // here rather than left for an adoption to pick up.
                let placement = {
                    let tree = st.tree();
                    shell::shell::placement::placement_for_new_window(&tree, st.strip_viewport())
                };
                {
                    let mut tree = st.tree_mut();
                    shell::compositor::tab_binding::open_hosted_app(
                        &mut tree, kind, title, placement,
                    );
                }
                st.bump();
            });
        }
        Action::DragWheelRim { dy, steps } => {
            driver.relayout();
            let Some(rect) = driver
                .host_mut()
                .sidebar_wheel_mut_for_tests()
                .map(|w| ui::Widget::get_rect(w))
            else {
                eprintln!("scenario: no wheel disc in the tree to drag");
                return;
            };
            if rect.area() <= 0.0 {
                eprintln!("scenario: the wheel disc has not laid out yet — paint first?");
                return;
            }
            // Continue an existing gesture rather than starting a new
            // one. Two of these back to back is how a scenario captures
            // a drag at more than one depth, and pressing again half way
            // through does not do that: the second press lands on
            // whichever sector has rotated under the pointer by then,
            // which the disc reads as *clicking* that workspace, and no
            // drag starts at all. The release afterwards then has
            // nothing to settle.
            let in_flight = driver
                .host_mut()
                .sidebar_wheel_mut_for_tests()
                .and_then(|w| w.drag_from.map(|_| w.last_cursor_pos))
                .flatten();
            let from = if let Some(p) = in_flight {
                p
            } else {
                // A third of the way in from the rim, vertically
                // centred: inside the arc at every rotation, and
                // clear of the add-workspace button at the bottom.
                let p = Point::new(
                    rect.x0 + rect.width() * 0.66,
                    f64::midpoint(rect.y0, rect.y1),
                );
                driver.move_cursor(p);
                driver.press();
                p
            };
            let n = (*steps).max(1);
            for s in 1..=n {
                let t = f64::from(s) / f64::from(n);
                driver.move_cursor(Point::new(from.x, from.y + dy * t));
                // One frame per increment so the follow spring actually
                // integrates — with no frames between moves the rim
                // never gets anywhere and the drag is a teleport.
                driver.step_frames(1);
            }
        }
        _ => {}
    }
}

/// `Action::SampleWheelSettle` and `Action::SampleTabReveal` — split out
/// of [`apply`] to keep that dispatcher short.
fn apply_wheel_and_reveal_samples<S: SnapshotSink>(
    driver: &mut ShellDriver,
    action: &Action,
    sink: &mut S,
) {
    match action {
        Action::SampleWheelSettle { prefix, at_ms } => {
            // Settle the content before sampling anything.
            //
            // Committing a workspace swaps the content underneath, and
            // the strip that comes in has its own per-widget animations
            // (a tab bar's reveal, a group's spawn fade). Sampling
            // straight after the release caught those a frame or two in,
            // and the earliest frames of the strip — which are the whole
            // job of a sampled-curve capture — showed them rather than
            // the wheel.
            //
            // Spending frames here is free: every sample re-derives the
            // wheel's curve from the settle's recorded starting state,
            // so whatever the spring does meanwhile is discarded. Only
            // the content keeps the time, which is the point.
            driver.step_frames(24);
            driver.relayout();
            for ms in *at_ms {
                let held = match driver.host_mut().sidebar_wheel_mut_for_tests() {
                    None => {
                        eprintln!("scenario: no wheel disc in the tree to sample at {ms} ms");
                        false
                    }
                    Some(w) => w.hold_rotation_at(std::time::Duration::from_millis(*ms)),
                };
                if !held {
                    eprintln!("scenario: no wheel settle recorded to sample at {ms} ms");
                }
                driver.relayout();
                let name: &'static str = Box::leak(format!("{prefix}-{ms:03}ms").into_boxed_str());
                sink.capture(driver, name);
            }
        }
        Action::SampleTabReveal { prefix, at_ms } => {
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
                let _ = (prefix, at_ms);
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
                let strips: Vec<_> = {
                    let st = driver.chrome();
                    let tree = st.tree();
                    tree.active_workspace()
                        .map(|ws| ws.strips.iter().map(|s| s.id).collect())
                        .unwrap_or_default()
                };
                for ms in *at_ms {
                    let mut any = false;
                    for sid in &strips {
                        if let Some(row) = driver.host_mut().group_row_mut_for_tests(*sid) {
                            for g in &mut row.groups {
                                any |= g
                                    .bar_mut_for_tests()
                                    .hold_reveal_at(std::time::Duration::from_millis(*ms));
                            }
                        }
                    }
                    if !any {
                        eprintln!("scenario: no tab reveal in flight to sample at {ms} ms");
                    }
                    driver.relayout();
                    let name: &'static str =
                        Box::leak(format!("{prefix}-{ms:03}ms").into_boxed_str());
                    sink.capture(driver, name);
                }
            }
        }
        _ => {}
    }
}

/// `Action::SampleMakeSpace` and `Action::SetWindowWidths` — split out of
/// [`apply`] to keep that dispatcher short.
fn apply_make_space_sample_and_widths<S: SnapshotSink>(
    driver: &mut ShellDriver,
    action: &Action,
    sink: &mut S,
) {
    match action {
        Action::SampleMakeSpace { prefix, at_ms } => {
            // Splice the new window into the widget tree, then lay out —
            // which is the pass that measures how far each pushed group
            // travels and starts the slide. Freeze it immediately, before
            // a render's worth of wall time can retire it.
            driver.step_frames(1);
            driver.relayout();
            let strips: Vec<_> = {
                let st = driver.chrome();
                let tree = st.tree();
                tree.active_workspace()
                    .map(|ws| ws.strips.iter().map(|s| s.id).collect())
                    .unwrap_or_default()
            };
            for ms in *at_ms {
                let mut any = false;
                for sid in &strips {
                    if let Some(row) = driver.host_mut().group_row_mut_for_tests(*sid) {
                        any |= row.hold_shift_at(std::time::Duration::from_millis(*ms));
                    }
                }
                if !any {
                    eprintln!("scenario: no make-space slide in flight to sample at {ms} ms");
                }
                driver.relayout();
                let name: &'static str = Box::leak(format!("{prefix}-{ms:03}ms").into_boxed_str());
                sink.capture(driver, name);
            }
        }
        Action::SetWindowWidths(permille) => {
            let permille = *permille;
            driver.with_chrome(|st: &mut ChromeState| {
                {
                    let mut tree = st.tree_mut();
                    if let Some(ws) = tree.active_workspace_mut() {
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
                tree.active_workspace()
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
        Action::SeedStatus(preset) => {
            let status = status_preset(preset);
            driver.with_chrome(|st: &mut ChromeState| {
                st.status = status.clone();
                st.bump();
            });
        }
        Action::SeedOsd(preset) => {
            let osd = osd_preset(preset);
            driver.with_chrome(|st: &mut ChromeState| {
                st.show_osd(osd);
            });
        }
        Action::SeedFilePicker(mode) => {
            use shell::portal::{
                FilePickerOptions, FilePickerQueue, FilePickerSpawner, PickerFilter, PortalCaller,
                PromptQueueHandle, PromptResolver,
            };
            use shell::shell::state::WorkspaceId;
            let mode = *mode;
            // Only one dialog is on screen at a time — a second request
            // while one is up is answered rather than stacked. So a
            // scenario capturing several shapes has to dismiss the
            // previous one first, the way the user would.
            if let Some(picker) = driver.host_mut().file_picker_mut() {
                picker.click_cancel();
            }
            driver.host_mut().pump_file_picker();
            let queue = FilePickerQueue::new();
            driver.host_mut().set_portal_queues(
                PromptQueueHandle::new(PromptResolver::Channel),
                queue.clone(),
            );

            let save = mode == "save";
            let caller = PortalCaller::new(WorkspaceId(1), "dev.tilo.word-processor");
            let opts = FilePickerOptions::new(
                caller,
                if save {
                    "Save Document"
                } else {
                    "Open Document"
                },
                save,
            )
            .multiple(mode == "open_multiple")
            .filters(vec![
                PickerFilter::from_pairs(
                    "Documents",
                    &[(0, "*.odt".into()), (0, "*.md".into()), (0, "*.txt".into())],
                ),
                PickerFilter::from_pairs("Images", &[(1, "image/png".into())]),
            ])
            .current_name(save.then(|| "Untitled.md".to_string()));
            // The reply receiver is dropped: a screenshot has nobody to
            // hand the answer to. The dialog does not care — it is the
            // portal side that reads a closed channel as "cancelled".
            let (tx, _rx) = tokio::sync::oneshot::channel();
            queue.spawn(shell::portal::FilePickerRequest {
                caller: opts.caller,
                title: opts.title,
                save: opts.save,
                directory: opts.directory,
                multiple: opts.multiple,
                accept_label: opts.accept_label,
                current_name: opts.current_name,
                filters: opts.filters,
                reply: tx,
            });
            driver.host_mut().pump_file_picker();
        }
        _ => {}
    }
}

/// `Action::SeedWifiPrompt` and `Action::SelectGroup` — split out of
/// [`apply`] to keep that dispatcher short.
fn apply_seed_wifi_and_select_group(driver: &mut ShellDriver, action: &Action) {
    match action {
        Action::SeedWifiPrompt(preset) => {
            let preset = *preset;
            driver.with_chrome(|st: &mut ChromeState| {
                use shell::shell::quick_settings::{QsLevel, WifiNetwork, WifiPasswordPrompt};
                st.overlays.quick_settings = true;
                st.quick_settings.nav_stack = vec![QsLevel::Root, QsLevel::Wifi];
                st.quick_settings.wifi_networks = vec![
                    WifiNetwork {
                        ssid: "Cafe WiFi".into(),
                        signal: 3,
                        secure: true,
                        connected: false,
                    },
                    WifiNetwork {
                        ssid: "Airport Free".into(),
                        signal: 2,
                        secure: false,
                        connected: false,
                    },
                ];
                let mut p = WifiPasswordPrompt::new("Cafe WiFi");
                match preset {
                    "typed" => {
                        p.password = "hunter2pass".into();
                        p.selection = ui::text_selection::Selection::collapsed(p.password.len());
                    }
                    "rejected" => {
                        p.password = "wrongpass".into();
                        p.selection.anchor = 0;
                        p.selection.cursor = p.password.len();
                        p.error = Some("Couldn't join. Check the password.".into());
                    }
                    _ => {}
                }
                st.quick_settings.wifi_password = Some(p);
                st.bump();
            });
        }
        Action::SelectGroup(idx) => {
            let idx = *idx;
            driver.with_chrome(|st: &mut ChromeState| {
                {
                    let mut tree = st.tree_mut();
                    let Some(strip_id) = tree
                        .active_workspace()
                        .and_then(|ws| ws.strips.first().map(|s| s.id))
                    else {
                        return;
                    };
                    match idx {
                        Some(i) => {
                            let gid = tree
                                .active_workspace()
                                .and_then(|ws| ws.strips.first())
                                .and_then(|s| s.window_groups.get(i).map(|g| g.id));
                            if let Some(gid) = gid {
                                let active = tree.active;
                                tree.active_strip.insert(active, strip_id);
                                tree.set_active_group(strip_id, gid);
                            }
                        }
                        None => {
                            tree.active_group.remove(&strip_id);
                        }
                    }
                }
                st.bump();
            });
        }
        _ => {}
    }
}

/// `Action::LoneWindow`, `Action::SwitchWorkspace`, `Action::SetWsSettings`
/// and `Action::PushQsLevel` — split out of [`apply`] to keep that
/// dispatcher short.
fn apply_window_and_workspace_actions(driver: &mut ShellDriver, action: &Action) {
    match action {
        Action::LoneWindow(idx) => {
            let idx = *idx;
            driver.with_chrome(|st: &mut ChromeState| {
                {
                    let mut tree = st.tree_mut();
                    if let Some(ws) = tree.active_workspace_mut()
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
            let strip_id = {
                let chrome = driver.chrome();
                let tree = chrome.tree();
                tree.active_workspace()
                    .and_then(|workspace| workspace.strips.first())
                    .map(|strip| strip.id)
            };
            if let Some(strip_id) = strip_id
                && let Some(host) = driver
                    .host_mut()
                    .group_at_for_tests(strip_id, *group_idx)
                    .and_then(|group| group.active_host_mut())
            {
                let body = (1..=52)
                    .map(|line| {
                        format!(
                            "Complete capture {line:02} — the document continues beyond the physical display while the live Notes window stays exactly where it is."
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                host.replace_content(ui::widget::TileContent::App(Box::new(
                    shell::shell::notes_app::NotesApp::with_seed(body),
                )));
                driver.relayout();
            }
        }
        Action::SwitchWorkspace(idx) => {
            driver.with_chrome(|st: &mut ChromeState| {
                let mut tree = st.tree_mut();
                if let Some(ws) = tree.workspaces.get(*idx) {
                    let id = ws.id;
                    tree.set_active(id);
                    drop(tree);
                    st.bump();
                }
            });
        }
        Action::SetWsSettings(tab) => {
            let tab_copy = *tab;
            driver.with_chrome(|st| {
                st.overlays.ws_settings = true;
                st.ws_settings_tab = tab_copy;
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

/// The [`AppKind`] behind an app name written the way a tab pill spells
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

fn push_groups_per_strip(out: &mut String, host: &shell::shell::overlay_host::ShellOverlayHost) {
    // Groups-per-strip snapshot (drag-out tests):
    // `[[[group_id, [tab_id, ...]], ...], ...]`.
    out.push_str("\"groups_per_strip\":[");
    for (strip_index, (_strip_id, groups)) in host.groups_per_strip().iter().enumerate() {
        if strip_index > 0 {
            out.push(',');
        }
        out.push('[');
        for (group_index, (group_id, tabs)) in groups.iter().enumerate() {
            if group_index > 0 {
                out.push(',');
            }
            out.push('[');
            out.push_str(&group_id.to_string());
            out.push_str(",[");
            for (tab_index, tab_id) in tabs.iter().enumerate() {
                if tab_index > 0 {
                    out.push(',');
                }
                out.push_str(&tab_id.to_string());
            }
            out.push_str("]]");
        }
        out.push(']');
    }
    out.push(']');
}

/// Serialise the chrome's externally-observable state into a compact
/// JSON object. Hand-rolled instead of pulling `serde_derive` because
/// `ChromeState` lives in another crate we don't want to annotate.
pub fn chrome_state_json(driver: &ShellDriver) -> String {
    let st = driver.chrome();
    let host = driver.host();
    let tree = st.tree();
    let active = tree.active.raw();
    let active_idx = tree
        .workspaces
        .iter()
        .position(|w| w.id == tree.active)
        .unwrap_or(0);
    let workspace_names: Vec<String> = tree.workspaces.iter().map(|w| w.name.clone()).collect();
    // Nothing past this point reads the tree; `st` (a separate guard) is
    // still needed below.
    drop(tree);
    let qs_level = match st.quick_settings.current_level() {
        QsLevel::Root => "root",
        QsLevel::Wifi => "wifi",
        QsLevel::Bluetooth => "bluetooth",
        QsLevel::WifiDetails(_) => "wifi-details",
        QsLevel::BluetoothDetails(_) => "bluetooth-details",
        QsLevel::Vpn => "vpn",
        QsLevel::Notifications => "notifications",
        QsLevel::Clipboard => "clipboard",
        QsLevel::Devices => "devices",
    };
    let ws_tab = match st.ws_settings_tab {
        WsTab::General => "general",
        WsTab::Members => "members",
        WsTab::Agents => "agents",
        WsTab::Permissions => "permissions",
        WsTab::Apps => "apps",
    };
    let palette_visible = host.palette_visible();
    let palette_query = host.palette_query();
    // Hand-format JSON so we don't have to plumb serde through the bin.
    // Strings escaped via `escape_json_str` for quotes / backslashes /
    // control chars. Arrays produced inline.
    let mut out = String::new();
    out.push('{');
    push_kv_str(&mut out, "scenario_step", "");
    out.push(',');
    push_kv_u64(&mut out, "active_workspace_id", active);
    out.push(',');
    push_kv_usize(&mut out, "active_workspace_idx", active_idx);
    out.push(',');
    push_kv_array(&mut out, "workspace_names", &workspace_names);
    out.push(',');
    push_kv_bool(&mut out, "overlay_overview", st.overlays.overview);
    out.push(',');
    push_kv_bool(
        &mut out,
        "overlay_quick_settings",
        st.overlays.quick_settings,
    );
    out.push(',');
    push_kv_bool(&mut out, "overlay_ws_settings", st.overlays.ws_settings);
    out.push(',');
    push_kv_bool(&mut out, "overlay_collaborators", st.overlays.collaborators);
    out.push(',');
    push_kv_bool(&mut out, "overlay_room_activity", st.overlays.room_activity);
    out.push(',');
    push_kv_str(
        &mut out,
        "room_activity_view",
        match host.room_activity().view() {
            shell::shell::room_activity::ActivityView::CatchUp => "catch_up",
            shell::shell::room_activity::ActivityView::Tasks => "tasks",
            shell::shell::room_activity::ActivityView::Explore => "explore",
            shell::shell::room_activity::ActivityView::Replay => "replay",
        },
    );
    out.push(',');
    push_kv_bool(
        &mut out,
        "room_activity_compared",
        host.room_activity().has_comparison(),
    );
    out.push(',');
    push_kv_str(
        &mut out,
        "room_activity_focus",
        host.room_activity()
            .focused_control_label()
            .as_deref()
            .unwrap_or(""),
    );
    out.push(',');
    push_kv_str(&mut out, "qs_level", qs_level);
    out.push(',');
    push_kv_str(&mut out, "ws_settings_tab", ws_tab);
    out.push(',');
    push_kv_bool(&mut out, "palette_visible", palette_visible);
    out.push(',');
    push_kv_str(&mut out, "palette_query", palette_query);
    out.push(',');
    push_kv_u64(&mut out, "revision", st.revision);
    out.push(',');
    push_groups_per_strip(&mut out, host);
    out.push('}');
    out
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

fn push_kv_str(out: &mut String, k: &str, v: &str) {
    out.push('"');
    out.push_str(k);
    out.push('"');
    out.push(':');
    out.push('"');
    out.push_str(&escape_json_str(v));
    out.push('"');
}
fn push_kv_u64(out: &mut String, k: &str, v: u64) {
    out.push('"');
    out.push_str(k);
    out.push('"');
    out.push(':');
    out.push_str(&v.to_string());
}
fn push_kv_usize(out: &mut String, k: &str, v: usize) {
    out.push('"');
    out.push_str(k);
    out.push('"');
    out.push(':');
    out.push_str(&v.to_string());
}
fn push_kv_bool(out: &mut String, k: &str, v: bool) {
    out.push('"');
    out.push_str(k);
    out.push('"');
    out.push(':');
    out.push_str(if v { "true" } else { "false" });
}
fn push_kv_array(out: &mut String, k: &str, vs: &[String]) {
    out.push('"');
    out.push_str(k);
    out.push('"');
    out.push(':');
    out.push('[');
    for (i, v) in vs.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(&escape_json_str(v));
        out.push('"');
    }
    out.push(']');
}
fn escape_json_str(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // `String`'s `Write` impl never actually errors; there's
            // nothing a caller could do with the `Result` anyway.
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

// =====================================================================
// Built-in catalogue
// =====================================================================

/// Static list of all built-in scenarios. The CLI's `--list-scenarios`
/// dumps these; `--scenario NAME` looks NAME up here.
pub fn catalogue() -> Vec<Scenario> {
    vec![
        host_desktop_first_login(),
        lock_screen_states(),
        capture_studio(),
        capture_multi_selection(),
        capture_selection_editing(),
        capture_countdown(),
        capture_screenshot_receipt(),
        capture_screenshot_copied_receipt(),
        capture_screenshot_markup(),
        capture_screenshot_markup_full_app(),
        capture_screenshot_full_app(),
        capture_recording_audio(),
        capture_recording_full_app(),
        capture_recording_active(),
        capture_recording_dock_midpoint(),
        capture_recording_receipt_actions(),
        capture_replay_active(),
        capture_replay_shortcut(),
        capture_pip_shortcut(),
        capture_pip_region(),
        capture_pip_full_app(),
        capture_pip_multi(),
        palette_typing(),
        palette_close_anim(),
        tab_drag(),
        tab_drag_out_to_new_group(),
        tab_drag_hover_line_indicator(),
        workspace_switch(),
        wheel_scroll_workspaces(),
        wheel_drag_feel(),
        wheel_overscroll_at_the_end(),
        wheel_summon_at_pointer(),
        overview_transition(),
        shortcut_guide(),
        solo_and_shared_shell_states(),
        agent_screen_exposure_disclosure(),
        collaboration_product_proof(),
        crowded_people_roster(),
        foreign_app_collaboration_fallback(),
        collaboration_status_privacy(),
        strip_horizontal_scroll(),
        notes_click_and_type(),
        two_notes_tabs_keep_their_own_text(),
        cycle_all_tabs(),
        word_processor_tile(),
        room_activity_from_palette(),
        room_activity_catch_up_keyboard(),
        automatic_work_session_correction(),
        room_activity_explore_keyboard(),
        room_activity_explore_detail_compare(),
        room_activity_replay(),
        window_open_beside_selection(),
        window_open_into_tabbed_group(),
        window_open_at_visible_seam(),
        window_open_at_next_seam_rightwards(),
        strip_scroll_across_apps(),
        narrow_groups_side_by_side(),
        tab_pill_reveal(),
        tab_pill_reveal_narrow(),
        tab_pill_states(),
        new_tab_from_the_plus_chip(),
        status_tray_states(),
        media_key_osd(),
        wifi_password_prompt(),
        file_chooser_dialog(),
    ]
}

fn lock_screen_states() -> Scenario {
    use super::driver::LockVisualState as Lock;
    Scenario {
        name: "lock_screen_states",
        description: "Render settled production lock-screen states without locking the live session.",
        actions: vec![
            Action::SetLockVisual(Lock::Default),
            Action::Snapshot("01-default"),
            Action::SetLockVisual(Lock::Password),
            Action::Snapshot("02-password-entered"),
            Action::SetLockVisual(Lock::Verifying),
            Action::Snapshot("03-verifying"),
            Action::SetLockVisual(Lock::Error),
            Action::Snapshot("04-auth-error"),
            Action::SetLockVisual(Lock::Notifications),
            Action::Snapshot("05-notification-count"),
        ],
    }
}

/// The exact identity introduction shown on first Tilo login when the package
/// is layered onto an existing Linux installation.
fn host_desktop_first_login() -> Scenario {
    use shell::onboarding_view::OnboardingVisualState as Visual;
    Scenario {
        name: "host_desktop_first_login",
        description: "Render the host-session identity setup exactly as the compositor shows it.",
        actions: vec![
            Action::ShowHostDesktopOnboarding,
            Action::Wait(4),
            Action::Snapshot("01-identity-introduction"),
            Action::SetOnboardingVisual(Visual::Connect),
            Action::Relayout,
            Action::Snapshot("02-restore-existing"),
            Action::SetOnboardingVisual(Visual::RestoreError),
            Action::Relayout,
            Action::Snapshot("03-restore-error"),
            Action::SetOnboardingVisual(Visual::Protect),
            Action::Relayout,
            Action::Snapshot("04-protect-identity"),
            Action::SetOnboardingVisual(Visual::CreateError),
            Action::Relayout,
            Action::Snapshot("05-create-error"),
            Action::SetOnboardingVisual(Visual::Backup),
            Action::Relayout,
            Action::Snapshot("06-encrypted-backup"),
            Action::SetOnboardingVisual(Visual::BackupVerified),
            Action::Relayout,
            Action::Snapshot("07-backup-verified"),
            Action::SetOnboardingVisual(Visual::Ready),
            Action::Relayout,
            Action::Snapshot("08-ready"),
            Action::SetOnboardingVisual(Visual::TourNavigate),
            Action::Relayout,
            Action::Snapshot("09-tour-navigate"),
            Action::SetOnboardingVisual(Visual::TourOrganise),
            Action::Relayout,
            Action::Snapshot("10-tour-organise"),
            Action::SetOnboardingVisual(Visual::TourCollaborate),
            Action::Relayout,
            Action::Snapshot("11-tour-collaborate"),
            Action::SetOnboardingVisual(Visual::TourStart),
            Action::Relayout,
            Action::Snapshot("12-tour-start"),
        ],
    }
}

/// Desktop help is itself a reachable control surface. F1 is modifier-free,
/// keeping the scenario stable when the developer running shots has
/// customized the ordinary help chord.
fn shortcut_guide() -> Scenario {
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
    }
}

/// The collaboration journey. Each frame is a reachable state: an invitation
/// is not shown as presence, a remote edit is attributed in room history, and
/// recovery creates a new forward change. The last frame switches to a second
/// native app to show that collaborator location is workspace data.
fn collaboration_product_proof() -> Scenario {
    Scenario {
        name: "collaboration_product_proof",
        description: "Prove solo work, a pending invite, live editing, attributed history, forward revert, and cross-app presence.",
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
            Action::SeedRoomActivity { explore: true },
            Action::FilterRoomActivityToAda,
            Action::Wait(2),
            Action::Snapshot("04-attributed-history"),
            Action::SelectRoomActivityEntry(0),
            Action::Snapshot("05-history-comparison"),
            Action::RevertRoomActivity,
            Action::Wait(18),
            Action::Snapshot("06-reverted-forward"),
            Action::ShowCollaboratorInFiles,
            Action::Wait(2),
            Action::Snapshot("07-ada-in-files"),
        ],
    }
}

/// The two resting states that define Tilo's chrome contract. The first frame
/// must read as an ordinary personal desktop; the second adds collaboration
/// without changing the underlying navigation model. The final frame proves
/// the persistent presence entry opens the roster it represents.
fn solo_and_shared_shell_states() -> Scenario {
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
    }
}

/// Trusted chrome must say both *who* can see the screen and that the viewer
/// is an agent, then retain that attribution in the expanded People surface.
fn agent_screen_exposure_disclosure() -> Scenario {
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
    }
}

fn crowded_people_roster() -> Scenario {
    Scenario {
        name: "crowded_people_roster",
        description: "Prove that a crowded live roster keeps full activity and intent rows, pages cleanly, and remains keyboard navigable.",
        actions: vec![
            Action::Wait(4),
            Action::Relayout,
            Action::Key(Key::Named(NamedKey::Escape)),
            Action::Wait(18),
            Action::SeedCrowdedWorkspace,
            Action::Wait(3),
            Action::Snapshot("01-first-three-people"),
            Action::Key(Key::Named(NamedKey::Home)),
            Action::Key(Key::Named(NamedKey::ArrowRight)),
            Action::Key(Key::Named(NamedKey::ArrowRight)),
            Action::Key(Key::Named(NamedKey::ArrowRight)),
            Action::Key(Key::Named(NamedKey::ArrowRight)),
            Action::Key(Key::Named(NamedKey::ArrowRight)),
            Action::Key(Key::Named(NamedKey::ArrowRight)),
            Action::Key(Key::Named(NamedKey::ArrowRight)),
            Action::Snapshot("02-next-page-keyboard-focus"),
            Action::Key(Key::Named(NamedKey::Enter)),
            Action::Wait(2),
            Action::Snapshot("03-next-three-people"),
        ],
    }
}

fn foreign_app_collaboration_fallback() -> Scenario {
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
    }
}

fn collaboration_status_privacy() -> Scenario {
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
            Action::Type("Closing the Q3 forecast".into()),
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
    }
}

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
            link: Some(wifi(4)),
            volume: Some(vol(0.45, false)),
            media: Vec::new(),
        },
        "charging" => SystemStatus {
            battery: Some(batt(38, BatteryState::Charging)),
            link: Some(wifi(3)),
            volume: Some(vol(0.45, false)),
            media: Vec::new(),
        },
        "low" => SystemStatus {
            battery: Some(batt(14, BatteryState::Discharging)),
            link: Some(wifi(2)),
            volume: Some(vol(0.20, false)),
            media: Vec::new(),
        },
        "critical" => SystemStatus {
            battery: Some(batt(3, BatteryState::Discharging)),
            link: Some(wifi(1)),
            volume: Some(vol(0.20, false)),
            media: Vec::new(),
        },
        "muted" => SystemStatus {
            battery: Some(batt(88, BatteryState::Full)),
            link: Some(wifi(4)),
            volume: Some(vol(0.6, true)),
            media: Vec::new(),
        },
        "offline" => SystemStatus {
            battery: Some(batt(64, BatteryState::Discharging)),
            link: Some(LinkStatus::offline()),
            volume: Some(vol(0.45, false)),
            media: Vec::new(),
        },
        "wired" => SystemStatus {
            battery: None,
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
        "mic_muted" => Osd::mic_mute(true),
        "next_track" => Osd::transport(Transport::Next),
        _ => Osd::volume(0.5, false),
    }
}

/// The hardware-key readout, across the kinds it reports.
fn media_key_osd() -> Scenario {
    Scenario {
        name: "media_key_osd",
        description: "The transient slab a volume / brightness / mute / transport key raises.",
        actions: vec![
            Action::SeedOsd("volume"),
            Action::Wait(3),
            Action::Snapshot("01-volume"),
            Action::SeedOsd("volume_low"),
            Action::Wait(3),
            Action::Snapshot("02-volume-low"),
            Action::SeedOsd("volume_muted"),
            Action::Wait(3),
            Action::Snapshot("03-muted"),
            Action::SeedOsd("brightness"),
            Action::Wait(3),
            Action::Snapshot("04-brightness"),
            Action::SeedOsd("mic_muted"),
            Action::Wait(3),
            Action::Snapshot("05-mic-muted"),
            Action::SeedOsd("next_track"),
            Action::Wait(3),
            Action::Snapshot("06-next-track"),
        ],
    }
}

/// The portal's file dialog, in the three shapes an app can ask for.
///
/// This is the surface a sandboxed app meets when it wants a file, and
/// the compositor draws it — so it is also the surface that has to look
/// like it belongs to the system rather than to the app.
fn file_chooser_dialog() -> Scenario {
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
    }
}

/// Joining a protected network from the chrome — the first-boot path.
fn wifi_password_prompt() -> Scenario {
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
    }
}

/// Every state the status tray can be in, so the styling of each is
/// something a reviewer can look at rather than infer.
fn status_tray_states() -> Scenario {
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
    }
}

/// The one affordance in the chrome that opens a tab.
///
/// Captured as a flow rather than a still because the "+" is only half
/// the feature: the click has to land the palette, and the palette has
/// to come up saying it will open *here*. A still of the chip proves it
/// is drawn; only the sequence proves it is wired.
fn new_tab_from_the_plus_chip() -> Scenario {
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
            Action::Type("term".to_string()),
            Action::Wait(8),
            Action::Snapshot("03-results-say-where-they-land"),
        ],
    }
}

// =====================================================================
// The strip itself: several hosted apps along it, and the pan across them.
// =====================================================================

/// Frames to let the eased focus travel settle before a snapshot — the
/// snap curve is 200 ms of real wall time, and a frame sleeps 16 ms.
const PAN_SETTLE_FRAMES: u32 = 16;

/// Window width the strip is held at, in per-mille of the display's
/// usable width. Half a screen each puts two windows in view with a
/// third clipped at the edge.
const HALF_WIDTH_PERMILLE: u16 = 500;

/// Four shell-hosted apps side by side along the strip, then the strip
/// panned across them.
///
/// Placement scenarios open *foreign* toplevels (card-only until a client
/// attaches), and other hosted-app scenarios have one window in the strip.
/// This opens Notes, Files, Terminal and Editor —
/// each drawing its own content in-process — as four separate windows
/// (each opened with the previous one selected and alone in its group,
/// so the placement rule's "beside a lone window" branch grows the
/// strip rightwards one real window at a time), then travels focus
/// across them and back. Half-width windows make the pan legible: two
/// on screen, a third clipped at the edge.
fn strip_scroll_across_apps() -> Scenario {
    let mut actions = vec![
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
        Action::Snapshot("04-four-windows"),
        // Back to the first window, which pans the strip home the way
        // focus always does — the starting position for the travel.
        Action::SelectGroup(Some(0)),
        Action::Wait(6),
        Action::Snapshot("05-strip-at-rest"),
    ];
    // Travel the strip by focus, the only sanctioned view motion: a
    // bare wheel deliberately does NOT move the strip (hard boundary,
    // 2026-08-22 — scrolling is for content inside a window). Focusing
    // the far window slides the whole strip across on the eased snap
    // curve, exactly what the Alt+Shift wheel jump produces live.
    actions.push(Action::SelectGroup(Some(3)));
    // Let the eased travel settle on its target before reading it.
    actions.push(Action::Wait(PAN_SETTLE_FRAMES));
    actions.push(Action::Snapshot("06-panned-right"));
    actions.push(Action::SelectGroup(Some(0)));
    actions.push(Action::Wait(PAN_SETTLE_FRAMES));
    actions.push(Action::Snapshot("07-panned-back"));
    Scenario {
        name: "strip_scroll_across_apps",
        description: "Open Notes, Files, Terminal and Editor as four windows along the strip, \
                      then travel focus across them and back.",
        actions,
    }
}

/// Window width, per-mille of the display's usable width, that puts three
/// windows on screen at once. A third of the strip each is the shape a
/// demo takes when it wants several apps visible together, and it is the
/// shape a group's tab strip has the least room in.
const THIRD_WIDTH_PERMILLE: u16 = 333;

/// Points along the pill's 180 ms reveal curve worth looking at. Dense
/// at the start, where the ease-out does most of its travelling.
const REVEAL_SAMPLES: &[u64] = &[0, 20, 40, 60, 90, 130, 180];

/// Hovering a tab pill: it opens to make room for its control cluster
/// rather than revealing the chips inside width it was already holding.
///
/// The interesting part is what the *neighbours* do. Growth is anchored
/// at the hovered pill's leading edge, so nothing at or left of it
/// moves and the pills to its right translate without being re-sized —
/// which is the property that keeps the pointer's target under the
/// pointer. Sampling the curve rather than only its endpoints is the
/// only way to see that in a capture.
/// The three tab states side by side in one row — the comparison a
/// reader actually makes.
///
/// A tab row's whole job is to say which tab you are looking at, so the
/// states have to separate at a glance rather than under a colour picker.
/// Captured together in one frame because that is how they are seen: an
/// active pill that reads well against a black backdrop and badly against
/// its own idle neighbours has still failed. The boot group hands us four
/// tabs, so activating the third and parking the pointer over the fourth
/// puts idle, active and hover in the same row with no synthetic state.
fn tab_pill_states() -> Scenario {
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
    }
}

fn tab_pill_reveal() -> Scenario {
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
            // the middle of the window card — the wheel disc down the
            // left edge reacts to a pointer near it, which would move
            // the whole strip and confuse what this is measuring.
            Action::MoveCursor(Point::new(800.0, 500.0)),
            Action::SampleTabReveal {
                prefix: "04-closing",
                at_ms: REVEAL_SAMPLES,
            },
            Action::Wait(12),
            Action::Snapshot("05-idle-again"),
        ],
    }
}

/// The same reveal in the configuration the demo actually renders:
/// three groups at a third of the strip each, the first crowded past
/// what its row can show at resting width. This is where tab chrome has
/// broken before — a pill that opens here has to find its room without
/// pushing anything over the group standing beside it, and the pills it
/// squeezes past still have to name their tabs.
fn tab_pill_reveal_narrow() -> Scenario {
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
    }
}

/// Several narrow windows side by side, one of them carrying more tabs
/// than it has room for.
///
/// The default group takes about three quarters of its strip, so two
/// never fit on screen and a group's tab strip is never actually short of
/// width. Narrow the groups until three fit — the configuration anything
/// showing off the strip uses — and the tab row is suddenly the tightest
/// thing on screen: titles have to elide, and every pill has to stay
/// inside its own group rather than painting over the one beside it.
fn narrow_groups_side_by_side() -> Scenario {
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
    }
}

// =====================================================================
// Where a new window lands, and the motion that makes room for it.
// One scenario per branch of `shell::shell::placement`, each driving the
// adopt path so the recording shows the placement rule.
// =====================================================================

/// Frames to let the motion finish before the settled capture.
const PLACEMENT_SETTLE_FRAMES: u32 = 8;

/// Points along the 420 ms make-space curve to render. Evenly spaced,
/// ending past the curve so the last one is the settled layout.
const MAKE_SPACE_SAMPLES: &[u64] = &[0, 60, 120, 180, 240, 300, 360, 420];

/// The palette route to the room-activity sheet, end to end: open the
/// palette, type the words a person would type, pick the result, and let the
/// sheet settle. In headless mode the state dump proves `overlay_room_activity`
/// flipped; in offscreen mode it is the picture of the pane over the room's
/// real history.
fn room_activity_from_palette() -> Scenario {
    Scenario {
        name: "room_activity_from_palette",
        description: "Search 'workspace activity' in the palette and open the sheet from the result.",
        actions: vec![
            Action::Snapshot("01-desktop"),
            Action::OpenPalette,
            Action::Wait(2),
            Action::Type("workspace activity".into()),
            Action::Wait(2),
            Action::Snapshot("02-palette-result"),
            Action::Key(Key::Named(NamedKey::Enter)),
            Action::Wait(20),
            Action::Snapshot("03-room-activity-open"),
        ],
    }
}

/// Work-session cards at rest, then the exact state reached without a
/// pointer: traverse the modal and expand a named session, revealing the
/// shared boundary that can move one underlying action in either direction.
fn room_activity_catch_up_keyboard() -> Scenario {
    Scenario {
        name: "room_activity_catch_up_keyboard",
        description: "Open Catch up, traverse its controls by keyboard, and reveal the editable boundary between adjacent work sessions.",
        actions: vec![
            Action::SeedRoomActivity { explore: false },
            Action::Relayout,
            Action::Snapshot("01-catch-up"),
            Action::Key(Key::Named(NamedKey::Home)),
            Action::Key(Key::Named(NamedKey::ArrowRight)),
            Action::Key(Key::Named(NamedKey::ArrowRight)),
            Action::Key(Key::Named(NamedKey::Enter)),
            Action::Key(Key::Named(NamedKey::End)),
            Action::Key(Key::Named(NamedKey::Enter)),
            Action::Snapshot("02-work-session-boundary-revealed"),
            Action::Key(Key::Named(NamedKey::End)),
            Action::Snapshot("03-work-session-boundary-focus-visible"),
            Action::Key(Key::Named(NamedKey::Enter)),
            Action::Snapshot("04-boundary-moved-one-action"),
        ],
    }
}

fn automatic_work_session_correction() -> Scenario {
    Scenario {
        name: "automatic_work_session_correction",
        description: "Expand two automatically grouped work sessions, then move their seam and persist the human correction.",
        actions: vec![
            Action::SeedAutomaticWorkSessions,
            Action::Relayout,
            Action::Snapshot("01-automatic-sessions"),
            Action::ExpandAutomaticWorkSession,
            Action::Relayout,
            Action::Snapshot("02-automatic-seam"),
            Action::CorrectAutomaticWorkSession,
            Action::Relayout,
            Action::Snapshot("03-human-correction-shared"),
        ],
    }
}

/// Wide Explorer plus a keyboard-selected lens. This gives both the PNG
/// reviewer and headless state dumps a stable focused-control assertion.
fn room_activity_explore_keyboard() -> Scenario {
    Scenario {
        name: "room_activity_explore_keyboard",
        description: "Open responsive Explorer and traverse lenses; capture at wide and narrow sweep viewports.",
        actions: vec![
            Action::SeedRoomActivity { explore: true },
            Action::Relayout,
            Action::Snapshot("01-explorer-three-panel"),
            Action::Key(Key::Named(NamedKey::Home)),
            Action::Key(Key::Named(NamedKey::ArrowRight)),
            Action::Key(Key::Named(NamedKey::ArrowRight)),
            Action::Key(Key::Named(NamedKey::ArrowRight)),
            Action::Key(Key::Named(NamedKey::ArrowRight)),
            Action::Key(Key::Named(NamedKey::ArrowRight)),
            Action::Key(Key::Named(NamedKey::Enter)),
            Action::Snapshot("02-explorer-browse-focus-visible"),
        ],
    }
}

fn room_activity_explore_detail_compare() -> Scenario {
    Scenario {
        name: "room_activity_explore_detail_compare",
        description: "Select a real history action, show provenance, and materialize its exact comparison.",
        actions: vec![
            Action::SeedRoomActivity { explore: true },
            Action::SelectRoomActivityEntry(0),
            Action::Relayout,
            Action::Key(Key::Named(NamedKey::Home)),
            Action::Key(Key::Named(NamedKey::ArrowRight)),
            Action::Key(Key::Named(NamedKey::ArrowRight)),
            Action::Key(Key::Named(NamedKey::ArrowRight)),
            Action::Key(Key::Named(NamedKey::ArrowRight)),
            Action::Key(Key::Named(NamedKey::ArrowRight)),
            Action::Key(Key::Named(NamedKey::ArrowRight)),
            Action::Key(Key::Named(NamedKey::Enter)),
            Action::Snapshot("01-selected-detail"),
            Action::Key(Key::Named(NamedKey::End)),
            Action::Key(Key::Named(NamedKey::ArrowLeft)),
            Action::Key(Key::Named(NamedKey::Enter)),
            Action::Wait(2),
            Action::Snapshot("02-exact-comparison"),
        ],
    }
}

fn room_activity_replay() -> Scenario {
    Scenario {
        name: "room_activity_replay",
        description: "Replay exact causal workspace layouts with checkpoint controls and presence trails.",
        actions: vec![
            Action::SeedRoomActivity { explore: true },
            Action::ShowRoomActivityReplay,
            Action::Relayout,
            Action::Snapshot("01-replay-checkpoint"),
            Action::Key(Key::Named(NamedKey::End)),
            Action::Key(Key::Named(NamedKey::Enter)),
            Action::Wait(3),
            Action::Snapshot("02-replay-playing"),
        ],
    }
}

/// Repro for "apps render but can't be interacted with". Click in the
/// middle of the Notes tile (the active tab on the default workspace)
/// and type "hi". The pre-click snapshot shows the blank note's
/// zero-state. The post-type snapshot is the proof: if interaction
/// works the zero-state has given way to a caret AND the typed text
/// appears in the body.
fn notes_click_and_type() -> Scenario {
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
            Action::Type("hi".into()),
            Action::Wait(2),
            Action::Snapshot("03-post-type"),
        ],
    }
}

/// Two Notes tabs, each with its own text.
///
/// The picture of the bug this branch closes: every tab of an app used
/// to share one document, so typing into the second tab showed up in
/// the first. Type into the tab the desktop boots with, open a second
/// one, type something else into it, then switch back — the first tab
/// still has to be showing what was typed into *it*.
///
/// The last snapshot is the one that matters. Anything that reverted
/// per-tab documents would show the second tab's text there.
fn two_notes_tabs_keep_their_own_text() -> Scenario {
    Scenario {
        name: "two_notes_tabs_keep_their_own_text",
        description: "Type in one Notes tab, open a second, type in it, switch back.",
        actions: vec![
            // Same body coordinate `notes_click_and_type` uses.
            Action::ClickAt(Point::new(660.0, 500.0)),
            Action::Wait(2),
            Action::Type("first tab writes alpha".into()),
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
            Action::Type("second tab writes beta".into()),
            Action::Wait(2),
            Action::Snapshot("03-second-tab-typed"),
            // Back to the first Notes pill, left-most in the bar.
            Action::ClickAt(Point::new(310.0, 103.0)),
            Action::Wait(4),
            Action::Snapshot("04-first-tab-still-alpha"),
        ],
    }
}

/// Companion to `notes_click_and_type` — click each non-active tab
/// (Files, Terminal, Browser) and snapshot, so we can SEE what
/// renders for each app today. Tabs switch active by clicking on
/// the pill in the tab bar.
fn cycle_all_tabs() -> Scenario {
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
    }
}

/// Activate the group's "Document" tab so the word processor is the tile on
/// screen.
///
/// The default four-tab group opens on Notes, and every other scenario leaves
/// it there, so the whole catalogue judged the window frame against apps that
/// paint dark. The word processor paints a light page — the case where an edge
/// derived from the wrong assumption stops being subtle and starts being a
/// white line drawn around the window. A frame is only as good as its worst
/// ground, so the catalogue needs one scene standing on the other one.
fn word_processor_tile() -> Scenario {
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
    }
}

pub fn lookup(name: &str) -> Option<Scenario> {
    catalogue().into_iter().find(|s| s.name == name)
}

/// Open the palette, type "h", capture; press Esc, capture closed.
/// Open the palette, then close it and capture the fade+scale EXIT
/// frame-by-frame. Before the Transition refactor the palette vanished
/// in a single frame on Escape (the draw block was gated on a bool);
/// the sequence frames here should now show it shrinking + fading out.
fn palette_close_anim() -> Scenario {
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
    }
}

fn capture_studio() -> Scenario {
    Scenario {
        name: "capture_studio",
        description: "Open the shared screenshot, recording and PiP selector.",
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
            Action::Snapshot("capture-studio"),
            Action::MoveCursor(Point::new(650.0, 50.0)),
            Action::Snapshot("capture-mode-hover"),
            Action::MoveCursor(Point::new(837.0, 50.0)),
            Action::Snapshot("capture-source-hover"),
        ],
    }
}

fn capture_multi_selection() -> Scenario {
    Scenario {
        name: "capture_multi_selection",
        description: "Multi-region capture replaces by default, extends with Shift, and keeps selection chrome out of the chosen pixels.",
        actions: vec![
            Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
            Action::Key(Key::Character("s".into())),
            Action::Modifiers(ModifiersState::empty()),
            Action::Wait(12),
            Action::ClickNamed("capture.multi"),
            Action::Drag {
                from: Point::new(180.0, 170.0),
                to: Point::new(500.0, 350.0),
                steps: 7,
            },
            Action::Snapshot("multi-single"),
            Action::Drag {
                from: Point::new(530.0, 210.0),
                to: Point::new(740.0, 390.0),
                steps: 7,
            },
            Action::Snapshot("multi-replaced"),
            Action::Modifiers(ModifiersState::SHIFT),
            Action::Drag {
                from: Point::new(70.0, 400.0),
                to: Point::new(300.0, 590.0),
                steps: 7,
            },
            Action::Modifiers(ModifiersState::empty()),
            Action::Snapshot("multi-shift-added"),
            Action::MoveCursorNamed("capture.selection.0.remove"),
            Action::Snapshot("multi-remove-hover"),
            Action::ClickNamed("capture.selection.0.remove"),
            Action::Snapshot("multi-removed"),
        ],
    }
}

fn capture_selection_editing() -> Scenario {
    Scenario {
        name: "capture_selection_editing",
        description: "A screenshot region is moved, resized from its live handle, and nudged precisely without being redrawn.",
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
    }
}

fn capture_countdown() -> Scenario {
    Scenario {
        name: "capture_countdown",
        description: "Undimmed, input-transparent delayed-capture countdown and its compact timer control.",
        actions: vec![
            Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
            Action::Key(Key::Character("s".into())),
            Action::Modifiers(ModifiersState::empty()),
            Action::Wait(12),
            Action::ClickNamed("capture.screen"),
            Action::ClickNamed("capture.delay"),
            Action::Snapshot("countdown-armed"),
            Action::ClickNamed("capture.save"),
            Action::Snapshot("countdown-live"),
        ],
    }
}

fn capture_screenshot_receipt() -> Scenario {
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
            Action::CaptureScreenshotFinished { copied: false },
            Action::CaptureScreenshotTransition(0),
            Action::Snapshot("receipt-enter-000"),
            Action::CaptureScreenshotTransition(250),
            Action::Snapshot("receipt-enter-250"),
            Action::CaptureScreenshotTransition(500),
            Action::Snapshot("receipt-enter-500"),
            Action::CaptureScreenshotTransition(750),
            Action::Snapshot("receipt-enter-750"),
            Action::CaptureScreenshotTransition(1_000),
            Action::Snapshot("receipt-enter-1000"),
            Action::MoveCursorNamed("capture.open-screenshot"),
            Action::Snapshot("receipt-open-hover"),
            Action::MoveCursorNamed("capture.pin-screenshot"),
            Action::Snapshot("receipt-pin-hover"),
            Action::MoveCursorNamed("capture.copy-screenshot"),
            Action::Snapshot("receipt-copy-hover"),
            Action::MoveCursorNamed("capture.show-screenshot"),
            Action::Snapshot("receipt-reveal-hover"),
            Action::ClickNamed("capture.dismiss-screenshot"),
            Action::Snapshot("receipt-dismissed"),
        ],
    }
}

fn capture_screenshot_copied_receipt() -> Scenario {
    Scenario {
        name: "capture_screenshot_copied_receipt",
        description: "A copied screenshot settles into one uncrowded top-bar status line.",
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
            Action::CaptureScreenshotFinished { copied: true },
            Action::CaptureScreenshotTransition(1_000),
            Action::Snapshot("receipt-copied"),
        ],
    }
}

fn capture_recording_audio() -> Scenario {
    Scenario {
        name: "capture_recording_audio",
        description: "Recording setup with independently editable desktop and microphone tracks.",
        actions: vec![
            Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
            Action::Key(Key::Character("s".into())),
            Action::Modifiers(ModifiersState::empty()),
            Action::CaptureAudioAvailable(true),
            Action::Wait(12),
            Action::ClickNamed("capture.record"),
            Action::ClickNamed("capture.screen"),
            Action::ClickNamed("capture.system-audio"),
            Action::ClickNamed("capture.microphone"),
            Action::ClickNamed("capture.delay"),
            Action::Snapshot("record-audio-ready"),
        ],
    }
}

fn capture_screenshot_full_app() -> Scenario {
    Scenario {
        name: "capture_screenshot_full_app",
        description: "A complete off-screen app flows through the real compositor into a saved image.",
        actions: vec![
            Action::LoneWindow(0),
            Action::SelectGroup(None),
            Action::OpenWindow {
                app_id: "org.example.current",
                surface_id: 5411,
            },
            Action::Wait(20),
            Action::SelectGroup(None),
            Action::OpenWindow {
                app_id: "org.example.next",
                surface_id: 5412,
            },
            Action::Wait(20),
            Action::SetWindowWidths(1_000),
            Action::SelectGroup(Some(2)),
            Action::Wait(24),
            Action::Relayout,
            Action::SeedLongNotes(0),
            Action::Wait(2),
            Action::Relayout,
            Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
            Action::Key(Key::Character("s".into())),
            Action::Modifiers(ModifiersState::empty()),
            Action::Wait(12),
            Action::ClickNamed("capture.window"),
            Action::ClickNamed("capture.picker:Notes"),
            Action::ClickNamed("capture.extent.full-app"),
            Action::Snapshot("shot-full-app-ready"),
            Action::CaptureExecutionEnabled(true),
            Action::ClickNamed("capture.save"),
            Action::Wait(1),
            Action::CaptureScreenshotTransition(1_000),
            Action::Snapshot("shot-full-app-saved"),
            Action::MoveCursorNamed("capture.pin-screenshot"),
            Action::Snapshot("shot-full-app-pin-hover"),
            Action::ClickNamed("capture.pin-screenshot"),
            Action::CapturePipTransition(0),
            Action::Snapshot("shot-full-app-pin-handoff-000"),
            Action::CapturePipTransition(100),
            Action::Snapshot("shot-full-app-pin-handoff-100"),
            Action::CapturePipTransition(200),
            Action::Snapshot("shot-full-app-pin-handoff-200"),
            Action::CapturePipTransition(300),
            Action::Snapshot("shot-full-app-pin-handoff-300"),
            Action::CapturePipTransition(400),
            Action::Snapshot("shot-full-app-pin-handoff-400"),
            Action::CapturePipTransition(500),
            Action::Snapshot("shot-full-app-pin-handoff-500"),
            Action::CapturePipTransition(600),
            Action::Snapshot("shot-full-app-pin-handoff-600"),
            Action::CapturePipTransition(700),
            Action::Snapshot("shot-full-app-pin-handoff-700"),
            Action::CapturePipTransition(800),
            Action::Snapshot("shot-full-app-pin-handoff-800"),
            Action::CapturePipTransition(900),
            Action::Snapshot("shot-full-app-pin-handoff-900"),
            Action::CapturePipTransition(1_000),
            Action::Snapshot("shot-full-app-pin-handoff-1000"),
        ],
    }
}

fn capture_screenshot_markup() -> Scenario {
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
            Action::ClickNamed("capture.save"),
            Action::Wait(1),
            Action::CaptureScreenshotTransition(1_000),
            Action::ClickNamed("capture.open-screenshot"),
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
    }
}

fn capture_screenshot_markup_full_app() -> Scenario {
    Scenario {
        name: "capture_screenshot_markup_full_app",
        description: "A taller-than-screen app opens in markup, then zooms and pans without altering its pixels.",
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
            Action::SelectGroup(Some(2)),
            Action::Wait(24),
            Action::Relayout,
            Action::SeedLongNotes(0),
            Action::Wait(2),
            Action::Relayout,
            Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
            Action::Key(Key::Character("s".into())),
            Action::Modifiers(ModifiersState::empty()),
            Action::Wait(12),
            Action::ClickNamed("capture.window"),
            Action::ClickNamed("capture.picker:Notes"),
            Action::ClickNamed("capture.extent.full-app"),
            Action::CaptureExecutionEnabled(true),
            Action::ClickNamed("capture.save"),
            Action::Wait(1),
            Action::CaptureScreenshotTransition(1_000),
            Action::ClickNamed("capture.open-screenshot"),
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
    }
}

fn capture_recording_full_app() -> Scenario {
    Scenario {
        name: "capture_recording_full_app",
        description: "Recording controls targeting a complete app beyond the visible monitor.",
        actions: vec![
            // Put Notes outside the current physical viewport while keeping
            // it alive on the continuous strip. The picker must still offer
            // and capture it without first dragging it back on screen.
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
            // With three wide groups and the last selected, Notes remains
            // live on the strip but is wholly beyond the physical output.
            Action::SetWindowWidths(1000),
            Action::SelectGroup(Some(2)),
            Action::Wait(24),
            Action::Relayout,
            Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
            Action::Key(Key::Character("s".into())),
            Action::Modifiers(ModifiersState::empty()),
            Action::Wait(12),
            Action::ClickNamed("capture.record"),
            Action::ClickNamed("capture.window"),
            Action::ClickNamed("capture.picker:Notes"),
            Action::ClickNamed("capture.extent.full-app"),
            Action::Snapshot("record-full-app"),
            Action::ClickNamed("capture.save"),
            Action::Snapshot("record-full-app-preparing"),
            Action::CaptureRecordingActive(true),
            Action::Snapshot("record-full-app-start"),
            Action::CaptureRecordingTransition(250),
            Action::Snapshot("record-full-app-flight-25"),
            Action::CaptureRecordingTransition(500),
            Action::Snapshot("record-full-app-flight-50"),
            Action::CaptureRecordingTransition(750),
            Action::Snapshot("record-full-app-flight-75"),
            Action::CaptureRecordingTransition(1_000),
            Action::Snapshot("record-full-app-pill"),
        ],
    }
}

fn capture_recording_active() -> Scenario {
    Scenario {
        name: "capture_recording_active",
        description: "Recorder start, pause/resume, and the compositor-confirmed saved receipt.",
        actions: vec![
            // Establish a real source before simulating compositor success so
            // the first frames also exercise the undimmed scope confirmation.
            Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
            Action::Key(Key::Character("s".into())),
            Action::Modifiers(ModifiersState::empty()),
            Action::Wait(12),
            Action::ClickNamed("capture.record"),
            Action::ClickNamed("capture.screen"),
            Action::ClickNamed("capture.save"),
            Action::Snapshot("recording-preparing"),
            Action::CaptureRecordingActive(true),
            Action::Snapshot("recording-start"),
            Action::CaptureRecordingTransition(250),
            Action::Snapshot("recording-flight-25"),
            Action::CaptureRecordingTransition(500),
            Action::Snapshot("recording-flight-50"),
            Action::CaptureRecordingTransition(750),
            Action::Snapshot("recording-flight-75"),
            Action::CaptureRecordingTransition(1_000),
            Action::Snapshot("recording-pill"),
            Action::MoveCursorNamed("capture.recording-status"),
            Action::Snapshot("recording-status-detail"),
            Action::MoveCursor(Point::new(20.0, 300.0)),
            Action::CaptureRecordingPaused(true),
            Action::CaptureRecordingStateTransition(0),
            Action::Snapshot("recording-pause-000"),
            Action::CaptureRecordingStateTransition(250),
            Action::Snapshot("recording-pause-250"),
            Action::CaptureRecordingStateTransition(500),
            Action::Snapshot("recording-pause-500"),
            Action::CaptureRecordingStateTransition(750),
            Action::Snapshot("recording-pause-750"),
            Action::CaptureRecordingStateTransition(1_000),
            Action::Snapshot("recording-paused"),
            Action::CaptureRecordingPaused(false),
            Action::CaptureRecordingStateTransition(0),
            Action::Snapshot("recording-resume-000"),
            Action::CaptureRecordingStateTransition(250),
            Action::Snapshot("recording-resume-250"),
            Action::CaptureRecordingStateTransition(500),
            Action::Snapshot("recording-resume-500"),
            Action::CaptureRecordingStateTransition(750),
            Action::Snapshot("recording-resume-750"),
            Action::CaptureRecordingStateTransition(1_000),
            Action::Snapshot("recording-resumed"),
            Action::CaptureRecordingFinished,
            Action::CaptureRecordingReceiptTransition(0),
            Action::Snapshot("recording-saved-000"),
            Action::CaptureRecordingReceiptTransition(250),
            Action::Snapshot("recording-saved-250"),
            Action::CaptureRecordingReceiptTransition(500),
            Action::Snapshot("recording-saved-500"),
            Action::CaptureRecordingReceiptTransition(750),
            Action::Snapshot("recording-saved-750"),
            Action::CaptureRecordingReceiptTransition(1_000),
            Action::Snapshot("recording-saved-1000"),
            Action::MoveCursorNamed("capture.play-recording"),
            Action::Snapshot("recording-saved-play-hover"),
            Action::MoveCursorNamed("capture.show-recording"),
            Action::Snapshot("recording-saved-folder-hover"),
            Action::MoveCursorNamed("capture.dismiss-recording"),
            Action::Snapshot("recording-saved-dismiss-hover"),
            // Release the pin by starting a fresh live receipt, then prove the
            // normal three-second lifecycle still hands the clock back.
            Action::CaptureRecordingActive(true),
            Action::CaptureRecordingFinished,
            Action::Wait(200),
            Action::Snapshot("recording-receipt-dismissed"),
        ],
    }
}

fn capture_recording_dock_midpoint() -> Scenario {
    Scenario {
        name: "capture_recording_dock_midpoint",
        description: "The recorder's sentence-to-timer rolling handoff at its most overlap-prone frame.",
        actions: vec![
            Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
            Action::Key(Key::Character("s".into())),
            Action::Modifiers(ModifiersState::empty()),
            Action::Wait(12),
            Action::ClickNamed("capture.record"),
            Action::ClickNamed("capture.screen"),
            Action::ClickNamed("capture.save"),
            Action::CaptureRecordingActive(true),
            Action::CaptureRecordingTransition(500),
            Action::Snapshot("recording-dock-midpoint"),
        ],
    }
}

fn capture_recording_receipt_actions() -> Scenario {
    Scenario {
        name: "capture_recording_receipt_actions",
        description: "The saved recording receipt teaches play, reveal, and dismiss without reopening a modal.",
        actions: vec![
            Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
            Action::Key(Key::Character("s".into())),
            Action::Modifiers(ModifiersState::empty()),
            Action::Wait(12),
            Action::ClickNamed("capture.record"),
            Action::ClickNamed("capture.screen"),
            Action::ClickNamed("capture.save"),
            Action::CaptureRecordingActive(true),
            Action::CaptureRecordingFinished,
            Action::CaptureRecordingReceiptTransition(1_000),
            Action::MoveCursorNamed("capture.play-recording"),
            Action::Snapshot("recording-receipt-play"),
            Action::MoveCursorNamed("capture.show-recording"),
            Action::Snapshot("recording-receipt-reveal"),
            Action::MoveCursorNamed("capture.dismiss-recording"),
            Action::Snapshot("recording-receipt-dismiss"),
        ],
    }
}

fn capture_replay_active() -> Scenario {
    Scenario {
        name: "capture_replay_active",
        description: "Explicit recent-capture warmup, ready state, save feedback, and privacy-visible audio sources.",
        actions: vec![
            Action::CaptureReplayActive(true),
            Action::CaptureReplayBuffered(4),
            Action::Snapshot("replay-warming"),
            Action::SnapshotSequence {
                prefix: "replay-settle",
                frames: 5,
                gap_frames: 4,
            },
            Action::CaptureReplayBuffered(30),
            Action::Snapshot("replay-ready"),
            Action::MoveCursorNamed("capture.replay-status"),
            Action::Snapshot("replay-status-detail"),
            Action::MoveCursor(Point::new(24.0, 180.0)),
            Action::CaptureReplaySaved,
            Action::Snapshot("replay-saved"),
            Action::MoveCursorNamed("capture.play-replay"),
            Action::Snapshot("replay-saved-play-hover"),
            Action::MoveCursorNamed("capture.show-replay"),
            Action::Snapshot("replay-saved-folder-hover"),
            Action::CaptureReplayActive(false),
            Action::Snapshot("replay-off"),
        ],
    }
}

fn capture_replay_shortcut() -> Scenario {
    Scenario {
        name: "capture_replay_shortcut",
        description: "The global replay chord opens directly in recording setup and teaches its instant-save reuse.",
        actions: vec![
            Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
            Action::Key(Key::Character("r".into())),
            Action::Modifiers(ModifiersState::empty()),
            Action::Wait(12),
            Action::MoveCursorNamed("capture.start-replay"),
            Action::Snapshot("replay-shortcut-setup"),
        ],
    }
}

fn capture_pip_shortcut() -> Scenario {
    Scenario {
        name: "capture_pip_shortcut",
        description: "The global PiP chord lands directly in a fast live-area picker and teaches the reusable shortcut.",
        actions: vec![
            Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
            Action::Key(Key::Character("p".into())),
            Action::Modifiers(ModifiersState::empty()),
            Action::Wait(12),
            Action::MoveCursor(Point::new(500.0, 300.0)),
            Action::Snapshot("pip-shortcut-ready"),
            Action::MoveCursorNamed("capture.pip"),
            Action::Snapshot("pip-shortcut-setup"),
        ],
    }
}

fn capture_pip_multi() -> Scenario {
    Scenario {
        name: "capture_pip_multi",
        description: "Fast multi-region selection composed into one floating PiP surface.",
        actions: vec![
            Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
            Action::Key(Key::Character("s".into())),
            Action::Modifiers(ModifiersState::empty()),
            Action::Wait(12),
            Action::ClickNamed("capture.pip"),
            Action::ClickNamed("capture.multi"),
            Action::Drag {
                from: Point::new(250.0, 180.0),
                to: Point::new(500.0, 330.0),
                steps: 7,
            },
            Action::Modifiers(ModifiersState::SHIFT),
            Action::Drag {
                from: Point::new(520.0, 300.0),
                to: Point::new(740.0, 470.0),
                steps: 7,
            },
            Action::Modifiers(ModifiersState::empty()),
            Action::ClickNamed("capture.pip.grid"),
            Action::Snapshot("pip-multi-grid"),
            Action::CaptureExecutionEnabled(true),
            Action::ClickNamed("capture.create-pip"),
            // Give the production executor one paint to create the live
            // mirror, then freeze representative phases immediately before
            // each software-rendered PNG.
            Action::Wait(1),
            Action::CapturePipTransition(0),
            Action::Snapshot("pip-handoff-000"),
            Action::CapturePipTransition(200),
            Action::Snapshot("pip-handoff-200"),
            Action::CapturePipTransition(400),
            Action::Snapshot("pip-handoff-400"),
            Action::CapturePipTransition(600),
            Action::Snapshot("pip-handoff-600"),
            Action::CapturePipTransition(700),
            Action::Snapshot("pip-handoff-700-reflow"),
            Action::CapturePipTransition(800),
            Action::Snapshot("pip-handoff-800"),
            Action::CapturePipTransition(1_000),
            Action::Snapshot("pip-handoff-1000"),
            Action::Snapshot("pip-live"),
            Action::MoveCursor(Point::new(20.0, 320.0)),
            Action::Wait(90),
            Action::Snapshot("pip-resting-clean"),
            Action::MoveCursorNamed("floating.chrome"),
            Action::Wait(12),
            Action::Snapshot("pip-controls-hover"),
            Action::MoveCursorNamed("floating.close"),
            Action::Snapshot("pip-close-hover"),
        ],
    }
}

fn capture_pip_region() -> Scenario {
    Scenario {
        name: "capture_pip_region",
        description: "A selected live area peels into a persistent floating PiP without a first-frame jump.",
        actions: vec![
            Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
            Action::Key(Key::Character("s".into())),
            Action::Modifiers(ModifiersState::empty()),
            Action::Wait(12),
            Action::ClickNamed("capture.pip"),
            Action::Drag {
                // Span the live Notes focal content at compact and wide
                // viewports; a tiny fixed-left sample becomes empty at 1600.
                from: Point::new(330.0, 160.0),
                to: Point::new(1_050.0, 620.0),
                steps: 7,
            },
            Action::Snapshot("pip-region-ready"),
            Action::CaptureExecutionEnabled(true),
            Action::ClickNamed("capture.create-pip"),
            Action::Wait(1),
            Action::CapturePipTransition(0),
            Action::Snapshot("pip-region-handoff-000"),
            Action::CapturePipTransition(400),
            Action::Snapshot("pip-region-handoff-400"),
            Action::CapturePipTransition(800),
            Action::Snapshot("pip-region-handoff-800"),
            Action::CapturePipTransition(1_000),
            Action::Snapshot("pip-region-live"),
            Action::MoveCursor(Point::new(20.0, 320.0)),
            Action::Wait(90),
            Action::Snapshot("pip-region-clean"),
            Action::MoveCursorNamed("floating.chrome"),
            Action::Wait(12),
            Action::Snapshot("pip-region-controls-hover"),
        ],
    }
}

fn capture_pip_full_app() -> Scenario {
    Scenario {
        name: "capture_pip_full_app",
        description: "A complete app outside the physical monitor enters PiP from its virtual-desktop edge.",
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
            Action::SelectGroup(Some(2)),
            Action::Wait(24),
            Action::Relayout,
            Action::SeedLongNotes(0),
            Action::Wait(2),
            Action::Relayout,
            Action::Modifiers(ModifiersState::SUPER | ModifiersState::SHIFT),
            Action::Key(Key::Character("s".into())),
            Action::Modifiers(ModifiersState::empty()),
            Action::Wait(12),
            Action::ClickNamed("capture.pip"),
            Action::ClickNamed("capture.window"),
            Action::ClickNamed("capture.picker:Notes"),
            Action::ClickNamed("capture.extent.full-app"),
            Action::Snapshot("pip-full-app-ready"),
            Action::CaptureExecutionEnabled(true),
            Action::ClickNamed("capture.create-pip"),
            Action::Wait(1),
            Action::CapturePipTransition(0),
            Action::Snapshot("pip-full-app-edge-000"),
            Action::CapturePipTransition(400),
            Action::Snapshot("pip-full-app-edge-400"),
            Action::CapturePipTransition(800),
            Action::Snapshot("pip-full-app-edge-800"),
            Action::CapturePipTransition(1_000),
            Action::Snapshot("pip-full-app-live"),
        ],
    }
}

fn palette_typing() -> Scenario {
    Scenario {
        name: "palette_typing",
        description: "Open the command palette, type 'h', then close with Esc.",
        actions: vec![
            Action::Snapshot("01-desktop"),
            Action::OpenPalette,
            Action::Wait(2),
            Action::Snapshot("02-palette-open"),
            Action::Type("h".into()),
            Action::Snapshot("03-palette-typed-h"),
            Action::Key(Key::Named(NamedKey::Escape)),
            Action::Wait(2),
            Action::Snapshot("04-palette-closed"),
        ],
    }
}

/// Drag a tab pill from a source x-coord to a target x-coord. The exact
/// pixel positions match the bootstrap tree's first strip's first group
/// (Notes / Files / Terminal / Browser pills). Coordinates approximate;
/// good enough for headless smoke / state JSON. For pixel-perfect drag
/// the `e2e_shell_flows::probe_tab_centers` helper is the source of truth.
fn tab_drag() -> Scenario {
    Scenario {
        name: "tab_drag",
        description: "Drag the second tab pill rightward across two pills.",
        actions: vec![
            Action::Snapshot("01-tabs-initial"),
            Action::Drag {
                // Approximate centres of pills 1 and 3 inside the first
                // window group's tab bar at viewport (1600, 900) with the
                // sidebar wheel taking ~200 px of left padding.
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
    }
}

/// Drag a tab pill from inside the bootstrap workspace's only group OUT
/// past its right edge so the inter-group gap resolves. Captures the
/// post-drag state so a JSON sink can verify `groups_per_strip` shows
/// exactly two groups (3-tab source + 1-tab new) — not one (the
/// "replace" perception the user reported).
///
/// Coordinates use the bootstrap default's first strip:
///   - viewport 1600×900 (default driver size)
///   - sidebar wheel takes ~200 px of the group row's leading inset, bounded by
///     `sidebar_wheel_disc::SIDEBAR_RESERVE_PX`
///   - the lone Wide group (800 px) sits roughly at x ∈ [224, 1024]
///   - tab pills live in the bar inside that group, around y≈80
///   - releasing at (1100, 80) is right of the source group → `gap_idx=1`
///
/// `from` targets the second tab pill (Files) — a non-active source so
/// the source group's active-tab promotion path doesn't muddy the
/// post-drag state.
fn tab_drag_out_to_new_group() -> Scenario {
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
                // `driver_drag_tab_out_creates_sibling_group` reads
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
    }
}

/// Rule 1: a window opened while a single window has focus takes the
/// column immediately to its right — and the window that was there
/// slides along to let it in.
fn window_open_beside_selection() -> Scenario {
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
    }
}

/// Rule 2: with the selection on one tab of several, the window joins
/// that group as the next tab along. Nothing in the strip moves — which
/// is the point.
fn window_open_into_tabbed_group() -> Scenario {
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
    }
}

/// Rule 3, first half: nothing selected and a border between windows on
/// screen — the window goes into that space.
fn window_open_at_visible_seam() -> Scenario {
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
    }
}

/// Rule 3, second half: nothing selected and no border visible at all —
/// which is what a full-width window gives you, and the reason the
/// rightwards search is the load-bearing half of the rule.
fn window_open_at_next_seam_rightwards() -> Scenario {
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
    }
}

/// Open the overview with the keyboard chord, hold on it, then leave
/// with Escape — capturing every frame of the crossfade in both
/// directions.
///
/// The transition is two-phase (the desktop shrinks out before the grid
/// zooms in, and the reverse on the way back), which a still cannot
/// show at all. `OVERVIEW_TRANSITION_MS` is 400 ms, so 26 frames at
/// ~16 ms covers a full leg plus a little settle either side.
fn overview_transition() -> Scenario {
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
    }
}

/// Drive the wheel disc via `MouseWheel` events over its hit area and
/// capture frame-by-frame so the user can verify the discrete-detent
/// `click_to_spoke` animation is visible (one workspace per scroll burst).
/// Scroll the strip until the docked wheel has panned off the left
/// edge, then call it back to the pointer with Super+S and dismiss it
/// with Escape.
///
/// The capture shows the state this gesture rescues: the wheel gone, not
/// merely somewhere else on screen.
fn wheel_summon_at_pointer() -> Scenario {
    Scenario {
        name: "wheel_summon_at_pointer",
        description: "Pan the strip until the wheel scrolls away, summon it back to the \
             pointer with Super+S, then dismiss it with Escape.",
        actions: vec![
            Action::MoveCursor(Point::new(900.0, 500.0)),
            Action::Snapshot("00-docked"),
            // Pan right, carrying the wheel off the left edge with the
            // rest of the workspace canvas.
            Action::MouseWheel { dx: -12.0, dy: 0.0 },
            Action::MouseWheel { dx: -12.0, dy: 0.0 },
            Action::Snapshot("01-scrolled-away"),
            // Call it to the pointer, out on the right where the work is.
            Action::MoveCursor(Point::new(1180.0, 620.0)),
            Action::Modifiers(ModifiersState::SUPER),
            Action::Key(Key::Character("s".into())),
            Action::Modifiers(ModifiersState::empty()),
            Action::Snapshot("02-summoned"),
            Action::Key(Key::Named(NamedKey::Escape)),
            Action::Snapshot("03-dismissed"),
        ],
    }
}

/// The wheel's feel, laid out frame by frame: the indicator stepping
/// across the list under a held drag, then the release settling.
///
/// Both halves need explaining. A drag is captured *mid-gesture*,
/// button still down, because the whole complaint was that the
/// highlight did not move until the end — the interesting states are
/// exactly the ones a released drag has already left. And the settle is
/// sampled rather than stepped, because it lasts about half a second
/// while a single offscreen render costs more than that, so real frames
/// only ever catch the two ends of it.
fn wheel_drag_feel() -> Scenario {
    // One workspace is `15° / 0.15 = 100 px` of drag. Up, because the
    // shell opens on its first workspace and the list runs downward from
    // there.
    const PER_WORKSPACE_PX: f64 = 100.0;
    const SETTLE_SAMPLES: &[u64] = &[0, 40, 80, 120, 180, 260, 360, 500];
    Scenario {
        name: "wheel_drag_feel",
        description: "Drag the workspace wheel's rim across the list with the button held,              then sample the release settle along its curve.",
        actions: vec![
            Action::Snapshot("00-rest"),
            // A third of a workspace: not yet past the boundary, so the
            // indicator is still on the workspace we started from while
            // the disc is visibly turned off its detent. Deliberately
            // not one HALF — that is exactly the boundary, and a
            // capture there tips one way or the other and shows neither
            // state clearly.
            Action::DragWheelRim {
                dy: -PER_WORKSPACE_PX * 0.35,
                steps: 10,
            },
            Action::Snapshot("01-part-way"),
            Action::MouseRelease,
            Action::Wait(45),
            // One, two, three workspaces, each captured while still held.
            Action::DragWheelRim {
                dy: -PER_WORKSPACE_PX,
                steps: 14,
            },
            Action::Snapshot("02-one-workspace-held"),
            Action::DragWheelRim {
                dy: -PER_WORKSPACE_PX,
                steps: 14,
            },
            Action::Snapshot("03-two-workspaces-held"),
            Action::MouseRelease,
            Action::Wait(60),
            Action::Snapshot("04-settled-at-the-end"),
            // Now a flick: the same distance covered in a third of the
            // moves, so the pointer is still travelling fast when the
            // button comes up. Sampling THIS is the point — a release
            // from rest has almost nothing to show, whereas a flick has
            // to coast the length of the list and decelerate, which is
            // the entire claim about weight.
            Action::DragWheelRim {
                dy: PER_WORKSPACE_PX * 1.6,
                steps: 5,
            },
            Action::MouseRelease,
            Action::SampleWheelSettle {
                prefix: "05-coast",
                at_ms: SETTLE_SAMPLES,
            },
            Action::Wait(60),
            Action::Snapshot("06-landed"),
        ],
    }
}

/// Pull the wheel off the end of the list and let it go.
///
/// The end is the one place a still is worth much: overscroll is a
/// *position* the wheel can be held in, so a capture at full pull shows
/// the give directly, and the sampled return shows it coming back.
fn wheel_overscroll_at_the_end() -> Scenario {
    const SETTLE_SAMPLES: &[u64] = &[0, 60, 140, 240, 400];
    Scenario {
        name: "wheel_overscroll_at_the_end",
        description: "Pull the workspace wheel past the first workspace, where the list ends,              and sample its return.",
        actions: vec![
            Action::Snapshot("00-rest"),
            // Downward from the first workspace is off the top of the
            // list. 90 px asks for 13.5°; resistance gives back far less.
            Action::DragWheelRim {
                dy: 90.0,
                steps: 12,
            },
            Action::Snapshot("01-pulled"),
            // More than three times the pull, for nowhere near three
            // times the give. Kept inside the disc's own column: the
            // pointer has to still be over the wheel when the button
            // comes up, or the release is routed to whatever is under
            // it instead and the gesture never ends.
            Action::DragWheelRim {
                dy: 200.0,
                steps: 20,
            },
            Action::Snapshot("02-pulled-hard"),
            Action::MouseRelease,
            Action::SampleWheelSettle {
                prefix: "03-returning",
                at_ms: SETTLE_SAMPLES,
            },
            Action::Wait(60),
            Action::Snapshot("04-back-at-the-end"),
        ],
    }
}

fn wheel_scroll_workspaces() -> Scenario {
    Scenario {
        name: "wheel_scroll_workspaces",
        description: "Scroll the workspace wheel via MouseWheel; capture every animation frame.",
        actions: vec![
            // Move cursor over the wheel disc (left edge of viewport).
            Action::MoveCursor(Point::new(80.0, 540.0)),
            Action::Snapshot("00-pre"),
            // Negative dy advances forward (LineDelta sign convention
            // matches a wheel-down gesture). 8 lines × 16 px/line =
            // 128 px, which at a 48-px detent is two workspaces plus a
            // remainder the idle timer clears.
            Action::MouseWheel { dx: 0.0, dy: -8.0 },
            Action::SnapshotSequence {
                prefix: "detent-1",
                frames: 12,
                gap_frames: 1,
            },
            Action::MouseWheel { dx: 0.0, dy: -8.0 },
            Action::SnapshotSequence {
                prefix: "detent-2",
                frames: 12,
                gap_frames: 1,
            },
        ],
    }
}

/// Drive the strip's `MouseWheel` path: cursor over the active tile,
/// fire horizontal-dominant wheel events, capture before / after.
/// On a single-group strip the strip pans into empty wallpaper space
/// past the rightmost group (Niri-style infinite canvas). The
/// snapshot should show the white tile shifted leftward in the
/// viewport (revealing wallpaper to the right of where it was).
fn strip_horizontal_scroll() -> Scenario {
    Scenario {
        name: "strip_horizontal_scroll",
        description: "Swipe sideways over the strip and travel along it — one window \
             forward, then back — with the rail reading out where that is.",
        actions: vec![
            // Cursor over the active tile / strip area (well below the
            // topbar's persistent band and well to the right of the sidebar
            // gutter).
            Action::MoveCursor(Point::new(900.0, 500.0)),
            Action::Snapshot("00-before"),
            // A sideways swipe steps focus one window along the strip —
            // the same operation Super+ArrowRight performs — and the
            // focus-follow pan brings it into view. Positive dx travels
            // forward, matching the chord's direction.
            Action::MouseWheel { dx: 10.0, dy: 0.0 },
            Action::Wait(24),
            Action::Snapshot("01-after-pan"),
            // And back the way it came.
            Action::MouseWheel { dx: -10.0, dy: 0.0 },
            Action::Wait(24),
            Action::Snapshot("02-back-to-start"),
        ],
    }
}

/// Mid-drag capture of the new minimalist white-line drop indicator
/// (`hover_drop_idx` in `quick-ui::widget::tabs`). Press inside the
/// second tab pill, move past the drag threshold to latch
/// `pending_drag`, then move the cursor over a different inter-pill
/// gap and snapshot WITHOUT releasing so the indicator is visible in
/// the captured frame. Pairs the visual with the unit tests in
/// `widget::tabs::tests::hover_drop_idx_*` that pin the same field
/// without going through the GPU. Composed out of the
/// `MoveCursor` / `MousePress` / `MouseRelease` primitives because
/// `Action::Drag` always releases — and a release clears the
/// indicator before the snapshot fires.
fn tab_drag_hover_line_indicator() -> Scenario {
    Scenario {
        name: "tab_drag_hover_line_indicator",
        description: "Capture the minimalist white-line drop indicator that paints in the tab \
             bar while a drag is in flight over an inter-pill gap. Holds the drag \
             open (no release) so the line is visible in the snapshot.",
        actions: vec![
            Action::MoveCursor(Point::new(424.0, 120.0)),
            Action::MousePress,
            Action::MoveCursor(Point::new(484.0, 120.0)),
            Action::MoveCursor(Point::new(700.0, 120.0)),
            Action::Wait(4),
            Action::Snapshot("01-mid-drag-with-line"),
            Action::MouseRelease,
            Action::Wait(12),
            Action::Snapshot("02-after-release"),
        ],
    }
}

/// Switch to workspace index 1 (Research in the bootstrap tree),
/// capture, then back to 0.
///
/// The `-immediately` captures take `gap_frames: 0`, so they are the
/// **first frame rendered after the switch**, with no animation ticks in
/// between. That is where the shell's old whole-screen slide showed
/// itself: it started the entire content tree — wallpaper and top bar
/// included — a full viewport off-axis, so the first frame of every
/// switch was blank. These captures must be complete desktops, and must
/// be indistinguishable from the settled ones taken a frame later.
fn workspace_switch() -> Scenario {
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
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
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
    fn scenario_palette_typing_lands_query() {
        let mut d = ShellDriver::new((1600, 900));
        let mut sink = CountingSink { seen: Vec::new() };
        let scenario = palette_typing();
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
        // Final state: palette closed.
        assert!(
            !d.host().palette_visible(),
            "Esc should close the palette by the end of the scenario"
        );
    }

    #[test]
    fn scenario_catalogue_lookup_round_trip() {
        for s in catalogue() {
            let found = lookup(s.name).expect("named scenario should be in catalogue");
            assert_eq!(found.name, s.name);
        }
        assert!(lookup("does_not_exist").is_none());
    }

    #[test]
    fn collaboration_product_proof_keeps_invitation_presence_and_recovery_distinct() {
        let scenario = collaboration_product_proof();
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
                "04-attributed-history",
                "05-history-comparison",
                "06-reverted-forward",
                "07-ada-in-files",
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

    /// Snapshot names are a contract with the visual review suite. Check the
    /// actual state at each consequential frame as well, so a future fixture
    /// regression cannot quietly emit a plausible but false PNG.
    #[allow(
        clippy::struct_excessive_bools,
        reason = "each flag records an independent collaboration-proof invariant"
    )]
    struct CollaborationProofSink {
        pending_invite_is_durable: bool,
        ada_activity_is_visible: bool,
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
            let Some(workspace) = tree.active_workspace() else {
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
                            && workspace.role_of(&"ada".to_string()).is_some()
                    });
                }
                "04-attributed-history" => {
                    let activity = driver.host().room_activity();
                    self.ada_activity_is_visible = activity
                        .author_filter()
                        .is_some_and(|author| author.eq_ignore_ascii_case("ada"))
                        && activity.visible_len() > 0
                        && (0..activity.visible_len()).all(|index| {
                            activity
                                .visible_entry(index)
                                .is_some_and(|entry| entry.author.name.eq_ignore_ascii_case("ada"))
                        });
                }
                "06-reverted-forward" => {
                    self.forward_revert_is_announced = state.notifications().iter().any(|notice| {
                        notice.title == "Reverted a workspace change"
                            && notice.body.contains("Written forward")
                    });
                }
                "07-ada-in-files" => {
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

    #[test]
    fn collaboration_product_proof_frames_are_backed_by_the_claimed_state() {
        let mut driver = ShellDriver::new((1600, 900));
        let mut sink = CollaborationProofSink {
            pending_invite_is_durable: false,
            ada_activity_is_visible: false,
            forward_revert_is_announced: false,
            ada_is_active_in_files: false,
        };
        run(&mut driver, &collaboration_product_proof(), &mut sink);
        assert!(
            sink.pending_invite_is_durable,
            "the pending-invite frame must show both durable access and non-live presence"
        );
        assert!(
            sink.ada_activity_is_visible,
            "the attributed-history frame must visibly filter to Ada's authored changes"
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

    #[test]
    fn scenario_workspace_switch_moves_active_index() {
        let mut d = ShellDriver::new((1600, 900));
        let initial = d.chrome().tree().active;
        let mut sink = NullSink;
        let scenario = workspace_switch();
        run(&mut d, &scenario, &mut sink);
        // After the round-trip the active workspace should match the
        // initial one again.
        assert_eq!(d.chrome().tree().active, initial);
    }

    /// Regression: post-action waits in built-in scenarios must outlast
    /// the longest in-flight animation, otherwise offscreen snapshots
    /// capture mid-animation pixels (slide voids, layout-spring pill
    /// overlap) instead of the settled state the scenario name implies.
    ///
    /// We only enforce the obvious cases here — the catalogue grows,
    /// and not every wait gates a known animation — but the two we did
    /// fix during the 2026-05-04 inspection sweep are pinned so a future
    /// edit that shortens them re-introduces the visual artefacts.
    #[test]
    fn scenario_post_animation_waits_outlast_their_animation() {
        // tab_drag releases a drag whose post-drop layout spring runs
        // for LAYOUT_ANIM_MS = 180 ms (see ui::widget::tabs).
        let td = tab_drag();
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

    /// `run_with(per_frame = Some(n))` must insert `n` intermediate
    /// snapshots between a named snapshot and the surrounding waits.
    /// Pinned so a future edit that drops the wait expansion would
    /// re-introduce the "single still frame for animated scenarios"
    /// gap the harness was added to close.
    #[test]
    fn per_frame_flag_inserts_snapshots_between_waits() {
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

    /// When `per_frame` is `None`, `run_with` must behave exactly like
    /// `run` — no extra snapshots inserted.
    #[test]
    fn per_frame_none_is_identical_to_plain_run() {
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

    /// Sink that records what the strip's scroll looks like at each
    /// named snapshot, straight off the live `GroupRow` the shell pans.
    struct StripScrollSink {
        seen: Vec<(String, f64, f64)>,
    }
    impl SnapshotSink for StripScrollSink {
        fn capture(&mut self, driver: &mut ShellDriver, name: &str) {
            let strip_id = {
                let st = driver.chrome();
                let tree = st.tree();
                tree.active_workspace()
                    .and_then(|ws| ws.strips.first().map(|s| s.id))
            };
            let (scroll_x, max_scroll_x) = strip_id
                .and_then(|sid| driver.host_mut().group_row_mut_for_tests(sid))
                .map_or((f64::NAN, f64::NAN), |row| {
                    (row.scroll_x(), row.max_scroll_x())
                });
            self.seen.push((name.to_string(), scroll_x, max_scroll_x));
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
        run(&mut d, &strip_scroll_across_apps(), &mut sink);

        let st = d.chrome();
        let tree = st.tree();
        let strip = &tree.active_workspace().expect("active workspace").strips[0];
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

    /// And it has to actually travel: the strip holds more than fits,
    /// focusing the far window carries it off its resting position, and
    /// focusing the first one returns it home. Read off the live
    /// `GroupRow` the focus-follow reconciler writes to.
    #[test]
    fn strip_scroll_scenario_pans_the_strip_and_back() {
        let mut d = ShellDriver::new((1600, 900));
        let mut sink = StripScrollSink { seen: Vec::new() };
        run(&mut d, &strip_scroll_across_apps(), &mut sink);

        let at = |name: &str| -> (f64, f64) {
            let (_, sx, mx) = sink
                .seen
                .iter()
                .find(|(n, _, _)| n == name)
                .unwrap_or_else(|| panic!("scenario should snapshot {name}"));
            (*sx, *mx)
        };
        let (rest_x, rest_max) = at("05-strip-at-rest");
        assert!(
            rest_max > 400.0,
            "four half-width windows must overflow the display by more than a third of \
             a screen, or there is nothing to pan across (max_scroll_x = {rest_max})"
        );
        assert!(
            rest_x < 1.0,
            "the strip starts at its leftmost position (scroll_x = {rest_x})"
        );
        let (panned_x, _) = at("06-panned-right");
        assert!(
            panned_x > 600.0,
            "focusing the far window must carry the strip most of the way \
             along it (scroll_x = {panned_x})"
        );
        let (back_x, _) = at("07-panned-back");
        assert!(
            back_x < 1.0,
            "focusing the first window again returns the strip home (scroll_x = {back_x})"
        );
    }

    #[test]
    fn chrome_state_json_includes_palette_query() {
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
}
