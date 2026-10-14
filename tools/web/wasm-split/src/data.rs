//! Which module each piece of static data is written by.
//!
//! The linked module keeps all its static data (string constants,
//! `include_bytes!` tables, lookup tables, mutable statics) in a few large
//! data segments. A byte range that only one split module's code can reach
//! moves into that module: the start module no longer carries it, and the
//! split module writes it, at the same address, when it is instantiated.
//!
//! # Who can reach a static
//!
//! The linker places every static at a fixed address and records, through
//! a relocation, every place its address is written into the program: an
//! instruction's immediate in a function (`i32.const`, a load's or store's
//! offset), or a pointer in another static's initial contents (a `&[u8]` in
//! a table, a vtable). Rust and LLVM only let a program reach a static
//! through a pointer derived from its address; stepping from one static
//! into another is undefined behaviour, and nothing compiled relies on it.
//! So a static can only be read or written after code holding one of those
//! immediates ran, or after code read a pointer to it out of another static
//! it could already reach.
//!
//! That gives each static the set of modules whose code can reach it
//! first: the modules holding a function whose code has its address, plus,
//! for every static whose contents point to it, that static's modules. A
//! static only one split module or shared chunk can reach moves into it.
//! Its bytes are in memory once that module is instantiated, which happens
//! before any of the module's functions runs (they enter the table when it
//! is instantiated), so before any pointer to it can exist anywhere;
//! from then on the pointer may go anywhere (to the start module's code, the
//! heap, other modules) and the bytes are there.
//!
//! What stays in the start module:
//! - a static the start module's own code or data reaches, or that two
//!   modules reach (split modules or shared chunks);
//! - a static nothing reaches (code the split removed was its only user);
//! - bytes no symbol covers, and runs too short to be worth a segment;
//! - everything, when the analysis cannot vouch for the module: data the
//!   bindgened module does not hold unchanged at the linked module's
//!   addresses, a passive or thread-local segment, or a relocation against
//!   a linker-defined symbol inside the data (`__start_*`, `__global_base`)
//!   through which code could walk across statics.
//!
//! The functions' references are read from the bindgened module's code, not
//! paired up through names with the linked module's: wasm-bindgen does not
//! move static data, so every address the linked code held through a
//! relocation is the same immediate in the bindgened code. An immediate
//! equal to such an address names the statics it was relocated against.
//! Any other immediate is a plain number when the linked code holds it
//! unrelocated at least as often as the bindgened code does (bindgened code
//! is the linked code, less what wasm-bindgen dropped, plus its glue);
//! otherwise it is taken as a reference to every static it falls inside or
//! ends. A number that only looks like an address keeps a static where it
//! is; it cannot move one.
//!
//! # Addresses
//!
//! Moved data keeps its address. The start module's memory still spans it:
//! the memory's size, the stack and the heap all start from the linked
//! layout, which ends after the last static, and the allocator only hands
//! out memory past it. Until the module that owns a range is instantiated,
//! the range holds zeros that nothing reads.

use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use walrus::{
    ConstExpr, DataKind, FunctionId, FunctionKind, GlobalKind, Module,
    ir::{self, Instr, InstrLocId, Visitor, dfs_in_order},
};
use wasmparser::{
    BinaryReader, Linking, LinkingSectionReader, Payload, RelocSectionReader, RelocationType,
    SegmentFlags, SymbolFlags, SymbolInfo,
};

/// Moving fewer bytes than this costs about as much in segment headers (one
/// in each module) as it saves.
const MIN_RUN: u32 = 32;

/// A split module leaves out a stretch of zeros at least this long: it costs
/// about as much as another segment's header.
const ZERO_GAP: usize = 8;

/// The linked module's static data: each data symbol's address range, what
/// points where, and every address its code holds.
pub(crate) struct DataLayout {
    /// Every sized data symbol's address range, and its name.
    symbols: Vec<Range<u32>>,
    names: Vec<String>,
    /// Data symbols whose contents point into other ones, by position in
    /// `symbols`.
    points_to: Vec<Vec<u32>>,
    /// Symbols the start module keeps whoever reaches them: what a pointer
    /// in bytes no symbol covers leads to, and what code reaches through a
    /// relocation whose address is not absolute.
    pinned: Vec<u32>,
    /// Each address the linked module's code holds through a relocation, and
    /// the symbols it was relocated against.
    code_addresses: HashMap<u32, Vec<u32>>,
    /// How often each value is an immediate of the linked module's code
    /// that no relocation produced: a plain number, not an address.
    plain: HashMap<u32, u32>,
    /// The active data segments, as `(address, bytes)`.
    segments: Vec<(u32, Vec<u8>)>,
    /// The boundaries of every symbol, sorted: `covering[k]` holds the
    /// symbols covering `bounds[k]..bounds[k + 1]`.
    bounds: Vec<u32>,
    covering: Vec<Vec<u32>>,
    /// Why no data can move, when the module is outside what this analysis
    /// vouches for.
    refused: Option<String>,
}

/// Where each static's bytes are written: the ranges each split module
/// carries, and the ones the start module therefore leaves out.
#[derive(Default)]
pub(crate) struct DataPlan {
    /// Per split module, then per shared chunk, the runs `(address, bytes)`
    /// it writes.
    pub moved: Vec<Vec<(u32, Vec<u8>)>>,
    /// Every moved range, sorted.
    removed: Vec<Range<u32>>,
}

/// One claimant of static data: the start module, or a split module or
/// shared chunk by its index in [`DataPlan::moved`].
pub(crate) enum Claimant {
    Start,
    Module(usize),
}

/// Why the start module keeps a symbol, for `WASM_SPLIT_DATA_WHY`.
#[derive(Clone, Copy)]
enum Cause {
    /// No module's code or data reaches it.
    Nobody,
    /// This start module function's code holds its address.
    Code(FunctionId),
    /// A global's initial value is its address.
    Global,
    /// Code reaches it through a base the program computes, or a pointer to
    /// it sits in bytes no symbol covers.
    Pinned,
    /// Two split modules reach it.
    Shared,
    /// This symbol, kept for its own cause, points to it.
    Via(u32),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Owner {
    Nobody,
    Start,
    Split(usize),
}

impl DataLayout {
    /// Read the linked module (built with `--emit-relocs` and its symbol
    /// table).
    pub(crate) fn new(linked: &[u8]) -> Result<Self> {
        let mut segments = Vec::new();
        let mut segment_files = Vec::new();
        let mut data_section = 0..0;
        let mut symbol_infos = Vec::new();
        let mut segment_flags = Vec::new();
        let mut relocs: HashMap<String, Vec<wasmparser::RelocationEntry>> = HashMap::new();
        let mut refused = None;
        let mut code_start = 0;
        let mut bodies = Vec::new();
        for payload in wasmparser::Parser::new(0).parse_all(linked) {
            match payload? {
                Payload::CodeSectionStart { range, .. } => code_start = range.start,
                Payload::CodeSectionEntry(body) => bodies.push(body),
                Payload::DataSection(section) => {
                    data_section = section.range();
                    for data in section {
                        let data = data?;
                        let wasmparser::DataKind::Active {
                            memory_index: 0,
                            offset_expr,
                        } = data.kind
                        else {
                            refused.get_or_insert_with(|| "a passive data segment".to_string());
                            segments.push((0, Vec::new()));
                            segment_files.push(0..0);
                            continue;
                        };
                        let mut ops = offset_expr.get_operators_reader();
                        let base = match ops.read()? {
                            wasmparser::Operator::I32Const { value } => value as u32,
                            _ => {
                                refused.get_or_insert_with(|| {
                                    "a data segment at a computed address".to_string()
                                });
                                0
                            }
                        };
                        let start = data.range.end - data.data.len();
                        segments.push((base, data.data.to_vec()));
                        segment_files.push(start..data.range.end);
                    }
                }
                Payload::CustomSection(section) if section.name() == "linking" => {
                    let reader = BinaryReader::new(section.data(), 0);
                    for subsection in LinkingSectionReader::new(reader)?.subsections() {
                        match subsection? {
                            Linking::SymbolTable(map) => {
                                symbol_infos = map.into_iter().collect::<Result<Vec<_>, _>>()?
                            }
                            Linking::SegmentInfo(map) => {
                                segment_flags = map
                                    .into_iter()
                                    .map(|segment| segment.map(|segment| segment.flags))
                                    .collect::<Result<Vec<_>, _>>()?
                            }
                            _ => {}
                        }
                    }
                }
                Payload::CustomSection(section) if section.name().starts_with("reloc.") => {
                    let reader = BinaryReader::new(section.data(), 0);
                    let entries = RelocSectionReader::new(reader)?
                        .entries()
                        .into_iter()
                        .collect::<Result<Vec<_>, _>>()?;
                    relocs.insert(section.name().to_string(), entries);
                }
                _ => {}
            }
        }
        anyhow::ensure!(
            !symbol_infos.is_empty(),
            "the linked module has no symbol table"
        );
        if segment_flags
            .iter()
            .any(|flags| flags.contains(SegmentFlags::TLS))
        {
            refused.get_or_insert_with(|| "a thread-local data segment".to_string());
        }

        // Each sized data symbol, by its symbol table index.
        let mut symbols = Vec::new();
        let mut names = Vec::new();
        let mut by_index: HashMap<u32, u32> = HashMap::new();
        // Where a symbol of any size is, for relocations against it.
        let mut addresses: HashMap<u32, u32> = HashMap::new();
        for (index, info) in symbol_infos.iter().enumerate() {
            let SymbolInfo::Data {
                flags,
                name,
                symbol: Some(symbol),
            } = info
            else {
                continue;
            };
            if flags.contains(SymbolFlags::ABSOLUTE) {
                continue;
            }
            let (base, bytes) = segments
                .get(symbol.index as usize)
                .context("data symbol in a missing segment")?;
            anyhow::ensure!(
                symbol.offset as usize + symbol.size as usize <= bytes.len()
                    || bytes.is_empty() && refused.is_some(),
                "data symbol {name} runs past its segment"
            );
            let start = base + symbol.offset;
            addresses.insert(index as u32, start);
            if symbol.size > 0 {
                by_index.insert(index as u32, symbols.len() as u32);
                symbols.push(start..start + symbol.size);
                names.push((*name).to_string());
            }
        }
        let data_span = segments
            .iter()
            .filter(|(_, bytes)| !bytes.is_empty())
            .map(|(base, bytes)| *base..base + bytes.len() as u32)
            .reduce(|a, b| a.start.min(b.start)..a.end.max(b.end))
            .unwrap_or(0..0);
        // A relocation against a symbol without a size: a zero-sized static
        // reaches nothing, but one the linker defines over a range (its
        // names start with `__`) lets code walk across the statics in it.
        let linker_range = |index: u32| -> Option<String> {
            let SymbolInfo::Data {
                flags,
                name,
                symbol,
            } = &symbol_infos[index as usize]
            else {
                return None;
            };
            if by_index.contains_key(&index) || !name.starts_with("__") {
                return None;
            }
            let address = match symbol {
                Some(symbol) if flags.contains(SymbolFlags::ABSOLUTE) => symbol.offset,
                Some(_) => *addresses.get(&index)?,
                None => return Some((*name).to_string()),
            };
            data_span.contains(&address).then(|| (*name).to_string())
        };

        let mut layout = Self {
            points_to: vec![Vec::new(); symbols.len()],
            symbols,
            names,
            pinned: Vec::new(),
            code_addresses: HashMap::new(),
            plain: HashMap::new(),
            segments,
            bounds: Vec::new(),
            covering: Vec::new(),
            refused,
        };
        layout.index_bounds();

        for entry in relocs.get("reloc.CODE").into_iter().flatten() {
            let address_of = |entry: &wasmparser::RelocationEntry| {
                addresses
                    .get(&entry.index)
                    .map(|address| (i64::from(*address) + entry.addend) as u32)
            };
            match entry.ty {
                RelocationType::MemoryAddrLeb
                | RelocationType::MemoryAddrSleb
                | RelocationType::MemoryAddrLeb64
                | RelocationType::MemoryAddrSleb64 => {
                    if let Some(name) = linker_range(entry.index) {
                        layout.refuse(format!("code takes the address of {name}"));
                    }
                    if let (Some(symbol), Some(address)) =
                        (by_index.get(&entry.index), address_of(entry))
                    {
                        layout
                            .code_addresses
                            .entry(address)
                            .or_default()
                            .push(*symbol);
                    }
                }
                // Addresses relative to a base the program computes: whoever
                // holds them, the start module keeps what they lead to.
                RelocationType::MemoryAddrRelSleb
                | RelocationType::MemoryAddrRelSleb64
                | RelocationType::MemoryAddrTlsSleb
                | RelocationType::MemoryAddrTlsSleb64 => {
                    if let Some(name) = linker_range(entry.index) {
                        layout.refuse(format!("code takes the address of {name}"));
                    }
                    if let Some(symbol) = by_index.get(&entry.index) {
                        layout.pinned.push(*symbol);
                    }
                }
                _ => {}
            }
        }
        // The immediates of the linked code no relocation produced.
        let mut relocated: Vec<usize> = relocs
            .get("reloc.CODE")
            .into_iter()
            .flatten()
            .map(|entry| code_start + entry.offset as usize)
            .collect();
        relocated.sort_unstable();
        for body in bodies {
            let mut reader = body.get_operators_reader()?;
            while !reader.eof() {
                let (operator, start) = reader.read_with_offset()?;
                let Some(value) = immediate(&operator) else {
                    continue;
                };
                let end = reader.original_position();
                let first = relocated.partition_point(|at| *at < start);
                if relocated.get(first).is_none_or(|at| *at >= end) {
                    *layout.plain.entry(value).or_default() += 1;
                }
            }
        }

        // A pointer in a static's contents: the static (each symbol covering
        // the pointer) points to the symbol it was relocated against.
        for entry in relocs.get("reloc.DATA").into_iter().flatten() {
            if !matches!(
                entry.ty,
                RelocationType::MemoryAddrI32
                    | RelocationType::MemoryAddrI64
                    | RelocationType::MemoryAddrLocrelI32
            ) {
                continue;
            }
            if let Some(name) = linker_range(entry.index) {
                layout.refuse(format!("a static holds the address of {name}"));
            }
            let Some(target) = by_index.get(&entry.index).copied() else {
                continue;
            };
            let at = data_section.start + entry.offset as usize;
            let Some(address) =
                segment_files
                    .iter()
                    .zip(&layout.segments)
                    .find_map(|(file, (base, _))| {
                        file.contains(&at).then(|| base + (at - file.start) as u32)
                    })
            else {
                layout.refuse("a data relocation outside every segment".to_string());
                continue;
            };
            let holders = layout.containing(address).to_vec();
            if holders.is_empty() {
                layout.pinned.push(target);
            }
            for holder in holders {
                layout.points_to[holder as usize].push(target);
            }
        }
        Ok(layout)
    }

    fn refuse(&mut self, why: String) {
        self.refused.get_or_insert(why);
    }

    /// Cut the address space at every symbol's start and end, and record
    /// the symbols covering each piece.
    fn index_bounds(&mut self) {
        let mut bounds: Vec<u32> = self
            .symbols
            .iter()
            .flat_map(|range| [range.start, range.end])
            .collect();
        bounds.sort_unstable();
        bounds.dedup();
        let mut covering = vec![Vec::new(); bounds.len()];
        for (symbol, range) in self.symbols.iter().enumerate() {
            let mut k = bounds.partition_point(|bound| *bound < range.start);
            while k + 1 < bounds.len() && bounds[k] < range.end {
                covering[k].push(symbol as u32);
                k += 1;
            }
        }
        self.bounds = bounds;
        self.covering = covering;
    }

    /// The symbols whose bytes include `address`.
    fn containing(&self, address: u32) -> &[u32] {
        let k = self.bounds.partition_point(|bound| *bound <= address);
        if k == 0 || k >= self.bounds.len() {
            return &[];
        }
        &self.covering[k - 1]
    }

    /// The symbols an immediate `value` in code refers to: those the linked
    /// module's code relocated to it, or else those it falls inside or ends.
    ///
    /// A relocated address names its symbol even when it lies outside it:
    /// one past the end of an array is the next static's start, and code
    /// holding it can only reach the array (stepping into the neighbour is
    /// undefined behaviour). Every address the linked code holds is
    /// relocated, so a value no relocation produced can only come from code
    /// wasm-bindgen generated, or be a plain number; it counts for whatever
    /// it would point at.
    fn referenced_by(&self, value: u32, out: &mut Vec<u32>) {
        if let Some(symbols) = self.code_addresses.get(&value) {
            out.extend_from_slice(symbols);
            return;
        }
        out.extend_from_slice(self.containing(value));
        if value > 0 {
            out.extend(
                self.containing(value - 1)
                    .iter()
                    .filter(|symbol| self.symbols[**symbol as usize].end == value),
            );
        }
    }

    /// The data symbols each local function of `module` (the bindgened
    /// module) refers to, read from its immediates.
    ///
    /// A value no relocation produced is a plain number when the linked
    /// code holds it, unrelocated, at least as often as the bindgened code
    /// does: wasm-bindgen copies the linked functions (or drops them) and
    /// adds its own glue, so every occurrence is accounted for by the
    /// linked code's plain numbers. One more occurrence than that could be
    /// an address wasm-bindgen's glue holds, so the value then counts for
    /// every static it falls inside.
    pub(crate) fn function_refs(&self, module: &Module) -> HashMap<FunctionId, Vec<u32>> {
        struct Immediates<'a> {
            values: &'a mut Vec<u32>,
        }
        impl<'instr> Visitor<'instr> for Immediates<'_> {
            fn visit_instr(&mut self, instr: &'instr Instr, _: &'instr InstrLocId) {
                let arg = match instr {
                    Instr::Const(ir::Const {
                        value: ir::Value::I32(value),
                    }) => {
                        self.values.push(*value as u32);
                        return;
                    }
                    Instr::Load(ir::Load { arg, .. })
                    | Instr::Store(ir::Store { arg, .. })
                    | Instr::AtomicRmw(ir::AtomicRmw { arg, .. })
                    | Instr::Cmpxchg(ir::Cmpxchg { arg, .. })
                    | Instr::AtomicNotify(ir::AtomicNotify { arg, .. })
                    | Instr::AtomicWait(ir::AtomicWait { arg, .. })
                    | Instr::LoadSimd(ir::LoadSimd { arg, .. }) => arg,
                    _ => return,
                };
                self.values.push(arg.offset);
            }
        }
        let mut functions = Vec::new();
        let mut occurrences: HashMap<u32, u32> = HashMap::new();
        for function in module.funcs.iter() {
            let FunctionKind::Local(local) = &function.kind else {
                continue;
            };
            let mut values = Vec::new();
            dfs_in_order(
                &mut Immediates {
                    values: &mut values,
                },
                local,
                local.entry_block(),
            );
            for value in &values {
                if !self.code_addresses.contains_key(value) {
                    *occurrences.entry(*value).or_default() += 1;
                }
            }
            values.sort_unstable();
            values.dedup();
            functions.push((function.id(), values));
        }
        let mut refs = HashMap::new();
        for (function, values) in functions {
            let mut symbols = Vec::new();
            for value in values {
                let plain = !self.code_addresses.contains_key(&value)
                    && occurrences[&value] <= self.plain.get(&value).copied().unwrap_or(0);
                if !plain {
                    self.referenced_by(value, &mut symbols);
                }
            }
            symbols.sort_unstable();
            symbols.dedup();
            if !symbols.is_empty() {
                refs.insert(function, symbols);
            }
        }
        refs
    }

    /// Decide where each static goes. `claimants` lists every module with
    /// the functions whose code it carries; `module` is the bindgened module
    /// and `refs` its functions' references ([`Self::function_refs`]).
    pub(crate) fn plan(
        &self,
        module: &Module,
        refs: &HashMap<FunctionId, Vec<u32>>,
        claimants: Vec<(Claimant, Vec<FunctionId>)>,
        modules: usize,
    ) -> DataPlan {
        let mut plan = DataPlan {
            moved: vec![Vec::new(); modules],
            removed: Vec::new(),
        };
        if let Some(why) = self.refusal(module) {
            eprintln!("wasm-split: all static data stays in the start module: {why}");
            return plan;
        }

        let mut owner = vec![Owner::Nobody; self.symbols.len()];
        // Why each symbol the start module keeps is there.
        let mut cause = vec![Cause::Nobody; self.symbols.len()];
        let claim = |owner: &mut Vec<Owner>,
                     cause: &mut Vec<Cause>,
                     who: Owner,
                     roots: Vec<(u32, Cause)>| {
            let mut seen = HashSet::new();
            let mut queue: VecDeque<(u32, Cause)> = roots.into_iter().collect();
            while let Some((symbol, why)) = queue.pop_front() {
                if !seen.insert(symbol) {
                    continue;
                }
                let current = &mut owner[symbol as usize];
                let next = match (*current, who) {
                    (Owner::Nobody, who) => who,
                    (current, who) if current == who => who,
                    _ => Owner::Start,
                };
                if next == Owner::Start && *current != Owner::Start {
                    cause[symbol as usize] = if who == Owner::Start {
                        why
                    } else {
                        Cause::Shared
                    };
                }
                *current = next;
                queue.extend(
                    self.points_to[symbol as usize]
                        .iter()
                        .map(|target| (*target, Cause::Via(symbol))),
                );
            }
        };

        // The start module's own: its code, its globals' initial values
        // (an address a global holds at start), and the pinned symbols.
        let mut start_roots: Vec<(u32, Cause)> = self
            .pinned
            .iter()
            .map(|symbol| (*symbol, Cause::Pinned))
            .collect();
        for global in module.globals.iter() {
            if let GlobalKind::Local(ConstExpr::Value(ir::Value::I32(value))) = global.kind {
                let mut symbols = Vec::new();
                self.referenced_by(value as u32, &mut symbols);
                start_roots.extend(symbols.into_iter().map(|symbol| (symbol, Cause::Global)));
            }
        }
        let mut split_roots = Vec::new();
        for (claimant, functions) in claimants {
            let roots: Vec<(u32, Cause)> = functions
                .iter()
                .flat_map(|function| {
                    refs.get(function)
                        .into_iter()
                        .flatten()
                        .map(move |symbol| (*symbol, Cause::Code(*function)))
                })
                .collect();
            match claimant {
                Claimant::Start => start_roots.extend(roots),
                Claimant::Module(index) => split_roots.push((index, roots)),
            }
        }
        claim(&mut owner, &mut cause, Owner::Start, start_roots);
        for (index, roots) in split_roots {
            claim(&mut owner, &mut cause, Owner::Split(index), roots);
        }
        if let Some(count) = std::env::var("WASM_SPLIT_DATA_WHY")
            .ok()
            .and_then(|count| count.parse::<usize>().ok())
        {
            self.explain(module, &owner, &cause, count);
        }

        // A piece of memory moves when every symbol covering it moves to the
        // same split module.
        let mut runs: Vec<(usize, Range<u32>)> = Vec::new();
        for k in 0..self.covering.len().saturating_sub(1) {
            let mut piece_owner = None;
            for symbol in &self.covering[k] {
                piece_owner = match (piece_owner, owner[*symbol as usize]) {
                    (None, Owner::Split(index)) => Some(Some(index)),
                    (Some(Some(a)), Owner::Split(b)) if a == b => Some(Some(a)),
                    _ => Some(None),
                };
            }
            let Some(Some(index)) = piece_owner else {
                continue;
            };
            let range = self.bounds[k]..self.bounds[k + 1];
            match runs.last_mut() {
                Some((last, run)) if *last == index && run.end == range.start => {
                    run.end = range.end
                }
                _ => runs.push((index, range)),
            }
        }
        let mut moved_bytes = 0u64;
        for (index, run) in runs {
            if run.end - run.start < MIN_RUN {
                continue;
            }
            let Some(bytes) = self.bytes(run.clone()) else {
                continue;
            };
            // Zeros are what the memory holds anyway; the start module's
            // copy of them costs nothing once wasm-opt drops it.
            if bytes.iter().all(|byte| *byte == 0) {
                continue;
            }
            moved_bytes += bytes.len() as u64;
            plan.removed.push(run.clone());
            // The range holds zeros until this module writes it, so its
            // longer zero stretches need no bytes (the split module's memory
            // is imported, so wasm-opt cannot drop them as it does in the
            // start module's own).
            let mut piece_start = 0;
            let mut at = 0;
            while at < bytes.len() {
                let zeros = bytes[at..].iter().take_while(|byte| **byte == 0).count();
                if zeros >= ZERO_GAP || (zeros > 0 && (at == 0 || at + zeros == bytes.len())) {
                    if at > piece_start {
                        plan.moved[index].push((
                            run.start + piece_start as u32,
                            bytes[piece_start..at].to_vec(),
                        ));
                    }
                    piece_start = at + zeros;
                }
                at += zeros.max(1);
            }
            if bytes.len() > piece_start {
                plan.moved[index].push((
                    run.start + piece_start as u32,
                    bytes[piece_start..].to_vec(),
                ));
            }
        }
        plan.removed.sort_by_key(|range| range.start);
        let modules = plan.moved.iter().filter(|runs| !runs.is_empty()).count();
        eprintln!(
            "wasm-split: {moved_bytes} bytes of static data moved out of the start module into {modules} other modules"
        );
        plan
    }

    /// Print the `count` largest symbols the start module keeps, and why
    /// (`WASM_SPLIT_DATA_WHY=count`): where to look for the next cut.
    fn explain(&self, module: &Module, owner: &[Owner], cause: &[Cause], count: usize) {
        let function = |id: FunctionId| {
            module
                .funcs
                .get(id)
                .name
                .clone()
                .unwrap_or_else(|| format!("{id:?}"))
        };
        let mut kept: Vec<u32> = (0..self.symbols.len() as u32)
            .filter(|symbol| !matches!(owner[*symbol as usize], Owner::Split(_)))
            .collect();
        kept.sort_by_key(|symbol| std::cmp::Reverse(self.symbols[*symbol as usize].len()));
        for symbol in kept.into_iter().take(count) {
            let mut chain = vec![symbol];
            let mut why = cause[symbol as usize];
            while let Cause::Via(parent) = why {
                if chain.contains(&parent) || chain.len() > 8 {
                    break;
                }
                chain.push(parent);
                why = cause[parent as usize];
            }
            let reason = match why {
                Cause::Nobody => "nothing reaches it".to_string(),
                Cause::Code(id) => format!("start code {}", function(id)),
                Cause::Global => "a global's initial value".to_string(),
                Cause::Pinned => "pinned".to_string(),
                Cause::Shared => "several modules".to_string(),
                Cause::Via(_) => "a cycle of statics".to_string(),
            };
            let path = chain
                .iter()
                .map(|symbol| self.names[*symbol as usize].as_str())
                .collect::<Vec<_>>()
                .join(" <- ");
            eprintln!(
                "{:>9} {path}: {reason}",
                self.symbols[symbol as usize].len()
            );
        }
    }

    /// Why the bindgened `module`'s data cannot be planned with this layout.
    fn refusal(&self, module: &Module) -> Option<String> {
        if let Some(why) = &self.refused {
            return Some(why.clone());
        }
        let mut active = Vec::new();
        for data in module.data.iter() {
            match &data.kind {
                DataKind::Active {
                    offset: ConstExpr::Value(ir::Value::I32(base)),
                    ..
                } => active.push((*base as u32, &data.value)),
                _ => return Some("the bindgened module has a passive data segment".into()),
            }
        }
        active.sort_by_key(|(base, _)| *base);
        let mut linked: Vec<_> = self
            .segments
            .iter()
            .filter(|(_, bytes)| !bytes.is_empty())
            .map(|(base, bytes)| (*base, bytes))
            .collect();
        linked.sort_by_key(|(base, _)| *base);
        let same = active.len() == linked.len()
            && active
                .iter()
                .zip(&linked)
                .all(|((a, x), (b, y))| a == b && x.as_slice() == y.as_slice());
        (!same).then(|| "the bindgened module's data differs from the linked module's".to_string())
    }

    /// The linked module's initial bytes at `range`, when one segment holds
    /// them all.
    fn bytes(&self, range: Range<u32>) -> Option<Vec<u8>> {
        self.segments.iter().find_map(|(base, bytes)| {
            let end = base + bytes.len() as u32;
            (*base <= range.start && range.end <= end)
                .then(|| bytes[(range.start - base) as usize..(range.end - base) as usize].to_vec())
        })
    }
}

impl DataPlan {
    /// Leave the moved ranges out of the start module's active segments.
    pub(crate) fn apply_to_start(&self, out: &mut Module) {
        if self.removed.is_empty() {
            return;
        }
        let ids: Vec<_> = out.data.iter().map(|data| data.id()).collect();
        for id in ids {
            let data = out.data.get(id);
            let DataKind::Active {
                memory,
                offset: ConstExpr::Value(ir::Value::I32(base)),
            } = data.kind
            else {
                continue;
            };
            let base = base as u32;
            let end = base + data.value.len() as u32;
            let first = self.removed.partition_point(|range| range.end <= base);
            let cuts: Vec<_> = self.removed[first..]
                .iter()
                .take_while(|range| range.start < end)
                .collect();
            if cuts.is_empty() {
                continue;
            }
            let value = std::mem::take(&mut out.data.get_mut(id).value);
            let mut kept = Vec::new();
            let mut at = base;
            for cut in cuts {
                if cut.start > at {
                    kept.push(at..cut.start);
                }
                at = at.max(cut.end);
            }
            if at < end {
                kept.push(at..end);
            }
            out.data.delete(id);
            out.memories.get_mut(memory).data_segments.remove(&id);
            for range in kept {
                let bytes =
                    value[(range.start - base) as usize..(range.end - base) as usize].to_vec();
                let piece = out.data.add(
                    DataKind::Active {
                        memory,
                        offset: ConstExpr::Value(ir::Value::I32(range.start as i32)),
                    },
                    bytes,
                );
                out.memories.get_mut(memory).data_segments.insert(piece);
            }
        }
    }

    /// Give split module or shared chunk `index` (whose memory is imported)
    /// the data it owns, at its addresses, in place of the copy of every
    /// segment.
    pub(crate) fn apply_to_split(&self, out: &mut Module, index: usize) {
        let ids: Vec<_> = out.data.iter().map(|data| data.id()).collect();
        for id in ids {
            out.data.delete(id);
        }
        let Some(memory) = out.memories.iter().next().map(|memory| memory.id()) else {
            return;
        };
        out.memories.get_mut(memory).data_segments.clear();
        for (address, bytes) in self.moved.get(index).into_iter().flatten() {
            let data = out.data.add(
                DataKind::Active {
                    memory,
                    offset: ConstExpr::Value(ir::Value::I32(*address as i32)),
                },
                bytes.clone(),
            );
            out.memories.get_mut(memory).data_segments.insert(data);
        }
    }
}

/// The value of an `i32.const`, or a load's or store's offset.
fn immediate(operator: &wasmparser::Operator) -> Option<u32> {
    use wasmparser::Operator::*;
    let memarg = match operator {
        I32Const { value } => return Some(*value as u32),
        I32Load { memarg }
        | I64Load { memarg }
        | F32Load { memarg }
        | F64Load { memarg }
        | I32Load8S { memarg }
        | I32Load8U { memarg }
        | I32Load16S { memarg }
        | I32Load16U { memarg }
        | I64Load8S { memarg }
        | I64Load8U { memarg }
        | I64Load16S { memarg }
        | I64Load16U { memarg }
        | I64Load32S { memarg }
        | I64Load32U { memarg }
        | I32Store { memarg }
        | I64Store { memarg }
        | F32Store { memarg }
        | F64Store { memarg }
        | I32Store8 { memarg }
        | I32Store16 { memarg }
        | I64Store8 { memarg }
        | I64Store16 { memarg }
        | I64Store32 { memarg } => memarg,
        _ => return None,
    };
    u32::try_from(memarg.offset).ok()
}
