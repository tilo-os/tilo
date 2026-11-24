//! The styles a device's installed packages offer (`docs/designs/app-language/shell.md` §6): a
//! style is a view of the person's layout, and the layout's type is the one whose origin is
//! `session.layout`. Any installed tilolang package may declare one: each installed package's
//! module is read ([`Bundle::read`] of its payload) and every view of its that takes the layout
//! and nothing else is listed ([`offered`]).
//!
//! The device publishes what this reads in its person's own workspace ([`installed`] as the
//! workspace service's `styles::Reader`), and reads its package store again whenever it moves.

use std::path::PathBuf;

use tilo_workspace_service::styles::{Reader, Style};

use tilolang_tilo::bundle::{Bundle, BundleError};
use tilolang_tilo::runtime::meta::{Meta, Ty};

/// The origin of the layout's type: a view of a type holding it is a style.
const LAYOUT_ORIGIN: &str = "session.layout";

/// The styles of the installed packages `apps` (each its id and its payload's directory, in the
/// order the store lists them), by the rule the module docs give. `shell` is the shell's own
/// packages when the store has no `so.tilo.shell` that reads: the layout's type is declared by
/// that package, a person's copy of a shipped style names it, and the four styles it ships are
/// offered from it, so a device whose store has no shell still offers a layout to pick.
#[must_use]
pub fn offered(
    apps: &[(String, PathBuf)],
    shell: impl FnOnce() -> Vec<tilolang_tilo::tilolang::PackageMeta>,
) -> Vec<Style> {
    // Every module the store keeps, read once: a style is a view of the layout, and the
    // layout's type is whose origin says so, wherever it is declared. A package of another
    // one writes the type with its own name (`shell.Layout`), so a style's parameter is read
    // against the layout the modules together declare, not only the package's own.
    let mut modules: Vec<(String, Meta)> = Vec::new();
    let mut out = Vec::new();
    for (app, payload) in apps {
        match Bundle::read(payload) {
            Ok(bundle) => modules.push((app.clone(), bundle.meta)),
            // A package's payload with no tilolang module — a Rust app's, whose payload is a
            // binary and no `app.wasm`, or a wasm that carries no tilolang metadata — offers no
            // style: it declares no view of the layout. Neither is a blank row in the switch.
            Err(BundleError::NotAModule(_)) => {}
            Err(BundleError::Read { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound => {}
            // Something was meant as a tilolang package and does not read: the switch says so,
            // named by the package, so the person who installed it sees why rather than nothing.
            Err(error) => {
                tracing::warn!(%app, %error, "a package's styles do not read");
                out.push(unreadable(app));
            }
        }
    }
    let shell_app = tilolang_tilo::shell::SHELL_APP;
    let fallback: Vec<Meta> = if modules.iter().any(|(app, _)| app == shell_app) {
        Vec::new()
    } else {
        shell()
    };
    // The layout types, by every module's own name for them: a module of another package
    // names the layout's type as `layout_app.Name`, so a style of it carries that name.
    let layouts: Vec<(&str, &Meta)> = modules
        .iter()
        .map(|(app, meta)| (app.as_str(), meta))
        .chain(fallback.iter().map(|meta| (meta.app.as_str(), meta)))
        .collect();
    for (package, meta) in &modules {
        out.extend(styles_of(package, meta, &layouts));
    }
    // The shell's own styles, from the shell this build carries, where the store has none.
    if let Some(shell) = fallback.last() {
        out.extend(styles_of(&shell.app, shell, &layouts));
    }
    out
}

/// The styles the installed packages `apps`, as [`offered`] lists them, with the shell this
/// build carries standing in where the store has none: compiled the first time a store without
/// one is read, and kept.
#[must_use]
pub fn installed(apps: &[(String, PathBuf)]) -> Vec<Style> {
    static SHELL: std::sync::OnceLock<Vec<tilolang_tilo::tilolang::PackageMeta>> =
        std::sync::OnceLock::new();
    offered(apps, || {
        SHELL
            .get_or_init(|| match tilolang_tilo::shell::compile() {
                Ok(packages) => packages.into_iter().map(|package| package.meta).collect(),
                // Every build checks its shell compiles; one that does not offers the store's
                // styles alone, and says why.
                Err(error) => {
                    tracing::error!(%error, "the shell this build carries does not compile");
                    Vec::new()
                }
            })
            .clone()
    })
}

impl Reader for crate::Service {
    fn installed(&self, apps: &[(String, PathBuf)]) -> Vec<Style> {
        installed(apps)
    }
}

/// One app whose module did not read: a style the switch says it cannot load, named by the
/// package, so a person who installed it sees why rather than seeing nothing.
fn unreadable(package: &str) -> Style {
    Style {
        style: format!("{package}:"),
        package: package.to_owned(),
        name: String::new(),
        line: String::new(),
        loads: false,
        reason: "the package carries no description that reads".to_owned(),
    }
}

/// The views `meta` declares that draw the layout: a style is a view whose one parameter is a
/// record type whose origin is `session.layout`. The layout's type is declared once, by the
/// package that owns it (`so.tilo.shell`); another package's style names it as `shell.Layout`,
/// so `layouts` is read to know which names name the layout, never a list in code.
fn styles_of(package: &str, meta: &Meta, layouts: &[(&str, &Meta)]) -> Vec<Style> {
    use std::collections::BTreeSet;
    // The layout type as any view names it: the package declaring it, and the name the
    // declaring package gives it.
    let layout: Vec<(&str, &str)> = layouts
        .iter()
        .flat_map(|(_, meta)| {
            meta.types
                .iter()
                .filter(|t| t.origin.as_deref() == Some(LAYOUT_ORIGIN))
                .map(|t| (meta.app.as_str(), t.name.as_str()))
        })
        .collect();
    if layout.is_empty() {
        return Vec::new();
    }
    // The layout type's default view is the screen layer the shell draws a style *through*
    // (`shell_view`): offering it as a style would have it draw itself.
    let defaults: BTreeSet<&str> = meta
        .types
        .iter()
        .filter(|t| t.origin.as_deref() == Some(LAYOUT_ORIGIN))
        .filter_map(|t| meta.default_view(&t.id))
        .collect();
    let rows: BTreeSet<&str> = meta.row_views.iter().map(|r| r.view.as_str()).collect();
    meta.views
        .iter()
        // A row draws one item, not the layout whole; a presented view is a hidden view of an
        // action; the layout's default view is the dispatch. None is a style a person picks.
        .filter(|v| {
            v.presents.is_none()
                && v.row_of.is_none()
                && !rows.contains(v.id.as_str())
                && !defaults.contains(v.id.as_str())
        })
        .filter(|v| {
            matches!(&v.params[..], [only]
            if matches!(
                tilolang_tilo::runtime::meta::parse_ty(&only.ty, &meta.app),
                Ok(Ty::Record(ref key))
                    if layout
                        .iter()
                        .any(|(app, name)| *app == key.app.as_str() && *name == key.name.as_str())
            ))
        })
        .map(|v| Style {
            style: format!("{package}:{}", v.name),
            package: package.to_owned(),
            name: v.name.clone(),
            line: v.doc.lines().next().unwrap_or("").trim().to_owned(),
            loads: true,
            reason: String::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A meta of one layout type and the views that draw it, as the store's module carries
    /// it: what the device reads to find the styles (`shell.md` §6).
    fn meta(types: &serde_json::Value, views: &serde_json::Value) -> Meta {
        meta_of("so.tilo.shell", types, views)
    }

    /// [`meta`] as the package `app` carries it: another package's module names the layout by
    /// the package that declares it.
    fn meta_of(app: &str, types: &serde_json::Value, views: &serde_json::Value) -> Meta {
        serde_json::from_value(serde_json::json!({
            "app": app,
            "title": app,
            "module_hash": "",
            "fns": [],
            "actions": [],
            "types": types,
            "views": views,
        }))
        .expect("the meta reads")
    }

    fn layout_type() -> serde_json::Value {
        serde_json::json!([{
            "id": "Layout",
            "name": "Layout",
            "doc": "",
            "origin": "session.layout",
            "fields": []
        }])
    }

    fn view(name: &str, doc: &str, param_ty: &str) -> serde_json::Value {
        serde_json::json!({
            "id": name,
            "name": name,
            "doc": doc,
            "params": [{"name": "a", "type": param_ty}],
            "anchors": [],
            "reaches": []
        })
    }

    #[test]
    fn a_view_of_the_layout_is_a_style_and_another_view_is_not() {
        let meta = meta(
            &layout_type(),
            &serde_json::json!([
                view(
                    "strips",
                    "Strips\n\nThe person's places as windows.",
                    "Layout"
                ),
                // A view of another type is no style.
                view("notes", "Notes", "Note"),
            ]),
        );
        let modules = [("so.tilo.shell", &meta)];
        let found = styles_of("so.tilo.shell", &meta, &modules);
        assert_eq!(
            found.iter().map(|s| s.style.as_str()).collect::<Vec<_>>(),
            ["so.tilo.shell:strips"],
            "only a view of the layout is a style: {found:?}"
        );
        assert_eq!(
            found[0].line, "Strips",
            "listed by its doc line's first line"
        );
        assert_eq!(found[0].name, "strips");
        assert!(found[0].loads);
    }

    #[test]
    fn a_package_with_no_layout_type_offers_no_style() {
        let meta = meta(&serde_json::json!([]), &serde_json::json!([]));
        let modules = [("so.tilo.notes", &meta)];
        assert!(styles_of("so.tilo.notes", &meta, &modules).is_empty());
    }

    /// A person's copy of a shipped style names the layout by the package that declares it
    /// (`so.tilo.shell.Layout`). A store that has only the copy — a device whose shell is the
    /// build's own, no store row — still offers it: the shell the build carries declares the
    /// type the name resolves against.
    #[test]
    fn a_copied_style_is_offered_when_the_store_holds_no_shell() {
        let shell = meta_of("so.tilo.shell", &layout_type(), &serde_json::json!([]));
        let copy = meta_of(
            "mine.shell",
            &serde_json::json!([]),
            &serde_json::json!([view("strips", "Strips", "so.tilo.shell.Layout")]),
        );
        let found = styles_of(
            "mine.shell",
            &copy,
            &[("mine.shell", &copy), ("so.tilo.shell", &shell)],
        );
        assert_eq!(
            found.iter().map(|s| s.style.as_str()).collect::<Vec<_>>(),
            ["mine.shell:strips"],
            "the copy is a style of the shell the build carries: {found:?}"
        );
        // Without the shell's package in the layout names, nothing declares the type: the
        // store's own modules alone offer no style of it.
        assert!(
            styles_of("mine.shell", &copy, &[("mine.shell", &copy)]).is_empty(),
            "the copy's parameter resolves to what the shell declares"
        );
    }

    /// A store that has no shell offers the four the shell ships, from the shell this build
    /// carries: a device with nothing installed still has a layout to pick.
    #[test]
    fn a_store_with_no_shell_offers_the_shells_own_styles() {
        let offered = offered(&[], || {
            tilolang_tilo::shell::compile()
                .expect("the shell this build carries compiles")
                .into_iter()
                .map(|package| package.meta)
                .collect()
        });
        for name in ["strips", "frames", "frameless", "desktop"] {
            assert!(
                offered
                    .iter()
                    .any(|style| style.style == format!("so.tilo.shell:{name}") && style.loads),
                "the shell's {name} style is offered: {offered:?}"
            );
        }
    }

    /// A package whose module does not read is listed by its package, never by nothing: the
    /// switch names every style it lists by its doc line, its view's name, or its package, so a
    /// style with no name would be a blank row nothing can be chosen from.
    #[test]
    fn a_style_that_does_not_read_is_named_by_its_package() {
        let scratch = std::env::temp_dir().join(format!(
            "unreadable-style-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("the clock")
                .as_nanos()
        ));
        // A module that cannot be read at all (a directory where the module is): the package
        // was meant as a tilolang one, and nothing of it reads.
        std::fs::create_dir_all(scratch.join("app.wasm")).expect("a payload that does not read");
        let offered = offered(&[("atest.broken".to_owned(), scratch.clone())], Vec::new);
        std::fs::remove_dir_all(&scratch).ok();
        let reading = offered
            .iter()
            .find(|style| style.package == "atest.broken")
            .unwrap_or_else(|| panic!("the broken package is listed: {offered:?}"));
        assert!(!reading.loads, "and says it does not load: {reading:?}");
        assert!(reading.line.is_empty() && reading.name.is_empty());
    }

    /// The app tilolang reads the styles through is the one the device publishes them at.
    #[test]
    fn a_view_reads_the_styles_where_the_device_publishes_them() {
        assert_eq!(
            tilo_workspace_service::styles::STYLES_APP,
            tilolang_tilo::platform_apps::STYLES_APP
        );
    }
}
