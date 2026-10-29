use super::TransferRequest;
use crate::setup::migration::PinnedDirectory;
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

/// The admitted tree is a new directory outside the source home.
pub(super) fn validate_target(request: &TransferRequest) -> Result<(), String> {
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
        || request.target_home.file_name().is_none()
    {
        return Err("Transfer target must be outside source home".into());
    }
    Ok(())
}
