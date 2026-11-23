//! `tilolang describe`: an app's catalogue entry printed as tilolang, native and tilolang apps
//! alike.
//!
//! A native app is read from the `surface.toml` it commits next to its manifest; a tilolang
//! package is printed from its own metadata, as it was compiled. Either way what is printed is
//! what a package that `use`s the app compiles against.
//!
//! `tilolang describe --fleet` prints the same app as the catalogue fleet reads it (where search,
//! the Store and a server that runs none of its apps show it): the facts a native app writes into
//! its manifest, read from the package's own declaration instead.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tilolang::ids::SeededEntropy;
use tilolang::meta::{PackageMeta, ParamMeta};
use tilolang::types::EffectClass;

use tilolang_tilo::catalogue;
use tilolang_tilo::meta::MetaExt as _;

/// The catalogue's package of the app in `dir`: a native app's
/// committed surface, or the metadata of the tilolang package there,
/// compiled with the packages it uses (found in `search`, else among
/// its siblings).
pub fn published(dir: &Path, search: &[PathBuf]) -> Result<PackageMeta, String> {
    if let Some(meta) = catalogue::native_app(dir)? {
        return Ok(meta);
    }
    let (built, _) =
        crate::compile(dir, search, &mut SeededEntropy(0)).map_err(|error| error.to_string())?;
    let meta = &built
        .last()
        .ok_or_else(|| format!("{}: no package", dir.display()))?
        .compiled
        .meta;
    // The package's own declarations, as a package that `use`s it compiles against them: its generated
    // surface names each action by the id a change of it names, which is no word an author reads here.
    Ok(meta.clone())
}

/// What `tilolang describe` prints for the app in `dir`. # Errors As [`published`].
pub fn describe(dir: &Path, search: &[PathBuf]) -> Result<String, String> {
    published(dir, search).map(|meta| tilolang::describe::declarations(&meta))
}

/// An app as the catalogue fleet knows it while it is not installed: the facts a native app's
/// signed manifest carries, read from the tilolang package's own declaration.
#[derive(Debug, Serialize)]
pub struct FleetEntry {
    pub id: String,
    pub name: String,
    /// Its menu category ("Office"); empty when it declares none.
    pub category: String,
    /// What it does in one plain phrase; empty when its header declares none.
    pub tagline: String,
    /// Its name in the other languages its catalogues translate it in.
    pub names: Vec<String>,
    /// The other words people know it by, which search matches.
    pub keywords: Vec<String>,
    /// `(kind, label)` of each kind of record it declares.
    pub resource_types: Vec<(String, String)>,
    /// The actions it offers while closed.
    pub offers: Vec<FleetOffer>,
    /// The kind of the record a window of it opens.
    pub window_kind: String,
    /// Whether a window of it is the person's own, never on a shared desktop.
    pub windows_personal: bool,
    /// Whether each window shows a document of its own rather than the workspace's.
    pub window_documents: bool,
    /// The signed `surface.toml` its package ships, as generated from the same metadata.
    pub surface: String,
    /// The ids it was published under before.
    pub previously: Vec<String>,
}

/// One action an app offers while closed, as its manifest's `[[offers]]` declare it.
#[derive(Debug, Serialize)]
pub struct FleetOffer {
    pub resource: String,
    /// The kind of that resource; empty when it is the resource itself.
    pub resource_kind: String,
    pub action: String,
    pub label: String,
    pub description: String,
    /// `read`, `undoable` or `outside`.
    pub kind: String,
    /// `document`, `effect` or `view`.
    pub operation: String,
    pub confirm: bool,
    /// The parameter one typed line fills; empty when it asks for none.
    pub asks: String,
    pub keywords: Vec<String>,
    pub icon: String,
}

/// The fleet entry of the tilolang package in `dir`, or `None` when it declares no app (a
/// package of views the app it belongs to compiles in). # Errors As [`published`].
pub fn fleet(dir: &Path, search: &[PathBuf]) -> Result<Option<FleetEntry>, String> {
    let source = tilolang::source::PackageSource::read_dir(dir)?;
    if source.header().0.is_none() {
        return Ok(None);
    }
    let (built, _) =
        crate::compile(dir, search, &mut SeededEntropy(0)).map_err(|error| error.to_string())?;
    let (last, used) = built
        .split_last()
        .ok_or_else(|| format!("{}: no package", dir.display()))?;
    let meta = &last.compiled.meta;
    let bundle =
        tilolang_tilo::bundle::Bundle::new(&last.compiled, used.iter().map(|used| &used.compiled));
    let surface = bundle.surface().map_err(|error| error.to_string())?;
    let key = tilolang::messages::key_of(&meta.app);
    let mut names = Vec::new();
    for (_, catalogue) in &last.source.locales {
        let Some(name) = tilolang::messages::word(catalogue, &key) else {
            continue;
        };
        if !name.eq_ignore_ascii_case(&meta.title)
            && !names
                .iter()
                .any(|known: &String| known.eq_ignore_ascii_case(&name))
        {
            names.push(name);
        }
    }
    // The other words people know it by, in every language it ships: the header's own, then each
    // catalogue's `d-<app>-keywords`, without repeats.
    let mut keywords = meta.keywords.clone();
    for (_, catalogue) in &last.source.locales {
        for word in tilolang::messages::keywords_word(catalogue, &meta.app).unwrap_or_default() {
            if !keywords
                .iter()
                .any(|known| known.eq_ignore_ascii_case(&word))
            {
                keywords.push(word);
            }
        }
    }
    let title = meta.title.to_lowercase();
    let resource_types = meta
        .types
        .iter()
        .map(|ty| (meta.kind_of(&ty.name), tilolang::messages::label(&ty.name)))
        .filter(|(_, label)| label.to_lowercase() != title)
        .collect();
    Ok(Some(FleetEntry {
        id: meta.app.clone(),
        name: meta.title.clone(),
        category: meta.category.as_deref().map_or(String::new(), |declared| {
            crate::package::menu_category(declared).to_owned()
        }),
        tagline: meta.tagline.clone().unwrap_or_default(),
        names,
        keywords,
        resource_types,
        offers: offers(meta),
        window_kind: meta.resource_type(),
        windows_personal: meta.profile_resources,
        window_documents: !meta.each_document.is_empty(),
        surface: String::from_utf8_lossy(&surface).into_owned(),
        previously: meta.previously.clone(),
    }))
}

/// The actions `meta` offers while it is closed: each primary action, and each action on the
/// package rather than a record, that a menu can run without asking more than one thing.
fn offers(meta: &PackageMeta) -> Vec<FleetOffer> {
    let mut offers = Vec::new();
    for action in meta.offered_actions() {
        let target = meta.target_of(&action.params);
        let primary = action.role.as_deref() == Some("primary");
        if !primary && target.is_some() {
            continue;
        }
        let asked: Vec<&ParamMeta> = meta
            .arguments(&action.params)
            .iter()
            .filter(|param| param.default.is_none())
            .collect();
        if asked.len() > 1 {
            continue;
        }
        let action_kind = match action.class {
            EffectClass::Read => "read",
            EffectClass::Undoable => "undoable",
            EffectClass::Outside => "outside",
        };
        // A change of a record is a change of the document holding it; anything else a closed
        // menu starts is an effect.
        let operation = if target.is_some() && action.class == EffectClass::Undoable {
            "document"
        } else {
            "effect"
        };
        offers.push(FleetOffer {
            resource: target.map_or_else(|| meta.package_kind(), |ty| meta.kind_of(&ty.name)),
            resource_kind: String::new(),
            action: action.name.clone(),
            label: if action.label.is_empty() {
                action.name.clone()
            } else {
                action.label.clone()
            },
            description: action
                .doc
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_owned(),
            kind: action_kind.to_owned(),
            operation: operation.to_owned(),
            confirm: action.role.as_deref() == Some("destructive"),
            asks: asked
                .first()
                .map_or(String::new(), |param| param.name.clone()),
            keywords: Vec::new(),
            icon: action.icon.clone().unwrap_or_default(),
        });
    }
    // The system's `open-link`, which a shell hands the app a link through: the one action an
    // app offers on its root that takes a line the caller supplies.
    if let Some(offer) = crate::package::link_offer(meta) {
        offers.push(FleetOffer {
            resource: offer.resource,
            resource_kind: offer.kind,
            action: ui::tilo_actions::remote::APPLICATION_OPEN_LINK_ACTION.to_owned(),
            label: "Open link".to_owned(),
            description: String::new(),
            kind: "read".to_owned(),
            operation: "view".to_owned(),
            confirm: false,
            asks: ui::tilo_actions::remote::OPEN_LINK_PARAM.to_owned(),
            keywords: Vec::new(),
            icon: String::new(),
        });
    }
    offers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(text: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("app.tl"), text).unwrap();
        dir
    }

    #[test]
    fn the_fleet_reads_keywords_in_every_language() {
        let dir = package(
            "app t.notes \"Notes\" (\n    keywords: [\"note\", \"memo\"],\n)\n\n\
             type Notebook {\n    notes: List<Text>\n}\n\n\
             root notebook: Notebook\n\n\
             default view notebook for Notebook\n\n\
             view notebook(nb: Notebook) {\n    text(\"Notes\")\n}\n",
        );
        std::fs::create_dir_all(dir.path().join("locales")).unwrap();
        std::fs::write(
            dir.path().join("locales/de-DE.ftl"),
            "d-t-notes = Notizen\n\nd-t-notes-keywords = Notizen, Merkzettel\n",
        )
        .unwrap();
        let entry = fleet(dir.path(), &[])
            .expect("the package reads")
            .expect("it declares an app");
        assert_eq!(
            entry.keywords,
            ["note", "memo", "Notizen", "Merkzettel"],
            "the catalogues' keywords join the header's, once each"
        );
    }

    #[test]
    fn the_fleet_reads_a_packages_own_declaration() {
        let dir = package(
            "app t.notes \"Notes\" (\n    icon: .file_text,\n    category: .office,\n    \
             tagline: \"Notes you write together\",\n    keywords: [\"note\", \"memo\"],\n    \
             schemes: \"t.notes\",\n)\n\n\
             type Note {\n    title: Text\n}\n\n\
             type Notebook {\n    notes: Collection<Note>\n}\n\n\
             root notebook: Notebook\n\n\
             default view notebook for Notebook\n\n\
             view notebook(nb: Notebook) {\n    text(\"Notes\")\n}\n\n\
             primary action new_note(nb: Notebook, title: Text = \"\") -> Note (icon: .plus) {\n    \
             nb.notes.insert(Note(title: title))\n}\n\n\
             action open_link(nb: Notebook, link: Text) -> host.Resource {\n    nb\n}\n",
        );
        let entry = fleet(dir.path(), &[])
            .expect("the package reads")
            .expect("it declares an app");
        assert_eq!(entry.id, "t.notes");
        assert_eq!(entry.name, "Notes");
        assert_eq!(entry.category, "Office");
        assert_eq!(entry.tagline, "Notes you write together");
        assert_eq!(entry.keywords, ["note", "memo"]);
        assert!(
            entry
                .resource_types
                .contains(&("tl.t.notes.Notebook".to_owned(), "Notebook".to_owned()))
        );
        assert_eq!(entry.window_kind, "tl.t.notes.Notebook");
        assert!(entry.surface.contains("tl.t.notes.Notebook"));
        let offer = entry.offers.first().expect("it offers its primary action");
        assert_eq!(offer.resource, "tl.t.notes.Notebook");
        assert_eq!(offer.action, "new_note");
        assert_eq!(offer.label, "New note");
        assert_eq!(
            (offer.kind.as_str(), offer.operation.as_str()),
            ("undoable", "document")
        );
        // An app that opens links offers the system's `open-link` on its root, where the manifest
        // declares it: what a shell hands a link to while no window of the app is open.
        let link = entry
            .offers
            .iter()
            .find(|offer| offer.action == "open-link")
            .expect("it offers the link it opens");
        assert_eq!(link.resource_kind, "tl.t.notes.Notebook");
        assert!(
            link.resource.starts_with("tl:t.notes:root-"),
            "the offer stands on the root: {}",
            link.resource
        );
        assert_eq!(
            (
                link.kind.as_str(),
                link.operation.as_str(),
                link.asks.as_str()
            ),
            ("read", "view", "link")
        );
    }

    #[test]
    fn a_package_that_takes_files_offers_those_actions_too() {
        let dir = package(
            "app t.import \"Import\" (\n    icon: .file_plus,\n)\n\n\
             type Page {\n    body: Text\n}\n\n\
             root pages: Page\n\n\
             default view pages for Page\n\n\
             view pages(p: Page) {\n    text(\"Pages\")\n}\n\n\
             /// Imports a file as a page.\n\
             action import_page(p: Page, file: File) {\n    p.body = file.name\n}\n",
        );
        // The surface the SDK and the fleet read names the action that takes a file, and the
        // declarations name the parameter as the file it is.
        let entry = fleet(dir.path(), &[])
            .expect("the package reads")
            .expect("it declares an app");
        assert!(
            entry.surface.contains("Imports a file as a page"),
            "the published surface lists it: {}",
            entry.surface
        );
        let declared = describe(dir.path(), &[]).expect("the package describes");
        assert!(
            declared.contains("action import_page(p: Page, file: File)"),
            "{declared}"
        );
    }

    #[test]
    fn a_package_that_declares_no_app_is_no_fleet_app() {
        let dir = package("type N {\n    a: Int\n}\n");
        assert!(fleet(dir.path(), &[]).expect("the source reads").is_none());
    }
}
