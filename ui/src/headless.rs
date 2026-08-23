//! Driving a `ui` app without a display.
//!
//! `--shots` already renders an app's cold-start frame offscreen, but it
//! builds a throwaway `State` and never delivers input, so it cannot answer
//! the question this module exists for: *does a change the user makes
//! survive a restart?* Answering that needs the real widget tree, the real
//! event dispatch, and the app's real persisted document — and no window.
//!
//! [`Driver`] provides exactly that. It stands up the app's root widget over
//! a Taffy tree at a fixed viewport, then feeds it the same `WindowEvent`
//! values winit would deliver, through the same
//! [`Widget::handle_event`](crate::widget::Widget::handle_event) the live app
//! uses. Nothing is stubbed: a click here follows the identical path to a
//! click on real hardware, including the CRDT mutations widgets perform on
//! the way.
//!
//! # Using it from an app
//!
//! Apps opt in with one line in `main`, next to their `--shots` handling:
//!
//! ```ignore
//! if let Some(spec) = ui::headless::spec_from_args(&args) {
//!     std::process::exit(ui::headless::run(spec, my_app::app_root(), "dev.qos.my-app"));
//! }
//! ```
//!
//! which gives the binary a `--headless-drive '<json>'` flag:
//!
//! ```text
//! ebook --headless-drive '{"actions":[{"click_named":"library_item[0]"}]}'
//! ebook --headless-drive '{"dump":"/tmp/after.json"}'
//! ```
//!
//! The first process makes a change and exits (flushing its store); the
//! second loads the store and writes the document out as JSON. Comparing the
//! two dumps is the whole persistence proof, and it is made against the
//! app's real document rather than a harness's idea of one.

use std::collections::HashMap;

use taffy::prelude::*;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::a11y::AccessNode;
use crate::accesskit_bridge::build_tree_update;
use crate::permissions::PermissionContext;
use crate::presence::PresenceCtx;
use crate::state::State;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

/// Viewport the driver lays out at unless a spec overrides it. Matches the
/// default `--shots` viewport so a driven frame and a screenshot agree.
pub const DEFAULT_VIEWPORT: (u32, u32) = (1280, 800);

/// One thing to do to the app.
#[derive(Debug, Clone)]
pub enum Action {
    /// Click the centre of the widget addressed by `name` — either its
    /// [`Widget::test_id`] or the `Parent/Child[index]` path fallback.
    ClickNamed(String),
    /// Click at raw logical coordinates.
    ClickAt(f64, f64),
    /// Press and release one named key (`Enter`, `Tab`, `Escape`, `Space`,
    /// `Backspace`, arrows) or a single character.
    Key(String),
    /// Type a string, one press/release pair per character.
    Type(String),
    /// Hold the given modifiers for subsequent actions. Empty clears them.
    Modifiers(Vec<String>),
}

/// What a `--headless-drive` invocation should do.
#[derive(Debug, Clone, Default)]
pub struct Spec {
    pub viewport: Option<(u32, u32)>,
    pub actions: Vec<Action>,
    /// Write the app's CRDT document here as JSON when the actions are done.
    pub dump: Option<std::path::PathBuf>,
    /// Write the addressable widget names here as JSON.
    pub targets: Option<std::path::PathBuf>,
}

/// A headless app: real widget tree, real Taffy layout, real event dispatch.
pub struct Driver {
    state: State,
    root: Box<dyn Widget>,
    taffy: TaffyTree<()>,
    node_map: HashMap<NodeId, *mut dyn Widget>,
    root_node: NodeId,
    child_node: NodeId,
    text: TextRenderer,
    theme: Theme,
    /// Scratch scene for the draw pass. Never rasterised — see
    /// [`Driver::relayout`] for why it is drawn at all.
    scene: Scene,
    presence: PresenceCtx,
    permissions: PermissionContext,
    modifiers: winit::event::Modifiers,
    viewport: (u32, u32),
    /// Records this driver has open, when the app under test uses them.
    ///
    /// Present so a headless run edits the same documents the live app does:
    /// without it a record-aware app would be driven against its room index
    /// instead of the record it has checked out, and the harness would be
    /// testing something the user never runs.
    records: Option<crate::record::RecordSet>,
    room_index: Option<automerge::AutoCommit>,
    /// The taffy node of the widget that currently holds keyboard focus, or
    /// `None`. Mirrors [`crate::app::App::focused_node`]: the driver manages
    /// focus itself (Tab / directional / focus-on-click) because focus is an
    /// App-level responsibility, not something the root widget's
    /// `handle_event` does. Remapped across every `relayout` by scanning for
    /// the widget whose `is_focused()` is still set (widget instances survive
    /// a relayout; their taffy ids do not).
    focused: Option<NodeId>,
    /// Per-pass frame profiler, installed by the perf harness via
    /// [`Driver::begin_profiling`]. `None` on a normal (persistence) run, so
    /// [`Driver::relayout`] and [`Driver::send`] pay nothing. When set, each
    /// pass is timed and every frame committed — see [`crate::profiling`].
    profiler: Option<crate::profiling::FrameProfiler>,
}

impl Driver {
    /// Stand the app up and run the first layout pass.
    pub fn new(state: State, root: Box<dyn Widget>, viewport: (u32, u32)) -> Self {
        let mut d = Self {
            state,
            root,
            taffy: TaffyTree::new(),
            node_map: HashMap::new(),
            root_node: NodeId::from(0usize),
            child_node: NodeId::from(0usize),
            text: TextRenderer::new(),
            theme: Theme::dark(),
            scene: Scene::new(),
            records: None,
            room_index: None,
            presence: PresenceCtx::empty(),
            permissions: PermissionContext::permissive(),
            modifiers: winit::event::Modifiers::default(),
            viewport,
            focused: None,
            profiler: None,
        };
        d.relayout();
        d
    }

    /// Drive a record-aware app: the live document is a record's, and the
    /// widget names which one. Mirrors [`crate::App::with_records`].
    pub fn with_records(
        mut self,
        records: crate::record::RecordSet,
        room_index: automerge::AutoCommit,
    ) -> Self {
        self.records = Some(records);
        self.room_index = Some(room_index);
        self.relayout();
        self
    }

    /// The records this driver has open, if the app under test uses them.
    pub fn records(&self) -> Option<&crate::record::RecordSet> {
        self.records.as_ref()
    }

    /// The record the root widget names, if any. Read before installing a
    /// record set so an app that does not use records is left alone.
    pub fn root_record_intent(&self) -> Option<crate::record::RecordCheckout> {
        self.root.record_intent()
    }

    /// Offer the live document to its record's writer. Returns whether a
    /// record set handled it — `false` means the caller should persist the
    /// document itself, as an app without records does.
    pub fn persist_tick(&mut self) -> bool {
        match self.records.as_mut() {
            Some(set) => {
                set.tick(&mut self.state.doc);
                true
            }
            None => false,
        }
    }

    /// Write out every open record and wait for it to land.
    pub fn persist_flush(&mut self) {
        if let Some(set) = self.records.as_mut() {
            set.flush(&mut self.state.doc);
        }
    }

    /// Re-run the app's frame: pull state from the CRDT, rebuild the Taffy
    /// tree, compute and apply layout, and draw. Called after every action so
    /// the next one sees where things actually moved to.
    ///
    /// The draw pass is not decoration. Many widgets record the rectangles
    /// they hit-test pointer input against *while drawing* — a click is
    /// resolved against where the widget last painted itself. Skipping the
    /// draw leaves those rects empty and every click silently misses, which
    /// is exactly the kind of quietly-passing test this harness must not
    /// produce. Nothing is rasterised: the `Scene` is encoded on the CPU and
    /// thrown away, so no GPU or display is involved.
    pub fn relayout(&mut self) {
        // Records first, as `ui::App` does: an action that opened a document
        // must be editing that document by the time the next frame syncs.
        if let Some(set) = self.records.as_mut() {
            crate::record::tick(set, &mut self.state, self.room_index.as_mut(), &*self.root);
        }
        let t_sync = self.profiler.as_ref().map(|_| std::time::Instant::now());
        self.root.sync(&mut self.state.doc, &self.state.root);
        if let Some(p) = self.profiler.as_mut() {
            p.record_since(crate::profiling::Pass::Sync, t_sync);
        }

        let t_build = self.profiler.as_ref().map(|_| std::time::Instant::now());
        self.taffy = TaffyTree::new();
        self.node_map.clear();
        let child = self
            .root
            .build_layout(&mut self.taffy, &self.theme, &mut self.node_map);
        let (w, h) = self.viewport;
        let root_node = self
            .taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(w as f32),
                        height: Dimension::length(h as f32),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .expect("headless root node");
        let measure_map = self.node_map.clone();
        let text = &mut self.text;
        self.taffy
            .compute_layout_with_measure(
                root_node,
                taffy::Size {
                    width: AvailableSpace::Definite(w as f32),
                    height: AvailableSpace::Definite(h as f32),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = measure_map.get(&node_id) {
                        // SAFETY: `node_map` holds pointers into `self.root`,
                        // which outlives this call and is not otherwise
                        // borrowed here. Same contract as `App`'s own measure
                        // closure.
                        unsafe { (*ptr).measure(known, available, text) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )
            .expect("headless layout");
        if let Some(p) = self.profiler.as_mut() {
            p.record_since(crate::profiling::Pass::BuildLayout, t_build);
        }

        let t_apply = self.profiler.as_ref().map(|_| std::time::Instant::now());
        self.root.apply_layout(&self.taffy, child, Point::ORIGIN);
        if let Some(p) = self.profiler.as_mut() {
            p.record_since(crate::profiling::Pass::ApplyLayout, t_apply);
        }
        self.root_node = root_node;
        self.child_node = child;

        let t_draw = self.profiler.as_ref().map(|_| std::time::Instant::now());
        self.scene.reset();
        self.root.draw_with_presence(
            &mut self.scene,
            Affine::IDENTITY,
            &mut self.text,
            &self.theme,
            &self.presence,
            &self.permissions,
        );
        if let Some(p) = self.profiler.as_mut() {
            p.record_since(crate::profiling::Pass::Draw, t_draw);
        }

        // The taffy tree was rebuilt, so the previous `focused` NodeId is
        // stale. The widget *instances* survive (a relayout rebuilds layout,
        // not widgets), and the focused one still reports `is_focused()`, so
        // recover the current node by scanning the fresh node map. This keeps
        // Tab focus + keystroke routing stable across the relayout every
        // action triggers.
        self.focused = self.node_map.iter().find_map(|(&n, &p)| {
            // SAFETY: same node_map contract the measure closure uses — the
            // pointers alias `self.root`, which is live and not otherwise
            // borrowed here.
            unsafe { (*p).is_focused() }.then_some(n)
        });

        // One `relayout` is one rendered frame. Commit it (folding in any
        // `handle_event` time banked by a preceding `send`) so the harness
        // sees a per-frame sample. The passes ran contiguously here, so the
        // frame total is taken as their sum.
        if let Some(p) = self.profiler.as_mut() {
            p.commit_frame_summed();
        }
    }

    /// The keyboard-focusable widgets in tree order — the same DFS
    /// `crate::app::App::update_focusable_order` walks (a node is included
    /// when `is_focusable() && is_enabled()`), so a Tab that reaches a widget
    /// here reaches it in the live app too. Focus-trap (modal) confinement is
    /// not modelled; a harness that needs it can filter the result.
    pub fn focus_order(&self) -> Vec<NodeId> {
        let mut order = Vec::new();
        collect_focus_order(self.child_node, self.root.as_ref(), &self.taffy, &mut order);
        order
    }

    /// The taffy node holding keyboard focus, if any.
    pub fn focused(&self) -> Option<NodeId> {
        self.focused
    }

    /// Move keyboard focus to the widget at `node`, toggling `set_focus` on
    /// the outgoing/incoming widgets exactly as
    /// [`crate::app::App::set_focused_node`] does.
    fn set_focused(&mut self, node: Option<NodeId>) {
        if self.focused == node {
            return;
        }
        if let Some(old) = self.focused {
            if let Some(&p) = self.node_map.get(&old) {
                unsafe { (*p).set_focus(false) };
            }
        }
        if let Some(new) = node {
            if let Some(&p) = self.node_map.get(&new) {
                unsafe { (*p).set_focus(true) };
            }
        }
        self.focused = node;
    }

    /// Advance keyboard focus like Tab (or Shift+Tab when `back`), cycling the
    /// focus order. Returns the newly-focused node.
    pub fn tab(&mut self, back: bool) -> Option<NodeId> {
        let order = self.focus_order();
        if order.is_empty() {
            return None;
        }
        let next = match self.focused.and_then(|f| order.iter().position(|&x| x == f)) {
            Some(idx) => {
                if back {
                    if idx == 0 { order.len() - 1 } else { idx - 1 }
                } else {
                    (idx + 1) % order.len()
                }
            }
            None => {
                if back { order.len() - 1 } else { 0 }
            }
        };
        let target = order[next];
        self.set_focused(Some(target));
        self.relayout();
        Some(target)
    }

    /// The accessibility node of every keyboard-focusable control, in focus
    /// order — one entry per Tab stop. Non-mutating (it does not move focus),
    /// so a test can assert the expected controls exist with the right roles,
    /// names, and states in a single pass.
    pub fn focus_access_nodes(&self) -> Vec<AccessNode> {
        self.focus_order()
            .into_iter()
            .filter_map(|id| {
                let &p = self.node_map.get(&id)?;
                unsafe { (*p).accessibility() }
            })
            .collect()
    }

    /// The local accessibility node of the currently-focused widget, if any —
    /// what a screen reader would announce when Tab lands here.
    pub fn focused_access_node(&self) -> Option<AccessNode> {
        let id = self.focused?;
        let &p = self.node_map.get(&id)?;
        unsafe { (*p).accessibility() }
    }

    /// The full accessibility tree the AccessKit bridge exports for the
    /// current frame — the exact `(NodeId, Node)` list that reaches AT-SPI2 /
    /// Orca and the built-in screen reader, with the driver's focus marked.
    pub fn a11y_nodes(&self) -> Vec<(accesskit::NodeId, accesskit::Node)> {
        build_tree_update(self.root.as_ref(), &self.node_map, self.focused, 1.0).nodes
    }

    /// The id of the exported accessibility node whose accessible name is
    /// `name` — the handle a headless test uses to address an element that has
    /// no widget of its own (a table row, a clip on a timeline track).
    ///
    /// Returns `None` when nothing carries that name this frame, which is a
    /// meaningful failure: an element a test cannot name is one a screen-reader
    /// user cannot find either.
    pub fn a11y_node_named(&self, name: &str) -> Option<accesskit::NodeId> {
        self.a11y_nodes()
            .into_iter()
            .find(|(_, n)| n.label().as_deref() == Some(name))
            .map(|(id, _)| id)
    }

    /// The accessibility node the exported tree currently marks focused — what
    /// a screen reader would be sitting on. For a canvas widget this is the
    /// element inside it, not the widget, so a test can assert where the user
    /// actually is.
    pub fn a11y_focus(&self) -> Option<(accesskit::NodeId, accesskit::Node)> {
        let update = build_tree_update(self.root.as_ref(), &self.node_map, self.focused, 1.0);
        let focus = update.focus;
        update.nodes.into_iter().find(|(id, _)| *id == focus)
    }

    /// Perform an accessibility [`Action`](crate::a11y::Action) on an exported
    /// node, exactly as an assistive technology would.
    ///
    /// This is the headless counterpart of
    /// [`App::dispatch_a11y_action`](crate::app::App::dispatch_a11y_action) and
    /// deliberately takes the same route: resolve the id against the live tree,
    /// then hand the owning widget the action *plus the key path naming the
    /// element*, so a synthetic node is operated rather than the widget that
    /// painted it. Without this a canvas app could be described in a headless
    /// test but not driven by one, which caps how well it can be verified at
    /// all.
    ///
    /// `Err` when the id names nothing in the current frame — a stale reference
    /// is reported, never quietly retargeted at whatever now occupies the slot.
    pub fn perform_a11y_action(
        &mut self,
        target: accesskit::NodeId,
        action: &crate::a11y::Action,
    ) -> Result<(), String> {
        use crate::accesskit_bridge::{resolve_node_id, A11yTarget};
        let resolved = resolve_node_id(self.root.as_ref(), &self.node_map, target)
            .ok_or_else(|| format!("no accessibility node {target:?} in the current frame"))?;
        let (taffy_id, key_path) = match resolved {
            A11yTarget::Root => return Err("the window root cannot be operated".into()),
            A11yTarget::Widget(id) => (id, Vec::new()),
            A11yTarget::Synthetic { owner, key_path } => (owner, key_path),
        };
        // Focusing an element inside a widget also moves the keyboard to that
        // widget, matching the App: the widget moves its own cursor, the host
        // moves the focus.
        if !key_path.is_empty() && matches!(action, crate::a11y::Action::Focus) {
            self.set_focused(Some(taffy_id));
        }
        let &p = self
            .node_map
            .get(&taffy_id)
            .ok_or_else(|| format!("widget {taffy_id:?} is not laid out"))?;
        let handled = unsafe {
            (*p).perform_action_at(action, &key_path, &mut self.state.doc, &self.state.root)
        };
        if !handled {
            match action {
                crate::a11y::Action::Focus => self.set_focused(Some(taffy_id)),
                // Fall back the way the App does: a click at the widget's
                // centre. Widget-granular by nature — it cannot reach inside a
                // canvas, which is why `perform_action_at` exists.
                crate::a11y::Action::Activate | crate::a11y::Action::ToggleCheck => {
                    let c = unsafe { (*p).get_rect() }.center();
                    self.click_at(c.x, c.y);
                }
                _ => {}
            }
        }
        self.relayout();
        Ok(())
    }

    /// Tab through the entire focus order from the top, returning the
    /// accessible `(role, name)` a screen reader would announce at each stop.
    /// One assertion proves both that every control is reachable by keyboard
    /// and that each announces a sensible role + label.
    pub fn tab_through(&mut self) -> Vec<(crate::a11y::Role, Option<String>)> {
        self.set_focused(None);
        let count = self.focus_order().len();
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            self.tab(false);
            if let Some(node) = self.focused_access_node() {
                out.push((node.role, node.name));
            }
        }
        out
    }

    /// Every widget the driver can address, by `test_id` or path, with the
    /// rect it currently occupies.
    pub fn targets(&self) -> HashMap<String, Rect> {
        let mut out = HashMap::new();
        let mut counts = HashMap::new();
        collect_targets(self.root.as_ref(), "", &mut counts, &mut out);
        out
    }

    /// The live document. Used to dump state and to hand to a saver.
    pub fn state_mut(&mut self) -> &mut State {
        &mut self.state
    }

    /// Deliver one event through the production dispatch path.
    pub fn send(&mut self, event: WindowEvent) {
        let mut handled = false;
        let mut focus_target: Option<NodeId> = None;
        let t_event = self.profiler.as_ref().map(|_| std::time::Instant::now());
        self.root.handle_event(
            &event,
            1.0,
            &self.modifiers,
            &mut self.state.doc,
            &self.state.root,
            &mut handled,
            &mut focus_target,
            self.child_node,
            &self.taffy,
        );
        if let Some(p) = self.profiler.as_mut() {
            // Banked; folded into the frame the following `relayout` commits.
            p.record_since(crate::profiling::Pass::HandleEvent, t_event);
        }
        // Mirror the App: a widget that claimed focus during dispatch (a
        // click on a field / button) becomes the focused node, so subsequent
        // keystrokes route to it.
        if let Some(t) = focus_target {
            self.set_focused(Some(t));
        }
        // Mirror the App's idle tick: a widget that asked for a commit gets
        // one, so undo boundaries and any commit-time CRDT writes happen the
        // way they do live.
        if self.root.needs_commit() {
            self.state.commit_action(None, None);
            self.root.on_commit();
        }
        self.relayout();
    }

    /// Move the pointer to `(x, y)` and press+release the left button — the
    /// three events a real click produces, in order.
    pub fn click_at(&mut self, x: f64, y: f64) {
        self.send(WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(x, y),
        });
        self.send(WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
        self.send(WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Released,
            button: MouseButton::Left,
        });
    }

    /// Click the centre of the named widget. `Err` names the target when it
    /// is not in the tree, so a stale scenario fails loudly instead of
    /// quietly doing nothing.
    pub fn click_named(&mut self, name: &str) -> Result<(), String> {
        let rect = self
            .targets()
            .get(name)
            .copied()
            .ok_or_else(|| format!("no addressable widget named {name:?}"))?;
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return Err(format!("widget {name:?} has an empty rect: {rect:?}"));
        }
        let c = rect.center();
        self.click_at(c.x, c.y);
        Ok(())
    }

    /// Press and release one key.
    ///
    /// `Tab` is intercepted and drives keyboard focus (Shift+Tab reverses),
    /// exactly as [`crate::app::App`] does in the live event loop — the root
    /// widget never sees Tab. Every other key is delivered to the root's
    /// `handle_event`, where the focused child consumes it.
    pub fn key(&mut self, spec: &str) {
        if spec == "Tab" {
            self.tab(self.modifiers.state().shift_key());
            return;
        }
        let key = parse_key(spec);
        for state in [ElementState::Pressed, ElementState::Released] {
            self.send(key_event(key.clone(), state));
        }
    }

    /// Type a string one character at a time.
    pub fn type_text(&mut self, text: &str) {
        for ch in text.chars() {
            self.key(&ch.to_string());
        }
    }

    /// Send one vertical wheel scroll of `(dx, dy)` line-deltas through the
    /// production event path (`MouseWheel { LineDelta }`), then relayout — the
    /// same three-step frame a real notch produces. Used by the perf harness's
    /// scroll scenarios. Content scrollables handle the wheel regardless of
    /// pointer position, so no prior cursor move is needed.
    pub fn scroll(&mut self, dx: f64, dy: f64) {
        self.send(WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: winit::event::MouseScrollDelta::LineDelta(dx as f32, dy as f32),
            phase: winit::event::TouchPhase::Moved,
        });
    }

    // -- Perf harness plumbing (see [`crate::profiling`]) ------------------

    /// Install a capture profiler so [`Self::relayout`] / [`Self::send`] time
    /// each pass and commit a per-frame sample. Called by
    /// [`crate::profiling::run_scenario`].
    pub fn begin_profiling(&mut self) {
        self.profiler = Some(crate::profiling::FrameProfiler::for_capture());
    }

    /// Render one frame with no input and record it. A frame here is one
    /// `relayout`, which runs the real sync → build → apply → draw passes.
    pub fn profile_frame(&mut self) {
        self.relayout();
    }

    /// Discard captured frames (e.g. warmup) without stopping capture.
    pub fn clear_profile_samples(&mut self) {
        if let Some(p) = self.profiler.as_mut() {
            p.clear_capture();
        }
    }

    /// Take every captured frame so far, draining the buffer.
    pub fn take_profile_samples(&mut self) -> Vec<crate::profiling::FrameTiming> {
        self.profiler
            .as_mut()
            .map(|p| p.take_capture())
            .unwrap_or_default()
    }

    /// Set the held modifier set. Names: `ctrl`, `shift`, `alt`, `logo`.
    pub fn set_modifiers(&mut self, names: &[String]) {
        let mut st = winit::keyboard::ModifiersState::empty();
        for n in names {
            match n.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => st |= winit::keyboard::ModifiersState::CONTROL,
                "shift" => st |= winit::keyboard::ModifiersState::SHIFT,
                "alt" => st |= winit::keyboard::ModifiersState::ALT,
                "logo" | "super" | "meta" => st |= winit::keyboard::ModifiersState::SUPER,
                _ => {}
            }
        }
        self.modifiers = winit::event::Modifiers::from(st);
        self.send(WindowEvent::ModifiersChanged(self.modifiers));
    }

    /// Run one action.
    pub fn perform(&mut self, action: &Action) -> Result<(), String> {
        match action {
            Action::ClickNamed(name) => self.click_named(name),
            Action::ClickAt(x, y) => {
                self.click_at(*x, *y);
                Ok(())
            }
            Action::Key(k) => {
                self.key(k);
                Ok(())
            }
            Action::Type(t) => {
                self.type_text(t);
                Ok(())
            }
            Action::Modifiers(m) => {
                self.set_modifiers(m);
                Ok(())
            }
        }
    }

    /// The document rendered as JSON, for a harness to diff across a
    /// restart.
    pub fn dump_json(&mut self) -> String {
        serde_json::to_string_pretty(&automerge::AutoSerde::from(&*self.state.doc.document()))
            .unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    }
}

/// DFS the widget tree collecting the taffy node of every keyboard-focusable
/// widget, in the order [`crate::app::App::update_focusable_order`] uses. The
/// widget's taffy children come from `taffy.children(node)`, zipped against
/// `widget.children()` — the two must line up, which every container upholds.
fn collect_focus_order(
    node: NodeId,
    widget: &dyn Widget,
    taffy: &TaffyTree<()>,
    out: &mut Vec<NodeId>,
) {
    if widget.is_focusable() && widget.is_enabled() {
        out.push(node);
    }
    if let Ok(children) = taffy.children(node) {
        for (child_widget, &child_node) in widget.children().iter().zip(children.iter()) {
            collect_focus_order(child_node, *child_widget, taffy, out);
        }
    }
}

fn collect_targets(
    w: &dyn Widget,
    parent_path: &str,
    sibling_counts: &mut HashMap<String, usize>,
    out: &mut HashMap<String, Rect>,
) {
    let name = w.name().to_string();
    let sibling_key = format!("{parent_path}::{name}");
    let idx = sibling_counts.entry(sibling_key).or_insert(0);
    let my_index = *idx;
    *idx += 1;
    let path = if parent_path.is_empty() {
        format!("{name}[{my_index}]")
    } else {
        format!("{parent_path}/{name}[{my_index}]")
    };
    out.insert(path.clone(), w.get_rect());
    if my_index == 0 {
        let bare = if parent_path.is_empty() {
            name.clone()
        } else {
            format!("{parent_path}/{name}")
        };
        out.entry(bare).or_insert_with(|| w.get_rect());
    }
    if let Some(id) = w.test_id() {
        out.insert(id.to_string(), w.get_rect());
    }
    let mut child_counts: HashMap<String, usize> = HashMap::new();
    for child in w.children() {
        collect_targets(child, &path, &mut child_counts, out);
    }
}

/// Build a `WindowEvent::KeyboardInput` winit itself would have delivered.
///
/// `KeyEvent::platform_specific` is `pub(crate)` in winit, so the struct
/// cannot be built field-by-field from outside that crate. Zero-initialising
/// and then overwriting every field we care about is the workaround the
/// compositor's own driver already uses (`shell/src/bin/shots/driver.rs`);
/// the zeroed platform payload is inert on the read paths widgets take.
fn key_event(key: Key, state: ElementState) -> WindowEvent {
    // SAFETY: `KeyEvent`'s only unwritten field is the platform payload,
    // which on Linux is a plain `Option`-shaped struct for which an
    // all-zero bit pattern is a valid value, and which nothing downstream
    // of `handle_event` reads.
    let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = key.clone();
    ev.physical_key =
        winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    ev.location = winit::keyboard::KeyLocation::Standard;
    ev.state = state;
    ev.repeat = false;
    ev.text = match (&key, state) {
        (Key::Character(c), ElementState::Pressed) => Some(c.clone()),
        _ => None,
    };
    WindowEvent::KeyboardInput {
        device_id: winit::event::DeviceId::dummy(),
        event: ev,
        is_synthetic: false,
    }
}

fn parse_key(spec: &str) -> Key {
    match spec {
        "Enter" | "Return" => Key::Named(NamedKey::Enter),
        "Tab" => Key::Named(NamedKey::Tab),
        "Escape" | "Esc" => Key::Named(NamedKey::Escape),
        "Space" => Key::Named(NamedKey::Space),
        "Backspace" => Key::Named(NamedKey::Backspace),
        "Delete" => Key::Named(NamedKey::Delete),
        "ArrowUp" | "Up" => Key::Named(NamedKey::ArrowUp),
        "ArrowDown" | "Down" => Key::Named(NamedKey::ArrowDown),
        "ArrowLeft" | "Left" => Key::Named(NamedKey::ArrowLeft),
        "ArrowRight" | "Right" => Key::Named(NamedKey::ArrowRight),
        "Home" => Key::Named(NamedKey::Home),
        "End" => Key::Named(NamedKey::End),
        other => Key::Character(other.into()),
    }
}

/// Parse `--headless-drive '<json>'` (or `--headless-drive=<json>`) out of
/// argv. `None` when the flag is absent, which is every normal launch.
///
/// The JSON is an object: `viewport` (`[w, h]`), `actions` (a list of
/// one-key objects — `{"click_named": "..."}`, `{"click_at": [x, y]}`,
/// `{"key": "Enter"}`, `{"type": "hello"}`, `{"modifiers": ["ctrl"]}`),
/// `dump` (path for the document JSON) and `targets` (path for the
/// addressable-widget list).
pub fn spec_from_args(args: &[String]) -> Option<Spec> {
    let raw = value_arg(args, "--headless-drive")?;
    match parse_spec(&raw) {
        Ok(spec) => Some(spec),
        Err(e) => {
            eprintln!("--headless-drive: {e}");
            std::process::exit(2);
        }
    }
}

fn value_arg(args: &[String], flag: &str) -> Option<String> {
    let mut it = args.iter().skip(1);
    let eq = format!("{flag}=");
    while let Some(a) = it.next() {
        if a == flag {
            return it.next().cloned();
        }
        if let Some(rest) = a.strip_prefix(&eq) {
            return Some(rest.to_string());
        }
    }
    None
}

/// Parse the `--headless-drive` payload.
pub fn parse_spec(raw: &str) -> Result<Spec, String> {
    let v: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("invalid JSON: {e}"))?;
    let mut spec = Spec::default();
    if let Some(vp) = v.get("viewport").and_then(|x| x.as_array()) {
        if vp.len() == 2 {
            spec.viewport = Some((
                vp[0].as_u64().unwrap_or(0) as u32,
                vp[1].as_u64().unwrap_or(0) as u32,
            ));
        }
    }
    if let Some(p) = v.get("dump").and_then(|x| x.as_str()) {
        spec.dump = Some(p.into());
    }
    if let Some(p) = v.get("targets").and_then(|x| x.as_str()) {
        spec.targets = Some(p.into());
    }
    for a in v.get("actions").and_then(|x| x.as_array()).unwrap_or(&vec![]) {
        let obj = a
            .as_object()
            .ok_or_else(|| format!("action must be an object, got {a}"))?;
        let (k, val) = obj
            .iter()
            .next()
            .ok_or_else(|| "action object is empty".to_string())?;
        let action = match k.as_str() {
            "click_named" => Action::ClickNamed(
                val.as_str()
                    .ok_or("click_named takes a string")?
                    .to_string(),
            ),
            "click_at" => {
                let xy = val.as_array().ok_or("click_at takes [x, y]")?;
                Action::ClickAt(
                    xy.first().and_then(|x| x.as_f64()).ok_or("click_at x")?,
                    xy.get(1).and_then(|x| x.as_f64()).ok_or("click_at y")?,
                )
            }
            "key" => Action::Key(val.as_str().ok_or("key takes a string")?.to_string()),
            "type" => Action::Type(val.as_str().ok_or("type takes a string")?.to_string()),
            "modifiers" => Action::Modifiers(
                val.as_array()
                    .ok_or("modifiers takes a list")?
                    .iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect(),
            ),
            other => return Err(format!("unknown action {other:?}")),
        };
        spec.actions.push(action);
    }
    Ok(spec)
}

/// Run `spec` against `root`, persisting through this package's store.
///
/// The document comes from, and goes back to, the same place the live app
/// uses — that is the point. Returns a process exit code: `0` when every
/// action succeeded, `1` when one did not (with the reason on stderr) and
/// `3` when persistence itself was unavailable, which a sweep must not
/// mistake for "nothing changed".
pub fn run(spec: Spec, root: Box<dyn Widget>, manifest_id: &str) -> i32 {
    let (state, mut autosave) = crate::persist::open(manifest_id);
    if !autosave.is_enabled() {
        eprintln!(
            "headless: persistence unavailable: {}",
            autosave.last_error().unwrap_or_default()
        );
        return 3;
    }
    // An app whose root names a record keeps its content in that record's own
    // document, so the driver has to hold the same record set the live app
    // does — otherwise the run would edit the room index and prove nothing
    // about what the user sees. An app that names no record is unaffected: the
    // set stays empty and the live document is the package's, as before.
    let mut driver = Driver::new(state, root, spec.viewport.unwrap_or(DEFAULT_VIEWPORT));
    if driver.root_record_intent().is_some() {
        let (room, _) = crate::persist::open(manifest_id);
        driver = driver.with_records(crate::record::RecordSet::open(manifest_id), room.doc);
    }

    let mut failure: Option<String> = None;
    for action in &spec.actions {
        if let Err(e) = driver.perform(action) {
            failure = Some(e);
            break;
        }
        if !driver.persist_tick() {
            autosave.request(&mut driver.state_mut().doc);
        }
    }
    driver.persist_flush();

    if let Some(path) = &spec.targets {
        let mut names: Vec<(String, [f64; 4])> = driver
            .targets()
            .into_iter()
            .map(|(n, r)| (n, [r.x0, r.y0, r.width(), r.height()]))
            .collect();
        names.sort_by(|a, b| a.0.cmp(&b.0));
        let json = serde_json::to_string_pretty(&names).unwrap_or_default();
        if let Err(e) = std::fs::write(path, json) {
            eprintln!("headless: writing targets to {}: {e}", path.display());
            return 1;
        }
    }
    if let Some(path) = &spec.dump {
        let json = driver.dump_json();
        if let Err(e) = std::fs::write(path, json) {
            eprintln!("headless: writing dump to {}: {e}", path.display());
            return 1;
        }
    }

    // The same two calls `App::exiting` makes, for the same reason.
    autosave.request(&mut driver.state_mut().doc);
    autosave.flush();
    if let Some(e) = autosave.last_error() {
        eprintln!("headless: state write failed: {e}");
        return 3;
    }
    match failure {
        Some(e) => {
            eprintln!("headless: {e}");
            1
        }
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::{Button, Widget};

    #[test]
    fn spec_parses_every_action_shape() {
        let spec = parse_spec(
            r#"{"viewport":[800,600],"dump":"/tmp/d.json","targets":"/tmp/t.json",
                "actions":[{"click_named":"save"},{"click_at":[10,20]},
                           {"key":"Enter"},{"type":"hi"},{"modifiers":["ctrl"]}]}"#,
        )
        .expect("spec parses");
        assert_eq!(spec.viewport, Some((800, 600)));
        assert_eq!(spec.dump.as_deref(), Some(std::path::Path::new("/tmp/d.json")));
        assert_eq!(spec.actions.len(), 5);
        assert!(matches!(&spec.actions[0], Action::ClickNamed(n) if n == "save"));
        assert!(matches!(&spec.actions[3], Action::Type(t) if t == "hi"));
    }

    #[test]
    fn an_unknown_action_is_rejected_rather_than_ignored() {
        let err = parse_spec(r#"{"actions":[{"teleport":"x"}]}"#).unwrap_err();
        assert!(err.contains("teleport"), "{err}");
    }

    /// A button click must reach the widget through the same
    /// `handle_event` the live app calls.
    #[test]
    fn clicking_a_named_target_reaches_the_widget() {
        let button = Button::new("btn", "Save", None);
        let mut driver = Driver::new(State::new(), Box::new(button), (400, 200));
        let targets = driver.targets();
        assert!(
            targets.keys().any(|k| k.starts_with("Button")),
            "the button must be addressable, got {:?}",
            targets.keys().collect::<Vec<_>>()
        );
        let name = targets
            .keys()
            .find(|k| k.starts_with("Button"))
            .unwrap()
            .clone();
        driver.click_named(&name).expect("click lands");
    }

    #[test]
    fn clicking_a_missing_target_is_an_error() {
        let mut driver = Driver::new(State::new(), Box::new(Button::new("b", "B", None)), (400, 200));
        let err = driver.click_named("no_such_widget").unwrap_err();
        assert!(err.contains("no_such_widget"), "{err}");
    }

    #[test]
    fn dump_json_reflects_the_document() {
        use automerge::transaction::Transactable;
        let mut state = State::new();
        state
            .doc
            .put(&automerge::ROOT, "greeting", "hello")
            .unwrap();
        let mut driver = Driver::new(state, Box::new(Button::new("b", "B", None)), (400, 200));
        let json = driver.dump_json();
        assert!(json.contains("greeting"), "{json}");
        assert!(json.contains("hello"), "{json}");
    }
}

#[cfg(test)]
mod a11y_driver_tests {
    use super::*;
    use crate::a11y::{AccessNode, AccessState, Action, Role};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A canvas widget: paints its own contents, exposes them only as synthetic
    /// accessibility nodes, and implements only the routed action hook. The
    /// shape a timeline or a spreadsheet grid takes.
    struct Strip {
        items: Vec<&'static str>,
        cursor: usize,
        focused: bool,
        activated: Rc<RefCell<Vec<String>>>,
    }

    impl Widget for Strip {
        fn sync(&mut self, _s: &mut automerge::AutoCommit, _p: &automerge::ObjId) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _t: &crate::theme::Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let n = taffy.new_leaf(taffy::Style::default()).unwrap();
            node_map.insert(n, self as *mut dyn Widget);
            n
        }
        fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: vello::kurbo::Point) {}
        fn draw(
            &mut self,
            _sc: &mut Scene,
            _tr: vello::kurbo::Affine,
            _tx: &mut crate::text_render::TextRenderer,
            _th: &crate::theme::Theme,
            _pr: &PresenceCtx,
        ) {
        }
        #[allow(clippy::too_many_arguments)]
        fn handle_event(
            &mut self,
            _e: &WindowEvent,
            _sf: f64,
            _m: &winit::event::Modifiers,
            _s: &mut automerge::AutoCommit,
            _p: &automerge::ObjId,
            _h: &mut bool,
            _ft: &mut Option<NodeId>,
            _n: NodeId,
            _t: &TaffyTree<()>,
        ) {
        }
        fn name(&self) -> &str {
            "Strip"
        }
        fn get_rect(&self) -> Rect {
            Rect::new(0.0, 0.0, 300.0, 80.0)
        }
        fn is_focusable(&self) -> bool {
            true
        }
        fn set_focus(&mut self, f: bool) {
            self.focused = f;
        }
        fn is_focused(&self) -> bool {
            self.focused
        }
        fn accessibility(&self) -> Option<AccessNode> {
            Some(
                AccessNode::new(Role::Canvas)
                    .with_name("Strip")
                    .with_state(AccessState::new().focusable(true).focused(self.focused))
                    .with_children(
                        self.items
                            .iter()
                            .enumerate()
                            .map(|(i, id)| {
                                AccessNode::new(Role::ListItem)
                                    .with_name(*id)
                                    .with_hashed_key(*id)
                                    .with_state(
                                        AccessState::new()
                                            .focusable(true)
                                            .focused(i == self.cursor),
                                    )
                            })
                            .collect(),
                    ),
            )
        }
        fn perform_action_at(
            &mut self,
            action: &Action,
            key_path: &[u64],
            _s: &mut automerge::AutoCommit,
            _p: &automerge::ObjId,
        ) -> bool {
            let Some(&key) = key_path.first() else { return false };
            let Some(i) = self
                .items
                .iter()
                .position(|id| crate::a11y::hash_key(id) == key)
            else {
                return false;
            };
            match action {
                Action::Focus => {
                    self.cursor = i;
                    true
                }
                Action::Activate => {
                    self.cursor = i;
                    self.activated.borrow_mut().push(self.items[i].into());
                    true
                }
                _ => false,
            }
        }
    }

    fn driver() -> (Driver, Rc<RefCell<Vec<String>>>) {
        let activated: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let strip = Strip {
            items: vec!["alpha", "beta", "gamma"],
            cursor: 0,
            focused: false,
            activated: activated.clone(),
        };
        (
            Driver::new(State::new(), Box::new(strip), (800, 600)),
            activated,
        )
    }

    #[test]
    fn a_headless_test_can_name_an_element_that_has_no_widget() {
        let (d, _a) = driver();
        for name in ["alpha", "beta", "gamma"] {
            assert!(
                d.a11y_node_named(name).is_some(),
                "an element a test cannot name is one a screen-reader user cannot find"
            );
        }
        assert_eq!(d.a11y_node_named("delta"), None);
    }

    #[test]
    fn a_headless_test_can_activate_one_element_inside_a_canvas() {
        // The whole point: before, a canvas could be described in a headless
        // test but only clicked at coordinates, which activates whatever
        // happens to sit there.
        let (mut d, activated) = driver();
        let beta = d.a11y_node_named("beta").unwrap();
        d.perform_a11y_action(beta, &Action::Activate).unwrap();
        assert_eq!(*activated.borrow(), vec!["beta".to_string()]);

        let gamma = d.a11y_node_named("gamma").unwrap();
        d.perform_a11y_action(gamma, &Action::Activate).unwrap();
        assert_eq!(
            *activated.borrow(),
            vec!["beta".to_string(), "gamma".to_string()]
        );
    }

    #[test]
    fn focusing_an_element_moves_the_exported_focus_onto_it() {
        let (mut d, _a) = driver();
        let gamma = d.a11y_node_named("gamma").unwrap();
        d.perform_a11y_action(gamma, &Action::Focus).unwrap();

        let (id, node) = d.a11y_focus().expect("something is focused");
        assert_eq!(id, gamma, "the reader sits on the element, not the canvas");
        assert_eq!(node.label(), Some("gamma"));
    }

    #[test]
    fn an_id_that_names_nothing_is_reported_not_silently_retargeted() {
        let (mut d, activated) = driver();
        let real = d.a11y_node_named("alpha").unwrap();
        let stale = accesskit::NodeId(real.0 ^ 0xdead_beef);
        let err = d
            .perform_a11y_action(stale, &Action::Activate)
            .expect_err("a stale id must be an error, not a guess");
        assert!(err.contains("no accessibility node"), "got {err}");
        assert!(activated.borrow().is_empty());
    }
}
