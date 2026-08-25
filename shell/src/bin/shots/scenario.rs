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

use vello::kurbo::{Point, Rect};
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
    /// Secondary-click at raw screen coordinates — the gesture that opens a
    /// context menu, and beside it the "say something about this" surface.
    SecondaryClickAt(Point),
    /// Open the compose surface about a named object, beside a rect given in
    /// the coordinates of the window at `group_fraction` across the strip.
    ///
    /// The subject is synthetic. What this captures is *placement* — that the
    /// panel is put against the display rather than against the window, and is
    /// painted over the application next door — and the address it carries is
    /// not what decides that. Which file it is about is proved where it is
    /// decided: in the file manager's own tests, on a real right-click.
    ///
    /// `beside` is a rect in the asking window's own coordinates, exactly as an
    /// application reports its context menu, so the conversion under test is
    /// the production one.
    ComposeRemarkBeside {
        /// Which hosted window is asking, as a fraction across the strip: 0.0
        /// is the leftmost, 1.0 the rightmost.
        window: f64,
        /// The menu rect, in that window's own coordinates.
        beside: Rect,
        /// What the chip says. A label, never an address.
        label: &'static str,
    },
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
    /// Open a window the way the running compositor really does: adopt the
    /// toplevel into the *protocol* tree, then mirror that tree into the
    /// chrome's with `foreign::reconcile_foreign_tabs`.
    ///
    /// [`Action::OpenWindow`] writes straight into the chrome's tree, so it
    /// depicts where a window lands but never exercises the mirror. The two
    /// trees are the thing worth photographing: they move their active
    /// workspace independently, and the mirror deciding what "already
    /// adopted" means across that gap is what once put a second copy of an
    /// application on a desktop nobody had opened it twice on.
    ///
    /// The protocol tree persists across the actions of one scenario, as the
    /// compositor's does across a session.
    MirrorWindow {
        app_id: &'static str,
        surface_id: u64,
    },
    /// Run the mirror again with no new window. Pair it with
    /// [`Action::SwitchWorkspace`]: this is the pass the compositor makes
    /// every frame, and the one that used to adopt a second time.
    MirrorAgain,
    /// [`Action::OpenWindow`], stopped at the moment before the client
    /// has anything to show.
    ///
    /// Same adoption, same placement rule; what it leaves out is the
    /// stand-in buffer. A toplevel that has been adopted and has not
    /// presented is what the compositor holds a window's place for, and
    /// past `foreign::OPENING_DELAY` the tile says so on a card. That
    /// card is only reachable this way — every hosted app in the
    /// catalogue is drawn in-process and is never "on its way" — so
    /// without it the one surface a person looks at while they wait
    /// cannot be photographed at all, which is how it shipped with its
    /// title at about 1.05:1 against its own ground.
    ///
    /// Follow it with enough `Wait` to clear the delay.
    OpenWindowStillStarting {
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
    /// Sample the strip's own travel at fixed points along its curve and
    /// capture each as `prefix-<ms>`.
    ///
    /// The viewport pan is the shortest motion the shell plays and the
    /// most expensive one to photograph: a capture costs more wall time
    /// than the whole curve lasts, so every still of a "moving" strip is
    /// really a still of a stopped one, and whether the view travelled
    /// or cut cannot be seen in the output at all. This holds the pan at
    /// a chosen point and renders the production `draw` there — the same
    /// escape [`Action::SampleMakeSpace`] gives the make-space slide.
    SamplePan {
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
    /// The real trigger is a media key, and a `--shots` render has no
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
    /// The real trigger is a sandboxed app calling
    /// `org.freedesktop.impl.portal.FileChooser`, which a `--shots`
    /// render has no session bus to carry — so the dialog would never
    /// appear in a capture, which is exactly where its styling wants
    /// reviewing. This pushes a `FilePickerRequest` onto the same
    /// `FilePickerQueue` the D-Bus backend pushes to, and turns the same
    /// `pump_file_picker` the compositor turns every frame. Everything
    /// downstream of the queue is the production path.
    SeedFilePicker(&'static str),
    /// Advance the open dialog's file-type filter, the way clicking its
    /// filter chip does. A filter that matches nothing in the folder is
    /// how the dialog's empty state is reached without needing an empty
    /// folder to point it at, and it is a state a person hits in normal
    /// use — pick Images in a folder of notes.
    CycleFilePickerFilter,
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
    /// Record a crossing between two applications, the way the door
    /// records one when an agent performs an action that carries content
    /// out of one window and into another.
    ///
    /// The real trigger is an agent invoking an action whose argument is a
    /// content hash, which a `--shots` render has no agent and no room to
    /// carry — so the motion that matters most would be the one thing a
    /// capture could never show. This puts real bytes in the blob store and
    /// calls the same two functions the production path calls, so
    /// everything downstream — resolving each application to its window,
    /// panning the strip, drawing the object — is the code that runs live.
    SeedActionFlight {
        /// The application the content came from, and the one whose action
        /// was addressed. Both are matched against the strip's real windows.
        from_app: &'static str,
        to_app: &'static str,
    },
    /// Record a crossing the way a participant **without windows** produces
    /// one: by reporting it over the collaboration socket.
    ///
    /// [`Action::SeedActionFlight`] above is the other path — an application
    /// on this machine offered the bytes, so the session watched the content
    /// leave and worked the pair out for itself. An agent produces no such
    /// offer, and the recording that matters is of *its* work, so this drives
    /// the branch the agent actually takes.
    ///
    /// Not by calling the recording function. A real broker is bound on a
    /// real socket, a client connects and announces itself the way the agent
    /// does, and the frame the agent sends is written to it — so what starts
    /// the motion is a frame decoded off a socket by the compositor's own
    /// reader, with the actor resolved from the connection rather than
    /// supplied. Everything downstream is the live code either way.
    ReportedActionFlight {
        /// The application the content came from, and the one whose action
        /// was addressed. Both are matched against the strip's real windows.
        from_app: &'static str,
        to_app: &'static str,
    },
    /// Sample a crossing at fixed points along its curve, capturing each as
    /// `prefix-<ms>`.
    ///
    /// One offscreen frame costs far more wall time than the crossing
    /// lasts, so stepping frames only ever catches an object that has not
    /// left and one that has already arrived — the two moments that prove
    /// least. This holds the production curve at a chosen millisecond
    /// instead, the same escape `SampleMakeSpace` gives the strip's slide.
    SampleActionFlight {
        prefix: &'static str,
        at_ms: &'static [u64],
    },
    /// Open a hosted Photos tab showing `path`, the way opening a file does:
    /// the path is written into the app's own document and the next `sync`
    /// decodes it. Nothing here reaches past the app's own public surface.
    OpenPhotograph {
        title: &'static str,
        path: &'static str,
    },
    /// Put an agent in the active room, with the standing to act in it.
    ///
    /// Without it every address it reaches for is refused by the capability
    /// gate, and the roster has nothing to call it — so the frames would show
    /// a desktop where nothing happened, correctly and uselessly.
    SeedAgent {
        id: &'static str,
        name: &'static str,
        /// The applications it may act in, by the namespace an address is
        /// gated under (`photos.image:…` is gated as `photos`).
        apps: &'static [&'static str],
    },
    /// Perform one of the focused app's exported operations, through the
    /// **production** action catalogue — the same registry, gate and door an
    /// agent reaches over the control socket.
    ///
    /// `id` is the address as a caller spells it: `<resource id>#<action id>`.
    /// Nothing is simulated: the address is resolved against a freshly built
    /// catalogue, the capability gate runs, and the app's own registry decodes
    /// and applies the action to its own document. If any of that refuses, the
    /// scenario says so and captures nothing — a mark drawn for an operation
    /// that did not happen is the one outcome worse than no mark.
    InvokeAppAction {
        id: &'static str,
        argument: Option<&'static str>,
        /// Who is asking. `None` is the person at the keyboard; a name is an
        /// agent, and resolves both the capabilities the gate checks and the
        /// identity the change is committed and attributed under.
        agent: Option<&'static str>,
    },
    /// Sample the mark on a touched resource at fixed points along its span,
    /// capturing each as `prefix-<ms>`.
    ///
    /// Same escape as [`Action::SampleActionFlight`] and for the same reason:
    /// one offscreen frame costs more wall time than the whole span, so
    /// stepping frames catches only the two ends — which are precisely the
    /// moments the mark is invisible.
    SampleActionTouch {
        prefix: &'static str,
        at_ms: &'static [u64],
    },
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

/// The compositor's protocol-side workspace tree, for the scenarios that
/// exercise the mirror between it and the chrome's.
///
/// One tree for the process, as the compositor has one for the session.
fn protocol_tree<R>(f: impl FnOnce(&mut shell::shell::state::WorkspaceTree) -> R) -> R {
    thread_local! {
        static PROTOCOL: RefCell<shell::shell::state::WorkspaceTree> =
            RefCell::new(shell::shell::state::WorkspaceTree::bootstrap_default());
    }
    PROTOCOL.with(|t| f(&mut t.borrow_mut()))
}

/// Run the production mirror once, protocol tree into chrome tree, bumping
/// the chrome when it changed anything — the two calls the compositor's
/// frame loop makes, in the order it makes them.
fn mirror_into_chrome(driver: &mut ShellDriver) {
    driver.with_chrome(|st: &mut ChromeState| {
        let changed = protocol_tree(|protocol| {
            let mut rendered = st.tree_mut();
            shell::compositor::foreign::reconcile_foreign_tabs(protocol, &mut rendered)
        });
        if changed {
            st.bump();
        }
    });
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
        Action::SecondaryClickAt(p) => driver.secondary_click_at(*p),
        Action::ComposeRemarkBeside {
            window,
            beside,
            label,
        } => {
            // Lay out first: the window rects this reads are the ones the last
            // pass produced, and a scenario that opened a window on the
            // previous action has not had one yet.
            driver.relayout();
            let mut rects: Vec<Rect> = driver
                .host()
                .group_hit_candidates_for_capture()
                .into_iter()
                .map(|(_, _, r)| r)
                .filter(|r| r.area() > 0.0)
                .collect();
            rects.sort_by(|a, b| a.x0.total_cmp(&b.x0));
            if rects.is_empty() {
                eprintln!("scenario: no hosted window to open a compose surface beside");
                return;
            }
            let idx = (((rects.len() - 1) as f64) * window.clamp(0.0, 1.0)).round() as usize;
            let origin = rects[idx].origin();
            let reference = qos_reference::ResourceRef::whole("files.entry", "shot:subject", *label);
            if !driver.host_mut().open_remark(reference, *beside, origin) {
                eprintln!("scenario: the compose surface opened nothing — no destinations?");
            }
        }
        Action::ClickNamed(name) => match driver.resolve_named_centre(name) {
            Some(p) => driver.click_at(p),
            None => eprintln!(
                "scenario: could not resolve named target '{name}' (rect not populated yet — paint first?)"
            ),
        },
        Action::Drag { from, to, steps } => driver.drag(*from, *to, *steps),
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
                let px = (w as usize) * (h as usize) * 4;
                shell::compositor::foreign::store().publish_rgba(
                    surface_id,
                    &vec![0xF4; px],
                    w,
                    h,
                );
            }
        }
        Action::MirrorWindow { app_id, surface_id } => {
            let app_id = *app_id;
            let surface_id = *surface_id;
            driver.with_chrome(|st: &mut ChromeState| {
                let tree = st.tree();
                shell::shell::placement::note_placement_for_launch(
                    app_id,
                    &tree,
                    st.strip_viewport(),
                );
            });
            // The protocol side adopts first — that is where a toplevel
            // actually arrives — and the mirror carries it across.
            protocol_tree(|protocol| {
                shell::compositor::tab_binding::adopt_toplevel(
                    protocol,
                    surface_id,
                    Some(app_id.to_string()),
                );
            });
            mirror_into_chrome(driver);
            driver.relayout();
            // Stand in for the client, as `OpenWindow` does: an adopted
            // toplevel with no process behind it has drawn nothing, and a
            // window that has drawn nothing is not painted.
            if let Some((w, h)) = shell::compositor::foreign::store().viewport(surface_id)
                && w > 0
                && h > 0
            {
                let px = (w as usize) * (h as usize) * 4;
                shell::compositor::foreign::store().publish_rgba(
                    surface_id,
                    &vec![0xF4; px],
                    w,
                    h,
                );
            }
        }
        Action::MirrorAgain => {
            mirror_into_chrome(driver);
            driver.relayout();
        }
        Action::OpenWindowStillStarting { app_id, surface_id } => {
            let app_id = *app_id;
            let surface_id = *surface_id;
            driver.with_chrome(|st: &mut ChromeState| {
                {
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
            // No stand-in buffer, deliberately: the tile has to stay in
            // the state this scenario is about. The layout still runs, so
            // the card has a rect to be centred in.
            driver.relayout();
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
            let from = match in_flight {
                Some(p) => p,
                None => {
                    // A third of the way in from the rim, vertically
                    // centred: inside the arc at every rotation, and
                    // clear of the add-workspace button at the bottom.
                    let p =
                        Point::new(rect.x0 + rect.width() * 0.66, (rect.y0 + rect.y1) / 2.0);
                    driver.move_cursor(p);
                    driver.press();
                    p
                }
            };
            let n = (*steps).max(1);
            for s in 1..=n {
                let t = s as f64 / n as f64;
                driver.move_cursor(Point::new(from.x, from.y + dy * t));
                // One frame per increment so the follow spring actually
                // integrates — with no frames between moves the rim
                // never gets anywhere and the drag is a teleport.
                driver.step_frames(1);
            }
        }
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
            for ms in at_ms.iter() {
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
                let name: &'static str =
                    Box::leak(format!("{prefix}-{ms:03}ms").into_boxed_str());
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
            for ms in at_ms.iter() {
                let mut any = false;
                for sid in &strips {
                    if let Some(row) = driver.host_mut().group_row_mut_for_tests(*sid) {
                        for g in row.groups.iter_mut() {
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
        Action::SeedActionFlight { from_app, to_app } => {
            let png = flight_test_content();
            let hash = qdoc::blobstore::put(&png);
            shell::shell::action_flight::note_offer(&hash, from_app);
            if !shell::shell::action_flight::note_flight("claude", &hash, to_app) {
                eprintln!(
                    "scenario: nothing recorded - {from_app} never offered the \
                     content, or it is the same application as {to_app}"
                );
            }
            // `update` is what promotes a record into a crossing: it is the
            // pass that can ask where each application's window is, and the
            // one that starts the pan toward the destination.
            driver.step_frames(1);
            driver.relayout();
        }
        Action::ReportedActionFlight { from_app, to_app } => {
            let png = flight_test_content();
            let hash = qdoc::blobstore::put(&png);
            report_flight_over_a_socket(&hash, from_app, to_app);
            // `update` is what promotes a record into a crossing: it is the
            // pass that can ask where each application's window is, and the
            // one that starts the pan toward the destination.
            driver.step_frames(1);
            driver.relayout();
        }
        Action::OpenPhotograph { title, path } => {
            let title = *title;
            let path = *path;
            // A window of its own, through the production launch path — the
            // same one the launcher and the palette take. A tab appended to
            // whatever group happened to be first would put the photograph
            // behind somebody else's window, which is not how opening a file
            // behaves and not what a viewer should be shown.
            apply(
                driver,
                &Action::OpenHostedWindow {
                    app: "Photos",
                    title,
                },
                sink,
            );
            // Frames, not a bare relayout: the widget the tab names is built
            // by the per-tick reconciler, which only a real update pass runs.
            // Before it has, there is no app to hand the file to.
            driver.step_frames(3);
            driver.relayout();
            if !driver.open_photograph(path) {
                eprintln!(
                    "scenario: no hosted Photos window took {path}; the frames \
                     after this show an empty viewer",
                );
            }
            // And a real pump after, so `sync` decodes the file and the
            // viewer lays the photograph out — which is what gives the
            // resource a rect to be marked on. `step_frames` alone advances
            // animation and never calls `sync`, so the viewer would sit on
            // its empty state with the document plainly naming a file.
            for _ in 0..3 {
                driver.pump_apps();
                driver.relayout();
                let _ = driver.paint();
            }
            // One more paint, thrown away. The first frame that carries a
            // decoded photograph is the frame vello discovers its per-frame
            // buffers are too small on: it grows them and renders the *next*
            // one correctly, but this one comes out an empty white rectangle
            // and is written to disk without complaint. Painting once here
            // means the capture that follows is the grown frame.
            let _ = driver.paint();
        }
        Action::SeedAgent { id, name, apps } => driver.seed_agent(id, name, apps),
        Action::InvokeAppAction {
            id,
            argument,
            agent,
        } => {
            let id = *id;
            // A pump first: the catalogue is built from what the focused app
            // publishes *now*, and an app that has not synced has not decoded
            // its file and publishes nothing.
            driver.pump_apps();
            driver.relayout();
            match driver.invoke_app_action(id, *argument, *agent) {
                Ok(()) => {
                    // `update` is the pass that promotes a record into a mark:
                    // it is the one that can ask the room what the participant
                    // is called and whether it is an agent. And the app has to
                    // re-render the photograph the action just changed, which
                    // is `sync`'s half.
                    driver.pump_apps();
                    driver.relayout();
                }
                Err(e) => eprintln!(
                    "scenario: {id} was not performed ({e}); there is nothing \
                     for the next frames to draw",
                ),
            }
        }
        Action::SampleActionTouch { prefix, at_ms } => {
            for ms in at_ms.iter() {
                if !shell::shell::action_touch::hold_at(
                    std::time::Duration::from_millis(*ms),
                ) {
                    eprintln!("scenario: no touch on screen to sample at {ms} ms");
                }
                driver.relayout();
                let name: &'static str =
                    Box::leak(format!("{prefix}-{ms:03}ms").into_boxed_str());
                sink.capture(driver, name);
            }
        }
        Action::SampleActionFlight { prefix, at_ms } => {
            for ms in at_ms.iter() {
                if !shell::shell::action_flight::hold_at(
                    std::time::Duration::from_millis(*ms),
                ) {
                    eprintln!("scenario: no crossing in flight to sample at {ms} ms");
                }
                driver.relayout();
                let name: &'static str =
                    Box::leak(format!("{prefix}-{ms:03}ms").into_boxed_str());
                sink.capture(driver, name);
            }
        }
        Action::SamplePan { prefix, at_ms } => {
            // Catch the pan while it is still in flight, which takes a
            // couple of frames to become true: the frame a window is
            // spliced in is the one whose layout pass notices the strip
            // is pointed at the wrong place, and the shell's focus
            // reconciler aims at the new window on the frame after that
            // — the first with a laid-out rect to aim at. Freezing takes
            // the pan out of the clock's reach, so as soon as one is
            // running it does not matter how slow the capture is.
            let mut held = false;
            let mut strips: Vec<_> = Vec::new();
            for _ in 0..4 {
                driver.step_frames(1);
                driver.relayout();
                strips = {
                    let st = driver.chrome();
                    let tree = st.tree();
                    tree.active_workspace()
                        .map(|ws| ws.strips.iter().map(|s| s.id).collect())
                        .unwrap_or_default()
                };
                for sid in &strips {
                    if let Some(row) = driver.host_mut().group_row_mut_for_tests(*sid) {
                        held |= row.hold_pan_at(std::time::Duration::ZERO);
                    }
                }
                if held {
                    break;
                }
            }
            if !held {
                // Said out loud rather than captured quietly: a resting
                // strip photographed as a frame of motion is exactly the
                // false pass this action exists to stop.
                eprintln!(
                    "scenario: no strip pan in flight for '{prefix}' — the frames \
                     below are of a strip standing still, not of one moving"
                );
            }
            for ms in at_ms.iter() {
                for sid in &strips {
                    if let Some(row) = driver.host_mut().group_row_mut_for_tests(*sid) {
                        row.hold_pan_at(std::time::Duration::from_millis(*ms));
                    }
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
            driver
                .host_mut()
                .set_portal_queues(PromptQueueHandle::new(PromptResolver::Channel), queue.clone());

            let save = mode == "save";
            let caller = PortalCaller::new(WorkspaceId(1), "dev.qos.word-processor");
            let opts = FilePickerOptions::new(
                caller,
                if save { "Save Document" } else { "Open Document" },
                save,
            )
            .multiple(mode == "open_multiple")
            .filters(vec![
                PickerFilter::from_pairs(
                    "Documents",
                    &[(0, "*.odt".into()), (0, "*.md".into()), (0, "*.txt".into())],
                ),
                PickerFilter::from_pairs("Images", &[(1, "image/png".into())]),
                PickerFilter::from_pairs("Spreadsheets", &[(0, "*.ods".into())]),
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
                multiple: opts.multiple,
                accept_label: opts.accept_label,
                current_name: opts.current_name,
                filters: opts.filters,
                reply: tx,
            });
            driver.host_mut().pump_file_picker();
        }
        Action::CycleFilePickerFilter => {
            if let Some(picker) = driver.host_mut().file_picker_mut() {
                picker.cycle_filter();
            }
            driver.host_mut().pump_file_picker();
        }
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
                        p.selection =
                            ui::text_selection::Selection::collapsed(p.password.len());
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
    out.push(',');
    // Where along each strip the viewport is sitting, and how far it
    // could travel. Two frames with an identical tree can be showing
    // completely different windows, so a capture that carries the tree
    // and not the pan cannot answer "what am I looking at" — and a
    // scenario about how the view moves has no subject without it.
    // `max_scroll_x` rides along because a strip whose windows all fit
    // is pinned at zero, and there "the view did not move" is true of
    // every possible change and proves nothing.
    out.push('"');
    out.push_str("strip_scroll");
    out.push('"');
    out.push(':');
    out.push('[');
    for (i, (strip, scroll_x, max_scroll_x)) in host.strip_scroll_offsets().iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"strip\":{strip},\"scroll_x\":{scroll_x:.2},\"max_scroll_x\":{max_scroll_x:.2}}}"
        ));
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
/// Encoded bytes for the object a seeded crossing carries.
///
/// Generated rather than shipped as a file, so a capture is deterministic
/// offline. Deliberately **not** symmetric: a horizon two-fifths down and a
/// sun left of centre give the image an unambiguous up, left and right, so a
/// frame sampled mid-flight proves the object is being drawn the right way
/// round and at the right size — which a flat colour or a centred blob would
/// not. A photograph is what really crosses here, and the one property that
/// has to survive the trip is being recognisable.
fn flight_test_content() -> Vec<u8> {
    const W: u32 = 480;
    const H: u32 = 320;
    let mut rgba = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let fx = x as f32 / W as f32;
            let fy = y as f32 / H as f32;
            let horizon = 0.42;
            let (r, g, b) = if fy < horizon {
                let t = fy / horizon;
                (
                    (32.0 + 150.0 * t) as u8,
                    (86.0 + 110.0 * t) as u8,
                    (168.0 + 60.0 * t) as u8,
                )
            } else {
                let t = (fy - horizon) / (1.0 - horizon);
                (
                    (18.0 + 40.0 * t) as u8,
                    (58.0 + 62.0 * t) as u8,
                    (112.0 - 40.0 * t) as u8,
                )
            };
            let dx = fx - 0.26;
            let dy = fy - 0.22;
            let sun = (dx * dx + dy * dy * 2.25).sqrt();
            let (r, g, b) = if sun < 0.10 { (255, 224, 152) } else { (r, g, b) };
            rgba.extend_from_slice(&[r, g, b, 255]);
        }
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, W, H);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().expect("png header");
        writer.write_image_data(&rgba).expect("png body");
    }
    out
}
/// Bind the compositor's own broker, connect to it, and send the frame an
/// agent sends when it has carried content from one application into another.
///
/// The point of doing it this way rather than calling the recording function
/// is what sits in between: the frame is serialised, written to a socket,
/// read back by the broker's reader thread, decoded, and attributed to the
/// connection it arrived on. A capture made after all of that is a capture of
/// the path the agent takes.
///
/// Panics rather than warns on every failure. A recording rig that quietly
/// captured a desktop with nothing crossing it would produce frames that look
/// like the feature is broken, and somebody would spend a day on the renderer.
fn report_flight_over_a_socket(hash: &str, from_app: &str, to_app: &str) {
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, Instant};

    let before = shell::shell::action_flight::pending_len();

    // Short, because an AF_UNIX path over about 108 bytes cannot be bound and
    // the failure reads as something else entirely.
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("qos-shots");
    std::fs::create_dir_all(&dir).expect("a directory to bind the broker socket in");
    let sock = dir.join(format!("flight-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&sock);

    let broker = shell::shell::collab_broker::spawn_full(sock.clone(), None, None)
        .expect("the broker binds its socket");

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut stream = loop {
        match UnixStream::connect(&sock) {
            Ok(s) => break s,
            Err(e) => {
                assert!(Instant::now() < deadline, "the broker never bound {sock:?}: {e}");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    };

    let mut send = |req: &ui::collaboration::CollabRequest| {
        let body = serde_json::to_vec(req).expect("encode the frame");
        stream
            .write_all(&(body.len() as u32).to_le_bytes())
            .and_then(|_| stream.write_all(&body))
            .expect("write the frame to the broker");
    };
    // Announced the way the agent announces: no app id, so the connection is
    // the room's rather than one application's, and a handle, which is what
    // the session resolves the crossing's actor from when the room has no
    // label of its own for the connection.
    send(&ui::collaboration::CollabRequest::Hello {
        app_id: String::new(),
        cluster_id: uuid::Uuid::new_v4(),
        record: None,
        handle: Some("Claude".to_string()),
    });
    send(&ui::collaboration::CollabRequest::CarriedContent {
        hash: hash.to_string(),
        from_app: from_app.to_string(),
        into_app: to_app.to_string(),
    });

    let deadline = Instant::now() + Duration::from_secs(5);
    while shell::shell::action_flight::pending_len() == before {
        assert!(
            Instant::now() < deadline,
            "the broker read the report and recorded no crossing from {from_app} \
             into {to_app}; there is nothing for the next frames to draw",
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    drop(broker);
    let _ = std::fs::remove_file(&sock);
}

pub fn catalogue() -> Vec<Scenario> {

    // Two applications side by side and an object carried from one into
    // the other, sampled across the whole crossing. This is the shape the
    // film turns on, so it is captured as a sequence rather than a still:
    // a single frame cannot show that the object is in flight, and in
    // flight is the only interesting thing about it.
    let mut out: Vec<Scenario> = Vec::new();
    out.push(Scenario {
        name: "action-flight",
        description: "an object an agent moved, crossing from the window it \
                      came from into the one its action placed it in",
        actions: vec![
            Action::LoneWindow(0),
            Action::OpenWindow { app_id: "dev.qos.photos", surface_id: 9101 },
            Action::OpenWindow { app_id: "dev.qos.word-processor", surface_id: 9102 },
            Action::SetWindowWidths(440),
            Action::Wait(6),
            Action::Relayout,
            // Frame the application the object leaves from. A crossing
            // whose source is off the edge starts with a photograph
            // arriving out of nothing, which says less than it looks like
            // it does — the demo's own note about dragging between
            // neighbours is the same observation.
            Action::SelectGroup(Some(0)),
            Action::Wait(10),
            Action::Relayout,
            Action::Snapshot("action-flight-before"),
            Action::SeedActionFlight {
                from_app: "dev.qos.photos",
                to_app: "word-processor",
            },
            Action::SampleActionFlight {
                prefix: "action-flight",
                at_ms: &[0, 90, 180, 270, 360, 450, 540, 630],
            },
        ],
    });
    // The same crossing with the destination off the edge of the screen,
    // which is the case the strip makes possible and most desktops cannot
    // have. One window fills the display, so the application the object is
    // going to is not on it when the action lands. The desktop has to travel
    // to where the thing is going, and the two captures either side of the
    // seed are what show that it did — and that it did so *because* of the
    // crossing, rather than jiggling for no reason a viewer can see.
    out.push(Scenario {
        name: "action-flight-offscreen",
        description: "a crossing whose destination is off the edge, so the \
                      strip travels to it as the object does",
        actions: vec![
            Action::LoneWindow(0),
            Action::OpenWindow { app_id: "dev.qos.photos", surface_id: 9201 },
            Action::OpenWindow { app_id: "dev.qos.word-processor", surface_id: 9202 },
            Action::SetWindowWidths(1000),
            Action::Wait(6),
            Action::Relayout,
            Action::SelectGroup(Some(0)),
            Action::Wait(8),
            Action::Relayout,
            Action::Snapshot("offscreen-before"),
            Action::SeedActionFlight {
                from_app: "dev.qos.photos",
                to_app: "word-processor",
            },
            Action::Snapshot("offscreen-revealed"),
            Action::SampleActionFlight {
                prefix: "offscreen",
                at_ms: &[0, 180, 360, 540, 660],
            },
        ],
    });
    // The same crossing, started the way the agent starts one: a frame off
    // the collaboration socket rather than an offer this session watched.
    //
    // Worth capturing separately from `action-flight` above because the two
    // reach the drawing code by different routes and only one of them is the
    // film's. There, an application on this machine handed the bytes over and
    // the session paired them itself. Here nothing did — the participant that
    // did the work has no windows, no sandbox to push content out of, and one
    // socket — so the report is the only thing that says a crossing happened
    // at all. If that path is broken these frames show a desktop with nothing
    // moving on it, which is precisely what beat five looks like today.
    out.push(Scenario {
        name: "agent-action-flight",
        description: "an object carried between two applications by a \
                      participant with no windows, reported over the \
                      collaboration socket the way the agent reports it",
        actions: vec![
            Action::LoneWindow(0),
            Action::OpenWindow { app_id: "dev.qos.photos", surface_id: 9301 },
            Action::OpenWindow { app_id: "dev.qos.word-processor", surface_id: 9302 },
            Action::SetWindowWidths(440),
            Action::Wait(6),
            Action::Relayout,
            Action::SelectGroup(Some(0)),
            Action::Wait(10),
            Action::Relayout,
            Action::Snapshot("agent-flight-before"),
            Action::ReportedActionFlight {
                from_app: "dev.qos.photos",
                to_app: "dev.qos.word-processor",
            },
            Action::SampleActionFlight {
                prefix: "agent-flight",
                at_ms: &[0, 90, 180, 270, 360, 450, 540, 630],
            },
        ],
    });
    out.push(agent_touches_the_photograph());
    out.extend(catalogue_rest());
    return out;
}

/// Where the demo's photograph is written so an app can open it by path.
///
/// A real file on disk, because opening one is what the app does: the path
/// goes into the document, `sync` reads the bytes, and the resource id the
/// action surface publishes is minted from that path. A scenario that handed
/// the app decoded pixels would produce an address nothing could be invoked
/// at.
fn demo_photograph_path() -> &'static str {
    use std::io::Write;
    static PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        let dir = std::env::temp_dir().join("qos-shots");
        std::fs::create_dir_all(&dir).expect("a directory to write the photograph into");
        let path = dir.join("harbour.png");
        let mut f = std::fs::File::create(&path).expect("the photograph to be writable");
        f.write_all(&flight_test_content())
            .expect("the photograph to be written");
        path.to_string_lossy().into_owned()
    })
    .as_str()
}

/// An agent performing an operation on a photograph, marked on the
/// photograph.
///
/// The frames this exists for. Everything a viewer is shown here is a
/// consequence of a real invocation: the address `photos.image:<path>#crop`
/// is resolved against the same catalogue the control socket serves, the
/// capability gate runs, the app's own registry decodes the action and
/// applies it to its own document — and only then is there a mark. Nothing is
/// scripted; if the door refuses, the capture is of a desktop with nothing on
/// it, which is what a refusal should look like.
///
/// Three beats, and the middle one is the whole feature:
///
/// * `01-opened` — Photos with the file it was opened with, at rest. The
///   control frame: no mark, because nothing has been invoked yet.
/// * `touch-*` — the crop, sampled across its span. The brackets converge on
///   the *photograph* rather than on the window, and the chip says who and
///   what.
/// * `03-after` — the mark gone and the photograph cropped. The two together
///   are what make the mark evidence rather than decoration: the picture
///   really did change, and it changed where the mark was.
fn agent_touches_the_photograph() -> Scenario {
    let path = demo_photograph_path();
    let address: &'static str =
        Box::leak(format!("photos.image:{path}#crop").into_boxed_str());
    // The same photograph, and the operation that would undo the crop. Aimed
    // at a participant this room has never heard of, so the catalogue's
    // capability gate leaves the row out and the address resolves to nothing.
    let undo: &'static str =
        Box::leak(format!("photos.image:{path}#reset").into_boxed_str());
    Scenario {
        name: "agent-touches-the-photograph",
        description: "an operation an agent performed, marked on the resource \
                      it was performed on",
        actions: vec![
            Action::LoneWindow(0),
            Action::SeedAgent {
                id: "claude",
                name: "Claude",
                apps: &["photos"],
            },
            Action::OpenPhotograph {
                title: "harbour.png",
                path,
            },
            // Long enough for the strip to finish travelling to the window it
            // just opened. Ten frames caught it mid-pan, so the control frame
            // was taken at one scroll offset and every frame after it at
            // another — which reads as the desktop jumping at the moment the
            // agent acts, and is the one thing this beat must not look like.
            Action::Wait(34),
            Action::Relayout,
            Action::Snapshot("01-opened"),
            Action::InvokeAppAction {
                id: address,
                // The region to keep, in the app's own normalized coordinates
                // — the argument its `crop` spec advertises, spelled the way a
                // caller reading that surface would spell it.
                argument: Some("0.18,0.10,0.56,0.62"),
                agent: Some("claude"),
            },
            Action::SampleActionTouch {
                prefix: "touch",
                // Inside the span at both ends. The mark is *designed* to be
                // invisible at 0 and at the very end — it fades in as it
                // arrives and out as it lets go — so sampling there wrote two
                // frames with nothing on them and spent two of eight captures
                // proving the fade works.
                at_ms: &[40, 110, 220, 450, 800, 1120, 1260, 1380],
            },
            Action::Wait(12),
            Action::Relayout,
            Action::Snapshot("03-after"),
            // And the case that decides whether any of this is evidence.
            //
            // A stranger asks for the crop to be undone. The gate refuses,
            // because it refuses everybody this room has not admitted — so
            // nothing is invoked, and the frame after it has to show *both*
            // that no mark was drawn and that the photograph is still
            // cropped. An overlay that played whenever something was asked
            // for would fail the first half; one drawn from a script rather
            // than from the door would fail both, and would be a lie told on
            // camera rather than a mistake.
            Action::InvokeAppAction {
                id: undo,
                argument: None,
                agent: Some("stranger"),
            },
            Action::Wait(6),
            Action::Relayout,
            Action::Snapshot("04-a-refusal-marks-nothing"),
        ],
    }
}

fn catalogue_rest() -> Vec<Scenario> {
    vec![
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
        strip_horizontal_scroll(),
        notes_click_and_type(),
        two_notes_tabs_keep_their_own_text(),
        cycle_all_tabs(),
        word_processor_tile(),
        word_processor_under_overlay(),
        room_activity_from_palette(),
        one_launch_one_window(),
        window_open_beside_selection(),
        window_open_into_tabbed_group(),
        window_open_at_visible_seam(),
        window_open_at_next_seam_rightwards(),
        strip_scroll_across_apps(),
        strip_pan_curve(),
        strip_reframe_curve(),
        narrow_groups_side_by_side(),
        tab_pill_reveal(),
        tab_pill_reveal_narrow(),
        tab_pill_states(),
        two_applications_in_one_tab_bar(),
        a_window_on_its_way(),
        files_in_a_workspace(),
        new_tab_from_the_plus_chip(),
        status_tray_states(),
        media_key_osd(),
        wifi_password_prompt(),
        file_chooser_dialog(),
        remark_compose_beside_a_window(),
        remark_compose_takes_a_sentence(),
    ]
}

/// The compose surface being used: it stays up, it is typed into, it sends.
///
/// The three frames the gesture was missing. The panel opened and went again
/// inside a tenth of a second, so the only picture anybody had of it was an
/// empty box with a placeholder in it — which looks identical to a box that
/// works and identical to a box that cannot be typed into.
///
/// The press in the middle is the point. It carries no position, which is the
/// state the chrome is in when the pointer has spent the whole gesture inside
/// an application window: motion over a window belongs to that window and
/// never reaches this tree. That press used to close the panel.
///
/// * `01-opened` — the panel as it arrives, empty, with its placeholder.
/// * `02-after-a-press-with-no-position` — still there.
/// * `03-typed` — the sentence in the field.
/// * `04-sent` — Enter, and the panel has gone because it sent.
fn remark_compose_takes_a_sentence() -> Scenario {
    let menu = Rect::new(430.0, 180.0, 610.0, 400.0);
    Scenario {
        name: "remark_compose_takes_a_sentence",
        description: "The compose surface surviving a press, taking a sentence, and sending it.",
        actions: vec![
            Action::LoneWindow(0),
            Action::SelectGroup(Some(0)),
            Action::Wait(4),
            Action::SetWindowWidths(THIRD_WIDTH_PERMILLE),
            Action::Wait(2),
            Action::OpenHostedWindow {
                app: "Notes",
                title: "Notes",
            },
            Action::Wait(PLACEMENT_SETTLE_FRAMES),
            Action::SetWindowWidths(THIRD_WIDTH_PERMILLE),
            Action::Wait(4),
            Action::ComposeRemarkBeside {
                window: 0.0,
                beside: menu,
                label: "harbour.png",
            },
            Action::Wait(2),
            Action::Snapshot("01-opened"),
            // No `MoveCursor` in front of it, deliberately.
            Action::MousePress,
            Action::MouseRelease,
            Action::Wait(2),
            Action::Snapshot("02-after-a-press-with-no-position"),
            Action::ClickNamed("remark_input"),
            Action::Type("claude: crop this to the boats".to_string()),
            Action::Wait(2),
            Action::Snapshot("03-typed"),
            Action::Key(Key::Named(NamedKey::Enter)),
            Action::Wait(2),
            Action::Snapshot("04-sent"),
        ],
    }
}

/// The "say something about this" surface, beside a window at the screen edge.
///
/// The picture this exists to take. Two applications sit side by side on the
/// strip; the menu is at the *right edge* of the left one, which is the case
/// that used to be impossible: the panel is wider than the window it was being
/// clamped into, so it was cut off mid-word with the neighbour visible past the
/// slice. Drawn by the compositor it simply carries on over that neighbour.
///
/// Three shots, and each answers a different objection:
///
/// * `01-beside-the-left-window` — the panel crossing a window boundary.
/// * `02-beside-the-rightmost-window` — no room on the right at all, so the
///   placement rule falls through to its next choice rather than clipping.
/// * `03-empty-strip` — the same request with nothing else open, which is the
///   shape the application used to draw and the control for the other two.
fn remark_compose_beside_a_window() -> Scenario {
    // A menu opened near the right edge of its window, which is where a
    // right-click on a file in the last column lands. The panel is 392 wide
    // and no window on a two-up strip is, so "beside" is off the window by
    // construction.
    let menu = Rect::new(430.0, 180.0, 610.0, 400.0);
    Scenario {
        name: "remark_compose_beside_a_window",
        description: "The compose surface placed against the display, over the window next door.",
        actions: vec![
            // Boot hands us one wide group. Cut it to a lone window and
            // narrow it, so the next application lands in a column of its
            // own rather than as another tab — the panel has to have a
            // *neighbour* to be drawn over, and a full-width window has none.
            Action::LoneWindow(0),
            Action::SelectGroup(Some(0)),
            Action::Wait(4),
            Action::SetWindowWidths(THIRD_WIDTH_PERMILLE),
            Action::Wait(2),
            Action::OpenHostedWindow {
                app: "Notes",
                title: "Notes",
            },
            Action::Wait(PLACEMENT_SETTLE_FRAMES),
            Action::SetWindowWidths(THIRD_WIDTH_PERMILLE),
            Action::Wait(4),
            Action::ComposeRemarkBeside {
                window: 0.0,
                beside: menu,
                label: "harbour.png",
            },
            Action::Wait(2),
            Action::Snapshot("01-beside-the-left-window"),
            Action::ComposeRemarkBeside {
                window: 1.0,
                beside: menu,
                label: "harbour.png",
            },
            Action::Wait(2),
            Action::Snapshot("02-beside-the-rightmost-window"),
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
    use qos_power_mgmt::{BatterySnapshot, BatteryState};
    use shell::shell::status::SystemStatus;

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
        },
        "charging" => SystemStatus {
            battery: Some(batt(38, BatteryState::Charging)),
            link: Some(wifi(3)),
            volume: Some(vol(0.45, false)),
        },
        "low" => SystemStatus {
            battery: Some(batt(14, BatteryState::Discharging)),
            link: Some(wifi(2)),
            volume: Some(vol(0.20, false)),
        },
        "critical" => SystemStatus {
            battery: Some(batt(3, BatteryState::Discharging)),
            link: Some(wifi(1)),
            volume: Some(vol(0.20, false)),
        },
        "muted" => SystemStatus {
            battery: Some(batt(88, BatteryState::Full)),
            link: Some(wifi(4)),
            volume: Some(vol(0.6, true)),
        },
        "offline" => SystemStatus {
            battery: Some(batt(64, BatteryState::Discharging)),
            link: Some(LinkStatus::offline()),
            volume: Some(vol(0.45, false)),
        },
        "wired" => SystemStatus {
            battery: None,
            link: Some(LinkStatus {
                kind: LinkKind::Wired,
                name: Some("Wired connection 1".to_string()),
                signal: 0,
            }),
            volume: Some(vol(0.75, false)),
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
            // A folder the active filter has nothing to show from. The
            // band used to go blank here, which reads as a dialog that
            // failed rather than a filter that excluded everything.
            Action::SeedFilePicker("open"),
            Action::Wait(4),
            // Twice: past Images, onto Spreadsheets, which is the one
            // with nothing to show in a folder of notes.
            Action::CycleFilePickerFilter,
            Action::CycleFilePickerFilter,
            Action::Wait(4),
            Action::Snapshot("04-nothing-matches"),
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
// The strip itself: several real apps along it, and the pan across them.
// =====================================================================

/// Frames to let the eased focus travel settle before a snapshot — the
/// snap curve is 300 ms of real wall time, and a frame sleeps 16 ms.
const PAN_SETTLE_FRAMES: u32 = 24;

/// Window width the strip is held at, in per-mille of the display's
/// usable width. Half a screen each puts two windows in view with a
/// third clipped at the edge.
const HALF_WIDTH_PERMILLE: u16 = 500;

/// Points along the viewport's 300 ms travel worth photographing.
/// Endpoints plus enough of the middle to tell a journey from a cut:
/// a strip that moved in one frame shows the destination at every one
/// of these, a strip that travelled shows a different place at each.
/// The early samples are also where a hard snap gives itself away — a
/// curve that leaves at full speed is already most of the way there by
/// the second frame.
const PAN_SAMPLES: &[u64] = &[0, 60, 120, 180, 240, 300];

/// The strip travelling, photographed while it is still moving.
///
/// Everything else in the catalogue captures the viewport at rest,
/// which is the one state that cannot answer the question the strip
/// raises: a viewer who looks away and back sees different windows and
/// has to work out whether the desktop moved or was replaced. A capture
/// taken after the motion finished looks identical either way.
///
/// So this stops the clock partway. Three windows are opened along the
/// strip until it overflows the display, the fourth arrival sets the
/// viewport travelling, and the pan is held at six points on its curve.
/// Read in order the frames are a dolly across one desktop; if any two
/// adjacent ones are the same picture, the strip cut instead of moving.
fn strip_pan_curve() -> Scenario {
    Scenario {
        name: "strip_pan_curve",
        description: "The strip's viewport travel, held at six points along its curve, so a \
                      pan can be told apart from a cut.",
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
            Action::SelectGroup(Some(1)),
            Action::Wait(4),
            Action::OpenHostedWindow {
                app: "Terminal",
                title: "Terminal",
            },
            Action::Wait(PLACEMENT_SETTLE_FRAMES),
            Action::SetWindowWidths(HALF_WIDTH_PERMILLE),
            Action::SelectGroup(Some(2)),
            Action::Wait(PAN_SETTLE_FRAMES),
            Action::Relayout,
            Action::Snapshot("00-before-the-fourth-window"),
            // The fourth window arrives. Held immediately, before a
            // render's worth of wall time can land the strip.
            Action::OpenHostedWindow {
                app: "Editor",
                title: "Editor",
            },
            Action::SamplePan {
                prefix: "01-arrival",
                at_ms: PAN_SAMPLES,
            },
        ],
    }
}

/// The strip re-framing itself, with nobody having moved focus.
///
/// [`strip_pan_curve`] photographs the pan a window's *arrival* causes,
/// and focus moves with the arrival — so what it shows is the
/// focus-follow travel. This one takes focus out of it. Three windows
/// are already open and one already has focus; then every window is
/// given a larger share of the display, which moves every edge and
/// leaves the offset chosen for the old widths framing nothing in
/// particular.
///
/// Nothing else can move the view here: no window opened, no focus
/// changed, no wheel turned. Whatever travel these frames show is the
/// strip re-framing itself, which is the one motion a viewer has no
/// event to attribute — and therefore the one that most has to be a
/// motion rather than a cut.
fn strip_reframe_curve() -> Scenario {
    Scenario {
        name: "strip_reframe_curve",
        description: "The strip re-framing itself after the windows change width, held at six \
                      points along its curve. No window opened and no focus moved.",
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
            // Focus lands, the strip settles, and everything stops.
            Action::SelectGroup(Some(2)),
            Action::Wait(PAN_SETTLE_FRAMES),
            Action::Relayout,
            Action::Snapshot("00-settled-three-across"),
            // Every window widens. Focus does not move.
            Action::SetWindowWidths(HALF_WIDTH_PERMILLE),
            Action::SamplePan {
                prefix: "01-reframe",
                at_ms: PAN_SAMPLES,
            },
        ],
    }
}

/// Four shell-hosted apps side by side along the strip, then the strip
/// panned across them.
///
/// The scrollable strip is the compositor's whole spatial model, and
/// nothing else in the catalogue shows it doing its job: the placement
/// scenarios open *foreign* toplevels, which hold their window's place
/// with a card naming the app until a real client attaches a buffer,
/// and every hosted-app scenario has one window in the strip. So this
/// one opens Notes, Files, Terminal and Editor — each drawing its own
/// content in-process — as four separate windows, then travels focus
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
/// The file manager sitting in a workspace.
///
/// The trail is the subject. A workspace is mounted wherever the driver
/// put it, and spelling that out — `/ > var > tmp > qos > workspace` —
/// tells a reader they are looking at scratch space on somebody's laptop,
/// which is the opposite of a folder that belongs to the room. Point
/// `QOS_WORKSPACE_ROOTS` at a mount before running this, or the app opens
/// somewhere with nothing to say.
fn files_in_a_workspace() -> Scenario {
    Scenario {
        name: "files_in_a_workspace",
        description: "The file manager open on a workspace, with the \\
                      breadcrumb naming it.",
        actions: vec![
            Action::LoneWindow(0),
            Action::SelectGroup(Some(0)),
            Action::Wait(4),
            Action::Relayout,
            Action::OpenHostedWindow { app: "Files", title: "Files" },
            Action::Wait(PLACEMENT_SETTLE_FRAMES),
            Action::Relayout,
            Action::MoveCursor(Point::new(1500.0, 900.0)),
            Action::Wait(12),
            Action::Relayout,
            Action::Snapshot("01-workspace-trail"),
            // And a held press in the empty part of the pane, which is
            // the other thing this frame of the film had in it. The ring
            // that marks a press is drawn around the pointer's hotspot,
            // and the arrow hangs down-and-right from that — so a ring
            // much wider than the arrow is tall put most of itself in
            // empty space beside the cursor and was read as an
            // unlabelled circle sitting in the middle of the pane.
            Action::MoveCursor(Point::new(600.0, 620.0)),
            Action::Wait(2),
            Action::MousePress,
            Action::Relayout,
            Action::Wait(2),
            Action::Snapshot("02-a-press-in-the-pane"),
            Action::MouseRelease,
        ],
    }
}

/// The card that holds a window's place while its app starts.
///
/// What a person looks at for as long as a launch takes, so the one
/// thing it must do is be readable. It took its ink from a fixed slot at
/// the dark end of the neutral ramp and painted it on the theme's
/// darkest surface: the name of the app you were waiting for came out at
/// about 1.05:1 against its own ground, and every viewer of the film saw
/// four dots in a void. The light presets had the mirror of it.
///
/// Both themes are shot, because a single-theme check is exactly what
/// let this through.
fn a_window_on_its_way() -> Scenario {
    Scenario {
        name: "a_window_on_its_way",
        description: "A window's place held by the opening card while the \
                      application behind it starts.",
        actions: vec![
            Action::LoneWindow(0),
            Action::SelectGroup(Some(0)),
            Action::Wait(4),
            Action::Relayout,
            Action::OpenWindowStillStarting {
                app_id: "dev.qos.word-processor",
                surface_id: 7701,
            },
            // Past `foreign::OPENING_DELAY`, so the card is up.
            Action::Wait(24),
            Action::Relayout,
            Action::MoveCursor(Point::new(1400.0, 900.0)),
            Action::Snapshot("01-opening-card"),
        ],
    }
}

/// Two different applications sharing one tab bar.
///
/// The arrangement this window model exists for, and the one a viewer is
/// most likely to mistake for a browser — so what the picture has to
/// show is that the two pills are two *programs*, not two pages. Each
/// pill carries its own application's mark: a document and a picture,
/// which are different shapes at 14 px and stay different in every
/// theme.
///
/// They used to be the same shape. A pill drew from a five-variant app
/// kind, and both of these fell to its catch-all, so the pair came out as
/// two identical cogs distinguished by nothing but their titles.
fn two_applications_in_one_tab_bar() -> Scenario {
    Scenario {
        name: "two_applications_in_one_tab_bar",
        description: "A word processor and a photo editor in one window \
                      group, each pill wearing its own application's icon.",
        actions: vec![
            Action::LoneWindow(0),
            Action::SelectGroup(Some(0)),
            Action::Wait(4),
            Action::Relayout,
            Action::AddHostedTab { app: "Word", title: "Word Processor" },
            Action::Wait(PLACEMENT_SETTLE_FRAMES),
            Action::Relayout,
            Action::AddHostedTab { app: "Photos", title: "Photos" },
            Action::Wait(PLACEMENT_SETTLE_FRAMES),
            Action::Relayout,
            // Pointer well off the bar: no hover state to blur which
            // difference the picture is about.
            Action::MoveCursor(Point::new(900.0, 620.0)),
            Action::Wait(24),
            Action::Relayout,
            Action::Snapshot("01-two-apps-one-bar"),
        ],
    }
}

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
            Action::SampleTabReveal { prefix: "02-opening", at_ms: REVEAL_SAMPLES },
            Action::Wait(12),
            Action::Snapshot("03-open"),
            // Off the bar entirely: the pill closes again, and its body
            // has to stay under the chips for the whole shrink. Park in
            // the middle of the window card — the wheel disc down the
            // left edge reacts to a pointer near it, which would move
            // the whole strip and confuse what this is measuring.
            Action::MoveCursor(Point::new(800.0, 500.0)),
            Action::SampleTabReveal { prefix: "04-closing", at_ms: REVEAL_SAMPLES },
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
            Action::OpenHostedWindow { app: "Files", title: "Files" },
            Action::Wait(PLACEMENT_SETTLE_FRAMES),
            Action::SetWindowWidths(THIRD_WIDTH_PERMILLE),
            Action::SelectGroup(Some(1)),
            Action::Wait(4),
            Action::OpenHostedWindow { app: "Terminal", title: "Terminal" },
            Action::Wait(PLACEMENT_SETTLE_FRAMES),
            Action::SetWindowWidths(THIRD_WIDTH_PERMILLE),
            Action::SelectGroup(Some(0)),
            Action::Wait(2),
            Action::AddHostedTab { app: "Editor", title: "Editor" },
            Action::Wait(PLACEMENT_SETTLE_FRAMES),
            Action::AddHostedTab { app: "Settings", title: "Settings" },
            Action::Wait(PLACEMENT_SETTLE_FRAMES),
            Action::AddHostedTab { app: "Browser", title: "Browser" },
            Action::Wait(PLACEMENT_SETTLE_FRAMES),
            Action::AddHostedTab { app: "Notes", title: "Document" },
            Action::Wait(PLACEMENT_SETTLE_FRAMES),
            Action::SetWindowWidths(THIRD_WIDTH_PERMILLE),
            Action::Wait(4),
            Action::Relayout,
            Action::Snapshot("01-idle"),
            Action::MoveCursorNamed("tab:Editor"),
            Action::SampleTabReveal { prefix: "02-opening", at_ms: REVEAL_SAMPLES },
            Action::Wait(12),
            Action::Snapshot("03-open"),
            Action::MoveCursor(Point::new(800.0, 500.0)),
            Action::SampleTabReveal { prefix: "04-closing", at_ms: REVEAL_SAMPLES },
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

/// The word processor's light page with an overlay over it — the one
/// scene where a scrim's behaviour is actually visible.
///
/// Every other overlay scenario stands on apps that paint dark, where a
/// dim of any strength moves the pixels by a handful of code values and
/// therefore looks like nothing at all. A `bg-white` page is where a
/// scrim either reads as "the page is behind something" or as "the page
/// went away", and the difference between those two is not visible on a
/// slate ground. Captured undimmed first so the same page can be
/// measured either side of the overlay.
fn word_processor_under_overlay() -> Scenario {
    Scenario {
        name: "word_processor_under_overlay",
        description: "The light word-processor page, then the palette and the \
                      workspace-settings modal over it.",
        actions: vec![
            // The "Document" pill, in the group's opening layout.
            Action::ClickAt(Point::new(717.0, 104.0)),
            Action::Wait(4),
            Action::Snapshot("01-page"),
            Action::OpenPalette,
            Action::Wait(20),
            Action::Snapshot("02-page-under-palette"),
            Action::Key(Key::Named(NamedKey::Escape)),
            Action::Wait(20),
            Action::SetWsSettings(WsTab::Members),
            Action::Wait(20),
            Action::Snapshot("03-page-under-modal"),
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

/// Drive the wheel disc via MouseWheel events over its hit area and
/// capture frame-by-frame so the user can verify the discrete-detent
/// click_to_spoke animation is visible (one workspace per scroll burst).
/// Scroll the strip until the docked wheel has panned off the left
/// edge, then call it back to the pointer with Super+S and dismiss it
/// with Escape.
///
/// This is the gesture's whole reason for existing, so the capture has
/// to show the state it is meant to rescue: the wheel genuinely gone,
/// not merely somewhere else on screen.
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
            Action::DragWheelRim { dy: -PER_WORKSPACE_PX * 0.35, steps: 10 },
            Action::Snapshot("01-part-way"),
            Action::MouseRelease,
            Action::Wait(45),
            // One, two, three workspaces, each captured while still held.
            Action::DragWheelRim { dy: -PER_WORKSPACE_PX, steps: 14 },
            Action::Snapshot("02-one-workspace-held"),
            Action::DragWheelRim { dy: -PER_WORKSPACE_PX, steps: 14 },
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
            Action::DragWheelRim { dy: PER_WORKSPACE_PX * 1.6, steps: 5 },
            Action::MouseRelease,
            Action::SampleWheelSettle { prefix: "05-coast", at_ms: SETTLE_SAMPLES },
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
            Action::DragWheelRim { dy: 90.0, steps: 12 },
            Action::Snapshot("01-pulled"),
            // More than three times the pull, for nowhere near three
            // times the give. Kept inside the disc's own column: the
            // pointer has to still be over the wheel when the button
            // comes up, or the release is routed to whatever is under
            // it instead and the gesture never ends.
            Action::DragWheelRim { dy: 200.0, steps: 20 },
            Action::Snapshot("02-pulled-hard"),
            Action::MouseRelease,
            Action::SampleWheelSettle { prefix: "03-returning", at_ms: SETTLE_SAMPLES },
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

/// One launch, one window — photographed across a workspace switch, which is
/// where a second one used to appear.
///
/// The window is opened through the mirror between the compositor's protocol
/// tree and the chrome's, because that is where the duplicate was made: the
/// two trees hold their own idea of which workspace is active, and the mirror
/// asked "is this surface in the one on screen?" when the question it needed
/// answered was "does this surface have a tab at all?". Walking to the next
/// workspace therefore adopted the same window again, and the desktop carried
/// a word processor nobody had opened.
///
/// The second frame is the one that matters: it is the workspace the user
/// never opened anything in, and it has to be empty.
fn one_launch_one_window() -> Scenario {
    Scenario {
        name: "one_launch_one_window",
        description: "A word processor opened through the compositor's tab \
                      mirror, then the next workspace — which nothing was \
                      opened in — and back again.",
        actions: vec![
            Action::LoneWindow(0),
            Action::Wait(4),
            Action::MirrorWindow {
                app_id: "dev.qos.word-processor",
                surface_id: 9401,
            },
            Action::Wait(PLACEMENT_SETTLE_FRAMES),
            Action::MoveCursor(Point::new(900.0, 620.0)),
            Action::Wait(8),
            Action::Snapshot("01-opened"),
            // The frame loop keeps mirroring while the user walks around.
            Action::SwitchWorkspace(1),
            Action::MirrorAgain,
            Action::Wait(PLACEMENT_SETTLE_FRAMES),
            Action::Snapshot("02-next-workspace-is-untouched"),
            Action::SwitchWorkspace(0),
            Action::MirrorAgain,
            Action::Wait(PLACEMENT_SETTLE_FRAMES),
            Action::Snapshot("03-back-to-the-one-window"),
        ],
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Counter sink for assertions: records each `(name, query, result
    /// count)` without writing any files. Lets the unit test verify the
    /// scenario engine actually invokes the sink in order.
    ///
    /// The result count is recorded at snapshot time because that is the
    /// only moment it means anything: the palette is refreshed as the
    /// scenario types, and a later step closes it.
    struct CountingSink {
        seen: Vec<(String, String, usize)>,
    }
    impl SnapshotSink for CountingSink {
        fn capture(&mut self, driver: &mut ShellDriver, name: &str) {
            self.seen.push((
                name.to_string(),
                driver.palette_query().to_string(),
                driver.host().palette_result_count(),
            ));
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
            .find(|(n, _, _)| n == "03-palette-typed-h")
            .expect("scenario should snapshot 03-palette-typed-h");
        assert_eq!(
            typed.1, "h",
            "after Type(\"h\") with palette open, the query should be 'h'"
        );
        // The half this test used to leave out. It asserted the letter
        // reached the input and stopped there, so it passed for the whole
        // time the list under that input read `No results for "h"` —
        // Ctrl+K seeds the focused app's scope, and on the empty
        // workspace this scenario opens in, that app has nothing.
        // Something has to answer a single letter, or search is broken.
        assert!(
            typed.2 > 0,
            "Ctrl+K then 'h' returned nothing. A person who sees `No \
             results for \"h\"` concludes search is broken, and on this \
             workspace Super+K answers the same letter."
        );
        // Final state: palette closed.
        assert!(
            !d.host().palette_visible(),
            "Esc should close the palette by the end of the scenario"
        );
    }

    /// The scenario is named for a sheet it never opened.
    ///
    /// `room activity` is a chrome action, and Ctrl+K's seeded scope
    /// admits only the focused app's own provider — so the query matched
    /// nothing, Enter had no row to activate, and `03-room-activity-open`
    /// was a picture of a palette reading `No results for "room
    /// activity"`. The scenario ran green throughout: nothing in it
    /// looked at whether the thing in its own name had happened.
    #[test]
    fn scenario_room_activity_opens_the_sheet_it_is_named_for() {
        let mut d = ShellDriver::new((1600, 900));
        let mut sink = CountingSink { seen: Vec::new() };
        let scenario = room_activity_from_palette();
        run(&mut d, &scenario, &mut sink);

        let result = sink
            .seen
            .iter()
            .find(|(n, _, _)| n == "02-palette-result")
            .expect("scenario should snapshot 02-palette-result");
        assert_eq!(result.1, "room activity");
        assert!(
            result.2 > 0,
            "typing 'room activity' into Ctrl+K matched nothing, so there \
             is no row for the Enter that follows to activate"
        );

        let mut open = false;
        d.with_chrome(|st| open = st.overlays.room_activity);
        assert!(
            open,
            "Enter on the 'room activity' result must open the sheet — \
             that is the whole of what this scenario photographs"
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
