//! "Kim opened Notes": a hint at the edge of the strip when someone else
//! opens a window out of sight.
//!
//! On a shared desktop the view stays where the person left it when someone
//! else opens a window: being pulled along would take them away from what
//! they were doing. A window opened off to one side then changed nothing
//! they could see. Now, for a few seconds, a small pill at that edge says
//! who opened what, in their presence colour, with an arrow toward it; a
//! click on it brings the window into view. It never covers more than its
//! own pill, takes no focus, and goes once the window is seen or its time
//! is up.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use tilo_desktop_model::{DesktopTree, TabId};
use ui::TextRole;
use ui::a11y::{AccessNode, Politeness, Role};
use ui::presence::PresenceCtx;
use ui::taffy::{Dimension, LengthPercentageAuto, NodeId, Position, Size, Style, TaffyTree};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::time::{Duration, Instant};
use ui::widget::{TaffyTreeExt as _, Widget};
use ui::winit::event::{ElementState, MouseButton, WindowEvent};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect};
use vello::peniko::Fill;

use super::{DesktopHandle, DesktopSurface};

/// How long a hint stays: long enough to notice from the corner of an eye
/// and decide whether to look, short enough not to linger as clutter.
pub(crate) const ARRIVAL_HOLD: Duration = Duration::from_secs(8);

/// A window someone else opened, and when it arrived here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Arrival {
    pub tab: TabId,
    pub at: Instant,
}

/// Which edge of the strip a window lies beyond.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

/// One hint as laid out.
#[derive(Clone, Debug, PartialEq)]
struct Hint {
    tab: TabId,
    text: String,
    rgb: (u8, u8, u8),
    side: Side,
    until: Instant,
    rect: Rect,
}

/// The windows `tree` has in strips `before` already had, which neither
/// `before` nor this page (`local`) holds: what someone else just opened.
pub(super) fn opened_elsewhere(
    before: &DesktopTree,
    tree: &DesktopTree,
    local: &[TabId],
) -> Vec<TabId> {
    let strips = |tree: &DesktopTree| -> HashMap<u64, HashSet<TabId>> {
        tree.workspaces
            .iter()
            .flat_map(|w| &w.strips)
            .map(|s| {
                let tabs = s
                    .window_groups
                    .iter()
                    .flat_map(|g| g.tabs.iter().map(|t| t.id))
                    .collect();
                (s.id.0, tabs)
            })
            .collect()
    };
    let known = strips(before);
    let mut opened: Vec<TabId> = strips(tree)
        .into_iter()
        .filter_map(|(strip, tabs)| Some((known.get(&strip)?, tabs)))
        .flat_map(|(had, tabs)| tabs.into_iter().filter(move |tab| !had.contains(tab)))
        .filter(|tab| !local.contains(tab))
        .collect();
    opened.sort_unstable();
    opened
}

impl DesktopHandle {
    /// Note `tabs` as just opened by someone else, for the edge hint.
    pub(super) fn note_arrivals(&self, tabs: Vec<TabId>, at: Instant) {
        if tabs.is_empty() {
            return;
        }
        let mut shared = self.shared.borrow_mut();
        shared
            .arrivals
            .retain(|arrival| !tabs.contains(&arrival.tab));
        shared
            .arrivals
            .extend(tabs.into_iter().map(|tab| Arrival { tab, at }));
    }
}

#[derive(Default)]
pub(crate) struct ArrivalHints {
    rect: Rect,
    hints: Vec<Hint>,
    /// Where hints may stand: the strip below the windows' bars.
    area: Rect,
    pointer: Option<Point>,
    /// What the last draw showed, by tab.
    drawn: Cell<Option<Vec<TabId>>>,
    chosen: Option<TabId>,
}

impl ArrivalHints {
    fn showing(&self, now: Instant) -> impl Iterator<Item = &Hint> {
        self.hints.iter().filter(move |hint| now < hint.until)
    }

    fn shown_tabs(&self, now: Instant) -> Vec<TabId> {
        self.showing(now).map(|hint| hint.tab).collect()
    }

    /// The window whose hint was clicked, once.
    pub(crate) fn take_chosen(&mut self) -> Option<TabId> {
        self.chosen.take()
    }

    /// Where each hint showing now is, by tab.
    #[cfg(test)]
    pub(crate) fn hint_rects(&self) -> Vec<(TabId, Rect)> {
        let now = ui::time::now();
        self.showing(now)
            .map(|hint| (hint.tab, hint.rect))
            .collect()
    }

    /// What each hint showing now says, by tab.
    #[cfg(test)]
    pub(crate) fn hint_texts(&self) -> Vec<String> {
        let now = ui::time::now();
        self.showing(now).map(|hint| hint.text.clone()).collect()
    }

    /// Show `hints` in `area` (the strip below the windows' bars).
    fn set(&mut self, hints: Vec<Hint>, area: Rect) {
        self.hints = hints;
        self.area = area;
    }

    /// Lay the hints out, stacked down their edge of the area.
    fn place(&mut self, theme: &Theme, text: &mut TextRenderer) {
        let area = self.area;
        let style = Self::style(theme);
        let spacing = &theme.tokens.spacing;
        let inset = f64::from(spacing.s4);
        let pad = f64::from(spacing.s3);
        let dot = f64::from(spacing.s2);
        let icon = f64::from(theme.tokens.metrics.icon_sm);
        let gap = f64::from(spacing.s2);
        let height = f64::from(spacing.s8);
        let max_text = (area.width() / 2.0 - 2.0 * (inset + pad) - dot - icon - 2.0 * gap).max(0.0);
        let mut tops = [area.y0 + inset, area.y0 + inset];
        for hint in &mut self.hints {
            let width = f64::from(text.measure_style(&hint.text, None, &style).0).min(max_text);
            let full = pad + dot + gap + width + gap + icon + pad;
            let slot = usize::from(hint.side == Side::Right);
            let top = tops.get(slot).copied().unwrap_or(area.y0);
            hint.rect = match hint.side {
                Side::Left => Rect::new(area.x0 + inset, top, area.x0 + inset + full, top + height),
                Side::Right => {
                    Rect::new(area.x1 - inset - full, top, area.x1 - inset, top + height)
                }
            };
            if let Some(next) = tops.get_mut(slot) {
                *next = top + height + gap;
            }
        }
    }

    fn style(theme: &Theme) -> ui::text_render::TextStyle {
        theme.text(TextRole::Label).with_weight(TextWeight::Medium)
    }

    fn draw_hint(
        hint: &Hint,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let spacing = &theme.tokens.spacing;
        let pad = f64::from(spacing.s3);
        let dot = f64::from(spacing.s2);
        let gap = f64::from(spacing.s2);
        let icon = f64::from(theme.tokens.metrics.icon_sm);
        let rect = hint.rect;
        let radius = rect.height() / 2.0;
        let colors = &theme.tokens.colors;
        ui::surface::shadow(
            scene,
            transform,
            rect,
            radius,
            &theme.tokens.elevation.floating,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.overlay,
            None,
            &RoundedRect::from_rect(rect, radius),
        );
        ui::surface::floating_edge(scene, transform, rect, radius, theme);
        let center_y = rect.center().y;
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.presence.of_rgb(hint.rgb),
            None,
            &Circle::new(Point::new(rect.x0 + pad + dot / 2.0, center_y), dot / 2.0),
        );
        let style = Self::style(theme);
        let ink = colors.on_surface(colors.surfaces.overlay).ink;
        let text_x = rect.x0 + pad + dot + gap;
        let max_width = (rect.x1 - pad - icon - gap - text_x).max(0.0);
        let (_, text_h) = text.measure_style(&hint.text, None, &style);
        text.draw_style(
            scene,
            &hint.text,
            ink,
            transform.pre_translate(vello::kurbo::Vec2::new(
                text_x,
                center_y - f64::from(text_h) / 2.0,
            )),
            Some(ui::num::f32_of_f64(max_width)),
            &style,
        );
        let arrow = match hint.side {
            Side::Left => ui::icons::Icon::ArrowLeft,
            Side::Right => ui::icons::Icon::ArrowRight,
        };
        ui::icons::draw_icon(
            scene,
            transform,
            Rect::from_center_size(
                Point::new(rect.x1 - pad - icon / 2.0, center_y),
                (icon, icon),
            ),
            arrow,
            ink,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
    }
}

impl Widget for ArrivalHints {
    fn name(&self) -> &'static str {
        "ArrivalHints"
    }

    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        _theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            position: Position::Absolute,
            inset: ui::taffy::Rect {
                left: LengthPercentageAuto::length(0.0),
                top: LengthPercentageAuto::length(0.0),
                right: LengthPercentageAuto::auto(),
                bottom: LengthPercentageAuto::auto(),
            },
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        };
        tree.widget_leaf(style, map, self)
    }

    ui::rect_geometry!();

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let now = ui::time::now();
        self.place(theme, text);
        for hint in self.showing(now) {
            Self::draw_hint(hint, scene, transform, text, theme);
        }
        self.drawn.set(Some(self.shown_tabs(now)));
    }

    fn needs_redraw(&self) -> bool {
        let drawn = self.drawn.take();
        let stale = drawn.as_ref() != Some(&self.shown_tabs(ui::time::now()));
        self.drawn.set(drawn);
        stale
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        self.showing(ui::time::now()).map(|hint| hint.until).min()
    }

    /// A press on a hint goes to its window; every other event passes.
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.pointer = Some(ctx.logical(position));
            return;
        }
        let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        else {
            return;
        };
        let Some(pointer) = self.pointer else {
            return;
        };
        let now = ui::time::now();
        let pressed = self
            .showing(now)
            .find(|hint| hint.rect.contains(pointer))
            .map(|hint| hint.tab);
        if let Some(tab) = pressed {
            self.chosen = Some(tab);
            self.hints.retain(|hint| hint.tab != tab);
            *ctx.handled = true;
        }
    }

    /// A polite live region saying who opened what, empty while nothing
    /// shows.
    fn accessibility(&self) -> Option<AccessNode> {
        let now = ui::time::now();
        let words: Vec<&str> = self.showing(now).map(|hint| hint.text.as_str()).collect();
        Some(
            AccessNode::new(Role::Label)
                .with_live(Politeness::Polite)
                .with_value(words.join(". ")),
        )
    }
}

impl DesktopSurface {
    /// Lay out a hint for each window someone else opened in the shown
    /// strip that lies wholly beyond its left or right edge.
    pub(super) fn place_arrival_hints(&mut self, content_top: f64) {
        let now = ui::time::now();
        let area = self.rect;
        let windows: Vec<(TabId, Rect)> = self
            .shown_row_ref()
            .map(|row| {
                row.groups
                    .iter()
                    .flat_map(|group| {
                        let rect = group.get_rect();
                        group.tabs().iter().map(move |tab| (tab.id, rect))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let hints = {
            let mut shared = self.shared.borrow_mut();
            // A window once seen needs no pointing at.
            let unseen = |tab: TabId| {
                windows
                    .iter()
                    .find(|(id, _)| *id == tab)
                    .is_none_or(|(_, rect)| rect.x1 <= area.x0 || rect.x0 >= area.x1)
            };
            shared.arrivals.retain(|arrival| {
                now.saturating_duration_since(arrival.at) < ARRIVAL_HOLD && unseen(arrival.tab)
            });
            let shared = &*shared;
            shared
                .arrivals
                .iter()
                .filter_map(|arrival| {
                    let holder = shared.holders.get(&arrival.tab)?;
                    let (_, rect) = windows.iter().find(|(tab, _)| *tab == arrival.tab)?;
                    let side = if rect.x1 <= area.x0 {
                        Side::Left
                    } else if rect.x0 >= area.x1 {
                        Side::Right
                    } else {
                        return None;
                    };
                    let title = title_of(&shared.tree, arrival.tab)?;
                    let text = crate::i18n::t_args(
                        "desktop-window-opened-elsewhere",
                        &[
                            ("person", holder.name.clone().into()),
                            ("app", title.into()),
                        ],
                    );
                    Some(Hint {
                        tab: arrival.tab,
                        text,
                        rgb: holder.rgb,
                        side,
                        until: arrival.at.checked_add(ARRIVAL_HOLD).unwrap_or(arrival.at),
                        rect: Rect::ZERO,
                    })
                })
                .collect()
        };
        // The windows' own bars stay clear: hints hang below them.
        let top = content_top.clamp(area.y0, area.y1);
        let area = Rect::new(area.x0, top, area.x1, area.y1);
        self.arrival_hints.set(hints, area);
    }

    /// The window whose hint was clicked, brought into view.
    pub(super) fn reveal_chosen_arrival(&mut self) {
        if let Some(tab) = self.arrival_hints.take_chosen() {
            let handle = DesktopHandle {
                shared: self.shared.clone(),
            };
            let _ = handle.focus_tab(tab);
        }
    }
}

/// The title of the window showing `tab`.
fn title_of(tree: &DesktopTree, tab: TabId) -> Option<String> {
    tree.workspaces
        .iter()
        .flat_map(|w| &w.strips)
        .flat_map(|s| &s.window_groups)
        .flat_map(|g| &g.tabs)
        .find(|t| t.id == tab)
        .map(|t| t.title.clone())
}
