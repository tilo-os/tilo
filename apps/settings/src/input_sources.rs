//! Installed XKB input-source catalogue.
//!
//! `/usr/share/X11/xkb/rules/evdev.xml` is the authority for layouts and
//! variants supported by this image. Settings reads it as untrusted data,
//! validates every resulting id through the shared compositor contract and
//! falls back to a small useful set only when the standard catalogue is not
//! installed. No layout is applied here; the per-user compositor owns that.

use std::collections::{HashMap, HashSet};
use std::io::BufReader;
use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::Event;
use settings_store::hardware::{InputMethodEngine, KeyboardInputSource};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CatalogEntry {
    pub id: String,
    pub label: String,
}

#[cfg(test)]
pub(crate) fn installed_catalog() -> Vec<CatalogEntry> {
    read_installed().0
}

/// The input sources this device offers and, when its layout list could not
/// be read, why — the sources are then a short built-in list. One read of
/// the catalogue: it parses the XKB rules file and checks each layout's
/// symbols, so it is never repeated for the error alone.
pub(crate) fn read_installed() -> (Vec<CatalogEntry>, Option<String>) {
    let (mut entries, error) = match read_installed_layouts() {
        Ok(entries) => (entries, None),
        Err(error) => {
            tracing::warn!(target: "tilo::settings", %error, "keyboard layout list unreadable; offering the built-in short list");
            (fallback_catalog(), Some(error))
        }
    };
    entries.extend(input_method_entries(InputMethodEngine::is_installed));
    (entries, error)
}

/// Input-method engines (Rime, Anthy, Hangul) the image ships, listed with
/// the layouts: they are chosen, ordered and switched exactly like them.
fn input_method_entries(installed: impl Fn(InputMethodEngine) -> bool) -> Vec<CatalogEntry> {
    InputMethodEngine::ALL
        .into_iter()
        .filter(|engine| installed(*engine))
        .map(|engine| CatalogEntry {
            id: engine.source_id(),
            label: engine.label().to_string(),
        })
        .collect()
}

/// Whether `id` fits beside `active`: keyboard layouts are bounded by the
/// XKB group limit, input-method engines are not XKB groups.
pub(crate) fn has_room_for(active: &[String], id: &str) -> bool {
    InputMethodEngine::from_source_id(id).is_some() || !layouts_full(active)
}

/// Whether `active` already holds as many keyboard layouts as XKB groups.
pub(crate) fn layouts_full(active: &[String]) -> bool {
    active
        .iter()
        .filter(|source| InputMethodEngine::from_source_id(source).is_none())
        .count()
        >= settings_store::hardware::MAX_KEYBOARD_INPUT_SOURCES
}

/// Why the device's keyboard layout list could not be read, when it could
/// not; the layouts offered are then a short built-in list.
#[cfg(test)]
pub(crate) fn layout_catalog_error() -> Option<String> {
    read_installed_layouts().err()
}

fn read_installed_layouts() -> Result<Vec<CatalogEntry>, String> {
    let mut errors = Vec::new();
    for path in [
        Path::new("/usr/share/X11/xkb/rules/evdev.xml"),
        Path::new("/usr/share/X11/xkb/rules/base.xml"),
    ] {
        match catalog_from_path(path) {
            Ok(entries) if !entries.is_empty() => {
                let installed = entries
                    .into_iter()
                    .filter_map(|entry| {
                        let source = KeyboardInputSource::parse(&entry.id)?;
                        match tilo_keyboard_map::is_installed(&source.layout) {
                            Ok(true) => Some(Ok(entry)),
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

fn catalog_from_path(path: &Path) -> Result<Vec<CatalogEntry>, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    parse_catalog(&mut Reader::from_reader(BufReader::new(file)))
}

/// The character an entity or character reference stands for. quick-xml reports
/// `&amp;`/`&#x2019;` as events of their own rather than inside the text around
/// them. An undeclared name has no character (the catalogue declares no DTD
/// entities) and is dropped, keeping the text on either side.
fn resolve_reference(reference: &quick_xml::events::BytesRef<'_>) -> Option<char> {
    if let Ok(Some(c)) = reference.resolve_char_ref() {
        return Some(c);
    }
    let name = reference.decode().ok()?;
    quick_xml::escape::resolve_predefined_entity(&name)?
        .chars()
        .next()
}

fn parse_catalog<R: std::io::BufRead>(reader: &mut Reader<R>) -> Result<Vec<CatalogEntry>, String> {
    // Trimming each text event would also eat the spaces next to a reference
    // ("A &amp; B" is three events); the collected value is trimmed instead.
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut text = String::new();
    let mut in_layout = false;
    let mut in_variant = false;
    let mut layout_name = String::new();
    let mut layout_description = String::new();
    let mut variant_name = String::new();
    let mut variant_description = String::new();
    let mut variants = Vec::<(String, String)>::new();
    let mut entries = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
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
                text.push_str(&chunk.xml10_content().map_err(|e| e.to_string())?);
            }
            Ok(Event::GeneralRef(reference)) if in_layout => {
                text.extend(resolve_reference(&reference));
            }
            Ok(Event::End(end)) => match end.name().as_ref() {
                b"name" | b"description" if in_layout => {
                    let value = text.trim().to_string();
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
                    push_entry(&mut entries, &layout_name, None, &layout_description);
                    for (variant, description) in &variants {
                        let label = if description.is_empty() {
                            format!("{layout_description} — {variant}")
                        } else {
                            format!("{layout_description} — {description}")
                        };
                        push_entry(&mut entries, &layout_name, Some(variant), &label);
                    }
                    in_layout = false;
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(error) => return Err(error.to_string()),
            _ => {}
        }
        buf.clear();
    }

    let mut seen = HashSet::new();
    entries.retain(|entry| seen.insert(entry.id.clone()));
    entries.sort_by_key(|entry| entry.label.to_lowercase());
    let counts = entries.iter().fold(HashMap::new(), |mut counts, entry| {
        *counts.entry(entry.label.clone()).or_insert(0usize) += 1;
        counts
    });
    for entry in &mut entries {
        if counts.get(&entry.label).copied().unwrap_or(0) > 1 {
            entry.label = format!("{} ({})", entry.label, entry.id);
        }
    }
    Ok(entries)
}

fn push_entry(entries: &mut Vec<CatalogEntry>, layout: &str, variant: Option<&str>, label: &str) {
    let raw = variant.map_or_else(|| layout.to_string(), |v| format!("{layout}:{v}"));
    let Some(source) = KeyboardInputSource::parse(&raw) else {
        return;
    };
    entries.push(CatalogEntry {
        id: source.id,
        label: if label.trim().is_empty() {
            raw
        } else {
            label.trim().to_string()
        },
    });
}

fn fallback_catalog() -> Vec<CatalogEntry> {
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
    .map(|(id, label)| CatalogEntry {
        id: id.into(),
        label: label.into(),
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The xkb catalogue parser yields base layouts and `layout:variant`
    /// entries, resolving entity and character references in descriptions;
    /// the installed catalogue contains a real US layout.
    #[test]
    fn input_source_catalog_parses_layouts_variants_and_references() {
        // (case, registry xml, base label, variant label)
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
            let entries = parse_catalog(&mut Reader::from_reader(xml)).unwrap();
            assert_eq!(
                entries,
                [
                    CatalogEntry {
                        id: "us".into(),
                        label: base.into(),
                    },
                    CatalogEntry {
                        id: "us:dvorak".into(),
                        label: variant.into(),
                    },
                ],
                "case {case}"
            );
        }

        assert!(installed_catalog().iter().any(|entry| entry.id == "us"));
    }

    #[test]
    fn installed_input_method_engines_are_offered_as_sources() {
        let entries = input_method_entries(|engine| {
            engine != settings_store::hardware::InputMethodEngine::Anthy
        });
        assert_eq!(
            entries,
            [
                CatalogEntry {
                    id: "ime/rime".into(),
                    label: "Chinese (Simplified) — Rime".into(),
                },
                CatalogEntry {
                    id: "ime/hangul".into(),
                    label: "Korean — Hangul".into(),
                },
            ],
            "an engine the image lacks is not offered"
        );
    }
}
