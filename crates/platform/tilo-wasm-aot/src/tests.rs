//! Compiled modules: the entry's seal, the cache's exact key, and a runtime deserializing only
//! what this build's compiler made for the module it holds.

use super::*;

fn module() -> Vec<u8> {
    wat::parse_str(r#"(module (func (export "answer") (result i32) (i32.const 42)))"#).unwrap()
}

#[test]
fn an_entry_opens_only_for_the_module_it_was_made_for() {
    let wasm = module();
    let entry = seal(&wasm, b"compiled bytes");
    let opened = open(&entry).unwrap();
    assert_eq!(opened.serialized, b"compiled bytes");
    assert_eq!(opened.module, *blake3::hash(&wasm).as_bytes());
    assert_eq!(open_for(&entry, &wasm).unwrap(), b"compiled bytes");

    // Another module's entry is refused, whatever its body says.
    let other =
        wat::parse_str(r#"(module (func (export "answer") (result i32) (i32.const 7)))"#).unwrap();
    assert!(open_for(&entry, &other).is_err());

    // A body that does not match its digest is refused.
    let mut damaged = entry.clone();
    let last = damaged.len() - 1;
    damaged[last] ^= 0xff;
    assert_eq!(open(&damaged), Err("its digest does not match"));
    assert_eq!(open(b"not an entry"), Err("not a compiled module"));
}

#[cfg(feature = "compile")]
#[test]
fn the_cache_names_an_entry_by_its_module_and_engine() {
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::at(dir.path()).unwrap();
    let wasm = module();
    let name = cache.name(&wasm);
    assert!(name.starts_with(&format!("{}-", blake3::hash(&wasm).to_hex())));
    assert!(
        std::path::Path::new(&name)
            .extension()
            .is_some_and(|extension| extension == "cwasm")
    );
    // The engine's part of the name is the one wasmtime checks: two engines of one
    // configuration agree, and the entry is read back from disk by the second.
    let again = Cache::at(dir.path()).unwrap();
    assert_eq!(again.name(&wasm), name);
}

#[cfg(feature = "compile")]
#[test]
fn the_daemons_entry_deserializes_in_a_runtime_with_no_compiler() {
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::at(dir.path()).unwrap();
    let wasm = module();
    let entry = cache.ensure(&wasm).unwrap();
    let serialized = open_for(&entry, &wasm).unwrap();

    let runtime = runtime_engine().unwrap();
    // SAFETY: the entry is checked to be the one made for `wasm` by this build's
    // configuration; wasmtime checks the version, configuration and target again when it
    // deserializes it.
    let module = unsafe { wasmtime::Module::deserialize(&runtime, serialized) }.unwrap();
    assert!(module.exports().any(|export| export.name() == "answer"));

    // The second ask reads the entry the first wrote, rather than compiling again.
    assert_eq!(cache.ensure(&wasm).unwrap(), entry);
    assert_eq!(cache.kept(&wasm).unwrap(), entry);
}

#[cfg(feature = "compile")]
#[test]
fn an_entry_is_refused_by_an_engine_of_another_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::at(dir.path()).unwrap();
    let wasm = module();
    let entry = cache.ensure(&wasm).unwrap();
    let serialized = open_for(&entry, &wasm).unwrap();

    let mut other = config();
    other.consume_fuel(false);
    let runtime = wasmtime::Engine::new(&other).unwrap();
    // SAFETY: the bytes are a sealed entry, but of another configuration: wasmtime must refuse
    // them, which is what the key protects.
    assert!(unsafe { wasmtime::Module::deserialize(&runtime, serialized) }.is_err());
}

#[cfg(feature = "compile")]
#[test]
fn the_cache_compiles_parts_side_by_side_and_reports_a_bad_one() {
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::at(dir.path()).unwrap();
    let first = module();
    let second = wat::parse_str(r#"(module (func (export "b")))"#).unwrap();
    let broken = b"not wasm".to_vec();
    let entries = cache.ensure_all(&[&first, &second, &broken]);
    assert!(entries[0].is_ok());
    assert!(entries[1].is_ok());
    assert!(entries[2].is_err());
    assert_ne!(entries[0].as_ref().unwrap(), entries[1].as_ref().unwrap());
}

#[cfg(feature = "compile")]
#[test]
fn a_damaged_entry_is_compiled_again() {    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::at(dir.path()).unwrap();
    let wasm = module();
    let entry = cache.ensure(&wasm).unwrap();
    let path = dir.path().join(cache.name(&wasm));
    let mut damaged = entry.clone();
    let last = damaged.len() - 1;
    damaged[last] ^= 0xff;
    std::fs::write(&path, &damaged).unwrap();
    // The damaged entry is not used: the module is compiled again and a whole entry written.
    assert_eq!(cache.ensure(&wasm).unwrap(), entry);
    assert_eq!(std::fs::read(&path).unwrap(), entry);
}

/// A module of about the size an app's is: many small exported functions, which is what a
/// compiled program is made of.
#[cfg(feature = "compile")]
fn app_sized_module() -> Vec<u8> {
    let mut text = String::from("(module (memory (export \"memory\") 16)");
    for index in 0..1_200 {
        use std::fmt::Write as _;
        let _ = write!(
            text,
            "(func (export \"f{index}\") (param i32) (result i32) \
             (i32.add (local.get 0) (i32.const {index})))"
        );
    }
    text.push(')');
    wat::parse_str(text).unwrap()
}

/// The first open after an install: what the daemon's open path pays for a module the install
/// compiled (a cache read) beside one it did not (a compile).
///
/// The install compiles the modules of the package it installs, so an app already on the device
/// opens without compiling anything: the daemon's handover finds its entry and reads it. Before
/// that hook, the first open compiled every part on the handover that opened it.
#[cfg(feature = "compile")]
#[test]
fn an_install_writes_what_the_next_open_reads() {
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::at(dir.path()).unwrap();
    let wasm = app_sized_module();

    // Before: the module the install did not compile is compiled on the open that first runs it.
    let began = std::time::Instant::now();
    let entry = cache.ensure(&wasm).unwrap();
    let uncompiled = began.elapsed();

    // After: the install wrote the entry, and the open reads it.
    let began = std::time::Instant::now();
    let read = cache.ensure(&wasm).unwrap();
    let cached = began.elapsed();
    assert_eq!(read, entry);

    let began = std::time::Instant::now();
    let kept = kept_in(dir.path(), &wasm).expect("the install's entry is in the store");
    let lookup = began.elapsed();
    assert_eq!(kept, entry);

    println!(
        "\nfirst open after install, {}-byte module: compiled on the open {uncompiled:?}, \
         cache read {cached:?}, read-only lookup {lookup:?}\n",
        wasm.len()
    );
}
