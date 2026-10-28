//! Bounded Valve KeyValues metadata parsers; never opens referenced libraries.
use super::{PinnedDirectory, invalid};
use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, Metadata, OpenOptions};
use std::{
    ffi::OsStr,
    io::{self, Read},
    path::{Path, PathBuf},
};

const MAX_METADATA: u64 = 1024 * 1024;

/// One installed Steam application, read from its own bounded manifest.
/// `name` and `install_dir` are Valve's public catalog metadata, not a path
/// chosen by the person.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SteamAppManifest {
    pub app_id: u32,
    pub name: String,
    pub install_dir: String,
}

pub(super) fn steam_libraries(
    parent: &PinnedDirectory,
    name: &OsStr,
    expected: &Metadata,
) -> io::Result<Vec<PathBuf>> {
    let text = read_bounded(parent, name, expected)?;
    libraries(&text)
}

pub(super) fn steam_app(
    parent: &PinnedDirectory,
    name: &OsStr,
    expected: &Metadata,
) -> io::Result<Option<SteamAppManifest>> {
    let text = read_bounded(parent, name, expected)?;
    app_manifest(&text)
}

pub(crate) fn read_bounded(
    parent: &PinnedDirectory,
    name: &OsStr,
    expected: &Metadata,
) -> io::Result<String> {
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
    Ok(text)
}

/// Accept modern path keys and old numeric path pairs. Unknown keys are not
/// interpreted as paths. Reject malformed/truncated input instead of claiming
/// complete coverage. Windows paths stay literal even when tested on Linux.
pub(crate) fn libraries(text: &str) -> io::Result<Vec<PathBuf>> {
    let tokens = tokens(text)?;
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

/// Read `appid`, `name` and `installdir` from an `appmanifest_<id>.acf` file.
/// Missing or malformed fields refuse the manifest instead of guessing a title.
pub(crate) fn app_manifest(text: &str) -> io::Result<Option<SteamAppManifest>> {
    let tokens = tokens(text)?;
    let mut app_id = None;
    let mut name = None;
    let mut install_dir = None;
    let mut key: Option<&str> = None;
    for token in &tokens {
        match token.as_str() {
            "{" | "}" => key = None,
            value => {
                let Some(field) = key.take() else {
                    key = Some(value);
                    continue;
                };
                if field.eq_ignore_ascii_case("appid") {
                    app_id = value.parse::<u32>().ok();
                } else if field.eq_ignore_ascii_case("name") {
                    name = Some(value.to_string());
                } else if field.eq_ignore_ascii_case("installdir") {
                    install_dir = Some(value.to_string());
                }
            }
        }
    }
    let (Some(app_id), Some(name), Some(install_dir)) = (app_id, name, install_dir) else {
        return Ok(None);
    };
    if app_id == 0
        || name.is_empty()
        || name.len() > 512
        || name.chars().any(char::is_control)
        || install_dir.is_empty()
        || install_dir.len() > 512
        || install_dir.contains(['/', '\\'])
        || install_dir == "."
        || install_dir == ".."
        || install_dir.chars().any(char::is_control)
    {
        return Err(invalid("invalid Steam application manifest"));
    }
    Ok(Some(SteamAppManifest {
        app_id,
        name,
        install_dir,
    }))
}

/// Shared bounded tokenizer. Section names, keys and values are one flat token
/// stream; callers validate the structure they need.
fn tokens(text: &str) -> io::Result<Vec<String>> {
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
    validate_structure(&tokens)?;
    Ok(tokens)
}

/// A key is either followed by its value or by a named section. Truncated or
/// over-nested input is refused rather than partially interpreted.
fn validate_structure(tokens: &[String]) -> io::Result<()> {
    let mut depth = 0usize;
    let mut pending = false;
    for token in tokens {
        match token.as_str() {
            "{" => {
                if !pending {
                    return Err(invalid("missing Steam section name"));
                }
                pending = false;
                depth = depth.saturating_add(1);
                if depth > 16 {
                    return Err(invalid("Steam metadata nesting limit"));
                }
            }
            "}" => {
                if pending || depth == 0 {
                    return Err(invalid("invalid Steam section"));
                }
                depth = depth.saturating_sub(1);
            }
            _ => pending = !pending,
        }
    }
    if depth != 0 || pending {
        return Err(invalid("incomplete Steam metadata"));
    }
    Ok(())
}
