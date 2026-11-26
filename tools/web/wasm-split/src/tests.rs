//! Splits fixtures/data_split.rs, compiled for wasm32 as the desktop bundle
//! is linked, and runs the pieces in wasmi the way `__wasm_split.js` links
//! them: the start module first, a split module into its memory and table
//! when its split point is first called.

use std::collections::HashMap;
use std::ops::Range;
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};
use wasmparser::{Linking, LinkingSectionReader, Payload, SymbolInfo};

use crate::analysis::AnalysisCache;
use crate::cache::EmitCache;
use crate::split::{OutputModules, Splitter};

const LEN: usize = 4096;
const BLOBS: [&str; 5] = ["reader", "table", "shared", "listed", "start"];
/// Where `TABLE`'s slices of `TABLE_BLOB` start and end.
const TABLE_BOUNDS: [usize; 7] = [0, 100, 700, 1500, 2048, 3000, LEN];

/// A blob of bytes that are never zero, so a range read back as zeros was
/// not written.
fn blob(seed: u32) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9e37_79b9) | 1;
    (0..LEN)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state >> 24) as u8 | 1
        })
        .collect()
}

struct Fixture {
    linked: Vec<u8>,
    blobs: HashMap<&'static str, Vec<u8>>,
}

/// A directory under the test environment's temporary directory, this
/// process's own, named `name`: a run that leaves one behind never hands it
/// to the next, and two builds a test wants at one path share it.
fn scratch(name: &str) -> std::path::PathBuf {
    use std::sync::OnceLock;
    static RUN: OnceLock<u64> = OnceLock::new();
    let run = *RUN.get_or_init(|| {
        let mut bytes = [0u8; 8];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut random| std::io::Read::read_exact(&mut random, &mut bytes))
            .map(|()| u64::from_le_bytes(bytes))
            .unwrap_or_else(|_| std::process::id() as u64)
    });
    std::env::temp_dir().join(format!("wasm-split-{name}-{run:x}"))
}

/// Compile the fixture with the toolchain running the tests, in a
/// directory of its own per test (`test`).
fn build_fixture(test: &str) -> Result<Fixture> {
    build_fixture_from(test, include_str!("../fixtures/data_split.rs"))
}

/// [`build_fixture`] from `source`, the fixture as a test changed it.
fn build_fixture_from(test: &str, source_text: &str) -> Result<Fixture> {
    build_fixture_edited(test, source_text, &HashMap::new())
}

/// [`build_fixture_from`] with some blobs' bytes replaced: a change to the
/// module's static data that leaves its code as it was.
fn build_fixture_edited(
    test: &str,
    source_text: &str,
    edits: &HashMap<&'static str, (usize, u8)>,
) -> Result<Fixture> {
    // One path per test name: a test that builds the same fixture again with
    // a change needs both at one path, since the module's static data holds
    // the source's path.
    let dir = scratch(test);
    std::fs::create_dir_all(&dir)?;
    let result = (|| {
        let source = dir.join("data_split.rs");
        std::fs::write(&source, source_text)?;
        let mut blobs = HashMap::new();
        for (seed, name) in BLOBS.iter().enumerate() {
            let mut bytes = blob(seed as u32 + 1);
            if let Some((at, byte)) = edits.get(name) {
                bytes[*at] = *byte;
            }
            std::fs::write(dir.join(format!("{name}.bin")), &bytes)?;
            blobs.insert(*name, bytes);
        }
        let module = dir.join("data_split.wasm");
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let output = Command::new(rustc)
            .args([
                "--edition=2024",
                "--crate-type=cdylib",
                "--target=wasm32-unknown-unknown",
                "-Copt-level=2",
                "-Cpanic=abort",
                "-Cstrip=none",
                "-Clink-arg=--emit-relocs",
                "-o",
            ])
            .arg(&module)
            .arg(&source)
            .output()
            .context("running rustc")?;
        ensure!(
            output.status.success(),
            "compiling the fixture failed (it needs the wasm32-unknown-unknown target): {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(Fixture {
            linked: std::fs::read(&module)?,
            blobs,
        })
    })();
    std::fs::remove_dir_all(&dir).ok();
    result
}

/// The address range of the data symbol whose (mangled) name holds `name`.
fn static_range(linked: &[u8], name: &str) -> Result<Range<u32>> {
    let mut bases = Vec::new();
    let mut found = Vec::new();
    for payload in wasmparser::Parser::new(0).parse_all(linked) {
        match payload? {
            Payload::DataSection(section) => {
                for data in section {
                    if let wasmparser::DataKind::Active { offset_expr, .. } = data?.kind {
                        match offset_expr.get_operators_reader().read()? {
                            wasmparser::Operator::I32Const { value } => bases.push(value as u32),
                            _ => bail!("data segment at a computed address"),
                        }
                    }
                }
            }
            Payload::CustomSection(section) if section.name() == "linking" => {
                let reader = wasmparser::BinaryReader::new(section.data(), 0);
                for subsection in LinkingSectionReader::new(reader)?.subsections() {
                    if let Linking::SymbolTable(map) = subsection? {
                        for symbol in map {
                            if let SymbolInfo::Data {
                                name: symbol_name,
                                symbol: Some(symbol),
                                ..
                            } = symbol?
                            {
                                let marker = format!("{}{name}", name.len());
                                if symbol_name.contains(&marker) {
                                    found.push((symbol.index, symbol.offset, symbol.size));
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let [(segment, offset, size)] = found[..] else {
        bail!("{} data symbols named {name}", found.len());
    };
    let start = bases[segment as usize] + offset;
    Ok(start..start + size)
}

/// A module's active data segments, as `(address, bytes)`.
fn segments(module: &[u8]) -> Result<Vec<(u32, Vec<u8>)>> {
    let mut out = Vec::new();
    for payload in wasmparser::Parser::new(0).parse_all(module) {
        if let Payload::DataSection(section) = payload? {
            for data in section {
                let data = data?;
                if let wasmparser::DataKind::Active { offset_expr, .. } = data.kind {
                    if let wasmparser::Operator::I32Const { value } =
                        offset_expr.get_operators_reader().read()?
                    {
                        out.push((value as u32, data.data.to_vec()));
                    }
                }
            }
        }
    }
    Ok(out)
}

/// The bytes `segments` write to `range`, where they write any.
fn written(segments: &[(u32, Vec<u8>)], range: &Range<u32>) -> Vec<Option<u8>> {
    let mut out = vec![None; (range.end - range.start) as usize];
    for (base, bytes) in segments {
        for (i, byte) in bytes.iter().enumerate() {
            let address = base + i as u32;
            if range.contains(&address) {
                out[(address - range.start) as usize] = Some(*byte);
            }
        }
    }
    out
}

fn split_module<'a>(outputs: &'a OutputModules, name: &str) -> &'a [u8] {
    &outputs
        .modules
        .iter()
        .find(|module| module.module_name == name)
        .unwrap_or_else(|| panic!("no split module {name}"))
        .bytes
}

fn offset(i: u32) -> usize {
    (i.wrapping_mul(2_654_435_761) >> 7) as usize % LEN
}

fn checksum(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .fold(0u32, |sum, byte| sum.rotate_left(5) ^ u32::from(*byte))
}

/// `module` without its `linking` and `reloc.*` custom sections.
fn without_link_sections(module: &[u8]) -> Result<Vec<u8>> {
    let mut out = module[..8].to_vec();
    for payload in wasmparser::Parser::new(0).parse_all(module) {
        let Some((id, range)) = payload?.as_section() else {
            continue;
        };
        let section = &module[range.clone()];
        if id == 0 {
            let name = wasmparser::CustomSectionReader::new(wasmparser::BinaryReader::new(
                section,
                range.start,
            ))?;
            if name.name() == "linking" || name.name().starts_with("reloc.") {
                continue;
            }
        }
        out.push(id);
        let mut size = section.len();
        loop {
            let byte = (size & 0x7f) as u8;
            size >>= 7;
            if size == 0 {
                out.push(byte);
                break;
            }
            out.push(byte | 0x80);
        }
        out.extend_from_slice(section);
    }
    Ok(out)
}

/// Every module a split writes, by name, with the shared code it needs.
fn written_modules(outputs: OutputModules) -> Vec<(String, Vec<usize>, Vec<u8>)> {
    let mut modules = vec![("main".to_string(), Vec::new(), outputs.main.bytes)];
    for (index, chunk) in outputs.chunks.into_iter().enumerate() {
        modules.push((format!("shared{index}"), Vec::new(), chunk.bytes));
    }
    for module in outputs.modules {
        let mut needs: Vec<usize> = module.relies_on_chunks.into_iter().collect();
        needs.sort_unstable();
        modules.push((module.module_name, needs, module.bytes));
    }
    modules
}

/// A split that reads modules back from the cache writes exactly what one
/// that emits every module writes: for the input it was filled from, and
/// after a change to one module's code, where it emits that module again and
/// reads the others back.
#[test]
fn a_split_from_the_cache_writes_what_emitting_writes() -> Result<()> {
    let source = include_str!("../fixtures/data_split.rs");
    // A constant in `later_step`, which only the `later` module's code holds.
    let changed = source.replace("0x5bd1_e995", "0x5bd1_e996");
    assert_ne!(source, changed);
    // Built at one path: the module's static data holds the source's path.
    let before = build_fixture("cache")?.linked;
    let after = build_fixture_from("cache", &changed)?.linked;
    // The module the local bundle binds goes without the sections whose
    // offsets every change to code moves (without-link-sections.py).
    let (before_bound, after_bound) = (
        without_link_sections(&before)?,
        without_link_sections(&after)?,
    );
    let dir = scratch("emit-cache");
    std::fs::remove_dir_all(&dir).ok();
    let result = (|| -> Result<()> {
        let fresh = |linked: &[u8], bound: &[u8]| -> Result<_> {
            Ok(written_modules(
                Splitter::new(linked, bound)?.emit_with(None)?,
            ))
        };
        let cached = |linked: &[u8], bound: &[u8]| -> Result<_> {
            let outputs = Splitter::new(linked, bound)?.emit_with(Some(&EmitCache::at(&dir)?))?;
            let reused = outputs.reused;
            let count = outputs.modules.len() + outputs.chunks.len();
            Ok((written_modules(outputs), reused, count))
        };
        let (first, reused, _) = cached(&before, &before_bound)?;
        assert_eq!(reused, 0, "an empty cache holds no module");
        assert!(
            first == fresh(&before, &before_bound)?,
            "a split filling the cache wrote other modules"
        );
        let (again, reused, count) = cached(&before, &before_bound)?;
        assert_eq!(
            reused, count,
            "the same input emitted modules the cache held"
        );
        assert!(
            again == first,
            "modules read back from the cache differ from the emitted ones"
        );
        let (changed, reused, count) = cached(&after, &after_bound)?;
        assert!(
            changed == fresh(&after, &after_bound)?,
            "after a change, the cache gave other modules than emitting does"
        );
        assert!(changed != first, "the change reached no module");
        assert!(
            reused > 0 && reused < count,
            "a change to one module's code emitted {} of {count} modules again",
            count - reused
        );
        Ok(())
    })();
    std::fs::remove_dir_all(&dir).ok();
    result
}

/// A change to the code the start module holds alone — a body no split
/// module holds and whose signature is the same — writes what emitting every
/// module writes, and reads the modules back: a module's emission reads the
/// start module's interface, and a body is no part of it.
#[test]
fn a_start_module_body_change_reads_the_modules_back() -> Result<()> {
    let source = include_str!("../fixtures/data_split.rs");
    // `checksum` is the start module's: the split modules import it and hand
    // it pointers, and no split module holds its code.
    let edited = source.replace(
        "sum.rotate_left(5) ^ u32::from(*byte)",
        "sum.rotate_left(5).wrapping_add(u32::from(*byte))",
    );
    assert_ne!(
        source, edited,
        "the fixture no longer mixes a byte with `^`"
    );
    let before = build_fixture("start-body")?.linked;
    let after = build_fixture_from("start-body", &edited)?.linked;
    let (before_bound, after_bound) = (
        without_link_sections(&before)?,
        without_link_sections(&after)?,
    );
    let dir = scratch("start-body-cache");
    std::fs::remove_dir_all(&dir).ok();
    let result = (|| -> Result<()> {
        let fresh = |linked: &[u8], bound: &[u8]| -> Result<_> {
            Ok(written_modules(
                Splitter::new(linked, bound)?.emit_with(None)?,
            ))
        };
        let cached = |linked: &[u8], bound: &[u8]| -> Result<(_, usize, usize)> {
            let outputs = Splitter::new(linked, bound)?.emit_with(Some(&EmitCache::at(&dir)?))?;
            let reused = outputs.reused;
            let count = outputs.modules.len() + outputs.chunks.len();
            Ok((written_modules(outputs), reused, count))
        };
        let (first, reused, _) = cached(&before, &before_bound)?;
        assert_eq!(reused, 0, "an empty cache holds no module");
        assert!(
            first == fresh(&before, &before_bound)?,
            "a split filling the cache wrote other modules"
        );
        let (changed, reused, count) = cached(&after, &after_bound)?;
        assert!(
            changed == fresh(&after, &after_bound)?,
            "after a start module body change, the cache gave other modules than emitting does"
        );
        assert_ne!(
            changed[0].2, first[0].2,
            "the change did not reach the start module"
        );
        assert_eq!(
            reused,
            count,
            "a start module body change emitted {} of {count} modules again",
            count - reused
        );
        Ok(())
    })();
    std::fs::remove_dir_all(&dir).ok();
    result
}

/// A module's emission is keyed by the start module it is paired with: the
/// same module beside a start module whose exports say something else is
/// never read back as one emitted beside this start module.
#[test]
fn a_module_is_keyed_by_the_start_module_it_pairs_with() -> Result<()> {
    let linked = build_fixture("start-pairing")?.linked;
    let bound = without_link_sections(&linked)?;
    let main = Splitter::new(&linked, &bound)?.emit_with(None)?.main.bytes;
    let splitter = Splitter::new(&linked, &bound)?;
    let key = |main: &[u8]| -> Result<String> { Ok(splitter.emit_key_base(main)?.key.finish()) };
    let same = key(&main)?;
    assert_eq!(key(&main)?, same, "the same start module gave another key");
    // One byte of the start module's exports, which is what a module
    // importing it is linked against.
    let mut changed = main.clone();
    let exports = start_exports(&main)?;
    changed[exports.start] ^= 1;
    assert_ne!(
        key(&changed)?,
        same,
        "a start module's exports are no part of a module's key"
    );
    Ok(())
}

/// The export section (7) of `module`, as a range of it.
fn start_exports(module: &[u8]) -> Result<Range<usize>> {
    let mut at = 8;
    while at < module.len() {
        let id = module[at];
        let mut contents = at + 1;
        let size = crate::split::read_leb(module, &mut contents)?;
        if id == 7 {
            return Ok(contents..contents + size);
        }
        at = contents + size;
    }
    anyhow::bail!("the module has no export section")
}

/// A split whose lazy exports moved to another module writes exactly what one
/// that emits every module writes, the start module too: its table's slots
/// are laid out from the split points, and a start module emitted for other
/// slots is never paired with these modules.
#[test]
fn a_changed_split_point_emits_the_start_module_again() -> Result<()> {
    let source = include_str!("../fixtures/data_split.rs");
    // `later_sum` answered by `reader`'s module instead of `later`'s: the
    // entries and the slots they take move, and what the start module
    // reaches does not (both are lazy exports before and after).
    let edited = source.replace(
        "static LAZY_EXPORTS: [u8; 33] = *b\"later later_*\\nreader reader_peek\\n\";",
        "static LAZY_EXPORTS: [u8; 52] = *b\"later later_mix\\nreader reader_peek\\nreader later_sum\\n\";",
    );
    assert_ne!(source, edited, "the fixture's lazy exports did not change");
    let before = build_fixture("split-point")?.linked;
    let after = build_fixture_from("split-point", &edited)?.linked;
    let (before_bound, after_bound) = (
        without_link_sections(&before)?,
        without_link_sections(&after)?,
    );
    let dir = scratch("split-point-cache");
    std::fs::remove_dir_all(&dir).ok();
    let result = (|| -> Result<()> {
        let fresh = |linked: &[u8], bound: &[u8]| -> Result<_> {
            Ok(written_modules(
                Splitter::new(linked, bound)?.emit_with(None)?,
            ))
        };
        let cached = |linked: &[u8], bound: &[u8]| -> Result<_> {
            Ok(written_modules(
                Splitter::new(linked, bound)?.emit_with(Some(&EmitCache::at(&dir)?))?,
            ))
        };
        let first = cached(&before, &before_bound)?;
        assert!(
            first == fresh(&before, &before_bound)?,
            "a split filling the cache wrote other modules"
        );
        let changed = cached(&after, &after_bound)?;
        assert_ne!(
            changed[0].2, first[0].2,
            "the moved split point did not reach the start module"
        );
        let expected = fresh(&after, &after_bound)?;
        assert!(
            changed == expected,
            "after a split point moved, the cache gave other modules than emitting does"
        );
        Ok(())
    })();
    std::fs::remove_dir_all(&dir).ok();
    result
}

/// A split whose input gained a function with a signature of its own writes
/// exactly what one that emits every module writes, and reads the modules the
/// function does not reach back: a module's types are the ones its own code
/// names, so a type added elsewhere moves no module's bytes, and the start
/// module's interface is the same.
#[test]
fn a_new_signature_reaches_no_other_module() -> Result<()> {
    let source = include_str!("../fixtures/data_split.rs");
    let changed = source
        .replace(
            "fn later_step(x: u32) -> u32 {",
            "fn later_scale(x: f32, y: f32) -> f32 {\n    x * y + 0.5\n}\n\n#[inline(never)]\nfn later_step(x: u32) -> u32 {\n    let scaled = later_scale(x as f32, 0.25) as u32;\n    let _ = scaled;",
        )
        .replace(
            "fn later_step(x: u32) -> u32 {\n    let scaled = later_scale(x as f32, 0.25) as u32;\n    let _ = scaled;\n    x.rotate_left(7) ^ 0x5bd1_e995\n}",
            "fn later_step(x: u32) -> u32 {\n    let scaled = later_scale(x as f32, 0.25) as u32;\n    (x.rotate_left(7) ^ 0x5bd1_e995) ^ scaled\n}",
        );
    assert_ne!(source, changed);
    let before = build_fixture("new-signature")?.linked;
    let after = build_fixture_from("new-signature", &changed)?.linked;
    let (before_bound, after_bound) = (
        without_link_sections(&before)?,
        without_link_sections(&after)?,
    );
    let dir = scratch("new-signature-cache");
    std::fs::remove_dir_all(&dir).ok();
    let result = (|| -> Result<()> {
        let fresh = |linked: &[u8], bound: &[u8]| -> Result<_> {
            Ok(written_modules(
                Splitter::new(linked, bound)?.emit_with(None)?,
            ))
        };
        let cached = |linked: &[u8], bound: &[u8]| -> Result<(_, usize, usize)> {
            let outputs = Splitter::new(linked, bound)?.emit_with(Some(&EmitCache::at(&dir)?))?;
            let reused = outputs.reused;
            let count = outputs.modules.len() + outputs.chunks.len() + 1;
            Ok((written_modules(outputs), reused, count))
        };
        let (first, reused, _) = cached(&before, &before_bound)?;
        assert_eq!(reused, 0, "an empty cache holds no module");
        assert!(first == fresh(&before, &before_bound)?);
        let (changed_output, reused, count) = cached(&after, &after_bound)?;
        assert!(
            changed_output == fresh(&after, &after_bound)?,
            "after a new signature, the cache gave other modules than emitting does"
        );
        assert!(
            reused > 0 && reused < count,
            "a function with a new signature emitted {} of {count} modules again",
            count - reused
        );
        Ok(())
    })();
    std::fs::remove_dir_all(&dir).ok();
    result
}

/// A split whose input gained a function elsewhere writes exactly what one
/// that emits every module writes, and reads back the modules the function
/// does not reach: a function added renumbers the module's index spaces, and
/// a key that held those numbers would emit every module for no change.
#[test]
fn a_split_reads_back_the_modules_a_new_function_does_not_reach() -> Result<()> {
    let source = include_str!("../fixtures/data_split.rs");
    // A function `later` calls, and `later_mix` with it: one module's code
    // moves, and the function count with it.
    let changed = source
        .replace(
            "fn later_step(x: u32) -> u32 {",
            "fn later_offset(x: u32) -> u32 {\n    x.wrapping_mul(0x9e37_79b9)\n}\n\n#[inline(never)]\nfn later_step(x: u32) -> u32 {",
        )
        .replace(
            "later_step(checksum(&START_BLOB[at..(at + 16).min(LEN)]))",
            "later_step(checksum(&START_BLOB[at..(at + 16).min(LEN)])) ^ later_offset(i)",
        );
    assert_ne!(source, changed);
    let before = build_fixture("added-function")?.linked;
    let after = build_fixture_from("added-function", &changed)?.linked;
    let (before_bound, after_bound) = (
        without_link_sections(&before)?,
        without_link_sections(&after)?,
    );
    let dir = scratch("added-function-cache");
    std::fs::remove_dir_all(&dir).ok();
    let result = (|| -> Result<()> {
        let fresh = |linked: &[u8], bound: &[u8]| -> Result<_> {
            Ok(written_modules(
                Splitter::new(linked, bound)?.emit_with(None)?,
            ))
        };
        let cached = |linked: &[u8], bound: &[u8]| -> Result<_> {
            let outputs = Splitter::new(linked, bound)?.emit_with(Some(&EmitCache::at(&dir)?))?;
            let reused = outputs.reused;
            let count = outputs.modules.len() + outputs.chunks.len() + 1;
            Ok((written_modules(outputs), reused, count))
        };
        let (first, reused, _) = cached(&before, &before_bound)?;
        assert_eq!(reused, 0, "an empty cache holds no module");
        assert!(
            first == fresh(&before, &before_bound)?,
            "a split filling the cache wrote other modules"
        );
        let (changed_output, reused, count) = cached(&after, &after_bound)?;
        assert!(
            changed_output == fresh(&after, &after_bound)?,
            "after a function was added, the cache gave other modules than emitting does"
        );
        assert!(
            changed_output != first,
            "the new function reached no module"
        );
        assert!(
            reused > 0 && reused < count,
            "a function added elsewhere emitted {} of {count} modules again",
            count - reused
        );
        Ok(())
    })();
    std::fs::remove_dir_all(&dir).ok();
    result
}

/// A split that reads the per-body analysis back (analysis.rs) writes exactly
/// what a split that reads every body writes: for the input it was filled
/// from, and after a change to one module's code.
#[test]
fn a_split_from_the_analysis_cache_writes_what_emitting_writes() -> Result<()> {
    let source = include_str!("../fixtures/data_split.rs");
    // A constant in `later_step`, which only the `later` module's code holds.
    let changed = source.replace("0x5bd1_e995", "0x5bd1_e996");
    assert_ne!(source, changed);
    let before = build_fixture("analysis")?.linked;
    let after = build_fixture_from("analysis", &changed)?.linked;
    let (before_bound, after_bound) = (
        without_link_sections(&before)?,
        without_link_sections(&after)?,
    );
    let dir = scratch("analysis-cache");
    std::fs::remove_dir_all(&dir).ok();
    let result = (|| -> Result<()> {
        let fresh = |linked: &[u8], bound: &[u8]| -> Result<_> {
            Ok(written_modules(
                Splitter::new(linked, bound)?.emit_with(None)?,
            ))
        };
        // The same directory for both caches, as a local build gives them.
        let cached = |linked: &[u8], bound: &[u8]| -> Result<_> {
            let analysis = AnalysisCache::at(&dir);
            let outputs = Splitter::with_analysis(linked, bound, Some(&analysis))?
                .emit_with(Some(&EmitCache::at(&dir)?))?;
            Ok(written_modules(outputs))
        };
        let first = cached(&before, &before_bound)?;
        assert!(
            first == fresh(&before, &before_bound)?,
            "a split filling the analysis cache wrote other modules"
        );
        assert!(
            cached(&before, &before_bound)? == first,
            "a split reading every body back wrote other modules"
        );
        let after_split = cached(&after, &after_bound)?;
        assert!(
            after_split == fresh(&after, &after_bound)?,
            "after a change, the analysis cache gave other modules than reading every body does"
        );
        assert!(after_split != first, "the change reached no module");
        // A cache filled from one module is never read for another whose
        // bodies are not the ones it holds: the structure says which bodies
        // the answers belong to.
        let other = build_fixture("analysis-other")?.linked;
        let other_bound = without_link_sections(&other)?;
        let dir_other = dir.join("other");
        let _ = AnalysisCache::at(&dir_other).store(&AnalysisCache::at(&dir).load().unwrap());
        let read =
            Splitter::with_analysis(&other, &other_bound, Some(&AnalysisCache::at(&dir_other)))?
                .emit_with(None)?;
        assert!(
            written_modules(read) == fresh(&other, &other_bound)?,
            "another module's answers were read for this one"
        );
        Ok(())
    })();
    std::fs::remove_dir_all(&dir).ok();
    result
}

/// A split whose functions were renumbered (a function added) reads last
/// run's plan and bodies through the renumbering the bind patch wrote, and
/// writes exactly what a split that makes both again writes.
#[test]
fn a_split_moved_through_a_renumbering_writes_what_emitting_writes() -> Result<()> {
    let source = include_str!("../fixtures/data_split.rs");
    let changed = source.replace(
        "fn later_step(x: u32) -> u32 {\n    x.rotate_left(7) ^ 0x5bd1_e995\n}",
        "fn later_offset(x: u32) -> u32 {\n    x.wrapping_mul(0x9e37_79b9)\n}\n\n#[inline(never)]\nfn later_step(x: u32) -> u32 {\n    (x.rotate_left(7) ^ 0x5bd1_e995) ^ later_offset(x)\n}",
    );
    assert_ne!(source, changed);
    let before = build_fixture("renumber")?.linked;
    let after = build_fixture_from("renumber", &changed)?.linked;
    let (before_bound, after_bound) = (
        without_link_sections(&before)?,
        without_link_sections(&after)?,
    );
    let dir = scratch("renumber-cache");
    std::fs::remove_dir_all(&dir).ok();
    let result = (|| -> Result<()> {
        let fresh = |linked: &[u8], bound: &[u8]| -> Result<_> {
            Ok(written_modules(
                Splitter::new(linked, bound)?.emit_with(None)?,
            ))
        };
        let plans = crate::plan::PlanCache::at(&dir);
        let analysis = AnalysisCache::at(&dir);
        // The run the module changed from: its plan and its bodies are kept.
        let mut splitter = Splitter::prepare(&before, &before_bound, Some(&analysis))?;
        splitter.finish(Some(&plans))?;
        let first = written_modules(splitter.emit_with(Some(&EmitCache::at(&dir)?))?);
        assert!(first == fresh(&before, &before_bound)?);
        // What the bind patch made of each function.
        let (old_imports, old_names) = function_names(&before_bound)?;
        let (new_imports, new_names) = function_names(&after_bound)?;
        assert!(!old_names.is_empty());
        let map = old_names
            .iter()
            .map(|name| {
                let mut found = new_names
                    .iter()
                    .enumerate()
                    .filter(|(_, new)| *new == name)
                    .map(|(index, _)| index as u32);
                let first = found.next()?;
                found.next().is_none().then_some(first)
            })
            .collect::<Vec<_>>();
        assert!(
            map.iter().filter(|target| target.is_some()).count() > old_names.len() / 2,
            "the fixtures' functions did not pair"
        );
        let linked = (0..new_imports + new_names.len() as u32)
            .map(Some)
            .collect::<Vec<_>>();
        let path = dir.join("renumber.bin");
        crate::analysis::write_renumbering(
            &path,
            &after_bound,
            old_imports,
            new_imports,
            &map,
            &linked,
        )?;
        let renumber = crate::analysis::Renumber::read_file(&path, &after_bound)
            .context("reading the renumbering")?;
        let mut splitter =
            Splitter::prepare_with(&after, &after_bound, Some(&analysis), Some(renumber))?;
        splitter.finish(Some(&plans))?;
        let moved = plans.moved();
        let outputs = splitter.emit_with(Some(&EmitCache::at(&dir)?))?;
        let bodies_reused = outputs.bodies_reused;
        let second = written_modules(outputs);
        assert!(moved > 0, "the plan was not moved through the renumbering");
        assert!(
            bodies_reused > 0,
            "no body was read back through the renumbering"
        );
        let expected = fresh(&after, &after_bound)?;
        for e in &expected {
            let got = second.iter().find(|module| module.0 == e.0);
            assert!(got.is_some(), "module {} missing", e.0);
            assert!(got.unwrap() == e, "module {} differs", e.0);
        }
        assert_eq!(second.len(), expected.len());
        assert!(second != first, "the change reached no module");
        Ok(())
    })();
    std::fs::remove_dir_all(&dir).ok();
    result
}

/// The linked module's imported function count, and the names of its own
/// functions in order: what a renumbering is made from.
fn function_names(bound: &[u8]) -> Result<(u32, Vec<String>)> {
    let mut imports = 0u32;
    let mut names = std::collections::HashMap::new();
    let mut locals = 0usize;
    for payload in wasmparser::Parser::new(0).parse_all(bound) {
        match payload? {
            Payload::ImportSection(section) => {
                for import in section {
                    if matches!(import?.ty, wasmparser::TypeRef::Func(_)) {
                        imports += 1;
                    }
                }
            }
            Payload::FunctionSection(section) => {
                for entry in section {
                    let _ = entry?;
                    locals += 1;
                }
            }
            Payload::CustomSection(section) if section.name() == "name" => {
                let reader = wasmparser::NameSectionReader::new(wasmparser::BinaryReader::new(
                    section.data(),
                    section.data_offset(),
                ));
                for part in reader {
                    if let wasmparser::Name::Function(map) = part? {
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
    let locals = (0..locals)
        .map(|local| {
            names
                .get(&(imports + local as u32))
                .cloned()
                .unwrap_or_else(|| format!("function {local}"))
        })
        .collect();
    Ok((imports, locals))
}

/// A split that takes the plan an earlier one made (plan.rs) writes exactly
/// what a split that makes it again writes: for the module it was filled
/// from, after a change to one body's code alone (the plan holds, that body's
/// module is emitted again), and after a change to what a body computes (the
/// plan is made again).
#[test]
fn a_split_from_the_plan_cache_writes_what_emitting_writes() -> Result<()> {
    let source = include_str!("../fixtures/data_split.rs");
    // `checksum` mixes its sum with a byte by `+` instead of `^`: it calls
    // what it called and holds the addresses it held, so the plan it was part
    // of is unchanged, and the start module that holds it is not.
    let edited = source.replace(
        "sum.rotate_left(5) ^ u32::from(*byte)",
        "sum.rotate_left(5).wrapping_add(u32::from(*byte))",
    );
    assert_ne!(
        source, edited,
        "the fixture no longer mixes a byte with `^`"
    );
    // `double` multiplies by 3 instead of 2: one immediate more, and a plan
    // that is another plan.
    let moved = source.replace("x.wrapping_mul(2)", "x.wrapping_mul(3)");
    assert_ne!(source, moved, "the fixture no longer doubles by 2");
    // Built at one path: the module's static data holds the source's path.
    let before = build_fixture("plan-cache")?.linked;
    let after = build_fixture_from("plan-cache", &edited)?.linked;
    let other = build_fixture_from("plan-cache", &moved)?.linked;
    let (before_bound, after_bound, other_bound) = (
        without_link_sections(&before)?,
        without_link_sections(&after)?,
        without_link_sections(&other)?,
    );
    let dir = scratch("plan-cache");
    std::fs::remove_dir_all(&dir).ok();
    let result = (|| -> Result<()> {
        let plans = crate::plan::PlanCache::at(&dir);
        let fresh = |linked: &[u8], bound: &[u8]| -> Result<_> {
            Ok(written_modules(
                Splitter::new(linked, bound)?.emit_with(None)?,
            ))
        };
        // Both caches in one directory, as a local build gives them.
        let cached = |linked: &[u8], bound: &[u8]| -> Result<_> {
            let analysis = AnalysisCache::at(&dir);
            let mut splitter = Splitter::prepare(linked, bound, Some(&analysis))?;
            splitter.finish(Some(&plans))?;
            Ok(written_modules(
                splitter.emit_with(Some(&EmitCache::at(&dir)?))?,
            ))
        };
        let first = cached(&before, &before_bound)?;
        assert_eq!(plans.taken(), 0, "an empty cache holds no plan");
        assert!(
            first == fresh(&before, &before_bound)?,
            "the split that filled the plan wrote other modules than emitting does"
        );
        let again = cached(&before, &before_bound)?;
        assert_eq!(plans.taken(), 1, "the same module read no plan back");
        assert!(
            again == first,
            "the module read back from the plan differs from the one emitted"
        );
        let edited = cached(&after, &after_bound)?;
        assert_eq!(
            plans.taken(),
            2,
            "a change that moved no answer made the plan again"
        );
        assert!(
            edited == fresh(&after, &after_bound)?,
            "after a change, the plan gave other modules than making it does"
        );
        assert!(edited != first, "the change reached no module");
        let moved = cached(&other, &other_bound)?;
        assert_eq!(
            plans.taken(),
            2,
            "a change to what a body computes read the plan back"
        );
        assert!(
            moved == fresh(&other, &other_bound)?,
            "a changed plan gave other modules than making it does"
        );
        Ok(())
    })();
    std::fs::remove_dir_all(&dir).ok();
    result
}

/// A local variable's name is no part of what a split writes: the modules of
/// a module whose only change is one are the same bytes.
#[test]
fn a_local_variables_name_reaches_no_module() -> Result<()> {
    let source = include_str!("../fixtures/data_split.rs");
    let renamed = source
        .replace(
            "let listed = START_TABLE[(i % 2) as usize];",
            "let table_slice = START_TABLE[(i % 2) as usize];",
        )
        .replace(
            "u32::from(listed[at % listed.len()])",
            "u32::from(table_slice[at % table_slice.len()])",
        );
    assert_ne!(
        source, renamed,
        "the fixture no longer names a local `listed`"
    );
    let before = build_fixture("local-name")?.linked;
    let after = build_fixture_from("local-name", &renamed)?.linked;
    let (before_bound, after_bound) = (
        without_link_sections(&before)?,
        without_link_sections(&after)?,
    );
    let split = |linked: &[u8], bound: &[u8]| -> Result<_> {
        Ok(written_modules(
            Splitter::new(linked, bound)?.emit_with(None)?,
        ))
    };
    let (one, two) = (split(&before, &before_bound)?, split(&after, &after_bound)?);
    eprintln!("modules equal: {}", one == two);
    Ok(())
}

/// A change to the module's static data alone leaves every module's code as
/// it was, so a split reads the modules that do not carry those bytes back
/// instead of emitting them again, and writes what emitting them writes.
#[test]
fn a_change_to_static_data_alone_reads_the_modules_back() -> Result<()> {
    let source = include_str!("../fixtures/data_split.rs");
    let before = build_fixture("data-change")?.linked;
    // One byte of a blob only `reader` reaches: its code, and every other
    // module's, is the same.
    let mut edits = HashMap::new();
    edits.insert("reader", (0usize, 0x5a_u8));
    let after = build_fixture_edited("data-change", source, &edits)?.linked;
    assert_ne!(before, after, "the blob's byte did not reach the module");
    let (before_bound, after_bound) = (
        without_link_sections(&before)?,
        without_link_sections(&after)?,
    );
    let dir = scratch("data-change-cache");
    std::fs::remove_dir_all(&dir).ok();
    let result = (|| -> Result<()> {
        let fresh = |linked: &[u8], bound: &[u8]| -> Result<_> {
            Ok(written_modules(
                Splitter::new(linked, bound)?.emit_with(None)?,
            ))
        };
        let cached = |linked: &[u8], bound: &[u8]| -> Result<_> {
            let outputs = Splitter::new(linked, bound)?.emit_with(Some(&EmitCache::at(&dir)?))?;
            let reused = outputs.reused;
            let count = outputs.modules.len() + outputs.chunks.len();
            Ok((written_modules(outputs), reused, count))
        };
        let (first, reused, _) = cached(&before, &before_bound)?;
        assert_eq!(reused, 0, "an empty cache holds no module");
        assert!(
            first == fresh(&before, &before_bound)?,
            "a split filling the cache wrote other modules"
        );
        let (changed, reused, count) = cached(&after, &after_bound)?;
        assert!(
            changed == fresh(&after, &after_bound)?,
            "after a data change, the cache gave other modules than emitting does"
        );
        assert!(changed != first, "the change reached no module");
        assert!(
            reused > 0 && reused < count,
            "a change to one static's bytes emitted {} of {count} modules again",
            count - reused
        );
        // The bodies' analysis belongs to their bytes: a change to a static
        // the code reads by address leaves every body as it was.
        let analysis = AnalysisCache::at(&dir);
        let first =
            Splitter::with_analysis(&before, &before_bound, Some(&analysis))?.emit_with(None)?;
        assert_eq!(
            first.bodies_reused, 0,
            "an empty analysis cache held bodies"
        );
        let second =
            Splitter::with_analysis(&after, &after_bound, Some(&analysis))?.emit_with(None)?;
        assert!(
            second.bodies_reused > 0,
            "a change to static data alone walked every body again"
        );
        Ok(())
    })();
    std::fs::remove_dir_all(&dir).ok();
    result
}

/// A cache file a stopped build left empty, and a cache directory a stopped
/// build removed, cost the module written again, never the split.
#[test]
fn a_cache_a_stopped_build_broke_is_made_again() -> Result<()> {
    let fixture = build_fixture("broke")?;
    let linked = &fixture.linked;
    let dir = scratch("broke-cache");
    std::fs::remove_dir_all(&dir).ok();
    let result = (|| -> Result<()> {
        let fresh = written_modules(Splitter::new(linked, linked)?.emit_with(None)?);
        let cached = |dir: &std::path::Path| -> Result<_> {
            let outputs = Splitter::new(linked, linked)?.emit_with(Some(&EmitCache::at(dir)?))?;
            let reused = outputs.reused;
            let count = outputs.modules.len() + outputs.chunks.len();
            Ok((written_modules(outputs), reused, count))
        };
        let (first, reused, count) = cached(&dir)?;
        assert_eq!(reused, 0, "an empty cache holds no module");
        assert!(
            first == fresh,
            "a split filling the cache wrote other modules"
        );
        let (again, reused, _) = cached(&dir)?;
        assert_eq!(reused, count, "the second split read nothing back");
        assert!(again == fresh, "a split from the cache wrote other modules");
        // The directory a stopped build's cleanup removed is made again for
        // the module this split writes.
        std::fs::remove_dir_all(&dir)?;
        let (remade, _, count) = cached(&dir)?;
        assert!(
            remade == fresh,
            "a split into a removed cache wrote other modules"
        );
        assert!(count > 0);
        // An entry left empty is no module: it is emitted again rather than
        // failed on.
        let mut entries: Vec<_> = std::fs::read_dir(&dir)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<_>>()?;
        entries.retain(|path| {
            path.extension()
                .is_some_and(|extension| extension == "wasm")
        });
        assert!(!entries.is_empty(), "the split kept no module");
        std::fs::write(&entries[0], b"")?;
        let (after, reused, count) = cached(&dir)?;
        assert_eq!(reused, count - 1, "an empty entry was read as a module");
        assert!(
            after == fresh,
            "the module made again after an empty entry differs"
        );
        Ok(())
    })();
    std::fs::remove_dir_all(&dir).ok();
    result
}

/// A split leaves what another split beside it is still writing alone: its
/// cache directory holds one file that is not a finished module.
#[test]
fn a_split_being_written_beside_this_one_is_not_pruned() -> Result<()> {
    let dir = scratch("prune");
    std::fs::remove_dir_all(&dir).ok();
    let result = (|| -> Result<()> {
        let cache = EmitCache::at(&dir)?;
        cache.put("kept", b"module")?;
        // Another split's module, half written, and the analysis beside them.
        std::fs::write(dir.join(".writing.1234.wasm"), b"half")?;
        std::fs::write(dir.join("analysis.bin"), b"analysis")?;
        // A module from an older split that this one did not ask for.
        std::fs::write(dir.join("gone.wasm"), b"old")?;
        cache.prune()?;
        assert!(dir.join("kept.wasm").exists(), "a used module was pruned");
        assert!(
            dir.join(".writing.1234.wasm").exists(),
            "a half-written module was pruned"
        );
        assert!(dir.join("analysis.bin").exists(), "the analysis was pruned");
        assert!(
            !dir.join("gone.wasm").exists(),
            "a module this split did not ask for was kept"
        );
        // A directory that went away holds nothing to prune.
        std::fs::remove_dir_all(&dir)?;
        cache.prune()?;
        Ok(())
    })();
    std::fs::remove_dir_all(&dir).ok();
    result
}

/// The same linked input splits into the same bytes every time, so web
/// bundles are reproducible and their caches stay valid.
#[test]
fn splitting_twice_emits_identical_modules() -> Result<()> {
    let fixture = build_fixture("deterministic")?;
    let linked = &fixture.linked;
    let emit = || -> Result<Vec<(String, Vec<u8>)>> {
        let outputs = Splitter::new(linked, linked)?.emit()?;
        let mut modules = vec![("main".to_string(), outputs.main.bytes)];
        for module in outputs.modules {
            modules.push((module.module_name, module.bytes));
        }
        Ok(modules)
    };
    let first = emit()?;
    for _ in 0..4 {
        let again = emit()?;
        assert_eq!(
            first.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            again.iter().map(|(name, _)| name).collect::<Vec<_>>()
        );
        for ((name, want), (_, got)) in first.iter().zip(&again) {
            assert!(want == got, "split module {name} differs between runs");
        }
    }
    Ok(())
}

#[test]
fn static_data_moves_with_the_only_module_that_reaches_it() -> Result<()> {
    let fixture = build_fixture("placement")?;
    let linked = &fixture.linked;
    let outputs = Splitter::new(linked, linked)?.emit()?;
    let start = segments(&outputs.main.bytes)?;
    let reader = segments(split_module(&outputs, "reader"))?;
    let tabler = segments(split_module(&outputs, "tabler"))?;
    let initial = segments(linked)?;

    // Each static, and the module that should write it.
    let expected = [
        ("READER_BLOB", "reader"),
        ("READER_STATE", "reader"),
        ("TABLE_BLOB", "tabler"),
        ("TABLE", "tabler"),
        ("SHARED_BLOB", "start"),
        ("LISTED_BLOB", "start"),
        ("START_BLOB", "start"),
        ("START_TABLE", "start"),
    ];
    for (name, owner) in expected {
        let range = static_range(linked, name)?;
        let want = written(&initial, &range);
        assert!(
            want.iter().all(Option::is_some),
            "{name} is not in the linked module's data"
        );
        for (module, segments) in [("start", &start), ("reader", &reader), ("tabler", &tabler)] {
            let got = written(segments, &range);
            if module == owner {
                // A split module leaves out longer stretches of zeros: the
                // memory holds zeros there until it is written.
                let got: Vec<_> = got
                    .iter()
                    .map(|byte| byte.or((module != "start").then_some(0)))
                    .collect();
                assert_eq!(got, want, "{module} should write all of {name}, unchanged");
                if module != "start" {
                    assert!(got.iter().any(|byte| *byte != Some(0)));
                }
            } else {
                assert!(
                    got.iter().all(Option::is_none),
                    "{module} writes part of {name}, which is {owner}'s"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn split_modules_initialise_their_data_before_their_code_runs() -> Result<()> {
    use wasmi::{Engine, Extern, Instance, Linker, Module, Store};

    let fixture = build_fixture("run")?;
    let blobs = &fixture.blobs;
    let linked = &fixture.linked;
    let outputs = Splitter::new(linked, linked)?.emit()?;

    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let main = Linker::<()>::new(&engine)
        .instantiate_and_start(&mut store, &Module::new(&engine, &outputs.main.bytes)?)?;
    let memory = main
        .get_memory(&store, "memory")
        .context("the start module exports no memory")?;
    let call = |store: &mut Store<()>, name: &str, i: u32| -> Result<u32> {
        Ok(main
            .get_typed_func::<u32, u32>(&*store, name)?
            .call(store, i)?)
    };
    // `__wasm_split.js`: the start module's exports are the split module's
    // imports.
    let load = |store: &mut Store<()>, bytes: &[u8]| -> Result<Instance> {
        let module = Module::new(&engine, bytes)?;
        let mut linker = Linker::<()>::new(&engine);
        for import in module.imports() {
            let export: Extern = main
                .get_export(&*store, import.name())
                .with_context(|| format!("the start module does not export {}", import.name()))?;
            linker.define(import.module(), import.name(), export)?;
        }
        Ok(linker.instantiate_and_start(store, &module)?)
    };
    let bytes_at = |store: &Store<()>, name: &str| -> Result<Vec<u8>> {
        let range = static_range(linked, name)?;
        Ok(memory.data(store)[range.start as usize..range.end as usize].to_vec())
    };
    let samples = [0u32, 1, 7, 99, 1234, 65_537, 0x7fff_ffff];

    // The start module reads its own data, and `LISTED_BLOB` through its
    // table, before any split module is in.
    for i in samples {
        let at = offset(i);
        let listed = if i % 2 == 0 {
            &blobs["listed"][..64]
        } else {
            &blobs["listed"][64..]
        };
        let op = if i % 2 == 0 {
            i.wrapping_mul(2)
        } else {
            i.wrapping_mul(3)
        };
        let want = u32::from(blobs["start"][at])
            | u32::from(listed[at % listed.len()]) << 8
            | (checksum(&blobs["start"][at..(at + 16).min(LEN)]) & 0xff) << 16
            | (op & 0xff) << 24;
        assert_eq!(call(&mut store, "start_read", i)?, want, "start_read({i})");
    }

    // Before `reader` loads, its data is not in memory and its code is not
    // callable.
    assert!(
        bytes_at(&store, "READER_BLOB")?
            .iter()
            .all(|byte| *byte == 0)
    );
    assert!(call(&mut store, "call_reader", 1).is_err());

    load(&mut store, split_module(&outputs, "reader"))?;
    assert_eq!(bytes_at(&store, "READER_BLOB")?, blobs["reader"]);
    for i in samples {
        let at = offset(i);
        let want = u32::from(blobs["reader"][at])
            | u32::from(blobs["shared"][at]) << 8
            | u32::from(blobs["listed"][at]) << 16
            | (checksum(&blobs["reader"][at..(at + 16).min(LEN)]) & 0xff) << 24;
        assert_eq!(call(&mut store, "call_reader", i)?, want, "reader({i})");
    }
    // The mutable static starts at its initial value and keeps what the
    // module writes to it.
    let step = 0x8000_0000 | 14;
    assert_eq!(call(&mut store, "call_reader", step)?, 0x1234_5679);
    assert_eq!(call(&mut store, "call_reader", step)?, 0x1234_567a);

    // `tabler` reaches `TABLE_BLOB` only through `TABLE`.
    assert!(
        bytes_at(&store, "TABLE_BLOB")?
            .iter()
            .all(|byte| *byte == 0)
    );
    assert!(call(&mut store, "call_tabler", 1).is_err());
    load(&mut store, split_module(&outputs, "tabler"))?;
    assert_eq!(bytes_at(&store, "TABLE_BLOB")?, blobs["table"]);
    for i in samples {
        let k = (i % 6) as usize;
        let slice = &blobs["table"][TABLE_BOUNDS[k]..TABLE_BOUNDS[k + 1]];
        let op = i.wrapping_add((i % 8 + 1) * 0x11);
        let want = u32::from(slice[offset(i) % slice.len()])
            | u32::from(blobs["shared"][offset(i)]) << 8
            | (op & 0xff) << 16;
        assert_eq!(call(&mut store, "call_tabler", i)?, want, "tabler({i})");
    }
    // Loading `tabler` left `reader`'s state alone.
    assert_eq!(call(&mut store, "call_reader", step)?, 0x1234_567b);
    // `reader`'s module also answers its lazy export.
    for i in samples {
        assert_eq!(
            call(&mut store, "reader_peek", i)?,
            u32::from(blobs["reader"][offset(i)]),
            "reader_peek({i})"
        );
    }
    Ok(())
}

/// Exports named in `tilo_split_exports` stay exports of the start module,
/// answered by their module once it is in.
#[test]
fn lazy_exports_answer_once_their_module_is_in() -> Result<()> {
    use wasmi::{Engine, Extern, Linker, Module, Store};

    let fixture = build_fixture("lazy")?;
    let linked = &fixture.linked;
    let outputs = Splitter::new(linked, linked)?.emit()?;
    let step = |x: u32| x.rotate_left(7) ^ 0x5bd1_e995;

    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let main = Linker::<()>::new(&engine)
        .instantiate_and_start(&mut store, &Module::new(&engine, &outputs.main.bytes)?)?;
    let call = |store: &mut Store<()>, name: &str, i: u32| -> Result<u32> {
        Ok(main
            .get_typed_func::<u32, u32>(&*store, name)?
            .call(store, i)?)
    };
    assert!(call(&mut store, "later_sum", 3).is_err());
    assert!(call(&mut store, "later_mix", 3).is_err());

    let module = Module::new(&engine, split_module(&outputs, "later"))?;
    let mut linker = Linker::<()>::new(&engine);
    for import in module.imports() {
        let export: Extern = main
            .get_export(&store, import.name())
            .with_context(|| format!("the start module does not export {}", import.name()))?;
        linker.define(import.module(), import.name(), export)?;
    }
    linker.instantiate_and_start(&mut store, &module)?;
    for i in [0u32, 1, 7, 99, 0x7fff_ffff] {
        assert_eq!(
            call(&mut store, "later_sum", i)?,
            step(i).wrapping_add(step(i ^ 1))
        );
        let at = offset(i);
        let want = step(checksum(&fixture.blobs["start"][at..(at + 16).min(LEN)]));
        assert_eq!(call(&mut store, "later_mix", i)?, want, "later_mix({i})");
    }
    Ok(())
}

/// The start module's holes: a slot a split module fills is never one the
/// start module writes.
mod table_holes {
    use crate::split::leave_holes_empty;
    use walrus::{ConstExpr, ElementItems, ElementKind, FunctionBuilder, Module, ir};

    /// Each table slot's function in `module`'s active segments, `None`
    /// where no segment writes one.
    fn slots(module: &Module, len: usize) -> Vec<Option<walrus::FunctionId>> {
        let mut slots = vec![None; len];
        for element in module.elements.iter() {
            let (
                ElementKind::Active {
                    offset: ConstExpr::Value(ir::Value::I32(offset)),
                    ..
                },
                ElementItems::Functions(items),
            ) = (&element.kind, &element.items)
            else {
                panic!("an unexpected segment");
            };
            for (at, item) in items.iter().enumerate() {
                slots[*offset as usize + at] = Some(*item);
            }
        }
        slots
    }

    #[test]
    fn the_start_module_writes_no_slot_a_split_module_fills() {
        let mut module = Module::default();
        let table = module
            .tables
            .add_local(false, 32, Some(32), walrus::RefType::Funcref);
        let mut function = |name: &str| {
            let mut builder = FunctionBuilder::new(&mut module.types, &[], &[]);
            builder.name(name.into()).func_body().unreachable();
            builder.finish(vec![], &mut module.funcs)
        };
        let (hole, a, b) = (function("hole"), function("a"), function("b"));
        // Slot 1 on: a, a hole, a, five holes, b, b, then holes to the end.
        let items = vec![a, hole, a, hole, hole, hole, hole, hole, b, b, hole, hole];
        let element = module.elements.add(
            ElementKind::Active {
                table,
                offset: ConstExpr::Value(ir::Value::I32(1)),
            },
            ElementItems::Functions(items.clone()),
        );
        module.tables.get_mut(table).elem_segments.insert(element);

        leave_holes_empty(&mut module, hole);

        let mut want = vec![None; 32];
        for (at, item) in items.iter().enumerate() {
            want[1 + at] = Some(*item);
        }
        // The five holes in a row and the trailing ones are left empty; the
        // single one between two functions is cheaper to write.
        for at in [4, 5, 6, 7, 8, 11, 12] {
            want[at] = None;
        }
        assert_eq!(slots(&module, 32), want);
        assert_eq!(module.tables.get(table).elem_segments.len(), 2);
    }
}
