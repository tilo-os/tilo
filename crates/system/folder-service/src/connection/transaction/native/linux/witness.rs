//! One retained reader and explicit semantic cuts; never called for each payload chunk.
use super::super::witness::{self as rules, Expected, Inventory, Passes, Role, Transcript};
use super::{
    AsRawFd, CStr, CString, EndpointError, File, FileExt, FromRawFd, Instant, NativeDirectory,
    NativeEpoch, ObjectId, ParentGuard, PrivateNamespace, Result, StableFileGuard, Stamp, Weak, d,
    descriptor_object, descriptor_stamp, destination_security, endpoint, io, object, open_at,
    rename_noreplace,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
enum WatchDirectory {
    Pinned(NativeDirectory),
    Retained(std::sync::Arc<File>),
}
impl WatchDirectory {
    fn file(&self) -> &File {
        match self {
            Self::Pinned(directory) => directory.file(),
            Self::Retained(file) => file,
        }
    }
    fn validate(&self) -> std::result::Result<(), EndpointError> {
        match self {
            Self::Pinned(directory) => directory.validate(true),
            Self::Retained(file) => {
                endpoint::native_directory_metadata(file)?;
                destination_security::check_private_file(file)?;
                Ok(())
            }
        }
    }
}
struct Watch {
    directory: WatchDirectory,
    identity: ObjectId,
    wd: i32,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum DescriptorOrigin {
    Created,
    FreshObserved,
}
#[derive(Clone)]
pub(super) struct DescriptorEvidence {
    pub identity: ObjectId,
    pub stamp: Stamp,
    pub bytes: Vec<u8>,
    origin: DescriptorOrigin,
}
pub(super) struct NamespaceWitness {
    directory_phase: Option<rules::DirectoryPhase>,
    pub(super) restarted: bool,
    reader: File,
    watches: Vec<Watch>,
    bindings: Vec<(i32, Role)>,
    names: Option<(CString, CString)>,
    inventory: BTreeSet<Vec<u8>>,
    expected: Vec<Expected>,
    descriptors: BTreeMap<Vec<u8>, DescriptorEvidence>,
    passes: Passes,
    failed: bool,
    closed: bool,
    buffer: Box<[u8]>,
}
fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}
impl NamespaceWitness {
    #[cfg(test)]
    pub(super) fn passes_for_test(&self) -> u8 {
        self.passes.used()
    }

    pub(super) fn new(
        root: &NativeDirectory,
        target: &ParentGuard,
        name: CString,
        bootstrap: bool,
    ) -> io::Result<Self> {
        let mut value = Self::reader(Some((name, target.name.clone())), !bootstrap)?;
        value.watch(root, Role::Root)?;
        value.watch(&target.directory, Role::Target)?;
        if bootstrap {
            value.expected.push(Expected {
                role: Role::Root,
                name: value.namespace_name()?.to_bytes().to_vec(),
                mask: rules::CREATE | rules::ISDIR,
            });
        }
        Ok(value)
    }
    fn namespace_name(&self) -> io::Result<&CStr> {
        self.names
            .as_ref()
            .map(|names| names.0.as_c_str())
            .ok_or_else(invalid)
    }
    fn target_name(&self) -> io::Result<&CStr> {
        self.names
            .as_ref()
            .map(|names| names.1.as_c_str())
            .ok_or_else(invalid)
    }
    fn reader(names: Option<(CString, CString)>, restarted: bool) -> io::Result<Self> {
        // SAFETY: fixed flags; successful call returns one new owned fd.
        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            directory_phase: None,
            // SAFETY: fd belongs solely to this owner.
            reader: unsafe { File::from_raw_fd(fd) },
            restarted,
            watches: Vec::new(),
            bindings: Vec::new(),
            names,
            inventory: BTreeSet::new(),
            expected: Vec::new(),
            descriptors: BTreeMap::new(),
            passes: Passes::default(),
            failed: false,
            closed: false,
            buffer: vec![0; rules::EVENT_BUFFER].into_boxed_slice(),
        })
    }
    /// Operationless root guards share the authenticated watch/reader engine.
    /// There are no namespace names, sentinel writes, or publication phases.
    pub(super) fn empty_roots(roots: [&NativeDirectory; 2]) -> io::Result<Self> {
        let mut value = Self::reader(None, false)?;
        value.watch(roots[0], Role::Root)?;
        value.watch(roots[1], Role::Target)?;
        Ok(value)
    }
    pub(super) fn check_empty_roots(&mut self, epoch: &mut NativeEpoch<'_>) -> Result<()> {
        if self.names.is_some() || self.failed || self.watches.len() != 2 {
            return Err(epoch.failure(EndpointError::Unverified));
        }
        let result = (|| {
            self.passes
                .enter()
                .map_err(|_| epoch.failure(EndpointError::LimitExceeded))?;
            let deadline = Instant::now() + std::time::Duration::from_secs(1);
            for watch in &self.watches {
                epoch.ep(watch.directory.validate())?;
                if epoch.ep(object(watch.directory.file()))? != watch.identity {
                    return Err(epoch.failure(EndpointError::IdentityChanged));
                }
                enumerate(epoch, &watch.directory, Inventory::exact(&[]), deadline)?;
            }
            epoch.check()?;
            // Empty roots permit no namespace or metadata events, including a
            // create/delete race whose final directory contents look unchanged.
            // SAFETY: sole nonblocking reader and a valid bounded output buffer.
            let count = unsafe {
                libc::read(
                    self.reader.as_raw_fd(),
                    self.buffer.as_mut_ptr().cast(),
                    self.buffer.len(),
                )
            };
            if count != -1 || io::Error::last_os_error().raw_os_error() != Some(libc::EAGAIN) {
                return Err(epoch.failure(EndpointError::Unstable));
            }
            for watch in &self.watches {
                epoch.ep(watch.directory.validate())?;
                if epoch.ep(object(watch.directory.file()))? != watch.identity {
                    return Err(epoch.failure(EndpointError::IdentityChanged));
                }
            }
            epoch.check()
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    pub(super) fn new_directory(
        root: &NativeDirectory,
        target: &ParentGuard,
        name: CString,
        bootstrap: bool,
    ) -> io::Result<Self> {
        let mut value = Self::new(root, target, name, bootstrap)?;
        value.directory_phase = Some(rules::DirectoryPhase::Unpublished);
        value.passes = Passes::directory();
        Ok(value)
    }
    pub(super) fn watch_directory(
        &mut self,
        file: std::sync::Arc<File>,
        published: bool,
    ) -> io::Result<()> {
        if self.directory_phase != Some(rules::DirectoryPhase::Unpublished)
            || published && !self.restarted
        {
            return Err(invalid());
        }
        self.watch_inner(WatchDirectory::Retained(file), Role::Directory)?;
        if published {
            self.directory_phase = Some(rules::DirectoryPhase::Published);
        }
        Ok(())
    }
    pub(super) fn watch(&mut self, directory: &NativeDirectory, role: Role) -> io::Result<()> {
        if role == Role::Directory {
            return Err(invalid());
        }
        self.watch_inner(WatchDirectory::Pinned(directory.clone()), role)
    }
    fn watch_inner(&mut self, directory: WatchDirectory, role: Role) -> io::Result<()> {
        directory.validate().map_err(|_| invalid())?;
        if self.failed || self.bindings.iter().any(|(_, bound)| *bound == role) {
            return Err(invalid());
        }
        let identity = object(directory.file()).map_err(|_| invalid())?;
        if let Some(existing) = self.watches.iter().find(|watch| watch.identity == identity) {
            endpoint::native_same_mount(existing.directory.file(), directory.file())
                .map_err(|_| invalid())?;
            self.bindings.push((existing.wd, role));
            return Ok(());
        }
        if self.watches.len() >= 4 {
            return Err(invalid());
        }
        // Authenticate procfs before following only this retained-fd reference.
        let proc = File::open("/proc/self/fd")?;
        let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::uninit();
        // SAFETY: live fd and a writable statfs result.
        if unsafe { libc::fstatfs(proc.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fstatfs succeeded.
        if unsafe { filesystem.assume_init() }.f_type != libc::PROC_SUPER_MAGIC {
            return Err(invalid());
        }
        let reference = CString::new(format!("/proc/self/fd/{}", directory.file().as_raw_fd()))
            .map_err(|_| invalid())?;
        // SAFETY: authentic proc fd reference; only this intentional final link is followed.
        let fd = unsafe {
            libc::open(
                reference.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: newly owned successful fd.
        let observed = unsafe { File::from_raw_fd(fd) };
        endpoint::native_same_mount(directory.file(), &observed).map_err(|_| invalid())?;
        if object(&observed).map_err(|_| invalid())? != identity {
            return Err(invalid());
        }
        // No MASK_ADD, replacement, rearm, ONESHOT, EXCL_UNLINK, or second reader.
        // SAFETY: reader is live and reference is NUL-terminated.
        let wd = unsafe {
            libc::inotify_add_watch(
                self.reader.as_raw_fd(),
                reference.as_ptr(),
                rules::MASK | libc::IN_ONLYDIR,
            )
        };
        if wd < 0 {
            return Err(io::Error::last_os_error());
        }
        if self.watches.iter().any(|watch| watch.wd == wd)
            || object(&observed).map_err(|_| invalid())? != identity
            || object(directory.file()).map_err(|_| invalid())? != identity
        {
            return Err(invalid());
        }
        self.watches.push(Watch {
            directory,
            identity,
            wd,
        });
        self.bindings.push((wd, role));
        Ok(())
    }
    pub(super) fn role(&self, role: Role) -> io::Result<NativeDirectory> {
        let wd = self
            .bindings
            .iter()
            .find(|(_, bound)| *bound == role)
            .ok_or_else(invalid)?
            .0;
        match &self
            .watches
            .iter()
            .find(|watch| watch.wd == wd)
            .ok_or_else(invalid)?
            .directory
        {
            WatchDirectory::Pinned(directory) => Ok(directory.clone()),
            WatchDirectory::Retained(_) => Err(invalid()),
        }
    }
    pub(super) fn set_inventory(&mut self, names: &[&[u8]]) -> io::Result<()> {
        if self.failed || !self.expected.is_empty() || names.len() > 6 || !self.inventory.is_empty()
        {
            return Err(invalid());
        }
        if self.directory_phase.is_some()
            && (names.len() > 3
                || names.iter().any(|name| {
                    !matches!(*name, b"intent" | b"staged-intent" | b"stage" | b"outcome")
                })
                || names.contains(&b"stage".as_slice()) && names.contains(&b"outcome".as_slice()))
        {
            return Err(invalid());
        }
        self.inventory = names.iter().map(|name| name.to_vec()).collect();
        if self.inventory.len() != names.len() {
            return Err(invalid());
        }
        Ok(())
    }
    pub(super) fn create(&mut self, name: &CStr) -> io::Result<()> {
        if let Some(phase) = self.directory_phase {
            if !matches!(
                name.to_bytes(),
                b"intent" | b"staged-intent" | b"stage" | b"outcome"
            ) || (name == c"outcome") != (phase == rules::DirectoryPhase::Published)
            {
                return Err(invalid());
            }
        }
        rules::creation_phase(&self.inventory, name.to_bytes()).map_err(|_| invalid())?;
        if self.failed
            || !self.expected.is_empty()
            || !self.inventory.insert(name.to_bytes().to_vec())
        {
            return Err(invalid());
        }
        self.closed = false;
        self.expected.push(Expected {
            role: Role::Namespace,
            name: name.to_bytes().to_vec(),
            mask: rules::CREATE
                | if self.directory_phase.is_some() && name == c"stage" {
                    rules::ISDIR
                } else {
                    0
                },
        });
        Ok(())
    }
    pub(super) fn move_entry(&mut self, publish: bool) -> io::Result<()> {
        rules::movement_phase(&self.inventory, publish).map_err(|_| invalid())?;
        if self.directory_phase.is_some()
            && (!publish || self.directory_phase != Some(rules::DirectoryPhase::Unpublished))
        {
            return Err(invalid());
        }
        if self.failed || !self.expected.is_empty() {
            return Err(invalid());
        }
        let (from, from_name, to, to_name) = if publish {
            if !self.inventory.remove(b"stage".as_slice()) {
                return Err(invalid());
            }
            (
                Role::Namespace,
                b"stage".to_vec(),
                Role::Target,
                self.target_name()?.to_bytes().to_vec(),
            )
        } else {
            if !self.inventory.insert(b"displaced".to_vec()) {
                return Err(invalid());
            }
            (
                Role::Target,
                self.target_name()?.to_bytes().to_vec(),
                Role::Namespace,
                b"displaced".to_vec(),
            )
        };
        self.closed = false;
        self.expected = vec![
            Expected {
                role: from,
                name: from_name,
                mask: rules::MOVED_FROM
                    | if self.directory_phase.is_some() {
                        rules::ISDIR
                    } else {
                        0
                    },
            },
            Expected {
                role: to,
                name: to_name,
                mask: rules::MOVED_TO
                    | if self.directory_phase.is_some() {
                        rules::ISDIR
                    } else {
                        0
                    },
            },
        ];
        if self.directory_phase.is_some() {
            self.directory_phase = Some(rules::DirectoryPhase::Publishing);
            self.expected.push(Expected {
                role: Role::Directory,
                name: Vec::new(),
                mask: rules::MOVE_SELF,
            });
        }
        Ok(())
    }
    pub(super) fn descriptor(&self, name: &CStr) -> Option<&DescriptorEvidence> {
        self.descriptors.get(name.to_bytes())
    }
    fn remember(&mut self, name: &CStr, evidence: DescriptorEvidence) -> io::Result<()> {
        if self.failed
            || self.descriptors.len() >= 4
            || self.descriptors.contains_key(name.to_bytes())
        {
            return Err(invalid());
        }
        self.descriptors.insert(name.to_bytes().to_vec(), evidence);
        Ok(())
    }
    pub(super) fn remember_created(
        &mut self,
        name: &CStr,
        mut evidence: DescriptorEvidence,
    ) -> io::Result<()> {
        evidence.origin = DescriptorOrigin::Created;
        self.remember(name, evidence)
    }
    pub(super) fn remember_observed(
        &mut self,
        name: &CStr,
        evidence: DescriptorEvidence,
    ) -> io::Result<()> {
        if !self.restarted || evidence.origin != DescriptorOrigin::FreshObserved {
            return Err(invalid());
        }
        self.remember(name, evidence)
    }
    pub(super) fn created_preparation(&self) -> bool {
        !self.restarted
            && !self.failed
            && self.closed
            && [c"intent", c"staged-intent"].into_iter().all(|name| {
                self.descriptor(name)
                    .is_some_and(|evidence| evidence.origin == DescriptorOrigin::Created)
            })
    }
    fn sample(&self, epoch: &mut NativeEpoch<'_>, full: bool) -> Result<()> {
        for watch in &self.watches {
            epoch.ep(watch.directory.validate())?;
            if epoch.ep(object(watch.directory.file()))? != watch.identity {
                return Err(epoch.failure(EndpointError::IdentityChanged));
            }
        }
        let namespace = epoch.io(self.role(Role::Namespace))?;
        epoch.io(destination_security::check_private_file(namespace.file()))?;
        if !full {
            return epoch.check();
        }
        for (name, evidence) in &self.descriptors {
            let name = CString::new(name.as_slice())
                .map_err(|_| epoch.failure(EndpointError::Unverified))?;
            let current = epoch.descriptor_snapshot(&namespace, &name, &evidence.bytes)?;
            if current.identity != evidence.identity || current.stamp != evidence.stamp {
                return Err(epoch.failure(EndpointError::IdentityChanged));
            }
        }
        epoch.validate_existing_ancestors(false)?;
        epoch.check()
    }
    pub(super) fn checkpoint(&mut self, epoch: &mut NativeEpoch<'_>) -> Result<()> {
        if self.failed {
            return Err(epoch.failure(EndpointError::Unstable));
        }
        let result = self.checkpoint_inner(epoch);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    #[expect(
        clippy::too_many_lines,
        reason = "The bounded sample/enumerate/drain/sample cut is ordered and indivisible"
    )]
    fn checkpoint_inner(&mut self, epoch: &mut NativeEpoch<'_>) -> Result<()> {
        self.passes
            .enter()
            .map_err(|_| epoch.failure(EndpointError::LimitExceeded))?;
        #[cfg(test)]
        test_cut("checkpoint", &epoch.io(self.role(Role::Namespace))?, c".");
        let deadline = Instant::now() + std::time::Duration::from_secs(1);
        self.sample(epoch, true)?;
        let namespace = epoch.io(self.role(Role::Namespace))?;
        for watch in &self.watches {
            let is_namespace = self
                .bindings
                .iter()
                .any(|(wd, role)| *wd == watch.wd && *role == Role::Namespace);
            let names: Vec<_> = self.inventory.iter().map(Vec::as_slice).collect();
            let is_unpublished = self.directory_phase == Some(rules::DirectoryPhase::Unpublished)
                && self
                    .bindings
                    .iter()
                    .any(|(wd, role)| *wd == watch.wd && *role == Role::Directory);
            // Both exact inventories must hold if two semantic roles alias.
            if is_namespace && is_unpublished && !names.is_empty() {
                return Err(epoch.failure(EndpointError::Unstable));
            }
            let inventory = if is_namespace {
                Inventory::exact(&names)
            } else if is_unpublished {
                Inventory::exact(&[])
            } else {
                Inventory::barrier()
            };
            enumerate(epoch, &watch.directory, inventory, deadline)?;
        }
        let root = epoch.io(self.role(Role::Root))?;
        let namespace_name = epoch.io(self.namespace_name())?;
        epoch.io(sentinel(root.file(), namespace_name))?;
        for name in [c"intent", c"stage", c"displaced"] {
            if self.inventory.contains(name.to_bytes()) {
                epoch.io(sentinel(namespace.file(), name))?;
            }
        }
        if self.inventory.contains(b"staged-intent".as_slice())
            && !self.inventory.contains(b"stage".as_slice())
        {
            let target = epoch.io(self.role(Role::Target))?;
            let target_name = epoch.io(self.target_name())?;
            epoch.io(sentinel(target.file(), target_name))?;
        }
        let mut transcript = if let Some(phase) = self.directory_phase {
            Transcript::directory(
                &self.bindings,
                epoch.io(self.namespace_name())?.to_bytes(),
                epoch.io(self.target_name())?.to_bytes(),
                &self.expected,
                phase,
            )
        } else {
            Transcript::new(
                &self.bindings,
                epoch.io(self.namespace_name())?.to_bytes(),
                epoch.io(self.target_name())?.to_bytes(),
                &self.expected,
            )
        }
        .map_err(|_| epoch.failure(EndpointError::Unstable))?;
        loop {
            epoch.check()?;
            if Instant::now() >= deadline {
                return Err(epoch.failure(EndpointError::LimitExceeded));
            }
            // SAFETY: sole nonblocking reader and a valid 64 KiB writable buffer.
            let count = unsafe {
                libc::read(
                    self.reader.as_raw_fd(),
                    self.buffer.as_mut_ptr().cast(),
                    self.buffer.len(),
                )
            };
            if count < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::EAGAIN) {
                    break;
                }
                return epoch.io(Err(error));
            }
            let count =
                usize::try_from(count).map_err(|_| epoch.failure(EndpointError::Unstable))?;
            let bytes = self
                .buffer
                .get(..count)
                .ok_or_else(|| epoch.failure(EndpointError::Unstable))?;
            transcript
                .feed(bytes)
                .map_err(|_| epoch.failure(EndpointError::Unstable))?;
        }
        transcript
            .finish()
            .map_err(|_| epoch.failure(EndpointError::Unstable))?;
        self.sample(epoch, false)?;
        if Instant::now() >= deadline {
            return Err(epoch.failure(EndpointError::LimitExceeded));
        }
        if self.directory_phase == Some(rules::DirectoryPhase::Publishing) {
            self.directory_phase = Some(rules::DirectoryPhase::Published);
        }
        self.expected.clear();
        self.closed = true;
        Ok(())
    }
}
fn sentinel(parent: &File, name: &CStr) -> io::Result<()> {
    if name.to_bytes().is_empty()
        || matches!(name.to_bytes(), b"." | b"..")
        || name.to_bytes().contains(&b'/')
    {
        return Err(invalid());
    }
    match rename_noreplace(parent, name, parent, name) {
        Err(error) if error.raw_os_error() == Some(libc::EEXIST) => Ok(()),
        Err(error) => Err(error),
        Ok(()) => Err(invalid()),
    }
}
fn enumerate(
    epoch: &mut NativeEpoch<'_>,
    directory: &WatchDirectory,
    mut inventory: Inventory,
    deadline: Instant,
) -> Result<()> {
    let identity = epoch.ep(object(directory.file()))?;
    // A new open description starts at cursor zero; never dup, seek, or iterator EOF.
    let file = epoch.io(open_at(
        directory.file(),
        c".",
        libc::O_RDONLY | libc::O_DIRECTORY,
    ))?;
    epoch.ep(endpoint::native_same_mount(directory.file(), &file))?;
    if epoch.ep(object(&file))? != identity {
        return Err(epoch.failure(EndpointError::IdentityChanged));
    }
    let mut buffer = [0_u8; 4096];
    for _ in 0..4 {
        epoch.check()?;
        if Instant::now() >= deadline {
            return Err(epoch.failure(EndpointError::LimitExceeded));
        }
        // SAFETY: fresh directory fd and a real positive-size writable buffer.
        let count = unsafe {
            libc::syscall(
                libc::SYS_getdents64,
                file.as_raw_fd(),
                buffer.as_mut_ptr(),
                buffer.len(),
            )
        };
        let result = if count < 0 {
            Err(rules::Lost)
        } else {
            usize::try_from(count)
                .ok()
                .and_then(|count| buffer.get(..count))
                .ok_or(rules::Lost)
        };
        let complete = inventory
            .response(result)
            .map_err(|_| epoch.failure(EndpointError::Unstable))?;
        if complete {
            if epoch.ep(object(&file))? != identity
                || epoch.ep(object(directory.file()))? != identity
            {
                return Err(epoch.failure(EndpointError::IdentityChanged));
            }
            epoch.ep(directory.validate())?;
            return epoch.check();
        }
    }
    Err(epoch.failure(EndpointError::LimitExceeded))
}

impl NativeEpoch<'_> {
    pub(super) fn witness_checkpoint(&mut self) -> Result<()> {
        let Some(mut witness) = self.witness.take() else {
            return self.check();
        };
        let result = witness.checkpoint(self);
        self.witness = Some(witness);
        result
    }
    pub(super) fn witness_create(&mut self, name: &CStr) -> Result<()> {
        if let Some(witness) = &mut self.witness {
            let result = witness.create(name);
            self.io(result)?;
        }
        Ok(())
    }
    pub(super) fn witness_move(&mut self, publish: bool) -> Result<()> {
        if let Some(witness) = &mut self.witness {
            let result = witness.move_entry(publish);
            self.io(result)?;
        }
        Ok(())
    }
    pub(super) fn witness_watch_namespace(&mut self, directory: &NativeDirectory) -> Result<()> {
        if let Some(witness) = &mut self.witness {
            let result = witness.watch(directory, Role::Namespace);
            self.io(result)?;
        }
        Ok(())
    }
    pub(super) fn descriptor_snapshot(
        &mut self,
        directory: &NativeDirectory,
        name: &CStr,
        expected: &[u8],
    ) -> Result<DescriptorEvidence> {
        self.check()?;
        self.admit_directory_descriptor()?;
        let file = self.io(open_at(directory.file(), name, libc::O_RDONLY))?;
        self.ep(endpoint::native_same_mount(directory.file(), &file))?;
        self.io(destination_security::check_private_file(&file))?;
        self.ep(endpoint::native_regular_metadata(&file))?;
        let identity = self.ep(object(&file))?;
        let stamp = Stamp::new(&self.io(file.metadata())?);
        if identity.kind != libc::S_IFREG
            || stamp.mode != (libc::S_IFREG | 0o600)
            || stamp.links != 1
            || expected.len() > d::MAX_BYTES
            || stamp.length != expected.len() as u64
        {
            return Err(self.failure(EndpointError::Unstable));
        }
        let mut bytes = vec![0; expected.len() + 1];
        self.charge(bytes.len() as u64)?;
        let count = self.io(file.read_at(&mut bytes, 0))?;
        if count != expected.len()
            || bytes.get(..count) != Some(expected)
            || self.ep(object(&file))? != identity
            || Stamp::new(&self.io(file.metadata())?) != stamp
        {
            return Err(self.failure(EndpointError::Unstable));
        }
        let named = self.io(open_at(directory.file(), name, libc::O_PATH))?;
        if self.ep(object(&named))? != identity {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        self.check()?;
        Ok(DescriptorEvidence {
            identity,
            stamp,
            bytes: expected.to_vec(),
            origin: DescriptorOrigin::FreshObserved,
        })
    }
}

/// One use, process-local accounting permission. It is never a `StepAdmission`.
pub(super) struct AccountingRepairPermit {
    binding: rules::RepairBinding,
    lifetime: Weak<()>,
    epoch: [u8; 16],
}
impl NativeEpoch<'_> {
    fn repair_binding(
        &mut self,
        context: &crate::connection::store::RecoveryContext,
        private: &PrivateNamespace,
        guard: &StableFileGuard,
        bytes: &[u8],
    ) -> Result<rules::RepairBinding> {
        self.recovery_check(context)?;
        self.namespace_check(private)?;
        self.validate_guard(guard, true)?;
        let (header, _, _) = private
            .descriptor
            .as_ref()
            .ok_or_else(|| self.failure(EndpointError::Unverified))?;
        let descriptor = d::decode(bytes).map_err(|_| self.failure(EndpointError::Unverified))?;
        if header.connection != context.id().connection.0
            || header.operation != context.id().serial
            || !matches!(&descriptor.body,d::Body::Displaced(body) if body.object==descriptor_object(&guard.actual) && body.stamp==descriptor_stamp(&guard.stamp) && body.matches_expected)
        {
            return Err(self.failure(EndpointError::Unverified));
        }
        Ok(rules::RepairBinding {
            connection: context.id().connection.0,
            operation: context.id().serial,
            attempt: header.attempt,
            revision: context.revision(),
            fence: context.fence(),
            namespace: descriptor_object(&private.identity),
            displaced: descriptor_object(&guard.actual),
            stamp: descriptor_stamp(&guard.stamp),
            bytes: bytes.to_vec(),
        })
    }
    fn closed_missing_k(&self) -> bool {
        self.witness.as_ref().is_some_and(|witness| {
            witness.restarted
                && witness.closed
                && !witness.failed
                && witness.expected.is_empty()
                && witness.inventory.contains(b"displaced".as_slice())
                && !witness.inventory.contains(b"displaced-intent".as_slice())
        })
    }
    pub(super) fn accounting_repair_permit(
        &mut self,
        context: &crate::connection::store::RecoveryContext,
        private: &PrivateNamespace,
        guard: &StableFileGuard,
        bytes: &[u8],
    ) -> Result<AccountingRepairPermit> {
        if !self.closed_missing_k() {
            return Err(self.failure(EndpointError::Unverified));
        }
        let binding = self.repair_binding(context, private, guard, bytes)?;
        Ok(AccountingRepairPermit {
            binding,
            lifetime: context.lifetime().clone(),
            epoch: self.nonce,
        })
    }
    #[expect(
        clippy::needless_pass_by_value,
        reason = "Consume this one-use repair permission; no propagation capability is returned."
    )]
    pub(super) fn repair_displaced_descriptor(
        &mut self,
        context: &crate::connection::store::RecoveryContext,
        private: &PrivateNamespace,
        guard: &StableFileGuard,
        bytes: &[u8],
        permit: AccountingRepairPermit,
    ) -> Result<()> {
        let current = self.repair_binding(context, private, guard, bytes)?;
        if permit.epoch != self.nonce
            || !permit.lifetime.ptr_eq(context.lifetime())
            || permit.lifetime.strong_count() == 0
        {
            return Err(self.failure(EndpointError::Unverified));
        }
        permit
            .binding
            .validate(&current, self.closed_missing_k())
            .map_err(|_| self.failure(EndpointError::Unverified))?;
        self.write_descriptor(
            &private.directory,
            c"displaced-intent",
            &permit.binding.bytes,
        )
    }
}

#[cfg(test)]
type CutHook = Box<dyn FnMut(&str, &NativeDirectory, &CStr)>;
#[cfg(test)]
thread_local! {
    static CUT_HOOK: std::cell::RefCell<Option<CutHook>> = const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
pub(super) fn test_cut(cut: &str, directory: &NativeDirectory, name: &CStr) {
    CUT_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().as_mut() {
            hook(cut, directory, name);
        }
    });
}

#[cfg(test)]
pub(super) fn set_test_cut(hook: Option<CutHook>) {
    CUT_HOOK.with(|slot| *slot.borrow_mut() = hook);
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::{
        model::{ConnectionId, Decision, EntryState, RelativePath, Side},
        store::{ConnectionPolicy, ConnectionStore, EndpointBinding, OperationIntent},
        transaction::{TransactionOutcome, execute_reviewed_initial},
    };
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    #[derive(Clone, Copy, Debug)]
    enum Fault {
        None,
        Swap(&'static str, &'static str),
        Mode,
        Writer,
        Metadata(bool),
        Traffic,
        Stop(usize),
        DescriptorStop(&'static str, &'static str),
    }

    // Source-only fixture: production executor, store, provider, lease helper and witness.
    // Every call remains ignored until exact deployed-kernel/provider qualification.
    #[expect(
        clippy::too_many_lines,
        reason = "Retain the actual executor fixture, cut injection and durable byte assertions in one scope."
    )]
    fn ordinary_case(
        reverse: bool,
        replacement: bool,
        executable: bool,
        length: usize,
        fault: Fault,
    ) {
        let tree = super::super::tests::fixture();
        let paths = [tree.path().join("left"), tree.path().join("right")];
        for path in &paths {
            std::fs::create_dir(path).unwrap();
        }
        let source = usize::from(reverse);
        let target = 1 - source;
        let bytes = vec![0x5a; length];
        std::fs::write(paths[source].join("file"), &bytes).unwrap();
        std::fs::set_permissions(
            paths[source].join("file"),
            std::fs::Permissions::from_mode(if executable { 0o700 } else { 0o600 }),
        )
        .unwrap();
        if replacement {
            std::fs::write(paths[target].join("file"), b"retained original").unwrap();
        }
        let endpoints = paths
            .each_ref()
            .map(|path| super::super::tests::endpoint(path));
        let bindings = paths.each_ref().map(|path| {
            let probe = endpoint::probe_endpoint(path).unwrap();
            EndpointBinding {
                identity: probe.identity,
                locator: probe.locator,
            }
        });
        let mut store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
        let [left, right] = bindings;
        let record = store
            .create_connection(
                ConnectionId([113; 16]),
                left,
                right,
                ConnectionPolicy {
                    deletion_retention_seconds: 2_592_000,
                },
            )
            .unwrap()
            .value()
            .clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let session = store.transaction_session(record.id).unwrap();
        let value = |bytes: &[u8], executable| {
            Some(EntryState::File {
                sha256: Sha256::digest(bytes).into(),
                bytes: bytes.len() as u64,
                executable,
            })
        };
        let proposed = value(&bytes, executable);
        let old = if replacement {
            value(b"retained original", false)
        } else {
            None
        };
        let reviewed =
            super::super::initial_preview(&store, record.id, &endpoints[0], &endpoints[1], &cancel)
                .unwrap()
                .accept(OperationIntent {
                    connection: record.id,
                    expected_revision: record.revision,
                    expected_baseline: None,
                    path: RelativePath::unix(b"file").unwrap(),
                    source_before: if reverse {
                        old.clone()
                    } else {
                        proposed.clone()
                    },
                    destination_before: if reverse { proposed.clone() } else { old },
                    decision: Decision::Propagate {
                        from: if reverse {
                            Side::Destination
                        } else {
                            Side::Source
                        },
                    },
                })
                .unwrap();
        let cuts = std::rc::Rc::new(std::cell::Cell::new(0));
        let hit = std::rc::Rc::new(std::cell::Cell::new(false));
        let seen = cuts.clone();
        let reached = hit.clone();
        let stop = cancel.clone();
        let retained = paths[target].join("retained-by-fixture");
        let retained_hook = retained.clone();
        let writer = std::rc::Rc::new(std::cell::RefCell::new(None));
        let held_writer = writer.clone();
        CUT_HOOK.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move |cut, directory, name| {
                if cut == "checkpoint" {
                    seen.set(seen.get() + 1);
                }
                if reached.get() {
                    return;
                }
                match fault {
                    Fault::Stop(index) if cut == "checkpoint" && seen.get() == index => {
                        reached.set(true);
                        stop.store(true, Ordering::Release);
                    }
                    Fault::DescriptorStop(at, component)
                        if cut == at && name.to_str().ok() == Some(component) =>
                    {
                        reached.set(true);
                        stop.store(true, Ordering::Release);
                    }
                    Fault::Swap(at, component)
                        if cut == at && name.to_str().ok() == Some(component) =>
                    {
                        reached.set(true);
                        let path = std::path::PathBuf::from(format!(
                            "/proc/self/fd/{}",
                            directory.file().as_raw_fd()
                        ))
                        .join(component);
                        let bytes = std::fs::read(&path).unwrap();
                        let permissions = std::fs::metadata(&path).unwrap().permissions();
                        std::fs::rename(&path, &retained_hook).unwrap();
                        let mut replacement = std::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .mode(permissions.mode() & 0o777)
                            .open(&path)
                            .unwrap();
                        std::io::Write::write_all(&mut replacement, &bytes).unwrap();
                        replacement.sync_all().unwrap();
                    }
                    Fault::Mode if cut == "stage-closed" => {
                        reached.set(true);
                        let path = format!("/proc/self/fd/{}/stage", directory.file().as_raw_fd());
                        // Two changes can coalesce: final bytes/mode alone cannot excuse ATTRIB.
                        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400))
                            .unwrap();
                        std::fs::set_permissions(
                            &path,
                            std::fs::Permissions::from_mode(if executable { 0o700 } else { 0o600 }),
                        )
                        .unwrap();
                    }
                    Fault::Writer if cut == "stage-closed" => {
                        reached.set(true);
                        *held_writer.borrow_mut() =
                            Some(open_at(directory.file(), c"stage", libc::O_WRONLY).unwrap());
                    }
                    Fault::Metadata(label) if cut == "stage-closed" => {
                        reached.set(true);
                        let file = open_at(directory.file(), c"stage", libc::O_RDONLY).unwrap();
                        metadata_fault(&file, label);
                    }
                    Fault::Traffic if cut == "stage-closed" => {
                        reached.set(true);
                        // Unrelated root traffic still spends the single reader's
                        // event credit. Alternating CREATE/DELETE cannot coalesce.
                        let root = std::path::PathBuf::from(format!(
                            "/proc/self/fd/{}/..",
                            directory.file().as_raw_fd()
                        ));
                        let capacity =
                            std::fs::read_to_string("/proc/sys/fs/inotify/max_queued_events")
                                .unwrap()
                                .trim()
                                .parse::<usize>()
                                .unwrap();
                        assert!(
                            (1..=131_072).contains(&capacity),
                            "bounded qualified overflow fixture capacity"
                        );
                        for _ in 0..=capacity / 2 {
                            let path = root.join("witness-unrelated-noise");
                            std::fs::write(&path, []).unwrap();
                            std::fs::remove_file(path).unwrap();
                        }
                    }
                    _ => {}
                }
            }));
        });
        let result = execute_reviewed_initial(
            &mut store,
            reviewed,
            &session,
            &endpoints[0],
            &endpoints[1],
            &cancel,
        );
        CUT_HOOK.with(|slot| *slot.borrow_mut() = None);
        drop(writer.borrow_mut().take());
        if matches!(fault, Fault::None) {
            assert!(
                matches!(result, TransactionOutcome::Verified { .. }),
                "executor must return the expected outcome"
            );
            assert_eq!(
                cuts.get(),
                if replacement { 44 } else { 34 },
                "actual two-epoch semantic call graph"
            );
            assert_eq!(std::fs::read(paths[target].join("file")).unwrap(), bytes);
            assert_eq!(
                std::fs::metadata(paths[target].join("file"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o7777,
                if executable { 0o700 } else { 0o600 }
            );
            assert_eq!(
                store
                    .load_baseline(record.id)
                    .unwrap()
                    .entries()
                    .unwrap()
                    .get(&RelativePath::unix(b"file").unwrap()),
                proposed.as_ref()
            );
        } else {
            assert!(hit.get(), "fault cut must be reached: {fault:?}");
            assert!(
                matches!(result, TransactionOutcome::RecoveryRequired { .. }),
                "executor must return the expected outcome"
            );
            assert_eq!(std::fs::read(paths[source].join("file")).unwrap(), bytes);
            if matches!(fault, Fault::Swap(..)) {
                assert!(retained.is_file());
            }
            // A final post-fsync refusal may have a whole durable baseline event;
            // reopening, rather than the poisoned owner, is authoritative.
            drop(store);
            let reopened = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
            if !matches!(fault, Fault::Stop(index) if index >= if replacement {43} else {33}) {
                assert!(
                    reopened
                        .load_baseline(record.id)
                        .unwrap()
                        .entries()
                        .is_none()
                );
            }
        }
    }
    fn metadata_fault(file: &File, label: bool) {
        let (name, bytes) = if label {
            // The qualified fixture supplies a permitted, different SELinux label.
            // No policy, privilege or host-global setting is changed by this fixture.
            (
                c"security.selinux",
                std::env::var("QOS_WITNESS_TEST_LABEL")
                    .expect("authorized alternate label")
                    .into_bytes(),
            )
        } else {
            let mut acl = 2_u32.to_le_bytes().to_vec();
            // POSIX ACL wire entries: owner, named user, group, mask, other.
            for (tag, permission, id) in [
                (1_u16, 6_u16, u32::MAX),
                (2, 0, 65534),
                (4, 0, u32::MAX),
                (16, 0, u32::MAX),
                (32, 0, u32::MAX),
            ] {
                acl.extend(tag.to_le_bytes());
                acl.extend(permission.to_le_bytes());
                acl.extend(id.to_le_bytes());
            }
            (c"system.posix_acl_access", acl)
        };
        // SAFETY: live fixture fd, NUL-terminated attribute name and exact input buffer.
        assert_eq!(
            unsafe {
                libc::fsetxattr(
                    file.as_raw_fd(),
                    name.as_ptr(),
                    bytes.as_ptr().cast(),
                    bytes.len(),
                    0,
                )
            },
            0,
            "fixture metadata mutation must really succeed"
        );
    }
    #[test]
    #[ignore = "HOLD: qualified Linux ACL/label/traffic fixture and helper; UNRUN"]
    fn ordinary_creation_acl_label_and_unrelated_event_bound_refuse() {
        ordinary_case(false, false, false, 32, Fault::Metadata(false));
        ordinary_case(false, false, false, 32, Fault::Metadata(true));
        ordinary_case(false, false, false, 32, Fault::Traffic);
    }
    #[test]
    #[ignore = "HOLD: authorized Linux volume and helper; source-only, UNRUN"]
    fn ordinary_modes_empty_and_many_chunks_add_replace_both_directions() {
        for reverse in [false, true] {
            for replacement in [false, true] {
                for executable in [false, true] {
                    for length in [0, 256 * 1024] {
                        ordinary_case(reverse, replacement, executable, length, Fault::None);
                    }
                }
            }
        }
    }
    #[test]
    #[ignore = "HOLD: authorized Linux volume and helper; source-only, UNRUN"]
    fn created_descriptor_and_stage_replacement_and_coalesced_attrib_refuse() {
        for name in ["intent", "staged-intent", "displaced-intent", "outcome"] {
            ordinary_case(
                false,
                true,
                false,
                32,
                Fault::Swap("descriptor-closed", name),
            );
        }
        ordinary_case(false, false, true, 32, Fault::Swap("stage-closed", "stage"));
        ordinary_case(false, true, false, 32, Fault::Mode);
        ordinary_case(false, false, false, 32, Fault::Writer);
    }
    #[test]
    #[ignore = "HOLD: authorized Linux volume and helper; source-only, UNRUN"]
    fn every_semantic_checkpoint_and_descriptor_write_fsync_interruption_refuses_ack() {
        for index in 1..=44 {
            ordinary_case(false, true, false, 32, Fault::Stop(index));
        }
        for cut in ["descriptor-prewrite", "descriptor-closed"] {
            for name in ["intent", "staged-intent", "displaced-intent", "outcome"] {
                ordinary_case(false, true, false, 32, Fault::DescriptorStop(cut, name));
            }
        }
    }
}
