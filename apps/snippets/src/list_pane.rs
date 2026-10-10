//! The overview column: the snippets of the chosen place, one
//! [`ListRowFace`] each, or the empty state when there are none.
//!
//! The list is one focus stop: Tab reaches it, the arrow keys, Home and End
//! move the open snippet, and Enter or Space brings it to the front of a
//! narrow window. A row opens on a completed click (press and release on
//! the same row), and shows its pressed state in between.

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
use winit::keyboard::{Key, NamedKey};

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
    /// The row a press landed on, until it is released.
    pressed: Option<SnippetId>,
    /// The list has keyboard focus.
    pub(crate) focused: bool,
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
            pressed: None,
            focused: false,
        }
    }
}

impl ListState {
    fn metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(self.content, self.rect.height(), self.scroll)
    }

    /// Scroll row `index` of rows `heights` tall into view. Scrolling down
    /// stops at the first row that keeps it in view, so the list starts on
    /// a whole row.
    fn reveal_row(&mut self, heights: &[f64], index: usize, inset: f64) {
        let tops: Vec<f64> = heights
            .iter()
            .scan(inset, |y, h| {
                let top = *y;
                *y += h;
                Some(top)
            })
            .collect();
        let top = tops.get(index).copied().unwrap_or(inset);
        let bottom = top + heights.get(index).copied().unwrap_or_default();
        if top - inset < self.scroll {
            self.scroll = top - inset;
        } else if bottom + inset > self.scroll + self.rect.height() {
            let least = bottom + inset - self.rect.height();
            self.scroll = tops
                .iter()
                .map(|row_top| row_top - inset)
                .find(|scroll| *scroll >= least)
                .unwrap_or(least);
        }
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

/// The secondary text of a row: what the snippet is for, or its first line
/// of code when it has no description.
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
    (count > 0).then(|| crate::tr_count("snippets-variables", count))
}

fn uses(snippet: &Snippet) -> String {
    crate::tr_count("snippets-uses", snippet.uses as usize)
}

impl Core {
    fn row_face<'a>(
        &self,
        snippet: &'a Snippet,
        meta: Option<&'a str>,
        column: f64,
        interaction: Interaction,
    ) -> ListRowFace<'a> {
        let face = ListRowFace::new(&snippet.title)
            .leading(RowLeading::Badge(snippet.language.badge()))
            .leading_column(column)
            .secondary(secondary(snippet))
            .two_line_secondary()
            .selected(self.open == Some(snippet.id))
            .interaction(interaction);
        let face = if snippet.favorite {
            face.mark(Icon::Star)
        } else {
            face
        };
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
        // The rows' text starts on the column's content edge, where the
        // column's title starts; a row's outer rect is that edge grown by
        // the lift's bleed.
        let edge = ui::chrome::column_gutter(theme) - ui::chrome::row_bleed(theme);
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
                self.row_face(s, meta.as_deref(), column, Interaction::default())
                    .height(text, theme)
            })
            .collect();
        self.list.line = heights.first().copied().unwrap_or_default();
        self.list.content = heights.iter().sum::<f64>() + inset * 2.0;
        if (std::mem::take(&mut self.list.reveal) || resized)
            && let Some(index) = snippets.iter().position(|s| Some(s.id) == self.open)
        {
            self.list.reveal_row(&heights, index, inset);
        }
        let overflows = self.list.metrics().overflows();
        let max = (self.list.content - area.height()).max(0.0);
        self.list.scroll = self.list.scroll.clamp(0.0, max);

        let x1 = area.x1 - edge.max(if overflows { gutter } else { 0.0 });
        let mut y = area.y0 + inset - self.list.scroll;
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &area);
        for (i, (snippet, meta)) in snippets.iter().zip(&metas).enumerate() {
            let h = heights.get(i).copied().unwrap_or_default();
            let rect = Rect::new(area.x0 + edge, y, x1, y + h);
            y += h;
            if rect.y1 < area.y0 || rect.y0 > area.y1 {
                continue;
            }
            let hovered = rect.contains(self.pointer) && area.contains(self.pointer);
            let interaction = Interaction {
                hovered,
                pressed: self.list.pressed == Some(snippet.id) && hovered,
                focused: self.list.focused && self.open == Some(snippet.id),
                disabled: false,
            };
            let face = self.row_face(snippet, meta.as_deref(), column, interaction);
            let face = match snippets.get(i + 1) {
                Some(next) => face.separator(self.open == Some(next.id)),
                None => face,
            };
            let lead = face.leading_rect(rect, text, theme);
            let lead = Rect::new(lead.x0, rect.y0, lead.x1, rect.y1);
            face.paint(&mut PaintCtx::new(scene, t, text, theme), rect);
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

    /// The list's zero state: one sentence saying what would fill the
    /// place. Creating a snippet is the header's action.
    fn paint_list_empty(
        &self,
        scene: &mut Scene,
        t: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) {
        let title = crate::tr(match &self.selection {
            _ if self.library.snippets.is_empty() => "snippets-empty-library",
            Selection::Smart(SmartFolder::Favorites) => "snippets-empty-favorites",
            Selection::Smart(SmartFolder::MostUsed) => "snippets-empty-most-used",
            _ => "snippets-empty-place",
        });
        let content = EmptyStateContent {
            icon: Some(match &self.selection {
                Selection::Smart(SmartFolder::Favorites) => Icon::Star,
                Selection::Smart(SmartFolder::MostUsed) => Icon::History,
                _ => Icon::Inbox,
            }),
            ..EmptyStateContent::new(&title)
        };
        EmptyState::paint(
            &mut PaintCtx::new(scene, t, text, theme),
            area,
            &content,
            self.pointer,
        );
    }

    /// Open the snippet `step` rows away from the open one (clamped to the
    /// list), without bringing it to the front of a narrow window.
    fn step_open(&mut self, step: isize) {
        let Some(last) = self.visible.len().checked_sub(1) else {
            return;
        };
        let current = self
            .open
            .and_then(|id| self.visible.iter().position(|v| *v == id));
        let next = match (current, step) {
            (None, _) => 0,
            (Some(i), isize::MIN..=-1) => i.saturating_sub(step.unsigned_abs()),
            (Some(i), _) => i.saturating_add(step.unsigned_abs()).min(last),
        };
        if let Some(id) = self.visible.get(next).copied() {
            self.open = Some(id);
            self.list.reveal = true;
        }
    }

    /// A key while the list has focus. Returns whether the list used it.
    pub(crate) fn list_key(&mut self, key: &Key) -> bool {
        let page = ((self.list.rect.height() / self.list.line.max(1.0)) as isize).max(1);
        match key {
            Key::Named(NamedKey::ArrowDown) => self.step_open(1),
            Key::Named(NamedKey::ArrowUp) => self.step_open(-1),
            Key::Named(NamedKey::PageDown) => self.step_open(page),
            Key::Named(NamedKey::PageUp) => self.step_open(-page),
            Key::Named(NamedKey::Home) => self.step_open(isize::MIN),
            Key::Named(NamedKey::End) => self.step_open(isize::MAX),
            Key::Named(NamedKey::Enter | NamedKey::Space) => match self.open {
                Some(id) => self.open_snippet(id),
                None => return false,
            },
            _ => return false,
        }
        true
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
        let row_at = |list: &ListState, point: Point| {
            list.hits
                .iter()
                .rev()
                .find(|(rect, _)| rect.contains(point))
                .and_then(|(_, action)| match action {
                    Action::OpenSnippet(id) => Some(*id),
                    _ => None,
                })
        };
        match event {
            WindowEvent::KeyboardInput { event: key, .. }
                if self.list.focused && key.state == ElementState::Pressed =>
            {
                let modifiers = ctx.modifiers.state();
                !(modifiers.control_key() || modifiers.super_key() || modifiers.alt_key())
                    && self.list_key(&key.logical_key)
            }
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
                self.list.pressed = row_at(&self.list, self.pointer);
                if self.list.pressed.is_some() {
                    self.body_editor.set_focused(false);
                    *ctx.focus_target = Some(ctx.node_id);
                }
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                let released = self.list.bar.release();
                let pressed = self.list.pressed.take();
                if let Some(id) = pressed
                    && inside
                    && row_at(&self.list, self.pointer) == Some(id)
                {
                    self.apply(Action::OpenSnippet(id));
                    return true;
                }
                released || pressed.is_some()
            }
            _ => false,
        }
    }

    fn list_accessibility(&self) -> AccessNode {
        let children = self
            .list
            .hits
            .iter()
            .filter_map(|(rect, action)| match action {
                Action::OpenSnippet(id) => self.library.get(*id).map(|snippet| {
                    let meta = variables(snippet);
                    let face = ListRowFace::new(&snippet.title)
                        .secondary(secondary(snippet))
                        .selected(self.open == Some(*id))
                        .interaction(Interaction {
                            focused: self.list.focused && self.open == Some(*id),
                            ..Interaction::default()
                        });
                    let face = if snippet.favorite {
                        face.mark(Icon::Star)
                    } else {
                        face
                    };
                    match &meta {
                        Some(meta) => face.meta(meta),
                        None => face,
                    }
                    .accessibility(*rect, action.semantic_id())
                }),
                _ => None,
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
            Some((*lead, snippet.language.name()))
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
        self.rect = ui::widget::layout_rect(taffy, node, origin);
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

    fn is_focusable(&self) -> bool {
        !self.core.borrow().visible.is_empty()
    }

    fn set_focus(&mut self, focused: bool) {
        self.core.borrow_mut().list.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.core.borrow().list.focused
    }

    fn accessibility(&self) -> Option<AccessNode> {
        Some(self.core.borrow().list_accessibility())
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        _state: &mut ui::widget::Edits<'_>,
        _path: &ui::widget::ModelPath,
    ) -> bool {
        // Synthetic list rows belong to this measured leaf, not the app root.
        // Resolve only a currently presented row, so removed or filtered rows
        // cannot reuse an old semantic target.
        let mut core = self.core.borrow_mut();
        let selected = core.list.hits.iter().find_map(|(_, candidate)| {
            let Action::OpenSnippet(id) = candidate else {
                return None;
            };
            (candidate.semantic_id() == target
                && core.visible.contains(id)
                && core.library.get(*id).is_some())
            .then_some(*id)
        });
        let Some(selected) = selected else {
            return false;
        };
        match action {
            ui::a11y::Action::Activate => {
                core.apply(Action::OpenSnippet(selected));
                true
            }
            ui::a11y::Action::Focus => {
                // Like directional list focus, select and reveal the row while
                // keeping the overview in front on a narrow window.
                core.open = Some(selected);
                core.list.reveal = true;
                core.list.focused = true;
                true
            }
            _ => false,
        }
    }

    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        self.core.borrow().list_tooltip(point)
    }

    fn wants_pointer_capture(&self) -> bool {
        let core = self.core.borrow();
        core.list.bar.is_dragging() || core.list.pressed.is_some()
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
