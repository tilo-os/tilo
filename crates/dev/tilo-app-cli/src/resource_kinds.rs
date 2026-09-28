//! The resource kinds an app declares with `#[resource(kind = "...")]`, read
//! from its sources so `tilo-app package` signs them into the manifest's
//! `[resources]` table. The attribute takes only a string literal, so the
//! sources name every kind the app crate declares.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};
use syn::visit_mut::VisitMut;

/// Every kind declared under `app_dir/src`, sorted and without repeats.
///
/// # Errors
///
/// Returns an error when a source file cannot be read or parsed.
pub fn declared_in(app_dir: &Path) -> Result<Vec<String>> {
    let mut kinds = Kinds::default();
    visit_sources(&app_dir.join("src"), &mut kinds)?;
    Ok(kinds.0.into_iter().collect())
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
            let mut file =
                syn::parse_file(&source).with_context(|| format!("parsing {}", path.display()))?;
            kinds.visit_file_mut(&mut file);
        }
    }
    Ok(())
}

#[derive(Default)]
struct Kinds(BTreeSet<String>);

impl VisitMut for Kinds {
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
}
