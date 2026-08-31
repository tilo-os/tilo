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
use settings_store::hardware::KeyboardInputSource;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CatalogEntry {
    pub id: String,
    pub label: String,
}

pub(crate) fn installed_catalog() -> Vec<CatalogEntry> {
    for path in [
        Path::new("/usr/share/X11/xkb/rules/evdev.xml"),
        Path::new("/usr/share/X11/xkb/rules/base.xml"),
    ] {
        if let Ok(entries) = catalog_from_path(path)
            && !entries.is_empty()
        {
            return entries;
        }
    }
    fallback_catalog()
}

fn catalog_from_path(path: &Path) -> Result<Vec<CatalogEntry>, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut reader = Reader::from_reader(BufReader::new(file));
    reader.config_mut().trim_text(true);
    parse_catalog(&mut reader)
}

fn parse_catalog<R: std::io::BufRead>(reader: &mut Reader<R>) -> Result<Vec<CatalogEntry>, String> {
    let mut buf = Vec::new();
    let mut tag = Vec::<u8>::new();
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
                tag = start.name().as_ref().to_vec();
                match tag.as_slice() {
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
            Ok(Event::Text(text)) if in_layout => {
                let value = text.decode().map_err(|e| e.to_string())?.into_owned();
                match (in_variant, tag.as_slice()) {
                    (true, b"name") => variant_name = value,
                    (true, b"description") => variant_description = value,
                    (false, b"name") => layout_name = value,
                    (false, b"description") => layout_description = value,
                    _ => {}
                }
            }
            Ok(Event::End(end)) => match end.name().as_ref() {
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
            entry.label = format!("{} · {}", entry.label, entry.id);
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

    #[test]
    fn parses_base_layouts_and_variants_from_standard_shape() {
        let xml = br"<xkbConfigRegistry><layoutList><layout><configItem>
            <name>us</name><description>English (US)</description>
          </configItem><variantList><variant><configItem>
            <name>dvorak</name><description>Dvorak</description>
          </configItem></variant></variantList></layout></layoutList></xkbConfigRegistry>";
        let mut reader = Reader::from_reader(&xml[..]);
        reader.config_mut().trim_text(true);
        let entries = parse_catalog(&mut reader).unwrap();
        assert_eq!(
            entries,
            [
                CatalogEntry {
                    id: "us".into(),
                    label: "English (US)".into(),
                },
                CatalogEntry {
                    id: "us:dvorak".into(),
                    label: "English (US) — Dvorak".into(),
                },
            ]
        );
    }

    #[test]
    fn installed_catalog_contains_a_real_us_layout() {
        assert!(installed_catalog().iter().any(|entry| entry.id == "us"));
    }
}
