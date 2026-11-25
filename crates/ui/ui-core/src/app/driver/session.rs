//! What a widget tree on a surface of its own shows and keeps, as a window of its own does: the
//! parts and layers its frames record for a host that poses and compares them, the composition it
//! shows where a host renders it as elements, the keyboard it holds and the tooltip it paints.
//!
//! Everything here reads the app's own laid-out frame ([`App`](super::App)) or is recorded from the
//! frames the driver runs; the driver keeps none of it of its own.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use vello::Scene;

use super::Driver;
use crate::composition::{Composed, Composition, Picture, css};
use crate::widget::Widget;

// ---- what a host that poses and compares frames keeps ----

/// What the driver keeps of the frames it runs for a host that lifts parts out of them, compares
/// each with the one it presented, and blurs what lies under its material.
#[derive(Default)]
pub(super) struct FrameRecords {
    /// The frames record their damage parts ([`Driver::track_scene_parts`]).
    track_parts: bool,
    /// The frames lift out the parts that move as a whole ([`Driver::lift_parts`]).
    lift_parts: bool,
    /// The damage parts of the last frame drawn.
    parts: Option<crate::scene_parts::SectionParts>,
    /// The parts lifted out of the last frame drawn, bottom first.
    lifted: Vec<crate::lift::Lifted>,
    /// The surfaces the last frame drew its blurred material over.
    backdrops: std::sync::Arc<crate::backdrop::BackdropFrame>,
    /// The layout failure last reported to a host, once it was read: a host whose log is not
    /// `tracing`'s reads it ([`Driver::take_skip_report`]).
    reported_skip: Option<String>,
}

/// The recordings a frame keeps, begun before its draw and finished after it.
pub(super) struct Recording {
    parts: Option<crate::scene_parts::Recording>,
    lift: Option<crate::lift::Recording>,
    backdrops: crate::backdrop::BackdropRecording,
}

impl FrameRecords {
    /// Begin what the frame about to draw into `scene` keeps: its parts when they are tracked, its
    /// lifted layers when they are lifted, and the surfaces drawn in blurred material always, since
    /// a host asks what lies under them every frame.
    pub(super) fn begin(&self, scene: &Scene) -> Recording {
        let backdrops = crate::backdrop::BackdropRecording::begin();
        crate::backdrop::layer_scene(scene, 0);
        let parts = self.track_parts.then(crate::scene_parts::Recording::begin);
        if parts.is_some() {
            crate::scene_parts::track(scene);
        }
        let lift = self
            .lift_parts
            .then(|| crate::lift::Recording::begin(scene));
        Recording {
            parts,
            lift,
            backdrops,
        }
    }

    /// Keep what `recording` saw of the frame drawn into `scene`.
    pub(super) fn finish(&mut self, recording: Recording, scene: &Scene) {
        self.lifted = recording
            .lift
            .map(crate::lift::Recording::finish)
            .unwrap_or_default();
        let recorded = recording.parts.map(crate::scene_parts::Recording::finish);
        self.parts = recorded.as_ref().and_then(|recorded| recorded.of(scene));
        if let Some(recorded) = &recorded {
            for lifted in &mut self.lifted {
                lifted.parts = recorded.of(&lifted.scene);
            }
        }
        self.backdrops = std::sync::Arc::new(crate::backdrop::BackdropFrame::from_single_scene(
            recording.backdrops.finish(),
        ));
    }

    /// The backdrops of the frame drawn last, for a renderer that blurs the material drawn over
    /// them.
    pub(super) fn captured_backdrops(&self) -> std::sync::Arc<crate::backdrop::BackdropFrame> {
        std::sync::Arc::clone(&self.backdrops)
    }
}

impl Driver {
    /// Record the damage parts of the frames drawn from now on
    /// ([`crate::scene_parts`]), for a host that compares each frame with the one it presented
    /// before.
    pub fn track_scene_parts(&mut self, track: bool) {
        self.records.track_parts = track;
        if !track {
            self.records.parts = None;
        }
    }

    /// Lift the parts that move as a whole out of the frames drawn from now on
    /// ([`crate::lift`]), for a host whose compositor poses them.
    pub fn lift_parts(&mut self, lift: bool) {
        self.records.lift_parts = lift;
        if !lift {
            self.records.lifted.clear();
        }
    }

    /// The damage parts of the frame drawn last, when they were recorded
    /// ([`Self::track_scene_parts`]) and well nested.
    #[must_use]
    pub fn scene_parts(&self) -> Option<&crate::scene_parts::SectionParts> {
        self.records.parts.as_ref()
    }

    /// The parts lifted out of the frame drawn last, bottom first: they stand below it.
    #[must_use]
    pub fn lifted(&self) -> &[crate::lift::Lifted] {
        &self.records.lifted
    }

    /// The backdrops the frame drawn last draws its blurred material over: what a compositor blurs
    /// and hands back.
    #[must_use]
    pub fn backdrops(&self) -> &crate::backdrop::BackdropFrame {
        &self.records.backdrops
    }
}

// ---- the keyboard, the tooltip and the controls it lays out ----

impl Driver {
    /// Say whether the surface this tree draws holds the keyboard. A surface that lost it keeps its
    /// fields' text and selection and paints no caret until it has it back. Whether that changed,
    /// so a host redraws the frame the caret was in.
    pub fn set_keyboard_held(&mut self, held: bool) -> bool {
        if self.app.input.keyboard_held == held {
            return false;
        }
        self.app.input.keyboard_held = held;
        self.app.root_widget.release_scene_cache();
        if let Some(palette) = self.app.palette_widget_mut() {
            palette.release_scene_cache();
        }
        if let Some(history) = self.app.history.as_mut() {
            history.release_scene_cache();
        }
        true
    }

    /// Whether the surface this tree draws holds the keyboard.
    #[must_use]
    pub fn keyboard_held(&self) -> bool {
        self.app.input.keyboard_held
    }

    /// The text and chip (logical) of the tooltip the last frame painted.
    #[must_use]
    pub fn drawn_tooltip(&self) -> Option<(&str, vello::kurbo::Rect)> {
        self.app.input.tooltips.drawn()
    }

    /// The tooltip the last frame painted, as a node of the frame's inspection: it is painted
    /// over the tree rather than being one of its widgets, so a reader and an agent are told it
    /// here.
    pub(super) fn tooltip_inspection(&self) -> Option<crate::headless::InspectionNode> {
        let (text, chip) = self.app.input.tooltips.drawn()?;
        Some(crate::headless::InspectionNode {
            path: "tooltip".into(),
            widget: "Tooltip".into(),
            test_id: None,
            focusable: false,
            rect: [chip.x0, chip.y0, chip.width(), chip.height()],
            child_clip: None,
            page_title: None,
            layout: None,
            paint_key: None,
            semantics: Some(
                crate::a11y::AccessNode::new(crate::a11y::Role::Tooltip).with_name(text.to_owned()),
            ),
            children: Vec::new(),
        })
    }

    /// Which surface this tree is on the thread's motion clock: what moves for what it draws runs
    /// on its behalf ([`crate::motion::on_surface`]).
    #[must_use]
    pub fn motion_surface(&self) -> crate::motion::MotionSurface {
        crate::motion::MotionSurface(self.app.motion_owner())
    }

    /// Ask for the tree to be built again before it is next laid out: what a widget that reads
    /// something outside it (a handle another part of the shell writes) asks when that changed,
    /// so the frame it brings takes the change up.
    pub fn request_layout(&mut self) {
        self.app.layout.request_rebuild();
        self.surface.redraw.set(true);
    }

    /// The widgets that make a retained frame re-solve sizes rather than only re-apply positions:
    /// each widget reporting [`Widget::layout_needs_measure`] that none of its children explains,
    /// as its path from the root with a test id in brackets.
    #[must_use]
    pub fn layout_measure_requesters(&self) -> Vec<String> {
        fn visit(widget: &dyn Widget, path: &mut Vec<String>, out: &mut Vec<String>) {
            let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
                return;
            };
            if !widget.layout_needs_measure() {
                return;
            }
            path.push(widget.test_id().map_or_else(
                || widget.name().to_owned(),
                |id| format!("{}[{id}]", widget.name()),
            ));
            let before = out.len();
            for child in widget.children() {
                visit(child, path, out);
            }
            if out.len() == before {
                out.push(path.join("/"));
            }
            path.pop();
        }
        let mut out = Vec::new();
        visit(self.app.root_widget.as_ref(), &mut Vec::new(), &mut out);
        out
    }

    /// Whether a capture may not show this app now: everything it asked for, and any window while
    /// the frame it drew shows a secret field with a value.
    #[must_use]
    pub fn capture_protected(&self) -> bool {
        self.app.withholds_captures()
    }

    /// The accessibility node `id` names, as the last frame exported it.
    #[must_use]
    pub fn a11y_node(&self, id: accesskit::NodeId) -> Option<accesskit::Node> {
        self.a11y_nodes()
            .into_iter()
            .find_map(|(other, node)| (other == id).then_some(node))
    }

    /// The ids of every exported node whose accessible name is `name`, or whose row title is
    /// exactly `name`, in tree order: a list row is named by all of its text, its title and its
    /// secondary line, so the line it is titled with finds it. Nodes named `name` come before rows
    /// titled it.
    #[must_use]
    pub fn a11y_nodes_named(&self, name: &str) -> Vec<accesskit::NodeId> {
        let titled: HashMap<_, _> = self.row_titles().into_iter().collect();
        let nodes = self.a11y_nodes();
        let mut named: Vec<accesskit::NodeId> = nodes
            .iter()
            .filter(|(_, node)| node.label() == Some(name))
            .map(|(id, _)| *id)
            .collect();
        named.extend(nodes.iter().filter_map(|(id, node)| {
            (node.label() != Some(name) && titled.get(id).is_some_and(|title| title == name))
                .then_some(*id)
        }));
        named
    }

    /// What this tree's layout said went wrong since this was last asked, once per failure: a
    /// frame whose layout failed and was skipped. The line is reported to a host whose log is not
    /// `tracing`'s; the failure itself is logged as every host logs it.
    pub fn take_skip_report(&mut self) -> Option<String> {
        let reported = self.app.layout.skipped_frames.reported()?.to_owned();
        if self.records.reported_skip.as_deref() == Some(reported.as_str()) {
            return None;
        }
        self.records.reported_skip = Some(reported.clone());
        Some(format!(
            "widget layout failed; skipping the frame: {reported}"
        ))
    }
}

// ---- the models a worker woke ----

/// The queue of owner models a worker woke for the components this tree hosts: what an idle host
/// services between frames.
#[derive(Default)]
pub(super) struct OwnerWakes {
    queue: Option<crate::owner_wake::OwnerWakeQueue>,
}

impl Driver {
    /// Attach the queue shared by this tree's hosted components, so a service reaches the owner a
    /// worker woke ([`Self::service_pending_owner_models`]).
    ///
    /// # Errors
    /// The registration identity cannot be replaced once it is attached.
    pub fn attach_owner_wake_queue(
        &mut self,
        queue: crate::owner_wake::OwnerWakeQueue,
    ) -> Result<(), crate::bound_dispatch::BoundDispatchError> {
        if self.wakes.queue.is_some() {
            return Err(crate::bound_dispatch::BoundDispatchError::ClosedLifetime);
        }
        self.wakes.queue = Some(queue);
        Ok(())
    }

    /// Service one batch of the owner models a worker woke: the owners whose record's save moved on
    /// are told the status they show, and any other owner is served its model through the tree,
    /// which the next frame builds again. How many owners were served.
    ///
    /// # Errors
    /// A refusal keeps the rest of the batch for a later service.
    pub fn service_pending_owner_models(
        &mut self,
    ) -> Result<usize, crate::bound_dispatch::BoundDispatchError> {
        let Some(queue) = self.wakes.queue.clone() else {
            return Ok(0);
        };
        let mut batch = queue.take_batch();
        let mut served = 0;
        if batch.front().is_none() {
            return Ok(0);
        }
        if batch.all_save_progress_only() {
            while let Some(work) = batch.front() {
                if crate::bound_dispatch::service_owner_save_progress(
                    self.app.root_widget.as_mut(),
                    work,
                )? {
                    self.surface.redraw.set(true);
                }
                batch.complete_front();
                served += 1;
            }
            return Ok(served);
        }
        // A component's model may replace the widgets under it: the frame that shows what it wrote
        // builds the tree again rather than reading maps into widgets that went.
        self.app.layout.node_map.clear();
        self.app.layout.request_rebuild();
        self.app.needs_sync = true;
        self.surface.redraw.set(true);
        while let Some(work) = batch.front() {
            crate::bound_dispatch::service_retained_owner_model(
                self.app.root_widget.as_mut(),
                &mut self.app.state.doc,
                work,
            )?;
            batch.complete_front();
            served += 1;
        }
        Ok(served)
    }

    /// Whether the owner queue holds anything to service.
    #[must_use]
    pub fn has_pending_owner_models(&self) -> bool {
        self.wakes
            .queue
            .as_ref()
            .is_some_and(crate::owner_wake::OwnerWakeQueue::has_pending)
    }
}

// ---- the composition a host renders as elements ----

/// Every widget of the laid-out tree by its address: the path semantic actions name it by, and the
/// widget.
fn index_widgets<'a>(
    widget: &'a dyn Widget,
    path: String,
    out: &mut HashMap<usize, Vec<(String, &'a dyn Widget)>>,
) {
    let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
        return;
    };
    let mut child_counts = HashMap::new();
    for (index, child) in widget.children().into_iter().enumerate() {
        let (_, child_path, _) = crate::widget::widget_path(
            child,
            &path,
            widget.child_path_name(index),
            &mut child_counts,
        );
        index_widgets(child, child_path, out);
    }
    // A widget whose first field is a child widget shares its address: the one that holds the
    // other comes first.
    out.entry(address(widget))
        .or_default()
        .insert(0, (path, widget));
}

fn address(widget: &dyn Widget) -> usize {
    std::ptr::from_ref(widget).cast::<()>() as usize
}

/// The layout tree, walked from its root, with the widget each node is.
struct Walk<'a> {
    /// The widgets at each address, the one holding the others first.
    widgets: HashMap<usize, Vec<(String, &'a dyn Widget)>>,
    node_widgets: HashMap<NodeId, usize>,
    taffy: &'a TaffyTree<()>,
    placed: std::cell::RefCell<std::collections::HashSet<usize>>,
    /// The addresses of the widgets being composed, outermost first.
    open: std::cell::RefCell<Vec<usize>>,
    /// What a widget's parent publishes for it in its own nodes, by the widget's address: a
    /// list's row with its place in the list, its level and its disclosure.
    represented: std::cell::RefCell<HashMap<usize, crate::a11y::AccessNode>>,
    /// The surfaces open over the window that its layout does not hold
    /// ([`Widget::overlay_surface`]), composed as layers of their own.
    lifted: std::cell::RefCell<Vec<Composed>>,
}

impl<'a> Walk<'a> {
    /// The widget a node at `address` is: the outermost of those there not already being
    /// composed around it, so a banner's node is the banner and its body's node is the body.
    fn widget_at(&self, address: usize) -> Option<&(String, &'a dyn Widget)> {
        let open = self
            .open
            .borrow()
            .iter()
            .filter(|&&at| at == address)
            .count();
        self.widgets.get(&address).and_then(|all| all.get(open))
    }

    /// The innermost widget at `address`.
    fn innermost(&self, address: usize) -> Option<&(String, &'a dyn Widget)> {
        self.widgets.get(&address).and_then(|all| all.last())
    }

    /// What `widget` publishes for each child it represents in its own nodes (`access`), kept
    /// for when that child is composed: the node at the path it hangs the child at, or the
    /// node standing where the child stands.
    fn represent(&self, widget: &dyn Widget, access: &crate::a11y::AccessNode) {
        let mut represented = self.represented.borrow_mut();
        for (index, child) in widget.children().into_iter().enumerate() {
            if !widget.accessibility_represents_child(index) {
                continue;
            }
            let node = match widget.accessibility_child_node(index) {
                Some(path) if !path.is_empty() => path
                    .iter()
                    .try_fold(access, |node, &at| node.children.get(at)),
                _ => {
                    let rect = child.get_rect();
                    access.children.iter().find(|node| {
                        node.bounds.is_some_and(|bounds| {
                            (bounds.x0 - rect.x0).abs() < 0.5
                                && (bounds.y0 - rect.y0).abs() < 0.5
                                && (bounds.x1 - rect.x1).abs() < 0.5
                                && (bounds.y1 - rect.y1).abs() < 0.5
                        })
                    })
                }
            };
            if let Some(node) = node {
                represented.insert(address(child), node.clone());
            }
        }
    }

    fn flows(&self, node: NodeId, origin: (f64, f64), children: &[Composed]) -> bool {
        let nodes = self.taffy.children(node).unwrap_or_default();
        // A page lays out a flex or grid container's children from its style, with the grid's
        // tracks and each child's place in them, the way the layout did.
        let laid = self.taffy.style(node).is_ok_and(|style| {
            matches!(style.display, taffy::Display::Flex | taffy::Display::Grid)
        });
        laid && !children.is_empty()
            && nodes.iter().zip(children).all(|(&child, composed)| {
                self.taffy
                    .style(child)
                    .is_ok_and(|style| style.position != taffy::Position::Absolute)
                    && self.taffy.layout(child).is_ok_and(|layout| {
                        (origin.0 + f64::from(layout.location.x) - composed.rect[0]).abs() < 0.5
                            && (origin.1 + f64::from(layout.location.y) - composed.rect[1]).abs()
                                < 0.5
                    })
            })
    }

    /// Compose `surface`, open over the window from the widget at `path`, as a layer of its own,
    /// and take it out of what the widget publishes (`owner`): it stands over the window, not in
    /// the widget's box. A press on one of its rows still reaches the widget that opened it, the
    /// one its path names before the `#`.
    fn lift(&self, path: &str, surface: &dyn Widget, owner: Option<&mut crate::a11y::AccessNode>) {
        let Some(access) = surface.accessibility() else {
            return;
        };
        if let Some(owner) = owner {
            owner
                .children
                .retain(|child| !(child.role == access.role && child.name == access.name));
        }
        let rect = surface.get_rect();
        let mut layer = Composed::new(
            format!("{path}#{}", surface.name()),
            surface.name(),
            [rect.x0, rect.y0, rect.width(), rect.height()],
        );
        layer.semantics = Some(crate::a11y::with_synthetic_bounds(access, surface));
        layer.appears_from = surface.appears_from().map(|point| [point.x, point.y]);
        self.lifted.borrow_mut().push(layer);
    }

    fn compose(
        &self,
        node: NodeId,
        origin: (f64, f64),
        anonymous: &mut usize,
        parent: &str,
    ) -> Composed {
        let layout = self.taffy.layout(node).ok();
        let (x, y) = layout.map_or(origin, |layout| {
            (
                origin.0 + f64::from(layout.location.x),
                origin.1 + f64::from(layout.location.y),
            )
        });
        let size = layout.map_or((0.0, 0.0), |layout| {
            (f64::from(layout.size.width), f64::from(layout.size.height))
        });
        let at = self.node_widgets.get(&node).copied();
        let widget = at.and_then(|at| self.widget_at(at));
        let css = self.taffy.style(node).ok().map(css);
        let children_nodes = self.taffy.children(node).unwrap_or_default();
        let Some((path, widget)) = widget else {
            *anonymous = anonymous.saturating_add(1);
            let path = format!("{parent}#{anonymous}");
            let mut inner = 0;
            let mut children: Vec<Composed> = children_nodes
                .into_iter()
                .map(|child| self.compose(child, (x, y), &mut inner, &path))
                .collect();
            let mut composed = Composed::new(path, "", [x, y, size.0, size.1]);
            composed.flows = self.flows(node, (x, y), &children);
            let tiled = if composed.flows {
                None
            } else {
                tiles(composed.rect, 0.0, &mut children)
            };
            composed.flows |= tiled.is_some();
            composed.layout = match tiled {
                Some(grid) => Some(format!("{}{grid}", css.unwrap_or_default())),
                None => css,
            };
            composed.children = children;
            return composed;
        };
        let widget: &dyn Widget = *widget;
        self.placed.borrow_mut().insert(address(widget));
        let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
            return Composed::new(path.clone(), "ClosedComponent", [x, y, size.0, size.1]);
        };
        let rect = widget.get_rect();
        let mut semantics = self
            .represented
            .borrow_mut()
            .remove(&address(widget))
            .or_else(|| {
                (!widget.accessibility_hidden())
                    .then(|| widget.accessibility())
                    .flatten()
                    .map(|access| crate::a11y::with_synthetic_bounds(access, widget))
            });
        if let Some(surface) = widget.overlay_surface() {
            self.lift(path, surface, semantics.as_mut());
        }
        if let Some(access) = semantics
            .as_ref()
            .filter(|access| !access.children.is_empty())
        {
            self.represent(widget, access);
        }
        let base = (rect.x0, rect.y0);
        // A widget hidden from assistive technology that still stands where it is drawn (a
        // control shown still in one state, as a style guide shows it) is a picture of its paint
        // on a page: what it holds is not there to use.
        let pictured = semantics.as_ref().is_some_and(|access| access.state.hidden)
            && rect.width() > 0.0
            && rect.height() > 0.0;
        self.open.borrow_mut().extend(at);
        let mut inner = 0;
        let mut children: Vec<Composed> = if pictured {
            Vec::new()
        } else {
            children_nodes
                .iter()
                .map(|&child| self.compose(child, base, &mut inner, path))
                .collect()
        };
        if at.is_some() {
            self.open.borrow_mut().pop();
        }
        let mut composed = Composed::new(
            path.clone(),
            widget.name(),
            [rect.x0, rect.y0, rect.width(), rect.height()],
        );
        composed.flows = self.flows(node, base, &children);
        // A widget that places its children by hand (a shell's columns, a list's rows, a field's
        // button beside its text, a banner's buttons over its body) lays out on a page as the
        // grid their edges make.
        let tiled = if composed.flows {
            None
        } else {
            let scrolled = widget
                .primary_vertical_scroll()
                .map_or(0.0, |region| region.metrics.offset);
            tiles(composed.rect, scrolled, &mut children)
        };
        composed.flows |= tiled.is_some();
        composed.clip = widget
            .child_clip_rect()
            .map(|r| [r.x0, r.y0, r.width(), r.height()]);
        composed.layout = match tiled {
            Some(grid) => Some(format!("{}{grid}", css.unwrap_or_default())),
            None => css,
        };
        composed.scroll = widget.primary_vertical_scroll().map(|region| {
            [
                region.metrics.offset,
                region.metrics.content,
                region.metrics.viewport,
            ]
        });
        composed.focusable = widget.is_focusable();
        composed.appears_from = widget.appears_from().map(|point| [point.x, point.y]);
        // A live picture is decoded by the host that shows it: no still of the widget's paint is
        // made for it, so a page carries the stream itself.
        composed.live = widget.live_picture();
        if composed.live.is_none()
            && (pictured
                || children.is_empty()
                    && crate::composition::page::shows_paint(semantics.as_ref(), composed.rect))
        {
            composed.picture = Some(Picture::default());
        }
        // A glyph is a vector: a host that draws no pictures still has the shape to show.
        composed.glyph = widget
            .as_any()
            .and_then(|any| any.downcast_ref::<crate::widget::IconWidget>())
            .map(|icon| icon.icon().name().to_owned());
        composed.semantics = semantics;
        composed.children = children;
        composed
    }
}

impl Driver {
    /// The laid-out app's composition: what a host that renders it with elements of its own shows
    /// ([`crate::composition`]).
    #[must_use]
    pub fn composition(&self) -> Composition {
        let mut widgets = HashMap::new();
        let mut sibling_counts = HashMap::new();
        for root in self.app.presentation_roots() {
            let (_, path, _) = crate::widget::widget_path(root, "", None, &mut sibling_counts);
            index_widgets(root, path, &mut widgets);
        }
        let walk = Walk {
            widgets,
            node_widgets: self
                .app
                .layout
                .node_map
                .iter()
                .map(|(&node, &widget)| (node, widget.cast::<()>() as usize))
                .collect(),
            taffy: &self.app.layout.taffy,
            placed: std::cell::RefCell::default(),
            open: std::cell::RefCell::default(),
            represented: std::cell::RefCell::default(),
            lifted: std::cell::RefCell::default(),
        };
        let (width, height) = self.viewport();
        let mut anonymous = 0;
        let root = if let Some(root_node) = self.app.layout.root_node {
            walk.compose(root_node, (0.0, 0.0), &mut anonymous, "")
        } else {
            Composed::new(
                String::new(),
                "",
                [0.0, 0.0, f64::from(width), f64::from(height)],
            )
        };
        // What the layout tree does not reach stands over it: the palette, an open menu, a dialog,
        // each laid out as a tree of its own.
        let layers = self
            .app
            .layout
            .node_map
            .keys()
            .copied()
            .filter(|&node| {
                Some(node) != self.app.layout.root_node
                    && self.app.layout.taffy.parent(node).is_none()
                    && walk.node_widgets.get(&node).is_some_and(|address| {
                        !walk.placed.borrow().contains(address)
                            && walk.innermost(*address).is_some_and(|(_, widget)| {
                                let rect = widget.get_rect();
                                rect.width() > 0.0 && rect.height() > 0.0
                            })
                    })
            })
            .collect::<Vec<_>>();
        let mut layers: Vec<Composed> = layers
            .into_iter()
            .map(|node| {
                let mut anonymous = 0;
                walk.compose(node, (0.0, 0.0), &mut anonymous, "")
            })
            .collect();
        layers.extend(walk.lifted.take());
        layers.sort_by(|a, b| a.path.cmp(&b.path));
        let focus = self.app.focused_node.and_then(|focused| {
            walk.node_widgets
                .get(&focused)
                .and_then(|address| walk.innermost(*address))
                .map(|(path, _)| path.clone())
        });
        Composition {
            viewport: [width, height],
            focus,
            root,
            layers,
        }
    }
}

/// The grid that lays out `children` where they stand in a box at `rect` whose content is
/// scrolled `scrolled` down: the tracks between every edge any of them has, and each child's
/// lines in them. Children that overlap share tracks and stand over one another in the order
/// they are drawn, as a banner's buttons over its body do. What lies past the box's far edges
/// extends the grid, as a list's rows past its end do. `None` when it shows no children.
fn tiles(rect: [f64; 4], scrolled: f64, children: &mut [Composed]) -> Option<String> {
    use std::fmt::Write as _;
    let [x, y, width, height] = rect;
    // The content's corner: a child's rect is where scrolling put it.
    let y = y - scrolled;
    let shown = |child: &&mut Composed| child.rect[2] > 0.0 && child.rect[3] > 0.0;
    let placed: Vec<&mut Composed> = children.iter_mut().filter(shown).collect();
    if placed.is_empty() {
        return None;
    }
    // The edges past the box's start are the first track's: a child there reaches back from it.
    let edges = |start: f64, length: f64, of: &dyn Fn(&Composed) -> [f64; 2]| {
        let mut edges = vec![0.0, length];
        for child in &placed {
            let [from, to] = of(child);
            edges.extend([(from - start).max(0.0), (to - start).max(0.0)]);
        }
        edges.sort_by(f64::total_cmp);
        edges.dedup_by(|a, b| (*a - *b).abs() < 0.5);
        edges
    };
    let columns = edges(x, width, &|child| {
        [child.rect[0], child.rect[0] + child.rect[2]]
    });
    let rows = edges(y, height, &|child| {
        [child.rect[1], child.rect[1] + child.rect[3]]
    });
    let line = |edges: &[f64], at: f64| {
        edges
            .iter()
            .position(|edge| (edge - at).abs() < 0.5)
            .map_or(1, |index| index + 1)
    };
    if columns.len() < 2 || rows.len() < 2 {
        return None;
    }
    let tracks = |edges: &[f64]| {
        edges
            .windows(2)
            .map(|pair| format!("{}px", pair[1] - pair[0]))
            .collect::<Vec<_>>()
            .join(" ")
    };
    // A child's lines: from the edge it starts at to the one it ends at, the first track when
    // it stands wholly before the box's start.
    let lines = |edges: &[f64], from: f64, to: f64| {
        let start = line(edges, from.max(0.0));
        (start, line(edges, to.max(0.0)).max(start + 1))
    };
    for child in placed {
        let [cx, cy, cw, ch] = child.rect;
        let (left, top) = (cx - x, cy - y);
        let mut place = child.layout.take().unwrap_or_else(|| {
            if child.children.is_empty() {
                String::new()
            } else {
                "display:flex;flex-direction:column;".to_owned()
            }
        });
        let (column, row) = (
            lines(&columns, left, left + cw),
            lines(&rows, top, top + ch),
        );
        let _ = write!(
            place,
            "grid-column:{}/{};grid-row:{}/{};margin:0;",
            column.0, column.1, row.0, row.1
        );
        // What stands before the box's start (rows a list scrolled up, a part reaching past its
        // wrapper) reaches back from the first track by as much, at the size it was laid out at.
        if left < -0.5 || top < -0.5 {
            let _ = write!(
                place,
                "margin:{}px 0 0 {}px;width:{cw}px;height:{ch}px;min-width:0;min-height:0;\
                 max-width:none;max-height:none;",
                top.min(0.0),
                left.min(0.0),
            );
        }
        child.layout = Some(place);
    }
    Some(format!(
        "display:grid;grid-template-columns:{};grid-template-rows:{};gap:0;padding:0;",
        tracks(&columns),
        tracks(&rows)
    ))
}
