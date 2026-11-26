// Adapted from wasm-split-cli 0.7.10 (Dioxus, https://github.com/DioxusLabs/dioxus,
// MIT OR Apache-2.0); see ../PROVENANCE.md for what changed.

use crate::analysis::{Analysis, AnalysisCache, Body, STRUCTURE_LEN, hash_body};
use crate::cache::{EmitCache, Key, data_section, key_sections, sections_not_keyed};
use crate::content;
use crate::data::{Claimant, DataLayout, DataPlan};
use crate::plan::PlanCache;
use crate::relocations::ModuleWithRelocations;
use anyhow::{Context, Result};
use itertools::Itertools;
use rayon::prelude::{
    IndexedParallelIterator, IntoParallelIterator, IntoParallelRefIterator,
    IntoParallelRefMutIterator, ParallelIterator,
};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    hash::Hash,
    ops::Range,
    sync::{Arc, RwLock},
};
use walrus::{
    ConstExpr, ElementItems, ElementKind, ExportId, ExportItem, FunctionBuilder, FunctionId,
    FunctionKind, GlobalKind, ImportId, ImportKind, Module, ModuleConfig, RefType, TableId, TypeId,
    ir::{self, Visitor, dfs_in_order},
};
use wasmparser::Payload;

mod planning;

/// A parsed wasm module with additional metadata and functionality for splitting and patching.
///
/// This struct assumes that relocations will be present in incoming wasm binary.
/// Upon construction, all the required metadata will be constructed.
pub struct Splitter<'a> {
    /// The original module we use as a reference
    source_module: Module,

    // The byte sources of the pre and post wasm-bindgen .wasm files
    // We need the original around since wasm-bindgen ruins the relocation locations.
    original: &'a [u8],
    bindgened: &'a [u8],

    // Mapping of indices of source functions
    // This lets us use a much faster approach to emitting split modules simply by maintaining a mapping
    // between the original Module and the new Module. Ideally we could just index the new module
    // with old FunctionIds but the underlying IndexMap actually checks that a key belongs to a particular
    // arena.
    fns_to_ids: HashMap<FunctionId, usize>,
    _ids_to_fns: Vec<FunctionId>,

    shared_symbols: BTreeSet<Node>,
    split_points: Vec<SplitPoint>,
    /// The table slots past the linked layout the split modules' entries
    /// take, before the shared functions' slots.
    entry_slots: usize,
    chunks: Vec<HashSet<Node>>,
    /// What each chunk's functions call outside it, by chunk index: read off
    /// the call graph when the chunks are made, so an emission needs none.
    chunk_imports: Vec<HashSet<Node>>,
    main_graph: HashSet<Node>,
    call_graph: HashMap<Node, HashSet<Node>>,
    parent_graph: HashMap<Node, HashSet<Node>>,
    /// Each defined data symbol's size, by its symbol index.
    data_sizes: HashMap<usize, usize>,
    /// The imported functions' count, before the module's own in index order.
    imported: usize,
    /// What each of the module's own functions calls and the immediates its
    /// code holds, in index order among them (analysis.rs): the bodies this
    /// run walked, and the ones a run before it did.
    bodies: Vec<Body>,
    /// How many of `bodies` a run before this one had read.
    bodies_reused: usize,
    /// The local indices of the bodies this run walked: the new and changed
    /// ones, whose answers a read-back plan has to make again.
    bodies_changed: Vec<usize>,
    /// What the bind patch made of the last split's functions, when it wrote
    /// the module again with functions renumbered (analysis.rs).
    renumber: Option<crate::analysis::Renumber>,
    /// What the plan every module is taken from was made from (plan.rs).
    plan_key: String,
    /// What an emission reads of the bound module that is not a body
    /// (analysis.rs): its sections, the type section among them, and its
    /// function counts. The start module's sections are read from it, so its
    /// emission is keyed by it (`Splitter::start_key`).
    structure: String,
    /// The linked module's lazy exports, which its sections elsewhere do not
    /// tell: which module answers which export (`lazy_exports`).
    lazy_exports: Vec<(String, String)>,
    /// The main module's symbols that nothing it keeps reaches: read off the
    /// call graph when the plan is made, so an emission needs none.
    planned_unused: Option<HashSet<Node>>,
    /// What each function index is by content rather than by number: its
    /// name, or an import's module and name. A module's emission is keyed by
    /// these, so a function added or renamed elsewhere moves no module's key
    /// (`Splitter::symbol_content_ids`).
    symbol_ids: Vec<u64>,
    /// Each local body's code by content, not by the numbers it holds
    /// (content.rs): the identity a module's emission is keyed by.
    content_hashes: Vec<u64>,
    /// Each data symbol's identity by name (`relocations::data_symbol_ids`),
    /// by symbol index.
    data_symbol_ids: Vec<u64>,
    /// Each data symbol's name, by its index in the linked module: the
    /// identity a renumbering leaves as it was.
    data_names: HashMap<usize, String>,
}

/// What every emission's key starts from (`Splitter::emit_key_base`), and
/// the bound module's function bodies, by defined index.
pub(crate) struct KeyBase {
    pub(crate) key: Key,
    pub(crate) imported: usize,
}

/// What one split module or shared chunk defines, imports and deletes (in
/// the source module's ids), and the function bodies it is parsed with.
#[derive(Clone)]
struct SplitParts {
    unique_symbols: HashSet<Node>,
    symbols_to_import: HashSet<Node>,
    symbols_to_delete: HashSet<Node>,
    relies_on_chunks: HashSet<usize>,
    bodies: HashSet<usize>,
}

/// Memory one split module's parse and emission takes, per byte of the
/// bindgened module it is parsed from, with the bodies it does not keep left
/// out: about 4.2 measured on the development desktop bundle (a 621 MB
/// module: 15.3 GiB at most emitting one at a time, 36.1 GiB nine at a
/// time), and some headroom.
const COPY_BYTES_PER_INPUT_BYTE: u64 = 8;

/// The most memory the split modules' parses may hold at once, however much
/// more is free. Bounded only by free memory, the splitter filled whatever
/// memory a node or a job's reservation left: sdk-web held up to 125 GiB for
/// one minute of a run that otherwise holds 1-13 GiB, and CI reserves a
/// job's peak for each of its runs, so every later run waited for a node
/// with that much free. Within this budget that bundle emits three modules
/// at a time in about two minutes (nine took 75 s, one 380 s); the
/// whole-program parse before it holds about 15 GiB regardless.
/// `WASM_SPLIT_THREADS` sets the count instead.
const EMIT_MEMORY_BUDGET: u64 = 16 << 30;

/// The least code a group of functions the same split modules share needs
/// to be a chunk of its own; smaller groups share one chunk, so a module
/// does not fetch hundreds of tiny files.
const MIN_CHUNK_BYTES: usize = 64 * 1024;

/// Report on stderr how long a step of the split took, so a slow split
/// says where its time went.
fn took(step: &str, started: std::time::Instant) {
    eprintln!(
        "wasm-split: {step} in {:.1} s",
        started.elapsed().as_secs_f64()
    );
}

/// Bytes this process can still allocate: what its cgroup's memory limit
/// leaves, or the system's available memory, whichever is less. `None` when
/// neither can be read.
fn available_memory() -> Option<u64> {
    let read = |path: &str| std::fs::read_to_string(path).ok();
    let cgroup = (|| {
        let max = read("/sys/fs/cgroup/memory.max")?
            .trim()
            .parse::<u64>()
            .ok()?;
        let current = read("/sys/fs/cgroup/memory.current")?
            .trim()
            .parse::<u64>()
            .ok()?;
        Some(max.saturating_sub(current))
    })();
    let system = read("/proc/meminfo").and_then(|meminfo| {
        let line = meminfo
            .lines()
            .find(|line| line.starts_with("MemAvailable:"))?;
        let kib = line.split_whitespace().nth(1)?.parse::<u64>().ok()?;
        Some(kib * 1024)
    });
    match (cgroup, system) {
        (Some(cgroup), Some(system)) => Some(cgroup.min(system)),
        (one, other) => one.or(other),
    }
}

/// The results of splitting the wasm module with some additional metadata for later use.
pub struct OutputModules {
    /// How many of the module's bodies a run before this one had read
    /// (analysis.rs).
    pub bodies_reused: usize,
    /// The main chunk
    pub main: SplitModule,

    /// The modules of the wasm module that were split.
    pub modules: Vec<SplitModule>,

    /// The chunks that might be imported by the main modules
    pub chunks: Vec<SplitModule>,

    /// How many split modules and chunks were read back from the cache
    /// rather than emitted.
    pub reused: usize,
}

/// A wasm module that was split from the main module.
///
/// All IDs here correspond to *this* module - not the parent main module
pub struct SplitModule {
    pub module_name: String,
    pub hash_id: Option<String>,
    pub component_name: Option<String>,
    pub bytes: Vec<u8>,
    pub relies_on_chunks: HashSet<usize>,
}

/// `module` with the copies of a function it carries more than once merged
/// into one ([`crate::dedupe`]).
fn merged(mut module: SplitModule) -> Result<SplitModule> {
    let (bytes, count) = crate::dedupe::merge_duplicates(&module.bytes)
        .with_context(|| format!("merging the duplicate functions of {}", module.module_name))?;
    tracing::info!("{}: merged {count} duplicate functions", module.module_name);
    module.bytes = bytes;
    Ok(module)
}

impl<'a> Splitter<'a> {
    /// Create a new "splitter" instance using the original wasm and the wasm from the output of wasm-bindgen.
    ///
    /// This will use the relocation data from the original module to create a call graph that we
    /// then use with the post-bindgened module to create the split modules.
    ///
    /// It's important to compile the wasm with --emit-relocs such that the relocations are available
    /// to construct the callgraph.
    #[cfg(test)]
    pub fn new(original: &'a [u8], bindgened: &'a [u8]) -> Result<Self> {
        Self::with_analysis(original, bindgened, None)
    }

    /// [`Self::new`], reading back from `cache` what an earlier run of this
    /// module's bodies read of them, and keeping this run's there
    /// (analysis.rs).
    #[cfg(test)]
    pub fn with_analysis(
        original: &'a [u8],
        bindgened: &'a [u8],
        cache: Option<&AnalysisCache>,
    ) -> Result<Self> {
        let mut splitter = Self::prepare(original, bindgened, cache)?;
        splitter.finish(None)?;
        Ok(splitter)
    }

    /// [`Self::with_analysis`], stopping once every body is read: what the
    /// split's plan is made from is settled then ([`Self::plan_key`]), so a
    /// run whose plan an earlier one kept (plan.rs) skips [`Self::finish`] —
    /// the call graph, the shared code and everything read from them.
    pub fn prepare(
        original: &'a [u8],
        bindgened: &'a [u8],
        cache: Option<&AnalysisCache>,
    ) -> Result<Self> {
        let renumber = crate::analysis::Renumber::from_env(bindgened);
        Self::prepare_with(original, bindgened, cache, renumber)
    }

    /// [`Self::prepare`] with the renumbering the bind patch wrote, as the
    /// caller read it (`WASM_SPLIT_RENUMBER` names it in a build).
    pub(crate) fn prepare_with(
        original: &'a [u8],
        bindgened: &'a [u8],
        cache: Option<&AnalysisCache>,
        renumber: Option<crate::analysis::Renumber>,
    ) -> Result<Self> {
        let started = std::time::Instant::now();
        let (module, ids, fns_to_ids) = parse_module_with_ids(bindgened)?;
        took("parsed the bindgened module", started);

        // Which body bytes this module has, and whether they are the ones an
        // earlier run's answers belong to: its sections other than the code,
        // and the two counts the indices follow.
        let started = std::time::Instant::now();
        let code = code_section(bindgened)?;
        let hashes: Vec<u64> = match &code {
            Some(code) => code
                .bodies
                .par_iter()
                .map(|body| hash_body(&bindgened[body.clone()]))
                .collect(),
            None => Vec::new(),
        };
        let imported = code
            .as_ref()
            .map_or(0, |code| module.funcs.iter().count() - code.bodies.len());
        let mut structure = Key::new();
        // A body's answers are its code's, so the sections that are not code
        // are what says which bodies they belong to: the data section is one
        // module's own ranges, written from the plan.
        hash_sections(&mut structure, bindgened, &sections_not_read(bindgened)?)?;
        structure.add(&imported).add(&hashes.len());
        let structure = structure.finish();
        assert_eq!(structure.len(), STRUCTURE_LEN);
        let lazy_exports = lazy_exports(original)?;
        let split_points = accumulate_split_points(&module, &lazy_exports)?;
        let entry_slots = split_points.iter().map(|split| split.entries.len()).sum();

        // Note that we can't trust the normal symbols - just the data symbols - and we can't use the data offset
        // since that's not reliable after bindgening

        let mut module = Self {
            source_module: module,
            original,
            bindgened,
            split_points,
            entry_slots,
            _ids_to_fns: ids,
            fns_to_ids,
            main_graph: Default::default(),
            chunks: Default::default(),
            chunk_imports: Default::default(),
            call_graph: Default::default(),
            parent_graph: Default::default(),
            shared_symbols: Default::default(),
            data_sizes: Default::default(),
            imported,
            bodies: Vec::new(),
            bodies_reused: 0,
            bodies_changed: Vec::new(),
            renumber: None,
            plan_key: String::new(),
            structure: String::new(),
            lazy_exports,
            planned_unused: None,
            symbol_ids: Vec::new(),
            content_hashes: Vec::new(),
            data_symbol_ids: Vec::new(),
            data_names: HashMap::new(),
        };

        // What each body does, by content: the identity a renumbering leaves
        // as it was (content.rs), and what a body is read back by.
        module.symbol_ids =
            content::symbol_ids(&module.source_module, &module._ids_to_fns, module.imported);
        module.content_hashes = content::body_hashes(module.bindgened, &module.symbol_ids);
        module.data_symbol_ids = crate::relocations::data_symbol_ids(original)?;
        took("read the bodies' content", started);

        let bodies = cache
            .and_then(AnalysisCache::load)
            .map(|analysis| match analysis.matching(&structure, &hashes) {
                Some(bodies) => bodies,
                None => renumber
                    .as_ref()
                    .and_then(|renumber| {
                        analysis.matching_renumbered(renumber, &module.content_hashes, &hashes)
                    })
                    .unwrap_or_default(),
            })
            .unwrap_or_default();
        if cache.is_some() {
            eprintln!(
                "wasm-split: read {} of {} function bodies from the last split",
                bodies.len(),
                hashes.len()
            );
        }
        module.bodies_reused = bodies.len();
        module.bodies_changed = (0..hashes.len())
            .filter(|local| !bodies.contains_key(local))
            .collect();
        module.renumber = renumber;
        took("read the bodies' hashes", started);

        // The bodies first: everything after this reads their answers, so a
        // run whose plan is kept reads them and then stops.
        let started = std::time::Instant::now();
        let content = module.content_hashes.clone();
        module.walk_bodies(&hashes, &content, bodies)?;
        took("walked the bound functions' bodies", started);

        let started = std::time::Instant::now();
        module.plan_key = crate::plan::key(&structure, original, &module.bodies)?;
        took("read what the split's plan is made from", started);

        if let Some(cache) = cache {
            cache.store(&Analysis {
                structure: structure.clone(),
                bodies: module.bodies.clone(),
            })?;
        }
        module.structure = structure;
        Ok(module)
    }

    /// Take the call graph and the shared code from the bodies [`Self::prepare`]
    /// read: what every module's emission is decided by. A plan an earlier
    /// run made from the same things is read back instead of made again
    /// (plan.rs); what each module then holds is settled against that
    /// module's own key, so a body whose code changed is still emitted.
    pub fn finish(&mut self, plans: Option<&PlanCache>) -> Result<()> {
        if let Some(plans) = plans {
            if let Some(plan) = plans.load(&self.plan_key) {
                self.use_plan(plan);
                // This plan is the module's: the next run moves this one.
                plans.touch(&self.plan_key);
                eprintln!("wasm-split: the plan is the last split's: its graphs read back");
                return Ok(());
            }
            if let Some(plan) = self.read_plan_back(plans)? {
                self.use_plan(plan);
                // This plan is the module's: the next run moves this one.
                plans.store(&self.plan_key, &self.plan())?;
                eprintln!("wasm-split: the plan is the last split's, its functions renumbered");
                return Ok(());
            }
        }
        let started = std::time::Instant::now();
        self.build_call_graph()?;
        took("built the call graph", started);
        let started = std::time::Instant::now();
        self.build_split_chunks()?;
        took("grouped the shared code", started);
        // What the start module keeps that nothing it reaches: the walk is
        // the call graph's, and every emission of the start module reads it.
        let started = std::time::Instant::now();
        let mut unused = self.unused_main_symbols();
        self.keep_referenced(&mut unused);
        self.planned_unused = Some(unused);
        took("found what the start module keeps", started);
        if let Some(plans) = plans {
            plans.store(&self.plan_key, &self.plan())?;
        }
        Ok(())
    }

    /// Split the module into multiple modules at the boundaries of split points.
    ///
    /// Note that the binaries might still be "large" at the end of this process. In practice, you
    /// need to push these binaries through wasm-bindgen and wasm-opt to take advantage of the
    /// optimizations and splitting. We perform a few steps like zero-ing out the data segments
    /// that will only be removed by the memory-packing step of wasm-opt.
    ///
    /// This returns the list of chunks, an import map, and some javascript to link everything together.
    pub fn emit(self) -> Result<OutputModules> {
        let cache = EmitCache::from_env()?;
        self.emit_with(cache.as_ref())
    }

    /// [`Self::emit`], reading back from `cache` each module emitted before
    /// from exactly what it is emitted from now, and keeping the others there.
    pub fn emit_with(mut self, cache: Option<&EmitCache>) -> Result<OutputModules> {
        tracing::info!("Emitting split modules.");

        // What each split module holds is settled while the whole program is
        // parsed. The main module is emitted next and takes that parse with
        // it, so the split modules are emitted without it in memory.
        // Each module's part is its own: settled at once, one module a thread.
        let started = std::time::Instant::now();
        let (chunk_parts, module_parts) = rayon::join(
            || {
                (0..self.chunks.len())
                    .into_par_iter()
                    .map(|idx| self.split_chunk_parts(idx))
                    .collect::<Vec<_>>()
            },
            || {
                (0..self.split_points.len())
                    .into_par_iter()
                    .map(|idx| self.split_module_parts(idx))
                    .collect::<Vec<_>>()
            },
        );
        took("settled what each module holds", started);

        // Which static data each module reaches is read from the code it
        // carries (see data.rs); the main module's is known once it is
        // pruned.
        let started = std::time::Instant::now();
        let data_layout = DataLayout::new(self.original)?;
        // What each of the module's own functions' code holds, as the walk
        // that built the call graph read it (analysis.rs).
        let held: Vec<(FunctionId, &[u32])> = self
            .bodies
            .iter()
            .enumerate()
            .map(|(index, body)| {
                (
                    self._ids_to_fns[self.imported + index],
                    body.values.as_slice(),
                )
            })
            .collect();
        let data_refs = data_layout.function_refs(&held);
        let data_claimants = self.data_claimants(&chunk_parts, &module_parts);
        let loaded = loaded_with(&module_parts, chunk_parts.len());
        took("read the static data's references", started);
        let bodies_reused = self.bodies_reused;

        let started = std::time::Instant::now();
        let start_key = self.start_key();
        let held_start = cache.and_then(|cache| cache.get_start(&start_key));
        let (main, data) = match held_start {
            Some((bytes, data)) => {
                eprintln!("wasm-split: the start module is the last split's: read back");
                (
                    SplitModule {
                        module_name: "main".to_string(),
                        component_name: None,
                        bytes,
                        relies_on_chunks: Default::default(),
                        hash_id: None,
                    },
                    data,
                )
            }
            None => {
                let (main, data) =
                    self.emit_main_module(&data_layout, &data_refs, data_claimants, &loaded)?;
                let main = merged(main)?;
                if let Some(cache) = cache {
                    cache.put_start(&start_key, &main.bytes, &data)?;
                }
                (main, data)
            }
        };
        took("emitted the start module", started);

        // Each module being emitted holds a parse of its own; run only as
        // many at once as there is memory for.
        let copy_bytes = (self.bindgened.len() as u64).saturating_mul(COPY_BYTES_PER_INPUT_BYTE);
        let cores = std::thread::available_parallelism().map_or(1, usize::from);
        let memory =
            available_memory().map_or(EMIT_MEMORY_BUDGET, |free| free.min(EMIT_MEMORY_BUDGET));
        let threads = std::env::var("WASM_SPLIT_THREADS")
            .ok()
            .and_then(|threads| threads.parse::<usize>().ok())
            .unwrap_or_else(|| usize::try_from(memory / copy_bytes.max(1)).unwrap_or(cores))
            .clamp(1, cores);
        let count = chunk_parts.len() + module_parts.len();
        let started = std::time::Instant::now();
        // With WASM_SPLIT_CACHE, a module emitted before from exactly what
        // this one is emitted from is read back instead (cache.rs).
        let base = cache.map(|_| self.emit_key_base(&main.bytes)).transpose()?;
        let reused = std::sync::atomic::AtomicUsize::new(0);
        let cached = |key: Option<&Key>,
                      emit: &dyn Fn() -> Result<SplitModule>,
                      held: &dyn Fn(Vec<u8>) -> SplitModule| {
            let (Some(cache), Some(key)) = (cache, key) else {
                return emit();
            };
            let key = key.finish();
            if let Some(bytes) = cache.get(&key) {
                reused.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return Ok(held(bytes));
            }
            let module = emit()?;
            cache.put(&key, &module.bytes)?;
            Ok(module)
        };
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .context("starting the split threads")?;
        let (chunks, modules) = pool.install(|| {
            let chunks = chunk_parts
                .into_par_iter()
                .enumerate()
                .map(|(idx, parts)| {
                    let key = base
                        .as_ref()
                        .map(|base| self.emit_key(base, idx, None, &parts, &data));
                    let held = |bytes| SplitModule {
                        bytes,
                        module_name: "split".to_string(),
                        component_name: None,
                        relies_on_chunks: Default::default(),
                        hash_id: None,
                    };
                    cached(
                        key.as_ref(),
                        &|| {
                            self.emit_split_chunk(idx, parts.clone(), &data)
                                .and_then(merged)
                        },
                        &held,
                    )
                })
                .collect::<Result<Vec<SplitModule>>>();
            let modules = module_parts
                .into_par_iter()
                .enumerate()
                .map(|(idx, parts)| {
                    let split = &self.split_points[idx];
                    let key = base
                        .as_ref()
                        .map(|base| self.emit_key(base, idx, Some(split), &parts, &data));
                    let held = |bytes| SplitModule {
                        bytes,
                        module_name: split.module_name.clone(),
                        component_name: split
                            .point
                            .as_ref()
                            .map(|point| point.component_name.clone()),
                        relies_on_chunks: parts.relies_on_chunks.clone(),
                        hash_id: split.point.as_ref().map(|point| point.hash_name.clone()),
                    };
                    cached(
                        key.as_ref(),
                        &|| {
                            self.emit_split_module(idx, parts.clone(), &data)
                                .and_then(merged)
                        },
                        &held,
                    )
                })
                .collect::<Result<Vec<SplitModule>>>();
            (chunks, modules)
        });
        if let Some(cache) = cache {
            cache.prune()?;
        }
        let reused = reused.into_inner();
        eprintln!(
            "wasm-split: emitted {count} modules ({reused} kept from the last split), {threads} at a time, in {:.1} s",
            started.elapsed().as_secs_f64()
        );

        Ok(OutputModules {
            modules: modules?,
            chunks: chunks?,
            main,
            reused,
            bodies_reused,
        })
    }

    /// What the start module's emission was made from (cache.rs): what it
    /// reaches and the data it writes, by content, and the code of every body
    /// it keeps, which is every one nothing it reaches prunes away.
    ///
    /// By content, not by index: a function added or removed elsewhere
    /// renumbers the module but leaves what the start module is made of as it
    /// was, and its emission is read back rather than made again.
    fn start_key(&self) -> String {
        let mut key = Key::new();
        key.add(&"start");
        // The bound module's structure, its types included, and the split
        // points' table slots: what the start module's sections hold besides
        // the bodies below.
        key.add(&self.structure);
        key.add(&self.lazy_exports);
        for split in &self.split_points {
            key.add(&split.module_name);
            if let Some(point) = &split.point {
                key.add(&point.hash_name).add(&point.component_name);
            }
            for entry in &split.entries {
                key.add(&self.node_key(&Node::Function(entry.func)))
                    .add(&entry.slot)
                    .add(&entry.lazy_export.is_some());
            }
        }
        key.add_set(self.main_graph.iter().map(|node| self.node_key(node)));
        let mut sizes: Vec<((u8, u64), u64)> = self
            .data_sizes
            .iter()
            .map(|(index, size)| (self.node_key(&Node::DataSymbol(*index)), *size as u64))
            .collect();
        sizes.sort_unstable();
        key.add(&sizes);
        // The static data itself: what the start module writes into memory is
        // its content, which a range of the same size does not tell.
        if let Some(data) = data_section(self.bindgened) {
            key.add(data);
        }
        // The bodies the start module's own code holds: what it reaches,
        // each by content. An app's body that changed is not one of them,
        // and a plan that prunes it from the start leaves the start the
        // module it was.
        let mut kept: Vec<u64> = self
            .main_graph
            .iter()
            .filter_map(|node| match node {
                Node::Function(id) => self
                    .fns_to_ids
                    .get(id)
                    .copied()
                    .filter(|index| *index >= self.imported)
                    .and_then(|index| self.content_hashes.get(index - self.imported))
                    .copied(),
                Node::DataSymbol(_) => None,
            })
            .collect();
        kept.sort_unstable();
        key.add(&kept);
        key.finish()
    }

    /// What every module's emission reads alike (see cache.rs): the start
    /// module's interface — what a module imports, the table's slots and the
    /// entries it fills — the bound module but for its function bodies, and
    /// the table slots the entries and the shared functions take, in order.
    ///
    /// A module is never read for one emitted beside another start module:
    /// its imports, its table's slots and the entries it fills are that
    /// module's as it was emitted. A start module whose code or static data
    /// changed alone is the same interface, and a module is read back for it.
    pub(crate) fn emit_key_base(&self, main: &[u8]) -> Result<KeyBase> {
        let mut key = Key::new();
        interface(&mut key, main)?;
        // What a module's emission reads that is not the code or the data:
        // its own bodies are keyed by their content, and the data it writes
        // is the plan's ranges for it (which its key holds). The function,
        // export and element sections and the name section are left out too:
        // a function added elsewhere renumbers and rewrites them, while the
        // module emitted from its own bodies is the same bytes.
        key_sections(
            &mut key,
            self.bindgened,
            &sections_not_keyed(self.bindgened)?,
        )?;
        key.add(&self.split_points.len()).add(&self.entry_slots);
        let shared: Vec<_> = self
            .shared_symbols
            .iter()
            .map(|node| self.node_key(node))
            .collect();
        key.add(&shared);
        Ok(KeyBase {
            key,
            imported: imported_functions(self.bindgened)?,
        })
    }

    /// The key of emitting split module `idx` (`split`), or shared chunk
    /// `idx` (no `split`): `base`, and what this one keeps, defines, imports
    /// and deletes, and the static data it writes.
    fn emit_key(
        &self,
        base: &KeyBase,
        idx: usize,
        split: Option<&SplitPoint>,
        parts: &SplitParts,
        data: &DataPlan,
    ) -> Key {
        let (what, data_index) = match split {
            Some(_) => ("module", idx),
            None => ("chunk", self.split_points.len() + idx),
        };
        let mut key = base.key.clone();
        key.add(what).add(&idx);
        if let Some(split) = split {
            key.add(&split.module_name);
            // The point and the entries by what they name, never by their
            // ids or indices: those move when a function is added elsewhere.
            if let Some(point) = &split.point {
                key.add(&point.hash_name)
                    .add(&point.component_name)
                    .add(&self.node_key(&Node::Function(point.import_func)));
            }
            for entry in &split.entries {
                key.add(&self.node_key(&Node::Function(entry.func)))
                    .add(&entry.slot)
                    .add(&entry.lazy_export.is_some());
            }
        }
        key.add_set(parts.unique_symbols.iter().map(|node| self.node_key(node)))
            .add_set(
                parts
                    .symbols_to_import
                    .iter()
                    .map(|node| self.node_key(node)),
            )
            .add_set(
                parts
                    .symbols_to_delete
                    .iter()
                    .map(|node| self.node_key(node)),
            )
            .add_set(parts.relies_on_chunks.iter().copied());
        // The bodies the parse keeps, by content: the parse leaves every
        // other one `unreachable`, and a body's index moves when a function
        // is added elsewhere while the module it is emitted into does not.
        let mut bodies: Vec<Option<u64>> = parts
            .bodies
            .iter()
            .map(|index| {
                index
                    .checked_sub(base.imported)
                    .and_then(|at| self.content_hashes.get(at))
                    .copied()
            })
            .collect();
        bodies.sort_unstable();
        key.add(&bodies);
        key.add(&data.moved.get(data_index));
        key
    }

    /// A node as the emission sees it: a function by its content (see
    /// `symbol_ids`), a data symbol by its symbol index.
    ///
    /// A function is not keyed by its index in the bound module: a function
    /// added or removed elsewhere renumbers it, and every module that names
    /// it would be emitted again for no change of its own. Two functions
    /// that share a name are told apart by their bodies' hashes, so a set of
    /// them keeps its key under a renumbering.
    fn node_key(&self, node: &Node) -> (u8, u64) {
        match node {
            Node::Function(id) => (
                0,
                self.fns_to_ids
                    .get(id)
                    .and_then(|index| self.symbol_ids.get(*index))
                    .copied()
                    .unwrap_or(u64::MAX),
            ),
            Node::DataSymbol(index) => (
                1,
                self.data_symbol_ids
                    .get(*index)
                    .copied()
                    .unwrap_or(*index as u64),
            ),
        }
    }

    /// Emit the main module.
    ///
    /// This will analyze the call graph and then perform some transformations on the module.
    /// - Clear out active segments that the split modules will initialize
    /// - Wipe away unused functions and data symbols
    /// - Re-export the memories, globals, and other items that the split modules will need
    /// - Convert the split module import functions to real functions that call the indirect function
    ///
    /// Once this is done, all the split module functions will have been removed, making the main module smaller.
    ///
    /// Emitting the main module is conceptually pretty simple. Emitting the split modules is more
    /// complex.
    fn emit_main_module(
        &mut self,
        data_layout: &DataLayout,
        data_refs: &HashMap<FunctionId, Vec<u32>>,
        mut data_claimants: Vec<(Claimant, Vec<FunctionId>)>,
        loaded: &[Vec<usize>],
    ) -> Result<(SplitModule, DataPlan)> {
        tracing::info!("Emitting main bundle split module");

        // The symbols nothing the start module keeps reaches, read off the
        // call graph when the plan was made (see `Splitter::finish`).
        let unused_symbols = self
            .planned_unused
            .clone()
            .context("the start module's kept symbols are read before it is emitted")?;

        let mut out = std::mem::take(&mut self.source_module);
        let hole = self.make_dummy_func(&mut out);

        // 1. Clear out the active segments that try to initialize functions for modules we just split off.
        //    When the side modules load, they will initialize functions into the table where the "holes" are.
        self.replace_segments_with_holes(&mut out, &unused_symbols, hole);

        // 2. Change the functions called from split modules to be local
        //    functions that call the indirect function, while the lazy
        //    exports' functions are still there to read their types from
        self.create_ifunc_table(&mut out, &unused_symbols, hole);
        leave_holes_empty(&mut out, hole);

        // 3. Wipe away the unused functions and data symbols
        self.prune_main_symbols(&mut out, &unused_symbols)?;

        // 4. Re-export the memories, globals, and other stuff
        self.re_export_items(&mut out);

        // 6. Remove the reloc and linking custom sections
        self.remove_custom_sections(&mut out);

        // 7. Run the garbage collector to remove unused functions
        walrus::passes::gc::run(&mut out);

        // 8. Leave out the static data only other modules reach: the
        //    functions left are exactly the code the main module runs
        data_claimants.push((Claimant::Start, out.funcs.iter().map(|f| f.id()).collect()));
        let data = data_layout.plan(&out, data_refs, data_claimants, loaded);
        data.apply_to_start(&mut out);

        let main = SplitModule {
            module_name: "main".to_string(),
            component_name: None,
            bytes: out.emit_wasm(),
            relies_on_chunks: Default::default(),
            hash_id: None,
        };
        Ok((main, data))
    }

    /// What split module `split_idx` defines, imports and deletes, and the
    /// function bodies it keeps.
    fn split_module_parts(&self, split_idx: usize) -> SplitParts {
        let split = &self.split_points[split_idx];

        // These are the symbols that will only exist in this module and not in the main module.
        let mut unique_symbols = split
            .reachable_graph
            .difference(&self.main_graph)
            .cloned()
            .collect::<HashSet<_>>();

        // The functions we'll need to import
        let mut symbols_to_import: HashSet<_> = split
            .reachable_graph
            .intersection(&self.main_graph)
            .cloned()
            .collect();

        // Identify the functions we'll delete
        let symbols_to_delete: HashSet<_> = self
            .main_graph
            .difference(&split.reachable_graph)
            .cloned()
            .collect();

        let mut relies_on_chunks = HashSet::new();
        for (idx, chunk) in self.chunks.iter().enumerate() {
            let nodes_to_extract = unique_symbols
                .intersection(chunk)
                .cloned()
                .collect::<Vec<_>>();
            for node in nodes_to_extract {
                if !self.main_graph.contains(&node) {
                    unique_symbols.remove(&node);
                    symbols_to_import.insert(node);
                    relies_on_chunks.insert(idx);
                }
            }
        }

        let roots = unique_symbols
            .iter()
            .copied()
            .chain(split.entries.iter().map(|entry| Node::Function(entry.func)));
        let stops = self.replaced_or_deleted(&symbols_to_import, &symbols_to_delete);
        let bodies = self.kept_bodies(roots, &stops);
        SplitParts {
            unique_symbols,
            symbols_to_import,
            symbols_to_delete,
            relies_on_chunks,
            bodies,
        }
    }

    /// Write the contents of the split modules to the output
    fn emit_split_module(
        &self,
        split_idx: usize,
        parts: SplitParts,
        data: &DataPlan,
    ) -> Result<SplitModule> {
        let split = self.split_points[split_idx].clone();
        let SplitParts {
            unique_symbols,
            symbols_to_import,
            symbols_to_delete,
            relies_on_chunks,
            bodies,
        } = parts;

        tracing::info!(
            "Emitting module {}/{} {}: {:?}",
            split_idx,
            self.split_points.len(),
            split.module_name,
            relies_on_chunks
        );

        let (mut out, ids_to_fns, _fns_to_ids) =
            parse_module_with_ids(&keep_function_bodies(self.bindgened, &bodies)?)?;

        // Remap the graph to our module's IDs
        let shared_funcs = self
            .shared_symbols
            .iter()
            .map(|f| self.remap_id(&ids_to_fns, f))
            .collect::<Vec<_>>();

        let unique_symbols = self.remap_ids(&unique_symbols, &ids_to_fns);
        let symbols_to_delete = self.remap_ids(&symbols_to_delete, &ids_to_fns);
        let symbols_to_import = self.remap_ids(&symbols_to_import, &ids_to_fns);
        let entries: Vec<(usize, FunctionId)> = split
            .entries
            .iter()
            .map(|entry| (entry.slot, ids_to_fns[self.fns_to_ids[&entry.func]]))
            .collect();

        // Do some basic cleanup of the module to make it smaller
        // This removes exports, imports, and the start function
        self.prune_split_module(&mut out);

        // Keep only the static data this module owns (see data.rs)
        data.apply_to_split(&mut out, split_idx);

        // Clear out the element segments and then add in the initializers for the shared imports
        self.create_ifunc_initializers(&mut out, &unique_symbols);

        // Convert our split module's functions to real functions that call the indirect function
        self.add_split_imports(&mut out, &entries, &symbols_to_import, &shared_funcs);

        // Delete all the functions that are not reachable from the main module
        self.delete_main_funcs_from_split(&mut out, &symbols_to_delete);

        self.remove_custom_sections(&mut out);

        // Run the gc to remove unused functions - also validates the module to ensure we can emit it properly
        // todo(jon): prefer to delete the items as we go so we don't need to run a gc pass. it/it's quite slow
        walrus::passes::gc::run(&mut out);

        Ok(SplitModule {
            bytes: out.emit_wasm(),
            module_name: split.module_name.clone(),
            component_name: split
                .point
                .as_ref()
                .map(|point| point.component_name.clone()),
            relies_on_chunks,
            hash_id: split.point.as_ref().map(|point| point.hash_name.clone()),
        })
    }

    /// What shared chunk `idx` imports and deletes, and the function bodies
    /// it keeps.
    fn split_chunk_parts(&self, idx: usize) -> SplitParts {
        let unique_symbols = &self.chunks[idx];

        // What the chunk's functions call outside it: the start module's
        // functions and other chunks', all reached through the table. Read
        // off the plan, whose chunks were made with it, so a split that takes
        // its plan from an earlier run needs no call graph.
        let symbols_to_import = &self.chunk_imports[idx];

        // The start module's functions it does not call.
        let symbols_to_delete: HashSet<_> = self
            .main_graph
            .difference(symbols_to_import)
            .cloned()
            .collect();

        let stops = self.replaced_or_deleted(symbols_to_import, &symbols_to_delete);
        let bodies = self.kept_bodies(unique_symbols.iter().copied(), &stops);
        SplitParts {
            unique_symbols: unique_symbols.clone(),
            symbols_to_import: symbols_to_import.clone(),
            symbols_to_delete,
            relies_on_chunks: HashSet::new(),
            bodies,
        }
    }

    /// Write a split chunk - this is a chunk with no special functions, just exports + initializers
    fn emit_split_chunk(
        &self,
        idx: usize,
        parts: SplitParts,
        data: &DataPlan,
    ) -> Result<SplitModule> {
        tracing::info!("emitting chunk {}", idx);
        let SplitParts {
            unique_symbols,
            symbols_to_import,
            symbols_to_delete,
            bodies,
            ..
        } = parts;

        // Make sure to remap any ids from the main module to this module
        let (mut out, ids_to_fns, _fns_to_ids) =
            parse_module_with_ids(&keep_function_bodies(self.bindgened, &bodies)?)?;

        // Remap the graph to our module's IDs
        let shared_funcs = self
            .shared_symbols
            .iter()
            .map(|f| self.remap_id(&ids_to_fns, f))
            .collect::<Vec<_>>();

        let unique_symbols = self.remap_ids(&unique_symbols, &ids_to_fns);
        let symbols_to_import = self.remap_ids(&symbols_to_import, &ids_to_fns);
        let symbols_to_delete = self.remap_ids(&symbols_to_delete, &ids_to_fns);

        self.prune_split_module(&mut out);

        // Write only the static data this chunk's code alone reaches
        data.apply_to_split(&mut out, self.split_points.len() + idx);

        // Clear out the element segments and then add in the initializers for the shared imports
        self.create_ifunc_initializers(&mut out, &unique_symbols);

        // We have to make sure our table matches that of the other tables even though we don't call them.
        let ifunc_table_id = self.load_funcref_table(&mut out);
        let segment_start = self
            .expand_ifunc_table_max(
                &mut out,
                ifunc_table_id,
                self.entry_slots + shared_funcs.len(),
            )
            .unwrap();

        self.convert_shared_to_imports(&mut out, segment_start, &shared_funcs, &symbols_to_import);

        let mut slot = segment_start + self.entry_slots;
        let mut own = Vec::new();
        for node in &shared_funcs {
            if let Node::Function(id) = node {
                if unique_symbols.contains(node) {
                    own.push((slot, *id));
                }
                slot += 1;
            }
        }
        for (slot, id) in own {
            let element = out.elements.add(
                ElementKind::Active {
                    table: ifunc_table_id,
                    offset: ConstExpr::Value(ir::Value::I32(slot as i32)),
                },
                ElementItems::Functions(vec![id]),
            );
            out.tables
                .get_mut(ifunc_table_id)
                .elem_segments
                .insert(element);
        }

        self.delete_main_funcs_from_split(&mut out, &symbols_to_delete);

        self.remove_custom_sections(&mut out);

        // Run the gc to remove unused functions - also validates the module to ensure we can emit it properly
        walrus::passes::gc::run(&mut out);

        Ok(SplitModule {
            bytes: out.emit_wasm(),
            module_name: "split".to_string(),
            component_name: None,
            relies_on_chunks: Default::default(),
            hash_id: None,
        })
    }

    /// The functions a split module deletes, or replaces with a call through
    /// the table: their code never reaches it.
    fn replaced_or_deleted(
        &self,
        symbols_to_import: &HashSet<Node>,
        symbols_to_delete: &HashSet<Node>,
    ) -> HashSet<Node> {
        symbols_to_import
            .iter()
            .filter(|node| self.shared_symbols.contains(node))
            .chain(symbols_to_delete)
            .copied()
            .collect()
    }

    /// The functions whose code each split module and shared chunk
    /// carries, for placing static data (see data.rs): split module `i` is
    /// claimant `i`, chunk `i` comes after the split modules.
    fn data_claimants(
        &self,
        chunk_parts: &[SplitParts],
        module_parts: &[SplitParts],
    ) -> Vec<(Claimant, Vec<FunctionId>)> {
        let bodies = |parts: &SplitParts| -> Vec<FunctionId> {
            parts
                .bodies
                .iter()
                .map(|index| self._ids_to_fns[*index])
                .collect()
        };
        module_parts
            .iter()
            .chain(chunk_parts)
            .enumerate()
            .map(|(index, parts)| (Claimant::Module(index), bodies(parts)))
            .collect()
    }

    /// The functions (by index) whose bodies a split module rooted at
    /// `roots` keeps; it is parsed with every other body `unreachable`.
    /// Parsing the whole program once per split module held a full copy of
    /// it in memory for each module being emitted (140 GB for a 170 MB
    /// module split in 34), only to delete nearly all of it again.
    ///
    /// A body is kept when the module's code reaches it, through calls or
    /// taken addresses, from `roots` without passing a function in `stops`.
    /// That is what the gc pass at the end keeps, so the result is the same
    /// as parsing every body.
    fn kept_bodies(
        &self,
        roots: impl IntoIterator<Item = Node>,
        stops: &HashSet<Node>,
    ) -> HashSet<usize> {
        struct Refs<'a> {
            found: &'a mut Vec<FunctionId>,
        }
        impl<'a> Visitor<'a> for Refs<'a> {
            fn visit_function_id(&mut self, function: &FunctionId) {
                self.found.push(*function);
            }
        }
        let mut queue: Vec<FunctionId> = roots
            .into_iter()
            .filter(|node| !stops.contains(node))
            .filter_map(|node| match node {
                Node::Function(id) => Some(id),
                Node::DataSymbol(_) => None,
            })
            .collect();
        let mut seen: HashSet<FunctionId> = queue.iter().copied().collect();
        while let Some(id) = queue.pop() {
            let FunctionKind::Local(local) = &self.source_module.funcs.get(id).kind else {
                continue;
            };
            let mut found = Vec::new();
            dfs_in_order(&mut Refs { found: &mut found }, local, local.entry_block());
            for child in found {
                if !stops.contains(&Node::Function(child)) && seen.insert(child) {
                    queue.push(child);
                }
            }
        }
        seen.iter().map(|id| self.fns_to_ids[id]).collect()
    }

    /// Convert functions coming in from outside the module to indirect calls to the ifunc table created in the main module
    fn convert_shared_to_imports(
        &self,
        out: &mut Module,
        segment_start: usize,
        ifuncs: &Vec<Node>,
        symbols_to_import: &HashSet<Node>,
    ) {
        let ifunc_table_id = self.load_funcref_table(out);

        let mut idx = self.entry_slots;
        for node in ifuncs {
            if let Node::Function(ifunc) = node {
                if symbols_to_import.contains(node) {
                    let ty_id = out.funcs.get(*ifunc).ty();
                    let stub = (idx + segment_start) as _;
                    out.funcs.get_mut(*ifunc).kind =
                        self.make_stub_funcs(out, ifunc_table_id, ty_id, stub);
                }

                idx += 1;
            }
        }
    }

    /// Convert split import functions to local functions that call an indirect function that will
    /// be filled in from the loaded split module.
    ///
    /// This is because these imports are going to be delayed until the split module is loaded
    /// and loading in the main module these as imports won't be possible since the imports won't
    /// be resolved until the split module is loaded.
    fn create_ifunc_table(
        &self,
        out: &mut Module,
        unused_symbols: &HashSet<Node>,
        dummy_func: FunctionId,
    ) {
        let ifunc_table = self.load_funcref_table(out);

        out.exports.add("__indirect_function_table", ifunc_table);

        // Expand the ifunc table to accommodate the new ifuncs
        let segment_start = self
            .expand_ifunc_table_max(
                out,
                ifunc_table,
                self.entry_slots + self.shared_symbols.len(),
            )
            .expect("failed to expand ifunc table");

        // Delete the split import functions and replace them with local functions
        //
        // Start by pushing all the shared imports into the list
        // These don't require an additional stub function
        let mut ifuncs = vec![];

        // Each entry's slot holds a placeholder until its module fills it in.
        // A split point's import becomes a local function calling through
        // the slot; a lazy export is taken over by such a function, so the
        // page's call reaches the module once it is in.
        for split in &self.split_points {
            for entry in &split.entries {
                let stub_idx = (segment_start + entry.slot) as i32;
                if let Some(export) = entry.lazy_export {
                    let ty_id = out.funcs.get(entry.func).ty();
                    let stub = self.make_stub_funcs(out, ifunc_table, ty_id, stub_idx);
                    let FunctionKind::Local(stub) = stub else {
                        unreachable!("a stub is local");
                    };
                    let stub = out.funcs.add_local(stub);
                    out.exports.get_mut(export).item = ExportItem::Function(stub);
                } else if let Some(point) = &split.point {
                    let ty_id = out.funcs.get(point.import_func).ty();
                    out.funcs.get_mut(point.import_func).kind =
                        self.make_stub_funcs(out, ifunc_table, ty_id, stub_idx);
                    out.imports.delete(point.import_id);
                }
                ifuncs.push(dummy_func);
            }
        }

        // Add the stub functions to the ifunc table
        // The callers of these functions will call the stub instead of the import.
        // A shared chunk's function, which this module no longer has, gets a
        // hole the chunk fills when it loads.
        for func in self.shared_symbols.iter() {
            if let Node::Function(id) = func {
                ifuncs.push(if unused_symbols.contains(func) {
                    dummy_func
                } else {
                    *id
                });
            }
        }

        // Now add segments to the ifunc table
        out.tables
            .get_mut(ifunc_table)
            .elem_segments
            .insert(out.elements.add(
                ElementKind::Active {
                    table: ifunc_table,
                    offset: ConstExpr::Value(ir::Value::I32(segment_start as _)),
                },
                ElementItems::Functions(ifuncs),
            ));
    }

    /// Re-export the memories, globals, and other items from the main module to the side modules
    fn re_export_items(&self, out: &mut Module) {
        // Re-export memories
        for (idx, memory) in out.memories.iter().enumerate() {
            let name = memory
                .name
                .clone()
                .unwrap_or_else(|| format!("__memory_{}", idx));
            out.exports.add(&name, memory.id());
        }

        // Re-export globals
        for (idx, global) in out.globals.iter().enumerate() {
            let global_name = format!("__global__{idx}");
            out.exports.add(&global_name, global.id());
        }

        // Export any tables
        for (idx, table) in out.tables.iter().enumerate() {
            if table.element_ty != RefType::Funcref {
                let table_name = format!("__imported_table_{}", idx);
                out.exports.add(&table_name, table.id());
            }
        }
    }

    fn prune_main_symbols(&self, out: &mut Module, unused_symbols: &HashSet<Node>) -> Result<()> {
        // Wipe the split point exports
        for point in self
            .split_points
            .iter()
            .filter_map(|split| split.point.as_ref())
        {
            // it's okay that we're not re-mapping IDs since this is just used by the main module
            out.exports.delete(point.export_id);
        }

        // And then any actual symbols from the callgraph
        for symbol in unused_symbols.iter().cloned() {
            match symbol {
                // Simply delete functions
                Node::Function(id) => {
                    out.funcs.delete(id);
                }

                // Static data is placed by data.rs instead: the call
                // graph's code-to-data edges come through name pairing, and
                // a data symbol misattributed to a split module was cleared
                // here while main still read it (wrong results, no trap).
                Node::DataSymbol(_) => {}
            }
        }

        Ok(())
    }

    // 2.1 Create a dummy func that will be overridden later as modules pop in
    // 2.2 swap the segment entries with the dummy func, leaving hole in its placed that will be filled in later
    fn replace_segments_with_holes(
        &self,
        out: &mut Module,
        unused_symbols: &HashSet<Node>,
        dummy_func: FunctionId,
    ) {
        for element in out.elements.iter_mut() {
            match &mut element.items {
                ElementItems::Functions(vec) => {
                    for item in vec.iter_mut() {
                        if unused_symbols.contains(&Node::Function(*item)) {
                            *item = dummy_func;
                        }
                    }
                }
                ElementItems::Expressions(_ref_type, const_exprs) => {
                    for item in const_exprs.iter_mut() {
                        if let &mut ConstExpr::RefFunc(id) = item {
                            if unused_symbols.contains(&Node::Function(id)) {
                                *item = ConstExpr::RefFunc(dummy_func);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Creates the jump points
    fn create_ifunc_initializers(&self, out: &mut Module, unique_symbols: &HashSet<Node>) {
        let ifunc_table = self.load_funcref_table(out);

        // The segments are emitted in this map's order: by table offset, so
        // the same input always yields the same module bytes.
        let mut initializers = BTreeMap::new();
        for segment in out.elements.iter_mut() {
            let ElementKind::Active { offset, .. } = &mut segment.kind else {
                continue;
            };

            let ConstExpr::Value(ir::Value::I32(offset)) = offset else {
                continue;
            };

            match &segment.items {
                ElementItems::Functions(vec) => {
                    for (idx, id) in vec.iter().enumerate() {
                        if unique_symbols.contains(&Node::Function(*id)) {
                            initializers
                                .insert(*offset + idx as i32, ElementItems::Functions(vec![*id]));
                        }
                    }
                }

                ElementItems::Expressions(ref_type, const_exprs) => {
                    for (idx, expr) in const_exprs.iter().enumerate() {
                        if let ConstExpr::RefFunc(id) = expr {
                            if unique_symbols.contains(&Node::Function(*id)) {
                                initializers.insert(
                                    *offset + idx as i32,
                                    ElementItems::Expressions(
                                        *ref_type,
                                        vec![ConstExpr::RefFunc(*id)],
                                    ),
                                );
                            }
                        }
                    }
                }
            }
        }

        // Wipe away references to these segments
        for table in out.tables.iter_mut() {
            table.elem_segments.clear();
        }

        // Wipe away the element segments themselves
        let segments_to_delete: Vec<_> = out.elements.iter().map(|e| e.id()).collect();
        for id in segments_to_delete {
            out.elements.delete(id);
        }

        let ifunc_table_ = out.tables.get_mut(ifunc_table);
        for (offset, items) in initializers {
            let kind = ElementKind::Active {
                table: ifunc_table,
                offset: ConstExpr::Value(ir::Value::I32(offset)),
            };

            ifunc_table_
                .elem_segments
                .insert(out.elements.add(kind, items));
        }
    }

    /// Fill this module's entries' slots in, and turn what it calls outside
    /// itself into calls through the table.
    fn add_split_imports(
        &self,
        out: &mut Module,
        entries: &[(usize, FunctionId)],
        symbols_to_import: &HashSet<Node>,
        ifuncs: &Vec<Node>,
    ) {
        let ifunc_table_id = self.load_funcref_table(out);
        let segment_start = self
            .expand_ifunc_table_max(out, ifunc_table_id, self.entry_slots + ifuncs.len())
            .unwrap();

        for (slot, func) in entries {
            out.tables
                .get_mut(ifunc_table_id)
                .elem_segments
                .insert(out.elements.add(
                    ElementKind::Active {
                        table: ifunc_table_id,
                        offset: ConstExpr::Value(ir::Value::I32((segment_start + slot) as i32)),
                    },
                    ElementItems::Functions(vec![*func]),
                ));
        }

        self.convert_shared_to_imports(out, segment_start, ifuncs, symbols_to_import);
    }

    fn delete_main_funcs_from_split(&self, out: &mut Module, symbols_to_delete: &HashSet<Node>) {
        for node in symbols_to_delete {
            if let Node::Function(id) = *node {
                // if out.exports.get_exported_func(id).is_none() {
                out.funcs.delete(id);
                // }
            }
        }
    }

    /// Remove un-needed stuff and then hoist
    fn prune_split_module(&self, out: &mut Module) {
        // Clear the module's start/main
        if let Some(start) = out.start.take() {
            if let Some(export) = out.exports.get_exported_func(start) {
                out.exports.delete(export.id());
            }
        }

        // A split module fills its entries of the start module's table, which
        // it imports below; a module that calls nothing indirectly was linked
        // with no table, so one is made here to be imported.
        self.load_funcref_table(out);

        // We're going to import the funcref table, so wipe it altogether
        for table in out.tables.iter_mut() {
            table.elem_segments.clear();
        }

        // Wipe all our imports - we're going to use a different set of imports
        let all_imports: HashSet<_> = out.imports.iter().map(|i| i.id()).collect();
        for import_id in all_imports {
            out.imports.delete(import_id);
        }

        // Wipe away memories
        let all_memories: Vec<_> = out.memories.iter().map(|m| m.id()).collect();
        for memory_id in all_memories {
            out.memories.get_mut(memory_id).data_segments.clear();
        }

        let exports = out.exports.iter().map(|e| e.id()).collect::<Vec<_>>();
        for export_id in exports {
            out.exports.delete(export_id);
        }

        // Convert the tables to imports.
        // Should be as simple as adding a new import and then writing the `.import` field
        for (idx, table) in out.tables.iter_mut().enumerate() {
            let name = table.name.clone().unwrap_or_else(|| {
                if table.element_ty == RefType::Funcref {
                    "__indirect_function_table".to_string()
                } else {
                    format!("__imported_table_{}", idx)
                }
            });
            let import = out.imports.add("__wasm_split", &name, table.id());
            table.import = Some(import);
        }

        // Convert the memories to imports
        // Should be as simple as adding a new import and then writing the `.import` field
        for (idx, memory) in out.memories.iter_mut().enumerate() {
            let name = memory
                .name
                .clone()
                .unwrap_or_else(|| format!("__memory_{}", idx));
            let import = out.imports.add("__wasm_split", &name, memory.id());
            memory.import = Some(import);
        }

        // Convert the globals to imports
        // We might not use the global, so if we don't, we can just get
        let global_ids: Vec<_> = out.globals.iter().map(|t| t.id()).collect();
        for (idx, global_id) in global_ids.into_iter().enumerate() {
            let global = out.globals.get_mut(global_id);
            let global_name = format!("__global__{idx}");
            let import = out.imports.add("__wasm_split", &global_name, global.id());
            global.kind = GlobalKind::Import(import);
        }
    }

    fn make_dummy_func(&self, out: &mut Module) -> FunctionId {
        let mut b = FunctionBuilder::new(&mut out.types, &[], &[]);
        b.name("dummy".into()).func_body().unreachable();
        b.finish(vec![], &mut out.funcs)
    }

    /// Load the funcref table from the main module. This *should* exist for all modules created by
    /// Rustc or Wasm-Bindgen, but we create it if it doesn't exist.
    fn load_funcref_table(&self, out: &mut Module) -> TableId {
        let ifunc_table = out
            .tables
            .iter()
            .find(|t| t.element_ty == RefType::Funcref)
            .map(|t| t.id());

        if let Some(table) = ifunc_table {
            table
        } else {
            out.tables.add_local(false, 0, None, RefType::Funcref)
        }
    }

    /// Convert the imported function to a local function that calls an indirect function from the table
    ///
    /// This will enable the main module (and split modules) to call functions from outside their own module.
    /// The functions might not exist when the main module is loaded, so we'll register some elements
    /// that fill those in eventually.
    fn make_stub_funcs(
        &self,
        out: &mut Module,
        table: TableId,
        ty_id: TypeId,
        table_idx: i32,
    ) -> FunctionKind {
        // Convert the import function to a local function that calls the indirect function from the table
        let ty = out.types.get(ty_id);

        let params = ty.params().to_vec();
        let results = ty.results().to_vec();
        let args: Vec<_> = params.iter().map(|ty| out.locals.add(*ty)).collect();

        // New function that calls the indirect function
        let mut builder = FunctionBuilder::new(&mut out.types, &params, &results);
        let mut body = builder.name("stub".into()).func_body();

        for arg in args.iter() {
            body.local_get(*arg);
        }

        // And then the address of the indirect function
        body.instr(ir::Instr::Const(ir::Const {
            value: ir::Value::I32(table_idx),
        }));

        // And call it
        body.instr(ir::Instr::CallIndirect(ir::CallIndirect {
            ty: ty_id,
            table,
        }));

        FunctionKind::Local(builder.local_func(args))
    }

    /// Expand the ifunc table to accommodate the new ifuncs
    ///
    /// returns the old maximum
    fn expand_ifunc_table_max(
        &self,
        out: &mut Module,
        table: TableId,
        num_ifuncs: usize,
    ) -> Option<usize> {
        let ifunc_table_ = out.tables.get_mut(table);
        // A module that calls nothing indirectly has no table, and the one
        // made for it ([`Self::load_funcref_table`]) has no maximum: it holds
        // its initial size.
        let max = ifunc_table_.maximum.unwrap_or(ifunc_table_.initial);
        ifunc_table_.maximum = Some(max + num_ifuncs as u64);
        ifunc_table_.initial += num_ifuncs as u64;
        usize::try_from(max).ok()
    }

    // only keep the target-features and names section so wasm-opt can use it to optimize the output
    fn remove_custom_sections(&self, out: &mut Module) {
        let sections_to_delete = out
            .customs
            .iter()
            .filter_map(|(id, section)| {
                if section.name() == "target_features" {
                    None
                } else {
                    Some(id)
                }
            })
            .collect::<Vec<_>>();

        for id in sections_to_delete {
            out.customs.delete(id);
        }
    }

    /// Group the code several split modules reach, and the start module does
    /// not, into shared chunks, so each copy is downloaded once instead of
    /// with every module that needs it. Functions are grouped by exactly
    /// which split modules reach them. A group's weight is its code plus the
    /// static data only those same modules reach, which goes with that code
    /// (`crate::data`). A group of at least [`MIN_CHUNK_BYTES`] is a chunk
    /// of its own, loaded only by those modules. A lighter group joins the
    /// chunk of the fewest modules that include all of its own, so only
    /// modules that already load that chunk fetch it; the rest share one
    /// chunk. A module loads every chunk holding a function it reaches, so
    /// any grouping is sound; this one only decides who downloads what.
    ///
    /// A chunk's functions that code outside the chunk calls or takes the
    /// address of get a slot in the function table, like the start module's
    /// functions split modules call: callers go through the slot, which the
    /// chunk fills when it loads.
    fn build_split_chunks(&mut self) -> Result<()> {
        let mut users: HashMap<Node, Vec<usize>> = HashMap::new();
        for (index, split) in self.split_points.iter().enumerate() {
            for item in split.reachable_graph.iter() {
                if !self.main_graph.contains(item) {
                    users.entry(*item).or_default().push(index);
                }
            }
        }
        users.retain(|_, splits| splits.len() > 1);

        let imported = self
            .source_module
            .funcs
            .iter()
            .filter(|f| matches!(f.kind, FunctionKind::Import(_)))
            .count();
        let bodies = code_section(self.bindgened)?
            .map(|code| code.bodies)
            .unwrap_or_default();
        let size = |node: &Node| match node {
            Node::Function(id) => self
                .fns_to_ids
                .get(id)
                .and_then(|index| index.checked_sub(imported))
                .and_then(|index| bodies.get(index))
                .map_or(0, ExactSizeIterator::len),
            Node::DataSymbol(_) => 0,
        };

        let mut groups: BTreeMap<Vec<usize>, (Vec<Node>, usize)> = BTreeMap::new();
        for (node, mut splits) in users {
            splits.sort_unstable();
            let (functions, weight) = groups.entry(splits).or_default();
            match node {
                Node::Function(_) => {
                    *weight += size(&node);
                    functions.push(node);
                }
                Node::DataSymbol(index) => {
                    *weight += self.data_sizes.get(&index).copied().unwrap_or(0);
                }
            }
        }
        groups.retain(|_, (functions, _)| !functions.is_empty());
        let (heavy, light): (Vec<_>, Vec<_>) = groups
            .into_iter()
            .partition(|(_, (_, weight))| *weight >= MIN_CHUNK_BYTES);
        let mut owners: Vec<Vec<usize>> = Vec::new();
        for (splits, (functions, _)) in heavy {
            owners.push(splits);
            self.chunks.push(functions.into_iter().collect());
        }
        let mut small = HashSet::new();
        for (splits, (functions, _)) in light {
            let host = owners
                .iter()
                .enumerate()
                .filter(|(_, owner)| {
                    splits
                        .iter()
                        .all(|split| owner.binary_search(split).is_ok())
                })
                .min_by_key(|(index, owner)| (owner.len(), *index))
                .map(|(index, _)| index);
            match host {
                Some(index) => self.chunks[index].extend(functions),
                None => small.extend(functions),
            }
        }
        if !small.is_empty() {
            self.chunks.push(small);
        }
        // Largest first, so the file names are stable for one program.
        self.chunks
            .sort_by_cached_key(|chunk| std::cmp::Reverse(chunk.iter().map(size).sum::<usize>()));

        for chunk in &self.chunks {
            for node in chunk {
                let called_from_outside = self
                    .parent_graph
                    .get(node)
                    .is_some_and(|parents| parents.iter().any(|parent| !chunk.contains(parent)));
                if called_from_outside {
                    self.shared_symbols.insert(*node);
                }
            }
        }
        // What each chunk's functions call outside it: the start module's
        // functions and other chunks', all reached through the table.
        let mut chunk_imports = Vec::with_capacity(self.chunks.len());
        for chunk in &self.chunks {
            chunk_imports.push(
                chunk
                    .iter()
                    .flat_map(|node| self.call_graph.get(node).into_iter().flatten())
                    .filter(|node| matches!(node, Node::Function(_)) && !chunk.contains(node))
                    .copied()
                    .collect::<HashSet<Node>>(),
            );
        }
        self.chunk_imports = chunk_imports;
        Ok(())
    }

    /// Read every one of the module's own functions' bodies: the functions it
    /// calls and the immediates it holds.
    ///
    /// Each body is walked on its own, so every body is walked at once, and a
    /// body an earlier run walked with the same bytes is read back instead
    /// (analysis.rs): the answers are what that body's code says, whatever the
    /// rest of the module holds.
    fn walk_bodies(
        &mut self,
        hashes: &[u64],
        content: &[u64],
        known: HashMap<usize, Body>,
    ) -> Result<()> {
        let ids = &self._ids_to_fns;
        let fns_to_ids = &self.fns_to_ids;
        let module = &self.source_module;
        let imported = self.imported;
        let locals: Vec<FunctionId> = ids[imported..].to_vec();
        self.bodies = locals
            .par_iter()
            .enumerate()
            .map(|(index, id)| {
                if let Some(body) = known.get(&index) {
                    return body.clone();
                }
                let FunctionKind::Local(local) = &module.funcs.get(*id).kind else {
                    unreachable!("a module's own functions are its local ones")
                };
                let mut found = Vec::new();
                let mut values = Vec::new();
                struct Walk<'a> {
                    found: &'a mut Vec<FunctionId>,
                    values: &'a mut Vec<u32>,
                }
                impl<'i> Visitor<'i> for Walk<'_> {
                    fn visit_function_id(&mut self, function: &walrus::FunctionId) {
                        self.found.push(*function);
                    }
                    // The immediates that can name an address, as data.rs
                    // reads them from a bound body.
                    fn visit_instr(&mut self, instr: &'i ir::Instr, _: &'i ir::InstrLocId) {
                        let arg = match instr {
                            ir::Instr::Const(ir::Const {
                                value: ir::Value::I32(value),
                            }) => {
                                self.values.push(*value as u32);
                                return;
                            }
                            ir::Instr::Load(ir::Load { arg, .. })
                            | ir::Instr::Store(ir::Store { arg, .. })
                            | ir::Instr::AtomicRmw(ir::AtomicRmw { arg, .. })
                            | ir::Instr::Cmpxchg(ir::Cmpxchg { arg, .. })
                            | ir::Instr::AtomicNotify(ir::AtomicNotify { arg, .. })
                            | ir::Instr::AtomicWait(ir::AtomicWait { arg, .. })
                            | ir::Instr::LoadSimd(ir::LoadSimd { arg, .. }) => arg,
                            _ => return,
                        };
                        self.values.push(arg.offset);
                    }
                }
                dfs_in_order(
                    &mut Walk {
                        found: &mut found,
                        values: &mut values,
                    },
                    local,
                    local.entry_block(),
                );
                let mut seen = HashSet::with_capacity(found.len());
                let callees = found
                    .iter()
                    .filter_map(|id| fns_to_ids.get(id).copied())
                    .filter(|index| seen.insert(*index))
                    .map(|index| index as u32)
                    .collect();
                values.sort_unstable();
                values.dedup();
                Body {
                    hash: hashes[index],
                    content: content[index],
                    callees,
                    values,
                }
            })
            .collect();
        Ok(())
    }

    /// Accumulate the relocations from the original module, create a relocation map, and then convert
    /// that to our *new* module's symbols.
    fn build_call_graph(&mut self) -> Result<()> {
        let mut step = std::time::Instant::now();
        let original = ModuleWithRelocations::new(self.original)?;
        self.data_names = original.data_names.clone();
        took("read the linked module's relocations", step);
        step = std::time::Instant::now();
        self.data_sizes = original
            .data_symbols
            .iter()
            .map(|(index, symbol)| (*index, symbol.range.len()))
            .collect();

        // Distinct functions can share a name in the name section (copies of
        // one generic shim), so a name maps to every function carrying it.
        // When the copies cannot be paired, an old function maps to all new
        // ones of its name: that over-approximates the call graph, which only
        // keeps more in the main module, while mapping it to one of them
        // loses edges and the main module then calls functions moved out.
        let mut old_names: HashMap<&str, Vec<FunctionId>> = HashMap::new();
        for f in original.module.funcs.iter() {
            if let Some(name) = &f.name {
                old_names.entry(name.as_str()).or_default().push(f.id());
            }
        }

        let mut new_names: HashMap<&str, Vec<FunctionId>> = HashMap::new();
        for f in self.source_module.funcs.iter() {
            if let Some(name) = &f.name {
                new_names.entry(name.as_str()).or_default().push(f.id());
            }
        }

        let mut old_to_new: HashMap<FunctionId, Vec<FunctionId>> = HashMap::new();
        let mut new_call_graph: HashMap<Node, HashSet<Node>> = HashMap::new();

        for (new_name, new_funcs) in new_names.iter() {
            if let Some(old_funcs) = old_names.get(new_name) {
                // wasm-bindgen keeps the order of the functions it does not
                // rewrite, so equally many functions of one name pair up in
                // order. Pairing every old one with every new one would let
                // one generic shim's copy reach every other copy's callees.
                if old_funcs.len() == new_funcs.len() {
                    for (old_func, new_func) in old_funcs.iter().zip(new_funcs) {
                        old_to_new.insert(*old_func, vec![*new_func]);
                    }
                } else {
                    for old_func in old_funcs {
                        old_to_new.insert(*old_func, new_funcs.clone());
                    }
                }
            } else {
                for new_func in new_funcs {
                    new_call_graph.insert(Node::Function(*new_func), HashSet::new());
                }
            }
        }

        // A function whose address is taken sits in the table, and wasm-bindgen
        // keeps the table's layout (data holds table indices). The slot is
        // the exact pairing; names are not: `#[inline]` functions get a local
        // copy per codegen unit, all under one name, and a vtable in main
        // that reaches one copy must keep that copy, not whichever the name
        // pairing chose, or main calls into a hole.
        let new_slots = table_slots(&self.source_module);
        for (slot, old_func) in table_slots(&original.module) {
            if let Some(new_func) = new_slots.get(&slot) {
                old_to_new.insert(old_func, vec![*new_func]);
            }
        }

        let get_old = |old: &Node| -> Vec<Node> {
            match old {
                Node::Function(id) => old_to_new
                    .get(id)
                    .map(|new_ids| new_ids.iter().map(|id| Node::Function(*id)).collect())
                    .unwrap_or_default(),
                Node::DataSymbol(id) => vec![Node::DataSymbol(*id)],
            }
        };

        // the symbols that we can't find in the original module touch functions that unfortunately
        // we can't figure out where should exist in the call graph
        //
        // we're going to walk and find every child we possibly can and then add it to the call graph
        // at the root
        //
        // wasm-bindgen will dissolve describe functions into the shim functions, but we don't have a
        // sense of lining up old to new, so we just assume everything ends up in the main chunk.
        let mut lost_children = HashSet::new();
        let mut call_graph: HashMap<Node, HashSet<Node>> = HashMap::new();
        for (old, children) in original.call_graph.iter() {
            let news = get_old(old);
            // If the old function isn't in the new module, we need to move all its descendents into the main chunk
            if news.is_empty() {
                // Descend only through functions that are gone too: a child
                // still in the new module brings its own edges, and walking
                // past it would pin everything it reaches (wasm-bindgen's
                // closure descriptors reach every closure's body) to main.
                let mut stack: Vec<Node> = children.iter().copied().collect();
                while let Some(node) = stack.pop() {
                    if !lost_children.insert(node) {
                        continue;
                    }
                    if !get_old(&node).is_empty() {
                        continue;
                    }
                    if let Some(children) = original.call_graph.get(&node) {
                        stack.extend(children.iter().copied());
                    }
                }
                continue;
            }

            let new_children: Vec<Node> = children.iter().flat_map(&get_old).collect();
            for new in news {
                call_graph
                    .entry(new)
                    .or_default()
                    .extend(new_children.iter().copied());
            }
        }
        self.call_graph = call_graph;

        let mut recovered_children = HashSet::new();
        for lost in lost_children {
            match lost {
                // Functions need to be found - the wasm describe functions are usually completely dissolved
                Node::Function(id) => {
                    let func = original.module.funcs.get(id);
                    let name = func.name.as_ref().unwrap();
                    for entry in new_names.get(name.as_str()).into_iter().flatten() {
                        recovered_children.insert(Node::Function(*entry));
                    }
                }

                // Data symbols are unchanged and fine to remap
                Node::DataSymbol(id) => {
                    recovered_children.insert(Node::DataSymbol(id));
                }
            }
        }

        // We're going to attach the recovered children to the main function.
        // Also attach the truly new symbols the table holds to it:
        // wasm-bindgen's shims, which no code calls by name. A new function
        // the code calls or exports is reached from its caller or as an
        // export (the walk below), so a lazy export's shims go with its
        // module.
        let mut attached = recovered_children;
        let in_table: HashSet<FunctionId> = new_slots.values().copied().collect();
        for (name, new) in new_names.iter() {
            if !old_names.contains_key(name) {
                attached.extend(
                    new.iter()
                        .filter(|id| in_table.contains(id))
                        .map(|id| Node::Function(*id)),
                );
            }
        }
        // A library module (`cdylib`) has no `main`: its wasm-bindgen start
        // export is always in the main module's graph, so it anchors them. A
        // module wasm-bindgen never saw (the app models, `crate::models`)
        // loses and gains nothing between its two parses, so it needs no
        // anchor and has none.
        if !attached.is_empty() {
            let main_fn = self
                .source_module
                .funcs
                .by_name("main")
                .or_else(|| self.source_module.exports.get_func("__wbindgen_start").ok())
                .context("Failed to find `main` or `__wbindgen_start` - was this built with LTO, --emit-relocs, and symbols?")?;
            new_call_graph
                .entry(Node::Function(main_fn))
                .or_default()
                .extend(attached);
        }
        for (node, children) in new_call_graph {
            self.call_graph.entry(node).or_default().extend(children);
        }

        took("paired the linked functions with the bound ones", step);

        // The bodies' answers are read before the plan, which is half made
        // from them (analysis.rs, plan.rs): the functions each one calls join
        // the graph here, under the edges the relocations gave above.
        {
            let ids = &self._ids_to_fns;
            let locals: Vec<FunctionId> = ids[self.imported..].to_vec();
            let bodies = std::mem::take(&mut self.bodies);
            for (index, body) in bodies.iter().enumerate() {
                if body.callees.is_empty() {
                    continue;
                }
                self.call_graph
                    .entry(Node::Function(locals[index]))
                    .or_default()
                    .extend(
                        body.callees
                            .iter()
                            .map(|index| Node::Function(ids[*index as usize])),
                    );
            }
            self.bodies = bodies;
        }

        self.derive_from_graph()
    }

    /// Convert this set of nodes to reference the new module
    fn remap_ids(&self, set: &HashSet<Node>, ids_to_fns: &[FunctionId]) -> HashSet<Node> {
        let mut out = HashSet::with_capacity(set.len());
        for node in set {
            out.insert(self.remap_id(ids_to_fns, node));
        }
        out
    }

    fn remap_id(&self, ids_to_fns: &[id_arena::Id<walrus::Function>], node: &Node) -> Node {
        match node {
            // Remap the function IDs
            Node::Function(id) => Node::Function(ids_to_fns[self.fns_to_ids[id]]),
            // data symbols don't need remapping
            Node::DataSymbol(id) => Node::DataSymbol(*id),
        }
    }
}

/// Where a module's code section and its function bodies are.
pub(crate) struct CodeSection {
    /// The section's id byte.
    start: usize,
    /// Its contents, after the size.
    pub(crate) contents: usize,
    end: usize,
    /// Each defined function's body, after its size.
    pub(crate) bodies: Vec<Range<usize>>,
}

pub(crate) fn read_leb(bytes: &[u8], at: &mut usize) -> Result<usize> {
    let mut value = 0usize;
    for shift in (0..35).step_by(7) {
        let byte = *bytes.get(*at).context("truncated module")?;
        *at += 1;
        value |= usize::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    anyhow::bail!("malformed LEB128 at byte {at}")
}

fn push_leb(out: &mut Vec<u8>, mut value: usize) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// `key` over what another module reads of a module: its types, its imports,
/// its functions' signatures, its tables, memories, globals, its exports and
/// the element segments that place functions in the table. Its code, its
/// static data and its custom sections are its own, and no module that
/// imports it sees them.
fn interface(key: &mut Key, bytes: &[u8]) -> Result<()> {
    let mut skipped = Vec::new();
    let mut at = 8;
    while at < bytes.len() {
        let start = at;
        let id = bytes[at];
        at += 1;
        let size = read_leb(bytes, &mut at)?;
        at = (at + size).min(bytes.len());
        // Custom sections (0), the code (10), the data (11) and the data
        // count (12).
        if id == 0 || (10..=12).contains(&id) {
            skipped.push(start..at);
        }
    }
    key_sections(key, bytes, &skipped)
}

/// The sections a module's emission reads function by function or range by
/// range, not as bytes: its code (10, its bodies are keyed one by one), its
/// data (11, each module writes the ranges the plan hands it) and the data
/// count (12, which follows from the data).
pub(crate) fn sections_not_read(bytes: &[u8]) -> Result<Vec<Range<usize>>> {
    let mut skipped = Vec::new();
    let mut at = 8;
    while at < bytes.len() {
        let start = at;
        let id = bytes[at];
        at += 1;
        let size = read_leb(bytes, &mut at)?;
        at += size;
        if (10..=12).contains(&id) {
            skipped.push(start..at.min(bytes.len()));
        }
    }
    Ok(skipped)
}

/// `key` over `bytes` without `skipped`, which must be in order, and without
/// the local variables' names in its name section: a local's name reaches no
/// module's bytes (the tests hold a module whose only change is one of them
/// to the same modules), so a change to one is no change of what a key
/// covers. Only a bundle compiled with debug information carries them - the
/// desktop bundle's `wasm-split` profiles emit the functions' names alone -
/// and a key that holds them re-emits every module when one is renamed.
///
/// Everything else is hashed as it is. This is the analysis's structure
/// (analysis.rs), which decides whose body answers may be read back: a
/// function added or renamed moves it, so another module's answers are never
/// read for this one. An emission is keyed otherwise (cache.rs), by content
/// rather than by these sections, so a renumbering does not re-emit a module
/// whose own code did not change.
pub(crate) fn hash_sections(key: &mut Key, bytes: &[u8], skipped: &[Range<usize>]) -> Result<()> {
    let mut at = 0;
    for range in skipped {
        if range.start > at {
            hash_locals_out(key, bytes, at..range.start)?;
        }
        at = at.max(range.end);
    }
    if at < bytes.len() {
        hash_locals_out(key, bytes, at..bytes.len())?;
    }
    Ok(())
}

/// The name section's subsection id of a function's local variables' names.
const LOCAL_NAMES: u8 = 2;

/// [`hash_sections`] over one section: a section as it is, but for a name
/// section, whose local variables' names are left out.
fn hash_locals_out(key: &mut Key, bytes: &[u8], section: Range<usize>) -> Result<()> {
    let mut at = section.start;
    let id = bytes[at];
    at += 1;
    let size = read_leb(bytes, &mut at)?;
    let name = if id == 0 {
        let mut name_at = at;
        let len = read_leb(bytes, &mut name_at)?;
        bytes.get(name_at..name_at + len)
    } else {
        None
    };
    if name != Some(&b"name"[..]) {
        key.add(&bytes[section]);
        return Ok(());
    }
    key.add(&bytes[section.start..at]);
    let end = (at + size).min(bytes.len());
    while at < end {
        let start = at;
        let id = bytes[at];
        at += 1;
        let size = read_leb(bytes, &mut at)?;
        at = (at + size).min(end);
        if id == LOCAL_NAMES {
            continue;
        }
        key.add(&bytes[start..at]);
    }
    Ok(())
}

pub(crate) fn code_section(bytes: &[u8]) -> Result<Option<CodeSection>> {
    // Sections follow the 8-byte header: an id byte, a LEB128 size, the
    // contents. The code section's id is 10.
    let mut at = 8;
    while at < bytes.len() {
        let start = at;
        let id = bytes[at];
        at += 1;
        let size = read_leb(bytes, &mut at)?;
        let contents = at;
        let end = at + size;
        if id != 10 {
            at = end;
            continue;
        }
        let count = read_leb(bytes, &mut at)?;
        let mut bodies = Vec::with_capacity(count);
        for _ in 0..count {
            let len = read_leb(bytes, &mut at)?;
            bodies.push(at..at + len);
            at += len;
        }
        anyhow::ensure!(at == end, "code section size disagrees with its bodies");
        return Ok(Some(CodeSection {
            start,
            contents,
            end,
            bodies,
        }));
    }
    Ok(None)
}

/// `bytes` with the body of every defined function whose index is not in
/// `keep` replaced by `unreachable`. Function indices, types and every other
/// section stay as they are.
pub(crate) fn keep_function_bodies(bytes: &[u8], keep: &HashSet<usize>) -> Result<Vec<u8>> {
    // Locals: none; `unreachable`; `end`.
    const STUB: [u8; 4] = [3, 0x00, 0x00, 0x0b];

    let Some(code) = code_section(bytes)? else {
        return Ok(bytes.to_vec());
    };
    let imported = imported_functions(bytes)?;
    let mut contents = Vec::with_capacity(code.end - code.contents);
    push_leb(&mut contents, code.bodies.len());
    for (index, body) in (imported..).zip(&code.bodies) {
        if keep.contains(&index) {
            push_leb(&mut contents, body.len());
            contents.extend_from_slice(&bytes[body.clone()]);
        } else {
            contents.extend_from_slice(&STUB);
        }
    }
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(&bytes[..code.start]);
    out.push(10);
    push_leb(&mut out, contents.len());
    out.extend_from_slice(&contents);
    out.extend_from_slice(&bytes[code.end..]);
    Ok(out)
}

/// How many functions `bytes` imports: the index its first defined
/// function has.
fn imported_functions(bytes: &[u8]) -> Result<usize> {
    let mut imported = 0usize;
    for payload in wasmparser::Parser::new(0).parse_all(bytes) {
        match payload? {
            Payload::ImportSection(reader) => {
                for import in reader {
                    if matches!(import?.ty, wasmparser::TypeRef::Func(_)) {
                        imported += 1;
                    }
                }
            }
            Payload::CodeSectionStart { .. } => break,
            _ => {}
        }
    }
    Ok(imported)
}

/// Parse a module and return the mapping of index to FunctionID.
/// We'll use this mapping to remap ModuleIDs
pub(crate) fn parse_module_with_ids(
    bindgened: &[u8],
) -> Result<(Module, Vec<FunctionId>, HashMap<FunctionId, usize>)> {
    let ids = Arc::new(RwLock::new(Vec::new()));
    let ids_ = ids.clone();
    let module = Module::from_buffer_with_config(
        bindgened,
        ModuleConfig::new().on_parse(move |_m, our_ids| {
            let mut ids = ids_.write().expect("No shared writers");
            let mut idx = 0;
            while let Ok(entry) = our_ids.get_func(idx) {
                ids.push(entry);
                idx += 1;
            }

            Ok(())
        }),
    )?;
    let mut ids_ = ids.write().expect("No shared writers");
    let mut ids = vec![];
    std::mem::swap(&mut ids, &mut *ids_);

    let mut fns_to_ids = HashMap::new();
    for (idx, id) in ids.iter().enumerate() {
        fns_to_ids.insert(*id, idx);
    }

    Ok((module, ids, fns_to_ids))
}

/// A module split off the start module, and every way into it.
#[derive(Debug, Clone)]
pub struct SplitPoint {
    module_name: String,
    /// The Rust side's split point into the module, when it declares one.
    point: Option<RustPoint>,
    /// The functions the module answers through the function table, each
    /// at its own slot past the table's linked layout.
    entries: Vec<Entry>,
    reachable_graph: HashSet<Node>,
}

/// A split point the Rust side declares as a pair of functions named by
/// the splitter's convention:
///
/// `__wasm_split_00___<module>___00_<import|export>_<hash>_<function>`
///
/// The start module calls the import, which becomes a call through the
/// table once the module filled the export's slot in.
#[derive(Debug, Clone)]
struct RustPoint {
    import_id: ImportId,
    import_func: FunctionId,
    export_id: ExportId,
    hash_name: String,
    component_name: String,
}

/// One function a split module answers through the table: a split point's
/// export, or a module export the page calls once the module is in
/// ([`LAZY_EXPORTS`]), whose export in the start module then calls through
/// the table.
#[derive(Debug, Clone)]
struct Entry {
    func: FunctionId,
    slot: usize,
    /// The start module's export of `func`, which a stub takes over.
    lazy_export: Option<ExportId>,
}

/// The custom section in which the linked module names the exports each
/// split module answers: one `<module> <export>` per line, `<export>`
/// ending in `*` for every export with that prefix. Such an export calls
/// into its module, which the page loads (`load(module)` of
/// `__wasm_split.js`) before it calls the export.
pub const LAZY_EXPORTS: &str = "tilo_split_exports";

/// The lines of the linked module's [`LAZY_EXPORTS`] sections.
pub(crate) fn lazy_exports(original: &[u8]) -> Result<Vec<(String, String)>> {
    let mut found = Vec::new();
    for payload in wasmparser::Parser::new(0).parse_all(original) {
        if let Payload::CustomSection(section) = payload? {
            if section.name() != LAZY_EXPORTS {
                continue;
            }
            let text = std::str::from_utf8(section.data()).context("lazy exports are not UTF-8")?;
            for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
                let (module, export) = line.split_once(' ').with_context(|| {
                    format!("lazy export line {line:?} is not `<module> <export>`")
                })?;
                found.push((module.to_owned(), export.trim().to_owned()));
            }
        }
    }
    Ok(found)
}

/// The modules to split off: the Rust side's split points, and the modules
/// `lazy` names exports of, merged by module name.
fn accumulate_split_points(module: &Module, lazy: &[(String, String)]) -> Result<Vec<SplitPoint>> {
    let mut points: BTreeMap<String, SplitPoint> = BTreeMap::new();
    fn point<'a>(points: &'a mut BTreeMap<String, SplitPoint>, name: &str) -> &'a mut SplitPoint {
        points.entry(name.to_owned()).or_insert_with(|| SplitPoint {
            module_name: name.to_owned(),
            point: None,
            entries: Vec::new(),
            reachable_graph: HashSet::new(),
        })
    }
    for import in module.imports.iter().sorted_by(|a, b| a.name.cmp(&b.name)) {
        if !import.name.starts_with("__wasm_split_00") {
            continue;
        }
        let ImportKind::Function(import_func) = import.kind else {
            continue;
        };
        let remain = import.name.trim_start_matches("__wasm_split_00___");
        let (module_name, rest) = remain
            .split_once("___00")
            .with_context(|| format!("split import {} has no module", import.name))?;
        let (hash, fn_name) = rest
            .trim_start_matches("_import_")
            .split_once('_')
            .with_context(|| format!("split import {} has no hash", import.name))?;
        let export_name = format!("__wasm_split_00___{module_name}___00_export_{hash}_{fn_name}");
        let export_func = module
            .exports
            .get_func(&export_name)
            .with_context(|| format!("no export {export_name} for split import {}", import.name))?;
        let export = module
            .exports
            .get_exported_func(export_func)
            .expect("exported");
        let split = point(&mut points, module_name);
        anyhow::ensure!(
            split.point.is_none(),
            "module {module_name} has two split points"
        );
        split.point = Some(RustPoint {
            import_id: import.id(),
            import_func,
            export_id: export.id(),
            hash_name: hash.to_owned(),
            component_name: fn_name.to_owned(),
        });
        split.entries.push(Entry {
            func: export_func,
            slot: 0,
            lazy_export: None,
        });
    }
    for (module_name, pattern) in lazy {
        let matches: Vec<_> = module
            .exports
            .iter()
            .filter(|export| match pattern.strip_suffix('*') {
                Some(prefix) => export.name.starts_with(prefix),
                None => export.name == *pattern,
            })
            .filter_map(|export| match export.item {
                ExportItem::Function(func) => Some((export.id(), func)),
                _ => None,
            })
            .sorted_by_key(|(_, func)| *func)
            .collect();
        // An export the runtime no longer has is nothing to move: say so,
        // so the list is kept, without failing the bundle.
        if matches.is_empty() {
            eprintln!("wasm-split: lazy export {pattern} of {module_name} is not exported");
        }
        let split = point(&mut points, module_name);
        for (export, func) in matches {
            split.entries.push(Entry {
                func,
                slot: 0,
                lazy_export: Some(export),
            });
        }
    }
    let mut slot = 0;
    let mut points: Vec<SplitPoint> = points.into_values().collect();
    for split in &mut points {
        for entry in &mut split.entries {
            entry.slot = slot;
            slot += 1;
        }
    }
    Ok(points)
}

#[derive(Debug, PartialEq, Eq, Hash, Copy, PartialOrd, Ord, Clone)]
pub enum Node {
    Function(FunctionId),
    DataSymbol(usize),
}

/// The node a plan's is: the same function, in this run's parse of the bound
/// module, whose functions a plan names by index (plan.rs).
fn from_plan_node(ids: &[FunctionId], node: crate::plan::Node) -> Node {
    let (index, data) = crate::plan::node_index(node);
    if data {
        Node::DataSymbol(index)
    } else {
        Node::Function(ids[index])
    }
}

fn reachable_graph(deps: &HashMap<Node, HashSet<Node>>, roots: &HashSet<Node>) -> HashSet<Node> {
    let mut queue: VecDeque<Node> = roots.iter().copied().collect();
    let mut reachable = HashSet::<Node>::new();
    let mut parents = HashMap::<Node, Node>::new();

    while let Some(node) = queue.pop_front() {
        reachable.insert(node);
        let Some(children) = deps.get(&node) else {
            continue;
        };
        for child in children {
            if reachable.contains(child) {
                continue;
            }
            parents.entry(*child).or_insert(node);
            queue.push_back(*child);
        }
    }

    reachable
}

/// For each split module, then each shared chunk (as data.rs numbers
/// them), the modules always in memory while its code runs: a split module
/// and the chunks it loads before it; a chunk, and what every module loading
/// it loads, since its code only runs once one of those is in.
fn loaded_with(module_parts: &[SplitParts], chunks: usize) -> Vec<Vec<usize>> {
    let modules = module_parts.len();
    let mut loaded: Vec<Vec<usize>> = module_parts
        .iter()
        .enumerate()
        .map(|(index, parts)| {
            let mut set: Vec<usize> = std::iter::once(index)
                .chain(parts.relies_on_chunks.iter().map(|chunk| modules + chunk))
                .collect();
            set.sort_unstable();
            set
        })
        .collect();
    for chunk in 0..chunks {
        let me = modules + chunk;
        let mut always: Option<Vec<usize>> = None;
        for set in loaded[..modules].iter().filter(|set| set.contains(&me)) {
            always = Some(match always {
                None => set.clone(),
                Some(mut always) => {
                    always.retain(|module| set.contains(module));
                    always
                }
            });
        }
        // A chunk no module loads never runs; it counts on itself alone.
        let mut set = always.unwrap_or_default();
        set.retain(|module| *module >= modules);
        if !set.contains(&me) {
            set.push(me);
        }
        set.sort_unstable();
        loaded.push(set);
    }
    loaded
}

/// Drop the start module's table entries for slots a split module or
/// shared chunk fills when it loads (`hole`, a function that traps): an
/// empty slot traps the same way when called, and costs no bytes. A run of
/// holes too short to pay for the segment it splits stays.
pub(crate) fn leave_holes_empty(out: &mut Module, hole: FunctionId) {
    /// Holes in a row worth another segment's header (its offset and
    /// count) to leave out: each written entry costs two or three bytes.
    const MIN_HOLES: usize = 4;
    let segments: Vec<_> = out
        .elements
        .iter()
        .filter_map(|element| match (&element.kind, &element.items) {
            (
                ElementKind::Active {
                    table,
                    offset: ConstExpr::Value(ir::Value::I32(offset)),
                },
                ElementItems::Functions(items),
            ) if items.contains(&hole) => Some((element.id(), *table, *offset, items.clone())),
            _ => None,
        })
        .collect();
    for (id, table, offset, items) in segments {
        out.tables.get_mut(table).elem_segments.remove(&id);
        out.elements.delete(id);
        let mut runs: Vec<(usize, Vec<FunctionId>)> = Vec::new();
        let mut at = 0;
        while at < items.len() {
            let holes = items[at..].iter().take_while(|item| **item == hole).count();
            if holes >= MIN_HOLES || (holes > 0 && at + holes == items.len()) {
                at += holes;
                continue;
            }
            let start = at;
            // Up to the next run of holes worth leaving out.
            while at < items.len() {
                let holes = items[at..].iter().take_while(|item| **item == hole).count();
                if holes >= MIN_HOLES || (holes > 0 && at + holes == items.len()) {
                    break;
                }
                at += holes.max(1);
            }
            runs.push((start, items[start..at].to_vec()));
        }
        for (start, run) in runs {
            let element = out.elements.add(
                ElementKind::Active {
                    table,
                    offset: ConstExpr::Value(ir::Value::I32(offset + start as i32)),
                },
                ElementItems::Functions(run),
            );
            out.tables.get_mut(table).elem_segments.insert(element);
        }
    }
}

impl Splitter<'_> {
    /// Print the shortest path from a main root to the first function whose
    /// name contains `target`: why that function stays in the start module.
    fn explain_main_path(&self, target: &str) {
        let roots = self.main_roots();
        // Every root that reaches it: each is a way in to close.
        let mut stack: Vec<Node> = self
            .main_graph
            .iter()
            .filter(|node| {
                matches!(node, Node::Function(id) if self_names(&self.source_module, *id).contains(target))
            })
            .copied()
            .collect();
        let mut seen: HashSet<Node> = stack.iter().copied().collect();
        while let Some(node) = stack.pop() {
            for parent in self.parent_graph.get(&node).into_iter().flatten() {
                if seen.insert(*parent) {
                    stack.push(*parent);
                }
            }
        }
        let reached: Vec<String> = roots
            .iter()
            .filter(|root| seen.contains(root))
            .filter_map(|root| match root {
                Node::Function(id) => Some(self_names(&self.source_module, *id)),
                Node::DataSymbol(_) => None,
            })
            .sorted()
            .collect();
        eprintln!(
            "  reached from {} roots: {}",
            reached.len(),
            reached.join(" ")
        );
        let mut queue: VecDeque<Node> = roots.iter().copied().collect();
        let mut parents = HashMap::<Node, Option<Node>>::new();
        for root in &roots {
            parents.insert(*root, None);
        }
        let label = |node: &Node| match node {
            Node::Function(id) => {
                let module = &self.source_module;
                let export = module
                    .exports
                    .iter()
                    .find(|e| matches!(e.item, ExportItem::Function(f) if f == *id));
                let import = module
                    .imports
                    .iter()
                    .find(|i| matches!(i.kind, ImportKind::Function(f) if f == *id));
                let kind = match &module.funcs.get(*id).kind {
                    FunctionKind::Local(_) => "local",
                    FunctionKind::Import(_) => "import",
                    FunctionKind::Uninitialized(_) => "uninitialized",
                };
                format!(
                    "{} [{kind}{}{}]",
                    self_names(module, *id),
                    export
                        .map(|e| format!(" export {}", e.name))
                        .unwrap_or_default(),
                    import
                        .map(|i| format!(" import {}::{}", i.module, i.name))
                        .unwrap_or_default(),
                )
            }
            Node::DataSymbol(index) => format!("data symbol {index}"),
        };
        while let Some(node) = queue.pop_front() {
            if matches!(node, Node::Function(_)) && label(&node).contains(target) {
                let mut path = vec![node];
                while let Some(Some(parent)) = parents.get(path.last().unwrap()) {
                    path.push(*parent);
                }
                let mut previous: Option<Node> = None;
                for step in path.iter().rev() {
                    let direct = match (previous, step) {
                        (Some(Node::Function(parent)), Node::Function(child)) => {
                            struct Refs(Vec<FunctionId>);
                            impl<'a> Visitor<'a> for Refs {
                                fn visit_function_id(&mut self, function: &FunctionId) {
                                    self.0.push(*function);
                                }
                            }
                            match &self.source_module.funcs.get(parent).kind {
                                FunctionKind::Local(local) => {
                                    let mut refs = Refs(Vec::new());
                                    dfs_in_order(&mut refs, local, local.entry_block());
                                    if refs.0.contains(child) {
                                        " (in code)"
                                    } else {
                                        " (relocation only)"
                                    }
                                }
                                _ => "",
                            }
                        }
                        _ => "",
                    };
                    eprintln!("  {}{direct}", label(step));
                    previous = Some(*step);
                }
                return;
            }
            for child in self.call_graph.get(&node).into_iter().flatten() {
                if !parents.contains_key(child) {
                    parents.insert(*child, Some(node));
                    queue.push_back(*child);
                }
            }
        }
        eprintln!("{target}: not reachable from the main roots");
    }
}

/// The function in each slot of the module's active element segments.
fn table_slots(module: &Module) -> HashMap<i32, FunctionId> {
    let mut slots = HashMap::new();
    for element in module.elements.iter() {
        let ElementKind::Active {
            offset: ConstExpr::Value(ir::Value::I32(offset)),
            ..
        } = element.kind
        else {
            continue;
        };
        match &element.items {
            ElementItems::Functions(items) => {
                for (index, id) in items.iter().enumerate() {
                    slots.insert(offset + index as i32, *id);
                }
            }
            ElementItems::Expressions(_, items) => {
                for (index, item) in items.iter().enumerate() {
                    if let ConstExpr::RefFunc(id) = item {
                        slots.insert(offset + index as i32, *id);
                    }
                }
            }
        }
    }
    slots
}

fn self_names(module: &Module, id: FunctionId) -> String {
    module
        .funcs
        .get(id)
        .name
        .clone()
        .unwrap_or_else(|| format!("{id:?}"))
        .chars()
        .take(160)
        .collect()
}
