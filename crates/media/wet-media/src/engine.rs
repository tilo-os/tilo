//! The boundary every medium sits behind.
//!
//! A [`MediumEngine`] simulates one kind of paint. It is given a layer's
//! marks one at a time and paints them into its own [`LayerSim`], which it
//! can composite to pixels at any point. Engines are deterministic: the same
//! marks with the same seeds give the same pixels on every machine, so a
//! painting syncs as marks and every collaborator simulates it locally.
//!
//! A [`Preset`] is plain data over an engine: its parameters, a suggested
//! ground and a palette. Presets are what users share; engines are code.
//! The built-in engines are linked in; a sandboxed plug-in engine implements
//! the same two traits behind a process boundary.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::brush::StrokePoint;
use crate::colour::{Paint, Pigment};
use crate::field::ColourField;
use crate::ground::{Heightfield, Texture};

/// How a medium's layer composes onto what is beneath it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Blend {
    /// Opaque paint covers what is beneath it.
    Normal,
    /// Transparent pigment darkens what is beneath it, as glazes do.
    Multiply,
}

/// One tunable of an engine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParamSpec {
    pub id: String,
    pub label: String,
    pub description: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
}

impl ParamSpec {
    pub fn new(id: &str, label: &str, description: &str, range: (f32, f32), default: f32) -> Self {
        Self {
            id: id.to_owned(),
            label: label.to_owned(),
            description: description.to_owned(),
            min: range.0,
            max: range.1,
            default,
        }
    }
}

/// Parameter values over an engine's specs: unknown ids are ignored, missing
/// ones take the spec default and every value is clamped to its range.
#[derive(Clone, Debug)]
pub struct Settings {
    specs: Arc<[ParamSpec]>,
    values: BTreeMap<String, f32>,
}

impl Settings {
    pub fn new(specs: Arc<[ParamSpec]>, values: &BTreeMap<String, f32>) -> Self {
        let mut settings = Self {
            specs,
            values: BTreeMap::new(),
        };
        settings.merge(values);
        settings
    }

    /// These settings with `overrides` on top.
    #[must_use]
    pub fn with(&self, overrides: &BTreeMap<String, f32>) -> Self {
        let mut settings = self.clone();
        settings.merge(overrides);
        settings
    }

    fn merge(&mut self, values: &BTreeMap<String, f32>) {
        for (id, value) in values {
            if let Some(spec) = self.specs.iter().find(|spec| spec.id == *id)
                && value.is_finite()
            {
                self.values
                    .insert(id.clone(), value.clamp(spec.min, spec.max));
            }
        }
    }

    pub fn get(&self, id: &str) -> f32 {
        self.values.get(id).copied().unwrap_or_else(|| {
            self.specs
                .iter()
                .find(|spec| spec.id == id)
                .map_or(0.0, |spec| spec.default)
        })
    }

    /// Every parameter's effective value.
    pub fn resolved(&self) -> BTreeMap<String, f32> {
        self.specs
            .iter()
            .map(|spec| (spec.id.clone(), self.get(&spec.id)))
            .collect()
    }
}

/// The support a layer is painted on, in simulation cells.
#[derive(Clone, Debug)]
pub struct Canvas {
    pub width: usize,
    pub height: usize,
    /// Cells per artwork unit.
    pub scale: f32,
    pub ground: Arc<Heightfield>,
}

/// A brush stroke, in cell coordinates.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stroke {
    pub points: Vec<StrokePoint>,
    pub size: f32,
    pub paint: Paint,
    pub opacity: f32,
    /// Paint into the layer while it is still wet (blending, bleeding),
    /// rather than over a dried layer.
    pub wet: bool,
    /// Per-mark parameter overrides on top of the preset.
    #[serde(default)]
    pub params: BTreeMap<String, f32>,
    /// What the stroke grows into: twigs, or a cluster of dabs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub form: Option<crate::form::Form>,
    /// Closed outlines the paint stays inside, as a painter cuts a stroke
    /// in against an edge; anywhere when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clip: Vec<Vec<(f32, f32)>>,
}

/// A filled region laid in with a brush, in cell coordinates.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Fill {
    /// Closed outlines; overlapping ones follow the non-zero rule.
    pub contours: Vec<Vec<(f32, f32)>>,
    pub paint: Paint,
    pub opacity: f32,
    pub wet: bool,
    /// The width of the brush that lays the region in.
    pub brush: f32,
    /// The direction of the brushwork, in degrees.
    pub angle: f32,
    #[serde(default)]
    pub params: BTreeMap<String, f32>,
    /// Colour that changes across the region, mixed as it is laid in;
    /// `paint` gives the handling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<ColourField>,
}

/// One mark as a medium receives it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Mark {
    Stroke(Stroke),
    Fill(Fill),
}

impl Mark {
    /// This mark with artwork coordinates converted to cells.
    #[must_use]
    pub fn scaled(&self, scale: f32) -> Self {
        match self {
            Self::Stroke(stroke) => Self::Stroke(Stroke {
                points: stroke
                    .points
                    .iter()
                    .map(|point| StrokePoint::new(point.x * scale, point.y * scale, point.pressure))
                    .collect(),
                size: stroke.size * scale,
                form: stroke.form.map(|form| form.scaled(scale)),
                clip: stroke
                    .clip
                    .iter()
                    .map(|contour| {
                        contour
                            .iter()
                            .map(|(x, y)| (x * scale, y * scale))
                            .collect()
                    })
                    .collect(),
                ..stroke.clone()
            }),
            Self::Fill(fill) => Self::Fill(Fill {
                contours: fill
                    .contours
                    .iter()
                    .map(|contour| {
                        contour
                            .iter()
                            .map(|(x, y)| (x * scale, y * scale))
                            .collect()
                    })
                    .collect(),
                brush: fill.brush * scale,
                field: fill.field.as_ref().map(|field| field.scaled(scale)),
                ..fill.clone()
            }),
        }
    }

    pub fn params(&self) -> &BTreeMap<String, f32> {
        match self {
            Self::Stroke(stroke) => &stroke.params,
            Self::Fill(fill) => &fill.params,
        }
    }

    pub fn wet(&self) -> bool {
        match self {
            Self::Stroke(stroke) => stroke.wet,
            Self::Fill(fill) => fill.wet,
        }
    }
}

/// One layer's simulation state.
pub trait LayerSim: Send + Sync {
    /// Paint one mark. `seed` makes its randomness reproducible.
    fn apply(&mut self, mark: &Mark, seed: u64);

    /// The layer as it looks once dry: straight-alpha sRGB RGBA8, row-major,
    /// `width * height * 4` bytes. Does not change the state.
    fn composite(&self) -> Vec<u8>;

    fn clone_box(&self) -> Box<dyn LayerSim>;
}

/// A kind of paint.
pub trait MediumEngine: Send + Sync {
    fn id(&self) -> &str;
    fn label(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Arc<[ParamSpec]>;
    fn blend(&self) -> Blend;
    fn new_layer(&self, canvas: &Canvas, settings: Settings) -> Box<dyn LayerSim>;
}

/// A shareable medium: an engine with its parameters, a suggested ground and
/// a palette of named pigments.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub id: String,
    pub label: String,
    pub engine: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub params: BTreeMap<String, f32>,
    #[serde(default)]
    pub ground: Option<Texture>,
    #[serde(default)]
    pub palette: Vec<Pigment>,
}

impl Preset {
    /// Check a preset against the engines it names, with a message an agent
    /// can act on.
    ///
    /// # Errors
    /// Names the first invalid id, engine, parameter or pigment.
    pub fn validate(&self, engines: &Engines) -> Result<(), String> {
        if !valid_id(&self.id) {
            return Err(format!(
                "medium id {:?} must be 1-48 lowercase letters, digits or '-'",
                self.id
            ));
        }
        let engine = engines.get(&self.engine).ok_or_else(|| {
            format!(
                "unknown engine {:?}; engines are {}",
                self.engine,
                engines.ids().join(", ")
            )
        })?;
        let specs = engine.parameters();
        for (id, value) in &self.params {
            let spec = specs.iter().find(|spec| spec.id == *id).ok_or_else(|| {
                format!(
                    "engine {:?} has no parameter {id:?}; it has {}",
                    self.engine,
                    specs
                        .iter()
                        .map(|spec| spec.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
            if !(spec.min..=spec.max).contains(value) {
                return Err(format!(
                    "{id} must be within {}..={}, got {value}",
                    spec.min, spec.max
                ));
            }
        }
        for pigment in &self.palette {
            if crate::colour::parse_hex(&pigment.colour).is_none() {
                return Err(format!(
                    "pigment {:?} colour {:?} must be #rrggbb",
                    pigment.name, pigment.colour
                ));
            }
            if !valid_id(&pigment.name) {
                return Err(format!(
                    "pigment name {:?} must be lowercase words joined by '-'",
                    pigment.name
                ));
            }
        }
        Ok(())
    }
}

fn valid_id(id: &str) -> bool {
    (1..=48).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// The engines a host can simulate.
#[derive(Clone)]
pub struct Engines {
    engines: Vec<Arc<dyn MediumEngine>>,
}

impl Engines {
    /// The built-in engines.
    pub fn builtin() -> Self {
        Self {
            engines: vec![
                Arc::new(crate::watercolour::Watercolour::new()),
                Arc::new(crate::oil::Oil::new()),
            ],
        }
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn MediumEngine>> {
        self.engines
            .iter()
            .find(|engine| engine.id() == id)
            .cloned()
    }

    pub fn ids(&self) -> Vec<&str> {
        self.engines.iter().map(|engine| engine.id()).collect()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn MediumEngine>> {
        self.engines.iter()
    }
}

impl std::fmt::Debug for Engines {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_list().entries(self.ids()).finish()
    }
}
