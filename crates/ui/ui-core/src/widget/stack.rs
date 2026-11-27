use taffy::style_helpers::percent;
use taffy::{NodeId, Style, TaffyTree};
#[cfg(feature = "model")]
use tilo_ui_base::permissions::PermissionScope;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::TaffyTreeExt;
use tilo_ui_base::widget::Widget;
use tilo_ui_base::{presence::PathSegment, presence::PresenceKey};
use tilo_ui_base::{widget::AppModel, widget::ModelPath};
use vello::kurbo::Point;
use winit::event::WindowEvent;

/// Children painted on top of one another, each `position: Absolute` and anchored at the
/// stack's top-left.
#[tilo_ui_base::component::not_a_component(layout, "the language's `stack`")]
pub struct Stack {
    children: Vec<Box<dyn Widget>>,
    rect: vello::kurbo::Rect,
    presence_path: Vec<PathSegment>,
    #[cfg(feature = "model")]
    permission_scope: Option<PermissionScope>,
}

impl Default for Stack {
    fn default() -> Self {
        Self::new()
    }
}

impl Stack {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
            rect: vello::kurbo::Rect::ZERO,
            presence_path: Vec::new(),
            #[cfg(feature = "model")]
            permission_scope: None,
        }
    }

    #[must_use]
    pub fn push(mut self, child: Box<dyn Widget>) -> Self {
        self.children.push(child);
        self
    }

    #[must_use]
    pub fn with_presence(mut self, path: Vec<PathSegment>) -> Self {
        self.presence_path = path;
        self
    }

    #[must_use]
    #[cfg(feature = "model")]
    pub fn with_permission_scope(mut self, scope: PermissionScope) -> Self {
        self.permission_scope = Some(scope);
        self
    }
}

impl Widget for Stack {
    tilo_ui_base::provides!(
        #[cfg(feature = "model")]
        dyn tilo_ui_base::widget::permission::PermissionGuarded,
        dyn tilo_ui_base::presence::capability::PublishesPresence
    );

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let mut changed = false;
        for child in &mut self.children {
            changed |= child.update(state, path);
        }
        changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child_nodes: Vec<NodeId> = self
            .children
            .iter_mut()
            .map(|c| c.build_layout(taffy, theme, node_map))
            .collect();

        for &node in &child_nodes {
            let mut style = taffy.style(node).cloned().unwrap_or_default();
            style.position = taffy::Position::Absolute;
            let _ = taffy.set_style(node, style);
        }

        let style = Style {
            display: taffy::Display::Flex,
            size: taffy::Size {
                width: percent(1.0_f32),
                // With every child `position: Absolute`, the stack contributes zero to
                // intrinsic size, so `Dimension::auto()` here made the parent ration
                // percent-based descendant heights from 0 rather than the viewport — a Column's
                // bottom-padding got silently dropped.
                height: percent(1.0_f32),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &child_nodes, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);

        let children = taffy.children(node).unwrap_or_default();
        for (child_widget, child_node) in self.children.iter_mut().zip(children.iter()) {
            child_widget.apply_layout(taffy, *child_node, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        for child in &mut self.children {
            tilo_ui_base::widget::draw_child(
                child.as_mut(),
                scene,
                transform,
                text_renderer,
                theme,
                &env,
            );
        }

        tilo_ui_base::widget::draw_over(
            self,
            &mut tilo_ui_base::draw::DrawCtx::new(
                scene,
                transform,
                text_renderer,
                theme,
                env.clone(),
            ),
        );
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        let child_nodes = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        // Reverse order: top-most first.
        for (child_widget, &child_node) in
            self.children.iter_mut().rev().zip(child_nodes.iter().rev())
        {
            child_widget.handle_event(event, &mut ctx.reborrow(ctx.path, child_node));
            if *ctx.handled {
                break;
            }
        }
    }

    /// What its children changed.
    fn collect_damage(&self, regions: &mut Vec<vello::kurbo::Rect>) {
        if self.guarded() || !self.presence_path.is_empty() {
            if self.needs_redraw() || self.needs_animation_frame() {
                regions.push(self.rect);
            }
        } else {
            for child in &self.children {
                child.collect_damage(regions);
            }
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.children
            .iter()
            .map(std::convert::AsRef::as_ref)
            .collect()
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for child in &self.children {
            visitor(child.as_ref());
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.children
            .iter_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .collect()
    }

    fn get_rect(&self) -> vello::kurbo::Rect {
        self.rect
    }

    /// A child that offers the scroll around this stack detents (a window's edge) offers them
    /// from the stack's own leading edge, so the view settles a whole window at a time wherever
    /// the stack is panned to.
    fn snap_points(&self, axis: tilo_ui_base::widget::ScrollAxis, out: &mut Vec<f32>) {
        let along = |rect: vello::kurbo::Rect| match axis {
            tilo_ui_base::widget::ScrollAxis::Vertical => rect.y0,
            tilo_ui_base::widget::ScrollAxis::Horizontal
            | tilo_ui_base::widget::ScrollAxis::Both => rect.x0,
        };
        let lead = along(self.rect);
        for child in &self.children {
            let start = out.len();
            child.snap_points(axis, out);
            let at = tilo_ui_base::num::f32_of_f64(along(child.get_rect()) - lead);
            for point in &mut out[start..] {
                *point += at;
            }
        }
    }

    fn name(&self) -> &'static str {
        "Stack"
    }

    #[allow(
        clippy::unnecessary_fold,
        reason = "the fold visits every child to clear every dirty flag; any would stop after the first dirty child"
    )]
    fn take_structure_dirty(&mut self) -> bool {
        self.children
            .iter_mut()
            .map(|c| c.take_structure_dirty())
            .fold(false, |acc, d| acc || d)
    }

    fn take_structure_change(&mut self) -> tilo_ui_base::widget::StructureChange {
        tilo_ui_base::widget::StructureChange::of_children(self.children_mut())
    }
}

impl tilo_ui_base::presence::capability::PublishesPresence for Stack {
    fn presence_key(&self) -> Option<PresenceKey> {
        PresenceKey::widget(&self.presence_path)
    }
}

impl Stack {
    /// Whether a permission scope guards this stack, so its denial scrim covers all of it.
    #[cfg(feature = "model")]
    fn guarded(&self) -> bool {
        self.permission_scope.is_some()
    }

    /// Without Tilo's model no permission scope guards a widget.
    #[cfg(not(feature = "model"))]
    #[allow(
        clippy::unused_self,
        reason = "the model build reads the stack's scope"
    )]
    const fn guarded(&self) -> bool {
        false
    }
}

#[cfg(feature = "model")]
impl tilo_ui_base::widget::permission::PermissionGuarded for Stack {
    fn permission_scope(&self) -> Option<PermissionScope> {
        self.permission_scope.clone()
    }
}
