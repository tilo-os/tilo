//! Package sources on disk: the `.tl` files of a package directory, the packages it imports
//! found among its siblings, and compiling them in dependency order.
//!
//! A package is a directory. Every `.tl` file directly in it belongs to the package's one
//! module; exactly one of them starts with the `app` header. The ids lock is one `app.ids` per
//! package, next to the sources.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::ast::Item;
use crate::diag::Diagnostic;
use crate::ids::{Entropy, IdsLock};
use crate::meta::PackageMeta;
use crate::package::{Compiled, compile_package_source_with};
use crate::parser::parse_file_at;

/// The ids lock's file name in a package directory.
pub const IDS_FILE: &str = "app.ids";
/// The compiled module's file name in a package directory.
pub const WASM_FILE: &str = "app.wasm";
/// The extension of source files.
pub const EXTENSION: &str = "tl";

/// One source file of a package.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceFile {
    /// The path diagnostics name the file by.
    pub path: String,
    pub text: String,
}

/// The source files of one package. Their order does not matter: the compiler reads them in
/// path order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackageSource {
    pub files: Vec<SourceFile>,
}

impl PackageSource {
    /// A package of the given `(path, text)` files.
    #[must_use]
    pub fn new(files: impl IntoIterator<Item = (String, String)>) -> Self {
        Self {
            files: files
                .into_iter()
                .map(|(path, text)| SourceFile { path, text })
                .collect(),
        }
    }

    /// A package of one file, `app.tl`.
    #[must_use]
    pub fn single(text: &str) -> Self {
        Self::new([("app.tl".to_string(), text.to_string())])
    }

    /// Every `.tl` file directly in `dir`, named by `dir` joined with the file name.
    ///
    /// # Errors
    /// The directory or a file cannot be read, or it holds no `.tl` file.
    pub fn read_dir(dir: &Path) -> Result<Self, String> {
        let files = source_paths(dir)?;
        if files.is_empty() {
            return Err(format!(
                "{}: no .{EXTENSION} files; a package is a directory of .{EXTENSION} files",
                dir.display()
            ));
        }
        let mut out = Vec::new();
        for path in files {
            let text =
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            out.push((path.display().to_string(), text));
        }
        Ok(Self::new(out))
    }

    /// The app id the package declares and the app ids it imports with `use`, read leniently:
    /// files that do not parse contribute what they could.
    #[must_use]
    pub fn header(&self) -> (Option<String>, Vec<String>) {
        let mut app = None;
        let mut uses = Vec::new();
        for f in &self.files {
            let (parsed, _) = parse_file_at(&f.text, 0, 0);
            if app.is_none() {
                app = parsed.header.map(|h| h.id);
            }
            for item in parsed.items {
                if let Item::Use(u) = item
                    && !uses.contains(&u.app)
                {
                    uses.push(u.app);
                }
            }
        }
        (app, uses)
    }
}

/// The `.tl` files directly in `dir`, sorted.
///
/// # Errors
/// The directory cannot be read.
pub fn source_paths(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut out: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == EXTENSION))
        .collect();
    out.sort();
    Ok(out)
}

/// The ids lock of the package in `dir`; an empty lock when there is none yet.
///
/// # Errors
/// The lock exists but does not parse.
pub fn read_ids(dir: &Path) -> Result<IdsLock, String> {
    let path = dir.join(IDS_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => IdsLock::parse(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(_) => Ok(IdsLock::new()),
    }
}

/// What the sources of some packages look like on disk: every `.tl` file with its modification
/// time and length. Two equal stamps mean nothing a compile reads changed (the ids lock is
/// left out: compiling writes it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stamp(Vec<(PathBuf, Option<SystemTime>, u64)>);

impl Stamp {
    /// The stamp of the packages in `dirs` now. A directory that cannot be read stamps empty.
    #[must_use]
    pub fn of(dirs: &[PathBuf]) -> Self {
        let mut out = Vec::new();
        for dir in dirs {
            for path in source_paths(dir).unwrap_or_default() {
                let meta = std::fs::metadata(&path).ok();
                let modified = meta.as_ref().and_then(|m| m.modified().ok());
                let len = meta.map_or(0, |m| m.len());
                out.push((path, modified, len));
            }
        }
        Self(out)
    }
}

/// The package directories `root` needs, dependencies first and `root` last.
///
/// Imports (`use <app id>`) are looked up among the package directories in `search`: each entry
/// is a package directory itself or a directory of package directories. With an empty `search`,
/// `root`'s siblings are searched.
///
/// # Errors
/// A package cannot be read, an import names no package found, or imports form a cycle.
pub fn dependency_order(root: &Path, search: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let default_search;
    let search = if search.is_empty() {
        default_search = vec![
            root.parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf(),
        ];
        &default_search
    } else {
        search
    };
    let mut index: HashMap<String, PathBuf> = HashMap::new();
    let mut headers: HashMap<PathBuf, Vec<String>> = HashMap::new();
    let known = |dir: &Path,
                 index: &mut HashMap<String, PathBuf>,
                 headers: &mut HashMap<PathBuf, Vec<String>>|
     -> Result<(), String> {
        let source = PackageSource::read_dir(dir)?;
        let (app, uses) = source.header();
        if let Some(app) = app {
            index.entry(app).or_insert_with(|| dir.to_path_buf());
        }
        headers.insert(dir.to_path_buf(), uses);
        Ok(())
    };
    known(root, &mut index, &mut headers)?;
    for entry in search {
        let candidates: Vec<PathBuf> = if source_paths(entry).is_ok_and(|p| !p.is_empty()) {
            vec![entry.clone()]
        } else {
            let mut dirs: Vec<PathBuf> = std::fs::read_dir(entry)
                .map_err(|e| format!("{}: {e}", entry.display()))?
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.is_dir() && source_paths(p).is_ok_and(|s| !s.is_empty()))
                .collect();
            dirs.sort();
            dirs
        };
        for dir in candidates {
            if !headers.contains_key(&dir) && !same_dir(&dir, root) {
                known(&dir, &mut index, &mut headers)?;
            }
        }
    }
    let mut order = Vec::new();
    let mut state: BTreeMap<PathBuf, bool> = BTreeMap::new();
    visit(root, &index, &headers, &mut state, &mut order, search)?;
    Ok(order)
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn visit(
    dir: &Path,
    index: &HashMap<String, PathBuf>,
    headers: &HashMap<PathBuf, Vec<String>>,
    state: &mut BTreeMap<PathBuf, bool>,
    order: &mut Vec<PathBuf>,
    search: &[PathBuf],
) -> Result<(), String> {
    match state.get(dir) {
        Some(true) => return Ok(()),
        Some(false) => {
            return Err(format!(
                "{}: the packages import each other in a cycle",
                dir.display()
            ));
        }
        None => {}
    }
    state.insert(dir.to_path_buf(), false);
    for app in headers.get(dir).cloned().unwrap_or_default() {
        if app == crate::host::HOST_APP {
            continue;
        }
        let Some(dep) = index.get(&app) else {
            let searched: Vec<String> = search.iter().map(|p| p.display().to_string()).collect();
            return Err(format!(
                "{}: `use {app}`: no package with that app id in {}; name its directory with --path",
                dir.display(),
                searched.join(", ")
            ));
        };
        visit(dep, index, headers, state, order, search)?;
    }
    state.insert(dir.to_path_buf(), true);
    order.push(dir.to_path_buf());
    Ok(())
}

/// A package compiled from its directory.
#[derive(Clone, Debug)]
pub struct BuiltPackage {
    pub dir: PathBuf,
    pub source: PackageSource,
    pub compiled: Compiled,
}

/// Why packages did not compile.
#[derive(Clone, Debug)]
pub enum BuildError {
    /// A package could not be read.
    Read(String),
    /// A package has errors; each diagnostic names its file.
    Diagnostics {
        dir: PathBuf,
        diags: Vec<Diagnostic>,
    },
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read(message) => f.write_str(message),
            Self::Diagnostics { dir, diags } => {
                let shown = dir.join("app.tl").display().to_string();
                let lines: Vec<String> = diags.iter().map(|d| d.render(&shown)).collect();
                f.write_str(&lines.join("\n"))
            }
        }
    }
}

/// Compiles the packages in `dirs` in order, each against `extra` (already built modules'
/// metadata) and the packages before it: pass [`dependency_order`]'s result. Nothing is
/// written.
///
/// # Errors
/// The first package that cannot be read or has errors.
pub fn compile_dirs(
    dirs: &[PathBuf],
    extra: &[PackageMeta],
    rng: &mut dyn Entropy,
) -> Result<Vec<BuiltPackage>, BuildError> {
    let mut out: Vec<BuiltPackage> = Vec::new();
    for dir in dirs {
        let source = PackageSource::read_dir(dir).map_err(BuildError::Read)?;
        let ids = read_ids(dir).map_err(BuildError::Read)?;
        let mut imports = extra.to_vec();
        imports.extend(out.iter().map(|b| b.compiled.meta.clone()));
        let compiled =
            compile_package_source_with(&source, &ids, &imports, rng).map_err(|diags| {
                BuildError::Diagnostics {
                    dir: dir.clone(),
                    diags,
                }
            })?;
        out.push(BuiltPackage {
            dir: dir.clone(),
            source,
            compiled,
        });
    }
    Ok(out)
}

/// Writes a built package's ids lock back when it changed. Returns whether it wrote.
///
/// # Errors
/// The lock cannot be written.
pub fn write_ids(built: &BuiltPackage) -> Result<bool, String> {
    let path = built.dir.join(IDS_FILE);
    let text = built.compiled.ids.to_text();
    if std::fs::read_to_string(&path).is_ok_and(|old| old == text) {
        return Ok(false);
    }
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(true)
}
