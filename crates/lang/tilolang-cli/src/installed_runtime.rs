//! The tilolang runtime of the Tilo installed on this computer: what `tilolang dev` runs a
//! package's window on when no Tilo checkout is there to build one from.
//!
//! An installed Tilo keeps its runtime bundles where its launcher looks for them
//! ([`runtime_bundle::resolve_bundle_path`]): the person's own (`$TILO_RUNTIME_BUNDLE_DIR`, else
//! `$XDG_DATA_HOME/tilo-runtime`), then the system's (`/usr/lib/tilo-runtime`, which a Tilo OS
//! image bakes). The tilolang runtime bundle is one directory there, named by the version every
//! package this tool builds names ([`crate::package::DEFAULT_TILOLANG_RUNTIME`]), and its one
//! binary is the window. That binary is thin: it links the toolkit and `libstd` from the Tilo
//! runtime bundle beside it, whose `lib/` is where the window's process finds them.

use std::path::{Path, PathBuf};

use crate::package::{DEFAULT_TILOLANG_RUNTIME, RUNTIME_BIN};

/// The installed tilolang runtime: the window's program, and the directories it finds the shared
/// libraries it links in.
#[derive(Debug)]
pub(crate) struct Installed {
    pub(crate) binary: PathBuf,
    pub(crate) loaders: Vec<PathBuf>,
}

/// The tilolang runtime bundle the Tilo installed here offers, or why there is none: where it
/// looked, and what to do instead.
pub(crate) fn find() -> Result<Installed, String> {
    let version = runtime_version();
    let Some(bundle) = runtime_bundle::resolve_bundle_path(version) else {
        return Err(not_installed(version));
    };
    let binary = bundle.join(RUNTIME_BIN);
    if !binary.is_file() {
        return Err(format!(
            "{}: the tilolang runtime bundle of the Tilo installed here has no {RUNTIME_BIN}",
            bundle.display()
        ));
    }
    let needed = linked_runtime_libraries(&binary)?;
    let loaders = if needed.is_empty() {
        Vec::new()
    } else {
        vec![runtime_libraries(&bundle, &needed)?]
    };
    Ok(Installed { binary, loaders })
}

/// The version of `tilolang/<version>`, as a runtime root names its directory.
fn runtime_version() -> &'static str {
    DEFAULT_TILOLANG_RUNTIME
        .split_once('/')
        .map_or(DEFAULT_TILOLANG_RUNTIME, |(_, version)| version)
}

/// Where a tilolang runtime was looked for, and what runs a package's window without one.
fn not_installed(version: &str) -> String {
    let person = runtime_bundle::default_bundle_path(version).map_or_else(
        |_| "$XDG_DATA_HOME/tilo-runtime".to_owned(),
        |path| path.display().to_string(),
    );
    format!(
        "no tilolang runtime to run the app on: the package is in no Tilo checkout, and no Tilo \
         installed on this computer offers the runtime tilolang/{version} (looked in {person} and \
         {}). Install Tilo on this computer, run `tilolang dev` in a Tilo checkout (or name one \
         with --repo DIR or $TILO_REPO), or name a tilolang runtime binary with --runtime BIN",
        runtime_bundle::system_bundle_path(version).display()
    )
}

/// The shared libraries of the Tilo runtime that `binary` links by name: the toolkit
/// (`libruntime_dylib.so`) and the `libstd-<hash>.so` of the compiler that built it. A binary that
/// links neither (an ordinary build) needs no bundle beside it.
fn linked_runtime_libraries(binary: &Path) -> Result<Vec<String>, String> {
    let bytes = std::fs::read(binary).map_err(|error| format!("{}: {error}", binary.display()))?;
    let mut names = Vec::new();
    for prefix in [b"libstd-".as_slice(), b"libruntime_dylib".as_slice()] {
        let mut rest = bytes.as_slice();
        while let Some(at) = position(rest, prefix) {
            let candidate = rest.get(at..).unwrap_or_default();
            let end = candidate
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(candidate.len());
            if let Ok(name) = std::str::from_utf8(candidate.get(..end).unwrap_or_default())
                && is_soname(name)
                && !names.iter().any(|known| known == name)
            {
                names.push(name.to_owned());
            }
            rest = candidate.get(prefix.len()..).unwrap_or_default();
        }
    }
    Ok(names)
}

/// Whether `name` is a library's file name, as a binary records one it links.
fn is_soname(name: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("so"))
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn position(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// The `lib/` of the Tilo runtime bundle that holds every library of `needed`: one beside the
/// tilolang bundle `bundle`, in the same runtime root, or in the system's. Each build of the Tilo
/// runtime has its own `libstd` hash, so the one that holds the binary's is the one it was built
/// against.
fn runtime_libraries(bundle: &Path, needed: &[String]) -> Result<PathBuf, String> {
    let roots: Vec<PathBuf> = bundle
        .parent()
        .map(Path::to_path_buf)
        .into_iter()
        .chain(
            runtime_bundle::system_bundle_path("")
                .parent()
                .map(Path::to_path_buf),
        )
        .collect();
    for root in &roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        let mut candidates: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path().join("lib"))
            .filter(|lib| needed.iter().all(|name| lib.join(name).is_file()))
            .collect();
        candidates.sort();
        if let Some(lib) = candidates.pop() {
            return Ok(lib);
        }
    }
    Err(format!(
        "{}: the tilolang runtime links {}, which no Tilo runtime installed beside it holds (looked \
         in {}): the Tilo installed here is incomplete; reinstall or update it",
        bundle.join(RUNTIME_BIN).display(),
        needed.join(", "),
        roots
            .iter()
            .map(|root| root.display().to_string())
            .collect::<Vec<_>>()
            .join(" and ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn an_installed_runtime_is_found_with_the_libraries_it_links;

        phase a_thin_binary_finds_the_tilo_runtime_that_holds_its_libstd;
        // Two builds of the Tilo runtime beside the tilolang bundle: the binary links the libstd of
        // one of them, and runs on that one's libraries.
        {
            let root = tempfile::tempdir().unwrap();
            let tilolang = root.path().join(runtime_version());
            std::fs::create_dir_all(tilolang.join("bin")).unwrap();
            std::fs::write(
                tilolang.join(RUNTIME_BIN),
                b"\x7fELF\0libruntime_dylib.so\0libstd-bbbb.so\0libc.so.6\0",
            )
            .unwrap();
            for (build, libstd) in [("2026.07+native.a", "libstd-aaaa.so"), ("2026.07+native.b", "libstd-bbbb.so")] {
                let lib = root.path().join(build).join("lib");
                std::fs::create_dir_all(&lib).unwrap();
                std::fs::write(lib.join(libstd), b"").unwrap();
                std::fs::write(lib.join("libruntime_dylib.so"), b"").unwrap();
            }
            let needed = linked_runtime_libraries(&tilolang.join(RUNTIME_BIN)).unwrap();
            assert_eq!(needed, ["libstd-bbbb.so", "libruntime_dylib.so"]);
            assert_eq!(
                runtime_libraries(&tilolang, &needed).unwrap(),
                root.path().join("2026.07+native.b/lib")
            );
        }

        phase a_binary_whose_libstd_no_runtime_holds_says_so;
        {
            let root = tempfile::tempdir().unwrap();
            let tilolang = root.path().join(runtime_version());
            std::fs::create_dir_all(&tilolang).unwrap();
            let error = runtime_libraries(&tilolang, &["libstd-cccc.so".to_owned()]).unwrap_err();
            assert!(error.contains("libstd-cccc.so"), "{error}");
        }

        phase an_ordinary_binary_needs_no_runtime_beside_it;
        {
            let root = tempfile::tempdir().unwrap();
            let binary = root.path().join("tilolang-process");
            std::fs::write(&binary, b"\x7fELF\0libc.so.6\0libm.so.6\0").unwrap();
            assert!(linked_runtime_libraries(&binary).unwrap().is_empty());
        }
    }
}
