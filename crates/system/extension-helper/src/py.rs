//! Text handling exactly as the helper has always done it.
//!
//! The status file, the journal lines and the checks on signed manifests were
//! defined by a Python implementation; owners' desktops and existing state
//! files depend on their exact form. These are the few Python behaviours the
//! port relies on: what counts as whitespace and as a line break, how a URL
//! splits, JSON as `json.dumps(indent=2, sort_keys=True)` writes it, and how a
//! value reads in a message.

use std::fmt::Write as _;

use serde_json::Value;
pub use tilo_pycompat::{Dumps, split, splitlines, strip, truthy};

/// The first `count` characters of `text` (`text[:count]`).
pub fn truncate(text: &str, count: usize) -> &str {
    match text.char_indices().nth(count) {
        Some((at, _)) => text.split_at(at).0,
        None => text,
    }
}

/// `len(text)`: characters, not bytes.
pub fn char_len(text: &str) -> usize {
    text.chars().count()
}

/// `urllib.parse.urlsplit(url)`, the parts the helper reads.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SplitUrl {
    pub scheme: String,
    pub netloc: String,
    pub path: String,
    pub query: String,
    pub fragment: String,
}

/// Why `urlsplit` or one of its properties raised `ValueError`.
#[derive(Debug, PartialEq, Eq)]
pub struct UrlValueError(pub String);

const C0_CONTROL_OR_SPACE: &[char] = &[
    '\u{00}', '\u{01}', '\u{02}', '\u{03}', '\u{04}', '\u{05}', '\u{06}', '\u{07}', '\u{08}',
    '\u{09}', '\u{0a}', '\u{0b}', '\u{0c}', '\u{0d}', '\u{0e}', '\u{0f}', '\u{10}', '\u{11}',
    '\u{12}', '\u{13}', '\u{14}', '\u{15}', '\u{16}', '\u{17}', '\u{18}', '\u{19}', '\u{1a}',
    '\u{1b}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{1f}', ' ',
];

/// Split `url` into scheme, network location, path, query and fragment.
///
/// # Errors
/// A network location with unbalanced IPv6 brackets.
pub fn urlsplit(url: &str) -> Result<SplitUrl, UrlValueError> {
    let cleaned: String = url
        .trim_start_matches(C0_CONTROL_OR_SPACE)
        .replace(['\t', '\r', '\n'], "");
    let mut rest = cleaned.as_str();
    let (mut scheme, mut netloc, mut query, mut fragment) = ("", "", "", "");
    if let Some(colon) = rest.find(':')
        && colon > 0
        && rest.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && rest
            .split_at(colon)
            .0
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        let (before, after) = rest.split_at(colon);
        scheme = before;
        rest = after.get(1..).unwrap_or_default();
    }
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        let (location, after) = after.split_at(end);
        if location.contains('[') != location.contains(']') {
            return Err(UrlValueError("Invalid IPv6 URL".to_owned()));
        }
        netloc = location;
        rest = after;
    }
    if let Some((before, after)) = rest.split_once('#') {
        fragment = after;
        rest = before;
    }
    if let Some((before, after)) = rest.split_once('?') {
        query = after;
        rest = before;
    }
    Ok(SplitUrl {
        scheme: scheme.to_ascii_lowercase(),
        netloc: netloc.to_owned(),
        path: rest.to_owned(),
        query: query.to_owned(),
        fragment: fragment.to_owned(),
    })
}

impl SplitUrl {
    fn userinfo(&self) -> Option<&str> {
        self.netloc.rsplit_once('@').map(|(info, _)| info)
    }

    fn hostinfo(&self) -> (&str, Option<&str>) {
        let hostinfo = self
            .netloc
            .rsplit_once('@')
            .map_or(self.netloc.as_str(), |(_, host)| host);
        let (hostname, port) = match hostinfo.split_once('[') {
            Some((_, bracketed)) => {
                let (host, after) = bracketed.split_once(']').unwrap_or((bracketed, ""));
                (host, after.split_once(':').map_or("", |(_, port)| port))
            }
            None => hostinfo.split_once(':').unwrap_or((hostinfo, "")),
        };
        (hostname, Some(port).filter(|p| !p.is_empty()))
    }

    /// `.username`: `None` without an `@`.
    pub fn username(&self) -> Option<&str> {
        self.userinfo()
            .map(|info| info.split_once(':').map_or(info, |(user, _)| user))
    }

    /// `.password`: `None` without an `@` or a `:` before it.
    pub fn password(&self) -> Option<&str> {
        self.userinfo()
            .and_then(|info| info.split_once(':').map(|(_, password)| password))
    }

    /// `.hostname`: lower-cased, `None` when empty.
    pub fn hostname(&self) -> Option<String> {
        let (hostname, _) = self.hostinfo();
        if hostname.is_empty() {
            return None;
        }
        Some(match hostname.split_once('%') {
            Some((host, zone)) => format!("{}%{zone}", host.to_lowercase()),
            None => hostname.to_lowercase(),
        })
    }

    /// `.port`.
    ///
    /// # Errors
    /// A port that is not ASCII digits or is above 65535.
    pub fn port(&self) -> Result<Option<u16>, UrlValueError> {
        let (_, port) = self.hostinfo();
        let Some(port) = port else { return Ok(None) };
        if !port.chars().all(|c| c.is_ascii_digit()) {
            return Err(UrlValueError(format!(
                "Port could not be cast to integer value as {}",
                repr_str(port)
            )));
        }
        let trimmed = port.trim_start_matches('0');
        if trimmed.len() > 5 {
            return Err(UrlValueError("Port out of range 0-65535".to_owned()));
        }
        trimmed
            .parse::<u32>()
            .ok()
            .or_else(|| trimmed.is_empty().then_some(0))
            .and_then(|value| u16::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| UrlValueError("Port out of range 0-65535".to_owned()))
    }
}

/// `urlsplit(url).hostname` as the helper's messages show it (`None` when
/// there is none or the URL does not split).
pub fn hostname_for_message(url: &str) -> String {
    urlsplit(url)
        .ok()
        .and_then(|split| split.hostname())
        .unwrap_or_else(|| "None".to_owned())
}

/// `urllib.parse.unquote`: `%XX` escapes as UTF-8, invalid bytes replaced.
pub fn unquote(text: &str) -> String {
    if !text.contains('%') {
        return text.to_owned();
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while let Some(&byte) = bytes.get(index) {
        if byte == b'%'
            && let (Some(high), Some(low)) = (
                bytes
                    .get(index.saturating_add(1))
                    .and_then(|b| hex_value(*b)),
                bytes
                    .get(index.saturating_add(2))
                    .and_then(|b| hex_value(*b)),
            )
        {
            out.push(high.wrapping_mul(16).wrapping_add(low));
            index = index.saturating_add(3);
            continue;
        }
        out.push(byte);
        index = index.saturating_add(1);
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    char::from(byte)
        .to_digit(16)
        .and_then(|d| u8::try_from(d).ok())
}

/// `repr()` of a string: single-quoted unless it holds a `'` and no `"`.
pub fn repr_str(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len().saturating_add(2));
    out.push(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `str(value)` of a parsed JSON value.
pub fn str_of(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => repr(other),
    }
}

/// `repr(value)` of a parsed JSON value.
pub fn repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Number(number) => tilo_pycompat::number_repr(number),
        Value::String(text) => repr_str(text),
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(repr).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Object(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", repr_str(k), repr(v)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

/// Whether a parsed JSON value equals 1 as Python's `==` sees it (`True == 1`,
/// `1.0 == 1`): how the schema numbers were always compared.
pub fn equals_one(value: &Value) -> bool {
    match value {
        Value::Bool(flag) => *flag,
        Value::Number(number) => {
            number.as_i64() == Some(1)
                || (!number.is_i64() && !number.is_u64() && number.to_string() == "1.0")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlsplit_matches_python() {
        let split = urlsplit("HTTPS://User:pw@Example.ORG:443/a/../b?q#f").unwrap();
        assert_eq!(split.scheme, "https");
        assert_eq!(split.username(), Some("User"));
        assert_eq!(split.password(), Some("pw"));
        assert_eq!(split.hostname().as_deref(), Some("example.org"));
        assert_eq!(split.port(), Ok(Some(443)));
        assert_eq!(
            (
                split.path.as_str(),
                split.query.as_str(),
                split.fragment.as_str()
            ),
            ("/a/../b", "q", "f")
        );
        let bare = urlsplit("https://h/x?").unwrap();
        assert_eq!(
            (bare.query.as_str(), bare.username(), bare.port()),
            ("", None, Ok(None))
        );
        assert!(urlsplit("https://h:x/").unwrap().port().is_err());
        assert!(urlsplit("https://h:65536/").unwrap().port().is_err());
        assert_eq!(urlsplit("https://h:/").unwrap().port(), Ok(None));
        assert!(urlsplit("https://[h/").is_err());
        assert_eq!(urlsplit("https://@h/").unwrap().username(), Some(""));
    }

    #[test]
    fn text_slices_and_unquotes_like_python() {
        assert_eq!(truncate("héllo", 2), "hé");
        assert_eq!(unquote("/a/%2e%2E/%zz%C3%A9"), "/a/../%zzé");
    }
}
