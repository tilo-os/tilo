//! Source-oriented migration review: recognizable sources, selectable children,
//! truthful local/online/unknown accounting and nested exclusions.
//!
//! The public tree never carries a filesystem path, a person's folder name, a
//! credential or a source identity string. Paths live in a private side table
//! and are only turned into the root-relative exclusion set that the existing
//! Home producer and Windows inventory already enforce. Discovery reuses those
//! two producers; it is not a second walker, staging area or copy engine.
//! Real entry names are exposed separately to the authorized private view;
//! serialized reports and action payloads carry only opaque identities.

pub mod catalog;
#[cfg(target_os = "linux")]
mod home;
mod steam;
#[cfg(test)]
mod tests;
mod windows;

pub use catalog::{CloudProvider, KnownFolder, SaveLocation, SaveRoot, SteamApp, SteamCatalog};

use crate::migration_directory::PinnedDirectory;
use crate::setup::migration_store::NativePath;
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

/// Upper bounds that keep one review responsive and the exclusion set small.
const MAX_NODES: usize = 4096;
const MAX_CHILDREN: usize = 128;
const MAX_DEPTH: usize = 4;
const MAX_EXCLUSIONS: usize = 1024;

/// What kind of recognizable source or child a node describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeKind {
    Source,
    Folder,
    Files,
    Game,
    GameAssets,
    GameSaves,
    ProtonPrefix,
    SteamUserdata,
    Unavailable,
}

/// A label drawn from the closed known-folder vocabulary or from public Steam
/// catalog metadata. An unrecognized folder is `Folder`; its own name is never
/// published.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Label {
    KnownFolder(KnownFolder),
    Cloud(CloudProvider),
    Steam,
    SteamLibrary,
    SteamUserdata,
    ProtonPrefix,
    Game,
    GameAssets,
    GameSaves,
    Outlook,
    Folder,
    Files,
    Other,
}

/// A statement about the source that the person needs in order to decide.
/// None of these is derived from a path string.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "fact")]
pub enum Fact {
    /// Some entries are online-only placeholders and stay at the source.
    OnlineOnlyNotCopied,
    /// Some entries have no readable size; they are not counted as zero bytes.
    UnknownSize,
    /// The inventory could not examine part of the source.
    CoverageIncomplete,
    /// A referenced location needs its own exact path grant.
    NeedsIndependentGrant,
    /// Entries exist that this copy engine cannot transfer.
    NotCopyableAtSource,
    /// Copying online-only content would download it first. Not performed.
    DownloadBeforeCopy,
    /// No working destination integration exists for this provider.
    ProviderConnectionUnavailable,
    CloudProvider(CloudProvider),
    /// Installed game files can be reinstalled.
    GameAssets,
    /// Save state that cannot be regenerated.
    IrreplaceableSaves,
    /// No documented save location was found. Not the same as no saves.
    SaveLocationUnknown,
    ProtonPrefix,
    /// Saves for this game are documented outside this Steam folder.
    ExternalSaveLocation(SaveRoot),
    /// A Steam Cloud label does not prove uploaded or complete saves.
    SteamCloudUnverified,
    /// This root is already inventoried through another selected root.
    AlreadyIncluded {
        covered_by: String,
    },
    /// Content kept only on a provider's server cannot be seen from here.
    RemoteCoverageUnknown,
}

/// Local, online-only and unknown accounting. `unknown_size_files` is a count
/// of entries whose size could not be read: it is never folded into a byte
/// total, so "0 bytes" is never shown for content that merely could not be read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub local_files: u64,
    pub local_bytes: u64,
    pub online_only_files: u64,
    pub online_only_bytes: u64,
    pub unknown_size_files: u64,
}

impl Counts {
    pub(crate) fn add(&mut self, other: Self) {
        self.local_files = self.local_files.saturating_add(other.local_files);
        self.local_bytes = self.local_bytes.saturating_add(other.local_bytes);
        self.online_only_files = self
            .online_only_files
            .saturating_add(other.online_only_files);
        self.online_only_bytes = self
            .online_only_bytes
            .saturating_add(other.online_only_bytes);
        self.unknown_size_files = self
            .unknown_size_files
            .saturating_add(other.unknown_size_files);
    }

    pub fn is_empty(self) -> bool {
        self == Self::default()
    }
}

/// One selectable source or child. `counts` is the subtree total; `own` is the
/// part attributed to this node itself, used to total a selection without
/// double counting nested children.
#[derive(Clone, Debug, Serialize)]
pub struct Node {
    /// Opaque, stable identity bound to the granted root's namespace identity
    /// and the entry's relative name. Never a path and never an unkeyed digest
    /// of a private name.
    pub id: String,
    pub label: Label,
    /// Valve's public catalog title, when the source is a game.
    pub title: Option<String>,
    pub kind: NodeKind,
    /// False for covered roots and references that need another grant.
    pub selectable: bool,
    pub counts: Counts,
    pub facts: Vec<Fact>,
    pub children: Vec<Node>,
    /// The person's own name for this entry, for the authorized private view
    /// only. It is never part of a serialized report or action payload.
    #[serde(skip)]
    private_label: Option<String>,
    #[serde(skip)]
    own: Counts,
    /// Root index this node belongs to.
    #[serde(skip)]
    root: u16,
    /// Paths whose entries belong to this node itself, relative to `root`.
    #[serde(skip)]
    own_paths: Vec<PathBuf>,
    /// Prefixes that cover this node's whole subtree, for exclusions.
    #[serde(skip)]
    paths: Vec<PathRef>,
}

/// Where a node's data lives. Private: never serialized into a report.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct PathRef {
    root: u16,
    relative: PathBuf,
}

/// What a private root is. A Home review has one; a Windows review has one per
/// granted root, keyed by the identity the inventory already assigns.
#[derive(Clone, Debug)]
pub(crate) enum RootRef {
    Home,
    Windows { source_id: String },
}

/// The discovered, immutable source tree. Selection is applied separately so
/// toggling a source never re-runs discovery.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Tree {
    pub sources: Vec<Node>,
    pub totals: Counts,
    #[serde(skip)]
    roots: Vec<RootRef>,
}

/// A person's selection over a discovered tree.
#[derive(Clone, Debug)]
pub struct Selection {
    tree: Tree,
    excluded: BTreeSet<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SelectionState {
    Selected,
    Excluded,
    Mixed,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Select,
    Exclude,
}

/// Root-relative exclusions, the only form the copy producers accept.
/// `home` is relative to the granted Home; `windows` is keyed by the inventory's
/// own source identity, which is an observed identity rather than a path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Exclusions {
    home: Vec<PathBuf>,
    windows: BTreeMap<String, Vec<PathBuf>>,
    windows_whole: Vec<String>,
}

impl Exclusions {
    pub fn is_empty(&self) -> bool {
        self.home.is_empty() && self.windows.is_empty() && self.windows_whole.is_empty()
    }

    pub fn home(&self) -> &[PathBuf] {
        &self.home
    }

    pub fn windows(&self) -> &BTreeMap<String, Vec<PathBuf>> {
        &self.windows
    }

    pub fn windows_whole(&self) -> &[String] {
        &self.windows_whole
    }

    /// The Home exclusion set as the private plan carries it. Shape is checked
    /// again by the plan and by the producer that applies it.
    ///
    /// # Errors
    /// Refuses paths that cannot be represented as a native relative path.
    pub fn home_paths(&self) -> std::io::Result<Vec<NativePath>> {
        self.home
            .iter()
            .map(|path| Ok(NativePath::encode(path)))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.home
            .len()
            .saturating_add(self.windows.values().map(Vec::len).sum::<usize>())
            .saturating_add(self.windows_whole.len())
    }
}

impl Node {
    /// The real entry name for the authorized private view. Public reports use
    /// [`Self::label`] and [`Self::title`] instead.
    pub fn private_label(&self) -> Option<&str> {
        self.private_label.as_deref()
    }

    pub(super) fn with_private_label(mut self, label: Option<String>) -> Self {
        self.private_label = label;
        self
    }
}

impl Tree {
    pub fn selection(self) -> Selection {
        Selection {
            tree: self,
            excluded: BTreeSet::new(),
        }
    }

    pub fn node(&self, id: &str) -> Option<&Node> {
        find(&self.sources, id)
    }
}

fn find<'a>(nodes: &'a [Node], id: &str) -> Option<&'a Node> {
    for node in nodes {
        if node.id == id {
            return Some(node);
        }
        if let Some(found) = find(&node.children, id) {
            return Some(found);
        }
    }
    None
}

impl Selection {
    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    pub fn state(&self, id: &str) -> Option<SelectionState> {
        let node = self.tree.node(id)?;
        if !node.selectable {
            return Some(SelectionState::Unavailable);
        }
        if self.excluded_ancestor(id).is_some() {
            return Some(SelectionState::Excluded);
        }
        let prefix = format!("{id}/");
        if self.excluded.iter().any(|other| other.starts_with(&prefix)) {
            return Some(SelectionState::Mixed);
        }
        Some(SelectionState::Selected)
    }

    /// Select or exclude one node. Selecting a node inside an excluded ancestor
    /// keeps the rest of that ancestor excluded by excluding the sibling
    /// branches: a selection never broadens to data the person did not pick.
    ///
    /// # Errors
    /// Refuses unknown or unavailable nodes and unbounded exclusion sets.
    pub fn set(&mut self, id: &str, decision: Decision) -> Result<(), String> {
        let node = self.tree.node(id).ok_or("unknown source node")?.clone();
        if !node.selectable {
            return Err("source node cannot be selected".into());
        }
        match decision {
            Decision::Exclude => {
                let prefix = format!("{id}/");
                self.excluded
                    .retain(|other| other != id && !other.starts_with(&prefix));
                self.excluded.insert(id.to_string());
            }
            Decision::Select => {
                let chain = chain(id);
                for (depth, ancestor) in chain.iter().enumerate().rev() {
                    if !self.excluded.remove(ancestor) {
                        continue;
                    }
                    let Some(next) = chain.get(depth.saturating_add(1)) else {
                        continue;
                    };
                    let Some(ancestor_node) = self.tree.node(ancestor) else {
                        continue;
                    };
                    let siblings = ancestor_node
                        .children
                        .iter()
                        .filter(|child| child.id != *next && child.selectable)
                        .map(|child| child.id.clone())
                        .collect::<Vec<_>>();
                    self.excluded.extend(siblings);
                }
                let prefix = format!("{id}/");
                self.excluded
                    .retain(|other| other != id && !other.starts_with(&prefix));
            }
        }
        if self.excluded.len() > MAX_EXCLUSIONS {
            return Err("too many excluded source subtrees".into());
        }
        Ok(())
    }

    /// Carry the same person's decisions into a freshly discovered tree.
    ///
    /// Only an identity that still names the same granted root entry is
    /// replayed. A new or replaced root keeps no old path exclusion; when the
    /// whole root set was replaced and the old selection was not simply "all",
    /// every new source starts excluded rather than silently taking in data the
    /// person never reviewed. A new child under a surviving parent falls under
    /// that parent's existing decision, which is the explicit parent intent.
    pub fn adopt(&mut self, previous: &Selection) {
        self.excluded.clear();
        let previous_sources = previous
            .tree
            .sources
            .iter()
            .filter(|source| source.selectable)
            .collect::<Vec<_>>();
        let previous_all_selected = !previous_sources.is_empty()
            && previous_sources
                .iter()
                .all(|source| previous.state(&source.id) == Some(SelectionState::Selected));
        let previous_all_excluded = !previous_sources.is_empty()
            && previous_sources
                .iter()
                .all(|source| previous.state(&source.id) == Some(SelectionState::Excluded));
        let root_replaced = !self
            .tree
            .sources
            .iter()
            .any(|source| previous.tree.sources.iter().any(|old| old.id == source.id));

        // An explicit decision is meaningful only while its own identity is
        // present. Descendants of a replayed parent inherit it as before.
        for id in &previous.excluded {
            if self.tree.node(id).is_some_and(|node| node.selectable) {
                self.excluded.insert(id.clone());
            }
        }

        // A new top-level source follows an explicit "nothing selected" intent;
        // its identity is new, so this is a default, not a replayed path.
        if previous_all_excluded {
            for source in &self.tree.sources {
                if source.selectable && previous.tree.node(&source.id).is_none() {
                    self.excluded.insert(source.id.clone());
                }
            }
        }

        // A root that was replaced has no identity to replay. Preserve the old
        // "not everything was selected" intent conservatively for every new
        // source instead of letting a different folder inherit the default.
        if root_replaced && !previous_sources.is_empty() && !previous_all_selected {
            for source in &self.tree.sources {
                if source.selectable {
                    self.excluded.insert(source.id.clone());
                }
            }
        }

        if self.excluded.len() > MAX_EXCLUSIONS {
            self.excluded.clear();
            for source in &self.tree.sources {
                if source.selectable {
                    self.excluded.insert(source.id.clone());
                }
            }
        }
    }

    /// The one toggle-all. Selecting everything clears every exclusion;
    /// excluding everything excludes each selectable source.
    pub fn select_all(&mut self, selected: bool) {
        self.excluded.clear();
        if !selected {
            self.excluded.extend(
                self.tree
                    .sources
                    .iter()
                    .filter(|source| source.selectable)
                    .map(|source| source.id.clone()),
            );
        }
    }

    /// Totals of the selected part, counting each node's own entries once.
    pub fn counts(&self) -> Counts {
        let mut counts = Counts::default();
        collect_selected(&self.tree.sources, &self.excluded, &mut counts);
        counts
    }

    /// Turn the selection into the root-relative exclusions the existing copy
    /// producers enforce. Whole-source exclusions stay explicit decisions.
    pub fn exclusions(&self) -> Exclusions {
        let mut exclusions = Exclusions::default();
        for id in &self.excluded {
            let Some(node) = self.tree.node(id) else {
                continue;
            };
            if node.kind == NodeKind::Source {
                if let Some(RootRef::Windows { source_id }) =
                    self.tree.roots.get(usize::from(node.root))
                {
                    exclusions.windows_whole.push(source_id.clone());
                }
                continue;
            }
            for path in &node.paths {
                match self.tree.roots.get(usize::from(path.root)) {
                    Some(RootRef::Home) => {
                        if !exclusions.home.contains(&path.relative) {
                            exclusions.home.push(path.relative.clone());
                        }
                    }
                    Some(RootRef::Windows { source_id }) => {
                        let list = exclusions.windows.entry(source_id.clone()).or_default();
                        if !list.contains(&path.relative) {
                            list.push(path.relative.clone());
                        }
                    }
                    None => {}
                }
            }
        }
        exclusions.home.sort();
        exclusions.windows_whole.sort();
        exclusions
    }

    /// Public, path-free projection for a UI or an agent: the same tree with
    /// selection state, totals and unknown counts.
    pub fn report(&self) -> serde_json::Value {
        let sources = self
            .tree
            .sources
            .iter()
            .map(|source| self.report_node(source))
            .collect::<Vec<_>>();
        serde_json::json!({
            "sources": sources,
            "totals": self.tree.totals,
            "selected": self.counts(),
            "exclusions": self.exclusions().len(),
        })
    }

    fn report_node(&self, node: &Node) -> serde_json::Value {
        serde_json::json!({
            "id": node.id,
            "label": node.label,
            "title": node.title,
            "kind": node.kind,
            "selectable": node.selectable,
            "state": self.state(&node.id),
            "counts": node.counts,
            "facts": node.facts,
            "children": node
                .children
                .iter()
                .map(|child| self.report_node(child))
                .collect::<Vec<_>>(),
        })
    }

    fn excluded_ancestor(&self, id: &str) -> Option<&String> {
        self.excluded
            .iter()
            .find(|other| id == other.as_str() || id.starts_with(&format!("{other}/")))
    }
}

fn nested(one: &Path, other: &Path) -> bool {
    one.starts_with(other) || other.starts_with(one)
}

/// A private key for node identities. It is generated once per process and
/// never serialized, so a public report cannot be used to test whether a
/// guessed folder name produced an id. Selections are held in memory for the
/// same process, so this stays stable across a refresh.
fn identity_key() -> &'static [u8; 32] {
    static KEY: OnceLock<[u8; 32]> = OnceLock::new();
    KEY.get_or_init(|| {
        let mut key = [0u8; 32];
        let (first, second) = key.split_at_mut(16);
        first.copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        second.copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        key
    })
}

fn opaque_token(parts: &[&[u8]]) -> String {
    let mut hasher = blake3::Hasher::new_keyed(identity_key());
    for part in parts {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    hasher.finalize().to_hex().to_string()
}

/// The private identity of a granted root, from the namespace identity the
/// directory capability already holds.
pub(super) fn root_identity(pin: &PinnedDirectory) -> String {
    use cap_fs_ext::MetadataExt;
    format!("{}:{}", pin.identity.dev(), pin.identity.ino())
}

pub(super) fn root_node_id(root_identity: &str) -> String {
    format!("r{}", opaque_token(&[root_identity.as_bytes()]))
}

/// A child of a root or folder, bound to the relative entry and, where the
/// caller can safely observe it, that entry's own namespace identity.
pub(super) fn entry_node_id(
    parent_id: &str,
    relative: &Path,
    entry_identity: Option<&str>,
) -> String {
    let mut parts = vec![relative.as_os_str().as_encoded_bytes()];
    if let Some(identity) = entry_identity {
        parts.push(identity.as_bytes());
    }
    format!("{parent_id}/d{}", opaque_token(&parts))
}

/// A stable child whose identity is the parent plus a semantic role, not a
/// position among siblings.
pub(super) fn semantic_node_id(parent_id: &str, role: &str) -> String {
    format!("{parent_id}/s{role}")
}

pub(super) fn derived_node_id(parent_id: &str, role: &str, material: &[u8]) -> String {
    format!(
        "{parent_id}/s{role}{}",
        opaque_token(&[role.as_bytes(), material])
    )
}

/// Observe one reviewed directory through the held grant, without following a
/// link or opening its listing. Used only to bind an identity, not to walk.
pub(super) fn relative_directory_identity(
    pin: &Arc<PinnedDirectory>,
    relative: &Path,
) -> Option<String> {
    use crate::nofollow::metadata_nofollow;
    use cap_fs_ext::MetadataExt;
    let mut directory = Arc::clone(pin);
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return None;
        };
        let metadata = metadata_nofollow(&directory.file, Path::new(name)).ok()?;
        if !metadata.is_dir() || metadata.is_symlink() {
            return None;
        }
        directory = directory.open_child(name, &metadata).ok()?;
    }
    Some(format!(
        "{}:{}",
        directory.identity.dev(),
        directory.identity.ino()
    ))
}

fn chain(id: &str) -> Vec<String> {
    let mut chain = Vec::new();
    let mut current = String::new();
    for part in id.split('/') {
        if !current.is_empty() {
            current.push('/');
        }
        current.push_str(part);
        chain.push(current.clone());
    }
    chain
}

fn collect_selected(nodes: &[Node], excluded: &BTreeSet<String>, out: &mut Counts) {
    for node in nodes {
        if excluded.contains(&node.id) {
            continue;
        }
        out.add(node.own);
        collect_selected(&node.children, excluded, out);
    }
}

/// A leaf node that owns the entries under `own_paths`.
pub(super) fn leaf(
    id: String,
    label: Label,
    kind: NodeKind,
    root: u16,
    own_paths: Vec<PathBuf>,
    facts: Vec<Fact>,
) -> Node {
    Node {
        id,
        label,
        title: None,
        kind,
        selectable: true,
        counts: Counts::default(),
        facts,
        children: Vec::new(),
        private_label: None,
        own: Counts::default(),
        root,
        own_paths,
        paths: Vec::new(),
    }
}

/// A node whose data is its children's data.
pub(super) fn group(
    id: String,
    label: Label,
    kind: NodeKind,
    root: u16,
    children: Vec<Node>,
    facts: Vec<Fact>,
) -> Node {
    Node {
        id,
        label,
        title: None,
        kind,
        selectable: true,
        counts: Counts::default(),
        facts,
        children,
        private_label: None,
        own: Counts::default(),
        root,
        own_paths: Vec::new(),
        paths: Vec::new(),
    }
}

/// Attribute each reviewed entry to the most specific node that owns it, then
/// compute subtree totals and the prefix set each node's exclusion covers.
pub(super) fn finalize(
    mut sources: Vec<Node>,
    entries: &[steam::EntryCount],
    roots: Vec<RootRef>,
) -> Tree {
    let mut ownable = BTreeSet::new();
    for node in &*sources {
        collect_own_paths(node, &mut ownable);
    }
    let mut counts: BTreeMap<PathRef, Counts> = BTreeMap::new();
    for entry in entries {
        let mut owner = None;
        for ancestor in entry.path.ancestors() {
            let key = PathRef {
                root: entry.root,
                relative: ancestor.to_path_buf(),
            };
            if ownable.contains(&key) {
                owner = Some(key);
                break;
            }
        }
        if let Some(owner) = owner {
            counts.entry(owner).or_default().add(entry.counts);
        }
    }
    let mut totals = Counts::default();
    for node in &mut *sources {
        settle(node, &counts);
        totals.add(node.counts);
    }
    Tree {
        sources,
        totals,
        roots,
    }
}

fn collect_own_paths(node: &Node, out: &mut BTreeSet<PathRef>) {
    out.extend(node.own_paths.iter().map(|relative| PathRef {
        root: node.root,
        relative: relative.clone(),
    }));
    for child in &node.children {
        collect_own_paths(child, out);
    }
}

fn settle(node: &mut Node, counts: &BTreeMap<PathRef, Counts>) -> Counts {
    let mut total = Counts::default();
    for relative in &node.own_paths {
        let key = PathRef {
            root: node.root,
            relative: relative.clone(),
        };
        if let Some(own) = counts.get(&key) {
            total.add(*own);
        }
    }
    node.own = total;
    for child in &mut node.children {
        let nested = settle(child, counts);
        total.add(nested);
    }
    node.counts = total;
    // The exclusion set must cover the subtree minimally: a folder's own path
    // already covers everything beneath it, so its children add nothing.
    let mut paths = Vec::new();
    for relative in &node.own_paths {
        let path = PathRef {
            root: node.root,
            relative: relative.clone(),
        };
        if !paths
            .iter()
            .any(|existing: &PathRef| path.relative.starts_with(&existing.relative))
        {
            paths.push(path);
        }
    }
    for child in &node.children {
        for path in &child.paths {
            if paths.iter().any(|existing| {
                existing.root == path.root && nested(&existing.relative, &path.relative)
            }) {
                continue;
            }
            paths.push(path.clone());
        }
    }
    node.paths = paths;
    total
}

/// Shared inputs for a folder subtree of one granted root.
pub(crate) struct Folders<'a> {
    pub root: u16,
    pub entries: &'a [steam::EntryCount],
    /// Reviewed directories, relative to the root.
    pub dirs: &'a BTreeSet<PathBuf>,
    /// Directories represented by a structured source instead (e.g. Steam).
    pub skip: &'a [PathBuf],
    pub label: &'a dyn Fn(&Path) -> Label,
    /// The entry's own namespace identity, when the caller can observe it.
    pub identity: &'a dyn Fn(&Path) -> Option<String>,
}

/// Build the selectable children of one reviewed folder. Depth and fan-out are
/// bounded; content below the bound stays owned by the deepest node, so totals
/// remain complete even when the tree stops offering finer selection.
pub(super) fn folder_children(
    parent_id: &str,
    parent: &Path,
    depth: usize,
    folders: &Folders<'_>,
    facts: &mut Vec<Fact>,
    budget: &mut usize,
) -> Vec<Node> {
    let mut children = Vec::new();
    if depth >= MAX_DEPTH || *budget == 0 {
        return children;
    }
    let mut dirs = folders
        .dirs
        .iter()
        .filter(|path| path.parent() == Some(parent))
        .filter(|path| !folders.skip.iter().any(|skip| path.starts_with(skip)))
        .cloned()
        .collect::<Vec<_>>();
    dirs.sort();
    if dirs.len() > MAX_CHILDREN {
        facts.push(Fact::CoverageIncomplete);
        dirs.truncate(MAX_CHILDREN.saturating_sub(1));
    }
    let mut files = folders
        .entries
        .iter()
        .filter(|entry| entry.root == folders.root && entry.path.parent() == Some(parent))
        .filter(|entry| !folders.dirs.contains(&entry.path))
        .map(|entry| entry.path.clone())
        .collect::<Vec<_>>();
    files.sort();
    files.dedup();
    if !files.is_empty() {
        if files.len() <= 256 {
            children.push(leaf(
                semantic_node_id(parent_id, "files"),
                Label::Files,
                NodeKind::Files,
                folders.root,
                files,
                Vec::new(),
            ));
        } else {
            // Too many loose files to exclude one by one: the folder itself
            // stays the selectable unit and the bound is disclosed.
            facts.push(Fact::CoverageIncomplete);
        }
    }
    for directory in &dirs {
        if *budget == 0 {
            facts.push(Fact::CoverageIncomplete);
            break;
        }
        *budget = budget.saturating_sub(1);
        let identity = (folders.identity)(directory);
        let id = entry_node_id(parent_id, directory, identity.as_deref());
        let mut nested_facts = Vec::new();
        let nested = folder_children(
            &id,
            directory,
            depth.saturating_add(1),
            folders,
            &mut nested_facts,
            budget,
        );
        let label = (folders.label)(directory);
        let node = Node {
            id,
            label,
            title: None,
            kind: NodeKind::Folder,
            selectable: true,
            counts: Counts::default(),
            facts: nested_facts,
            children: nested,
            private_label: directory
                .file_name()
                .map(|name| name.to_string_lossy().into_owned()),
            own: Counts::default(),
            root: folders.root,
            own_paths: vec![directory.clone()],
            paths: Vec::new(),
        };
        children.push(node);
    }
    children
}

/// Build the tree for a reviewed Windows inventory.
pub fn discover_windows(
    inventory: &crate::windows_inventory::Inventory,
    catalog: &SteamCatalog,
) -> Tree {
    windows::build(inventory, *catalog)
}

/// Decode and shape-check root-relative exclusion paths. They are consent
/// metadata, never a path grant: the producer still revalidates each path under
/// its own pinned source before skipping anything.
///
/// # Errors
/// Refuses too many paths, unrepresentable bytes and non-relative components.
pub fn exclusion_paths(values: &[NativePath]) -> std::io::Result<Vec<PathBuf>> {
    if values.len() > MAX_EXCLUSIONS {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "too many excluded source subtrees",
        ));
    }
    let mut paths = Vec::new();
    for value in values {
        let path = value.decode().map_err(std::io::Error::other)?;
        if path.as_os_str().is_empty()
            || path.as_os_str().len() > 4096
            || path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "excluded source subtree is not a normalized relative path",
            ));
        }
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    Ok(paths)
}

/// Validate that each exclusion resolves under `root` without following a link.
/// A vanished or replaced exclusion refuses the operation instead of silently
/// including data the person excluded.
///
/// # Errors
/// Refuses missing, linked, replaced or non-file-and-folder exclusion paths.
pub fn validate_exclusions_under(root: &std::fs::File, paths: &[PathBuf]) -> std::io::Result<()> {
    use crate::nofollow::{metadata_nofollow, open_directory_nofollow};
    for path in paths {
        let mut directory = root.try_clone()?;
        let count = path.components().count();
        for (index, component) in path.components().enumerate() {
            let std::path::Component::Normal(name) = component else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "excluded source subtree is not a normalized relative path",
                ));
            };
            let metadata = metadata_nofollow(&directory, std::path::Path::new(name))?;
            if metadata.is_symlink() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "excluded source subtree is a link",
                ));
            }
            if index.saturating_add(1) == count {
                if !metadata.is_dir() && !metadata.is_file() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "excluded source subtree is not a file or folder",
                    ));
                }
            } else {
                if !metadata.is_dir() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "excluded source subtree ancestor is not a folder",
                    ));
                }
                directory =
                    open_directory_nofollow(&directory, std::path::Path::new(name), &metadata)?;
            }
        }
    }
    Ok(())
}

/// Discover the sources of an explicitly granted Home without creating a
/// transfer, a permission or a private staging area. The walk is the canonical
/// Home inventory, so the tree describes exactly what a later export would see.
/// A host that cannot read a Home inventory has no Home sources to offer, so it
/// refuses here instead of making every caller decide by target.
///
/// # Errors
/// Refuses unsafe grants, unsupported mounts and changed or unreadable sources.
pub fn discover_home(home: &std::path::Path, catalog: &SteamCatalog) -> std::io::Result<Tree> {
    #[cfg(target_os = "linux")]
    {
        let pin = crate::setup::migration::PinnedDirectory::open_absolute(home)?;
        pin.validate()?;
        let inventory = crate::migration_review::SourceInventory::capture_home(&pin)?;
        home::build(&pin, &inventory, *catalog)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (home, catalog);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Home sources require Linux",
        ))
    }
}
