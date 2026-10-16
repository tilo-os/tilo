//! The presets every Tilo machine has. They are ordinary [`Preset`] data:
//! a user preset is the same shape, saved in the artwork and shared with it.

use std::collections::BTreeMap;

use crate::colour::Pigment;
use crate::engine::Preset;
use crate::ground::Texture;

fn pigment(name: &str, colour: &str, granulation: f32, strength: f32) -> Pigment {
    Pigment {
        name: name.to_owned(),
        colour: colour.to_owned(),
        granulation,
        strength,
    }
}

fn params(values: &[(&str, f32)]) -> BTreeMap<String, f32> {
    values
        .iter()
        .map(|(id, value)| ((*id).to_owned(), *value))
        .collect()
}

/// A Romantic-era oil palette: earths, lead white and the blues of the
/// period, as a landscape painter of 1820 would have had them.
fn oil_palette() -> Vec<Pigment> {
    vec![
        pigment("lead-white", "#f2ede1", 0.0, 1.0),
        pigment("naples-yellow", "#e8c77a", 0.0, 1.0),
        pigment("yellow-ochre", "#c8902f", 0.0, 1.0),
        pigment("raw-sienna", "#b5733a", 0.0, 1.0),
        pigment("burnt-sienna", "#8a3c1e", 0.0, 1.0),
        pigment("vermilion", "#d23a24", 0.0, 1.0),
        pigment("madder-lake", "#9b2236", 0.0, 1.0),
        pigment("raw-umber", "#5e4a32", 0.0, 1.0),
        pigment("burnt-umber", "#4a2e1d", 0.0, 1.0),
        pigment("green-earth", "#6f7a52", 0.0, 1.0),
        pigment("viridian", "#2f6b55", 0.0, 1.0),
        pigment("prussian-blue", "#1f3550", 0.0, 1.0),
        pigment("ultramarine", "#2b3f8f", 0.0, 1.0),
        pigment("cobalt-blue", "#3b62a8", 0.0, 1.0),
        pigment("ivory-black", "#1e1c1a", 0.0, 1.0),
    ]
}

/// A modern artists' watercolour palette, with the granulating pigments
/// marked so they settle into the paper.
fn watercolour_palette() -> Vec<Pigment> {
    vec![
        pigment("hansa-yellow", "#f2c12e", 0.0, 1.0),
        pigment("new-gamboge", "#e59a1c", 0.0, 1.0),
        pigment("quinacridone-gold", "#c27c1e", 0.1, 1.0),
        pigment("burnt-sienna", "#9a4a24", 0.35, 1.0),
        pigment("pyrrol-scarlet", "#d8352a", 0.0, 1.1),
        pigment("quinacridone-rose", "#c8326b", 0.0, 1.0),
        pigment("perylene-maroon", "#6e2430", 0.0, 1.0),
        pigment("ultramarine", "#2e45a6", 0.85, 1.0),
        pigment("cerulean", "#3a86b8", 0.7, 0.8),
        pigment("phthalo-blue", "#14407a", 0.0, 1.5),
        pigment("phthalo-green", "#0f5c4d", 0.0, 1.5),
        pigment("sap-green", "#5a7a2a", 0.1, 1.0),
        pigment("sepia", "#4b3626", 0.3, 1.0),
        pigment("paynes-grey", "#36414d", 0.2, 1.0),
        pigment("lunar-black", "#2a2826", 0.9, 1.0),
    ]
}

/// The built-in presets, in the order a picker shows them.
pub fn builtin() -> Vec<Preset> {
    vec![
        Preset {
            id: "oil".into(),
            label: "Oil, alla prima".into(),
            engine: "oil".into(),
            description: "Opaque oil laid wet into wet on primed linen; strokes blend where they meet."
                .into(),
            params: params(&[("load", 0.8), ("thickness", 0.45), ("blend", 0.45)]),
            ground: Some(Texture::Linen),
            palette: oil_palette(),
        },
        Preset {
            id: "oil-impasto".into(),
            label: "Oil, impasto".into(),
            engine: "oil".into(),
            description: "Heavy, loaded paint that stands in lit ridges; for highlights and texture."
                .into(),
            params: params(&[
                ("load", 1.3),
                ("thickness", 1.0),
                ("bristles", 14.0),
                ("streak", 0.6),
                ("relief", 0.9),
                ("blend", 0.25),
            ]),
            ground: Some(Texture::Linen),
            palette: oil_palette(),
        },
        Preset {
            id: "oil-glaze".into(),
            label: "Oil, scumble and dry brush".into(),
            engine: "oil".into(),
            description: "A lightly loaded brush that drags broken colour over the weave; for skies, haze and texture."
                .into(),
            params: params(&[
                ("load", 0.25),
                ("thickness", 0.15),
                ("bristles", 30.0),
                ("streak", 0.7),
                ("blend", 0.2),
            ]),
            ground: Some(Texture::Linen),
            palette: oil_palette(),
        },
        Preset {
            id: "watercolour".into(),
            label: "Watercolour, washes".into(),
            engine: "watercolour".into(),
            description: "Transparent washes on cold-pressed paper with dark dried edges and granulating blues and earths."
                .into(),
            params: params(&[("water", 0.6), ("flow", 0.45), ("edge", 0.6)]),
            ground: Some(Texture::ColdPress),
            palette: watercolour_palette(),
        },
        Preset {
            id: "watercolour-wet".into(),
            label: "Watercolour, wet-in-wet".into(),
            engine: "watercolour".into(),
            description: "Very wet paint that blooms and bleeds into what is still wet; soft skies and backgrounds."
                .into(),
            params: params(&[
                ("water", 1.0),
                ("flow", 0.95),
                ("edge", 0.35),
                ("roughness", 0.7),
            ]),
            ground: Some(Texture::ColdPress),
            palette: watercolour_palette(),
        },
        Preset {
            id: "watercolour-dry".into(),
            label: "Watercolour, dry brush".into(),
            engine: "watercolour".into(),
            description: "Little water and crisp edges on rough paper; for detail, grasses and sparkle on water."
                .into(),
            params: params(&[
                ("water", 0.1),
                ("flow", 0.05),
                ("edge", 0.8),
                ("granulation", 0.8),
                ("roughness", 0.3),
            ]),
            ground: Some(Texture::Rough),
            palette: watercolour_palette(),
        },
    ]
}
