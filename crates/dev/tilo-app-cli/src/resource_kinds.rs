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
            if !source.contains("resource") && !source.contains("Resource") {
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

    #[test]
    fn manual_resource_implementations_declare_kinds_without_attributes() {
        let app = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(app.path().join("src")).unwrap();
        std::fs::write(
            app.path().join("src/lib.rs"),
            r#"
            impl Resource for Music { const KIND: &'static str = "music.library"; }
            impl ui::tilo_resource::Resource for Book { const KIND: &'static str = "books.library"; }
            impl Unrelated { const KIND: &'static str = "unrelated.kind"; }
            #[cfg(test)]
            impl Resource for Fixture { const KIND: &'static str = "test.fixture"; }
            #[cfg(test)]
            mod tests {
                impl Resource for Page { const KIND: &'static str = "test.page"; }
            }
            #[cfg(test)]
            #[cfg(not(target_arch = "wasm32"))]
            mod native_tests {
                impl Resource for Fixture { const KIND: &'static str = "test.native"; }
            }
            #[cfg(any(test, feature = "live"))]
            impl Resource for Live { const KIND: &'static str = "live.library"; }
            "#,
        ).unwrap();
        std::fs::write(
            app.path().join("src/uppercase.rs"),
            r#"impl Resource for Uppercase { const KIND: &'static str = "uppercase.library"; }"#,
        )
        .unwrap();
        // A test module in a file of its own, as `mod shelf_tests;` declares it.
        std::fs::write(
            app.path().join("src/shelf_tests.rs"),
            r#"
            #![cfg(all(test, not(target_arch = "wasm32")))]
            impl Resource for Shelf { const KIND: &'static str = "test.shelf"; }
            "#,
        )
        .unwrap();
        assert_eq!(
            super::declared_in(app.path()).unwrap(),
            [
                "books.library",
                "live.library",
                "music.library",
                "uppercase.library"
            ]
        );
    }

    #[test]
    fn music_production_resource_is_declared_for_packaging() {
        let app = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/music");
        assert_eq!(super::declared_in(&app).unwrap(), ["music.library"]);
        let authored = tilo_app_package::Manifest::from_toml_str(
            &std::fs::read_to_string(app.join("tilo-app.toml")).unwrap(),
        )
        .unwrap();
        let desktop = tilo_app_package::desktop::desktop_manifest(
            &authored,
            "linux",
            "x86_64",
            "x86_64",
            "bin/music",
        )
        .unwrap();
        assert_eq!(desktop.resources.kinds, ["music.library"]);
        assert_eq!(
            super::declared_by_app(&app).unwrap(),
            [
                "music.library",
                "tilo.accounts",
                "tilo.feedback-draft",
                "tilo.text",
                "tilo.workbook"
            ],
            "the production dependency resources are included; SDK test fixtures are excluded"
        );
    }

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
