//! Media as the artwork names them: the built-in presets every machine has
//! and the presets saved in this artwork, which travel with it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tilo_wet_media::{Engines, Pigment, Preset, Texture};
use ui::tilo_resource::{self, JsonSchema};

use crate::model::Artwork;

/// A pigment in a medium's palette, used as `pigment:<name>`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PigmentSpec {
    /// Lowercase words joined by `-`, such as `burnt-sienna`.
    pub name: String,
    /// Masstone colour, `#rrggbb`.
    pub colour: String,
    /// Watercolour: how much it settles into the paper's tooth, `0..=1`.
    #[serde(default)]
    pub granulation: f64,
    /// How strongly it tints, around `1`.
    #[serde(default = "one")]
    pub strength: f64,
}

fn one() -> f64 {
    1.0
}

/// A medium: one of the built-in engines with its parameters, a suggested
/// ground and a palette. Saving one makes it part of the artwork, shared
/// with everyone who opens it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MediumSpec {
    /// Lowercase id, letters, digits and `-`.
    pub id: String,
    pub label: String,
    /// `oil` or `watercolour`.
    pub engine: String,
    #[serde(default)]
    pub description: String,
    /// Engine parameters by id; `media` lists each engine's parameters and
    /// ranges.
    #[serde(default)]
    pub params: BTreeMap<String, f64>,
    /// The ground this medium suits: `smooth`, `paper`, `cold-press`,
    /// `rough` or `linen`.
    #[serde(default)]
    pub ground: Option<String>,
    #[serde(default)]
    pub palette: Vec<PigmentSpec>,
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "medium parameters are small bounded values"
)]
fn narrow(value: f64) -> f32 {
    value as f32
}

impl MediumSpec {
    pub fn from_preset(preset: &Preset) -> Self {
        Self {
            id: preset.id.clone(),
            label: preset.label.clone(),
            engine: preset.engine.clone(),
            description: preset.description.clone(),
            params: preset
                .params
                .iter()
                .map(|(id, value)| (id.clone(), f64::from(*value)))
                .collect(),
            ground: preset.ground.map(|texture| texture.name().to_owned()),
            palette: preset
                .palette
                .iter()
                .map(|pigment| PigmentSpec {
                    name: pigment.name.clone(),
                    colour: pigment.colour.clone(),
                    granulation: f64::from(pigment.granulation),
                    strength: f64::from(pigment.strength),
                })
                .collect(),
        }
    }

    pub fn to_preset(&self) -> Preset {
        Preset {
            id: self.id.clone(),
            label: self.label.clone(),
            engine: self.engine.clone(),
            description: self.description.clone(),
            params: self
                .params
                .iter()
                .map(|(id, value)| (id.clone(), narrow(*value)))
                .collect(),
            ground: self.ground.as_deref().and_then(Texture::parse),
            palette: self.pigments(),
        }
    }

    pub fn pigments(&self) -> Vec<Pigment> {
        self.palette
            .iter()
            .map(|pigment| Pigment {
                name: pigment.name.clone(),
                colour: pigment.colour.clone(),
                granulation: narrow(pigment.granulation),
                strength: narrow(pigment.strength),
            })
            .collect()
    }

    /// Why this medium cannot be saved, in words an agent can act on.
    ///
    /// # Errors
    /// Names the first invalid field.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(ground) = &self.ground
            && Texture::parse(ground).is_none()
        {
            return Err(format!(
                "ground {ground:?} must be one of smooth, paper, cold-press, rough, linen"
            ));
        }
        if self.label.trim().is_empty() {
            return Err("a medium needs a label".into());
        }
        self.to_preset().validate(&Engines::builtin())
    }
}

/// The built-in media, in the order a picker offers them.
pub fn builtin() -> Vec<MediumSpec> {
    tilo_wet_media::presets::builtin()
        .iter()
        .map(MediumSpec::from_preset)
        .collect()
}

/// The medium `id` names in `art`: one saved in the artwork, else a
/// built-in one.
pub fn resolve(art: &Artwork, id: &str) -> Option<MediumSpec> {
    art.media
        .get(id)
        .cloned()
        .or_else(|| builtin().into_iter().find(|medium| medium.id == id))
}

/// Every medium a layer can use here: the artwork's own, then the built-in
/// ones it does not override.
pub fn available(art: &Artwork) -> Vec<MediumSpec> {
    let mut media: Vec<MediumSpec> = art.media.values().cloned().collect();
    for medium in builtin() {
        if !art.media.contains_key(&medium.id) {
            media.push(medium);
        }
    }
    media
}

/// The pigments `pigment:<name>` resolves against on a layer: its medium's
/// palette, or on a plain layer every palette.
pub fn palette_for(art: &Artwork, layer: &str) -> Vec<Pigment> {
    let medium = art
        .layer(layer)
        .filter(|layer| !layer.medium.is_empty())
        .and_then(|layer| resolve(art, &layer.medium));
    match medium {
        Some(medium) => medium.pigments(),
        None => available(art)
            .iter()
            .flat_map(MediumSpec::pigments)
            .collect(),
    }
}
