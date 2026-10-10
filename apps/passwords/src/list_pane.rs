//! The overview column: the items of the chosen place, one [`ListRowFace`]
//! each, or the empty state when there are none.

use std::collections::HashMap;
use std::rc::Rc;
use ui::widget::TaffyTreeExt;

use accounts::SecretId;
use taffy::{NodeId, Style, TaffyTree};
use ui::a11y::{AccessNode, Role};
use ui::chrome::{SelectionState, selection_tier_on};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{
    AvatarFace, Button, ButtonFace, EmptyState, EmptyStateContent, Interaction, ListRowFace,
    RowAction, RowLeading, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::vault::{Core, Request, Requests, Shared, VaultFilter, is_favorite, summary};

/// The semantic target of a row.
pub(crate) fn row_target(id: &SecretId) -> String {
    format!("passwords.item:{}", id.0)
}

fn star_label(favorite: bool) -> &'static str {
    if favorite {
        "Remove from favorites"
    } else {
        "Add to favorites"
    }
}

/// Where a row's favorite toggle sits: the slot of the row's trailing
/// action.
fn star_rect(row: Rect, favorite: bool, theme: &Theme) -> Option<Rect> {
    let action = [RowAction {
        icon: Icon::Star,
        label: star_label(favorite),
    }];
    ListRowFace::new("")
        .actions(&action)
        .interaction(Interaction {
            hovered: true,
            ..Interaction::default()
        })
        .action_rects(row, theme)
        .first()
        .copied()
}

/// Reserve the star's slot on a row that shows it, so a long title ends
/// before the star instead of running under it.
fn hold_star_slot<'a>(
    face: ListRowFace<'a>,
    row: Rect,
    shown: bool,
    favorite: bool,
    theme: &Theme,
) -> ListRowFace<'a> {
    match shown.then(|| star_rect(row, favorite, theme)).flatten() {
        Some(star) => face.trailing_width(star.width()),
        None => face,
    }
}

/// What a row's favorite toggle depends on.
#[derive(Clone, Copy)]
struct StarRow {
    rect: Rect,
    favorite: bool,
    selected: bool,
    hovered: bool,
}

/// Paint a row's favorite toggle where it shows: a favorite keeps its star
/// on, and any row shows it under the pointer. Returns its rect.
fn paint_star(
    scene: &mut Scene,
    t: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    row: StarRow,
    pointer: Point,
) -> Option<Rect> {
    if !row.favorite && !row.hovered {
        return None;
    }
    let star = star_rect(row.rect, row.favorite, theme)?;
    let face = ButtonFace::icon_only(star, Icon::Star, star_label(row.favorite))
        .toggled(row.favorite)
        .hovered(star.contains(pointer));
    // The toggle stands on the row's lift when the row has one, so "on"
    // still reads on the open item.
    if row.selected || row.hovered {
        let state = SelectionState::new(row.selected, row.hovered, false);
        let ground = selection_tier_on(theme, state, theme.tokens.colors.surfaces.content);
        Button::paint(scene, t, text, &theme.on_ground(ground.fill), &face);
    } else {
        Button::paint(scene, t, text, theme, &face);
    }
    Some(star)
}

impl Core {
    pub(crate) fn paint_list(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) {
        self.list.rows.clear();
        self.list.stars.clear();
        self.list.empty_action = None;
        let resized = self.list.rect.size() != area.size();
        self.list.rect = area;
        self.list.scroller.set_viewport(area, theme);
        if area.width() <= 0.0 || area.height() <= 0.0 {
            return;
        }
        let visible: Vec<accounts::SecretItem> = self.visible().into_iter().cloned().collect();
        if visible.is_empty() {
            self.list.scroller.set_content_size(0.0, 0.0);
            self.paint_list_empty(scene, t, text, theme, area);
            return;
        }
        let inset = f64::from(theme.tokens.spacing.s2);
        let summaries: Vec<String> = visible.iter().map(summary).collect();
        let height = ListRowFace::new("")
            .leading(RowLeading::Avatar(AvatarFace::new("", "")))
            .secondary("")
            .height(text, theme);
        let count = ui::num::f64_of_usize(visible.len());
        self.list
            .scroller
            .set_content_size(area.width(), height * count + inset * 2.0);
        if (std::mem::take(&mut self.list.reveal) || resized)
            && let Some(index) = visible
                .iter()
                .position(|item| Some(&item.id) == self.selected.as_ref())
        {
            // The row with the list's inset above and below it.
            let top = ui::num::f64_of_usize(index) * height;
            self.list.scroller.scroll_into_view(Rect::new(
                0.0,
                top,
                area.width(),
                top + height + inset * 2.0,
            ));
        }
        let overflows = self.list.scroller.overflows_vertically();

        // The rows' text starts on the column's content edge, where the
        // column's title starts; a row's lift bleeds past it.
        let edge = ui::chrome::column_gutter(theme) - ui::chrome::row_bleed(theme);
        let x1 = if overflows {
            self.list.scroller.content_rect().x1
        } else {
            area.x1 - edge
        };
        let mut y = area.y0 + inset - self.list.scroller.offset().y;
        let pointer = self.pointer;
        // A new item is not in the list yet: nothing there is the open item.
        let open = if self.screen == crate::vault::Screen::Editor && self.editing.is_none() {
            None
        } else {
            self.selected.clone()
        };
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &area);
        for (i, (item, secondary)) in visible.iter().zip(&summaries).enumerate() {
            let rect = Rect::new(area.x0 + edge, y, x1, y + height);
            y += height;
            if rect.y1 < area.y0 || rect.y0 > area.y1 {
                continue;
            }
            let hovered = rect.contains(pointer) && area.contains(pointer);
            let favorite = is_favorite(item);
            let id = item.id.0.as_str();
            let selected = Some(&item.id) == open.as_ref();
            let face = ListRowFace::new(&item.title)
                .leading(RowLeading::Avatar(
                    AvatarFace::new(&item.title, id).square(),
                ))
                .secondary(secondary)
                .selected(selected)
                .interaction(Interaction {
                    hovered,
                    ..Interaction::default()
                });
            let face = hold_star_slot(face, rect, favorite || hovered, favorite, theme);
            let face = match visible.get(i.saturating_add(1)) {
                Some(next) => face.separator(Some(&next.id) == open.as_ref()),
                None => face,
            };
            face.paint(&mut PaintCtx::new(scene, t, text, theme), rect);
            let row = StarRow {
                rect,
                favorite,
                selected,
                hovered,
            };
            if let Some(star) = paint_star(scene, t, text, theme, row, pointer) {
                self.list
                    .stars
                    .push((star.intersect(area), item.id.clone()));
            }
            self.list.rows.push((rect.intersect(area), item.id.clone()));
        }
        scene.pop_layer();
        self.list.scroller.paint_bars(scene, t, theme);
    }

    fn paint_list_empty(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) {
        let vault_empty = self.items.is_empty();
        let title = if vault_empty {
            "Your vault is empty"
        } else {
            match self.filter {
                VaultFilter::Favorites => "No favorites yet",
                _ => "Nothing of this kind yet",
            }
        };
        let content = EmptyStateContent {
            icon: Some(if self.filter == VaultFilter::Favorites {
                Icon::Star
            } else {
                Icon::Shield
            }),
            action_label: vault_empty.then_some("New item"),
            ..EmptyStateContent::new(title)
        };
        self.list.empty_action = EmptyState::paint(
            &mut PaintCtx::new(scene, t, text, theme),
            area,
            &content,
            self.pointer,
        );
    }

    /// Route `event` to the list, marking it handled when the list used it.
    fn list_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        theme: &Theme,
        requests: &Requests,
    ) {
        // The scroll region takes the wheel, touchpad gestures and its bar.
        self.list.scroller.handle_event(event, ctx, theme);
        let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        else {
            return;
        };
        if *ctx.handled || !self.list.rect.contains(self.pointer) {
            return;
        }
        let pointer = self.pointer;
        let request = if self.list.empty_action.is_some_and(|r| r.contains(pointer)) {
            Some(Request::NewItem)
        } else if let Some((_, id)) = self.list.stars.iter().find(|(r, _)| r.contains(pointer)) {
            Some(Request::ToggleFavorite(id.clone()))
        } else {
            self.list
                .rows
                .iter()
                .find(|(rect, _)| rect.contains(pointer))
                .map(|(_, id)| Request::Open(id.clone()))
        };
        if let Some(request) = request {
            requests.borrow_mut().push(request);
        }
        *ctx.handled = true;
    }

    fn list_accessibility(&self) -> AccessNode {
        let children = self
            .list
            .rows
            .iter()
            .filter_map(|(rect, id)| {
                let item = self.item(id)?;
                let secondary = summary(item);
                Some(
                    ListRowFace::new(&item.title)
                        .secondary(&secondary)
                        .selected(self.selected.as_ref() == Some(id))
                        .accessibility(*rect, row_target(id)),
                )
            })
            .collect();
        AccessNode::new(Role::List)
            .with_name(self.filter.label())
            .with_bounds(self.list.rect)
            .with_children(children)
    }
}

/// The overview column widget: paints the list from the shared core.
pub(crate) struct ListPane {
    core: Shared,
    requests: Requests,
    rect: Rect,
    /// The theme of the last layout, for the scroll region's lanes, steps
    /// and fade.
    theme: Theme,
}

impl ListPane {
    pub(crate) fn new(core: &Shared, requests: &Requests) -> Self {
        Self {
            core: Rc::clone(core),
            requests: Rc::clone(requests),
            rect: Rect::ZERO,
            theme: Theme::dark(),
        }
    }
}

impl Widget for ListPane {
    #[allow(clippy::expect_used, reason = "a fresh taffy leaf cannot fail")]
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = theme.clone();
        let style = Style {
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::percent(1.0),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
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
        self.core
            .borrow_mut()
            .list_event(event, ctx, &self.theme, &self.requests);
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
        if !matches!(action, ui::a11y::Action::Activate) {
            return false;
        }
        let Some(id) = target.strip_prefix("passwords.item:") else {
            return false;
        };
        self.requests
            .borrow_mut()
            .push(Request::Open(SecretId(id.to_owned())));
        true
    }

    fn wants_pointer_capture(&self) -> bool {
        self.core.borrow().list.scroller.is_dragging()
    }

    fn needs_animation_frame(&self) -> bool {
        self.core
            .try_borrow()
            .is_ok_and(|core| core.list.scroller.needs_animation_frame(&self.theme))
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        self.core
            .try_borrow()
            .ok()
            .and_then(|core| core.list.scroller.next_wake_deadline(&self.theme))
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        let core = self.core.borrow();
        (core.list.rows.iter().any(|(rect, _)| rect.contains(point))
            || core.list.empty_action.is_some_and(|r| r.contains(point)))
        .then_some(winit::window::CursorIcon::Pointer)
    }

    fn name(&self) -> &'static str {
        "PasswordsList"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
