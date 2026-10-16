//! Split the browser desktop bundle into a start module and modules loaded on
//! demand.
//!
//! `wasm-split ORIGINAL BINDGENED PKG_DIR NAME` reads the linked module
//! (`ORIGINAL`, built with `--emit-relocs` and its symbol table) and the
//! wasm-bindgen output made from it (`BINDGENED`), and writes into `PKG_DIR`:
//!
//! - `NAME_bg.wasm`: the start module, replacing the wasm-bindgen output;
//! - `NAME.<module>.wasm`: what only one split point reaches (an app), code
//!   and the static data only that code reaches (src/data.rs);
//! - `NAME.shared<N>.wasm`: code several split points share but the start
//!   module does not, with the static data only that code reaches, loaded
//!   before any module that needs it;
//! - `__wasm_split.js`: the loaders the start module imports, one per split
//!   point, which fetch and instantiate a module into the start module's
//!   memory and function table and then call back into it, and `prefetch`,
//!   with which the page fetches and compiles a module by name before the
//!   start module is in;
//! - `split.json`: every module with its size and the shared code it needs.
//!
//! `wasm-split --unsplit BINDGENED PKG_DIR` leaves the module whole and
//! writes only a `__wasm_split.js` that answers the split points from it,
//! for quick development bundles.
//!
//! A split point is a pair of functions the Rust side declares by name
//! (`__wasm_split_00___<module>___00_import_<hash>_<name>` imported from
//! `./__wasm_split.js`, `…_export_…` exported); see
//! crates/browser/tilo-web-desktop/src/split.rs.

mod data;
mod split;
#[cfg(test)]
mod tests;

use std::fmt::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [flag, bindgened, out] = args.as_slice() {
        if flag == "--unsplit" {
            return unsplit(bindgened, &PathBuf::from(out));
        }
    }
    let [original, bindgened, out, name] = args.as_slice() else {
        bail!(
            "usage: wasm-split ORIGINAL BINDGENED PKG_DIR NAME | wasm-split --unsplit BINDGENED PKG_DIR"
        );
    };
    let original = std::fs::read(original).with_context(|| format!("reading {original}"))?;
    let bindgened = std::fs::read(bindgened).with_context(|| format!("reading {bindgened}"))?;
    let out = PathBuf::from(out);

    let outputs = split::Splitter::new(&original, &bindgened)?.emit()?;

    let main_file = format!("{name}_bg.wasm");
    std::fs::write(out.join(&main_file), &outputs.main.bytes)?;
    let mut manifest = serde_json::json!({
        "schema": 1,
        "main": {"file": main_file, "bytes": outputs.main.bytes.len()},
        "shared": [],
        "modules": [],
    });

    let mut glue = String::from(LOADER);
    let mut shared_files = Vec::new();
    for (index, chunk) in outputs.chunks.iter().enumerate() {
        let file = format!("{name}.shared{index}.wasm");
        std::fs::write(out.join(&file), &chunk.bytes)?;
        writeln!(
            glue,
            "const shared{index} = loader({file:?}, []);\nexport const __wasm_split_load_chunk_{index} = shared{index}.export;"
        )?;
        manifest["shared"]
            .as_array_mut()
            .expect("array")
            .push(serde_json::json!({"file": file, "bytes": chunk.bytes.len()}));
        shared_files.push(file);
    }
    for module in &outputs.modules {
        let component = module
            .component_name
            .as_deref()
            .context("split point without a name")?;
        let hash = module
            .hash_id
            .as_deref()
            .context("split point without a hash")?;
        let file = format!("{name}.{}.wasm", module.module_name);
        std::fs::write(out.join(&file), &module.bytes)?;
        let mut needs: Vec<usize> = module.relies_on_chunks.iter().copied().collect();
        needs.sort_unstable();
        let deps = needs
            .iter()
            .map(|index| format!("shared{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(
            glue,
            "export const __wasm_split_load_{}_{hash}_{component} = (modules[{:?}] = loader({file:?}, [{deps}])).export;",
            module.module_name, module.module_name
        )?;
        manifest["modules"]
            .as_array_mut()
            .expect("array")
            .push(serde_json::json!({
                "module": module.module_name,
                "file": file,
                "bytes": module.bytes.len(),
                "shared": needs.iter().map(|index| &shared_files[*index]).collect::<Vec<_>>(),
            }));
    }
    std::fs::write(out.join("__wasm_split.js"), glue)?;
    std::fs::write(
        out.join("split.json"),
        serde_json::to_string_pretty(&manifest)? + "\n",
    )?;
    eprintln!(
        "wasm-split: start module {} bytes, {} modules, {} shared",
        outputs.main.bytes.len(),
        outputs.modules.len(),
        outputs.chunks.len()
    );
    Ok(())
}

/// `--unsplit`: leave the module whole and write a `__wasm_split.js` that
/// answers its split points from the module itself. Every loader reports
/// its module loaded at once, and every import calls the matching export,
/// which the module still holds. For a quick development bundle; release
/// bundles are always split.
fn unsplit(bindgened: &str, out: &std::path::Path) -> Result<()> {
    let bytes = std::fs::read(bindgened).with_context(|| format!("reading {bindgened}"))?;
    let mut glue = String::from(UNSPLIT_LOADER);
    let mut points = 0;
    for payload in wasmparser::Parser::new(0).parse_all(&bytes) {
        match payload? {
            wasmparser::Payload::ImportSection(imports) => {
                for import in imports {
                    let import = import?;
                    if import.module != "./__wasm_split.js" {
                        continue;
                    }
                    let name = import.name;
                    if name.starts_with("__wasm_split_load_") {
                        writeln!(glue, "export const {name} = loaded;")?;
                    } else if let Some((module, rest)) = name.split_once("_import_") {
                        writeln!(
                            glue,
                            "export const {name} = (...args) => initSync(undefined).{module}_export_{rest}(...args);"
                        )?;
                        points += 1;
                    } else {
                        bail!("unknown split import {name}");
                    }
                }
                break;
            }
            wasmparser::Payload::CodeSectionStart { .. } => break,
            _ => {}
        }
    }
    std::fs::write(out.join("__wasm_split.js"), glue)?;
    eprintln!("wasm-split: left whole, {points} split points answered in place");
    Ok(())
}

const UNSPLIT_LOADER: &str = r#"// Generated by tools/web/wasm-split --unsplit. The module was left whole:
// each app is already in it, so its loader reports it loaded at once. The
// whole module does not export its function table, so the report goes
// through the export the Rust side keeps for this
// (crates/browser/tilo-web-desktop/src/split.rs), not the callback.
import {initSync} from './tilo_browser_runtime.js';

function loaded(_callback, data) {
  queueMicrotask(() => initSync(undefined).tilo_split_loaded_in_place(data));
}

/** Nothing to fetch: every app is in the module already. */
export function prefetch(_module) {}
"#;

/// The loaders' common part. A loader fetches and compiles its module once
/// (again after a failure), instantiates it against the start module's
/// exports and those of the modules loaded before it, and reports through
/// the callback the Rust side passed: `table[callback](data, loaded)`.
/// Compiling needs nothing from the start module, so `prefetch` can start it
/// while the start module itself still downloads: the page does so for the
/// app it is about to show, whose module then arrives beside the start
/// module instead of after it. A module the server no longer has is also
/// announced to the page (`tilo-module-unavailable` on its global, with the
/// module's URL): a page that outlived its deployed version finds its modules
/// gone, and the page offers to reload into the new one
/// (crates/browser/tilo-browser-runtime/web/bundle-update.mjs).
const LOADER: &str = r#"// Generated by tools/web/wasm-split. Loads the modules split off the start
// module when the Rust side first needs them.
import {initSync} from './tilo_browser_runtime.js';

const linked = {};
const modules = {};

function loader(file, needs) {
  const url = new URL(file, import.meta.url);
  let compiled;
  const compile = () => compiled ??= (async () => {
    const response = await fetch(url);
    if (!response.ok) {
      globalThis.dispatchEvent?.(new CustomEvent('tilo-module-unavailable', {detail: {url: url.href}}));
      throw new Error(`${url.href}: HTTP ${response.status}`);
    }
    return WebAssembly.compileStreaming(response);
  })().catch(error => {
    compiled = undefined;
    throw error;
  });
  let pending;
  const load = () => pending ??= (async () => {
    await Promise.all(needs.map(need => need.load()));
    const main = initSync(undefined);
    const imports = {env: {memory: main.memory}, __wasm_split: {...main, ...linked}};
    const instance = await WebAssembly.instantiate(await compile(), imports);
    Object.assign(linked, instance.exports);
  })().catch(error => {
    pending = undefined;
    console.error(`could not load ${file}`, error);
    throw error;
  });
  const prefetch = () => {
    for (const need of needs) need.prefetch();
    // A failure is the load's to report: it compiles again.
    compile().catch(() => {});
  };
  const exported = (callback, data) => {
    load().then(() => true, () => false).then(loaded => {
      if (callback !== undefined) initSync(undefined).__indirect_function_table.get(callback)(data, loaded);
    });
  };
  return {load, prefetch, export: exported};
}

/** Fetch and compile split module `module` and the shared code it needs
 * now, so that its first load only instantiates them. An unknown name is
 * ignored. */
export function prefetch(module) {
  if (Object.hasOwn(modules, module)) modules[module].prefetch();
}
"#;
