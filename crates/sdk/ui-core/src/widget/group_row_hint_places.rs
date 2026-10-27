//! What a strip's row tells control hints ([`crate::control_hints`]): where
//! its windows' bars and edges are, and whether windows lie out of view;
//! and what the person did with them, as it happens.

use super::GroupRow;
use crate::control_hints::performed::{self, Performed};
use crate::widget::{Widget, WindowEdge};
use vello::kurbo::{Line, Point, Rect};

/// Where a row's controls are for control hints, in the row's coordinates.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HintPlaces {
    /// The row's view.
    pub viewport: Rect,
    /// How many windows the row holds.
    pub windows: usize,
    /// Windows lie past the view's leading and trailing edge.
    pub beyond: (bool, bool),
    /// Where the bar under the pointer takes its window, beside its title,
    /// where the bar is free past it rather than among its buttons.
    pub hovered_bar: Option<Rect>,
    /// Where the focused window's bar takes it, beside its title,
    /// where the bar is free past it rather than among its buttons.
    pub focused_bar: Option<Rect>,
    /// The focused window's edge nearest the pointer that resizes it, as
    /// the line it is lit along, and which edge it is.
    pub focused_edge: Option<(Line, WindowEdge)>,
}

impl GroupRow {
    /// Where this row's controls are for control hints.
    #[must_use]
    pub fn hint_places(&self) -> HintPlaces {
        let viewport = self.viewport();
        let (seen_from, seen_to) = self.visible_canvas_span();
        let beyond = self.group_canvas_spans().iter().fold(
            (false, false),
            |(before, after), (_, x0, x1)| {
                (
                    before || *x0 < seen_from - 1.0,
                    after || *x1 > seen_to + 1.0,
                )
            },
        );
        let shown = |i: usize, group: &&super::WindowGroup| {
            !self.layout_hides_group(i) && !group.get_rect().intersect(viewport).is_zero_area()
        };
        let hovered_bar = self
            .groups
            .iter()
            .enumerate()
            .find(|(i, group)| shown(*i, group) && group.bar_grab_hit(self.last_cursor))
            .and_then(|(_, group)| title_grab(group));
        let focused = self
            .groups
            .iter()
            .enumerate()
            .find(|(i, group)| group.focused && shown(*i, group))
            .map(|(_, group)| group);
        HintPlaces {
            viewport,
            windows: self.groups.len(),
            beyond,
            hovered_bar,
            focused_bar: focused.and_then(title_grab),
            focused_edge: focused.and_then(|group| nearest_edge(group, viewport, self.last_cursor)),
        }
    }

    /// The pointer moved: resting on a window's bar, where the window can
    /// be taken, is noted for control hints.
    pub(super) fn note_bar_hover(&self) {
        if self.group_drag.is_none()
            && self.edge_drag.is_none()
            && self
                .groups
                .iter()
                .any(|group| group.bar_grab_hit(self.last_cursor))
        {
            performed::note(Performed::HoveredWindowBar);
        }
    }
}

/// How finely a bar is walked for where it takes its window.
const GRAB_STEP: f64 = 4.0;

/// The stretch of `group`'s bar around `x`, a point that takes the window,
/// as far as the bar takes it on either side.
fn grab_stretch(group: &super::WindowGroup, x: f64) -> Rect {
    let bar = group.bar_rect();
    let y = bar.center().y;
    let takes = |x: f64| x >= bar.x0 && x <= bar.x1 && free(group, Point::new(x, y));
    let mut from = x;
    while takes(from - GRAB_STEP) {
        from -= GRAB_STEP;
    }
    let mut to = x;
    while takes(to + GRAB_STEP) {
        to += GRAB_STEP;
    }
    Rect::new(from, bar.y0, to, bar.y1)
}

/// Whether the bar takes its window at `p`, clear of its buttons.
fn free(group: &super::WindowGroup, p: Point) -> bool {
    group.bar_grab_hit(p) && !group.window_control_hit(p)
}

/// How wide a stretch of bar must be to be where a window is taken: the
/// slivers between a bar's buttons take it too, but pointing there points at
/// the buttons.
const GRAB_MIN_W: f64 = 32.0;

/// How much of the free bar beside the title a hint points at.
const TITLE_GRAB_W: f64 = 40.0;

/// Where `group`'s bar takes its window beside its title: the start of the
/// first free stretch past its tabs (a window of one tab shows it as its
/// title), wide enough to take hold of, nearer the title than the buttons
/// further along; on a bar with no such stretch, the title. Most of a bar
/// is tabs, a location and controls, which a drag does not take the window
/// by.
fn title_grab(group: &super::WindowGroup) -> Option<Rect> {
    let bar = group.bar_rect();
    let y = bar.center().y;
    let past_tabs = group
        .tab_rects()
        .iter()
        .map(|tab| tab.x1)
        .fold(bar.x0, f64::max);
    let next_free = |from: f64| {
        std::iter::successors(Some(from), |x| Some(x + GRAB_STEP))
            .take_while(|x| *x < bar.x1)
            .find(|x| free(group, Point::new(*x, y)))
    };
    let mut from = past_tabs;
    while let Some(x) = next_free(from) {
        let stretch = grab_stretch(group, x);
        if stretch.width() >= GRAB_MIN_W {
            let x1 = stretch.x1.min(stretch.x0 + TITLE_GRAB_W);
            return Some(Rect::new(stretch.x0, bar.y0, x1, bar.y1));
        }
        from = stretch.x1 + GRAB_STEP;
    }
    // A bar its slots and buttons fill: the title itself, the active tab.
    let active = group.active_tab_id();
    group
        .tabs()
        .iter()
        .position(|tab| tab.id == active)
        .and_then(|i| group.tab_rects().get(i).copied())
        .filter(|title| title.area() > 0.0)
}

/// The edge of `group` nearest `pointer` that a drag resizes and that is in
/// `viewport`, along the window's body under its bar.
fn nearest_edge(
    group: &super::WindowGroup,
    viewport: Rect,
    pointer: Point,
) -> Option<(Line, WindowEdge)> {
    let rect = group.get_rect();
    let top = group.bar_rect().y1.max(rect.y0);
    let inset = 12.0;
    let line_at = |x: f64| Line::new((x, top + inset), (x, rect.y1 - inset));
    let candidates = [
        group
            .leading_resize_grab_rect()
            .map(|_| (line_at(rect.x0), WindowEdge::Leading)),
        group
            .resize_grab_rect()
            .map(|_| (line_at(rect.x1), WindowEdge::Trailing)),
    ];
    let usable: Vec<(Line, WindowEdge)> = candidates
        .into_iter()
        .flatten()
        .filter(|(line, _)| line.p0.x > viewport.x0 + 1.0 && line.p0.x < viewport.x1 - 1.0)
        .filter(|(line, _)| line.p1.y > line.p0.y)
        .collect();
    // The trailing edge, the one between windows that a resize most often
    // takes; the leading one when the pointer rests close by it.
    let near_leading = rect.contains(pointer) && pointer.x < rect.x0 + LEADING_SHARE * rect.width();
    let wanted = if near_leading {
        WindowEdge::Leading
    } else {
        WindowEdge::Trailing
    };
    usable
        .iter()
        .find(|(_, edge)| *edge == wanted)
        .or_else(|| usable.first())
        .copied()
}

/// How far into a window, as a share of its width, the pointer counts as
/// near its leading edge.
const LEADING_SHARE: f64 = 0.25;

#[cfg(test)]
mod tests {
    use super::super::GroupRow;
    use super::super::tests::{make_row_with_groups_with_taffy, relayout_row_owned};
    use vello::kurbo::{Point, Rect};

    // The move hint points at a window's title, where its bar takes it,
    // never at the buttons further along the bar.
    #[test]
    fn hint_places_point_beside_the_title_not_at_the_buttons() {
        use crate::window_toolbar::{ToolbarAction, WindowToolbar};
        let action = |id: &str| ToolbarAction {
            id: id.into(),
            icon: "refresh".into(),
            name: id.into(),
            enabled: true,
            on: false,
            menu: false,
            items: Vec::new(),
            navigates: false,
            toggles: false,
            destructive: false,
            labelled: false,
        };
        let (mut row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        for group in &mut row.groups {
            group.bar_mut().set_window_controls(true);
        }
        let _ = row.groups[0].bar_mut().set_toolbar(
            1,
            Some(WindowToolbar {
                actions: vec![action("more"), action("share"), action("pin")],
                ..WindowToolbar::default()
            }),
        );
        row.groups[0].focused = true;
        let (row, _taffy, _root) = relayout_row_owned(row, 2000.0);
        // A bar the app's buttons fill: the title itself.
        let filled = beside_the_title(&row);
        assert_eq!(filled, row.groups[0].tab_rects()[0], "the title");
        // A bar with room past its title: the room beside it.
        let (mut row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        for group in &mut row.groups {
            group.bar_mut().set_window_controls(true);
        }
        row.groups[0].focused = true;
        let (row, _taffy, _root) = relayout_row_owned(row, 2000.0);
        let free = beside_the_title(&row);
        let group = &row.groups[0];
        let y = free.center().y;
        assert!(group.bar_grab_hit(Point::new(free.x0, y)), "{free:?}");
        assert!(group.bar_grab_hit(Point::new(free.x1, y)), "{free:?}");
        assert!(
            free.x0 >= group.tab_rects()[0].x1,
            "{free:?} past the title"
        );
    }

    /// The move hint's place on `row`'s focused first window, checked to
    /// stand at its title and clear of every button on its bar.
    fn beside_the_title(row: &GroupRow) -> Rect {
        let group = &row.groups[0];
        let anchor = row.hint_places().focused_bar.expect("a place to take");
        let title = group.tab_rects()[0];
        let bar = group.bar_for_tests();
        let (_, _, _, actions) = bar.toolbar_rects();
        let buttons: Vec<Rect> = actions
            .into_iter()
            .chain(bar.window_controls().iter().map(|(_, rect)| rect))
            .chain([group.window_search().anchor()])
            .filter(|rect| rect.area() > 0.0 && rect.x0 >= title.x1)
            .collect();
        assert!(!buttons.is_empty(), "fixture: the bar shows its buttons");
        for button in &buttons {
            assert!(
                anchor.x1 <= button.x0 || anchor.x0 >= button.x1,
                "{anchor:?} over the button {button:?}"
            );
        }
        let last_button = buttons
            .iter()
            .map(|rect| rect.x0)
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            anchor.center().x - title.center().x < last_button - anchor.center().x,
            "{anchor:?} nearer the title {title:?} than the buttons {buttons:?}"
        );
        anchor
    }
}
