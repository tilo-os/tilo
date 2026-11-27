//! A layout's keys as a keyboard draws them: the physical model's outlines with what each key
//! prints at every modifier level, read from the machine's own XKB metadata.
//!
//! A host with no native provider still knows the shapes and places, so it draws the keys with
//! nothing printed on them; what a key types is the layout's to say.

use tilo_ui_base::Keycaps;

/// One corner of a key's outline, in the drawing's units from its top left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// One key of a drawing: where its body is and what it types.
#[derive(Clone, Debug, PartialEq)]
pub struct DrawnKey {
    /// The physical key's XKB name (`AD01`, `SPCE`), which inspection names it by.
    pub name: String,
    /// Its body's corners, in the drawing's units from its top left.
    pub outline: Vec<Point>,
    /// What it types at each modifier level, the base level first: `a`, `A`, `á`, …
    pub levels: Vec<String>,
}

/// A keyboard's keys, drawn for one layout and physical model.
#[derive(Clone, Debug, PartialEq)]
pub struct Drawing {
    /// The source id the keys are drawn for.
    pub layout: String,
    /// The physical model they are laid out as: `pc105`.
    pub model: String,
    /// The width and height of the box the keys fill, in the drawing's units.
    pub width: f64,
    pub height: f64,
    /// How many modifier levels the layout supplies; every key's `levels` has as many.
    pub level_count: i64,
    pub keys: Vec<DrawnKey>,
}

/// The keys of `source` as `model` lays them out, with their legends, cut to their box.
pub fn draw(source: &str, model: &str, options: &str, compact: bool) -> Result<Drawing, String> {
    let (layout, variant) = source
        .split_once(':')
        .map_or((source, ""), |(layout, variant)| (layout, variant));
    let geometries = pc_keyboard_geometry::models()?;
    let geometry = geometries
        .get(model)
        .ok_or_else(|| format!("`{model}` is not a physical keyboard model"))?;
    let keys = if compact {
        pc_keyboard_geometry::compact(geometry)
    } else {
        geometry.keys.clone()
    };
    let (width, height, keys) = pc_keyboard_geometry::tight(keys);
    let names: Vec<String> = keys.iter().map(|key| key.name.clone()).collect();
    let native = match super::compile(layout, variant, model, options, &names) {
        Ok(native) => native,
        // No installed keyboard data on this machine: the drawing's shape and the keys'
        // positions are still known, so a key can be found and inspected; what it prints is
        // the layout's to say, and nothing is printed.
        Err(error) if error.is_unsupported() => {
            return Ok(unlabelled(source, model, width, height, &keys, &names));
        }
        Err(error) => return Err(error.to_string()),
    };
    let keycaps = Keycaps::from_xkb(layout, variant);
    let english = Keycaps::from_xkb("us", "");
    let mut levels = 0;
    let keys = native
        .iter()
        .zip(&keys)
        .map(|(key, geometry)| {
            levels = levels.max(key.levels.len());
            DrawnKey {
                name: key.name.clone(),
                outline: geometry
                    .outline
                    .iter()
                    .map(|&[x, y]| Point { x, y })
                    .collect(),
                levels: key
                    .levels
                    .iter()
                    .map(|symbols| {
                        symbols
                            .iter()
                            .map(|symbol| {
                                let printed = keycaps.native_symbol(&symbol.name, &symbol.text);
                                if printed == english.native_symbol(&symbol.name, &symbol.text) {
                                    short_legend(&symbol.name).map_or(printed, str::to_owned)
                                } else {
                                    printed
                                }
                            })
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .collect(),
            }
        })
        .collect();
    Ok(Drawing {
        layout: source.to_owned(),
        model: model.to_owned(),
        width,
        height,
        level_count: i64::try_from(levels).unwrap_or(0),
        keys,
    })
}

/// The drawing where this machine cannot compile a layout: the keys' shapes and places, with
/// nothing printed on them.
fn unlabelled(
    source: &str,
    model: &str,
    width: f64,
    height: f64,
    keys: &[pc_keyboard_geometry::Key],
    names: &[String],
) -> Drawing {
    Drawing {
        layout: source.to_owned(),
        model: model.to_owned(),
        width,
        height,
        level_count: 1,
        keys: names
            .iter()
            .zip(keys)
            .map(|(name, key)| DrawnKey {
                name: name.clone(),
                outline: key.outline.iter().map(|&[x, y]| Point { x, y }).collect(),
                levels: vec![String::new()],
            })
            .collect(),
    }
}

/// A long key's name in the short form a keycap prints, where the layout does not print a name
/// of its own for it.
fn short_legend(name: &str) -> Option<&'static str> {
    Some(match name {
        "Print" | "Sys_Req" => "PrtSc",
        "Scroll_Lock" => "ScrLk",
        "Pause" | "Break" => "Pause",
        "Num_Lock" => "Num",
        "Prior" | "Page_Up" => "PgUp",
        "Next" | "Page_Down" => "PgDn",
        "Insert" => "Ins",
        "Delete" => "Del",
        "Caps_Lock" => "Caps",
        "BackSpace" => "Bksp",
        "Escape" => "Esc",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A keycap's short name is what a long key prints, and a key the table does not know
    /// prints as itself.
    #[test]
    fn long_key_names_print_short() {
        assert_eq!(short_legend("Escape"), Some("Esc"));
        assert_eq!(short_legend("Page_Up"), Some("PgUp"));
        assert_eq!(short_legend("a"), None);
    }
}
