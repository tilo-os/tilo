//! Physical outlines of the PC keyboard models `pc104`, `pc105`, `jp106` and `kr106`,
//! for drawing a keyboard with its layout's legends.
//!
//! The outlines are derived from xkeyboard-config's `geometry/pc` (The Open Group
//! 1996/1998; provenance and license in `data/`), in millimetres. They say where each
//! key is and what shape it has, never what it types: the symbols come from the XKB
//! compiler of the layout shown.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde::Deserialize;

/// The outlines as published, a JSON object of models.
pub const SOURCE: &str = include_str!("../data/pc-keyboard-geometry.json");

#[derive(Clone, Debug, Deserialize)]
pub struct Geometry {
    // The published drawing's `width` and `height` are not read: a drawing
    // is cut to its keys.
    pub keys: Vec<Key>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Key {
    pub name: String,
    pub outline: Vec<[f64; 2]>,
}

pub fn models() -> Result<&'static BTreeMap<String, Geometry>, &'static str> {
    static MODELS: LazyLock<Result<BTreeMap<String, Geometry>, serde_json::Error>> =
        LazyLock::new(|| serde_json::from_str(SOURCE));
    MODELS.as_ref().map_err(|_| "keyboard geometry is invalid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_model_reads_with_its_keys() {
        let models = models().unwrap();
        for (model, keys) in [
            ("pc104", 104),
            ("pc105", 105),
            ("jp106", 106),
            ("kr106", 106),
        ] {
            assert_eq!(models[model].keys.len(), keys, "{model}");
        }
    }
}
