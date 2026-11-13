//! The desk styles every installed package offers (`docs/designs/app-language/shell.md` §6).
//!
//! A desk style is a view of the person's arrangement, and the arrangement's type is the one
//! whose origin is `session.arrangement`. Any installed tilolang package may declare one: the
//! session reads each installed package's module the way `seeded_apps` reads every app — its
//! payload from the store row, `Bundle::read`, and the metadata the module embeds — and lists
//! each view of its that takes the arrangement and nothing else. There is no list of styles in
//! code and no keyword: the type is the contract, as it is for every view.
//!
//! The list is read when the session starts and again when its package store moved, so a style
//! a person installs (their own, an agent's) is offered without a restart. It is published as
//! the desk's `styles` (`session.desk_style`), which the person's switch and the Desk page of
//! Settings list.

use std::collections::BTreeSet;

use tilo_session_core::shell::DeskStyleReading;
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::runtime::meta::{Meta, Ty};

/// The origin of the arrangement's type: a view of a type holding it is a desk style.
const ARRANGEMENT_ORIGIN: &str = "session.arrangement";

/// What the person's package store's registry says now.
///
/// Read on every frame, so this is the cheap reading that decides whether the store has to be
/// read again: the registry file's length and modification time move when a package is installed
/// or taken away, and a store that reads the same is not read again — a frame with nothing
/// installed costs one `stat`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Registry {
    /// The registry as it stands.
    Reads(String),
    /// The store holds no registry file yet: nothing was ever installed from it. That is how
    /// every device starts, and a store that holds nothing is not a failure to report.
    Absent,
    /// The registry is there and does not read: why, for the one line the log says of it.
    Unreadable(String),
}

/// [`Registry`] as the person's store stands now.
#[must_use]
pub fn registry(catalog: &package_store::Catalog) -> Registry {
    match std::fs::metadata(catalog.user().registry()) {
        Ok(meta) => {
            let modified = meta
                .modified()
                .ok()
                .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |since| since.as_nanos());
            Registry::Reads(format!("{}:{modified}", meta.len()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Registry::Absent,
        Err(error) => Registry::Unreadable(error.to_string()),
    }
}

/// What the log has to say about the store now, given what it said last: `None` while it says the
/// same thing. The store is read on every frame, so the same failure is news only when it starts,
/// and the frame that finds the store reading again is the one that says so.
#[must_use]
pub fn news(former: Option<&Registry>, now: &Registry) -> Option<News> {
    if former == Some(now) {
        return None;
    }
    match now {
        Registry::Unreadable(why) => Some(News::Unreadable(why.clone())),
        Registry::Reads(_) | Registry::Absent => {
            matches!(former, Some(Registry::Unreadable(_))).then_some(News::ReadsAgain)
        }
    }
}

/// What changed about the store since the last frame, as the log says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum News {
    /// The registry does not read any more, and why.
    Unreadable(String),
    /// The registry reads again after a frame that found it would not.
    ReadsAgain,
}

/// The styles the shell's own package offers — the four that ship — from the packages the shell
/// runs, whether they came from the store or this build's fallback copy (`shell.md` §6). The
/// switch always offers them, so a device with no store still shows a desk.
pub fn of_shell(packages: &[tilolang_tilo::tilolang::Compiled]) -> Vec<DeskStyleReading> {
    let metas: Vec<(String, Meta)> = packages
        .iter()
        .map(|package| (package.meta.app.clone(), package.meta.clone()))
        .collect();
    match metas.last() {
        Some((package, meta)) => styles_of(package, meta, &metas),
        None => Vec::new(),
    }
}

/// The desk styles every installed package offers, in the order the store lists its apps.
///
/// `loads` says whether the style's package compiles enough to run: a module whose metadata
/// reads is one whose views are declared, and the shell's runtime finds them by the type they
/// take. `false` for one whose payload does not read, so the switch can say so without the
/// style drawing anything.
pub fn installed(catalog: &package_store::Catalog) -> Vec<DeskStyleReading> {
    let Ok(apps) = app_catalogue::apps::read(catalog) else {
        return Vec::new();
    };
    // Every module the store holds, read once: a style is a view of the arrangement, and the
    // arrangement's type is whose origin says so, wherever it is declared. A package of another
    // one writes the type with its own name (`shell.Arrangement`), so a style's parameter is
    // read against the arrangement the modules together declare, not only the package's own.
    let mut modules = Vec::new();
    let mut broken = Vec::new();
    for app in &apps {
        match Bundle::read(&app.payload_root) {
            Ok(bundle) => modules.push((app.app_id.to_string(), bundle.meta)),
            // A package's payload with no tilolang module — a Rust app's, whose payload is a
            // binary and no `app.wasm`, or a wasm that carries no tilolang metadata — offers no
            // style: it declares no view of the arrangement. The file simply not being there is
            // the Rust app's case (`BundleError::Read` with `NotFound`), which is no more broken
            // than a wasm with no metadata section: both are packages that write no styles, and
            // neither is a blank row in the switch.
            Err(tilolang_tilo::bundle::BundleError::NotAModule(_)) => {}
            Err(tilolang_tilo::bundle::BundleError::Read { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound => {}
            // Something was meant as a tilolang package and does not read: the switch says so,
            // named by the package, so the person who installed it sees why rather than nothing.
            Err(error) => {
                tracing::warn!(app = %app.app_id, %error, "a package's styles do not read");
                broken.push(unreadable(&app.app_id));
            }
        }
    }
    // The arrangement types, by every module's own name for them: a module of another package
    // names the arrangement's type as `arrangement_app.Name`, so a style of it holds that name.
    let mut out = broken;
    for (package, meta) in &modules {
        out.extend(styles_of(package, meta, &modules));
    }
    out
}

/// The compiled module of the installed package `app`, from the person's store: what a view of
/// its that the shell composes runs as, when the session's own services keep no records of it.
///
/// A desk style is a view of another installed package, and no session service runs that
/// package's records — a style is code and nothing else — so the module is read from the
/// store's row the way `installed_shell` reads the shell's (`Bundle::read`). Without it, a
/// style the person chose would wait for a package nothing would ever publish.
#[must_use]
pub fn module_from_store(app: &str) -> Option<std::sync::Arc<[u8]>> {
    // The shell's own package is what the part runs, never one it loads beside itself: the
    // store holds it, and sending it here would load a second copy.
    if app == tilo_session_core::shell::SHELL_APP {
        return None;
    }
    let catalog = package_store::Catalog::from_env().ok()?;
    let installed = app_catalogue::apps::read_one(&catalog, app)
        .ok()
        .flatten()?;
    let bundle = Bundle::read(&installed.payload_root).ok()?;
    Some(std::sync::Arc::from(bundle.wasm))
}

/// One app whose module did not read: a style the switch says it cannot load, named by the
/// package, so a person who installed it sees why rather than seeing nothing.
fn unreadable(package: &str) -> DeskStyleReading {
    DeskStyleReading {
        style: format!("{package}:"),
        package: package.to_owned(),
        name: String::new(),
        line: String::new(),
        loads: false,
        reason: "the package carries no description that reads".to_owned(),
    }
}

/// The views `meta` declares that draw the arrangement: a desk style is a view whose one
/// parameter is a record type whose origin is `session.arrangement`. The arrangement's type is
/// declared once, by the package that owns it (`so.tilo.shell`); another package's style names
/// it as `shell.Arrangement`, so `modules` is read to know which names hold the arrangement,
/// never a list in code (`shell.md` §6).
fn styles_of(package: &str, meta: &Meta, modules: &[(String, Meta)]) -> Vec<DeskStyleReading> {
    // The arrangement type as any view names it: the package declaring it, and the name the
    // declaring package gives it. `parse_ty`, below, turns a style's parameter into the same
    // app and name, whether the style writes `Arrangement` (the declaring package) or
    // `shell.Arrangement` (another one).
    let arrangement: Vec<(String, String)> = modules
        .iter()
        .flat_map(|(_, meta)| {
            meta.types
                .iter()
                .filter(|t| t.origin.as_deref() == Some(ARRANGEMENT_ORIGIN))
                .map(|t| (meta.app.clone(), t.name.clone()))
        })
        .collect();
    // A module that declares none of its own still offers none: a style must draw the
    // arrangement this session keeps, and the module's views are found by the type they take.
    if arrangement.is_empty() {
        return Vec::new();
    }
    // The arrangement type's default view is the screen layer the shell draws a style *through*
    // (`desk_view`): offering it as a style would have it draw itself. Its `show` picks the
    // style, so it is the dispatch, never one of the choices.
    let defaults: BTreeSet<&str> = meta
        .types
        .iter()
        .filter(|t| t.origin.as_deref() == Some(ARRANGEMENT_ORIGIN))
        .filter_map(|t| meta.default_view(&t.id))
        .collect();
    let rows: BTreeSet<&str> = meta.row_views.iter().map(|r| r.view.as_str()).collect();
    meta.views
        .iter()
        // A row draws one item, not the arrangement whole; a presented view is a hidden view
        // of an action; the arrangement's default view is the dispatch. None is a style a
        // person picks.
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
                    if arrangement
                        .iter()
                        .any(|(app, name)| app == &key.app && name == &key.name)
            ))
        })
        .map(|v| DeskStyleReading {
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

    /// A meta of one arrangement type and the views that draw it, as the store's module carries
    /// it: what the session reads to find the desk styles (`shell.md` §6).
    fn meta(types: &serde_json::Value, views: &serde_json::Value) -> Meta {
        serde_json::from_value(serde_json::json!({
            "app": "so.tilo.shell",
            "title": "Shell",
            "module_hash": "",
            "fns": [],
            "actions": [],
            "types": types,
            "views": views,
        }))
        .expect("the meta reads")
    }

    fn arrangement_type() -> serde_json::Value {
        serde_json::json!([{
            "id": "Arrangement",
            "name": "Arrangement",
            "doc": "",
            "origin": "session.arrangement",
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
    fn a_store_that_did_not_change_says_nothing_and_a_change_is_said_each_way() {
        let absent = Registry::Absent;
        let reads = Registry::Reads("12:34".to_owned());
        let broken = Registry::Unreadable("permission denied".to_owned());
        // A frame that finds the store as the frame before it did says nothing: the store is read
        // on every frame, and a line repeated that often is not news.
        assert_eq!(
            news(None, &absent),
            None,
            "nothing installed is not a failure"
        );
        assert_eq!(news(Some(&absent), &absent), None);
        assert_eq!(news(Some(&reads), &reads), None);
        assert_eq!(news(Some(&broken), &broken), None);
        // The frame that finds the store broken says so, and how; the one that finds it reading
        // again says that.
        assert_eq!(
            news(Some(&absent), &broken),
            Some(News::Unreadable("permission denied".to_owned()))
        );
        assert_eq!(
            news(Some(&broken), &absent),
            Some(News::ReadsAgain),
            "a store whose registry was taken away reads again"
        );
        assert_eq!(news(Some(&broken), &reads), Some(News::ReadsAgain));
        // A store that moved while it was readable is no news: its styles are read again, and
        // nothing about reading it changed.
        assert_eq!(news(Some(&reads), &absent), None);
    }

    #[test]
    fn a_view_of_the_arrangement_is_a_desk_style_and_another_view_is_not() {
        let meta = meta(
            &arrangement_type(),
            &serde_json::json!([
                view(
                    "strips",
                    "Strips\n\nThe person's places as windows.",
                    "Arrangement"
                ),
                // A view of another type is no style.
                view("notes", "Notes", "Desk"),
            ]),
        );
        let modules = [("so.tilo.shell".to_owned(), meta.clone())];
        let found = styles_of("so.tilo.shell", &meta, &modules);
        assert_eq!(
            found.iter().map(|s| s.style.as_str()).collect::<Vec<_>>(),
            ["so.tilo.shell:strips"],
            "only a view of the arrangement is a style: {found:?}"
        );
        assert_eq!(
            found[0].line, "Strips",
            "listed by its doc line's first line"
        );
        assert_eq!(found[0].name, "strips");
        assert!(found[0].loads);
    }

    #[test]
    fn a_package_with_no_arrangement_type_offers_no_style() {
        let meta = meta(&serde_json::json!([]), &serde_json::json!([]));
        let modules = [("so.tilo.notes".to_owned(), meta.clone())];
        assert!(styles_of("so.tilo.notes", &meta, &modules).is_empty());
    }

    /// A package whose module does not read is listed by its package, never by nothing: the
    /// switch names every style it lists (`style_label`: its doc line, its view's name, or its
    /// package), so a style with no name is a blank row above the menu's first row that nothing
    /// can be chosen from — the live run's empty band above "Strips".
    #[test]
    fn a_style_that_does_not_read_is_named_by_its_package() {
        let reading = unreadable("atest.broken");
        assert!(
            !reading.package.is_empty(),
            "the switch names the broken style by its package: {reading:?}"
        );
        assert!(!reading.loads, "and says it does not load: {reading:?}");
        // The name the switch reads off it, as `style_label` reads it: the doc line, else the
        // view's name, else the package — never empty.
        let label = if !reading.line.is_empty() {
            &reading.line
        } else if !reading.name.is_empty() {
            &reading.name
        } else {
            &reading.package
        };
        assert_eq!(
            label, "atest.broken",
            "the broken style reads as its package, never as nothing"
        );
    }
}
