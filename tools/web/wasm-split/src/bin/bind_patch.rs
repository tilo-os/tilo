//! Patch a wasm-bindgen output instead of binding the whole module again
//! (`tools/web/wasm-split/bind-patch.sh`).
//!
//! Binding a 264 MB module costs tens of seconds, almost all of it re-encoding
//! bodies that did not change. walrus, the library wasm-bindgen binds with,
//! emits the local functions sorted by instruction count (largest first, then
//! the order the module declares them), renumbers their locals (arguments,
//! then the used locals grouped by type in `ValType` order) and rewrites
//! function and type indices canonically. Every part of that is a function of
//! the module's structure and of each body alone, so one body's output can be
//! worked out without binding the rest.
//!
//! `record` is run once after a real bind: it reads the linked module and
//! wasm-bindgen's output, pairs every function by name (names survive the
//! bind; functions the bind dropped are left unresolved), learns the
//! function and type index maps, and checks that the emitted order is the
//! one above. It keeps the bound module and that map beside the build
//! directory's caches.
//!
//! `apply` reads the next linked module and patches the kept bound module
//! when the change is one of bodies or of the set of functions: the same
//! imports, exports, tables, globals, data and custom sections, functions
//! added or removed at their source order, and every changed body
//! translatable with the maps. A function added or removed moves the
//! positions the bind emits functions at, and every index after it; the kept
//! bodies are renumbered and the new and changed ones translated, and the
//! type, import, function, export, start, element, name and code sections
//! are written again. Anything else exits `NEEDS_BIND` and the caller binds
//! fully: this runs in a development loop, where a slower bundle is a
//! nuisance and a bundle differing from a fresh build is a bug.
//!
//! A patch is used only when the bound module it keeps records that this
//! tool writes every section again exactly (its `record` reconstructs them
//! from the kept bytes), and that the functions wasm-bindgen keeps are
//! exactly the ones reachable from the module's roots in the bound module:
//! a function a full bind's garbage collector would drop or keep then cannot
//! make the patched module differ.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use wasm_encoder::reencode::{Error as ReError, Reencode, utils};
use wasm_encoder::{Encode, Instruction};
use wasmparser_content as wp;
use wp::{Operator, Parser, Payload, TypeRef, ValType};

/// Exit status saying the caller must bind fully: the module is no body-only
/// change away from what the kept bound module was made from.
const NEEDS_BIND: i32 = 3;

const MAGIC: &[u8; 8] = b"WSBIND\0\x08";
/// The file `apply` writes for the splitter: what every function of the kept
/// bound module became (see [`write_renumbering`]).
const RENUMBER_MAGIC: &[u8; 8] = b"WSBFNM\0\x02";

/// How many bodies one patch may replace. A command that reformats a file
/// still patches; one that rewrites a module binds.
const MAX_CHANGED: usize = 64;

/// How many functions or types one patch may add or remove. A one-line
/// change adds one closure or helper; a module that changed shape more than
/// this binds.
const MAX_DELTA: usize = 8;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((command, rest)) = args.split_first() else {
        bail!("usage: wasm-bind-patch record|apply …");
    };
    let mut options = Options::default();
    let mut at = 0;
    while at < rest.len() {
        let value = rest
            .get(at + 1)
            .with_context(|| format!("{} needs a value", rest[at]))?;
        match rest[at].as_str() {
            "--linked" => options.linked = Some(PathBuf::from(value)),
            "--bound" => options.bound = Some(PathBuf::from(value)),
            "--cache" => options.cache = Some(PathBuf::from(value)),
            "--out" => options.out = Some(PathBuf::from(value)),
            other => bail!("unknown option {other}"),
        }
        at += 2;
    }
    match command.as_str() {
        "record" => match record(&options) {
            Ok(()) => Ok(()),
            Err(error) if unusable(&error) => {
                eprintln!("wasm-bind-patch: not keeping a map: {error}");
                std::process::exit(NEEDS_BIND)
            }
            Err(error) => Err(error),
        },
        "apply" => match apply(&options) {
            Ok(()) => Ok(()),
            Err(error) if unusable(&error) => {
                eprintln!("wasm-bind-patch: binding fully: {error}");
                std::process::exit(NEEDS_BIND)
            }
            Err(error) => Err(error),
        },
        other => bail!("unknown command {other}"),
    }
}

#[derive(Default)]
struct Options {
    linked: Option<PathBuf>,
    bound: Option<PathBuf>,
    cache: Option<PathBuf>,
    out: Option<PathBuf>,
}

impl Options {
    fn linked(&self) -> Result<&Path> {
        self.linked.as_deref().context("--linked is required")
    }
    fn cache(&self) -> Result<&Path> {
        self.cache.as_deref().context("--cache is required")
    }
}

/// An expected reason to leave the module to a full bind, told apart from a
/// bug (a file that cannot be read is a bug: the caller already read it).
#[derive(Debug)]
struct Unusable(String);

impl std::fmt::Display for Unusable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Unusable {}

fn unusable(error: &anyhow::Error) -> bool {
    error.downcast_ref::<Unusable>().is_some()
}

fn needs_bind(reason: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Unusable(reason.into()))
}

// ---------------------------------------------------------------- the module

/// One section of a wasm module: its id, and the range its contents occupy
/// (the id and size LEBs are not part of it).
struct Section {
    id: u8,
    /// Where the section's id byte is.
    start: usize,
    contents: Range<usize>,
}

fn sections(bytes: &[u8]) -> Result<Vec<Section>> {
    let mut out = Vec::new();
    let mut at = 8;
    while at < bytes.len() {
        let id = bytes[at];
        let section_start = at;
        at += 1;
        let size = read_leb(bytes, &mut at)?;
        let start = at;
        at = at
            .checked_add(size as usize)
            .context("section size overflows")?;
        if at > bytes.len() {
            bail!("section {id} runs past the module");
        }
        out.push(Section {
            id,
            start: section_start,
            contents: start..at,
        });
    }
    Ok(out)
}

fn read_leb(bytes: &[u8], at: &mut usize) -> Result<u32> {
    let mut result = 0u32;
    let mut shift = 0;
    loop {
        let byte = *bytes.get(*at).context("LEB runs past the module")?;
        *at += 1;
        result |= ((byte & 0x7f) as u32) << shift;
        if byte & 0x80 == 0 {
            return Ok(result);
        }
        shift += 7;
        if shift >= 32 {
            bail!("LEB is too long");
        }
    }
}

fn write_leb(out: &mut Vec<u8>, mut value: u32) {
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

fn code_section(bytes: &[u8], sections: &[Section]) -> Result<(Section, Vec<Range<usize>>)> {
    let code = sections
        .iter()
        .find(|section| section.id == 10)
        .context("the module has no code section")?;
    let mut at = code.contents.start;
    let count = read_leb(bytes, &mut at)? as usize;
    let mut bodies = Vec::with_capacity(count);
    for _ in 0..count {
        let size = read_leb(bytes, &mut at)? as usize;
        let start = at;
        at = at.checked_add(size).context("body size overflows")?;
        if at > code.contents.end {
            bail!("a body runs past the code section");
        }
        bodies.push(start..at);
    }
    Ok((
        Section {
            id: 10,
            start: code.start,
            contents: code.contents.clone(),
        },
        bodies,
    ))
}

/// A custom section's name, which its contents hold first.
fn custom_name(bytes: &[u8], contents: usize) -> Result<Option<String>> {
    let mut at = contents;
    let len = read_leb(bytes, &mut at)? as usize;
    let Some(name) = at.checked_add(len).and_then(|end| bytes.get(at..end)) else {
        return Ok(None);
    };
    Ok(String::from_utf8(name.to_vec()).ok())
}

/// What a function added or removed does not move, hashed as one. The
/// sections it leaves out are the ones it rewrites (the types, functions,
/// exports, start, elements, code and the name section) and the data, which
/// the bind copies from the module it is given: those are compared by
/// content instead.
fn structure_hash(bytes: &[u8], sections: &[Section]) -> Result<Vec<(u8, u64)>> {
    let mut out = Vec::new();
    for section in sections {
        // Custom sections are wasm-bindgen's own: it drops the linker's and
        // the debug information's, and writes the rest from what it is
        // given, not from the module's code.
        let indexed = matches!(section.id, 0 | 1 | 3 | 7 | 8 | 9 | 10 | 11);
        if indexed {
            continue;
        }
        let mut hasher = Fnv::default();
        hasher.byte(section.id);
        hasher.raw(&bytes[section.contents.clone()]);
        out.push((section.id, hasher.finish()));
    }
    Ok(out)
}

/// Every section but the code, hashed as one: a body-only change leaves all
/// of it as it was.
fn structure_hash_of_all(bytes: &[u8], sections: &[Section]) -> Result<u64> {
    let mut hasher = Fnv::default();
    for section in sections {
        if section.id == 10 {
            continue;
        }
        hasher.byte(section.id);
        hasher.raw(&bytes[section.contents.clone()]);
    }
    Ok(hasher.finish())
}

/// A function type, compared as walrus compares and orders types.
#[derive(Clone, PartialEq, Eq, Hash)]
struct Shape {
    params: Vec<u8>,
    results: Vec<u8>,
}

fn valtype_code(ty: ValType) -> Result<u8> {
    Ok(match ty {
        ValType::I32 => 0,
        ValType::I64 => 1,
        ValType::F32 => 2,
        ValType::F64 => 3,
        ValType::V128 => 4,
        ValType::Ref(reference) if reference == wp::RefType::FUNCREF => 5,
        ValType::Ref(reference) if reference == wp::RefType::EXTERNREF => 6,
        ValType::Ref(_) => bail!("a reference type this tool does not order"),
    })
}

impl Shape {
    fn of(ty: &wp::FuncType) -> Result<Self> {
        Ok(Self {
            params: ty
                .params()
                .iter()
                .map(|ty| valtype_code(*ty))
                .collect::<Result<_>>()?,
            results: ty
                .results()
                .iter()
                .map(|ty| valtype_code(*ty))
                .collect::<Result<_>>()?,
        })
    }

    fn encode(&self, out: &mut Vec<u8>) {
        fn value(code: u8) -> u8 {
            match code {
                0 => 0x7f,
                1 => 0x7e,
                2 => 0x7d,
                3 => 0x7c,
                4 => 0x7b,
                5 => 0x70,
                _ => 0x6f,
            }
        }
        out.push(0x60);
        write_leb(out, self.params.len() as u32);
        out.extend(self.params.iter().map(|code| value(*code)));
        write_leb(out, self.results.len() as u32);
        out.extend(self.results.iter().map(|code| value(*code)));
    }
}

/// One element segment: an active segment of function indices.
#[derive(Clone, PartialEq, Eq)]
struct Element {
    table: u32,
    /// The offset's constant expression, bytes and all.
    offset: Vec<u8>,
    funcs: Vec<u32>,
}

/// The module's exports, start and element segments: the places a function
/// added or removed renumbers, and what says the module's shape did not move.
#[derive(Clone, PartialEq, Eq)]
struct Links {
    /// Each export: its name, kind (0 function, 1 table, 2 memory, 3 global)
    /// and index.
    exports: Vec<(String, u8, u32)>,
    start: Option<u32>,
    elements: Vec<Element>,
}

/// A module's function and type tables and names.
struct Meta {
    imported: u32,
    /// The imported functions' type indices, in order.
    import_types: Vec<u32>,
    /// The local functions' type indices, in source order.
    func_types: Vec<u32>,
    shapes: Vec<Shape>,
    /// Each type's parameter count.
    params: Vec<u32>,
    names: HashMap<u32, String>,
    type_names: HashMap<u32, String>,
    /// The local functions' names and type shapes, in source order.
    local_names: Vec<Option<String>>,
    local_shapes: Vec<Shape>,
}

impl Meta {
    fn nargs(&self, local: usize) -> u32 {
        self.func_types
            .get(local)
            .and_then(|ty| self.params.get(*ty as usize))
            .copied()
            .unwrap_or(0)
    }

    fn name(&self, function: u32) -> Option<&str> {
        self.names.get(&function).map(String::as_str)
    }
}

fn read_meta(bytes: &[u8]) -> Result<Meta> {
    let mut imported = 0u32;
    let mut import_types = Vec::new();
    let mut func_types = Vec::new();
    let mut shapes = Vec::new();
    let mut params = Vec::new();
    let mut names = HashMap::new();
    let mut type_names = HashMap::new();
    for payload in Parser::new(0).parse_all(bytes) {
        match payload? {
            Payload::ImportSection(imports) => {
                for group in imports {
                    for import in group? {
                        if let TypeRef::Func(ty) = import?.1.ty {
                            import_types.push(ty);
                            imported += 1;
                        }
                    }
                }
            }
            Payload::TypeSection(types) => {
                for ty in types.into_iter_err_on_gc_types() {
                    let ty = ty?;
                    params.push(ty.params().len() as u32);
                    shapes.push(Shape::of(&ty)?);
                }
            }
            Payload::FunctionSection(functions) => {
                for ty in functions {
                    func_types.push(ty?);
                }
            }
            Payload::CustomSection(c) if c.name() == "name" => {
                let reader =
                    wp::NameSectionReader::new(wp::BinaryReader::new(c.data(), c.data_offset()));
                for part in reader {
                    match part? {
                        wp::Name::Function(map) => {
                            for naming in map {
                                let naming = naming?;
                                names.insert(naming.index, naming.name.to_owned());
                            }
                        }
                        wp::Name::Type(map) => {
                            for naming in map {
                                let naming = naming?;
                                type_names.insert(naming.index, naming.name.to_owned());
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    let local_names = (0..func_types.len())
        .map(|local| names.get(&(local as u32 + imported)).cloned())
        .collect::<Vec<_>>();
    let local_shapes = func_types
        .iter()
        .map(|ty| {
            shapes
                .get(*ty as usize)
                .cloned()
                .context("a function names a type the module does not have")
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Meta {
        imported,
        import_types,
        func_types,
        shapes,
        params,
        names,
        type_names,
        local_names,
        local_shapes,
    })
}

fn read_links(bytes: &[u8]) -> Result<Links> {
    let mut links = Links {
        exports: Vec::new(),
        start: None,
        elements: Vec::new(),
    };
    for payload in Parser::new(0).parse_all(bytes) {
        match payload? {
            Payload::ExportSection(exports) => {
                for export in exports {
                    let export = export?;
                    let kind = match export.kind {
                        wp::ExternalKind::Func => 0,
                        wp::ExternalKind::Table => 1,
                        wp::ExternalKind::Memory => 2,
                        wp::ExternalKind::Global => 3,
                        other => bail!("an export this tool does not renumber: {other:?}"),
                    };
                    links
                        .exports
                        .push((export.name.to_owned(), kind, export.index));
                }
            }
            Payload::StartSection { func, .. } => links.start = Some(func),
            Payload::ElementSection(elements) => {
                for element in elements {
                    let element = element?;
                    let wp::ElementItems::Functions(functions) = element.items else {
                        bail!("an element segment of expressions");
                    };
                    let wp::ElementKind::Active {
                        table_index,
                        offset_expr,
                    } = element.kind
                    else {
                        bail!("an element segment that is not active");
                    };
                    let mut funcs = Vec::new();
                    for function in functions {
                        funcs.push(function?);
                    }
                    let mut operators = offset_expr.get_operators_reader();
                    let start = operators.original_position();
                    while operators.read()? != Operator::End {}
                    let end = operators.original_position();
                    links.elements.push(Element {
                        // walrus writes no table index for table 0.
                        table: table_index.unwrap_or(0),
                        offset: bytes[start..end].to_vec(),
                        funcs,
                    });
                }
            }
            Payload::GlobalSection(globals) => {
                for global in globals {
                    let global = global?;
                    let mut operators = global.init_expr.get_operators_reader();
                    while !operators.eof() {
                        if matches!(operators.read()?, Operator::RefFunc { .. }) {
                            bail!("a global names a function");
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(links)
}

/// A body's identity: its operators with the function and type indices
/// replaced by what they are called in another index space. The bind
/// renumbers indices but does not change what a body does, so equal
/// identities mean equal bodies.
#[derive(Clone)]
struct BodyMeta {
    hash: u64,
    /// walrus's `size()`: instructions without the block terminators.
    size: u32,
}

fn body_meta(
    bytes: &[u8],
    span: &Range<usize>,
    funcs: &dyn Fn(u32) -> u32,
    types: &dyn Fn(u32) -> u32,
) -> Result<BodyMeta> {
    let body = wp::FunctionBody::new(wp::BinaryReader::new(&bytes[span.clone()], span.start));
    let mut operators = body.get_operators_reader()?;
    let mut hasher = Fnv::default();
    let mut size = 0u32;
    while !operators.eof() {
        let start = operators.original_position();
        let operator = operators.read()?;
        let end = operators.original_position();
        match operator {
            Operator::Call { function_index }
            | Operator::ReturnCall { function_index }
            | Operator::RefFunc { function_index } => {
                hasher.byte(1);
                hasher.u32(funcs(function_index));
            }
            Operator::CallIndirect { type_index, .. }
            | Operator::ReturnCallIndirect { type_index, .. } => {
                hasher.byte(2);
                hasher.u32(types(type_index));
            }
            Operator::Block { blockty }
            | Operator::Loop { blockty }
            | Operator::If { blockty }
            | Operator::Try { blockty } => match blockty {
                wp::BlockType::FuncType(ty) => {
                    hasher.byte(3);
                    hasher.u32(types(ty));
                }
                _ => {
                    hasher.byte(4);
                    hasher.raw(&bytes[start..end]);
                }
            },
            _ => {
                hasher.byte(5);
                hasher.raw(&bytes[start..end]);
            }
        }
        if !matches!(operator, Operator::End | Operator::Else) {
            size += 1;
        }
    }
    Ok(BodyMeta {
        hash: hasher.finish(),
        size,
    })
}

fn bodies_meta(
    bytes: &[u8],
    spans: &[Range<usize>],
    funcs: &[u32],
    types: &[u32],
) -> Result<Vec<BodyMeta>> {
    let func = |index: u32| funcs.get(index as usize).copied().unwrap_or(u32::MAX);
    let ty = |index: u32| types.get(index as usize).copied().unwrap_or(u32::MAX);
    spans
        .iter()
        .map(|span| body_meta(bytes, span, &func, &ty))
        .collect()
}

// ------------------------------------------------------------- the index maps

/// L's function index space to B's, `u32::MAX` where unresolved (a function
/// the bind dropped).
fn function_map(
    lmeta: &Meta,
    lspans: &[Range<usize>],
    lmeta_bodies: &[BodyMeta],
    bmeta: &Meta,
    bmeta_bodies: &[BodyMeta],
) -> Vec<u32> {
    let total = lmeta.imported as usize + lspans.len();
    let mut map = vec![u32::MAX; total];
    // Imports by name: the bind keeps their names; one only becomes two
    // imports it added.
    let mut imported: HashMap<&str, Vec<u32>> = HashMap::new();
    for (index, name) in &bmeta.names {
        if *index < bmeta.imported {
            imported.entry(name.as_str()).or_default().push(*index);
        }
    }
    for (index, name) in &lmeta.names {
        if *index < lmeta.imported {
            if let Some(candidates) = imported.get(name.as_str()) {
                if candidates.len() == 1 {
                    map[*index as usize] = candidates[0];
                }
            }
        }
    }
    // Local functions by name and size order: the bind emits them sorted by
    // (size descending, source order), and drops the ones nothing reaches.
    // A name whose group has the same functions on both sides with the same
    // sizes, in that order, maps one to one; anything else stays unresolved.
    let mut lgroups: HashMap<&str, Vec<(u32, usize)>> = HashMap::new();
    let mut bgroups: HashMap<&str, Vec<(u32, usize)>> = HashMap::new();
    for (local, meta) in lmeta_bodies.iter().enumerate() {
        if let Some(name) = lmeta.name(local as u32 + lmeta.imported) {
            lgroups.entry(name).or_default().push((meta.size, local));
        }
    }
    for (local, meta) in bmeta_bodies.iter().enumerate() {
        if let Some(name) = bmeta.name(local as u32 + bmeta.imported) {
            bgroups.entry(name).or_default().push((meta.size, local));
        }
    }
    for group in lgroups.values_mut() {
        group.sort_by_key(|(size, local)| (std::cmp::Reverse(*size), *local));
    }
    for group in bgroups.values_mut() {
        group.sort_by_key(|(size, local)| (std::cmp::Reverse(*size), *local));
    }
    for (name, lgroup) in &lgroups {
        let Some(bgroup) = bgroups.get(name) else {
            continue;
        };
        if lgroup.len() != bgroup.len() {
            continue;
        }
        if !lgroup
            .iter()
            .zip(bgroup)
            .all(|((lsize, _), (bsize, _))| lsize == bsize)
        {
            continue;
        }
        for ((_, llocal), (_, blocal)) in lgroup.iter().zip(bgroup) {
            map[*llocal + lmeta.imported as usize] = *blocal as u32 + bmeta.imported;
        }
    }
    map
}

/// L's type indices to B's, voted by the functions mapped above: a function's
/// own type is the same type on both sides.
fn type_map(lmeta: &Meta, bmeta: &Meta, map: &[u32]) -> Vec<u32> {
    let imported = lmeta.imported as usize;
    let mut votes: HashMap<u32, HashMap<u32, u32>> = HashMap::new();
    for (index, target) in map.iter().enumerate() {
        if *target == u32::MAX || index < imported {
            continue;
        }
        let local = index - imported;
        let target_local = *target as usize - bmeta.imported as usize;
        if let (Some(lty), Some(bty)) = (
            lmeta.func_types.get(local),
            bmeta.func_types.get(target_local),
        ) {
            *votes.entry(*lty).or_default().entry(*bty).or_insert(0) += 1;
        }
    }
    let mut types = vec![u32::MAX; lmeta.shapes.len()];
    for (lty, counts) in votes {
        let mut sorted: Vec<_> = counts.into_iter().collect();
        sorted.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        let conflicts = sorted.len() > 1 && sorted[0].1 == sorted[1].1;
        if !conflicts {
            types[lty as usize] = sorted[0].0;
        }
    }
    // A type no function names answers by its shape when one bound type has
    // it.
    let mut by_shape: HashMap<&Shape, Vec<usize>> = HashMap::new();
    for (index, shape) in bmeta.shapes.iter().enumerate() {
        by_shape.entry(shape).or_default().push(index);
    }
    for (index, shape) in lmeta.shapes.iter().enumerate() {
        if types[index] != u32::MAX {
            continue;
        }
        if let Some(candidates) = by_shape.get(shape) {
            if candidates.len() == 1 {
                types[index] = candidates[0] as u32;
            }
        }
    }
    types
}

// -------------------------------------------------------------------- hashes

/// FNV-1a, so a hash a cache file holds means the same across runs (and the
/// splitter's files mean the same here).
struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv {
    fn byte(&mut self, byte: u8) {
        self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
    }
    fn raw(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.byte(*byte);
        }
    }
    fn u32(&mut self, value: u32) {
        self.raw(&value.to_le_bytes());
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = Fnv::default();
    hasher.raw(bytes);
    hasher.finish()
}

// --------------------------------------------------------------- the state

/// One of the linked module's local functions.
#[derive(Clone)]
struct Func {
    name: Option<String>,
    shape: Shape,
    hash: u64,
    size: u32,
}

/// One of the bound module's local functions: how large the bind emitted it,
/// and which linked function it is, if any (wasm-bindgen's own shims have
/// none).
#[derive(Clone)]
struct BoundFunc {
    size: u32,
    linked: u32,
    /// Whether this function's body is the linked one translated: a
    /// function the map paired. One paired by name alone may have had its
    /// body rewritten by the bind, so a changed body it has is not one this
    /// tool may translate.
    translatable: bool,
}

#[derive(Clone)]
struct State {
    linked_hash: u64,
    /// Every section but the code, hashed as one: what says a body-only
    /// change is one.
    structure: u64,
    /// The sections a change to the function set does not touch
    /// ([`structure_hash`]), each by its own hash.
    fixed: Vec<(u8, u64)>,
    imports: u32,
    /// How many imports the bound module has: its function indices start
    /// after them.
    bound_imports: u32,
    bodies: Vec<Func>,
    map: Vec<u32>,
    types: Vec<u32>,
    type_shapes: Vec<Shape>,
    linked_links: Links,
    bound: Vec<BoundFunc>,
    /// Whether the bound module's data section is the linked module's, byte
    /// for byte: the bind copies the static data, so a patch takes the data
    /// of the module it is given (a function added or removed moves the
    /// table indices the data holds).
    data_copied: bool,
    /// Whether `record` reconstructed the bound module's index-bearing
    /// sections from its own bytes, so a patch may write them again.
    rebuild_ok: bool,
    /// The bound functions our reachability does not reach but the bind
    /// kept (its garbage collector has roots this tool does not read), by
    /// name: a patch never decides about them.
    stuck: Vec<String>,
    /// Whether sorting the functions by size and source order gives the
    /// order the bind emitted them in, checked on the module the map was
    /// recorded from. A patch only reorders functions when it did.
    order_ok: bool,
}

impl State {
    fn write(&self, dir: &Path) -> Result<()> {
        let mut out = Vec::with_capacity(256 + self.bodies.len() * 32 + self.map.len() * 4);
        out.extend_from_slice(MAGIC);
        for value in [
            self.linked_hash,
            self.structure,
            self.imports as u64,
            self.bound_imports as u64,
        ] {
            put(&mut out, value);
        }
        put(&mut out, self.fixed.len() as u64);
        for (id, hash) in &self.fixed {
            out.push(*id);
            put(&mut out, *hash);
        }
        out.push(self.data_copied as u8);
        out.push(self.rebuild_ok as u8);
        out.push(self.order_ok as u8);
        put(&mut out, self.stuck.len() as u64);
        for name in &self.stuck {
            put_str(&mut out, Some(name));
        }
        put(&mut out, self.bodies.len() as u64);
        for body in &self.bodies {
            put(&mut out, body.hash);
            put(&mut out, body.size as u64);
            put_str(&mut out, body.name.as_deref());
            put_shape(&mut out, &body.shape);
        }
        put(&mut out, self.map.len() as u64);
        for target in &self.map {
            put(&mut out, *target as u64);
        }
        put(&mut out, self.types.len() as u64);
        for ty in &self.types {
            put(&mut out, *ty as u64);
        }
        put(&mut out, self.type_shapes.len() as u64);
        for shape in &self.type_shapes {
            put_shape(&mut out, shape);
        }
        put_links(&mut out, &self.linked_links);
        put(&mut out, self.bound.len() as u64);
        for body in &self.bound {
            put(&mut out, body.size as u64);
            put(&mut out, body.linked as u64);
            out.push(body.translatable as u8);
        }
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join("state.bin");
        let next = dir.join(format!(".state.{}.next", std::process::id()));
        std::fs::write(&next, &out).with_context(|| format!("writing {}", next.display()))?;
        std::fs::rename(&next, &path).with_context(|| format!("renaming {}", path.display()))
    }

    fn read(dir: &Path) -> Result<Option<Self>> {
        let path = dir.join("state.bin");
        let Ok(bytes) = std::fs::read(&path) else {
            return Ok(None);
        };
        let mut reader = Reader {
            bytes: &bytes,
            at: 0,
        };
        if reader.take(8) != Some(MAGIC.as_slice()) {
            return Ok(None);
        }
        let linked_hash = reader.u64()?;
        let structure = reader.u64()?;
        let imports = reader.u64()? as u32;
        let bound_imports = reader.u64()? as u32;
        let fixed_len = reader.u64()? as usize;
        let mut fixed = Vec::with_capacity(fixed_len);
        for _ in 0..fixed_len {
            let id = reader.byte()?;
            let hash = reader.u64()?;
            fixed.push((id, hash));
        }
        let data_copied = reader.byte()? != 0;
        let rebuild_ok = reader.byte()? != 0;
        let order_ok = reader.byte()? != 0;
        let stuck_len = reader.u64()? as usize;
        let mut stuck = Vec::with_capacity(stuck_len);
        for _ in 0..stuck_len {
            stuck.push(reader.string()?.context("a stuck function has no name")?);
        }
        let count = reader.u64()? as usize;
        let mut bodies = Vec::with_capacity(count);
        for _ in 0..count {
            let hash = reader.u64()?;
            let size = reader.u64()? as u32;
            let name = reader.string()?;
            let shape = reader.shape()?;
            bodies.push(Func {
                name,
                shape,
                hash,
                size,
            });
        }
        let map_len = reader.u64()? as usize;
        let mut map = Vec::with_capacity(map_len);
        for _ in 0..map_len {
            map.push(reader.u64()? as u32);
        }
        let types_len = reader.u64()? as usize;
        let mut types = Vec::with_capacity(types_len);
        for _ in 0..types_len {
            types.push(reader.u64()? as u32);
        }
        let shapes_len = reader.u64()? as usize;
        let mut type_shapes = Vec::with_capacity(shapes_len);
        for _ in 0..shapes_len {
            type_shapes.push(reader.shape()?);
        }
        let linked_links = reader.links()?;
        let bound_len = reader.u64()? as usize;
        let mut bound = Vec::with_capacity(bound_len);
        for _ in 0..bound_len {
            let size = reader.u64()? as u32;
            let linked = reader.u64()? as u32;
            let translatable = reader.byte()? != 0;
            bound.push(BoundFunc {
                size,
                linked,
                translatable,
            });
        }
        Ok(Some(Self {
            linked_hash,
            structure,
            fixed,
            imports,
            bound_imports,
            bodies,
            map,
            types,
            type_shapes,
            linked_links,
            bound,
            data_copied,
            rebuild_ok,
            stuck,
            order_ok,
        }))
    }
}

fn put(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_str(out: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            put(out, value.len() as u64 + 1);
            out.extend_from_slice(value.as_bytes());
        }
        None => put(out, 0),
    }
}

fn put_shape(out: &mut Vec<u8>, shape: &Shape) {
    put(out, shape.params.len() as u64);
    out.extend_from_slice(&shape.params);
    put(out, shape.results.len() as u64);
    out.extend_from_slice(&shape.results);
}

fn put_links(out: &mut Vec<u8>, links: &Links) {
    put(out, links.exports.len() as u64);
    for (name, kind, index) in &links.exports {
        put_str(out, Some(name));
        out.push(*kind);
        put(out, *index as u64);
    }
    match links.start {
        Some(start) => {
            out.push(1);
            put(out, start as u64);
        }
        None => out.push(0),
    }
    put(out, links.elements.len() as u64);
    for element in &links.elements {
        put(out, element.table as u64);
        put(out, element.offset.len() as u64);
        out.extend_from_slice(&element.offset);
        put(out, element.funcs.len() as u64);
        for index in &element.funcs {
            put(out, *index as u64);
        }
    }
}

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
    fn u64(&mut self) -> Result<u64> {
        let bytes = self.take(8).context("the state file ends early")?;
        Ok(u64::from_le_bytes(bytes.try_into().expect("8 bytes")))
    }
    fn byte(&mut self) -> Result<u8> {
        let bytes = self.take(1).context("the state file ends early")?;
        Ok(bytes[0])
    }
    fn string(&mut self) -> Result<Option<String>> {
        let len = self.u64()? as usize;
        if len == 0 {
            return Ok(None);
        }
        let bytes = self.take(len - 1).context("the state file ends early")?;
        Ok(Some(String::from_utf8(bytes.to_vec()).context("a name")?))
    }
    fn shape(&mut self) -> Result<Shape> {
        let len = self.u64()? as usize;
        let params = self
            .take(len)
            .context("the state file ends early")?
            .to_vec();
        let len = self.u64()? as usize;
        let results = self
            .take(len)
            .context("the state file ends early")?
            .to_vec();
        Ok(Shape { params, results })
    }
    fn links(&mut self) -> Result<Links> {
        let count = self.u64()? as usize;
        let mut exports = Vec::with_capacity(count);
        for _ in 0..count {
            let name = self
                .string()?
                .context("the state file has an unnamed export")?;
            let kind = self.byte()?;
            let index = self.u64()? as u32;
            exports.push((name, kind, index));
        }
        let start = match self.byte()? {
            0 => None,
            _ => Some(self.u64()? as u32),
        };
        let count = self.u64()? as usize;
        let mut elements = Vec::with_capacity(count);
        for _ in 0..count {
            let table = self.u64()? as u32;
            let len = self.u64()? as usize;
            let offset = self
                .take(len)
                .context("the state file ends early")?
                .to_vec();
            let count = self.u64()? as usize;
            let mut funcs = Vec::with_capacity(count);
            for _ in 0..count {
                funcs.push(self.u64()? as u32);
            }
            elements.push(Element {
                table,
                offset,
                funcs,
            });
        }
        Ok(Links {
            exports,
            start,
            elements,
        })
    }
}

/// wasm-bindgen's other outputs (the page's JavaScript, typings, snippets):
/// a bind of the same structure writes them the same, so the kept ones are
/// what a full bind would write. Copied beside the patched module.
fn copy_glue(cache: &Path, out: &Path) -> Result<()> {
    let glue = cache.join("glue");
    let Some(dir) = out.parent() else {
        return Ok(());
    };
    if glue.is_dir() {
        copy_tree(&glue, dir)?;
    }
    Ok(())
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
    for entry in std::fs::read_dir(from).with_context(|| format!("reading {}", from.display()))? {
        let entry = entry?;
        // A bind's own directory may also hold the input this build staged
        // there (build.sh's `.bind-input.wasm`): not part of what a bind
        // writes.
        if entry.file_name().to_string_lossy().starts_with('.') || entry.path() == to {
            continue;
        }
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)
                .with_context(|| format!("copying into {}", target.display()))?;
        }
    }
    Ok(())
}

// ----------------------------------------------------------------- record

fn record(options: &Options) -> Result<()> {
    let linked = std::fs::read(options.linked()?).context("reading the linked module")?;
    let bound_path = options.bound.as_deref().context("--bound is required")?;
    let bound = std::fs::read(bound_path).context("reading the bound module")?;
    let cache = options.cache()?;
    let lsecs = sections(&linked)?;
    let bsecs = sections(&bound)?;
    let (_, lspans) = code_section(&linked, &lsecs)?;
    let (_, bspans) = code_section(&bound, &bsecs)?;
    let lmeta = read_meta(&linked)?;
    let bmeta = read_meta(&bound)?;
    if lmeta.func_types.len() != lspans.len() || bmeta.func_types.len() != bspans.len() {
        return Err(needs_bind("the function and code sections disagree"));
    }
    let identity_funcs: Vec<u32> = (0..lmeta.imported + lspans.len() as u32).collect();
    let identity_types: Vec<u32> = (0..lmeta.shapes.len() as u32).collect();
    let lbodies = bodies_meta(&linked, &lspans, &identity_funcs, &identity_types)?;
    let bbodies = bodies_meta(&bound, &bspans, &identity_funcs, &identity_types)?;
    // The bind emits local functions sorted by size, largest first, then
    // source order; a module whose output does not is one this map does not
    // describe.
    if bbodies.windows(2).any(|pair| pair[0].size < pair[1].size) {
        return Err(needs_bind(
            "the bound functions are not in the order this map assumes",
        ));
    }
    let mut map = function_map(&lmeta, &lspans, &lbodies, &bmeta, &bbodies);
    let types = type_map(&lmeta, &bmeta, &map);
    // A sample of bodies must translate to exactly the bound bytes: it is
    // the translation the patch will run, checked against what the bind
    // wrote.
    let mut checked = 0;
    for local in (0..lspans.len()).step_by(97) {
        let function = local as u32 + lmeta.imported;
        let target = map[function as usize];
        if target == u32::MAX {
            continue;
        }
        let target_local = target as usize - bmeta.imported as usize;
        // A body naming something the map could not resolve says nothing
        // about the translation: it is one a patch would fall back on.
        let Ok(translated) = translate(&linked, &lspans[local], &lmeta, &types, &map, local) else {
            continue;
        };
        let bound_body = &bound[bspans[target_local].clone()];
        let start = bound_body_instr_start(bound_body)?;
        if translated[bound_body_instr_start(&translated)?..] != bound_body[start..] {
            return Err(needs_bind(
                "a sampled body does not translate to the bound one",
            ));
        }
        checked += 1;
        if checked >= 64 {
            break;
        }
    }
    if checked == 0 {
        return Err(needs_bind("no body could be translated for a check"));
    }
    let linked_links = read_links(&linked)?;
    // The functions wasm-bindgen made (shims, wrappers) have no linked
    // counterpart; the linked ones are the inverse of `map`. A function the
    // map could not pair -- the bind rewrote its body, so its size group no
    // longer lines up -- is still one of the linked module's when its name is
    // unique there, and its source order is what the bind sorts ties by.
    let mut bound_linked = vec![u32::MAX; bspans.len()];
    let mut translatable = vec![false; bspans.len()];
    for (function, target) in map.iter().enumerate() {
        if *target == u32::MAX || function < lmeta.imported as usize {
            continue;
        }
        let bound = *target as usize - bmeta.imported as usize;
        bound_linked[bound] = function as u32 - lmeta.imported;
        translatable[bound] = true;
    }
    let mut unpaired: HashMap<&str, Vec<u32>> = HashMap::new();
    for (function, target) in map.iter().enumerate() {
        if *target != u32::MAX || function < lmeta.imported as usize {
            continue;
        }
        let local = function as u32 - lmeta.imported;
        if let Some(name) = lmeta.name(function as u32) {
            unpaired.entry(name).or_default().push(local);
        }
    }
    let mut order_ok = true;
    let mut claimed: HashSet<u32> = HashSet::new();
    for (local, _) in bspans.iter().enumerate() {
        if bound_linked[local] != u32::MAX {
            continue;
        }
        let Some(name) = bmeta.name((local + bmeta.imported as usize) as u32) else {
            continue;
        };
        match unpaired.get(name) {
            Some(candidates) if candidates.len() == 1 => {
                if !claimed.insert(candidates[0]) {
                    // Two functions of one name: there is no telling which is
                    // which by order.
                    order_ok = false;
                }
                bound_linked[local] = candidates[0];
            }
            Some(_) => order_ok = false,
            None => {}
        }
    }
    // Their bodies are the bind's, not this tool's, but what calls them
    // still names them: the map follows them too.
    for (local, linked) in bound_linked.iter().enumerate() {
        if *linked == u32::MAX {
            continue;
        }
        let function = (*linked + lmeta.imported) as usize;
        if map.get(function) == Some(&u32::MAX) {
            if let Some(target) = map.get_mut(function) {
                *target = bmeta.imported + local as u32;
            }
        }
    }
    let bound_funcs = bbodies
        .iter()
        .enumerate()
        .map(|(local, body)| BoundFunc {
            size: body.size,
            linked: bound_linked[local],
            translatable: translatable[local],
        })
        .collect::<Vec<_>>();
    // The order the bind sorted them in is ours: sizes descending, then a
    // linked function before one wasm-bindgen made, then source order.
    if order_ok {
        let key = |local: usize, body: &BoundFunc| {
            if body.linked == u32::MAX {
                (1u8, local)
            } else {
                (0u8, body.linked as usize)
            }
        };
        for (index, pair) in bound_funcs.windows(2).enumerate() {
            let (before, after) = (key(index, &pair[0]), key(index + 1, &pair[1]));
            let sizes = (pair[0].size, pair[1].size);
            if sizes.0 < sizes.1 || (sizes.0 == sizes.1 && before >= after) {
                order_ok = false;
                break;
            }
        }
    }
    let bound_links = read_links(&bound)?;
    let refs = bound_refs(&bound, &bspans)?;
    let roots = roots_of(&bound_links);
    let reachable = local_reachability(
        refs.bodies.iter().map(|body| &body.funcs),
        &roots,
        bmeta.imported as usize,
        bspans.len(),
    );
    // What the bind kept but this tool does not reach: it is rooted
    // somewhere this tool does not read, and is never decided about.
    let stuck = (0..bspans.len())
        .filter(|local| !reachable.contains(local))
        .map(|local| {
            bmeta
                .name((bmeta.imported as usize + local) as u32)
                .map(str::to_owned)
                .context("a function this tool does not reach has no name")
        })
        .collect::<Result<Vec<_>>>()
        .unwrap_or_default();
    let rebuild_ok = rebuild_check(&bound, &bsecs, &bmeta, &bound_links)?;
    // The bind copies the linked module's data; a patch then takes the data
    // of the module it is given.
    let data_of = |bytes: &[u8], sections: &[Section]| -> Option<Vec<u8>> {
        sections
            .iter()
            .find(|section| section.id == 11)
            .map(|section| bytes[section.contents.clone()].to_vec())
    };
    let data_copied = data_of(&linked, &lsecs) == data_of(&bound, &bsecs);
    let state = State {
        linked_hash: hash_bytes(&linked),
        structure: structure_hash_of_all(&linked, &lsecs)?,
        fixed: structure_hash(&linked, &lsecs)?,
        imports: lmeta.imported,
        bound_imports: bmeta.imported,
        bodies: lmeta
            .local_names
            .iter()
            .zip(&lmeta.local_shapes)
            .zip(&lbodies)
            .map(|((name, shape), body)| Func {
                name: name.clone(),
                shape: shape.clone(),
                hash: body.hash,
                size: body.size,
            })
            .collect(),
        map,
        types,
        type_shapes: lmeta.shapes.clone(),
        linked_links,
        bound: bound_funcs,
        data_copied,
        rebuild_ok,
        stuck,
        order_ok,
    };
    state.write(cache)?;
    let next = cache.join(format!(".bound.{}.next", std::process::id()));
    std::fs::write(&next, &bound).context("keeping the bound module")?;
    std::fs::rename(&next, cache.join("bound.wasm")).context("keeping the bound module")?;
    let _ = std::fs::remove_file(cache.join("renumber.bin"));
    let bound_dir = bound_path
        .parent()
        .context("the bound module has no directory")?;
    let glue = cache.join("glue");
    let _ = std::fs::remove_dir_all(&glue);
    copy_tree(bound_dir, &glue)?;
    let _ = std::fs::remove_file(glue.join(bound_path.file_name().context("no file name")?));
    eprintln!(
        "wasm-bind-patch: kept a map for {} bodies",
        state.bodies.len()
    );
    Ok(())
}

/// Whether every part of the bound module this tool writes again can be
/// written from its own bytes: `true` when a rebuild with the identity maps
/// is exactly the kept module.
fn rebuild_check(bound: &[u8], bsecs: &[Section], bmeta: &Meta, links: &Links) -> Result<bool> {
    let written = (|| -> Result<Vec<u8>> {
        let (_, bspans) = code_section(bound, bsecs)?;
        let bodies = bspans
            .iter()
            .map(|span| bound[span.clone()].to_vec())
            .collect::<Vec<_>>();
        let identity: Vec<u32> = (0..bound.len() as u32).collect();
        let names = (0..bodies.len())
            .map(|local| {
                bmeta
                    .name((bmeta.imported as usize + local) as u32)
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>();
        rebuild(
            &Kept {
                bytes: bound,
                sections: bsecs,
                meta: bmeta,
                links,
            },
            &Rewrite {
                bodies: &bodies,
                func_types: &bmeta.func_types,
                shapes: &bmeta.shapes,
                import_types: &bmeta.import_types,
                names: &names,
                type_names: &bmeta.type_names,
                data: None,
                funcs: &identity,
                import_type: &|ty| ty,
            },
        )
    })();
    Ok(written.is_ok_and(|written| written == bound))
}

// ------------------------------------------------------------------ apply

fn apply(options: &Options) -> Result<()> {
    let linked = std::fs::read(options.linked()?).context("reading the linked module")?;
    let cache = options.cache()?;
    let out = options.out.as_deref().context("--out is required")?;
    let Some(state) = State::read(cache)? else {
        return Err(needs_bind("no map kept"));
    };
    let Ok(bound) = std::fs::read(cache.join("bound.wasm")) else {
        return Err(needs_bind("the kept bound module is gone"));
    };
    if bound.is_empty() {
        return Err(needs_bind("the kept bound module is empty"));
    }
    let lsecs = sections(&linked)?;
    let (_, lspans) = code_section(&linked, &lsecs)?;
    if hash_bytes(&linked) == state.linked_hash {
        // Nothing moved: the kept bind is the answer (a build that stopped
        // after binding, and is run again).
        std::fs::write(out, &bound).with_context(|| format!("writing {}", out.display()))?;
        copy_glue(cache, out)?;
        return Ok(());
    }
    let lmeta = read_meta(&linked)?;
    let update = Update {
        linked: &linked,
        lsecs: &lsecs,
        lspans: &lspans,
        lmeta: &lmeta,
        bound: &bound,
        state: &state,
        cache,
        out,
    };
    if structure_hash_of_all(&linked, &lsecs)? == state.structure
        && lmeta.imported == state.imports
        && lspans.len() == state.bodies.len()
    {
        let identity_funcs: Vec<u32> = (0..lmeta.imported + lspans.len() as u32).collect();
        let identity_types: Vec<u32> = (0..lmeta.shapes.len() as u32).collect();
        let lbodies = bodies_meta(&linked, &lspans, &identity_funcs, &identity_types)?;
        let mut changed = Vec::new();
        for (index, (before, after)) in state.bodies.iter().zip(&lbodies).enumerate() {
            if before.hash != after.hash {
                changed.push(index);
            }
        }
        // Same functions in the same order: a body that kept its
        // instruction count was emitted where it was, and is spliced in. One
        // that changed size moved its function in the bind's order, and the
        // whole module is written again instead.
        if !changed.is_empty()
            && changed.len() <= MAX_CHANGED
            && changed
                .iter()
                .all(|index| state.bodies[*index].size == lbodies[*index].size)
        {
            match patch_bodies(&update, &changed, &lbodies) {
                Ok(()) => return Ok(()),
                Err(error) if !unusable(&error) => return Err(error),
                Err(_) => {}
            }
        }
    }
    patch_functions(&update)
}

/// What a patch reads and writes: the module it is given, the module it
/// keeps, and where both live.
struct Update<'a> {
    linked: &'a [u8],
    lsecs: &'a [Section],
    lspans: &'a [Range<usize>],
    lmeta: &'a Meta,
    bound: &'a [u8],
    state: &'a State,
    cache: &'a Path,
    out: &'a Path,
}

/// The body-only path: same functions, same order, bodies spliced into the
/// kept code section.
fn patch_bodies(update: &Update, changed: &[usize], lbodies: &[BodyMeta]) -> Result<()> {
    let Update {
        linked,
        lspans,
        lmeta,
        bound,
        state,
        cache,
        out,
        ..
    } = *update;
    // Translate every changed body before writing anything.
    let mut replacements = Vec::new();
    for index in changed {
        let function = *index as u32 + lmeta.imported;
        let target = *state
            .map
            .get(function as usize)
            .context("the map does not reach this function")?;
        if target == u32::MAX {
            return Err(needs_bind("a changed function has no bound counterpart"));
        }
        if !state
            .bound
            .get(target as usize - state.bound_imports as usize)
            .is_some_and(|bound| bound.translatable)
        {
            return Err(needs_bind(
                "a changed body is one the bind rewrote and this tool cannot",
            ));
        }
        let body = translate(
            linked,
            &lspans[*index],
            lmeta,
            &state.types,
            &state.map,
            *index,
        )?;
        let target = (target as usize)
            .checked_sub(state.bound_imports as usize)
            .context("a bound function index is before the imports")?;
        replacements.push((*index, target, body));
    }
    replacements.sort_by_key(|(_, target, _)| *target);
    let bsecs = sections(bound)?;
    let (code, bspans) = code_section(bound, &bsecs)?;
    let mut patched = Vec::with_capacity(bound.len() + 1024);
    patched.extend_from_slice(&bound[..code.start]);
    let mut code_bytes = Vec::with_capacity(bound[code.contents.clone()].len() + 1024);
    write_leb(&mut code_bytes, bspans.len() as u32);
    let mut replacement = replacements.iter().peekable();
    for (index, span) in bspans.iter().enumerate() {
        if let Some((_, _, body)) = replacement.next_if(|(_, target, _)| *target == index) {
            write_leb(&mut code_bytes, body.len() as u32);
            code_bytes.extend_from_slice(body);
        } else {
            write_leb(&mut code_bytes, (span.end - span.start) as u32);
            code_bytes.extend_from_slice(&bound[span.clone()]);
        }
    }
    if replacement.next().is_some() {
        bail!("a replacement's body index is out of range");
    }
    patched.push(10);
    write_leb(&mut patched, code_bytes.len() as u32);
    patched.extend_from_slice(&code_bytes);
    patched.extend_from_slice(&bound[code.contents.end..]);
    std::fs::write(out, &patched).with_context(|| format!("writing {}", out.display()))?;
    copy_glue(cache, out)?;
    // The kept state follows the module it now describes: same map (the
    // functions and their order did not move), the changed bodies' new
    // metadata, the patched bound module.
    let mut state = state.clone();
    state.linked_hash = hash_bytes(linked);
    for (index, body) in lbodies.iter().enumerate() {
        state.bodies[index].hash = body.hash;
        state.bodies[index].size = body.size;
    }
    state.write(cache)?;
    let next = cache.join(format!(".bound.{}.next", std::process::id()));
    std::fs::write(&next, &patched).context("keeping the patched bound module")?;
    std::fs::rename(&next, cache.join("bound.wasm")).context("keeping the patched bound module")?;
    let _ = std::fs::remove_file(cache.join("renumber.bin"));
    eprintln!(
        "wasm-bind-patch: patched {} of {} bodies",
        replacements.len(),
        state.bodies.len()
    );
    Ok(())
}

/// The general path: the set of functions or a body's size moved, so the
/// module is written again with every function renumbered and the new and
/// changed ones translated.
fn patch_functions(update: &Update) -> Result<()> {
    let Update {
        linked,
        lsecs,
        lspans,
        lmeta,
        bound,
        state,
        cache,
        out,
    } = *update;
    if !state.data_copied {
        return Err(needs_bind(
            "the kept bind's data is not the linked module's",
        ));
    }
    if !state.order_ok {
        return Err(needs_bind(
            "the kept bind cannot tell the order wasm-bindgen emits functions in",
        ));
    }
    if !state.rebuild_ok {
        return Err(needs_bind(
            "the kept bind cannot write this module's sections again",
        ));
    }
    let fixed = structure_hash(linked, lsecs)?;
    for (id, hash) in &fixed {
        match state.fixed.iter().find(|(before, _)| before == id) {
            Some((_, before)) if before == hash => {}
            Some(_) => return Err(needs_bind(format!("section {id} changed"))),
            None => return Err(needs_bind(format!("a section {id} appeared"))),
        }
    }
    if fixed.len() != state.fixed.len() {
        return Err(needs_bind("a section went away"));
    }
    let old_keys: Vec<_> = state
        .bodies
        .iter()
        .map(|body| (body.name.clone(), body.shape.clone()))
        .collect();
    let new_keys: Vec<_> = lmeta
        .local_names
        .iter()
        .zip(&lmeta.local_shapes)
        .map(|(name, shape)| (name.clone(), shape.clone()))
        .collect();
    if old_keys.iter().any(|(name, _)| name.is_none())
        || new_keys.iter().any(|(name, _)| name.is_none())
    {
        return Err(needs_bind("a function has no name"));
    }
    let functions = align(&old_keys, &new_keys)?;
    let type_alignment = align(&state.type_shapes, &lmeta.shapes)?;
    if functions.added.len() + functions.removed.len() > MAX_DELTA
        || type_alignment.added.len() + type_alignment.removed.len() > MAX_DELTA
    {
        return Err(needs_bind("too many functions or types changed"));
    }
    let links = read_links(linked)?;
    if !same_links(
        &state.linked_links,
        &links,
        &functions,
        state.imports,
        lmeta.imported,
    ) {
        return Err(needs_bind("an export, start function or element changed"));
    }
    // Bodies in the old linked index space: a body that did not change reads
    // the same whatever its indices are called.
    let old_funcs: Vec<u32> = (0..lmeta.imported)
        .chain(functions.new_to_old.iter().map(|old| match old {
            Some(old) => *old as u32 + lmeta.imported,
            None => u32::MAX,
        }))
        .collect();
    let old_types: Vec<u32> = (0..lmeta.shapes.len())
        .map(|index| match type_alignment.new_to_old[index] {
            Some(old) => old as u32,
            None => u32::MAX,
        })
        .collect();
    let step = std::time::Instant::now();
    let lbodies = bodies_meta(linked, lspans, &old_funcs, &old_types)?;
    eprintln!(
        "wasm-bind-patch: read the bodies' indices in {:.1} s",
        step.elapsed().as_secs_f64()
    );
    let step = std::time::Instant::now();
    let mut changed = Vec::new();
    for (old, new) in functions.old_to_new.iter().enumerate() {
        let Some(new) = new else { continue };
        if state.bodies[old].hash != lbodies[*new].hash {
            changed.push(*new);
        }
    }
    if changed.len() > MAX_CHANGED {
        return Err(needs_bind("too many bodies changed"));
    }
    let bsecs = sections(bound)?;
    let (_, bspans) = code_section(bound, &bsecs)?;
    eprintln!(
        "wasm-bind-patch: read what changed in {:.1} s",
        step.elapsed().as_secs_f64()
    );
    let step = std::time::Instant::now();
    if state.bound.len() != bspans.len() {
        return Err(needs_bind("the kept map is not of the kept module"));
    }
    let bmeta = read_meta(bound)?;
    let bound_links = read_links(bound)?;
    let refs = bound_refs(bound, &bspans)?;
    eprintln!(
        "wasm-bind-patch: read the kept module in {:.1} s",
        step.elapsed().as_secs_f64()
    );
    let step = std::time::Instant::now();
    // The functions the bind emits, in its order: the kept locals that are
    // still there, and the new ones. A linked function comes before the ones
    // wasm-bindgen made when they are the same size, its id being created
    // first.
    let mut entries: Vec<(u32, u8, usize, Entry)> = Vec::new();
    for (old, body) in state.bound.iter().enumerate() {
        match body.linked {
            u32::MAX => entries.push((body.size, 1, old, Entry::Kept(old))),
            linked => match functions.old_to_new.get(linked as usize).copied().flatten() {
                None => continue,
                Some(new) => {
                    let size = if changed.contains(&new) {
                        lbodies[new].size
                    } else {
                        body.size
                    };
                    entries.push((size, 0, new, Entry::Kept(old)));
                }
            },
        }
    }
    for (new, body) in lbodies.iter().enumerate() {
        if functions.new_to_old[new].is_none() {
            entries.push((body.size, 0, new, Entry::New(new)));
        }
    }
    entries.sort_by_key(|(size, group, key, _)| (std::cmp::Reverse(*size), *group, *key));
    let mut old_bound_to_new = vec![u32::MAX; state.bound.len()];
    let mut order = Vec::with_capacity(entries.len());
    let mut new_local_position = vec![None; lspans.len()];
    for (position, (_, _, _, entry)) in entries.iter().enumerate() {
        match entry {
            Entry::Kept(old) => old_bound_to_new[*old] = position as u32,
            Entry::New(new) => new_local_position[*new] = Some(position),
        }
        order.push(*entry);
    }
    // The kept module's function indices, imports first.
    let mut bound_map = vec![u32::MAX; state.bound_imports as usize + old_bound_to_new.len()];
    for (index, target) in bound_map.iter_mut().enumerate() {
        *target = if index < state.bound_imports as usize {
            index as u32
        } else {
            match old_bound_to_new[index - state.bound_imports as usize] {
                u32::MAX => u32::MAX,
                position => state.bound_imports + position,
            }
        };
    }
    // The map from the linked module to the bound one, in the new spaces.
    let mut map = vec![u32::MAX; lmeta.imported as usize + lspans.len()];
    for (index, target) in map.iter_mut().enumerate().take(lmeta.imported as usize) {
        *target = index as u32;
    }
    for new in 0..lspans.len() {
        match functions.new_to_old[new] {
            Some(old) => {
                let target = state.map[old + state.imports as usize];
                if target == u32::MAX {
                    continue;
                }
                let old_bound = target as usize - state.bound_imports as usize;
                map[new + lmeta.imported as usize] =
                    state.bound_imports + old_bound_to_new[old_bound];
            }
            None => {
                let position =
                    new_local_position[new].context("a new function is not in the bound order")?;
                map[new + lmeta.imported as usize] = state.bound_imports + position as u32;
            }
        }
    }
    // The bound types: the kept shapes, in the order the bind sorts them,
    // and the shapes the new and changed bodies name that it has not got.
    // The types the kept functions and their bodies still name: a type
    // nothing names any more is one the bind drops, and the ones after it
    // move. An unchanged kept body keeps its types; a changed one is
    // translated from the linked body, whose shapes are gathered below.
    let mut used_old_types: HashSet<u32> = bmeta.import_types.iter().copied().collect();
    for entry in &order {
        let Entry::Kept(old) = entry else { continue };
        used_old_types.insert(bmeta.func_types[*old]);
        let linked_local = state.bound[*old].linked;
        let changed_new = linked_local != u32::MAX
            && functions
                .old_to_new
                .get(linked_local as usize)
                .copied()
                .flatten()
                .is_some_and(|new| changed.contains(&new));
        if !changed_new {
            used_old_types.extend(refs.bodies[*old].types.iter().copied());
        }
    }
    // The shapes the new and changed bodies need.
    let mut needed: HashSet<Shape> = HashSet::new();
    for new in 0..lspans.len() {
        if functions.new_to_old[new].is_some() && !changed.contains(&new) {
            continue;
        }
        needed.insert(lmeta.local_shapes[new].clone());
        let body = wp::FunctionBody::new(wp::BinaryReader::new(
            &linked[lspans[new].clone()],
            lspans[new].start,
        ));
        let mut operators = body.get_operators_reader()?;
        while !operators.eof() {
            match operators.read()? {
                Operator::CallIndirect { type_index, .. }
                | Operator::ReturnCallIndirect { type_index, .. } => {
                    if let Some(shape) = lmeta.shapes.get(type_index as usize) {
                        needed.insert(shape.clone());
                    }
                }
                Operator::Block { blockty }
                | Operator::Loop { blockty }
                | Operator::If { blockty }
                | Operator::Try { blockty } => {
                    if let wp::BlockType::FuncType(ty) = blockty {
                        if let Some(shape) = lmeta.shapes.get(ty as usize) {
                            needed.insert(shape.clone());
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let mut type_shapes: Vec<Shape> = used_old_types
        .iter()
        .map(|old| {
            bmeta
                .shapes
                .get(*old as usize)
                .cloned()
                .context("a function names a type the bound module does not have")
        })
        .collect::<Result<_>>()?;
    type_shapes.sort_by(|a, b| {
        (a.params.as_slice(), a.results.as_slice())
            .cmp(&(b.params.as_slice(), b.results.as_slice()))
    });
    for shape in &needed {
        if type_shapes.contains(shape) {
            continue;
        }
        let at = type_shapes
            .iter()
            .position(|candidate| {
                (candidate.params.as_slice(), candidate.results.as_slice())
                    > (shape.params.as_slice(), shape.results.as_slice())
            })
            .unwrap_or(type_shapes.len());
        type_shapes.insert(at, shape.clone());
    }
    for (index, shape) in type_shapes.iter().enumerate() {
        if type_shapes[..index].contains(shape) {
            return Err(needs_bind("two bound types have one shape"));
        }
    }
    let old_type_index = |old: u32| -> Result<u32> {
        let shape = bmeta
            .shapes
            .get(old as usize)
            .context("a function names a type the bound module does not have")?;
        type_shapes
            .iter()
            .position(|candidate| candidate == shape)
            .map(|index| index as u32)
            .context("a kept function's type is not in the type list")
    };
    let mut types = vec![u32::MAX; lmeta.shapes.len()];
    for (new, old) in type_alignment.new_to_old.iter().enumerate() {
        let Some(old) = old else { continue };
        if let Some(target) = state.types.get(*old).copied().filter(|t| *t != u32::MAX) {
            types[new] = old_type_index(target)?;
        }
    }
    for (new, shape) in lmeta.shapes.iter().enumerate() {
        if types[new] != u32::MAX {
            continue;
        }
        match type_shapes.iter().position(|candidate| candidate == shape) {
            Some(at) => types[new] = at as u32,
            // A type no kept or translated body names needs no place.
            None if !needed.contains(shape) => {}
            None => {
                return Err(needs_bind(
                    "a body names a type the bound module has not got",
                ));
            }
        }
    }
    eprintln!(
        "wasm-bind-patch: made the maps in {:.1} s",
        step.elapsed().as_secs_f64()
    );
    let step = std::time::Instant::now();
    // The bodies, in the bind's order, and what each names.
    let mut bodies: Vec<Vec<u8>> = Vec::with_capacity(order.len());
    let mut funcs_of: Vec<Vec<u32>> = Vec::with_capacity(order.len());
    let mut func_types: Vec<u32> = Vec::with_capacity(order.len());
    let mut names: Vec<Option<String>> = Vec::with_capacity(order.len());
    for entry in &order {
        let (body, funcs, ty, name) = match entry {
            Entry::Kept(old) => {
                let linked_local = state.bound[*old].linked;
                let changed_new = linked_local != u32::MAX
                    && functions
                        .old_to_new
                        .get(linked_local as usize)
                        .copied()
                        .flatten()
                        .is_some_and(|new| changed.contains(&new));
                let name = bmeta
                    .name((bmeta.imported as usize + *old) as u32)
                    .map(str::to_owned);
                let ty = old_type_index(bmeta.func_types[*old])?;
                if changed_new {
                    if !state.bound[*old].translatable {
                        return Err(needs_bind(
                            "a changed body is one the bind rewrote and this tool cannot",
                        ));
                    }
                    let new = functions.old_to_new[linked_local as usize].expect("checked");
                    let body = translate(linked, &lspans[new], lmeta, &types, &map, new)?;
                    let refs = body_refs(&body)?;
                    (body, refs.funcs, ty, name)
                } else {
                    // A body none of whose indices moved is the module a
                    // full bind writes: it is kept as it is.
                    let body = if unchanged_indices(&refs.bodies[*old], &bound_map, &old_type_index)
                    {
                        bound[bspans[*old].clone()].to_vec()
                    } else {
                        let type_of = |ty: u32| old_type_index(ty).unwrap_or(u32::MAX);
                        renumber_body(&bound[bspans[*old].clone()], &bound_map, &type_of)?
                    };
                    let funcs = refs.bodies[*old]
                        .funcs
                        .iter()
                        .map(|function| {
                            if (*function as usize) < state.bound_imports as usize {
                                Ok(*function)
                            } else {
                                let local = *function as usize - state.bound_imports as usize;
                                match old_bound_to_new.get(local).copied() {
                                    Some(target) if target != u32::MAX => {
                                        Ok(state.bound_imports + target)
                                    }
                                    _ => Err(needs_bind(
                                        "a kept body calls a function nothing reaches",
                                    )),
                                }
                            }
                        })
                        .collect::<Result<Vec<_>>>()?;
                    (body, funcs, ty, name)
                }
            }
            Entry::New(new) => {
                let body = translate(linked, &lspans[*new], lmeta, &types, &map, *new)?;
                let refs = body_refs(&body)?;
                let name = lmeta.local_names[*new].clone();
                let shape = &lmeta.local_shapes[*new];
                let ty = type_shapes
                    .iter()
                    .position(|candidate| candidate == shape)
                    .context("a new function's type is not in the type list")?
                    as u32;
                (body, refs.funcs, ty, name)
            }
        };
        bodies.push(body);
        funcs_of.push(funcs);
        func_types.push(ty);
        names.push(name);
    }
    // The functions the bind keeps are the ones reachable from the module's
    // roots. The kept module's were; if the new one's differ, a full bind's
    // garbage collection would write another module.
    let roots = roots_of(&bound_links)
        .into_iter()
        .map(|function| {
            if (function as usize) < state.bound_imports as usize {
                Ok(function)
            } else {
                let local = function as usize - state.bound_imports as usize;
                match old_bound_to_new.get(local).copied() {
                    Some(target) if target != u32::MAX => Ok(state.bound_imports + target),
                    _ => Err(needs_bind("an export names a function nothing keeps")),
                }
            }
        })
        .collect::<Result<HashSet<_>>>()?;
    let reachable = local_reachability(
        funcs_of.iter(),
        &roots,
        state.bound_imports as usize,
        bodies.len(),
    );
    // The functions this tool reaches are ones the bind keeps; the ones it
    // kept without reaching (its roots are not all this tool's) stay kept as
    // long as they are here. Anything else would be a function the bind
    // drops or keeps differently.
    let mut neither: HashSet<usize> = (0..bodies.len()).collect();
    neither.retain(|local| !reachable.contains(local));
    for name in &state.stuck {
        let mut found = None;
        for (position, entry) in order.iter().enumerate() {
            let entry_name = match entry {
                Entry::Kept(old) => bmeta.name((bmeta.imported as usize + *old) as u32),
                Entry::New(new) => lmeta.local_names[*new].as_deref(),
            };
            if entry_name == Some(name.as_str()) {
                if found.is_some() {
                    return Err(needs_bind("two functions keep one name"));
                }
                found = Some(position);
            }
        }
        let Some(position) = found else {
            return Err(needs_bind(
                "a function this tool does not reach is gone from the module",
            ));
        };
        neither.remove(&position);
    }
    if !neither.is_empty() {
        return Err(needs_bind(
            "a function the bind would drop would be kept, or one it keeps dropped",
        ));
    }
    // The type names, in the new type index space.
    let mut type_names: HashMap<u32, String> = HashMap::new();
    for (old, name) in &bmeta.type_names {
        let Ok(ty) = old_type_index(*old) else {
            continue;
        };
        match type_names.get(&ty) {
            Some(before) if before != name => {
                return Err(needs_bind("two types with one place have different names"));
            }
            _ => {
                type_names.insert(ty, name.clone());
            }
        }
    }
    for (new, name) in &lmeta.type_names {
        if let Some(ty) = types.get(*new as usize).copied().filter(|t| *t != u32::MAX) {
            match type_names.get(&ty) {
                Some(before) if before != name => {
                    return Err(needs_bind("two types with one place have different names"));
                }
                _ => {
                    type_names.insert(ty, name.clone());
                }
            }
        }
    }
    eprintln!(
        "wasm-bind-patch: checked the functions in {:.1} s",
        step.elapsed().as_secs_f64()
    );
    let step = std::time::Instant::now();
    let rebuilt = rebuild(
        &Kept {
            bytes: bound,
            sections: &bsecs,
            meta: &bmeta,
            links: &bound_links,
        },
        &Rewrite {
            bodies: &bodies,
            func_types: &func_types,
            shapes: &type_shapes,
            import_types: &bmeta.import_types,
            names: &names,
            type_names: &type_names,
            data: lsecs
                .iter()
                .find(|section| section.id == 11)
                .map(|section| &linked[section.contents.clone()]),
            funcs: &bound_map,
            import_type: &|ty| old_type_index(ty).unwrap_or(u32::MAX),
        },
    )?;
    eprintln!(
        "wasm-bind-patch: wrote the module in {:.1} s",
        step.elapsed().as_secs_f64()
    );
    let step = std::time::Instant::now();
    std::fs::write(out, &rebuilt).with_context(|| format!("writing {}", out.display()))?;
    copy_glue(cache, out)?;
    // The kept state follows the module it now describes.
    let identity_funcs: Vec<u32> = (0..lmeta.imported + lspans.len() as u32).collect();
    let identity_types: Vec<u32> = (0..lmeta.shapes.len() as u32).collect();
    let identity_bodies = bodies_meta(linked, lspans, &identity_funcs, &identity_types)?;
    let new_bound = order
        .iter()
        .map(|entry| match entry {
            Entry::Kept(old) => {
                let linked = state.bound[*old].linked;
                let (size, linked) = match linked {
                    u32::MAX => (state.bound[*old].size, u32::MAX),
                    linked => {
                        let new = functions.old_to_new[linked as usize]
                            .expect("a kept bound function comes from a kept function");
                        let size = if changed.contains(&new) {
                            lbodies[new].size
                        } else {
                            state.bound[*old].size
                        };
                        (size, new as u32)
                    }
                };
                BoundFunc {
                    size,
                    linked,
                    translatable: state.bound[*old].translatable,
                }
            }
            Entry::New(new) => BoundFunc {
                size: lbodies[*new].size,
                linked: *new as u32,
                translatable: true,
            },
        })
        .collect::<Vec<_>>();
    let state = State {
        linked_hash: hash_bytes(linked),
        structure: structure_hash_of_all(linked, lsecs)?,
        fixed: structure_hash(linked, lsecs)?,
        imports: lmeta.imported,
        bound_imports: state.bound_imports,
        bodies: lmeta
            .local_names
            .iter()
            .zip(&lmeta.local_shapes)
            .zip(&identity_bodies)
            .map(|((name, shape), body)| Func {
                name: name.clone(),
                shape: shape.clone(),
                hash: body.hash,
                size: body.size,
            })
            .collect(),
        map,
        types,
        type_shapes: lmeta.shapes.clone(),
        linked_links: links,
        bound: new_bound,
        data_copied: state.data_copied,
        rebuild_ok: state.rebuild_ok,
        stuck: state.stuck.clone(),
        order_ok: state.order_ok,
    };
    eprintln!(
        "wasm-bind-patch: kept the state in {:.1} s",
        step.elapsed().as_secs_f64()
    );
    state.write(cache)?;
    let next = cache.join(format!(".bound.{}.next", std::process::id()));
    std::fs::write(&next, &rebuilt).context("keeping the patched bound module")?;
    std::fs::rename(&next, cache.join("bound.wasm")).context("keeping the patched bound module")?;
    write_renumbering(
        cache,
        &rebuilt,
        state.bound_imports,
        &old_bound_to_new,
        &state.map,
    )?;
    eprintln!(
        "wasm-bind-patch: wrote {} functions, {} of them new",
        order.len(),
        functions.added.len()
    );
    Ok(())
}

/// A slot in the bind's function order: one of the kept bound functions, or
/// one of the linked module's new ones.
#[derive(Clone, Copy)]
enum Entry {
    Kept(usize),
    New(usize),
}

/// The correspondence two sequences that differ by insertions and removals
/// alone have.
struct Alignment {
    /// For each old position, its position in the new sequence, if it is
    /// there.
    old_to_new: Vec<Option<usize>>,
    new_to_old: Vec<Option<usize>>,
    added: Vec<usize>,
    removed: Vec<usize>,
}

/// The correspondence two sequences that differ by insertions and removals
/// alone have: the shortest edit script (Myers), so equal entries pair in
/// order even when several share a name, and entries moved any other way
/// come out as more edits than [`MAX_DELTA`] allows.
fn align<T: PartialEq>(old: &[T], new: &[T]) -> Result<Alignment> {
    let n = old.len() as isize;
    let m = new.len() as isize;
    // A change of one function is one removal and one insertion at most;
    // allow a few, and no more.
    let max = (MAX_DELTA * 2) as isize;
    let offset = max + 1;
    let mut v = vec![0isize; (2 * max + 3) as usize];
    let mut trace = Vec::new();
    for d in 0..=max {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let index = (k + offset) as usize;
            let mut x = if k == -d || (k != d && v[index - 1] < v[index + 1]) {
                v[index + 1]
            } else {
                v[index - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && old[x as usize] == new[y as usize] {
                x += 1;
                y += 1;
            }
            v[index] = x;
            if x >= n && y >= m {
                let mut old_to_new = vec![None; old.len()];
                let mut new_to_old = vec![None; new.len()];
                let mut added = Vec::new();
                let mut removed = Vec::new();
                let (mut x, mut y) = (n, m);
                for d in (0..=d).rev() {
                    let v = &trace[d as usize];
                    let k = x - y;
                    let index = (k + offset) as usize;
                    let prev_k = if k == -d || (k != d && v[index - 1] < v[index + 1]) {
                        k + 1
                    } else {
                        k - 1
                    };
                    let prev_x = if d == 0 {
                        0
                    } else {
                        v[(prev_k + offset) as usize]
                    };
                    let prev_y = prev_x - prev_k;
                    while x > prev_x && y > prev_y {
                        x -= 1;
                        y -= 1;
                        old_to_new[x as usize] = Some(y as usize);
                        new_to_old[y as usize] = Some(x as usize);
                    }
                    if d > 0 {
                        if x == prev_x {
                            added.push((y - 1) as usize);
                        } else {
                            removed.push((x - 1) as usize);
                        }
                    }
                    x = prev_x;
                    y = prev_y;
                }
                added.reverse();
                removed.reverse();
                return Ok(Alignment {
                    old_to_new,
                    new_to_old,
                    added,
                    removed,
                });
            }
            k += 2;
        }
    }
    Err(needs_bind("too many functions or types changed"))
}

/// Whether two sets of exports, start and elements name the same functions,
/// through the correspondence `functions`.
fn same_links(
    before: &Links,
    after: &Links,
    functions: &Alignment,
    old_imported: u32,
    new_imported: u32,
) -> bool {
    let same = |old: u32, new: u32| -> bool {
        if old < old_imported || new < new_imported {
            return old == new;
        }
        let old_local = (old - old_imported) as usize;
        functions.old_to_new.get(old_local).copied().flatten()
            == Some((new - new_imported) as usize)
    };
    if before.exports.len() != after.exports.len() {
        return false;
    }
    for ((old_name, old_kind, old_index), (new_name, new_kind, new_index)) in
        before.exports.iter().zip(&after.exports)
    {
        if old_name != new_name || old_kind != new_kind {
            return false;
        }
        if *old_kind == 0 && !same(*old_index, *new_index) {
            return false;
        }
        if *old_kind != 0 && old_index != new_index {
            return false;
        }
    }
    if before.start.is_some() != after.start.is_some() {
        return false;
    }
    if let (Some(old), Some(new)) = (before.start, after.start) {
        if !same(old, new) {
            return false;
        }
    }
    if before.elements.len() != after.elements.len() {
        return false;
    }
    for (old, new) in before.elements.iter().zip(&after.elements) {
        if old.table != new.table || old.offset != new.offset || old.funcs.len() != new.funcs.len()
        {
            return false;
        }
        for (old, new) in old.funcs.iter().zip(&new.funcs) {
            if !same(*old, *new) {
                return false;
            }
        }
    }
    true
}

// --------------------------------------------------------------- the bounds

/// What each body names: the functions it calls or takes the address of, and
/// the types its instructions name (in the module's index spaces).
struct RefsOfBody {
    funcs: Vec<u32>,
    types: Vec<u32>,
}

struct Refs {
    bodies: Vec<RefsOfBody>,
}

fn body_refs(body: &[u8]) -> Result<RefsOfBody> {
    let function = wp::FunctionBody::new(wp::BinaryReader::new(body, 0));
    let mut operators = function.get_operators_reader()?;
    let mut funcs = Vec::new();
    let mut types = Vec::new();
    while !operators.eof() {
        match operators.read()? {
            Operator::Call { function_index }
            | Operator::ReturnCall { function_index }
            | Operator::RefFunc { function_index } => funcs.push(function_index),
            Operator::CallIndirect { type_index, .. }
            | Operator::ReturnCallIndirect { type_index, .. } => types.push(type_index),
            Operator::Block { blockty }
            | Operator::Loop { blockty }
            | Operator::If { blockty }
            | Operator::Try { blockty } => {
                if let wp::BlockType::FuncType(ty) = blockty {
                    types.push(ty);
                }
            }
            _ => {}
        }
    }
    Ok(RefsOfBody { funcs, types })
}

fn bound_refs(bytes: &[u8], spans: &[Range<usize>]) -> Result<Refs> {
    Ok(Refs {
        bodies: spans
            .iter()
            .map(|span| body_refs(&bytes[span.clone()]))
            .collect::<Result<_>>()?,
    })
}

/// The function indices exported, started or in an element segment: where
/// wasm-bindgen's garbage collection starts.
fn roots_of(links: &Links) -> HashSet<u32> {
    let mut roots = HashSet::new();
    for (_, kind, index) in &links.exports {
        if *kind == 0 {
            roots.insert(*index);
        }
    }
    if let Some(start) = links.start {
        roots.insert(start);
    }
    for element in &links.elements {
        roots.extend(element.funcs.iter().copied());
    }
    roots
}

/// The local functions reachable from `roots` through `bodies`, as local
/// indices (the module's own functions, imports left out).
fn local_reachability<'a>(
    bodies: impl Iterator<Item = &'a Vec<u32>>,
    roots: &HashSet<u32>,
    imported: usize,
    locals: usize,
) -> HashSet<usize> {
    let refs = bodies.collect::<Vec<_>>();
    let mut seen = HashSet::new();
    let mut queue: Vec<u32> = roots.iter().copied().collect();
    while let Some(function) = queue.pop() {
        let index = function as usize;
        if index < imported || index >= imported + locals || !seen.insert(index - imported) {
            continue;
        }
        if let Some(body) = refs.get(index - imported) {
            queue.extend(body.iter().copied());
        }
    }
    seen
}

// ---------------------------------------------------------- writing sections

/// Write the bound module again: its sections, with the functions in
/// `bodies`, their type indices in `func_types`, the type section in
/// The module a rebuild keeps.
struct Kept<'a> {
    bytes: &'a [u8],
    sections: &'a [Section],
    meta: &'a Meta,
    links: &'a Links,
}

/// What a rebuild writes: the functions and types, and the map from the kept
/// module's indices to the new ones.
struct Rewrite<'a> {
    bodies: &'a [Vec<u8>],
    func_types: &'a [u32],
    shapes: &'a [Shape],
    import_types: &'a [u32],
    names: &'a [Option<String>],
    type_names: &'a HashMap<u32, String>,
    data: Option<&'a [u8]>,
    funcs: &'a [u32],
    import_type: &'a dyn Fn(u32) -> u32,
}

/// `kept`'s sections written again as `rewrite` describes.
fn rebuild(kept: &Kept, rewrite: &Rewrite) -> Result<Vec<u8>> {
    let Kept {
        bytes: bound,
        sections: bsecs,
        meta: bmeta,
        links,
    } = *kept;
    let Rewrite {
        bodies,
        func_types,
        shapes,
        import_types,
        names,
        type_names,
        data,
        funcs,
        import_type,
    } = *rewrite;
    let mut out = Vec::with_capacity(bound.len());
    out.extend_from_slice(&bound[..8]);
    let mut import_seen = 0usize;
    for section in bsecs {
        match section.id {
            1 => {
                let mut bytes = Vec::new();
                write_leb(&mut bytes, shapes.len() as u32);
                for shape in shapes {
                    shape.encode(&mut bytes);
                }
                push_section(&mut out, 1, &bytes);
            }
            2 => {
                let mut bytes = Vec::new();
                let mut at = section.contents.start;
                let count = read_leb(bound, &mut at)?;
                write_leb(&mut bytes, count);
                for _ in 0..count {
                    let len = read_leb(bound, &mut at)? as usize;
                    let module = &bound[at..at + len];
                    at += len;
                    let len = read_leb(bound, &mut at)? as usize;
                    let field = &bound[at..at + len];
                    at += len;
                    for part in [module, field] {
                        write_leb(&mut bytes, part.len() as u32);
                        bytes.extend_from_slice(part);
                    }
                    let kind = bound[at];
                    at += 1;
                    bytes.push(kind);
                    match kind {
                        0 => {
                            let old = read_leb(bound, &mut at)?;
                            let ty = import_types
                                .get(import_seen)
                                .copied()
                                .context("an import's type is not in the list")?;
                            if old != ty {
                                return Err(needs_bind("an import's type is not the one read"));
                            }
                            import_seen += 1;
                            let ty = import_type(ty);
                            if ty == u32::MAX {
                                return Err(needs_bind("an import names a type nothing keeps"));
                            }
                            write_leb(&mut bytes, ty);
                        }
                        1 => {
                            // A table: an element type and its limits.
                            bytes.push(bound[at]);
                            at += 1;
                            let flags = read_leb(bound, &mut at)?;
                            write_leb(&mut bytes, flags);
                            for _ in 0..if flags & 1 != 0 { 2 } else { 1 } {
                                let value = read_leb(bound, &mut at)?;
                                write_leb(&mut bytes, value);
                            }
                        }
                        2 => {
                            // A memory: its limits.
                            let flags = read_leb(bound, &mut at)?;
                            write_leb(&mut bytes, flags);
                            for _ in 0..if flags & 1 != 0 { 2 } else { 1 } {
                                let value = read_leb(bound, &mut at)?;
                                write_leb(&mut bytes, value);
                            }
                        }
                        3 => {
                            // A global: a value type and its mutability.
                            bytes.extend_from_slice(&bound[at..at + 2]);
                            at += 2;
                        }
                        _ => return Err(needs_bind("an import this tool cannot copy")),
                    }
                }
                if at != section.contents.end || import_seen != import_types.len() {
                    return Err(needs_bind("an import section this tool cannot read"));
                }
                push_section(&mut out, 2, &bytes);
            }
            3 => {
                let mut bytes = Vec::new();
                write_leb(&mut bytes, func_types.len() as u32);
                for ty in func_types {
                    write_leb(&mut bytes, *ty);
                }
                push_section(&mut out, 3, &bytes);
            }
            7 => {
                let mut bytes = Vec::new();
                write_leb(&mut bytes, links.exports.len() as u32);
                for (name, kind, index) in &links.exports {
                    write_leb(&mut bytes, name.len() as u32);
                    bytes.extend_from_slice(name.as_bytes());
                    bytes.push(*kind);
                    if *kind == 0 {
                        write_leb(&mut bytes, map_index(funcs, *index)?);
                    } else {
                        write_leb(&mut bytes, *index);
                    }
                }
                push_section(&mut out, 7, &bytes);
            }
            8 => {
                let mut bytes = Vec::new();
                write_leb(
                    &mut bytes,
                    map_index(
                        funcs,
                        links.start.context("a start section without a start")?,
                    )?,
                );
                push_section(&mut out, 8, &bytes);
            }
            9 => {
                let mut bytes = Vec::new();
                write_leb(&mut bytes, links.elements.len() as u32);
                for element in &links.elements {
                    if element.table == 0 {
                        bytes.push(0);
                    } else {
                        bytes.push(2);
                        write_leb(&mut bytes, element.table);
                    }
                    bytes.extend_from_slice(&element.offset);
                    if element.table != 0 {
                        bytes.push(0);
                    }
                    write_leb(&mut bytes, element.funcs.len() as u32);
                    for index in &element.funcs {
                        write_leb(&mut bytes, map_index(funcs, *index)?);
                    }
                }
                push_section(&mut out, 9, &bytes);
            }
            10 => {
                let mut bytes = Vec::new();
                write_leb(&mut bytes, bodies.len() as u32);
                for body in bodies {
                    write_leb(&mut bytes, body.len() as u32);
                    bytes.extend_from_slice(body);
                }
                push_section(&mut out, 10, &bytes);
            }
            11 => match data {
                Some(data) => push_section(&mut out, 11, data),
                None => out.extend_from_slice(&bound[section.start..section.contents.end]),
            },
            0 if custom_name(bound, section.contents.start)?.as_deref() == Some("name") => {
                let name = rebuild_name(bound, section, bmeta, names, type_names)?;
                push_section(&mut out, 0, &name);
            }
            _ => out.extend_from_slice(&bound[section.start..section.contents.end]),
        }
    }
    Ok(out)
}

fn map_index(map: &[u32], index: u32) -> Result<u32> {
    match map.get(index as usize) {
        Some(target) if *target != u32::MAX => Ok(*target),
        _ => Err(needs_bind(format!(
            "function {index} has no bound counterpart"
        ))),
    }
}

fn push_section(out: &mut Vec<u8>, id: u8, contents: &[u8]) {
    out.push(id);
    write_leb(out, contents.len() as u32);
    out.extend_from_slice(contents);
}

/// The bound module's name section with its functions and types renumbered,
/// the new functions named, and every other part as it was.
fn rebuild_name(
    bound: &[u8],
    section: &Section,
    bmeta: &Meta,
    names: &[Option<String>],
    type_names: &HashMap<u32, String>,
) -> Result<Vec<u8>> {
    let mut at = section.contents.start;
    let len = read_leb(bound, &mut at)? as usize;
    if bound.get(at..at + len) != Some(b"name") {
        return Err(needs_bind("a custom section called name is not one"));
    }
    let mut cursor = at + len;
    let mut subsections = Vec::new();
    while cursor < section.contents.end {
        let start = cursor;
        let id = bound[cursor];
        cursor += 1;
        let size = read_leb(bound, &mut cursor)? as usize;
        cursor += size;
        subsections.push((id, start..cursor));
    }
    let mut out = Vec::new();
    for (id, range) in subsections {
        match id {
            1 => {
                let mut sub = Vec::new();
                let mut map: Vec<(u32, String)> = Vec::new();
                for index in 0..bmeta.imported as usize {
                    if let Some(name) = bmeta.name(index as u32) {
                        map.push((index as u32, name.to_owned()));
                    }
                }
                for (local, name) in names.iter().enumerate() {
                    let Some(name) = name else { continue };
                    map.push(((bmeta.imported as usize + local) as u32, name.clone()));
                }
                map.retain(|(index, _)| *index != u32::MAX);
                map.sort_by_key(|(index, _)| *index);
                write_leb(&mut sub, map.len() as u32);
                for (index, name) in &map {
                    write_leb(&mut sub, *index);
                    write_leb(&mut sub, name.len() as u32);
                    sub.extend_from_slice(name.as_bytes());
                }
                push_subsection(&mut out, 1, &sub);
            }
            2 => return Err(needs_bind("the bound module names its locals")),
            4 => {
                let mut sub = Vec::new();
                let mut map: Vec<(u32, String)> = type_names
                    .iter()
                    .map(|(index, name)| (*index, name.clone()))
                    .collect();
                map.sort_by_key(|(index, _)| *index);
                write_leb(&mut sub, map.len() as u32);
                for (index, name) in &map {
                    write_leb(&mut sub, *index);
                    write_leb(&mut sub, name.len() as u32);
                    sub.extend_from_slice(name.as_bytes());
                }
                push_subsection(&mut out, 4, &sub);
            }
            _ => out.extend_from_slice(&bound[range]),
        }
    }
    let mut named = Vec::new();
    write_leb(&mut named, 4);
    named.extend_from_slice(b"name");
    named.extend_from_slice(&out);
    Ok(named)
}

fn push_subsection(out: &mut Vec<u8>, id: u8, contents: &[u8]) {
    out.push(id);
    write_leb(out, contents.len() as u32);
    out.extend_from_slice(contents);
}

/// Whether every index in `body`'s body already is its new one: the bytes a
/// full bind writes, kept where they are.
fn unchanged_indices(body: &RefsOfBody, funcs: &[u32], types: &dyn Fn(u32) -> Result<u32>) -> bool {
    body.funcs
        .iter()
        .all(|function| funcs.get(*function as usize).copied() == Some(*function))
        && body
            .types
            .iter()
            .all(|ty| types(*ty).is_ok_and(|new| new == *ty))
}

/// A kept body with its function and type indices renumbered.
fn renumber_body(body: &[u8], funcs: &[u32], types: &dyn Fn(u32) -> u32) -> Result<Vec<u8>> {
    let function = wp::FunctionBody::new(wp::BinaryReader::new(body, 0));
    let mut operators = function.get_operators_reader()?;
    let header = operators.original_position();
    let mut out = Vec::with_capacity(body.len());
    out.extend_from_slice(&body[..header]);
    let function_index = |index: u32| -> Result<u32> {
        match funcs.get(index as usize) {
            Some(target) if *target != u32::MAX => Ok(*target),
            _ => Err(needs_bind(format!("function {index} is unresolved"))),
        }
    };
    let type_index = |ty: u32| -> Result<u32> {
        let target = types(ty);
        if target == u32::MAX {
            return Err(needs_bind(format!("type {ty} is unresolved")));
        }
        Ok(target)
    };
    while !operators.eof() {
        let start = operators.original_position();
        let operator = operators.read()?;
        let end = operators.original_position();
        match &operator {
            // One opcode byte, then the function's index.
            Operator::Call {
                function_index: index,
            }
            | Operator::ReturnCall {
                function_index: index,
            }
            | Operator::RefFunc {
                function_index: index,
            } => {
                out.push(body[start]);
                write_leb(&mut out, function_index(*index)?);
            }
            // One opcode byte, then the type's index, then the rest.
            Operator::CallIndirect { type_index: ty, .. }
            | Operator::ReturnCallIndirect { type_index: ty, .. } => {
                out.push(body[start]);
                let mut at = start + 1;
                let _old = read_leb(body, &mut at)?;
                write_leb(&mut out, type_index(*ty)?);
                out.extend_from_slice(&body[at..end]);
            }
            Operator::Block { blockty }
            | Operator::Loop { blockty }
            | Operator::If { blockty }
            | Operator::Try { blockty }
                if matches!(blockty, wp::BlockType::FuncType(_)) =>
            {
                struct Map<'a> {
                    funcs: &'a [u32],
                    types: &'a dyn Fn(u32) -> u32,
                }
                impl Reencode for Map<'_> {
                    type Error = String;
                    fn function_index(&mut self, function: u32) -> Result<u32, ReError<String>> {
                        match self.funcs.get(function as usize) {
                            Some(target) if *target != u32::MAX => Ok(*target),
                            _ => Err(ReError::UserError(format!(
                                "function {function} is unresolved"
                            ))),
                        }
                    }
                    fn type_index(&mut self, ty: u32) -> Result<u32, ReError<String>> {
                        let target = (self.types)(ty);
                        if target == u32::MAX {
                            return Err(ReError::UserError(format!("type {ty} is unresolved")));
                        }
                        Ok(target)
                    }
                }
                let mut map = Map { funcs, types };
                let instruction = utils::instruction(&mut map, operator)
                    .map_err(|error| needs_bind(format!("renumbering a body: {error:?}")))?;
                instruction.encode(&mut out);
            }
            _ => out.extend_from_slice(&body[start..end]),
        }
    }
    Ok(out)
}

// -------------------------------------------------------------- translation

/// A body translated from the linked module's index space to the bound one.
///
/// walrus emits a body's locals as its arguments, then the used non-argument
/// locals grouped by type in `ValType` order; its instructions canonically,
/// with function and type indices mapped and local indices renumbered.
fn translate(
    linked: &[u8],
    span: &Range<usize>,
    lmeta: &Meta,
    types: &[u32],
    map: &[u32],
    local: usize,
) -> Result<Vec<u8>> {
    let bytes = &linked[span.clone()];
    let body = wp::FunctionBody::new(wp::BinaryReader::new(bytes, span.start));
    let nargs = lmeta.nargs(local);
    let mut decl = Vec::new();
    for pair in body.get_locals_reader()? {
        let (count, ty) = pair?;
        decl.push((count, ty));
    }
    let mut operators = body.get_operators_reader()?;
    let mut used = HashSet::new();
    while !operators.eof() {
        match operators.read()? {
            Operator::LocalGet { local_index }
            | Operator::LocalSet { local_index }
            | Operator::LocalTee { local_index } => {
                used.insert(local_index);
            }
            _ => {}
        }
    }
    // The used locals, grouped by type: each type's locals in declaration
    // order, and the types in `ValType` order.
    let mut groups: BTreeMap<ValType, Vec<u32>> = BTreeMap::new();
    let mut index = nargs;
    for (count, ty) in &decl {
        for _ in 0..*count {
            if used.contains(&index) {
                groups.entry(*ty).or_default().push(index);
            }
            index += 1;
        }
    }
    let mut locals = Vec::new();
    let mut local_index: HashMap<u32, u32> = (0..nargs).map(|i| (i, i)).collect();
    let mut next = nargs;
    for (ty, indices) in &groups {
        locals.push((indices.len() as u32, wasm_encoder::ValType::try_from(*ty)?));
        for source in indices {
            local_index.insert(*source, next);
            next += 1;
        }
    }
    struct Tr<'a> {
        types: &'a [u32],
        map: &'a [u32],
        locals: &'a HashMap<u32, u32>,
    }
    impl Reencode for Tr<'_> {
        type Error = String;
        fn function_index(&mut self, function: u32) -> Result<u32, ReError<String>> {
            match self.map.get(function as usize) {
                Some(target) if *target != u32::MAX => Ok(*target),
                _ => Err(ReError::UserError(format!(
                    "function {function} is unresolved"
                ))),
            }
        }
        fn type_index(&mut self, ty: u32) -> Result<u32, ReError<String>> {
            match self.types.get(ty as usize) {
                Some(target) if *target != u32::MAX => Ok(*target),
                _ => Err(ReError::UserError(format!("type {ty} is unresolved"))),
            }
        }
    }
    let mut tr = Tr {
        types,
        map,
        locals: &local_index,
    };
    let mut reader = body.get_operators_reader()?;
    let mut function = wasm_encoder::Function::new(locals);
    while !reader.eof() {
        let instruction = utils::parse_instruction(&mut tr, &mut reader)
            .map_err(|error| needs_bind(format!("translating a body: {error:?}")))?;
        let instruction = match instruction {
            Instruction::LocalGet(i) => {
                Instruction::LocalGet(*tr.locals.get(&i).context("a local is unresolved")?)
            }
            Instruction::LocalSet(i) => {
                Instruction::LocalSet(*tr.locals.get(&i).context("a local is unresolved")?)
            }
            Instruction::LocalTee(i) => {
                Instruction::LocalTee(*tr.locals.get(&i).context("a local is unresolved")?)
            }
            other => other,
        };
        function.instruction(&instruction);
    }
    // `Function::encode` writes the body's size LEB first; the patch writes
    // its own, so the body is kept as its content alone.
    let mut out = Vec::new();
    function.encode(&mut out);
    let mut at = 0;
    let _size = read_leb(&out, &mut at)?;
    Ok(out.split_off(at))
}

fn bound_body_instr_start(body: &[u8]) -> Result<usize> {
    Ok(wp::FunctionBody::new(wp::BinaryReader::new(body, 0))
        .get_operators_reader()?
        .original_position())
}

/// What the splitter reads of a patch: what each function of the old bound
/// module became, what each of the new linked module's functions became, and
/// the imports of both.
fn write_renumbering(
    cache: &Path,
    bound: &[u8],
    old_imports: u32,
    map: &[u32],
    linked: &[u32],
) -> Result<()> {
    let mut out = Vec::with_capacity(32 + map.len() * 4 + linked.len() * 4);
    out.extend_from_slice(RENUMBER_MAGIC);
    out.extend_from_slice(&hash_bytes(bound).to_le_bytes());
    // The imports do not move: both modules start their functions after the
    // same ones.
    out.extend_from_slice(&old_imports.to_le_bytes());
    out.extend_from_slice(&old_imports.to_le_bytes());
    out.extend_from_slice(&(map.len() as u32).to_le_bytes());
    for target in map {
        out.extend_from_slice(&target.to_le_bytes());
    }
    out.extend_from_slice(&(linked.len() as u32).to_le_bytes());
    for target in linked {
        out.extend_from_slice(&target.to_le_bytes());
    }
    let path = cache.join("renumber.bin");
    let next = cache.join(format!(".renumber.{}.next", std::process::id()));
    std::fs::write(&next, &out).with_context(|| format!("writing {}", next.display()))?;
    std::fs::rename(&next, &path).with_context(|| format!("writing {}", path.display()))
}
