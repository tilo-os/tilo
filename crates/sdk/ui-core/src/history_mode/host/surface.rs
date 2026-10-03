//! Layout and input routing for chrome plus a borrowed presentation slot.
//! Live ownership/lifecycle traversal stays entirely with the runner.
use super::{
    Affine, Dimension, FlexDirection, HashMap, HistoryHost, NodeId, Point, PresenceCtx,
    PreviewSide, Rect, Scene, Size, Style, TaffyTree, TaffyTreeExt, TextRenderer, Theme, Widget,
    WindowEvent,
};
use crate::widget::{AppModel, ModelPath};

impl HistoryHost {
    pub(crate) fn build_composition(
        &mut self,
        live: &mut dyn Widget,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.build_presentation(Some(live), tree, theme, nodes)
    }
    fn build_presentation(
        &mut self,
        live: Option<&mut dyn Widget>,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.admit_composition();
        let toolbar = self.toolbar.build_layout(tree, theme, nodes);
        // Open History lays out its own navigation rows edge to edge; only
        // the closed strip is inset like a bar.
        if self.has_toolbar_controls()
            && !self.open
            && let Ok(style) = tree.style(toolbar)
        {
            let mut style = style.clone();
            style.padding = crate::chrome::bar_padding(theme);
            let _ = tree.set_style(toolbar, style);
        }
        self.toolbar_node = Some(toolbar);
        self.timeline_node = None;
        self.canvas_node = None;
        self.live_node = None;
        self.unavailable_node = None;
        let mut body_children = vec![];
        if self.show_timeline() && !self.timeline_available() {
            let message = self.unavailable.build_layout(tree, theme, nodes);
            self.unavailable_node = Some(message);
            body_children.push(message);
        } else if self.show_timeline() {
            let timeline = self.timeline.build_layout(tree, theme, nodes);
            if let Ok(style) = tree.style(timeline) {
                let mut style = style.clone();
                style.size.width = if self.width < 600.0 {
                    Dimension::percent(1.0)
                } else {
                    Dimension::length(320.0)
                };
                style.min_size.width = Dimension::length(0.0);
                style.flex_shrink = 0.0;
                let _ = tree.set_style(timeline, style);
            }
            self.timeline_node = Some(timeline);
            body_children.push(timeline);
        }
        if self.show_preview() {
            let canvas = self.canvas_mut().build_layout(tree, theme, nodes);
            self.canvas_node = Some(canvas);
            body_children.push(canvas);
        } else if self.live_visible()
            && let Some(live) = live
        {
            let node = live.build_layout(tree, theme, nodes);
            self.live_node = Some(node);
            body_children.push(node);
        }
        let body = tree.parent_node(
            Style {
                flex_direction: FlexDirection::Row,
                flex_grow: 1.0,
                min_size: Size {
                    width: Dimension::length(0.0),
                    height: Dimension::length(0.0),
                },
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::auto(),
                },
                ..Default::default()
            },
            &body_children,
        );
        self.body_node = Some(body);
        let mut composition_children = vec![toolbar, body];
        self.review_node = None;
        if let Some(panel) = &mut self.review {
            let node = panel.dialog.build_layout(tree, theme, nodes);
            if let Ok(style) = tree.style(node) {
                let mut style = style.clone();
                style.position = taffy::Position::Absolute;
                let _ = tree.set_style(node, style);
            }
            self.review_node = Some(node);
            composition_children.push(node);
        }
        let root = tree.parent_node(
            Style {
                flex_direction: FlexDirection::Column,
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            },
            &composition_children,
        );
        self.chrome_node = Some(root);
        // This is a structural composition node, not an actionable widget.
        // Registering `self` could alias the thin data pointer of its first
        // inline child and make reverse-node lookup resolve to the wrong node.
        root
    }
    /// A closed History with its button in the host's chrome leaves the
    /// toolbar empty; it then takes no bar at all.
    fn has_toolbar_controls(&self) -> bool {
        !self.toolbar.children().is_empty()
    }
    fn body_origin(&self, tree: &TaffyTree<()>) -> Point {
        let layout = self
            .body_node
            .and_then(|node| tree.layout(node).ok())
            .copied()
            .unwrap_or_default();
        Point::new(
            self.rect.x0 + f64::from(layout.location.x),
            self.rect.y0 + f64::from(layout.location.y),
        )
    }
    pub(crate) fn apply_composition(
        &mut self,
        live: &mut dyn Widget,
        tree: &TaffyTree<()>,
        root: NodeId,
        origin: Point,
    ) {
        self.apply_layout(tree, root, origin);
        if let Some(node) = self.live_node {
            live.apply_layout(tree, node, self.body_origin(tree));
        }
    }
    pub(crate) fn live_layout_node(&self) -> Option<NodeId> {
        self.live_node
    }
    /// Route `event` to the History chrome, then (when the chrome did not
    /// take it) to the `live` app. Each is dispatched at its own layout node;
    /// `ctx.node_id` is the node the event arrived at and is not forwarded.
    pub(crate) fn route_event(
        &mut self,
        live: &mut dyn Widget,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        let scale = ctx.scale_factor;
        let chrome_event = self.controls_event(event, scale);
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Point::new(position.x / scale, position.y / scale);
        }
        if let Some(root) = self.chrome_node {
            self.handle_event(event, &mut ctx.reborrow(ctx.path, root));
        }
        if chrome_event && !matches!(event, WindowEvent::KeyboardInput { .. }) {
            *ctx.handled = true;
        }
        // Toolbar focus makes `controls_event` true for every key. Letters
        // still belong on the live Terminal; leftover Space/Enter must too
        // or App Activate steals them (A01).
        let leftover_typing = matches!(
            event,
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
        ) && !self.captures_input();
        if (!chrome_event || leftover_typing)
            && !*ctx.handled
            && let Some(child) = self.live_node
        {
            live.handle_event(event, &mut ctx.reborrow(ctx.path, child));
        }
    }
}
impl Widget for HistoryHost {
    fn inactive_callback_boundary(&self) -> Option<NodeId> {
        self.live_node.filter(|_| !self.live_interactive())
    }
    fn name(&self) -> &'static str {
        "History"
    }
    fn sync(&mut self, _: &mut AppModel, _: &ModelPath) {}
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.build_presentation(None, tree, theme, nodes)
    }
    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = tree.layout(node).copied().unwrap_or_default();
        let origin = Point::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
        );
        self.rect = Rect::from_origin_size(
            origin,
            (f64::from(layout.size.width), f64::from(layout.size.height)),
        );
        if let Some(toolbar) = self.toolbar_node {
            self.toolbar.apply_layout(tree, toolbar, origin);
        }
        if let (Some(panel), Some(node)) = (&mut self.review, self.review_node) {
            panel.dialog.apply_layout(tree, node, origin);
        }
        let body = self.body_origin(tree);
        if let Some(timeline) = self.timeline_node {
            self.timeline.apply_layout(tree, timeline, body);
        }
        if let Some(message) = self.unavailable_node {
            self.unavailable.apply_layout(tree, message, body);
        }
        if let Some(canvas) = self.canvas_node {
            self.canvas_mut().apply_layout(tree, canvas, body);
        }
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.has_toolbar_controls() {
            crate::chrome::paint_bar(scene, transform, theme, self.toolbar.get_rect());
        }
        self.toolbar.draw(scene, transform, text, theme, presence);
        if self.show_timeline() && !self.timeline_available() {
            self.unavailable
                .draw(scene, transform, text, theme, presence);
        } else if self.show_timeline() {
            self.timeline.draw(scene, transform, text, theme, presence);
        }
        if self.show_preview() {
            self.canvas_mut()
                .draw(scene, transform, text, theme, presence);
        }
        if let Some(panel) = &mut self.review {
            panel.dialog.draw(scene, transform, text, theme, presence);
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if let (Some(panel), Some(node)) = (&mut self.review, self.review_node) {
            panel
                .dialog
                .handle_event(event, &mut ctx.reborrow(ctx.path, node));
            *ctx.handled = true;
            return;
        }
        if let Some(toolbar) = self.toolbar_node {
            self.toolbar
                .handle_event(event, &mut ctx.reborrow(ctx.path, toolbar));
        }
        if let Some(timeline) = self.timeline_node
            && (!matches!(
                event,
                WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
            ) || self.timeline.is_focused())
        {
            self.timeline
                .handle_event(event, &mut ctx.reborrow(ctx.path, timeline));
        }
        if let Some(canvas) = self.canvas_node {
            self.canvas_mut()
                .handle_event(event, &mut ctx.reborrow(ctx.path, canvas));
        }
    }
    fn children(&self) -> Vec<&dyn Widget> {
        if let Some(panel) = &self.review {
            return vec![&panel.dialog];
        }
        let mut children: Vec<&dyn Widget> = vec![&self.toolbar];
        if self.show_timeline() {
            if self.timeline_available() {
                children.push(&self.timeline);
            } else {
                children.push(&self.unavailable);
            }
        }
        if self.show_preview() {
            children.push(self.canvas());
        }
        children
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let show_timeline = self.show_timeline();
        let timeline_available = self.timeline_available();
        let show_preview = self.show_preview();
        let side = self.side;
        if let Some(panel) = &mut self.review {
            return vec![&mut panel.dialog];
        }
        let mut children: Vec<&mut dyn Widget> = vec![&mut self.toolbar];
        if show_timeline {
            if timeline_available {
                children.push(&mut self.timeline);
            } else {
                children.push(&mut self.unavailable);
            }
        }
        if show_preview {
            children.push(if side == PreviewSide::Before {
                &mut self.before
            } else {
                &mut self.after
            });
        }
        children
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = self.composition_changed();
        for child in self.children_mut() {
            dirty |= child.take_structure_dirty();
        }
        dirty
    }
}
