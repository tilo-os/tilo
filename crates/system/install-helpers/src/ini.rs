//! `configparser.ConfigParser(interpolation=None, strict=True)` as the
//! helpers read bootc's `.origin` files: `[section]` headers, `key = value`
//! or `key: value` options with lowercased keys, full-line `#`/`;` comments,
//! indented continuation lines, and a refusal for a repeated section or
//! option or a line that is none of these.

use std::fmt::Write as _;

use crate::{Error, Result};

const DEFAULT: &str = "DEFAULT";

/// One section: its options in order, each with its value's lines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Section {
    name: String,
    options: Vec<(String, Vec<String>)>,
}

/// One parsed file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ini {
    sections: Vec<Section>,
    defaults: Section,
}

fn leading_whitespace(line: &str) -> usize {
    line.len()
        .saturating_sub(line.trim_start_matches(crate::py::is_space).len())
}

/// `\[(?P<header>.+)\]` matched at the start: up to the last `]` after at
/// least one character.
fn header(value: &str) -> Option<&str> {
    let rest = value.strip_prefix('[')?;
    let end = rest.rfind(']')?;
    let name = rest.get(..end)?;
    (!name.is_empty()).then_some(name)
}

/// `(?P<option>.*?)\s*[=:]\s*(?P<value>.*)`: the key before the first
/// delimiter and the value after it.
fn option(value: &str) -> Option<(String, &str)> {
    let delimiter = value.find(['=', ':'])?;
    let key = crate::py::strip(value.get(..delimiter)?).to_lowercase();
    let content = crate::py::strip(value.get(delimiter.saturating_add(1)..)?);
    (!key.is_empty()).then_some((key, content))
}

fn parsing_error(number: usize, raw: &str) -> Error {
    Error::new(format!(
        "Source contains parsing errors: '<string>'\n\t[line {:2}]: {raw:?}",
        number.saturating_add(1)
    ))
}

/// The section lines go to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Place {
    /// Before the first header.
    #[default]
    Nowhere,
    Default,
    Section(usize),
}

/// Where the parse is, and whether an option's value may still continue.
#[derive(Debug, Clone, Copy, Default)]
struct Cursor {
    place: Place,
    continuing: bool,
    indent: usize,
}

impl Ini {
    fn section_mut(&mut self, place: Place) -> Option<&mut Section> {
        match place {
            Place::Nowhere => None,
            Place::Default => Some(&mut self.defaults),
            Place::Section(at) => self.sections.get_mut(at),
        }
    }

    fn continue_value(&mut self, cursor: Cursor, line: &str) {
        if let Some((_, lines)) = self
            .section_mut(cursor.place)
            .and_then(|section| section.options.last_mut())
        {
            lines.push(line.to_owned());
        }
    }

    /// Reads one line that is neither empty nor a comment.
    fn line(&mut self, cursor: &mut Cursor, number: usize, raw: &str, value: &str) -> Result<()> {
        let level = leading_whitespace(raw);
        if cursor.place != Place::Nowhere && cursor.continuing && level > cursor.indent {
            self.continue_value(*cursor, value);
            return Ok(());
        }
        cursor.indent = level;
        if let Some(name) = header(value) {
            cursor.continuing = false;
            if name == DEFAULT {
                cursor.place = Place::Default;
                return Ok(());
            }
            if self.sections.iter().any(|section| section.name == name) {
                return Err(Error::new(format!(
                    "While reading from '<string>' [line {:2}]: section '{name}' already exists",
                    number.saturating_add(1)
                )));
            }
            self.sections.push(Section {
                name: name.to_owned(),
                options: Vec::new(),
            });
            cursor.place = Place::Section(self.sections.len().saturating_sub(1));
            return Ok(());
        }
        if cursor.place == Place::Nowhere {
            return Err(Error::new(format!(
                "File contains no section headers.\nfile: '<string>', line: {}\n{raw:?}",
                number.saturating_add(1)
            )));
        }
        let (key, content) = option(value).ok_or_else(|| parsing_error(number, raw))?;
        let Some(section) = self.section_mut(cursor.place) else {
            return Err(parsing_error(number, raw));
        };
        if section.options.iter().any(|(existing, _)| *existing == key) {
            return Err(Error::new(format!(
                "While reading from '<string>' [line {:2}]: option '{key}' in section '{}' already exists",
                number.saturating_add(1),
                section.name
            )));
        }
        section.options.push((key, vec![content.to_owned()]));
        cursor.continuing = true;
        Ok(())
    }

    /// Parses `text`.
    pub fn parse(text: &str) -> Result<Self> {
        let mut ini = Self {
            defaults: Section {
                name: DEFAULT.into(),
                options: Vec::new(),
            },
            ..Self::default()
        };
        let mut cursor = Cursor {
            indent: usize::MAX,
            ..Cursor::default()
        };
        let text = crate::py::universal_newlines(text);
        for (number, raw) in text.split('\n').enumerate() {
            let stripped = crate::py::strip(raw);
            if stripped.starts_with('#') || stripped.starts_with(';') {
                continue;
            }
            if stripped.is_empty() {
                // An empty line inside a value is part of it.
                if cursor.continuing {
                    ini.continue_value(cursor, "");
                }
                continue;
            }
            ini.line(&mut cursor, number, raw, stripped)?;
        }
        Ok(ini)
    }

    /// `get(section, option)`: the value's lines joined, trailing whitespace
    /// removed.
    ///
    /// # Errors
    /// No such section, or no such option in it or in `DEFAULT`.
    pub fn get(&self, section: &str, option: &str) -> Result<String> {
        let Some(found) = self.sections.iter().find(|s| s.name == section) else {
            return Err(Error::new(format!("No section: '{section}'")));
        };
        let key = option.to_lowercase();
        found
            .options
            .iter()
            .chain(&self.defaults.options)
            .find(|(name, _)| *name == key)
            .map(|(_, lines)| {
                lines
                    .join("\n")
                    .trim_end_matches(crate::py::is_space)
                    .to_owned()
            })
            .ok_or_else(|| Error::new(format!("No option '{key}' in section: '{section}'")))
    }

    /// `set(section, option, value)`: replaces the option's value in place,
    /// or adds the option at the end of the section.
    ///
    /// # Errors
    /// No such section.
    pub fn set(&mut self, section: &str, option: &str, value: &str) -> Result<()> {
        let Some(found) = self.sections.iter_mut().find(|s| s.name == section) else {
            return Err(Error::new(format!("No section: '{section}'")));
        };
        let key = option.to_lowercase();
        let lines = vec![value.to_owned()];
        match found.options.iter_mut().find(|(name, _)| *name == key) {
            Some((_, existing)) => *existing = lines,
            None => found.options.push((key, lines)),
        }
        Ok(())
    }

    /// The text `write()` writes: each section as `[name]`, its options as
    /// `key = value` (continuation lines indented by a tab), then a blank
    /// line; `DEFAULT` first when it has options.
    #[must_use]
    pub fn write(&self) -> String {
        let mut out = String::new();
        let defaults = (!self.defaults.options.is_empty()).then_some(&self.defaults);
        for section in defaults.into_iter().chain(&self.sections) {
            let _ = writeln!(out, "[{}]", section.name);
            for (key, lines) in &section.options {
                let value = lines.join("\n");
                let value = value
                    .trim_end_matches(crate::py::is_space)
                    .replace('\n', "\n\t");
                let _ = writeln!(out, "{key} = {value}");
            }
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failed fixture call fails the test")]

    use super::*;

    #[test]
    fn reads_origins_as_configparser_did() {
        let ini = Ini::parse(
            "# bootc\n[image]\nManifest_Digest = sha256:a\n[origin]\ncontainer-image-reference: x\n  continued\n\n[boot]\nboot_type=bls\ndigest=d\n",
        )
        .unwrap();
        assert_eq!(ini.get("image", "manifest_digest").unwrap(), "sha256:a");
        assert_eq!(
            ini.get("origin", "container-image-reference").unwrap(),
            "x\ncontinued"
        );
        assert_eq!(ini.get("boot", "boot_type").unwrap(), "bls");
        assert!(ini.get("image", "missing").is_err());
        assert!(ini.get("missing", "x").is_err());
    }

    #[test]
    fn strict_refusals() {
        assert!(Ini::parse("[a]\nx=1\n[a]\n").is_err());
        assert!(Ini::parse("[a]\nx=1\nX=2\n").is_err());
        assert!(Ini::parse("x=1\n").is_err());
        assert!(Ini::parse("[a]\njunk\n").is_err());
        assert!(Ini::parse("[a]\n=1\n").is_err());
    }

    #[test]
    fn writes_what_configparser_wrote() {
        let mut ini = Ini::parse(
            "[origin]\ncontainer-image-reference=x\n  continued\n[boot]\nboot_type=bls\ndigest=d\n",
        )
        .unwrap();
        ini.set("boot", "boot_type", "uki").unwrap();
        assert!(ini.set("missing", "x", "y").is_err());
        assert_eq!(
            ini.write(),
            "[origin]\ncontainer-image-reference = x\n\tcontinued\n\n[boot]\nboot_type = uki\ndigest = d\n\n"
        );
    }

    #[test]
    fn defaults_apply_to_every_section() {
        let ini = Ini::parse("[DEFAULT]\nk=v\n[a]\n").unwrap();
        assert_eq!(ini.get("a", "k").unwrap(), "v");
    }
}
