//! The content column: the open snippet's language, folder and tags, what it
//! is for, its actions, and its body in the code editor.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use ui::a11y::{AccessNode, AccessState};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{
    Button, ButtonFace, ButtonRole, ChipFace, ChipLead, ControlSize, EmptyState, EmptyStateContent,
    Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::app::{Action, Core, Shared};
use crate::model::Selection;

/// What the content column painted last frame, for hit-testing.
#[derive(Default)]
pub(crate) struct DetailState {
    pub(crate) rect: Rect,
    pub(crate) hits: Vec<(Rect, Action)>,
}

/// One control of the content column, before it is placed.
enum Control {
    Chip {
        label: String,
        lead: ChipLead,
        action: Action,
    },
    Button {
        label: String,
        icon: Option<Icon>,
        role: ButtonRole,
        action: Action,
    },
    IconButton {
        icon: Icon,
        label: &'static str,
        action: Action,
    },
}

impl Control {
    fn width(&self, text: &mut TextRenderer, theme: &Theme) -> f64 {
        match self {
            Self::Chip { label, lead, .. } => {
                ChipFace::intrinsic_width(label, *lead, false, text, theme)
            }
            Self::Button {
                label, icon, role, ..
            } => Self::button_face(Rect::ZERO, label, *icon, *role).intrinsic_width(text, theme),
            Self::IconButton { icon, label, .. } => {
                ButtonFace::icon_only(Rect::ZERO, *icon, label).intrinsic_width(text, theme)
            }
        }
    }

    fn height(&self, theme: &Theme) -> f64 {
        match self {
            Self::Chip { .. } => ChipFace::height(theme),
            Self::Button { .. } | Self::IconButton { .. } => {
                ButtonFace::height(ControlSize::Regular, theme)
            }
        }
    }

    fn button_face(
        rect: Rect,
        label: &str,
        icon: Option<Icon>,
        role: ButtonRole,
    ) -> ButtonFace<'_> {
        let face = ButtonFace::new(rect, label).role(role);
        match icon {
            Some(icon) => face.icon(icon),
            None => face,
        }
    }

    fn action(&self) -> &Action {
        match self {
            Self::Chip { action, .. }
            | Self::Button { action, .. }
            | Self::IconButton { action, .. } => action,
        }
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect, hovered: bool) {
        match self {
            Self::Chip { label, lead, .. } => ChipFace::new(rect, label)
                .lead(*lead)
                .hovered(hovered)
                .paint(ctx.scene, ctx.transform, ctx.text, ctx.theme),
            Self::Button {
                label, icon, role, ..
            } => Button::paint(
                ctx.scene,
                ctx.transform,
                ctx.text,
                ctx.theme,
                &Self::button_face(rect, label, *icon, *role).hovered(hovered),
            ),
            Self::IconButton { icon, label, .. } => Button::paint(
                ctx.scene,
                ctx.transform,
                ctx.text,
                ctx.theme,
                &ButtonFace::icon_only(rect, *icon, label).hovered(hovered),
            ),
        }
    }
}

impl Core {
    /// Lay `controls` out in rows from `top`, wrapping at the column's
    /// edge, paint them and record their hit rects. Returns the bottom of
    /// the last row.
    fn paint_control_rows(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        span: (f64, f64),
        top: f64,
        controls: &[Control],
    ) -> f64 {
        let theme = ctx.theme;
        let gap = f64::from(theme.tokens.spacing.rhythm.related);
        let (x0, x1) = span;
        let mut x = x0;
        let mut y = top;
        let mut row_h: f64 = 0.0;
        for control in controls {
            let width = control.width(ctx.text, theme).min(x1 - x0);
            let height = control.height(theme);
            if x > x0 && x + width > x1 {
                x = x0;
                y += row_h + gap;
                row_h = 0.0;
            }
            row_h = row_h.max(height);
            let rect = Rect::new(x, y, x + width, y + height);
            let hovered = rect.contains(self.pointer);
            control.paint(ctx, rect, hovered);
            self.detail.hits.push((rect, control.action().clone()));
            x += width + gap;
        }
        y + row_h
    }

    /// The snippet's language, folder and tags as chips, then what it is
    /// for. Returns the bottom of what it painted.
    fn paint_detail_facts(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        span: (f64, f64),
        top: f64,
        snippet: &crate::model::Snippet,
    ) -> f64 {
        let id = snippet.id;
        let mut facts = vec![Control::Chip {
            label: snippet.language.label().to_string(),
            lead: ChipLead::Glyph(Icon::Code),
            action: Action::PickLanguage(id),
        }];
        if !snippet.folder.is_empty() {
            facts.push(Control::Chip {
                label: snippet.folder.clone(),
                lead: ChipLead::Glyph(Icon::Folder),
                action: Action::Select(Selection::Folder(snippet.folder.clone())),
            });
        }
        for tag in &snippet.tags {
            facts.push(Control::Chip {
                label: tag.clone(),
                lead: ChipLead::None,
                action: Action::Select(Selection::Tag(tag.clone())),
            });
        }
        let mut y = self.paint_control_rows(ctx, span, top, &facts);
        if !snippet.description.trim().is_empty() {
            y += f64::from(ctx.theme.tokens.spacing.rhythm.sibling);
            let width = (span.1 - span.0) as f32;
            let size = ctx.measure_role(&snippet.description, TextRole::Body, Some(width));
            ctx.text_role(
                &snippet.description,
                TextRole::Body,
                ctx.theme.text_dim,
                Point::new(span.0, y),
                Some(width),
            );
            y += size.height;
        }
        y
    }

    pub(crate) fn paint_detail(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) {
        self.detail.hits.clear();
        self.detail.rect = area;
        if area.width() <= 0.0 || area.height() <= 0.0 {
            return;
        }
        let Some(snippet) = self.open.and_then(|id| self.library.get(id)).cloned() else {
            if !self.library.snippets.is_empty() {
                EmptyState::paint(
                    &mut PaintCtx::new(scene, t, text, theme),
                    area,
                    &EmptyStateContent::new("No snippet selected"),
                    self.pointer,
                );
            }
            return;
        };
        let spacing = &theme.tokens.spacing;
        let gutter = f64::from(spacing.rhythm.group);
        let span = (area.x0 + gutter, (area.x1 - gutter).max(area.x0 + gutter));
        let id = snippet.id;
        let mut ctx = PaintCtx::new(scene, t, text, theme);
        let y = self.paint_detail_facts(
            &mut ctx,
            span,
            area.y0 + f64::from(spacing.rhythm.section),
            &snippet,
        );

        // What it does.
        let actions = vec![
            Control::Button {
                label: self.copy_label(id).to_string(),
                icon: None,
                role: ButtonRole::Primary,
                action: Action::Copy(id),
            },
            Control::Button {
                label: self.favorite_label(id).to_string(),
                icon: Some(Icon::Star),
                role: ButtonRole::Secondary,
                action: Action::ToggleFavorite(id),
            },
            Control::IconButton {
                icon: Icon::MoreHorizontal,
                label: "More actions",
                action: Action::More(id),
            },
        ];
        let y = self.paint_control_rows(
            &mut ctx,
            span,
            y + f64::from(spacing.rhythm.group),
            &actions,
        );

        // The body: an editable code well.
        let top = y + f64::from(spacing.rhythm.group);
        let bottom = (area.y1 - gutter).max(top);
        let well = Rect::new(span.0, top, span.1, bottom);
        if well.height() <= 0.0 {
            return;
        }
        let ground = theme.tokens.colors.surfaces.raised;
        ui::surface::card(
            ctx.scene,
            ctx.transform,
            well,
            f64::from(theme.tokens.radii.control),
            ground,
        );
        self.body_editor.set_rect(well);
        self.body_editor.draw_embedded(
            ctx.scene,
            ctx.transform,
            ctx.text,
            &theme.on_ground(ground),
        );
    }

    /// Route a press to the column's controls. Returns whether it used it.
    pub(crate) fn detail_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if !matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        ) || !self.detail.rect.contains(self.pointer)
        {
            return false;
        }
        let hit = self
            .detail
            .hits
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(self.pointer))
            .map(|(_, action)| action.clone());
        if let Some(action) = hit {
            self.body_editor.set_focused(false);
            self.apply(action, ctx.state);
            return true;
        }
        false
    }

    fn detail_accessibility(&self) -> AccessNode {
        let children = self
            .detail
            .hits
            .iter()
            .map(|(rect, action)| {
                AccessNode::new(action.role())
                    .with_name(action.accessible_label(self))
                    .with_action_target(action.semantic_id())
                    .with_bounds(*rect)
                    .with_state(AccessState {
                        focusable: true,
                        checked: match action {
                            Action::ToggleFavorite(id) => {
                                self.library.get(*id).map(|snippet| snippet.favorite)
                            }
                            _ => None,
                        },
                        ..Default::default()
                    })
            })
            .collect();
        AccessNode::new(ui::a11y::Role::Group)
            .with_name(self.open_title())
            .with_bounds(self.detail.rect)
            .with_children(children)
    }
}

/// The content column widget: paints the open snippet from the shared core.
pub(crate) struct DetailPane {
    core: Shared,
    rect: Rect,
}

impl DetailPane {
    pub(crate) fn new(core: &Shared) -> Self {
        Self {
            core: std::rc::Rc::clone(core),
            rect: Rect::ZERO,
        }
    }
}

impl Widget for DetailPane {
    ui::provides!(dyn ui::widget::controls::ActionControls);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        crate::draw::fill_leaf(taffy, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::draw::leaf_rect(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.core
            .borrow_mut()
            .paint_detail(scene, transform, text, theme, self.rect);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if self.core.borrow_mut().detail_event(event, ctx) {
            *ctx.handled = true;
        }
    }

    fn accessibility(&self) -> Option<AccessNode> {
        Some(self.core.borrow().detail_accessibility())
    }

    fn name(&self) -> &'static str {
        "SnippetDetail"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ui::widget::controls::ActionControls for DetailPane {
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        let core = self.core.borrow();
        let mut seen = std::collections::HashSet::new();
        core.detail
            .hits
            .iter()
            .filter(|(_, action)| seen.insert(action.semantic_id()))
            .map(|(_, action)| action.binding())
            .collect()
    }
}
