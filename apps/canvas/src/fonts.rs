//! The fonts text marks can use: the families Tilo ships, the same on every
//! machine, so an artwork's lettering looks alike wherever it is opened.

use std::sync::OnceLock;

/// The generic names a text's `font` may use, and the family each is.
pub const GENERIC: [(&str, &str); 3] = [
    ("sans", ui::UI_SANS_FAMILY),
    ("serif", ui::UI_SERIF_FAMILY),
    ("mono", ui::UI_MONOSPACE_FAMILY),
];

/// The families Tilo ships, in catalog order.
pub fn families() -> &'static [String] {
    static FAMILIES: OnceLock<Vec<String>> = OnceLock::new();
    FAMILIES.get_or_init(|| {
        let mut families: Vec<String> = Vec::new();
        for asset in tilo_font_catalog::manifest()
            .map(|manifest| manifest.font.as_slice())
            .unwrap_or_default()
            .iter()
            .filter(|asset| asset.scope == tilo_font_catalog::Scope::Product)
        {
            for face in &asset.faces {
                if !families.contains(&face.family) {
                    families.push(face.family.clone());
                }
            }
        }
        families
    })
}

/// The shipped family `font` names (a generic name or a family, any case).
pub fn family(font: &str) -> Option<&'static str> {
    let font = font.trim();
    if let Some((_, family)) = GENERIC.iter().find(|(name, _)| *name == font) {
        return Some(family);
    }
    families()
        .iter()
        .find(|family| family.eq_ignore_ascii_case(font))
        .map(String::as_str)
}

/// Whether `font` names a font text can use.
pub fn known(font: &str) -> bool {
    family(font).is_some()
}

/// The closest font for a family another program named: the shipped family
/// of that name, else the generic kind its name suggests.
pub fn nearest(font: &str) -> String {
    if let Some(family) = family(font) {
        return GENERIC
            .iter()
            .find(|(_, generic)| *generic == family)
            .map_or_else(|| family.to_owned(), |(name, _)| (*name).to_owned());
    }
    let lower = font.to_ascii_lowercase();
    if ["mono", "courier", "consol", "code"]
        .iter()
        .any(|word| lower.contains(word))
    {
        "mono".into()
    } else if !lower.contains("sans")
        && ["serif", "times", "georgia", "garamond", "roman", "book"]
            .iter()
            .any(|word| lower.contains(word))
    {
        "serif".into()
    } else {
        "sans".into()
    }
}

/// The fonts as `media` lists them.
pub fn listing() -> serde_json::Value {
    serde_json::json!({
        "generic": GENERIC.iter().map(|(name, family)| (name.to_string(), serde_json::Value::from(*family))).collect::<serde_json::Map<_, _>>(),
        "families": families(),
        "weights": "100-900 accepted; Noto Sans, Serif and Sans Mono draw 400-700 continuously, lighter or heavier clamps",
        "italic": "Noto Sans and Noto Serif",
    })
}
