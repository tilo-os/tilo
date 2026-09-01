//! Descriptor-backed portal documents for the RAIL drive redirector.
//!
//! The public capability is an opaque [`PortalDocumentGrant`], never a host
//! path.  This broker owns the descriptor selected by the qOS portal and makes
//! a short-lived copy below the RAIL lease root.  FreeRDP receives only that
//! directory.  Revocation removes it, and a read/write projection is copied
//! back through the already-open descriptor before it is removed.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek as _};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::io::AsRawFd as _;
use std::os::unix::io::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rand::RngCore as _;

use crate::{
    BrokeredLaunch, CapabilityBroker, LaunchError, LaunchRequest, PortalDocumentAccess,
    PortalDocumentGrant, ShareBinding,
};

const DEFAULT_LEASE_ROOT: &str = "/run/qos/windows-vm/leases";
const MAX_PORTAL_DOCUMENTS_PER_LAUNCH: usize = 32;
const MAX_PORTAL_DOCUMENT_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// A broker-owned descriptor selected by qOS's file portal.
///
/// There is intentionally no constructor from a path.  The portal/opening
/// component has already opened a regular file and supplies that descriptor.
/// The returned [`PortalDocumentGrant`] is the only value a launch request may
/// retain.
#[derive(Debug)]
struct StoredDocument {
    workspace_id: String,
    application_id: String,
    access: PortalDocumentAccess,
    source: Mutex<File>,
}

#[derive(Clone, Debug)]
struct LeaseDocument {
    document: Arc<StoredDocument>,
    projection: PathBuf,
    read_only_mount: Option<PathBuf>,
}

/// qOS's host-side mapping from opaque portal IDs to opened document handles.
///
/// A store may be shared by the portal bridge and the RAIL capability broker;
/// neither Windows, FreeRDP, nor an untrusted manager client can resolve a
/// handle to its source path.
#[derive(Debug)]
pub struct PortalDocumentStore {
    root: PathBuf,
    next: AtomicU64,
    documents: Mutex<HashMap<String, Arc<StoredDocument>>>,
    leases: Mutex<HashMap<String, Vec<LeaseDocument>>>,
}

impl Default for PortalDocumentStore {
    fn default() -> Self {
        Self::new(DEFAULT_LEASE_ROOT)
    }
}

impl PortalDocumentStore {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            next: AtomicU64::new(0),
            documents: Mutex::new(HashMap::new()),
            leases: Mutex::new(HashMap::new()),
        }
    }

    /// Registers the file descriptor returned by qOS's file portal.
    ///
    /// The caller identity is retained with the descriptor, so a handle picked
    /// in one workspace/app cannot be projected into another.  `ReadWrite`
    /// requires a descriptor actually opened for writing; checking its open
    /// flags rather than path permissions avoids a time-of-check/path-reopen
    /// hole.
    pub fn register(
        &self,
        workspace_id: &str,
        application_id: &str,
        source: File,
        access: PortalDocumentAccess,
    ) -> Result<PortalDocumentGrant, LaunchError> {
        if !valid_owner_component(workspace_id) || !valid_owner_component(application_id) {
            return Err(LaunchError::InvalidRequest("portal document owner"));
        }
        let metadata = source
            .metadata()
            .map_err(|error| portal_error(format!("could not inspect portal document: {error}")))?;
        if !metadata.is_file() {
            return Err(portal_error("portal document is not a regular file"));
        }
        if metadata.len() > MAX_PORTAL_DOCUMENT_BYTES {
            return Err(portal_error("portal document exceeds the 16 GiB limit"));
        }
        if access == PortalDocumentAccess::ReadWrite {
            let flags = unsafe { libc::fcntl(source.as_raw_fd(), libc::F_GETFL) };
            if flags < 0 || flags & libc::O_ACCMODE == libc::O_RDONLY {
                return Err(portal_error(
                    "read/write portal projection requires a writable document descriptor",
                ));
            }
        }
        let mut random = [0_u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut random);
        let sequence = self.next.fetch_add(1, Ordering::Relaxed);
        let handle = format!("doc-{}-{}", hex::encode(random), sequence);
        let document = Arc::new(StoredDocument {
            workspace_id: workspace_id.to_string(),
            application_id: application_id.to_string(),
            access,
            source: Mutex::new(source),
        });
        self.documents
            .lock()
            .map_err(|_| portal_error("portal document store is unavailable"))?
            .insert(handle.clone(), document);
        Ok(PortalDocumentGrant { handle, access })
    }

    /// Registers the descriptor received over the compatibility service's
    /// `SCM_RIGHTS` portal handoff.  This makes the descriptor boundary
    /// explicit at the service/compositor seam and avoids ever reopening a
    /// client-supplied pathname.
    pub fn register_owned_fd(
        &self,
        workspace_id: &str,
        application_id: &str,
        source: OwnedFd,
        access: PortalDocumentAccess,
    ) -> Result<PortalDocumentGrant, LaunchError> {
        self.register(workspace_id, application_id, File::from(source), access)
    }

    fn materialize(
        &self,
        lease_id: &str,
        request: &LaunchRequest,
    ) -> Result<Vec<ShareBinding>, LaunchError> {
        let lease_dir = self.root.join(lease_id);
        if request.grants.portal_documents.len() > MAX_PORTAL_DOCUMENTS_PER_LAUNCH {
            return Err(portal_error("too many portal documents in one RAIL launch"));
        }
        create_private_dir(&self.root)?;
        fs::create_dir(&lease_dir)
            .map_err(|error| portal_error(format!("could not create portal lease: {error}")))?;
        set_mode(&lease_dir, 0o700)?;

        let result = (|| {
            let documents = self
                .documents
                .lock()
                .map_err(|_| portal_error("portal document store is unavailable"))?;
            let mut lease_documents = Vec::with_capacity(request.grants.portal_documents.len());
            let mut shares = Vec::with_capacity(request.grants.portal_documents.len());
            for (index, grant) in request.grants.portal_documents.iter().enumerate() {
                let document = documents
                    .get(&grant.handle)
                    .ok_or_else(|| portal_error("unknown or revoked portal document"))?
                    .clone();
                if document.workspace_id != request.workspace_id
                    || document.application_id != request.application_id
                    || document.access != grant.access
                {
                    return Err(portal_error(
                        "portal document does not belong to this launch",
                    ));
                }
                let drive_name = format!("QOS{:02}", index + 1);
                let directory = lease_dir.join(&drive_name);
                fs::create_dir(&directory).map_err(|error| {
                    portal_error(format!("could not create portal drive projection: {error}"))
                })?;
                set_mode(&directory, 0o700)?;
                // Modes are not a boundary against FreeRDP running under the
                // same uid.  Read-only projections therefore use a kernel
                // read-only bind mount at the path passed to RDPDR.
                let (backing, read_only_mount) = if grant.access.is_read_only() {
                    let backing = lease_dir.join(format!(".portal-{index}"));
                    fs::create_dir(&backing).map_err(|error| {
                        portal_error(format!("could not create portal backing store: {error}"))
                    })?;
                    set_mode(&backing, 0o700)?;
                    let projection = backing.join("document");
                    copy_from_portal_descriptor(&document, &projection, grant.access)?;
                    // Nothing has received `directory` yet, so populate the
                    // private backing before making the RDP-visible mount.
                    mount_read_only(&backing, &directory)?;
                    (backing, Some(directory.clone()))
                } else {
                    (directory.clone(), None)
                };
                let projection = backing.join("document");
                if !grant.access.is_read_only() {
                    copy_from_portal_descriptor(&document, &projection, grant.access)?;
                }
                lease_documents.push(LeaseDocument {
                    document,
                    projection: projection.clone(),
                    read_only_mount,
                });
                shares.push(ShareBinding {
                    drive_name,
                    portal_document_id: grant.handle.clone(),
                    host_path: directory,
                    read_only: grant.access.is_read_only(),
                });
            }
            self.leases
                .lock()
                .map_err(|_| portal_error("portal lease store is unavailable"))?
                .insert(lease_id.to_string(), lease_documents);
            Ok(shares)
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&lease_dir);
        }
        result
    }

    /// Revokes a lease and reports a write-back or unmount failure after
    /// removing the RDP-visible projection.  Callers with a user-facing
    /// operation should use this rather than losing a failed writable save.
    pub fn revoke_checked(&self, lease_id: &str) -> Result<(), LaunchError> {
        let lease = self
            .leases
            .lock()
            .ok()
            .and_then(|mut leases| leases.remove(lease_id));
        let mut first_error = None;
        if let Some(documents) = lease {
            for lease_document in documents {
                if lease_document.document.access == PortalDocumentAccess::ReadWrite {
                    // The descriptor, not a remembered source path, receives
                    // edits. A failed write-back still revokes visibility.
                    if let Err(error) = copy_back_to_portal_descriptor(&lease_document) {
                        first_error.get_or_insert(error);
                    }
                }
                if let Some(mount) = &lease_document.read_only_mount {
                    if let Err(error) = unmount(mount) {
                        first_error.get_or_insert(error);
                    }
                }
            }
        }
        if let Err(error) = fs::remove_dir_all(self.root.join(lease_id)) {
            if error.kind() != io::ErrorKind::NotFound {
                first_error.get_or_insert_with(|| {
                    portal_error(format!("could not remove portal lease: {error}"))
                });
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(())
    }
}

/// RAIL capability broker that turns only registered portal descriptors into
/// explicit FreeRDP drives. It is suitable for the native RAIL presentation;
/// SPICE remains deny-all for files.
#[derive(Debug)]
pub struct PortalRailCapabilityBroker {
    documents: Arc<PortalDocumentStore>,
    next: AtomicU64,
}

impl PortalRailCapabilityBroker {
    #[must_use]
    pub fn new(documents: Arc<PortalDocumentStore>) -> Self {
        Self {
            documents,
            next: AtomicU64::new(0),
        }
    }
}

impl CapabilityBroker for PortalRailCapabilityBroker {
    fn materialize(&self, request: &LaunchRequest) -> Result<BrokeredLaunch, LaunchError> {
        if request.grants.network {
            return Err(portal_error(
                "the Windows appliance received network without a filtered guest broker",
            ));
        }
        let sequence = self.next.fetch_add(1, Ordering::Relaxed);
        let lease_id = format!("rail-{}-{sequence}", std::process::id());
        let shares = self.documents.materialize(&lease_id, request)?;
        Ok(BrokeredLaunch {
            lease_id,
            network: false,
            network_binding: None,
            clipboard: request.grants.clipboard,
            microphone: request.grants.microphone,
            shares,
        })
    }

    fn revoke(&self, lease_id: &str) {
        // The older CapabilityBroker trait cannot return revocation errors.
        // Keep the fallible API above for portal-facing callers and make an
        // integrity failure observable here rather than silently discarding.
        if let Err(error) = self.documents.revoke_checked(lease_id) {
            eprintln!("qOS Windows portal lease {lease_id} revoke failed: {error}");
        }
    }
}

fn copy_from_portal_descriptor(
    document: &StoredDocument,
    destination: &Path,
    access: PortalDocumentAccess,
) -> Result<(), LaunchError> {
    let mut source = document
        .source
        .lock()
        .map_err(|_| portal_error("portal document descriptor is unavailable"))?
        .try_clone()
        .map_err(|error| portal_error(format!("could not duplicate portal descriptor: {error}")))?;
    source
        .rewind()
        .map_err(|error| portal_error(format!("could not rewind portal descriptor: {error}")))?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|error| portal_error(format!("could not create portal projection: {error}")))?;
    io::copy(&mut source, &mut output)
        .and_then(|_| output.sync_all())
        .map_err(|error| portal_error(format!("could not copy portal document: {error}")))?;
    set_mode(
        destination,
        if access.is_read_only() { 0o400 } else { 0o600 },
    )
}

fn copy_back_to_portal_descriptor(lease: &LeaseDocument) -> Result<(), LaunchError> {
    let mut input = File::open(&lease.projection)
        .map_err(|error| portal_error(format!("could not read portal projection: {error}")))?;
    let mut source = lease
        .document
        .source
        .lock()
        .map_err(|_| portal_error("portal document descriptor is unavailable"))?;
    source
        .set_len(0)
        .and_then(|()| source.rewind())
        .and_then(|()| io::copy(&mut input, &mut *source).map(|_| ()))
        .and_then(|()| source.sync_all())
        .map_err(|error| portal_error(format!("could not write portal document: {error}")))
}

fn create_private_dir(path: &Path) -> Result<(), LaunchError> {
    fs::create_dir_all(path)
        .map_err(|error| portal_error(format!("could not create portal lease root: {error}")))?;
    set_mode(path, 0o700)
}

fn mount_read_only(source: &Path, target: &Path) -> Result<(), LaunchError> {
    let source = path_to_cstring(source)?;
    let target = path_to_cstring(target)?;
    // SAFETY: both strings are NUL terminated and live for these syscalls;
    // source/target are private directories created immediately above.
    let bind = unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND,
            std::ptr::null(),
        )
    };
    if bind != 0 {
        return Err(portal_error(format!(
            "kernel read-only portal projection is unavailable: {}",
            io::Error::last_os_error()
        )));
    }
    // SAFETY: this remount changes only the validated bind mount, not the
    // hidden backing directory retained by the broker.
    let remount = unsafe {
        libc::mount(
            std::ptr::null(),
            target.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY,
            std::ptr::null(),
        )
    };
    if remount == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    let _ = unmount_path(target.as_ptr());
    Err(portal_error(format!(
        "could not make portal projection read-only: {error}"
    )))
}

fn unmount(path: &Path) -> Result<(), LaunchError> {
    let path = path_to_cstring(path)?;
    unmount_path(path.as_ptr())
}

fn unmount_path(path: *const libc::c_char) -> Result<(), LaunchError> {
    // SAFETY: every caller supplies a live NUL-terminated mountpoint path
    // created by this broker.
    let result = unsafe { libc::umount2(path, libc::MNT_DETACH) };
    if result == 0 {
        Ok(())
    } else {
        Err(portal_error(format!(
            "could not unmount portal projection: {}",
            io::Error::last_os_error()
        )))
    }
}

fn path_to_cstring(path: &Path) -> Result<std::ffi::CString, LaunchError> {
    use std::os::unix::ffi::OsStrExt as _;
    std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| portal_error("portal projection path contained a NUL"))
}

fn set_mode(path: &Path, mode: u32) -> Result<(), LaunchError> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|error| portal_error(format!("could not protect portal projection: {error}")))
}

fn valid_owner_component(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn portal_error(message: impl Into<String>) -> LaunchError {
    LaunchError::CapabilityBroker(message.into())
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};

    use tempfile::TempDir;

    use super::*;

    fn source(directory: &TempDir, name: &str, contents: &[u8]) -> File {
        let path = directory.path().join(name);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap();
        file.write_all(contents).unwrap();
        file.rewind().unwrap();
        file
    }

    fn request(grant: PortalDocumentGrant) -> LaunchRequest {
        LaunchRequest {
            workspace_id: "workspace".to_string(),
            application_id: "published.app".to_string(),
            grants: crate::VmLaunchGrants {
                portal_documents: vec![grant],
                ..crate::VmLaunchGrants::default()
            },
        }
    }

    #[test]
    fn read_only_portal_file_is_projected_under_only_its_lease_and_revoked() {
        let temp = TempDir::new().unwrap();
        let store = Arc::new(PortalDocumentStore::new(temp.path().join("leases")));
        let grant = store
            .register(
                "workspace",
                "published.app",
                source(&temp, "input", b"private"),
                PortalDocumentAccess::ReadOnly,
            )
            .unwrap();
        let broker = PortalRailCapabilityBroker::new(store);
        let lease = match broker.materialize(&request(grant)) {
            Ok(lease) => lease,
            Err(LaunchError::CapabilityBroker(message))
                if message.contains("kernel read-only portal projection") =>
            {
                // An unprivileged development process cannot make a bind
                // mount. That is a fail-closed result, not a weak chmod
                // fallback. CI with mount capability runs the assertions.
                return;
            }
            Err(error) => panic!("unexpected portal projection failure: {error}"),
        };
        assert_eq!(lease.shares.len(), 1);
        assert!(lease.shares[0].read_only);
        assert!(
            lease.shares[0]
                .host_path
                .starts_with(temp.path().join("leases").join(&lease.lease_id))
        );
        assert_eq!(
            fs::read(lease.shares[0].host_path.join("document")).unwrap(),
            b"private"
        );
        broker.revoke(&lease.lease_id);
        assert!(!temp.path().join("leases").join(&lease.lease_id).exists());
    }

    #[test]
    fn read_only_mount_refuses_same_uid_chmod_and_write() {
        let temp = TempDir::new().unwrap();
        let store = Arc::new(PortalDocumentStore::new(temp.path().join("leases")));
        let grant = store
            .register(
                "workspace",
                "published.app",
                source(&temp, "input", b"private"),
                PortalDocumentAccess::ReadOnly,
            )
            .unwrap();
        let broker = PortalRailCapabilityBroker::new(store);
        let lease = match broker.materialize(&request(grant)) {
            Ok(lease) => lease,
            Err(LaunchError::CapabilityBroker(message))
                if message.contains("kernel read-only portal projection") =>
            {
                return;
            }
            Err(error) => panic!("unexpected portal projection failure: {error}"),
        };
        let document = lease.shares[0].host_path.join("document");
        assert!(OpenOptions::new().write(true).open(&document).is_err());
        use std::os::unix::ffi::OsStrExt as _;
        let name = std::ffi::CString::new(document.as_os_str().as_bytes()).unwrap();
        // SAFETY: `name` is a valid live C string for this filesystem call.
        assert_ne!(unsafe { libc::chmod(name.as_ptr(), 0o600) }, 0);
        broker.revoke(&lease.lease_id);
    }

    #[test]
    fn writable_projection_round_trips_only_through_the_portal_descriptor() {
        let temp = TempDir::new().unwrap();
        let source_path = temp.path().join("input");
        let store = Arc::new(PortalDocumentStore::new(temp.path().join("leases")));
        let grant = store
            .register(
                "workspace",
                "published.app",
                source(&temp, "input", b"before"),
                PortalDocumentAccess::ReadWrite,
            )
            .unwrap();
        let broker = PortalRailCapabilityBroker::new(store);
        let lease = broker.materialize(&request(grant)).unwrap();
        fs::write(lease.shares[0].host_path.join("document"), b"after").unwrap();
        broker.revoke(&lease.lease_id);
        let mut contents = String::new();
        File::open(source_path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert_eq!(contents, "after");
    }

    #[test]
    fn a_handle_is_scoped_to_the_portal_caller_that_selected_it() {
        let temp = TempDir::new().unwrap();
        let store = Arc::new(PortalDocumentStore::new(temp.path().join("leases")));
        let grant = store
            .register(
                "workspace-a",
                "published.app",
                source(&temp, "input", b"private"),
                PortalDocumentAccess::ReadOnly,
            )
            .unwrap();
        let broker = PortalRailCapabilityBroker::new(store);
        assert!(matches!(
            broker.materialize(&request(grant)),
            Err(LaunchError::CapabilityBroker(_))
        ));
    }

    #[test]
    fn document_count_is_bounded_before_any_projection_is_created() {
        let temp = TempDir::new().unwrap();
        let store = Arc::new(PortalDocumentStore::new(temp.path().join("leases")));
        let broker = PortalRailCapabilityBroker::new(store);
        let request = LaunchRequest {
            workspace_id: "workspace".to_string(),
            application_id: "published.app".to_string(),
            grants: crate::VmLaunchGrants {
                portal_documents: (0..=MAX_PORTAL_DOCUMENTS_PER_LAUNCH)
                    .map(|number| PortalDocumentGrant {
                        handle: format!("doc-{number}"),
                        access: PortalDocumentAccess::ReadOnly,
                    })
                    .collect(),
                ..crate::VmLaunchGrants::default()
            },
        };
        assert!(matches!(
            broker.materialize(&request),
            Err(LaunchError::CapabilityBroker(message)) if message.contains("too many")
        ));
        assert!(!temp.path().join("leases").exists());
    }
}
