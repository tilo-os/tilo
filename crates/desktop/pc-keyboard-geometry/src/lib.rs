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

/// How much room a cut drawing keeps around its outermost keys, in millimetres.
const ROOM: f64 = 1.5;

/// How far down the function row sits in the compact drawing, so it meets the digits.
const FUNCTION_ROW_DROP: f64 = 20.0;

/// `geometry` as a laptop's keyboard: the main block and one row of arrows. The navigation
/// cluster, the numpad and the right Control are left out and the function row moves down to
/// meet the digits, so a laptop's preview matches the laptop and every legend stays readable.
#[must_use]
pub fn compact(geometry: &Geometry) -> Vec<Key> {
    const DROPPED: [&str; 12] = [
        "PRSC", "SCLK", "PAUS", "INS", "HOME", "PGUP", "DELE", "END", "PGDN", "NMLK", "RWIN",
        "MENU",
    ];
    let arrows = [
        ("LEFT", 230.0),
        ("UP", 249.0),
        ("DOWN", 268.0),
        ("RGHT", 287.0),
    ];
    let mut keys = Vec::with_capacity(geometry.keys.len());
    for key in &geometry.keys {
        if DROPPED.contains(&key.name.as_str())
            || key.name.starts_with("KP")
            || key.name == "RCTL"
            || arrows.iter().any(|(name, _)| *name == key.name)
        {
            continue;
        }
        let mut key = key.clone();
        if key.name == "ESC" || key.name.starts_with("FK") {
            for point in &mut key.outline {
                point[1] += FUNCTION_ROW_DROP;
            }
        }
        keys.push(key);
    }
    for (name, x0) in arrows {
        keys.push(Key {
            name: name.to_owned(),
            outline: vec![
                [x0, 138.0],
                [x0 + 19.0, 138.0],
                [x0 + 19.0, 156.0],
                [x0, 156.0],
            ],
        });
    }
    keys
}

/// `keys` moved so their outermost point sits [`ROOM`] from the origin, with the width and
/// height of the box that holds them: what a drawing is cut to.
#[must_use]
pub fn tight(mut keys: Vec<Key>) -> (f64, f64, Vec<Key>) {
    let (mut x0, mut y0) = (f64::MAX, f64::MAX);
    let (mut x1, mut y1) = (f64::MIN, f64::MIN);
    for point in keys.iter().flat_map(|key| key.outline.iter()) {
        x0 = x0.min(point[0]);
        y0 = y0.min(point[1]);
        x1 = x1.max(point[0]);
        y1 = y1.max(point[1]);
    }
    if x0 > x1 || y0 > y1 {
        return (1.0, 1.0, keys);
    }
    for key in &mut keys {
        for point in &mut key.outline {
            *point = [point[0] - x0 + ROOM, point[1] - y0 + ROOM];
        }
    }
    (x1 - x0 + 2.0 * ROOM, y1 - y0 + 2.0 * ROOM, keys)
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

    /// A laptop's drawing keeps the main block, one row of arrows and nothing of the cluster
    /// beside it, and is cut to the keys it holds.
    #[test]
    fn the_compact_drawing_is_the_main_block_with_arrows_and_fits_its_keys() {
        let models = models().unwrap();
        let keys = compact(&models["pc105"]);
        for gone in ["PRSC", "PGUP", "NMLK", "KP5", "RCTL", "MENU"] {
            assert!(keys.iter().all(|key| key.name != gone), "{gone} stays out");
        }
        for kept in ["ESC", "AD06", "SPCE", "LEFT", "UP", "DOWN", "RGHT"] {
            assert!(keys.iter().any(|key| key.name == kept), "{kept} stays in");
        }
        let (width, height, keys) = tight(keys);
        let points = || keys.iter().flat_map(|key| key.outline.iter());
        let min_x = points().map(|point| point[0]).fold(f64::MAX, f64::min);
        let max_x = points().map(|point| point[0]).fold(f64::MIN, f64::max);
        let min_y = points().map(|point| point[1]).fold(f64::MAX, f64::min);
        let max_y = points().map(|point| point[1]).fold(f64::MIN, f64::max);
        assert!((min_x - ROOM).abs() < 1e-9, "{min_x}");
        assert!((min_y - ROOM).abs() < 1e-9, "{min_y}");
        assert!((max_x - (width - ROOM)).abs() < 1e-9, "{max_x} in {width}");
        assert!(
            (max_y - (height - ROOM)).abs() < 1e-9,
            "{max_y} in {height}"
        );
        assert!(height < 140.0, "the model's margins are gone: {height}");
    }
}
