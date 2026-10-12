//! The brush presets as a person picks them: each swatch is a stroke drawn
//! by that preset, through the same renderer the canvas draws with, in the
//! paint the brush is loaded with.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo's Affine composition is float geometry"
)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use ui::vello::kurbo::{Affine, Rect, RoundedRect};
use ui::vello::peniko::{BlendMode, Fill};
use ui::widget::ColorSwatchPicker;

use crate::brushes::Preset;
use crate::model::{Artwork, Ground, Layer, Mark, Shape, Style};
use crate::render::{Options, Renderer};

/// The presets a brush lays paint with, in the picker's order.
pub const PAINTING: [Preset; 8] = [
    Preset::Round,
    Preset::Pen,
    Preset::Pencil,
    Preset::Marker,
    Preset::Airbrush,
    Preset::Charcoal,
    Preset::Calligraphy,
    Preset::Pixel,
];

/// A swatch's sample, in its own units.
const SAMPLE: (f64, f64) = (100.0, 46.0); // the sample sheet's size in artwork units, not spacing
/// A swatch's width and height in the sidebar.
const CELL: (f64, f64) = (64.0, 40.0); // four to a row across the sidebar: a cell size, not spacing

/// A stroke `preset` lays across the sample sheet, pressing harder toward
/// its middle, in `colour`.
fn sample(preset: Preset, colour: &str) -> Artwork {
    let size = match preset {
        Preset::Pencil => 5.0,
        Preset::Airbrush => 16.0,
        Preset::Marker => 11.0,
        Preset::Charcoal => 12.0,
        Preset::Pixel => 3.0,
        _ => 9.0,
    };
    let points = (0..=12)
        .map(|step| {
            let t = f64::from(step) / 12.0;
            let x = 12.0 + t * 76.0;
            let y = 23.0 + (t * std::f64::consts::TAU).sin() * 9.0;
            let pressure = 0.25 + 0.75 * (t * std::f64::consts::PI).sin();
            vec![x, y, pressure]
        })
        .collect();
    Artwork {
        width: SAMPLE.0,
        height: SAMPLE.1,
        ground: Ground::default(),
        layers: vec![Layer {
            group: String::new(),
            id: "sample".into(),
            name: String::new(),
            order: "a".into(),
            medium: String::new(),
            opacity: 1.0,
            visible: true,
            texture: 0.0,
            locked: false,
            mask_of: String::new(),
            base: 0.0,
            invert: false,
            clip: false,
            unlinked: false,
            adjust: None,
        }]
        .into(),
        marks: vec![Mark {
            id: format!("sample-{}", preset.name()),
            layer: "sample".into(),
            order: "a".into(),
            name: String::new(),
            shape: Shape::plain_stroke(points, size),
            style: Style {
                stroke: Some(colour.to_owned()),
                brush: crate::brushes::stored(preset.name()),
                params: BTreeMap::new(),
                ..Style::default()
            },
            transform: None,
            group: String::new(),
            clip: String::new(),
            area: String::new(),
        }]
        .into(),
        ..Artwork::default()
    }
}

/// A picker of `presets`, `chosen` shown as picked, each swatch drawn in
/// the colour `colour` answers when it is drawn; `on_change` is told the
/// preset picked.
pub fn picker(
    presets: &'static [Preset],
    chosen: Preset,
    colour: impl Fn() -> String + 'static,
    mut on_change: impl FnMut(Preset) + 'static,
) -> ColorSwatchPicker {
    let renderer = Rc::new(RefCell::new(Renderer::default()));
    let labels = presets
        .iter()
        .map(|preset| ui::i18n::I18nManager::lookup_plain(&preset.label()))
        .collect();
    // The sheet's colour names each swatch's ring; the painter draws it.
    let paper = 0x00f4_efe3; // document colour: the sample sheet's paper
    let mut samples: Vec<(String, Artwork)> = Vec::new();
    super::inspector::swatches(&vec![paper; presets.len()], labels, 4)
        .with_cell(CELL.0)
        .with_cell_height(CELL.1)
        .selecting(presets.iter().position(|preset| *preset == chosen))
        .with_painter(move |scene, transform, rect: Rect, index, theme, text| {
            let ink = colour();
            if samples.len() != presets.len()
                || samples
                    .get(index)
                    .is_some_and(|(drawn_in, _)| *drawn_in != ink)
            {
                samples = presets
                    .iter()
                    .map(|preset| (ink.clone(), sample(*preset, &ink)))
                    .collect();
            }
            let Some((_, art)) = samples.get(index) else {
                return;
            };
            let frame = RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.item));
            scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &frame);
            let scale = (rect.width() / SAMPLE.0).min(rect.height() / SAMPLE.1);
            let offset = rect.center()
                - ui::vello::kurbo::Vec2::new(SAMPLE.0 * scale / 2.0, SAMPLE.1 * scale / 2.0);
            let place = transform * Affine::translate(offset.to_vec2()) * Affine::scale(scale);
            renderer
                .borrow_mut()
                .draw(art, scene, place, text, &Options::default());
            scene.pop_layer();
        })
        .on_change(move |index| {
            if let Some(preset) = presets.get(index) {
                on_change(*preset);
            }
        })
}

/// Showing a picked swatch while building a picker.
trait Selecting {
    fn selecting(self, index: Option<usize>) -> Self;
}

impl Selecting for ColorSwatchPicker {
    fn selecting(mut self, index: Option<usize>) -> Self {
        self.set_selected(index);
        self
    }
}
