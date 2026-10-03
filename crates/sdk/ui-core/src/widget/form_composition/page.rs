//! Page ownership: viewport, gutters, readable measure and vertical scrolling.
use super::{
    Affine, AutoCommit, Effective, Element, Form, HashMap, LayoutInfo, LayoutOrigin, NodeId, ObjId,
    PageLayout, Parts, Point, PresenceCtx, Rect, Scene, Scrollable, TaffyTree, Text, TextRenderer,
    TextRole, Theme, Widget, auto, column_style, fr, length, minmax, percent, retained_parts,
    spacing,
};

/// A form page with a compact title/description, one gutter and vertical scroll.
/// `with_max_content_width(None)` resets only the readable-width override.
pub struct FormPage {
    parts: Parts,
    effective: std::rc::Rc<std::cell::Cell<Option<PageLayout>>>,
}
impl FormPage {
    pub fn new(title: impl Into<String>, form: Form) -> Self {
        let effective = std::rc::Rc::new(std::cell::Cell::new(None));
        let body = PageBody {
            parts: Parts::new(vec![
                Box::new(
                    Text::new(&title.into())
                        .with_role(TextRole::Heading)
                        .with_heading_level(1),
                ),
                Box::new(form),
            ]),
            max_width: None,
            effective: effective.clone(),
            has_description: false,
        };
        Self {
            parts: Parts::new(vec![Box::new(
                Scrollable::new_vertical().with_child(Box::new(body)),
            )]),
            effective,
        }
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
    #[must_use]
    pub fn with_description(mut self, text: impl Into<String>) -> Self {
        let Some(body) = self.body() else {
            return self;
        };
        let text: Element = Box::new(Text::new(&text.into()).with_role(TextRole::Body));
        if body.has_description {
            if let Some(slot) = body.parts.children.get_mut(1) {
                *slot = text;
            }
        } else {
            body.parts.children.insert(1, text);
            body.has_description = true;
        }
        self
    }
    #[must_use]
    pub fn with_max_content_width(mut self, value: Option<f32>) -> Self {
        if let Some(body) = self.body() {
            body.max_width = spacing(value);
        }
        self
    }
    pub fn effective_layout(&self) -> Option<PageLayout> {
        self.effective.get()
    }
}
struct PageBody {
    parts: Parts,
    max_width: Option<f32>,
    effective: std::rc::Rc<std::cell::Cell<Option<PageLayout>>>,
    has_description: bool,
}

impl Widget for FormPage {
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn layout_info(&self) -> Option<LayoutInfo> {
        self.effective_layout().map(LayoutInfo::Page)
    }
    retained_parts!("FormPage");
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut style = column_style(0.0);
        style.size.height = percent(1.0_f32);
        style.grid_template_rows = vec![minmax(length(0.0_f32), fr(1.0_f32))];
        let node = self.parts.build(style, tree, theme, map);
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
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.content,
            None,
            &self.parts.rect,
        );
        self.parts.draw(scene, transform, text, theme, presence);
    }
}
impl Widget for PageBody {
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    retained_parts!("FormPageContent");
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let gutter = theme.tokens.spacing.s6;
        let width = self.max_width.map_or_else(
            || Effective::new(720.0 * theme.ui_scale, LayoutOrigin::Pattern),
            |value| Effective::new(value, LayoutOrigin::Page),
        );
        self.effective.set(Some(PageLayout {
            max_content_width: width,
            gutter: Effective::new(gutter, LayoutOrigin::Theme),
        }));
        let mut style = column_style(theme.tokens.spacing.s2);
        style.max_size.width = length(width.value + gutter * 2.0);
        style.padding = taffy::Rect {
            left: length(gutter),
            right: length(gutter),
            top: length(gutter),
            bottom: length(gutter),
        };
        style.margin.left = auto();
        style.margin.right = auto();
        let node = self.parts.build(style, tree, theme, map);
        // A section-strength gap after the compact heading/description group.
        if let Some(&form) = self.parts.nodes.last()
            && let Ok(current) = tree.style(form)
        {
            let mut style = current.clone();
            style.margin.top = length(theme.tokens.spacing.s6);
            let _ = tree.set_style(form, style);
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
