//! [`PaintSurface`] as a described component: the artwork, the brush and the view it shows.

use std::collections::BTreeMap;

use tilo_paint_strokes::{Brush, Preset};
use vello::peniko::Color;

use tilo_ui_base::{component::Component, component::Described, component::Event, component::Value};

use super::{MAX_SIZE, PaintSurface};
use crate::pictures::Pictures;
use crate::widget::PictureSource;

/// A surface a person paints on: the artwork beneath, the stroke under the hand previewed over
/// it.
#[derive(Component, Default)]
#[component(name = "PaintSurface", role = "Image", label = "label")]
pub struct PaintSurfaceProps {
    /// The artwork as it stands, its layers composited: the picture under the paint.
    pub picture: Option<PictureSource>,
    /// The artwork's width in its own units.
    #[component(required)]
    pub width: f64,
    /// The artwork's height in its own units.
    #[component(required)]
    pub height: f64,
    /// The brush that lays a stroke; the round ink brush when none is named.
    pub brush: Option<String>,
    /// The brush's width in artwork units.
    pub size: Option<f64>,
    /// The colour a stroke lays, `#rrggbb` or `#rrggbbaa`; ink when none is named.
    pub colour: Option<String>,
    /// How far the view is zoomed in; 1 fits the artwork in the surface.
    pub zoom: Option<f64>,
    /// The artwork unit at the surface's top left: where the view looks.
    pub pan: Option<Vec<f64>>,
    /// A picture laid over the artwork, such as a comparison's changed region.
    pub overlay: Option<PictureSource>,
    /// The frame the overlay fills, `[x, y, width, height]` in artwork units; the whole artwork
    /// when absent.
    pub overlay_rect: Option<Vec<f64>>,
    /// How strongly the overlay shows over the artwork, 0 to 1 (default 1).
    pub overlay_opacity: Option<f64>,
    /// What it shows, as assistive technology announces it.
    pub label: Option<String>,
    /// The window's pictures, which load the artwork's; a host hands them in.
    #[component(skip)]
    pub pictures: Option<Pictures>,
    /// A stroke began, at `[x, y, pressure]` in the artwork's units.
    #[component(event(value = Vec<f64>))]
    pub stroke_start: Event,
    /// A stroke went on, at `[x, y, pressure]` in the artwork's units.
    #[component(event(value = Vec<f64>))]
    pub stroke: Event,
    /// A stroke ended where the hand lifted, at `[x, y, pressure]`.
    #[component(event(value = Vec<f64>))]
    pub stroke_end: Event,
    /// The pointer moved over the surface, at `[x, y]` in the artwork's units.
    #[component(event(value = Vec<f64>))]
    pub hover: Event,
    /// The wheel turned over the surface: `[across, down]` in logical pixels.
    #[component(event(value = Vec<f64>))]
    pub wheel: Event,
    /// A pinch over the surface: the scale factor the fingers ask for, 1 for none.
    #[component(event(value = f64))]
    pub pinch: Event,
    /// A middle-button drag panned the view by `[dx, dy]` artwork units.
    #[component(event(value = Vec<f64>))]
    pub panned: Event,
}

impl Described for PaintSurface {
    type Props = PaintSurfaceProps;

    fn from_props(_key: &str, props: PaintSurfaceProps) -> Self {
        let mut surface = Self::default();
        surface.set_props(props);
        surface
    }

    fn set_props(&mut self, props: PaintSurfaceProps) -> bool {
        self.width = props.width.max(1.0);
        self.height = props.height.max(1.0);
        let preset = props
            .brush
            .as_deref()
            .and_then(Preset::parse)
            .unwrap_or_default();
        self.brush = Brush::of(preset, &BTreeMap::new(), self.brush.angle);
        self.size = props.size.unwrap_or(6.0).clamp(0.25, MAX_SIZE);
        self.colour = props
            .colour
            .as_deref()
            .and_then(crate::color::parse_hex)
            .unwrap_or_else(|| Color::from_rgba8(29, 27, 25, 255));
        self.zoom = props.zoom.unwrap_or(1.0).clamp(0.05, 64.0);
        if let Some(pan) = props.pan.as_deref() {
            self.pan = vello::kurbo::Point::new(
                pan.first().copied().unwrap_or(0.0),
                pan.get(1).copied().unwrap_or(0.0),
            );
        }
        let held = props.pictures.unwrap_or_else(Pictures::window);
        let same = match (&props.picture, self.picture.as_ref()) {
            (None, None) => true,
            (Some(want), Some(shown)) => want.src == shown.src,
            _ => false,
        };
        if !same {
            self.picture = props.picture;
        }
        self.pictures = Some(held);
        // An empty address is no overlay: a view that compares nothing passes one.
        let overlay = props.overlay.filter(|picture| !picture.src.is_empty());
        let same_overlay = match (&overlay, self.overlay.as_ref()) {
            (None, None) => true,
            (Some(want), Some(shown)) => want.src == shown.src,
            _ => false,
        };
        if !same_overlay {
            self.overlay = overlay;
        }
        self.overlay_rect = props.overlay_rect.as_deref().and_then(|rect| {
            let [x, y, width, height] = [rect.first(), rect.get(1), rect.get(2), rect.get(3)];
            Some([*x?, *y?, *width?, *height?])
        });
        self.overlay_opacity = props.overlay_opacity.unwrap_or(1.0);
        self.label = props.label;
        self.on_stroke_start = fire_vec(props.stroke_start);
        self.on_stroke = fire_vec(props.stroke);
        self.on_stroke_end = fire_vec(props.stroke_end);
        self.on_hover = fire_vec(props.hover);
        self.on_wheel = fire_vec(props.wheel);
        self.on_panned = fire_vec(props.panned);
        self.on_pinch = (!props.pinch.is_none()).then(|| {
            let pinch = props.pinch.clone();
            Box::new(move |scale: f64| pinch.fire(Some(Value::Float(scale)))) as Box<dyn FnMut(f64)>
        });
        self.layout_dirty = true;
        true
    }
}

/// The event as a handler of a list of floats, when the view listens for it.
fn fire_vec(event: Event) -> Option<Box<dyn FnMut(Vec<f64>)>> {
    (!event.is_none()).then(|| {
        Box::new(move |at: Vec<f64>| {
            event.fire(Some(Value::List(
                at.into_iter().map(Value::Float).collect(),
            )));
        }) as Box<dyn FnMut(Vec<f64>)>
    })
}
