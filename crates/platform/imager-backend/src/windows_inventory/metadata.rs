//! Bounded Valve KeyValues metadata parser; never opens referenced libraries.
use super::{PinnedDirectory, invalid};
use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, Metadata, OpenOptions};
use std::{
    ffi::OsStr,
    io::{self, Read},
    path::{Path, PathBuf},
};

const MAX_METADATA: u64 = 1024 * 1024;

pub(super) fn steam_libraries(
    parent: &PinnedDirectory,
    name: &OsStr,
    expected: &Metadata,
) -> io::Result<Vec<PathBuf>> {
    if expected.len() > MAX_METADATA {
        return Err(invalid("Steam metadata exceeds size limit"));
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let file = cap_primitives::fs::open(&parent.file, Path::new(name), &options)?;
    let opened = Metadata::from_file(&file)?;
    if !crate::migration_directory::same_snapshot(expected, &opened) {
        return Err(invalid("Steam metadata changed during inventory"));
    }
    let mut text = String::new();
    file.take(MAX_METADATA + 1).read_to_string(&mut text)?;
    if text.len() as u64 > MAX_METADATA {
        return Err(invalid("Steam metadata exceeds size limit"));
    }
    libraries(&text)
}

/// Accept modern path keys and old numeric path pairs. Unknown keys are not
/// interpreted as paths. Reject malformed/truncated input instead of claiming
/// complete coverage. Windows paths stay literal even when tested on Linux.
pub(super) fn libraries(text: &str) -> io::Result<Vec<PathBuf>> {
    let mut chars = text.chars().peekable();
    let mut tokens = Vec::new();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        if c == '/' && chars.peek() == Some(&'/') {
            for c in chars.by_ref() {
                if c == '\n' {
                    break;
                }
            }
            continue;
        }
        if c == '{' || c == '}' {
            tokens.push(c.to_string());
            continue;
        }
        if c != '"' {
            return Err(invalid("unsupported Steam metadata token"));
        }
        let mut value = String::new();
        let mut closed = false;
        while let Some(c) = chars.next() {
            match c {
                '"' => {
                    closed = true;
                    break;
                }
                '\\' => match chars.next() {
                    Some(c @ ('\\' | '"')) => value.push(c),
                    _ => return Err(invalid("invalid Steam metadata escape")),
                },
                '\0' => return Err(invalid("invalid Steam metadata character")),
                c => value.push(c),
            }
            if value.len() > 32768 {
                return Err(invalid("Steam metadata token too long"));
            }
        }
        if !closed {
            return Err(invalid("truncated Steam metadata"));
        }
        tokens.push(value);
        if tokens.len() > 32768 {
            return Err(invalid("too many Steam metadata tokens"));
        }
    }
    let mut depth = 0usize;
    let mut pending: Option<&str> = None;
    let mut paths = Vec::new();
    for token in &tokens {
        match token.as_str() {
            "{" => {
                if pending.take().is_none() {
                    return Err(invalid("missing Steam section name"));
                }
                depth += 1;
                if depth > 16 {
                    return Err(invalid("Steam metadata nesting limit"));
                }
            }
            "}" => {
                if pending.is_some() || depth == 0 {
                    return Err(invalid("invalid Steam section"));
                }
                depth -= 1;
            }
            value => {
                if let Some(key) = pending.take() {
                    let candidate = key.eq_ignore_ascii_case("path")
                        || (depth == 1 && key.bytes().all(|c| c.is_ascii_digit()));
                    if candidate {
                        let bytes = value.as_bytes();
                        let drive_absolute = bytes.len() > 2
                            && bytes[0].is_ascii_alphabetic()
                            && bytes[1] == b':'
                            && matches!(bytes[2], b'\\' | b'/');
                        if !(drive_absolute || value.starts_with("\\\\") || value.starts_with('/'))
                        {
                            return Err(invalid("Steam library path is not absolute"));
                        }
                        let path = PathBuf::from(value);
                        if !paths.contains(&path) {
                            paths.push(path);
                        }
                        if paths.len() > 64 {
                            return Err(invalid("too many Steam libraries"));
                        }
                    }
                } else {
                    pending = Some(value);
                }
            }
        }
    }
    if depth != 0 || pending.is_some() {
        return Err(invalid("incomplete Steam metadata"));
    }
    Ok(paths)
}
