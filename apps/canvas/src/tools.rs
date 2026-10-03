//! What the person is holding: the tool, the paint on the brush, the layer
//! new marks go on and what is selected. The rail and the panels set it;
//! the canvas reads it. None of it is part of the artwork: it is never
//! saved, shared or undone.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use ui::LocalState;
use ui::icons::Icon;
use ui::vello::peniko::Color;

use crate::gesture::Ink;
use crate::model::Artwork;
use crate::render::Renderer;

/// What the pointer does on the canvas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tool {
    #[default]
    Select,
    Brush,
    Rect,
    Ellipse,
    Line,
    Pen,
    Text,
    Eyedropper,
}

impl Tool {
    /// The tools in the rail's order.
    pub const ALL: [Self; 8] = [
        Self::Select,
        Self::Brush,
        Self::Rect,
        Self::Ellipse,
        Self::Line,
        Self::Pen,
        Self::Text,
        Self::Eyedropper,
    ];

    /// The Fluent id of the tool's name.
    pub fn label(self) -> &'static str {
        match self {
            Self::Select => "canvas-tool-select",
            Self::Brush => "canvas-tool-brush",
            Self::Rect => "canvas-tool-rect",
            Self::Ellipse => "canvas-tool-ellipse",
            Self::Line => "canvas-tool-line",
            Self::Pen => "canvas-tool-pen",
            Self::Text => "canvas-tool-text",
            Self::Eyedropper => "canvas-tool-eyedropper",
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            Self::Select => Icon::MousePointer2,
            Self::Brush => Icon::Brush,
            Self::Rect => Icon::Square,
            Self::Ellipse => Icon::Circle,
            Self::Line => Icon::Slash,
            Self::Pen => Icon::PenTool,
            Self::Text => Icon::Type,
            Self::Eyedropper => Icon::Pipette,
        }
    }

    /// The key that picks the tool up while the canvas has focus.
    pub fn shortcut(self) -> &'static str {
        match self {
            Self::Select => "v",
            Self::Brush => "b",
            Self::Rect => "r",
            Self::Ellipse => "e",
            Self::Line => "l",
            Self::Pen => "p",
            Self::Text => "t",
            Self::Eyedropper => "i",
        }
    }

    /// The tool `key` picks up, if any.
    pub fn for_key(key: &str) -> Option<Self> {
        let key = key.to_lowercase();
        Self::ALL.into_iter().find(|tool| tool.shortcut() == key)
    }
}

/// The tool settings the rail and panels share with the canvas.
#[derive(Clone)]
pub struct Tools {
    pub tool: LocalState<Tool>,
    /// The brush's colour when no pigment is chosen, `#rrggbb` as typed.
    pub colour: LocalState<String>,
    /// The pigment of the layer's medium the brush is loaded with: its
    /// name and masstone `#rrggbb`. None paints `colour`.
    pub pigment: LocalState<Option<(String, String)>>,
    /// Brush size, `0..=1` of the slider's travel.
    pub size: LocalState<f64>,
    /// How much paint a painted layer's brush carries, `0..=1` of the
    /// medium's range. Choosing a painted layer starts it at the medium's
    /// own ([`load_fraction`]).
    pub load: LocalState<f64>,
    /// The layer new marks go on; the top layer when empty.
    pub layer: LocalState<String>,
    /// The marks selected on the canvas.
    pub selection: LocalState<Vec<String>>,
    /// A past version shown in place of the artwork, by change id.
    pub preview: LocalState<Option<String>>,
    /// How far the canvas is zoomed: screen pixels per artwork unit, as
    /// the canvas last showed it.
    pub zoom: LocalState<f64>,
    /// A change of view asked for from outside the canvas (the zoom
    /// buttons), carried out by the canvas on its next frame.
    pub view: LocalState<Option<ViewRequest>>,
    /// Whether the sidebar of inspector, layers and history is shown.
    pub sidebar: LocalState<bool>,
    /// The stop of a gradient fill the swatches recolour.
    pub stop: LocalState<usize>,
    /// The selected picture's handles crop it, and its turn handle
    /// straightens it, instead of stretching and turning the mark.
    pub crop: LocalState<bool>,
    /// The inspector's controls being dragged, shown on the canvas before
    /// they are let go and become an edit.
    pub live: Rc<RefCell<Vec<Live>>>,
    /// Whether the chosen layer's adjustments are shown, with nothing
    /// selected.
    pub layer_adjust: LocalState<bool>,
    /// The format a person asked to export the artwork in (`png`, `jpeg`,
    /// `webp`), until the canvas renders it; empty when none was asked.
    pub export: LocalState<String>,
    /// An export being saved where the person chooses, until it is.
    pub saving: Rc<RefCell<Option<ui::portal::SaveChannel>>>,
    /// A picture being chosen to add, until the chooser answers.
    pub picking: Rc<RefCell<Option<ui::portal::ContentsChannel>>>,
    /// Draws the artwork and keeps painted layers' simulations, shared by
    /// the canvas and the layer thumbnails so each layer is simulated once.
    pub renderer: Rc<RefCell<Renderer>>,
}

/// A control whose value the canvas shows while it is dragged.
#[derive(Clone)]
pub struct Live {
    /// The control's value, as it moves.
    pub state: LocalState<f64>,
    /// Its value when it was built: what the artwork already shows.
    pub committed: f64,
    /// The value onto the artwork as shown.
    pub apply: Onto,
}

/// How a control's value changes the artwork as shown.
pub type Onto = Rc<dyn Fn(&mut Artwork, f64)>;

impl Live {
    /// The value while it differs from what the artwork has.
    pub fn moved(&self) -> Option<f64> {
        let value = self.state.with(|value| *value);
        ((value - self.committed).abs() > 1e-9).then_some(value)
    }
}

/// A change of view the canvas makes when asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewRequest {
    ZoomIn,
    ZoomOut,
    /// The whole artwork in view.
    Fit,
    /// One artwork unit to a screen pixel.
    Actual,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            tool: LocalState::new(Tool::Brush),
            // The brush starts loaded with ultramarine: a paint colour, not
            // chrome, so it does not follow the theme.
            colour: LocalState::new(INK.to_owned()),
            pigment: LocalState::new(None),
            size: LocalState::new(0.2),
            load: LocalState::new(0.5),
            layer: LocalState::new(String::new()),
            selection: LocalState::new(Vec::new()),
            preview: LocalState::new(None),
            zoom: LocalState::new(1.0),
            view: LocalState::new(None),
            sidebar: LocalState::new(true),
            stop: LocalState::new(0),
            crop: LocalState::new(false),
            live: Rc::default(),
            picking: Rc::default(),
            export: LocalState::new(String::new()),
            saving: Rc::default(),
            layer_adjust: LocalState::new(false),
            renderer: Rc::default(),
        }
    }
}

/// The ink a brush starts with: ultramarine, a paint colour rather than
/// chrome, so it does not follow the theme.
const INK: &str = "#2b3f8f";

/// The smallest and largest brush, in artwork units.
const BRUSH_RANGE: (f64, f64) = (1.0, 120.0);

impl Tools {
    /// The brush width in artwork units.
    pub fn brush_size(&self) -> f64 {
        let travel = self.size.with(|size| size.clamp(0.0, 1.0));
        BRUSH_RANGE.0 + travel * travel * (BRUSH_RANGE.1 - BRUSH_RANGE.0)
    }

    /// The paint on the brush as a mark names it: `pigment:<name>` or
    /// `#rrggbb`.
    pub fn paint(&self) -> String {
        self.pigment.with(|pigment| match pigment {
            Some((name, _)) => format!("pigment:{name}"),
            None => self.hex(),
        })
    }

    /// The brush colour as `#rrggbb`; ink black while what was typed is
    /// not a colour.
    pub fn hex(&self) -> String {
        let rgba = self.colour().to_rgba8();
        format!("#{:02x}{:02x}{:02x}", rgba.r, rgba.g, rgba.b)
    }

    /// The colour the brush paints, for drawing its preview: the pigment's
    /// masstone, else the ink.
    pub fn colour(&self) -> Color {
        let pigment = self
            .pigment
            .with(|pigment| pigment.as_ref().map(|(_, hex)| hex.clone()));
        if let Some(colour) = pigment.and_then(|hex| crate::render::colour(&hex, &[])) {
            return colour;
        }
        self.colour.with(|text| {
            crate::render::colour(text.trim(), &[])
                .or_else(|| crate::render::colour(INK, &[]))
                .unwrap_or(Color::TRANSPARENT)
        })
    }

    /// What the tools put down now.
    pub fn ink(&self) -> Ink {
        Ink {
            colour: self.paint(),
            width: self.brush_size(),
            layer: Some(self.layer.with(Clone::clone)).filter(|layer| !layer.is_empty()),
        }
    }

    /// Load the brush with `colour`, picked from the artwork.
    pub fn pick(&self, colour: Color) {
        let rgba = colour.to_rgba8();
        self.pigment.set(None);
        self.colour
            .set(format!("#{:02x}{:02x}{:02x}", rgba.r, rgba.g, rgba.b));
    }

    /// The medium parameters a stroke on `layer` carries: its load, when
    /// the layer is painted and the load was set.
    pub fn stroke_params(&self, art: &Artwork, layer: &str) -> Option<BTreeMap<String, f64>> {
        let travel = self.load.with(|load| *load);
        let (min, max) = load_range(art, layer)?;
        Some(BTreeMap::from([(
            "load".to_owned(),
            min + travel.clamp(0.0, 1.0) * (max - min),
        )]))
    }
}

/// The range of `load` of the medium painting `layer`, when it has one.
pub fn load_range(art: &Artwork, layer: &str) -> Option<(f64, f64)> {
    load_param(art, layer).map(|(range, _)| range)
}

/// The medium's own `load` on `layer` as a fraction of its range: where
/// the brush starts when the layer is chosen.
pub fn load_fraction(art: &Artwork, layer: &str) -> Option<f64> {
    let ((min, max), value) = load_param(art, layer)?;
    (max > min).then(|| ((value - min) / (max - min)).clamp(0.0, 1.0))
}

/// `load`'s range and the value the medium painting `layer` gives it.
fn load_param(art: &Artwork, layer: &str) -> Option<((f64, f64), f64)> {
    let layer = art.layer(layer)?;
    if layer.medium.is_empty() {
        return None;
    }
    let medium = crate::media::resolve(art, &layer.medium)?;
    let engines = tilo_wet_media::Engines::builtin();
    let engine = engines.get(&medium.engine)?;
    let param = engine
        .parameters()
        .iter()
        .find(|param| param.id == "load")
        .cloned()?;
    let value = medium
        .params
        .get("load")
        .copied()
        .unwrap_or_else(|| f64::from(param.default));
    Some(((f64::from(param.min), f64::from(param.max)), value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_its_own_key() {
        for tool in Tool::ALL {
            assert_eq!(Tool::for_key(tool.shortcut()), Some(tool));
        }
        assert_eq!(Tool::for_key("B"), Some(Tool::Brush));
        assert_eq!(Tool::for_key("bb"), None);
        assert_eq!(Tool::for_key("z"), None);
    }

    #[test]
    fn a_chosen_pigment_is_the_paint() {
        let tools = Tools::default();
        assert_eq!(tools.paint(), "#2b3f8f");
        tools
            .pigment
            .set(Some(("burnt-sienna".into(), "#8a3c1e".into())));
        assert_eq!(tools.paint(), "pigment:burnt-sienna");
    }
}
