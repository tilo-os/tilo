//! Pure data model for the code editor: the project file tree, the set of
//! open buffers, and language detection. No rendering, no IO, no CRDT — this
//! layer is the in-memory source of truth the widgets read from.
//!
//! Buffer *text* lives in the shared Automerge document (so edits are
//! collaborative and undoable); this model only tracks the structural facts
//! around it — which files exist, which are open, which tab is active, and
//! where each caret sits.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use uuid::Uuid;

/// Ephemeral arena identifier for a node in this running project projection.
/// Never publish this outside the process; [`TreeNode::resource_id`] is the
/// durable collaborative identity.
pub type NodeId = usize;

/// The recognised source languages. Drives both the syntax-highlighting
/// grammar selection and the status-bar label. `PlainText` is the fallback
/// for anything we don't have a grammar for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    Toml,
    Markdown,
    Json,
    Yaml,
    Python,
    JavaScript,
    TypeScript,
    Html,
    Css,
    Shell,
    C,
    Cpp,
    Go,
    Verilog,
    SystemVerilog,
    PlainText,
}

impl Language {
    /// Human-readable name shown in the status bar.
    pub fn label(self) -> &'static str {
        match self {
            Language::Rust => "Rust",
            Language::Toml => "TOML",
            Language::Markdown => "Markdown",
            Language::Json => "JSON",
            Language::Yaml => "YAML",
            Language::Python => "Python",
            Language::JavaScript => "JavaScript",
            Language::TypeScript => "TypeScript",
            Language::Html => "HTML",
            Language::Css => "CSS",
            Language::Shell => "Shell",
            Language::C => "C",
            Language::Cpp => "C++",
            Language::Go => "Go",
            Language::Verilog => "Verilog",
            Language::SystemVerilog => "SystemVerilog",
            Language::PlainText => "Plain Text",
        }
    }

    /// The file extension the toolkit's syntax highlighter resolves this
    /// language's grammar by.
    pub fn grammar_token(self) -> &'static str {
        match self {
            Language::Rust => "rs",
            Language::Toml => "toml",
            Language::Markdown => "md",
            Language::Json => "json",
            Language::Yaml => "yaml",
            Language::Python => "py",
            Language::JavaScript => "js",
            Language::TypeScript => "ts",
            Language::Html => "html",
            Language::Css => "css",
            Language::Shell => "sh",
            Language::C => "c",
            Language::Cpp => "cpp",
            Language::Go => "go",
            Language::Verilog => "v",
            Language::SystemVerilog => "sv",
            Language::PlainText => "txt",
        }
    }

    /// Resolve a language from a file name's extension.
    pub fn from_path(name: &str) -> Language {
        let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        match ext.as_str() {
            "rs" => Language::Rust,
            "toml" => Language::Toml,
            "md" | "markdown" => Language::Markdown,
            "json" => Language::Json,
            "yaml" | "yml" => Language::Yaml,
            "py" => Language::Python,
            "js" | "mjs" | "cjs" => Language::JavaScript,
            "ts" | "tsx" => Language::TypeScript,
            "html" | "htm" => Language::Html,
            "css" => Language::Css,
            "sh" | "bash" | "zsh" => Language::Shell,
            "c" | "h" => Language::C,
            "cpp" | "cc" | "cxx" | "hpp" => Language::Cpp,
            "go" => Language::Go,
            "v" | "vh" => Language::Verilog,
            "sv" | "svh" => Language::SystemVerilog,
            _ => match name {
                "Cargo.lock" => Language::Toml,
                "Makefile" | "Dockerfile" | "Containerfile" => Language::Shell,
                _ => Language::PlainText,
            },
        }
    }

    /// Default indentation width (in spaces) for the language. Drives the
    /// status-bar indentation indicator and Tab insertion. Most of our
    /// supported languages settle on 4-wide; 2-wide communities get 2.
    pub fn indent_width(self) -> usize {
        match self {
            Language::JavaScript
            | Language::TypeScript
            | Language::Html
            | Language::Css
            | Language::Json
            | Language::Yaml => 2,
            _ => 4,
        }
    }

    /// The token that begins a line comment in this language, if it has one.
    /// Drives the toggle-comment line operation. Languages without a line
    /// comment (JSON, HTML, CSS, plain text) return `None` and toggle is a
    /// no-op for them.
    pub fn line_comment(self) -> Option<&'static str> {
        match self {
            Language::Rust
            | Language::JavaScript
            | Language::TypeScript
            | Language::C
            | Language::Cpp
            | Language::Go
            | Language::Verilog
            | Language::SystemVerilog => Some("//"),
            Language::Python | Language::Shell | Language::Toml | Language::Yaml => Some("#"),
            Language::Markdown
            | Language::Html
            | Language::Css
            | Language::Json
            | Language::PlainText => None,
        }
    }
}

/// One node in the project tree. Folders own an ordered list of children;
/// files carry the language and the seed body used to populate the buffer
/// when first opened.
#[derive(Clone, Debug)]
pub struct TreeNode {
    pub id: NodeId,
    /// Stable file resource identity. Local `id`s are permitted to differ on
    /// peers after lazy filesystem loading; this UUID is what actions, search,
    /// history and collaboration use.
    pub resource_id: String,
    pub name: String,
    pub parent: Option<NodeId>,
    pub kind: NodeKind,
    /// Depth from the project root (root children are depth 0). Cached so the
    /// flattened render list doesn't have to walk parents every frame.
    pub depth: usize,
    /// The real filesystem path this node projects, when the project was opened
    /// from disk (`Project::from_dir` / `open_single_file`). `None` for the
    /// bundled sample project and freshly-created scratch files, which live
    /// only in memory until saved. Real file I/O (read/write) uses this path.
    pub disk_path: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub enum NodeKind {
    /// `loaded` is `false` for a disk-backed folder whose children have not yet
    /// been read from the filesystem; the first expansion reads them lazily
    /// (`Project::load_children`). In-memory folders are always `loaded`.
    Folder {
        expanded: bool,
        loaded: bool,
        children: Vec<NodeId>,
    },
    File {
        language: Language,
        seed: String,
    },
}

impl TreeNode {
    pub fn is_folder(&self) -> bool {
        matches!(self.kind, NodeKind::Folder { .. })
    }
}

/// The whole project: a flat arena of nodes plus the root id. Tree shape is
/// expressed through `parent` / `children`.
#[derive(Clone, Debug, Default)]
pub struct Project {
    pub name: String,
    pub nodes: BTreeMap<NodeId, TreeNode>,
    pub root: NodeId,
    next_id: NodeId,
    /// Nodes read from disk since [`Self::take_read_from_disk`] last asked.
    read_from_disk: Vec<NodeId>,
}

impl Project {
    pub fn new(name: &str) -> Self {
        let mut p = Project {
            name: name.to_string(),
            nodes: BTreeMap::new(),
            root: 0,
            next_id: 0,
            read_from_disk: Vec::new(),
        };
        let root = p.alloc();
        p.nodes.insert(
            root,
            TreeNode {
                id: root,
                resource_id: Uuid::new_v4().to_string(),
                name: name.to_string(),
                parent: None,
                kind: NodeKind::Folder {
                    expanded: true,
                    loaded: true,
                    children: Vec::new(),
                },
                depth: 0,
                disk_path: None,
            },
        );
        p.root = root;
        p
    }

    fn alloc(&mut self) -> NodeId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// The id `alloc` will hand out next.
    #[cfg(test)]
    pub(crate) fn peek_next_id(&self) -> NodeId {
        self.next_id
    }

    /// Surface a buffer that already exists in the shared document (created by a
    /// Ctrl+K "New file" action or by a peer) as a tracked in-memory file under
    /// the root, using its document-assigned `id` verbatim so the in-memory node
    /// and the buffer key stay in agreement. Idempotent, and bumps the allocator
    /// past `id` so a later `alloc` cannot collide with it. The
    /// buffer's text stays in the CRDT; this only creates the in-memory shell the
    /// tree and tabs read from — no persisted file model is invented.
    pub fn adopt_file(&mut self, id: NodeId, name: &str) {
        if self.nodes.contains_key(&id) {
            return;
        }
        let depth = self.nodes.get(&self.root).map_or(0, |n| n.depth + 1);
        let lang = Language::from_path(name);
        self.nodes.insert(
            id,
            TreeNode {
                id,
                resource_id: format!("legacy-node-{id}"),
                name: name.to_string(),
                parent: Some(self.root),
                kind: NodeKind::File {
                    language: lang,
                    seed: String::new(),
                },
                depth,
                disk_path: None,
            },
        );
        let root = self.root;
        self.attach(root, id);
        if id >= self.next_id {
            self.next_id = id + 1;
        }
    }

    /// Materialise a shared file record in this process's tree projection.
    /// The caller owns the durable `resource_id`; the arena id is intentionally
    /// allocated locally and is never used in a resource/action payload.
    pub fn adopt_collaborative_file(
        &mut self,
        resource_id: &str,
        parent: Option<&str>,
        name: &str,
        language: Language,
    ) -> NodeId {
        self.adopt_collaborative_node(resource_id, "file", parent, name, language)
    }

    pub fn adopt_collaborative_folder(
        &mut self,
        resource_id: &str,
        parent: Option<&str>,
        name: &str,
    ) -> NodeId {
        self.adopt_collaborative_node(resource_id, "folder", parent, name, Language::PlainText)
    }

    fn adopt_collaborative_node(
        &mut self,
        resource_id: &str,
        kind: &str,
        parent: Option<&str>,
        name: &str,
        language: Language,
    ) -> NodeId {
        if let Some((id, _)) = self
            .nodes
            .iter()
            .find(|(_, node)| node.resource_id == resource_id)
        {
            let id = *id;
            let parent_id = parent
                .and_then(|parent| {
                    self.nodes
                        .iter()
                        .find_map(|(id, node)| (node.resource_id == parent).then_some(*id))
                })
                .unwrap_or(self.root);
            let old_parent = self.nodes.get(&id).and_then(|node| node.parent);
            if id != self.root && old_parent != Some(parent_id) {
                if let Some(old_parent) = old_parent
                    && let Some(TreeNode {
                        kind: NodeKind::Folder { children, .. },
                        ..
                    }) = self.nodes.get_mut(&old_parent)
                {
                    children.retain(|child| *child != id);
                }
                let depth = self.nodes.get(&parent_id).map_or(0, |node| node.depth + 1);
                if let Some(node) = self.nodes.get_mut(&id) {
                    node.parent = Some(parent_id);
                    node.depth = depth;
                }
                self.attach(parent_id, id);
            }
            if let Some(node) = self.nodes.get_mut(&id) {
                node.name = name.to_string();
                match (&mut node.kind, kind) {
                    (
                        NodeKind::File {
                            language: node_language,
                            ..
                        },
                        "file",
                    ) => *node_language = language,
                    (NodeKind::Folder { .. }, "folder") => {}
                    (_, "folder") => {
                        node.kind = NodeKind::Folder {
                            expanded: false,
                            loaded: true,
                            children: Vec::new(),
                        }
                    }
                    (_, _) => {
                        node.kind = NodeKind::File {
                            language,
                            seed: String::new(),
                        }
                    }
                }
            }
            return id;
        }
        let parent_id = parent
            .and_then(|parent| {
                self.nodes
                    .iter()
                    .find_map(|(id, node)| (node.resource_id == parent).then_some(*id))
            })
            .unwrap_or(self.root);
        let depth = self.nodes.get(&parent_id).map_or(0, |node| node.depth + 1);
        let id = self.alloc();
        self.nodes.insert(
            id,
            TreeNode {
                id,
                resource_id: resource_id.to_string(),
                name: name.to_string(),
                parent: Some(parent_id),
                kind: if kind == "folder" {
                    NodeKind::Folder {
                        expanded: false,
                        loaded: true,
                        children: Vec::new(),
                    }
                } else {
                    NodeKind::File {
                        language,
                        seed: String::new(),
                    }
                },
                depth,
                disk_path: None,
            },
        );
        self.attach(parent_id, id);
        id
    }

    pub fn node_for_resource(&self, resource_id: &str) -> Option<NodeId> {
        self.nodes
            .iter()
            .find_map(|(id, node)| (node.resource_id == resource_id).then_some(*id))
    }

    /// Remove a file projection after its durable record becomes a tombstone.
    /// The tombstone remains in the CRDT; only this process's render/cache node
    /// disappears, so a later sync cannot resurrect it by accident.
    pub fn remove_collaborative_file(&mut self, resource_id: &str) -> Option<NodeId> {
        let id = self.node_for_resource(resource_id)?;
        let parent = self.nodes.get(&id).and_then(|node| node.parent);
        self.nodes.remove(&id);
        if let Some(parent) = parent
            && let Some(TreeNode {
                kind: NodeKind::Folder { children, .. },
                ..
            }) = self.nodes.get_mut(&parent)
        {
            children.retain(|child| *child != id);
        }
        Some(id)
    }

    /// Order every projected folder from the durable `order` field.  Arena
    /// allocation deliberately differs between replicas, so it must never be
    /// used as a tie breaker for a collaborative tree.
    pub fn order_collaborative_children(&mut self, ordering: &[(String, i64)]) {
        let order_by_resource: BTreeMap<_, _> = ordering.iter().cloned().collect();
        let order_by_node: BTreeMap<_, _> = self
            .nodes
            .iter()
            .map(|(id, child)| {
                (
                    *id,
                    (
                        order_by_resource
                            .get(&child.resource_id)
                            .copied()
                            .unwrap_or(i64::MAX),
                        child.resource_id.clone(),
                    ),
                )
            })
            .collect();
        for node in self.nodes.values_mut() {
            if let NodeKind::Folder { children, .. } = &mut node.kind {
                children
                    .sort_by(|left, right| order_by_node.get(left).cmp(&order_by_node.get(right)));
            }
        }
    }

    /// Add an in-memory folder under `parent`, returning its id.
    pub fn add_folder(&mut self, parent: NodeId, name: &str) -> NodeId {
        self.insert_folder(parent, name, None, false)
    }

    /// Add a file under `parent` with a seed body, inferring the language.
    pub fn add_file(&mut self, parent: NodeId, name: &str, seed: &str) -> NodeId {
        self.insert_file(parent, name, seed, None)
    }

    /// Add a disk-backed folder under `parent`. Its children are read lazily
    /// from `path` the first time it is expanded (`load_children`).
    pub fn add_disk_folder(&mut self, parent: NodeId, name: &str, path: &Path) -> NodeId {
        if let Some(id) = self.claim_disk_node(parent, name, path, true) {
            return id;
        }
        let id = self.insert_folder(parent, name, Some(path.to_path_buf()), false);
        self.identify_from_disk(id);
        id
    }

    /// Add a disk-backed file under `parent`. Its content is read lazily from
    /// `path` the first time the buffer is opened (`file_seed`).
    pub fn add_disk_file(&mut self, parent: NodeId, name: &str, path: &Path) -> NodeId {
        if let Some(id) = self.claim_disk_node(parent, name, path, false) {
            return id;
        }
        let id = self.insert_file(parent, name, "", Some(path.to_path_buf()));
        self.identify_from_disk(id);
        id
    }

    /// The identity of the entry `name` read from disk under `parent`: the
    /// same for the same place in the project on every launch and every
    /// device that opens it, so reopening a folder meets the records it
    /// left.
    fn disk_identity(&self, parent: Option<NodeId>, name: &str) -> String {
        use sha2::{Digest, Sha256};
        let path = match parent {
            Some(parent) => format!("{}/{name}", self.path_of(parent)),
            None => name.to_string(),
        };
        let digest = Sha256::digest(path.as_bytes());
        let mut bytes = [0u8; 16];
        for (byte, digested) in bytes.iter_mut().zip(digest.iter()) {
            *byte = *digested;
        }
        uuid::Builder::from_random_bytes(bytes)
            .into_uuid()
            .to_string()
    }

    /// Give the node `id`, just read from disk, its identity, and note it
    /// for the project to record.
    fn identify_from_disk(&mut self, id: NodeId) {
        let Some(node) = self.nodes.get(&id) else {
            return;
        };
        let identity = self.disk_identity(node.parent, &node.name.clone());
        if let Some(node) = self.nodes.get_mut(&id) {
            node.resource_id = identity;
        }
        self.read_from_disk.push(id);
    }

    /// A node the project's records already put at this place: it is the
    /// entry on disk, read now.
    fn claim_disk_node(
        &mut self,
        parent: NodeId,
        name: &str,
        path: &Path,
        folder: bool,
    ) -> Option<NodeId> {
        let identity = self.disk_identity(Some(parent), name);
        let id = self.node_for_resource(&identity)?;
        let node = self.nodes.get_mut(&id)?;
        if node.is_folder() != folder {
            return None;
        }
        node.disk_path = Some(path.to_path_buf());
        if let NodeKind::Folder { loaded, .. } = &mut node.kind {
            *loaded = false;
        }
        Some(id)
    }

    /// The nodes read from disk since the last call.
    pub fn take_read_from_disk(&mut self) -> Vec<NodeId> {
        std::mem::take(&mut self.read_from_disk)
    }

    fn insert_folder(
        &mut self,
        parent: NodeId,
        name: &str,
        disk_path: Option<PathBuf>,
        expanded: bool,
    ) -> NodeId {
        let depth = self.nodes.get(&parent).map_or(0, |n| n.depth + 1);
        let id = self.alloc();
        // A disk-backed folder starts unloaded (children read on expand); an
        // in-memory folder is fully known, so it is `loaded`.
        let loaded = disk_path.is_none();
        self.nodes.insert(
            id,
            TreeNode {
                id,
                // A resource id is shared metadata, never a local filesystem
                // address. The first project import persists this UUID in the
                // collaborative document; `disk_path` remains local provenance.
                resource_id: Uuid::new_v4().to_string(),
                name: name.to_string(),
                parent: Some(parent),
                kind: NodeKind::Folder {
                    expanded,
                    loaded,
                    children: Vec::new(),
                },
                depth,
                disk_path,
            },
        );
        self.attach(parent, id);
        id
    }

    fn insert_file(
        &mut self,
        parent: NodeId,
        name: &str,
        seed: &str,
        disk_path: Option<PathBuf>,
    ) -> NodeId {
        let depth = self.nodes.get(&parent).map_or(0, |n| n.depth + 1);
        let lang = Language::from_path(name);
        let id = self.alloc();
        self.nodes.insert(
            id,
            TreeNode {
                id,
                // Do not leak an absolute path into history/search/resource
                // identity. See the corresponding folder insertion above.
                resource_id: Uuid::new_v4().to_string(),
                name: name.to_string(),
                parent: Some(parent),
                kind: NodeKind::File {
                    language: lang,
                    seed: seed.to_string(),
                },
                depth,
                disk_path,
            },
        );
        self.attach(parent, id);
        id
    }

    fn attach(&mut self, parent: NodeId, child: NodeId) {
        if let Some(TreeNode {
            kind: NodeKind::Folder { children, .. },
            ..
        }) = self.nodes.get_mut(&parent)
        {
            children.push(child);
        }
    }

    pub fn get(&self, id: NodeId) -> Option<&TreeNode> {
        self.nodes.get(&id)
    }

    /// Toggle a folder's expanded flag. No-op on files.
    pub fn toggle_folder(&mut self, id: NodeId) {
        if let Some(TreeNode {
            kind: NodeKind::Folder { expanded, .. },
            ..
        }) = self.nodes.get_mut(&id)
        {
            *expanded = !*expanded;
        }
    }

    /// Set a folder's expanded flag. No-op on files.
    pub fn set_folder_expanded(&mut self, id: NodeId, open: bool) {
        if let Some(TreeNode {
            kind: NodeKind::Folder { expanded, .. },
            ..
        }) = self.nodes.get_mut(&id)
        {
            *expanded = open;
        }
    }

    // ---- real filesystem projection ---------------------------------------

    /// Build a project rooted at the real directory `path`, reading its top
    /// level immediately. Sub-folders are read lazily on first expansion via
    /// [`Project::load_children`], so opening a large tree is cheap.
    ///
    /// The root is canonicalised, so every path the editor derives from it —
    /// the language server's workspace root, a search hit, a save target — is
    /// absolute and symlink-resolved, and two ways of naming the same directory
    /// cannot open as two different projects.
    ///
    /// # Errors
    /// Returns an error if `path` does not exist, is not a directory, or its
    /// top level can't be read.
    pub fn from_dir(path: &Path) -> io::Result<Project> {
        let md = std::fs::metadata(path)?;
        if !md.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!("{} is not a directory", path.display()),
            ));
        }
        let path = crate::disk::canonical(path);
        let name = dir_display_name(&path);
        let mut p = Project::new(&name);
        let root = p.root;
        // Point the root at the real directory and mark it unloaded so
        // `load_children` reads it.
        p.identify_from_disk(root);
        if let Some(node) = p.nodes.get_mut(&root) {
            node.disk_path = Some(path);
            if let NodeKind::Folder {
                loaded, expanded, ..
            } = &mut node.kind
            {
                *loaded = false;
                *expanded = true;
            }
        }
        p.load_children(root)?;
        Ok(p)
    }

    /// The real directory this project is rooted at, if it came from disk.
    pub fn root_path(&self) -> Option<PathBuf> {
        self.nodes.get(&self.root).and_then(|n| n.disk_path.clone())
    }

    /// Whether this project projects a real directory (as opposed to the
    /// bundled demo tree or a set of scratch buffers).
    pub fn is_disk_backed(&self) -> bool {
        self.root_path().is_some()
    }

    /// Find the node for a real path, if the tree has read that far. Used to
    /// focus the file a search hit or a go-to-definition landed in.
    pub fn node_for_path(&self, path: &Path) -> Option<NodeId> {
        self.nodes
            .iter()
            .find(|(_, n)| n.disk_path.as_deref() == Some(path))
            .map(|(id, _)| *id)
    }

    /// Read every folder along `path` from the project root so the file at the
    /// end becomes a node the tree and the tabs can address, expanding the
    /// folders on the way. Returns the file's node id.
    ///
    /// This is what lets a project-wide search hit open its file: the hit names
    /// a path in a folder that has very likely never been expanded, so the
    /// lazily-loaded tree has no node for it yet.
    pub fn reveal_path(&mut self, path: &Path) -> Option<NodeId> {
        if let Some(id) = self.node_for_path(path) {
            return Some(id);
        }
        let root_path = self.root_path()?;
        let rel = path.strip_prefix(&root_path).ok()?;

        let mut current = self.root;
        let mut walked = root_path;
        let mut components = rel.components().peekable();
        while let Some(component) = components.next() {
            walked = walked.join(component);
            // Reading the parent is what materialises this component as a node.
            self.load_children(current).ok()?;
            let child = self.node_for_path(&walked)?;
            if components.peek().is_some() {
                // An intermediate component: a folder we must descend into.
                if let Some(TreeNode {
                    kind: NodeKind::Folder { expanded, .. },
                    ..
                }) = self.nodes.get_mut(&child)
                {
                    *expanded = true;
                }
                current = child;
            } else {
                return Some(child);
            }
        }
        None
    }

    /// Build a single-file project: a root folder named after the file's parent
    /// directory containing just `path`. Returns the project and the file's
    /// node id (ready to open into a tab). The file's bytes are read lazily
    /// when the buffer is first opened (see [`Project::file_seed`]).
    ///
    /// # Errors
    /// Returns an error if `path` does not exist or can't be read.
    pub fn open_single_file(path: &Path) -> io::Result<(Project, NodeId)> {
        // Surface an unreadable path up front (permission / not-found) rather
        // than silently opening an empty tab.
        let _ = std::fs::metadata(path)?;
        let file_name = path.file_name().map_or_else(
            || path.display().to_string(),
            |s| s.to_string_lossy().to_string(),
        );
        let parent_name = path
            .parent()
            .map_or_else(|| "/".to_string(), dir_display_name);
        let mut p = Project::new(&parent_name);
        let root = p.root;
        p.identify_from_disk(root);
        let node = p.add_disk_file(root, &file_name, path);
        Ok((p, node))
    }

    /// Read a disk-backed folder's children from the filesystem the first time
    /// it is expanded. Idempotent: a folder with no `disk_path`, or one already
    /// loaded, is left untouched.
    ///
    /// One directory level is read at a time, which is what keeps opening a
    /// repository the size of this one instant — the alternative, walking the
    /// whole tree up front, would read hundreds of thousands of entries before
    /// the first frame. Build output and VCS internals are skipped entirely
    /// (see `crate::disk::IGNORED_DIRS`): `target/` here holds more files
    /// than the rest of the repository by two orders of magnitude, and none of
    /// them is source anyone opens.
    ///
    /// # Errors
    /// Returns an error if the folder's directory listing can't be read.
    pub fn load_children(&mut self, id: NodeId) -> io::Result<()> {
        let (path, already_loaded) = match self.nodes.get(&id) {
            Some(TreeNode {
                disk_path: Some(p),
                kind: NodeKind::Folder { loaded, .. },
                ..
            }) => (p.clone(), *loaded),
            _ => return Ok(()),
        };
        if already_loaded {
            return Ok(());
        }
        // Collect and sort before inserting, so the arena order matches the
        // display order and a folder's children do not shuffle between reads.
        let mut folders: Vec<(String, PathBuf)> = Vec::new();
        let mut files: Vec<(String, PathBuf)> = Vec::new();
        for entry in std::fs::read_dir(&path)? {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name().to_string_lossy().to_string();
            let child_path = entry.path();
            // Skipped by *name*, not by type: in a git worktree `.git` is a
            // file holding a gitdir pointer, not a directory, and a
            // directory-only test lets it through as an openable source file.
            if crate::disk::is_ignored_dir(&name) {
                continue;
            }
            let Ok(ft) = entry.file_type() else { continue };
            // `file_type` does not follow symlinks, so a link pointing back up
            // the tree cannot make the walk cycle.
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                folders.push((name, child_path));
            } else if ft.is_file() {
                files.push((name, child_path));
            }
        }
        folders.sort_by_key(|a| a.0.to_lowercase());
        files.sort_by_key(|a| a.0.to_lowercase());
        for (name, p) in folders {
            self.add_disk_folder(id, &name, &p);
        }
        for (name, p) in files {
            self.add_disk_file(id, &name, &p);
        }

        if let Some(TreeNode {
            kind: NodeKind::Folder { loaded, .. },
            ..
        }) = self.nodes.get_mut(&id)
        {
            *loaded = true;
        }
        Ok(())
    }

    /// The bytes to seed a file buffer with: the in-memory seed if present,
    /// otherwise the real file read from disk.
    ///
    /// The disk read is fallible and its failure is *returned*, not swallowed.
    /// An unreadable file that opened as a silently-empty buffer would be saved
    /// back as an empty file the moment the user pressed Ctrl+S — the read
    /// error has to reach them before that.
    ///
    /// # Errors
    /// Returns an error if the file has no in-memory seed and can't be read
    /// from disk.
    pub fn file_seed(
        &self,
        id: NodeId,
    ) -> Result<(String, Option<crate::disk::Stamp>), crate::disk::LoadError> {
        match self.nodes.get(&id) {
            Some(TreeNode {
                kind: NodeKind::File { seed, .. },
                disk_path,
                ..
            }) => match disk_path {
                Some(path) => {
                    let loaded = crate::disk::load(path)?;
                    Ok((loaded.text, Some(loaded.stamp)))
                }
                None => Ok((seed.clone(), None)),
            },
            _ => Ok((String::new(), None)),
        }
    }

    /// Point a file node at a real path (used by Save As), updating its name
    /// and inferred language to match. No-op on folders / missing nodes.
    pub fn set_file_path(&mut self, id: NodeId, path: &Path, name: &str) {
        if let Some(node) = self.nodes.get_mut(&id)
            && let NodeKind::File { language, .. } = &mut node.kind
        {
            *language = Language::from_path(name);
            node.name = name.to_string();
            node.disk_path = Some(path.to_path_buf());
        }
    }

    /// The real filesystem path a node projects, if any.
    pub fn disk_path_of(&self, id: NodeId) -> Option<PathBuf> {
        self.nodes.get(&id).and_then(|n| n.disk_path.clone())
    }

    /// Flatten the visible tree into an ordered list, honouring folder
    /// expansion. Folders sort before files, then alphabetically — the
    /// conventional file-explorer ordering.
    pub fn visible_rows(&self) -> Vec<NodeId> {
        let mut out = Vec::new();
        self.push_children(self.root, &mut out);
        out
    }

    fn push_children(&self, parent: NodeId, out: &mut Vec<NodeId>) {
        let Some(node) = self.nodes.get(&parent) else {
            return;
        };
        let NodeKind::Folder {
            expanded, children, ..
        } = &node.kind
        else {
            return;
        };
        // Root's own children are the top-level rows; the root itself isn't
        // rendered as a row (its name shows in the sidebar header instead).
        let mut kids: Vec<NodeId> = children.clone();
        kids.sort_by(|a, b| {
            let (Some(na), Some(nb)) = (self.nodes.get(a), self.nodes.get(b)) else {
                return std::cmp::Ordering::Equal;
            };
            match (na.is_folder(), nb.is_folder()) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => na.name.to_lowercase().cmp(&nb.name.to_lowercase()),
            }
        });
        for kid in kids {
            out.push(kid);
            if *expanded
                && let Some(TreeNode {
                    kind:
                        NodeKind::Folder {
                            expanded: child_expanded,
                            ..
                        },
                    ..
                }) = self.nodes.get(&kid)
                && *child_expanded
            {
                self.push_children(kid, out);
            }
        }
    }

    /// The breadcrumb path of a node from the project root, e.g.
    /// `myproj / src / main.rs`.
    pub fn path_of(&self, id: NodeId) -> String {
        let mut chain = Vec::new();
        let mut cur = Some(id);
        while let Some(c) = cur {
            let Some(node) = self.nodes.get(&c) else {
                break;
            };
            chain.push(node.name.clone());
            cur = node.parent;
        }
        chain.reverse();
        chain.join("/")
    }
}

/// A human-friendly name for a directory path — its last component, or the
/// whole path for a root like `/`. Used to label a project opened from disk.
fn dir_display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |s| s.to_string_lossy().to_string(),
    )
}

/// The editor buffer: the file this window shows. The actual
/// text lives in the CRDT, keyed by `node`; this struct holds the per-buffer
/// caret/selection and the cached language.
#[derive(Clone, Debug)]
pub struct OpenBuffer {
    pub node: NodeId,
    pub name: String,
    pub language: Language,
    /// Whether the buffer has unsaved edits relative to its on-disk seed.
    pub dirty: bool,
    /// The real file this buffer writes back to, if it has one. A scratch
    /// buffer has none until Save As gives it one.
    pub path: Option<PathBuf>,
    /// The state the file was in when we last agreed with it — set on open and
    /// on every successful save. Save re-stats against this to refuse
    /// clobbering someone else's write; see `crate::disk::save`.
    pub stamp: Option<crate::disk::Stamp>,
    /// The text as last read from or written to disk. Dirty is
    /// `buffer_text != saved_text` — a real comparison rather than a flag that
    /// an undo back to the original would leave stuck on.
    pub saved_text: String,
    /// Set once the file is known to have changed on disk since it was opened,
    /// so the tab can say so before the user tries to save over it.
    pub stale: bool,
}

impl OpenBuffer {
    /// Whether the buffer's current text differs from what is on disk.
    pub fn is_dirty(&self, current: &str) -> bool {
        current != self.saved_text
    }

    /// Adopt `text` as the on-disk truth (after a load, a save, or a reload).
    pub fn mark_saved(&mut self, text: String, stamp: Option<crate::disk::Stamp>) {
        self.saved_text = text;
        self.stamp = stamp;
        self.dirty = false;
        self.stale = false;
    }
}

/// The editor session: the project, and the one file this window shows.
///
/// A window edits one file. Another file side by side is another editor in a
/// tab of the desktop's window group, never a tab of this one.
#[derive(Clone, Debug, Default)]
pub struct Session {
    pub project: Project,
    pub buffer: Option<OpenBuffer>,
}

impl Session {
    pub fn new(project: Project) -> Self {
        Session {
            project,
            buffer: None,
        }
    }

    /// Show the file `node`, replacing the file shown before. Returns
    /// whether `node` is a file. The caller keeps unsaved work: replacing a
    /// modified buffer is its decision, after asking.
    pub fn open_file(&mut self, node: NodeId) -> bool {
        let Some(tn) = self.project.get(node) else {
            return false;
        };
        let NodeKind::File { language, .. } = tn.kind else {
            return false;
        };
        if self.buffer.as_ref().is_some_and(|b| b.node == node) {
            return true;
        }
        self.buffer = Some(OpenBuffer {
            node,
            name: tn.name.clone(),
            language,
            dirty: false,
            path: tn.disk_path.clone(),
            stamp: None,
            saved_text: String::new(),
            stale: false,
        });
        true
    }

    /// The shown buffer when it is `node`.
    pub fn buffer_of(&self, node: NodeId) -> Option<&OpenBuffer> {
        self.buffer.as_ref().filter(|b| b.node == node)
    }

    /// Mutable access to the shown buffer when it is `node`.
    pub fn buffer_of_mut(&mut self, node: NodeId) -> Option<&mut OpenBuffer> {
        self.buffer.as_mut().filter(|b| b.node == node)
    }

    /// Stop showing `node`, if it is the shown file.
    pub fn close_file(&mut self, node: NodeId) {
        if self.buffer.as_ref().is_some_and(|b| b.node == node) {
            self.buffer = None;
        }
    }

    pub fn active_buffer(&self) -> Option<&OpenBuffer> {
        self.buffer.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_from_extension() {
        assert_eq!(Language::from_path("main.rs"), Language::Rust);
        assert_eq!(Language::from_path("Cargo.toml"), Language::Toml);
        assert_eq!(Language::from_path("README.md"), Language::Markdown);
        assert_eq!(Language::from_path("Cargo.lock"), Language::Toml);
        assert_eq!(Language::from_path("mystery.xyz"), Language::PlainText);
    }

    #[test]
    fn hardware_source_and_header_paths_select_their_dialect() {
        for name in ["mac.v", "rtl/defines.vh", "RTL/MAC.V", "include/DEFINES.VH"] {
            assert_eq!(Language::from_path(name), Language::Verilog, "{name}");
        }
        for name in ["mac.sv", "rtl/types.svh", "RTL/MAC.SV", "include/TYPES.SVH"] {
            assert_eq!(Language::from_path(name), Language::SystemVerilog, "{name}");
        }
        for language in [Language::Verilog, Language::SystemVerilog] {
            assert_eq!(language.line_comment(), Some("//"));
        }
    }

    #[test]
    fn in_memory_tree_orders_expands_and_adopts_document_ids() {
        // Each phase builds its own project.
        tree_visible_rows_respect_expansion();
        folders_sort_before_files();
        adopt_file_tracks_a_document_buffer_and_advances_the_allocator();
        durable_tree_ids_project_across_different_local_arena_layouts();
    }

    fn tree_visible_rows_respect_expansion() {
        let mut p = Project::new("proj");
        let src = p.add_folder(p.root, "src");
        let _main = p.add_file(src, "main.rs", "fn main() {}");
        // Collapsed by default: src shows but not its child.
        let rows = p.visible_rows();
        assert_eq!(rows.len(), 1);
        p.toggle_folder(src);
        let rows = p.visible_rows();
        assert_eq!(rows.len(), 2);
    }

    fn folders_sort_before_files() {
        let mut p = Project::new("proj");
        let _z = p.add_file(p.root, "z.rs", "");
        let _a = p.add_folder(p.root, "alpha");
        let rows = p.visible_rows();
        // Folder "alpha" comes before file "z.rs".
        assert!(p.get(rows[0]).unwrap().is_folder());
    }

    fn adopt_file_tracks_a_document_buffer_and_advances_the_allocator() {
        let mut p = Project::new("proj");
        // A buffer id from the document that the in-memory tree does not know.
        let orphan = 7;
        assert!(p.get(orphan).is_none());
        p.adopt_file(orphan, "untitled-7.txt");
        // It is now a tracked file under the root, visible in the tree.
        assert!(!p.get(orphan).unwrap().is_folder());
        assert!(p.visible_rows().contains(&orphan));
        // The allocator will not reissue the adopted id.
        assert!(p.peek_next_id() > orphan);
        let next = p.add_file(p.root, "z.rs", "");
        assert_ne!(next, orphan);
        // Adopting the same id twice is a no-op.
        p.adopt_file(orphan, "different.txt");
        assert_eq!(p.get(orphan).unwrap().name, "untitled-7.txt");
    }

    #[test]
    fn a_session_shows_one_file_and_compares_dirty_state() {
        // Each phase builds its own session or buffer.
        opening_another_file_replaces_the_shown_one();
        dirty_is_a_comparison_with_disk_not_a_sticky_flag();
    }

    fn opening_another_file_replaces_the_shown_one() {
        let mut p = Project::new("proj");
        let a = p.add_file(p.root, "a.rs", "x");
        let b = p.add_file(p.root, "b.rs", "y");
        let folder = p.root;
        let mut s = Session::new(p);
        assert!(s.open_file(a));
        assert!(s.open_file(a), "showing the shown file again is a no-op");
        assert_eq!(s.active_buffer().unwrap().node, a);
        assert!(s.open_file(b));
        assert_eq!(s.active_buffer().unwrap().node, b, "one file per window");
        assert!(s.buffer_of(a).is_none());
        assert!(!s.open_file(folder), "a folder is not a file to show");
        assert_eq!(s.active_buffer().unwrap().node, b);
        s.close_file(a);
        assert!(
            s.active_buffer().is_some(),
            "closing another file changes nothing"
        );
        s.close_file(b);
        assert!(s.active_buffer().is_none());
    }

    // ---- real filesystem projection -----------------------------------

    /// A small on-disk project shaped like a real one: nested source, build
    /// output that must be invisible, and a VCS directory.
    fn disk_fixture() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        std::fs::create_dir_all(r.join("src/inner")).unwrap();
        std::fs::create_dir_all(r.join("target/debug")).unwrap();
        std::fs::create_dir_all(r.join(".git/objects")).unwrap();
        std::fs::write(r.join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(r.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(r.join("src/inner/deep.rs"), "// deep\n").unwrap();
        std::fs::write(r.join("target/debug/junk.rs"), "// build output\n").unwrap();
        d
    }

    fn names(p: &Project) -> Vec<String> {
        p.visible_rows()
            .into_iter()
            .map(|id| p.get(id).unwrap().name.clone())
            .collect()
    }

    #[test]
    fn disk_projection_is_lazy_and_reads_files_on_demand() {
        // Each phase builds a fresh on-disk fixture and projection.
        from_dir_projects_the_real_top_level_only();
        expanding_a_folder_reads_its_children_on_demand();
        load_children_is_idempotent();
        reveal_path_walks_a_never_expanded_tree_down_to_the_file();
        file_seed_reads_the_real_bytes_and_reports_a_stamp();
        an_unreadable_file_surfaces_its_error_instead_of_an_empty_buffer();
        the_sample_project_is_not_disk_backed_but_a_real_one_is();
    }

    fn from_dir_projects_the_real_top_level_only() {
        let d = disk_fixture();
        let p = Project::from_dir(d.path()).expect("open the fixture directory");

        // The top level is there…
        let top = names(&p);
        assert!(top.contains(&"src".to_string()));
        assert!(top.contains(&"Cargo.toml".to_string()));
        // …build output and VCS internals are not…
        assert!(
            !top.contains(&"target".to_string()),
            "target/ must be hidden"
        );
        assert!(!top.contains(&".git".to_string()), ".git/ must be hidden");
        // …and the nested file has NOT been read: the tree is lazy, which is
        // what makes opening a large repository instant.
        assert!(!top.contains(&"main.rs".to_string()));
        assert_eq!(p.nodes.len(), 3, "root + src + Cargo.toml, nothing deeper");
    }

    fn expanding_a_folder_reads_its_children_on_demand() {
        let d = disk_fixture();
        let mut p = Project::from_dir(d.path()).unwrap();
        let src = p.nodes.values().find(|n| n.name == "src").unwrap().id;

        p.load_children(src).expect("read src/");
        p.toggle_folder(src);
        let shown = names(&p);
        assert!(shown.contains(&"main.rs".to_string()));
        assert!(shown.contains(&"inner".to_string()));
        // Still lazy one level further down.
        assert!(!shown.contains(&"deep.rs".to_string()));
    }

    fn load_children_is_idempotent() {
        let d = disk_fixture();
        let mut p = Project::from_dir(d.path()).unwrap();
        let src = p.nodes.values().find(|n| n.name == "src").unwrap().id;
        p.load_children(src).unwrap();
        let after_first = p.nodes.len();
        p.load_children(src).unwrap();
        assert_eq!(
            p.nodes.len(),
            after_first,
            "a second read adds no duplicates"
        );
    }

    fn reveal_path_walks_a_never_expanded_tree_down_to_the_file() {
        // This is the search-hit path: a hit names a file several folders deep
        // that the lazy tree has no node for yet.
        let d = disk_fixture();
        let mut p = Project::from_dir(d.path()).unwrap();
        let target = crate::disk::canonical(d.path()).join("src/inner/deep.rs");
        assert!(p.node_for_path(&target).is_none(), "not yet read");

        let id = p.reveal_path(&target).expect("reveal the nested file");
        assert_eq!(p.get(id).unwrap().name, "deep.rs");
        assert_eq!(p.disk_path_of(id).as_deref(), Some(target.as_path()));
        // The folders along the way were expanded, so it is actually visible.
        assert!(names(&p).contains(&"deep.rs".to_string()));
        // And it is idempotent.
        assert_eq!(p.reveal_path(&target), Some(id));
    }

    fn file_seed_reads_the_real_bytes_and_reports_a_stamp() {
        let d = disk_fixture();
        let mut p = Project::from_dir(d.path()).unwrap();
        let main = p
            .reveal_path(&crate::disk::canonical(d.path()).join("src/main.rs"))
            .unwrap();
        let (text, stamp) = p.file_seed(main).expect("read the file");
        assert_eq!(text, "fn main() {}\n");
        assert_eq!(stamp.expect("a disk file has a stamp").len, 13);
    }

    fn an_unreadable_file_surfaces_its_error_instead_of_an_empty_buffer() {
        // Opening a binary as a silently-empty buffer would let the next Ctrl+S
        // truncate it, so the failure has to be returned.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.bin"), [0u8, 1, 2]).unwrap();
        let mut p = Project::from_dir(d.path()).unwrap();
        let node = p
            .reveal_path(&crate::disk::canonical(d.path()).join("a.bin"))
            .unwrap();
        assert!(matches!(
            p.file_seed(node),
            Err(crate::disk::LoadError::NotText)
        ));
    }

    #[test]
    fn opening_a_path_roots_single_files_and_rejects_missing_or_non_directories() {
        // Each phase uses its own fixture (or none).
        opening_a_single_file_gives_a_project_rooted_at_its_folder();
        opening_a_missing_path_fails_rather_than_opening_an_empty_project();
        a_file_is_not_a_project_directory();
    }

    fn opening_a_single_file_gives_a_project_rooted_at_its_folder() {
        let d = disk_fixture();
        let file = d.path().join("src/main.rs");
        let (p, node) = Project::open_single_file(&file).expect("open one file");
        assert_eq!(p.get(node).unwrap().name, "main.rs");
        assert_eq!(p.name, "src");
        assert_eq!(p.disk_path_of(node).as_deref(), Some(file.as_path()));
    }

    fn opening_a_missing_path_fails_rather_than_opening_an_empty_project() {
        assert!(Project::from_dir(Path::new("/nonexistent/tilo/project")).is_err());
        assert!(Project::open_single_file(Path::new("/nonexistent/tilo/a.rs")).is_err());
    }

    fn a_file_is_not_a_project_directory() {
        let d = disk_fixture();
        assert!(
            Project::from_dir(&d.path().join("Cargo.toml")).is_err(),
            "a file must not open as a project tree"
        );
    }

    fn the_sample_project_is_not_disk_backed_but_a_real_one_is() {
        assert!(!crate::sample_project::build().is_disk_backed());
        let d = disk_fixture();
        let p = Project::from_dir(d.path()).unwrap();
        assert!(p.is_disk_backed());
        assert_eq!(p.root_path(), Some(crate::disk::canonical(d.path())));
    }

    fn dirty_is_a_comparison_with_disk_not_a_sticky_flag() {
        let mut buf = OpenBuffer {
            node: 0,
            name: "a.rs".into(),
            language: Language::Rust,
            dirty: false,
            path: None,
            stamp: None,
            saved_text: "fn main() {}".into(),
            stale: false,
        };
        assert!(!buf.is_dirty("fn main() {}"));
        assert!(buf.is_dirty("fn main() { x }"));
        // Undoing back to the original is clean again — a flag would be stuck.
        assert!(!buf.is_dirty("fn main() {}"));

        buf.dirty = true;
        buf.stale = true;
        buf.mark_saved("new".into(), Some(crate::disk::Stamp::default()));
        assert!(!buf.dirty);
        assert!(!buf.stale);
        assert!(!buf.is_dirty("new"));
    }

    fn durable_tree_ids_project_across_different_local_arena_layouts() {
        let folder = "00000000-0000-4000-8000-000000000101";
        let file = "00000000-0000-4000-8000-000000000102";
        let mut first = Project::new("first");
        let mut second = Project::new("second");
        // Give just one replica unrelated local allocations.  Durable IDs must
        // still resolve the same shared folder/file after projection.
        let root = second.root;
        second.add_file(root, "local-only.txt", "");
        let first_folder = first.adopt_collaborative_folder(folder, None, "src");
        let first_file =
            first.adopt_collaborative_file(file, Some(folder), "main.rs", Language::Rust);
        let second_folder = second.adopt_collaborative_folder(folder, None, "src");
        let second_file =
            second.adopt_collaborative_file(file, Some(folder), "main.rs", Language::Rust);
        assert_ne!((first_folder, first_file), (second_folder, second_file));
        assert_eq!(first.node_for_resource(file), Some(first_file));
        assert_eq!(second.node_for_resource(file), Some(second_file));
        assert_eq!(first.path_of(first_file), "first/src/main.rs");
        assert_eq!(second.path_of(second_file), "second/src/main.rs");
    }
}
