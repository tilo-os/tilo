//! The few Python behaviours the command line has always had.
//!
//! The helper was a Python script, and `tilo-firstboot-user` and existing
//! installs pass it paths and ids the way that script read them: the path split
//! by `os.path.dirname`/`basename` and the parent resolved by
//! `os.path.realpath`.

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
}
