//! Physical outlines of the PC keyboard models `pc104`, `pc105`, `jp106` and
//! `kr106`, for drawing a keyboard with its layout's legends.
//!
//! The outlines are derived from xkeyboard-config's `geometry/pc` (The Open
//! Group 1996/1998; provenance and license in `data/`), in millimetres. They
//! say where each key is and what shape it has, never what it types: the
//! symbols come from the native XKB compiler of the layout shown. Reading
//! them is a parse of about 130 KB of JSON, done once, the first time a
//! keyboard is drawn.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde::Deserialize;

/// The outlines as published, a JSON object of models.
pub const SOURCE: &str = include_str!("../data/pc-keyboard-geometry.json");

/// One keyboard model's keys.
#[derive(Clone, Debug, Deserialize)]
pub struct Geometry {
    // The published drawing's `width` and `height` are not read: a drawing
    // is cut to its keys.
    /// Every key of the model.
    pub keys: Vec<Key>,
}

/// One physical key.
#[derive(Clone, Debug, Deserialize)]
pub struct Key {
    /// Its XKB name (`AE01`, `SPCE`, `LSGT`).
    pub name: String,
    /// Its outline, points in millimetres.
    pub outline: Vec<[f64; 2]>,
}

/// Every model, by name.
///
/// # Errors
/// Says the published outlines do not read, which a test of this crate rules
/// out for the data it ships.
pub fn models() -> Result<&'static BTreeMap<String, Geometry>, &'static str> {
    static MODELS: LazyLock<Result<BTreeMap<String, Geometry>, serde_json::Error>> =
        LazyLock::new(|| serde_json::from_str(SOURCE));
    MODELS.as_ref().map_err(|_| "keyboard geometry is invalid")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every model reads, with the keys its name promises.
    #[test]
    fn every_model_reads_with_its_keys() {
        let models = models().unwrap();
        for (model, keys) in [("pc104", 104), ("pc105", 105), ("jp106", 106), ("kr106", 106)] {
            assert_eq!(models[model].keys.len(), keys, "{model}");
        }
    }
}
