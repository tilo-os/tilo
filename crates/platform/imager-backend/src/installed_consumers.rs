//! Installed consumer capability producers and read-only checks.
//!
//! The image build runs each producer once as root to publish a receipt under
//! `/usr/share/tilo`; admission runs the matching check in-process before any
//! target custody write. Checks re-execute the installed consumers' own
//! capability probes and rehash their artifacts, so a receipt can never stand
//! in for the consumers it describes. None of these entry points accept paths,
//! alternate helpers, saved evidence or environment configuration.
pub(crate) mod account;
pub(crate) mod data;
mod invoke;
mod json;
mod service;
#[cfg(test)]
pub(crate) mod tests;

use std::{ffi::OsString, io};

/// The absolute image path `absolute` inside the namespace rooted at `root`.
fn under(root: &std::path::Path, absolute: &str) -> std::path::PathBuf {
    root.join(absolute.strip_prefix('/').unwrap_or(absolute))
}

/// Stream `reader` into its lowercase hex SHA-256.
fn sha256_hex(reader: &mut impl io::Read) -> io::Result<String> {
    use sha2::Digest;
    let mut hash = sha2::Sha256::new();
    let mut chunk = vec![0_u8; 1024 * 1024];
    loop {
        match reader.read(&mut chunk)? {
            0 => break,
            count => hash.update(chunk.get(..count).unwrap_or_default()),
        }
    }
    Ok(hex::encode(hash.finalize()))
}

static NULL: serde_json::Value = serde_json::Value::Null;

/// `value[key]` without indexing: absent fields read as null.
fn field<'v>(value: &'v serde_json::Value, key: &str) -> &'v serde_json::Value {
    value.get(key).unwrap_or(&NULL)
}

/// Set `key` on a JSON object; other values are left unchanged.
fn insert(value: &mut serde_json::Value, key: &str, item: impl Into<serde_json::Value>) {
    if let Some(fields) = value.as_object_mut() {
        fields.insert(key.to_owned(), item.into());
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_owned())
}

fn require(condition: bool, message: &str) -> io::Result<()> {
    if condition {
        Ok(())
    } else {
        Err(invalid(message))
    }
}

/// The installed command-line producers, as `tilo-prepared-setup` modes.
pub const MODES: [&str; 4] = [
    "--verify-home-consumer-capability",
    "--verify-applications-consumer-capability",
    "--verify-account-consumer-capability",
    "--data-consumer-catalog-facts",
];

/// Run one installed producer mode. Returns stdout on success, or the closed
/// diagnostic for stderr; the caller maps these to exit status 0 and 1.
///
/// # Errors
/// Returns the diagnostic line for unknown arguments and failed checks.
pub fn run(mode: &str, args: impl IntoIterator<Item = OsString>) -> Result<Vec<u8>, String> {
    let args = args.into_iter().collect::<Vec<_>>();
    let check = match args.as_slice() {
        [] => Some(false),
        [flag] if flag == "--check" => Some(true),
        _ => None,
    };
    let line = |mut bytes: Vec<u8>| {
        bytes.push(b'\n');
        bytes
    };
    let mut runner = data::invoke;
    match mode {
        "--verify-home-consumer-capability" | "--verify-applications-consumer-capability" => {
            let home = mode == "--verify-home-consumer-capability";
            let failed = |error: io::Error| {
                let kind = if home { "home" } else { "applications" };
                format!("installed {kind} consumer unavailable: {error}")
            };
            let check =
                check.ok_or_else(|| failed(invalid("consumer verifier accepts only --check")))?;
            if !check && !rustix::process::geteuid().is_root() {
                return Err(failed(invalid("consumer producer requires root")));
            }
            let store = data::Store::installed();
            let proof = if home {
                data::home(&store, check, &mut runner)
            } else {
                data::applications(&store, check, &mut runner)
            };
            Ok(line(json::canonical(&proof.map_err(failed)?)))
        }
        "--verify-account-consumer-capability" => {
            let failed =
                |error: io::Error| format!("account consumer capability unavailable: {error}");
            let check = check.ok_or_else(|| {
                failed(invalid("account capability verifier accepts only --check"))
            })?;
            if check {
                Ok(line(json::canonical_ascii(
                    &account::check().map_err(failed)?,
                )))
            } else {
                account::publish().map(|_| Vec::new()).map_err(failed)
            }
        }
        "--data-consumer-catalog-facts" => {
            let failed = |error: io::Error| format!("consumer catalog facts unavailable: {error}");
            if !args.is_empty() {
                return Err(failed(invalid(
                    "catalog fact exporter accepts no arguments",
                )));
            }
            let facts = data::catalog_facts(&data::Store::installed(), &mut runner);
            Ok(line(json::canonical(&facts.map_err(failed)?)))
        }
        _ => Err(format!("unknown installed consumer mode {mode}")),
    }
}
