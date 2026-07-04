//! Pure data model for the code editor: the project file tree, the set of
//! open buffers, and language detection. No rendering, no IO, no CRDT — this
//! layer is the in-memory source of truth the widgets read from.
//!
//! Buffer *text* lives in the shared Automerge document (so edits are
//! collaborative and undoable); this model only tracks the structural facts
//! around it — which files exist, which are open, which tab is active, and
//! where each caret sits.

use std::collections::BTreeMap;

/// Stable identifier for a node in the project tree (file or folder).
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
            Language::PlainText => "Plain Text",
        }
    }

    /// The token syntect uses to locate a grammar by file extension. Picked
    /// so that `SyntaxSet::find_syntax_by_extension` resolves to the right
    /// Sublime grammar in the bundled default set.
    pub fn syntect_extension(self) -> &'static str {
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
            _ => match name {
                "Cargo.lock" => Language::Toml,
                "Makefile" => Language::Shell,
                "Dockerfile" | "Containerfile" => Language::Shell,
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
}

/// One node in the project tree. Folders own an ordered list of children;
/// files carry the language and the seed body used to populate the buffer
/// when first opened.
#[derive(Clone, Debug)]
pub struct TreeNode {
    pub id: NodeId,
    pub name: String,
    pub parent: Option<NodeId>,
    pub kind: NodeKind,
    /// Depth from the project root (root children are depth 0). Cached so the
    /// flattened render list doesn't have to walk parents every frame.
    pub depth: usize,
}

#[derive(Clone, Debug)]
pub enum NodeKind {
    Folder { expanded: bool, children: Vec<NodeId> },
    File { language: Language, seed: String },
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
}

impl Project {
    pub fn new(name: &str) -> Self {
        let mut p = Project {
            name: name.to_string(),
            nodes: BTreeMap::new(),
            root: 0,
            next_id: 0,
        };
        let root = p.alloc();
        p.nodes.insert(
            root,
            TreeNode {
                id: root,
                name: name.to_string(),
                parent: None,
                kind: NodeKind::Folder {
                    expanded: true,
                    children: Vec::new(),
                },
                depth: 0,
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

    /// Add a folder under `parent`, returning its id.
    pub fn add_folder(&mut self, parent: NodeId, name: &str) -> NodeId {
        let depth = self.nodes.get(&parent).map(|n| n.depth + 1).unwrap_or(0);
        let id = self.alloc();
        self.nodes.insert(
            id,
            TreeNode {
                id,
                name: name.to_string(),
                parent: Some(parent),
                kind: NodeKind::Folder {
                    expanded: false,
                    children: Vec::new(),
                },
                depth,
            },
        );
        self.attach(parent, id);
        id
    }

    /// Add a file under `parent` with a seed body, inferring the language.
    pub fn add_file(&mut self, parent: NodeId, name: &str, seed: &str) -> NodeId {
        let depth = self.nodes.get(&parent).map(|n| n.depth + 1).unwrap_or(0);
        let lang = Language::from_path(name);
        let id = self.alloc();
        self.nodes.insert(
            id,
            TreeNode {
                id,
                name: name.to_string(),
                parent: Some(parent),
                kind: NodeKind::File {
                    language: lang,
                    seed: seed.to_string(),
                },
                depth,
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
        let NodeKind::Folder { expanded, children } = &node.kind else {
            return;
        };
        // Root's own children are the top-level rows; the root itself isn't
        // rendered as a row (its name shows in the sidebar header instead).
        let mut kids: Vec<NodeId> = children.clone();
        kids.sort_by(|a, b| {
            let na = &self.nodes[a];
            let nb = &self.nodes[b];
            match (na.is_folder(), nb.is_folder()) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => na.name.to_lowercase().cmp(&nb.name.to_lowercase()),
            }
        });
        for kid in kids {
            out.push(kid);
            if *expanded {
                if let Some(TreeNode {
                    kind: NodeKind::Folder { expanded: child_expanded, .. },
                    ..
                }) = self.nodes.get(&kid)
                {
                    if *child_expanded {
                        self.push_children(kid, out);
                    }
                }
            }
        }
    }

    /// The breadcrumb path of a node from the project root, e.g.
    /// `myproj / src / main.rs`.
    pub fn path_of(&self, id: NodeId) -> String {
        let mut chain = Vec::new();
        let mut cur = Some(id);
        while let Some(c) = cur {
            let Some(node) = self.nodes.get(&c) else { break };
            chain.push(node.name.clone());
            cur = node.parent;
        }
        chain.reverse();
        chain.join("/")
    }
}

/// An open editor buffer: a file the user has opened into a tab. The actual
/// text lives in the CRDT, keyed by `node`; this struct holds the per-buffer
/// caret/selection and the cached language.
#[derive(Clone, Debug)]
pub struct OpenBuffer {
    pub node: NodeId,
    pub name: String,
    pub language: Language,
    /// Whether the buffer has unsaved edits relative to its on-disk seed.
    pub dirty: bool,
}

/// The full editor session: the project, the ordered list of open buffers,
/// and which one is active.
#[derive(Clone, Debug, Default)]
pub struct Session {
    pub project: Project,
    pub open: Vec<OpenBuffer>,
    pub active: Option<usize>,
}

impl Session {
    pub fn new(project: Project) -> Self {
        Session {
            project,
            open: Vec::new(),
            active: None,
        }
    }

    /// Open the file `node` in a tab, or focus the existing tab if already
    /// open. Returns the index of the (possibly newly-created) tab.
    pub fn open_file(&mut self, node: NodeId) -> Option<usize> {
        let tn = self.project.get(node)?;
        let NodeKind::File { language, .. } = tn.kind else {
            return None;
        };
        if let Some(idx) = self.open.iter().position(|b| b.node == node) {
            self.active = Some(idx);
            return Some(idx);
        }
        let buf = OpenBuffer {
            node,
            name: tn.name.clone(),
            language,
            dirty: false,
        };
        self.open.push(buf);
        let idx = self.open.len() - 1;
        self.active = Some(idx);
        Some(idx)
    }

    /// Close the tab at `idx`, adjusting the active index so it stays valid.
    pub fn close_tab(&mut self, idx: usize) {
        if idx >= self.open.len() {
            return;
        }
        self.open.remove(idx);
        if self.open.is_empty() {
            self.active = None;
        } else {
            let new_active = match self.active {
                Some(a) if a > idx => a - 1,
                Some(a) if a == idx => idx.min(self.open.len() - 1),
                other => other.unwrap_or(0),
            };
            self.active = Some(new_active.min(self.open.len() - 1));
        }
    }

    pub fn active_buffer(&self) -> Option<&OpenBuffer> {
        self.active.and_then(|i| self.open.get(i))
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

    #[test]
    fn folders_sort_before_files() {
        let mut p = Project::new("proj");
        let _z = p.add_file(p.root, "z.rs", "");
        let _a = p.add_folder(p.root, "alpha");
        let rows = p.visible_rows();
        // Folder "alpha" comes before file "z.rs".
        assert!(p.get(rows[0]).unwrap().is_folder());
    }

    #[test]
    fn open_file_is_idempotent() {
        let mut p = Project::new("proj");
        let f = p.add_file(p.root, "a.rs", "x");
        let mut s = Session::new(p);
        let i1 = s.open_file(f).unwrap();
        let i2 = s.open_file(f).unwrap();
        assert_eq!(i1, i2);
        assert_eq!(s.open.len(), 1);
    }

    #[test]
    fn close_tab_adjusts_active() {
        let mut p = Project::new("proj");
        let a = p.add_file(p.root, "a.rs", "");
        let b = p.add_file(p.root, "b.rs", "");
        let c = p.add_file(p.root, "c.rs", "");
        let mut s = Session::new(p);
        s.open_file(a);
        s.open_file(b);
        s.open_file(c);
        s.active = Some(2);
        s.close_tab(0);
        // Active shifts down by one to still point at "c".
        assert_eq!(s.active, Some(1));
        assert_eq!(s.open.len(), 2);
    }
}
