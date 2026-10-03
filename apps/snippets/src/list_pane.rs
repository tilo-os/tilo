//! The overview column: the snippets of the chosen place, one
//! [`ListRowFace`] each, or the empty state when there are none.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use ui::a11y::{AccessNode, Role};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{
    EmptyState, EmptyStateContent, Interaction, ListRowFace, PressOutcome, RowLeading,
    ScrollMetrics, Scrollbar, Widget, badge_column_width,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};

use crate::app::{Action, Core, Shared};
use crate::model::{Selection, SmartFolder, Snippet, SnippetId};

/// What the list painted last frame, for hit-testing and scrolling.
pub(crate) struct ListState {
    pub(crate) rect: Rect,
    pub(crate) scroll: f64,
    /// Scroll the open snippet's row into view on the next paint.
    pub(crate) reveal: bool,
    content: f64,
    /// One row's height: a wheel line scrolls by it.
    line: f64,
    bar: Scrollbar,
    pub(crate) hits: Vec<(Rect, Action)>,
    /// Each row's leading column and the snippet it shows, for tooltips.
    rows: Vec<(Rect, Rect, SnippetId)>,
}

impl Default for ListState {
    fn default() -> Self {
        Self {
            rect: Rect::ZERO,
            scroll: 0.0,
            reveal: true,
            content: 0.0,
            line: 0.0,
            bar: Scrollbar::vertical(),
            hits: Vec::new(),
            rows: Vec::new(),
        }
    }
}

impl ListState {
    fn metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(self.content, self.rect.height(), self.scroll)
    }

    fn track(&self, theme_gutter: f64) -> Rect {
        Rect::new(
            self.rect.x1 - theme_gutter,
            self.rect.y0,
            self.rect.x1,
            self.rect.y1,
        )
    }
}

/// The one-line secondary text of a row: what the snippet is for, or its
/// first line of code when it has no description.
fn secondary(snippet: &Snippet) -> &str {
    if snippet.description.trim().is_empty() {
        snippet.body.lines().next().unwrap_or("").trim()
    } else {
        snippet.description.as_str()
    }
}

/// "2 variables" for a templated snippet.
fn variables(snippet: &Snippet) -> Option<String> {
    let count = snippet.placeholders().len();
    (count > 0).then(|| {
        ui::i18n::I18nManager::format(
            "snippets-variables",
            &[("count", ui::i18n::ArgValue::Number(count as f64))],
        )
    })
}

fn uses(snippet: &Snippet) -> String {
    ui::i18n::I18nManager::format(
        "snippets-uses",
        &[("count", ui::i18n::ArgValue::Number(f64::from(snippet.uses)))],
    )
}

impl Core {
    fn row_face<'a>(
        &self,
        snippet: &'a Snippet,
        meta: Option<&'a str>,
        column: f64,
        hovered: bool,
    ) -> ListRowFace<'a> {
        let face = ListRowFace::new(&snippet.title)
            .leading(RowLeading::Badge(snippet.language.badge()))
            .leading_column(column)
            .secondary(secondary(snippet))
            .selected(self.open == Some(snippet.id))
            .interaction(Interaction {
                hovered,
                ..Interaction::default()
            });
        match meta {
            Some(meta) => face.meta(meta),
            None => face,
        }
    }

    fn badge_column(&self, text: &mut TextRenderer, theme: &Theme) -> f64 {
        let badges: Vec<&str> = self
            .visible
            .iter()
            .filter_map(|id| self.library.get(*id))
            .map(|s| s.language.badge())
            .collect();
        badge_column_width(text, theme, &badges)
    }

    pub(crate) fn paint_list(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) {
        self.list.hits.clear();
        self.list.rows.clear();
        // A list that changes size keeps the open snippet in view.
        let resized = self.list.rect.size() != area.size();
        self.list.rect = area;
        if area.width() <= 0.0 || area.height() <= 0.0 {
            return;
        }
        if self.visible.is_empty() {
            self.list.content = 0.0;
            self.paint_list_empty(scene, t, text, theme, area);
            return;
        }
        let spacing = &theme.tokens.spacing;
        let inset = f64::from(spacing.s2);
        let gutter = f64::from(theme.tokens.metrics.scrollbar_gutter);
        let column = self.badge_column(text, theme);

        let snippets: Vec<Snippet> = self
            .visible
            .iter()
            .filter_map(|id| self.library.get(*id).cloned())
            .collect();
        let metas: Vec<Option<String>> = snippets.iter().map(variables).collect();
        let heights: Vec<f64> = snippets
            .iter()
            .zip(&metas)
            .map(|(s, meta)| {
                self.row_face(s, meta.as_deref(), column, false)
                    .height(text, theme)
            })
            .collect();
        self.list.line = heights.first().copied().unwrap_or_default();
        self.list.content = heights.iter().sum::<f64>() + inset * 2.0;
        if (std::mem::take(&mut self.list.reveal) || resized)
            && let Some(index) = snippets.iter().position(|s| Some(s.id) == self.open)
        {
            let top = inset + heights.iter().take(index).sum::<f64>();
            let bottom = top + heights.get(index).copied().unwrap_or_default();
            if top - inset < self.list.scroll {
                self.list.scroll = top - inset;
            } else if bottom + inset > self.list.scroll + area.height() {
                self.list.scroll = bottom + inset - area.height();
            }
        }
        let overflows = self.list.metrics().overflows();
        let max = (self.list.content - area.height()).max(0.0);
        self.list.scroll = self.list.scroll.clamp(0.0, max);

        let x1 = area.x1 - inset.max(if overflows { gutter } else { 0.0 });
        let mut y = area.y0 + inset - self.list.scroll;
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &area);
        for (i, (snippet, meta)) in snippets.iter().zip(&metas).enumerate() {
            let h = heights.get(i).copied().unwrap_or_default();
            let rect = Rect::new(area.x0 + inset, y, x1, y + h);
            y += h;
            if rect.y1 < area.y0 || rect.y0 > area.y1 {
                continue;
            }
            let hovered = rect.contains(self.pointer) && area.contains(self.pointer);
            let face = self.row_face(snippet, meta.as_deref(), column, hovered);
            let face = match snippets.get(i + 1) {
                Some(next) => face.separator(self.open == Some(next.id)),
                None => face,
            };
            face.paint(&mut PaintCtx::new(scene, t, text, theme), rect);
            let lead_x = rect.x0 + f64::from(spacing.s3);
            let lead = Rect::new(lead_x, rect.y0, lead_x + column, rect.y1);
            let shown = rect.intersect(area);
            self.list
                .rows
                .push((shown, lead.intersect(area), snippet.id));
            self.list
                .hits
                .push((shown, Action::OpenSnippet(snippet.id)));
        }
        scene.pop_layer();

        if overflows {
            self.list.bar.draw(
                scene,
                t,
                self.list.track(gutter),
                self.list.metrics(),
                theme,
                false,
            );
        }
    }

    /// The list's zero state: an empty library offers its one action; an
    /// empty smart folder says what would fill it.
    fn paint_list_empty(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) {
        let library_empty = self.library.snippets.is_empty();
        let title = if library_empty {
            "No snippets yet"
        } else {
            match &self.selection {
                Selection::Smart(SmartFolder::Favorites) => "No favorites yet",
                Selection::Smart(SmartFolder::MostUsed) => "Nothing copied yet",
                _ => "No snippets here",
            }
        };
        let content = EmptyStateContent {
            icon: Some(if library_empty {
                Icon::Code
            } else {
                Icon::Inbox
            }),
            action_label: library_empty.then_some("New snippet"),
            ..EmptyStateContent::new(title)
        };
        let action = EmptyState::paint(
            &mut PaintCtx::new(scene, t, text, theme),
            area,
            &content,
            self.pointer,
        );
        if let Some(rect) = action {
            self.list.hits.push((rect, Action::NewSnippet));
        }
    }

    /// Route `event` to the list. Returns whether the list used it.
    pub(crate) fn list_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        gutter: f64,
    ) -> bool {
        let area = self.list.rect;
        let inside = area.contains(self.pointer);
        match event {
            WindowEvent::CursorMoved { .. } => {
                let track = self.list.track(gutter);
                let metrics = self.list.metrics();
                if let Some(offset) = self.list.bar.drag_to(track, metrics, self.pointer) {
                    self.list.scroll = offset;
                    return true;
                }
                self.list.bar.update_hover(track, metrics, self.pointer);
                false
            }
            WindowEvent::MouseWheel { delta, .. } if inside => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * self.list.line,
                    MouseScrollDelta::PixelDelta(p) => p.y / ctx.scale_factor,
                };
                let max = (self.list.content - area.height()).max(0.0);
                self.list.scroll = (self.list.scroll - dy).clamp(0.0, max);
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if inside => {
                let metrics = self.list.metrics();
                if metrics.overflows() {
                    match self
                        .list
                        .bar
                        .press(self.list.track(gutter), metrics, self.pointer)
                    {
                        PressOutcome::Thumb => return true,
                        PressOutcome::Page(offset) => {
                            self.list.scroll = offset;
                            return true;
                        }
                        PressOutcome::Miss => {}
                    }
                }
                let hit = self
                    .list
                    .hits
                    .iter()
                    .rev()
                    .find(|(rect, _)| rect.contains(self.pointer))
                    .map(|(_, action)| action.clone());
                if let Some(action) = hit {
                    self.body_editor.set_focused(false);
                    self.apply(action, ctx.state);
                }
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => self.list.bar.release(),
            _ => false,
        }
    }

    fn list_accessibility(&self) -> AccessNode {
        let children = self
            .list
            .hits
            .iter()
            .map(|(rect, action)| match action {
                Action::OpenSnippet(id) => {
                    let face = self.library.get(*id).map(|snippet| {
                        ListRowFace::new(&snippet.title)
                            .secondary(secondary(snippet))
                            .selected(self.open == Some(*id))
                            .accessibility(*rect, action.semantic_id())
                    });
                    face.unwrap_or_else(|| AccessNode::new(Role::ListItem))
                }
                _ => AccessNode::new(action.role())
                    .with_name(action.accessible_label(self))
                    .with_action_target(action.semantic_id())
                    .with_bounds(*rect)
                    .with_state(ui::a11y::AccessState {
                        focusable: true,
                        ..Default::default()
                    }),
            })
            .collect();
        AccessNode::new(Role::List)
            .with_name(self.selection_title())
            .with_bounds(self.list.rect)
            .with_children(children)
    }

    fn list_tooltip(&self, point: Point) -> Option<(Rect, String)> {
        let (row, lead, id) = self
            .list
            .rows
            .iter()
            .find(|(row, _, _)| row.contains(point))?;
        let snippet = self.library.get(*id)?;
        if lead.contains(point) {
            Some((*lead, snippet.language.label().to_string()))
        } else {
            Some((*row, uses(snippet)))
        }
    }
}

/// The overview column widget: paints the list from the shared core.
pub(crate) struct ListPane {
    core: Shared,
    rect: Rect,
    gutter: f64,
}

impl ListPane {
    pub(crate) fn new(core: &Shared) -> Self {
        Self {
            core: std::rc::Rc::clone(core),
            rect: Rect::ZERO,
            gutter: 0.0,
        }
    }
}

impl Widget for ListPane {
    ui::provides!(dyn ui::widget::controls::ActionControls);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.gutter = f64::from(theme.tokens.metrics.scrollbar_gutter);
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
            .paint_list(scene, transform, text, theme, self.rect);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if self.core.borrow_mut().list_event(event, ctx, self.gutter) {
            *ctx.handled = true;
        }
    }

    fn accessibility(&self) -> Option<AccessNode> {
        Some(self.core.borrow().list_accessibility())
    }

    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        self.core.borrow().list_tooltip(point)
    }

    fn wants_pointer_capture(&self) -> bool {
        self.core.borrow().list.bar.is_dragging()
    }

    fn name(&self) -> &'static str {
        "SnippetList"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ui::widget::controls::ActionControls for ListPane {
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        let core = self.core.borrow();
        let mut seen = std::collections::HashSet::new();
        core.list
            .hits
            .iter()
            .filter(|(_, action)| seen.insert(action.semantic_id()))
            .map(|(_, action)| action.binding())
            .collect()
    }
}
