//! An owned inventory binds the remote mirror to the caller's actual source
//! bytes, including dirty SDK/UI files. Git metadata need not be mirrored.

use std::collections::BTreeMap;
use std::path::{Component, Path};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::{build, context::Worktree, error::CliError};

#[derive(Serialize, Deserialize)]
struct Inputs {
    provenance: tilo_vm_protocol::Provenance,
    files: BTreeMap<String, String>,
}

pub(crate) struct Snapshot {
    pub file: tempfile::NamedTempFile,
    pub digest: String,
    pub provenance: tilo_vm_protocol::Provenance,
    paths: Vec<String>,
}

fn input_hash(root: &Path, path: &Path) -> Result<String, CliError> {
    let metadata = std::fs::symlink_metadata(path).map_err(CliError::io("read source metadata"))?;
    let resolved = path
        .canonicalize()
        .map_err(CliError::io("resolve source input"))?;
    if !resolved.starts_with(
        root.canonicalize()
            .map_err(CliError::io("resolve source root"))?,
    ) {
        return Err(CliError::InvalidInput(
            "source input escapes the worktree".into(),
        ));
    }
    let bytes = if metadata.is_symlink() {
        std::fs::read_link(path)
            .map_err(CliError::io("read source symlink"))?
            .as_os_str()
            .as_encoded_bytes()
            .to_vec()
    } else if metadata.is_file() {
        std::fs::read(path).map_err(CliError::io("read source bytes"))?
    } else {
        return Err(CliError::InvalidInput(
            "source inventory contains a non-file".into(),
        ));
    };
    let mut encoded = vec![u8::from(metadata.is_symlink())];
    encoded.extend(bytes);
    if metadata.is_symlink() {
        encoded
            .extend(std::fs::read(&resolved).map_err(CliError::io("read symlink source bytes"))?);
    }
    Ok(build::sha256_hex(&encoded))
}

impl Snapshot {
    pub(crate) fn unchanged(&self, root: &Path) -> Result<(), CliError> {
        let worktree = Worktree {
            root: root.to_owned(),
            branch: None,
        };
        if worktree.head() != self.provenance.source_commit
            || listed_paths(root)? != self.paths
            || verify(root, self.file.path())? != self.digest
        {
            return Err(CliError::InvalidInput(
                "source inventory changed during delivery".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn create(worktree: &Worktree, component: &str) -> Result<Self, CliError> {
        let paths = listed_paths(&worktree.root)?;
        let mut files = BTreeMap::new();
        for path in &paths {
            let full = worktree.root.join(path);
            files.insert(path.clone(), input_hash(&worktree.root, &full)?);
        }
        let provenance = build::source_provenance(worktree, component, "remote");
        let bytes = serde_json::to_vec(&Inputs {
            provenance: provenance.clone(),
            files,
        })
        .map_err(|error| CliError::InvalidInput(error.to_string()))?;
        let file = tempfile::Builder::new()
            .prefix(".tilo-vm-inputs-")
            .suffix(".json")
            .tempfile_in(&worktree.root)
            .map_err(CliError::io("create source inventory"))?;
        std::fs::write(file.path(), &bytes).map_err(CliError::io("write source inventory"))?;
        Ok(Self {
            file,
            digest: build::sha256_hex(&bytes),
            provenance,
            paths,
        })
    }
}

fn listed_paths(root: &Path) -> Result<Vec<String>, CliError> {
    let listed = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z", "-co", "--exclude-standard"])
        .output()
        .map_err(CliError::io("list source inputs"))?;
    if !listed.status.success() {
        return Err(CliError::InvalidInput(
            "cannot enumerate source inputs".into(),
        ));
    }
    let mut paths = Vec::new();
    for raw in listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|raw| !raw.is_empty())
    {
        let path = std::str::from_utf8(raw)
            .map_err(|_| CliError::InvalidInput("source path is not UTF-8".into()))?;
        if path.split('/').any(|part| part == ".git")
            || path.starts_with("target/")
            || path.starts_with(".tilo-vm-inputs-")
        {
            continue;
        }
        if !root.join(path).exists() {
            continue;
        }
        paths.push(path.to_owned());
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

pub(crate) fn verify(root: &Path, inventory: &Path) -> Result<String, CliError> {
    use std::io::Read as _;
    let file = std::fs::File::open(inventory).map_err(CliError::io("open source inventory"))?;
    let mut bytes = Vec::new();
    file.take((32 << 20) + 1)
        .read_to_end(&mut bytes)
        .map_err(CliError::io("read source inventory"))?;
    if bytes.len() > 32 << 20 {
        return Err(CliError::InvalidInput(
            "source inventory exceeds its bound".into(),
        ));
    }
    let inputs: Inputs = serde_json::from_slice(&bytes)
        .map_err(|_| CliError::InvalidInput("invalid source inventory".into()))?;
    if inputs.files.is_empty()
        || inputs.files.len() > 65_536
        || inputs.provenance.source_commit.is_none()
    {
        return Err(CliError::InvalidInput(
            "source inventory has no committed source identity or exceeds its bound".into(),
        ));
    }
    for (path, hash) in inputs.files {
        if path.len() > 4096
            || !Path::new(&path)
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
            || input_hash(root, &root.join(&path))? != hash
        {
            return Err(CliError::InvalidInput(
                "remote source bytes differ from the requested inventory".into(),
            ));
        }
    }
    Ok(build::sha256_hex(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_app_source_inventory_detects_dirty_sdk_and_cleans_owned_file() {
        let root = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .current_dir(root.path())
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        git(&["init", "-q"]);
        std::fs::create_dir(root.path().join("sdk")).unwrap();
        std::fs::write(root.path().join("sdk/ui.rs"), "initial").unwrap();
        git(&["add", "sdk/ui.rs"]);
        git(&[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "-qm",
            "initial",
        ]);
        std::fs::write(root.path().join("sdk/ui.rs"), "dirty SDK").unwrap();
        let worktree = Worktree::detect(root.path()).unwrap();
        let snapshot = Snapshot::create(&worktree, "app:terminal").unwrap();
        let path = snapshot.file.path().to_path_buf();
        assert_eq!(verify(root.path(), &path).unwrap(), snapshot.digest);
        snapshot.unchanged(root.path()).unwrap();
        std::fs::write(root.path().join("new-input.rs"), "new source").unwrap();
        assert!(
            snapshot.unchanged(root.path()).is_err(),
            "new source files must not enter a frozen cohort"
        );
        std::fs::remove_file(root.path().join("new-input.rs")).unwrap();
        git(&[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "--allow-empty",
            "-qm",
            "different head",
        ]);
        assert!(
            snapshot.unchanged(root.path()).is_err(),
            "source commit changes must invalidate the cohort"
        );
        git(&["reset", "--soft", "HEAD~1"]);
        snapshot.unchanged(root.path()).unwrap();
        let original = std::fs::read(&path).unwrap();
        let mut altered: serde_json::Value = serde_json::from_slice(&original).unwrap();
        altered["provenance"]["builder"] = serde_json::json!("different builder");
        std::fs::write(&path, serde_json::to_vec(&altered).unwrap()).unwrap();
        assert!(snapshot.unchanged(root.path()).is_err());
        std::fs::write(&path, original).unwrap();
        std::fs::write(root.path().join("sdk/ui.rs"), "other SDK").unwrap();
        assert!(verify(root.path(), &path).is_err());
        drop(snapshot);
        assert!(!path.exists());
    }
}
