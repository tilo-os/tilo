//! Persistent imported website sessions, isolated from the ordinary browser profile.
//!
//! Linux namespaces are traversed through retained, no-follow directory handles.
//! A cooperative exclusive lease spans the engine lifetime. Incomplete attempts
//! are never reused; cleanup is explicit and requires the caller to have waited
//! for its dedicated engine to terminate. This does not defeat a hostile process
//! running as the same user.
use crate::sessions::SessionCookies;
use rustix::fs::{self, AtFlags, Dir, Mode, OFlags};
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use tilo_files::native::atomic::lease::{FileLease, LeaseMode};
use tilo_files::native::atomic::{
    Privacy, PublicationError, PublicationState, PublishMode, StorageDirectory,
};

const RECEIPT: &str = "completed.json";
const LEASE: &str = "engine.lease";
const FORGETTING: &str = "forgetting";
const MAX_RECEIPT: u64 = 4096;

#[derive(Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: u32,
    input: [u8; 32],
    attempt: String,
}

/// A host-authorized storage root, retained independently of its ambient name.
///
/// The path is needed only by native engines that cannot accept a directory
/// handle. Admission and each engine-path access reject aliases and replacement.
pub struct GrantedRoot {
    directory: File,
    path: PathBuf,
}

impl GrantedRoot {
    /// Admit the host-selected native storage path, creating missing private children.
    ///
    /// The caller chooses and authorizes the path; this never discovers a data root.
    /// Existing permissions are never changed, and aliases are never followed.
    ///
    /// # Errors
    /// Rejects relative paths, aliases, uncontrolled parents of missing children,
    /// non-private new children, and a final namespace the caller does not control.
    pub fn admit_native(path: &Path) -> io::Result<GrantedRoot> {
        if !path.is_absolute() {
            return Err(unavailable());
        }
        let mut held = File::from(fs::open(
            "/",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => match open_child(&held, name) {
                    Ok(child) => held = child,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        StorageDirectory::from_file(held.try_clone()?)?
                            .validate_namespace_control()?;
                        match fs::mkdirat(&held, name, Mode::from_raw_mode(0o700)) {
                            Ok(()) => held.sync_all()?,
                            Err(rustix::io::Errno::EXIST) => {}
                            Err(error) => return Err(error.into()),
                        }
                        let child = open_child(&held, name)?;
                        StorageDirectory::from_file(child.try_clone()?)?
                            .validate_privacy(Privacy::OwnerOnly)?;
                        child.sync_all()?;
                        held = child;
                    }
                    Err(error) => return Err(error),
                },
                _ => return Err(unavailable()),
            }
        }
        Self::from_file(held, path.to_owned())
    }

    /// Bind an already-authorized directory handle to its engine-visible path.
    /// This does not discover, create, or grant access to an ambient data root.
    ///
    /// # Errors
    /// Rejects non-directories, uncontrolled namespaces, aliases, or a path that
    /// no longer names the supplied handle.
    pub fn from_file(directory: File, path: PathBuf) -> io::Result<Self> {
        StorageDirectory::from_file(directory.try_clone()?)?.validate_namespace_control()?;
        same_object(&open_root(&path)?, &directory)?;
        Ok(Self { directory, path })
    }
}

/// The lease must outlive every engine and `WebView` that accesses this profile.
pub struct Profile {
    path: PathBuf,
    parent: File,
    directory: File,
    input: [u8; 32],
    attempt: String,
    needs_import: bool,
    cancelled: bool,
    receipt_uncertain: bool,
    _lease: FileLease,
}

impl Profile {
    /// Admit the identity-bound selection within the host's granted root.
    ///
    /// # Errors
    /// Rejects invalid selections, unsafe namespaces, conflicting leases, or
    /// receipts that do not attest this exact selection.
    pub fn prepare(root: &GrantedRoot, session: &SessionCookies) -> io::Result<Self> {
        session.validate()?;
        let serialized = zeroize::Zeroizing::new(serde_json::to_vec(session)?);
        let input = *blake3::hash(&serialized).as_bytes();
        drop(serialized);
        let root_file = root.directory.try_clone()?;
        StorageDirectory::from_file(root_file.try_clone()?)?.validate_namespace_control()?;
        let namespace = private_child(&root_file, OsStr::new("service-sessions"))?;
        let identity = identity_name(&session.identity_public_key)?;
        let identity_dir = private_child(&namespace, OsStr::new(&identity))?;
        let parent = private_child(&identity_dir, OsStr::new(&session.session_id))?;
        let storage = StorageDirectory::from_file(parent.try_clone()?)?;
        match storage.publish(
            OsStr::new(LEASE),
            b"tilo-browser-service-engine-v1\n",
            PublishMode::IfAbsent,
            Privacy::OwnerOnly,
        ) {
            Ok(()) => {}
            Err(error) if error.io_error().kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into_io_error()),
        }
        storage.open_read(OsStr::new(LEASE), Privacy::OwnerOnly)?;
        let lease = storage
            .try_acquire_lease(OsStr::new(LEASE), LeaseMode::Exclusive)
            .map_err(|_| {
                io::Error::new(io::ErrorKind::WouldBlock, "Website session is already open")
            })?;
        if marker_exists(&parent)? {
            return Err(unavailable());
        }
        let (attempt, needs_import) = match read_receipt(&storage)? {
            Some(receipt) => {
                if receipt.schema != 1 || receipt.input != input || !valid_attempt(&receipt.attempt)
                {
                    return Err(unavailable());
                }
                parent.sync_all()?;
                (receipt.attempt, false)
            }
            None => (format!("attempt-{}", uuid::Uuid::new_v4()), true),
        };
        let directory = if needs_import {
            fs::mkdirat(&parent, attempt.as_str(), Mode::from_raw_mode(0o700))?;
            parent.sync_all()?;
            let opened = open_child(&parent, OsStr::new(&attempt))?;
            validate_private(&opened)?;
            opened.sync_all()?;
            opened
        } else {
            let opened = open_child(&parent, OsStr::new(&attempt))?;
            validate_private(&opened)?;
            opened
        };
        let path = root
            .path
            .join("service-sessions")
            .join(identity)
            .join(&session.session_id)
            .join(&attempt);
        let result = Self {
            path,
            parent,
            directory,
            input,
            attempt,
            needs_import,
            cancelled: false,
            receipt_uncertain: false,
            _lease: lease,
        };
        result.verify_path()?;
        Ok(result)
    }

    /// Return a freshly verified path for the native engine's existing API.
    ///
    /// # Errors
    /// Rejects cancelled, fenced, aliased, or replaced profiles.
    pub fn path(&self) -> io::Result<&Path> {
        self.verify_path()?;
        Ok(&self.path)
    }

    /// Whether the native engine must install the selected cookies.
    #[must_use]
    pub fn needs_import(&self) -> bool {
        self.needs_import
    }

    /// Call only after actual CEF `SetCookie` callbacks and `FlushStore` succeed.
    /// A receipt records installation, never provider authentication.
    ///
    /// # Errors
    /// Rejects replaced or fenced profiles and failed receipt publication.
    pub fn complete(&mut self) -> io::Result<()> {
        self.verify_path()?;
        if !self.needs_import {
            return Ok(());
        }
        let receipt = Receipt {
            schema: 1,
            input: self.input,
            attempt: self.attempt.clone(),
        };
        let publication = StorageDirectory::from_file(self.parent.try_clone()?)?.publish(
            OsStr::new(RECEIPT),
            &serde_json::to_vec(&receipt)?,
            PublishMode::IfAbsent,
            Privacy::OwnerOnly,
        );
        if let Err(error) = publication {
            // A possibly published receipt must retain its referenced profile.
            // A later open commits the existing file and parent before reuse.
            self.receipt_uncertain = error.state() != PublicationState::NotPublished;
            return Err(error.into_io_error());
        }
        self.needs_import = false;
        Ok(())
    }

    /// Caller must first shut down and wait for its dedicated native engine.
    /// Drop alone never deletes an attempt: the child might still be alive.
    ///
    /// # Errors
    /// Rejects completed or uncertain imports, replaced entries, and unsafe or
    /// failed descriptor-relative deletion.
    pub fn cancel_after_engine_exit(&mut self) -> io::Result<()> {
        if self.cancelled {
            return Ok(());
        }
        if !self.needs_import || self.receipt_uncertain {
            return Err(unavailable());
        }
        verify_child(&self.parent, OsStr::new(&self.attempt), &self.directory)?;
        let mut budget = 100_000usize;
        remove_contents(
            &self.directory,
            self.directory.metadata()?.dev(),
            0,
            &mut budget,
        )?;
        verify_child(&self.parent, OsStr::new(&self.attempt), &self.directory)?;
        fs::unlinkat(&self.parent, self.attempt.as_str(), AtFlags::REMOVEDIR)?;
        self.cancelled = true;
        self.parent.sync_all()
    }

    /// Fence reuse durably while still holding the engine's exclusive lease.
    /// The marker deliberately survives successful deletion: canonical ownership
    /// needs a separate backend operation before this selection can reconnect.
    ///
    /// # Errors
    /// Rejects invalid attestations, replaced profiles, or failed publication.
    pub fn begin_forget(&mut self) -> io::Result<()> {
        if marker_exists(&self.parent)? {
            let marker = read_attestation(
                &StorageDirectory::from_file(self.parent.try_clone()?)?,
                FORGETTING,
            )?
            .ok_or_else(unavailable)?;
            if marker.schema != 1 || marker.input != self.input || marker.attempt != self.attempt {
                return Err(unavailable());
            }
            return self.parent.sync_all();
        }
        self.verify_path()?;
        StorageDirectory::from_file(self.parent.try_clone()?)?
            .publish(
                OsStr::new(FORGETTING),
                &serde_json::to_vec(&Receipt {
                    schema: 1,
                    input: self.input,
                    attempt: self.attempt.clone(),
                })?,
                PublishMode::IfAbsent,
                Privacy::OwnerOnly,
            )
            .map_err(PublicationError::into_io_error)
    }

    /// Caller must confirm the dedicated child was reaped, not merely closed.
    ///
    /// # Errors
    /// Rejects unsafe attestations, replaced entries, or failed deletion.
    pub fn cleanup_after_engine_exit(&mut self) -> io::Result<()> {
        if !marker_exists(&self.parent)? {
            return if self.needs_import {
                self.cancel_after_engine_exit()
            } else {
                Ok(())
            };
        }
        // Recommit and attest even a marker whose publication had an uncertain
        // durability result before removing any credential-bearing entries.
        let marker = read_attestation(
            &StorageDirectory::from_file(self.parent.try_clone()?)?,
            FORGETTING,
        )?
        .ok_or_else(unavailable)?;
        if marker.schema != 1 || marker.input != self.input || marker.attempt != self.attempt {
            return Err(unavailable());
        }
        self.parent.sync_all()?;
        if !self.cancelled {
            verify_child(&self.parent, OsStr::new(&self.attempt), &self.directory)?;
            let mut budget = 100_000usize;
            remove_contents(
                &self.directory,
                self.directory.metadata()?.dev(),
                0,
                &mut budget,
            )?;
            verify_child(&self.parent, OsStr::new(&self.attempt), &self.directory)?;
            fs::unlinkat(&self.parent, self.attempt.as_str(), AtFlags::REMOVEDIR)?;
            self.cancelled = true;
            self.parent.sync_all()?;
        }
        remove_attempts(&self.parent)?;
        // Unlink only the descriptor-relative entry; a symlink is not followed.
        match fs::unlinkat(&self.parent, RECEIPT, AtFlags::empty()) {
            Ok(()) | Err(rustix::io::Errno::NOENT) => {}
            Err(error) => return Err(error.into()),
        }
        self.parent.sync_all()
    }

    /// Fence an already-closed (or never opened) selection under its exclusive
    /// lease. The durable marker also makes retries safe after broker failure.
    ///
    /// # Errors
    /// Rejects unsafe IDs or namespaces, live owners, invalid attestations, or
    /// failed durable fencing and cleanup.
    pub fn forget_closed(root: &GrantedRoot, person: [u8; 32], id: &str) -> io::Result<()> {
        if id.is_empty()
            || !matches!(
                Path::new(id).components().collect::<Vec<_>>().as_slice(),
                [Component::Normal(_)]
            )
        {
            return Err(unavailable());
        }
        {
            let root = root.directory.try_clone()?;
            StorageDirectory::from_file(root.try_clone()?)?.validate_namespace_control()?;
            let namespace = private_child(&root, OsStr::new("service-sessions"))?;
            let identity = identity_name(&person)?;
            let identity = private_child(&namespace, OsStr::new(&identity))?;
            let parent = private_child(&identity, OsStr::new(id))?;
            let storage = StorageDirectory::from_file(parent.try_clone()?)?;
            match storage.publish(
                OsStr::new(LEASE),
                b"tilo-browser-service-engine-v1\n",
                PublishMode::IfAbsent,
                Privacy::OwnerOnly,
            ) {
                Ok(()) => {}
                Err(error) if error.io_error().kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into_io_error()),
            }
            storage.open_read(OsStr::new(LEASE), Privacy::OwnerOnly)?;
            let _lease = storage
                .try_acquire_lease(OsStr::new(LEASE), LeaseMode::Exclusive)
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::WouldBlock, "Website session is already open")
                })?;
            if !marker_exists(&parent)? {
                let receipt = read_receipt(&storage)?.unwrap_or_else(|| Receipt {
                    schema: 1,
                    input: [0; 32],
                    attempt: format!("attempt-{}", uuid::Uuid::new_v4()),
                });
                if receipt.schema != 1 || !valid_attempt(&receipt.attempt) {
                    return Err(unavailable());
                }
                storage
                    .publish(
                        OsStr::new(FORGETTING),
                        &serde_json::to_vec(&receipt)?,
                        PublishMode::IfAbsent,
                        Privacy::OwnerOnly,
                    )
                    .map_err(PublicationError::into_io_error)?;
            }
            parent.sync_all()?;
        }
        Self::retry_forget(root, person, id)
    }

    /// Recover only a previously fenced local deletion. No ownership resolution or
    /// native engine admission is involved; the exclusive lease fences live owners.
    ///
    /// # Errors
    /// Rejects unsafe IDs or namespaces, live owners, missing or mismatched
    /// attestations, or failed descriptor-relative deletion.
    pub fn retry_forget(root: &GrantedRoot, person: [u8; 32], id: &str) -> io::Result<()> {
        if id.is_empty()
            || !matches!(
                Path::new(id).components().collect::<Vec<_>>().as_slice(),
                [Component::Normal(_)]
            )
        {
            return Err(unavailable());
        }
        let root = root.directory.try_clone()?;
        StorageDirectory::from_file(root.try_clone()?)?.validate_namespace_control()?;
        let namespace = open_child(&root, OsStr::new("service-sessions"))?;
        validate_private(&namespace)?;
        let identity = identity_name(&person)?;
        let identity = open_child(&namespace, OsStr::new(&identity))?;
        validate_private(&identity)?;
        let parent = open_child(&identity, OsStr::new(id))?;
        validate_private(&parent)?;
        let storage = StorageDirectory::from_file(parent.try_clone()?)?;
        storage.open_read(OsStr::new(LEASE), Privacy::OwnerOnly)?;
        let _lease = storage
            .try_acquire_lease(OsStr::new(LEASE), LeaseMode::Exclusive)
            .map_err(|_| {
                io::Error::new(io::ErrorKind::WouldBlock, "Website session is already open")
            })?;
        let marker = read_attestation(&storage, FORGETTING)?.ok_or_else(unavailable)?;
        if marker.schema != 1 || !valid_attempt(&marker.attempt) {
            return Err(unavailable());
        }
        if read_receipt(&storage)?.is_some_and(|receipt| receipt != marker) {
            return Err(unavailable());
        }
        // Commit the attestation before deleting anything. A malformed or unsafe
        // marker fences admission but never authorizes destructive recovery.
        parent.sync_all()?;
        match open_child(&parent, OsStr::new(&marker.attempt)) {
            Ok(directory) => {
                validate_private(&directory)?;
                let mut budget = 100_000;
                remove_contents(&directory, directory.metadata()?.dev(), 0, &mut budget)?;
                verify_child(&parent, OsStr::new(&marker.attempt), &directory)?;
                fs::unlinkat(&parent, marker.attempt.as_str(), AtFlags::REMOVEDIR)?;
                parent.sync_all()?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        remove_attempts(&parent)?;
        match fs::unlinkat(&parent, RECEIPT, AtFlags::empty()) {
            Ok(()) | Err(rustix::io::Errno::NOENT) => {}
            Err(error) => return Err(error.into()),
        }
        parent.sync_all()
    }

    fn verify_path(&self) -> io::Result<()> {
        if self.cancelled || marker_exists(&self.parent)? {
            return Err(unavailable());
        }
        validate_private(&self.parent)?;
        validate_private(&self.directory)?;
        let current = open_root(&self.path)?;
        same_object(&current, &self.directory)
    }
}

/// Remove orphaned imports as well as the receipted attempt, keeping only the
/// durable fence and lease namespace. Every traversal is descriptor-relative.
fn remove_attempts(parent: &File) -> io::Result<()> {
    let mut entries = Dir::read_from(parent)?;
    let mut budget = 100_000;
    while let Some(entry) = entries.read() {
        let entry = entry?;
        let name = OsStr::from_bytes(entry.file_name().to_bytes());
        if !name.to_str().is_some_and(valid_attempt) {
            continue;
        }
        let stat = fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)?;
        if fs::FileType::from_raw_mode(stat.st_mode) == fs::FileType::Directory {
            let child = open_child(parent, name)?;
            validate_private(&child)?;
            remove_contents(&child, parent.metadata()?.dev(), 0, &mut budget)?;
            verify_child(parent, name, &child)?;
            fs::unlinkat(parent, name, AtFlags::REMOVEDIR)?;
        } else {
            fs::unlinkat(parent, name, AtFlags::empty())?;
        }
    }
    parent.sync_all()
}

fn marker_exists(parent: &File) -> io::Result<bool> {
    match fs::statat(parent, FORGETTING, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => Ok(true), // Any entry fences reuse, including a malformed marker.
        Err(rustix::io::Errno::NOENT) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Website session profile is unavailable",
    )
}

fn identity_name(person: &[u8; 32]) -> io::Result<String> {
    let mut name = String::with_capacity(64);
    for byte in person {
        write!(name, "{byte:02x}").map_err(io::Error::other)?;
    }
    Ok(name)
}

fn valid_attempt(name: &str) -> bool {
    name.strip_prefix("attempt-")
        .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
}

fn read_receipt(parent: &StorageDirectory) -> io::Result<Option<Receipt>> {
    read_attestation(parent, RECEIPT)
}

fn read_attestation(parent: &StorageDirectory, name: &str) -> io::Result<Option<Receipt>> {
    let file = match parent.open_read(OsStr::new(name), Privacy::OwnerOnly) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if file.metadata()?.len() > MAX_RECEIPT {
        return Err(unavailable());
    }
    let mut bytes = Vec::new();
    (&file).take(MAX_RECEIPT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RECEIPT {
        return Err(unavailable());
    }
    let receipt = serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
    file.sync_all()?;
    Ok(Some(receipt))
}

fn open_child(parent: &File, name: &OsStr) -> io::Result<File> {
    Ok(File::from(fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )?))
}

fn validate_private(directory: &File) -> io::Result<()> {
    StorageDirectory::from_file(directory.try_clone()?)?.validate_privacy(Privacy::OwnerOnly)
}

fn private_child(parent: &File, name: &OsStr) -> io::Result<File> {
    match fs::mkdirat(parent, name, Mode::from_raw_mode(0o700)) {
        Ok(()) => parent.sync_all()?,
        Err(rustix::io::Errno::EXIST) => {}
        Err(error) => return Err(error.into()),
    }
    let child = open_child(parent, name)?;
    validate_private(&child)?;
    child.sync_all()?;
    Ok(child)
}

/// Verify the engine-visible name without following aliases in any component.
/// Ambient root selection belongs to the granting host.
fn open_root(path: &Path) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(unavailable());
    }
    let mut held = File::from(fs::open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => held = open_child(&held, name)?,
            _ => return Err(unavailable()),
        }
    }
    Ok(held)
}

fn same_object(first: &File, second: &File) -> io::Result<()> {
    let first = first.metadata()?;
    let second = second.metadata()?;
    if first.dev() == second.dev() && first.ino() == second.ino() {
        Ok(())
    } else {
        Err(unavailable())
    }
}

fn verify_child(parent: &File, name: &OsStr, held: &File) -> io::Result<()> {
    same_object(&open_child(parent, name)?, held)
}

fn remove_contents(
    directory: &File,
    device: u64,
    depth: usize,
    budget: &mut usize,
) -> io::Result<()> {
    if depth > 64 || directory.metadata()?.dev() != device {
        return Err(unavailable());
    }
    let mut entries = Dir::read_from(directory)?;
    while let Some(entry) = entries.read() {
        let entry = entry?;
        let name = OsStr::from_bytes(entry.file_name().to_bytes());
        if name == "." || name == ".." {
            continue;
        }
        *budget = budget.checked_sub(1).ok_or_else(unavailable)?;
        let name = OsString::from(name);
        let stat = fs::statat(directory, &name, AtFlags::SYMLINK_NOFOLLOW)?;
        if fs::FileType::from_raw_mode(stat.st_mode) == fs::FileType::Directory {
            let child = open_child(directory, &name)?;
            if child.metadata()?.uid() != rustix::process::geteuid().as_raw() {
                return Err(unavailable());
            }
            remove_contents(
                &child,
                device,
                depth.checked_add(1).ok_or_else(unavailable)?,
                budget,
            )?;
            verify_child(directory, &name, &child)?;
            fs::unlinkat(directory, &name, AtFlags::REMOVEDIR)?;
        } else {
            // Symlinks are removed as entries, never traversed.
            fs::unlinkat(directory, &name, AtFlags::empty())?;
        }
    }
    directory.sync_all()
}
