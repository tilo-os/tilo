//! Unix trusted-host directory admission and complete leaf-to-root commit.
use crate::{FileIdentity, Privacy, StorageDirectory, native};
use std::{
    error::Error,
    ffi::OsString,
    fmt, io,
    path::{Component, Path, PathBuf},
};

/// Known namespace effects of a failed provisioning attempt, not a durability certificate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectoryState {
    /// This call is proved to have created no directory; existing names may be uncommitted.
    Unchanged,
    /// At least one directory was created; the complete commit did not succeed.
    CreatedDurabilityUnknown,
    /// A submitted mkdir has an unproven result, even if earlier creation succeeded.
    NamespaceUnknown,
}

struct Node {
    directory: StorageDirectory,
    path: PathBuf,
    identity: FileIdentity,
    private: bool,
}
struct Chain {
    requested: PathBuf,
    binding: Option<(PathBuf, usize)>,
    nodes: Vec<Node>,
    privacy: Privacy,
    created_any: bool,
    existing_only: bool,
    // Retain the submitted name and its parent (last node) even if opening the
    // child failed. This evidence never authorizes deletion or implicit replay.
    pending_child: Option<OsString>,
}
enum Attempt {
    Partial(Chain),
    ReadyToCommit(Chain),
}

/// Native error plus retained admission/commit authority. Dropping it never removes directories.
pub struct ProvisionError {
    state: DirectoryState,
    source: io::Error,
    attempt: Attempt,
}
impl ProvisionError {
    pub fn state(&self) -> DirectoryState {
        self.state
    }
    pub fn io_error(&self) -> &io::Error {
        &self.source
    }
    pub fn into_io_error(self) -> io::Error {
        io::Error::new(self.source.kind(), self)
    }

    /// Replay all real barriers only for a completely admitted requested chain.
    /// A partial attempt is returned unchanged: start a fresh provision call to
    /// reconcile the namespace. Retry never creates or cleans up directories.
    pub fn retry_commit(mut self) -> Result<StorageDirectory, Self> {
        let Attempt::ReadyToCommit(chain) = &self.attempt else {
            return Err(self);
        };
        if let Err(source) = chain.commit() {
            self.source = source;
            return Err(self);
        }
        let Attempt::ReadyToCommit(mut chain) = self.attempt else {
            unreachable!()
        };
        Ok(chain
            .nodes
            .pop()
            .expect("complete chain includes requested leaf")
            .directory)
    }
}
impl fmt::Debug for ProvisionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (ready, chain) = match &self.attempt {
            Attempt::Partial(c) => (false, c),
            Attempt::ReadyToCommit(c) => (true, c),
        };
        f.debug_struct("ProvisionError")
            .field("state", &self.state)
            .field("source", &self.source)
            .field("requested", &chain.requested)
            .field("ready_to_commit", &ready)
            .field("created_any", &chain.created_any)
            .field("pending_child", &chain.pending_child)
            .finish()
    }
}
impl fmt::Display for ProvisionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.source.fmt(f)
    }
}
impl Error for ProvisionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}
/// Recover directory effects through the compatibility I/O wrapper.
pub fn directory_state(error: &io::Error) -> Option<DirectoryState> {
    error
        .get_ref()
        .and_then(|e| e.downcast_ref::<ProvisionError>())
        .map(ProvisionError::state)
}

/// Create/admit the whole directory chain, then fsync every held node leaf-to-`/`.
///
/// Existing host aliases are admitted by canonicalization and forward held
/// lookup; missing suffixes must contain only normal components. An unresolved
/// `missing/../other` is rejected before mutation. This is trusted-host ambient
/// admission, not hostile-path confinement. The host must maintain namespace,
/// mount topology and ACL stability. OwnerOnly validates newly created/adopted
/// suffix components and the requested leaf, not existing traversal ancestors;
/// no permissions are repaired. Permission admission is not storage qualification.
/// Every fresh call repeats all barriers, including already-existing ancestors.
pub fn provision_directory(
    path: &Path,
    privacy: Privacy,
) -> Result<StorageDirectory, ProvisionError> {
    let mut chain = Chain {
        requested: path.to_owned(),
        binding: None,
        nodes: Vec::new(),
        privacy,
        created_any: false,
        existing_only: false,
        pending_child: None,
    };
    let mut state = DirectoryState::Unchanged;
    if let Err(source) = chain.prepare(&mut state) {
        return Err(ProvisionError {
            state,
            source,
            attempt: Attempt::Partial(chain),
        });
    }
    let attempt = Attempt::ReadyToCommit(chain);
    let Attempt::ReadyToCommit(chain) = &attempt else {
        unreachable!()
    };
    if let Err(source) = chain.commit() {
        return Err(ProvisionError {
            state,
            source,
            attempt,
        });
    }
    let Attempt::ReadyToCommit(mut chain) = attempt else {
        unreachable!()
    };
    Ok(chain
        .nodes
        .pop()
        .expect("complete chain includes requested leaf")
        .directory)
}

impl Chain {
    fn push(
        &mut self,
        directory: native::Directory,
        path: PathBuf,
        private: bool,
    ) -> io::Result<()> {
        let directory = StorageDirectory { inner: directory };
        let identity = directory.identity()?;
        // Retain the opened child even when its privacy check later fails.
        self.nodes.push(Node {
            directory,
            path,
            identity,
            private,
        });
        self.pending_child = None;
        if private {
            self.nodes
                .last()
                .expect("just pushed")
                .directory
                .validate_privacy(self.privacy)?;
        }
        Ok(())
    }
    fn prepare(&mut self, state: &mut DirectoryState) -> io::Result<()> {
        self.prepare_mode(state, false)
    }
    fn prepare_mode(&mut self, state: &mut DirectoryState, existing_only: bool) -> io::Result<()> {
        if self.requested.as_os_str().is_empty() {
            return Err(invalid("empty directory request"));
        }
        if !self.requested.is_absolute() {
            self.requested = std::env::current_dir()?.join(&self.requested);
        }
        let mut prefix = self.requested.clone();
        let mut suffix = Vec::new();
        let canonical = loop {
            match std::fs::metadata(&prefix) {
                Ok(metadata) => {
                    if !metadata.is_dir() {
                        if existing_only {
                            return Err(binding_changed(&prefix));
                        }
                        return Err(io::Error::new(
                            io::ErrorKind::NotADirectory,
                            "existing prefix is not a directory",
                        ));
                    }
                    break std::fs::canonicalize(&prefix)
                        .map_err(|e| self.observe_binding(e, &prefix))?;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    if existing_only {
                        return Err(binding_changed(&self.requested));
                    }
                    // A dangling link must not be treated as a creatable directory.
                    match std::fs::symlink_metadata(&prefix) {
                        Ok(_) => return Err(error),
                        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                        Err(e) => return Err(e),
                    }
                    let Some(Component::Normal(name)) = prefix.components().next_back() else {
                        return Err(invalid(
                            "unresolved directory suffix must contain only normal components",
                        ));
                    };
                    native::validate_component(name)?;
                    suffix.push(name.to_owned());
                    if !prefix.pop() {
                        return Err(error);
                    }
                }
                Err(error) => return Err(error),
            }
        };
        self.push(
            native::Directory::open(Path::new("/"))?,
            PathBuf::from("/"),
            false,
        )?;
        for component in canonical.components() {
            if let Component::Normal(name) = component {
                let parent = self.nodes.last().expect("root held");
                let path = parent.path.join(name);
                let child = parent
                    .directory
                    .inner
                    .open_directory(name)
                    .map_err(|e| self.observe_binding(e, &path))?;
                self.push(child, path, false)?;
            }
        }
        self.binding = Some((prefix, self.nodes.len() - 1));
        self.validate_binding()?;
        for name in suffix.into_iter().rev() {
            let parent = self.nodes.last().expect("root held");
            let physical = parent.path.join(&name);
            let mut created = false;
            let child = match parent.directory.inner.open_directory(&name) {
                Ok(child) => child,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    parent.directory.inner.creation_preflight(self.privacy)?;
                    checkpoint(Kind::BeforeCreate, &physical, None)?;
                    self.pending_child = Some(name.clone());
                    match parent
                        .directory
                        .inner
                        .create_directory(&name, self.privacy, &physical)
                    {
                        Ok(()) => {
                            self.created_any = true;
                            *state = DirectoryState::CreatedDurabilityUnknown;
                            created = true;
                        }
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                        Err(error) => {
                            *state = DirectoryState::NamespaceUnknown;
                            return Err(error);
                        }
                    }
                    parent.directory.inner.open_directory(&name)?
                }
                Err(error) => return Err(error),
            };
            self.push(child, physical, self.privacy == Privacy::OwnerOnly)?;
            if created {
                let node = self.nodes.last().expect("created child held");
                checkpoint(Kind::Created, &node.path, Some(&node.identity))?;
            }
        }
        let leaf = self.nodes.last_mut().expect("root held");
        leaf.private = self.privacy == Privacy::OwnerOnly;
        self.validate()
    }
    fn observe_binding(&self, error: io::Error, path: &Path) -> io::Error {
        if self.existing_only {
            binding_observation(error, path)
        } else {
            error
        }
    }
    fn validate_binding(&self) -> io::Result<()> {
        if let Some((path, index)) = &self.binding {
            same(
                &StorageDirectory::open(path)
                    .map_err(|e| self.observe_binding(e, path))?
                    .identity()?,
                &self.nodes[*index].identity,
                path,
            )?;
        }
        Ok(())
    }
    fn validate(&self) -> io::Result<()> {
        self.validate_binding()?;
        same(
            &StorageDirectory::open(Path::new("/"))?.identity()?,
            &self.nodes[0].identity,
            Path::new("/"),
        )?;
        for pair in self.nodes.windows(2) {
            let name = pair[1].path.file_name().expect("non-root edge");
            let opened = pair[0]
                .directory
                .inner
                .open_directory(name)
                .map_err(|e| self.observe_binding(e, &pair[1].path))?;
            same(
                &FileIdentity {
                    inner: opened.identity()?,
                },
                &pair[1].identity,
                &pair[1].path,
            )?;
        }
        for node in &self.nodes {
            if node.private {
                node.directory.validate_privacy(self.privacy)?;
            }
        }
        same(
            &StorageDirectory::open(&self.requested)
                .map_err(|e| self.observe_binding(e, &self.requested))?
                .identity()?,
            &self.nodes.last().expect("leaf held").identity,
            &self.requested,
        )
    }
    fn commit(&self) -> io::Result<()> {
        self.validate()?;
        for node in self.nodes.iter().rev() {
            checkpoint(Kind::BeforeSync, &node.path, Some(&node.identity))?;
            node.directory.inner.sync()?;
            checkpoint(Kind::Synced, &node.path, Some(&node.identity))?;
        }
        self.validate()
    }
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn same(actual: &FileIdentity, held: &FileIdentity, path: &Path) -> io::Result<()> {
    if actual == held {
        Ok(())
    } else {
        Err(binding_changed(path))
    }
}
#[derive(Clone, Copy)]
enum Kind {
    BeforeCreate,
    Created,
    BeforeSync,
    Synced,
}
fn checkpoint(kind: Kind, path: &Path, identity: Option<&FileIdentity>) -> io::Result<()> {
    #[cfg(feature = "test-support")]
    {
        use crate::test_support::{self, ProvisionCheckpoint as P, ProvisionEvent};
        let checkpoint = match kind {
            Kind::BeforeCreate => P::BeforeCreate,
            Kind::Created => P::Created,
            Kind::BeforeSync => P::BeforeSync,
            Kind::Synced => P::Synced,
        };
        test_support::provision_checkpoint(ProvisionEvent {
            checkpoint,
            path: path.to_owned(),
            identity: identity.cloned(),
        })
    }
    #[cfg(not(feature = "test-support"))]
    {
        let _ = (kind, path, identity);
        Ok(())
    }
}

/// A changed existing namespace binding, distinct from native barrier I/O.
#[derive(Debug)]
struct BindingChanged(PathBuf);
impl fmt::Display for BindingChanged {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "directory binding changed: {}", self.0.display())
    }
}
impl Error for BindingChanged {}
fn binding_changed(path: &Path) -> io::Error {
    io::Error::other(BindingChanged(path.to_owned()))
}
fn binding_observation(error: io::Error, path: &Path) -> io::Error {
    if matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    ) {
        binding_changed(path)
    } else {
        error
    }
}

/// An existing-chain failure retaining all admitted authority and commit obligations.
#[derive(Debug)]
pub struct DirectoryReconcileError {
    inner: ProvisionError,
}
impl DirectoryReconcileError {
    pub fn source_changed(&self) -> bool {
        self.inner
            .source
            .get_ref()
            .is_some_and(|e| e.is::<BindingChanged>())
    }
    pub fn io_error(&self) -> &io::Error {
        self.inner.io_error()
    }
    pub fn into_io_error(self) -> io::Error {
        io::Error::new(self.io_error().kind(), self)
    }
    /// Revalidate retained bindings and repeat every barrier, without creation.
    pub fn retry_commit(self) -> Result<(), Self> {
        self.inner
            .retry_commit()
            .map(|_| ())
            .map_err(|inner| Self { inner })
    }
}
impl fmt::Display for DirectoryReconcileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.fmt(f)
    }
}
impl Error for DirectoryReconcileError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.inner)
    }
}

/// Admit an entirely existing physical chain bound to the held leaf, then commit
/// every retained ancestor. Never creates, replaces or removes any name.
pub fn reconcile_directory(
    path: &Path,
    expected: &StorageDirectory,
    privacy: Privacy,
) -> Result<(), DirectoryReconcileError> {
    let mut chain = Chain {
        requested: path.to_owned(),
        binding: None,
        nodes: Vec::new(),
        privacy,
        created_any: false,
        existing_only: true,
        pending_child: None,
    };
    let prepared = chain
        .prepare_mode(&mut DirectoryState::Unchanged, true)
        .and_then(|()| {
            same(
                &chain.nodes.last().expect("existing leaf held").identity,
                &expected.identity()?,
                &chain.requested,
            )
        });
    if let Err(source) = prepared {
        return Err(DirectoryReconcileError {
            inner: ProvisionError {
                state: DirectoryState::Unchanged,
                source,
                attempt: Attempt::Partial(chain),
            },
        });
    }
    match chain.commit() {
        Ok(()) => Ok(()),
        Err(source) => Err(DirectoryReconcileError {
            inner: ProvisionError {
                state: DirectoryState::Unchanged,
                source,
                attempt: Attempt::ReadyToCommit(chain),
            },
        }),
    }
}
