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

use crate::brushes::{DYNAMICS, Dynamics, Preset};
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
    /// Selects a rectangle of the artwork.
    Marquee,
    /// Selects an ellipse.
    Oval,
    /// Selects freehand (drag) or point by point (click).
    Lasso,
    /// Selects like colour touching a point.
    Wand,
    /// Fills like colour touching a point.
    Bucket,
    Eraser,
}

impl Tool {
    /// The tools in the rail's order.
    pub const ALL: [Self; 14] = [
        Self::Select,
        Self::Marquee,
        Self::Oval,
        Self::Lasso,
        Self::Wand,
        Self::Brush,
        Self::Bucket,
        Self::Eraser,
        Self::Rect,
        Self::Ellipse,
        Self::Line,
        Self::Pen,
        Self::Text,
        Self::Eyedropper,
    ];

    /// Whether the tool selects an area of the artwork.
    pub fn selects_area(self) -> bool {
        matches!(self, Self::Marquee | Self::Oval | Self::Lasso | Self::Wand)
    }

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
            Self::Marquee => "canvas-tool-marquee",
            Self::Oval => "canvas-tool-oval",
            Self::Lasso => "canvas-tool-lasso",
            Self::Wand => "canvas-tool-wand",
            Self::Bucket => "canvas-tool-bucket",
            Self::Eraser => "canvas-tool-eraser",
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
            Self::Marquee => Icon::SquareDashed,
            Self::Oval => Icon::CircleDashed,
            Self::Lasso => Icon::Lasso,
            Self::Wand => Icon::WandSparkles,
            Self::Bucket => Icon::PaintBucket,
            Self::Eraser => Icon::Eraser,
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
            Self::Marquee => "m",
            Self::Oval => "o",
            Self::Lasso => "q",
            Self::Wand => "w",
            Self::Bucket => "g",
            Self::Eraser => "x",
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
    /// The preset the brush lays strokes with on a plain layer.
    pub preset: LocalState<Preset>,
    /// The brush's opacity, `0..=1`.
    pub opacity: LocalState<f64>,
    /// The brush's dynamics as the person set them, starting at its
    /// preset's own.
    pub flow: LocalState<f64>,
    pub hardness: LocalState<f64>,
    pub smoothing: LocalState<f64>,
    pub taper: LocalState<f64>,
    /// What the eraser takes: whole strokes, or the paint under it.
    pub erasing: LocalState<Erasing>,
    /// How crisp the eraser's edge is, `0..=1`.
    pub eraser_hardness: LocalState<f64>,
    /// How much paint the eraser takes off, `0..=1`.
    pub eraser_strength: LocalState<f64>,
    /// Draws the artwork and keeps painted layers' simulations, shared by
    /// the canvas and the layer thumbnails so each layer is simulated once.
    pub renderer: Rc<RefCell<Renderer>>,
    /// The dialog open over the window, if any.
    pub sheet: LocalState<Option<Sheet>>,
    /// What the person asked of Files, carried out by the file desk.
    pub files: LocalState<Option<FileRequest>>,
    /// The file this window's artwork was last opened from or saved to.
    pub file: LocalState<Option<String>>,
    /// What the last file request came to, said beside the title.
    pub notice: LocalState<Option<String>>,
    /// The artworks opened or saved lately, newest first.
    pub recents: LocalState<Vec<crate::document::Recent>>,
    /// The welcome over a blank artwork was put away.
    pub welcomed: LocalState<bool>,
    /// What the export dialog is set to.
    pub export: crate::panels::export::Choices,
    /// What the new-artwork dialog is set to.
    pub new: crate::panels::start::Choices,
    /// The part of the artwork the canvas shows, in artwork units.
    pub visible: LocalState<Option<ui::vello::kurbo::Rect>>,
}

/// A dialog over the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sheet {
    /// A size, a ground and a medium to start a new artwork on.
    NewArtwork,
    /// Exporting the artwork as a picture.
    Export,
    /// The keyboard shortcuts.
    Shortcuts,
}

/// Something asked of Files.
#[derive(Clone, Debug, PartialEq)]
pub enum FileRequest {
    /// Choose an artwork or SVG to open in place of this one.
    Open,
    /// Open the artwork at this path.
    OpenRecent(String),
    /// Save to the file the artwork came from, or choose one.
    Save,
    /// Choose a file to save the artwork to, and keep working on it there.
    SaveAs,
    /// Save a copy to a file of its own and keep working on this one.
    Duplicate,
    /// Export a picture as set in the export dialog.
    Export(crate::panels::export::Settings),
}

/// The brush's paint and the chosen layer, apart from the rest of the
/// tools.
#[derive(Clone)]
pub struct Loaded {
    colour: LocalState<String>,
    pigment: LocalState<Option<(String, String)>>,
    layer: LocalState<String>,
}

impl Loaded {
    /// The paint as a mark names it, as [`Tools::paint`] says.
    pub fn paint(&self) -> String {
        if let Some((name, _)) = self.pigment.with(Clone::clone) {
            return format!("pigment:{name}");
        }
        let rgba = self
            .colour
            .with(|text| crate::render::colour(text.trim(), &[]))
            .or_else(|| crate::render::colour(INK, &[]))
            .unwrap_or(Color::TRANSPARENT)
            .to_rgba8();
        format!("#{:02x}{:02x}{:02x}", rgba.r, rgba.g, rgba.b)
    }

    /// The chosen layer, if one is chosen.
    pub fn layer(&self) -> Option<String> {
        Some(self.layer.with(Clone::clone)).filter(|layer| !layer.is_empty())
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

/// What the eraser takes off.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Erasing {
    /// The paint under it, as an eraser mark of its own.
    #[default]
    Paint,
    /// Every stroke it touches, whole.
    Strokes,
}

impl Default for Tools {
    fn default() -> Self {
        let round = Dynamics::of(Preset::Round, &BTreeMap::new());
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
            preset: LocalState::new(Preset::Round),
            opacity: LocalState::new(1.0),
            flow: LocalState::new(round.flow),
            hardness: LocalState::new(round.hardness),
            smoothing: LocalState::new(round.smoothing),
            taper: LocalState::new(round.taper),
            erasing: LocalState::new(Erasing::Paint),
            eraser_hardness: LocalState::new(1.0),
            eraser_strength: LocalState::new(1.0),
            renderer: Rc::default(),
            sheet: LocalState::new(None),
            files: LocalState::new(None),
            file: LocalState::new(None),
            notice: LocalState::new(None),
            recents: LocalState::new(Vec::new()),
            welcomed: LocalState::new(false),
            export: crate::panels::export::Choices::default(),
            new: crate::panels::start::Choices::default(),
            visible: LocalState::new(None),
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

    /// The paint and the chosen layer alone, for a control that reads them
    /// when it is used.
    pub fn loaded(&self) -> Loaded {
        Loaded {
            colour: self.colour.clone(),
            pigment: self.pigment.clone(),
            layer: self.layer.clone(),
        }
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

    /// Load the brush with `preset`, its dynamics starting at the
    /// preset's own.
    pub fn choose_preset(&self, preset: Preset) {
        let dynamics = Dynamics::of(preset, &BTreeMap::new());
        self.preset.set(preset);
        self.flow.set(dynamics.flow);
        self.hardness.set(dynamics.hardness);
        self.smoothing.set(dynamics.smoothing);
        self.taper.set(dynamics.taper);
    }

    /// The preset name a stroke on `layer` is laid with: the brush's on a
    /// plain layer; a painted layer's medium paints it.
    pub fn brush_name(&self, art: &Artwork, layer: &str) -> Option<String> {
        if art
            .layer(layer)
            .is_some_and(|layer| !layer.medium.is_empty())
        {
            return None;
        }
        crate::brushes::stored(self.preset.with(|preset| preset.name()))
    }

    /// The parameters a stroke on `layer` carries: on a painted layer its
    /// load, when the load was set; on a plain layer the dynamics the
    /// person moved away from the preset's.
    pub fn stroke_params(&self, art: &Artwork, layer: &str) -> Option<BTreeMap<String, f64>> {
        let plain = art
            .layer(layer)
            .or_else(|| art.layers.last())
            .is_some_and(|layer| layer.medium.is_empty());
        if plain {
            let preset = self.preset.with(|preset| *preset);
            let defaults = Dynamics::of(preset, &BTreeMap::new());
            let set = [
                (self.flow.with(|value| *value), defaults.flow),
                (self.hardness.with(|value| *value), defaults.hardness),
                (defaults.spacing, defaults.spacing),
                (self.smoothing.with(|value| *value), defaults.smoothing),
                (self.taper.with(|value| *value), defaults.taper),
            ];
            let params: BTreeMap<String, f64> = DYNAMICS
                .iter()
                .zip(set)
                .filter(|(_, (value, default))| (value - default).abs() > 0.005)
                .map(|(name, (value, _))| ((*name).to_owned(), (value * 100.0).round() / 100.0))
                .collect();
            return (!params.is_empty()).then_some(params);
        }
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
