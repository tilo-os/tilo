//! `GuidedPage`: where a person is in a short sequence, the page that step shows, and the ways
//! forward under it.
//!
//! A setup wizard, an import, a first-run card: one page whose body scrolls under a fixed
//! account of where the person is, with what needs attention and the ways forward kept in view.
//! The account is either a [`Stepper`] of named stations or a progress track with the stage's
//! name and which step of how many; the body scrolls when it is taller than the room, keeping
//! the bar's gutter whether it is or not, so what stands on one step does not move on the next;
//! a notice stands above the ways forward, never inside the scrolling page.
//!
//! It stands two ways. Alone on a window's ground ([`GuidedPage::new`]) it takes the readable
//! column, centred, the page's own gutter round it. In a surface that already measures it
//! ([`GuidedPage::embedded`]) it takes the room it is offered: a card's body, held to the card's
//! height while its page scrolls.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::style_helpers::{auto, fr, length, minmax, percent};
use taffy::{AlignItems, NodeId, Size, Style, TaffyTree};
use vello::kurbo::Point;

use tilo_ui_base::component::{Component, Described, Event, PartViews};
use tilo_ui_base::draw::DrawCtx;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::{Rhythm, TextRole};
use tilo_ui_base::widget::composition_parts::Parts;
use tilo_ui_base::widget::layout::insets_all;
use tilo_ui_base::widget::{Effective, Element, LayoutInfo, LayoutOrigin, PageLayout, Widget};

use crate::widget::form_composition::column_style;
use crate::widget::stepper::Stepper;
use crate::widget::{Column, ProgressBar, Row, Scrollable, Spacer, Text};

/// Where a person is in a short sequence, the page that step shows, and the ways forward under
/// it: a stepper or a progress track, a body that scrolls, an optional notice and the page's
/// actions, all in view under it.
#[derive(Component, Default)]
#[component(name = "GuidedPage", role = "Group", label = "stage")]
pub struct GuidedPageProps {
    /// The steps of the sequence, in order; empty when the page shows a progress track instead.
    pub steps: Vec<String>,
    /// Which step is current, counted from 0. None before the first step starts.
    pub current: Option<i64>,
    /// The step gone back to: carries its index. Without it, no station takes the pointer.
    #[component(event(value = i64))]
    pub station: Event,
    /// What the sequence is setting up, beside the track: "Tilo identity setup".
    pub stage: Option<String>,
    /// Which step it is among them, in words: "Step 2 of 4".
    pub position: Option<String>,
    /// How far the track is, from 0 to 1; absent, no track shows where no stepper does.
    pub progress: Option<f64>,
    /// What assistive technology hears the track say: the whole stage, in words.
    pub progress_name: Option<String>,
    /// The page, which scrolls when it is taller than the room.
    #[component(part(default))]
    pub body: Option<Element>,
    /// What needs attention, above the ways forward and outside the scroll: a banner.
    #[component(part)]
    pub notice: Option<Element>,
    /// The ways forward, under the page and outside the scroll.
    #[component(part)]
    pub actions: Option<Element>,
    /// Show the scroll bar whenever the page is longer than the room.
    pub shows_bar: bool,
    /// Scroll a control the keyboard reaches into view: a form held to a card's height.
    pub reveals_focus: bool,
    /// What the scrolling page holds, for assistive technology, where Tab stops on it.
    pub scroll_label: Option<String>,
    /// Take the height the surface leaves (a card held to its height) instead of the height of
    /// what it holds.
    pub fills_height: bool,
}

/// See the module docs.
pub struct GuidedPage {
    /// The parts in the order they stand: where the person is (if named), the page that scrolls,
    /// what needs attention (if any), and the ways forward (if any).
    parts: Parts,
    /// Where the page that scrolls stands among the parts: after the account of where the person
    /// is, or first when there is none.
    scroll_at: usize,
    notice: bool,
    actions: bool,
    /// Stand alone on a window's ground, on the readable column, rather than in a surface that
    /// already measures it.
    standalone: bool,
    /// Take the height the surface leaves.
    fills_height: bool,
    shows_bar: bool,
    reveals_focus: bool,
    scroll_label: Option<String>,
    /// Centre a short page in the room under its header.
    center: bool,
    cursor: Option<Point>,
    offset: Option<Rc<Cell<(f32, f32)>>>,
    page: Option<PageLayout>,
    /// The part a host is changing.
    editing: Option<Part>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    Body,
    Notice,
    Actions,
}

impl GuidedPage {
    /// A page alone on a window's ground, showing `body`, on the readable column.
    pub fn new(body: impl Widget + 'static) -> Self {
        let mut page = Self::bare(true, true);
        page.put_scroll(Some(Box::new(body)));
        page
    }

    /// A page in a surface that already measures it (a card's body): it takes the room it is
    /// offered and draws no readable column or gutter of its own.
    #[must_use]
    pub fn embedded(mut self) -> Self {
        self.standalone = false;
        self.reseat();
        self
    }

    /// Take the height the surface leaves, so a long page scrolls inside it. A card that is as
    /// tall as its content leaves this off.
    #[must_use]
    pub fn fills_height(mut self) -> Self {
        self.fills_height = true;
        self.reseat();
        self
    }

    /// Where the person is, as named stations.
    #[must_use]
    pub fn stepper(mut self, stepper: Stepper) -> Self {
        self.set_header(Some(Box::new(stepper)));
        self
    }

    /// Keep the page's scroll position in `offset` across rebuilds.
    #[must_use]
    pub fn offset(mut self, offset: Rc<Cell<(f32, f32)>>) -> Self {
        self.offset = Some(offset);
        self.reseat();
        self
    }

    /// Seed the pointer position when the page is rebuilt every frame.
    #[must_use]
    pub fn cursor(mut self, cursor: Point) -> Self {
        self.cursor = Some(cursor);
        self.reseat();
        self
    }

    /// What needs attention, above the ways forward and outside the scroll.
    #[must_use]
    pub fn maybe_notice(mut self, notice: Option<Box<dyn Widget>>) -> Self {
        if let Some(notice) = notice {
            self.put(Part::Notice, notice);
        }
        self
    }

    /// The ways forward, under the page and outside the scroll.
    #[must_use]
    pub fn actions(mut self, actions: impl Widget + 'static) -> Self {
        self.put(Part::Actions, Box::new(actions));
        self
    }

    /// Centre a short page in the room under its header: a page of explanation on a tall window,
    /// rather than one that floats at the top.
    #[must_use]
    pub fn center_short_body(mut self) -> Self {
        if !self.center {
            self.center = true;
            self.reseat();
        }
        self
    }

    /// Box this widget in the SDK so thin apps reuse its shared implementation.
    #[must_use]
    pub fn build(self) -> Box<dyn Widget> {
        Box::new(self)
    }

    fn bare(standalone: bool, fills_height: bool) -> Self {
        Self {
            parts: Parts::new(Vec::new()),
            scroll_at: 0,
            notice: false,
            actions: false,
            standalone,
            fills_height,
            shows_bar: true,
            reveals_focus: true,
            scroll_label: None,
            center: false,
            cursor: None,
            offset: None,
            page: None,
            editing: None,
        }
    }

    /// The scrolling page, built from the flags this page carries and holding `body`.
    fn make_scroll(&self, body: Option<Element>) -> Scrollable {
        let mut scroll = Scrollable::new_vertical();
        if self.shows_bar {
            // The page under the account is one of several, each a step of the same sequence:
            // the bar's gutter stays kept whichever step fits and whichever overflows, so a
            // control stands where it stood on the last page.
            scroll = scroll.always_visible().with_stable_vertical_gutter();
        }
        if self.reveals_focus {
            scroll = scroll.with_focus_reveal();
        }
        if let Some(label) = &self.scroll_label {
            scroll = scroll.with_label(label.clone());
        }
        if self.standalone || self.fills_height {
            scroll = scroll.fill_parent_height();
        }
        if let Some(offset) = &self.offset {
            scroll = scroll.with_persisted_offset(Rc::clone(offset));
        }
        if let Some(cursor) = self.cursor {
            scroll = scroll.with_cursor(cursor);
        }
        let Some(body) = body else {
            return scroll;
        };
        if !self.center {
            return scroll.with_child(body);
        }
        // The page stands in a row that fills the viewport, centred in it.
        scroll
            .with_content_min_viewport_height()
            .with_child(Box::new(
                Row::new()
                    .no_wrap()
                    .with_padding_rhythm(None, None)
                    .with_align_items(AlignItems::Center)
                    .push(body),
            ))
    }

    /// Put a scrolling page holding its current body back in place, keeping its options.
    fn reseat(&mut self) {
        let body = self.scroll_mut().and_then(|scroll| {
            scroll
                .child_mut()
                .map(|body| std::mem::replace(body, Box::new(Spacer::new()) as Element))
        });
        self.put_scroll(body);
    }

    fn put_scroll(&mut self, body: Option<Element>) {
        let scroll: Element = Box::new(self.make_scroll(body));
        let at = self.scroll_at;
        if at < self.parts.children.len() {
            self.parts.children[at] = scroll;
        } else {
            self.parts.children.push(scroll);
        }
    }

    fn set_header(&mut self, header: Option<Element>) {
        match (header, self.scroll_at) {
            (Some(header), 1) => self.parts.children[0] = header,
            (Some(header), _) => {
                self.parts.children.insert(0, header);
                self.scroll_at = 1;
            }
            (None, _) => {}
        }
    }

    fn scroll_ref(&self) -> Option<&Scrollable> {
        self.parts
            .children
            .get(self.scroll_at)?
            .as_any()?
            .downcast_ref()
    }

    fn scroll_mut(&mut self) -> Option<&mut Scrollable> {
        self.parts
            .children
            .get_mut(self.scroll_at)?
            .as_any_mut()?
            .downcast_mut()
    }

    /// The views of the page that scrolls.
    fn body(&mut self) -> Option<&mut dyn PartViews> {
        <Scrollable as Described>::part_views(self.scroll_mut()?, "content")
    }

    /// Where a part stands, when it holds one.
    fn index(&self, part: Part) -> Option<usize> {
        match part {
            Part::Body => Some(self.scroll_at),
            Part::Notice => self.notice.then_some(self.scroll_at + 1),
            Part::Actions => self.actions.then(|| self.parts.children.len() - 1),
        }
    }

    /// Put `view` where `part` stands, or first when it held none.
    fn put(&mut self, part: Part, view: Element) {
        if let Some(at) = self.index(part) {
            if let Some(slot) = self.parts.children.get_mut(at) {
                *slot = view;
            }
        } else {
            let at = match part {
                Part::Body => self.scroll_at,
                Part::Notice => {
                    self.notice = true;
                    self.scroll_at + 1
                }
                Part::Actions => {
                    self.actions = true;
                    self.parts.children.len()
                }
            };
            self.parts.children.insert(at, view);
        }
    }

    /// The room a page stands in: the readable column, centred, a group's gutter round it alone
    /// on a window's ground, or the room a surface offers it.
    fn page_style(&self, theme: &Theme, rows: Vec<taffy::GridTemplateComponent<String>>) -> Style {
        let group = theme.tokens.spacing.rhythm.get(Rhythm::Group);
        let mut style = column_style(group);
        style.grid_template_rows = rows;
        if self.standalone {
            style.padding = insets_all(group);
            style.size = Size {
                width: percent(1.0_f32),
                height: percent(1.0_f32),
            };
            style.max_size = Size {
                width: length(theme.tokens.metrics.readable_width + 2.0 * group),
                height: auto(),
            };
            style.margin = taffy::Rect {
                left: auto(),
                right: auto(),
                top: length(0.0_f32),
                bottom: length(0.0_f32),
            };
        } else if self.fills_height {
            style.size.height = percent(1.0_f32);
        }
        style
    }
}

/// Where the person is, as a track: the stage's name on the leading edge, which step of how many
/// on the trailing one, and a bar between the page and the words that say it.
fn stage_track(
    stage: Option<&str>,
    position: Option<&str>,
    progress: f32,
    name: Option<&str>,
) -> Element {
    let mut row = Row::new()
        .no_wrap()
        .with_rhythm(Rhythm::Related)
        .with_align_items(AlignItems::Center);
    if let Some(stage) = stage.filter(|stage| !stage.is_empty()) {
        row = row.push(Box::new(
            Text::new(stage).with_role(TextRole::Label).dimmed(),
        ));
    }
    row = row.push(Box::new(Spacer::new()));
    if let Some(position) = position.filter(|position| !position.is_empty()) {
        row = row.push(Box::new(
            Text::new(position)
                .with_role(TextRole::Label)
                .dimmed()
                .numeric(),
        ));
    }
    let mut bar = ProgressBar::new(progress.clamp(0.0, 1.0));
    if let Some(name) = name.filter(|name| !name.is_empty()) {
        bar = bar.with_label(name);
    }
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(Rhythm::Related)
            .push(Box::new(row))
            .push(Box::new(bar)),
    )
}

/// Where the person is, from the props that say it: a stepper of named stations, or a track.
fn header_of(props: &mut GuidedPageProps) -> Option<Element> {
    let station = std::mem::take(&mut props.station);
    if props.steps.is_empty() {
        return props.progress.map(|progress| {
            stage_track(
                props.stage.as_deref(),
                props.position.as_deref(),
                tilo_ui_base::num::f32_of_f64(progress),
                props.progress_name.as_deref(),
            )
        });
    }
    let steps = std::mem::take(&mut props.steps);
    let mut stepper =
        Stepper::new(steps).current(props.current.and_then(|index| usize::try_from(index).ok()));
    if !station.is_none() {
        stepper = stepper.on_station(move |index| {
            station.fire(Some(tilo_ui_base::component::Value::Int(
                i64::try_from(index).unwrap_or(i64::MAX),
            )));
        });
    }
    Some(Box::new(stepper) as Element)
}

impl Described for GuidedPage {
    type Props = GuidedPageProps;

    fn from_props(_key: &str, mut props: GuidedPageProps) -> Self {
        let header = header_of(&mut props);
        let mut page = Self::bare(false, props.fills_height);
        page.shows_bar = props.shows_bar;
        page.reveals_focus = props.reveals_focus;
        page.scroll_label = props.scroll_label.take();
        page.set_header(header);
        page.put_scroll(props.body.take());
        if let Some(notice) = props.notice.take() {
            page.put(Part::Notice, notice);
        }
        if let Some(actions) = props.actions.take() {
            page.put(Part::Actions, actions);
        }
        page
    }

    /// The account of where the person is and the room the page stands in are the props': they
    /// are taken here, where the page stands, so a person's field, press or open menu in the page
    /// that scrolls stays. A part a host hands with them takes its place; absent, the part it
    /// already holds stays.
    fn set_props(&mut self, mut props: GuidedPageProps) -> bool {
        let header = header_of(&mut props);
        let others = self.shows_bar != props.shows_bar
            || self.reveals_focus != props.reveals_focus
            || self.fills_height != props.fills_height
            || self.scroll_label != props.scroll_label;
        self.shows_bar = props.shows_bar;
        self.reveals_focus = props.reveals_focus;
        self.fills_height = props.fills_height;
        self.scroll_label = props.scroll_label.take();
        self.set_header(header);
        if others {
            self.reseat();
        }
        if let Some(body) = props.body.take() {
            self.put_scroll(Some(body));
        }
        if let Some(notice) = props.notice.take() {
            self.put(Part::Notice, notice);
        }
        if let Some(actions) = props.actions.take() {
            self.put(Part::Actions, actions);
        }
        self.parts.described();
        true
    }

    fn part_views(&mut self, part: &str) -> Option<&mut dyn PartViews> {
        self.editing = match part {
            "body" => Some(Part::Body),
            "notice" => Some(Part::Notice),
            "actions" => Some(Part::Actions),
            _ => return None,
        };
        Some(self)
    }
}

impl PartViews for GuidedPage {
    fn len(&self) -> usize {
        match self.editing {
            Some(Part::Body) => self.scroll_ref().map_or(0, <Scrollable as PartViews>::len),
            Some(Part::Notice) => usize::from(self.notice),
            Some(Part::Actions) => usize::from(self.actions),
            None => 0,
        }
    }

    fn view_mut(&mut self, index: usize) -> Option<&mut Element> {
        match self.editing {
            Some(Part::Body) => self.body()?.view_mut(index),
            Some(part) => {
                let at = self.index(part)?;
                self.parts.children.get_mut(at)
            }
            None => None,
        }
    }

    fn insert(&mut self, index: usize, view: Element) {
        match self.editing {
            Some(Part::Body) => {
                if let Some(body) = self.body() {
                    body.insert(index, view);
                }
            }
            Some(part) => self.put(part, view),
            None => return,
        }
        self.parts.described();
    }

    fn remove(&mut self, index: usize) {
        match self.editing {
            Some(Part::Body) => {
                if let Some(body) = self.body() {
                    body.remove(index);
                }
            }
            Some(part) => {
                let Some(at) = self.index(part) else {
                    return;
                };
                self.parts.children.remove(at);
                match part {
                    Part::Notice => self.notice = false,
                    Part::Actions => self.actions = false,
                    Part::Body => {}
                }
            }
            None => return,
        }
        self.parts.described();
    }

    fn move_view(&mut self, from: usize, to: usize) {
        if self.editing == Some(Part::Body)
            && let Some(body) = self.body()
        {
            body.move_view(from, to);
            self.parts.described();
        }
    }

    fn replace(&mut self, index: usize, view: Element) {
        match self.editing {
            Some(Part::Body) => {
                if let Some(body) = self.body() {
                    body.replace(index, view);
                }
            }
            Some(part) => self.put(part, view),
            None => return,
        }
        self.parts.described();
    }
}

impl Widget for GuidedPage {
    tilo_ui_base::described!();

    fn name(&self) -> &'static str {
        "GuidedPage"
    }

    fn layout_info(&self) -> Option<LayoutInfo> {
        self.page.map(LayoutInfo::Page)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let group = theme.tokens.spacing.rhythm.get(Rhythm::Group);
        // Each part is a row of its own, the page the one that takes the room left.
        let rows = (0..self.parts.children.len())
            .map(|at| {
                if at == self.scroll_at {
                    minmax(length(0.0_f32), fr(1.0_f32))
                } else {
                    auto()
                }
            })
            .collect();
        self.page = self.standalone.then(|| PageLayout {
            max_content_width: Effective::new(
                theme.tokens.metrics.readable_width,
                LayoutOrigin::Pattern,
            ),
            gutter: Effective::new(group, LayoutOrigin::Theme),
        });
        let style = self.page_style(theme, rows);
        let node = self.parts.build(style, taffy, theme, map);
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        self.parts.draw(ctx);
    }

    tilo_ui_base::retained_parts!(lifecycle: parts);
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "in a test these are the assertion"
    )]

    use super::*;
    use crate::component_testing::Node;
    use tilo_ui_base::component::Value;

    /// The page's parts change where they stand, between the track above and the ways forward
    /// below.
    #[test]
    fn a_guided_page_reconciles_its_parts() {
        let queue = tilo_ui_base::Intents::default();
        let mut node = Node::new("GuidedPage", "setup", &queue)
            .prop(
                "steps",
                Value::List(vec![Value::Text("One".into()), Value::Text("Two".into())]),
            )
            .prop("current", Value::Int(0))
            .view("body", Box::new(Text::new("First page")))
            .view("actions", Box::new(Text::new("Continue")));
        let props = GuidedPageProps::from_source(&mut node).unwrap();
        let mut page = GuidedPage::from_props("setup", props);
        // The track, the page and the ways forward; no notice yet.
        assert_eq!(page.children().len(), 3);
        {
            let body = Described::part_views(&mut page, "body").unwrap();
            assert_eq!(body.len(), 1, "the page holds the one view given it");
            body.replace(0, Box::new(Text::new("Second page")));
        }
        {
            let notice = Described::part_views(&mut page, "notice").unwrap();
            notice.insert(0, Box::new(Text::new("Something needs attention")));
        }
        assert_eq!(
            page.children().len(),
            4,
            "a notice stands between the page and the ways forward"
        );
    }

    #[test]
    fn a_page_alone_stands_on_the_readable_column_and_in_a_surface_takes_the_room() {
        let mut taffy = TaffyTree::new();
        let mut map = HashMap::new();
        let mut page = GuidedPage::new(Text::new("A page"));
        let _ = page.build_layout(&mut taffy, &Theme::light(), &mut map);
        let Some(LayoutInfo::Page(layout)) = page.layout_info() else {
            panic!("a page alone says it stands on a page");
        };
        assert!(layout.max_content_width.value > 0.0);

        let mut page = GuidedPage::new(Text::new("A page")).embedded();
        let _ = page.build_layout(&mut taffy, &Theme::light(), &mut map);
        assert_eq!(
            page.layout_info(),
            None,
            "a page in a surface draws no column of its own"
        );
    }
}
