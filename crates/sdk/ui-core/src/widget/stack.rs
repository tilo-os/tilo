use crate::permissions::PermissionScope;
use crate::presence::{PathSegment, PresenceKey};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::{AppModel, ModelPath};
use taffy::style_helpers::percent;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use winit::event::WindowEvent;

/// Children painted on top of one another, each `position: Absolute` and
/// anchored at the stack's top-left. There is deliberately no alignment
/// option: a child that wants a corner or the centre positions itself (a
/// `Column`/`Row` with its own insets and justify, or a `Container` with
/// `with_centered_max_width`), so the stack stays a pure z-order primitive
/// and the layer's geometry is owned by the layer. If a caller needs the
/// stack itself to anchor, add an opt-in that maps to `inset` on the child
/// node rather than changing this default.
pub struct Stack {
    children: Vec<Box<dyn Widget>>,
    rect: vello::kurbo::Rect,
    presence_path: Vec<PathSegment>,
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
    pub fn with_permission_scope(mut self, scope: PermissionScope) -> Self {
        self.permission_scope = Some(scope);
        self
    }
}

impl Widget for Stack {
    crate::provides!(
        dyn crate::permissions::capability::PermissionGated,
        dyn crate::presence::capability::PublishesPresence
    );

    /// Not short-circuited — a child that skipped its pass would miss a
    /// frame of state.
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

        // Absolute so all children overlap.
        for &node in &child_nodes {
            let mut style = taffy.style(node).cloned().unwrap_or_default();
            style.position = taffy::Position::Absolute;
            let _ = taffy.set_style(node, style);
        }

        let style = Style {
            display: taffy::Display::Flex,
            size: taffy::Size {
                width: percent(1.0_f32),
                // With every child `position: Absolute`, the stack
                // contributes zero to intrinsic size, so `Dimension::auto()`
                // here made the parent ration percent-based descendant
                // heights from 0 rather than the viewport — a Column's
                // bottom-padding got silently dropped. Pinning to the
                // parent's height makes the viewport size flow through
                // to absolute children correctly.
                height: percent(1.0_f32),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &child_nodes, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);

        let children = taffy.children(node).unwrap_or_default();
        for (child_widget, child_node) in self.children.iter_mut().zip(children.iter()) {
            child_widget.apply_layout(taffy, *child_node, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        for child in &mut self.children {
            crate::widget::draw_child(
                child.as_mut(),
                scene,
                transform,
                text_renderer,
                theme,
                presence,
            );
        }

        crate::widget::draw_gate_and_peer_focus(self, scene, transform, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
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

    fn visit_retained_components(
        &mut self,
        request: &mut crate::bound_dispatch::RetainedComponentRequest<'_, '_>,
    ) -> Result<(), crate::bound_dispatch::BoundDispatchError> {
        for child in self.children_mut() {
            request.visit(child)?;
        }
        Ok(())
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

    fn name(&self) -> &'static str {
        "Stack"
    }

    // `.any` would short-circuit and skip `take_structure_dirty()` on later
    // children once one reports dirty, leaving their flags uncleared --
    // the non-short-circuiting `.fold` below is required, not incidental.
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
}

impl crate::presence::capability::PublishesPresence for Stack {
    fn presence_key(&self) -> Option<PresenceKey> {
        PresenceKey::widget(&self.presence_path)
    }
}

impl crate::permissions::capability::PermissionGated for Stack {
    fn permission_scope(&self) -> Option<PermissionScope> {
        self.permission_scope.clone()
    }
}
