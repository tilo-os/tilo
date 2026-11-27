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
use taffy::{AlignContent, AlignItems, NodeId, Style, TaffyTree};
use vello::kurbo::{Point, Rect};

use tilo_ui_base::component::{Component, Described, Event, PartViews};
use tilo_ui_base::draw::DrawCtx;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::{Rhythm, TextRole};
use tilo_ui_base::widget::layout::insets_all;
use tilo_ui_base::widget::{
    DrawChild, Effective, Element, LayoutInfo, LayoutOrigin, PageLayout, StructureChange,
    TaffyTreeExt, Widget,
};

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
    /// Stand alone on a window's ground, on the readable column and the page's own gutter, rather
    /// than in a surface that already measures it (a card's body).
    pub standalone: bool,
}

/// See the module docs.
pub struct GuidedPage {
    header: Option<Element>,
    scroll: Scrollable,
    notice: Option<Element>,
    actions: Option<Element>,
    /// Stand alone on a window's ground, on the readable column, rather than in a surface that
    /// already measures it.
    standalone: bool,
    /// Take the height the surface leaves.
    fills_height: bool,
    header_node: Option<NodeId>,
    scroll_node: Option<NodeId>,
    notice_node: Option<NodeId>,
    actions_node: Option<NodeId>,
    rect: Rect,
    page: Option<PageLayout>,
    structure: StructureChange,
    /// The part a host is changing.
    editing: Option<Part>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    Body,
    Notice,
    Actions,
}

/// The rows a page lays its parts out in: each part its own row, the page a row that takes the
/// room left when the surface holds a height and its content's height when it does not, and a
/// group's space between them.
fn stack_rows(rows: Vec<taffy::GridTemplateComponent<String>>, gap: f32) -> Style {
    Style {
        display: taffy::Display::Grid,
        grid_template_columns: vec![minmax(length(0.0_f32), fr(1.0_f32))],
        grid_template_rows: rows,
        grid_auto_rows: vec![auto()],
        align_content: Some(AlignContent::Start),
        gap: taffy::Size {
            width: length(0.0_f32),
            height: length(gap),
        },
        flex_shrink: 0.0,
        ..Default::default()
    }
}

impl GuidedPage {
    /// A page alone on a window's ground, showing `body`, on the readable column.
    pub fn new(body: impl Widget + 'static) -> Self {
        Self::of(Some(Box::new(body)), true, true)
    }

    /// A page in a surface that already measures it (a card's body): it takes the room it is
    /// offered and draws no readable column or gutter of its own.
    #[must_use]
    pub fn embedded(mut self) -> Self {
        self.standalone = false;
        self.rebuild_scroll();
        self
    }

    /// Take the height the surface leaves, so a long page scrolls inside it. A card that is as
    /// tall as its content leaves this off.
    #[must_use]
    pub fn fills_height(mut self) -> Self {
        self.fills_height = true;
        self.rebuild_scroll();
        self
    }

    /// Where the person is, as named stations.
    #[must_use]
    pub fn stepper(mut self, stepper: Stepper) -> Self {
        self.header = Some(Box::new(stepper));
        self
    }

    /// Where the person is, as a track: what is being set up, which step of how many, how far
    /// along, and what a reader hears it say.
    #[must_use]
    pub fn stage_track(mut self, stage: &str, position: &str, progress: f32, name: &str) -> Self {
        let name = (!name.is_empty()).then_some(name);
        self.header = Some(stage_track(Some(stage), Some(position), progress, name));
        self
    }

    /// Keep the page's scroll position in `offset` across rebuilds.
    #[must_use]
    pub fn offset(mut self, offset: Rc<Cell<(f32, f32)>>) -> Self {
        self.scroll = self.scroll.with_persisted_offset(offset);
        self
    }

    /// Seed the pointer position when the page is rebuilt every frame.
    #[must_use]
    pub fn cursor(mut self, cursor: Point) -> Self {
        self.scroll = self.scroll.with_cursor(cursor);
        self
    }

    /// What needs attention, above the ways forward and outside the scroll.
    #[must_use]
    pub fn notice(mut self, notice: impl Widget + 'static) -> Self {
        self.notice = Some(Box::new(notice));
        self
    }

    /// [`Self::notice`] for a notice the caller may have none of.
    #[must_use]
    pub fn maybe_notice(mut self, notice: Option<Box<dyn Widget>>) -> Self {
        self.notice = notice;
        self
    }

    /// The ways forward, under the page and outside the scroll.
    #[must_use]
    pub fn actions(mut self, actions: impl Widget + 'static) -> Self {
        self.actions = Some(Box::new(actions));
        self
    }

    /// Centre a short page in the room under its header: a page of explanation on a tall window,
    /// rather than one that floats at the top.
    #[must_use]
    pub fn center_short_body(mut self) -> Self {
        self.scroll = center(self.scroll, true);
        self
    }

    /// Box this widget in the SDK so thin apps reuse its shared implementation.
    #[must_use]
    pub fn build(self) -> Box<dyn Widget> {
        Box::new(self)
    }

    fn of(body: Option<Element>, standalone: bool, fills_height: bool) -> Self {
        let scroll = page_scroll(Scroll {
            fills: standalone || fills_height,
            shows_bar: true,
            reveals_focus: true,
            label: None,
            center: false,
        });
        Self {
            header: None,
            scroll: put(scroll, body),
            notice: None,
            actions: None,
            standalone,
            fills_height,
            header_node: None,
            scroll_node: None,
            notice_node: None,
            actions_node: None,
            rect: Rect::ZERO,
            page: None,
            structure: StructureChange::None,
            editing: None,
        }
    }

    /// Rebuild the scroll with the current flags, keeping its page.
    fn rebuild_scroll(&mut self) {
        let body = self
            .scroll
            .child_mut()
            .map(|body| std::mem::replace(body, Box::new(Spacer::new()) as Element));
        let scroll = page_scroll(Scroll {
            fills: self.standalone || self.fills_height,
            shows_bar: true,
            reveals_focus: true,
            label: None,
            center: false,
        });
        self.scroll = put(scroll, body);
    }

    fn described(&mut self) {
        self.structure = self.structure.join(StructureChange::Described);
    }

    /// The room a page alone on a window stands in: the readable column, centred, a group's
    /// gutter round it.
    fn page_style(theme: &Theme, rows: Vec<taffy::GridTemplateComponent<String>>) -> Style {
        let group = theme.tokens.spacing.rhythm.get(Rhythm::Group);
        Style {
            padding: insets_all(group),
            size: taffy::Size {
                width: percent(1.0_f32),
                height: percent(1.0_f32),
            },
            max_size: taffy::Size {
                width: length(theme.tokens.metrics.readable_width + 2.0 * group),
                height: auto(),
            },
            margin: taffy::Rect {
                left: auto(),
                right: auto(),
                top: length(0.0_f32),
                bottom: length(0.0_f32),
            },
            ..stack_rows(rows, group)
        }
    }

    /// The room a page in a surface takes: the width it is offered, and the height when the
    /// surface holds it to one.
    fn embedded_style(
        &self,
        theme: &Theme,
        rows: Vec<taffy::GridTemplateComponent<String>>,
    ) -> Style {
        let group = theme.tokens.spacing.rhythm.get(Rhythm::Group);
        Style {
            size: taffy::Size {
                width: percent(1.0_f32),
                height: if self.fills_height {
                    percent(1.0_f32)
                } else {
                    auto()
                },
            },
            min_size: taffy::Size {
                width: length(0.0_f32),
                height: auto(),
            },
            ..stack_rows(rows, group)
        }
    }
}

/// What a page's scroll is built with: its bar, focus reveal, label and whether it takes the
/// room left under the header.
#[derive(Clone, Default)]
struct Scroll {
    fills: bool,
    shows_bar: bool,
    reveals_focus: bool,
    label: Option<String>,
    center: bool,
}

fn page_scroll(options: Scroll) -> Scrollable {
    let mut scroll = Scrollable::new_vertical();
    if options.shows_bar {
        // The page under the track is one of several, each a step of the same account: the bar's
        // gutter stays kept whichever step fits and whichever overflows, so a control stands
        // where it stood on the last page.
        scroll = scroll.always_visible().with_stable_vertical_gutter();
    }
    if options.reveals_focus {
        scroll = scroll.with_focus_reveal();
    }
    if let Some(label) = options.label {
        scroll = scroll.with_label(label);
    }
    if options.fills {
        scroll = scroll.fill_parent_height();
    }
    center(scroll, options.center)
}

/// Wrap a short page so it fills the viewport and stands centred in the room, rather than at the
/// top of a tall window.
fn center(mut scroll: Scrollable, center: bool) -> Scrollable {
    if !center {
        return scroll;
    }
    // The page stands in a row that fills the viewport, centred in it. The row goes in before
    // the scroll is told its content fills the viewport, so that wrapper is the one round it.
    let body = scroll
        .child_mut()
        .map(|body| std::mem::replace(body, Box::new(Spacer::new()) as Element));
    let Some(body) = body else {
        return scroll;
    };
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

fn put(mut scroll: Scrollable, body: Option<Element>) -> Scrollable {
    if let Some(body) = body {
        scroll = scroll.with_child(body);
    }
    scroll
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

impl Described for GuidedPage {
    type Props = GuidedPageProps;

    fn from_props(_key: &str, props: GuidedPageProps) -> Self {
        let GuidedPageProps {
            steps,
            current,
            station,
            stage,
            position,
            progress,
            progress_name,
            body,
            notice,
            actions,
            shows_bar,
            reveals_focus,
            scroll_label,
            fills_height,
            standalone,
        } = props;
        let header = if steps.is_empty() {
            progress.map(|progress| {
                stage_track(
                    stage.as_deref(),
                    position.as_deref(),
                    tilo_ui_base::num::f32_of_f64(progress),
                    progress_name.as_deref(),
                )
            })
        } else {
            let mut stepper =
                Stepper::new(steps).current(current.and_then(|index| usize::try_from(index).ok()));
            if !station.is_none() {
                stepper = stepper.on_station(move |index| {
                    station.fire(Some(tilo_ui_base::component::Value::Int(
                        i64::try_from(index).unwrap_or(i64::MAX),
                    )));
                });
            }
            Some(Box::new(stepper) as Element)
        };
        let mut scroll = page_scroll(Scroll {
            fills: standalone || fills_height,
            shows_bar,
            reveals_focus,
            label: scroll_label,
            center: false,
        });
        if let Some(body) = body {
            scroll = scroll.with_child(body);
        }
        Self {
            header,
            scroll,
            notice,
            actions,
            standalone,
            fills_height,
            header_node: None,
            scroll_node: None,
            notice_node: None,
            actions_node: None,
            rect: Rect::ZERO,
            page: None,
            structure: StructureChange::None,
            editing: None,
        }
    }

    /// What it takes is set when it is built: where the person is, how tall the page stands and
    /// how it scrolls are the surface's, and a change of them builds it again. What its parts
    /// hold changes where they stand.
    fn set_props(&mut self, _props: GuidedPageProps) -> bool {
        false
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

/// A part that holds one view or none.
fn slot(held: &mut Option<Element>, index: usize) -> Option<&mut Element> {
    (index == 0).then_some(held).and_then(Option::as_mut)
}

impl PartViews for GuidedPage {
    fn len(&self) -> usize {
        match self.editing {
            Some(Part::Body) => <Scrollable as PartViews>::len(&self.scroll),
            Some(Part::Notice) => usize::from(self.notice.is_some()),
            Some(Part::Actions) => usize::from(self.actions.is_some()),
            None => 0,
        }
    }

    fn view_mut(&mut self, index: usize) -> Option<&mut Element> {
        match self.editing {
            Some(Part::Body) => {
                <Scrollable as Described>::part_views(&mut self.scroll, "content")?.view_mut(index)
            }
            Some(Part::Notice) => slot(&mut self.notice, index),
            Some(Part::Actions) => slot(&mut self.actions, index),
            None => None,
        }
    }

    fn insert(&mut self, index: usize, view: Element) {
        match self.editing {
            Some(Part::Body) => {
                if let Some(body) =
                    <Scrollable as Described>::part_views(&mut self.scroll, "content")
                {
                    body.insert(index, view);
                }
            }
            Some(Part::Notice) => self.notice = Some(view),
            Some(Part::Actions) => self.actions = Some(view),
            None => return,
        }
        self.described();
    }

    fn remove(&mut self, index: usize) {
        match self.editing {
            Some(Part::Body) => {
                if let Some(body) =
                    <Scrollable as Described>::part_views(&mut self.scroll, "content")
                {
                    body.remove(index);
                }
            }
            Some(Part::Notice) => self.notice = None,
            Some(Part::Actions) => self.actions = None,
            None => return,
        }
        self.described();
    }

    fn move_view(&mut self, from: usize, to: usize) {
        if self.editing == Some(Part::Body)
            && let Some(body) = <Scrollable as Described>::part_views(&mut self.scroll, "content")
        {
            body.move_view(from, to);
            self.described();
        }
    }

    fn replace(&mut self, index: usize, view: Element) {
        match self.editing {
            Some(Part::Body) => {
                if let Some(body) =
                    <Scrollable as Described>::part_views(&mut self.scroll, "content")
                {
                    body.replace(index, view);
                }
            }
            Some(Part::Notice) => self.notice = Some(view),
            Some(Part::Actions) => self.actions = Some(view),
            None => return,
        }
        self.described();
    }
}

impl Widget for GuidedPage {
    tilo_ui_base::described!();

    fn name(&self) -> &'static str {
        "GuidedPage"
    }

    fn get_rect(&self) -> Rect {
        self.rect
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
        self.header_node = self
            .header
            .as_mut()
            .map(|header| header.build_layout(taffy, theme, map));
        self.scroll_node = Some(self.scroll.build_layout(taffy, theme, map));
        self.notice_node = self
            .notice
            .as_mut()
            .map(|notice| notice.build_layout(taffy, theme, map));
        self.actions_node = self
            .actions
            .as_mut()
            .map(|actions| actions.build_layout(taffy, theme, map));
        // Each part is a row of its own, the page the one that takes the room left: the
        // header, the page, what needs attention, the ways forward.
        let mut nodes = Vec::with_capacity(4);
        let mut rows: Vec<taffy::GridTemplateComponent<String>> = Vec::with_capacity(4);
        for (node, page) in [
            (self.header_node, false),
            (self.scroll_node, true),
            (self.notice_node, false),
            (self.actions_node, false),
        ] {
            let Some(node) = node else {
                continue;
            };
            nodes.push(node);
            rows.push(if page {
                minmax(length(0.0_f32), fr(1.0_f32))
            } else {
                auto()
            });
        }
        let style = if self.standalone {
            Self::page_style(theme, rows)
        } else {
            self.embedded_style(theme, rows)
        };
        self.page = self.standalone.then(|| PageLayout {
            max_content_width: Effective::new(
                theme.tokens.metrics.readable_width,
                LayoutOrigin::Pattern,
            ),
            gutter: Effective::new(group, LayoutOrigin::Theme),
        });
        let node = taffy.parent_node(style, &nodes);
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        let at = self.rect.origin();
        if let (Some(header), Some(header_node)) = (&mut self.header, self.header_node) {
            header.apply_layout(taffy, header_node, at);
        }
        if let Some(scroll_node) = self.scroll_node {
            self.scroll.apply_layout(taffy, scroll_node, at);
        }
        if let (Some(notice), Some(notice_node)) = (&mut self.notice, self.notice_node) {
            notice.apply_layout(taffy, notice_node, at);
        }
        if let (Some(actions), Some(actions_node)) = (&mut self.actions, self.actions_node) {
            actions.apply_layout(taffy, actions_node, at);
        }
    }

    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        if let Some(header) = &mut self.header {
            header.draw_child(scene, transform, text, theme, &env);
        }
        self.scroll.draw_child(scene, transform, text, theme, &env);
        if let Some(notice) = &mut self.notice {
            notice.draw_child(scene, transform, text, theme, &env);
        }
        if let Some(actions) = &mut self.actions {
            actions.draw_child(scene, transform, text, theme, &env);
        }
    }

    fn handle_event(
        &mut self,
        event: &tilo_ui_base::winit::event::WindowEvent,
        ctx: &mut tilo_ui_base::widget::EventCtx<'_>,
    ) {
        if let (Some(header), Some(node)) = (&mut self.header, self.header_node) {
            let path = ctx.path;
            header.handle_event(event, &mut ctx.reborrow(path, node));
        }
        if !*ctx.handled
            && let Some(node) = self.scroll_node
        {
            let path = ctx.path;
            self.scroll
                .handle_event(event, &mut ctx.reborrow(path, node));
        }
        if !*ctx.handled
            && let (Some(notice), Some(node)) = (&mut self.notice, self.notice_node)
        {
            let path = ctx.path;
            notice.handle_event(event, &mut ctx.reborrow(path, node));
        }
        if !*ctx.handled
            && let (Some(actions), Some(node)) = (&mut self.actions, self.actions_node)
        {
            let path = ctx.path;
            actions.handle_event(event, &mut ctx.reborrow(path, node));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = Vec::with_capacity(4);
        if let Some(header) = &self.header {
            children.push(header.as_ref());
        }
        children.push(&self.scroll);
        if let Some(notice) = &self.notice {
            children.push(notice.as_ref());
        }
        if let Some(actions) = &self.actions {
            children.push(actions.as_ref());
        }
        children
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut children: Vec<&mut (dyn Widget + '_)> = Vec::with_capacity(4);
        if let Some(header) = &mut self.header {
            children.push(header.as_mut());
        }
        children.push(&mut self.scroll);
        if let Some(notice) = &mut self.notice {
            children.push(notice.as_mut());
        }
        if let Some(actions) = &mut self.actions {
            children.push(actions.as_mut());
        }
        children
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = std::mem::take(&mut self.structure).rebuilds();
        for child in self.children_mut() {
            dirty |= child.take_structure_dirty();
        }
        dirty
    }

    fn take_structure_change(&mut self) -> StructureChange {
        let mut change = std::mem::take(&mut self.structure);
        for child in self.children_mut() {
            change = change.join(child.take_structure_change());
        }
        change
    }
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
