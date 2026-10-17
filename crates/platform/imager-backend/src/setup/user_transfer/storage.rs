use super::{TransferRequest, flatpak};
use crate::setup::{home_transfer::HomeReview, migration::PinnedDirectory};
use cap_fs_ext::OpenOptionsFollowExt;
use cap_primitives::fs::{FollowSymlinks, OpenOptions};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    path::Path,
};

#[derive(Clone)]
pub(super) struct PreparedRemote {
    pub(super) approved: flatpak::ApprovedRemote,
    bytes: Vec<u8>,
}

pub(super) fn validate_grants(request: &TransferRequest) -> Result<(), String> {
    let source =
        PinnedDirectory::open_absolute(&request.source_home).map_err(|error| error.to_string())?;
    let journals = PinnedDirectory::open_absolute(&request.private_journals)
        .map_err(|error| error.to_string())?;
    journals
        .validate_destination()
        .map_err(|error| error.to_string())?;
    crate::setup::destination_security::check_private_file(&journals.file)
        .map_err(|error| error.to_string())?;
    if journals.contains_identity(&source.identity)
        || request.private_journals.starts_with(&request.target_home)
    {
        return Err("Private transfer journals must be outside source and target homes".into());
    }
    Ok(())
}

pub(super) fn target_identity(
    path: &Path,
) -> Result<crate::setup::migration_durability::Identity, String> {
    let pinned = PinnedDirectory::open_absolute(path).map_err(|error| error.to_string())?;
    pinned
        .validate_destination()
        .map_err(|error| error.to_string())?;
    let file = crate::setup::migration_durability::readable_directory(&pinned.file)
        .map_err(|error| error.to_string())?;
    crate::setup::migration_durability::identity(&file).map_err(|error| error.to_string())
}

pub(super) fn review_flatpak_data(request: &TransferRequest) -> Result<Vec<HomeReview>, String> {
    let parent = PinnedDirectory::open_absolute(
        request
            .target_home
            .parent()
            .ok_or("Target home must name a new directory")?,
    )
    .map_err(|error| error.to_string())?;
    let source =
        PinnedDirectory::open_absolute(&request.source_home).map_err(|error| error.to_string())?;
    if parent.contains_identity(&source.identity)
        || request.source_home.starts_with(&request.target_home)
    {
        return Err("Transfer target must be outside source home".into());
    }
    let root = parent
        .create_child(
            request
                .target_home
                .file_name()
                .ok_or("Target home has no name")?,
        )
        .map_err(|error| error.to_string())?;
    let local = root
        .create_child(std::ffi::OsStr::new(".local"))
        .map_err(|error| error.to_string())?;
    local
        .create_child(std::ffi::OsStr::new("share"))
        .map_err(|error| error.to_string())?;
    root.create_child(std::ffi::OsStr::new(".var"))
        .map_err(|error| error.to_string())?;
    let mut result = Vec::new();
    for subtree in flatpak::data_subtrees(
        &request.source_home,
        &request.target_home,
        &request.private_journals,
        false,
    )? {
        match std::fs::symlink_metadata(&subtree.source) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
            Ok(_) => result.push(flatpak::review_subtree(&subtree)?),
        }
    }
    Ok(result)
}

pub(super) fn prepare_remotes(
    runner: &impl flatpak::Runner,
    request: &TransferRequest,
) -> Result<(Vec<PreparedRemote>, Vec<(String, String)>), String> {
    if request.system_refs.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let inventory = flatpak::discover(runner, &request.source_home)?;
    let mut target_refs = BTreeSet::new();
    for selected in &request.system_refs {
        if selected.installation == flatpak::Installation::User
            || !inventory.refs.contains(selected)
        {
            return Err("Selected Flatpak installation changed; review apps again".into());
        }
        if !target_refs.insert(selected.reference.clone()) {
            return Err(
                "Choose one Flatpak installation for each target application or runtime".into(),
            );
        }
        if inventory.refs.iter().any(|item| {
            item.installation == flatpak::Installation::User && item.reference == selected.reference
        }) {
            return Err(
                "The user's Flatpak version is already included in the data transfer".into(),
            );
        }
    }
    let roots = flatpak::installation_roots(runner, &request.source_home, &inventory)?;
    let mut remotes = BTreeMap::new();
    let mut failed = Vec::new();
    let mut seen = BTreeSet::new();
    for selected in &request.system_refs {
        if !seen.insert((selected.installation.clone(), selected.reference.clone())) {
            return Err("Duplicate Flatpak selection".into());
        }
        let key = (selected.installation.clone(), selected.origin.clone());
        if remotes.contains_key(&key) {
            continue;
        }
        let definition = inventory
            .remotes
            .iter()
            .find(|remote| {
                remote.installation == selected.installation && remote.name == selected.origin
            })
            .ok_or_else(|| "Flatpak source repository disappeared".to_owned())
            .and_then(|remote| {
                flatpak::extract_remote_definition(
                    remote,
                    roots
                        .get(&selected.installation)
                        .ok_or("Flatpak installation location unavailable")?,
                )
            });
        match definition {
            Ok(bytes) => {
                let identity = serde_json::to_vec(&key).map_err(|error| error.to_string())?;
                let digest = Sha256::digest(identity);
                let name = format!(
                    "tilo-migrated-{}",
                    hex::encode(digest.get(..8).ok_or("Repository digest is truncated")?)
                );
                remotes.insert(
                    key,
                    PreparedRemote {
                        approved: flatpak::ApprovedRemote {
                            source_installation: selected.installation.clone(),
                            source_name: selected.origin.clone(),
                            target_name: name.clone(),
                            definition: request
                                .private_journals
                                .join(format!("{name}.flatpakrepo")),
                        },
                        bytes,
                    },
                );
            }
            Err(error) => failed.push((selected.reference.clone(), error)),
        }
    }
    Ok((remotes.into_values().collect(), failed))
}

pub(super) fn publish_remotes(
    journals: &Path,
    remotes: &[PreparedRemote],
) -> Result<Vec<flatpak::ApprovedRemote>, String> {
    let directory = PinnedDirectory::open_absolute(journals).map_err(|error| error.to_string())?;
    directory
        .validate_destination()
        .map_err(|error| error.to_string())?;
    crate::setup::destination_security::check_private_file(&directory.file)
        .map_err(|error| error.to_string())?;
    for remote in remotes {
        let name = remote
            .approved
            .definition
            .file_name()
            .ok_or("Flatpak repository has no file name")?;
        let mut options = OpenOptions::new();
        use cap_primitives::fs::OpenOptionsExt;
        options
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No)
            .mode(0o600);
        match cap_primitives::fs::open(&directory.file, Path::new(name), &options) {
            Ok(mut file) => {
                file.write_all(&remote.bytes)
                    .map_err(|error| error.to_string())?;
                file.sync_all().map_err(|error| error.to_string())?;
                crate::setup::migration_durability::sync_directory(&directory.file)
                    .map_err(|error| error.to_string())?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let mut options = OpenOptions::new();
                options.read(true).follow(FollowSymlinks::No);
                let file = cap_primitives::fs::open(&directory.file, Path::new(name), &options)
                    .map_err(|error| error.to_string())?;
                crate::setup::destination_security::check_private_file(&file)
                    .map_err(|error| error.to_string())?;
                let mut bytes = Vec::new();
                file.take(2 * 1024 * 1024)
                    .read_to_end(&mut bytes)
                    .map_err(|error| error.to_string())?;
                if bytes != remote.bytes {
                    return Err("Prepared Flatpak repository changed; review again".into());
                }
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(remotes
        .iter()
        .map(|remote| remote.approved.clone())
        .collect())
}
