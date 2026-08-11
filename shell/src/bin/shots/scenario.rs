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
#[allow(dead_code)]
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
    /// Synthesise a `WindowEvent::MouseWheel { LineDelta(dx, dy) }`.
    /// The host's wheel handlers gate on `last_cursor`, so the caller
    /// is expected to have dispatched a recent `MoveCursor` to align
    /// the cursor over the target widget first.
    MouseWheel {
        dx: f32,
        dy: f32,
    },
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
    /// Type a literal string (one Key::Character per char, skipping
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
    /// Capture: render to PNG (offscreen mode) or dump state JSON
    /// (headless mode). Window mode treats this as a no-op (you can
    /// see it).
    Snapshot(&'static str),
    /// State-mutate shortcuts that bypass the event layer (handy for
    /// setup steps where dispatching real events would be brittle —
    /// e.g. opening the palette before typing a query).
    OpenPalette,
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
    /// Put the selection on the `idx`-th group of the active strip, or
    /// clear it entirely with `None` — which is the state rule 3 (place
    /// at a seam) is about.
    SelectGroup(Option<usize>),
    /// Cut the `idx`-th group of the active strip down to one tab, so a
    /// scenario can set up the "lone window selected" shape rather than
    /// the four-tab group the default workspace boots with.
    LoneWindow(usize),
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
                let per_slice = total / frames.max(1);
                let remainder = total - per_slice * frames;
                let prefix = last_snapshot.unwrap_or("frame");
                for i in 0..frames {
                    // Distribute the remainder over the first `remainder`
                    // slices so the total advance still equals `n`.
                    let extra = if i < remainder { 1 } else { 0 };
                    let advance = per_slice + extra;
                    if advance > 0 {
                        driver.step_frames(advance);
                    }
                    driver.relayout();
                    let name: &'static str =
                        Box::leak(format!("{}-frame{:02}", prefix, i).into_boxed_str());
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
    match action {
        Action::ClickAt(p) => driver.click_at(*p),
        Action::ClickNamed(name) => match driver.resolve_named_centre(name) {
            Some(p) => driver.click_at(p),
            None => eprintln!(
                "scenario: could not resolve named target '{name}' (rect not populated yet — paint first?)"
            ),
        },
        Action::Drag { from, to, steps } => driver.drag(*from, *to, *steps),
        Action::MouseWheel { dx, dy } => driver.mouse_wheel(*dx, *dy),
        Action::MoveCursor(p) => driver.move_cursor(*p),
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
                let name: &'static str = Box::leak(format!("{}-{:03}", prefix, i).into_boxed_str());
                sink.capture(driver, name);
            }
        }
        Action::Key(k) => driver.press_key(k.clone()),
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
            // Drive Ctrl+K through the host's keyboard branch instead
            // of `set_palette_visible(true)` directly — the hotkey path
            // calls `toggle_palette()` which also focuses the palette
            // input + clears the residual query (see ShellOverlayHost::
            // toggle_palette). Without that, subsequent `Type(_)`
            // actions would fall through because the palette ignores
            // characters when `!is_focused`.
            driver.set_modifiers(ModifiersState::CONTROL);
            driver.press_key(Key::Character(winit::keyboard::SmolStr::new("k")));
            driver.set_modifiers(ModifiersState::empty());
        }
        Action::OpenWindow { app_id, surface_id } => {
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
        }
        Action::AddHostedTab { app, title } => {
            let app = *app;
            let title = *title;
            driver.with_chrome(|st: &mut ChromeState| {
                {
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
                        + 1;
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
                }
                st.bump();
            });
        }
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
            for ms in at_ms.iter() {
                let mut any = false;
                for sid in &strips {
                    if let Some(row) = driver.host_mut().group_row_mut_for_tests(*sid) {
                        any |= row.hold_shift_at(std::time::Duration::from_millis(*ms));
                    }
                }
                if !any {
                    eprintln!(
                        "scenario: no make-space slide in flight to sample at {ms} ms"
                    );
                }
                driver.relayout();
                let name: &'static str =
                    Box::leak(format!("{prefix}-{ms:03}ms").into_boxed_str());
                sink.capture(driver, name);
            }
        }
        Action::SetWindowWidths(permille) => {
            let permille = *permille;
            driver.with_chrome(|st: &mut ChromeState| {
                {
                    let mut tree = st.tree_mut();
                    if let Some(ws) = tree.active_workspace_mut() {
                        for strip in ws.strips.iter_mut() {
                            for g in strip.window_groups.iter_mut() {
                                g.width =
                                    shell::shell::state::WindowWidth::Fraction(permille);
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
                    for g in row.groups.iter_mut() {
                        g.set_width(ui::widget::WindowWidth::Fraction(permille));
                    }
                }
            }
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
                                .and_then(|ws| ws.strips[0].window_groups.get(i).map(|g| g.id));
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
        Action::LoneWindow(idx) => {
            let idx = *idx;
            driver.with_chrome(|st: &mut ChromeState| {
                {
                    let mut tree = st.tree_mut();
                    if let Some(ws) = tree.active_workspace_mut() {
                        if let Some(g) = ws
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
                }
                st.bump();
            });
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
    let qs_level = match st.quick_settings.current_level() {
        QsLevel::Root => "root",
        QsLevel::Wifi => "wifi",
        QsLevel::Bluetooth => "bluetooth",
        QsLevel::WifiDetails(_) => "wifi-details",
        QsLevel::BluetoothDetails(_) => "bluetooth-details",
    };
    let ws_tab = match st.ws_settings_tab {
        WsTab::General => "general",
        WsTab::Members => "members",
        WsTab::Permissions => "permissions",
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
    // Groups-per-strip snapshot (drag-out tests): "[[[g_id,[t1,t2,...]],...],...]"
    // Outer array indexed by strip; each element is the strip's group list,
    // each group is `[group_id, [tab_ids...]]`. Order of strips matches
    // `host.groups_per_strip()` (HashMap iter order — not guaranteed
    // stable across runs, but each scenario's assertions look up the
    // count not a specific strip slot).
    let gps = host.groups_per_strip();
    out.push('"');
    out.push_str("groups_per_strip");
    out.push('"');
    out.push(':');
    out.push('[');
    for (si, (_strip_id, groups)) in gps.iter().enumerate() {
        if si > 0 {
            out.push(',');
        }
        out.push('[');
        for (gi, (gid, tabs)) in groups.iter().enumerate() {
            if gi > 0 {
                out.push(',');
            }
            out.push('[');
            out.push_str(&gid.to_string());
            out.push(',');
            out.push('[');
            for (ti, tid) in tabs.iter().enumerate() {
                if ti > 0 {
                    out.push(',');
                }
                out.push_str(&tid.to_string());
            }
            out.push(']');
            out.push(']');
        }
        out.push(']');
    }
    out.push(']');
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

impl<'a> SnapshotSink for JsonDirSink<'a> {
    fn capture(&mut self, driver: &mut ShellDriver, name: &str) {
        let path = self.dir.join(format!("{name}.json"));
        let json = chrome_state_json(driver);
        std::fs::write(&path, json).expect("write scenario state JSON");
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
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
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
        palette_typing(),
        palette_close_anim(),
        tab_drag(),
        tab_drag_out_to_new_group(),
        tab_drag_hover_line_indicator(),
        workspace_switch(),
        workspace_switch_animated(),
        wheel_scroll_workspaces(),
        strip_horizontal_scroll(),
        notes_click_and_type(),
        two_notes_tabs_keep_their_own_text(),
        cycle_all_tabs(),
        room_activity_from_palette(),
        window_open_beside_selection(),
        window_open_into_tabbed_group(),
        window_open_at_visible_seam(),
        window_open_at_next_seam_rightwards(),
        strip_scroll_across_apps(),
    ]
}

// =====================================================================
// The strip itself: several real apps along it, and the pan across them.
// =====================================================================

/// Lines of wheel delta per pan step. One `LineDelta` is 30 px in
/// `GroupRow::handle_event`, so this is 60 px of travel per step — small
/// enough that a recording reads as a pan rather than a cut.
const PAN_STEP_LINES: f32 = 2.0;

/// Pan steps in each direction. 16 × 60 px ≈ 960 px, which is a little
/// under the overflow four half-width windows produce on a 1600 px
/// desktop — so the pan runs the length of the strip without stranding
/// it in the empty canvas past the last window.
const PAN_STEPS: usize = 16;

/// Window width the strip is held at, in per-mille of the display's
/// usable width. Half a screen each puts two windows in view with a
/// third clipped at the edge.
const HALF_WIDTH_PERMILLE: u16 = 500;

/// Four shell-hosted apps side by side along the strip, then the strip
/// panned across them.
///
/// The scrollable strip is the compositor's whole spatial model, and
/// nothing else in the catalogue shows it doing its job: the placement
/// scenarios open *foreign* toplevels, which render as an empty
/// "waiting for first frame" card until a real client attaches a buffer,
/// and every hosted-app scenario has one window in the strip. So this
/// one opens Notes, Files, Terminal and Editor — each drawing its own
/// content in-process — as four separate windows, then wheels the strip
/// across them and back.
///
/// Each app is opened with the previous one selected and alone in its
/// group, which is the placement rule's first branch: a window opened
/// beside a lone window takes the column to its right. So the strip
/// grows rightwards one real window at a time, rather than the apps
/// stacking up as tabs of a single group.
///
/// Half-width windows are what make the pan legible — two windows on
/// screen at once, a third clipped at the right edge, and content
/// leaving the left edge as the strip moves.
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
        // focus always does — the starting position for the pan.
        Action::SelectGroup(Some(0)),
        Action::Wait(6),
        Action::Snapshot("05-strip-at-rest"),
        // Park the cursor over the strip — `GroupRow` gates its wheel
        // handler on the cursor being inside the row, and a `MouseWheel`
        // event carries no position of its own.
        Action::MoveCursor(Point::new(900.0, 520.0)),
    ];
    // Pan forward, then back. Each step is its own action so recording
    // mode encodes a frame per step: the strip's position is where the
    // wheel left it, so the motion survives the harness spending far
    // more wall time per frame than a real one costs.
    for _ in 0..PAN_STEPS {
        actions.push(Action::MouseWheel {
            dx: -PAN_STEP_LINES,
            dy: 0.0,
        });
    }
    actions.push(Action::Snapshot("06-panned-right"));
    for _ in 0..PAN_STEPS {
        actions.push(Action::MouseWheel {
            dx: PAN_STEP_LINES,
            dy: 0.0,
        });
    }
    actions.push(Action::Snapshot("07-panned-back"));
    Scenario {
        name: "strip_scroll_across_apps",
        description: "Open Notes, Files, Terminal and Editor as four windows along the strip, \
                      then wheel the strip across them and back.",
        actions,
    }
}

// =====================================================================
// Where a new window lands, and the motion that makes room for it.
// One scenario per branch of `shell::shell::placement`, each driving the
// production adopt path so the recording shows the rule, not a mock.
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
        description: "Search 'room activity' in the palette and open the sheet from the result.",
        actions: vec![
            Action::Snapshot("01-desktop"),
            Action::OpenPalette,
            Action::Wait(2),
            Action::Type("room activity".into()),
            Action::Wait(2),
            Action::Snapshot("02-palette-result"),
            Action::Key(Key::Named(NamedKey::Enter)),
            Action::Wait(20),
            Action::Snapshot("03-room-activity-open"),
        ],
    }
}

/// Repro for "apps render but can't be interacted with". Click in the
/// middle of the Notes tile (the active tab on the default workspace)
/// and type "hi". The pre-click snapshot shows the "Click to edit"
/// hint. The post-type snapshot is the proof: if interaction works
/// the hint reads "Type to edit · Ctrl+Z to undo" AND the typed text
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
            Action::AddHostedTab { app: "Notes", title: "Notes" },
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
/// the e2e_shell_flows::probe_tab_centers helper is the source of truth.
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
///   - sidebar wheel takes ~200 px of left padding via
///     `Column::with_extra_left_padding(SIDEBAR_WIDTH_PX)`
///   - the lone Wide group (800 px) sits roughly at x ∈ [224, 1024]
///   - tab pills live in the bar inside that group, around y≈80
///   - releasing at (1100, 80) is right of the source group → gap_idx=1
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
                // bar live around y=120 (topbar=40 + Column padding=24
                // + workspace_view + strip_view chrome ≈ 56 → bar
                // mid-y ≈ 120). Pill 1 (Files) sits at x≈424 with the
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
            Action::OpenWindow { app_id: "org.example.right", surface_id: 5001 },
            Action::Wait(20),
            // Windows at a third of the display each: a capture of windows
            // moving needs more than one of them on screen, and the
            // default width is the whole display.
            Action::SetWindowWidths(333),
            Action::SelectGroup(Some(0)),
            Action::Wait(4),
            Action::Snapshot("01-before"),
            Action::OpenWindow { app_id: "org.example.new", surface_id: 5002 },
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
            Action::OpenWindow { app_id: "org.example.tab", surface_id: 5003 },
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
            Action::OpenWindow { app_id: "org.example.second", surface_id: 5011 },
            Action::Wait(20),
            Action::OpenWindow { app_id: "org.example.third", surface_id: 5012 },
            Action::Wait(20),
            Action::SetWindowWidths(333),
            Action::SelectGroup(None),
            Action::Wait(4),
            Action::Snapshot("01-before"),
            Action::OpenWindow { app_id: "org.example.atseam", surface_id: 5013 },
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
            Action::OpenWindow { app_id: "org.example.second", surface_id: 5021 },
            Action::Wait(20),
            Action::OpenWindow { app_id: "org.example.third", surface_id: 5022 },
            Action::Wait(20),
            // Full width: the border after the window you are looking at
            // lands exactly on the screen's right edge, so none of the
            // space between windows is on screen.
            Action::SetWindowWidths(1000),
            Action::SelectGroup(None),
            Action::Wait(4),
            Action::Snapshot("01-before"),
            Action::OpenWindow { app_id: "org.example.nextseam", surface_id: 5023 },
            Action::SampleMakeSpace {
                prefix: "02-next-seam",
                at_ms: MAKE_SPACE_SAMPLES,
            },
            Action::Wait(PLACEMENT_SETTLE_FRAMES),
            Action::Snapshot("03-settled"),
        ],
    }
}

/// Capture every frame of the workspace-slide animation. After
/// triggering a switch we snapshot N frames in a row with 1 animation
/// tick between each so the slide ease is visible. `WS_SLIDE_MS = 260`,
/// so 18 frames at 16 ms each (288 ms) covers the slide + a touch of
/// post-settle.
fn workspace_switch_animated() -> Scenario {
    Scenario {
        name: "workspace_switch_animated",
        description: "Per-frame capture of the workspace-slide animation (ws 0 → ws 1).",
        actions: vec![
            Action::Snapshot("00-rest"),
            Action::SwitchWorkspace(1),
            Action::SnapshotSequence {
                prefix: "slide",
                frames: 18,
                gap_frames: 1,
            },
        ],
    }
}

/// Drive the wheel disc via MouseWheel events over its hit area and
/// capture frame-by-frame so the user can verify the discrete-detent
/// click_to_spoke animation is visible (one workspace per scroll burst).
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
            // 128 px > the 120-px detent → fires one click_to_spoke.
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

/// Drive the strip's MouseWheel path: cursor over the active tile,
/// fire horizontal-dominant wheel events, capture before / after.
/// On a single-group strip the strip pans into empty wallpaper space
/// past the rightmost group (Niri-style infinite canvas). The
/// snapshot should show the white tile shifted leftward in the
/// viewport (revealing wallpaper to the right of where it was).
fn strip_horizontal_scroll() -> Scenario {
    Scenario {
        name: "strip_horizontal_scroll",
        description: "Fire a horizontal-dominant wheel over the strip and assert the active \
             tab cycled (visible in the topbar's pill highlight).",
        actions: vec![
            // Cursor over the active tile / strip area (well below the
            // topbar's 40 px band and well to the right of the 320 px
            // sidebar gutter).
            Action::MoveCursor(Point::new(900.0, 500.0)),
            Action::Snapshot("00-before"),
            // Horizontal swipe that pans the strip forward (reveals
            // content to the right). With the natural sign convention a
            // swipe-left (dx<0) increases scroll_x, so a negative dx is
            // what moves the strip off its leftmost (clamped-to-0)
            // resting position. 10 LineDelta units × 30 px = 300 px of
            // pan, well inside the viewport*3 upper clamp.
            Action::MouseWheel { dx: -10.0, dy: 0.0 },
            Action::Wait(2),
            Action::Snapshot("01-after-pan"),
            // And the reverse direction — should bring the strip back to
            // the leftmost position (clamped to 0).
            Action::MouseWheel { dx: 10.0, dy: 0.0 },
            Action::Wait(2),
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
fn workspace_switch() -> Scenario {
    Scenario {
        name: "workspace_switch",
        description: "Switch to workspace index 1, then back to 0.",
        actions: vec![
            Action::Snapshot("01-ws0"),
            Action::SwitchWorkspace(1),
            // Wait long enough for the workspace-slide animation
            // (`WS_SLIDE_MS = 260 ms` in
            // `quick-shell::shell::overlay_host`) to settle. 20 frames
            // × 16 ms = 320 ms covers it with margin; anything shorter
            // snapshots the content widget mid-translate, leaving the
            // viewport with stale offsets and painting voids where the
            // slide hasn't yet finished bringing the new workspace
            // back to the origin.
            Action::Wait(20),
            Action::Snapshot("02-ws1"),
            Action::SwitchWorkspace(0),
            Action::Wait(20),
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
        // workspace_switch slides the content widget over WS_SLIDE_MS = 260 ms
        // (see shell::shell::overlay_host). Waits must cover that.
        let ws = workspace_switch();
        let ws_waits: Vec<u32> = ws
            .actions
            .iter()
            .filter_map(|a| match a {
                Action::Wait(n) => Some(*n),
                _ => None,
            })
            .collect();
        assert!(
            !ws_waits.is_empty(),
            "workspace_switch must contain Wait actions"
        );
        // 16 ms per frame × frames must cover 260 ms slide.
        for n in &ws_waits {
            assert!(
                u64::from(*n) * 16 >= 260,
                "workspace_switch Wait({n}) is shorter than WS_SLIDE_MS=260; snapshot will be mid-slide"
            );
        }

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
                .map(|row| (row.scroll_x(), row.max_scroll_x()))
                .unwrap_or((f64::NAN, f64::NAN));
            self.seen.push((name.to_string(), scroll_x, max_scroll_x));
        }
    }

    /// The strip scenario has to end up with four *windows*, each one a
    /// live app — not four tabs of one window, and not stub cards.
    #[test]
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

    /// And it has to actually pan: the strip holds more than fits, the
    /// wheel moves it off its resting position, and wheeling back returns
    /// it home. Read off the live `GroupRow` the production wheel handler
    /// writes to.
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
            "wheeling {PAN_STEPS} steps of {PAN_STEP_LINES} lines must carry the strip \
             most of the way along it (scroll_x = {panned_x})"
        );
        let (back_x, _) = at("07-panned-back");
        assert!(
            back_x < 1.0,
            "wheeling the same distance back returns the strip home (scroll_x = {back_x})"
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
