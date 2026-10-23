//! Files' archive codecs, as an engine library beside the shared runtime
//! (`libtilo_archive_engine.so`).
//!
//! Browsing into an archive, extracting it and packing a selection need the
//! tar, zip and 7z readers and the gzip, xz, zstd and bzip2 codecs. Only
//! Files reaches them, so none belongs in the runtime every app maps, and
//! linked into Files' tapp they would grow the app past its size budget.
//! Here they are a library of their own in the runtime bundle that only
//! Files names `DT_NEEDED`.
//!
//! Like `runtime-dylib`, this crate re-exports the crate it provides, which
//! records it as provided by this library for the apps' `-C prefer-dynamic`
//! link. Outside the thin build (`thin`) it is empty: see the manifest.

// Load the runtime explicitly, as thin apps do: rustc ignores an `--extern`
// nothing names, and without it in the crate graph this library would link
// its own copy of every crate the runtime already provides, which the app's
// link then refuses (each crate may show up once).
#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(feature = "thin")]
pub use tilo_archive;

/// Run the engine's own code: pack a file into a tar.zst, list the archive
/// and extract it, checking what comes back.
///
/// The engine's check binary calls it, so a release proves the library loads
/// and runs against the runtime it ships with.
///
/// # Errors
/// Names the step that failed or what was read back wrongly.
#[cfg(feature = "thin")]
pub fn self_check() -> Result<String, String> {
    let scratch =
        std::env::temp_dir().join(format!("tilo-archive-engine-check-{}", std::process::id()));
    let result = check_in(&scratch);
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

#[cfg(feature = "thin")]
fn fail(step: &'static str) -> impl Fn(std::io::Error) -> String {
    move |error| format!("{step}: {error}")
}

#[cfg(feature = "thin")]
fn check_in(scratch: &std::path::Path) -> Result<String, String> {
    let source = scratch.join("hello.txt");
    std::fs::create_dir_all(scratch).map_err(fail("making the scratch folder"))?;
    std::fs::write(&source, b"hello from the archive engine")
        .map_err(fail("writing the file to pack"))?;
    let archive = scratch.join("check.tar.zst");
    let out = std::fs::File::create_new(&archive).map_err(fail("making the archive"))?;
    tilo_archive::create(&[source], out, tilo_archive::Packing::TarZst, &())
        .map_err(fail("packing"))?;
    let entries = tilo_archive::list(&archive, &()).map_err(fail("listing"))?;
    if entries.len() != 1 {
        return Err(format!("{} entries listed, not 1", entries.len()));
    }
    let unpacked = scratch.join("out");
    std::fs::create_dir(&unpacked).map_err(fail("making the destination"))?;
    let done = tilo_archive::extract_into(&archive, &unpacked, &()).map_err(fail("extracting"))?;
    let read = std::fs::read(unpacked.join("hello.txt")).map_err(fail("reading back"))?;
    if read != b"hello from the archive engine" {
        return Err("the extracted file differs from the packed one".into());
    }
    Ok(format!(
        "packed, listed and extracted {} file of {} bytes",
        done.files, done.bytes
    ))
}
