//! Trees kept in this process: `VirtualRoot::Memory` locations (one tree for
//! the process, or the page) and [`Dir::memory`](crate::Dir::memory) (one tree
//! per call).

use std::collections::HashMap;
use std::collections::btree_map::{self, BTreeMap};
use std::future::Future;
use std::ops::Bound;
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::task::{Context, Poll, Waker};

use tilo_clock::SystemTime;

use crate::watch::{ChangeKind, Watchers};
use crate::{Entry, FsError, Kind, LockMode, Meta, VPath};

/// A tree: every entry keyed by its full `/`-joined path, the root being the
/// empty key, plus the locks held on its paths.
pub(crate) struct Tree {
    state: Mutex<State>,
    released: Condvar,
    watchers: Watchers,
}

#[derive(Default)]
struct State {
    nodes: BTreeMap<String, Node>,
    locks: HashMap<String, Holders>,
    waiting: Vec<Waker>,
}

enum Node {
    File {
        bytes: Vec<u8>,
        modified: SystemTime,
    },
    Dir {
        modified: SystemTime,
    },
}

#[derive(Default)]
struct Holders {
    shared: usize,
    exclusive: bool,
}

impl Holders {
    fn admit(&mut self, mode: LockMode) -> bool {
        match mode {
            LockMode::Shared if !self.exclusive => {
                self.shared = self.shared.saturating_add(1);
                true
            }
            LockMode::Exclusive if !self.exclusive && self.shared == 0 => {
                self.exclusive = true;
                true
            }
            LockMode::Shared | LockMode::Exclusive => false,
        }
    }

    fn release(&mut self, mode: LockMode) {
        match mode {
            LockMode::Shared => self.shared = self.shared.saturating_sub(1),
            LockMode::Exclusive => self.exclusive = false,
        }
    }

    fn is_free(&self) -> bool {
        !self.exclusive && self.shared == 0
    }
}

impl Tree {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::default()),
            released: Condvar::new(),
            watchers: Watchers::default(),
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A [`Dir`](crate::Dir) in memory: a tree and the path of its root there.
#[derive(Clone)]
pub(crate) struct Scope {
    tree: Arc<Tree>,
    prefix: VPath,
}

impl Scope {
    /// A private tree whose root already exists.
    pub(crate) fn fresh() -> Self {
        let tree = Tree::new();
        tree.state().nodes.insert(
            String::new(),
            Node::Dir {
                modified: tilo_clock::wall(),
            },
        );
        Self {
            tree,
            prefix: VPath::root(),
        }
    }

    /// `prefix` in the tree every memory location of this process shares.
    pub(crate) fn global(prefix: VPath) -> Self {
        static GLOBAL: OnceLock<Arc<Tree>> = OnceLock::new();
        Self {
            tree: Arc::clone(GLOBAL.get_or_init(|| {
                let tree = Tree::new();
                tree.state().nodes.insert(
                    String::new(),
                    Node::Dir {
                        modified: tilo_clock::wall(),
                    },
                );
                tree
            })),
            prefix,
        }
    }

    /// The same tree, rooted at `prefix` below this scope's root.
    #[cfg_attr(
        not(all(target_arch = "wasm32", target_os = "unknown")),
        allow(dead_code, reason = "only the web mirror views subtrees")
    )]
    pub(crate) fn below(&self, prefix: &VPath) -> Self {
        Self {
            tree: Arc::clone(&self.tree),
            prefix: self.prefix.join(prefix),
        }
    }

    pub(crate) fn prefix(&self) -> &VPath {
        &self.prefix
    }

    fn key(&self, path: &VPath) -> String {
        self.prefix.join(path).as_str().to_owned()
    }

    pub(crate) fn read(&self, path: &VPath) -> Result<Vec<u8>, FsError> {
        let key = self.key(path);
        let state = self.tree.state();
        match state.nodes.get(&key) {
            Some(Node::File { bytes, .. }) => Ok(bytes.clone()),
            Some(Node::Dir { .. }) => Err(FsError::IsADirectory),
            None => Err(missing(&state.nodes, &key)),
        }
    }

    pub(crate) fn write_atomic(&self, path: &VPath, bytes: &[u8]) -> Result<(), FsError> {
        let key = self.key(path);
        let now = tilo_clock::wall();
        let mut state = self.tree.state();
        ensure_parents(&mut state.nodes, &key, now)?;
        if let Some(Node::Dir { .. }) = state.nodes.get(&key) {
            return Err(FsError::IsADirectory);
        }
        state.nodes.insert(
            key.clone(),
            Node::File {
                bytes: bytes.to_vec(),
                modified: now,
            },
        );
        drop(state);
        self.tree.watchers.notify(&key, ChangeKind::Changed);
        Ok(())
    }

    pub(crate) fn create_new(&self, path: &VPath, bytes: &[u8]) -> Result<bool, FsError> {
        let key = self.key(path);
        let now = tilo_clock::wall();
        let mut state = self.tree.state();
        ensure_parents(&mut state.nodes, &key, now)?;
        match state.nodes.entry(key.clone()) {
            btree_map::Entry::Occupied(_) => Ok(false),
            btree_map::Entry::Vacant(vacant) => {
                vacant.insert(Node::File {
                    bytes: bytes.to_vec(),
                    modified: now,
                });
                drop(state);
                self.tree.watchers.notify(&key, ChangeKind::Changed);
                Ok(true)
            }
        }
    }

    pub(crate) fn rename_noreplace(&self, from: &VPath, to: &VPath) -> Result<(), FsError> {
        let (from, to) = (self.key(from), self.key(to));
        let mut state = self.tree.state();
        if !state.nodes.contains_key(&from) {
            return Err(missing(&state.nodes, &from));
        }
        if state.nodes.contains_key(&to) {
            return Err(FsError::AlreadyExists);
        }
        ensure_parents(&mut state.nodes, &to, tilo_clock::wall())?;
        let mut moved = descendants(&state.nodes, &from);
        moved.push(from.clone());
        for old in moved {
            let suffix = old.get(from.len()..).unwrap_or_default();
            let new = format!("{to}{suffix}");
            if let Some(node) = state.nodes.remove(&old) {
                state.nodes.insert(new, node);
            }
        }
        drop(state);
        self.tree.watchers.notify(&from, ChangeKind::Removed);
        self.tree.watchers.notify(&to, ChangeKind::Changed);
        Ok(())
    }

    pub(crate) fn remove(&self, path: &VPath) -> Result<(), FsError> {
        let key = self.key(path);
        let mut state = self.tree.state();
        match state.nodes.get(&key) {
            None => return Err(missing(&state.nodes, &key)),
            Some(Node::Dir { .. }) if !descendants(&state.nodes, &key).is_empty() => {
                return Err(FsError::DirectoryNotEmpty);
            }
            Some(_) => {}
        }
        state.nodes.remove(&key);
        drop(state);
        self.tree.watchers.notify(&key, ChangeKind::Removed);
        Ok(())
    }

    pub(crate) fn create_dir_all(&self, path: &VPath) -> Result<(), FsError> {
        let key = self.key(path);
        let now = tilo_clock::wall();
        let mut state = self.tree.state();
        ensure_parents(&mut state.nodes, &key, now)?;
        match state.nodes.entry(key.clone()) {
            btree_map::Entry::Occupied(occupied) => match occupied.get() {
                Node::Dir { .. } => Ok(()),
                Node::File { .. } => Err(FsError::AlreadyExists),
            },
            btree_map::Entry::Vacant(vacant) => {
                vacant.insert(Node::Dir { modified: now });
                drop(state);
                self.tree.watchers.notify(&key, ChangeKind::Changed);
                Ok(())
            }
        }
    }

    pub(crate) fn list(&self, path: &VPath) -> Result<Vec<Entry>, FsError> {
        let key = self.key(path);
        let state = self.tree.state();
        match state.nodes.get(&key) {
            None => return Err(missing(&state.nodes, &key)),
            Some(Node::File { .. }) => return Err(FsError::NotADirectory),
            Some(Node::Dir { .. }) => {}
        }
        let start = if key.is_empty() {
            0
        } else {
            key.len().saturating_add(1)
        };
        let entries = descendant_range(&state.nodes, &key)
            .filter_map(|(child, node)| {
                let name = child.get(start..)?;
                (!name.contains('/')).then(|| Entry {
                    name: name.to_owned(),
                    kind: node.kind(),
                })
            })
            .collect();
        drop(state);
        Ok(entries)
    }

    pub(crate) fn metadata(&self, path: &VPath) -> Result<Meta, FsError> {
        let key = self.key(path);
        let state = self.tree.state();
        let (len, modified, kind) = match state.nodes.get(&key) {
            None => return Err(missing(&state.nodes, &key)),
            Some(Node::File { bytes, modified }) => (bytes.len() as u64, *modified, Kind::File),
            Some(Node::Dir { modified }) => (0, *modified, Kind::Dir),
        };
        drop(state);
        Ok(Meta {
            len,
            modified,
            kind,
            mode: None,
            symlink: false,
        })
    }

    /// Queue every change at or below `path` for the returned inbox.
    pub(crate) fn watch(&self, path: &VPath) -> Arc<crate::watch::Inbox> {
        self.tree
            .watchers
            .register(&self.prefix.join(path), &self.prefix)
    }

    /// Put a file loaded from elsewhere into the tree, with its own
    /// modification time, telling no watcher.
    #[cfg_attr(
        not(all(target_arch = "wasm32", target_os = "unknown")),
        allow(dead_code, reason = "only the web mirror loads trees")
    )]
    pub(crate) fn load_file(&self, path: &VPath, bytes: Vec<u8>, modified: SystemTime) {
        let key = self.key(path);
        let mut state = self.tree.state();
        if ensure_parents(&mut state.nodes, &key, modified).is_ok() {
            state.nodes.insert(key, Node::File { bytes, modified });
        }
    }

    pub(crate) fn try_lock(&self, path: &VPath, mode: LockMode) -> Result<Option<Held>, FsError> {
        let key = self.key(path);
        let mut state = self.tree.state();
        Ok(admit(&mut state, &key, mode)?.then(|| self.held(key, mode)))
    }

    #[cfg_attr(
        all(target_arch = "wasm32", target_os = "unknown"),
        allow(dead_code, reason = "the page's thread never waits for a lock")
    )]
    pub(crate) fn lock_blocking(&self, path: &VPath, mode: LockMode) -> Result<Held, FsError> {
        let key = self.key(path);
        let mut state = self.tree.state();
        loop {
            if admit(&mut state, &key, mode)? {
                drop(state);
                return Ok(self.held(key, mode));
            }
            state = self
                .tree
                .released
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    pub(crate) fn lock(&self, path: &VPath, mode: LockMode) -> LockFuture {
        LockFuture {
            scope: self.clone(),
            key: self.key(path),
            mode,
        }
    }

    fn held(&self, key: String, mode: LockMode) -> Held {
        Held {
            tree: Arc::clone(&self.tree),
            key,
            mode,
        }
    }
}

/// Creates the lock file like the native backend, then admits `mode` on
/// the path if no conflicting holder has it.
fn admit(state: &mut State, key: &str, mode: LockMode) -> Result<bool, FsError> {
    ensure_parents(&mut state.nodes, key, tilo_clock::wall())?;
    match state.nodes.get(key) {
        Some(Node::Dir { .. }) => return Err(FsError::IsADirectory),
        Some(Node::File { .. }) => {}
        None => {
            state.nodes.insert(
                key.to_owned(),
                Node::File {
                    bytes: Vec::new(),
                    modified: tilo_clock::wall(),
                },
            );
        }
    }
    Ok(state.locks.entry(key.to_owned()).or_default().admit(mode))
}

/// Resolves once [`Scope::lock`]'s path admits the mode, waking when any lock
/// of the tree is released.
pub(crate) struct LockFuture {
    scope: Scope,
    key: String,
    mode: LockMode,
}

impl Future for LockFuture {
    type Output = Result<Held, FsError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let mut state = this.scope.tree.state();
        match admit(&mut state, &this.key, this.mode) {
            Ok(true) => {
                drop(state);
                Poll::Ready(Ok(this.scope.held(this.key.clone(), this.mode)))
            }
            Ok(false) => {
                state.waiting.push(cx.waker().clone());
                Poll::Pending
            }
            Err(error) => Poll::Ready(Err(error)),
        }
    }
}

/// A lock admitted in a tree, released on drop.
pub(crate) struct Held {
    tree: Arc<Tree>,
    key: String,
    mode: LockMode,
}

impl Drop for Held {
    fn drop(&mut self) {
        let mut state = self.tree.state();
        if let Some(holders) = state.locks.get_mut(&self.key) {
            holders.release(self.mode);
            if holders.is_free() {
                state.locks.remove(&self.key);
            }
        }
        let waiting = std::mem::take(&mut state.waiting);
        drop(state);
        self.tree.released.notify_all();
        for waker in waiting {
            waker.wake();
        }
    }
}

impl Node {
    fn kind(&self) -> Kind {
        match self {
            Self::File { .. } => Kind::File,
            Self::Dir { .. } => Kind::Dir,
        }
    }
}

/// The parent key of `key` and its ancestors, nearest last; the root is "".
fn ancestors(key: &str) -> impl Iterator<Item = &str> {
    std::iter::once("").chain(
        key.match_indices('/')
            .filter_map(|(index, _)| key.get(..index)),
    )
}

/// Makes every ancestor of `key` a directory, failing where a file is in the
/// way.
fn ensure_parents(
    nodes: &mut BTreeMap<String, Node>,
    key: &str,
    now: SystemTime,
) -> Result<(), FsError> {
    if key.is_empty() {
        return Ok(());
    }
    for ancestor in ancestors(key) {
        match nodes.get(ancestor) {
            Some(Node::Dir { .. }) => {}
            Some(Node::File { .. }) => return Err(FsError::NotADirectory),
            None => {
                nodes.insert(ancestor.to_owned(), Node::Dir { modified: now });
            }
        }
    }
    Ok(())
}

/// Why `key` is absent: a file where a directory should be, or nothing.
fn missing(nodes: &BTreeMap<String, Node>, key: &str) -> FsError {
    if ancestors(key).any(|ancestor| matches!(nodes.get(ancestor), Some(Node::File { .. }))) {
        FsError::NotADirectory
    } else {
        FsError::NotFound
    }
}

/// Every entry below `key`, in key order. Keys below `a` sort between
/// `a/` and `a0`, because `0` follows `/`.
fn descendant_range<'a>(
    nodes: &'a BTreeMap<String, Node>,
    key: &str,
) -> btree_map::Range<'a, String, Node> {
    if key.is_empty() {
        nodes.range::<String, _>((Bound::Excluded(String::new()), Bound::Unbounded))
    } else {
        nodes.range(format!("{key}/")..format!("{key}0"))
    }
}

fn descendants(nodes: &BTreeMap<String, Node>, key: &str) -> Vec<String> {
    descendant_range(nodes, key)
        .map(|(child, _)| child.clone())
        .collect()
}
