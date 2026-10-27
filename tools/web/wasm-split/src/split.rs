// Adapted from wasm-split-cli 0.7.10 (Dioxus, https://github.com/DioxusLabs/dioxus,
// MIT OR Apache-2.0); see ../PROVENANCE.md for what changed.

use crate::data::{Claimant, DataLayout, DataPlan};
use anyhow::{Context, Result};
use itertools::Itertools;
use rayon::prelude::{IndexedParallelIterator, IntoParallelIterator, ParallelIterator};
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
use wasmparser::{
    BinaryReader, Linking, LinkingSectionReader, Payload, RelocSectionReader, RelocationEntry,
    RelocationType, SymbolInfo,
};

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
    chunks: Vec<HashSet<Node>>,
    main_graph: HashSet<Node>,
    call_graph: HashMap<Node, HashSet<Node>>,
    parent_graph: HashMap<Node, HashSet<Node>>,
    /// Each defined data symbol's size, by its symbol index.
    data_sizes: HashMap<usize, usize>,
}

/// What one split module or shared chunk defines, imports and deletes (in
/// the source module's ids), and the function bodies it is parsed with.
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
    /// The main chunk
    pub main: SplitModule,

    /// The modules of the wasm module that were split.
    pub modules: Vec<SplitModule>,

    /// The chunks that might be imported by the main modules
    pub chunks: Vec<SplitModule>,
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
    pub fn new(original: &'a [u8], bindgened: &'a [u8]) -> Result<Self> {
        let (module, ids, fns_to_ids) = parse_module_with_ids(bindgened)?;

        let split_points = accumulate_split_points(&module);

        // Note that we can't trust the normal symbols - just the data symbols - and we can't use the data offset
        // since that's not reliable after bindgening

        let mut module = Self {
            source_module: module,
            original,
            bindgened,
            split_points,
            _ids_to_fns: ids,
            fns_to_ids,
            main_graph: Default::default(),
            chunks: Default::default(),
            call_graph: Default::default(),
            parent_graph: Default::default(),
            shared_symbols: Default::default(),
            data_sizes: Default::default(),
        };

        module.build_call_graph()?;
        module.build_split_chunks()?;

        Ok(module)
    }

    /// Split the module into multiple modules at the boundaries of split points.
    ///
    /// Note that the binaries might still be "large" at the end of this process. In practice, you
    /// need to push these binaries through wasm-bindgen and wasm-opt to take advantage of the
    /// optimizations and splitting. We perform a few steps like zero-ing out the data segments
    /// that will only be removed by the memory-packing step of wasm-opt.
    ///
    /// This returns the list of chunks, an import map, and some javascript to link everything together.
    pub fn emit(mut self) -> Result<OutputModules> {
        tracing::info!("Emitting split modules.");

        // What each split module holds is settled while the whole program is
        // parsed. The main module is emitted next and takes that parse with
        // it, so the split modules are emitted without it in memory.
        let chunk_parts = (0..self.chunks.len())
            .map(|idx| self.split_chunk_parts(idx))
            .collect::<Vec<_>>();
        let module_parts = (0..self.split_points.len())
            .map(|idx| self.split_module_parts(idx))
            .collect::<Vec<_>>();

        // Which static data each module reaches is read from the code it
        // carries (see data.rs); the main module's is known once it is
        // pruned.
        let data_layout = DataLayout::new(self.original)?;
        let data_refs = data_layout.function_refs(&self.source_module);
        let data_claimants = self.data_claimants(&chunk_parts, &module_parts);

        let (main, data) = self.emit_main_module(&data_layout, &data_refs, data_claimants)?;
        let main = merged(main)?;

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
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .context("starting the split threads")?;
        let (chunks, modules) = pool.install(|| {
            let chunks = chunk_parts
                .into_par_iter()
                .enumerate()
                .map(|(idx, parts)| self.emit_split_chunk(idx, parts, &data).and_then(merged))
                .collect::<Result<Vec<SplitModule>>>();
            let modules = module_parts
                .into_par_iter()
                .enumerate()
                .map(|(idx, parts)| self.emit_split_module(idx, parts, &data).and_then(merged))
                .collect::<Result<Vec<SplitModule>>>();
            (chunks, modules)
        });
        eprintln!(
            "wasm-split: emitted {count} modules, {threads} at a time, in {:.1} s",
            started.elapsed().as_secs_f64()
        );

        Ok(OutputModules {
            modules: modules?,
            chunks: chunks?,
            main,
        })
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
    ) -> Result<(SplitModule, DataPlan)> {
        tracing::info!("Emitting main bundle split module");

        // Perform some analysis of the module before we start messing with it
        let mut unused_symbols = self.unused_main_symbols();
        self.keep_referenced(&mut unused_symbols);

        let mut out = std::mem::take(&mut self.source_module);

        // 1. Clear out the active segments that try to initialize functions for modules we just split off.
        //    When the side modules load, they will initialize functions into the table where the "holes" are.
        self.replace_segments_with_holes(&mut out, &unused_symbols);

        // 2. Wipe away the unused functions and data symbols
        self.prune_main_symbols(&mut out, &unused_symbols)?;

        // 3. Change the functions called from split modules to be local functions that call the indirect function
        self.create_ifunc_table(&mut out, &unused_symbols);

        // 4. Re-export the memories, globals, and other stuff
        self.re_export_items(&mut out);

        // 6. Remove the reloc and linking custom sections
        self.remove_custom_sections(&mut out);

        // 7. Run the garbage collector to remove unused functions
        walrus::passes::gc::run(&mut out);

        // 8. Leave out the static data only other modules reach: the
        //    functions left are exactly the code the main module runs
        data_claimants.push((Claimant::Start, out.funcs.iter().map(|f| f.id()).collect()));
        let modules = self.split_points.len() + self.chunks.len();
        let data = data_layout.plan(&out, data_refs, data_claimants, modules);
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
            .chain([Node::Function(split.export_func)]);
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
        let split_export_func = ids_to_fns[self.fns_to_ids[&split.export_func]];

        // Do some basic cleanup of the module to make it smaller
        // This removes exports, imports, and the start function
        self.prune_split_module(&mut out);

        // Keep only the static data this module owns (see data.rs)
        data.apply_to_split(&mut out, split_idx);

        // Clear out the element segments and then add in the initializers for the shared imports
        self.create_ifunc_initializers(&mut out, &unique_symbols);

        // Convert our split module's functions to real functions that call the indirect function
        self.add_split_imports(
            &mut out,
            split.index,
            split_export_func,
            split.export_name,
            &symbols_to_import,
            &shared_funcs,
        );

        // Delete all the functions that are not reachable from the main module
        self.delete_main_funcs_from_split(&mut out, &symbols_to_delete);

        self.remove_custom_sections(&mut out);

        // Run the gc to remove unused functions - also validates the module to ensure we can emit it properly
        // todo(jon): prefer to delete the items as we go so we don't need to run a gc pass. it/it's quite slow
        walrus::passes::gc::run(&mut out);

        Ok(SplitModule {
            bytes: out.emit_wasm(),
            module_name: split.module_name.clone(),
            component_name: Some(split.component_name.clone()),
            relies_on_chunks,
            hash_id: Some(split.hash_name.clone()),
        })
    }

    /// What shared chunk `idx` imports and deletes, and the function bodies
    /// it keeps.
    fn split_chunk_parts(&self, idx: usize) -> SplitParts {
        let unique_symbols = &self.chunks[idx];

        // What the chunk's functions call outside it: the start module's
        // functions and other chunks', all reached through the table.
        let symbols_to_import: HashSet<_> = unique_symbols
            .iter()
            .flat_map(|node| self.call_graph.get(node).into_iter().flatten())
            .filter(|node| matches!(node, Node::Function(_)) && !unique_symbols.contains(node))
            .copied()
            .collect();

        // The start module's functions it does not call.
        let symbols_to_delete: HashSet<_> = self
            .main_graph
            .difference(&symbols_to_import)
            .cloned()
            .collect();

        let stops = self.replaced_or_deleted(&symbols_to_import, &symbols_to_delete);
        let bodies = self.kept_bodies(unique_symbols.iter().copied(), &stops);
        SplitParts {
            unique_symbols: unique_symbols.clone(),
            symbols_to_import,
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
                self.split_points.len() + shared_funcs.len(),
            )
            .unwrap();

        self.convert_shared_to_imports(&mut out, segment_start, &shared_funcs, &symbols_to_import);

        let mut slot = segment_start + self.split_points.len();
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

        let mut idx = self.split_points.len();
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
    fn create_ifunc_table(&self, out: &mut Module, unused_symbols: &HashSet<Node>) {
        let ifunc_table = self.load_funcref_table(out);
        let dummy_func = self.make_dummy_func(out);

        out.exports.add("__indirect_function_table", ifunc_table);

        // Expand the ifunc table to accommodate the new ifuncs
        let segment_start = self
            .expand_ifunc_table_max(
                out,
                ifunc_table,
                self.split_points.len() + self.shared_symbols.len(),
            )
            .expect("failed to expand ifunc table");

        // Delete the split import functions and replace them with local functions
        //
        // Start by pushing all the shared imports into the list
        // These don't require an additional stub function
        let mut ifuncs = vec![];

        // Push the split import functions into the list - after we've pushed in the shared imports
        for idx in 0..self.split_points.len() {
            // this is okay since we're in the main module
            let import_func = self.split_points[idx].import_func;
            let import_id = self.split_points[idx].import_id;
            let ty_id = out.funcs.get(import_func).ty();
            let stub_idx = segment_start + ifuncs.len();

            // Replace the import function with a local function that calls the indirect function
            out.funcs.get_mut(import_func).kind =
                self.make_stub_funcs(out, ifunc_table, ty_id, stub_idx as _);

            // And remove the corresponding import
            out.imports.delete(import_id);

            // Push into the list the properly typed dummy func so the entry is populated
            // unclear if the typing is important here
            ifuncs.push(dummy_func);
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
        for split in self.split_points.iter() {
            // it's okay that we're not re-mapping IDs since this is just used by the main module
            out.exports.delete(split.export_id);
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
    fn replace_segments_with_holes(&self, out: &mut Module, unused_symbols: &HashSet<Node>) {
        let dummy_func = self.make_dummy_func(out);
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

        let mut initializers = HashMap::new();
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

    fn add_split_imports(
        &self,
        out: &mut Module,
        split_idx: usize,
        split_export_func: FunctionId,
        split_export_name: String,
        symbols_to_import: &HashSet<Node>,
        ifuncs: &Vec<Node>,
    ) {
        let ifunc_table_id = self.load_funcref_table(out);
        let segment_start = self
            .expand_ifunc_table_max(out, ifunc_table_id, self.split_points.len() + ifuncs.len())
            .unwrap();

        out.exports.add(&split_export_name, split_export_func);

        out.tables
            .get_mut(ifunc_table_id)
            .elem_segments
            .insert(out.elements.add(
                ElementKind::Active {
                    table: ifunc_table_id,
                    offset: ConstExpr::Value(ir::Value::I32((segment_start + split_idx) as i32)),
                },
                ElementItems::Functions(vec![split_export_func]),
            ));

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

        if let Some(max) = ifunc_table_.maximum {
            ifunc_table_.maximum = Some(max + num_ifuncs as u64);
            ifunc_table_.initial += num_ifuncs as u64;
            return Some(max as usize);
        }

        None
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
        Ok(())
    }

    fn unused_main_symbols(&self) -> HashSet<Node> {
        self.split_points
            .iter()
            .flat_map(|split| split.reachable_graph.iter())
            .filter(|sym| {
                // Make sure the symbol isn't in the main graph
                if self.main_graph.contains(sym) {
                    return false;
                }

                // And ensure we aren't also exporting it
                match sym {
                    Node::Function(u) => self.source_module.exports.get_exported_func(*u).is_none(),
                    _ => true,
                }
            })
            .cloned()
            .collect()
    }

    /// Accumulate the relocations from the original module, create a relocation map, and then convert
    /// that to our *new* module's symbols.
    fn build_call_graph(&mut self) -> Result<()> {
        let original = ModuleWithRelocations::new(self.original)?;
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
        let mut old_names: HashMap<String, Vec<FunctionId>> = HashMap::new();
        for f in original.module.funcs.iter() {
            if let Some(name) = &f.name {
                old_names.entry(name.clone()).or_default().push(f.id());
            }
        }

        let mut new_names: HashMap<String, Vec<FunctionId>> = HashMap::new();
        for f in self.source_module.funcs.iter() {
            if let Some(name) = &f.name {
                new_names.entry(name.clone()).or_default().push(f.id());
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
                    for entry in new_names.get(name).into_iter().flatten() {
                        recovered_children.insert(Node::Function(*entry));
                    }
                }

                // Data symbols are unchanged and fine to remap
                Node::DataSymbol(id) => {
                    recovered_children.insert(Node::DataSymbol(id));
                }
            }
        }

        // We're going to attach the recovered children to the main function
        // A library module (`cdylib`) has no `main`: its wasm-bindgen start
        // export is always in the main module's graph, so it anchors them.
        let main_fn = self
            .source_module
            .funcs
            .by_name("main")
            .or_else(|| self.source_module.exports.get_func("__wbindgen_start").ok())
            .context("Failed to find `main` or `__wbindgen_start` - was this built with LTO, --emit-relocs, and symbols?")?;
        let main_fn_entry = new_call_graph.entry(Node::Function(main_fn)).or_default();
        main_fn_entry.extend(recovered_children);

        // Also attach any truly new symbols to the main function. Usually these are the shim functions
        for (name, new) in new_names.iter() {
            if !old_names.contains_key(name) {
                main_fn_entry.extend(new.iter().map(|id| Node::Function(*id)));
            }
        }
        for (node, children) in new_call_graph {
            self.call_graph.entry(node).or_default().extend(children);
        }

        // Walk the functions and try to disconnect any holes manually
        // This will attempt to resolve any of the new symbols like the shim functions
        for func in self.source_module.funcs.iter() {
            struct CallGrapher<'a> {
                cur: FunctionId,
                call_graph: &'a mut HashMap<Node, HashSet<Node>>,
            }
            impl<'a> Visitor<'a> for CallGrapher<'a> {
                fn visit_function_id(&mut self, function: &walrus::FunctionId) {
                    self.call_graph
                        .entry(Node::Function(self.cur))
                        .or_default()
                        .insert(Node::Function(*function));
                }
            }
            if let FunctionKind::Local(local) = &func.kind {
                let mut call_grapher = CallGrapher {
                    cur: func.id(),
                    call_graph: &mut self.call_graph,
                };
                dfs_in_order(&mut call_grapher, local, local.entry_block());
            }
        }

        for (parnet, children) in self.call_graph.iter() {
            for child in children {
                self.parent_graph.entry(*child).or_default().insert(*parnet);
            }
        }

        // Now go fill in the reachability graph for each of the split points
        // We want to be as narrow as possible since we've reparented any new symbols to the main module
        self.split_points.iter_mut().for_each(|split| {
            let roots: HashSet<_> = [Node::Function(split.export_func)].into();
            split.reachable_graph = reachable_graph(&self.call_graph, &roots);
        });

        // And then the reachability graph for main
        self.main_graph = reachable_graph(&self.call_graph, &self.main_roots());
        // WASM_SPLIT_WHY=name,name prints why each function stays in the main
        // module (its path from a root) and stops: the way to find the next cut.
        if let Ok(targets) = std::env::var("WASM_SPLIT_WHY") {
            for target in targets.split(',') {
                eprintln!("{target}:");
                self.explain_main_path(target);
            }
            std::process::exit(0);
        }

        // And then the symbols shared between all
        self.shared_symbols = {
            let mut shared_funcs = HashSet::new();

            for split in self.split_points.iter() {
                shared_funcs.extend(self.main_graph.intersection(&split.reachable_graph));
            }

            // And then all our imports will be callabale via the ifunc table too
            for import in self.source_module.imports.iter() {
                if let ImportKind::Function(id) = import.kind {
                    shared_funcs.insert(Node::Function(id));
                }
            }

            // Make sure to make this *ordered*
            shared_funcs.into_iter().collect()
        };

        Ok(())
    }

    fn main_roots(&self) -> HashSet<Node> {
        // Accumulate all the split entrypoints
        // This will include wasm_bindgen functions too
        let exported_splits = self
            .split_points
            .iter()
            .map(|f| f.export_func)
            .collect::<HashSet<_>>();

        // And only return the functions that are reachable from the main module's start function
        let mut roots = self
            .source_module
            .exports
            .iter()
            .filter_map(|e| match e.item {
                ExportItem::Function(id) if !exported_splits.contains(&id) => {
                    Some(Node::Function(id))
                }
                _ => None,
            })
            .chain(self.source_module.start.map(Node::Function))
            .collect::<HashSet<Node>>();

        // Also add "imports" to the roots
        for import in self.source_module.imports.iter() {
            if let ImportKind::Function(id) = import.kind {
                roots.insert(Node::Function(id));
            }
        }

        roots
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
struct CodeSection {
    /// The section's id byte.
    start: usize,
    /// Its contents, after the size.
    contents: usize,
    end: usize,
    /// Each defined function's body, after its size.
    bodies: Vec<Range<usize>>,
}

fn read_leb(bytes: &[u8], at: &mut usize) -> Result<usize> {
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

fn code_section(bytes: &[u8]) -> Result<Option<CodeSection>> {
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
fn keep_function_bodies(bytes: &[u8], keep: &HashSet<usize>) -> Result<Vec<u8>> {
    // Locals: none; `unreachable`; `end`.
    const STUB: [u8; 4] = [3, 0x00, 0x00, 0x0b];

    let Some(code) = code_section(bytes)? else {
        return Ok(bytes.to_vec());
    };
    let mut imported = 0usize;
    for payload in wasmparser::Parser::new(0).parse_all(bytes) {
        if let Payload::ImportSection(reader) = payload? {
            for import in reader {
                if matches!(import?.ty, wasmparser::TypeRef::Func(_)) {
                    imported += 1;
                }
            }
        }
    }
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

/// Parse a module and return the mapping of index to FunctionID.
/// We'll use this mapping to remap ModuleIDs
fn parse_module_with_ids(
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

struct ModuleWithRelocations<'a> {
    /// The module's functions, tables and names; function bodies are
    /// `unreachable` (see `new`).
    module: Module,
    /// Each local function's code, as relocation offsets count: from the
    /// code section's contents, including the body's size.
    code_ranges: Vec<(FunctionId, Range<usize>)>,
    symbols: Vec<SymbolInfo<'a>>,
    funcs_by_index: Vec<FunctionId>,
    call_graph: HashMap<Node, HashSet<Node>>,
    parents: HashMap<Node, HashSet<Node>>,
    relocation_map: HashMap<Node, Vec<RelocationEntry>>,
    data_symbols: BTreeMap<usize, DataSymbol>,
    data_section_range: Range<usize>,
}

impl<'a> ModuleWithRelocations<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self> {
        // The call graph comes from relocations, not from the code itself,
        // so the module is parsed without its function bodies: parsing them
        // took gigabytes for a large module and none of it was read.
        let module = Module::from_buffer(&keep_function_bodies(bytes, &HashSet::new())?)?;
        let raw_data = parse_bytes_to_data_segment(bytes)?;
        // walrus adds a parsed module's functions in index order, imports
        // first.
        let funcs_by_index: Vec<FunctionId> = module.funcs.iter().map(|f| f.id()).collect();
        let code = code_section(bytes)?.context("module has no code section")?;
        let imported = funcs_by_index.len() - code.bodies.len();
        // A relocation's offset counts from the code section's contents, and
        // a function's code from its size, written as LEB128 in as few bytes
        // as the size needs.
        let code_ranges = code
            .bodies
            .iter()
            .zip(&funcs_by_index[imported..])
            .map(|(body, id)| {
                let size_len = (usize::BITS - body.len().leading_zeros())
                    .div_ceil(7)
                    .max(1);
                let start = body.start - code.contents - size_len as usize;
                (*id, start..body.end - code.contents)
            })
            .collect::<Vec<_>>();

        let mut module = Self {
            module,
            code_ranges,
            data_symbols: raw_data.data_symbols,
            data_section_range: raw_data.data_range,
            symbols: raw_data.symbols,
            funcs_by_index,
            call_graph: Default::default(),
            relocation_map: Default::default(),
            parents: Default::default(),
        };

        module.build_code_call_graph()?;
        module.build_data_call_graph()?;

        for (func, children) in module.call_graph.iter() {
            for child in children {
                module.parents.entry(*child).or_default().insert(*func);
            }
        }

        Ok(module)
    }

    fn build_code_call_graph(&mut self) -> Result<()> {
        let codes_relocations = self.collect_relocations_from_section("reloc.CODE")?;
        let mut relocations = codes_relocations.iter().peekable();

        for (func_id, range) in &self.code_ranges {
            let (func_id, range) = (*func_id, range.clone());

            while let Some(entry) =
                relocations.next_if(|entry| entry.relocation_range().start < range.end)
            {
                let reloc_range = entry.relocation_range();
                assert!(reloc_range.start >= range.start);
                assert!(reloc_range.end <= range.end);

                if let Some(target) = self.relocation_target(entry)? {
                    let us = Node::Function(func_id);
                    self.call_graph.entry(us).or_default().insert(target);
                    self.relocation_map.entry(us).or_default().push(*entry);
                }
            }
        }

        assert!(relocations.next().is_none());

        Ok(())
    }

    fn build_data_call_graph(&mut self) -> Result<()> {
        let data_relocations = self.collect_relocations_from_section("reloc.DATA")?;
        let mut relocations = data_relocations.iter().peekable();

        let symbols_sorted = self
            .data_symbols
            .values()
            .sorted_by(|a, b| a.range.start.cmp(&b.range.start));

        for symbol in symbols_sorted {
            let start = symbol.range.start - self.data_section_range.start;
            let end = symbol.range.end - self.data_section_range.start;
            let range = start..end;

            while let Some(entry) =
                relocations.next_if(|entry| entry.relocation_range().start < range.end)
            {
                let reloc_range = entry.relocation_range();
                assert!(reloc_range.start >= range.start);
                assert!(reloc_range.end <= range.end);

                if let Some(target) = self.relocation_target(entry)? {
                    let dep = Node::DataSymbol(symbol.index);
                    self.call_graph.entry(dep).or_default().insert(target);
                    self.relocation_map.entry(dep).or_default().push(*entry);
                }
            }
        }

        assert!(relocations.next().is_none());

        Ok(())
    }

    /// Accumulate all relocations from a section.
    ///
    /// Parses the section using the RelocSectionReader and returns a vector of relocation entries.
    fn collect_relocations_from_section(&self, name: &str) -> Result<Vec<RelocationEntry>> {
        let (_reloc_id, code_reloc) = self
            .module
            .customs
            .iter()
            .find(|(_, c)| c.name() == name)
            .context("Module does not contain the reloc section")?;

        let code_reloc_data = code_reloc.data(&Default::default());
        let reader = BinaryReader::new(&code_reloc_data, 0);
        let relocations = RelocSectionReader::new(reader)
            .context("failed to parse reloc section")?
            .entries()
            .into_iter()
            .flatten()
            .collect();

        Ok(relocations)
    }

    /// The node a relocation refers to. A type-index relocation (the
    /// signature of a `call_indirect`) carries a type index, not a symbol
    /// index; reading it as a symbol links the caller to whatever function
    /// happens to hold that symbol slot.
    fn relocation_target(&self, entry: &RelocationEntry) -> Result<Option<Node>> {
        if entry.ty == RelocationType::TypeIndexLeb {
            return Ok(None);
        }
        self.get_symbol_dep_node(entry.index as usize)
    }

    /// Get the symbol's corresponding entry in the call graph
    ///
    /// This might panic if the source module isn't built properly. Make sure to enable LTO and `--emit-relocs`
    /// when building the source module.
    fn get_symbol_dep_node(&self, index: usize) -> Result<Option<Node>> {
        let res = match self.symbols[index] {
            SymbolInfo::Data { .. } => Some(Node::DataSymbol(index)),
            // The symbol names its function by index. Looking it up by name
            // instead picks an arbitrary one of the functions sharing that
            // name (copies of a generic shim), which wires unrelated code
            // together.
            SymbolInfo::Func { index, .. } => {
                let func = self.funcs_by_index.get(index as usize).copied();
                Some(Node::Function(func.with_context(|| {
                    format!("function symbol names function {index}, past the module's functions")
                })?))
            }

            _ => None,
        };

        Ok(res)
    }
}

#[derive(Debug, Clone)]
pub struct SplitPoint {
    module_name: String,
    import_id: ImportId,
    export_id: ExportId,
    import_func: FunctionId,
    export_func: FunctionId,
    component_name: String,
    index: usize,
    reachable_graph: HashSet<Node>,
    hash_name: String,

    #[allow(unused, reason = "kept for diagnostics")]
    import_name: String,

    #[allow(unused, reason = "kept for diagnostics")]
    export_name: String,
}

/// Search the module's imports and exports for functions marked as split points.
///
/// These will be in the form of:
///
/// `__wasm_split_00<module>00_<import|export>_<hash>_<function>`
///
/// For a function named `SomeRoute2` in the module `add_body_element`, the pairings would be:
///
/// `__wasm_split_00add_body_element00_import_abef5ee3ebe66ff17677c56ee392b4c2_SomeRoute2`
/// `__wasm_split_00add_body_element00_export_abef5ee3ebe66ff17677c56ee392b4c2_SomeRoute2`
///
fn accumulate_split_points(module: &Module) -> Vec<SplitPoint> {
    let mut index = 0;

    module
        .imports
        .iter()
        .sorted_by(|a, b| a.name.cmp(&b.name))
        .flat_map(|import| {
            if !import.name.starts_with("__wasm_split_00") {
                return None;
            }

            let ImportKind::Function(import_func) = import.kind else {
                return None;
            };

            // Parse the import name to get the module name, the hash, and the function name
            let remain = import.name.trim_start_matches("__wasm_split_00___");
            let (module_name, rest) = remain.split_once("___00").unwrap();
            let (hash, fn_name) = rest.trim_start_matches("_import_").split_once("_").unwrap();

            // Look for the export with the same name
            let export_name =
                format!("__wasm_split_00___{module_name}___00_export_{hash}_{fn_name}");
            let export_func = module
                .exports
                .get_func(&export_name)
                .expect("Could not find export");
            let export = module.exports.get_exported_func(export_func).unwrap();

            let our_index = index;
            index += 1;

            Some(SplitPoint {
                export_id: export.id(),
                import_id: import.id(),
                module_name: module_name.to_string(),
                import_name: import.name.clone(),
                import_func,
                export_func,
                export_name,
                hash_name: hash.to_string(),
                component_name: fn_name.to_string(),
                index: our_index,
                reachable_graph: Default::default(),
            })
        })
        .collect()
}

#[derive(Debug, PartialEq, Eq, Hash, Copy, PartialOrd, Ord, Clone)]
pub enum Node {
    Function(FunctionId),
    DataSymbol(usize),
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

struct RawDataSection<'a> {
    data_range: Range<usize>,
    symbols: Vec<SymbolInfo<'a>>,
    data_symbols: BTreeMap<usize, DataSymbol>,
}

#[derive(Debug)]
struct DataSymbol {
    index: usize,
    range: Range<usize>,
}

/// Manually parse the data section from a wasm module
///
/// We need to do this for data symbols because walrus doesn't provide the right range and offset
/// information for data segments. Fortunately, it provides it for code sections, so we only need to
/// do a small amount extra of parsing here.
fn parse_bytes_to_data_segment(bytes: &[u8]) -> Result<RawDataSection<'_>> {
    let parser = wasmparser::Parser::new(0);
    let mut parser = parser.parse_all(bytes);
    let mut segments = vec![];
    let mut data_range = 0..0;
    let mut symbols = vec![];

    // Process the payloads in the raw wasm file so we can extract the specific sections we need
    while let Some(Ok(payload)) = parser.next() {
        match payload {
            Payload::DataSection(section) => {
                data_range = section.range();
                segments = section.into_iter().collect::<Result<Vec<_>, _>>()?
            }
            Payload::CustomSection(section) if section.name() == "linking" => {
                let reader = BinaryReader::new(section.data(), 0);
                let reader = LinkingSectionReader::new(reader)?;
                for subsection in reader.subsections() {
                    if let Linking::SymbolTable(map) = subsection? {
                        symbols = map.into_iter().collect::<Result<Vec<_>, _>>()?;
                    }
                }
            }
            _ => {}
        }
    }

    // Accumulate the data symbols into a btreemap for later use
    let mut data_symbols = BTreeMap::new();
    for (index, symbol) in symbols.iter().enumerate() {
        let SymbolInfo::Data {
            symbol: Some(symbol),
            ..
        } = symbol
        else {
            continue;
        };

        if symbol.size == 0 {
            continue;
        }

        let data_segment = segments
            .get(symbol.index as usize)
            .context("Failed to find data segment")?;
        let offset: usize =
            data_segment.range.end - data_segment.data.len() + (symbol.offset as usize);
        let range = offset..(offset + symbol.size as usize);

        data_symbols.insert(index, DataSymbol { index, range });
    }

    Ok(RawDataSection {
        data_range,
        symbols,
        data_symbols,
    })
}

impl Splitter<'_> {
    /// Keep in the main module every function that code staying there calls
    /// or takes the address of. The relocation call graph can miss an edge
    /// (wasm-bindgen rewrites and renames functions); deleting a function
    /// that main still references breaks the module, while keeping one only
    /// costs its bytes.
    fn keep_referenced(&self, unused: &mut HashSet<Node>) {
        struct Refs<'a> {
            found: &'a mut Vec<FunctionId>,
        }
        impl<'a> Visitor<'a> for Refs<'a> {
            fn visit_function_id(&mut self, function: &FunctionId) {
                self.found.push(*function);
            }
        }
        let module = &self.source_module;
        let split_exports: HashSet<FunctionId> = self
            .split_points
            .iter()
            .map(|split| split.export_func)
            .collect();
        // Walk from everything main keeps, through both the code's own
        // references and the relocation graph: a function kept only by
        // this walk still reaches vtables (data symbols) whose methods are
        // called through the table, and those must stay too, or the call
        // lands on the hole left for a split module.
        let mut queue: Vec<Node> = module
            .funcs
            .iter()
            .map(|f| Node::Function(f.id()))
            .filter(|node| !unused.contains(node))
            .filter(|node| !matches!(node, Node::Function(id) if split_exports.contains(id)))
            .collect();
        let mut seen: HashSet<Node> = queue.iter().copied().collect();
        let mut kept = 0usize;
        while let Some(node) = queue.pop() {
            let mut next: Vec<Node> = self
                .call_graph
                .get(&node)
                .into_iter()
                .flatten()
                .copied()
                .collect();
            if let Node::Function(id) = node {
                if let FunctionKind::Local(local) = &module.funcs.get(id).kind {
                    let mut found = Vec::new();
                    dfs_in_order(&mut Refs { found: &mut found }, local, local.entry_block());
                    next.extend(found.into_iter().map(Node::Function));
                }
            }
            for child in next {
                if seen.insert(child) {
                    if unused.remove(&child) && matches!(child, Node::Function(_)) {
                        kept += 1;
                    }
                    queue.push(child);
                }
            }
        }
        if kept > 0 {
            tracing::warn!("kept {kept} functions in the main module the call graph had moved out");
            eprintln!(
                "wasm-split: kept {kept} functions in the start module that the call graph missed"
            );
        }
    }

    /// Print the shortest path from a main root to the first function whose
    /// name contains `target`: why that function stays in the start module.
    fn explain_main_path(&self, target: &str) {
        let roots = self.main_roots();
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
