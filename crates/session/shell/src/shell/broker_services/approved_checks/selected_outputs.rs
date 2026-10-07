//! Capture only reviewed relative outputs of a fresh source candidate.
use runtime_collab::approved_checks::{
    MAX_OUTPUT_BYTES, MAX_SELECTED_OUTPUTS, MAX_TOTAL_OUTPUT_BYTES, SelectedOutput,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read as _;
use std::os::fd::{AsRawFd as _, FromRawFd as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Component, Path};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Definition {
    pub check_id: String,
    pub outputs: Vec<Output>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Output {
    id: String,
    role: String,
    media_type: String,
    path: String,
    max_bytes: u64,
}
impl Definition {
    pub(super) fn validate(&self) -> Result<(), String> {
        if self.outputs.is_empty() || self.outputs.len() > MAX_SELECTED_OUTPUTS {
            return Err("selected output count outside bound".into());
        }
        let mut ids = BTreeSet::new();
        let mut paths = BTreeSet::new();
        let mut total = 0_u64;
        for output in &self.outputs {
            relative(&output.path)?;
            let vocabulary = SelectedOutput {
                id: output.id.clone(),
                role: output.role.clone(),
                media_type: output.media_type.clone(),
                size_bytes: output.max_bytes,
                content_hash: "0".repeat(64),
                sha256: "0".repeat(64),
            };
            total = total
                .checked_add(output.max_bytes)
                .ok_or("selected output bound overflow")?;
            if !vocabulary.valid()
                || output.max_bytes == 0
                || output.max_bytes > MAX_OUTPUT_BYTES
                || !ids.insert(&output.id)
                || !paths.insert(&output.path)
                || total > MAX_TOTAL_OUTPUT_BYTES
            {
                return Err("selected output registration invalid or oversized".into());
            }
        }
        Ok(())
    }
}
fn relative(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 1024
        || value.contains('\\')
        || value
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || Path::new(value)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("selected output must be an exact relative file path".into());
    }
    Ok(())
}
fn read(root: &Path, output: &Output) -> Result<Vec<u8>, String> {
    relative(&output.path)?;
    let metadata = std::fs::symlink_metadata(root).map_err(|error| error.to_string())?;
    if !metadata.is_dir()
        || metadata.uid() != super::effective_uid()
        || metadata.mode() & 0o077 != 0
    {
        return Err("selected output root is not the private candidate build".into());
    }
    if !root.is_absolute() || root.canonicalize().map_err(|error| error.to_string())? != root {
        return Err("selected output root changed from the fresh candidate".into());
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root)
        .map_err(|error| error.to_string())?;
    let opened = file.metadata().map_err(|error| error.to_string())?;
    if opened.ino() != metadata.ino() || opened.dev() != metadata.dev() {
        return Err("selected output root changed while opened".into());
    }
    let components: Vec<_> = Path::new(&output.path).components().collect();
    for (index, part) in components.iter().enumerate() {
        let Component::Normal(name) = part else {
            return Err("invalid output component".into());
        };
        let name = std::ffi::CString::new(name.as_bytes()).map_err(|error| error.to_string())?;
        let last = index.checked_add(1) == Some(components.len());
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | if last { 0 } else { libc::O_DIRECTORY };
        // SAFETY: live owned directory fd, NUL-terminated single component, read-only fixed flags.
        let fd = unsafe { libc::openat(file.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // SAFETY: successful openat returns a fresh exclusively owned descriptor.
        file = unsafe { File::from_raw_fd(fd) };
    }
    let before = file.metadata().map_err(|error| error.to_string())?;
    if !before.is_file()
        || before.nlink() != 1
        || before.uid() != super::effective_uid()
        || before.len() > output.max_bytes
    {
        return Err("selected output is linked, nonregular, foreign or oversized".into());
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(output.max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    let after = file.metadata().map_err(|error| error.to_string())?;
    if u64::try_from(bytes.len()).map_err(|error| error.to_string())? != before.len()
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err("selected output changed while captured".into());
    }
    Ok(bytes)
}
/// Recheck before and after every read/store operation. A hash grants no authority.
pub(super) fn publish(
    definition: &Definition,
    root: &Path,
    recheck: impl Fn() -> Result<(), String>,
    put: impl Fn(&[u8], &str) -> Result<String, String>,
) -> Result<Vec<SelectedOutput>, String> {
    definition.validate()?;
    let mut outputs = Vec::new();
    for output in &definition.outputs {
        recheck()?;
        let bytes = read(root, output)?;
        recheck()?;
        let content_hash = put(&bytes, &output.media_type)?;
        recheck()?;
        let captured = SelectedOutput {
            id: output.id.clone(),
            role: output.role.clone(),
            media_type: output.media_type.clone(),
            size_bytes: u64::try_from(bytes.len()).map_err(|error| error.to_string())?,
            content_hash,
            sha256: hex::encode(Sha256::digest(&bytes)),
        };
        if !captured.valid() || captured.content_hash != blake3::hash(&bytes).to_hex().as_str() {
            return Err("content store returned an invalid output identity".into());
        }
        outputs.push(captured);
    }
    Ok(outputs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    fn fixture() -> (tempfile::TempDir, Definition) {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let definition = serde_json::from_value(serde_json::json!({"check_id":"eda", "outputs":[{"id":"summary","role":"reported_evidence","media_type":"application/json","path":"summary.json","max_bytes":64}]})).unwrap();
        (root, definition)
    }
    #[test]
    fn selected_outputs_capture_only_reviewed_bytes_and_preserve_content_identity() {
        let (root, definition) = fixture();
        let bytes = b"{\"reported\":true}";
        std::fs::write(root.path().join("summary.json"), bytes).unwrap();
        std::fs::write(root.path().join("private.json"), b"never selected").unwrap();
        let kept = RefCell::new(Vec::new());
        let rechecks = Cell::new(0);
        let outputs = publish(
            &definition,
            root.path(),
            || {
                rechecks.set(rechecks.get() + 1);
                Ok(())
            },
            |actual, mime| {
                assert_eq!(mime, "application/json");
                kept.borrow_mut().push(actual.to_vec());
                Ok(blake3::hash(actual).to_hex().to_string())
            },
        )
        .unwrap();
        assert_eq!(*kept.borrow(), vec![bytes.to_vec()]);
        assert_eq!(rechecks.get(), 3);
        assert_eq!(outputs[0].size_bytes, u64::try_from(bytes.len()).unwrap());
        assert_eq!(outputs[0].sha256, hex::encode(Sha256::digest(bytes)));
        assert!(
            publish(
                &definition,
                root.path(),
                || Ok(()),
                |_, _| Ok("a".repeat(64))
            )
            .is_err()
        );
    }
    #[test]
    fn selected_outputs_refuse_absent_oversized_symlink_and_hardlink_before_store() {
        let (root, definition) = fixture();
        let calls = Cell::new(0);
        let put = |_: &[u8], _: &str| {
            calls.set(calls.get() + 1);
            Ok("a".repeat(64))
        };
        let selected = root.path().join("summary.json");
        assert!(publish(&definition, root.path(), || Ok(()), put).is_err());
        std::fs::write(&selected, vec![0; 65]).unwrap();
        assert!(publish(&definition, root.path(), || Ok(()), put).is_err());
        std::fs::remove_file(&selected).unwrap();
        let outside = root.path().join("private.json");
        std::fs::write(&outside, b"private").unwrap();
        symlink(&outside, &selected).unwrap();
        assert!(publish(&definition, root.path(), || Ok(()), put).is_err());
        std::fs::remove_file(&selected).unwrap();
        std::fs::hard_link(&outside, &selected).unwrap();
        assert!(publish(&definition, root.path(), || Ok(()), put).is_err());
        assert_eq!(
            calls.get(),
            0,
            "unsafe bytes never reach scoped content publication"
        );
    }
    #[test]
    fn selected_outputs_recheck_source_and_custody_before_read_before_put_and_before_return() {
        let (root, definition) = fixture();
        std::fs::write(root.path().join("summary.json"), b"fresh").unwrap();
        for revoked_at in 1..=3 {
            let checks = Cell::new(0);
            let writes = Cell::new(0);
            let result = publish(
                &definition,
                root.path(),
                || {
                    checks.set(checks.get() + 1);
                    if checks.get() == revoked_at {
                        Err("original source/custody revoked".into())
                    } else {
                        Ok(())
                    }
                },
                |bytes, _| {
                    writes.set(writes.get() + 1);
                    Ok(blake3::hash(bytes).to_hex().to_string())
                },
            );
            assert!(result.is_err());
            assert_eq!(writes.get(), usize::from(revoked_at == 3));
        }
    }
    #[test]
    fn selected_outputs_owner_policy_refuses_traversal_duplicate_ids_and_unbounded_defaults() {
        let (_, mut definition) = fixture();
        for path in [
            "../private.json",
            "/private.json",
            "foo/../bar",
            "foo//bar",
            "foo/./bar",
        ] {
            definition.outputs[0].path = path.into();
            assert!(definition.validate().is_err());
        }
        definition.outputs[0].path = "summary.json".into();
        definition.outputs[0].max_bytes = MAX_OUTPUT_BYTES + 1;
        assert!(definition.validate().is_err());
        definition.outputs[0].max_bytes = 64;
        definition.outputs.push(definition.outputs[0].clone());
        assert!(definition.validate().is_err());
    }
}
