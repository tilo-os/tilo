//! The `tilolang` command line: `check`, `build`, `fmt`, `ids` and `run`.
//!
//! The compiler's own binary runs every command but `run`, which needs the platform: the host
//! app's binary (`apps/tilolang`) passes a [`Runner`] and so runs all of them.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use crate::diag::{Diagnostic, diagnostics_json};
use crate::ids::{OsEntropy, SeededEntropy};
use crate::lexer::plain_comments;
use crate::meta::PackageMeta;
use crate::package::parse_package_files;
use crate::print::print_file;
use crate::source::{
    BuildError, BuiltPackage, PackageSource, Stamp, WASM_FILE, compile_dirs, dependency_order,
    read_ids, write_ids,
};

pub const USAGE: &str = "usage:
  tilolang check [dir] [--json] [--watch] [--path <dir>]... [--import <app.wasm>]...
  tilolang build [dir] [--path <dir>]... [--import <app.wasm>]...
  tilolang fmt [dir] [--check]
  tilolang ids [dir] [--rename <old> <new>]
  tilolang run [dir] [--peers <n>] [--fresh] [--path <dir>]...

[dir] is a package directory (default: the current directory). Every .tl file in it is part of
the package. `use` imports are found among the package's sibling directories and in each
--path, which names a package directory or a directory of packages.";

/// What `tilolang run` asks of the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunRequest {
    /// The package directories, dependencies first and the package being run last.
    pub dirs: Vec<PathBuf>,
    /// How many replicas of the room to show side by side, each its own collaborator.
    pub peers: usize,
    /// Start from an empty room instead of the one the last run left.
    pub fresh: bool,
}

/// Runs a package for development; provided by the host app.
pub type Runner<'a> = &'a dyn Fn(&RunRequest) -> Result<(), String>;

/// The commands this module handles: a host binary dispatches to [`main`] when its first
/// argument is one of them.
pub const COMMANDS: &[&str] = &["check", "build", "fmt", "ids", "run"];

#[derive(Debug, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is one command-line flag, independent of the others"
)]
struct Options {
    dir: Option<PathBuf>,
    json: bool,
    watch: bool,
    check: bool,
    fresh: bool,
    peers: Option<usize>,
    imports: Vec<PathBuf>,
    search: Vec<PathBuf>,
    rename: Option<(String, String)>,
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    let mut o = Options::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" => o.json = true,
            "--watch" => o.watch = true,
            "--check" => o.check = true,
            "--fresh" => o.fresh = true,
            "--peers" => {
                let n = it.next().ok_or("--peers needs a number")?;
                let n: usize = n
                    .parse()
                    .map_err(|_| format!("--peers needs a number, not `{n}`"))?;
                if !(1..=8).contains(&n) {
                    return Err("--peers takes 1 to 8 replicas".into());
                }
                o.peers = Some(n);
            }
            "--import" => o.imports.push(PathBuf::from(
                it.next().ok_or("--import needs a path to an app.wasm")?,
            )),
            "--path" => o
                .search
                .push(PathBuf::from(it.next().ok_or("--path needs a directory")?)),
            "--rename" => {
                let old = it.next().ok_or("--rename needs the old and the new name")?;
                let new = it.next().ok_or("--rename needs the old and the new name")?;
                o.rename = Some((old.clone(), new.clone()));
            }
            other if other.starts_with("--") => return Err(format!("unknown option `{other}`")),
            other => {
                if o.dir.is_some() {
                    return Err(format!("one package directory, not also `{other}`"));
                }
                o.dir = Some(PathBuf::from(other));
            }
        }
    }
    Ok(o)
}

fn read_imports(paths: &[PathBuf]) -> Result<Vec<PackageMeta>, String> {
    paths
        .iter()
        .map(|p| {
            let bytes = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
            PackageMeta::from_wasm(&bytes).ok_or_else(|| {
                format!(
                    "{}: not a tilolang package (no tilolang.meta section)",
                    p.display()
                )
            })
        })
        .collect()
}

fn report(diags: &[Diagnostic], json: bool) {
    if json {
        println!("{}", diagnostics_json(diags));
    } else {
        for d in diags {
            eprintln!("{}", d.render("app.tl"));
        }
    }
}

/// Compiles the package in `dir` and its imports. `extra` are metadata of already built modules
/// (`--import`), visible to every package.
fn build_all(
    dir: &Path,
    o: &Options,
    entropy: &mut dyn crate::ids::Entropy,
) -> Result<Vec<BuiltPackage>, BuildError> {
    let dirs = dependency_order(dir, &o.search).map_err(BuildError::Read)?;
    let extra = read_imports(&o.imports).map_err(BuildError::Read)?;
    compile_dirs(&dirs, &extra, entropy)
}

/// Reports a build failure; `Ok(false)` for errors in the sources.
fn failed(error: BuildError, json: bool) -> Result<bool, String> {
    match error {
        BuildError::Read(message) => Err(message),
        BuildError::Diagnostics { diags, .. } => {
            report(&diags, json);
            Ok(false)
        }
    }
}

fn check_once(dir: &Path, o: &Options) -> Result<bool, String> {
    match build_all(dir, o, &mut SeededEntropy(0)) {
        Ok(_) => {
            if o.json {
                println!("[]");
            }
            Ok(true)
        }
        Err(error) => failed(error, o.json),
    }
}

fn check(dir: &Path, o: &Options) -> Result<bool, String> {
    if !o.watch {
        return check_once(dir, o);
    }
    let watched = dependency_order(dir, &o.search).unwrap_or_else(|_| vec![dir.to_path_buf()]);
    let mut last = Stamp::default();
    loop {
        let now = Stamp::of(&watched);
        if now != last {
            last = now;
            match check_once(dir, o) {
                Ok(true) if !o.json => eprintln!("{}: no errors", dir.display()),
                Ok(_) => {}
                Err(message) => eprintln!("{message}"),
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

fn build(dir: &Path, o: &Options) -> Result<bool, String> {
    let built = match build_all(dir, o, &mut OsEntropy::default()) {
        Ok(built) => built,
        Err(error) => return failed(error, o.json),
    };
    for b in &built {
        if write_ids(b)? {
            eprintln!("{}: ids lock updated", b.dir.display());
        }
    }
    if let Some(root) = built.last() {
        let path = root.dir.join(WASM_FILE);
        std::fs::write(&path, &root.compiled.wasm)
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(true)
}

/// The canonical text of every file of the package in `dir`, with the files whose plain `//`
/// comments it would drop.
pub struct Formatted {
    /// `(path, old text, canonical text)` for each file.
    pub files: Vec<(String, String, String)>,
    /// Files with plain `//` comments, which `fmt` leaves alone: the canonical form has only
    /// `///` doc comments, and rewriting would delete what someone wrote.
    pub with_comments: Vec<(String, usize)>,
}

/// Formats a package's sources without writing anything.
///
/// # Errors
/// The package has syntax errors; they name their files.
pub fn format_package(source: &PackageSource) -> Result<Formatted, Vec<Diagnostic>> {
    let parsed = parse_package_files(source)?;
    let mut out = Formatted {
        files: Vec::new(),
        with_comments: Vec::new(),
    };
    for file in parsed {
        let old = source
            .files
            .iter()
            .find(|f| f.path == file.path)
            .map(|f| f.text.clone())
            .unwrap_or_default();
        let comments = plain_comments(&old);
        if let Some(first) = comments.first() {
            let mut d = Diagnostic::new("E000", *first, "");
            d.locate(&old);
            out.with_comments.push((file.path.clone(), d.line));
            continue;
        }
        let new = print_file(file.header.as_ref(), &file.items);
        out.files.push((file.path, old, new));
    }
    Ok(out)
}

fn fmt(dir: &Path, o: &Options) -> Result<bool, String> {
    let source = PackageSource::read_dir(dir)?;
    let formatted = match format_package(&source) {
        Ok(f) => f,
        Err(diags) => {
            report(&diags, false);
            return Ok(false);
        }
    };
    let mut ok = true;
    for (path, line) in &formatted.with_comments {
        eprintln!(
            "{path}:{line}: has `//` comments, which the canonical form drops; make them `///` doc comments on a declaration or remove them, then format again"
        );
        ok = false;
    }
    for (path, old, new) in &formatted.files {
        if old == new {
            continue;
        }
        if o.check {
            eprintln!("{path}: not in canonical form");
            ok = false;
        } else {
            std::fs::write(path, new).map_err(|e| format!("{path}: {e}"))?;
            eprintln!("{path}: formatted");
        }
    }
    Ok(ok)
}

fn ids(dir: &Path, o: &Options) -> Result<bool, String> {
    let mut ids = read_ids(dir)?;
    if let Some((old, new)) = &o.rename {
        ids.rename(old, new).map_err(|e| e.to_string())?;
        std::fs::write(dir.join(crate::source::IDS_FILE), ids.to_text())
            .map_err(|e| e.to_string())?;
    } else {
        print!("{}", ids.to_text());
    }
    Ok(true)
}

fn run(dir: &Path, o: &Options, runner: Option<Runner<'_>>) -> Result<bool, String> {
    let Some(runner) = runner else {
        return Err(
            "`tilolang run` needs the host app: build it with `cargo build -p tilolang-app` and \
             run its `tilolang` binary (target/debug/tilolang), or `cargo run -p tilolang-app -- run <dir>`"
                .into(),
        );
    };
    // Refuse to start on errors: the terminal shows them, and the window would only be empty.
    let built = match build_all(dir, o, &mut OsEntropy::default()) {
        Ok(built) => built,
        Err(error) => return failed(error, false),
    };
    for b in &built {
        if write_ids(b)? {
            eprintln!("{}: ids lock updated", b.dir.display());
        }
    }
    let request = RunRequest {
        dirs: built.into_iter().map(|b| b.dir).collect(),
        peers: o.peers.unwrap_or(1),
        fresh: o.fresh,
    };
    runner(&request)?;
    Ok(true)
}

/// Runs the command line `args` (without the program name). `runner` runs a package for
/// development; without one, `run` explains where it lives.
pub fn main(args: &[String], runner: Option<Runner<'_>>) -> ExitCode {
    let result = (|| {
        let (cmd, rest) = args.split_first().ok_or(USAGE)?;
        if matches!(cmd.as_str(), "-h" | "--help" | "help") {
            eprintln!("{USAGE}");
            return Ok(true);
        }
        let o = parse_options(rest)?;
        let dir = o.dir.clone().unwrap_or_else(|| PathBuf::from("."));
        match cmd.as_str() {
            "check" => check(&dir, &o),
            "build" => build(&dir, &o),
            "fmt" => fmt(&dir, &o),
            "ids" => ids(&dir, &o),
            "run" => run(&dir, &o, runner),
            _ => Err(USAGE.to_string()),
        }
    })();
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
    }
}
