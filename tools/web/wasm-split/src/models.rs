//! `wasm-split --models`: split one module holding every app's model into
//! the model runtime every app shares and one small module per app.
//!
//! The linked module is the combined model build (crates/sdk/tilo-models-wasm),
//! linked with `--emit-relocs` and its symbol table, and never seen by
//! wasm-bindgen. Each app's guest crate declares its entry with
//! `tilo_model_guest::model!`, which exports `tilo_model_apply_<app>` and names
//! it in the `tilo_split_exports` section as `<app>`'s lazy export. The split
//! then moves what only that entry reaches into the app's module, and leaves
//! everything else (automerge, std, the resource layer, serde_json) in the
//! start module and the shared chunks: the model runtime.
//!
//! `wasm-split --models LINKED OUT_DIR [--wasm-opt FLAGS_FILE]` writes (each
//! module optimized with `wasm-opt` and the arguments in `FLAGS_FILE` first,
//! when one is named: tools/web/wasm-opt-flags, as every module the repository
//! ships):
//!
//! - `OUT_DIR/app-model-runtime/main.wasm` and `shared<N>.wasm`, the runtime, and
//!   `runtime.json`, which lists them and names the runtime by [`hash`];
//! - `OUT_DIR/<app>/model.wasm`, each app's module, with a
//!   [`RUNTIME_SECTION`] custom section naming the runtime it was split
//!   against and the export a device calls once it is in.
//!
//! A device links them as the page links the desktop's modules
//! (`__wasm_split.js`): it instantiates the runtime's start module with the
//! model ABI, then every shared chunk and the app's module, each import
//! answered by the export of that name of the instances before it, and calls
//! the entry, which the start module still exports and which calls through the
//! table entry the app's module filled (crates/lang/tilo-model-engine).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};

use crate::split::{OutputModules, Splitter};

/// The custom section of an app's split module: JSON naming the runtime it
/// links against (`runtime`, [`hash`]) and the start module's export that
/// calls it (`entry`).
pub const RUNTIME_SECTION: &str = "tilo_model_runtime";

/// The directory under the output the runtime is written to.
pub const RUNTIME_DIR: &str = "app-model-runtime";

/// The import module of the model ABI: the only one the start module may
/// import from, since a device links nothing else.
const ABI_MODULE: &str = "tilo_model";

/// The runtime and the apps' modules of one split.
pub struct ModelSplit {
    /// The start module, then each shared chunk in order.
    pub runtime: Vec<Vec<u8>>,
    /// [`hash`] of `runtime`.
    pub hash: String,
    /// Each app's module, its [`RUNTIME_SECTION`] written, by app.
    pub apps: BTreeMap<String, Vec<u8>>,
}

/// The runtime's name: BLAKE3 over each of its modules in order, each as its
/// length (eight bytes, little-endian) and its bytes. A device computes the same
/// over the files it reads, so an app module split against another runtime is
/// refused rather than linked to the wrong code.
pub fn hash(parts: &[Vec<u8>]) -> String {
    let mut hasher = blake3::Hasher::new();
    for part in parts {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    hasher.finalize().to_hex().to_string()
}

/// What each written module goes through after the split, before the
/// runtime is named by its bytes: an optimizer, or nothing.
pub type Optimize<'a> = &'a dyn Fn(&[u8]) -> Result<Vec<u8>>;

/// Split `linked`, the combined model build, passing each module through
/// `optimize` when there is one.
pub fn split(linked: &[u8], optimize: Option<Optimize<'_>>) -> Result<ModelSplit> {
    let entries = entries(linked)?;
    ensure!(
        !entries.is_empty(),
        "the module names no app's entry in its tilo_split_exports section"
    );
    let outputs = Splitter::new(linked, linked)?.emit()?;
    assemble(outputs, &entries, optimize)
}

/// Each app's entry, from the lazy exports the guests declared: one export
/// per app, named in full.
fn entries(linked: &[u8]) -> Result<BTreeMap<String, String>> {
    let mut entries = BTreeMap::new();
    for (app, export) in crate::split::lazy_exports(linked)? {
        ensure!(
            !export.ends_with('*'),
            "{app}'s entry {export} is a pattern; a model names its one entry"
        );
        if let Some(earlier) = entries.insert(app.clone(), export.clone()) {
            bail!("{app} declares two entries, {earlier} and {export}");
        }
    }
    Ok(entries)
}

fn assemble(
    outputs: OutputModules,
    entries: &BTreeMap<String, String>,
    optimize: Option<Optimize<'_>>,
) -> Result<ModelSplit> {
    let finish = |module: &[u8]| -> Result<Vec<u8>> {
        let module = bare(module)?;
        match optimize {
            Some(optimize) => optimize(&module),
            None => Ok(module),
        }
    };
    let mut runtime = vec![finish(&outputs.main.bytes)?];
    for chunk in &outputs.chunks {
        runtime.push(finish(&chunk.bytes)?);
    }
    for (module, name) in imports(&runtime[0])? {
        ensure!(
            module == ABI_MODULE,
            "the model runtime imports {module}::{name}; a device answers only {ABI_MODULE}"
        );
    }
    let hash = hash(&runtime);
    let mut apps = BTreeMap::new();
    for module in outputs.modules {
        let entry = entries.get(&module.module_name).with_context(|| {
            format!(
                "split module {} is no app's: only models' entries split",
                module.module_name
            )
        })?;
        let section = serde_json::json!({"runtime": hash, "entry": entry}).to_string();
        let mut bytes = finish(&module.bytes)?;
        append_custom(&mut bytes, RUNTIME_SECTION, section.as_bytes());
        apps.insert(module.module_name, bytes);
    }
    for app in entries.keys() {
        ensure!(
            apps.contains_key(app),
            "{app} declares an entry but the split wrote no module for it"
        );
    }
    Ok(ModelSplit {
        runtime,
        hash,
        apps,
    })
}

/// Write `split` under `out` ([`RUNTIME_DIR`] and one directory per app).
pub fn write(split: &ModelSplit, out: &Path) -> Result<()> {
    let dir = out.join(RUNTIME_DIR);
    let mut files = Vec::new();
    for (index, part) in split.runtime.iter().enumerate() {
        let file = if index == 0 {
            "main.wasm".to_owned()
        } else {
            format!("shared{}.wasm", index - 1)
        };
        crate::write(&dir.join(&file), part)?;
        files.push(file);
    }
    let manifest = serde_json::json!({
        "schema": 1,
        "hash": split.hash,
        "modules": files,
        "bytes": split.runtime.iter().map(Vec::len).sum::<usize>(),
        "apps": split
            .apps
            .iter()
            .map(|(app, bytes)| (app.clone(), serde_json::json!(bytes.len())))
            .collect::<serde_json::Map<_, _>>(),
    });
    crate::write(
        &dir.join("runtime.json"),
        (serde_json::to_string_pretty(&manifest)? + "\n").as_bytes(),
    )?;
    for (app, bytes) in &split.apps {
        crate::write(&out.join(app).join("model.wasm"), bytes)?;
    }
    Ok(())
}

/// `module` without its custom sections: the split keeps names and target
/// features for the browser bundle's optimiser, which a model module, read by
/// no optimiser and shipped to every device, has no use for.
fn bare(module: &[u8]) -> Result<Vec<u8>> {
    let mut out = module.get(..8).context("not a module")?.to_vec();
    for payload in wasmparser::Parser::new(0).parse_all(module) {
        let payload = payload?;
        let Some((id, range)) = payload.as_section() else {
            continue;
        };
        if id == 0 {
            continue;
        }
        // The section's id, its size and its contents, as they were.
        out.extend_from_slice(&module[section_start(module, range.start)?..range.end]);
    }
    Ok(out)
}

/// Where the section whose contents start at `contents` starts: its id byte,
/// before the LEB128 size.
fn section_start(module: &[u8], contents: usize) -> Result<usize> {
    // The size is at most five bytes, each but the last with its high bit
    // set; the id byte is before them.
    let mut at = contents.checked_sub(1).context("truncated module")?;
    ensure!(
        module[at] & 0x80 == 0,
        "a section's size does not end before it"
    );
    while at > 0 && module[at - 1] & 0x80 != 0 {
        at -= 1;
    }
    at.checked_sub(1).context("truncated module")
}

/// The module's function imports, as `(module, name)`.
fn imports(module: &[u8]) -> Result<Vec<(String, String)>> {
    let mut found = Vec::new();
    for payload in wasmparser::Parser::new(0).parse_all(module) {
        if let wasmparser::Payload::ImportSection(section) = payload? {
            for import in section {
                let import = import?;
                found.push((import.module.to_owned(), import.name.to_owned()));
            }
        }
    }
    Ok(found)
}

/// Append a custom section `name` holding `data` to `module`.
pub fn append_custom(module: &mut Vec<u8>, name: &str, data: &[u8]) {
    let mut contents = Vec::new();
    leb(&mut contents, name.len());
    contents.extend_from_slice(name.as_bytes());
    contents.extend_from_slice(data);
    module.push(0);
    leb(module, contents.len());
    module.extend_from_slice(&contents);
}

fn leb(out: &mut Vec<u8>, mut value: usize) {
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
