//! Exact selected app state; no entire user installation or unselected app data.
use super::InstalledRef;
use crate::setup::{
    destination_security,
    migration::{PinnedDirectory, changed, check_cancel},
    migration_durability as durable,
    migration_review::SourceInventory,
    nofollow::metadata_nofollow,
};
use cap_fs_ext::OpenOptionsFollowExt;
use cap_primitives::fs::{FollowSymlinks, OpenOptions, OpenOptionsExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    ffi::OsStr,
    io::{self, Read},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};
use tilo_files::native::atomic::{Privacy, PublishMode, StorageDirectory};
use zeroize::Zeroizing;
const OVERRIDE_LIMIT: u64 = 128 * 1024;
const MAX_APPS: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppState {
    pub app_id: String,
    pub data_present: bool,
    pub override_present: bool,
}
impl AppState {
    pub(super) fn validate_selected(states: &[Self], selected: &[InstalledRef]) -> io::Result<()> {
        let expected: BTreeSet<_> = selected.iter().filter_map(InstalledRef::app_id).collect();
        let mut actual = BTreeSet::new();
        if states.len() > MAX_APPS {
            return Err(changed(
                "Selected application state exceeds supported bounds",
            ));
        }
        for state in states {
            if !valid_id(&state.app_id) || !actual.insert(state.app_id.as_str()) {
                return Err(changed("Invalid selected application state"));
            }
        }
        if actual != expected {
            return Err(changed(
                "Application state differs from selected application IDs",
            ));
        }
        Ok(())
    }
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 255
        && !id.starts_with('.')
        && !id.ends_with('.')
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}
struct DirectorySource {
    path: PathBuf,
    pin: Arc<PinnedDirectory>,
    inventory: SourceInventory,
}
struct SelectedApp {
    declaration: AppState,
    data: Option<DirectorySource>,
    override_path: PathBuf,
    override_sha256: Option<[u8; 32]>,
}
pub(super) struct SelectedState {
    home: PathBuf,
    home_pin: Arc<PinnedDirectory>,
    apps: Vec<SelectedApp>,
    bytes: u64,
}
fn exists(path: &Path) -> io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}
fn optional_directory(path: &Path) -> io::Result<Option<Arc<PinnedDirectory>>> {
    match PinnedDirectory::open_absolute(path) {
        Ok(pin) => {
            pin.validate()?;
            // Source access belongs to the original account, never root fallback.
            if pin.file.metadata()?.uid() != rustix::process::geteuid().as_raw() {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            Ok(Some(pin))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
fn optional_override(path: &Path) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
    let Some(parent) = optional_directory(
        path.parent()
            .ok_or_else(|| changed("Missing override parent"))?,
    )?
    else {
        return Ok(None);
    };
    let name = path
        .file_name()
        .ok_or_else(|| changed("Missing override name"))?;
    let expected = match metadata_nofollow(&parent.file, Path::new(name)) {
        Ok(value) => value,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if !expected.is_file() || expected.is_symlink() || expected.len() > OVERRIDE_LIMIT {
        return Err(changed("Invalid selected application override"));
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .follow(FollowSymlinks::No)
        .custom_flags(libc::O_NONBLOCK);
    let file = cap_primitives::fs::open(&parent.file, Path::new(name), &options)?;
    let before = file.metadata()?;
    if !before.is_file()
        || before.uid() != rustix::process::geteuid().as_raw()
        || before.mode() & 0o7022 != 0
        || before.len() > OVERRIDE_LIMIT
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let mut bytes = Zeroizing::new(Vec::new());
    (&file).take(OVERRIDE_LIMIT + 1).read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    let named = metadata_nofollow(&parent.file, Path::new(name))?;
    let observed = cap_primitives::fs::Metadata::from_file(&file)?;
    if bytes.len() as u64 > OVERRIDE_LIMIT
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
        || !crate::setup::nofollow::same_directory_identity(&expected, &observed)
        || !crate::setup::nofollow::same_directory_identity(&named, &observed)
    {
        return Err(changed("Selected application override changed during read"));
    }
    parent.validate()?;
    Ok(Some(bytes))
}
impl SelectedState {
    pub(super) fn capture(home: &Path, selected: &[InstalledRef]) -> io::Result<Self> {
        let home_pin = optional_directory(home)?.ok_or_else(|| changed("Source home missing"))?;
        let ids: BTreeSet<_> = selected.iter().filter_map(InstalledRef::app_id).collect();
        if ids.len() > MAX_APPS || ids.iter().any(|id| !valid_id(id)) {
            return Err(changed("Invalid selected application IDs"));
        }
        if optional_override(&home.join(".local/share/flatpak/overrides/global"))?.is_some() {
            return Err(changed(
                "Global application overrides need a separately reviewed selected-app mapping",
            ));
        }
        let mut apps = Vec::new();
        let mut bytes = 0u64;
        for id in ids {
            let path = home.join(".var/app").join(id);
            let data = optional_directory(&path)?
                .map(|pin| {
                    let inventory = SourceInventory::capture_home(&pin)?;
                    let stored = crate::setup::portable_transfer::stored_bytes(&pin, &inventory)?;
                    bytes = bytes
                        .checked_add(stored)
                        .ok_or_else(|| changed("Selected app state size overflows"))?;
                    Ok::<_, io::Error>(DirectorySource {
                        path,
                        pin,
                        inventory,
                    })
                })
                .transpose()?;
            let override_path = home.join(".local/share/flatpak/overrides").join(id);
            let original = optional_override(&override_path)?;
            bytes = bytes
                .checked_add(original.as_ref().map_or(0, |value| value.len() as u64))
                .ok_or_else(|| changed("Selected app overrides size overflows"))?;
            let declaration = AppState {
                app_id: id.into(),
                data_present: data.is_some(),
                override_present: original.is_some(),
            };
            apps.push(SelectedApp {
                declaration,
                data,
                override_path,
                override_sha256: original
                    .as_ref()
                    .map(|value| Sha256::digest(value.as_slice()).into()),
            });
        }
        home_pin.validate()?;
        let entry_bound = apps.iter().try_fold(2u64, |sum, app| {
            sum.checked_add(4)
                .and_then(|n| {
                    n.checked_add(
                        app.data
                            .as_ref()
                            .map_or(0, |data| data.inventory.entry_count() as u64),
                    )
                })
                .ok_or_else(|| changed("Selected app state entry bound overflows"))
        })?;
        bytes = bytes
            .checked_add(
                entry_bound
                    .checked_mul(8192)
                    .ok_or_else(|| changed("App data bound overflows"))?,
            )
            .ok_or_else(|| changed("App data bound overflows"))?;
        Ok(Self {
            home: home.into(),
            home_pin,
            apps,
            bytes,
        })
    }
    #[cfg(any(
        feature = "capture-worker",
        all(test, feature = "native-capture-qualification")
    ))]
    pub(super) fn entry_bound(&self) -> io::Result<usize> {
        self.apps.iter().try_fold(2usize, |total, app| {
            total
                .checked_add(4)
                .and_then(|n| {
                    n.checked_add(
                        app.data
                            .as_ref()
                            .map_or(0, |data| data.inventory.entry_count()),
                    )
                })
                .ok_or_else(|| changed("Selected app data entry bound overflows"))
        })
    }
    pub(super) fn bytes(&self) -> u64 {
        self.bytes
    }

    pub(super) fn declarations(&self) -> Vec<AppState> {
        self.apps
            .iter()
            .map(|app| app.declaration.clone())
            .collect()
    }
    pub(super) fn validate(&self) -> io::Result<()> {
        self.home_pin.validate()?;
        if optional_override(&self.home.join(".local/share/flatpak/overrides/global"))?.is_some() {
            return Err(changed(
                "Global application overrides changed after source review",
            ));
        }
        for app in &self.apps {
            let path = self.home.join(".var/app").join(&app.declaration.app_id);
            match (&app.data, optional_directory(&path)?) {
                (Some(source), Some(current)) => {
                    source.pin.validate()?;
                    if !crate::setup::nofollow::same_directory_identity(
                        &source.pin.identity,
                        &current.identity,
                    ) {
                        return Err(changed("Selected app data root changed"));
                    }
                    source.inventory.validate(&source.pin)?;
                }
                (None, None) => {}
                _ => return Err(changed("Selected app data presence changed")),
            }
            let value = optional_override(&app.override_path)?;
            if value
                .as_ref()
                .map(|bytes| <[u8; 32]>::from(Sha256::digest(bytes.as_slice())))
                != app.override_sha256
            {
                return Err(changed("Selected app override changed"));
            }
        }
        self.home_pin.validate()
    }
    pub(super) fn stage(
        &self,
        stage: &Path,
        journals: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.validate()?;
        let root = PinnedDirectory::open_absolute(stage)?;
        let journal_root = PinnedDirectory::open_absolute(journals)?;
        for pin in [&root, &journal_root] {
            pin.validate_destination()?;
            destination_security::check_private_file(&pin.file)?;
        }
        root.create_child(OsStr::new("app-state"))?;
        let states = PinnedDirectory::open_absolute(&stage.join("app-state"))?;
        for app in &self.apps {
            check_cancel(cancel)?;
            states.create_child(OsStr::new(&app.declaration.app_id))?;
            let destination = stage.join("app-state").join(&app.declaration.app_id);
            if let Some(data) = &app.data {
                let review = crate::setup::home_transfer::review(
                    &data.path,
                    &destination.join("data"),
                    &journals.join(format!("app-{}.journal", app.declaration.app_id)),
                )
                .map_err(io::Error::other)?;
                let report = review
                    .transfer(false, cancel, |_| {})
                    .map_err(io::Error::other)?;
                if !report.complete() {
                    return Err(changed("Selected app state copy is incomplete"));
                }
            }
            if let Some(bytes) = optional_override(&app.override_path)? {
                let pin = PinnedDirectory::open_absolute(&destination)?;
                let store = StorageDirectory::from_file(durable::readable_directory(&pin.file)?)?;
                store
                    .publish(
                        OsStr::new("override"),
                        bytes.as_slice(),
                        PublishMode::IfAbsent,
                        Privacy::OwnerOnly,
                    )
                    .map_err(|e| e.into_io_error())?;
            }
        }
        self.validate()
    }
}

fn child(parent: &Arc<PinnedDirectory>, name: &str) -> io::Result<Arc<PinnedDirectory>> {
    match metadata_nofollow(&parent.file, Path::new(name)) {
        Ok(expected) => {
            let opened = parent.open_child(OsStr::new(name), &expected)?;
            opened.validate_destination()?;
            Ok(opened)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => parent.create_child(OsStr::new(name)),
        Err(e) => Err(e),
    }
}
fn closed_names(pin: &Arc<PinnedDirectory>) -> io::Result<BTreeSet<String>> {
    let directory = cap_std::fs::Dir::from_std_file(durable::readable_directory(&pin.file)?);
    let mut names = BTreeSet::new();
    for entry in directory.entries()? {
        let name = entry?
            .file_name()
            .into_string()
            .map_err(|_| changed("Invalid app state entry name"))?;
        if names.len() >= MAX_APPS || !names.insert(name) {
            return Err(changed("App state entries exceed supported bounds"));
        }
    }
    pin.validate()?;
    Ok(names)
}
/// Authenticate the closed selected state layout before any target publication.
pub(super) fn verify_tree(root: &Path, states: &[AppState]) -> io::Result<()> {
    let parent = PinnedDirectory::open_absolute(&root.join("app-state"))?;
    destination_security::check_private_file(&parent.file)?;
    let expected: BTreeSet<_> = states.iter().map(|state| state.app_id.clone()).collect();
    if closed_names(&parent)? != expected {
        return Err(changed(
            "Archived app state differs from declared selected IDs",
        ));
    }
    for state in states {
        let path = root.join("app-state").join(&state.app_id);
        let pin = PinnedDirectory::open_absolute(&path)?;
        destination_security::check_private_file(&pin.file)?;
        let mut expected = BTreeSet::new();
        if state.data_present {
            expected.insert("data".to_owned());
        }
        if state.override_present {
            expected.insert("override".to_owned());
        }
        if closed_names(&pin)? != expected {
            return Err(changed(
                "Archived selected application state layout differs",
            ));
        }
        if state.data_present {
            PinnedDirectory::open_absolute(&path.join("data"))?.validate()?;
        }
        if state.override_present && optional_override(&path.join("override"))?.is_none() {
            return Err(changed("Archived application override missing"));
        }
    }
    parent.validate()
}

impl super::ApplicationImport {
    /// Verify existing selected-state copy journals and exact overrides without
    /// adopting missing data or trusting an importer completion flag.
    pub(super) fn verify_adopted_selected_state(
        &mut self,
        home: &Path,
        workspace: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        if workspace
            != home
                .join(".local/state/tilo/portable-applications")
                .join(&self.expectation.operation)
            || self.root != workspace.join("imported/home")
        {
            return Err(changed(
                "Selected app state verification belongs to another operation",
            ));
        }
        self.review.verify_existing_tree(&self.root, cancel)?;
        AppState::validate_selected(&self.manifest.app_state, &self.manifest.selected)?;
        verify_tree(&self.root, &self.manifest.app_state)?;
        for state in &self.manifest.app_state {
            check_cancel(cancel)?;
            if state.data_present {
                let source = self.root.join("app-state").join(&state.app_id).join("data");
                let destination = home.join(".var/app").join(&state.app_id);
                let journal = workspace
                    .join("selected-data-journals")
                    .join(format!("app-{}.journal", state.app_id));
                let review = crate::setup::home_transfer::HomeReview::recover(
                    &source,
                    &destination,
                    &journal,
                )
                .map_err(io::Error::other)?;
                let report = review.verify_completed_tree().map_err(io::Error::other)?;
                if !report.complete() {
                    return Err(changed(
                        "Selected application data is not verified complete",
                    ));
                }
            }
            if state.override_present {
                let archived = optional_override(
                    &self
                        .root
                        .join("app-state")
                        .join(&state.app_id)
                        .join("override"),
                )?
                .ok_or_else(|| changed("Archived application override is missing"))?;
                if optional_override(
                    &home
                        .join(".local/share/flatpak/overrides")
                        .join(&state.app_id),
                )? != Some(archived)
                {
                    return Err(changed("Adopted application override changed"));
                }
            }
        }
        self.review.verify_existing_tree(&self.root, cancel)?;
        PinnedDirectory::open_absolute(home)?.validate_destination()?;
        PinnedDirectory::open_absolute(workspace)?.validate_destination()
    }
    pub(super) fn adopt_selected_state(
        &mut self,
        home: &Path,
        workspace: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        if workspace
            != home
                .join(".local/state/tilo/portable-applications")
                .join(&self.expectation.operation)
            || self.root != workspace.join("imported/home")
        {
            return Err(changed(
                "Selected app state needs its authenticated protected operation",
            ));
        }
        self.review.verify_existing_tree(&self.root, cancel)?;
        AppState::validate_selected(&self.manifest.app_state, &self.manifest.selected)?;
        verify_tree(&self.root, &self.manifest.app_state)?;
        let target = PinnedDirectory::open_absolute(home)?;
        let operation = PinnedDirectory::open_absolute(workspace)?;
        for pin in [&target, &operation] {
            pin.validate_destination()?;
            destination_security::check_private_file(&pin.file)?;
        }
        let store = StorageDirectory::from_file(durable::readable_directory(&operation.file)?)?;
        store.validate_privacy(Privacy::OwnerOnly)?;
        match store.publish(
            OsStr::new("selected-data.lock"),
            b"",
            PublishMode::IfAbsent,
            Privacy::OwnerOnly,
        ) {
            Ok(_) => {}
            Err(error) => {
                let error = error.into_io_error();
                if error.kind() != io::ErrorKind::AlreadyExists {
                    return Err(error);
                }
            }
        }
        let _lease = advisory_lock::FileLock::try_exclusive(
            store.open_append_existing(OsStr::new("selected-data.lock"), Privacy::OwnerOnly)?,
        )
        .map_err(|_| changed("Selected application data adoption is busy"))?;
        operation.validate_destination()?;
        // Refuse collisions across the entire selection before copying any app.
        for state in &self.manifest.app_state {
            let destination = home.join(".var/app").join(&state.app_id);
            let journal = workspace
                .join("selected-data-journals")
                .join(format!("app-{}.journal", state.app_id));
            if state.data_present && exists(&destination)? && !exists(&journal)? {
                return Err(changed("Existing application data was not replaced"));
            }
            let override_path = home
                .join(".local/share/flatpak/overrides")
                .join(&state.app_id);
            if state.override_present {
                let original = optional_override(
                    &self
                        .root
                        .join("app-state")
                        .join(&state.app_id)
                        .join("override"),
                )?
                .ok_or_else(|| changed("Archived override missing"))?;
                if let Some(existing) = optional_override(&override_path)? {
                    if existing != original {
                        return Err(changed("Existing application override was not replaced"));
                    }
                }
            }
        }
        let journals = child(&operation, "selected-data-journals")?;
        destination_security::check_private_file(&journals.file)?;
        let data_parent = if self
            .manifest
            .app_state
            .iter()
            .any(|state| state.data_present)
        {
            Some(child(&child(&target, ".var")?, "app")?)
        } else {
            None
        };
        let overrides = if self
            .manifest
            .app_state
            .iter()
            .any(|state| state.override_present)
        {
            Some(child(
                &child(&child(&child(&target, ".local")?, "share")?, "flatpak")?,
                "overrides",
            )?)
        } else {
            None
        };
        for state in &self.manifest.app_state {
            check_cancel(cancel)?;
            self.review.verify_existing_tree(&self.root, cancel)?;
            if state.data_present {
                let parent = data_parent
                    .as_ref()
                    .ok_or_else(|| changed("App data parent missing"))?;
                parent.validate_destination()?;
                let source = self.root.join("app-state").join(&state.app_id).join("data");
                let destination = home.join(".var/app").join(&state.app_id);
                let journal = workspace
                    .join("selected-data-journals")
                    .join(format!("app-{}.journal", state.app_id));
                let (review, retry) = match std::fs::symlink_metadata(&journal) {
                    Ok(_) => (
                        crate::setup::home_transfer::HomeReview::recover(
                            &source,
                            &destination,
                            &journal,
                        )
                        .map_err(io::Error::other)?,
                        true,
                    ),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => (
                        crate::setup::home_transfer::review(&source, &destination, &journal)
                            .map_err(io::Error::other)?,
                        false,
                    ),
                    Err(e) => return Err(e),
                };
                let report = review
                    .transfer(retry, cancel, |_| {})
                    .map_err(io::Error::other)?;
                if !report.complete() {
                    return Err(changed("Selected application data adoption is incomplete"));
                }
                parent.validate_destination()?;
            }
            if state.override_present {
                let parent = overrides
                    .as_ref()
                    .ok_or_else(|| changed("Overrides parent missing"))?;
                parent.validate_destination()?;
                let bytes = optional_override(
                    &self
                        .root
                        .join("app-state")
                        .join(&state.app_id)
                        .join("override"),
                )?
                .ok_or_else(|| changed("Archived override missing"))?;
                let path = home
                    .join(".local/share/flatpak/overrides")
                    .join(&state.app_id);
                match optional_override(&path)? {
                    Some(existing) if existing == bytes => {}
                    Some(_) => {
                        return Err(changed("Existing application override was not replaced"));
                    }
                    None => {
                        let store = StorageDirectory::from_file(durable::readable_directory(
                            &parent.file,
                        )?)?;
                        store
                            .publish(
                                OsStr::new(&state.app_id),
                                bytes.as_slice(),
                                PublishMode::IfAbsent,
                                Privacy::OwnerOnly,
                            )
                            .map_err(|e| e.into_io_error())?;
                    }
                }
                parent.validate_destination()?;
            }
        }
        self.review.verify_existing_tree(&self.root, cancel)?;
        target.validate_destination()
    }
}
