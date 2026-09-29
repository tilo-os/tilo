//! The resource kinds an app declares with `#[resource(kind = "...")]`, read
//! from its sources so `tilo-app package` signs them into the manifest's
//! `[resources]` table. The attribute takes only a string literal, so the
//! sources name every kind a crate declares.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;
use syn::visit_mut::VisitMut;

/// Every kind the app at `app_dir` holds records of: those declared in its
/// own sources and in the sources of the local crates it depends on, such as
/// a shared model crate. Registry and git dependencies are not scanned, nor
/// are dev- or build-dependencies, which never hold the app's records.
///
/// # Errors
///
/// Returns an error when `cargo metadata` fails or a source file cannot be
/// read or parsed.
pub fn declared_by_app(app_dir: &Path) -> Result<Vec<String>> {
    let metadata = crate::cargo_metadata(app_dir)?;
    let mut kinds = Kinds::default();
    visit_sources(&app_dir.join("src"), &mut kinds)?;
    for dependency in local_dependencies(&metadata)? {
        visit_sources(&dependency.join("src"), &mut kinds)?;
    }
    Ok(kinds.0.into_iter().collect())
}

/// Every kind declared under `crate_dir/src`, sorted and without repeats.
///
/// # Errors
///
/// Returns an error when a source file cannot be read or parsed.
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
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            visit_sources(&path, kinds)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            let source = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            if !source.contains("resource") {
                continue;
            }
            let mut file =
                syn::parse_file(&source).with_context(|| format!("parsing {}", path.display()))?;
            kinds.visit_file_mut(&mut file);
        }
    }
    Ok(())
}

/// Whether the attributes compile their item only into the crate's tests,
/// whose resources no package holds.
fn test_only(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && attribute
                .parse_args::<syn::Ident>()
                .is_ok_and(|predicate| predicate == "test")
    })
}

#[derive(Default)]
struct Kinds(BTreeSet<String>);

impl VisitMut for Kinds {
    fn visit_item_mod_mut(&mut self, module: &mut syn::ItemMod) {
        if !test_only(&module.attrs) {
            syn::visit_mut::visit_item_mod_mut(self, module);
        }
    }

    fn visit_item_impl_mut(&mut self, item: &mut syn::ItemImpl) {
        if !test_only(&item.attrs) {
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

    #[test]
    fn kinds_are_read_from_every_resource_attribute_in_the_sources() {
        let app = tempfile::tempdir().unwrap();
        let src = app.path().join("src");
        std::fs::create_dir_all(src.join("model")).unwrap();
        std::fs::write(
            src.join("main.rs"),
            r#"
            #[resource(kind = "example.todo", label = "Todo list")]
            impl Todo {}
            #[tilo_app::resource(label = "Note", kind = "example.note", search = notes::search)]
            impl Note {}
            #[serde(rename = "kind")]
            struct Unrelated;
            #[cfg(test)]
            mod tests {
                #[resource(kind = "test.page")]
                impl Page {}
            }
            #[cfg(test)]
            #[resource(kind = "test.shelf")]
            impl Shelf {}
            "#,
        )
        .unwrap();
        std::fs::write(
            src.join("model/mod.rs"),
            r#"#[resource(kind = "example.note")] impl Again {}"#,
        )
        .unwrap();
        assert_eq!(
            super::declared_in(app.path()).unwrap(),
            ["example.note", "example.todo"]
        );
        assert!(
            super::declared_in(tempfile::tempdir().unwrap().path())
                .unwrap()
                .is_empty()
        );
    }

    fn write_crate(dir: &Path, manifest: &str, source: &str) {
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), manifest).unwrap();
        std::fs::write(dir.join("src/lib.rs"), source).unwrap();
    }

    #[test]
    fn kinds_declared_in_local_normal_dependencies_belong_to_the_app() {
        let workspace = tempfile::tempdir().unwrap();
        let root = workspace.path();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nresolver = \"3\"\nmembers = [\"app\", \"model\", \"shared\", \"fixtures\"]\n",
        )
        .unwrap();
        let package = |name: &str, deps: &str| {
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n{deps}")
        };
        write_crate(
            &root.join("app"),
            &package(
                "app",
                "[dependencies]\nmodel = { path = \"../model\" }\n\
                 [dev-dependencies]\nfixtures = { path = \"../fixtures\" }\n",
            ),
            r#"#[resource(kind = "example.app")] impl App {}"#,
        );
        write_crate(
            &root.join("model"),
            &package(
                "model",
                "[dependencies]\nshared = { path = \"../shared\" }\n",
            ),
            r#"#[resource(kind = "example.notebook")] impl Notebook {}"#,
        );
        write_crate(
            &root.join("shared"),
            &package("shared", ""),
            r#"#[resource(kind = "example.tag")] impl Tag {}"#,
        );
        write_crate(
            &root.join("fixtures"),
            &package("fixtures", ""),
            r#"#[resource(kind = "example.fixture")] impl Fixture {}"#,
        );

        assert_eq!(
            super::declared_by_app(&root.join("app")).unwrap(),
            ["example.app", "example.notebook", "example.tag"],
            "a model crate's kinds, transitively, and not a dev-dependency's"
        );
    }
}
