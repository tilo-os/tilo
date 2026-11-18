//! `Place`: a resource's view on a canvas as content, not in a window (canvas.md §2).
//!
//! A place is its view on the window surface's own ground — rounded, with the surface's soft
//! edge and shadow — and nothing else: no title bar, no border of a window. Its handle (what it
//! is, the resource's actions, pin and close) comes in a band over its top edge, at its trailing
//! end, while the pointer is over it or something in it holds focus, the way a message's toolbar
//! does, and fades away after. The band is kept whether the handle shows or not, so the handle
//! never covers the view's first line and showing it moves nothing. Picking the handle up carries the place's resource on the drag bus, so
//! letting it go over the canvas moves the place there and over a composer attaches it.
//!
//! The look is the theme's: the surface is [`SurfaceLevel::Window`], the handle a floating
//! surface of the hover toolbar's, the title a label in the theme's secondary ink.

use std::cell::RefCell;
use std::rc::Rc;

use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect};
use vello::peniko::{BlendMode, Fill};

use crate::component::{Component, Described};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::{Rhythm, SpacingStep, TextRole};
use crate::widget::arrange::{Align, Fit, Part, Stack};
use crate::widget::primitives::{SurfaceLevel, paint_surface};
use crate::widget::{
    ContextMenuArea, Decorator, DragSource, DrawChild, Element, HoverToolbar, MenuItemProps,
    Spacer, Text, Widget,
};

/// A resource's view on a canvas: the view on the window surface, with a handle shown
/// over its corner on hover and focus that names it, holds its controls and moves it.
#[derive(Component, Default)]
#[component(name = "Place", role = "Group", label = "name")]
pub struct PlaceProps {
    /// What the place shows, as its handle names it and assistive technology reads it.
    pub name: Option<String>,
    /// The resource the place shows, by its published address: what picking the handle up
    /// carries, so letting it go on the canvas moves the place and on a composer attaches it.
    #[component(data)]
    pub carries: Option<String>,
    /// The handle's controls after the name: the resource's actions, pin and close.
    #[component(part)]
    pub controls: Option<Element>,
    /// The view the place shows, filling it.
    #[component(part(default))]
    pub body: Option<Element>,
    /// The resource the place shows, whose published actions its menu lists, which the host
    /// fills as it fills a `ui.MenuButton`'s: a right-click on the place opens them, and typing
    /// there filters them as every context menu does (`canvas.md` §1, "Right-click keeps its
    /// menu"). A right-click on content with a menu of its own (a field, a row) is that
    /// content's.
    pub target: Option<crate::component::ResourceId>,
    /// The place's menu: the commands the host filled from its target.
    #[component(part)]
    pub items: Vec<MenuItemProps>,
}

/// How tall the band a place keeps over its view for its handle stands, in logical pixels: a
/// control's row in a floating surface, a tight step above the view. The canvas stands a place's
/// pin this much over the point it was put at, so the view's own top is at the point.
pub const PLACE_BAND: f64 = crate::tokens::Metrics::BASE.panel_row_height as f64;

/// See the module docs.
pub struct CanvasPlace {
    /// The place's menu around its view and its handle, so a right-click on either opens it.
    menu: ContextMenuArea,
    /// The name its handle shows, and the resource the handle carries: what it was built with.
    name: String,
    carries: String,
    /// The commands its menu offers, as the props last gave them.
    items: Rc<RefCell<Vec<MenuItemProps>>>,
}

fn empty() -> Element {
    Box::new(Spacer::token(SpacingStep::S0))
}

impl Described for CanvasPlace {
    type Props = PlaceProps;

    fn from_props(key: &str, props: PlaceProps) -> Self {
        let PlaceProps {
            name,
            carries,
            controls,
            body,
            target: _,
            items,
        } = props;
        let items = Rc::new(RefCell::new(items));
        let props_name = name.clone();
        let props_carries = carries.clone().unwrap_or_default();
        let title = title_of(name.as_deref());
        // What the place shows, said quietly at rest in its band: the caption's size, in the
        // theme's secondary ink, wrapped and said in full (a turn's caption is the person's whole
        // question), the band growing to hold it. It is what a person picks the place up by: a
        // press on it that travels carries the place's resource.
        let caption = Text::new(&title)
            .with_role(TextRole::Caption)
            .dimmed()
            .with_wrap_mode(crate::text_render::TextWrapMode::WordOrGlyph);
        let caption: Element = match carries {
            Some(resource) if !resource.is_empty() => Box::new(DragSource::new(
                format!("{key}\u{1f}handle"),
                Box::new(caption),
                resource,
                title.clone(),
            )),
            _ => Box::new(caption),
        };
        // The handle: the resource's actions, pin and close, on hover and focus only.
        let handle = Stack::row()
            .align(Align::Center)
            .gap(Rhythm::Tight)
            .push(controls.unwrap_or_else(empty));
        let mut part = Part::new(body.unwrap_or_else(empty));
        part.fit = Fit::fill(1.0);
        let ground = PlaceGround {
            name: name.clone(),
            column: Stack::stack()
                .fill_width()
                .fill_height()
                .gap(Rhythm::None)
                .push_part(part),
        };
        // The place's menu: the resource's own actions, read when it opens so it offers what
        // is true then. It stands around the view and the handle both, so a right-click on the
        // handle — or on a part of the view that has no menu of its own — opens it.
        let offered = Rc::clone(&items);
        let toolbar = HoverToolbar::above_captioned(
            Box::new(ground),
            vec![Box::new(handle)],
            PLACE_BAND,
            caption,
        );
        let menu = ContextMenuArea::new(Box::new(toolbar), move |_| {
            offered
                .borrow()
                .iter()
                .cloned()
                .map(MenuItemProps::into_entry)
                .collect()
        });
        Self {
            menu,
            name: title_of(props_name.as_deref()),
            carries: props_carries,
            items,
        }
    }

    /// The same place told it again keeps its view, its handle and the hover it is in: only
    /// another name, another resource or new views in its parts build it again.
    fn set_props(&mut self, props: PlaceProps) -> bool {
        let kept = props.controls.is_none()
            && props.body.is_none()
            && title_of(props.name.as_deref()) == self.name
            && props.carries.unwrap_or_default() == self.carries;
        if kept {
            *self.items.borrow_mut() = props.items;
        }
        kept
    }
}

/// The name a place's handle shows.
fn title_of(name: Option<&str>) -> String {
    name.unwrap_or_default().to_owned()
}

impl Decorator for CanvasPlace {
    type Inner = ContextMenuArea;

    fn inner(&self) -> &ContextMenuArea {
        &self.menu
    }

    fn inner_mut(&mut self) -> &mut ContextMenuArea {
        &mut self.menu
    }

    fn name(&self) -> &'static str {
        "Place"
    }
}

/// The place's ground: the window surface under its view, which is cut at the surface's rounded
/// edge so nothing it holds is seen past it. It is the place as assistive technology reads it,
/// named for what it shows: the place's surface, where it was put, its handle's band over it.
struct PlaceGround {
    name: Option<String>,
    column: Stack,
}

impl Decorator for PlaceGround {
    type Inner = Stack;

    fn inner(&self) -> &Stack {
        &self.column
    }

    fn inner_mut(&mut self) -> &mut Stack {
        &mut self.column
    }

    fn name(&self) -> &'static str {
        "PlaceGround"
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        Some(
            crate::a11y::AccessNode::new(crate::a11y::Role::Group)
                .with_optional_name(self.name.clone()),
        )
    }

    /// Its view, on the window surface it is painted on.
    fn html(&self, cx: &mut crate::composition::HtmlCx<'_>) {
        self.column.html(cx);
        let theme = cx.theme();
        SurfaceLevel::Window.compose(theme, self.column.get_rect(), cx.node());
    }

    fn child_clip_rect(&self) -> Option<Rect> {
        let rect = self.column.get_rect();
        (rect.width() > 0.0).then_some(rect)
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let rect = self.column.get_rect();
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let radius = SurfaceLevel::Window.radius(theme).min(rect.height() / 2.0);
        let on = paint_surface(scene, transform, rect, SurfaceLevel::Window, theme);
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &RoundedRect::from_rect(rect, radius),
        );
        self.column
            .draw_child(scene, transform, text, &on, presence);
        scene.pop_layer();
    }
}

#[cfg(test)]
#[path = "canvas_place_tests.rs"]
mod tests;
