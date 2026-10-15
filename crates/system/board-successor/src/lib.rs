//! `tilo-board-successor`: selects an authenticated successor release for
//! a board image. Tags are discovery hints only.
//!
//! ```text
//! tilo-board-successor applicable   exit 0 on an official board release image, 3 otherwise
//! tilo-board-successor NOW_NS       print the verified successor (digest reference,
//!                                   generation, source revision, freshness expiry),
//!                                   or exit 3 when there is none newer
//! ```
//!
//! `tilo-verified-update` runs it. All cryptographic verification uses the
//! image's pinned Cosign verifier and key; this helper never changes the
//! deployment, owner trust or rollback state. Exit status 1 on any failure,
//! with the reason on stderr; 128 + the signal when stopped.

pub mod command;
pub mod profile;
pub mod py;
pub mod registry;
pub mod release;

use std::fmt;
use std::fs;
use std::path::Path;
use std::time::Duration;

use serde_json::{Map, Value};

use crate::profile::{Root, development_origin, read_profile};
use crate::registry::Registry;
use crate::release::{MAX_OUTPUT, PROFILE, select};

/// A failure, by what the original raised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A check refused (`ValueError` and the lookups that raised
    /// `KeyError`, `TypeError` or `AttributeError`).
    Value(String),
    /// The operating system refused (`OSError`).
    Os(String),
    /// A command outlived its deadline (`subprocess.TimeoutExpired`).
    Timeout(String),
    /// SIGTERM or SIGINT arrived (`SystemExit(128 + signal)`).
    Cancelled(i32),
}

impl Error {
    /// A check's refusal.
    pub fn value(message: impl Into<String>) -> Self {
        Self::Value(message.into())
    }

    /// An operating-system error on `path`, worded as Python worded it.
    #[must_use]
    pub fn os(error: &std::io::Error, path: &Path) -> Self {
        let code = error.raw_os_error().unwrap_or(0);
        Self::Os(format!(
            "[Errno {code}] {error}: {:?}",
            path.display().to_string()
        ))
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Os(error.to_string())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Value(message) | Self::Os(message) | Self::Timeout(message) => {
                f.write_str(message)
            }
            Self::Cancelled(signal) => write!(f, "stopped by signal {signal}"),
        }
    }
}

impl std::error::Error for Error {}

/// `Result` with this crate's [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// `require(condition, message)`.
///
/// # Errors
/// `message` when `condition` is false.
pub fn require(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Error::Value(message.into()))
    }
}

/// `json.loads(bytes)`: the encoding detected as Python detects it (UTF-8,
/// with or without a BOM, UTF-16 or UTF-32), then strict JSON.
///
/// # Errors
/// Undecodable bytes or invalid JSON.
pub fn json_loads_bytes(bytes: &[u8]) -> Result<Value> {
    let invalid = |e: String| Error::Value(e);
    let utf16 = |data: &[u8], big: bool| -> Result<String> {
        let units: Vec<u16> = data
            .chunks(2)
            .map(|pair| match pair {
                [a, b] if big => u16::from_be_bytes([*a, *b]),
                [a, b] => u16::from_le_bytes([*a, *b]),
                _ => 0xDC00,
            })
            .collect();
        char::decode_utf16(units)
            .collect::<Result<String, _>>()
            .map_err(|e| invalid(e.to_string()))
    };
    let utf32 = |data: &[u8], big: bool| -> Result<String> {
        data.chunks(4)
            .map(|quad| match quad {
                [a, b, c, d] => {
                    let unit = if big {
                        u32::from_be_bytes([*a, *b, *c, *d])
                    } else {
                        u32::from_le_bytes([*a, *b, *c, *d])
                    };
                    char::from_u32(unit).ok_or_else(|| invalid("invalid UTF-32".into()))
                }
                _ => Err(invalid("truncated UTF-32".into())),
            })
            .collect()
    };
    let text = match bytes {
        [0, 0, 0xFE, 0xFF, rest @ ..] => utf32(rest, true)?,
        [0xFF, 0xFE, 0, 0, rest @ ..] => utf32(rest, false)?,
        [0xFE, 0xFF, rest @ ..] => utf16(rest, true)?,
        [0xFF, 0xFE, rest @ ..] => utf16(rest, false)?,
        [0xEF, 0xBB, 0xBF, rest @ ..] => {
            String::from_utf8(rest.to_vec()).map_err(|e| invalid(e.to_string()))?
        }
        [0, 0, _, _, ..] => utf32(bytes, true)?,
        [_, 0, 0, 0, ..] => utf32(bytes, false)?,
        [0, _, _, _, ..] | [0, _] => utf16(bytes, true)?,
        [_, 0, _, _, ..] | [_, 0] => utf16(bytes, false)?,
        _ => String::from_utf8(bytes.to_vec()).map_err(|e| invalid(e.to_string()))?,
    };
    serde_json::from_str(&text).map_err(|e| invalid(e.to_string()))
}

/// The installed profile when this is an official release image of the
/// board this helper serves; `None` for any other image.
///
/// # Errors
/// A profile the hardware-profile reader refuses, or a board image whose
/// update origin is not an official immutable release.
pub fn installed_profile(root: &Root) -> Result<Option<Map<String, Value>>> {
    let Some(profile) = read_profile(root)? else {
        return Ok(None);
    };
    if development_origin(root)?.is_some() {
        return Ok(None);
    }
    if profile.get("id").and_then(Value::as_str) != Some(PROFILE) {
        return Ok(None);
    }
    let origin = profile
        .get("target_update_ref")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let official = regex::Regex::new(
        r"^(?:code\.tilo\.so/tilo/tilo(?::lenovo-83ed-[0-9a-f]{40}|@sha256:[0-9a-f]{64}))$",
    )
    .is_ok_and(|re| re.is_match(origin));
    require(
        profile.get("architecture").and_then(Value::as_str) == Some("aarch64") && official,
        "installed board update origin is not an official immutable release",
    )?;
    Ok(Some(profile))
}

/// The running image's generation, from the sealed image.
fn image_generation(root: &Root) -> Result<u64> {
    let path = root.trusted_path("usr/lib/tilo/generation", true)?;
    let size = fs::metadata(&path).map_err(|e| Error::os(&e, &path))?.len();
    require(size <= 32, "oversized image generation")?;
    let text = fs::read_to_string(&path).map_err(|e| Error::os(&e, &path))?;
    let generation = py::int(py::strip(&text)).map_err(Error::Value)?;
    require(
        0 < generation && generation < (1i128 << 64),
        "invalid image generation",
    )?;
    u64::try_from(generation).map_err(|_| Error::value("invalid image generation"))
}

fn selected(args: &[String]) -> Result<i32> {
    let [argument] = args else {
        return Err(Error::value(
            "usage: tilo-board-successor applicable|NOW_NS",
        ));
    };
    let root = Root::system();
    if installed_profile(&root)?.is_none() {
        return Ok(3);
    }
    if argument == "applicable" {
        return Ok(0);
    }
    let now_ns = py::int(argument).map_err(Error::Value)?;
    require(
        0 < now_ns && now_ns < (1i128 << 64),
        "invalid authenticated time",
    )?;
    let generation = image_generation(&root)?;
    let successor = {
        let trusted = |relative: &str| root.trusted_path(relative, true);
        let registry = Registry::new(&root.path, &trusted, Path::new("/run"), 0)?;
        select(
            generation,
            now_ns / 1_000_000_000,
            &|arguments: &[&str]| registry.run(arguments),
        )?
    };
    command::check_cancelled()?;
    if successor.generation == generation {
        // The same generation may not introduce another digest. Compare
        // bootc's running image identity, not an old tag name.
        let output = command::run(
            &["/usr/bin/bootc", "status", "--format", "json"],
            &[],
            Duration::from_secs(120),
            MAX_OUTPUT,
        )?;
        let status: Value =
            serde_json::from_str(&output).map_err(|e| Error::Value(e.to_string()))?;
        let mut booted = &status;
        for key in ["status", "booted", "image", "imageDigest"] {
            booted = profile::index(booted, key)?;
        }
        let digest = successor.exact.split('@').nth(1).unwrap_or_default();
        require(
            booted.as_str() == Some(digest),
            "release changed digest without advancing generation",
        )?;
        return Ok(3);
    }
    println!("{}", successor.exact);
    println!("{}", successor.generation);
    println!("{}", successor.source);
    println!("{}", successor.expires);
    Ok(0)
}

/// The command line, without the program name; returns the exit status.
#[must_use]
pub fn main(args: &[String]) -> i32 {
    match selected(args) {
        Ok(code) => code,
        Err(Error::Cancelled(signal)) => signal.saturating_add(128),
        Err(error) => {
            eprintln!("tilo-board-successor: {error}");
            1
        }
    }
}
