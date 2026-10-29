//! Native target-user access to the retained original files, held over a
//! bounded headless protocol until the controlling pipe closes.
use imager_backend::{
    destination_security,
    migration::PinnedDirectory,
    setup::migration_handoff::{self, Plan},
};
use std::{
    ffi::OsString,
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    sync::mpsc,
    time::Duration,
};
const LIMIT: usize = 16 * 1024;
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    op: String,
}
fn failure() -> io::Error {
    io::Error::other("Installed migration is unavailable or its grants changed")
}

/// The active first-boot person; no key is supplied through argv or JSON.
fn current_person() -> [u8; 32] {
    runtime_identity::identity_dir_from_env()
        .ok()
        .and_then(|directory| runtime_identity::resolve_active_identity(&directory))
        .and_then(|active| runtime_identity::load_from_path(active).ok())
        .map_or([0; 32], |signer| signer.group_public_key())
}
fn private(path: &Path) -> io::Result<()> {
    let root = PinnedDirectory::open_absolute(path)?;
    root.validate_destination()?;
    destination_security::check_private_file(&root.file)
}
struct Grants {
    plans: PathBuf,
    operation: String,
    home: PathBuf,
    workspace: PathBuf,
}
fn arguments(args: impl IntoIterator<Item = OsString>) -> io::Result<Grants> {
    let mut values = std::collections::BTreeMap::new();
    let mut args = args.into_iter();
    while let Some(key) = args.next() {
        let key = key.into_string().map_err(|_| failure())?;
        if !["--plans", "--operation", "--home", "--workspace"].contains(&key.as_str())
            || values.contains_key(&key)
        {
            return Err(failure());
        }
        values.insert(key, args.next().ok_or_else(failure)?);
    }
    if values.len() != 4 {
        return Err(failure());
    }
    let get = |name: &str| values.get(name).cloned().ok_or_else(failure);
    let grants = Grants {
        plans: PathBuf::from(get("--plans")?),
        operation: get("--operation")?.into_string().map_err(|_| failure())?,
        home: PathBuf::from(get("--home")?),
        workspace: PathBuf::from(get("--workspace")?),
    };
    for path in [&grants.plans, &grants.home, &grants.workspace] {
        if !path.is_absolute() {
            return Err(failure());
        }
        private(path)?;
    }
    if grants.workspace
        != grants
            .home
            .join(".local/state/tilo/migration")
            .join(&grants.operation)
    {
        return Err(failure());
    }
    Ok(grants)
}
fn emit(output: &mut impl Write, value: &serde_json::Value) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > LIMIT {
        return Err(failure());
    }
    output.write_all(&bytes)?;
    output.write_all(b"\n")?;
    output.flush()
}
fn lines(input: impl io::Read, sender: mpsc::SyncSender<Result<Command, ()>>) {
    let mut reader = io::BufReader::new(input);
    loop {
        let mut bytes = Vec::new();
        loop {
            let buffer = if let Ok(buffer) = reader.fill_buf() {
                buffer
            } else {
                let _ = sender.send(Err(()));
                return;
            };
            if buffer.is_empty() {
                return;
            }
            let count = buffer
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(buffer.len(), |index| index + 1);
            let Some(chunk) = buffer.get(..count) else {
                return;
            };
            if bytes
                .len()
                .checked_add(count)
                .is_none_or(|length| length > LIMIT)
            {
                let _ = sender.send(Err(()));
                return;
            }
            bytes.extend_from_slice(chunk);
            reader.consume(count);
            if bytes.last() == Some(&b'\n') {
                break;
            }
        }
        if sender
            .send(serde_json::from_slice(&bytes).map_err(|_| ()))
            .is_err()
        {
            return;
        }
    }
}
mod access;
/// Hold an explicitly requested shared writable original-files grant until EOF.
///
/// # Errors
/// Refuses unknown mount safety, changed source/target identities or UID mismatch.
pub fn run_shared_access(
    args: impl IntoIterator<Item = OsString>,
    input: impl io::Read + Send + 'static,
    output: &mut impl Write,
) -> io::Result<()> {
    access::run(
        arguments(args)?,
        migration_handoff::AccessMode::ReadWrite,
        input,
        output,
    )
}
#[cfg(test)]
mod tests;
