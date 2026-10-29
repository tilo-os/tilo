//! A host whose files are in memory: what the shell runs on where there is
//! no file system of its own to read (a test, a page), and the host every
//! test of the interpreter runs against.
//!
//! Its file actions do what the environment's would, without the record:
//! enough for a script to see its own changes. It runs no programs: a command
//! that is none of the shell's own needs tier 2 or 3 here.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::host::{DirEntry, FileCall, Host, Kind, Metadata, Program, WriteMode};
use crate::io::Stdio;
use crate::paths;

#[derive(Debug, Clone)]
enum Node {
    File(Vec<u8>),
    Dir,
    Symlink(PathBuf),
}

#[derive(Default)]
struct Tree {
    nodes: BTreeMap<PathBuf, (Node, i64)>,
    clock: i64,
}

/// Files in memory, every one of them readable and writable.
#[derive(Clone, Default)]
pub struct MemoryHost {
    tree: Arc<Mutex<Tree>>,
    /// Milliseconds slept so far: the host's clock moves only when
    /// something sleeps, so time in a test is exact.
    slept: Arc<std::sync::atomic::AtomicI64>,
    /// Every file action asked for, as its command line.
    actions: Arc<Mutex<Vec<Vec<String>>>>,
    /// The run each file action was part of, in order.
    runs: Arc<Mutex<Vec<String>>>,
    /// The terminal's width, when the host has a terminal to edit lines on.
    terminal: Option<usize>,
    /// Whether the terminal takes keys one at a time now, and how often it
    /// was switched.
    editing: Arc<Mutex<(bool, usize)>>,
    /// The person's history, oldest first, when this host holds it.
    history: Option<Arc<Mutex<Vec<String>>>>,
    /// The programs it says it can run, for completion.
    programs: Vec<String>,
    /// Whether `sleep` waits as long as it says, for tests of commands
    /// that run side by side.
    real_time: bool,
    /// Ctrl+Z was pressed and no job took it yet.
    suspended: Arc<std::sync::atomic::AtomicBool>,
}

impl MemoryHost {
    pub fn new() -> Self {
        let host = Self::default();
        host.tree().nodes.insert(PathBuf::from("/"), (Node::Dir, 0));
        host
    }

    /// A host with a terminal `columns` wide: the line editor runs on it,
    /// reading keys from the shell's standard input.
    #[must_use]
    pub fn with_terminal(mut self, columns: usize) -> Self {
        self.terminal = Some(columns);
        self
    }

    /// A host that holds the person's history, `lines` oldest first.
    #[must_use]
    pub fn with_history(mut self, lines: &[&str]) -> Self {
        self.history = Some(Arc::new(Mutex::new(
            lines.iter().map(|l| (*l).to_owned()).collect(),
        )));
        self
    }

    /// A host that says it runs `names`, as the environment's tools.
    #[must_use]
    pub fn with_programs(mut self, names: &[&str]) -> Self {
        self.programs = names.iter().map(|n| (*n).to_owned()).collect();
        self
    }

    /// A host whose `sleep` takes as long as it says.
    #[must_use]
    pub const fn with_real_time(mut self) -> Self {
        self.real_time = true;
        self
    }

    /// Press Ctrl+Z: the job in the foreground stops.
    pub fn suspend(&self) {
        self.suspended
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Whether the terminal takes keys one at a time now, and how many
    /// times the editor switched it.
    pub fn editing(&self) -> (bool, usize) {
        *self.editing.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn tree(&self) -> MutexGuard<'_, Tree> {
        self.tree.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Put a file at `path` with `contents`, making its directories.
    pub fn file(&self, path: &str, contents: impl Into<Vec<u8>>) -> &Self {
        let path = PathBuf::from(path);
        if let Some(parent) = path.parent() {
            self.make_dirs(parent);
        }
        let mut tree = self.tree();
        tree.clock = tree.clock.saturating_add(1);
        let at = tree.clock;
        tree.nodes.insert(path, (Node::File(contents.into()), at));
        drop(tree);
        self
    }

    /// Make the directory `path` and every one above it.
    pub fn dir(&self, path: &str) -> &Self {
        self.make_dirs(Path::new(path));
        self
    }

    pub fn symlink(&self, path: &str, target: &str) -> &Self {
        let mut tree = self.tree();
        tree.nodes.insert(
            PathBuf::from(path),
            (Node::Symlink(PathBuf::from(target)), 0),
        );
        drop(tree);
        self
    }

    /// The contents of the file at `path`.
    pub fn contents(&self, path: &str) -> Option<Vec<u8>> {
        match self.tree().nodes.get(Path::new(path)) {
            Some((Node::File(bytes), _)) => Some(bytes.clone()),
            _ => None,
        }
    }

    pub fn exists(&self, path: &str) -> bool {
        self.tree().nodes.contains_key(Path::new(path))
    }

    /// The run each file action so far was part of, by its id.
    pub fn action_runs(&self) -> Vec<String> {
        self.runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The file actions asked for so far, each as its command line.
    pub fn actions(&self) -> Vec<Vec<String>> {
        self.actions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn make_dirs(&self, path: &Path) {
        let mut tree = self.tree();
        for ancestor in path.ancestors() {
            if ancestor.as_os_str().is_empty() {
                continue;
            }
            tree.nodes
                .entry(ancestor.to_path_buf())
                .or_insert((Node::Dir, 0));
        }
    }

    fn resolve(&self, path: &Path) -> PathBuf {
        let tree = self.tree();
        let mut current = paths::normalize(path);
        for _ in 0..16 {
            let mut resolved = PathBuf::from("/");
            let mut changed = false;
            for component in current.components().skip(1) {
                resolved.push(component);
                if let Some((Node::Symlink(target), _)) = tree.nodes.get(&resolved) {
                    let base = resolved
                        .parent()
                        .unwrap_or_else(|| Path::new("/"))
                        .to_path_buf();
                    resolved = paths::normalize(&base.join(target));
                    changed = true;
                }
            }
            current = resolved;
            if !changed {
                break;
            }
        }
        current
    }

    fn meta_of(node: &Node, modified: i64) -> Metadata {
        let (kind, len, mode) = match node {
            Node::File(bytes) => (Kind::File, bytes.len() as u64, 0o644),
            Node::Dir => (Kind::Dir, 0, 0o755),
            Node::Symlink(target) => (Kind::Symlink, target.as_os_str().len() as u64, 0o777),
        };
        Metadata {
            kind,
            len,
            mode,
            modified,
            uid: 1000,
            gid: 1000,
            nlink: 1,
            dev: 1,
            ino: 0,
        }
    }

    fn not_found(path: &Path) -> io::Error {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("{}: No such file or directory", path.display()),
        )
    }

    fn apply(&self, name: &str, args: &[String], cwd: &Path) -> Result<(), String> {
        let operands: Vec<PathBuf> = args
            .iter()
            .filter(|a| !a.starts_with('-'))
            .map(|a| paths::join(cwd, a))
            .collect();
        let flags: String = args
            .iter()
            .filter(|a| a.starts_with('-') && a.len() > 1)
            .flat_map(|a| a.chars().skip(1))
            .collect();
        let mut tree = self.tree();
        tree.clock = tree.clock.saturating_add(1);
        let now = tree.clock;
        match name {
            "mkdir" => {
                for dir in &operands {
                    if flags.contains('p') {
                        drop(tree);
                        self.make_dirs(dir);
                        tree = self.tree();
                    } else {
                        if tree.nodes.contains_key(dir) {
                            return Err(format!("{}: File exists", dir.display()));
                        }
                        tree.nodes.insert(dir.clone(), (Node::Dir, now));
                    }
                }
            }
            "touch" => {
                for file in &operands {
                    tree.nodes
                        .entry(file.clone())
                        .or_insert((Node::File(Vec::new()), now));
                }
            }
            "append" => {
                let (text, files) = args.split_first().ok_or("missing text operand")?;
                for file in files {
                    let entry = tree
                        .nodes
                        .entry(paths::join(cwd, file))
                        .or_insert((Node::File(Vec::new()), now));
                    if let Node::File(bytes) = &mut entry.0 {
                        bytes.extend_from_slice(text.as_bytes());
                    }
                }
            }
            "rm" | "rmdir" => {
                for path in &operands {
                    let inside: Vec<PathBuf> = tree
                        .nodes
                        .keys()
                        .filter(|p| p.starts_with(path))
                        .cloned()
                        .collect();
                    if inside.is_empty() && !flags.contains('f') {
                        return Err(format!("{}: No such file or directory", path.display()));
                    }
                    for p in inside {
                        tree.nodes.remove(&p);
                    }
                }
            }
            "mv" | "cp" => {
                let (destination, sources) = operands.split_last().ok_or("missing file operand")?;
                let into_dir = matches!(tree.nodes.get(destination), Some((Node::Dir, _)));
                for source in sources {
                    let target = if into_dir {
                        destination.join(source.file_name().unwrap_or_default())
                    } else {
                        destination.clone()
                    };
                    let moved: Vec<(PathBuf, (Node, i64))> = tree
                        .nodes
                        .iter()
                        .filter(|(p, _)| p.starts_with(source))
                        .map(|(p, n)| (p.clone(), n.clone()))
                        .collect();
                    if moved.is_empty() {
                        return Err(format!("{}: No such file or directory", source.display()));
                    }
                    for (p, node) in moved {
                        let rest = p.strip_prefix(source).unwrap_or_else(|_| Path::new(""));
                        let to = if rest.as_os_str().is_empty() {
                            target.clone()
                        } else {
                            target.join(rest)
                        };
                        if name == "mv" {
                            tree.nodes.remove(&p);
                        }
                        tree.nodes.insert(to, node);
                    }
                }
            }
            other => return Err(format!("{other}: not a file action here")),
        }
        drop(tree);
        Ok(())
    }
}

struct MemoryWriter {
    host: MemoryHost,
    path: PathBuf,
}

impl Write for MemoryWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut tree = self.host.tree();
        tree.clock = tree.clock.saturating_add(1);
        let now = tree.clock;
        match tree.nodes.get_mut(&self.path) {
            Some((Node::File(bytes), at)) => {
                bytes.extend_from_slice(buf);
                *at = now;
            }
            _ => {
                tree.nodes
                    .insert(self.path.clone(), (Node::File(buf.to_vec()), now));
            }
        }
        drop(tree);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Host for MemoryHost {
    fn metadata(&self, path: &Path) -> io::Result<Metadata> {
        let path = self.resolve(path);
        let found = self
            .tree()
            .nodes
            .get(&path)
            .map(|(node, at)| Self::meta_of(node, *at));
        found.ok_or_else(|| Self::not_found(&path))
    }

    fn symlink_metadata(&self, path: &Path) -> io::Result<Metadata> {
        let normal = paths::normalize(path);
        let parent = normal.parent().map(|p| self.resolve(p));
        let path = match (parent, normal.file_name()) {
            (Some(parent), Some(name)) => parent.join(name),
            _ => normal,
        };
        let found = self
            .tree()
            .nodes
            .get(&path)
            .map(|(node, at)| Self::meta_of(node, *at));
        found.ok_or_else(|| Self::not_found(&path))
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        let path = self.resolve(path);
        let tree = self.tree();
        match tree.nodes.get(&path) {
            Some((Node::Dir, _)) => {}
            Some(_) => return Err(io::ErrorKind::NotADirectory.into()),
            None => return Err(Self::not_found(&path)),
        }
        Ok(tree
            .nodes
            .iter()
            .filter(|(p, _)| p.parent() == Some(path.as_path()) && **p != path)
            .map(|(p, (node, _))| DirEntry {
                name: p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                kind: Self::meta_of(node, 0).kind,
            })
            .collect())
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        let path = self.resolve(path);
        let tree = self.tree();
        match tree.nodes.get(&path) {
            Some((Node::File(bytes), _)) => Ok(Box::new(io::Cursor::new(bytes.clone()))),
            Some(_) => Err(io::ErrorKind::IsADirectory.into()),
            None => Err(Self::not_found(&path)),
        }
    }

    fn open_write(
        &self,
        path: &Path,
        mode: WriteMode,
        _run: Option<&crate::host::Run>,
    ) -> io::Result<Box<dyn Write + Send>> {
        let path = self.resolve(path);
        let mut tree = self.tree();
        let parent_is_dir = path
            .parent()
            .is_some_and(|p| matches!(tree.nodes.get(p), Some((Node::Dir, _))));
        if !parent_is_dir {
            return Err(Self::not_found(&path));
        }
        match (tree.nodes.get(&path), mode) {
            (Some((Node::Dir, _)), _) => return Err(io::ErrorKind::IsADirectory.into()),
            (Some(_), WriteMode::CreateNew) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "cannot overwrite existing file",
                ));
            }
            (Some(_), WriteMode::Append) => {}
            _ => {
                tree.nodes.insert(path.clone(), (Node::File(Vec::new()), 0));
            }
        }
        drop(tree);
        Ok(Box::new(MemoryWriter {
            host: self.clone(),
            path,
        }))
    }

    fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
        match self.tree().nodes.get(&paths::normalize(path)) {
            Some((Node::Symlink(target), _)) => Ok(target.clone()),
            _ => Err(io::ErrorKind::InvalidInput.into()),
        }
    }

    fn file_action(&self, call: &FileCall<'_>) -> i32 {
        let FileCall {
            name,
            args,
            cwd,
            io,
            run,
        } = *call;
        if let Some(run) = run {
            self.runs
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(run.id.clone());
        }
        let mut line = vec![name.to_string()];
        line.extend(args.iter().cloned());
        self.actions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(line);
        match self.apply(name, args, cwd) {
            Ok(()) => 0,
            Err(reason) => {
                let _ = io.stderr.write_str(&format!("{name}: {reason}\n"));
                1
            }
        }
    }

    fn program(&self, _: &[String], _: &[(String, String)], _: &Path, _: &Stdio) -> Program {
        Program::NeedsTier
    }

    fn now(&self) -> i64 {
        self.now_ms().div_euclid(1000)
    }

    fn now_ms(&self) -> i64 {
        1_700_000_000_000_i64.saturating_add(self.slept.load(std::sync::atomic::Ordering::SeqCst))
    }

    fn sleep(&self, millis: u64) {
        if self.real_time {
            std::thread::sleep(std::time::Duration::from_millis(millis));
        }
        let millis = i64::try_from(millis).unwrap_or(i64::MAX);
        self.slept
            .fetch_add(millis, std::sync::atomic::Ordering::SeqCst);
    }

    fn edit_mode(&self, on: bool) -> bool {
        if self.terminal.is_none() {
            return false;
        }
        let mut editing = self.editing.lock().unwrap_or_else(PoisonError::into_inner);
        editing.0 = on;
        editing.1 = editing.1.saturating_add(1);
        true
    }

    fn columns(&self) -> usize {
        self.terminal.unwrap_or(80)
    }

    fn take_suspend(&self) -> bool {
        self.suspended
            .swap(false, std::sync::atomic::Ordering::SeqCst)
    }

    fn history(&self, query: &crate::host::HistoryQuery) -> Option<Vec<String>> {
        let lines = self.history.as_ref()?;
        let lines = lines.lock().unwrap_or_else(PoisonError::into_inner);
        Some(
            lines
                .iter()
                .rev()
                .filter(|line| line.contains(&query.matching))
                .skip(query.skip)
                .take(query.limit)
                .cloned()
                .collect(),
        )
    }

    fn programs(&self, _path: &str) -> Vec<String> {
        self.programs.clone()
    }
}
