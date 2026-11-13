//! What a strip's row tells control hints ([`crate::control_hints`]): where its windows'
//! bars and edges are, and whether windows lie out of view; and what the person did with
//! them, as it happens.

use super::GroupRow;
use crate::control_hints::performed::{self, Performed};
use crate::widget::{Widget, WindowEdge};
use vello::kurbo::{Line, Point, Rect};

/// Where a row's controls are for control hints, in the row's coordinates.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HintPlaces {
    pub viewport: Rect,
    pub windows: usize,
    pub beyond: (bool, bool),
    pub hovered_bar: Option<Rect>,
    pub focused_bar: Option<Rect>,
    pub focused_edge: Option<(Line, WindowEdge)>,
    pub asking: bool,
    pub over: Vec<Rect>,
    pub windows_on_desk: Vec<(Rect, Rect)>,
    pub app_cards: Vec<Rect>,
}

fn asking_for_input(window: &super::WindowGroup) -> bool {
    window
        .active_host()
        .is_some_and(super::TileHost::asking_for_input)
}

impl GroupRow {
    /// Where this row's controls are for control hints.
    #[must_use]
    pub fn hint_places(&self) -> HintPlaces {
        let viewport = self.viewport();
        let (seen_from, seen_to) = self.visible_canvas_span();
        let beyond = self
            .group_canvas_spans()
            .iter()
            .filter(|(_, x0, x1)| x1 > x0)
            .fold((false, false), |(before, after), (_, x0, x1)| {
                (
                    before || *x0 < seen_from - 1.0,
                    after || *x1 > seen_to + 1.0,
                )
            });
        let shown = |i: usize, group: &&super::WindowGroup| {
            !self.layout_hides_group(i) && !group.get_rect().intersect(viewport).is_zero_area()
        };
        let hovered_bar = self
            .groups
            .iter()
            .enumerate()
            .find(|(i, group)| {
                shown(*i, group)
                    && !self.is_floating(group.id)
                    && group.bar_grab_hit(self.last_cursor)
            })
            .and_then(|(_, group)| title_grab(group));
        let focused = self
            .groups
            .iter()
            .enumerate()
            .find(|(i, group)| group.focused && shown(*i, group) && !self.is_floating(group.id))
            .map(|(_, group)| group);
        let on_desk: Vec<(Rect, Rect)> = self
            .groups
            .iter()
            .enumerate()
            .filter(|(i, group)| shown(*i, group) && !self.is_floating(group.id))
            .map(|(_, group)| {
                let rect = group.get_rect();
                let content =
                    Rect::new(rect.x0, group.bar_rect().y1.max(rect.y0), rect.x1, rect.y1);
                (rect, content)
            })
            .collect();
        HintPlaces {
            viewport,
            windows: self
                .groups
                .iter()
                .filter(|group| !self.is_floating(group.id))
                .count(),
            beyond,
            hovered_bar,
            focused_bar: focused.and_then(title_grab),
            focused_edge: focused.and_then(|group| nearest_edge(group, viewport, self.last_cursor)),
            asking: focused.is_some_and(asking_for_input),
            over: self
                .groups
                .iter()
                .filter(|group| self.is_floating(group.id))
                .map(|group| group.get_rect().union(group.bar_rect()))
                .collect(),
            app_cards: on_desk
                .iter()
                .flat_map(|(_, content)| {
                    crate::presence::interaction_card_slots(*content)
                        .into_iter()
                        .map(move |slot| slot.intersect(*content))
                        .filter(|slot| !slot.is_zero_area())
                })
                .collect(),
            windows_on_desk: on_desk,
        }
    }

    /// The pointer moved: resting on a window's bar, where the window can be taken, is
    /// noted for control hints.
    pub(super) fn note_bar_hover(&self) {
        if self.group_drag.is_none()
            && self.edge_drag.is_none()
            && self
                .groups
                .iter()
                .any(|group| !self.is_floating(group.id) && group.bar_grab_hit(self.last_cursor))
        {
            performed::note(Performed::HoveredWindowBar);
        }
    }
}

const GRAB_STEP: f64 = 4.0;

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

fn free(group: &super::WindowGroup, p: Point) -> bool {
    group.bar_grab_hit(p) && !group.window_control_hit(p)
}

/// How wide a stretch of bar must be to be where a window is taken: the
/// slivers between a bar's buttons take it too, but pointing there points at
/// the buttons.
const GRAB_MIN_W: f64 = 32.0;

const TITLE_GRAB_W: f64 = 40.0;

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
    let active = group.active_tab_id();
    group
        .tabs()
        .iter()
        .position(|tab| tab.id == active)
        .and_then(|i| group.tab_rects().get(i).copied())
        .filter(|title| title.area() > 0.0)
}

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

const LEADING_SHARE: f64 = 0.25;

#[cfg(test)]
mod tests {
    use super::super::GroupRow;
    use super::super::tests::{
        make_row_with_groups_with_taffy, relayout_row_owned, relayout_row_owned_with,
    };
    use crate::theme::Theme;
    use crate::widget::Widget as _;
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
            primary: false,
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
        let filled = beside_the_title(&row);
        assert_eq!(filled, row.groups[0].tab_rects()[0], "the title");
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

    // An app stands cards of its own over its window's content (an agent's activity): the corners it
    // stands them in are reported as places no hint may stand, so the two never cover each other.
    #[test]
    fn a_shown_window_reports_the_corners_its_app_stands_cards_in() {
        let (mut row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        row.groups[0].focused = true;
        let (row, _taffy, _root) = relayout_row_owned(row, 1200.0);
        let places = row.hint_places();
        let cards = places.app_cards;
        assert_eq!(
            cards.len(),
            6,
            "three cards' places for each of two windows: {cards:?}"
        );
        for (index, group) in row.groups.iter().enumerate() {
            let rect = group.get_rect();
            let content = Rect::new(rect.x0, group.bar_rect().y1.max(rect.y0), rect.x1, rect.y1);
            let first = cards[index * 3];
            assert_eq!(
                first.x1,
                content.x1 - 12.0,
                "the window's trailing edge, a card's margin in: {first:?}"
            );
            assert_eq!(
                first.y0,
                content.y0 + 12.0,
                "the top of its content: {first:?}"
            );
            assert!(content.contains_rect(first), "{first:?} in {content:?}");
        }
        use crate::widget::Widget as _;
        use std::collections::HashMap;
        let (mut row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        let mut floats = HashMap::new();
        floats.insert(row.groups[1].id, Rect::new(880.0, 120.0, 1240.0, 640.0));
        row.set_floats(floats);
        let (row, _taffy, _root) = relayout_row_owned(row, 1200.0);
        assert_eq!(
            row.hint_places().app_cards.len(),
            3,
            "the strip's one window keeps its corners"
        );
    }

    // The desktop's bar overlays the desk's top edge and the windows stand clear of it: each
    // window is reported whole, and its content from below its own bar, so a hint never stands
    // over either.
    #[test]
    fn a_shown_window_reports_its_content_below_its_own_bar() {
        let (mut row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        row.set_top_overlay_inset(32.0);
        let (row, _taffy, _root) =
            relayout_row_owned_with(row, 1200.0, &Theme::dark().with_reduce_motion(true));
        let places = row.hint_places();
        assert_eq!(places.windows_on_desk.len(), 2, "{places:?}");
        for (window, content) in &places.windows_on_desk {
            assert!(
                window.y0 > places.viewport.y0 + 1.0,
                "fixture: the window stands clear of the bar: {window:?}"
            );
            let bar = row
                .groups
                .iter()
                .find(|group| group.get_rect() == *window)
                .map(|group| group.bar_rect());
            assert_eq!(bar.map(|bar| bar.y1), Some(content.y0), "below its bar");
            assert_eq!(
                content.x0..content.x1,
                window.x0..window.x1,
                "its own width"
            );
        }
    }

    #[test]
    fn a_floating_card_is_no_window_a_hint_points_at() {
        use crate::widget::Widget as _;
        use std::collections::HashMap;

        let (mut row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        let card = Rect::new(880.0, 120.0, 1240.0, 640.0);
        let mut floats = HashMap::new();
        floats.insert(row.groups[1].id, card);
        row.set_floats(floats);
        row.groups[1].focused = true;
        let (row, _taffy, _root) = relayout_row_owned(row, 2000.0);
        let places = row.hint_places();
        assert_eq!(places.windows, 1, "the strip holds one window, not two");
        assert_eq!(places.focused_bar, None, "a card's header takes no hint");
        assert_eq!(places.focused_edge, None, "a card's edge resizes nothing");
        let over = places.over;
        assert_eq!(over.len(), 1, "{over:?}");
        assert!(
            over[0].contains_rect(row.groups[1].get_rect()),
            "the card whole, header and body, is content no hint may stand over: {over:?}"
        );
        let (mut row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        row.groups[0].focused = true;
        let (row, _taffy, _root) = relayout_row_owned(row, 2000.0);
        assert!(row.hint_places().focused_edge.is_some());
        assert!(row.hint_places().over.is_empty());
    }

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

#[cfg(test)]
mod asking_tests {
    use super::super::tests::make_row_with_groups_with_taffy;
    use super::super::{TileContent, Widget};
    use crate::AppKind;
    use crate::ime::InputContent;
    use crate::widget::TaffyTreeExt as _;
    use crate::widget::app_widget::{AppWidget, IconDescriptor};

    struct Field(bool);

    impl Widget for Field {
        fn build_layout(
            &mut self,
            taffy: &mut taffy::TaffyTree<()>,
            _theme: &crate::theme::Theme,
            _map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            taffy.leaf_node(taffy::Style::default())
        }
        fn name(&self) -> &'static str {
            "Field"
        }
        fn ime_content(&self) -> Option<InputContent> {
            self.0.then(InputContent::default)
        }
    }

    impl AppWidget for Field {
        fn app_kind(&self) -> AppKind {
            AppKind::Files
        }
        fn app_title(&self) -> &'static str {
            "Field"
        }
        fn app_icon(&self) -> IconDescriptor {
            IconDescriptor::Glyph("F")
        }
    }

    fn row_whose_window_types(typing: bool) -> super::GroupRow {
        let (mut row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        let host = row.groups[0].host_for_tab_mut(1).expect("a host");
        host.replace_content(TileContent::App(Box::new(Field(typing))));
        row.groups[0].focused = true;
        row
    }

    #[test]
    fn a_window_a_person_is_typing_in_says_so_to_the_hints() {
        assert!(row_whose_window_types(true).hint_places().asking);
        assert!(!row_whose_window_types(false).hint_places().asking);
        let (mut row, _taffy, _root) = make_row_with_groups_with_taffy(&[&[1], &[2]]);
        let host = row.groups[0].host_for_tab_mut(1).expect("a host");
        host.replace_content(TileContent::App(Box::new(Field(true))));
        assert!(!row.hint_places().asking);
    }
}
