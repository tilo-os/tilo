//! Pages: a readable column on the window's leading edge that scrolls, with
//! an optional footer of actions pinned under it.
//!
//! [`Page`] holds sections of facts, lists and settings; [`FormPage`] holds
//! one [`Form`] under a title. Both bound their content to the readable
//! width (`metrics.readable_width`) and align it to the leading edge of the
//! column they fill, so a wide window leaves its extra room beside the
//! content instead of around it. Neither paints a ground: the window's
//! region does.
use std::cell::Cell;
use std::rc::Rc;

use super::{
    Actions, Affine, Effective, Element, Form, HashMap, LayoutInfo, LayoutOrigin, NodeId, ObjId,
    PageLayout, Parts, Point, PresenceCtx, Rect, Scene, Scrollable, TaffyTree, Text, TextRenderer,
    TextRole, Theme, Widget, column_style, fr, length, minmax, percent, spacing,
};
use crate::tokens::Rhythm;

/// The retained-child lifecycle over the `Parts` at `self.<path>`: every
/// method but identity, layout building and drawing.
macro_rules! lifecycle {
    ($($path:ident).+) => {
        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            self.$($path).+.apply_layout(taffy, node, origin);
        }
        fn start_operation(
            &mut self,
            op: &crate::widget::ResourceOperation<'_>,
            doc: &mut crate::widget::Edits<'_>,
            path: &ObjId,
        ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
            crate::widget::start_in_children(self, op, doc, path)
        }
        fn update(
            &mut self,
            state: &crate::widget::AppModel,
            path: &crate::widget::ModelPath,
        ) -> bool {
            let mut changed = false;
            for child in &mut self.$($path).+.children {
                changed |= child.update(state, path);
            }
            changed
        }
        fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
            for child in &mut self.$($path).+.children {
                child.sync(state, path);
            }
        }
        fn handle_event(
            &mut self,
            event: &winit::event::WindowEvent,
            ctx: &mut crate::widget::EventCtx<'_>,
        ) {
            let parts = &mut self.$($path).+;
            for (child, &node) in parts.children.iter_mut().zip(&parts.nodes) {
                let path = ctx.path;
                child.handle_event(event, &mut ctx.reborrow(path, node));
                if *ctx.handled {
                    break;
                }
            }
        }
        fn children(&self) -> Vec<&dyn Widget> {
            self.$($path).+.children.iter().map(|child| child.as_ref()).collect()
        }
        fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
            for child in &self.$($path).+.children {
                visitor(child.as_ref());
            }
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            self.$($path).+
                .children
                .iter_mut()
                .map(|child| child.as_mut() as &mut (dyn Widget + '_))
                .collect()
        }
        fn mark_drawn(&mut self) {
            for child in &mut self.$($path).+.children {
                child.mark_drawn();
            }
        }
        fn take_structure_dirty(&mut self) -> bool {
            let mut dirty = false;
            for child in &mut self.$($path).+.children {
                dirty |= child.take_structure_dirty();
            }
            dirty
        }
        fn wants_pointer_capture(&self) -> bool {
            self.$($path).+
                .children
                .iter()
                .any(|child| child.wants_pointer_capture())
        }
        fn get_rect(&self) -> Rect {
            self.$($path).+.rect
        }
    };
}

/// How far an item of a page's body sits below the one before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lead {
    /// The first item.
    None,
    /// The step between related items (a title and its description).
    Related,
    /// Between head items.
    Section,
    /// Between sections, and between the head and the first section.
    Group,
}

/// The readable column itself: the gutter on every side, at most the
/// readable width wide, on the leading edge.
struct PageBody {
    parts: Parts,
    leads: Vec<Lead>,
    name: &'static str,
    max_width: Option<f32>,
    /// A page of objects (a dashboard of cards) takes the whole column.
    full_width: bool,
    effective: Rc<Cell<Option<PageLayout>>>,
}

impl PageBody {
    fn new(name: &'static str, effective: Rc<Cell<Option<PageLayout>>>) -> Self {
        Self {
            parts: Parts::new(Vec::new()),
            leads: Vec::new(),
            name,
            max_width: None,
            full_width: false,
            effective,
        }
    }

    fn push(&mut self, item: Element, lead: Lead) {
        self.parts.children.push(item);
        self.leads.push(lead);
    }

    fn insert(&mut self, index: usize, item: Element, lead: Lead) {
        let index = index.min(self.parts.children.len());
        self.parts.children.insert(index, item);
        self.leads.insert(index, lead);
    }

    fn readable_measure(&self, theme: &Theme) -> (Effective<f32>, f32) {
        let gutter = theme.tokens.spacing.rhythm.group;
        let width = self.max_width.map_or_else(
            || Effective::new(theme.tokens.metrics.readable_width, LayoutOrigin::Pattern),
            |value| Effective::new(value, LayoutOrigin::Page),
        );
        (width, gutter)
    }
}

impl Widget for PageBody {
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    lifecycle!(parts);
    fn name(&self) -> &'static str {
        self.name
    }
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let (width, gutter) = self.readable_measure(theme);
        self.effective.set(Some(PageLayout {
            max_content_width: width,
            gutter: Effective::new(gutter, LayoutOrigin::Theme),
        }));
        let mut style = column_style(0.0);
        if !self.full_width {
            style.max_size.width = length(width.value + gutter * 2.0);
        }
        style.padding = taffy::Rect {
            left: length(gutter),
            right: length(gutter),
            top: length(gutter),
            bottom: length(gutter),
        };
        let node = self.parts.build(style, tree, theme, map);
        let rhythm = theme.tokens.spacing.rhythm;
        for (&child, &lead) in self.parts.nodes.iter().zip(&self.leads) {
            let gap = match lead {
                Lead::None => 0.0,
                Lead::Related => rhythm.get(Rhythm::Related),
                Lead::Section => rhythm.get(Rhythm::Section),
                Lead::Group => rhythm.get(Rhythm::Group),
            };
            if let Ok(current) = tree.style(child) {
                let mut style = current.clone();
                style.margin.top = length(gap);
                let _ = tree.set_style(child, style);
            }
        }
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.parts.draw(scene, transform, text, theme, presence);
    }
}

/// Actions pinned under a page's scroller: the same measure and gutter as
/// the column above, a hairline between them.
struct PinnedActions {
    parts: Parts,
    gutter_probe: Rc<Cell<f32>>,
    gutter: f32,
}

impl Widget for PinnedActions {
    lifecycle!(parts);
    fn name(&self) -> &'static str {
        "PinnedActions"
    }
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let rhythm = theme.tokens.spacing.rhythm;
        self.gutter = rhythm.group;
        let readable = theme.tokens.metrics.readable_width;
        let mut style = column_style(0.0);
        style.padding = taffy::Rect {
            left: length(self.gutter),
            right: length(self.gutter + self.gutter_probe.get()),
            top: length(rhythm.section),
            bottom: length(rhythm.section),
        };
        let node = self.parts.build(style, tree, theme, map);
        // The actions end where the readable column above ends.
        if let Some(&actions) = self.parts.nodes.first()
            && let Ok(current) = tree.style(actions)
        {
            let mut style = current.clone();
            style.size.width = percent(1.0_f32);
            style.max_size.width = length(readable);
            let _ = tree.set_style(actions, style);
        }
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn adjust_layout_style(
        &mut self,
        tree: &mut TaffyTree<()>,
        node: NodeId,
        _text: &mut crate::text_render::TextRenderer,
    ) -> bool {
        let right = length(self.gutter + self.gutter_probe.get());
        match tree.style(node) {
            Ok(current) if current.padding.right != right => {
                let mut style = current.clone();
                style.padding.right = right;
                tree.set_style(node, style).is_ok()
            }
            _ => false,
        }
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let rect = self.parts.rect;
        crate::surface::separator(
            scene,
            transform,
            rect.x0,
            rect.x1,
            rect.y0 + 0.5,
            0.0,
            theme.tokens.colors.surfaces.content,
        );
        self.parts.draw(scene, transform, text, theme, presence);
    }
}

/// The scroller with its body, and the pinned footer under it.
struct Frame {
    parts: Parts,
    gutter_probe: Rc<Cell<f32>>,
    has_footer: bool,
}

impl Frame {
    fn new(body: PageBody, reveal_focus: bool) -> Self {
        let gutter_probe = Rc::new(Cell::new(0.0));
        let scroller = Scrollable::new_vertical()
            .with_gutter_probe(Rc::clone(&gutter_probe))
            .with_child(Box::new(body));
        let scroller = if reveal_focus {
            scroller.with_focus_reveal()
        } else {
            scroller
        };
        Self {
            parts: Parts::new(vec![Box::new(scroller)]),
            gutter_probe,
            has_footer: false,
        }
    }

    fn scroller(&mut self) -> Option<&mut Scrollable> {
        self.parts
            .children
            .first_mut()?
            .as_any_mut()?
            .downcast_mut::<Scrollable>()
    }

    fn body(&mut self) -> Option<&mut PageBody> {
        self.parts
            .children
            .first_mut()?
            .children_mut()
            .into_iter()
            .next()?
            .as_any_mut()?
            .downcast_mut::<PageBody>()
    }

    fn pin(&mut self, footer: Element) {
        let footer: Element = Box::new(PinnedActions {
            parts: Parts::new(vec![footer]),
            gutter_probe: Rc::clone(&self.gutter_probe),
            gutter: 0.0,
        });
        if self.has_footer {
            if let Some(slot) = self.parts.children.get_mut(1) {
                *slot = footer;
            }
        } else {
            self.parts.children.push(footer);
            self.has_footer = true;
        }
    }

    fn build(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut style = column_style(0.0);
        style.size.height = percent(1.0_f32);
        style.grid_template_rows = vec![minmax(length(0.0_f32), fr(1.0_f32))];
        style.grid_auto_rows = vec![taffy::style_helpers::auto()];
        style.min_size.height = length(0.0_f32);
        self.parts.build(style, tree, theme, map)
    }
}

/// A page of sections: optional head items (a summary, a filter), then
/// sections spaced `rhythm.group` apart, on a readable column aligned to
/// the leading edge of the column it fills, scrolling vertically. It has
/// no title of its own: the shell's header titles the page. Actions that
/// must stay reachable while it scrolls are pinned under it with
/// [`Page::with_pinned_actions`].
pub struct Page {
    frame: Frame,
    effective: Rc<Cell<Option<PageLayout>>>,
    sections: usize,
}

impl Default for Page {
    fn default() -> Self {
        Self::new()
    }
}

impl Page {
    pub fn new() -> Self {
        let effective = Rc::new(Cell::new(None));
        Self {
            frame: Frame::new(PageBody::new("PageContent", Rc::clone(&effective)), false),
            effective,
            sections: 0,
        }
    }
    /// An item above the sections (a summary, a filter, a banner). Head
    /// items sit `rhythm.section` apart and a group step above the first
    /// section.
    #[must_use]
    pub fn head(mut self, item: Element) -> Self {
        if let Some(body) = self.frame.body() {
            let heads = body.parts.children.len() - self.sections;
            let lead = if heads == 0 {
                Lead::None
            } else {
                Lead::Section
            };
            body.insert(heads, item, lead);
            if self.sections > 0
                && let Some(first_section) = body.leads.get_mut(heads + 1)
            {
                *first_section = Lead::Group;
            }
        }
        self
    }
    /// The next section: a `Section`, a `FormSection`, a `Form`, a
    /// `PropertyList`, a `CardGrid`.
    #[must_use]
    pub fn section(mut self, section: Element) -> Self {
        if let Some(body) = self.frame.body() {
            let lead = if body.parts.children.is_empty() {
                Lead::None
            } else {
                Lead::Group
            };
            body.push(section, lead);
            self.sections += 1;
        }
        self
    }
    /// Keep the scroll position in `cell` across rebuilds, restored and
    /// clamped to the content on the first layout.
    #[must_use]
    pub fn with_persisted_offset(mut self, cell: Rc<Cell<(f32, f32)>>) -> Self {
        if let Some(scroller) = self.frame.scroller() {
            scroller.set_persisted_offset(cell);
        }
        self
    }
    /// Pin `actions` under the scroller, on the page's measure and gutter
    /// with a hairline above them, so the page's commit stays in reach.
    #[must_use]
    pub fn with_pinned_actions(mut self, actions: Actions) -> Self {
        self.frame.pin(Box::new(actions));
        self
    }
    /// [`Self::with_pinned_actions`] for a footer that says more than its
    /// actions: the outcome of the last operation above the buttons that
    /// start one (a `ModelView` of a status and an `Actions`).
    #[must_use]
    pub fn with_pinned(mut self, footer: Element) -> Self {
        self.frame.pin(footer);
        self
    }
    /// Give the page's scroll group a stable semantic key, so the controls
    /// on it keep durable identities (`Scrollable::with_semantic_key`).
    #[must_use]
    pub fn with_semantic_key(mut self, key: impl Into<String>) -> Self {
        if let Some(scroller) = self.frame.scroller() {
            scroller.set_semantic_key(key);
        }
        self
    }
    /// Seed the pointer position when the page is rebuilt every frame
    /// (`Scrollable::with_cursor`).
    #[must_use]
    pub fn with_cursor(mut self, cursor: Point) -> Self {
        if let Some(scroller) = self.frame.scroller() {
            scroller.set_cursor(cursor);
        }
        self
    }
    /// Override the readable width; `None` restores `metrics.readable_width`.
    #[must_use]
    pub fn with_max_content_width(mut self, value: Option<f32>) -> Self {
        if let Some(body) = self.frame.body() {
            body.max_width = spacing(value);
        }
        self
    }
    /// A page of objects (a dashboard of cards, a grid of media) that
    /// reflows across the whole column instead of keeping the readable
    /// width; the gutter stays.
    #[must_use]
    pub fn full_width(mut self) -> Self {
        if let Some(body) = self.frame.body() {
            body.full_width = true;
        }
        self
    }
    pub fn effective_layout(&self) -> Option<PageLayout> {
        self.effective.get()
    }
}

/// A form under its title, on a page: the title in `LargeTitle` (or none,
/// [`FormPage::untitled`]) with an optional header action, an optional
/// description, then the form, on the readable column aligned to the
/// leading edge, scrolling vertically, with optional pinned actions.
/// `with_max_content_width(None)` resets only the readable-width override.
pub struct FormPage {
    frame: Frame,
    effective: Rc<Cell<Option<PageLayout>>>,
    title: Option<String>,
    has_description: bool,
}

impl FormPage {
    pub fn new(title: impl Into<String>, form: Form) -> Self {
        let title = title.into();
        let mut page = Self::untitled(form);
        if let Some(body) = page.frame.body() {
            body.insert(0, title_text(&title), Lead::None);
            if let Some(lead) = body.leads.get_mut(1) {
                *lead = Lead::Group;
            }
        }
        page.title = Some(title);
        page
    }
    /// The form alone, for a page the shell's header already titles.
    pub fn untitled(form: Form) -> Self {
        let effective = Rc::new(Cell::new(None));
        let mut body = PageBody::new("FormPageContent", Rc::clone(&effective));
        body.push(Box::new(form), Lead::None);
        Self {
            // Keyboard and assistive-technology focus must reveal a form field
            // even when its measured row starts beyond the current viewport.
            frame: Frame::new(body, true),
            effective,
            title: None,
            has_description: false,
        }
    }
    /// One action beside the title ("Add account", "Reset"), on its
    /// trailing edge.
    #[must_use]
    pub fn with_header_action(mut self, action: super::super::Button) -> Self {
        let Some(title) = self.title.clone() else {
            return self;
        };
        if let Some(body) = self.frame.body()
            && let Some(slot) = body.parts.children.first_mut()
        {
            *slot = Box::new(
                super::super::Row::new()
                    .no_wrap()
                    .with_justify(taffy::JustifyContent::SpaceBetween)
                    .with_rhythm(Rhythm::Related)
                    .push(title_text(&title))
                    .push(Box::new(action)),
            );
        }
        self
    }
    #[must_use]
    pub fn with_description(mut self, text: impl Into<String>) -> Self {
        let has_title = self.title.is_some();
        let has_description = self.has_description;
        let Some(body) = self.frame.body() else {
            return self;
        };
        let text: Element = Box::new(Text::new(&text.into()).with_role(TextRole::Body));
        let index = usize::from(has_title);
        if has_description {
            if let Some(slot) = body.parts.children.get_mut(index) {
                *slot = text;
            }
        } else {
            let lead = if has_title { Lead::Related } else { Lead::None };
            body.insert(index, text, lead);
            if let Some(next) = body.leads.get_mut(index + 1) {
                *next = Lead::Group;
            }
            self.has_description = true;
        }
        self
    }
    #[must_use]
    pub fn with_max_content_width(mut self, value: Option<f32>) -> Self {
        if let Some(body) = self.frame.body() {
            body.max_width = spacing(value);
        }
        self
    }
    /// Keep the scroll position in `cell` across rebuilds, restored and
    /// clamped to the content on the first layout.
    #[must_use]
    pub fn with_persisted_offset(mut self, cell: Rc<Cell<(f32, f32)>>) -> Self {
        if let Some(scroller) = self.frame.scroller() {
            scroller.set_persisted_offset(cell);
        }
        self
    }
    /// Give the page's scroll group a stable semantic key, so the fields
    /// keep durable identities (`Scrollable::with_semantic_key`).
    #[must_use]
    pub fn with_semantic_key(mut self, key: impl Into<String>) -> Self {
        if let Some(scroller) = self.frame.scroller() {
            scroller.set_semantic_key(key);
        }
        self
    }
    /// Pin `actions` under the scroller ("Save", "Cancel"), on the page's
    /// measure and gutter with a hairline above them, instead of at the end
    /// of a long form.
    #[must_use]
    pub fn with_pinned_actions(mut self, actions: Actions) -> Self {
        self.frame.pin(Box::new(actions));
        self
    }
    /// [`Self::with_pinned_actions`] for a footer that says more than its
    /// actions: the outcome of the last operation above the buttons that
    /// start one (a `ModelView` of a status and an `Actions`).
    #[must_use]
    pub fn with_pinned(mut self, footer: Element) -> Self {
        self.frame.pin(footer);
        self
    }
    pub fn effective_layout(&self) -> Option<PageLayout> {
        self.effective.get()
    }
}

fn title_text(title: &str) -> Element {
    Box::new(
        Text::new(title)
            .with_role(TextRole::LargeTitle)
            .with_heading_level(1),
    )
}

macro_rules! framed_page {
    ($ty:ty, $name:literal) => {
        impl Widget for $ty {
            fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
                Some(self)
            }
            fn layout_info(&self) -> Option<LayoutInfo> {
                self.effective_layout().map(LayoutInfo::Page)
            }
            lifecycle!(frame.parts);
            fn name(&self) -> &'static str {
                $name
            }
            fn build_layout(
                &mut self,
                tree: &mut TaffyTree<()>,
                theme: &Theme,
                map: &mut HashMap<NodeId, *mut dyn Widget>,
            ) -> NodeId {
                let node = self.frame.build(tree, theme, map);
                map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
                node
            }
            fn draw(
                &mut self,
                scene: &mut Scene,
                transform: Affine,
                text: &mut TextRenderer,
                theme: &Theme,
                presence: &PresenceCtx,
            ) {
                self.frame
                    .parts
                    .draw(scene, transform, text, theme, presence);
            }
        }
    };
}

framed_page!(Page, "Page");
framed_page!(FormPage, "FormPage");
