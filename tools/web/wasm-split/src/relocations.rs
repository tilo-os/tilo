//! The linked module's relocations, as the call graph the split is made from:
//! which function calls which, read from the entries the linker left in the
//! module (`--emit-relocs`), and the symbols that say where each function and
//! data symbol is.
//!
//! The entries are read instead of the code because wasm-bindgen rewrites
//! bodies; a body's size moves an entry's offset, the edge it holds not.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;

use anyhow::{Context, Result};
use itertools::Itertools;
use walrus::{FunctionId, Module};
use wasmparser::{
    BinaryReader, Linking, LinkingSectionReader, Payload, RelocSectionReader, RelocationEntry,
    RelocationType, SymbolInfo,
};

use crate::split::{Node, code_section, keep_function_bodies};

pub(crate) struct ModuleWithRelocations<'a> {
    /// The module's functions, tables and names; function bodies are
    /// `unreachable` (see `new`).
    pub(crate) module: Module,
    /// Each local function's code, as relocation offsets count: from the
    /// code section's contents, including the body's size.
    code_ranges: Vec<(FunctionId, Range<usize>)>,
    symbols: Vec<SymbolInfo<'a>>,
    funcs_by_index: Vec<FunctionId>,
    pub(crate) call_graph: HashMap<Node, HashSet<Node>>,
    /// Each data symbol's name, by its index in the linked module.
    pub(crate) data_names: HashMap<usize, String>,
    parents: HashMap<Node, HashSet<Node>>,
    relocation_map: HashMap<Node, Vec<RelocationEntry>>,
    pub(crate) data_symbols: BTreeMap<usize, DataSymbol>,
    data_section_range: Range<usize>,
}

impl<'a> ModuleWithRelocations<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Result<Self> {
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

        let data_names = raw_data
            .symbols
            .iter()
            .enumerate()
            .filter_map(|(index, symbol)| match symbol {
                SymbolInfo::Data { name, .. } if !name.is_empty() => {
                    Some((index, name.to_string()))
                }
                _ => None,
            })
            .collect();
        let mut module = Self {
            module,
            code_ranges,
            data_symbols: raw_data.data_symbols,
            data_section_range: raw_data.data_range,
            symbols: raw_data.symbols,
            funcs_by_index,
            call_graph: Default::default(),
            data_names,
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
        // A module whose static data holds no address has no data
        // relocations, and the linker writes no section for none.
        let data_relocations = if self
            .module
            .customs
            .iter()
            .any(|(_, c)| c.name() == "reloc.DATA")
        {
            self.collect_relocations_from_section("reloc.DATA")?
        } else {
            Vec::new()
        };
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

pub(crate) struct RawDataSection<'a> {
    data_range: Range<usize>,
    symbols: Vec<SymbolInfo<'a>>,
    data_symbols: BTreeMap<usize, DataSymbol>,
}

#[derive(Debug)]
pub(crate) struct DataSymbol {
    pub(crate) index: usize,
    pub(crate) range: Range<usize>,
}

/// Manually parse the data section from a wasm module
///
/// We need to do this for data symbols because walrus doesn't provide the right range and offset
/// information for data segments. Fortunately, it provides it for code sections, so we only need to
/// do a small amount extra of parsing here.
pub(crate) fn parse_bytes_to_data_segment(bytes: &[u8]) -> Result<RawDataSection<'_>> {
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

/// Each data symbol's identity by content: its name, hashed, by symbol
/// index. A symbol's index moves when a function is added or removed (the
/// table lists both), while what it names does not: a module keyed by these
/// keeps its key under a renumbering (split.rs's `node_key`).
///
/// A symbol without a name is its index: the linker names data it cannot
/// merge, so an unnamed one is rare and changes little with the numbering.
pub(crate) fn data_symbol_ids(original: &[u8]) -> Result<Vec<u64>> {
    use std::hash::Hasher as _;

    let mut ids = Vec::new();
    for payload in wasmparser::Parser::new(0).parse_all(original) {
        let Payload::CustomSection(section) = payload? else {
            continue;
        };
        if section.name() != "linking" {
            continue;
        }
        let reader = LinkingSectionReader::new(BinaryReader::new(section.data(), 0))?;
        for subsection in reader.subsections() {
            let Linking::SymbolTable(map) = subsection? else {
                continue;
            };
            for (index, symbol) in map.into_iter().enumerate() {
                let symbol = symbol?;
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                match &symbol {
                    SymbolInfo::Data { name, .. } => {
                        hasher.write_u8(1);
                        hasher.write(name.as_bytes());
                    }
                    _ => {
                        hasher.write_u8(0);
                        hasher.write_usize(index);
                    }
                }
                ids.resize(index + 1, 0);
                ids[index] = hasher.finish();
            }
        }
    }
    Ok(ids)
}

/// Each data symbol's name, by its index in the linked module: the identity
/// a function added or removed leaves as it was, while the symbol's index
/// moves (the table lists functions and data together).
pub(crate) fn data_names(original: &[u8]) -> Result<HashMap<usize, String>> {
    let mut names = HashMap::new();
    for payload in wasmparser::Parser::new(0).parse_all(original) {
        let Payload::CustomSection(section) = payload? else {
            continue;
        };
        if section.name() != "linking" {
            continue;
        }
        let reader = LinkingSectionReader::new(BinaryReader::new(section.data(), 0))?;
        for subsection in reader.subsections() {
            let Linking::SymbolTable(map) = subsection? else {
                continue;
            };
            for (index, symbol) in map.into_iter().enumerate() {
                if let SymbolInfo::Data { name, .. } = symbol? {
                    if !name.is_empty() {
                        names.insert(index, name.to_string());
                    }
                }
            }
        }
    }
    Ok(names)
}

/// The data symbols each of `wanted`'s functions holds a relocation to, by
/// the linked module's local function index.
///
/// The splitter's plan has an edge from a function to every symbol its code
/// renames; a plan a function added or removed leaves as it was keeps the
/// edges of untouched functions, and this reads the ones the changed
/// functions have now without reading the whole call graph again.
pub(crate) fn code_data_edges(
    original: &[u8],
    wanted: &HashSet<usize>,
) -> Result<HashMap<usize, Vec<usize>>> {
    if wanted.is_empty() {
        return Ok(HashMap::new());
    }
    let raw = parse_bytes_to_data_segment(original)?;
    let symbols = &raw.symbols;
    let code = code_section(original)?.context("module has no code section")?;
    let mut imported = 0usize;
    for payload in wasmparser::Parser::new(0).parse_all(original) {
        let Payload::ImportSection(imports) = payload? else {
            continue;
        };
        for import in imports {
            if matches!(import?.ty, wasmparser::TypeRef::Func(_)) {
                imported += 1;
            }
        }
    }
    // Each local function's code, as relocation offsets count: from the code
    // section's contents, including the body's size LEB.
    let ranges = code
        .bodies
        .iter()
        .map(|body| {
            let size_len = (usize::BITS - body.len().leading_zeros())
                .div_ceil(7)
                .max(1);
            let start = body.start - code.contents - size_len as usize;
            start..body.end - code.contents
        })
        .collect::<Vec<_>>();
    let mut relocations = Vec::new();
    for payload in wasmparser::Parser::new(0).parse_all(original) {
        let Payload::CustomSection(section) = payload? else {
            continue;
        };
        if section.name() != "reloc.CODE" {
            continue;
        }
        let reader = RelocSectionReader::new(BinaryReader::new(section.data(), 0))?;
        relocations.extend(reader.entries().into_iter().flatten());
    }
    let mut edges: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut at = 0;
    for entry in relocations {
        let offset = entry.relocation_range().start;
        while at < ranges.len() && ranges[at].end <= offset {
            at += 1;
        }
        let Some(range) = ranges.get(at) else { break };
        if offset < range.start {
            continue;
        }
        if entry.ty == wasmparser::RelocationType::TypeIndexLeb
            || !wanted.contains(&(imported + at))
        {
            continue;
        }
        let Some(SymbolInfo::Data { .. }) = symbols.get(entry.index as usize) else {
            continue;
        };
        edges
            .entry(imported + at)
            .or_default()
            .push(entry.index as usize);
    }
    Ok(edges)
}
