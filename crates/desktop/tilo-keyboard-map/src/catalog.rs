//! The keyboard layouts this device has: xkeyboard-config's own list of layouts and their
//! variants, each by the name it is typed by (`de`, `us:intl`), and a short built-in list where
//! the device's list cannot be read.

use std::collections::{HashMap, HashSet};
use std::io::BufReader;
use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::Event;

/// One layout or variant a person can type with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Its source id: `de`, `us:dvorak`.
    pub id: String,
    /// What it calls itself: `German`, `English (US) — Dvorak`.
    pub label: String,
}

/// The layouts installed on this device and, when its list could not be read, why: `layouts`
/// is then [`fallback`]. One read of xkeyboard-config's rules and symbols.
#[must_use]
pub fn installed() -> (Vec<Layout>, Option<String>) {
    match read_installed() {
        Ok(layouts) => (layouts, None),
        Err(error) => (fallback(), Some(error)),
    }
}

fn read_installed() -> Result<Vec<Layout>, String> {
    let mut errors = Vec::new();
    for path in [
        Path::new("/usr/share/X11/xkb/rules/evdev.xml"),
        Path::new("/usr/share/X11/xkb/rules/base.xml"),
    ] {
        match read_rules(path) {
            Ok(layouts) if !layouts.is_empty() => {
                let installed = layouts
                    .into_iter()
                    .filter_map(|layout| {
                        let source = parse_source(&layout.id)?;
                        match super::is_installed(&source.0) {
                            Ok(true) => Some(Ok(layout)),
                            Ok(false) => None,
                            Err(error) => Some(Err(error.to_string())),
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if !installed.is_empty() {
                    return Ok(installed);
                }
                errors.push(format!("{}: no installed symbols", path.display()));
            }
            Ok(_) => errors.push(format!("{}: no layouts", path.display())),
            Err(error) => errors.push(format!("{}: {error}", path.display())),
        }
    }
    Err(errors.join("; "))
}

fn read_rules(path: &Path) -> Result<Vec<Layout>, String> {
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    parse(&mut Reader::from_reader(BufReader::new(file)))
}

/// `id` as a layout and its variant, when it is one: names the XKB compiler accepts.
pub(crate) fn parse_source(id: &str) -> Option<(String, String)> {
    let (layout, variant) = match id.split_once(':') {
        Some((layout, variant)) if !variant.is_empty() => (layout, variant),
        // A colon with nothing after it names no variant, which no layout is typed by.
        Some(_) => return None,
        None => (id, ""),
    };
    if !valid(layout) || !variant.is_empty() && !valid(variant) {
        return None;
    }
    Some((layout.to_owned(), variant.to_owned()))
}

fn valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn resolve_reference(reference: &quick_xml::events::BytesRef<'_>) -> Option<char> {
    if let Ok(Some(character)) = reference.resolve_char_ref() {
        return Some(character);
    }
    let name = reference.decode().ok()?;
    quick_xml::escape::resolve_predefined_entity(&name)?
        .chars()
        .next()
}

/// The layouts and variants `reader` describes, as xkeyboard-config's rules write them.
fn parse<R: std::io::BufRead>(reader: &mut Reader<R>) -> Result<Vec<Layout>, String> {
    // Trimming each text event would also eat the spaces next to a reference
    // ("A &amp; B" is three events); the collected value is trimmed instead.
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut text = String::new();
    let mut in_layout = false;
    let mut in_variant = false;
    let mut layout_name = String::new();
    let mut layout_description = String::new();
    let mut variant_name = String::new();
    let mut variant_description = String::new();
    let mut variants = Vec::<(String, String)>::new();
    let mut layouts = Vec::new();

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(start)) => {
                text.clear();
                match start.name().as_ref() {
                    b"layout" => {
                        in_layout = true;
                        layout_name.clear();
                        layout_description.clear();
                        variants.clear();
                    }
                    b"variant" if in_layout => {
                        in_variant = true;
                        variant_name.clear();
                        variant_description.clear();
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(chunk)) if in_layout => {
                text.push_str(&chunk.xml10_content().map_err(|error| error.to_string())?);
            }
            Ok(Event::GeneralRef(reference)) if in_layout => {
                text.extend(resolve_reference(&reference));
            }
            Ok(Event::End(end)) => match end.name().as_ref() {
                b"name" | b"description" if in_layout => {
                    let value = text.trim().to_owned();
                    match (in_variant, end.name().as_ref()) {
                        (true, b"name") => variant_name = value,
                        (true, _) => variant_description = value,
                        (false, b"name") => layout_name = value,
                        (false, _) => layout_description = value,
                    }
                }
                b"variant" if in_variant => {
                    if !variant_name.is_empty() {
                        variants.push((variant_name.clone(), variant_description.clone()));
                    }
                    in_variant = false;
                }
                b"layout" if in_layout => {
                    push(&mut layouts, &layout_name, None, &layout_description);
                    for (variant, description) in &variants {
                        let label = if description.is_empty() {
                            format!("{layout_description} — {variant}")
                        } else {
                            format!("{layout_description} — {description}")
                        };
                        push(&mut layouts, &layout_name, Some(variant), &label);
                    }
                    in_layout = false;
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(error) => return Err(error.to_string()),
            _ => {}
        }
        buffer.clear();
    }

    let mut seen = HashSet::new();
    layouts.retain(|layout| seen.insert(layout.id.clone()));
    layouts.sort_by_key(|layout| layout.label.to_lowercase());
    let mut counts = HashMap::new();
    for layout in &layouts {
        *counts.entry(layout.label.clone()).or_insert(0usize) += 1;
    }
    for layout in &mut layouts {
        if counts.get(&layout.label).copied().unwrap_or(0) > 1 {
            layout.label = format!("{} ({})", layout.label, layout.id);
        }
    }
    Ok(layouts)
}

fn push(layouts: &mut Vec<Layout>, layout: &str, variant: Option<&str>, label: &str) {
    let raw = variant.map_or_else(
        || layout.to_owned(),
        |variant| format!("{layout}:{variant}"),
    );
    if parse_source(&raw).is_none() {
        return;
    }
    let label = label.trim();
    layouts.push(Layout {
        id: raw,
        label: if label.is_empty() {
            layout.to_owned()
        } else {
            label.to_owned()
        },
    });
}

/// The layouts offered where the device's own list cannot be read: the ones nearly every
/// machine has.
#[must_use]
pub fn fallback() -> Vec<Layout> {
    [
        ("us", "English (US)"),
        ("gb", "English (UK)"),
        ("us:colemak", "English (US) — Colemak"),
        ("us:dvorak", "English (US) — Dvorak"),
        ("de", "German"),
        ("fr", "French"),
        ("nl", "Dutch"),
        ("es", "Spanish"),
        ("it", "Italian"),
        ("pt", "Portuguese"),
        ("br", "Portuguese (Brazil)"),
        ("pl", "Polish"),
        ("ru", "Russian"),
        ("se", "Swedish"),
        ("no", "Norwegian"),
        ("dk", "Danish"),
    ]
    .into_iter()
    .map(|(id, label)| Layout {
        id: id.to_owned(),
        label: label.to_owned(),
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Layouts and their variants are read with their names, with references resolved and a
    /// variant's label standing under its layout's.
    #[test]
    fn the_rules_list_layouts_and_their_variants() {
        let cases: [(&str, &[u8], &str, &str); 2] = [
            (
                "standard shape",
                br"<xkbConfigRegistry><layoutList><layout><configItem>
            <name>us</name><description>English (US)</description>
          </configItem><variantList><variant><configItem>
            <name>dvorak</name><description>Dvorak</description>
          </configItem></variant></variantList></layout></layoutList></xkbConfigRegistry>",
                "English (US)",
                "English (US) — Dvorak",
            ),
            (
                "entity and character references",
                br"<xkbConfigRegistry><layoutList><layout><configItem>
            <name>us</name><description>English (US) &amp; &lt;intl&gt; &#x1F600;</description>
          </configItem><variantList><variant><configItem>
            <name>dvorak</name><description>Dvorak &amp;&#32;more</description>
          </configItem></variant></variantList></layout></layoutList></xkbConfigRegistry>",
                "English (US) & <intl> \u{1F600}",
                "English (US) & <intl> \u{1F600} — Dvorak & more",
            ),
        ];
        for (case, xml, base, variant) in cases {
            let layouts = parse(&mut Reader::from_reader(xml)).unwrap();
            assert_eq!(
                layouts,
                [
                    Layout {
                        id: "us".into(),
                        label: base.into(),
                    },
                    Layout {
                        id: "us:dvorak".into(),
                        label: variant.into(),
                    },
                ],
                "case {case}"
            );
        }
    }

    /// A machine without xkeyboard-config still offers a short list, and every id it offers is
    /// one the compiler would accept.
    #[test]
    fn the_fallback_list_names_layouts_the_compiler_accepts() {
        let layouts = fallback();
        assert!(layouts.iter().any(|layout| layout.id == "us"));
        for layout in &layouts {
            let (source, _) = parse_source(&layout.id).expect("a source id");
            assert!(super::valid(&source), "{}", layout.id);
        }
        assert_eq!(parse_source("../evil"), None);
        assert_eq!(parse_source("us:"), None, "an empty variant is no variant");
        assert_eq!(
            parse_source("us:dvorak"),
            Some(("us".into(), "dvorak".into()))
        );
    }
}
