//! `ShellDriver` — programmable, headless-friendly driver for the nirios
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

use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Point, Rect};
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
#[cfg(test)]
use winit::keyboard::NamedKey;
use winit::keyboard::{Key, KeyLocation, ModifiersState, PhysicalKey};

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
    capture_executor: Option<CaptureExecutor>,
}

impl ShellDriver {
    /// Build a driver around the canonical bootstrap workspace tree at
    /// `viewport` size. Equivalent to what `bin/shots`'s headless path
    /// did per-scene, but stateful across actions.
    pub fn new(viewport: (u32, u32)) -> Self {
        Self::with_tree(WorkspaceTree::bootstrap_demo(), viewport)
    }

    /// Build a driver around the provided workspace tree at `viewport` size.
    pub fn with_tree(tree: WorkspaceTree, viewport: (u32, u32)) -> Self {
        // Boot adopts every ownerless workspace before the session ever
        // paints (`compositor::hosted`), so a room you made is a room
        // you own; skipping it renders "no members" for rooms that on a
        // real machine are yours.
        let mut tree = tree;
        tree.adopt_ownerless_workspaces(&shell::shell::state::local_user_id());
        Self {
            fb: FrameBuilder::with_tree(tree, viewport),
            state: AutoCommit::new(),
            root_obj: automerge::ROOT.clone(),
            modifiers: Modifiers::default(),
            focus: None,
            last_cursor: Point::ORIGIN,
            capture_executor: None,
        }
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
            &mut self.state,
            &self.root_obj,
            &mut self.focus,
        )
    }

    /// Synthesise a click at `p` — a `CursorMoved` to position, then a
    /// `Pressed`+`Released` left-button pair. Each step gets its own
    /// `dispatch` so per-event hit-tests run in the same order they
    /// would on real hardware.
    pub fn click_at(&mut self, p: Point) {
        self.dispatch(&WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(p.x, p.y),
        });
        self.dispatch(&WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
        self.dispatch(&WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        });
    }

    /// Synthesise a `MouseWheel` event at the cursor's current position
    /// (the host's wheel handlers gate on `last_cursor`, which is the
    /// last `CursorMoved` value the driver dispatched). `dx`/`dy` are
    /// `LineDelta` units — one wheel detent ≈ `1.0`. Trackpad-style
    /// pixel deltas should multiply by ~`1.0/16.0` first to stay
    /// concept-faithful with the strip's per-line factor.
    pub fn mouse_wheel(&mut self, dx: f32, dy: f32) {
        self.dispatch(&WindowEvent::MouseWheel {
            device_id: unsafe { std::mem::zeroed() },
            delta: winit::event::MouseScrollDelta::LineDelta(dx, dy),
            phase: winit::event::TouchPhase::Moved,
        });
    }

    /// Synthesise a `CursorMoved` event WITHOUT pressing or releasing.
    /// Use with `press()`/`release()` to compose drags that need
    /// intermediate snapshots — e.g. mid-drag drop-zone capture or
    /// scrub-then-pause behaviour. Updates `self.last_cursor` so any
    /// follow-up `mouse_wheel()` lands at the new position.
    pub fn move_cursor(&mut self, p: Point) {
        self.dispatch(&WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(p.x, p.y),
        });
    }

    /// Synthesise a left-button `Pressed` at the current cursor without
    /// releasing. Caller is responsible for the matching `release()`
    /// once the drag is complete.
    pub fn press(&mut self) {
        self.dispatch(&WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
    }

    /// Synthesise a left-button `Released` at the current cursor.
    pub fn release(&mut self) {
        self.dispatch(&WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        });
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
        self.dispatch(&WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(from.x, from.y),
        });
        self.dispatch(&WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button,
        });
        let n = f64::from(steps.max(1));
        for s in 1..=steps.max(1) {
            let t = f64::from(s) / n;
            let p = Point::new(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
            self.dispatch(&WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: PhysicalPosition::new(p.x, p.y),
            });
        }
        self.dispatch(&WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button,
        });
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
            // Drive the per-frame update + redraw so animation state
            // advances. `update` runs the per-tick reconciler too.
            self.fb.host_mut().update(&mut self.state, &self.root_obj);
            // Re-draw into a throwaway scene so tick functions inside
            // `draw` (overview crossfade tick, etc.) see elapsed time.
            let _ = self.paint();
        }
    }

    /// Render the current frame into a fresh Vello scene. Caller is
    /// responsible for passing it to a renderer (offscreen or live).
    pub fn paint(&mut self) -> Scene {
        let mut scene = self.fb.paint();
        if let Some(executor) = self.capture_executor.as_mut() {
            let _ = executor.execute_ready(&mut self.fb, &mut scene);
        } else {
            self.fb.draw_capture_overlays(&mut scene);
        }
        scene
    }

    pub fn set_capture_execution_enabled(&mut self, enabled: bool) {
        self.capture_executor = enabled.then(CaptureExecutor::default);
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
        self.fb.paint_settled()
    }
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
            let active = tree.active_workspace().map(|w| w.room);
            tree.workspaces
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
        use shell::shell::state::{CapOverride, Scope, Subject, Verb};
        let handle = self.chrome_handle();
        let owner = handle.borrow().local_user_id.clone();
        {
            let st = handle.borrow();
            let mut tree = st.tree_mut();
            let ws_id = tree.active;
            let room = ws_id.raw().to_string();
            for (member, verb) in [("ada", Verb::Write), ("bo", Verb::Read)] {
                tree.grant_in_workspace(
                    ws_id,
                    &owner,
                    CapOverride::membership(Subject::person(member), room.clone(), verb),
                );
            }
            // Both members at their desks: a room only carries a display
            // name while a session of theirs is present.
            if let Some(ws) = tree.active_workspace_mut() {
                ws.collaborators
                    .push(shell::shell::state::Collaborator::new("ada", "Ada"));
                ws.collaborators
                    .push(shell::shell::state::Collaborator::new("bo", "Bo"));
            }
            for (path, who, verb) in [
                ("/var/workspace/design", "ada", Verb::Write),
                ("/var/workspace/brief.pdf", "bo", Verb::Read),
            ] {
                tree.grant_in_workspace(
                    ws_id,
                    &owner,
                    CapOverride::grant(Subject::person(who), Scope::FsPath(path.to_string()), verb),
                );
            }
            // Research already has two people present. Give them the
            // membership their presence implies, so the row shows the
            // faces of people who can actually reach it.
            let research: Vec<_> = tree
                .workspaces
                .iter()
                .filter(|w| w.name == "Research")
                .map(|w| (w.id, w.id.raw().to_string()))
                .collect();
            for (id, room) in research {
                for (member, verb) in [("alice", Verb::Write), ("bob", Verb::Read)] {
                    tree.grant_in_workspace(
                        id,
                        &owner,
                        CapOverride::membership(Subject::person(member), room.clone(), verb),
                    );
                }
            }
        }
        self.with_chrome(|_| {});
    }

    /// Scenario-facing wrapper for the canonical shared-workspace fixture.
    /// Keeping the mutation here ensures screenshots use the same
    /// authority-checked capabilities and presence data as legacy scenes.
    pub fn seed_shared_workspace_for_shots(&mut self) {
        self.seed_shared_room();
    }

    /// Materialise the durable half of an invite without pretending the
    /// recipient is already online. The membership record takes the same
    /// authority-checked path as workspace settings; the `invited` presence
    /// row is intentionally non-followable until a session arrives.
    pub fn seed_invite_pending_for_shots(&mut self) {
        use shell::shell::state::{CapOverride, Collaborator, Subject, Verb};

        let handle = self.chrome_handle();
        let owner = handle.borrow().local_user_id.clone();
        {
            let st = handle.borrow();
            let mut tree = st.tree_mut();
            tree.adopt_ownerless_workspaces(&owner);
            let workspace = tree.active;
            tree.grant_in_workspace(
                workspace,
                &owner,
                CapOverride::membership(
                    Subject::person("ada"),
                    workspace.raw().to_string(),
                    Verb::Write,
                ),
            );
            if let Some(ws) = tree.active_workspace_mut()
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
            let notes_tab = tree.active_workspace().and_then(|ws| {
                ws.strips
                    .iter()
                    .flat_map(|strip| &strip.window_groups)
                    .flat_map(|group| &group.tabs)
                    .find(|tab| tab.title == "Notes")
                    .map(|tab| tab.id)
            });
            if let (Some(notes_tab), Some(ws)) = (notes_tab, tree.active_workspace_mut())
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
        admitted
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
                    .revert_to(&mut tree, &id)
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

    /// Show that presence attaches to app content. We focus the real Files
    /// hosted window and move Ada's location to its tab; it is not a second
    /// shell-only mock of the same avatar.
    pub fn show_collaborator_in_files_for_shots(&mut self) -> bool {
        let target = {
            let state = self.chrome();
            let tree = state.tree();
            tree.active_workspace().and_then(|ws| {
                ws.strips.iter().find_map(|strip| {
                    strip.window_groups.iter().find_map(|group| {
                        group
                            .tabs
                            .iter()
                            .find(|tab| tab.title == "Files")
                            .map(|tab| (strip.id, group.id, tab.id))
                    })
                })
            })
        };
        let Some((strip, group, files_tab)) = target else {
            return false;
        };
        self.host_mut()
            .focus_window_on_strip(strip, group, files_tab);
        self.with_chrome(|state| {
            let mut tree = state.tree_mut();
            if let Some(ada) = tree.active_workspace_mut().and_then(|ws| {
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
        true
    }

    /// Seeds the active workspace with membership and granted capabilities
    /// via `WorkspaceTree::grant_in_workspace` — the same authority-checked
    /// path the settings panel and portal use, so this is a state the
    /// compositor can actually be in.
    fn seed_workspace_permissions(&mut self) {
        use shell::shell::state::{CapOverride, Expiry, Scope, Subject, Verb};
        let handle = self.chrome_handle();
        let owner = handle.borrow().local_user_id.clone();
        {
            let st = handle.borrow();
            let mut tree = st.tree_mut();
            tree.adopt_ownerless_workspaces(&owner);
            let ws_id = tree.active;
            let room = ws_id.raw().to_string();
            for (member, verb) in [("ada", Verb::Write), ("bo", Verb::Read)] {
                tree.grant_in_workspace(
                    ws_id,
                    &owner,
                    CapOverride::membership(Subject::person(member), room.clone(), verb),
                );
            }
            for (app, scope, verb, expiry) in [
                (
                    "Files",
                    Scope::FsPath("/var/workspace".to_string()),
                    Verb::Write,
                    Expiry::Never,
                ),
                (
                    "Terminal",
                    Scope::Device("camera".to_string()),
                    Verb::Read,
                    Expiry::EndOfSession,
                ),
                (
                    "Browser",
                    Scope::NetHost("*".to_string()),
                    Verb::Read,
                    Expiry::AfterUse,
                ),
            ] {
                tree.grant_in_workspace(
                    ws_id,
                    &owner,
                    CapOverride::grant(Subject::app(app), scope, verb).expiring(expiry),
                );
            }
            tree.grant_in_workspace(
                ws_id,
                &owner,
                CapOverride::grant(
                    Subject::person("ada"),
                    Scope::FsPath("/var/workspace/design".to_string()),
                    Verb::Write,
                ),
            );
        }
        self.with_chrome(|_| {});
    }

    /// Applies the named "scene" mutation set (the legacy `--scenes`
    /// catalogue) — a one-shot mutation; call on a fresh driver per scene.
    pub fn scene_setup(&mut self, name: &str) -> Result<(), String> {
        match name {
            "desktop" => Self::scene_desktop(),
            "overview" => self.scene_overview(),
            "overview-shared" => self.scene_overview_shared(),
            "palette" => self.scene_palette(),
            "palette_app_query" => self.scene_palette_app_query(),
            "palette_in_app_query" => self.scene_palette_in_app_query(),
            "palette_records" => self.scene_palette_records(),
            "palette_no_results" => self.scene_palette_no_results(),
            "palette_typed" => self.scene_palette_typed(),
            "quick-settings" => self.scene_quick_settings(),
            "qs-wifi-drillin" => self.scene_qs_wifi_drillin(),
            "qs-bt-drillin" => self.scene_qs_bt_drillin(),
            "ws-settings-general" => self.scene_ws_settings_general(),
            "ws-settings-members" => self.scene_ws_settings_members(),
            "ws-settings-apps" => self.scene_ws_settings_apps(),
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

    /// Bare desktop — the driver's default state already renders it, so
    /// this scene is a deliberate no-op kept for catalogue symmetry.
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
        self.fb.host_mut().set_palette_query("nirios");
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

    fn scene_quick_settings(&mut self) {
        self.with_chrome(|st| st.overlays.quick_settings = true);
        self.fb.host_mut().force_quick_settings_settled();
    }

    fn scene_qs_wifi_drillin(&mut self) {
        self.with_chrome(|st| {
            st.overlays.quick_settings = true;
            st.quick_settings.push_level(QsLevel::Wifi);
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

    /// Drives the room through the production mirrors, not a seeded
    /// `HistoryEntry` — these are the same calls the portal, permission
    /// prompt, drop handler and settings modal make.
    fn scene_room_activity(&mut self) {
        use shell::shell::state::{CapOverride, Scope, Subject, Verb};
        let handle = self.chrome_handle();
        let owner = {
            let st = handle.borrow();
            st.local_user_id.clone()
        };
        {
            let st = handle.borrow();
            let mut tree = st.tree_mut();
            tree.adopt_ownerless_workspaces(&owner);
            let ws_id = tree.active;
            tree.grant_in_workspace(
                ws_id,
                &owner,
                CapOverride::grant(
                    Subject::app("Files"),
                    Scope::FsPath("/var/workspace/design".to_string()),
                    Verb::Write,
                ),
            );
            let temp = tree.grant_in_workspace(
                ws_id,
                &owner,
                CapOverride::grant(
                    Subject::app("Terminal"),
                    Scope::Device("camera".to_string()),
                    Verb::Read,
                ),
            );
            tree.doc_append_tab_to_first_group_of_active_workspace(
                90_001,
                "Release notes".to_string(),
                b"{}".to_vec(),
            );
            tree.commit_room_change();
            if let Some(cap_id) = temp {
                tree.revoke_in_workspace(ws_id, &owner, &cap_id);
            }
            if let Some(ws) = tree.active_workspace_mut() {
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
            if peer.apply_changes(base).is_ok() {
                const DAY: i64 = 86_400;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
                for (days_ago, title) in
                    [(11_i64, "Q3 budget"), (5, "Onboarding notes"), (1, "Retro")]
                {
                    if peer_append_tab(&mut peer, title) {
                        qdoc::history::commit_stamped_at(
                            &mut peer,
                            now.saturating_sub(days_ago.saturating_mul(DAY)),
                        );
                    }
                }
                let blobs: Vec<Vec<u8>> = peer
                    .get_changes(&tree.doc.get_heads())
                    .into_iter()
                    .map(|c| c.raw_bytes().to_vec())
                    .collect();
                let _ = tree.apply_changes(&blobs);
            }
        }
        self.with_chrome(|st| st.overlays.room_activity = true);
        self.fb.host_mut().force_room_activity_settled();
    }

    /// Deterministic scenario entry point for the two history zoom levels.
    /// It seeds the same collaborative changes as the legacy visual scene,
    /// then switches through the pane's public API rather than reaching into
    /// drawing state.
    pub fn seed_room_activity_for_shots(&mut self, explore: bool) {
        self.scene_room_activity();
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
                .compare_selection(&mut tree, None)
                .is_some()
            {
                break;
            }
        }
        let _ = self.fb.paint();
    }

    /// Two runs named, one by the peer under its own actor, so the band
    /// reads "named by" someone other than the viewer. Both go in through
    /// the pane's own write path (the [Name] chip's call).
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
                qdoc::history::commit_stamped(&mut peer);
                let _ = tree.doc.merge(&mut peer);
            }

            if let (Some(first), Some(fourth)) = (entries.first(), entries.get(3)) {
                let pane = self.fb.host_mut().room_activity_mut();
                pane.apply_name(
                    &mut tree,
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
        self.fb
            .host_mut()
            .room_activity_mut()
            .set_scope(shell::shell::room_activity::RoomScope::AllWorkspaces, &tree);
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
        use shell::shell::notification::{Notification, NotificationSeverity};
        // Backdate `created_at` past the slide-in window: at t=0 a
        // toast's `enter` progress is ~0 and `slide_dx == TOAST_WIDTH`
        // pushes it entirely off-screen-right.
        let settled_at = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap_or_else(Instant::now);
        self.with_chrome(|st| {
            // Severity ramp in one frame; long dismiss windows so
            // the still-image scene captures the stack visible.
            st.push_notification_full(
                Notification::new("Switched to workspace Research", "Active tab: Notes")
                    .with_severity(NotificationSeverity::Info)
                    .with_dismiss_after(Duration::from_secs(60))
                    .with_created_at(settled_at),
            );
            st.push_notification_full(
                Notification::new("Slow network", "Peer sync delayed; retrying")
                    .with_severity(NotificationSeverity::Warning)
                    .with_dismiss_after(Duration::from_secs(60))
                    .with_created_at(settled_at),
            );
            st.push_notification_full(
                Notification::new("Permission denied", "Files cannot access /etc/shadow")
                    .with_severity(NotificationSeverity::Error)
                    .with_dismiss_after(Duration::from_secs(60))
                    .with_created_at(settled_at),
            );
        });
    }

    fn scene_notifications_one(&mut self) {
        use shell::shell::notification::{Notification, NotificationSeverity};
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
        use shell::shell::notification::{Notification, NotificationSeverity};
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
            if let Some(ws) = tree.active_workspace_mut()
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
                });
                strip_id = Some(strip.id);
                sibling = Some(gid);
            }
            // Focus the newly-added right-hand window, so the
            // scene shows the signal moving off the default.
            if let (Some(sid), Some(gid)) = (strip_id, sibling) {
                tree.set_active_group(sid, gid);
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
                g.set_width(half.to_ui());
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
                if let Some(ws) = tree.active_workspace_mut() {
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
                        });
                        ws.strips.push(strip);
                    }
                    focus = ws.strips.get(1).map(|s| s.id).map(|sid| (ws.id, sid));
                }
                if let Some((ws_id, sid)) = focus {
                    tree.active_strip.insert(ws_id, sid);
                }
            }
            st.bump();
        });
    }

    /// Catalogue of scene names supported by `scene_setup`. Used by the
    /// legacy `--scenes` flag for default expansion + validation.
    pub const ALL_SCENES: &'static [&'static str] = &[
        "desktop",
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
        "quick-settings",
        "qs-wifi-drillin",
        "qs-bt-drillin",
        "ws-settings-general",
        "ws-settings-members",
        "ws-settings-permissions",
        "ws-settings-apps",
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
// implementation. See `shell/src/compositor/frame.rs`.

/// Synthesise a `WindowEvent::KeyboardInput { state: Pressed }` for
/// `key`. winit's `KeyEvent` has a `pub(crate)` field
/// (`platform_specific`) so we zero-init the struct + overwrite the
/// fields the shell actually reads. Same workaround the e2e harness
/// uses (see `tests/e2e_shell_flows.rs::escape_event`).
pub fn key_press_event(key: &Key) -> WindowEvent {
    let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = key.clone();
    ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    ev.location = KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    // `text` is `Option<SmolStr>` — for ASCII characters we mirror what
    // winit normally fills so the palette's character-input branch
    // sees the same glyph it would on real hardware.
    ev.text = if let Key::Character(s) = key {
        Some(s.clone())
    } else {
        None
    };
    WindowEvent::KeyboardInput {
        device_id: unsafe { std::mem::zeroed() },
        event: ev,
        is_synthetic: false,
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collaboration_fixture_keeps_pending_invite_out_of_live_presence() {
        let mut d = ShellDriver::new((1600, 900));
        d.seed_invite_pending_for_shots();
        let invited = d
            .chrome()
            .tree()
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
            .active_workspace()
            .and_then(|ws| ws.collaborators.iter().find(|person| person.id == "ada"))
            .cloned()
            .expect("acceptance retains the same identity");
        assert!(!live.invited);
        let notes = d.chrome().tree().active_workspace().and_then(|ws| {
            ws.strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .find(|tab| tab.title == "Notes")
                .map(|tab| tab.id)
        });
        assert_eq!(live.active_tab, notes);
    }

    /// Drive Cmd+Tab through `dispatch` and verify the chrome's
    /// `overlays.overview` flag flips. Mirrors the live binary's
    /// hotkey path through real `handle_event` dispatch on the host
    /// — exercises the full `dispatch → ShellOverlayHost::handle_event
    /// → chrome.borrow_mut()` chain.
    #[test]
    fn driver_dispatch_routes_click_to_chrome() {
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

    /// Toggle the palette open via the public host API, step a few
    /// frames so the palette open animation can register, then assert
    /// the palette is still visible. Pinned smoke for `step_frames`
    /// not panicking and not nuking visibility flags.
    #[test]
    fn driver_step_frames_advances_palette_fade() {
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

    /// Paint the overview, then verify the named-rect resolver can pick
    /// out the "+ Add workspace" tile (which `draw_overview` populates
    /// each visible frame).
    #[test]
    fn driver_resolve_named_rect_for_overview_add_workspace() {
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

    /// Dragging the second tab pill (Files) out of the bootstrap default's
    /// 4-tab Wide group, past its right edge into the empty strip area,
    /// must produce TWO groups (3-tab source + 1-tab sibling), not a
    /// replace. Reads the live `tab_rects()` off the bar so the press
    /// lands on the pill regardless of layout drift.
    #[test]
    fn driver_drag_tab_out_creates_sibling_group() {
        let mut d = ShellDriver::new((1600, 900));
        let _ = d.paint(); // seed any draw-only caches the layout missed
        d.relayout();

        // Snapshot pre-drag layout: bootstrap default seeds one strip
        // with one Wide group carrying Notes/Files/Terminal/Browser.
        let strip_id = {
            let st = d.chrome();
            let tree = st.tree();
            let ws_id = tree.active;
            *tree.active_strip.get(&ws_id).expect("active strip")
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
            // Release point: 100 px right of the source group's right
            // edge, same y as the bar — squarely in the trailing gap.
            let to = Point::new(group_rect.x1 + 100.0, from.y);
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

    /// Smoke for `scene_setup` — drive every named scene and verify it
    /// returns Ok, then verify a fresh driver returns Err for an
    /// unknown name.
    #[test]
    fn driver_scene_setup_covers_catalogue() {
        for &name in ShellDriver::ALL_SCENES {
            let mut d = ShellDriver::new((1600, 900));
            d.scene_setup(name)
                .unwrap_or_else(|e| panic!("scene_setup({name}) failed: {e}"));
            let _ = d.paint_settled();
        }
        let mut d = ShellDriver::new((1600, 900));
        assert!(d.scene_setup("nope").is_err());
    }

    /// `widget_registry` must enumerate the live widget tree after a
    /// paint, surfacing a `TopBarChrome` entry from the bootstrap default —
    /// pinned so a regression in `ShellOverlayHost::children()` fails here
    /// rather than silently returning an empty registry.
    #[test]
    fn widget_registry_finds_topbar_buttons() {
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

    /// `resolve_named_rect` must fall through to the widget tree when the
    /// hand-curated table misses: `command_palette_input` isn't in that
    /// table, but the `CommandPalette` widget opts in via `test_id()`.
    #[test]
    fn resolve_named_rect_falls_back_to_registry() {
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

    /// Drag the Files tab into its own sibling group. Split out of
    /// [`detach_click_body_ctrl_k_scopes_to_clicked_tab`] to keep that test
    /// short.
    fn detach_files_tab_into_sibling_group(d: &mut ShellDriver) -> shell::shell::state::StripId {
        let strip_id = {
            let st = d.chrome();
            let tree = st.tree();
            let ws_id = tree.active;
            *tree.active_strip.get(&ws_id).expect("active strip")
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
            let to = Point::new(group_rect.x1 + 100.0, from.y);
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
    /// [`detach_click_body_ctrl_k_scopes_to_clicked_tab`] to keep that test
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

    /// Drag a tab into its own group, click that group's window body, then
    /// press Ctrl+K. The palette must not make the sibling group disappear,
    /// and must scope to the CLICKED group's tab, not whichever group was
    /// active before the detach. Extends
    /// [`driver_drag_tab_out_creates_sibling_group`]'s setup.
    #[test]
    fn detach_click_body_ctrl_k_scopes_to_clicked_tab() {
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
