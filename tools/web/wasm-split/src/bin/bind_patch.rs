//! Patch a wasm-bindgen output for a body-only change instead of binding the
//! whole module again (`tools/web/wasm-split/bind-patch.sh`).
//!
//! Binding a 264 MB module costs twenty seconds, almost all of it re-encoding
//! bodies that did not change. walrus, the library wasm-bindgen binds with,
//! emits the local functions sorted by instruction count (largest first, then
//! source order), renumbers their locals (arguments, then the used locals
//! grouped by type in `ValType` order) and rewrites function and type
//! indices canonically. Every part of that is a function of the module's
//! structure and of each body alone, so one body's output can be worked out
//! without binding the rest.
//!
//! `record` is run once after a real bind: it reads the linked module and
//! wasm-bindgen's output, pairs every function by name (names survive the
//! bind; functions the bind dropped are left unresolved), learns the
//! function and type index maps, and checks that the emitted order is the
//! one above. It keeps the bound module and that map beside the build
//! directory's caches.
//!
//! `apply` reads the next linked module and patches the kept bound module
//! when nothing but bodies changed: the same imports, functions, exports,
//! tables, globals, data and custom sections, the same number of bodies, and
//! every changed body the same size and with the same callees (so the order
//! the bind emits, and the functions its reachability pass keeps, cannot
//! move). Each changed body is translated with the learned index maps and
//! spliced in. Anything else exits `NEEDS_BIND` and the caller binds fully:
//! this runs in a development loop, where a slower bundle is a nuisance and
//! a bundle differing from a fresh build is a bug.

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
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

const MAGIC: &[u8; 8] = b"WSBIND\0\x02";

/// How many bodies one patch may replace. A command that reformats a file
/// still patches; one that rewrites a module binds.
const MAX_CHANGED: usize = 16;

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

/// What the bind reads of the module besides each body: everything a body-only
/// change leaves as it was, hashed as one.
fn structure_hash(bytes: &[u8], sections: &[Section]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for section in sections {
        if section.id == 10 {
            continue;
        }
        section.id.hash(&mut hasher);
        bytes[section.contents.clone()].hash(&mut hasher);
    }
    hasher.finish()
}

/// The module's function and type tables and names, and the index spaces the
/// bound output shares with it.
struct Meta {
    imported: u32,
    /// The local functions' type indices, in source order.
    func_types: Vec<u32>,
    type_shapes: Vec<String>,
    /// Each type's parameter count.
    params: Vec<u32>,
    names: HashMap<u32, String>,
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

/// What one body is for the purpose of knowing whether it moved, and whether
/// patching may keep its neighbours where they are.
#[derive(Clone)]
struct BodyMeta {
    /// Its bytes, hashed: a body-only change moves exactly the bodies it
    /// touched.
    hash: u64,
    /// walrus's `size()`: instructions without the block terminators. The
    /// bind emits the local functions sorted by it, so a body whose size
    /// moved would move its function, renumbering the module.
    size: u32,
    /// The functions it calls or names, sorted: a change to those can change
    /// which functions the bind's reachability pass keeps.
    callees: u64,
}

fn body_meta(bytes: &[u8], span: &Range<usize>) -> Result<BodyMeta> {
    let body = wp::FunctionBody::new(wp::BinaryReader::new(&bytes[span.clone()], span.start));
    let mut operators = body.get_operators_reader()?;
    let mut size = 0u32;
    let mut callees = Vec::new();
    while !operators.eof() {
        let operator = operators.read()?;
        if !matches!(operator, Operator::End | Operator::Else) {
            size += 1;
        }
        match operator {
            Operator::Call { function_index }
            | Operator::ReturnCall { function_index }
            | Operator::RefFunc { function_index } => callees.push(function_index),
            _ => {}
        }
    }
    callees.sort_unstable();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    callees.hash(&mut hasher);
    let mut body_hasher = std::collections::hash_map::DefaultHasher::new();
    bytes[span.clone()].hash(&mut body_hasher);
    Ok(BodyMeta {
        hash: body_hasher.finish(),
        size,
        callees: hasher.finish(),
    })
}

fn read_meta(bytes: &[u8]) -> Result<Meta> {
    let mut imported = 0u32;
    let mut func_types = Vec::new();
    let mut type_shapes = Vec::new();
    let mut params = Vec::new();
    let mut names = HashMap::new();
    for payload in Parser::new(0).parse_all(bytes) {
        match payload? {
            Payload::ImportSection(imports) => {
                for group in imports {
                    for import in group? {
                        if let TypeRef::Func(_) = import?.1.ty {
                            imported += 1;
                        }
                    }
                }
            }
            Payload::TypeSection(types) => {
                for ty in types.into_iter_err_on_gc_types() {
                    let ty = ty?;
                    params.push(ty.params().len() as u32);
                    type_shapes.push(format!("{:?}->{:?}", ty.params(), ty.results()));
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
                    if let wp::Name::Function(map) = part? {
                        for naming in map {
                            let naming = naming?;
                            names.insert(naming.index, naming.name.to_owned());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(Meta {
        imported,
        func_types,
        type_shapes,
        params,
        names,
    })
}

/// Every body's metadata, and the ranges it occupies.
fn bodies_meta(bytes: &[u8], spans: &[Range<usize>]) -> Result<Vec<BodyMeta>> {
    spans.iter().map(|span| body_meta(bytes, span)).collect()
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
        if *target == u32::MAX {
            continue;
        }
        let (lty, bty) = if index >= imported {
            let local = index - imported;
            let target_local = *target as usize - bmeta.imported as usize;
            (
                lmeta.func_types.get(local),
                bmeta.func_types.get(target_local),
            )
        } else {
            continue;
        };
        if let (Some(lty), Some(bty)) = (lty, bty) {
            *votes.entry(*lty).or_default().entry(*bty).or_insert(0) += 1;
        }
    }
    let mut types = vec![u32::MAX; lmeta.type_shapes.len()];
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
    let mut by_shape: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, shape) in bmeta.type_shapes.iter().enumerate() {
        by_shape.entry(shape.as_str()).or_default().push(index);
    }
    for (index, shape) in lmeta.type_shapes.iter().enumerate() {
        if types[index] != u32::MAX {
            continue;
        }
        if let Some(candidates) = by_shape.get(shape.as_str()) {
            if candidates.len() == 1 {
                types[index] = candidates[0] as u32;
            }
        }
    }
    types
}

// --------------------------------------------------------------- the state

struct State {
    linked_hash: u64,
    structure: u64,
    imports: u32,
    /// How many imports the bound module has: its function indices start
    /// after them.
    bound_imports: u32,
    bodies: Vec<BodyMeta>,
    map: Vec<u32>,
    types: Vec<u32>,
}

impl State {
    fn write(&self, dir: &Path) -> Result<()> {
        let mut out = Vec::with_capacity(64 + self.bodies.len() * 20 + self.map.len() * 4);
        out.extend_from_slice(MAGIC);
        put(&mut out, self.linked_hash);
        put(&mut out, self.structure);
        put(&mut out, self.imports as u64);
        put(&mut out, self.bound_imports as u64);
        put(&mut out, self.bodies.len() as u64);
        for body in &self.bodies {
            put(&mut out, body.hash);
            put(&mut out, body.size as u64);
            put(&mut out, body.callees);
        }
        put(&mut out, self.map.len() as u64);
        for target in &self.map {
            put(&mut out, *target as u64);
        }
        put(&mut out, self.types.len() as u64);
        for ty in &self.types {
            put(&mut out, *ty as u64);
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
        let count = reader.u64()? as usize;
        let mut bodies = Vec::with_capacity(count);
        for _ in 0..count {
            let hash = reader.u64()?;
            let size = reader.u64()? as u32;
            let callees = reader.u64()?;
            bodies.push(BodyMeta {
                hash,
                size,
                callees,
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
        Ok(Some(Self {
            linked_hash,
            structure,
            imports,
            bound_imports,
            bodies,
            map,
            types,
        }))
    }
}

fn put(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
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

fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
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
    let lbodies = bodies_meta(&linked, &lspans)?;
    let bbodies = bodies_meta(&bound, &bspans)?;
    // The bind emits local functions sorted by size; a module whose output
    // does not is one this map does not describe.
    if bbodies.windows(2).any(|pair| pair[0].size < pair[1].size) {
        return Err(needs_bind(
            "the bound functions are not in the order this map assumes",
        ));
    }
    let map = function_map(&lmeta, &lspans, &lbodies, &bmeta, &bbodies);
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
    let state = State {
        linked_hash: hash_bytes(&linked),
        structure: structure_hash(&linked, &lsecs),
        imports: lmeta.imported,
        bound_imports: bmeta.imported,
        bodies: lbodies,
        map,
        types,
    };
    state.write(cache)?;
    let next = cache.join(format!(".bound.{}.next", std::process::id()));
    std::fs::write(&next, &bound).context("keeping the bound module")?;
    std::fs::rename(&next, cache.join("bound.wasm")).context("keeping the bound module")?;
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
    if structure_hash(&linked, &lsecs) != state.structure {
        return Err(needs_bind("something outside the bodies changed"));
    }
    let lmeta = read_meta(&linked)?;
    if lmeta.imported != state.imports || lspans.len() != state.bodies.len() {
        return Err(needs_bind("the functions changed"));
    }
    let lbodies = bodies_meta(&linked, &lspans)?;
    let mut changed = Vec::new();
    for (index, (before, after)) in state.bodies.iter().zip(&lbodies).enumerate() {
        if before.hash != after.hash {
            changed.push(index);
        }
    }
    if changed.is_empty() {
        return Err(needs_bind("no body changed"));
    }
    if changed.len() > MAX_CHANGED {
        return Err(needs_bind("too many bodies changed"));
    }
    for index in &changed {
        let (before, after) = (&state.bodies[*index], &lbodies[*index]);
        if before.size != after.size {
            return Err(needs_bind("a changed body changed size"));
        }
        if before.callees != after.callees {
            return Err(needs_bind("a changed body changed its callees"));
        }
    }
    // Translate every changed body before writing anything.
    let mut replacements = Vec::new();
    for index in &changed {
        let function = *index as u32 + lmeta.imported;
        let target = *state
            .map
            .get(function as usize)
            .context("the map does not reach this function")?;
        if target == u32::MAX {
            return Err(needs_bind("a changed function has no bound counterpart"));
        }
        let body = translate(
            &linked,
            &lspans[*index],
            &lmeta,
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
    let bsecs = sections(&bound)?;
    let (code, bspans) = code_section(&bound, &bsecs)?;
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
    let mut state = state;
    state.linked_hash = hash_bytes(&linked);
    for (index, body) in lbodies.into_iter().enumerate() {
        state.bodies[index] = body;
    }
    state.write(cache)?;
    let next = cache.join(format!(".bound.{}.next", std::process::id()));
    std::fs::write(&next, &patched).context("keeping the patched bound module")?;
    std::fs::rename(&next, cache.join("bound.wasm")).context("keeping the patched bound module")?;
    eprintln!(
        "wasm-bind-patch: patched {} of {} bodies",
        replacements.len(),
        state.bodies.len()
    );
    Ok(())
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
    let mut used = std::collections::HashSet::new();
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
