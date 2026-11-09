//! Root-owned selected-object bridge. Relative names route a bounded lookup;
//! only actual peer, namespace, source epoch and matching held objects admit it.
//! No serialized value or public descriptor grant constructor exists.
use super::{
    installed_retained_actor::InstalledRetainedLaunchInput,
    retained_consumer_lease::RetainedConsumerLease,
};
use runtime_portal::process_binding::ExecutingPeer;
use std::{
    ffi::{CString, OsString},
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    sync::{Arc, Mutex, atomic::AtomicBool},
};
fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}

/// Transported object observations only. They grant no access until the root
/// lease qualifies the actual executing Files task and its projected view.
pub(super) struct SelectedObjects {
    pub(super) selected: File,
    pub(super) parent: File,
    pub(super) components: Vec<OsString>,
}

pub(super) struct RetainedOpenSelection {
    input: Arc<InstalledRetainedLaunchInput>,
    files: Arc<ExecutingPeer>,
    view: File,
    objects: SelectedObjects,
    selected_identity: (u64, u64),
    parent_identity: (u64, u64),
}
/// Checked process-local handoff only. Fields expose held observations to the
/// sibling root factory, never a serialized grant or a public constructor.
pub(super) struct SelectedOriginCustody {
    selection: RetainedOpenSelection,
    pub(super) lease: Arc<Mutex<RetainedConsumerLease>>,
    pub(super) input: Arc<InstalledRetainedLaunchInput>,
    pub(super) files: Arc<ExecutingPeer>,
    pub(super) endpoint: Arc<launcher::owned_service::TargetProjectionEndpoint>,
    pub(super) native_origin: Arc<super::retained_source_relay::SourceSelectedOrigin>,
    pub(super) components: Vec<Vec<u8>>,
    pub(super) selected_identity: (u64, u64),
    pub(super) parent_identity: (u64, u64),
}
impl SelectedOriginCustody {
    pub(super) fn check(&self, cancel: &AtomicBool) -> io::Result<()> {
        super::migration::check_cancel(cancel)?;
        // This caller belongs to a pending editor. Cancellation of that new
        // request cannot be latched as revocation of the original Files actor.
        let origin_cancel = AtomicBool::new(false);
        let lease = self.lease.lock().map_err(|_| refused())?;
        self.selection.check(&lease, &origin_cancel)?;
        if !self.native_origin.uses_input(&self.input)
            || self.selected_identity != self.selection.selected_identity
            || self.parent_identity != self.selection.parent_identity
            || !self
                .native_origin
                .components()
                .iter()
                .map(|name| name.as_os_str().as_encoded_bytes())
                .eq(self.components.iter().map(Vec::as_slice))
        {
            return Err(refused());
        }
        self.native_origin.check_named()?;
        lease.check_selected_origin(&origin_cancel, &self.input, &self.files, &self.endpoint)?;
        super::migration::check_cancel(cancel)
    }
}
impl RetainedOpenSelection {
    pub(super) fn admit(
        lease: &RetainedConsumerLease,
        cancel: &AtomicBool,
        input: Arc<InstalledRetainedLaunchInput>,
        files: Arc<ExecutingPeer>,
        view: File,
        objects: SelectedObjects,
    ) -> io::Result<Self> {
        if !lease.uses_open_input(&input) {
            return Err(refused());
        }
        lease.check_open_origin(cancel, &files)?;
        lease.check_open_view(&view)?;
        let (selected_identity, parent_identity) = review_objects(&view, &objects)?;
        let admitted = Self {
            input,
            files,
            view,
            objects,
            selected_identity,
            parent_identity,
        };
        admitted.check(lease, cancel)?;
        Ok(admitted)
    }
    pub(super) fn check(
        &self,
        lease: &RetainedConsumerLease,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        if !lease.uses_open_input(&self.input) {
            return Err(refused());
        }
        lease.check_open_origin(cancel, &self.files)?;
        self.input.revalidate(cancel)?;
        lease.check_open_view(&self.view)?;
        let observed = review_objects(&self.view, &self.objects)?;
        if observed != (self.selected_identity, self.parent_identity) {
            return Err(refused());
        }
        lease.check_open_origin(cancel, &self.files)
    }
    /// Installed source operation comes only from the SAME retained input.
    pub(super) fn operation_id(&self) -> &str {
        &self.input.binding().operation_id
    }
}

type ObjectIdentity = (u64, u64);
fn identity(file: &File) -> io::Result<ObjectIdentity> {
    let facts = file.metadata()?;
    Ok((facts.dev(), facts.ino()))
}
fn review_objects(
    view: &File,
    objects: &SelectedObjects,
) -> io::Result<(ObjectIdentity, ObjectIdentity)> {
    for file in [&objects.selected, &objects.parent] {
        // RetainedObject selects an O_PATH observation. Never carry a caller's
        // read/write open description forward as a supposedly read-only grant.
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        if flags & libc::O_PATH == 0 {
            return Err(refused());
        }
    }
    if !view.metadata()?.is_dir()
        || !objects.selected.metadata()?.is_file()
        || !objects.parent.metadata()?.is_dir()
    {
        return Err(refused());
    }
    let (name, ancestors) = objects.components.split_last().ok_or_else(refused)?;
    if objects.components.len() > 64 {
        return Err(refused());
    }
    let mut parent = view.try_clone()?;
    for component in ancestors {
        parent = beneath(&parent, component, true)?;
    }
    let selected = beneath(&parent, name, false)?;
    let observed = (identity(&selected)?, identity(&parent)?);
    if observed != (identity(&objects.selected)?, identity(&objects.parent)?) {
        return Err(refused());
    }
    Ok(observed)
}

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}
fn beneath(parent: &File, name: &std::ffi::OsStr, directory: bool) -> io::Result<File> {
    let bytes = name.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 255
        || bytes == b"."
        || bytes == b".."
        || bytes.contains(&b'/')
        || bytes.contains(&0)
    {
        return Err(refused());
    }
    let name = CString::new(bytes).map_err(io::Error::other)?;
    let how = OpenHow {
        flags: u64::from(
            (libc::O_PATH
                | libc::O_CLOEXEC
                | libc::O_NOFOLLOW
                | if directory { libc::O_DIRECTORY } else { 0 })
            .cast_unsigned(),
        ),
        mode: 0,
        // RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS.
        resolve: 0x08 | 0x04 | 0x02,
    };
    // SAFETY: held directory, one validated component and initialized Linux
    // open_how. Success transfers a fresh O_PATH descriptor; no payload I/O.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            parent.as_raw_fd(),
            name.as_ptr(),
            &raw const how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = i32::try_from(fd).map_err(io::Error::other)?;
    // SAFETY: successful openat2 returned a fresh owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn observation(path: &std::path::Path) -> File {
        use std::os::unix::fs::OpenOptionsExt;
        File::options()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .unwrap()
    }
    #[test]
    fn actual_object_parent_and_beneath_view_must_all_match() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("Documents")).unwrap();
        std::fs::write(root.path().join("Documents/report"), b"selected").unwrap();
        std::fs::write(other.path().join("report"), b"same name foreign file").unwrap();
        let view = File::open(root.path()).unwrap();
        let parent = observation(&root.path().join("Documents"));
        let file = observation(&root.path().join("Documents/report"));
        let mut objects = SelectedObjects {
            selected: file,
            parent,
            components: vec!["Documents".into(), "report".into()],
        };
        review_objects(&view, &objects).unwrap();
        let retained = objects.selected.try_clone().unwrap();
        objects.selected = File::options()
            .read(true)
            .write(true)
            .open(root.path().join("Documents/report"))
            .unwrap();
        assert!(
            review_objects(&view, &objects).is_err(),
            "writable description is never a read-only observation"
        );
        objects.selected = retained;

        objects.parent = observation(other.path());
        assert!(review_objects(&view, &objects).is_err());
        objects.parent = observation(&root.path().join("Documents"));
        objects.selected = observation(&other.path().join("report"));
        assert!(review_objects(&view, &objects).is_err());
        objects.selected = observation(&root.path().join("Documents/report"));
        std::fs::rename(
            root.path().join("Documents/report"),
            root.path().join("Documents/original"),
        )
        .unwrap();
        std::fs::write(root.path().join("Documents/report"), b"replacement").unwrap();
        assert!(review_objects(&view, &objects).is_err());
        objects.components = vec!["..".into(), "report".into()];
        assert!(review_objects(&view, &objects).is_err());
        objects.components = vec!["Documents".into(), "link".into()];
        std::os::unix::fs::symlink(
            other.path().join("report"),
            root.path().join("Documents/link"),
        )
        .unwrap();
        assert!(review_objects(&view, &objects).is_err());
    }
}
