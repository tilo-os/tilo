//! Splits fixtures/data_split.rs, compiled for wasm32 as the desktop bundle
//! is linked, and runs the pieces in wasmi the way `__wasm_split.js` links
//! them: the start module first, a split module into its memory and table
//! when its split point is first called.

use std::collections::HashMap;
use std::ops::Range;
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};
use wasmparser::{Linking, LinkingSectionReader, Payload, SymbolInfo};

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

/// Compile the fixture with the toolchain running the tests, in a
/// directory of its own per test (`test`).
fn build_fixture(test: &str) -> Result<Fixture> {
    let dir = std::env::temp_dir().join(format!("wasm-split-{test}-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let result = (|| {
        let source = dir.join("data_split.rs");
        std::fs::write(&source, include_str!("../fixtures/data_split.rs"))?;
        let mut blobs = HashMap::new();
        for (seed, name) in BLOBS.iter().enumerate() {
            let bytes = blob(seed as u32 + 1);
            std::fs::write(dir.join(format!("{name}.bin")), &bytes)?;
            blobs.insert(*name, bytes);
        }
        let output = dir.join("data_split.wasm");
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let status = Command::new(rustc)
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
            .arg(&output)
            .arg(&source)
            .status()
            .context("running rustc")?;
        ensure!(
            status.success(),
            "compiling the fixture failed (it needs the wasm32-unknown-unknown target)"
        );
        Ok(Fixture {
            linked: std::fs::read(&output)?,
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
        let want =
            u32::from(slice[offset(i) % slice.len()]) | u32::from(blobs["shared"][offset(i)]) << 8;
        assert_eq!(call(&mut store, "call_tabler", i)?, want, "tabler({i})");
    }
    // Loading `tabler` left `reader`'s state alone.
    assert_eq!(call(&mut store, "call_reader", step)?, 0x1234_567b);
    Ok(())
}
