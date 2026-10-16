//! Unix trusted-host directory admission and complete leaf-to-root commit.
use crate::native::atomic::{FileIdentity, Privacy, StorageDirectory, native};
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
    // Boxed so the retained chain does not make every `Result` carrying this
    // error as large as the whole chain.
    attempt: Box<Attempt>,
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
    ///
    /// # Errors
    ///
    /// Returns `self` unchanged for a partial attempt, or with the new native
    /// error when revalidation or any barrier fails again.
    pub fn retry_commit(mut self) -> Result<StorageDirectory, Self> {
        let Attempt::ReadyToCommit(chain) = &mut *self.attempt else {
            return Err(self);
        };
        if let Err(source) = chain.commit() {
            self.source = source;
            return Err(self);
        }
        if let Some(leaf) = chain.nodes.pop() {
            Ok(leaf.directory)
        } else {
            self.source = lost_node();
            Err(self)
        }
    }
}
impl fmt::Debug for ProvisionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (ready, chain) = match &*self.attempt {
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
/// mount topology and ACL stability. `OwnerOnly` validates newly created/adopted
/// suffix components and the requested leaf, not existing traversal ancestors;
/// no permissions are repaired. Permission admission is not storage qualification.
/// Every fresh call repeats all barriers, including already-existing ancestors.
///
/// # Errors
///
/// Returns a [`ProvisionError`] carrying the known [`DirectoryState`] and any
/// retained commit authority when admission, creation or a barrier fails.
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
            attempt: Box::new(Attempt::Partial(chain)),
        });
    }
    if let Err(source) = chain.commit() {
        return Err(ProvisionError {
            state,
            source,
            attempt: Box::new(Attempt::ReadyToCommit(chain)),
        });
    }
    match chain.nodes.pop() {
        Some(leaf) => Ok(leaf.directory),
        None => Err(ProvisionError {
            state,
            source: lost_node(),
            attempt: Box::new(Attempt::Partial(chain)),
        }),
    }
}

/// The chain always holds `/` once preparation succeeds; losing it is a bug,
/// reported as data corruption rather than a panic.
fn lost_node() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "directory chain lost a held node",
    )
}
fn last(nodes: &[Node]) -> io::Result<&Node> {
    nodes.last().ok_or_else(lost_node)
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
        let privacy = if private {
            directory.validate_privacy(self.privacy)
        } else {
            Ok(())
        };
        // Retain the opened child even when its privacy check failed.
        self.nodes.push(Node {
            directory,
            path,
            identity,
            private,
        });
        self.pending_child = None;
        privacy
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
                let parent = last(&self.nodes)?;
                let path = parent.path.join(name);
                let child = parent
                    .directory
                    .inner
                    .open_directory(name)
                    .map_err(|e| self.observe_binding(e, &path))?;
                self.push(child, path, false)?;
            }
        }
        self.binding = Some((prefix, self.nodes.len().saturating_sub(1)));
        self.validate_binding()?;
        self.create_suffix(suffix, state)?;
        let leaf = self.nodes.last_mut().ok_or_else(lost_node)?;
        leaf.private = self.privacy == Privacy::OwnerOnly;
        self.validate()
    }
    fn create_suffix(
        &mut self,
        suffix: Vec<OsString>,
        state: &mut DirectoryState,
    ) -> io::Result<()> {
        for name in suffix.into_iter().rev() {
            let parent = last(&self.nodes)?;
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
                let node = last(&self.nodes)?;
                checkpoint(Kind::Created, &node.path, Some(&node.identity))?;
            }
        }
        Ok(())
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
                &self.nodes.get(*index).ok_or_else(lost_node)?.identity,
                path,
            )?;
        }
        Ok(())
    }
    fn validate(&self) -> io::Result<()> {
        self.validate_binding()?;
        same(
            &StorageDirectory::open(Path::new("/"))?.identity()?,
            &self.nodes.first().ok_or_else(lost_node)?.identity,
            Path::new("/"),
        )?;
        for pair in self.nodes.windows(2) {
            let [parent, child] = pair else {
                return Err(lost_node());
            };
            let name = child
                .path
                .file_name()
                .ok_or_else(|| invalid("held non-root directory has no name"))?;
            let opened = parent
                .directory
                .inner
                .open_directory(name)
                .map_err(|e| self.observe_binding(e, &child.path))?;
            same(
                &FileIdentity {
                    inner: opened.identity()?,
                },
                &child.identity,
                &child.path,
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
            &last(&self.nodes)?.identity,
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
#[cfg_attr(
    not(feature = "test-support"),
    expect(
        clippy::unnecessary_wraps,
        reason = "the test-support build injects failures through the same signature"
    )
)]
fn checkpoint(kind: Kind, path: &Path, identity: Option<&FileIdentity>) -> io::Result<()> {
    #[cfg(feature = "test-support")]
    {
        use crate::native::atomic::test_support::{self, ProvisionCheckpoint as P, ProvisionEvent};
        let checkpoint = match kind {
            Kind::BeforeCreate => P::BeforeCreate,
            Kind::Created => P::Created,
            Kind::BeforeSync => P::BeforeSync,
            Kind::Synced => P::Synced,
        };
        test_support::provision_checkpoint(&ProvisionEvent {
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
            .and_then(|e| e.downcast_ref::<BindingChanged>())
            .is_some()
    }
    pub fn io_error(&self) -> &io::Error {
        self.inner.io_error()
    }
    pub fn into_io_error(self) -> io::Error {
        io::Error::new(self.io_error().kind(), self)
    }
    /// Revalidate retained bindings and repeat every barrier, without creation.
    ///
    /// # Errors
    ///
    /// Returns `self` with the new native error when revalidation or any
    /// barrier fails again, or unchanged when admission never completed.
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
///
/// # Errors
///
/// Returns a [`DirectoryReconcileError`] retaining the admitted chain when the
/// existing binding changed, admission failed, or a barrier failed.
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
                &last(&chain.nodes)?.identity,
                &expected.identity()?,
                &chain.requested,
            )
        });
    if let Err(source) = prepared {
        return Err(DirectoryReconcileError {
            inner: ProvisionError {
                state: DirectoryState::Unchanged,
                source,
                attempt: Box::new(Attempt::Partial(chain)),
            },
        });
    }
    match chain.commit() {
        Ok(()) => Ok(()),
        Err(source) => Err(DirectoryReconcileError {
            inner: ProvisionError {
                state: DirectoryState::Unchanged,
                source,
                attempt: Box::new(Attempt::ReadyToCommit(chain)),
            },
        }),
    }
}
