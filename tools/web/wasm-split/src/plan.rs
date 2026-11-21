//! The plan a split is made by, kept under what it was made from, so a run
//! whose change leaves that plan as it was takes it from here instead of
//! reading the module's relocations and walking the call graph again
//! (`WASM_SPLIT_CACHE/plans`).
//!
//! A plan is everything a split decides before it writes a module: what the
//! start module reaches, what each split module reaches, which shared code
//! forms a chunk, which symbols are shared, and how large each data symbol
//! is. It is made from little:
//!
//! - the bound module's sections other than its code and data — the
//!   functions, their names, the split points, the table's slots — and its
//!   two function counts;
//! - the linked module's sections other than its code, its static data and
//!   its relocations: its symbols say where each function and data symbol is
//!   and how large it is, while the bytes a static holds are read when a
//!   module that carries them is emitted. A relocation entry's offset moves
//!   with the code it sits in, the edge it holds does not;
//! - every body's answers: the functions it calls and the addresses it holds
//!   ([`crate::analysis`]).
//!
//! A run that reads the same of all three decides the same plan. What each
//! module then holds is still settled against that module's own key
//! ([`crate::cache`]), so a body whose code changed is emitted again and the
//! rest is read back: the plan says what to emit, never what it says.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::analysis::Body;
use crate::cache::Key;
use crate::split::{hash_sections, read_leb};

/// Raised whenever what a plan is made from, or what it holds, changes, so a
/// directory an older splitter filled is never read as this one's.
const MAGIC: &[u8; 8] = b"WSSPLN\0\x04";

/// The code section's id; its bytes move with a change and are no part of a
/// plan.
const CODE_SECTION: u8 = 10;

/// The data sections' last id (the data count): a plan holds the size of each
/// data symbol, never the bytes it writes.
const DATA_COUNT_SECTION: u8 = 12;

/// How many plans the directory keeps: a module alternates between a few
/// states (a change and its revert, two branches).
const KEEP: usize = 4;

/// A symbol as a plan holds it: a function by its index in the bound module,
/// or a data symbol by its own.
///
/// The two index spaces overlap, so the lowest bit says which one it is.
pub type Node = u64;

pub fn function_node(index: usize) -> Node {
    (index as Node) << 1
}

pub fn data_node(index: usize) -> Node {
    ((index as Node) << 1) | 1
}

/// A node's index, and whether it is a data symbol's.
pub fn node_index(node: Node) -> (usize, bool) {
    ((node >> 1) as usize, node & 1 == 1)
}

/// What a split decided, in the module's own index spaces (see the module
/// comment).
#[derive(Default)]
pub struct Plan {
    /// What the start module reaches.
    pub main_graph: HashSet<Node>,
    /// What each split module reaches, in `Splitter::split_points`' order.
    pub reachable: Vec<HashSet<Node>>,
    /// The functions of each shared chunk.
    pub chunks: Vec<HashSet<Node>>,
    /// What each chunk's functions call outside it, by chunk index.
    pub chunk_imports: Vec<HashSet<Node>>,
    /// The symbols a split module and the start module both reach, sorted.
    pub shared_symbols: BTreeSet<Node>,
    /// Each defined data symbol's size, by its index.
    pub data_sizes: HashMap<usize, usize>,
    /// The start module's symbols that nothing it keeps reaches.
    pub unused: HashSet<Node>,
    /// Every function and data symbol's outgoing edges: what a plan read
    /// back through a renumbering is moved by without reading the relocations
    /// again (split.rs's `read_plan_back`).
    pub graph: HashMap<Node, HashSet<Node>>,
    /// Each data symbol's name, by its index: the identity a function added
    /// or removed leaves as it was while the index moves.
    pub data_names: Vec<(usize, String)>,
}

/// What a split's plan was made from (see the module comment): `structure` is
/// the bound module's (analysis.rs), `linked` the module the split reads its
/// symbols and static data from, `bodies` every body's answers, in index
/// order.
pub fn key(structure: &str, linked: &[u8], bodies: &[Body]) -> Result<String> {
    let mut key = Key::new();
    key.add(&MAGIC).add(structure).add(&bodies.len());
    sections(&mut key, linked)?;
    for body in bodies {
        key.add(&body.callees).add(&body.values);
    }
    Ok(key.finish())
}

/// `key` over every section of `linked` that a plan reads: all but the code,
/// the static data and the relocations.
fn sections(key: &mut Key, linked: &[u8]) -> Result<()> {
    // What a plan does not read, as ranges of the module: the code, the
    // static data (a symbol's size is the plan's, from the symbols, and its
    // bytes are read when a module is emitted) and the relocations
    // (`split::hash_sections` also leaves out the local variables' names,
    // which reach no module's bytes).
    let mut skipped = Vec::new();
    let mut at = 8;
    while at < linked.len() {
        let start = at;
        let id = linked[at];
        at += 1;
        let size = read_leb(linked, &mut at)?;
        let contents = at;
        at = (at + size).min(linked.len());
        let not_the_plans = (CODE_SECTION..=DATA_COUNT_SECTION).contains(&id)
            || (id == 0
                && custom_name(linked, contents)?.is_some_and(|name| name.starts_with("reloc.")));
        if not_the_plans {
            skipped.push(start..at);
        }
    }
    hash_sections(key, linked, &skipped)
}

/// A custom section's name, which its contents hold first.
fn custom_name(linked: &[u8], contents: usize) -> Result<Option<&str>> {
    let mut at = contents;
    let len = read_leb(linked, &mut at)?;
    let Some(name) = at.checked_add(len).and_then(|end| linked.get(at..end)) else {
        return Ok(None);
    };
    Ok(std::str::from_utf8(name).ok())
}

/// A directory of plans, one file per key.
pub struct PlanCache {
    dir: PathBuf,
    /// How many plans this cache read back: a split that took one says so.
    taken: std::sync::atomic::AtomicUsize,
    /// How many plans were moved to another module's indices.
    moved: std::sync::atomic::AtomicUsize,
}

impl PlanCache {
    /// The plans kept beside the emitted modules, in the directory
    /// `WASM_SPLIT_CACHE` names, if it names one (empty names none).
    pub fn from_env() -> Option<Self> {
        std::env::var_os("WASM_SPLIT_CACHE")
            .filter(|dir| !dir.is_empty())
            .map(|dir| Self::at(Path::new(&dir)))
    }

    pub fn at(cache: &Path) -> Self {
        Self {
            dir: cache.join("plans"),
            taken: std::sync::atomic::AtomicUsize::new(0),
            moved: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.plan"))
    }

    /// How many plans this cache read back.
    #[cfg(test)]
    pub fn taken(&self) -> usize {
        self.taken.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The plan an earlier split made from `key`, if this cache holds it.
    /// Anything unreadable is no plan: the caller makes it again.
    pub fn load(&self, key: &str) -> Option<Plan> {
        let plan = self.read(key)?;
        self.taken
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(plan)
    }

    fn read(&self, key: &str) -> Option<Plan> {
        let bytes = std::fs::read(self.path(key)).ok()?;
        let mut read = Reader {
            bytes: &bytes,
            at: 0,
        };
        (read.take(MAGIC.len())? == MAGIC).then_some(())?;
        let mut plan = Plan {
            main_graph: read.nodes()?,
            ..Plan::default()
        };
        for _ in 0..read.u64()? {
            plan.reachable.push(read.nodes()?);
        }
        for _ in 0..read.u64()? {
            plan.chunks.push(read.nodes()?);
        }
        for _ in 0..read.u64()? {
            plan.chunk_imports.push(read.nodes()?);
        }
        for _ in 0..read.u64()? {
            plan.shared_symbols.insert(read.u64()?);
        }
        for _ in 0..read.u64()? {
            plan.data_sizes
                .insert(read.u64()? as usize, read.u64()? as usize);
        }
        plan.unused = read.nodes()?;
        for _ in 0..read.u64()? {
            let node = read.u64()?;
            plan.graph.insert(node, read.nodes()?);
        }
        for _ in 0..read.u64()? {
            let index = read.u64()? as usize;
            let len = read.u64()? as usize;
            let name = read.take(len)?;
            plan.data_names
                .push((index, String::from_utf8(name.to_vec()).ok()?));
        }
        Some(plan)
    }

    /// Keep `plan` as the plan made from `key`. Written beside and renamed
    /// in, so a split that stops leaves no half plan behind, and made again
    /// when the directory went away under the split.
    pub fn store(&self, key: &str, plan: &Plan) -> Result<()> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        write_nodes(&mut out, &plan.main_graph);
        out.extend_from_slice(&(plan.reachable.len() as u64).to_le_bytes());
        for set in &plan.reachable {
            write_nodes(&mut out, set);
        }
        out.extend_from_slice(&(plan.chunks.len() as u64).to_le_bytes());
        for set in &plan.chunks {
            write_nodes(&mut out, set);
        }
        out.extend_from_slice(&(plan.chunk_imports.len() as u64).to_le_bytes());
        for set in &plan.chunk_imports {
            write_nodes(&mut out, set);
        }
        out.extend_from_slice(&(plan.shared_symbols.len() as u64).to_le_bytes());
        for node in &plan.shared_symbols {
            out.extend_from_slice(&node.to_le_bytes());
        }
        out.extend_from_slice(&(plan.data_sizes.len() as u64).to_le_bytes());
        for (index, size) in &plan.data_sizes {
            out.extend_from_slice(&(*index as u64).to_le_bytes());
            out.extend_from_slice(&(*size as u64).to_le_bytes());
        }
        write_nodes(&mut out, &plan.unused);
        out.extend_from_slice(&(plan.graph.len() as u64).to_le_bytes());
        for (node, children) in &plan.graph {
            out.extend_from_slice(&node.to_le_bytes());
            write_nodes(&mut out, children);
        }
        out.extend_from_slice(&(plan.data_names.len() as u64).to_le_bytes());
        for (index, name) in &plan.data_names {
            out.extend_from_slice(&(*index as u64).to_le_bytes());
            out.extend_from_slice(&(name.len() as u64).to_le_bytes());
            out.extend_from_slice(name.as_bytes());
        }
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating {}", self.dir.display()))?;
        let path = self.path(key);
        let partial = self.dir.join(format!(".{key}.{}", std::process::id()));
        std::fs::write(&partial, &out).with_context(|| format!("writing {}", partial.display()))?;
        std::fs::rename(&partial, &path).with_context(|| format!("writing {}", path.display()))?;
        // The plan this run made: the next one may move it through a
        // renumbering when its own key is not here.
        let latest = self.dir.join("latest");
        let partial = self.dir.join(format!(".latest.{}", std::process::id()));
        std::fs::write(&partial, key).with_context(|| format!("writing {}", partial.display()))?;
        std::fs::rename(&partial, &latest)
            .with_context(|| format!("writing {}", latest.display()))?;
        self.prune(key);
        Ok(())
    }

    /// Say this run took the plan under `key`: the next run moves this one.
    pub fn touch(&self, key: &str) {
        let latest = self.dir.join("latest");
        let partial = self.dir.join(format!(".latest.{}", std::process::id()));
        if std::fs::write(&partial, key).is_ok() {
            let _ = std::fs::rename(&partial, &latest);
        }
    }

    /// The plan the last run made, if this cache holds it: the one a
    /// renumbering moves to this run's module.
    pub fn load_latest(&self) -> Option<Plan> {
        let key = std::fs::read_to_string(self.dir.join("latest")).ok()?;
        let plan = self.load(key.trim())?;
        self.moved
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(plan)
    }

    /// How many plans this cache moved to another module's indices.
    #[cfg(test)]
    pub fn moved(&self) -> usize {
        self.moved.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Drop the plans this run did not use, past the last few: a plan that
    /// changed is never asked for again under its old key.
    fn prune(&self, used: &str) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let mut plans = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|extension| extension != "plan") {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let when = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            plans.push((when, name.to_owned()));
        }
        plans.sort_by_key(|(when, _)| std::cmp::Reverse(*when));
        for (_, name) in plans
            .into_iter()
            .filter(|(_, name)| name != used)
            .skip(KEEP - 1)
        {
            std::fs::remove_file(self.path(&name)).ok();
        }
    }
}

fn write_nodes(out: &mut Vec<u8>, nodes: &HashSet<Node>) {
    out.extend_from_slice(&(nodes.len() as u64).to_le_bytes());
    for node in nodes {
        out.extend_from_slice(&node.to_le_bytes());
    }
}

/// Reading a plan file: bytes, and how far into them the reader is.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let slice = self.bytes.get(self.at..self.at.checked_add(len)?)?;
        self.at += len;
        Some(slice)
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn nodes(&mut self) -> Option<HashSet<Node>> {
        let len = self.u64()? as usize;
        let mut nodes = HashSet::with_capacity(len.min(1 << 24));
        for _ in 0..len {
            nodes.insert(self.u64()?);
        }
        Some(nodes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a run keeps is what the next one reads back, in the same sets.
    #[test]
    fn a_stored_plan_reads_back() {
        let dir = std::env::temp_dir().join(format!("wasm-split-plan-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let cache = PlanCache::at(&dir);
        let plan = Plan {
            main_graph: [1, 2, 3].into_iter().map(function_node).collect(),
            reachable: vec![
                [4, 5].into_iter().map(function_node).collect(),
                [data_node(7)].into_iter().collect(),
            ],
            chunks: vec![[9, 10].into_iter().map(function_node).collect()],
            chunk_imports: vec![[11, 12].into_iter().map(function_node).collect()],
            shared_symbols: [function_node(11), data_node(12)].into_iter().collect(),
            data_sizes: [(12, 4096)].into_iter().collect(),
            unused: [function_node(13), data_node(14)].into_iter().collect(),
            graph: [
                (
                    function_node(1),
                    [function_node(2), data_node(3)].into_iter().collect(),
                ),
                (data_node(3), [function_node(4)].into_iter().collect()),
            ]
            .into_iter()
            .collect(),
            data_names: [(3, "a static".to_string())].into_iter().collect(),
        };
        cache.store("abc", &plan).expect("store");
        let read = cache.load("abc").expect("load");
        assert_eq!(read.main_graph, plan.main_graph);
        assert_eq!(read.reachable.len(), 2);
        assert_eq!(read.reachable[0], plan.reachable[0]);
        assert_eq!(read.reachable[1], plan.reachable[1]);
        assert_eq!(read.chunks, plan.chunks);
        assert_eq!(read.chunk_imports, plan.chunk_imports);
        assert_eq!(read.shared_symbols, plan.shared_symbols);
        assert_eq!(read.data_sizes, plan.data_sizes);
        assert_eq!(read.unused, plan.unused);
        assert_eq!(read.graph, plan.graph);
        assert_eq!(read.data_names, plan.data_names);
        assert!(cache.load("other").is_none(), "another key is no plan");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A node says which index space it is in, whatever the index.
    #[test]
    fn a_node_keeps_its_index_space() {
        assert_eq!(node_index(function_node(0)), (0, false));
        assert_eq!(node_index(data_node(0)), (0, true));
        assert_eq!(node_index(function_node(541_065)), (541_065, false));
        assert_eq!(node_index(data_node(541_065)), (541_065, true));
    }
}
