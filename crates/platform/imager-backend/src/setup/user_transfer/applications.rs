//! Signed transaction continuity backed by actual tree bytes and original fidelity.
use super::{TransferRequest, durable, flatpak};
use crate::setup::{
    migration::{self, PinnedDirectory},
    migration_durability as durability,
    migration_journal::Snapshot,
    migration_store::NativePath,
    nofollow::metadata_nofollow,
};
use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, MetadataExt, OpenOptions};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};
const ORIGINAL: &str = "user-transfer-original-applications.json";
const OUTPUT: &str = "user-transfer-application-output.json";
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RefState {
    pub reference: String,
    pub origin: String,
    pub commit: String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: u32,
    digest: [u8; 32],
    refs: Vec<RefState>,
    #[serde(default)]
    relocation: Option<Relocation>,
}
#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Relocation {
    root: durability::Identity,
    digest: [u8; 32],
}
pub fn states(runner: &impl flatpak::Runner, home: &Path) -> Result<Vec<RefState>, String> {
    let mut states = Vec::new();
    let mut seen = BTreeSet::new();
    for (kind, flag) in [("app", "--app"), ("runtime", "--runtime")] {
        let output = runner.run(
            home,
            &[
                "list".into(),
                "--user".into(),
                flag.into(),
                "--columns=ref:f,origin:f".into(),
            ],
        )?;
        for line in output.lines().filter(|line| !line.is_empty()) {
            let (reference, origin) = line
                .split_once('\t')
                .ok_or("Invalid target application inventory")?;
            if reference.split('/').count() != 3
                || !reference
                    .bytes()
                    .all(|v| v.is_ascii_alphanumeric() || matches!(v, b'/' | b'.' | b'_' | b'-'))
                || origin.is_empty()
                || !origin
                    .bytes()
                    .all(|v| v.is_ascii_alphanumeric() || matches!(v, b'_' | b'-'))
            {
                return Err("Invalid target application reference or repository".into());
            }
            let reference = format!("{kind}/{reference}");
            if !seen.insert(reference.clone()) {
                return Err("Duplicate target application reference".into());
            }
            let commit = runner
                .run(
                    home,
                    &[
                        "info".into(),
                        "--user".into(),
                        "--show-commit".into(),
                        reference.clone(),
                    ],
                )?
                .trim()
                .to_owned();
            if commit.len() != 64
                || !commit
                    .bytes()
                    .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
            {
                return Err("Invalid deployed application commit".into());
            }
            states.push(RefState {
                reference,
                origin: origin.into(),
                commit,
            });
        }
    }
    states.sort_by(|a, b| a.reference.cmp(&b.reference));
    Ok(states)
}
pub(super) fn begin(
    request: &TransferRequest,
    runner: &impl flatpak::Runner,
) -> Result<(), String> {
    if durable::read_artifact(request, ORIGINAL)?.is_none() {
        let original = states(runner, &request.target_home)?;
        durable::write_artifact(
            request,
            ORIGINAL,
            &serde_json::to_value(original).map_err(|error| error.to_string())?,
        )?;
    }
    durable::applications_started(request).map_err(|error| error.to_string())
}
pub(super) fn validate_recovery(request: &TransferRequest) -> Result<(), String> {
    let original: Vec<RefState> = serde_json::from_value(
        durable::read_artifact(request, ORIGINAL)?
            .ok_or("Original application phase receipt is missing")?,
    )
    .map_err(|error| error.to_string())?;
    if original.len() > 100_000 {
        return Err("Original application count exceeds its bound".into());
    }
    Ok(())
}
pub(super) fn verify_original_config(request: &TransferRequest) -> Result<(), String> {
    let relative = Path::new(".local/share/flatpak/repo/config");
    let source = request.source_home.join(relative);
    match std::fs::symlink_metadata(&source) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
        Ok(_) => {}
    }
    let original = config(&source)?;
    let actual = config(&request.target_home.join(relative))?;
    if original
        .iter()
        .any(|(key, value)| actual.get(key) != Some(value))
    {
        return Err("An original Flatpak configuration or signing policy changed".into());
    }
    Ok(())
}
pub fn config(path: &Path) -> Result<BTreeMap<(String, String), String>, String> {
    let parent = PinnedDirectory::open_absolute(path.parent().ok_or("Missing repository parent")?)
        .map_err(|error| error.to_string())?;
    let name = path.file_name().ok_or("Missing configuration name")?;
    let expected =
        metadata_nofollow(&parent.file, Path::new(name)).map_err(|error| error.to_string())?;
    if !expected.is_file() || expected.is_symlink() || expected.len() > 2 * 1024 * 1024 {
        return Err("Repository configuration is unavailable".into());
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let file = cap_primitives::fs::open(&parent.file, Path::new(name), &options)
        .map_err(|error| error.to_string())?;
    let before =
        cap_primitives::fs::Metadata::from_file(&file).map_err(|error| error.to_string())?;
    if !migration::same_snapshot(&expected, &before) {
        return Err("Repository configuration changed while opening".into());
    }
    let mut text = String::new();
    file.try_clone()
        .map_err(|error| error.to_string())?
        .take(2 * 1024 * 1024 + 1)
        .read_to_string(&mut text)
        .map_err(|error| error.to_string())?;
    if !migration::same_snapshot(
        &before,
        &cap_primitives::fs::Metadata::from_file(&file).map_err(|error| error.to_string())?,
    ) {
        return Err("Repository configuration changed while reading".into());
    }
    if text.len() > 2 * 1024 * 1024 {
        return Err("Repository configuration exceeds its bound".into());
    }
    parent.validate().map_err(|error| error.to_string())?;
    parse_config(&text)
}
pub fn parse_config(text: &str) -> Result<BTreeMap<(String, String), String>, String> {
    let mut section = String::new();
    let mut result = BTreeMap::new();
    for line in text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with(['#', ';']))
    {
        if line.starts_with('[') && line.ends_with(']') {
            section = line.into();
        } else if let Some((key, value)) = line.split_once('=') {
            if result
                .insert((section.clone(), key.trim().into()), value.trim().into())
                .is_some()
            {
                return Err("Ambiguous original repository configuration".into());
            }
        } else {
            return Err("Unsupported repository configuration".into());
        }
    }
    Ok(result)
}
fn verify_refs(
    request: &TransferRequest,
    runner: &impl flatpak::Runner,
    approved: &[flatpak::ApprovedRemote],
) -> Result<Vec<RefState>, String> {
    let original: Vec<RefState> = serde_json::from_value(
        durable::read_artifact(request, ORIGINAL)?
            .ok_or("Original application phase receipt is missing")?,
    )
    .map_err(|error| error.to_string())?;
    let actual = states(runner, &request.target_home)?;
    if original.iter().any(|item| !actual.contains(item)) {
        return Err("An original user application changed or disappeared".into());
    }
    let mut expected: BTreeMap<String, String> = original
        .iter()
        .map(|item| (item.reference.clone(), item.origin.clone()))
        .collect();
    for item in &request.system_refs {
        let remote = approved
            .iter()
            .find(|remote| {
                remote.source_installation == item.installation && remote.source_name == item.origin
            })
            .ok_or("Reviewed repository is unavailable")?;
        if expected
            .insert(item.reference.clone(), remote.target_name.clone())
            .is_some()
        {
            return Err("Application restoration would replace an original user version".into());
        }
    }
    if actual.len() != expected.len()
        || actual
            .iter()
            .any(|item| expected.get(&item.reference) != Some(&item.origin))
    {
        return Err(
            "Restored application set or repository differs from the reviewed selection".into(),
        );
    }
    Ok(actual)
}
pub(super) fn checkpoint(
    request: &TransferRequest,
    runner: &impl flatpak::Runner,
    approved: &[flatpak::ApprovedRemote],
) -> Result<(), String> {
    verify_original_config(request)?;
    runner.verify_repository(&request.target_home)?;
    let receipt = Receipt {
        schema: 1,
        refs: verify_refs(request, runner, approved)?,
        digest: tree_digest(request)?,
        relocation: Some(relocation_digest(request)?),
    };
    durable::write_artifact(
        request,
        OUTPUT,
        &serde_json::to_value(receipt).map_err(|error| error.to_string())?,
    )
}
pub(super) fn verify_checkpoint(request: &TransferRequest) -> Result<(), String> {
    let value = durable::read_artifact(request, OUTPUT)?
        .ok_or("Completed signed application output receipt missing")?;
    let receipt: Receipt = serde_json::from_value(value).map_err(|e| e.to_string())?;
    verify_original_config(request)?;
    if receipt.schema != 1 || receipt.digest != tree_digest(request)? {
        return Err("Application output bytes changed after verification".into());
    }
    Ok(())
}
pub(super) fn verify_output(
    request: &TransferRequest,
    runner: &impl flatpak::Runner,
    approved: &[flatpak::ApprovedRemote],
) -> Result<bool, String> {
    let Some(value) = durable::read_artifact(request, OUTPUT)? else {
        return Ok(false);
    };
    let receipt: Receipt = serde_json::from_value(value).map_err(|error| error.to_string())?;
    verify_original_config(request)?;
    runner.verify_repository(&request.target_home)?;
    if receipt.schema != 1
        || receipt.digest != tree_digest(request)?
        || receipt.refs != verify_refs(request, runner, approved)?
    {
        return Err("Application output changed after its verified transaction".into());
    }
    Ok(true)
}
/// Only a complete installation subtree at one journal-witnessed root can
/// receive this proof. An arbitrary active-home path is never a relocation grant.
pub(super) fn verify_adopted_output(
    request: &TransferRequest,
    target_home: &Path,
    runner: &impl flatpak::Runner,
    approved: &[flatpak::ApprovedRemote],
) -> Result<(), String> {
    let value = durable::read_artifact(request, OUTPUT)?
        .ok_or("Completed application output receipt is missing")?;
    let receipt: Receipt = serde_json::from_value(value).map_err(|error| error.to_string())?;
    let mut mapped = request.clone();
    target_home.clone_into(&mut mapped.target_home);
    let refusal = "Relocated application output differs from the verified signed transaction";
    if receipt.schema != 1 || receipt.relocation != Some(relocation_digest(&mapped)?) {
        return Err(refusal.into());
    }
    verify_original_config(&mapped)?;
    runner.verify_repository(&mapped.target_home)?;
    if receipt.refs != verify_refs(&mapped, runner, approved)?
        || receipt.relocation != Some(relocation_digest(&mapped)?)
    {
        return Err(refusal.into());
    }
    Ok(())
}
fn relocation_digest(request: &TransferRequest) -> Result<Relocation, String> {
    let root = PinnedDirectory::open_absolute(&request.target_home.join(".local/share/flatpak"))
        .map_err(|error| error.to_string())?;
    let inventory = crate::setup::migration_review::SourceInventory::capture_home(&root)
        .map_err(|error| error.to_string())?;
    let readable = durability::readable_directory(&root.file).map_err(|error| error.to_string())?;
    let identity = durability::identity(&readable).map_err(|error| error.to_string())?;
    let metadata =
        cap_primitives::fs::Metadata::from_file(&readable).map_err(|error| error.to_string())?;
    let mut hash = Sha256::new();
    // Renaming this root changes ctime, but not its inode, permissions or mtime.
    hash.update(
        serde_json::to_vec(&(metadata.mode(), metadata.mtime(), metadata.mtime_nsec()))
            .map_err(|error| error.to_string())?,
    );
    let mut count = 0;
    hash_tree(&root, Path::new(""), &mut hash, &mut count, 0).map_err(|error| error.to_string())?;
    inventory
        .validate(&root)
        .map_err(|error| error.to_string())?;
    Ok(Relocation {
        root: identity,
        digest: hash.finalize().into(),
    })
}
pub fn tree_digest(request: &TransferRequest) -> Result<[u8; 32], String> {
    let root = PinnedDirectory::open_absolute(&request.target_home.join(".local/share/flatpak"))
        .map_err(|error| error.to_string())?;
    let inventory = crate::setup::migration_review::SourceInventory::capture_home(&root)
        .map_err(|error| error.to_string())?;
    let mut hash = Sha256::new();
    hash.update(inventory.revision().map_err(|error| error.to_string())?);
    let mut count = 0;
    hash_tree(&root, Path::new(""), &mut hash, &mut count, 0).map_err(|error| error.to_string())?;
    inventory
        .validate(&root)
        .map_err(|error| error.to_string())?;
    Ok(hash.finalize().into())
}
fn hash_tree(
    root: &Arc<PinnedDirectory>,
    relative: &Path,
    hash: &mut Sha256,
    count: &mut usize,
    depth: usize,
) -> std::io::Result<()> {
    if depth > 128 || *count > 1_000_000 {
        return Err(std::io::Error::other("Application tree exceeds its bound"));
    }
    let directory = cap_std::fs::Dir::from_std_file(durability::readable_directory(&root.file)?);
    let mut names = directory
        .entries()?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    names.sort();
    for name in names {
        *count = count
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("Application count overflow"))?;
        let path = relative.join(&name);
        let metadata = metadata_nofollow(&root.file, Path::new(&name))?;
        let bytes = serde_json::to_vec(&(
            NativePath::encode(&path),
            Snapshot::new(&metadata)?,
            metadata.mode(),
        ))?;
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
        if metadata.is_symlink() {
            let link = cap_primitives::fs::read_link_contents(&root.file, Path::new(&name))?;
            let bytes = serde_json::to_vec(&NativePath::encode(&link))?;
            hash.update((bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        } else if metadata.is_dir() {
            hash_tree(
                &root.open_child(&name, &metadata)?,
                &path,
                hash,
                count,
                depth + 1,
            )?;
        } else if metadata.is_file() {
            let mut options = OpenOptions::new();
            options.read(true).follow(FollowSymlinks::No).nonblock(true);
            let mut file = cap_primitives::fs::open(&root.file, Path::new(&name), &options)?;
            if !migration::same_snapshot(
                &metadata,
                &cap_primitives::fs::Metadata::from_file(&file)?,
            ) {
                return Err(std::io::Error::other(
                    "Application file changed while opening",
                ));
            }
            file.seek(SeekFrom::Start(0))?;
            let (_, digest) =
                migration::stream(&mut file, None, metadata.len(), &AtomicBool::new(false))?;
            hash.update(digest);
        } else {
            return Err(std::io::Error::other(
                "Application repository contains an unsupported inode",
            ));
        }
    }
    root.validate_destination()
}

#[cfg(test)]
mod tests {
    #[test]
    fn original_repository_configuration_policy_and_literal_file_authority_survive() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("target");
        for home in [&source, &target] {
            std::fs::create_dir_all(home.join(".local/share/flatpak/repo")).unwrap();
        }
        let request = crate::setup::user_transfer::TransferRequest {
            source_home: source.clone(),
            target_home: target.clone(),
            private_journals: root.path().join("private"),
            copy_home: true,
            copy_flatpak: true,
            system_refs: Vec::new(),
        };
        let original = source.join(".local/share/flatpak/repo/config");
        let output = target.join(".local/share/flatpak/repo/config");
        std::fs::write(&original, b"[core]\nmode=bare-user-only\n[remote \"personal\"]\ngpg-verify=true\nurl=https://example.org/repo\n").unwrap();
        std::fs::write(&output, b"[core]\nmode=bare-user-only\n[remote \"personal\"]\ngpg-verify=true\nurl=https://example.org/repo\n[remote \"new\"]\ngpg-verify=true\n").unwrap();
        super::verify_original_config(&request).unwrap();
        std::fs::write(&output, b"[core]\nmode=bare-user-only\n[remote \"personal\"]\ngpg-verify=false\nurl=https://example.org/repo\n").unwrap();
        assert!(super::verify_original_config(&request).is_err());
        std::fs::remove_file(&output).unwrap();
        std::os::unix::fs::symlink(&original, &output).unwrap();
        assert!(super::verify_original_config(&request).is_err());
    }
}
