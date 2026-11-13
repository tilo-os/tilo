//! `Place`: a resource's view standing on a canvas as content, not in a window (canvas.md §2).
//!
//! A place is its view on the window surface's own ground — rounded, with the surface's soft
//! edge and shadow — and nothing else: no title bar, no border of a window. Its handle (what it
//! is, the resource's actions, pin and close) comes over its top trailing corner while the
//! pointer is over it or something in it holds focus, the way a message's toolbar does, and
//! fades away after. Picking the handle up carries the place's resource on the drag bus, so
//! letting it go over the canvas moves the place there and over a composer attaches it.
//!
//! The look is the theme's: the surface is [`SurfaceLevel::Window`], the handle a floating
//! surface of the hover toolbar's, the title a label in the theme's secondary ink.

use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect};
use vello::peniko::{BlendMode, Fill};

use crate::component::{Component, Described};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::{Rhythm, SpacingStep, TextRole};
use crate::widget::arrange::{Align, Fit, Slot, Stack};
use crate::widget::primitives::{SurfaceLevel, paint_surface};
use crate::widget::{Decorator, DragSource, Element, HoverToolbar, Spacer, Text, Widget};

/// A resource's view standing on a canvas: the view on the window surface, with a handle shown
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
    #[component(slot)]
    pub controls: Option<Element>,
    /// The view the place shows, filling it.
    #[component(slot(default))]
    pub body: Option<Element>,
}

/// See the module docs.
pub struct CanvasPlace {
    toolbar: HoverToolbar,
    /// The name its handle shows, and the resource the handle carries: what it was built with.
    name: String,
    carries: String,
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
        } = props;
        let props_name = name.clone();
        let props_carries = carries.clone().unwrap_or_default();
        let title = title_of(&name);
        let mut label = Text::new(&title).with_role(TextRole::Label).dimmed();
        label = label.with_truncate(true);
        let handle = Stack::row()
            .align(Align::Center)
            .gap(Rhythm::Tight)
            .push(Box::new(label))
            .push(controls.unwrap_or_else(empty));
        // The handle is what a person picks the place up by: a press on it that travels carries
        // the place's resource; one that does not is the control's under it.
        let handle: Element = match carries {
            Some(resource) if !resource.is_empty() => Box::new(DragSource::new(
                format!("{key}\u{1f}handle"),
                Box::new(handle),
                resource,
                title,
            )),
            _ => Box::new(handle),
        };
        let mut slot = Slot::new(body.unwrap_or_else(empty));
        slot.fit = Fit::fill(1.0);
        let ground = PlaceGround {
            name,
            column: Stack::stack()
                .fill_width()
                .fill_height()
                .gap(Rhythm::None)
                .push_slot(slot),
        };
        Self {
            toolbar: HoverToolbar::filling(Box::new(ground), vec![handle]),
            name: title_of(&props_name),
            carries: props_carries,
        }
    }

    /// The same place told it again keeps its view, its handle and the hover it is in: only
    /// another name, another resource or new views in its slots build it again.
    fn set_props(&mut self, props: PlaceProps) -> bool {
        props.controls.is_none()
            && props.body.is_none()
            && title_of(&props.name) == self.name
            && props.carries.unwrap_or_default() == self.carries
    }
}

/// The name a place's handle shows.
fn title_of(name: &Option<String>) -> String {
    name.clone().unwrap_or_default()
}

impl Decorator for CanvasPlace {
    type Inner = HoverToolbar;

    fn inner(&self) -> &HoverToolbar {
        &self.toolbar
    }

    fn inner_mut(&mut self) -> &mut HoverToolbar {
        &mut self.toolbar
    }

    fn name(&self) -> &'static str {
        "Place"
    }
}

/// The place's ground: the window surface under its view, which is cut at the surface's rounded
/// edge so nothing it holds is seen past it. It is the place's own layout node, so it is the group
/// assistive technology reads the place as, named for what it shows.
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
        self.column.draw(scene, transform, text, &on, presence);
        scene.pop_layer();
    }
}

#[cfg(test)]
#[path = "canvas_place_tests.rs"]
mod tests;
