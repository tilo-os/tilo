//! Driving a `ui` app without a display.
//!
//! `--shots` renders a cold-start frame offscreen but never delivers input,
//! so it can't answer *does a change survive a restart?* [`Driver`] does:
//! it stands up the app's real widget tree over a Taffy layout at a fixed
//! viewport and feeds it the same `WindowEvent`s winit would, through the
//! same [`Widget::handle_event`](crate::widget::Widget::handle_event) the
//! live app uses — nothing stubbed, including CRDT mutations.
//!
//! Apps opt in with one line in `main`, next to `--shots`:
//!
//! ```ignore
//! if let Some(spec) = ui::headless::spec_from_args(&args) {
//!     std::process::exit(ui::headless::run(spec, my_app::app_root(), "dev.qos.my-app"));
//! }
//! ```
//!
//! which adds a `--headless-drive '<json>'` flag:
//!
//! ```text
//! ebook --headless-drive '{"actions":[{"click_named":"library_item[0]"}]}'
//! ebook --headless-drive '{"dump":"/tmp/after.json"}'
//! ```
//!
//! The first process changes state and exits (flushing its store); the
//! second loads the store and dumps the document as JSON. Diffing the two
//! dumps is the persistence proof, against the app's real document.

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
use crate::widget::{TaffyTreeExt, Widget};

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

/// The canonical display-independent frame pipeline.
///
/// Both screenshot rendering and [`Driver`] input scenarios use this type, so
/// they cannot drift into subtly different sync/layout/draw implementations.
/// Live [`crate::App`] additionally owns the window and event loop, but follows
/// the same pass ordering documented by [`Self::relayout`].
pub struct FrameHarness {
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
    /// Records this driver has open, when the app under test uses them —
    /// without this a record-aware app would be driven against its room
    /// index instead of the record it checked out, testing something the
    /// user never runs.
    records: Option<crate::record::RecordSet>,
    room_index: Option<automerge::AutoCommit>,
    /// The taffy node holding keyboard focus, or `None`. Mirrors
    /// [`crate::app::App::focused_node`] — focus is App-level, not
    /// something the root widget's `handle_event` does. Remapped across
    /// every `relayout` by scanning for `is_focused()`: widget instances
    /// survive a relayout, their taffy ids do not.
    focused: Option<NodeId>,
    /// Identity the driver's commits and undos are attributed to. Mirrors
    /// [`crate::app::App`]'s `local_author` so a scenario exercises the same
    /// author-scoped history a user does.
    author: crate::permissions::UserId,
    /// Per-pass frame profiler, installed by the perf harness via
    /// [`Driver::begin_profiling`]. `None` on a normal (persistence) run, so
    /// [`Driver::relayout`] and [`Driver::send`] pay nothing. When set, each
    /// pass is timed and every frame committed — see [`crate::profiling`].
    profiler: Option<crate::profiling::FrameProfiler>,
}

/// Backwards-compatible name for the input-driving use of [`FrameHarness`].
pub type Driver = FrameHarness;

impl FrameHarness {
    /// Stand the app up and run the first layout pass.
    pub fn new(state: State, root: Box<dyn Widget>, viewport: (u32, u32)) -> Self {
        Self::new_with_theme(state, root, viewport, Theme::dark())
    }

    /// Stand the app up under an explicit theme and run the first frame.
    pub fn new_with_theme(
        state: State,
        root: Box<dyn Widget>,
        viewport: (u32, u32),
        theme: Theme,
    ) -> Self {
        let mut d = Self {
            state,
            root,
            taffy: TaffyTree::new(),
            node_map: HashMap::new(),
            root_node: NodeId::from(0usize),
            child_node: NodeId::from(0usize),
            text: TextRenderer::new(),
            theme,
            scene: Scene::new(),
            records: None,
            room_index: None,
            presence: PresenceCtx::empty(),
            permissions: PermissionContext::permissive(),
            modifiers: winit::event::Modifiers::default(),
            viewport,
            focused: None,
            author: crate::collaboration::local_user_id(),
            profiler: None,
        };
        d.relayout();
        d
    }

    /// Undo the last action, the way the live host does — through
    /// [`State::undo_as`](crate::state::State::undo_as) with this driver's
    /// author. Returns whether anything was undone.
    ///
    /// The driver's one undo door: `Ctrl+Z` and an
    /// [`UndoButton`](crate::widget::UndoButton) both arrive here, so a
    /// scenario cannot pass against a path the user never takes.
    pub fn perform_undo(&mut self) -> bool {
        let author = self.author.clone();
        let done = self.state.undo_as(Some(&author), None, None).is_some();
        if done {
            self.relayout();
        }
        done
    }

    /// Redo the last undone action. The mirror of [`Self::perform_undo`].
    pub fn perform_redo(&mut self) -> bool {
        let author = self.author.clone();
        let done = self.state.redo_as(Some(&author), None, None).is_some();
        if done {
            self.relayout();
        }
        done
    }

    /// What the history can do right now — the value published to the widget
    /// tree each relayout.
    pub fn undo_status(&mut self) -> crate::undo::UndoStatus {
        let author = self.author.clone();
        self.state.undo_status(Some(&author))
    }

    /// Perform any undo/redo the widget tree asked for. Returns whether
    /// anything happened.
    fn drain_undo_requests(&mut self) -> bool {
        let mut requests = Vec::new();
        for &w_ptr in self.node_map.values() {
            // SAFETY: same invariant as every other `node_map` dereference in
            // this driver — pointers are valid until the next `relayout`.
            let w = unsafe { &mut *w_ptr };
            if let Some(req) = w.take_undo_request() {
                requests.push(req);
            }
        }
        let mut acted = false;
        for req in requests {
            acted |= match req {
                crate::undo::UndoRequest::Undo => self.perform_undo(),
                crate::undo::UndoRequest::Redo => self.perform_redo(),
            };
        }
        acted
    }

    /// Hand the live undo status to every laid-out widget, so a control
    /// renders and reports the same truth it would live.
    fn publish_undo_status(&mut self) {
        let status = self.undo_status();
        for &w_ptr in self.node_map.values() {
            // SAFETY: as in `drain_undo_requests`.
            let w = unsafe { &mut *w_ptr };
            w.set_undo_status(&status);
        }
    }
    /// Use a different palette for subsequent frames.
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        self.relayout();
    }

    /// Rasterise the most recently laid-out frame to a PNG.
    ///
    /// `new`, input delivery, and `set_theme` all finish with `relayout`, so
    /// this never runs a separate screenshot-only widget pipeline.
    pub fn render_png(&self, out: &std::path::Path) {
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let (w, h) = self.viewport;
        let mut renderer = crate::offscreen::OffscreenRenderer::new(w, h);
        renderer.render_png(&self.scene, vello::peniko::Color::TRANSPARENT, out);
    }

    /// Drive a record-aware app: the live document is a record's, and the
    /// widget names which one. Mirrors [`crate::App::with_records`].
    #[must_use]
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

    /// Re-run the app's frame: sync from the CRDT, rebuild Taffy, apply
    /// layout, and draw. Called after every action so the next one sees
    /// where things actually moved.
    ///
    /// The draw pass isn't decoration: many widgets record their hit-test
    /// rects while drawing, so skipping it would leave every click silently
    /// missing — exactly the false-pass this harness must avoid. Nothing is
    /// rasterised; the `Scene` is encoded on the CPU and discarded.
    #[allow(
        clippy::cast_precision_loss,
        reason = "viewport width/height are pixel dimensions, always far below \
                  f32's 2^24 exact-integer range"
    )]
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
        // `TaffyTreeExt::parent_node` is panic-free by construction (see its
        // doc comment): `child` was just built in this same fresh tree.
        let root_node = self.taffy.parent_node(
            Style {
                size: Size {
                    width: Dimension::length(w as f32),
                    height: Dimension::length(h as f32),
                },
                ..Default::default()
            },
            &[child],
        );
        let measure_map = self.node_map.clone();
        let text = &mut self.text;
        let layout_result = self.taffy.compute_layout_with_measure(
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
        );
        // Mirrors `App`'s own frame loop (`ui/src/app.rs`): a computed-layout
        // failure skips this frame rather than crashing the driven process.
        if let Err(err) = layout_result {
            tracing::error!(
                ?err,
                "headless: compute_layout_with_measure failed; skipping frame"
            );
            return;
        }
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

        // After layout (so a control built this pass is in `node_map`) and
        // before draw and any accessibility read — a shot must show the
        // enabled state the user would see.
        self.publish_undo_status();

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
        // stale; recover it by scanning the fresh node map for whichever
        // widget still reports `is_focused()`.
        self.focused = self.node_map.iter().find_map(|(&n, &p)| {
            // SAFETY: same node_map contract the measure closure uses — the
            // pointers alias `self.root`, which is live and not otherwise
            // borrowed here.
            unsafe { (*p).is_focused() }.then_some(n)
        });

        // One `relayout` is one rendered frame; commit it, folding in any
        // `handle_event` time banked by a preceding `send`.
        if let Some(p) = self.profiler.as_mut() {
            p.commit_frame_summed();
        }
    }

    /// The keyboard-focusable widgets in tree order — the same DFS
    /// `crate::app::App::update_focusable_order` walks, so a Tab that
    /// reaches a widget here reaches it in the live app too. Focus-trap
    /// (modal) confinement is not modelled.
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
        if let Some(old) = self.focused
            && let Some(&p) = self.node_map.get(&old)
        {
            unsafe { (*p).set_focus(false) };
        }
        if let Some(new) = node
            && let Some(&p) = self.node_map.get(&new)
        {
            unsafe { (*p).set_focus(true) };
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
        // `order` is non-empty (checked above), so `order.len() - 1` never
        // underflows and `next` always lands in `0..order.len()`; the
        // arithmetic is still spelled with saturating/wrapping-safe ops so
        // clippy can see that too.
        let next = match self
            .focused
            .and_then(|f| order.iter().position(|&x| x == f))
        {
            Some(idx) => {
                if back {
                    if idx == 0 {
                        order.len().saturating_sub(1)
                    } else {
                        idx.saturating_sub(1)
                    }
                } else {
                    let next = idx.saturating_add(1);
                    if next < order.len() { next } else { 0 }
                }
            }
            None => {
                if back {
                    order.len().saturating_sub(1)
                } else {
                    0
                }
            }
        };
        // `next` is constructed above from `0..order.len()` in every branch.
        let target = *order.get(next)?;
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
            .find(|(_, n)| n.label() == Some(name))
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
    /// # Errors
    ///
    /// Returns an error when the id names nothing in the current frame — a
    /// stale reference is reported, never quietly retargeted at whatever now
    /// occupies the slot — or when it addresses the inoperable window root.
    pub fn perform_a11y_action(
        &mut self,
        target: accesskit::NodeId,
        action: &crate::a11y::Action,
    ) -> Result<(), String> {
        use crate::accesskit_bridge::{A11yTarget, resolve_node_id};
        let resolved = resolve_node_id(self.root.as_ref(), &self.node_map, target)
            .ok_or_else(|| format!("no accessibility node {target:?} in the current frame"))?;
        let (taffy_id, key_path) = match resolved {
            A11yTarget::Root => return Err("the window root cannot be operated".into()),
            A11yTarget::Widget(id) => (id, Vec::new()),
            // Keys here: the driver dispatches through `perform_action_at`,
            // which identifies a synthetic child by identity.
            A11yTarget::Synthetic {
                owner, key_path, ..
            } => (owner, key_path),
        };
        // Operating an element inside a widget also moves the keyboard to that
        // widget, matching the App: the widget moves its own cursor, the host
        // moves the focus. Every verb, not just Focus — an activation must
        // leave the reader on what it operated.
        if !key_path.is_empty() {
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
        // The widget may have mutated the document from `perform_action_at` —
        // dispatching an action, or asking for a commit — and this route does
        // not go through `send`. Close the tick here too, or an app driven the
        // way an assistive technology drives it leaves no history behind.
        self.host_tick();
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
    pub fn send(&mut self, event: &WindowEvent) {
        let mut handled = false;
        let mut focus_target: Option<NodeId> = None;
        let t_event = self.profiler.as_ref().map(|_| std::time::Instant::now());
        self.root.handle_event(
            event,
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
        // Mirror the App: a widget that claimed focus during dispatch
        // becomes the focused node, so subsequent keystrokes route to it.
        if let Some(t) = focus_target {
            self.set_focused(Some(t));
        }
        self.host_tick();
        self.relayout();
    }

    /// Everything the live host does between one interaction and the next
    /// frame, in the order [`App`](crate::app::App)'s idle tick does it.
    ///
    /// Runs after *every* route that can reach a widget — a window event
    /// ([`send`](Self::send)) and an accessibility verb
    /// ([`perform_a11y_action`](Self::perform_a11y_action)) alike. A route that
    /// skipped it would let a scenario mutate the document without the history
    /// ever learning about it, and a test written over that route could drive
    /// an app but never observe what the driving produced.
    ///
    /// The three steps are ordered, not incidental:
    ///
    /// 1. **Dispatched actions** become one undo boundary, through the shared
    ///    [`undo::record_dispatched_action`](crate::undo::record_dispatched_action)
    ///    the App calls.
    /// 2. **A requested commit** closes the transaction, for widgets that edit
    ///    the document directly instead of dispatching actions.
    /// 3. **Undo/redo the tree asked for** runs last, so an action performed in
    ///    the same tick is already on the stack and is what gets reverted.
    fn host_tick(&mut self) {
        let author = self.author.clone();
        let root: *mut dyn Widget = self.root.as_mut();
        crate::undo::record_dispatched_action(&mut self.state, root, &self.node_map, &author);
        if self.root.needs_commit() {
            // Authored *and* carrying the widget's commit context. An
            // anonymous entry is invisible to author-scoped undo, so a
            // scenario that pressed Ctrl+Z would find nothing where the user
            // finds their last edit; dropping the context loses the
            // provenance the history explorer reads.
            let context = self.root.take_commit_context();
            self.state.commit_action_authored_with_context(
                None,
                None,
                Some(author.clone()),
                context,
            );
            self.root.on_commit();
        }
        // An Undo/Redo control the widget tree activated during that event.
        // Same route the live host takes: the widget asks, the host performs.
        self.drain_undo_requests();
    }

    /// Move the pointer to `(x, y)` and press+release the left button — the
    /// three events a real click produces, in order.
    pub fn click_at(&mut self, x: f64, y: f64) {
        self.send(&WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(x, y),
        });
        self.send(&WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
        self.send(&WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Released,
            button: MouseButton::Left,
        });
    }

    /// Click the centre of the named widget. `Err` names the target when it
    /// is not in the tree, so a stale scenario fails loudly instead of
    /// quietly doing nothing.
    ///
    /// # Errors
    ///
    /// Returns an error if the widget is not found or has a zero-sized rect.
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
        // History bindings are the host's, not the focused widget's — resolved
        // through `undo::shortcut`, the one table the live host reads.
        let mods = self.modifiers.state();
        if let Some(req) = crate::undo::shortcut(spec, mods.control_key(), mods.shift_key()) {
            match req {
                crate::undo::UndoRequest::Undo => self.perform_undo(),
                crate::undo::UndoRequest::Redo => self.perform_redo(),
            };
            return;
        }
        let key = parse_key(spec);
        for state in [ElementState::Pressed, ElementState::Released] {
            self.send(&key_event(&key, state));
        }
    }

    /// Type a string one character at a time.
    pub fn type_text(&mut self, text: &str) {
        for ch in text.chars() {
            self.key(&ch.to_string());
        }
    }

    /// Send one vertical wheel scroll of `(dx, dy)` line-deltas through the
    /// production event path. Content scrollables handle the wheel
    /// regardless of pointer position, so no prior cursor move is needed.
    pub fn scroll(&mut self, dx: f64, dy: f64) {
        // Deltas are line counts chosen by the driver's own caller (test
        // code or an agent), always small integers; there is no fallible
        // f64->f32 conversion to fall back to, and winit's own delta type
        // is f32.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "line-delta counts are small integers; winit's `LineDelta` is f32 and there is no fallible f64->f32 conversion"
        )]
        let (dx, dy) = (dx as f32, dy as f32);
        self.send(&WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: winit::event::MouseScrollDelta::LineDelta(dx, dy),
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
            .map(super::profiling::FrameProfiler::take_capture)
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
        self.send(&WindowEvent::ModifiersChanged(self.modifiers));
    }

    /// Run one action.
    ///
    /// # Errors
    ///
    /// Returns an error if the action cannot be performed.
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
        serde_json::to_string_pretty(&automerge::AutoSerde::from(self.state.doc.document()))
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
    // Bounded by the number of sibling widgets under one parent, nowhere
    // near `usize::MAX`; saturating just makes the bound explicit.
    *idx = idx.saturating_add(1);
    let path = if parent_path.is_empty() {
        format!("{name}[{my_index}]")
    } else {
        format!("{parent_path}/{name}[{my_index}]")
    };
    out.insert(path.clone(), w.get_rect());
    if my_index == 0 {
        let bare = if parent_path.is_empty() {
            name
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
/// can't be built field-by-field from outside that crate. Zero-initialising
/// and overwriting every field we care about is the same workaround the
/// compositor's own driver uses (`shell/src/bin/shots/driver.rs`).
fn key_event(key: &Key, state: ElementState) -> WindowEvent {
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
// Every app's `main` calls this before building any window state (see the
// module doc), so exiting on a malformed `--headless-drive` payload is the
// same "fail before anything is built" contract a CLI arg parser has —
// there is nothing yet to unwind. Threading a `Result` through instead
// would mean every one of the ~15 call sites across `apps/*` duplicates
// this same exit, for no behavioural change.
#[allow(
    clippy::exit,
    reason = "arg-parsing entry point mirrors a CLI flag parser: fails fast before any window/Drop state exists, exactly like the callers already do via `std::process::exit(ui::headless::run(..))`"
)]
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
///
/// # Errors
///
/// Returns an error if the JSON is invalid or does not conform to the expected structure.
pub fn parse_spec(raw: &str) -> Result<Spec, String> {
    let v: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("invalid JSON: {e}"))?;
    if !v.is_object() {
        return Err("payload must be a JSON object".to_owned());
    }
    let mut spec = Spec::default();
    if let Some(value) = v.get("viewport") {
        let vp = value
            .as_array()
            .filter(|vp| vp.len() == 2)
            .ok_or("viewport must be [width, height]")?;
        let width = vp
            .first()
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0)
            .ok_or("viewport width must be a non-zero u32")?;
        let height = vp
            .get(1)
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0)
            .ok_or("viewport height must be a non-zero u32")?;
        spec.viewport = Some((width, height));
    }
    if let Some(p) = v.get("dump").and_then(|x| x.as_str()) {
        spec.dump = Some(p.into());
    }
    if let Some(p) = v.get("targets").and_then(|x| x.as_str()) {
        spec.targets = Some(p.into());
    }
    for a in v
        .get("actions")
        .and_then(|x| x.as_array())
        .unwrap_or(&vec![])
    {
        let obj = a
            .as_object()
            .ok_or_else(|| format!("action must be an object, got {a}"))?;
        if obj.len() != 1 {
            return Err("each action must contain exactly one operation".to_owned());
        }
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
                    xy.first()
                        .and_then(serde_json::Value::as_f64)
                        .ok_or("click_at x")?,
                    xy.get(1)
                        .and_then(serde_json::Value::as_f64)
                        .ok_or("click_at y")?,
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
pub fn run(spec: &Spec, root: Box<dyn Widget>, manifest_id: &str) -> i32 {
    let (state, mut autosave) = crate::persist::open(manifest_id);
    if !autosave.is_enabled() {
        eprintln!(
            "headless: persistence unavailable: {}",
            autosave.last_error().unwrap_or_default()
        );
        return 3;
    }
    // An app whose root names a record must be driven against that record's
    // document, not the room index — see `records` on `Driver`.
    let mut driver = Driver::new(state, root, spec.viewport.unwrap_or(DEFAULT_VIEWPORT));
    if driver.root_record_intent().is_some() {
        let (records_state, _) = crate::persist::open(manifest_id);
        driver = driver.with_records(
            crate::record::RecordSet::open(manifest_id),
            records_state.doc,
        );
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
    use crate::widget::Button;

    #[test]
    fn spec_parses_every_action_shape() {
        let spec = parse_spec(
            r#"{"viewport":[800,600],"dump":"/tmp/d.json","targets":"/tmp/t.json",
                "actions":[{"click_named":"save"},{"click_at":[10,20]},
                           {"key":"Enter"},{"type":"hi"},{"modifiers":["ctrl"]}]}"#,
        )
        .expect("spec parses");
        assert_eq!(spec.viewport, Some((800, 600)));
        assert_eq!(
            spec.dump.as_deref(),
            Some(std::path::Path::new("/tmp/d.json"))
        );
        assert_eq!(spec.actions.len(), 5);
        assert!(matches!(&spec.actions[0], Action::ClickNamed(n) if n == "save"));
        assert!(matches!(&spec.actions[3], Action::Type(t) if t == "hi"));
    }

    #[test]
    fn an_unknown_action_is_rejected_rather_than_ignored() {
        let err = parse_spec(r#"{"actions":[{"teleport":"x"}]}"#).unwrap_err();
        assert!(err.contains("teleport"), "{err}");
    }

    #[test]
    fn malformed_headless_viewports_and_actions_are_rejected() {
        for raw in [
            r#"{"viewport":[0,600]}"#,
            r#"{"viewport":[800]}"#,
            r#"{"viewport":"800x600"}"#,
            r#"{"actions":[{"key":"Enter","type":"oops"}]}"#,
            "[]",
        ] {
            assert!(parse_spec(raw).is_err(), "accepted {raw}");
        }
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
        let mut driver = Driver::new(
            State::new(),
            Box::new(Button::new("b", "B", None)),
            (400, 200),
        );
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
        items: Rc<RefCell<Vec<&'static str>>>,
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
            node_map.insert(n, std::ptr::from_mut::<dyn Widget>(self));
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
        fn name(&self) -> &'static str {
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
                            .borrow()
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
            let Some(&key) = key_path.first() else {
                return false;
            };
            let Some(i) = self
                .items
                .borrow()
                .iter()
                .position(|id| crate::a11y::hash_key(id) == key)
            else {
                return false;
            };
            let name = self.items.borrow()[i];
            match action {
                Action::Focus => {
                    self.cursor = i;
                    true
                }
                Action::Activate => {
                    self.cursor = i;
                    self.activated.borrow_mut().push(name.into());
                    true
                }
                _ => false,
            }
        }
    }

    fn driver() -> (Driver, Rc<RefCell<Vec<String>>>) {
        let (d, activated, _items) = driver_with_items();
        (d, activated)
    }

    #[allow(clippy::type_complexity)]
    fn driver_with_items() -> (
        Driver,
        Rc<RefCell<Vec<String>>>,
        Rc<RefCell<Vec<&'static str>>>,
    ) {
        let activated: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let items: Rc<RefCell<Vec<&'static str>>> =
            Rc::new(RefCell::new(vec!["alpha", "beta", "gamma"]));
        let strip = Strip {
            items: items.clone(),
            cursor: 0,
            focused: false,
            activated: activated.clone(),
        };
        (
            Driver::new(State::new(), Box::new(strip), (800, 600)),
            activated,
            items,
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
    fn a_node_reference_survives_a_relayout() {
        // The gap this closes: the host throws its TaffyTree away and rebuilds
        // it on every relayout, and ids used to be derived from taffy's
        // per-tree node ids — so every id in the exported tree changed on each
        // layout pass. An assistive technology holding a reference across a
        // repaint got an id that no longer resolved, or one that resolved to a
        // different element, which is worse than having no id at all.
        let (mut d, _a) = driver();
        let before: Vec<(String, accesskit::NodeId)> = d
            .a11y_nodes()
            .into_iter()
            .map(|(id, n)| (n.label().unwrap_or_default().to_string(), id))
            .collect();
        assert!(before.len() >= 4, "the canvas and its items are exported");

        // A layout pass, exactly as any frame does.
        d.relayout();

        let after: Vec<(String, accesskit::NodeId)> = d
            .a11y_nodes()
            .into_iter()
            .map(|(id, n)| (n.label().unwrap_or_default().to_string(), id))
            .collect();
        assert_eq!(
            before, after,
            "every exported id must be the same after a relayout"
        );
    }

    #[test]
    fn an_id_taken_before_a_relayout_still_operates_the_same_element() {
        // Stability is only worth anything if the id still *works*: an AT holds
        // its reference across frames and then acts on it.
        let (mut d, activated) = driver();
        let beta = d.a11y_node_named("beta").unwrap();

        d.relayout();
        d.relayout();

        d.perform_a11y_action(beta, &Action::Activate).unwrap();
        assert_eq!(
            *activated.borrow(),
            vec!["beta".to_string()],
            "the reference must still operate the element it named"
        );
    }

    #[test]
    fn a_held_reference_follows_its_element_when_an_earlier_one_is_removed() {
        // Identity keys, observed. Removing an earlier element renumbers every
        // element after it. Keyed by ordinal, a reference an assistive
        // technology took before the removal would afterwards name a DIFFERENT
        // element — it would operate the wrong one, silently. Keyed by
        // identity, it keeps naming the element it was taken from.
        let (mut d, activated, items) = driver_with_items();
        let gamma = d.a11y_node_named("gamma").expect("gamma is addressable");

        // Remove the first element, so gamma slides from third place to second.
        items.borrow_mut().remove(0);
        d.relayout();
        assert!(d.a11y_node_named("alpha").is_none(), "alpha is gone");
        assert!(
            d.a11y_node_named("gamma").is_some(),
            "gamma is still listed"
        );

        d.perform_a11y_action(gamma, &Action::Activate)
            .expect("the reference must still resolve");
        assert_eq!(
            *activated.borrow(),
            vec!["gamma".to_string()],
            "a held reference must operate the element it named, not the one \
             that took its place"
        );
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

/// Undo, driven through the headless harness — the path every scenario and
/// every screenshot takes. A control that worked only under the live event
/// loop would be dead in all of them.
#[cfg(test)]
mod undo_driver_tests {
    use super::*;
    use crate::state::State;

    /// A minimal editor: appends every typed character to `body` and asks for
    /// a commit boundary straight away.
    ///
    /// Deliberately a widget rather than a direct write to the document —
    /// the edit has to travel the driver's own `send` → `needs_commit` →
    /// commit path, or a test cannot see how that path attributes the action.
    struct Typer {
        rect: vello::kurbo::Rect,
        pending: bool,
    }

    impl crate::widget::Widget for Typer {
        fn sync(&mut self, _s: &mut automerge::AutoCommit, _p: &automerge::ObjId) {}

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn crate::widget::Widget>,
        ) -> NodeId {
            let node = taffy
                .new_leaf(Style {
                    size: Size {
                        width: Dimension::length(40.0),
                        height: Dimension::length(20.0),
                    },
                    ..Default::default()
                })
                .unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn crate::widget::Widget>(self));
            node
        }

        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            let l = taffy.layout(node).unwrap();
            let x = origin.x + f64::from(l.location.x);
            let y = origin.y + f64::from(l.location.y);
            self.rect = vello::kurbo::Rect::new(
                x,
                y,
                x + f64::from(l.size.width),
                y + f64::from(l.size.height),
            );
        }

        fn draw(
            &mut self,
            _s: &mut Scene,
            _t: Affine,
            _tr: &mut TextRenderer,
            _th: &Theme,
            _p: &PresenceCtx,
        ) {
        }

        #[allow(clippy::too_many_arguments)]
        fn handle_event(
            &mut self,
            event: &WindowEvent,
            _sf: f64,
            _m: &winit::event::Modifiers,
            doc: &mut automerge::AutoCommit,
            _path: &automerge::ObjId,
            handled: &mut bool,
            _ft: &mut Option<NodeId>,
            _node: NodeId,
            _taffy: &TaffyTree<()>,
        ) {
            use automerge::{ReadDoc, transaction::Transactable};
            let WindowEvent::KeyboardInput { event: ke, .. } = event else {
                return;
            };
            if ke.state != ElementState::Pressed {
                return;
            }
            let winit::keyboard::Key::Character(c) = &ke.logical_key else {
                return;
            };
            let obj = match doc.get(&automerge::ROOT, "body") {
                Ok(Some((_, o))) => o,
                _ => doc
                    .put_object(&automerge::ROOT, "body", automerge::ObjType::Text)
                    .unwrap(),
            };
            let len = doc.length(&obj);
            doc.splice_text(&obj, len, 0, c.as_str()).unwrap();
            self.pending = true;
            *handled = true;
        }

        fn name(&self) -> &'static str {
            "Typer"
        }

        fn get_rect(&self) -> vello::kurbo::Rect {
            self.rect
        }

        fn needs_commit(&self) -> bool {
            self.pending
        }

        fn on_commit(&mut self) {
            self.pending = false;
        }
    }

    /// A typing surface plus history controls, and a driver over them.
    fn undo_driver() -> Driver {
        let row = crate::widget::Row::new()
            .push(Box::new(Typer {
                rect: vello::kurbo::Rect::ZERO,
                pending: false,
            }))
            .push(Box::new(crate::widget::UndoButton::undo()))
            .push(Box::new(crate::widget::UndoButton::redo()));
        Driver::new(State::new(), Box::new(row), (400, 120))
    }

    /// One real edit, typed through the driver and committed by the driver —
    /// so how the driver attributes an action is part of what is under test.
    fn type_action(d: &mut Driver, text: &str) {
        d.type_text(text);
        assert!(
            d.undo_status().can_undo,
            "typing through the driver must produce an undoable action"
        );
    }

    fn body(d: &mut Driver) -> String {
        use automerge::ReadDoc;
        let state = d.state_mut();
        match state.doc.get(&automerge::ROOT, "body") {
            Ok(Some((_, obj))) => state.doc.text(&obj).unwrap_or_default(),
            _ => String::new(),
        }
    }

    /// Clicking the control in a scenario undoes, and reaches the same place
    /// the harness's Ctrl+Z does. A control that only worked live would be
    /// dead in every screenshot and every driven test.
    #[test]
    fn clicking_undo_and_pressing_ctrl_z_reach_the_same_undo() {
        let mut via_click = undo_driver();
        type_action(&mut via_click, "hello");
        via_click
            .click_named("undo_button")
            .expect("the control is addressable once it can act");
        assert_eq!(
            body(&mut via_click),
            "hell",
            "the click undid the last action — one keystroke, one boundary"
        );

        let mut via_key = undo_driver();
        type_action(&mut via_key, "hello");
        via_key.set_modifiers(&["ctrl".to_string()]);
        via_key.key("z");
        assert_eq!(
            body(&mut via_key),
            body(&mut via_click),
            "the shortcut and the control land in the same place"
        );

        // And redo, through the control.
        via_click
            .click_named("redo_button")
            .expect("redo became available");
        assert_eq!(body(&mut via_click), "hello");
    }

    /// The harness must publish the status, or a control renders dead in every
    /// shot regardless of the history behind it.
    #[test]
    fn the_driver_publishes_the_status_to_the_widget_tree() {
        let mut d = undo_driver();

        // Nothing to undo: the control is disabled, so it is not in the focus
        // order the harness tabs through.
        assert!(
            d.focus_order().is_empty(),
            "an empty history leaves no focusable history control"
        );

        type_action(&mut d, "hello");
        assert_eq!(
            d.focus_order().len(),
            1,
            "the Undo control becomes focusable once there is an action"
        );
        let status = d.undo_status();
        assert!(status.can_undo && !status.can_redo);
        // Each keystroke is its own commit boundary here, so the newest
        // action inserted one character.
        assert_eq!(status.undo_summary.as_deref(), Some("+1 -0 chars"));
    }

    /// A scenario that clicks a control with nothing to undo must not silently
    /// change the document.
    #[test]
    fn clicking_a_disabled_control_in_a_scenario_does_nothing() {
        let mut d = undo_driver();
        type_action(&mut d, "hello");
        assert!(!d.undo_status().can_redo, "nothing has been undone");
        // Redo is unavailable: nothing has been undone yet.
        let _ = d.click_named("redo_button");
        assert_eq!(body(&mut d), "hello", "the document is untouched");
    }
}

/// Undo for an app whose edits are dispatched [`qos_actions`] actions — the
/// shape every migrated qOS app has — driven headlessly.
///
/// The module above drives the *other* editing path, `needs_commit` /
/// `on_commit`, where the widget writes the document itself. An app built on
/// [`ActionHost`](crate::action_host::ActionHost) never sets `needs_commit`:
/// its boundary arrives through
/// [`take_committed_action`](crate::widget::Widget::take_committed_action)
/// instead. Until the driver drained that, a headless test of such an app could
/// deliver the input but could not observe the undo history the input produced,
/// so undo behaviour was untestable for the majority of the fleet.
#[cfg(test)]
mod dispatched_action_driver_tests {
    use super::*;
    use crate::a11y::{AccessNode, AccessState, Role};
    use crate::state::State;
    use qos_actions::{ActionError, Doc, ExecutionLocus};
    use serde::{Deserialize, Serialize};

    /// A real action: append `text` to the document's `body`. Deterministic, as
    /// the executor requires.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct Append {
        text: String,
    }

    const APPEND: &str = "test.append";

    impl qos_actions::Action for Append {
        fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
            use automerge::{ReadDoc, transaction::Transactable};
            let obj = match doc.get(&automerge::ROOT, "body") {
                Ok(Some((_, o))) => o,
                _ => doc.put_object(&automerge::ROOT, "body", automerge::ObjType::Text)?,
            };
            let len = doc.length(&obj);
            doc.splice_text(&obj, len, 0, &self.text)?;
            Ok(())
        }
        fn locus(&self) -> ExecutionLocus {
            ExecutionLocus::ClientLocal
        }
    }

    /// An editor in the migrated shape: every edit is dispatched through
    /// [`ActionHost`](crate::action_host::ActionHost), and the only thing it
    /// tells the host is the bracket. It deliberately does **not** implement
    /// `needs_commit` — that is the whole distinction under test.
    ///
    /// Two ways in, because a canvas app has two: a keystroke, and an
    /// accessibility verb routed to `perform_action_at` (how an assistive
    /// technology, and every headless scenario over a canvas, drives it).
    struct ActionEditor {
        rect: vello::kurbo::Rect,
        actions: crate::action_host::ActionHost,
        focused: bool,
        /// Whether this editor claims the event. A `Row` stops dispatching to
        /// later siblings once a child claims one, so a second editor that must
        /// also see the keystroke is built without the claim — the shape of a
        /// pane that reacts to an edit alongside the widget that owns it.
        claims: bool,
    }

    impl ActionEditor {
        fn new() -> Self {
            Self {
                rect: vello::kurbo::Rect::ZERO,
                actions: crate::action_host::ActionHost::new(),
                focused: false,
                claims: true,
            }
        }

        /// An editor that dispatches but lets the event travel on.
        fn passive() -> Self {
            Self {
                claims: false,
                ..Self::new()
            }
        }
    }

    impl crate::widget::Widget for ActionEditor {
        fn sync(&mut self, _s: &mut automerge::AutoCommit, _p: &automerge::ObjId) {}

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn crate::widget::Widget>,
        ) -> NodeId {
            let node = taffy
                .new_leaf(Style {
                    size: Size {
                        width: Dimension::length(60.0),
                        height: Dimension::length(20.0),
                    },
                    ..Default::default()
                })
                .unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn crate::widget::Widget>(self));
            node
        }

        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            let l = taffy.layout(node).unwrap();
            let x = origin.x + f64::from(l.location.x);
            let y = origin.y + f64::from(l.location.y);
            self.rect = vello::kurbo::Rect::new(
                x,
                y,
                x + f64::from(l.size.width),
                y + f64::from(l.size.height),
            );
        }

        fn draw(
            &mut self,
            _s: &mut Scene,
            _t: Affine,
            _tr: &mut TextRenderer,
            _th: &Theme,
            _p: &PresenceCtx,
        ) {
        }

        #[allow(clippy::too_many_arguments)]
        fn handle_event(
            &mut self,
            event: &WindowEvent,
            _sf: f64,
            _m: &winit::event::Modifiers,
            doc: &mut automerge::AutoCommit,
            _path: &automerge::ObjId,
            handled: &mut bool,
            _ft: &mut Option<NodeId>,
            _node: NodeId,
            _taffy: &TaffyTree<()>,
        ) {
            let WindowEvent::KeyboardInput { event: ke, .. } = event else {
                return;
            };
            if ke.state != ElementState::Pressed {
                return;
            }
            let winit::keyboard::Key::Character(c) = &ke.logical_key else {
                return;
            };
            // "2" dispatches two actions within the one event, so a test can
            // prove the host brackets the *event*, not each action.
            if c.as_str() == "2" {
                for text in ["x", "y"] {
                    self.actions.dispatch(
                        doc,
                        APPEND,
                        &Append {
                            text: text.to_string(),
                        },
                    );
                }
            } else {
                self.actions.dispatch(
                    doc,
                    APPEND,
                    &Append {
                        text: c.as_str().to_string(),
                    },
                );
            }
            *handled = self.claims;
        }

        fn perform_action_at(
            &mut self,
            action: &crate::a11y::Action,
            _key_path: &[u64],
            doc: &mut automerge::AutoCommit,
            _path: &automerge::ObjId,
        ) -> bool {
            if !matches!(action, crate::a11y::Action::Activate) {
                return false;
            }
            self.actions.dispatch(
                doc,
                APPEND,
                &Append {
                    text: "!".to_string(),
                },
            );
            true
        }

        fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
            self.actions.take_committed()
        }

        fn name(&self) -> &'static str {
            "ActionEditor"
        }

        fn get_rect(&self) -> vello::kurbo::Rect {
            self.rect
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
                    .with_name("Editor")
                    .with_state(AccessState::new().focusable(true).focused(self.focused)),
            )
        }
    }

    /// The editor plus the history controls an app would put in its toolbar.
    fn action_driver() -> Driver {
        let row = crate::widget::Row::new()
            .push(Box::new(ActionEditor::new()))
            .push(Box::new(crate::widget::UndoButton::undo()))
            .push(Box::new(crate::widget::UndoButton::redo()));
        Driver::new(State::new(), Box::new(row), (400, 120))
    }

    fn body(d: &mut Driver) -> String {
        use automerge::ReadDoc;
        let state = d.state_mut();
        match state.doc.get(&automerge::ROOT, "body") {
            Ok(Some((_, obj))) => state.doc.text(&obj).unwrap_or_default(),
            _ => String::new(),
        }
    }

    /// The whole covenant on one interaction: a real edit dispatched as an
    /// action, the Undo control lighting because of it, pressing that control,
    /// and the edit going away.
    ///
    /// Every step is the driver's own output — `undo_status` is what the
    /// harness publishes to the tree, `click_named` is the production click
    /// path, and the document is read back from the live state.
    #[test]
    fn an_edit_dispatched_as_an_action_lights_undo_and_reverts_when_pressed() {
        let mut d = action_driver();

        assert!(
            !d.undo_status().can_undo,
            "nothing has happened yet, so the control must be dark"
        );

        d.type_text("a");
        assert_eq!(
            body(&mut d),
            "a",
            "the dispatched action reached the document"
        );
        assert!(
            d.undo_status().can_undo,
            "an action-dispatched edit must be an undo boundary the local user owns"
        );

        d.click_named("undo_button")
            .expect("the control is addressable once it can act");
        assert_eq!(body(&mut d), "", "pressing Undo reverted the edit");
        assert!(
            d.undo_status().can_redo,
            "and the reverted action is available to redo"
        );

        d.click_named("redo_button").expect("redo became available");
        assert_eq!(body(&mut d), "a", "redo put the action back");
    }

    /// The keyboard and the control must reach the same undo — for the action
    /// path too, not just the `needs_commit` one.
    #[test]
    fn ctrl_z_and_the_control_undo_the_same_dispatched_action() {
        let mut via_key = action_driver();
        via_key.type_text("a");
        via_key.set_modifiers(&["ctrl".to_string()]);
        via_key.key("z");

        let mut via_click = action_driver();
        via_click.type_text("a");
        via_click.click_named("undo_button").expect("addressable");

        assert_eq!(body(&mut via_key), "");
        assert_eq!(
            body(&mut via_key),
            body(&mut via_click),
            "the shortcut and the control land in the same place"
        );
    }

    /// A canvas app is driven through the accessibility tree, not through
    /// clicks — `perform_a11y_action` is the route
    /// [`docs/headless-testing.md`] tells scenarios to take. That route mutates
    /// the document without going through `send`, so it needs the boundary
    /// recorded too.
    #[test]
    fn an_action_performed_through_accessibility_is_undoable() {
        let mut d = action_driver();
        let node = d
            .a11y_node_named("Editor")
            .expect("the canvas is exported to the accessibility tree");

        d.perform_a11y_action(node, &crate::a11y::Action::Activate)
            .expect("the widget consumes Activate");

        assert_eq!(body(&mut d), "!", "the a11y verb dispatched the action");
        assert!(
            d.undo_status().can_undo,
            "an action performed through accessibility must be undoable too"
        );
        assert!(d.perform_undo(), "and the history actually pops it");
        assert_eq!(body(&mut d), "");
    }

    /// One event is one undo boundary, however many actions it dispatched —
    /// [`ActionHost`](crate::action_host::ActionHost)'s contract, observed
    /// through the driver rather than asserted against the host in isolation.
    #[test]
    fn several_actions_in_one_event_are_one_boundary() {
        let mut d = action_driver();
        d.type_text("2"); // dispatches two Appends within the single keystroke
        assert_eq!(body(&mut d), "xy", "both actions applied");

        assert!(d.perform_undo(), "the batch is undoable");
        assert_eq!(
            body(&mut d),
            "",
            "one undo reverted the whole event, not half of it"
        );
        assert!(
            !d.undo_status().can_undo,
            "and there is no second boundary left behind"
        );
    }

    /// Two separate interactions are two separate boundaries, undone newest
    /// first. Guards the rising-edge drain: a host that failed to clear the
    /// bracket would bracket the second edit from the first edit's heads and
    /// revert both at once.
    #[test]
    fn separate_interactions_are_separate_boundaries() {
        let mut d = action_driver();
        d.type_text("a");
        d.type_text("b");
        assert_eq!(body(&mut d), "ab");

        assert!(d.perform_undo());
        assert_eq!(body(&mut d), "a", "undo reverted only the newer edit");
        assert!(d.perform_undo());
        assert_eq!(body(&mut d), "");
    }

    /// Two widgets dispatching in the same tick is still one boundary, and it
    /// spans from the earlier of their brackets.
    ///
    /// The second widget captures its bracket *after* the first has already
    /// changed the document, so recording from that later bracket would strand
    /// the first widget's edit outside the entry: one Ctrl+Z would revert half
    /// the tick and leave the rest with nothing to undo it. Which bracket the
    /// sweep happens to see first is a hash-map order, so this cannot be left
    /// to luck.
    #[test]
    fn two_widgets_dispatching_in_one_tick_are_one_boundary_from_the_earlier() {
        // Both editors handle the same keystroke, so a single key press
        // dispatches from each in turn.
        let row = crate::widget::Row::new()
            .push(Box::new(ActionEditor::passive()))
            .push(Box::new(ActionEditor::new()))
            .push(Box::new(crate::widget::UndoButton::undo()));
        let mut d = Driver::new(State::new(), Box::new(row), (400, 120));

        d.type_text("a");
        assert_eq!(body(&mut d), "aa", "both widgets dispatched");

        assert!(d.perform_undo(), "the tick is undoable");
        assert_eq!(
            body(&mut d),
            "",
            "one undo reverted the whole tick — the entry started before the \
             first widget's action, not between the two"
        );
        assert!(
            !d.undo_status().can_undo,
            "and left no orphaned second boundary"
        );
    }

    /// An event that dispatches nothing must not push an empty entry, or the
    /// next Ctrl+Z is spent on a no-op and the user's real edit survives a
    /// press they expected to undo it.
    #[test]
    fn an_event_that_dispatches_nothing_records_no_boundary() {
        let mut d = action_driver();
        d.type_text("a");

        // A pointer move: reaches the tree, dispatches no action.
        d.send(&WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(5.0, 5.0),
        });

        assert!(
            d.perform_undo(),
            "the real edit is still the top of the stack"
        );
        assert_eq!(body(&mut d), "", "and one press reverted it");
        assert!(
            !d.undo_status().can_undo,
            "the pointer move left nothing behind"
        );
    }
}
