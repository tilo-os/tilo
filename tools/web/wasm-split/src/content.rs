//! What a function body is by what it does, not by the numbers it holds.
//!
//! A body calls the functions and uses the types it names; when a function
//! is added or removed elsewhere, every index after it moves and the body's
//! bytes change, though the body still does the same. The cache key of a
//! module that holds it would move with it, and the module be emitted again
//! for a change that is no change of its own (cache.rs).
//!
//! [`body_content`] hashes a body with each index it holds replaced by the
//! identity of what it names (a function's name, a type's shape): the same
//! body of code hashes the same whatever the module's numbering is. It is
//! the key of a body, never its bytes: the module is emitted from the bytes.

use std::hash::Hasher as _;

use anyhow::{Result, anyhow};
use wasm_encoder::reencode::{Error as ReError, Reencode, utils};

/// What each index in the module names, by content: a function by its
/// identity (`Splitter::symbol_ids`), a type by its shape.
pub struct Content {
    pub functions: Vec<u64>,
    /// A type's identity: a dense number per distinct shape, in the order the
    /// shapes sort in, so a type added before others does not move them.
    pub types: Vec<u32>,
}

struct Canonical<'a> {
    content: &'a Content,
    /// Every index resolved, in the order the body holds it, with its whole
    /// identity: what the encoded bytes below carry is truncated to 32 bits.
    named: Vec<u64>,
}

impl Reencode for Canonical<'_> {
    type Error = String;

    fn function_index(&mut self, function: u32) -> Result<u32, ReError<String>> {
        let id = *self
            .content
            .functions
            .get(function as usize)
            .ok_or_else(|| ReError::UserError(format!("function {function} out of range")))?;
        self.named.push(id);
        Ok(id as u32)
    }

    fn type_index(&mut self, ty: u32) -> Result<u32, ReError<String>> {
        self.content
            .types
            .get(ty as usize)
            .copied()
            .ok_or_else(|| ReError::UserError(format!("type {ty} out of range")))
    }
}

/// `body`'s hash by what it does (see the module comment).
pub fn body_content(body: &[u8], content: &Content) -> Result<u64> {
    let function =
        wasmparser_content::FunctionBody::new(wasmparser_content::BinaryReader::new(body, 0));
    let mut canonical = Canonical {
        content,
        named: Vec::new(),
    };
    let mut section = wasm_encoder::CodeSection::new();
    let mut out = utils::new_function_with_parsed_locals(&mut canonical, &function)
        .map_err(|error| anyhow!("reading a body's locals: {error:?}"))?;
    let mut readers = function
        .get_operators_reader()
        .map_err(|error| anyhow!("reading a body: {error}"))?;
    while !readers.eof() {
        let instruction = utils::parse_instruction(&mut canonical, &mut readers)
            .map_err(|error| anyhow!("reading a body's instructions: {error:?}"))?;
        out.instruction(&instruction);
    }
    section.function(&out);
    let mut encoded = Vec::new();
    wasm_encoder::Section::append_to(&section, &mut encoded);

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write_usize(canonical.named.len());
    for id in &canonical.named {
        hasher.write_u64(*id);
    }
    hasher.write(&encoded);
    Ok(hasher.finish())
}

/// Every type's dense identity, in the module's order: two types of the same
/// shape answer the same, so a type added before them moves no body's hash.
pub fn type_content(types: &[String]) -> Vec<u32> {
    let mut shapes: Vec<&str> = types.iter().map(String::as_str).collect();
    shapes.sort_unstable();
    shapes.dedup();
    let dense = |shape: &str| {
        shapes
            .binary_search(&shape)
            .expect("every shape is in the sorted list") as u32
    };
    types.iter().map(|shape| dense(shape)).collect()
}

/// Every type's shape, in the module's order, as [`type_content`] reads
/// them.
pub fn type_shapes(module: &[u8]) -> Result<Vec<String>> {
    let mut shapes = Vec::new();
    for payload in wasmparser_content::Parser::new(0).parse_all(module) {
        if let wasmparser_content::Payload::TypeSection(reader) = payload? {
            for ty in reader.into_iter_err_on_gc_types() {
                let ty = ty?;
                shapes.push(format!("({:?})->({:?})", ty.params(), ty.results()));
            }
        }
    }
    Ok(shapes)
}

/// What each function index is by content: a local's name, an imported
/// function's module and name.
///
/// A function's identity, not its code: what a module's imports and table
/// slots name is the function, and a body that changed is keyed by the module
/// that holds it (`crate::split::SplitParts`), so one function's code moving
/// emits its own module and no other.
pub fn symbol_ids(
    module: &walrus::Module,
    ids: &[walrus::FunctionId],
    imported: usize,
) -> Vec<u64> {
    let _ = imported;
    ids.iter()
        .map(|id| {
            let function = &module.funcs.get(*id);
            let mut key = crate::cache::Key::new();
            key.add(&function.name);
            match &function.kind {
                walrus::FunctionKind::Local(_) => {
                    key.add(&"local");
                }
                walrus::FunctionKind::Import(imported) => {
                    let import = &module.imports.get(imported.import);
                    key.add(&"import").add(&import.module).add(&import.name);
                }
                _ => {
                    key.add(&"other");
                }
            }
            let finished = key.finish();
            u64::from_str_radix(&finished[..16], 16).unwrap_or(0)
        })
        .collect()
}

/// Told apart from a body's own hash in [`body_hashes`]: a body whose content
/// could not be read is keyed by its bytes.
const CONTENT_FALLBACK: u64 = 0x5157_1d3c_9a6e_2b08;

/// What each local body is by content, in index order among them: a body
/// whose bytes moved only because a function was added or removed elsewhere
/// hashes as it did. A body the content hash cannot read (an index outside
/// the module the shape parser saw) is keyed by its bytes: an emission is
/// made again where it need not be, never read back where it must not be.
pub fn body_hashes(module: &[u8], symbols: &[u64]) -> Vec<u64> {
    use rayon::prelude::*;

    let Some(code) = crate::split::code_section(module).ok().flatten() else {
        return Vec::new();
    };
    let types = type_shapes(module)
        .map(|shapes| type_content(&shapes))
        .unwrap_or_default();
    let content = Content {
        functions: symbols.to_vec(),
        types,
    };
    let content = &content;
    code.bodies
        .par_iter()
        .map(|body| {
            let bytes = &module[body.clone()];
            body_content(bytes, content)
                .unwrap_or_else(|_| crate::analysis::hash_body(bytes) ^ CONTENT_FALLBACK)
        })
        .collect()
}
