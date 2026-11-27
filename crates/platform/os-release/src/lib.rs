//! The `os-release` format: the shell-like `KEY=value` assignments
//! `/etc/os-release` and the `extension-release` files are written in, read
//! strictly so a value the format does not allow is refused, not misread.

use std::collections::BTreeMap;

/// Every assignment in `text`, by key, with quotes removed and escapes
/// resolved; `#` comments and blank lines are skipped.
///
/// # Errors
/// A line that is not `KEY=value` in the format's quoting rules, a key that is
/// not an uppercased name, a repeated key, or a control character in a value.
pub fn parse(text: &str) -> Result<BTreeMap<String, String>, String> {
    let mut fields = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, raw) = line
            .split_once('=')
            .ok_or("Malformed os-release assignment.")?;
        if key.is_empty()
            || !key
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            || key.as_bytes().first().is_some_and(u8::is_ascii_digit)
        {
            return Err("Invalid os-release key.".into());
        }
        let value = parse_value(raw)?;
        if fields.insert(key.to_owned(), value).is_some() {
            return Err("Duplicate os-release key.".into());
        }
    }
    Ok(fields)
}

/// The fields a release is selected by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OsRelease {
    pub id: String,
    /// Missing is distinct from a version; only an exact `ID=arch` means rolling.
    pub version_id: Option<String>,
    pub name: Option<String>,
}

impl OsRelease {
    /// The release `text` describes, checked the way selecting one needs: a
    /// valid `ID`, a `VERSION_ID` free of padding and control characters, and a
    /// display `NAME` only. Whatever [`parse`] refuses is refused here too.
    ///
    /// # Errors
    /// `ID` missing or invalid, or `VERSION_ID` invalid.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut fields = parse(text)?;
        let id = fields.remove("ID").ok_or("Missing os-release ID.")?;
        if id.is_empty()
            || !id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
        {
            return Err("Invalid os-release ID.".into());
        }
        let version_id = fields.remove("VERSION_ID");
        if version_id
            .as_ref()
            .is_some_and(|v| v.is_empty() || v.trim() != v || v.chars().any(char::is_control))
        {
            return Err("Invalid os-release VERSION_ID.".into());
        }
        // Only a display name: never used to choose a release.
        let name = fields
            .remove("NAME")
            .filter(|name| !name.trim().is_empty() && !name.chars().any(char::is_control));
        Ok(Self {
            id,
            version_id,
            name,
        })
    }
}

fn parse_value(raw: &str) -> Result<String, String> {
    let mut chars = raw.chars();
    let quote = chars.next().filter(|c| matches!(c, '\'' | '"'));
    let Some(quote) = quote else {
        return if raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-/".contains(&b))
        {
            Ok(raw.to_owned())
        } else {
            Err("Malformed unquoted os-release value.".into())
        };
    };
    let mut result = String::new();
    while let Some(c) = chars.next() {
        if c == quote {
            return if chars.next().is_none() {
                Ok(result)
            } else {
                Err("Trailing os-release data.".into())
            };
        }
        if c.is_control() {
            return Err("Control character in os-release.".into());
        }
        if c == '\\' && quote == '"' {
            let escaped = chars.next().ok_or("Unterminated os-release escape.")?;
            if !matches!(escaped, '$' | '`' | '"' | '\\') {
                result.push('\\');
            }
            result.push(escaped);
        } else {
            result.push(c);
        }
    }
    Err("Unterminated os-release quote.".into())
}

#[cfg(test)]
mod tests {
    use super::{OsRelease, parse};

    #[test]
    fn assignments_read_literally_and_ambiguous_ones_are_refused() {
        let fields = parse(
            "# Fedora fixture\nNAME=\"Fedora Linux\"\nID='fedora'\nVERSION_ID=\"44\"\nPRETTY_NAME=\"Fedora \\\"44\\\" \\$HOME\"\n",
        )
        .unwrap();
        assert_eq!(fields["ID"], "fedora");
        assert_eq!(fields["NAME"], "Fedora Linux");
        assert_eq!(fields["VERSION_ID"], "44");
        assert_eq!(fields["PRETTY_NAME"], "Fedora \"44\" $HOME");
        for text in [
            "ID=fedora\nID=fedora\n",
            "ID=fedora\nVERSION_ID=44\nVERSION_ID=45\n",
            "ID=\"fedora",
            "ID=fedora extra",
            "export ID=fedora",
            "ID=$(whoami)",
            "ID=fedora;true",
            "ID=fedora\nBROKEN",
            "ID=fedora\nVERSION_ID=\"44\"junk",
        ] {
            assert!(parse(text).is_err(), "accepted {text:?}");
        }
        let release =
            OsRelease::parse("NAME=\"Fedora Linux\"\nID=fedora\nVERSION_ID=44\n").unwrap();
        assert_eq!(release.id, "fedora");
        assert_eq!(release.version_id.as_deref(), Some("44"));
        assert_eq!(release.name.as_deref(), Some("Fedora Linux"));
        assert!(OsRelease::parse("").is_err(), "no ID");
        assert!(OsRelease::parse("ID=Fedora").is_err(), "invalid ID");
        assert!(
            OsRelease::parse("ID=fedora\nVERSION_ID=\" 44\"").is_err(),
            "padded version"
        );
        assert_eq!(OsRelease::parse("ID=fedora\n").unwrap().version_id, None);
    }
}
