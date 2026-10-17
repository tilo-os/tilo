//! The few Python behaviours the command line has always had.
//!
//! The helper was a Python script, and `tilo-firstboot-user` and existing
//! installs pass it paths and ids the way that script read them: the path split
//! by `os.path.dirname`/`basename`, the parent resolved by `os.path.realpath`
//! and the ids read by `int()`.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

use rustix::fs::{AtFlags, CWD, FileType, readlinkat, statat};

/// `os.path.dirname(path)`: up to the last `/`, trailing slashes removed
/// unless that leaves nothing but slashes.
#[must_use]
pub fn dirname(path: &OsStr) -> OsString {
    let bytes = path.as_bytes();
    let cut = bytes
        .iter()
        .rposition(|b| *b == b'/')
        .map_or(0, |i| i.saturating_add(1));
    let head = bytes.get(..cut).unwrap_or_default();
    let trimmed = if head.iter().all(|b| *b == b'/') {
        head
    } else {
        let end = head
            .iter()
            .rposition(|b| *b != b'/')
            .map_or(0, |i| i.saturating_add(1));
        head.get(..end).unwrap_or_default()
    };
    OsString::from_vec(trimmed.to_vec())
}

/// `os.path.basename(path)`: everything after the last `/`.
#[must_use]
pub fn basename(path: &OsStr) -> OsString {
    let bytes = path.as_bytes();
    let cut = bytes
        .iter()
        .rposition(|b| *b == b'/')
        .map_or(0, |i| i.saturating_add(1));
    OsString::from_vec(bytes.get(cut..).unwrap_or_default().to_vec())
}

enum Part {
    Name(Vec<u8>),
    /// The symlink pushed just below has been resolved to the current path.
    Resolved,
}

fn split_reversed(path: &[u8]) -> Vec<Part> {
    path.split(|b| *b == b'/')
        .rev()
        .map(|part| Part::Name(part.to_vec()))
        .collect()
}

/// `os.path.realpath(path)` (not strict): symlinks that exist are followed,
/// a component that cannot be read is taken as written, and a symlink loop
/// stops at the looping link.
///
/// # Errors
/// A relative path when the working directory cannot be read.
pub fn realpath(path: &OsStr) -> std::io::Result<OsString> {
    let path = path.as_bytes();
    let mut rest = split_reversed(path);
    let mut part_count = rest.len();
    let mut resolved = if path.starts_with(b"/") {
        b"/".to_vec()
    } else {
        std::env::current_dir()?.into_os_string().into_vec()
    };
    let mut seen: HashMap<Vec<u8>, Option<Vec<u8>>> = HashMap::new();
    while part_count > 0 {
        let Some(part) = rest.pop() else { break };
        let name = match part {
            Part::Resolved => {
                if let Some(Part::Name(link)) = rest.pop() {
                    seen.insert(link, Some(resolved.clone()));
                }
                continue;
            }
            Part::Name(name) => name,
        };
        part_count = part_count.saturating_sub(1);
        if name.is_empty() || name == b"." {
            continue;
        }
        if name == b".." {
            let cut = resolved.iter().rposition(|b| *b == b'/').unwrap_or(0);
            resolved.truncate(cut);
            if resolved.is_empty() {
                resolved = b"/".to_vec();
            }
            continue;
        }
        let mut candidate = resolved.clone();
        if candidate != b"/" {
            candidate.push(b'/');
        }
        candidate.extend_from_slice(&name);
        let is_link = statat(CWD, candidate.as_slice(), AtFlags::SYMLINK_NOFOLLOW)
            .is_ok_and(|metadata| FileType::from_raw_mode(metadata.st_mode) == FileType::Symlink);
        if !is_link {
            resolved = candidate;
            continue;
        }
        if let Some(known) = seen.get(&candidate) {
            // A link resolved before is reused; one still being resolved is a
            // loop, which stops here.
            resolved = known.clone().unwrap_or(candidate);
            continue;
        }
        let Ok(target) = readlinkat(CWD, candidate.as_slice(), Vec::new()) else {
            resolved = candidate;
            continue;
        };
        let target = target.into_bytes();
        if target.starts_with(b"/") {
            resolved = b"/".to_vec();
        }
        seen.insert(candidate.clone(), None);
        rest.push(Part::Name(candidate));
        rest.push(Part::Resolved);
        let parts = split_reversed(&target);
        part_count = part_count.saturating_add(parts.len());
        rest.extend(parts);
    }
    Ok(OsString::from_vec(resolved))
}

/// Why `int()` refused its argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntError(pub String);

/// `str.isspace()`: Unicode whitespace plus the four ASCII separators
/// (U+001C..U+001F) that Rust does not count.
fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `int(text)` for a decimal command-line argument: surrounding whitespace,
/// a sign, and `_` between digits. A value beyond `i128` saturates; every
/// value that large is refused as an id later, as Python refused it.
///
/// Python also reads non-ASCII decimal digits; the caller passes ids it has
/// matched against `^[0-9]+$`, so those are refused here.
///
/// # Errors
/// Anything else.
pub fn int(text: &OsStr) -> Result<i128, IntError> {
    let invalid = || {
        IntError(format!(
            "invalid literal for int() with base 10: {:?}",
            text.to_string_lossy()
        ))
    };
    let text_str = text.to_str().ok_or_else(invalid)?;
    let trimmed = text_str.trim_matches(is_space);
    let (negative, digits) = match trimmed.as_bytes().first() {
        Some(b'-') => (true, trimmed.get(1..).unwrap_or_default()),
        Some(b'+') => (false, trimmed.get(1..).unwrap_or_default()),
        _ => (false, trimmed),
    };
    let bytes = digits.as_bytes();
    if bytes.is_empty() || bytes.first() == Some(&b'_') || bytes.last() == Some(&b'_') {
        return Err(invalid());
    }
    let mut value: i128 = 0;
    let mut previous_underscore = false;
    for byte in bytes {
        if *byte == b'_' {
            if previous_underscore {
                return Err(invalid());
            }
            previous_underscore = true;
            continue;
        }
        previous_underscore = false;
        if !byte.is_ascii_digit() {
            return Err(invalid());
        }
        let digit = i128::from(byte.saturating_sub(b'0'));
        value = value.saturating_mul(10).saturating_add(digit);
    }
    Ok(if negative {
        value.saturating_neg()
    } else {
        value
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failed fixture call fails the test")]

    use super::*;

    fn os(text: &str) -> &OsStr {
        OsStr::new(text)
    }

    #[test]
    fn dirname_and_basename_split_like_posixpath() {
        for (path, dir, base) in [
            (
                "/etc/tilo/install-user.transaction",
                "/etc/tilo",
                "install-user.transaction",
            ),
            ("/home/alice", "/home", "alice"),
            ("/var//home//alice", "/var//home", "alice"),
            ("alice", "", "alice"),
            ("/alice", "/", "alice"),
            ("//alice", "//", "alice"),
            ("/home/alice/", "/home/alice", ""),
            ("", "", ""),
        ] {
            assert_eq!(dirname(os(path)), os(dir), "{path}");
            assert_eq!(basename(os(path)), os(base), "{path}");
        }
    }

    #[test]
    fn realpath_follows_existing_links_and_keeps_the_rest() {
        let work = tempfile::tempdir().unwrap();
        let root = work.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("var/home")).unwrap();
        std::os::unix::fs::symlink("var/home", root.join("home")).unwrap();
        std::os::unix::fs::symlink("loop", root.join("loop")).unwrap();
        let at = |rest: &str| root.join(rest).into_os_string();
        assert_eq!(realpath(&at("home")).unwrap(), at("var/home"));
        assert_eq!(realpath(&at("home/../home/./")).unwrap(), at("var/home"));
        assert_eq!(realpath(&at("missing/../home")).unwrap(), at("var/home"));
        assert_eq!(
            realpath(&at("home/missing/more")).unwrap(),
            at("var/home/missing/more")
        );
        assert_eq!(realpath(&at("loop")).unwrap(), at("loop"));
        assert_eq!(realpath(os("/")).unwrap(), os("/"));
        assert_eq!(realpath(os("/..")).unwrap(), os("/"));
    }

    #[test]
    fn int_reads_like_python() {
        for (text, value) in [
            ("1000", 1000),
            (" 1000\n", 1000),
            ("+7", 7),
            ("-1", -1),
            ("0_0_1", 1),
            ("007", 7),
            ("99999999999999999999999999999999999999999999", i128::MAX),
        ] {
            assert_eq!(int(os(text)), Ok(value), "{text}");
        }
        for text in ["", " ", "-", "1_", "_1", "1__0", "0x10", "1.0", "1 0", "١"] {
            assert!(int(os(text)).is_err(), "{text}");
        }
    }
}
