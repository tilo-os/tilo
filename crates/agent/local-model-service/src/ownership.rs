//! Persisted container custody and observed cleanup gates for the local runtime.
//! No container name or caller-supplied selector grants lifecycle authority.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Exact prepared container identity, persisted before starting model code.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerOwnership {
    pub container_id: String,
    pub run_id: String,
    pub image: String,
    pub model_id_sha256: String,
    pub arguments_sha256: String,
}

fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl ContainerOwnership {
    /// Refuse malformed persistent identities before passing any value to a process.
    ///
    /// # Errors
    /// Returns an error for noncanonical IDs or an image without its exact digest.
    pub fn validate(&self) -> Result<(), String> {
        let image = self
            .image
            .rsplit_once("@sha256:")
            .ok_or("container image is not digest pinned")?;
        if !digest(&self.container_id)
            || !digest(&self.run_id)
            || !digest(&self.model_id_sha256)
            || !digest(&self.arguments_sha256)
            || image.0.is_empty()
            || image.0.starts_with('-')
            || !image.0.contains('/')
            || !digest(image.1)
            || self.image.chars().any(char::is_whitespace)
        {
            return Err("invalid persisted local runtime ownership".into());
        }
        Ok(())
    }

    /// Exact unit name bound to this random run, never a broad model selector.
    #[must_use]
    pub fn unit(&self) -> String {
        format!("tilo-local-model-{}", self.run_id)
    }

    /// Require actual podman metadata to agree with the entire prepared identity.
    ///
    /// # Errors
    /// Returns an error for missing fields, a foreign container or changed image/labels.
    pub fn verify_observation(&self, body: &serde_json::Value) -> Result<(), String> {
        self.validate()?;
        let labels = body
            .pointer("/Config/Labels")
            .ok_or("missing container labels")?;
        let arguments: Vec<String> = serde_json::from_value(
            body.pointer("/Config/Cmd")
                .ok_or("missing container arguments")?
                .clone(),
        )
        .map_err(|_| "observed container arguments are absent or invalid")?;
        let expected_digest = self.image.rsplit_once('@').ok_or("image digest missing")?.1;
        if arguments_digest(&arguments) != self.arguments_sha256
            || body.get("Id").and_then(serde_json::Value::as_str)
                != Some(self.container_id.as_str())
            || body.get("ImageDigest").and_then(serde_json::Value::as_str) != Some(expected_digest)
            || body.get("ImageName").and_then(serde_json::Value::as_str)
                != Some(self.image.as_str())
            || labels
                .get("io.tilo.local-model.run")
                .and_then(serde_json::Value::as_str)
                != Some(self.run_id.as_str())
            || labels
                .get("io.tilo.local-model.model")
                .and_then(serde_json::Value::as_str)
                != Some(self.model_id_sha256.as_str())
            || labels
                .get("io.tilo.local-model.image")
                .and_then(serde_json::Value::as_str)
                != Some(self.image.as_str())
        {
            return Err(
                "observed container does not match persisted local runtime ownership".into(),
            );
        }
        Ok(())
    }
}

/// Domain-separated identity of exact argument boundaries, with no shell conversion.
#[must_use]
pub fn arguments_digest(arguments: &[String]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"tilo-local-model-argv-v1\0");
    for argument in arguments {
        hasher.update(argument.len().to_le_bytes());
        hasher.update(argument.as_bytes());
    }
    hex::encode(hasher.finalize())
}

/// A manager's successful stop alone does not establish process quiescence.
///
/// # Errors
/// Returns an error if the exact unit is active, queued or cannot be observed.
pub fn verify_stopped_unit(properties: &str) -> Result<(), String> {
    let mut fields = std::collections::BTreeMap::new();
    for line in properties.lines() {
        let (key, value) = line.split_once('=').ok_or("invalid manager property")?;
        if fields.insert(key, value).is_some() {
            return Err("duplicate manager property".into());
        }
    }
    let found = fields.get("LoadState") == Some(&"not-found");
    if !matches!(fields.get("LoadState"), Some(&"loaded" | &"not-found"))
        || !matches!(fields.get("ActiveState"), Some(&"inactive" | &"failed"))
        || !matches!(fields.get("Job"), Some(&"" | &"0"))
        || !(fields.get("MainPID") == Some(&"0") || found && !fields.contains_key("MainPID"))
    {
        return Err("local runtime unit cleanup is not observable and quiescent".into());
    }
    Ok(())
}

/// Detached custody when a newer canonical model state refuses admission.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OrphanCustody {
    schema: u16,
    model_id: String,
    process: crate::ProcessHandle,
}

fn private_directory(root: &std::path::Path) -> Result<std::path::PathBuf, String> {
    use std::os::unix::fs::DirBuilderExt;
    let directory = root.join("orphan-custody");
    match std::fs::DirBuilder::new().mode(0o700).create(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.to_string()),
    }
    let metadata = std::fs::symlink_metadata(&directory).map_err(|error| error.to_string())?;
    if !metadata.is_dir() || !tilo_files::native::private::private_to_me(&metadata) {
        return Err("orphan custody directory is not private and owned".into());
    }
    Ok(directory)
}

fn custody_id(handle: &crate::ProcessHandle) -> Result<&str, String> {
    let container = handle
        .container
        .as_ref()
        .ok_or("prepared runtime has no container custody")?;
    container.validate()?;
    if handle.id != container.unit() {
        return Err("prepared runtime unit differs from container custody".into());
    }
    Ok(&container.run_id)
}

pub(crate) fn retain_orphan(
    root: &std::path::Path,
    model: &str,
    handle: &crate::ProcessHandle,
) -> Result<String, String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let id = custody_id(handle)?;
    let directory = private_directory(root)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(directory.join(format!("{id}.json")))
        .map_err(|error| error.to_string())?;
    let receipt = OrphanCustody {
        schema: 1,
        model_id: model.into(),
        process: handle.clone(),
    };
    file.write_all(&serde_json::to_vec(&receipt).map_err(|error| error.to_string())?)
        .and_then(|()| file.sync_all())
        .map_err(|error| error.to_string())?;
    std::fs::File::open(&directory)
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())?;
    Ok(id.into())
}

/// Recover only an independently retained, exact prepared container receipt.
/// This does not adopt the receipt into canonical model state.
///
/// # Errors
/// Refuses malformed/private-file violations, current canonical custody,
/// unavailable observations, or unsuccessful supervisor cleanup.
pub fn recover_orphan<P: crate::ProcessSupervisor>(
    root: &std::path::Path,
    supervisor: &mut P,
    run_id: &str,
) -> Result<(), crate::ServiceError> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let refuse = |message| crate::ServiceError::Backend(message);
    if !digest(run_id) {
        return Err(refuse("invalid orphan run identity".into()));
    }
    let directory = private_directory(root).map_err(refuse)?;
    let path = directory.join(format!("{run_id}.json"));
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || !tilo_files::native::private::private_to_me(&metadata)
        || metadata.len() > 65536
    {
        return Err(refuse(
            "orphan custody is not a bounded private regular file".into(),
        ));
    }
    let mut body = Vec::new();
    file.take(65537).read_to_end(&mut body)?;
    if body.len() > 65536 {
        return Err(refuse("orphan custody grew beyond its bound".into()));
    }
    let receipt: OrphanCustody =
        serde_json::from_slice(&body).map_err(|error| refuse(error.to_string()))?;
    if receipt.schema != 1 || custody_id(&receipt.process).map_err(refuse)? != run_id {
        return Err(refuse("orphan custody receipt identity mismatch".into()));
    }
    let state = crate::load_state(&root.join(crate::STATE_FILE))?;
    let owned = receipt
        .process
        .container
        .as_ref()
        .ok_or_else(|| refuse("missing custody".into()))?;
    if state
        .models
        .values()
        .filter_map(|model| model.process.as_ref())
        .any(|handle| {
            handle.id == receipt.process.id
                || handle.container.as_ref().is_some_and(|container| {
                    container.container_id == owned.container_id || container.run_id == owned.run_id
                })
        })
    {
        return Err(refuse(
            "orphan recovery refuses current canonical process custody".into(),
        ));
    }
    supervisor.stop(&receipt.process)?;
    std::fs::remove_file(path)?;
    std::fs::File::open(directory)?.sync_all()?;
    Ok(())
}

/// Short private state transaction; never retained during a runtime operation.
pub(crate) struct StateFileLock;
impl StateFileLock {
    pub(crate) fn acquire(state: &std::path::Path) -> Result<advisory_lock::FileLock, String> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(state.with_extension("lock"))
            .map_err(|error| error.to_string())?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() || !tilo_files::native::private::private_to_me(&metadata) {
            return Err("local model state lock is not a private regular file".into());
        }
        let began = std::time::Instant::now();
        loop {
            match advisory_lock::FileLock::try_exclusive(
                file.try_clone().map_err(|error| error.to_string())?,
            ) {
                Ok(guard) => return Ok(guard),
                Err(advisory_lock::TryLockError::WouldBlock)
                    if began.elapsed() < std::time::Duration::from_secs(5) => {}
                Err(error) => {
                    return Err(format!(
                        "local model state transaction unavailable: {error}"
                    ));
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn owned() -> ContainerOwnership {
        ContainerOwnership {
            container_id: "a".repeat(64),
            run_id: "b".repeat(64),
            image: format!("registry.example/runtime@sha256:{}", "c".repeat(64)),
            model_id_sha256: "d".repeat(64),
            arguments_sha256: arguments_digest(&["serve".into()]),
        }
    }

    fn observed(owned: &ContainerOwnership) -> serde_json::Value {
        serde_json::json!({"Id":owned.container_id,"ImageName":owned.image,
            "ImageDigest":format!("sha256:{}", "c".repeat(64)),
            "Config":{"Cmd":["serve"],"Labels":{"io.tilo.local-model.run":owned.run_id,
                "io.tilo.local-model.model":owned.model_id_sha256,
                "io.tilo.local-model.image":owned.image}}})
    }

    #[test]
    fn exact_container_gate_refuses_foreign_identity_image_or_labels() {
        let owned = owned();
        let body = observed(&owned);
        owned.verify_observation(&body).unwrap();
        for (pointer, value) in [
            ("/Config/Cmd", serde_json::json!(["foreign-model"])),
            ("/Id", serde_json::json!("e".repeat(64))),
            (
                "/ImageDigest",
                serde_json::json!(format!("sha256:{}", "e".repeat(64))),
            ),
            (
                "/ImageName",
                serde_json::json!("registry.example/runtime:latest"),
            ),
            (
                "/Config/Labels/io.tilo.local-model.run",
                serde_json::json!("e".repeat(64)),
            ),
            (
                "/Config/Labels/io.tilo.local-model.model",
                serde_json::Value::Null,
            ),
            (
                "/Config/Labels/io.tilo.local-model.image",
                serde_json::Value::Null,
            ),
        ] {
            let mut changed = body.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(owned.verify_observation(&changed).is_err(), "{pointer}");
        }
        assert!(owned.verify_observation(&serde_json::json!({})).is_err());
    }

    #[test]
    fn persisted_container_selector_cannot_be_a_prefix_flag_or_tag() {
        for malformed in ["", "a", "--all", "A", "a\n"] {
            let mut changed = owned();
            changed.container_id = malformed.into();
            assert!(changed.validate().is_err());
        }
        let mut changed = owned();
        changed.image = "registry.example/runtime:latest".into();
        assert!(changed.validate().is_err());
        changed = owned();
        changed.run_id = "../another-unit".into();
        assert!(changed.validate().is_err());
    }

    #[test]
    fn stopped_unit_requires_no_queued_work_and_observable_quiescence() {
        verify_stopped_unit("LoadState=loaded\nActiveState=inactive\nMainPID=0\nJob=\n").unwrap();
        verify_stopped_unit("LoadState=not-found\nActiveState=inactive\nJob=\n").unwrap();
        for properties in [
            "",
            "LoadState=loaded\nActiveState=activating\nMainPID=0\nJob=\n",
            "LoadState=loaded\nActiveState=inactive\nMainPID=0\nJob=12\n",
            "LoadState=loaded\nActiveState=inactive\nMainPID=4\nJob=\n",
            "LoadState=loaded\nActiveState=inactive\nMainPID=0\n",
            "LoadState=loaded\nActiveState=inactive\nJob=\n",
            "LoadState=error\nActiveState=inactive\nMainPID=0\nJob=\n",
            "LoadState=loaded\nActiveState=active\nActiveState=inactive\nMainPID=0\nJob=\n",
        ] {
            assert!(verify_stopped_unit(properties).is_err(), "{properties}");
        }
    }
}
