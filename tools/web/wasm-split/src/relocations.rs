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
