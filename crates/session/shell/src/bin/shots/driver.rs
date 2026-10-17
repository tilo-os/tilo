//! `ShellDriver` — programmable, headless-friendly driver for the tilo
//! shell tree. Owns a `ShellOverlayHost` plus everything the layout +
//! draw + event passes need (taffy tree, text renderer, theme, viewport,
//! Automerge doc), and exposes a small API the three modes (window,
//! offscreen, headless) and the `Scenario` engine all share.
//!
//! Intentionally generous in surface so the same driver covers:
//!   - "render this scene to PNG" (offscreen)
//!   - "drive a scenario, dump state" (headless / AI-control / e2e)
//!   - "pre-run a scenario then go interactive" (window mode)

use std::collections::HashMap;
use std::time::{Duration, Instant};
use tilo_automerge::ApplyIncoming;

use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

#[cfg(target_os = "linux")]
use shell::compositor::capture::CaptureExecutor;
use shell::compositor::frame::FrameBuilder;
use shell::shell::chrome_state::{Chrome, ChromeState, WsTab};
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::quick_settings::QsLevel;
use shell::shell::state::WorkspaceTree;
use ui::Widget;

use taffy::prelude::NodeId;

/// Synthetic frame duration for `step_frames` — matches a 60 Hz monitor.
/// Used only as the unit for the spin-wait loop; animations themselves
/// read real wall time off `Instant::now()` inside the host so we still
/// have to spend that time. Kept short enough that sleeping 60 frames
/// (~1 s) doesn't blow scenario runtime.
const FRAME_MS: u64 = 16;
/// How long a capture waits for pending visual work before it shows it.
const CAPTURE_SETTLE: Duration = Duration::from_secs(5);

pub struct ShellDriver {
    /// Layout + paint core, shared with the native compositor mode.
    /// The driver wraps it and adds input dispatch, focus tracking,
    /// and modifier state on top.
    fb: FrameBuilder,
    state: AutoCommit,
    root_obj: ObjId,
    modifiers: Modifiers,
    focus: Option<NodeId>,
    last_cursor: Point,
    /// Opt-in because screenshot/recording intents perform filesystem/GPU
    /// work; scenarios enable it only when they are explicitly verifying the
    /// production capture handoff.
    /// Linux-only: the capture pipeline it drives is `compositor::capture`,
    /// which does not exist on a macOS review build of `shots`.
    #[cfg(target_os = "linux")]
    capture_executor: Option<CaptureExecutor>,
    lock_visual: Option<LockVisualState>,
    /// Another device of the shown device's person, and the person's
    /// workspace list as the shown device keeps it.
    own_device: Option<(
        crate::own_device::OwnDevice,
        shell::shell::personal_workspaces::PersonalWorkspaces,
    )>,
}

#[derive(Clone, Copy, Debug)]
pub enum LockVisualState {
    Default,
    Password,
    Verifying,
    Error,
    Notifications,
}

impl ShellDriver {
    /// Build a driver around the canonical bootstrap workspace tree at
    /// `viewport` size. Equivalent to what `bin/shots`'s headless path
    /// did per-scene, but stateful across actions.
    pub fn new(viewport: (u32, u32)) -> Self {
        Self::with_tree(WorkspaceTree::bootstrap_demo(), viewport)
    }

    /// [`Self::new`] for scene `name`. `offline` shows the session whose
    /// collaboration service went away: its apps reach for the session's
    /// broker at a socket nobody serves, so a room's documents lose their
    /// route as they do when the service stops.
    pub fn for_scene(name: &str, viewport: (u32, u32)) -> Self {
        let broker = (name == "offline").then(|| {
            let gone =
                std::env::temp_dir().join(format!("tilo-shots-gone-{}", uuid::Uuid::new_v4()));
            (gone.join("collab.sock"), gone.join("identity.sock"))
        });
        Self::build(WorkspaceTree::bootstrap_demo(), viewport, broker)
    }

    /// Build a driver around the provided workspace tree at `viewport` size.
    pub fn with_tree(tree: WorkspaceTree, viewport: (u32, u32)) -> Self {
        Self::build(tree, viewport, None)
    }

    /// [`Self::with_tree`], its apps reaching the session broker at
    /// `broker`'s collaboration and identity sockets, when given.
    fn build(
        tree: WorkspaceTree,
        viewport: (u32, u32),
        broker: Option<(std::path::PathBuf, std::path::PathBuf)>,
    ) -> Self {
        // Boot adopts every ownerless workspace before the session ever
        // paints (`compositor::session_bootstrap`), so a room you made is a room
        // you own; skipping it renders "no members" for rooms that on a
        // real machine are yours.
        // The driver then acts as that same owner in every room: the
        // identity registry of the machine rendering would otherwise pick
        // who "you" are, and a roster whose viewer is not the room's owner
        // offers no Invite.
        let owner = shell::shell::state::local_user_id();
        let mut tree = tree;
        tree.adopt_ownerless_workspaces(&owner);
        let hosted_tabs = hosted_app_tabs(&tree);
        // An owned fixture never reads the wall clock, so frames compare
        // across minutes; it shows the time it is given, like a session
        // shows the time it reads.
        let mut fb = match shell::shell::hosted_inputs::HostedTileInputs::fixture()
            .and_then(|inputs| inputs.with_fixture_clock(shots_clock()))
            .and_then(|inputs| match broker {
                Some((collab, identity)) => inputs.with_session_broker_at(collab, identity),
                None => Ok(inputs),
            }) {
            Ok(inputs) => FrameBuilder::with_tree_and_inputs(&tree, viewport, inputs),
            Err(error) => {
                eprintln!(
                    "shots: fixture inputs unavailable ({error}); the bar shows a placeholder time"
                );
                FrameBuilder::with_tree(tree, viewport)
            }
        };
        fb.with_chrome(|st| st.act_as(owner));
        // Binding a tab never creates its document: the action that opens
        // an app does. This driver opens every tab of the tree it is handed
        // into its own fresh record store, so it is that action here —
        // without it every in-process app renders "record does not exist".
        let records = fb.chrome().records().clone();
        for tab in hosted_tabs {
            // A refusal stays on screen as that tile's unavailable card;
            // the line here says which tab and why.
            if let Err(error) =
                shell::shell::tab_documents::create_scratch_tab_record(&records, tab)
            {
                eprintln!("shots: tab {tab} has no document: {error}");
            }
        }
        Self {
            fb,
            state: AutoCommit::new(),
            root_obj: automerge::ROOT.clone(),
            modifiers: Modifiers::default(),
            focus: None,
            last_cursor: Point::ORIGIN,
            #[cfg(target_os = "linux")]
            capture_executor: None,
            lock_visual: None,
            own_device: None,
        }
    }

    /// Lay the shell out across `outputs` — the displays the live
    /// compositor would report — rather than one display the size of the
    /// viewport, so each window group's width is a fraction of its own
    /// monitor.
    pub fn set_outputs(&mut self, outputs: &[shell::compositor::backend::Output]) {
        self.fb
            .set_output_geometry(shell::compositor::backend::output_geometry(outputs));
        self.relayout();
    }

    /// Run `compute_layout` + `apply_layout` against the current tree. The
    /// host's animation state is preserved because we don't rebuild the
    /// taffy tree — only the layout pass re-runs.
    ///
    /// If the host reports `take_structure_dirty()` (e.g. a tab drop
    /// spawned a new `WindowGroup`, or a closed group's fade elapsed),
    /// the taffy tree is rebuilt first so the new structure participates
    /// in layout. Mirrors `App::handle_window_event`'s post-event sweep.
    pub fn relayout(&mut self) {
        // A compositor frame harvests pending tab/group gestures before it
        // rebuilds layout. A drag can enqueue a topology change without
        // changing the live rows until this production update pass runs.
        self.fb.pump_apps(&mut self.state, &self.root_obj);
        self.fb.relayout();
    }

    /// Mutate chrome state — opens overlays, switches tabs, tweaks
    /// quick-settings, etc. Borrows the chrome internally so callers
    /// don't have to juggle the `RefCell`.
    pub fn with_chrome<F: FnOnce(&mut ChromeState)>(&mut self, f: F) {
        self.fb.with_chrome(f);
    }

    /// Read-only handle to the chrome cell. Use for assertions /
    /// serialisation — the caller is responsible for keeping the
    /// `Ref` short-lived.
    pub fn chrome(&self) -> std::cell::Ref<'_, ChromeState> {
        self.fb.chrome()
    }

    /// Get a cheap-clone handle to the chrome cell.
    pub fn chrome_handle(&self) -> Chrome {
        self.fb.chrome_handle()
    }

    /// Get a read-only reference to the overlay host.
    pub fn host(&self) -> &ShellOverlayHost {
        self.fb.host()
    }
    /// Mutable overlay host, for scenarios and tests that put the shell
    /// into a specific overlay state before stepping frames.
    pub fn host_mut(&mut self) -> &mut ShellOverlayHost {
        self.fb.host_mut()
    }

    /// Move to the empty tab-bar space that owns whole-group dragging.
    /// Resolving from live group geometry keeps held-drag visual scenarios
    /// valid at both compact and normal viewport widths.
    pub fn move_cursor_to_group_grab(&mut self, group_idx: usize) -> bool {
        let strip_id = {
            let state = self.chrome();
            let tree = state.tree();
            tree.desktop
                .active_workspace()
                .and_then(|workspace| workspace.strips.first())
                .map(|strip| strip.id)
        };
        let Some(strip_id) = strip_id else {
            return false;
        };
        let point = {
            let Some(group) = self.host_mut().group_at_for_tests(strip_id, group_idx) else {
                return false;
            };
            let bar = group.bar_rect();
            let last = group.tab_rects().last().copied().unwrap_or(bar);
            Point::new(
                f64::midpoint(last.x1, bar.x1),
                f64::midpoint(bar.y0, bar.y1),
            )
        };
        self.move_cursor(point);
        true
    }

    /// Carry the active whole-group drag past another group's midpoint.
    /// The small over-travel makes the intended after-slot unambiguous while
    /// retaining the source grab's y coordinate and pointer capture.
    pub fn move_cursor_to_group_reorder_target(&mut self, group_idx: usize) -> bool {
        let strip_id = {
            let state = self.chrome();
            let tree = state.tree();
            tree.desktop
                .active_workspace()
                .and_then(|workspace| workspace.strips.first())
                .map(|strip| strip.id)
        };
        let Some(strip_id) = strip_id else {
            return false;
        };
        let x = {
            let Some(group) = self.host_mut().group_at_for_tests(strip_id, group_idx) else {
                return false;
            };
            group.get_rect().center().x + 24.0
        };
        self.move_cursor(Point::new(x, self.last_cursor.y));
        true
    }

    /// Show the same reduced first-login ceremony used when Tilo is installed
    /// as an additional session on an existing Linux desktop. The path is
    /// deliberately read-only/nonexistent: this visual scenario never mints
    /// an identity, and cannot touch the developer's real first-login state.
    pub fn show_host_desktop_onboarding(&mut self) {
        self.fb
            .enable_host_desktop_onboarding(std::path::PathBuf::from(
                "/var/empty/tilo-shots-identity",
            ));
    }

    pub fn set_onboarding_visual_state(
        &mut self,
        state: shell::onboarding_view::OnboardingVisualState,
    ) {
        self.fb
            .host_mut()
            .set_onboarding_visual_state_for_snapshot(state);
        self.fb.relayout();
    }
    /// The palette's current query — what a scenario typed, read back
    /// through the same accessor the overlay uses.
    #[cfg(test)]
    pub fn palette_query(&self) -> &str {
        self.fb.host().palette_query()
    }

    /// Get the viewport size as (width, height) in logical pixels.
    pub fn viewport(&self) -> (f64, f64) {
        self.fb.viewport()
    }

    /// The size of the painted scene in physical pixels: what a renderer
    /// target is sized to. Differs from [`Self::viewport`] once a display
    /// draws at a scale other than 1.
    pub fn scene_size(&self) -> (f64, f64) {
        self.fb.scene_size()
    }

    /// Replace the active modifier set. Scenarios call this when they
    /// need a chord (e.g. Cmd+K, Cmd+Tab); a `dispatch` of the resulting
    /// `Key(Tab)` then sees `is_mod = true` inside the host.
    pub fn set_modifiers(&mut self, state: ModifiersState) {
        self.modifiers = Modifiers::from(state);
    }

    /// Dispatch a single `WindowEvent` through the host's `handle_event`
    /// pipeline. Returns whether the event was consumed.
    ///
    /// The shared wire to the host lives on [`FrameBuilder::dispatch_event`];
    /// this method only adds driver-local bookkeeping (cursor cache,
    /// modifier snapshot) before delegating.
    pub fn dispatch(&mut self, event: &WindowEvent) -> bool {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = Point::new(position.x, position.y);
        }
        if let WindowEvent::ModifiersChanged(m) = event {
            self.modifiers = *m;
        }
        self.fb.dispatch_event(
            event,
            &self.modifiers,
            &mut ui::widget::Edits::lend(&mut self.state),
            &self.root_obj,
            &mut self.focus,
        )
    }

    /// Synthesise a click at `p` — a `CursorMoved` to position, then a
    /// `Pressed`+`Released` left-button pair. Each step gets its own
    /// `dispatch` so per-event hit-tests run in the same order they
    /// would on real hardware.
    pub fn click_at(&mut self, p: Point) {
        self.move_cursor(p);
        self.press();
        self.release();
    }

    /// Synthesise a `MouseWheel` event at the cursor's current position
    /// (the host's wheel handlers gate on `last_cursor`, which is the
    /// last `CursorMoved` value the driver dispatched). `dx`/`dy` are
    /// `LineDelta` units — one wheel detent ≈ `1.0`. Trackpad-style
    /// pixel deltas should multiply by ~`1.0/16.0` first to stay
    /// concept-faithful with the strip's per-line factor.
    pub fn mouse_wheel(&mut self, dx: f32, dy: f32) {
        self.dispatch(&ui::synthetic_input::mouse_wheel(dx, dy));
    }

    /// Synthesise a `CursorMoved` event WITHOUT pressing or releasing.
    /// Use with `press()`/`release()` to compose drags that need
    /// intermediate snapshots — e.g. mid-drag drop-zone capture or
    /// scrub-then-pause behaviour. Updates `self.last_cursor` so any
    /// follow-up `mouse_wheel()` lands at the new position.
    pub fn move_cursor(&mut self, p: Point) {
        self.dispatch(&ui::synthetic_input::PointerAction::MoveTo(p).event(1.0));
    }

    /// Synthesise a left-button `Pressed` at the current cursor without
    /// releasing. Caller is responsible for the matching `release()`
    /// once the drag is complete.
    pub fn press(&mut self) {
        self.dispatch(&ui::synthetic_input::PointerAction::Press(MouseButton::Left).event(1.0));
    }

    /// Synthesise a left-button `Released` at the current cursor.
    pub fn release(&mut self) {
        self.dispatch(&ui::synthetic_input::PointerAction::Release(MouseButton::Left).event(1.0));
    }

    /// Press at `from`, drag through `steps` intermediate `CursorMoved`
    /// events, release at `to`. Used by scenarios that exercise the
    /// tab-drag / wheel-spin flows.
    pub fn drag(&mut self, from: Point, to: Point, steps: u32) {
        self.drag_with_button(from, to, steps, MouseButton::Left);
    }

    /// Drag with the middle button. The markup viewport reserves this
    /// gesture for panning so drawing can remain a one-step left drag.
    pub fn middle_drag(&mut self, from: Point, to: Point, steps: u32) {
        self.drag_with_button(from, to, steps, MouseButton::Middle);
    }

    fn drag_with_button(&mut self, from: Point, to: Point, steps: u32, button: MouseButton) {
        self.dispatch(&ui::synthetic_input::cursor_moved(from.x, from.y));
        self.dispatch(&ui::synthetic_input::mouse_input(
            ElementState::Pressed,
            button,
        ));
        let n = f64::from(steps.max(1));
        for s in 1..=steps.max(1) {
            let t = f64::from(s) / n;
            let p = Point::new(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
            self.dispatch(&ui::synthetic_input::cursor_moved(p.x, p.y));
        }
        self.dispatch(&ui::synthetic_input::mouse_input(
            ElementState::Released,
            button,
        ));
    }

    /// Synthesise a `KeyboardInput { state: Pressed }` for `key`.
    pub fn press_key(&mut self, key: &Key) {
        let event = key_press_event(key);
        self.dispatch(&event);
    }

    /// Type a literal string — one `Key::Character` event per non-control
    /// character. Triggers the same `handle_event` path the live binary's
    /// keyboard handler does. Caller is expected to have opened an
    /// input target (palette, name editor, etc.) first.
    pub fn type_str(&mut self, s: &str) {
        for c in s.chars() {
            if c.is_control() {
                continue;
            }
            let event = key_press_event(&Key::Character(winit::keyboard::SmolStr::new(
                c.to_string(),
            )));
            self.dispatch(&event);
        }
    }

    /// Step animation timers by `frames` × ~16 ms. Drives close fades,
    /// crossfades, palette/QS/WS-settings open animations, etc. Sleeps
    /// real time because the host reads `Instant::now()` directly —
    /// there's no virtual clock hook (yet).
    pub fn step_frames(&mut self, frames: u32) {
        for _ in 0..frames {
            std::thread::sleep(Duration::from_millis(FRAME_MS));
            // Run the same readonly sync, typed settlement and update
            // as the native compositor before painting animations.
            self.fb.pump_apps(&mut self.state, &self.root_obj);
            // Re-draw into a throwaway scene so tick functions inside
            // `draw` (overview crossfade tick, etc.) see elapsed time.
            let _ = self.paint();
        }
    }

    /// Render the current frame into a fresh Vello scene. Caller is
    /// responsible for passing it to a renderer (offscreen or live).
    /// Draw floating chrome as under the reduced-transparency setting.
    pub fn set_reduce_transparency(&mut self, on: bool) {
        self.fb.set_reduce_transparency_for_shots(on);
    }

    /// The backdrops the latest [`Self::paint`] drew floating surfaces over;
    /// see `FrameBuilder::take_backdrop`.
    pub fn take_backdrop(&mut self) -> Option<std::sync::Arc<ui::backdrop::BackdropFrame>> {
        self.fb.take_backdrop()
    }

    pub fn paint(&mut self) -> Scene {
        if let Some(state) = self.lock_visual {
            // The lock scene has no floating surfaces; drop any earlier frame's.
            let _ = self.fb.take_backdrop();
            let (width, height) = self.fb.scene_size();
            let output = shell::compositor::lock_screen::LockOutput {
                origin: (0.0, 0.0),
                size: (width, height),
                scale: 1.0,
            };
            let view = match state {
                LockVisualState::Default => shell::compositor::lock_screen::LockView {
                    clock: "12:34",
                    date: "Monday, 7 September",
                    ..Default::default()
                },
                LockVisualState::Password => shell::compositor::lock_screen::LockView {
                    clock: "12:34",
                    date: "Monday, 7 September",
                    password_len: 8,
                    ..Default::default()
                },
                LockVisualState::Verifying => shell::compositor::lock_screen::LockView {
                    clock: "12:34",
                    date: "Monday, 7 September",
                    password_len: 8,
                    verifying: true,
                    ..Default::default()
                },
                LockVisualState::Error => shell::compositor::lock_screen::LockView {
                    clock: "12:34",
                    date: "Monday, 7 September",
                    error: Some(shell::compositor::lock_auth::INCORRECT_MSG),
                    ..Default::default()
                },
                LockVisualState::Notifications => shell::compositor::lock_screen::LockView {
                    clock: "12:34",
                    date: "Monday, 7 September",
                    notification_count: 3,
                    ..Default::default()
                },
            };
            let theme = self.fb.theme().clone();
            return shell::compositor::lock_screen::draw_lock_scene(
                &[output],
                &theme,
                self.fb.text_renderer_mut(),
                &view,
            );
        }
        let mut scene = self.fb.paint();
        #[cfg(target_os = "linux")]
        if let Some(executor) = self.capture_executor.as_mut() {
            let _ = executor.execute_ready(&mut self.fb, &mut scene);
            // A still shows the capture as finished: the compositor applies
            // the worker's result on a later frame, a shot waits for it.
            let _ = executor.finish_io_blocking(&mut self.fb);
            return scene;
        }
        self.fb.draw_capture_overlays(&mut scene);
        scene
    }

    pub fn set_lock_visual_state(&mut self, state: LockVisualState) {
        self.lock_visual = Some(state);
    }

    pub fn set_capture_execution_enabled(&mut self, enabled: bool) {
        #[cfg(target_os = "linux")]
        {
            self.capture_executor = enabled.then(CaptureExecutor::default);
        }
        #[cfg(not(target_os = "linux"))]
        let _ = enabled;
    }

    pub fn set_capture_recording_active(&mut self, active: bool) {
        self.fb.set_capture_recording_active(active);
    }

    pub fn set_capture_recording_transition_progress(&mut self, permille: u16) {
        self.fb.set_capture_recording_transition_progress(permille);
    }

    pub fn set_capture_pip_transition_progress(&mut self, permille: u16) {
        self.fb.set_capture_pip_transition_progress(permille);
    }

    pub fn set_capture_recording_paused(&mut self, paused: bool) {
        self.fb.set_capture_recording_paused(paused);
    }

    pub fn set_capture_recording_state_transition_progress(&mut self, permille: u16) {
        self.fb
            .set_capture_recording_state_transition_progress(permille);
    }

    pub fn set_capture_recording_receipt_transition_progress(&mut self, permille: u16) {
        self.fb
            .set_capture_recording_receipt_transition_progress(permille);
    }

    pub fn set_capture_recording_finished(&mut self) {
        self.fb.set_capture_recording_finished();
    }

    pub fn set_capture_replay_active(&mut self, active: bool, audio: ui::widget::AudioCaptureMode) {
        self.fb.set_capture_replay_active(active, audio);
    }

    pub fn set_capture_replay_buffered_seconds(&mut self, seconds: u8) {
        self.fb.set_capture_replay_buffered_seconds(seconds);
    }

    pub fn set_capture_replay_saved(&mut self) {
        self.fb.set_capture_replay_saved();
    }

    pub fn set_capture_screenshot_finished(&mut self, copied: bool) {
        let palette = [
            [17, 19, 24, 255],
            [24, 29, 39, 255],
            [31, 46, 70, 255],
            [54, 104, 180, 255],
            [88, 142, 224, 255],
            [22, 24, 30, 255],
        ];
        let pixels = palette.into_iter().cycle().take(24).collect();
        self.fb.set_capture_screenshot_finished(
            copied,
            1_584,
            716,
            ui::widget::CapturePreview {
                columns: 6,
                rows: 4,
                pixels,
            },
        );
    }

    pub fn set_capture_screenshot_transition_progress(&mut self, permille: u16) {
        self.fb.set_capture_screenshot_transition_progress(permille);
    }

    pub fn set_screenshot_markup_transition_progress(&mut self, permille: u16) {
        self.fb.set_screenshot_markup_transition_progress(permille);
    }

    pub fn set_capture_audio_available(&mut self, available: bool) {
        self.fb.set_capture_audio_available(available, available);
    }

    /// Convenience: relayout + draw. Used by the offscreen scene-set
    /// path (legacy `--scenes`) where state mutations between scenes
    /// can affect the structural layout (overlay reservations).
    pub fn paint_settled(&mut self) -> Scene {
        // A live frame is `pump_apps` → paint (see `compositor::native`).
        // Pump first here too, so every reconciler `Widget::update` runs
        // (active-tab writeback, focused-group marking, scroll-into-view,
        // cross-strip dispatch) is reflected in what gets captured.
        let root = self.root_obj.clone();
        self.fb.pump_apps(&mut self.state, &root);
        // Work a person would see finish a moment after the window opened
        // (an app's first save being confirmed, an image arriving) finishes
        // before the capture too; what is still pending after the bound is
        // stuck, and the shot shows it.
        let deadline = Instant::now().checked_add(CAPTURE_SETTLE);
        while self.fb.host().has_pending_visual_work() {
            if deadline.is_none_or(|deadline| Instant::now() >= deadline) {
                eprintln!(
                    "shots: visual work still pending after {CAPTURE_SETTLE:?}; capturing it"
                );
                break;
            }
            std::thread::sleep(Duration::from_millis(FRAME_MS));
            self.fb.pump_apps(&mut self.state, &root);
            let _ = self.fb.paint_settled();
        }
        self.fb.paint_settled()
    }
}

/// The moment every shot shows: a weekday morning.
fn shots_clock() -> tilo_desktop::chrome::ClockSnapshot {
    tilo_desktop::chrome::ClockSnapshot {
        unix_seconds: 1_700_000_000,
        clock: "09:41".into(),
        weekday: "Tue".into(),
        date: "14 November".into(),
    }
}

// Real room-owned task state qualifies catch-up review, only in shots fixtures.
fn seed_catch_up_tasks(tree: &mut WorkspaceTree, local: &str) {
    let workspace = tree.desktop.active;
    for (title, status) in [
        (
            "Finish release notes",
            shell::shell::state::WorkspaceTaskStatus::Done,
        ),
        (
            "Check yesterday's goals",
            shell::shell::state::WorkspaceTaskStatus::InProgress,
        ),
        (
            "Review the draft result",
            shell::shell::state::WorkspaceTaskStatus::Waiting,
        ),
    ] {
        let task = tree.create_workspace_task(
            workspace,
            title.to_string(),
            local.to_string(),
            None,
            shell::shell::state::WorkspaceTaskRoute::Manual,
            None,
        );
        assert!(
            task.is_some(),
            "shots fixture must create real workspace work"
        );
        if let Some(task) = task {
            assert!(tree.set_workspace_task_status(workspace, &task, status, local));
        }
    }
}

/// Every tab in `tree` whose tile is an in-process app — the tabs that
/// the production open-app actions give a scratch document when they
/// mint them (`window_actions`).
fn all_tab_ids(tree: &WorkspaceTree) -> Vec<u64> {
    tree.desktop
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .map(|tab| tab.id)
        .collect()
}

fn hosted_app_tabs(tree: &WorkspaceTree) -> Vec<u64> {
    use shell::shell::state::TileContent;
    tree.desktop
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .filter(|tab| {
            matches!(
                tab.tile,
                TileContent::AppPlaceholder { .. } | TileContent::Stub { .. }
            )
        })
        .map(|tab| tab.id)
        .collect()
}

/// Append a tab to the first window group of the first workspace, straight
/// into a forked document. The `room-activity` scene's peer has no
/// `WorkspaceTree` — just a bare document, like a remote replica that has
/// only ever received change blobs — so this hand-writes the same shape
/// `WorkspaceTree::doc_append_tab_to_first_group_of_active_workspace` writes.
/// Returns whether anything was written.
fn peer_append_tab(doc: &mut automerge::AutoCommit, title: &str) -> bool {
    use automerge::transaction::Transactable;
    use automerge::{ObjType, ROOT, ReadDoc, ScalarValue};
    let Ok(Some((_, workspaces))) = doc.get(&ROOT, "workspaces") else {
        return false;
    };
    let Ok(Some((_, ws))) = doc.get(&workspaces, 0_usize) else {
        return false;
    };
    let Ok(Some((_, strips))) = doc.get(&ws, "strips") else {
        return false;
    };
    let Ok(Some((_, strip))) = doc.get(&strips, 0_usize) else {
        return false;
    };
    let Ok(Some((_, groups))) = doc.get(&strip, "window_groups") else {
        return false;
    };
    let Ok(Some((_, group))) = doc.get(&groups, 0_usize) else {
        return false;
    };
    let Ok(Some((_, tabs))) = doc.get(&group, "tabs") else {
        return false;
    };
    let at = doc.length(&tabs);
    let Ok(entry) = doc.insert_object(&tabs, at, ObjType::Map) else {
        return false;
    };
    let _ = doc.put(
        &entry,
        "id",
        ScalarValue::Uint(70_000u64.saturating_add(at as u64)),
    );
    let _ = doc.put(&entry, "title", title.to_string());
    true
}

impl ShellDriver {
    /// Puts a few records in rooms the user is *not* in, so search has
    /// something to bring here. Reachable only from `--shots`, but goes
    /// through the real `RoomRecords::reference` path, so what the scene
    /// paints is what a real room index produces.
    fn seed_records_in_other_rooms(&self) {
        use ui::record::{RecordId, RecordKind};
        let rooms: Vec<_> = {
            let handle = self.chrome_handle();
            let st = handle.borrow();
            let tree = st.tree();
            let active = tree.desktop.active_workspace().map(|w| w.room);
            tree.desktop
                .workspaces
                .iter()
                .filter(|w| Some(w.room) != active)
                .map(|w| w.room)
                .collect()
        };
        if rooms.is_empty() {
            return;
        }
        let records = [
            ("/home/q/books/dune.epub", "book"),
            ("/home/q/books/dune-messiah.epub", "book"),
            ("/home/q/notes/dune-reading-notes.md", "document"),
        ];
        let index = self.fb.host().room_records();
        let mut index = index.borrow_mut();
        for (i, (path, kind)) in records.iter().enumerate() {
            let idx = i.checked_rem(rooms.len()).unwrap_or(0);
            let Some(room) = rooms.get(idx) else { continue };
            index.reference(*room, &RecordId::for_file(path), &RecordKind::new(*kind));
        }
    }

    /// Record what an accepted invite and a couple of shares would have
    /// recorded: memberships in this room and in Research (whose peers
    /// are already present, so their faces are backed by a capability
    /// rather than by hearsay), and two resources handed to named
    /// people.
    fn seed_shared_room(&mut self) {
        use shell::shell::state::{Access, CapOverride, Scope, Subject};
        use tilo_action_policy::Role;
        let handle = self.chrome_handle();
        let owner = handle.borrow().local_user_id.clone();
        {
            let st = handle.borrow();
            let mut tree = st.tree_mut();
            let ws_id = tree.desktop.active;
            for (member, role) in [("ada", Role::Editor), ("bo", Role::Viewer)] {
                let _ = tree.admit_in_workspace(ws_id, &owner, member, role);
            }
            // Both members at their desks: a room only carries a display
            // name while a session of theirs is present.
            if let Some(ws) = tree.desktop.active_workspace_mut() {
                ws.collaborators
                    .push(shell::shell::state::Collaborator::new("ada", "Ada"));
                ws.collaborators
                    .push(shell::shell::state::Collaborator::new("bo", "Bo"));
            }
            for (path, who, verb) in [
                ("/var/workspace/design", "ada", Access::Write),
                ("/var/workspace/brief.pdf", "bo", Access::Read),
            ] {
                tree.grant_sandbox_in_workspace(
                    ws_id,
                    &owner,
                    CapOverride::grant(Subject::person(who), Scope::FsPath(path.to_string()), verb),
                );
            }
            // Research already has two people present. Give them the
            // membership their presence implies, so the row shows the
            // faces of people who can actually reach it.
            let research: Vec<_> = tree
                .desktop
                .workspaces
                .iter()
                .filter(|w| w.name == "Research")
                .map(|w| w.id)
                .collect();
            for id in research {
                for (member, role) in [("alice", Role::Editor), ("bob", Role::Viewer)] {
                    let _ = tree.admit_in_workspace(id, &owner, member, role);
                }
            }
        }
        self.with_chrome(|_| {});
    }

    /// Create the person's "Research" workspace on this device and show it.
    pub fn seed_own_workspace_for_shots(&mut self) {
        let handle = self.chrome_handle();
        let tabs = {
            let st = handle.borrow();
            let person = st.local_user_id.clone();
            let mut tree = st.tree_mut();
            tree.set_salt_override(crate::own_device::SHOWN_SALT);
            let before = all_tab_ids(&tree);
            crate::own_device::seed_research(&mut tree, crate::own_device::SHOWN_SALT, &person);
            let after = all_tab_ids(&tree);
            drop(tree);
            drop(st);
            after
                .into_iter()
                .filter(|tab| !before.contains(tab))
                .collect::<Vec<_>>()
        };
        self.open_tab_records(&tabs);
        self.with_chrome(ChromeState::bump);
    }

    /// Split the outline of this device's own "Research" workspace off.
    pub fn split_own_outline_for_shots(&mut self) {
        let handle = self.chrome_handle();
        {
            let st = handle.borrow();
            let mut tree = st.tree_mut();
            let room = tree.desktop.room_of(tree.desktop.active);
            if let Some(room) = room {
                crate::own_device::split_outline(&mut tree, room, crate::own_device::SHOWN_SALT);
            }
            drop(tree);
            drop(st);
        }
        self.with_chrome(ChromeState::bump);
    }

    /// Hold the person's laptop in memory beside this device.
    pub fn seed_other_own_device_for_shots(&mut self) {
        let person = self.chrome().local_user_id.clone();
        let handle = self.chrome_handle();
        handle
            .borrow()
            .tree_mut()
            .set_salt_override(crate::own_device::SHOWN_SALT);
        self.own_device = Some((
            crate::own_device::OwnDevice::laptop(&person),
            crate::own_device::board_list(),
        ));
    }

    /// Receive the laptop's workspace list and room content, and look at
    /// the laptop's workspace.
    pub fn receive_from_other_own_device_for_shots(&mut self) {
        let Some((mut laptop, mut list)) = self.own_device.take() else {
            return;
        };
        let person = self.chrome().local_user_id.clone();
        let handle = self.chrome_handle();
        let arrived = {
            let st = handle.borrow();
            let mut tree = st.tree_mut();
            let arrived = laptop.deliver(&person, &mut tree, &mut list);
            let shown = tree
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room == laptop.room)
                .map(|workspace| workspace.id);
            if let Some(shown) = shown {
                tree.desktop.set_active(shown);
            }
            arrived
        };
        // A tab arriving from another device opens its record here; the
        // live shell fetches it from that device, a scene starts it empty.
        self.open_tab_records(&arrived);
        self.own_device = Some((laptop, list));
        self.with_chrome(ChromeState::bump);
    }

    /// The laptop splits its outline off.
    pub fn other_own_device_splits_outline_for_shots(&mut self) {
        if let Some((laptop, _)) = self.own_device.as_mut() {
            let room = laptop.room;
            crate::own_device::split_outline(
                &mut laptop.tree,
                room,
                crate::own_device::LAPTOP_SALT,
            );
        }
    }

    fn open_tab_records(&self, tabs: &[u64]) {
        let records = self.chrome().records().clone();
        for tab in tabs {
            if let Err(error) =
                shell::shell::tab_documents::create_scratch_tab_record(&records, *tab)
            {
                eprintln!("shots: tab {tab} has no document: {error}");
            }
        }
    }

    /// Scenario-facing wrapper for the canonical shared-workspace fixture.
    /// Keeping the mutation here ensures screenshots use the same
    /// authority-checked capabilities and presence data as legacy scenes.
    pub fn seed_shared_workspace_for_shots(&mut self) {
        self.seed_shared_room();
    }

    /// Build the privacy-sensitive state through the same capability and
    /// compositor exposure stores the live shell reads. The display label is
    /// deliberately different from the principal id to guard agent typing.
    pub fn seed_agent_screen_exposure_for_shots(&self, surface_id: u64) {
        use shell::shell::chrome_state::ActiveExposure;
        self.seed_agent_member_for_shots();
        self.chrome_handle()
            .borrow_mut()
            .set_active_exposures(vec![ActiveExposure {
                peer: 9,
                surface: surface_id,
                can_control: true,
            }]);
    }

    /// An authenticated agent fixture shared by roster, settings and exposure
    /// scenes. Sample membership stays entirely within the shots renderer.
    fn seed_agent_member_for_shots(&self) {
        use shell::shell::state::Collaborator;
        use tilo_action_policy::roles::AgentPreset;

        let handle = self.chrome_handle();
        let owner = handle.borrow().local_user_id.clone();
        {
            let st = handle.borrow();
            let mut tree = st.tree_mut();
            tree.adopt_ownerless_workspaces(&owner);
            let workspace_id = tree.desktop.active;
            let changes = AgentPreset::Operator
                .grants(
                    &tree
                        .room_of(workspace_id)
                        .map(|room| room.to_string())
                        .unwrap_or_default(),
                    "scribe",
                    &tilo_action_policy::Subject::person(owner.clone()),
                )
                .into_iter()
                .map(tilo_action_policy::Change::Grant)
                .collect();
            let _ = tree.govern_in_workspace(workspace_id, &owner, changes);
            if let Some(workspace) = tree.desktop.active_workspace_mut() {
                workspace.collaborators.retain(|row| row.id != "scribe");
                workspace.collaborators.insert(
                    0,
                    Collaborator::new("scribe", "Release Notes Agent").with_session(9),
                );
            }
        }
    }

    pub fn seed_inferred_status_for_shots(&mut self, text: &str) {
        self.host_mut().seed_inferred_status_for_shots(text);
        let actor = {
            let state = self.chrome();
            if state.local_user_id.trim().is_empty() {
                shell::shell::state::local_user_id()
            } else {
                state.local_user_id.clone()
            }
        };
        let resource_id = "shots:q3-forecast".to_string();
        self.fb.apply_remote_presence_for_shots(
            actor.clone(),
            ui::presence::PresenceKey::Resource {
                app: "org.tilo.Sheets".to_string(),
                id: resource_id.clone(),
                kind: "sheet".to_string(),
            },
            ui::collaboration::PresencePayload::Interaction(Box::new(
                tilo_actions::SemanticInteraction {
                    version: tilo_actions::INTERACTION_VERSION,
                    invocation: tilo_actions::InvocationId::new(),
                    sequence: 1,
                    actor: tilo_actions::InteractionActor {
                        id: actor,
                        kind: tilo_actions::ActorKind::Person,
                        for_person: None,
                    },
                    phase: tilo_actions::InteractionPhase::Applying,
                    action: tilo_actions::InteractionAction {
                        id: "reconciling".to_string(),
                        label: "Reconciling".to_string(),
                        app: Some(tilo_actions::ActionApp {
                            id: "org.tilo.Sheets".to_string(),
                            label: "Sheets".to_string(),
                        }),
                    },
                    resource: tilo_actions::ActionResource {
                        id: resource_id,
                        kind: "sheet".to_string(),
                        label: "Q3 forecast".to_string(),
                    },
                    anchor: tilo_actions::InteractionAnchor::Resource,
                    group: None,
                },
            )),
        );
    }

    /// A desk to show the widgets on: a player in another workspace than
    /// this one, and three people here, one with a status line they wrote,
    /// one whose machine generated theirs, one with none, each on a window.
    pub fn seed_desk_for_shots(&mut self) {
        use shell::shell::now_playing::{FixturePlayers, PlaybackStatus, PlayerSnapshot};
        use shell::shell::state::Collaborator;
        let player = PlayerSnapshot {
            bus_name: "org.mpris.MediaPlayer2.spotify.instance7".to_string(),
            identity: "Spotify".to_string(),
            status: PlaybackStatus::Playing,
            title: Some("Teardrop".to_string()),
            artist: Some("Massive Attack".to_string()),
            artwork: None,
            track_id: Some("/org/tilo/shots/track/1".to_string()),
            length_us: Some(330_000_000),
            position_us: 94_000_000,
            can_play: true,
            can_pause: true,
            can_seek: true,
            can_go_next: true,
            can_go_previous: true,
            observed_at: Instant::now(),
        };
        self.host_mut()
            .set_now_playing_source_for_shots(std::rc::Rc::new(FixturePlayers::new(vec![player])));
        let handle = self.chrome_handle();
        let room = {
            let st = handle.borrow();
            let mut tree = st.tree_mut();
            let tab_ids = tree
                .desktop
                .active_workspace()
                .into_iter()
                .flat_map(|workspace| &workspace.strips)
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .map(|tab| tab.id)
                .collect::<Vec<_>>();
            if let Some(active) = tree.desktop.active_workspace_mut() {
                for (index, (id, name)) in [("mara", "Mara"), ("jonas", "Jonas"), ("ada", "Ada")]
                    .into_iter()
                    .enumerate()
                {
                    if !active.collaborators.iter().any(|person| person.id == id) {
                        let mut person = Collaborator::new(id, name);
                        person.active_tab = index
                            .checked_rem(tab_ids.len())
                            .and_then(|slot| tab_ids.get(slot))
                            .copied();
                        active.collaborators.push(person);
                    }
                }
            }
            tree.desktop
                .active_workspace()
                .map(|workspace| workspace.room.to_string())
        };
        if let Some(room) = room {
            for (id, text, automatic) in [
                ("mara", "Finishing the layout review before 3", false),
                ("jonas", "Comparing two onboarding drafts", true),
            ] {
                self.fb.apply_remote_presence_for_shots(
                    id.to_string(),
                    ui::presence::PresenceKey::Workspace { id: room.clone() },
                    ui::collaboration::PresencePayload::Status {
                        text: text.to_string(),
                        automatic,
                    },
                );
            }
        }
    }

    /// A key reached the focused window just now.
    pub fn desk_typing_for_shots(&mut self) {
        let now = Instant::now();
        self.host_mut().note_client_typing(Some(now), now);
    }

    /// Super alone, as the compositor classifies it.
    pub fn super_gesture_for_shots(&mut self, gesture: shell::shell::input_policy::SuperGesture) {
        let state = std::mem::take(&mut self.state);
        let root = self.root_obj.clone();
        self.host_mut().super_gesture(gesture, &state, &root);
        self.state = state;
    }

    /// The focused field's caret, in desktop coordinates.
    pub fn set_text_caret_for_shots(&mut self, caret: Option<Rect>) {
        self.host_mut().set_text_caret(caret);
    }

    /// Populate the live room through the same collaborator/capability state
    /// the People overlay reads. The varied tab attribution lets the rendered
    /// proof exercise meaningful location lines instead of screenshot-only
    /// placeholder text.
    pub fn seed_crowded_workspace_for_shots(&mut self) {
        use shell::shell::state::Collaborator;
        use tilo_action_policy::Role;

        let handle = self.chrome_handle();
        let owner = handle.borrow().local_user_id.clone();
        {
            let st = handle.borrow();
            let mut tree = st.tree_mut();
            tree.adopt_ownerless_workspaces(&owner);
            let workspace = tree.desktop.active;
            let tab_ids = tree
                .desktop
                .active_workspace()
                .into_iter()
                .flat_map(|workspace| &workspace.strips)
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .map(|tab| tab.id)
                .collect::<Vec<_>>();
            for (index, (id, name)) in [
                ("ada", "Ada"),
                ("bo", "Bo"),
                ("cam", "Cam"),
                ("dev", "Dev"),
                ("eli", "Eli"),
                ("flo", "Flo"),
                ("gia", "Gia"),
                ("han", "Han"),
            ]
            .into_iter()
            .enumerate()
            {
                let _ = tree.admit_in_workspace(
                    workspace,
                    &owner,
                    id,
                    if index % 3 == 2 {
                        Role::Viewer
                    } else {
                        Role::Editor
                    },
                );
                if let Some(active) = tree.desktop.active_workspace_mut()
                    && !active.collaborators.iter().any(|person| person.id == id)
                {
                    let mut collaborator = Collaborator::new(id, name);
                    collaborator.active_tab = index
                        .checked_rem(tab_ids.len())
                        .and_then(|slot| tab_ids.get(slot))
                        .copied();
                    active.collaborators.push(collaborator);
                }
            }
        }
        self.with_chrome(|state| {
            state.overlays.collaborators = true;
            state.bump();
        });
    }

    /// Materialise the durable half of an invite without pretending the
    /// recipient is already online. The membership record takes the same
    /// authority-checked path as workspace settings; the `invited` presence
    /// row is intentionally non-followable until a session arrives.
    pub fn seed_invite_pending_for_shots(&mut self) {
        use shell::shell::state::Collaborator;
        use tilo_action_policy::Role;

        let handle = self.chrome_handle();
        let owner = handle.borrow().local_user_id.clone();
        {
            let st = handle.borrow();
            let mut tree = st.tree_mut();
            tree.adopt_ownerless_workspaces(&owner);
            let workspace = tree.desktop.active;
            let _ = tree.admit_in_workspace(workspace, &owner, "ada", Role::Editor);
            if let Some(ws) = tree.desktop.active_workspace_mut()
                && !ws.collaborators.iter().any(|person| person.id == "ada")
            {
                ws.collaborators
                    .push(Collaborator::new("ada", "Ada").as_invited());
            }
        }
        self.with_chrome(|state| {
            state.overlays.collaborators = true;
            state.bump();
        });
    }

    /// Turn the same invited identity into live presence on Notes. This is a
    /// separate transition because a sent invite is access, whereas an active
    /// tab is session-only information that must not appear prematurely.
    pub fn admit_invited_collaborator_for_shots(&mut self) -> bool {
        let handle = self.chrome_handle();
        let mut admitted = false;
        {
            let st = handle.borrow();
            let mut tree = st.tree_mut();
            let notes_tab = tree.desktop.active_workspace().and_then(|ws| {
                ws.strips
                    .iter()
                    .flat_map(|strip| &strip.window_groups)
                    .flat_map(|group| &group.tabs)
                    .find(|tab| tab.title == "Notes")
                    .map(|tab| tab.id)
            });
            if let (Some(notes_tab), Some(ws)) = (notes_tab, tree.desktop.active_workspace_mut())
                && let Some(ada) = ws
                    .collaborators
                    .iter_mut()
                    .find(|person| person.id == "ada")
            {
                ada.invited = false;
                ada.active_tab = Some(notes_tab);
                admitted = true;
            }
        }
        self.with_chrome(|state| {
            state.overlays.collaborators = true;
            state.bump();
        });
        if admitted {
            self.seed_ada_intent_for_shots();
            self.seed_ada_live_work_for_shots("Editing", "Onboarding notes");
        }
        admitted
    }

    /// The remote participant's on-device model publishes only this short,
    /// room-scoped result. Their live tab remains a separate, factual action
    /// line in People, so the proof cannot conflate "where" with "why".
    fn seed_ada_intent_for_shots(&mut self) {
        let room = {
            let state = self.chrome();
            let tree = state.tree();
            tree.desktop
                .active_workspace()
                .map(|workspace| workspace.room.to_string())
        };
        if let Some(room) = room {
            self.fb.apply_remote_presence_for_shots(
                "ada".to_string(),
                ui::presence::PresenceKey::Workspace { id: room },
                ui::collaboration::PresencePayload::Status {
                    text: "Preparing the board review".to_string(),
                    automatic: true,
                },
            );
        }
    }

    fn seed_ada_live_work_for_shots(&mut self, action: &str, resource: &str) {
        let room = {
            let state = self.chrome();
            let tree = state.tree();
            tree.desktop
                .active_workspace()
                .map(|workspace| workspace.room.to_string())
        };
        if room.is_some() {
            let resource_id = format!("shots:{}", resource.to_ascii_lowercase().replace(' ', "-"));
            self.fb.apply_remote_presence_for_shots(
                "ada".to_string(),
                ui::presence::PresenceKey::Resource {
                    app: "tilo-shell-shots".to_string(),
                    id: resource_id.clone(),
                    kind: "workspace.resource".to_string(),
                },
                ui::collaboration::PresencePayload::Interaction(Box::new(
                    tilo_actions::SemanticInteraction {
                        version: tilo_actions::INTERACTION_VERSION,
                        invocation: tilo_actions::InvocationId::new(),
                        sequence: 1,
                        actor: tilo_actions::InteractionActor {
                            id: "ada".to_string(),
                            kind: tilo_actions::ActorKind::Person,
                            for_person: None,
                        },
                        phase: tilo_actions::InteractionPhase::Applying,
                        action: tilo_actions::InteractionAction {
                            id: action.to_ascii_lowercase(),
                            label: action.to_string(),
                            app: None,
                        },
                        resource: tilo_actions::ActionResource {
                            id: resource_id,
                            kind: "workspace.resource".to_string(),
                            label: resource.to_string(),
                        },
                        anchor: tilo_actions::InteractionAnchor::Resource,
                        group: None,
                    },
                )),
            );
        }
    }

    pub fn seed_foreign_app_work_for_shots(&mut self, app_id: &str, action: &str, resource: &str) {
        let resource_id = format!("shots:{}", resource.to_ascii_lowercase().replace(' ', "-"));
        self.fb.set_remote_display_name_for_shots("ada", "Ada");
        self.fb.apply_remote_presence_for_shots(
            "ada".to_string(),
            ui::presence::PresenceKey::Resource {
                app: app_id.to_string(),
                id: resource_id.clone(),
                kind: "workspace.resource".to_string(),
            },
            ui::collaboration::PresencePayload::Interaction(Box::new(
                tilo_actions::SemanticInteraction {
                    version: tilo_actions::INTERACTION_VERSION,
                    invocation: tilo_actions::InvocationId::new(),
                    sequence: 1,
                    actor: tilo_actions::InteractionActor {
                        id: "ada".to_string(),
                        kind: tilo_actions::ActorKind::Person,
                        for_person: None,
                    },
                    phase: tilo_actions::InteractionPhase::Applying,
                    action: tilo_actions::InteractionAction {
                        id: action.to_ascii_lowercase().replace(' ', "-"),
                        label: action.to_string(),
                        app: Some(tilo_actions::ActionApp {
                            id: app_id.to_string(),
                            label: app_id.to_string(),
                        }),
                    },
                    resource: tilo_actions::ActionResource {
                        id: resource_id,
                        kind: "workspace.resource".to_string(),
                        label: resource.to_string(),
                    },
                    anchor: tilo_actions::InteractionAnchor::Resource,
                    group: None,
                },
            )),
        );
    }

    /// Revert the earliest non-current entry that can produce a forward
    /// change. The room activity pane owns the operation, so the scenario
    /// exercises the same immutable-history recovery path as its button.
    pub fn revert_room_activity_for_shots(&mut self) -> bool {
        let candidates: Vec<_> = self
            .host()
            .room_activity()
            .entries()
            .iter()
            .map(|entry| entry.id)
            .collect();
        let handle = self.chrome_handle();
        for id in candidates {
            let changed = {
                let st = handle.borrow();
                let mut tree = st.tree_mut();
                self.host_mut()
                    .room_activity_mut()
                    .revert_to(&mut *tree, &id)
            };
            if changed {
                self.with_chrome(|state| {
                    state.overlays.room_activity = false;
                    state.overlays.collaborators = false;
                    state.bump();
                    state.push_notification(
                        "Reverted a workspace change",
                        "Written forward: the version you reverted from is still in the history.",
                    );
                });
                self.host_mut().force_room_activity_closed_for_shots();
                // Prime the notification host. It drains newly queued chrome
                // notifications during paint, so the named recovery frame
                // must be the frame that displays the result, not the next
                // unrelated step in the journey.
                let _ = self.fb.paint();
                return true;
            }
        }
        false
    }

    /// Focus the active workspace's tab titled `title`, returning its id.
    fn focus_tab_titled(&mut self, title: &str) -> Option<u64> {
        let (strip, group, tab) = {
            let state = self.chrome();
            let tree = state.tree();
            tree.desktop.active_workspace().and_then(|ws| {
                ws.strips.iter().find_map(|strip| {
                    strip.window_groups.iter().find_map(|group| {
                        group
                            .tabs
                            .iter()
                            .find(|tab| tab.title == title)
                            .map(|tab| (strip.id, group.id, tab.id))
                    })
                })
            })
        }?;
        self.host_mut().focus_window_on_strip(strip, group, tab);
        Some(tab)
    }

    /// Show that presence attaches to app content. Focus the Files hosted
    /// window and move Ada's location to its tab.
    pub fn show_collaborator_in_files_for_shots(&mut self) -> bool {
        let Some(files_tab) = self.focus_tab_titled("Files") else {
            return false;
        };
        self.with_chrome(|state| {
            let mut tree = state.tree_mut();
            if let Some(ada) = tree.desktop.active_workspace_mut().and_then(|ws| {
                ws.collaborators
                    .iter_mut()
                    .find(|person| person.id == "ada")
            }) {
                ada.invited = false;
                ada.active_tab = Some(files_tab);
            }
            drop(tree);
            state.overlays.room_activity = false;
            state.overlays.collaborators = true;
            state.bump();
        });
        self.seed_ada_live_work_for_shots("Organizing", "Board review packet");
        true
    }

    /// Seeds the active workspace with membership and granted capabilities
    /// via `WorkspaceTree::grant_in_workspace` — the same authority-checked
    /// path the settings panel and portal use, so this is a state the
    /// compositor can actually be in.
    fn seed_workspace_permissions(&mut self) {
        use shell::shell::state::{Access, CapOverride, Expiry, Scope, Subject};
        use tilo_action_policy::Role;
        let handle = self.chrome_handle();
        let owner = handle.borrow().local_user_id.clone();
        {
            let st = handle.borrow();
            let mut tree = st.tree_mut();
            tree.adopt_ownerless_workspaces(&owner);
            let ws_id = tree.desktop.active;
            for (member, role) in [("ada", Role::Editor), ("bo", Role::Viewer)] {
                let _ = tree.admit_in_workspace(ws_id, &owner, member, role);
            }
            for (app, scope, verb, expiry) in [
                (
                    "Files",
                    Scope::FsPath("/var/workspace".to_string()),
                    Access::Write,
                    Expiry::Never,
                ),
                (
                    "Terminal",
                    Scope::Device("camera".to_string()),
                    Access::Read,
                    Expiry::EndOfSession,
                ),
                (
                    "Browser",
                    Scope::NetHost("*".to_string()),
                    Access::Read,
                    Expiry::AfterUse,
                ),
            ] {
                tree.grant_sandbox_in_workspace(
                    ws_id,
                    &owner,
                    CapOverride::grant(Subject::app(app), scope, verb).expiring(expiry),
                );
            }
            tree.grant_sandbox_in_workspace(
                ws_id,
                &owner,
                CapOverride::grant(
                    Subject::person("ada"),
                    Scope::FsPath("/var/workspace/design".to_string()),
                    Access::Write,
                ),
            );
        }
        self.with_chrome(|_| {});
    }

    /// Applies the named "scene" mutation set (the legacy `--scenes`
    /// catalogue) — a one-shot mutation; call on a fresh driver per scene.
    pub fn scene_setup(&mut self, name: &str) -> Result<(), String> {
        match name {
            "desktop" | "offline" => Self::scene_desktop(),
            "overview" => self.scene_overview(),
            "overview-shared" => self.scene_overview_shared(),
            "palette" => self.scene_palette(),
            "palette_app_query" => self.scene_palette_app_query(),
            "palette_in_app_query" => self.scene_palette_in_app_query(),
            "palette_records" => self.scene_palette_records(),
            "palette_no_results" => self.scene_palette_no_results(),
            "palette_typed" => self.scene_palette_typed(),
            "topbar-edit" => self.scene_topbar_edit(),
            "quick-settings" => self.scene_quick_settings(),
            "qs-edit" => self.scene_qs_edit(false),
            "qs-edit-drag" => self.scene_qs_edit(true),
            "qs-hardware-wired" => self.scene_qs_hardware_wired(),
            "qs-compact" => self.scene_qs_compact(),
            "qs-scrolled" => self.scene_qs_scrolled(),
            "qs-wifi-drillin" => self.scene_qs_wifi_drillin(),
            "qs-wifi-long-labels" => self.scene_qs_wifi_long_labels(),
            "qs-bt-drillin" => self.scene_qs_bt_drillin(),
            "qs-bt-long-labels" => self.scene_qs_bt_long_labels(),
            "qs-vpn-long-labels" => self.scene_qs_vpn_long_labels(),
            "power-menu" => self.scene_power_menu(false),
            "power-menu-keyboard" => self.scene_power_menu(true),
            "ws-settings-general" => self.scene_ws_settings_general(),
            "ws-settings-members" => self.scene_ws_settings_members(),
            "ws-settings-apps" => self.scene_ws_settings_apps(),
            "ws-settings-agents" => self.scene_ws_settings_agents(),
            "ws-settings-agent-access" => self.scene_ws_settings_agent_access(),
            "collaborators" => self.scene_collaborators(),
            "launchpad" => self.scene_launchpad(),
            "launchpad-notes-card" => self.scene_launchpad_notes_card(),
            "peer-sync" => self.scene_peer_sync(),
            "projection" => self.scene_projection(),
            "window-switcher" => self.scene_window_switcher(),
            "emoji-picker" => self.scene_emoji_picker(),
            "display-confirm" => self.scene_display_confirm(),
            "portal-permission" => self.scene_portal_permission(),
            "dictation-consent"
            | "dictation-listening"
            | "dictation-finishing"
            | "dictation-model-required" => {
                #[cfg(debug_assertions)]
                self.host_mut()
                    .seed_dictation_for_shots(name)
                    .map_err(str::to_owned)?;
                #[cfg(not(debug_assertions))]
                return Err("dictation shots require a debug build".into());
            }
            "agent-composer"
            | "agent-composer-typed"
            | "agent-composer-answer"
            | "agent-composer-working" => {
                #[cfg(debug_assertions)]
                self.host_mut()
                    .seed_agent_composer_for_shots(name)
                    .map_err(str::to_owned)?;
                #[cfg(not(debug_assertions))]
                return Err("agent composer shots require a debug build".into());
            }
            "room-activity" => self.scene_room_activity(),
            "room-activity-named" => self.scene_room_activity_named(),
            "room-activity-all" => self.scene_room_activity_all(),
            "ws-settings-permissions" => self.scene_ws_settings_permissions(),
            "notifications" => self.scene_notifications(),
            "notifications-one" => self.scene_notifications_one(),
            "notifications-dnd" => self.scene_notifications_dnd(),
            "multi-group" => self.scene_multi_group(),
            "maximized" => return self.scene_maximized(),
            "two-groups-focus" => self.scene_two_groups_focus(),
            "multi-strip" => self.scene_multi_strip(),
            other => return Err(format!("unknown scene '{other}'")),
        }
        Ok(())
    }

    /// Bare desktop; the driver's default state already renders it.
    fn scene_desktop() {}

    fn scene_overview(&mut self) {
        self.with_chrome(|st| st.overlays.overview = true);
        self.fb.host_mut().force_overview_settled(true);
    }

    /// The same panel with the room actually shared: members on this room
    /// and on Research, and two resources handed out.
    fn scene_overview_shared(&mut self) {
        self.seed_shared_room();
        self.with_chrome(|st| st.overlays.overview = true);
        self.fb.host_mut().force_overview_settled(true);
    }

    fn scene_palette(&mut self) {
        self.fb.host_mut().set_palette_visible(true);
        // Production opens the palette focused (`open_palette_with_scope`);
        // otherwise the focus ring is absent from the shot.
        self.fb.host_mut().set_palette_focus(true);
        // Settle the rising-edge open animation: frame 0 (`alpha == 0`)
        // is indistinguishable from bare desktop.
        self.fb.host_mut().force_palette_settled();
        // Live compositor populates results on every keystroke via
        // `handle_event`; scenes seed it explicitly instead.
        let root = self.root_obj.clone();
        self.fb
            .host_mut()
            .refresh_search_results_for_shots(&mut self.state, &root);
    }

    /// "calculator" is both an installed app's name and a hit in dozens of
    /// source paths — the app has to rank first.
    fn scene_palette_app_query(&mut self) {
        self.fb.host_mut().set_palette_visible(true);
        self.fb.host_mut().force_palette_settled();
        self.fb.host_mut().set_palette_focus(true);
        self.fb.host_mut().set_palette_query("calculator");
        let root = self.root_obj.clone();
        self.fb
            .host_mut()
            .refresh_search_results_for_shots(&mut self.state, &root);
    }

    /// Ctrl+K's half of search, scoped to the focused app; pumps apps first
    /// (as the live frame loop does) so the note's text is actually in the
    /// document to match against.
    fn scene_palette_in_app_query(&mut self) {
        let root = self.root_obj.clone();
        self.fb.pump_apps(&mut self.state, &root);
        self.with_chrome(|st| {
            st.search_scope = shell::shell::chrome_state::SearchScope::ActiveApp;
        });
        self.fb.host_mut().set_palette_visible(true);
        self.fb.host_mut().set_palette_focus(true);
        self.fb.host_mut().force_palette_settled();
        self.fb.host_mut().set_palette_query("tilo");
        self.fb
            .host_mut()
            .refresh_search_results_for_shots(&mut self.state, &root);
    }

    /// Seeded in other rooms deliberately — a record already in the active
    /// room needs no bringing.
    fn scene_palette_records(&mut self) {
        self.seed_records_in_other_rooms();
        self.fb.host_mut().set_palette_visible(true);
        self.fb.host_mut().set_palette_focus(true);
        self.fb.host_mut().force_palette_settled();
        self.fb.host_mut().set_palette_query("dune");
        let root = self.root_obj.clone();
        self.fb
            .host_mut()
            .refresh_search_results_for_shots(&mut self.state, &root);
    }

    /// Empty results are a state the palette spends real time in and the
    /// one most likely to be left looking broken.
    fn scene_palette_no_results(&mut self) {
        self.fb.host_mut().set_palette_visible(true);
        self.fb.host_mut().set_palette_focus(true);
        self.fb.host_mut().force_palette_settled();
        self.fb
            .host_mut()
            .set_palette_query("zzzz-nothing-matches-this");
        let root = self.root_obj.clone();
        self.fb
            .host_mut()
            .refresh_search_results_for_shots(&mut self.state, &root);
    }

    fn scene_palette_typed(&mut self) {
        self.fb.host_mut().set_palette_visible(true);
        self.fb.host_mut().set_palette_focus(true);
        self.fb.host_mut().force_palette_settled();
        self.fb.host_mut().set_palette_query("open");
        let root = self.root_obj.clone();
        self.fb
            .host_mut()
            .refresh_search_results_for_shots(&mut self.state, &root);
    }

    /// Open the production top-bar widget editor through its actual Ctrl+click
    /// gesture. Keeping this as a named still makes the otherwise hidden
    /// customization surface part of normal compact/wide visual review.
    fn scene_topbar_edit(&mut self) {
        // The centre-band hit rect is populated by draw, just as it is before
        // the first real pointer event in the compositor frame loop.
        let _ = self.paint();
        let (width, _) = self.viewport();
        self.set_modifiers(ModifiersState::CONTROL);
        self.click_at(Point::new(width / 2.0, 24.0));
        self.set_modifiers(ModifiersState::empty());
    }

    fn scene_quick_settings(&mut self) {
        self.with_chrome(|st| {
            st.overlays.quick_settings = true;
            st.quick_settings.radios.wifi_available = true;
            st.quick_settings.bluetooth_available = true;
        });
        self.fb.host_mut().force_quick_settings_settled();
    }

    fn scene_qs_edit(&mut self, dragging: bool) {
        self.scene_quick_settings();
        self.fb
            .host_mut()
            .force_quick_settings_edit_for_visual_test(dragging);
    }

    fn scene_qs_hardware_wired(&mut self) {
        self.with_chrome(|st| {
            st.overlays.quick_settings = true;
            st.quick_settings.radios.wifi_available = false;
            st.quick_settings.bluetooth_available = false;
            st.quick_settings.wired = Some(shell::shell::quick_settings::actions::WiredInfo {
                connected: true,
                device: Some("enp0s31f6".into()),
                ..Default::default()
            });
        });
        self.fb.host_mut().force_quick_settings_settled();
    }

    /// Quick Settings in a window too short for its page, scrolled to the
    /// end with the wheel: the tiles keep their size and the lower controls
    /// come into view (compare `quick-settings` at the same size).
    fn scene_qs_scrolled(&mut self) {
        self.scene_quick_settings();
        // The first paint sizes the page's scroll region, as the first
        // frame does before any pointer event reaches it.
        let _ = self.paint();
        let panel = self.fb.host().quick_settings_rect_for_tests();
        self.move_cursor(panel.center());
        self.mouse_wheel(0.0, -40.0);
    }

    fn scene_qs_compact(&mut self) {
        self.with_chrome(|st| {
            st.overlays.quick_settings = true;
            st.quick_settings.radios.wifi_available = false;
            st.quick_settings.bluetooth_available = false;
            st.quick_settings.wired = None;
        });
        self.fb.host_mut().force_quick_settings_settled();
    }

    fn scene_qs_wifi_drillin(&mut self) {
        self.with_chrome(|st| {
            st.overlays.quick_settings = true;
            st.quick_settings.push_level(QsLevel::Wifi);
        });
        self.fb.host_mut().force_quick_settings_settled();
    }

    /// Long SSIDs are normal in apartment blocks and enterprise networks;
    /// keep this deterministic scene so their text cannot regress across the
    /// trailing signal/connection indicators.
    fn scene_qs_wifi_long_labels(&mut self) {
        self.with_chrome(|st| {
            st.overlays.quick_settings = true;
            st.quick_settings.push_level(QsLevel::Wifi);
            st.quick_settings.wifi_networks = vec![
                shell::shell::quick_settings::WifiNetwork {
                    ssid: "Conference-room-very-long-5GHz-network-name".into(),
                    signal: 4,
                    secure: true,
                    security: os_actions::WifiSecurity::Wpa2,
                    connected: true,
                },
                shell::shell::quick_settings::WifiNetwork {
                    ssid: "Café Gäste-WLAN mit einem außergewöhnlich langen Namen".into(),
                    signal: 2,
                    secure: false,
                    security: os_actions::WifiSecurity::Open,
                    connected: false,
                },
            ];
        });
        self.fb.host_mut().force_quick_settings_settled();
    }

    fn scene_qs_bt_drillin(&mut self) {
        self.with_chrome(|st| {
            st.overlays.quick_settings = true;
            st.quick_settings.push_level(QsLevel::Bluetooth);
        });
        self.fb.host_mut().force_quick_settings_settled();
    }

    /// Device names from `BlueZ` are user-controlled. Keep a deterministic
    /// long-name scene so the battery and connected indicators remain a
    /// reserved trailing region instead of becoming an accidental text
    /// overlay at a compact panel width.
    fn scene_qs_bt_long_labels(&mut self) {
        self.with_chrome(|st| {
            st.overlays.quick_settings = true;
            st.quick_settings.push_level(QsLevel::Bluetooth);
            st.quick_settings.bt_devices = vec![
                shell::shell::quick_settings::BtDevice {
                    name: "Studio-headphones-with-a-very-long-pairing-name".into(),
                    address: "02:00:00:00:00:01".into(),
                    battery: 73,
                    connected: true,
                },
                shell::shell::quick_settings::BtDevice {
                    name: "Wohnzimmer-Lautsprecher außerordentlich lang".into(),
                    address: "02:00:00:00:00:02".into(),
                    battery: 18,
                    connected: false,
                },
            ];
        });
        self.fb.host_mut().force_quick_settings_settled();
    }

    /// VPN connection names are also supplied by `NetworkManager`. Exercise
    /// two rows with distinct active state, an intentionally long label and
    /// the trailing toggle region in the same render contract as Wi-Fi and
    /// Bluetooth.
    fn scene_qs_vpn_long_labels(&mut self) {
        self.with_chrome(|st| {
            st.overlays.quick_settings = true;
            st.quick_settings.push_level(QsLevel::Vpn);
            st.quick_settings.vpns = vec![
                os_actions::VpnConnectionInfo {
                    name: "Engineering production WireGuard tunnel Frankfurt".into(),
                    uuid: "shot-engineering-vpn-uuid".into(),
                    active: true,
                    kind: "wireguard".into(),
                },
                os_actions::VpnConnectionInfo {
                    name: "Personal research network with a long name".into(),
                    uuid: "shot-personal-vpn-uuid".into(),
                    active: false,
                    kind: "vpn".into(),
                },
            ];
        });
        self.fb.host_mut().force_quick_settings_settled();
    }

    fn scene_power_menu(&mut self, select_shutdown: bool) {
        self.fb
            .host_mut()
            .force_power_menu_for_visual_test(select_shutdown);
    }

    fn scene_ws_settings_general(&mut self) {
        self.with_chrome(|st| {
            st.overlays.ws_settings = true;
            st.ws_settings_tab = WsTab::General;
        });
        self.fb.host_mut().force_ws_settings_settled();
    }

    fn scene_ws_settings_members(&mut self) {
        self.seed_workspace_permissions();
        self.with_chrome(|st| {
            let owner_key = ed25519_dalek::SigningKey::from_bytes(&[41; 32]);
            let recipient_key = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
            let owner_public =
                collab_types::Ed25519PublicKey::from_bytes(owner_key.verifying_key().to_bytes());
            let recipient_public = collab_types::Ed25519PublicKey::from_bytes(
                recipient_key.verifying_key().to_bytes(),
            );
            let owner = hex::encode(owner_public.as_bytes());
            st.local_user_id.clone_from(&owner);
            if let Some(workspace) = st.tree_mut().desktop.active_workspace_mut() {
                workspace.set_owner(owner);
                workspace.collaborators.push(
                    shell::shell::state::Collaborator::new(
                        hex::encode(recipient_public.as_bytes()),
                        "Morgan",
                    )
                    .as_invited(),
                );
                workspace.admit(
                    hex::encode(recipient_public.as_bytes()),
                    tilo_action_policy::Role::Editor,
                );
                workspace.pending_ownership_transfer =
                    collab_types::OwnershipTransferProposalV1::new(
                        workspace.room.to_string(),
                        [23; 32],
                        owner_public,
                        recipient_public,
                        u64::MAX,
                        &owner_key,
                    )
                    .ok();
            }
            st.overlays.ws_settings = true;
            st.ws_settings_tab = WsTab::Members;
        });
        self.fb.host_mut().force_ws_settings_settled();
    }

    fn scene_ws_settings_apps(&mut self) {
        self.with_chrome(|st| {
            st.overlays.ws_settings = true;
            st.ws_settings_tab = WsTab::Apps;
        });
        self.fb.host_mut().force_ws_settings_settled();
    }

    fn scene_ws_settings_agents(&mut self) {
        self.seed_workspace_permissions();
        self.seed_agent_member_for_shots();
        self.with_chrome(|st| {
            st.overlays.ws_settings = true;
            st.ws_settings_tab = WsTab::Agents;
        });
        self.fb.host_mut().force_ws_settings_settled();
    }

    /// One agent's Allow / Ask first / Deny answers: Mail open with a
    /// changed answer, Files narrowed, and sending mail always allowed.
    fn scene_ws_settings_agent_access(&mut self) {
        use tilo_action_policy::agent_answers::AnswerScope;
        use tilo_action_policy::{ActionKind, Answer};
        self.seed_agent_member_for_shots();
        let mail = AnswerScope::app("so.tilo.mail");
        self.with_chrome(|st| {
            let actor = st.local_user_id.clone();
            {
                let mut tree = st.tree_mut();
                if let Some(ws) = tree.desktop.active_workspace_mut() {
                    tilo_desktop::workspace_settings::agent_access::set_agent_answers(
                        ws,
                        &actor,
                        "scribe",
                        &[
                            (mail.clone(), ActionKind::Undoable, Some(Answer::Ask)),
                            (
                                AnswerScope::app("so.tilo.file-explorer"),
                                ActionKind::Outside,
                                Some(Answer::Deny),
                            ),
                            (
                                AnswerScope::action("so.tilo.mail", "mail.mailbox", "send_message"),
                                ActionKind::Outside,
                                Some(Answer::Allow),
                            ),
                        ],
                    );
                }
            }
            st.overlays.ws_settings = true;
            st.ws_settings_tab = WsTab::Agents;
        });
        let host = self.fb.host_mut();
        host.open_agent_review_for_shots("scribe", Some(mail));
        host.force_ws_settings_settled();
    }

    /// The People roster (`collaborators_overlay.rs`): two members present,
    /// one invited, and — the local user owning the room — the Invite / Join
    /// band and the Workspace activity footer row.
    fn scene_collaborators(&mut self) {
        self.seed_shared_room();
        self.seed_agent_member_for_shots();
        self.with_chrome(|st| {
            {
                let mut tree = st.tree_mut();
                if let Some(ws) = tree.desktop.active_workspace_mut()
                    && !ws.collaborators.iter().any(|person| person.id == "cy")
                {
                    ws.collaborators
                        .push(shell::shell::state::Collaborator::new("cy", "Cy").as_invited());
                }
            }
            // The Invite / Join band is offered only while peer sync is on,
            // as it is on a booted desktop.
            st.peer_sync_enabled = true;
            st.overlays.collaborators = true;
            st.bump();
        });
    }

    /// The empty-workspace launchpad (`collaboration_launchpad.rs`): every
    /// window group of the active workspace is closed, which is the state
    /// that mounts the six cards (`shell::build_workspace_root`).
    fn scene_launchpad(&mut self) {
        self.with_chrome(|st| {
            {
                let mut tree = st.tree_mut();
                if let Some(ws) = tree.desktop.active_workspace_mut() {
                    for strip in &mut ws.strips {
                        strip.window_groups.clear();
                    }
                }
            }
            st.bump();
        });
        self.relayout();
    }

    /// The launchpad a moment after its Notes card is clicked: Notes is
    /// opening, and no launcher stands between the card and the app.
    fn scene_launchpad_notes_card(&mut self) {
        fn card_bounds(widget: &dyn Widget, card: &str) -> Option<Rect> {
            if widget.name() == "CollaborationLaunchpad" {
                return widget
                    .accessibility()?
                    .children
                    .into_iter()
                    .find_map(|node| {
                        let bounds = node.bounds?;
                        (node.name.as_deref() == Some(card))
                            .then(|| Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1))
                    });
            }
            widget
                .children()
                .into_iter()
                .find_map(|child| card_bounds(child, card))
        }
        self.scene_launchpad();
        let _ = self.paint_settled();
        let card = card_bounds(self.host(), "Notes").unwrap_or(Rect::ZERO);
        assert!(
            card.area() > 0.0,
            "launchpad-notes-card scene: the launchpad shows no Notes card"
        );
        self.click_at(card.center());
        // The live frame loop's pump takes the card's choice from the
        // document; `step_frames` alone only runs `update`.
        let root = self.root_obj.clone();
        self.fb.pump_apps(&mut self.state, &root);
        self.step_frames(12);
        self.fb.host_mut().force_palette_settled();
    }

    /// The invite card (`peer_sync_overlay.rs`), reached the way a person
    /// reaches it — the roster's Invite button through the production event
    /// path — so the host wires the invite context exactly as it would live.
    fn scene_peer_sync(&mut self) {
        self.scene_collaborators();
        let _ = self.paint_settled();
        let (invite, _) = self.host().collaborators_action_rects_for_tests();
        // A missing Invite button (peer sync off, or the local user may not
        // invite) fails the scene instead of quietly capturing the roster
        // under the name `peer-sync`.
        assert!(
            invite.area() > 0.0,
            "peer-sync scene: the roster offers no Invite button to open the invite card from"
        );
        self.click_at(invite.center());
        self.step_frames(2);
    }

    /// The Alt+Tab switcher (`window_switcher.rs`): Alt held past the
    /// reveal delay over the demo desktop's windows.
    fn scene_window_switcher(&mut self) {
        self.set_modifiers(ModifiersState::ALT);
        self.press_key(&Key::Named(NamedKey::Tab));
        self.step_frames(12);
    }

    /// The emoji picker with no focused field reporting a caret, so it opens
    /// centred on the display.
    fn scene_emoji_picker(&mut self) {
        self.host_mut().toggle_emoji_picker();
        self.step_frames(2);
    }

    /// The keep-or-revert prompt a layout tool's display change raises. No
    /// staged transaction carries a deadline here, so it shows no countdown.
    fn scene_display_confirm(&mut self) {
        self.host_mut().show_display_confirmation(1, None);
    }

    /// The display sheet (`projection_overlay.rs`, Win+P), settled
    /// past its 220 ms slide-in by stepping the frames a live session would.
    fn scene_projection(&mut self) {
        self.with_chrome(|st| {
            st.overlays.projection = true;
            st.bump();
        });
        self.step_frames(20);
    }

    /// A portal permission sheet: a sandboxed app asks for the camera and the
    /// request travels the production prompt queue into the compositor's
    /// prompt (`PermissionPrompt`, hosted by `overlay_host`).
    fn scene_portal_permission(&mut self) {
        use shell::portal::{FilePickerQueue, PortalCaller, PromptQueueHandle, PromptResolver};
        use shell::shell::state::{Capability, Expiry, WorkspaceId};
        let prompts = PromptQueueHandle::new(PromptResolver::Channel);
        self.host_mut()
            .set_portal_queues(prompts.clone(), FilePickerQueue::new());
        // The reply receiver is dropped: a screenshot has nobody to hand the
        // answer to, and the sheet does not care until it is answered.
        let _rx = prompts.enqueue(
            PortalCaller::new(WorkspaceId(1), "dev.tilo.camera"),
            Capability::Camera {
                scope: Expiry::EndOfSession,
            },
        );
        self.host_mut().drain_portal_prompts();
        self.host_mut().pump_permission_prompt();
        self.step_frames(2);
    }

    /// Drives the room through the production mirrors, not a seeded
    /// `HistoryEntry` — these are the same calls the portal, permission
    /// prompt, drop handler and settings modal make.
    fn scene_room_activity(&mut self) {
        use shell::shell::state::{Access, CapOverride, Scope, Subject};
        let handle = self.chrome_handle();
        let owner = {
            let st = handle.borrow();
            st.local_user_id.clone()
        };
        {
            let st = handle.borrow();
            let mut tree = st.tree_mut();
            tree.adopt_ownerless_workspaces(&owner);
            let ws_id = tree.desktop.active;
            tree.grant_sandbox_in_workspace(
                ws_id,
                &owner,
                CapOverride::grant(
                    Subject::app("Files"),
                    Scope::FsPath("/var/workspace/design".to_string()),
                    Access::Write,
                ),
            );
            let temp = tree.grant_sandbox_in_workspace(
                ws_id,
                &owner,
                CapOverride::grant(
                    Subject::app("Terminal"),
                    Scope::Device("camera".to_string()),
                    Access::Read,
                ),
            );
            tree.doc_append_tab_to_first_group_of_active_workspace(
                90_001,
                "Release notes".to_string(),
                b"{}".to_vec(),
            );
            tree.commit_room_change();
            if let Some(cap_id) = temp {
                tree.revoke_sandbox_in_workspace(ws_id, &owner, &cap_id);
            }
            if let Some(ws) = tree.desktop.active_workspace_mut() {
                ws.name = "Design review".to_string();
            }
            tree.seed_doc_from_cache();

            // A second person and a history spread over time: the
            // peer forks the document under its own actor, makes a
            // real change, and merges back — the shape a remote
            // replica's sync round has — with commits stamped
            // across the past fortnight so the author directory and
            // date filter both have something to exercise.
            let base: Vec<automerge::Change> = tree
                .export_changes_since(&[])
                .iter()
                .filter_map(|b| automerge::Change::try_from(b.as_slice()).ok())
                .collect();
            let mut peer = automerge::AutoCommit::new();
            peer.set_actor(automerge::ActorId::from("ada".as_bytes()));
            if peer.apply_incoming(base).is_ok() {
                const DAY: i64 = 86_400;
                let now = tilo_clock::unix_now_i64();
                for (days_ago, title) in
                    [(11_i64, "Q3 budget"), (5, "Onboarding notes"), (1, "Retro")]
                {
                    if peer_append_tab(&mut peer, title) {
                        tilo_resource::history::commit_stamped_at(
                            &mut peer,
                            now.saturating_sub(days_ago.saturating_mul(DAY)),
                        );
                    }
                }
                // Tab appends are layout and omitted from the explorer feed;
                // rename so Ada still authors a Change row and stays filterable.
                let mut ada_desktop = tree.desktop.clone();
                if let Some(ws) = ada_desktop.active_workspace_mut() {
                    ws.name = "Ada's desk".into();
                }
                tilo_desktop_model::desktop_doc::seed(&mut peer, &ada_desktop);
                tilo_resource::history::commit_stamped_at(&mut peer, now.saturating_sub(DAY));
                // Synchronise through the same dependency-closed delta path
                // used by ordinary peers. Merging the synthetic full replica
                // directly can leave a parked dependency that only surfaces
                // when replay asks Automerge to walk every historical head.
                let changes: Vec<Vec<u8>> = peer
                    .get_changes(&tree.doc.get_heads())
                    .into_iter()
                    .map(|change| change.raw_bytes().to_vec())
                    .collect();
                let _ = tree.apply_changes(&changes);
            }
        }
        self.with_chrome(|st| st.open_room_activity(None));
        self.fb.host_mut().force_room_activity_settled();
    }

    /// Deterministic scenario entry point for the two history zoom levels.
    /// It seeds the same collaborative changes as the legacy visual scene,
    /// then switches through the pane's public API rather than reaching into
    /// drawing state.
    pub fn seed_room_activity_for_shots(&mut self, explore: bool) {
        self.scene_room_activity();
        if !explore {
            use collab_types::ArtifactKind;
            use ui::tilo_actions::ActionContext;
            use ui::widget::history::{HistoryEntry, HistoryEntryId};

            let local = shell::shell::state::local_user_id();
            self.fb
                .host_mut()
                .room_activity_mut()
                .open_external_entry(HistoryEntry {
                    id: HistoryEntryId([0x51; 16]),
                    author: ui::history_source::author_for_identity(local.clone(), &local),
                    entry_kind: ui::widget::EntryKind::Text,
                    artifact_kind: ArtifactKind::File,
                    timestamp: std::time::SystemTime::now(),
                    summary: "Updated revenue assumptions".into(),
                    text_delta: Some((18, 4)),
                    diff_payload: None,
                    redaction: None,
                    context: Some(
                        ActionContext::for_resource(
                            "sheets:q2-finance",
                            "spreadsheet",
                            "Q2 finance report",
                        )
                        .with_app("org.tilo.Sheets", "Sheets")
                        .with_purpose("Preparing the board review"),
                    ),
                });

            // Catch up needs two adjacent, shared work sessions so expanding
            // either card proves the editable seam in the rendered product.
            // These names go through the same document write path as the UI;
            // the external resume hint above is deliberately not grouped.
            let handle = self.chrome_handle();
            let state = handle.borrow();
            let mut tree = state.tree_mut();
            let entries = self.fb.host().room_activity().entries().to_vec();
            #[expect(
                clippy::indexing_slicing,
                reason = "entries.len() >= 4, so 0 <= first_end < split <= last < entries.len()"
            )]
            if entries.len() >= 4 {
                let split = entries.len() / 2;
                if split >= 2 && entries.len().saturating_sub(split) >= 2 {
                    let first_end = split.saturating_sub(1);
                    let last = entries.len().saturating_sub(1);
                    let pane = self.fb.host_mut().room_activity_mut();
                    pane.apply_name(
                        &mut *tree,
                        &ui::GroupNameRequest {
                            record_id: None,
                            start: entries[0].id,
                            end: entries[first_end].id,
                            label: "Write Q3 financial report".to_string(),
                        },
                    );
                    pane.apply_name(
                        &mut *tree,
                        &ui::GroupNameRequest {
                            record_id: None,
                            start: entries[split].id,
                            end: entries[last].id,
                            label: "Onboard Sarah".to_string(),
                        },
                    );
                }
            }
            self.fb
                .host_mut()
                .room_activity_mut()
                .prepare_away_recap_for_shots(&mut *tree);
            seed_catch_up_tasks(&mut tree, &local);
            drop(tree);
            drop(state);
        }
        self.fb.host_mut().room_activity_mut().set_view(if explore {
            shell::shell::room_activity::ActivityView::Explore
        } else {
            shell::shell::room_activity::ActivityView::CatchUp
        });
        self.fb.host_mut().force_room_activity_settled();
        // Headless snapshots do not render, while this immediate-mode sheet
        // derives its keyboard order from the controls it actually paints.
        // Build one CPU-side scene (no adapter/GPU involved) so keyboard
        // actions exercise the same rendered control set in every mode.
        let _ = self.fb.paint();
    }

    pub fn seed_automatic_work_sessions_for_shots(&mut self) {
        let handle = self.chrome_handle();
        {
            let state = handle.borrow();
            let mut tree = state.tree_mut();
            let workspace = tree.desktop.active;
            for (actor, group_id, group_label, action) in [
                (
                    "Xaver",
                    "finance",
                    "Write Q3 financial report",
                    "Draft totals",
                ),
                (
                    "Quinten",
                    "finance",
                    "Write Q3 financial report",
                    "Check forecast",
                ),
                (
                    "Sarah",
                    "finance",
                    "Write Q3 financial report",
                    "Reconcile actuals",
                ),
                (
                    "Ada",
                    "finance",
                    "Write Q3 financial report",
                    "Review variance",
                ),
                ("Sarah", "onboarding", "Onboard Sarah", "Open welcome notes"),
                ("Ada", "onboarding", "Onboard Sarah", "Assign mentor"),
            ] {
                let context = tilo_actions::ActionContext::for_resource(
                    format!("shots:{group_id}"),
                    "document",
                    group_label,
                )
                .with_app("org.tilo.Writer", "Writer")
                .with_described(tilo_actions::Described::new(tilo_actions::Did::verb(
                    tilo_actions::ActionKey::new(
                        "org.tilo.Writer",
                        "document",
                        tilo_actions::entry::kebab(action),
                    ),
                    [],
                )))
                .with_group(group_id, group_label, "work-session");
                let _ = tree.append_workspace_effect_receipt_for_shots(
                    workspace,
                    tilo_actions::InvocationId::new(),
                    actor,
                    context,
                    tilo_actions::EffectReceiptStatus::Succeeded,
                );
            }
        }
        self.with_chrome(|state| state.open_room_activity(None));
        self.fb
            .host_mut()
            .room_activity_mut()
            .set_view(shell::shell::room_activity::ActivityView::CatchUp);
        self.fb.host_mut().force_room_activity_settled();
        let _ = self.fb.paint();
    }

    pub fn expand_automatic_work_session_for_shots(&mut self) -> bool {
        let expanded = self
            .fb
            .host_mut()
            .room_activity_mut()
            .expand_work_session_for_shots(0);
        let _ = self.fb.paint();
        expanded
    }

    pub fn focus_first_room_activity_work_session_for_shots(&mut self) -> bool {
        self.fb
            .host_mut()
            .room_activity_mut()
            .focus_first_work_session_for_shots()
    }

    pub fn correct_automatic_work_session_for_shots(&mut self) -> bool {
        let handle = self.chrome_handle();
        let state = handle.borrow();
        let mut tree = state.tree_mut();
        let corrected = self
            .fb
            .host_mut()
            .room_activity_mut()
            .move_work_session_boundary_for_shots(&mut *tree, 1, 0, true);
        drop(tree);
        drop(state);
        self.fb.host_mut().force_room_activity_settled();
        let _ = self.fb.paint();
        corrected
    }

    pub fn show_room_activity_replay_for_shots(&mut self) {
        self.fb
            .host_mut()
            .room_activity_mut()
            .set_view(shell::shell::room_activity::ActivityView::Replay);
        self.fb.host_mut().force_room_activity_settled();
        let _ = self.fb.paint();
    }

    /// Select Ada in the contributor controls using the history surface's
    /// public filter API. Returns false if the seeded remote replica failed
    /// to produce an authored change, rather than emitting a misleading PNG.
    pub fn filter_room_activity_to_ada_for_shots(&mut self) -> bool {
        let ada = self
            .fb
            .host()
            .room_activity()
            .contributors()
            .iter()
            .find(|name| name.eq_ignore_ascii_case("ada"))
            .cloned();
        let Some(ada) = ada else {
            return false;
        };
        self.fb
            .host_mut()
            .room_activity_mut()
            .set_author_filter(Some(ada));
        self.fb.host_mut().force_room_activity_settled();
        let _ = self.fb.paint();
        true
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the shell fixture owns long-lived permits by design and lives for the whole run"
    )]
    pub fn select_room_activity_entry(&mut self, index: usize) {
        let (start, len) = {
            let activity = self.fb.host().room_activity();
            let entries = activity.timeline().entries();
            let start = activity
                .visible_entry(index)
                .and_then(|visible| entries.iter().position(|entry| entry.id == visible.id))
                .unwrap_or(index);
            (start, entries.len())
        };
        for candidate in start..len {
            self.fb
                .host_mut()
                .room_activity_mut()
                .timeline_mut()
                .set_selection(Some((candidate, candidate)));
            let handle = self.chrome_handle();
            let state = handle.borrow();
            let mut tree = state.tree_mut();
            if self
                .fb
                .host()
                .room_activity()
                .compare_selection(&mut *tree, None)
                .is_some()
            {
                break;
            }
        }
        let _ = self.fb.paint();
    }

    /// Two runs named, one by the peer under its own actor, so the band
    /// reads "named by" someone other than the viewer. Both go in through
    /// the pane's own write path (the `[Name]` chip's call).
    fn scene_room_activity_named(&mut self) {
        self.scene_room_activity();
        let handle = self.chrome_handle();
        let st = handle.borrow();
        let mut tree = st.tree_mut();
        let entries = self.fb.host().room_activity().entries().to_vec();
        if entries.len() >= 6 {
            // The peer's own run: it named it before syncing.
            let mut peer = tree.doc.fork();
            peer.set_actor(automerge::ActorId::from("ada".as_bytes()));
            let end = entries.len().saturating_sub(1);
            let start = entries.len().saturating_sub(3);
            if let (Some(start_entry), Some(end_entry)) = (entries.get(start), entries.get(end)) {
                ui::history_groups::name_range(
                    &mut peer,
                    &start_entry.id,
                    Some(&end_entry.id),
                    "Reading week",
                );
                tilo_resource::history::commit_stamped(&mut peer);
                let _ = tree.doc.merge_incoming(&mut peer);
            }

            if let (Some(first), Some(fourth)) = (entries.first(), entries.get(3)) {
                let pane = self.fb.host_mut().room_activity_mut();
                pane.apply_name(
                    &mut *tree,
                    &ui::GroupNameRequest {
                        record_id: None,
                        start: first.id,
                        end: fourth.id,
                        label: "Set up the room".to_string(),
                    },
                );
            }
        }
        drop(tree);
        drop(st);
        self.fb.host_mut().force_room_activity_settled();
    }

    /// Widened to the whole tree; every row now says which workspace it
    /// happened in.
    fn scene_room_activity_all(&mut self) {
        self.scene_room_activity();
        let handle = self.chrome_handle();
        let st = handle.borrow();
        let tree = st.tree();
        self.fb.host_mut().room_activity_mut().set_scope(
            shell::shell::room_activity::RoomScope::AllWorkspaces,
            &*tree,
        );
    }

    fn scene_ws_settings_permissions(&mut self) {
        self.seed_workspace_permissions();
        self.with_chrome(|st| {
            st.overlays.ws_settings = true;
            st.ws_settings_tab = WsTab::Permissions;
        });
        self.fb.host_mut().force_ws_settings_settled();
    }

    fn scene_notifications(&mut self) {
        use tilo_desktop::notifications::{Notification, NotificationSeverity};
        // Backdate `created_at` past the slide-in window: at t=0 a
        // toast's `enter` progress is ~0 and `slide_dx == TOAST_WIDTH`
        // pushes it entirely off-screen-right.
        let settled_at = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap_or_else(Instant::now);
        // Over a window with no document of its own, so no save status
        // competes with the banners.
        let _ = self.focus_tab_titled("Files");
        self.with_chrome(|st| {
            // What a person meets over their work: a collaborator who
            // needs them, something shared with them, and a failure they
            // did not cause. Long dismiss windows keep the still-image
            // scene's stack on screen.
            st.push_notification_full(
                Notification::new("Ada mentioned you", "“Can you look at the launch notes?”")
                    .with_app_name("Chat")
                    .with_from("Ada", "peer:ada")
                    .with_dismiss_after(Duration::from_secs(60))
                    .with_created_at(settled_at),
            );
            st.push_notification_full(
                Notification::new("Lena shared “Q3 plan”", "You can edit it.")
                    .with_app_name("Files")
                    .with_from("Lena", "peer:lena")
                    .with_dismiss_after(Duration::from_secs(60))
                    .with_created_at(settled_at),
            );
            st.push_notification_full(
                Notification::new(
                    "Backup paused",
                    "The backup drive was disconnected. Reconnect it to continue.",
                )
                .with_app_name("Backups")
                .with_severity(NotificationSeverity::Warning)
                .with_dismiss_after(Duration::from_secs(60))
                .with_created_at(settled_at),
            );
        });
    }

    fn scene_notifications_one(&mut self) {
        use tilo_desktop::notifications::{Notification, NotificationSeverity};
        let settled_at = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap_or_else(Instant::now);
        self.with_chrome(|st| {
            st.push_notification_full(
                Notification::new("Build finished", "All 524 tests passed")
                    .with_severity(NotificationSeverity::Info)
                    .with_dismiss_after(Duration::from_secs(60))
                    .with_created_at(settled_at),
            );
        });
    }

    fn scene_notifications_dnd(&mut self) {
        use tilo_desktop::notifications::{Notification, NotificationSeverity};
        let settled_at = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap_or_else(Instant::now);
        self.with_chrome(|st| {
            // One banner posted *before* DND is engaged stays on
            // screen (DND only gates new posts).
            st.push_notification_full(
                Notification::new("Meeting in 10 minutes", "Standup — Research workspace")
                    .with_severity(NotificationSeverity::Info)
                    .with_dismiss_after(Duration::from_secs(60))
                    .with_created_at(settled_at),
            );
            // Engage Do-Not-Disturb, then post two more — these are
            // suppressed from the banner surface and held in
            // history, so the DND pill reads "… · 2 held".
            st.set_dnd(true);
            st.push_notification_full(
                Notification::new("Slow network", "Peer sync delayed")
                    .with_severity(NotificationSeverity::Warning),
            );
            st.push_notification_full(
                Notification::new("New mail", "3 unread in Inbox")
                    .with_severity(NotificationSeverity::Info),
            );
        });
    }

    /// The only scene that exercises inter-group chrome (resize handles,
    /// active-group outlines, gap drop-zones) — every other scene operates
    /// on the single-group bootstrap.
    fn scene_multi_group(&mut self) {
        use shell::shell::state::{Tab, TileContent, WindowGroup, WindowWidth};
        self.with_chrome(|st| {
            let mut tree = st.tree_mut();
            if let Some(ws) = tree.desktop.active_workspace_mut()
                && let Some(strip) = ws.strips.first_mut()
            {
                // IDs picked far above the bootstrap range so
                // they don't collide with persisted state.
                for (gid, tid, label) in
                    [(9001u64, 9101u64, "Stub-A"), (9002u64, 9102u64, "Stub-B")]
                {
                    let stub_tab = Tab {
                        id: tid,
                        title: label.to_string(),
                        tile: TileContent::Stub {
                            kind: shell::shell::state::AppKind::Settings,
                            label: label.to_string(),
                        },
                    };
                    let group = WindowGroup {
                        id: gid,
                        tabs: vec![stub_tab],
                        active_tab: tid,
                        width: WindowWidth::Normal,
                        maximized: false,
                        focus_history: Vec::new(),
                    };
                    strip.window_groups.push(group);
                }
            }
            drop(tree);
            st.bump();
        });
    }

    /// One window filling the strip. Maximizing used to stroke a 2 px
    /// accent rectangle around the group's bounding box — sharp corners a
    /// pixel outside a rounded card — and there was no scene that showed
    /// it, which is how it survived to be reported from a live session
    /// instead of from a review.
    fn scene_maximized(&mut self) -> Result<(), String> {
        let mut target = None;
        self.with_chrome(|st| {
            {
                let mut tree = st.tree_mut();
                if let Some(strip) = tree
                    .desktop
                    .active_workspace_mut()
                    .and_then(|ws| ws.strips.first_mut())
                {
                    let sid = strip.id;
                    if let Some(g) = strip.window_groups.first_mut() {
                        g.maximized = true;
                        target = Some((sid, g.id));
                    }
                }
            }
            if let Some((_, gid)) = target {
                st.set_maximized_group(Some(gid));
            }
            st.bump();
        });
        let Some((strip_id, group_id)) = target else {
            return Err("maximized: active workspace has no window group".into());
        };
        // The widget tree is built before `scene_setup` runs, so
        // the flag has to land on the live group as well — the
        // same pair of writes the maximize click performs.
        if let Some(row) = self.host_mut().group_row_mut_for_tests(strip_id) {
            for g in &mut row.groups {
                if g.id == group_id {
                    g.maximized = true;
                }
            }
        }
        Ok(())
    }

    /// Two half-width windows, focus on the right-hand one — nothing about
    /// the layout alone says which takes the next keystroke.
    fn scene_two_groups_focus(&mut self) {
        use shell::shell::state::{Tab, TileContent, WindowGroup, WindowWidth};
        let half = WindowWidth::Fraction(500);
        let mut placed = None;
        self.with_chrome(|st| {
            let mut tree = st.tree_mut();
            let mut sibling = None;
            let mut strip_id = None;
            if let Some(strip) = tree
                .desktop
                .active_workspace_mut()
                .and_then(|ws| ws.strips.first_mut())
            {
                for g in &mut strip.window_groups {
                    g.width = half;
                }
                let gid = 9101u64;
                let tid = 9111u64;
                strip.window_groups.push(WindowGroup {
                    id: gid,
                    tabs: vec![Tab {
                        id: tid,
                        title: "Settings".to_string(),
                        tile: TileContent::Stub {
                            kind: shell::shell::state::AppKind::Settings,
                            label: "Settings".to_string(),
                        },
                    }],
                    active_tab: tid,
                    width: half,
                    maximized: false,
                    focus_history: Vec::new(),
                });
                strip_id = Some(strip.id);
                sibling = Some(gid);
            }
            // Focus the newly-added right-hand window, so the
            // scene shows the signal moving off the default.
            if let (Some(sid), Some(gid)) = (strip_id, sibling) {
                tree.desktop.set_active_group(sid, gid);
                placed = Some(sid);
            }
            drop(tree);
            st.bump();
        });
        // Widths only reach the widget tree at construction, which
        // ran before this; halve the live groups too or the scene
        // renders one full-width window with its sibling off-edge.
        if let Some(sid) = placed
            && let Some(row) = self.host_mut().group_row_mut_for_tests(sid)
        {
            for g in &mut row.groups {
                g.set_width(half);
            }
        }
        // The pushed group latches its 420 ms arrival fade off the
        // wall clock the moment it's spliced in; paint once so it
        // becomes a widget, then wait the curve out before the
        // caller's `paint_settled` captures the resting state.
        let _ = self.paint_settled();
        std::thread::sleep(std::time::Duration::from_millis(600));
    }

    /// Three strips in the active workspace, focus on the middle one — the
    /// only scene exercising the strip ribbon.
    fn scene_multi_strip(&mut self) {
        use shell::shell::state::{
            AppKind, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth,
        };
        self.with_chrome(|st| {
            let mut focus = None;
            {
                let mut tree = st.tree_mut();
                if let Some(ws) = tree.desktop.active_workspace_mut() {
                    for (n, name) in ["Build", "Review"].iter().enumerate() {
                        let n = n as u64;
                        let sid = StripId(9200u64.saturating_add(n));
                        let mut strip = Strip::new(sid, (*name).to_string());
                        let gid = 9300u64.saturating_add(n);
                        let tid = 9400u64.saturating_add(n);
                        strip.window_groups.push(WindowGroup {
                            id: gid,
                            tabs: vec![Tab {
                                id: tid,
                                title: (*name).to_string(),
                                tile: TileContent::Stub {
                                    kind: AppKind::Settings,
                                    label: (*name).to_string(),
                                },
                            }],
                            active_tab: tid,
                            width: WindowWidth::Normal,
                            maximized: false,
                            focus_history: Vec::new(),
                        });
                        ws.strips.push(strip);
                    }
                    focus = ws.strips.get(1).map(|s| s.id).map(|sid| (ws.id, sid));
                }
                if let Some((ws_id, sid)) = focus {
                    tree.desktop.active_strip.insert(ws_id, sid);
                }
            }
            st.bump();
        });
    }

    /// Catalogue of scene names supported by `scene_setup`. Used by the
    /// legacy `--scenes` flag for default expansion + validation.
    pub const ALL_SCENES: &'static [&'static str] = &[
        "desktop",
        "offline",
        "multi-group",
        "multi-strip",
        "maximized",
        "two-groups-focus",
        "overview",
        "overview-shared",
        "palette",
        "palette_typed",
        "palette_in_app_query",
        "palette_app_query",
        "palette_records",
        "palette_no_results",
        "topbar-edit",
        "quick-settings",
        "qs-edit",
        "qs-edit-drag",
        "qs-hardware-wired",
        "qs-compact",
        "qs-scrolled",
        "qs-wifi-drillin",
        "qs-wifi-long-labels",
        "qs-bt-drillin",
        "qs-bt-long-labels",
        "qs-vpn-long-labels",
        "power-menu",
        "power-menu-keyboard",
        "ws-settings-general",
        "ws-settings-members",
        "ws-settings-permissions",
        "ws-settings-apps",
        "ws-settings-agents",
        "ws-settings-agent-access",
        "collaborators",
        "launchpad",
        "launchpad-notes-card",
        "peer-sync",
        "projection",
        "window-switcher",
        "emoji-picker",
        "display-confirm",
        "portal-permission",
        "dictation-consent",
        "dictation-listening",
        "dictation-finishing",
        "dictation-model-required",
        "agent-composer",
        "agent-composer-typed",
        "agent-composer-answer",
        "agent-composer-working",
        "notifications",
        "notifications-one",
        "notifications-dnd",
        "room-activity",
        "room-activity-all",
        "room-activity-named",
    ];

    /// Resolve a "named" UI target (button, pill, tab) to its on-screen
    /// rect in viewport-local coordinates. `None` means `Rect::ZERO` —
    /// generally "not drawn yet" (call `paint` once first).
    ///
    /// Checks a hand-curated table first (`"add_workspace"`,
    /// `"ws_settings_tab[general|members|permissions]"` — keys needing
    /// chrome-internal accessors no tree walk reaches), then falls back to
    /// the widget-tree registry keyed by `test_id` or path
    /// (`ShellOverlayHost/TopBarChrome`).
    ///
    /// # A client tile has two names, and only one is the one you guessed
    ///
    /// A foreign surface is `foreign-surface-<id>`, and once bound to a
    /// tab also answers to `foreign-surface-<id>[<tab_id>]`. Both forms
    /// exist within one session, so look it up **by prefix on the surface
    /// id**, never the bare name:
    ///
    /// ```ignore
    /// let stem = format!("foreign-surface-{surface_id}");
    /// let named = names.iter().find(|n| **n == stem || n.starts_with(&format!("{stem}[")));
    /// ```
    ///
    /// Getting this wrong returns `None` — indistinguishable from "not
    /// open" — for a window plainly on screen, so a harness that declines
    /// to click blames whatever assertion runs next.
    pub fn resolve_named_rect(&self, name: &str) -> Option<Rect> {
        let host = self.fb.host();
        let curated = match name {
            "add_workspace" => Some(host.add_workspace_rect_for_tests()),
            "ws_settings_tab[general]" => Some(host.ws_settings_tab_pill_rect(WsTab::General)),
            "ws_settings_tab[members]" => Some(host.ws_settings_tab_pill_rect(WsTab::Members)),
            "ws_settings_tab[permissions]" => {
                Some(host.ws_settings_tab_pill_rect(WsTab::Permissions))
            }
            "ws_settings_tab[apps]" => Some(host.ws_settings_tab_pill_rect(WsTab::Apps)),
            _ => None,
        };
        if let Some(r) = curated
            && r.area() > 0.0
        {
            return Some(r);
        }
        // Fall through to the registry — the curated key may have
        // missed because its prerequisite chrome state isn't set
        // (overview closed, ws-settings closed). Registry can still
        // succeed for an opt-in `test_id` regardless.
        let registry = self.widget_registry();
        registry.get(name).copied().filter(|r| r.area() > 0.0)
    }

    /// Snapshot of every addressable widget's rect, keyed by `test_id()`
    /// or, failing that, a path key `Parent/Child[index]`.
    ///
    /// Rebuilt on every call — no hidden cache — so re-fetch after each
    /// `paint` in a multi-action scenario. Widgets at `Rect::ZERO` are
    /// still registered; `resolve_named_rect` filters those out.
    pub fn widget_registry(&self) -> HashMap<String, Rect> {
        // Delegate to the shared walk in `compositor::frame` so the
        // out-of-process `compositor::test_control` and in-process
        // ShellDriver see exactly the same target set.
        self.fb.widget_registry()
    }

    /// Centre of a named rect — convenience for the scenario engine.
    pub fn resolve_named_centre(&self, name: &str) -> Option<Point> {
        self.resolve_named_rect(name).map(|r| r.center())
    }
}

// ---------- helpers ----------

// `walk_widget` lives in `compositor::frame` so the out-of-process
// `compositor::test_control` and the in-process ShellDriver share one
// implementation. See `crates/session/shell/src/compositor/frame.rs`.

pub fn key_press_event(key: &Key) -> WindowEvent {
    let mut ev = ui::synthetic_input::key_event(key.clone(), None, ElementState::Pressed);
    // `text` is `Option<SmolStr>` — for ASCII characters we mirror what
    // winit normally fills so the palette's character-input branch
    // sees the same glyph it would on real hardware.
    ev.text = if let Key::Character(s) = key {
        Some(s.clone())
    } else {
        None
    };
    ui::synthetic_input::keyboard_input(ev)
}

// =====================================================================
// Tests
// =====================================================================

#[allow(
    unused_imports,
    reason = "The native WorkspaceExt methods are selected by host-specific workspace code and test configurations."
)]
use shell::shell::state::WorkspaceExt as _;

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
mod tests {
    use super::*;

    #[test]
    fn driver_scene_setup_covers_the_catalogue_and_keeps_pending_invites_out_of_presence() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_UI_DEBUG", "1");
        // Phase `driver_scene_setup_covers_catalogue`.
        // Smoke for `scene_setup` — drive every named scene and verify it
        // returns Ok, then verify a fresh driver returns Err for an
        // unknown name.
        {
            for &name in ShellDriver::ALL_SCENES {
                let mut d = ShellDriver::new((1600, 900));
                d.scene_setup(name)
                    .unwrap_or_else(|e| panic!("scene_setup({name}) failed: {e}"));
                let _ = d.paint_settled();
            }
            let mut d = ShellDriver::new((1600, 900));
            assert!(d.scene_setup("nope").is_err());
        }
        // Phase `collaboration_fixture_keeps_pending_invite_out_of_live_presence`.
        {
            let mut d = ShellDriver::new((1600, 900));
            d.seed_invite_pending_for_shots();
            let invited = d
                .chrome()
                .tree()
                .desktop
                .active_workspace()
                .and_then(|ws| ws.collaborators.iter().find(|person| person.id == "ada"))
                .cloned()
                .expect("pending invitation creates Ada's durable row");
            assert!(invited.invited);
            assert_eq!(invited.active_tab, None);

            assert!(d.admit_invited_collaborator_for_shots());
            let live = d
                .chrome()
                .tree()
                .desktop
                .active_workspace()
                .and_then(|ws| ws.collaborators.iter().find(|person| person.id == "ada"))
                .cloned()
                .expect("acceptance retains the same identity");
            assert!(!live.invited);
            let notes = d.chrome().tree().desktop.active_workspace().and_then(|ws| {
                ws.strips
                    .iter()
                    .flat_map(|strip| &strip.window_groups)
                    .flat_map(|group| &group.tabs)
                    .find(|tab| tab.title == "Notes")
                    .map(|tab| tab.id)
            });
            assert_eq!(live.active_tab, notes);
        }
    }

    #[test]
    fn driver_dispatches_steps_and_resolves_named_targets() {
        // Phase `driver_dispatch_routes_click_to_chrome`.
        // Drive Cmd+Tab through `dispatch` and verify the chrome's
        // `overlays.overview` flag flips. Mirrors the live binary's
        // hotkey path through real `handle_event` dispatch on the host
        // — exercises the full `dispatch → ShellOverlayHost::handle_event
        // → chrome.borrow_mut()` chain.
        {
            let mut d = ShellDriver::new((1600, 900));
            let _ = d.paint();
            assert!(!d.chrome().overlays.overview);
            d.set_modifiers(ModifiersState::SUPER);
            d.press_key(&Key::Named(NamedKey::Tab));
            assert!(
                d.chrome().overlays.overview,
                "Cmd+Tab should toggle the overview overlay on through driver dispatch"
            );
        }
        // Phase `driver_step_frames_advances_palette_fade`.
        // Toggle the palette open via the public host API, step a few
        // frames so the palette open animation can register, then assert
        // the palette is still visible. Pinned smoke for `step_frames`
        // not panicking and not nuking visibility flags.
        {
            let mut d = ShellDriver::new((1600, 900));
            assert!(!d.host().palette_visible());
            d.host_mut().set_palette_visible(true);
            d.step_frames(6);
            assert!(
                d.host().palette_visible(),
                "step_frames should advance animation timers without flipping visibility"
            );
            let _ = d.paint();
        }
        // Phase `driver_resolve_named_rect_for_overview_add_workspace`.
        // Paint the overview, then verify the named-rect resolver can pick
        // out the "+ Add workspace" tile (which `draw_overview` populates
        // each visible frame).
        {
            let mut d = ShellDriver::new((1600, 900));
            d.with_chrome(|st| st.overlays.overview = true);
            d.host_mut().force_overview_settled(true);
            let _ = d.paint_settled();
            let rect = d.resolve_named_rect("add_workspace");
            assert!(
                rect.is_some(),
                "overview should populate the add_workspace rect after a settled paint"
            );
        }
        // Phase `widget_registry_finds_topbar_buttons`.
        // `widget_registry` must enumerate the live widget tree after a
        // paint, surfacing a `TopBarChrome` entry from the bootstrap default —
        // pinned so a regression in `ShellOverlayHost::children()` fails here
        // rather than silently returning an empty registry.
        {
            let mut d = ShellDriver::new((1600, 900));
            let _ = d.paint();
            d.relayout();
            let _ = d.paint();
            let registry = d.widget_registry();
            assert!(
                !registry.is_empty(),
                "registry must not be empty after paint"
            );
            assert!(
                registry.keys().any(|k| k.contains("ShellOverlayHost")),
                "registry should contain ShellOverlayHost; keys={:?}",
                registry.keys().collect::<Vec<_>>()
            );
            assert!(
                registry.keys().any(|k| k.contains("TopBarChrome")),
                "registry should contain TopBarChrome; keys={:?}",
                registry.keys().collect::<Vec<_>>()
            );
        }
        // Phase `resolve_named_rect_falls_back_to_registry`.
        // `resolve_named_rect` must fall through to the widget tree when the
        // hand-curated table misses: `command_palette_input` isn't in that
        // table, but the `CommandPalette` widget opts in via `test_id()`.
        {
            let mut d = ShellDriver::new((1600, 900));
            // Open the palette — drives the same code path scenarios use,
            // so the palette's rect gets populated on the subsequent paint.
            d.host_mut().set_palette_visible(true);
            let _ = d.paint();
            d.relayout();
            let _ = d.paint();

            // Sanity: hand-curated table doesn't know this key.
            // (No direct assertion — we can't call the private inner match —
            // but the registry-fallback path is what makes the next assertion
            // pass; if a future edit accidentally adds this key to the
            // curated table, the test still passes.)
            let rect = d.resolve_named_rect("command_palette_input");
            assert!(
                rect.is_some(),
                "command_palette_input must resolve through the registry once \
                 the palette is visible; registry keys: {:?}",
                d.widget_registry().keys().collect::<Vec<_>>()
            );
            let r = rect.unwrap();
            assert!(
                r.area() > 0.0,
                "resolved command_palette_input rect must be non-zero; got {r:?}"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn driver_tab_drag_and_detach_scope_to_the_right_group() {
        // Phase `driver_drag_tab_out_creates_sibling_group`.
        // Dragging the second tab pill (Files) out of the bootstrap default's
        // 4-tab Wide group, past its right edge into the empty strip area,
        // must produce TWO groups (3-tab source + 1-tab sibling), not a
        // replace. Reads the live `tab_rects()` off the bar so the press
        // lands on the pill regardless of layout drift.
        {
            let mut d = ShellDriver::new((1600, 900));
            let _ = d.paint(); // seed any draw-only caches the layout missed
            d.relayout();

            // Snapshot pre-drag layout: bootstrap default seeds one strip
            // with one Wide group carrying Notes/Files/Terminal/Browser.
            let strip_id = {
                let st = d.chrome();
                let tree = st.tree();
                let ws_id = tree.desktop.active;
                *tree.desktop.active_strip.get(&ws_id).expect("active strip")
            };
            let pre = d.host().groups_per_strip();
            let pre_strip = pre
                .iter()
                .find(|(s, _)| *s == strip_id)
                .expect("strip in groups_per_strip");
            assert_eq!(
                pre_strip.1.len(),
                1,
                "precondition: bootstrap default has exactly one group"
            );
            assert_eq!(
                pre_strip.1[0].1.len(),
                4,
                "precondition: that group has four tabs"
            );

            // Read the second pill's rect off the live bar so the press
            // hit-tests against the pill the layout actually painted.
            let (from, to) = {
                let group = d
                    .host_mut()
                    .group_at_for_tests(strip_id, 0)
                    .expect("group at idx 0");
                let group_rect = group.get_rect();
                let rects = group.tab_rects().to_vec();
                assert_eq!(rects.len(), 4, "bar must have 4 pill rects after layout");
                // Pill 1 (Files) — non-active source.
                let pill = rects[1];
                let from = pill.center();
                // Release inside the generous right body-edge band. The bootstrap
                // group fills the viewport, so an apparent gap beyond x1 is not a
                // reachable pointer target.
                let to = Point::new(
                    group_rect.x1 - 24.0,
                    f64::midpoint(group.bar_rect().y1, group_rect.y1),
                );
                (from, to)
            };

            d.drag(from, to, 16);
            d.relayout();

            let post = d.host().groups_per_strip();
            let post_strip = post
                .iter()
                .find(|(s, _)| *s == strip_id)
                .expect("strip still present after drag");
            assert_eq!(
                post_strip.1.len(),
                2,
                "drag-out from a 4-tab group must produce exactly two groups \
                 (3-tab source + 1-tab sibling), not one (the 'replace' bug). \
                 post = {:?}",
                post_strip.1,
            );
            let counts: Vec<usize> = post_strip.1.iter().map(|(_, t)| t.len()).collect();
            assert!(
                counts.contains(&3) && counts.contains(&1),
                "expected one group with 3 tabs and one with 1 tab; got {counts:?}",
            );

            // Both groups must be laid out: a `structure_dirty` that fails to
            // bubble up to a `relayout` leaves the new group at `Rect::ZERO`,
            // silently invisible rather than failing here.
            for i in 0..2 {
                let g = d
                    .host_mut()
                    .group_at_for_tests(strip_id, i)
                    .expect("group laid out");
                let r = g.get_rect();
                assert!(
                    r.area() > 0.0,
                    "group[{i}] must have a non-zero rect after relayout; got {r:?}",
                );
            }
        }
        // Phase `detach_click_body_ctrl_k_scopes_to_clicked_tab`.
        // Drag a tab into its own group, click that group's window body, then
        // press Ctrl+K. The palette must not make the sibling group disappear,
        // and must scope to the CLICKED group's tab, not whichever group was
        // active before the detach. Extends
        // the `driver_drag_tab_out_creates_sibling_group` phase's setup.
        {
            let mut d = ShellDriver::new((1600, 900));
            let _ = d.paint();
            d.relayout();

            let strip_id = detach_files_tab_into_sibling_group(&mut d);
            let (files_tab_id, files_rect) = assert_detached_into_two_groups(&mut d, strip_id);

            // Well below the tab bar so we don't hit a pill.
            let body_click = Point::new(
                f64::midpoint(files_rect.x0, files_rect.x1),
                files_rect.y0 + files_rect.height() * 0.6,
            );
            d.click_at(body_click);
            d.relayout();
            let _ = d.paint();
            d.relayout();

            let after_click = d.host().groups_per_strip();
            let found = after_click
                .iter()
                .find(|(s, _)| *s == strip_id)
                .expect("strip present after body click");
            assert_eq!(
                found.1.len(),
                2,
                "body click alone should not drop a group; got {:?}",
                found.1
            );

            d.set_modifiers(ModifiersState::CONTROL);
            d.press_key(&Key::Character(winit::keyboard::SmolStr::new("k")));
            d.set_modifiers(ModifiersState::empty());
            let _ = d.paint();
            d.relayout();

            // Sibling group must still exist with a non-zero rect.
            let post2 = d.host().groups_per_strip();
            let strip2 = post2
                .iter()
                .find(|(s, _)| *s == strip_id)
                .expect("strip present after ctrl+k");
            assert_eq!(
                strip2.1.len(),
                2,
                "sibling group must survive Ctrl+K (got groups={:?})",
                strip2.1
            );
            for i in 0..2 {
                let r = d
                    .host_mut()
                    .group_at_for_tests(strip_id, i)
                    .expect("group laid out post ctrl+k")
                    .get_rect();
                assert!(r.area() > 0.0, "group[{i}] must still render (rect={r:?})");
            }

            let active_tab = d.chrome().active_tab_id();
            assert_eq!(
                active_tab,
                Some(files_tab_id),
                "Ctrl+K must scope to the tab in the CLICKED group; \
                 expected files_tab_id={files_tab_id}, got {active_tab:?}"
            );
        }
    }

    /// Drag the Files tab into its own sibling group. Split out of
    /// [`driver_tab_drag_and_detach_scope_to_the_right_group`] to keep that test
    /// short.
    fn detach_files_tab_into_sibling_group(d: &mut ShellDriver) -> shell::shell::state::StripId {
        let strip_id = {
            let st = d.chrome();
            let tree = st.tree();
            let ws_id = tree.desktop.active;
            *tree.desktop.active_strip.get(&ws_id).expect("active strip")
        };
        let (from, to) = {
            let group = d
                .host_mut()
                .group_at_for_tests(strip_id, 0)
                .expect("group at idx 0");
            let group_rect = group.get_rect();
            let rects = group.tab_rects().to_vec();
            assert_eq!(rects.len(), 4, "bar must have 4 pills after layout");
            let files_pill = rects[1];
            let from = files_pill.center();
            let to = Point::new(
                group_rect.x1 - 24.0,
                f64::midpoint(group.bar_rect().y1, group_rect.y1),
            );
            (from, to)
        };
        d.drag(from, to, 16);
        d.relayout();
        let _ = d.paint();
        d.relayout();
        strip_id
    }

    /// Assert the detach left exactly two groups in `strip_id`, and return
    /// the detached (singleton) group's tab id and rect. Split out of
    /// [`driver_tab_drag_and_detach_scope_to_the_right_group`] to keep that test
    /// short.
    fn assert_detached_into_two_groups(
        d: &mut ShellDriver,
        strip_id: shell::shell::state::StripId,
    ) -> (u64, Rect) {
        let post = d.host().groups_per_strip();
        let strip = post
            .iter()
            .find(|(s, _)| *s == strip_id)
            .expect("strip present");
        assert_eq!(strip.1.len(), 2, "detach must produce two groups");

        // The detached group is the singleton.
        let files_group_idx = strip
            .1
            .iter()
            .position(|(_, tabs)| tabs.len() == 1)
            .expect("singleton (detached) group");
        // Exactly two groups are asserted above, so the other index is
        // whichever of {0, 1} `files_group_idx` isn't — no arithmetic needed.
        let notes_group_idx = usize::from(files_group_idx == 0);
        let files_tab_id = strip.1[files_group_idx].1[0];
        let files_rect = d
            .host_mut()
            .group_at_for_tests(strip_id, files_group_idx)
            .expect("files-group widget")
            .get_rect();
        let notes_rect = d
            .host_mut()
            .group_at_for_tests(strip_id, notes_group_idx)
            .expect("notes-group widget")
            .get_rect();
        assert!(
            files_rect.area() > 0.0 && notes_rect.area() > 0.0,
            "both groups laid out before body click"
        );
        (files_tab_id, files_rect)
    }
}
