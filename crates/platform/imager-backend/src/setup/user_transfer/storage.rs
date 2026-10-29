use super::{TransferRequest, flatpak};
use crate::setup::{home_transfer::HomeReview, migration::PinnedDirectory};
use std::path::Path;

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
