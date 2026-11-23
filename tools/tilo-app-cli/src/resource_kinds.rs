//! The resource kinds an app declares with `#[resource(kind = "...")]` or
//! a manual `Resource` implementation, read from its sources so `tilo-app
//! package` signs them into the manifest's `[resources]` table. Both forms
//! name their kinds with string literals.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;
use syn::visit_mut::VisitMut;

/// Every kind the app at `app_dir` holds records of: those declared in its
/// own sources and in the sources of the local crates it depends on, such as
/// a shared model crate. Registry and git dependencies are not scanned, nor
/// are dev- or build-dependencies, which never hold the app's records.
pub fn declared_by_app(app_dir: &Path) -> Result<Vec<String>> {
    let metadata = crate::cargo_metadata(app_dir)?;
    declared_with_metadata(app_dir, &metadata)
}

pub(crate) fn declared_with_metadata(app_dir: &Path, metadata: &Value) -> Result<Vec<String>> {
    let mut kinds = Kinds::default();
    visit_sources(&app_dir.join("src"), &mut kinds)?;
    for dependency in local_dependencies(metadata)? {
        visit_sources(&dependency.join("src"), &mut kinds)?;
    }
    Ok(kinds.0.into_iter().collect())
}

/// The kinds that are the app's own: declared in its sources or in a local
/// crate it depends on directly, such as a shared model, and not by the SDK,
/// whose kinds are the framework's. They are what the app's manifest must
/// list in `[resources] kinds`, so a governed workspace lets the app create
/// its records.
/// Keep the historical queue kind for apps with chain parts so earlier
/// records can still drain after upgrading.
pub fn owned_by_app(app_dir: &Path) -> Result<Vec<String>> {
    let mut kinds: BTreeSet<String> = declared_in(app_dir)?.into_iter().collect();
    for dependency in direct_path_dependencies(app_dir)? {
        kinds.extend(declared_in(&dependency)?);
    }
    if keeps_waiting_actions(app_dir)? {
        kinds.insert(tilo_actions::surface::CHAIN_UNSENT_KIND.to_owned());
    }
    Ok(kinds.into_iter().collect())
}

/// Whether the app's records are the parts of a chain (`impl Chained for`, in
/// its sources or a local crate it depends on directly). Earlier versions
/// used [`tilo_actions::surface::CHAIN_UNSENT_KIND`] for waiting actions.
pub fn keeps_waiting_actions(app_dir: &Path) -> Result<bool> {
    let mut dirs = vec![app_dir.to_path_buf()];
    dirs.extend(direct_path_dependencies(app_dir)?);
    for dir in dirs {
        if implements_chained(&dir.join("src"))? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether a source file under `dir` implements `Chained`.
fn implements_chained(dir: &Path) -> Result<bool> {
    if !dir.is_dir() {
        return Ok(false);
    }
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            if implements_chained(&path)? {
                return Ok(true);
            }
        } else if path.extension().is_some_and(|extension| extension == "rs")
            && std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?
                .contains("Chained for ")
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The directories of the path crates `app_dir/Cargo.toml` lists as normal
/// dependencies, the SDK's own crates excepted.
fn direct_path_dependencies(app_dir: &Path) -> Result<Vec<PathBuf>> {
    let path = app_dir.join("Cargo.toml");
    let manifest: toml::Table = toml::from_str(
        &std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?,
    )
    .with_context(|| format!("parsing {}", path.display()))?;
    let Some(dependencies) = manifest.get("dependencies").and_then(toml::Value::as_table) else {
        return Ok(Vec::new());
    };
    Ok(dependencies
        .values()
        .filter_map(|dependency| dependency.get("path")?.as_str())
        .filter_map(|path| app_dir.join(path).canonicalize().ok())
        .filter(|dir| !dir.components().any(|part| part.as_os_str() == "sdk"))
        .collect())
}

/// Every kind declared under `crate_dir/src`, sorted and without repeats.
pub fn declared_in(crate_dir: &Path) -> Result<Vec<String>> {
    let mut kinds = Kinds::default();
    visit_sources(&crate_dir.join("src"), &mut kinds)?;
    Ok(kinds.0.into_iter().collect())
}

/// The directories of the path packages in the metadata root's closure of
/// normal dependencies, the root excluded.
fn local_dependencies(metadata: &Value) -> Result<Vec<PathBuf>> {
    let root = metadata
        .pointer("/resolve/root")
        .and_then(Value::as_str)
        .context("cargo metadata names no root package")?;
    let packages = metadata
        .get("packages")
        .and_then(Value::as_array)
        .context("cargo metadata lists no packages")?;
    let nodes = metadata
        .pointer("/resolve/nodes")
        .and_then(Value::as_array)
        .context("cargo metadata has no resolve graph")?;
    let local_dir = |id: &str| {
        let package = packages
            .iter()
            .find(|package| package.get("id").and_then(Value::as_str) == Some(id))?;
        if !package.get("source").is_some_and(Value::is_null) {
            return None;
        }
        let manifest = package.get("manifest_path").and_then(Value::as_str)?;
        Path::new(manifest).parent().map(Path::to_path_buf)
    };
    let mut seen = HashSet::from([root]);
    let mut pending = vec![root];
    let mut dirs = Vec::new();
    while let Some(id) = pending.pop() {
        let deps = nodes
            .iter()
            .find(|node| node.get("id").and_then(Value::as_str) == Some(id))
            .and_then(|node| node.get("deps"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten();
        for dep in deps {
            let normal = dep
                .get("dep_kinds")
                .and_then(Value::as_array)
                .is_some_and(|kinds| kinds.iter().any(|kind| kind["kind"].is_null()));
            let Some(dep) = dep.get("pkg").and_then(Value::as_str) else {
                continue;
            };
            if !normal || !seen.insert(dep) {
                continue;
            }
            if let Some(dir) = local_dir(dep) {
                dirs.push(dir);
                pending.push(dep);
            }
        }
    }
    Ok(dirs)
}

fn visit_sources(dir: &Path, kinds: &mut Kinds) -> Result<()> {
    visit_dir(dir, &HashSet::new(), kinds)
}

/// The sources under `dir`, less `excluded`: the files and directories of
/// modules a parent declared test-only (`#[cfg(test)] mod tests;`), whose
/// own files carry no `cfg` of their own.
fn visit_dir(dir: &Path, excluded: &HashSet<PathBuf>, kinds: &mut Kinds) -> Result<()> {
    if !dir.is_dir() || excluded.contains(dir) {
        return Ok(());
    }
    let mut candidates = Vec::new();
    let mut dirs = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            dirs.push(path);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            let source = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            // A kind is named only by a `resource(kind = "...")` attribute or
            // the `KIND` of an `impl Resource for`: a file without either
            // declares none, however often it says "resource".
            let declares = (source.contains("resource(") && source.contains("kind"))
                || (source.contains("Resource for") && source.contains("KIND"));
            // A file that declares no kind is parsed only for the test-only
            // modules it declares in files of their own (`#[cfg(test)] mod
            // tests;`); most files carry only inline test modules, and parsing
            // every one of them made a scan of the SDK take most of a minute.
            if !declares && (!source.contains("cfg(") || !declares_module_file(&source)) {
                continue;
            }
            candidates.push((path, source, declares));
        }
    }
    let read = read_files(candidates)?;
    let mut excluded = excluded.clone();
    for file in &read {
        excluded.extend(file.test_only.iter().cloned());
    }
    for file in read {
        if !excluded.contains(&file.path) {
            kinds.0.extend(file.kinds.0);
        }
    }
    for dir in dirs {
        visit_dir(&dir, &excluded, kinds)?;
    }
    Ok(())
}

/// What one source file says: the files of the test-only modules it
/// declares, and the kinds it declares itself.
struct ReadFile {
    path: PathBuf,
    test_only: Vec<PathBuf>,
    kinds: Kinds,
}

/// Parse `candidates` (path, source, whether it may declare a kind) side by
/// side: a directory of the SDK holds hundreds of files, and parsing them one
/// after another was most of a scan. A parsed file stays on the thread that
/// parsed it; only what it says comes back.
fn read_files(candidates: Vec<(PathBuf, String, bool)>) -> Result<Vec<ReadFile>> {
    let read_one = |(path, source, declares): (PathBuf, String, bool)| -> Result<ReadFile> {
        let mut file =
            syn::parse_file(&source).with_context(|| format!("parsing {}", path.display()))?;
        let test_only = test_only_module_paths(&path, &file);
        let mut kinds = Kinds::default();
        if declares {
            kinds.visit_file_mut(&mut file);
        }
        Ok(ReadFile {
            path,
            test_only,
            kinds,
        })
    };
    let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    if candidates.len() < 2 || threads < 2 {
        return candidates.into_iter().map(read_one).collect();
    }
    let per_thread = candidates.len().div_ceil(threads);
    let mut chunks = Vec::new();
    let mut rest = candidates;
    while !rest.is_empty() {
        let tail = rest.split_off(per_thread.min(rest.len()));
        chunks.push(rest);
        rest = tail;
    }
    std::thread::scope(|scope| {
        let reads: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                scope.spawn(move || chunk.into_iter().map(read_one).collect::<Result<Vec<_>>>())
            })
            .collect();
        let mut files = Vec::new();
        for read in reads {
            files.extend(
                read.join()
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("parsing a source file panicked")))?,
            );
        }
        Ok(files)
    })
}

/// Where the modules `file` declares out of line and only for tests live:
/// `name.rs` and `name/` beside a `lib.rs`, `main.rs` or `mod.rs`, and under
/// a directory named for any other file.
/// Whether `source` may declare a module whose body is a file of its own
/// (`mod name;`): a cheap look at the text, which may say yes for a file that
/// does not, never no for one that does.
fn declares_module_file(source: &str) -> bool {
    source.lines().any(|line| {
        let line = line.trim();
        line.ends_with(';')
            && !line.contains('{')
            && (line.starts_with("mod ") || line.contains(" mod "))
    })
}

fn test_only_module_paths(path: &Path, file: &syn::File) -> Vec<PathBuf> {
    let Some(dir) = path.parent() else {
        return Vec::new();
    };
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("");
    let base = if matches!(stem, "lib" | "main" | "mod") {
        dir.to_path_buf()
    } else {
        dir.join(stem)
    };
    file.items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Mod(module) if module.content.is_none() && test_only(&module.attrs) => {
                // `#[path = "..."]` names the file beside the declaring one.
                if let Some(path) = module_path(&module.attrs) {
                    return Some(vec![dir.join(path)]);
                }
                let name = module.ident.to_string();
                Some(vec![base.join(format!("{name}.rs")), base.join(name)])
            }
            _ => None,
        })
        .flatten()
        .collect()
}

/// The file a `#[path = "..."]` attribute puts a module in.
fn module_path(attributes: &[syn::Attribute]) -> Option<String> {
    attributes
        .iter()
        .find_map(|attribute| match &attribute.meta {
            syn::Meta::NameValue(name_value) if name_value.path.is_ident("path") => {
                match &name_value.value {
                    syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(path),
                        ..
                    }) => Some(path.value()),
                    _ => None,
                }
            }
            _ => None,
        })
}

/// Whether the attributes compile their item only into the crate's tests,
/// whose resources no package holds.
fn test_only(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && attribute
                .parse_args::<syn::Meta>()
                .is_ok_and(|predicate| requires_test(&predicate))
    })
}

/// A production build cannot satisfy `test`, or an `all` containing it.
/// An `any` is test-only only when every arm requires `test`.
fn requires_test(predicate: &syn::Meta) -> bool {
    match predicate {
        syn::Meta::Path(path) => path.is_ident("test"),
        syn::Meta::List(list) => {
            let Ok(arms) = list.parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            ) else {
                return false;
            };
            if list.path.is_ident("all") {
                arms.iter().any(requires_test)
            } else if list.path.is_ident("any") && !arms.is_empty() {
                arms.iter().all(requires_test)
            } else {
                false
            }
        }
        syn::Meta::NameValue(_) => false,
    }
}

#[derive(Default)]
struct Kinds(BTreeSet<String>);

impl VisitMut for Kinds {
    /// A module file whose inner `cfg` compiles it only into the crate's
    /// tests holds no packaged resource, whatever declares the module.
    fn visit_file_mut(&mut self, file: &mut syn::File) {
        if !test_only(&file.attrs) {
            syn::visit_mut::visit_file_mut(self, file);
        }
    }

    fn visit_item_mod_mut(&mut self, module: &mut syn::ItemMod) {
        if !test_only(&module.attrs) {
            syn::visit_mut::visit_item_mod_mut(self, module);
        }
    }

    fn visit_item_impl_mut(&mut self, item: &mut syn::ItemImpl) {
        if !test_only(&item.attrs) {
            if item.trait_.as_ref().is_some_and(|(_, path, _)| {
                path.segments
                    .last()
                    .is_some_and(|segment| segment.ident == "Resource")
            }) {
                for member in &item.items {
                    if let syn::ImplItem::Const(constant) = member
                        && constant.ident == "KIND"
                        && let syn::Expr::Lit(literal) = &constant.expr
                        && let syn::Lit::Str(kind) = &literal.lit
                    {
                        self.0.insert(kind.value());
                    }
                }
            }
            syn::visit_mut::visit_item_impl_mut(self, item);
        }
    }

    fn visit_attribute_mut(&mut self, attribute: &mut syn::Attribute) {
        if attribute
            .path()
            .segments
            .last()
            .is_none_or(|segment| segment.ident != "resource")
        {
            return;
        }
        // Another crate's `resource` attribute may take other arguments;
        // only a `kind = "..."` argument names a kind.
        let _ = attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("kind") {
                let kind: syn::LitStr = meta.value()?.parse()?;
                self.0.insert(kind.value());
            } else if meta.input.peek(syn::Token![=]) {
                meta.value()?.parse::<syn::Expr>()?;
            }
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// The directories of the path crates `app_dir/Cargo.toml` lists as
    /// normal dependencies, the SDK's own crates excepted.
    fn direct_local_dependencies(app_dir: &Path) -> Vec<std::path::PathBuf> {
        let manifest: toml::Table =
            toml::from_str(&std::fs::read_to_string(app_dir.join("Cargo.toml")).unwrap()).unwrap();
        let Some(dependencies) = manifest.get("dependencies").and_then(toml::Value::as_table)
        else {
            return Vec::new();
        };
        dependencies
            .values()
            .filter_map(|dependency| dependency.get("path")?.as_str())
            .map(|path| app_dir.join(path).canonicalize().unwrap())
            .filter(|dir| !dir.components().any(|part| part.as_os_str() == "sdk"))
            .collect()
    }

    #[test]
    fn every_app_manifest_declares_the_record_kinds_its_sources_hold() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let mut apps: Vec<_> = std::fs::read_dir(root.join("apps"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|dir| dir.join("tilo-app.toml").is_file())
            .collect();
        apps.sort();
        assert!(
            apps.len() >= 40,
            "expected the app fleet, found {}",
            apps.len()
        );
        let mut dependency_kinds = std::collections::HashMap::new();
        let mut missing = Vec::new();
        for app in apps {
            let manifest = tilo_app_package::Manifest::from_toml_str(
                &std::fs::read_to_string(app.join("tilo-app.toml")).unwrap(),
            )
            .unwrap_or_else(|error| panic!("{}: {error}", app.display()));
            let mut kinds = super::declared_in(&app).unwrap();
            for dependency in direct_local_dependencies(&app) {
                kinds.extend(
                    dependency_kinds
                        .entry(dependency.clone())
                        .or_insert_with(|| super::declared_in(&dependency).unwrap())
                        .iter()
                        .cloned(),
                );
            }
            if super::keeps_waiting_actions(&app).unwrap() {
                kinds.push(tilo_actions::surface::CHAIN_UNSENT_KIND.to_owned());
            }
            for kind in kinds {
                if !manifest.resources.kinds.contains(&kind) {
                    missing.push(format!(
                        "{}: {kind}",
                        app.file_name().unwrap().to_string_lossy()
                    ));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "kinds missing from `[resources] kinds` (or `own`) in apps/*/tilo-app.toml:\n{}",
            missing.join("\n")
        );
    }
}
