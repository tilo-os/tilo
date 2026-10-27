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
        mut live: Option<&mut dyn Widget>,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Closed, the History control stands in the app's header row; open,
        // the live app is inert, so the control moves into this bar.
        let control = (self.open_button && !self.open && !self.invalid_owner)
            .then_some(crate::widget::window_header::HistoryControl::Closed);
        let offered = live
            .as_deref_mut()
            .is_some_and(|live| crate::widget::window_header::offer_history_control(live, control));
        // An app whose controls are its window bar's slots keeps History
        // there too: whoever shows the slots (the desktop's bar, the
        // runner's row) shows it, and this bar never stands above the app.
        let presents =
            offered || (control.is_some() && crate::window_toolbar::declared().is_some());
        // Closed, the sync status reads as quiet text in the same row; open,
        // it stays with this bar.
        let status = (!self.open).then_some(self.sync_status.as_ref()).flatten();
        let presents_status = live
            .as_deref_mut()
            .is_some_and(|live| crate::widget::window_header::offer_sync_status(live, status));
        if presents != self.app_presents_button || presents_status != self.app_presents_status {
            self.app_presents_button = presents;
            self.app_presents_status = presents_status;
            self.rebuild_toolbar();
        }
        // What this build lays out, including the toolbar the offer above
        // just rebuilt: admitted before it, the first build of every app
        // with History read its own toolbar as a change after it and built
        // the whole tree a second time.
        self.admit_composition();
        self.timeline_node = None;
        self.canvas_node = None;
        self.live_node = None;
        self.unavailable_node = None;
        self.panel_node = None;
        self.content_node = None;
        self.footer_node = None;
        self.banner_node = None;
        if self.side_panel() {
            return self.build_side_panel(live, tree, theme, nodes);
        }
        let toolbar = self.toolbar.build_layout(tree, theme, nodes);
        // A selected change lays out its own navigation rows edge to edge;
        // the strip holding the History control is inset like a bar.
        if self.has_toolbar_controls()
            && self.selected.is_empty()
            && let Ok(style) = tree.style(toolbar)
        {
            let mut style = style.clone();
            style.padding = crate::widget::toolbar::band_padding(theme);
            let _ = tree.set_style(toolbar, style);
        }
        self.toolbar_node = Some(toolbar);
        let mut body_children = vec![];
        // The timeline's gutter: the whole width when narrow, a column beside
        // the preview otherwise. Why there is no timeline stands in it too.
        let gutter = |tree: &mut TaffyTree<()>, node: NodeId, width: f64| {
            if let Ok(style) = tree.style(node) {
                let mut style = style.clone();
                style.size.width = if width < 600.0 {
                    Dimension::percent(1.0)
                } else {
                    Dimension::length(320.0)
                };
                style.min_size.width = Dimension::length(0.0);
                style.flex_shrink = 0.0;
                let _ = tree.set_style(node, style);
            }
        };
        if self.show_timeline() && !self.timeline_available() {
            let message = self.unavailable.build_layout(tree, theme, nodes);
            gutter(tree, message, self.width);
            self.unavailable_node = Some(message);
            body_children.push(message);
        } else if self.show_timeline() {
            let timeline = self.timeline.build_layout(tree, theme, nodes);
            gutter(tree, timeline, self.width);
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
                min_size: Size::from_lengths(0.0, 0.0),
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::auto(),
                },
                ..Default::default()
            },
            &body_children,
        );
        self.body_node = Some(body);
        self.compose_root(vec![toolbar, body], tree, theme, nodes)
    }

    /// The composition's root: `children`, and the restore review over them.
    fn compose_root(
        &mut self,
        mut composition_children: Vec<NodeId>,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
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
                size: Size::from_percent(1.0, 1.0),
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

    /// History as a panel at the window's trailing side: the content stays
    /// where it is (the live app, or the selected version in its place,
    /// read-only), and the panel beside it holds the timeline, its controls
    /// and, for a selected change, what can be done with it.
    fn build_side_panel(
        &mut self,
        live: Option<&mut dyn Widget>,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let inset = f32::from(theme.tokens.spacing.s4);
        // The content: the preview under its banner, or the live app.
        let mut content_children = vec![];
        if self.show_preview() {
            let banner = self.banner.build_layout(tree, theme, nodes);
            self.banner_node = Some(banner);
            content_children.push(banner);
            let canvas = self.canvas_mut().build_layout(tree, theme, nodes);
            if let Ok(style) = tree.style(canvas) {
                let mut style = style.clone();
                style.flex_grow = 1.0;
                style.min_size = Size::from_lengths(0.0, 0.0);
                let _ = tree.set_style(canvas, style);
            }
            self.canvas_node = Some(canvas);
            content_children.push(canvas);
        } else if self.live_visible()
            && let Some(live) = live
        {
            let node = live.build_layout(tree, theme, nodes);
            self.live_node = Some(node);
            content_children.push(node);
        }
        let content = tree.parent_node(
            Style {
                flex_direction: FlexDirection::Column,
                flex_grow: 1.0,
                flex_basis: Dimension::length(0.0),
                min_size: Size::from_lengths(0.0, 0.0),
                size: Size {
                    width: Dimension::auto(),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            },
            &content_children,
        );
        self.content_node = Some(content);
        // The panel: its head and controls, the timeline, the selected
        // change's foot.
        let toolbar = self.toolbar.build_layout(tree, theme, nodes);
        if let Ok(style) = tree.style(toolbar) {
            let mut style = style.clone();
            style.padding = taffy::Rect {
                left: taffy::LengthPercentage::length(inset),
                right: taffy::LengthPercentage::length(f32::from(theme.tokens.spacing.s2)),
                top: taffy::LengthPercentage::length(f32::from(theme.tokens.spacing.s2)),
                bottom: taffy::LengthPercentage::length(f32::from(theme.tokens.spacing.s2)),
            };
            style.flex_shrink = 0.0;
            let _ = tree.set_style(toolbar, style);
        }
        self.toolbar_node = Some(toolbar);
        let mut panel_children = vec![toolbar];
        // The timeline stands on the panel's own ground.
        self.timeline.set_draws_own_surface(false);
        let list = if self.timeline_available() {
            let timeline = self.timeline.build_layout(tree, theme, nodes);
            self.timeline_node = Some(timeline);
            timeline
        } else {
            let message = self.unavailable.build_layout(tree, theme, nodes);
            self.unavailable_node = Some(message);
            message
        };
        if let Ok(style) = tree.style(list) {
            let mut style = style.clone();
            style.flex_grow = 1.0;
            style.flex_basis = Dimension::length(0.0);
            style.size.width = Dimension::percent(1.0);
            style.min_size = Size::from_lengths(0.0, 0.0);
            let _ = tree.set_style(list, style);
        }
        panel_children.push(list);
        if self.show_preview() {
            let footer = self.footer.build_layout(tree, theme, nodes);
            if let Ok(style) = tree.style(footer) {
                let mut style = style.clone();
                style.padding = taffy::Rect::length(inset);
                style.flex_shrink = 0.0;
                let _ = tree.set_style(footer, style);
            }
            self.footer_node = Some(footer);
            panel_children.push(footer);
        }
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the panel's width is a small logical-pixel constant"
        )]
        let panel_width = super::SIDE_PANEL_WIDTH as f32;
        let panel = tree.parent_node(
            Style {
                flex_direction: FlexDirection::Column,
                flex_shrink: 0.0,
                size: Size {
                    width: Dimension::length(panel_width),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            },
            &panel_children,
        );
        self.panel_node = Some(panel);
        let body = tree.parent_node(
            Style {
                flex_direction: FlexDirection::Row,
                flex_grow: 1.0,
                min_size: Size::from_lengths(0.0, 0.0),
                size: Size::from_percent(1.0, 1.0),
                ..Default::default()
            },
            &[content, panel],
        );
        self.body_node = Some(body);
        self.compose_root(vec![body], tree, theme, nodes)
    }

    /// The origin of `node`, a child of the composition's body, for the
    /// widgets inside it.
    fn body_child_origin(&self, tree: &TaffyTree<()>, node: Option<NodeId>) -> Point {
        let body = self.body_origin(tree);
        node.map_or(body, |node| {
            crate::widget::layout_rect(tree, node, body).origin()
        })
    }

    /// Where the live app is laid out: the content beside the side panel, or
    /// the body.
    fn live_origin(&self, tree: &TaffyTree<()>) -> Point {
        if self.content_node.is_some() {
            self.body_child_origin(tree, self.content_node)
        } else {
            self.body_origin(tree)
        }
    }
    /// A closed History with its button in the host's chrome leaves the
    /// toolbar empty; it then takes no bar at all.
    fn has_toolbar_controls(&self) -> bool {
        !self.toolbar.children().is_empty()
    }
    fn body_origin(&self, tree: &TaffyTree<()>) -> Point {
        let origin = self.rect.origin();
        self.body_node.map_or(origin, |node| {
            crate::widget::layout_rect(tree, node, origin).origin()
        })
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
            live.apply_layout(tree, node, self.live_origin(tree));
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
        // Escape leaves History, as the History control does, unless the
        // restore review (a modal of its own) is up.
        if self.open
            && self.review.is_none()
            && matches!(
                event,
                WindowEvent::KeyboardInput { event: key, .. }
                    if key.state == winit::event::ElementState::Pressed
                        && key.logical_key
                            == winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
            )
        {
            self.request_close();
            *ctx.handled = true;
            return;
        }
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
    fn sync(&mut self, _: &AppModel, _: &ModelPath) {}
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.build_presentation(None, tree, theme, nodes)
    }
    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(tree, node, origin);
        let origin = self.rect.origin();
        if let (Some(panel), Some(node)) = (&mut self.review, self.review_node) {
            panel.dialog.apply_layout(tree, node, origin);
        }
        // Beside the side panel, its parts are laid out in it and the preview
        // in the content; otherwise the toolbar heads the composition and the
        // rest stands in its body.
        let side = self.panel_node.is_some();
        let (bar, list, preview) = if side {
            let panel = self.body_child_origin(tree, self.panel_node);
            self.panel_rect = self.panel_node.map_or(Rect::ZERO, |node| {
                crate::widget::layout_rect(tree, node, self.body_origin(tree))
            });
            (
                panel,
                panel,
                self.body_child_origin(tree, self.content_node),
            )
        } else {
            self.panel_rect = Rect::ZERO;
            let body = self.body_origin(tree);
            (origin, body, body)
        };
        if let Some(toolbar) = self.toolbar_node {
            self.toolbar.apply_layout(tree, toolbar, bar);
        }
        if let Some(timeline) = self.timeline_node {
            self.timeline.apply_layout(tree, timeline, list);
        }
        if let Some(message) = self.unavailable_node {
            self.unavailable.apply_layout(tree, message, list);
        }
        if let Some(footer) = self.footer_node {
            self.footer.apply_layout(tree, footer, list);
        }
        if let Some(banner) = self.banner_node {
            self.banner.apply_layout(tree, banner, preview);
        }
        if let Some(canvas) = self.canvas_node {
            self.canvas_mut().apply_layout(tree, canvas, preview);
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
        // Authors are named by the room's participants; a name that arrived
        // or changed shows on the next refresh.
        if self
            .controller
            .as_mut()
            .is_some_and(|controller| controller.follow_roster(presence))
        {
            self.toolbar_dirty = true;
        }
        self.motion = theme.motion();
        // The content first: the selected version in place under its banner,
        // drawn as the live view is in this window.
        if self.show_preview() {
            let window_presents = self.window_presents;
            let canvas = self.canvas_mut();
            if let Some(preview) = canvas.preview.as_mut() {
                preview.set_window_presents_search(window_presents);
            }
            canvas.draw(scene, transform, text, theme, presence);
        }
        if self.banner_node.is_some() {
            let band = self.banner.get_rect();
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.inspector,
                None,
                &band,
            );
            self.banner.draw(scene, transform, text, theme, presence);
        }
        let side = self.panel_node.is_some();
        // However History came to be open, its panel slides in the first
        // frame it shows; once History closes, the next opening slides again.
        if side && !self.panel_motion.is_open() {
            self.panel_motion.show(
                true,
                crate::motion::PanelSlide::SPEC,
                self.motion,
                crate::motion::input_source(),
                crate::motion::frame_now(),
            );
            // The panel is drawn outside the tree a frame pump polls: the
            // motion clock keeps frames coming until it has slid in.
            if let Some(until) = self.panel_motion.settles_at() {
                crate::motion::want_frames_until(until);
            }
        } else if !self.open && self.panel_motion.is_open() {
            self.panel_motion.snap(false);
        }
        // Then the panel, where its slide has it, clipped to its own place.
        let slide = if side {
            let progress = self.panel_motion.progress(crate::motion::frame_now());
            crate::motion::PanelSlide::shown(progress, self.panel_rect.width())
        } else {
            self.panel_rect.width()
        };
        let sliding = side && slide < self.panel_rect.width();
        let bar_transform = if side {
            transform * Affine::translate((self.panel_rect.width() - slide, 0.0))
        } else {
            transform
        };
        if sliding {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                transform,
                &self.panel_rect,
            );
        }
        if side {
            // The panel is a region of the window: one step recessed from
            // the content beside it, which is all that parts the two.
            scene.fill(
                vello::peniko::Fill::NonZero,
                bar_transform,
                theme.tokens.colors.surfaces.inspector,
                None,
                &self.panel_rect,
            );
        } else if self.has_toolbar_controls() {
            crate::standalone_history_host::bar_edge(
                scene,
                transform,
                theme,
                self.toolbar.get_rect(),
            );
        }
        self.toolbar
            .draw(scene, bar_transform, text, theme, presence);
        if side && let Some(head) = self.toolbar.children().first() {
            // The panel's name, at the head's leading edge, on its centre line.
            let style = theme.text(crate::tokens::TextRole::Title);
            let title = crate::i18n::I18nManager::lookup_plain("history-panel-title");
            let row = head.get_rect();
            let line = f64::from(text.measure_style(&title, None, &style).1);
            text.draw_style(
                scene,
                &title,
                theme.tokens.colors.text_primary,
                bar_transform * Affine::translate((row.x0, row.center().y - line / 2.0)),
                None,
                &style,
            );
        }
        if self.bar_status.text().is_some()
            && let Some(row) = self.toolbar.children().first()
            // The status ends before the row's first control: its Retry,
            // when it has one, or the History control.
            && let Some(control) = row.children().first()
        {
            let row = row.get_rect();
            self.bar_status.paint(
                scene,
                bar_transform,
                text,
                theme,
                row,
                row.x0,
                control.get_rect().x0 - f64::from(theme.tokens.spacing.s3),
            );
        }
        if self.show_timeline() && !self.timeline_available() {
            self.unavailable
                .draw(scene, bar_transform, text, theme, presence);
        } else if self.show_timeline() {
            self.timeline
                .draw(scene, bar_transform, text, theme, presence);
        }
        if self.footer_node.is_some() {
            let foot = self.footer.get_rect();
            crate::surface::separator(
                scene,
                bar_transform,
                foot.x0,
                foot.x1,
                foot.y0,
                0.0,
                theme.tokens.colors.surfaces.inspector,
            );
            self.footer
                .draw(scene, bar_transform, text, theme, presence);
        }
        if sliding {
            scene.pop_layer();
        }
        // The restore review is a modal: the host's top layer paints it
        // (`draw_overlay` reaches it through `children_mut`).
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
        if let Some(footer) = self.footer_node {
            self.footer
                .handle_event(event, &mut ctx.reborrow(ctx.path, footer));
        }
        if let Some(banner) = self.banner_node {
            self.banner
                .handle_event(event, &mut ctx.reborrow(ctx.path, banner));
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
        if self.bar_status.text().is_some() {
            children.push(&self.bar_status);
        }
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
        if self.footer_node.is_some() {
            children.push(&self.footer);
        }
        if self.banner_node.is_some() {
            children.push(&self.banner);
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
        if self.bar_status.text().is_some() {
            children.push(&mut self.bar_status);
        }
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
        if self.footer_node.is_some() {
            children.push(&mut self.footer);
        }
        if self.banner_node.is_some() {
            children.push(&mut self.banner);
        }
        children
    }
    /// Everything a composition can lay out, also under an open review and
    /// whichever side the preview is not showing. The live app is its
    /// owner's child, not this one's.
    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        visitor(&self.toolbar);
        visitor(&self.bar_status);
        visitor(&self.timeline);
        visitor(&self.unavailable);
        visitor(&self.before);
        visitor(&self.after);
        visitor(&self.footer);
        visitor(&self.banner);
        if let Some(panel) = &self.review {
            visitor(&panel.dialog);
        }
        true
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn needs_animation_frame(&self) -> bool {
        self.panel_motion.is_moving(crate::motion::frame_now())
            || self
                .children()
                .into_iter()
                .any(Widget::needs_animation_frame)
    }
    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = self.composition_changed();
        for child in self.children_mut() {
            dirty |= child.take_structure_dirty();
        }
        dirty
    }
}
