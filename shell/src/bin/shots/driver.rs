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
use winit::keyboard::{Key, KeyLocation, ModifiersState, NamedKey, PhysicalKey};

use shell::compositor::frame::FrameBuilder;
use shell::shell::chrome_state::{Chrome, ChromeState, WsTab};
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::quick_settings::QsLevel;
use shell::shell::state::WorkspaceTree;
use ui::Widget;

use taffy::prelude::NodeId;
use ui::theme::Theme;

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
}

impl ShellDriver {
    /// Build a driver around the canonical bootstrap workspace tree at
    /// `viewport` size. Equivalent to what `bin/shots`'s headless path
    /// did per-scene, but stateful across actions.
    pub fn new(viewport: (u32, u32)) -> Self {
        Self::with_tree(WorkspaceTree::bootstrap_demo(), viewport)
    }

    pub fn with_tree(tree: WorkspaceTree, viewport: (u32, u32)) -> Self {
        // Boot adopts every ownerless workspace before the session ever
        // paints (`compositor::hosted`), so a room you made is a room
        // you own. A harness that skipped it rendered surfaces reporting
        // "no members" for rooms that on a real machine are yours —
        // a state no user has ever seen.
        let mut tree = tree;
        tree.adopt_ownerless_workspaces(&shell::shell::state::local_user_id());
        Self {
            fb: FrameBuilder::with_tree(tree, viewport),
            state: AutoCommit::new(),
            root_obj: automerge::ROOT.clone(),
            modifiers: Modifiers::default(),
            focus: None,
            last_cursor: Point::ORIGIN,
        }
    }

    /// Run compute_layout + apply_layout against the current tree. The
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
    pub fn chrome_handle(&self) -> Chrome {
        self.fb.chrome_handle()
    }

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
    pub fn palette_query(&self) -> &str {
        self.fb.host().palette_query()
    }
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
    pub fn dispatch(&mut self, event: WindowEvent) -> bool {
        if let WindowEvent::CursorMoved { position, .. } = &event {
            self.last_cursor = Point::new(position.x, position.y);
        }
        if let WindowEvent::ModifiersChanged(m) = &event {
            self.modifiers = *m;
        }
        self.fb.dispatch_event(
            &event,
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
        self.dispatch(WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(p.x, p.y),
        });
        self.dispatch(WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
        self.dispatch(WindowEvent::MouseInput {
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
        self.dispatch(WindowEvent::MouseWheel {
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
        self.dispatch(WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(p.x, p.y),
        });
    }

    /// Synthesise a left-button `Pressed` at the current cursor without
    /// releasing. Caller is responsible for the matching `release()`
    /// once the drag is complete.
    pub fn press(&mut self) {
        self.dispatch(WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
    }

    /// Synthesise a left-button `Released` at the current cursor.
    pub fn release(&mut self) {
        self.dispatch(WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        });
    }

    /// Press at `from`, drag through `steps` intermediate `CursorMoved`
    /// events, release at `to`. Used by scenarios that exercise the
    /// tab-drag / wheel-spin flows.
    pub fn drag(&mut self, from: Point, to: Point, steps: u32) {
        self.dispatch(WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(from.x, from.y),
        });
        self.dispatch(WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
        let n = steps.max(1) as f64;
        for s in 1..=steps.max(1) {
            let t = s as f64 / n;
            let p = Point::new(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
            self.dispatch(WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: PhysicalPosition::new(p.x, p.y),
            });
        }
        self.dispatch(WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        });
    }

    /// Synthesise a `KeyboardInput { state: Pressed }` for `key`.
    pub fn press_key(&mut self, key: Key) {
        let event = key_press_event(key);
        self.dispatch(event);
    }

    /// Type a literal string — one Key::Character event per non-control
    /// character. Triggers the same handle_event path the live binary's
    /// keyboard handler does. Caller is expected to have opened an
    /// input target (palette, name editor, etc.) first.
    pub fn type_str(&mut self, s: &str) {
        for c in s.chars() {
            if c.is_control() {
                continue;
            }
            let event =
                key_press_event(Key::Character(winit::keyboard::SmolStr::new(c.to_string())));
            self.dispatch(event);
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
            let _ = self.fb.paint();
        }
    }

    /// Render the current frame into a fresh Vello scene. Caller is
    /// responsible for passing it to a renderer (offscreen or live).
    pub fn paint(&mut self) -> Scene {
        self.fb.paint()
    }

    /// Convenience: relayout + draw. Used by the offscreen scene-set
    /// path (legacy `--scenes`) where state mutations between scenes
    /// can affect the structural layout (overlay reservations).
    pub fn paint_settled(&mut self) -> Scene {
        self.fb.paint_settled()
    }
}

/// Append a tab to the first window group of the first workspace, straight
/// into a forked document.
///
/// The peer in the `room-activity` scene has no `WorkspaceTree` of its own —
/// it is a bare document, exactly like a remote replica that has only ever
/// received change blobs. So it writes the same shape
/// `WorkspaceTree::doc_append_tab_to_first_group_of_active_workspace` writes,
/// by hand. Returns whether anything was written.
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
    let _ = doc.put(&entry, "id", ScalarValue::Uint(70_000 + at as u64));
    let _ = doc.put(&entry, "title", title.to_string());
    true
}

impl ShellDriver {
    /// Apply the named "scene" mutation set (the legacy `--scenes`
    /// catalogue). Each scene is a one-shot mutation — call on a fresh
    /// driver per scene if a clean slate is required.
    /// Seed the active workspace with a membership and a set of granted
    /// capabilities, so the Members and Permissions panes have something
    /// real to render.
    ///
    /// Everything goes through `WorkspaceTree::grant_in_workspace` — the
    /// authority-checked, doc-mirroring path the settings panel and the
    /// portal both use. Nothing here writes a capability the enforcement
    /// path would not honour, so a shot of this scene is a shot of a
    /// state the compositor can actually be in.
    /// Put a few records in the rooms the user is *not* in, so a search
    /// has something to bring here.
    ///
    /// Sample content, and only ever reachable from `--shots`: the live
    /// compositor's corpus is the records apps actually wrote. It goes
    /// through the same `RoomRecords::reference` the palette's own Enter
    /// does, so what the scene paints is what a real room index produces.
    fn seed_records_in_other_rooms(&mut self) {
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
            index.reference(
                rooms[i % rooms.len()],
                &RecordId::for_file(path),
                &RecordKind::new(*kind),
            );
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
            // Both members are at their desks. A room only carries a
            // person's display name while a session of theirs is in it,
            // so a member with nobody home shows as the principal the
            // capability names — true, and the seam a room document
            // grows names through.
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
                    CapOverride::grant(
                        Subject::person(who),
                        Scope::FsPath(path.to_string()),
                        verb,
                    ),
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

    pub fn scene_setup(&mut self, name: &str) -> Result<(), String> {
        match name {
            "desktop" => {}
            "overview" => {
                self.with_chrome(|st| st.overlays.overview = true);
                self.fb.host_mut().force_overview_settled(true);
            }
            // The same panel with the room actually shared: members on
            // this room and on Research, and two resources handed out.
            "overview-shared" => {
                self.seed_shared_room();
                self.with_chrome(|st| st.overlays.overview = true);
                self.fb.host_mut().force_overview_settled(true);
            }
            "palette" => {
                self.fb.host_mut().set_palette_visible(true);
                // Production opens the palette focused (see
                // `open_palette_with_scope`); a scene that skipped this
                // rendered the field's idle border and hid the focus ring
                // from every screenshot review.
                self.fb.host_mut().set_palette_focus(true);
                // Settle the rising-edge open animation so a single-frame
                // paint shows the fully-opened modal instead of frame 0
                // (`alpha == 0` → indistinguishable from bare desktop).
                self.fb.host_mut().force_palette_settled();
                // Populate the palette via the search registry so the
                // scene shows a real result list. The live compositor
                // does this on every keystroke via `handle_event`;
                // scenes seed it explicitly.
                let root = self.root_obj.clone();
                self.fb
                    .host_mut()
                    .refresh_search_results_for_shots(&mut self.state, &root);
            }
            // The query that exposed the palette's ranking: an installed
            // app's name that also appears in dozens of source paths.
            // Enter activates the top row, so the app has to be it.
            "palette_app_query" => {
                self.fb.host_mut().set_palette_visible(true);
                self.fb.host_mut().force_palette_settled();
                self.fb.host_mut().set_palette_focus(true);
                self.fb.host_mut().set_palette_query("calculator");
                let root = self.root_obj.clone();
                self.fb
                    .host_mut()
                    .refresh_search_results_for_shots(&mut self.state, &root);
            }
            // Ctrl+K's half of search: scoped to the focused app, with a
            // query that appears nowhere but the focused Notes tab's own
            // body. Pumps the apps first — exactly what the live frame
            // loop does every frame — so the note's text is in the
            // document and the provider has content to match rather than
            // a title to fall back on.
            "palette_in_app_query" => {
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
            // Records from other rooms, which is what makes a room that
            // starts empty workable: the rows say where each record
            // lives, and Enter brings it here.
            //
            // The records are seeded into rooms *other* than the active
            // one, because that is the case the gesture exists for — a
            // record already here needs no bringing.
            "palette_records" => {
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
            // What the card looks like when nothing matches. Worth a
            // scene of its own: an empty result list is the state a
            // palette spends real time in, and it is the one most likely
            // to be left looking broken.
            "palette_no_results" => {
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
            "palette_typed" => {
                self.fb.host_mut().set_palette_visible(true);
                self.fb.host_mut().set_palette_focus(true);
                self.fb.host_mut().force_palette_settled();
                self.fb.host_mut().set_palette_query("open");
                let root = self.root_obj.clone();
                self.fb
                    .host_mut()
                    .refresh_search_results_for_shots(&mut self.state, &root);
            }
            "quick-settings" => {
                self.with_chrome(|st| st.overlays.quick_settings = true);
                self.fb.host_mut().force_quick_settings_settled();
            }
            "qs-wifi-drillin" => {
                self.with_chrome(|st| {
                    st.overlays.quick_settings = true;
                    st.quick_settings.push_level(QsLevel::Wifi);
                });
                self.fb.host_mut().force_quick_settings_settled();
            }
            "qs-bt-drillin" => {
                self.with_chrome(|st| {
                    st.overlays.quick_settings = true;
                    st.quick_settings.push_level(QsLevel::Bluetooth);
                });
                self.fb.host_mut().force_quick_settings_settled();
            }
            "ws-settings-general" => {
                self.with_chrome(|st| {
                    st.overlays.ws_settings = true;
                    st.ws_settings_tab = WsTab::General;
                });
                self.fb.host_mut().force_ws_settings_settled();
            }
            "ws-settings-members" => {
                self.seed_workspace_permissions();
                self.with_chrome(|st| {
                    st.overlays.ws_settings = true;
                    st.ws_settings_tab = WsTab::Members;
                });
                self.fb.host_mut().force_ws_settings_settled();
            }
            "room-activity" => {
                // Drive the room through the *production* mirrors first, so
                // the pane has a history to show that the shell actually
                // recorded. Nothing here seeds a `HistoryEntry`: these are the
                // same calls the portal, the permission prompt, the drop
                // handler and the settings modal make.
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

                    // A second person, and a history that is not all one
                    // second old. The peer forks the document under its own
                    // actor, makes a real change, and merges back — the same
                    // shape a remote replica's sync round has — and the
                    // commits are stamped across the past fortnight through
                    // the clock the FS layer uses when it replays a durable
                    // chain. Without this the scene exercises neither the
                    // author directory nor the date filter.
                    let base: Vec<automerge::Change> = tree
                        .export_changes_since(&[])
                        .iter()
                        .filter_map(|b| automerge::Change::try_from(b.as_slice()).ok())
                        .collect();
                    let mut peer = automerge::AutoCommit::new();
                    peer.set_actor(automerge::ActorId::from("ada".as_bytes()));
                    if peer.apply_changes(base).is_ok() {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs() as i64)
                            .unwrap_or(0);
                        const DAY: i64 = 86_400;
                        for (days_ago, title) in
                            [(11_i64, "Q3 budget"), (5, "Onboarding notes"), (1, "Retro")]
                        {
                            if peer_append_tab(&mut peer, title) {
                                qdoc::history::commit_stamped_at(&mut peer, now - days_ago * DAY);
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
            "room-activity-named" => {
                // The same room, with two of its runs named — one of them by the
                // peer, under the peer's own actor, so the band reads "named by"
                // somebody other than whoever is looking. Both names go in through
                // the pane's own write path, which is what the [Name] chip calls.
                self.scene_setup("room-activity")?;
                let handle = self.chrome_handle();
                let st = handle.borrow();
                let mut tree = st.tree_mut();
                let entries = self.fb.host().room_activity().entries().to_vec();
                if entries.len() >= 6 {
                    // The peer's own run: it named it before syncing.
                    let mut peer = tree.doc.fork();
                    peer.set_actor(automerge::ActorId::from("ada".as_bytes()));
                    ui::history_groups::name_range(
                        &mut peer,
                        &entries[entries.len() - 3].id,
                        Some(&entries[entries.len() - 1].id),
                        "Reading week",
                    );
                    qdoc::history::commit_stamped(&mut peer);
                    let _ = tree.doc.merge(&mut peer);

                    let pane = self.fb.host_mut().room_activity_mut();
                    pane.apply_name(
                        &mut tree,
                        &ui::GroupNameRequest {
                            record_id: None,
                            start: entries[0].id,
                            end: entries[3].id,
                            label: "Set up the room".to_string(),
                        },
                    );
                }
                drop(tree);
                drop(st);
                self.fb.host_mut().force_room_activity_settled();
            }
            "room-activity-all" => {
                // The same room, widened to the whole tree — which is what
                // the document actually holds. Every row now says which
                // workspace it happened in.
                self.scene_setup("room-activity")?;
                let handle = self.chrome_handle();
                let st = handle.borrow();
                let tree = st.tree();
                self.fb
                    .host_mut()
                    .room_activity_mut()
                    .set_scope(shell::shell::room_activity::RoomScope::AllWorkspaces, &tree);
            }
            "ws-settings-permissions" => {
                self.seed_workspace_permissions();
                self.with_chrome(|st| {
                    st.overlays.ws_settings = true;
                    st.ws_settings_tab = WsTab::Permissions;
                });
                self.fb.host_mut().force_ws_settings_settled();
            }
            "notifications" => {
                use shell::shell::notification::{Notification, NotificationSeverity};
                // Backdate `created_at` past the slide-in window so the
                // still-frame snapshot captures the toasts settled at
                // their resting position. Without this, every toast's
                // `enter` progress is ~0 at snapshot time and
                // `slide_dx == TOAST_WIDTH` pushes them entirely off-
                // screen-right — the audit on 2026-06-24 mistook this
                // for a text-truncation bug in the notification widget.
                let settled_at = Instant::now() - Duration::from_secs(1);
                self.with_chrome(|st| {
                    // Three sample toasts spanning the severity ramp so
                    // the snapshot exercises info / warning / error
                    // colour treatment in one frame. Long dismiss
                    // windows so the still-image scene captures the
                    // stack fully visible.
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
            "notifications-one" => {
                use shell::shell::notification::{Notification, NotificationSeverity};
                let settled_at = Instant::now() - Duration::from_secs(1);
                self.with_chrome(|st| {
                    st.push_notification_full(
                        Notification::new("Build finished", "All 524 tests passed")
                            .with_severity(NotificationSeverity::Info)
                            .with_dismiss_after(Duration::from_secs(60))
                            .with_created_at(settled_at),
                    );
                });
            }
            "notifications-dnd" => {
                use shell::shell::notification::{Notification, NotificationSeverity};
                let settled_at = Instant::now() - Duration::from_secs(1);
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
            "multi-group" => {
                // Construct a 3-group strip in the active workspace by
                // pushing two extra `WindowGroup` rows alongside the
                // bootstrap default. This is the only scene-time mutation
                // that exercises the inter-group chrome (resize handles,
                // active-group outlines, gap drop-zones) — every other
                // scene operates on the single-group bootstrap.
                use shell::shell::state::{Tab, TileContent, WindowGroup, WindowWidth};
                self.with_chrome(|st| {
                    let mut tree = st.tree_mut();
                    if let Some(ws) = tree.active_workspace_mut() {
                        if let Some(strip) = ws.strips.first_mut() {
                            // Two synthetic groups, Normal-width, each
                            // with one stub tab. IDs picked far above the
                            // bootstrap range so they don't collide with
                            // anything the persistence layer might
                            // produce.
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
                    }
                    drop(tree);
                    st.bump();
                });
            }
            // One window filling the strip. Maximizing used to stroke a
            // 2 px accent rectangle around the group's bounding box —
            // sharp corners a pixel outside a rounded card — and there
            // was no scene that showed it, which is how it survived to
            // be reported from a live session instead of from a review.
            "maximized" => {
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
                    for g in row.groups.iter_mut() {
                        if g.id == group_id {
                            g.maximized = true;
                        }
                    }
                }
            }
            // Two half-width windows side by side, keyboard focus on the
            // right-hand one. The configuration the focus signal exists
            // for: nothing about the layout says which of the two takes
            // the next keystroke, so it has to be painted.
            "two-groups-focus" => {
                use shell::shell::state::{Tab, TileContent, WindowGroup, WindowWidth};
                self.with_chrome(|st| {
                    let mut sibling = None;
                    {
                        let mut tree = st.tree_mut();
                        let mut strip_id = None;
                        if let Some(ws) = tree.active_workspace_mut() {
                            if let Some(strip) = ws.strips.first_mut() {
                                for g in strip.window_groups.iter_mut() {
                                    g.width = WindowWidth::Fraction(500);
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
                                    width: WindowWidth::Fraction(500),
                                    maximized: false,
                                });
                                strip_id = Some(strip.id);
                                sibling = Some(gid);
                            }
                        }
                        // Focus the newly-added right-hand window, so the
                        // scene shows the signal moving off the default.
                        if let (Some(sid), Some(gid)) = (strip_id, sibling) {
                            tree.set_active_group(sid, gid);
                        }
                    }
                    st.bump();
                });
            }
            "multi-strip" => {
                // Three strips in the active workspace, with focus on
                // the middle one. Nothing in the previous catalogue had
                // more than one strip, so the strip ribbon had no scene
                // that showed it doing its job — which is also why the
                // absence of any strip affordance went unnoticed for so
                // long.
                use shell::shell::state::{
                    AppKind, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth,
                };
                self.with_chrome(|st| {
                    let mut focus = None;
                    {
                        let mut tree = st.tree_mut();
                        if let Some(ws) = tree.active_workspace_mut() {
                            for (n, name) in ["Build", "Review"].iter().enumerate() {
                                let sid = StripId(9200 + n as u64);
                                let mut strip = Strip::new(sid, (*name).to_string());
                                let gid = 9300 + n as u64;
                                let tid = 9400 + n as u64;
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
            other => return Err(format!("unknown scene '{other}'")),
        }
        Ok(())
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
        "notifications",
        "notifications-one",
        "notifications-dnd",
        "room-activity",
        "room-activity-all",
        "room-activity-named",
    ];

    /// Resolve a "named" UI target (button, pill, tab) to its on-screen
    /// rect in viewport-local coordinates. Returns `None` if the rect
    /// is `Rect::ZERO` (which generally means "this widget hasn't been
    /// drawn yet" — call `paint` once first to populate it).
    ///
    /// Resolution order (first non-zero match wins):
    /// 1. Hand-curated table — the five legacy keys that depend on
    ///    chrome-internal accessors that aren't reachable by tree walk.
    /// 2. Widget-tree registry — every widget with a `test_id` or
    ///    discoverable name/path key (see `widget_registry`).
    ///
    /// Names supported by the hand-curated table:
    ///   - `"add_workspace"` — the "+ Add workspace" tile in the overview
    ///     (only valid while the overview is open).
    ///   - `"ws_settings_tab[general|members|permissions]"` — modal
    ///     tab pills (only valid while ws-settings is open).
    ///
    /// Beyond those, scenarios can address any widget by its `test_id`
    /// (e.g. `command_palette_input`, `wheel_disc`) or by its path key
    /// (e.g. `ShellOverlayHost/TopBarChrome` if the widget didn't opt in
    /// to a `test_id`).
    pub fn resolve_named_rect(&self, name: &str) -> Option<Rect> {
        let host = self.fb.host();
        let curated = match name {
            "add_workspace" => Some(host.add_workspace_rect_for_tests()),
            "ws_settings_tab[general]" => Some(host.ws_settings_tab_pill_rect(WsTab::General)),
            "ws_settings_tab[members]" => Some(host.ws_settings_tab_pill_rect(WsTab::Members)),
            "ws_settings_tab[permissions]" => {
                Some(host.ws_settings_tab_pill_rect(WsTab::Permissions))
            }
            _ => None,
        };
        if let Some(r) = curated {
            if r.area() > 0.0 {
                return Some(r);
            }
            // Fall through to the registry — the curated key may have
            // missed because its prerequisite chrome state isn't set
            // (overview closed, ws-settings closed). Registry can still
            // succeed for an opt-in `test_id` regardless.
        }
        let registry = self.widget_registry();
        registry.get(name).copied().filter(|r| r.area() > 0.0)
    }

    /// Walk the widget tree from the overlay host and return a snapshot
    /// of every addressable widget's rect, keyed by:
    ///   - its `test_id()` if it opted in, OR
    ///   - a path key `Parent/Child[index]` reflecting the position in
    ///     the tree (index disambiguates siblings of the same name).
    ///
    /// The registry is **rebuilt on every call** — there's no hidden
    /// cache. Callers that need the registry across multiple actions
    /// in a scenario should re-fetch it after each `paint` because
    /// layout (and therefore rects) can shift between frames.
    ///
    /// Widgets at `Rect::ZERO` are still registered (a missing rect is
    /// information too — the caller can detect a widget that exists
    /// but isn't laid out yet). `resolve_named_rect` filters those out.
    pub fn widget_registry(&self) -> HashMap<String, Rect> {
        // Delegate to the shared walk in `compositor::frame` so the
        // out-of-process `compositor::test_control` and the in-process
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
pub fn key_press_event(key: Key) -> WindowEvent {
    let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = key.clone();
    ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    ev.location = KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    // `text` is `Option<SmolStr>` — for ASCII characters we mirror what
    // winit normally fills so the palette's character-input branch
    // sees the same glyph it would on real hardware.
    ev.text = if let Key::Character(s) = &key {
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
        d.press_key(Key::Named(NamedKey::Tab));
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

    /// End-to-end pin for the user's "drag-tab-out replaces instead of
    /// creating a sibling" report: with the bootstrap default's 4-tab
    /// Wide group, dragging the second tab pill (Files) past the source
    /// group's right edge and releasing in the empty strip area must
    /// produce TWO groups — the 3-tab source and a 1-tab new group —
    /// not one.
    ///
    /// The earlier `tab_drag_out_to_new_group` headless scenario used
    /// guessed coords (420, 80) → (1100, 80) which missed the actual
    /// pill rect (the leading icon column shifts pill 1 well past
    /// x=420 inside a Wide=800 group laid out from x≈224). This test
    /// reads the live `tab_rects()` off the bar so the press lands
    /// exactly on a pill regardless of layout drift.
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
        // Source kept 3 tabs, new group has 1.
        let counts: Vec<usize> = post_strip.1.iter().map(|(_, t)| t.len()).collect();
        assert!(
            counts.contains(&3) && counts.contains(&1),
            "expected one group with 3 tabs and one with 1 tab; got {:?}",
            counts,
        );

        // Both groups must be laid out — the user's visible "replace"
        // perception was the new group living at `Rect::ZERO` because
        // `GroupRow::structure_dirty` never bubbled up to `App` /
        // driver-level `relayout`. Pin both rects > 0 so a regression
        // would surface as a failure here, not as a silent invisible
        // sibling group at runtime.
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
    /// paint. The bootstrap default seeds a topbar inside the content
    /// chrome, so a registry walk should surface a `TopBarChrome` entry
    /// (either as a bare name or with a `[0]` suffix).
    ///
    /// Pinned so a future regression that breaks `ShellOverlayHost::
    /// children()` — the recursion entry-point — fails this test instead
    /// of silently returning an empty registry.
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
        // The host (root) should be there.
        assert!(
            registry.keys().any(|k| k.contains("ShellOverlayHost")),
            "registry should contain ShellOverlayHost; keys={:?}",
            registry.keys().collect::<Vec<_>>()
        );
        // And the topbar chrome should be reachable through the content
        // subtree (added in Addition B once host.children() forwards
        // content + sidebar + palette).
        assert!(
            registry.keys().any(|k| k.contains("TopBarChrome")),
            "registry should contain TopBarChrome; keys={:?}",
            registry.keys().collect::<Vec<_>>()
        );
    }

    /// `resolve_named_rect` must fall through to the widget tree when
    /// the hand-curated table misses. The CommandPalette widget opts in
    /// to `test_id() == "command_palette_input"` (a high-value target);
    /// after opening the palette, the resolver should return its rect
    /// even though `command_palette_input` is not in the hand-curated
    /// match arm.
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

    /// Regression test for the 2026-07-09 report: drag a tab into its
    /// own group, click that group's window body, then press Ctrl+K.
    /// The palette must:
    ///
    /// 1. Not make the sibling group disappear (both groups still laid
    ///    out with non-zero rects).
    /// 2. Scope to the CLICKED group's tab, not the group that happened
    ///    to be active before the detach.
    ///
    /// Reproduces the setup used by
    /// [`driver_drag_tab_out_creates_sibling_group`] then extends it
    /// with a body-click + Ctrl+K.
    #[test]
    fn detach_click_body_ctrl_k_scopes_to_clicked_tab() {
        let mut d = ShellDriver::new((1600, 900));
        let _ = d.paint();
        d.relayout();

        let strip_id = {
            let st = d.chrome();
            let tree = st.tree();
            let ws_id = tree.active;
            *tree.active_strip.get(&ws_id).expect("active strip")
        };
        // Drag Files (pill 1) out past the source group's right edge.
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

        // Two groups must exist now.
        let post = d.host().groups_per_strip();
        let strip = post
            .iter()
            .find(|(s, _)| *s == strip_id)
            .expect("strip present");
        assert_eq!(strip.1.len(), 2, "detach must produce two groups");

        // Find the group that contains the Files tab. Titles come from
        // the bootstrap default seed; the detached one is the singleton.
        let files_group_idx = strip
            .1
            .iter()
            .position(|(_, tabs)| tabs.len() == 1)
            .expect("singleton (detached) group");
        let notes_group_idx = 1 - files_group_idx;
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

        // Click the middle of the Files group's WINDOW BODY (well
        // below the tab bar so we don't hit a pill).
        let body_click = Point::new(
            (files_rect.x0 + files_rect.x1) / 2.0,
            files_rect.y0 + files_rect.height() * 0.6,
        );
        d.click_at(body_click);
        d.relayout();
        let _ = d.paint();
        d.relayout();

        // Sanity: click alone must not drop the group.
        let mid = d.host().groups_per_strip();
        let strip_mid = mid
            .iter()
            .find(|(s, _)| *s == strip_id)
            .expect("strip present after body click");
        assert_eq!(
            strip_mid.1.len(),
            2,
            "body click alone should not drop a group; got {:?}",
            strip_mid.1
        );

        // Fire Ctrl+K.
        d.set_modifiers(ModifiersState::CONTROL);
        d.press_key(Key::Character(winit::keyboard::SmolStr::new("k")));
        d.set_modifiers(ModifiersState::empty());
        let _ = d.paint();
        d.relayout();

        // Postcondition 1: the sibling group must still exist AND still
        // have a non-zero rect. The "window disappears" repro was the
        // group getting silently dropped or rendered at zero.
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
            assert!(
                r.area() > 0.0,
                "group[{i}] must still render (rect={:?})",
                r
            );
        }

        // Postcondition 2: the tree's active_tab must point at Files,
        // not the group that was active before the drag.
        let active_tab = d.chrome().active_tab_id();
        assert_eq!(
            active_tab,
            Some(files_tab_id),
            "Ctrl+K must scope to the tab in the CLICKED group; \
             expected files_tab_id={files_tab_id}, got {active_tab:?}"
        );
    }
}
