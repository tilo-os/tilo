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
    /// Every action its windows run on keys, with the keys it declares: what a person picks from
    /// to move one to other keys.
    pub commands: Vec<FleetCommand>,
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
    /// The values it asks for before it runs, each with its type and the words beside its field.
    pub asks: Vec<FleetAsked>,
    pub keywords: Vec<String>,
    pub icon: String,
}

/// One action a window of the app runs on keys (`key: "Ctrl+Enter"`).
#[derive(Debug, Serialize)]
pub struct FleetCommand {
    /// The action's id, as it is published and as a moved command names it.
    pub action: String,
    pub label: String,
    /// The keys the app gives it.
    pub key: String,
}

/// One value an offer asks for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FleetAsked {
    pub name: String,
    /// What it takes, as a form's parameter type spells it (`text`, `secret`, `integer`…).
    #[serde(rename = "type")]
    pub ty: String,
    pub description: String,
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
        commands: commands(meta),
        window_kind: meta.resource_type(),
        windows_personal: meta.windows_are_personal(),
        window_documents: meta.windows_hold_documents(),
        surface: String::from_utf8_lossy(&surface).into_owned(),
        previously: meta.previously.clone(),
    }))
}

/// The actions of `meta` its windows run on keys, in the order the app declares them.
fn commands(meta: &PackageMeta) -> Vec<FleetCommand> {
    meta.actions
        .iter()
        .filter(|action| !action.scenario)
        .filter_map(|action| {
            Some(FleetCommand {
                action: tilolang_tilo::meta::published_name(action).to_owned(),
                label: action.label.clone(),
                key: action.key.clone()?,
            })
        })
        .collect()
}

/// The actions `meta` offers while it is closed: each primary action, and each command on the
/// package rather than a record. An action asking several values is offered with them all: a
/// small form fills the parameter's own type, and the words beside each field are its doc.
///
/// An action acts on the package when its target is the app's root: it needs no record at hand,
/// so it is a command a person starts with the app closed ("Save a place"). One the app runs by
/// itself is not: a trigger's handler, or a step that carries no words for a person and that
/// only the app's own code calls.
fn offers(meta: &PackageMeta) -> Vec<FleetOffer> {
    let mut offers = Vec::new();
    // The actions the app runs on its own clock: nobody starts them.
    let automatic: Vec<&str> = meta.triggers.iter().map(|t| t.action.as_str()).collect();
    let root = meta.resource_type();
    for action in meta.offered_actions() {
        let target = meta.target_of(&action.params);
        let primary = action.role.as_deref() == Some("primary");
        let on_the_package = target.is_some_and(|ty| meta.kind_of(&ty.name) == root);
        // What the app presents as something a person does: an icon, a shortcut, words it wrote
        // about the action, or a name other than the one its identifier would read as. An action
        // with none of them is a step only the app's own code runs, and no name to offer it by.
        let a_command = action.icon.is_some()
            || action.key.is_some()
            || !action.doc.trim().is_empty()
            || action.label != tilolang::resolve::sentence_case(&action.name);
        if !primary && (!on_the_package || !a_command || automatic.contains(&action.id.as_str())) {
            continue;
        }
        let asked: Vec<&ParamMeta> = meta
            .arguments(&action.params)
            .iter()
            .filter(|param| param.default.is_none())
            .collect();
        // A value a form cannot show here (a record, a list) keeps the action out: its own view
        // is where a person gives that.
        if asked.iter().any(|param| !asked_in_a_field(&param.ty)) {
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
            asks: asked.iter().map(|param| asked_of(param)).collect(),
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
            asks: vec![FleetAsked {
                name: ui::tilo_actions::remote::OPEN_LINK_PARAM.to_owned(),
                ty: "text".to_owned(),
                description: String::new(),
            }],
            keywords: Vec::new(),
            icon: String::new(),
        });
    }
    offers
}

/// Whether a form field gives a value of this parameter type: the scalars a person types. A
/// record, a list or anything else is given where its own view shows it.
fn asked_in_a_field(ty: &str) -> bool {
    matches!(
        inner_type(ty),
        "Text"
            | "RichText"
            | "Date"
            | "Time"
            | "Identifier"
            | "Int"
            | "Float"
            | "Bool"
            | "Duration"
    )
}

/// What an offer says of the value `param` asks: its name, the type the form shows and the words
/// beside its field.
fn asked_of(param: &ParamMeta) -> FleetAsked {
    let kind = match inner_type(&param.ty) {
        "Int" | "Duration" => "integer",
        "Float" => "number",
        "Bool" => "boolean",
        "Time" => "datetime",
        _ => "text",
    };
    FleetAsked {
        name: param.name.clone(),
        ty: kind.to_owned(),
        description: param.words(),
    }
}

/// A parameter type with the `Option` around it gone and the type inside a container named.
fn inner_type(ty: &str) -> &str {
    let ty = ty.trim();
    let ty = ty
        .strip_prefix("Option<")
        .and_then(|rest| rest.strip_suffix('>'))
        .unwrap_or(ty)
        .trim();
    ty.split('<').next().unwrap_or(ty).trim()
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
            (link.kind.as_str(), link.operation.as_str()),
            ("read", "view")
        );
        assert_eq!(
            link.asks
                .iter()
                .map(|asked| asked.name.as_str())
                .collect::<Vec<_>>(),
            ["link"]
        );
    }

    #[test]
    fn an_action_asking_two_values_is_offered_with_both_its_fields() {
        let dir = package(
            "app t.vault \"Vault\" (icon: .key, category: .office)\n\n\
             type Vault {\n    items: Collection<Item>\n}\n\n\
             type Item {\n    name: Text\n}\n\n\
             root vault: Vault\n\n\
             default view vault for Vault\n\n\
             view vault(v: Vault) {\n    text(\"Vault\")\n}\n\n\
             primary action add_item(\n    v: Vault,\n    issuer: Text (label: \"Issuer\"),\n    \
             secret: Text (label: \"Shared secret\"),\n) -> Item {\n    v.items.insert(Item(name: issuer))\n}\n",
        );
        let entry = fleet(dir.path(), &[])
            .expect("the package reads")
            .expect("it declares an app");
        let offer = entry.offers.first().expect("it offers its primary action");
        assert_eq!(offer.action, "add_item");
        assert_eq!(
            offer
                .asks
                .iter()
                .map(|asked| (
                    asked.name.as_str(),
                    asked.ty.as_str(),
                    asked.description.as_str()
                ))
                .collect::<Vec<_>>(),
            [
                ("issuer", "text", "Issuer"),
                ("secret", "text", "Shared secret"),
            ],
            "both values are offered, with the words beside their fields"
        );
    }

    #[test]
    fn a_command_on_the_app_itself_is_offered_while_it_is_closed() {
        let dir = package(
            "app t.weather \"Weather\" (icon: .cloud_sun, category: .utility)\n\n\
             type Place (title: name) {\n    name: Text\n    latitude: Float\n    longitude: Float\n}\n\n\
             type Locations {\n    places: Collection<Place>\n}\n\n\
             root weather: Locations\n\n\
             default view weather_window for Locations\n\n\
             view weather_window(w: Locations) {\n    text(\"Weather\")\n}\n\n\
             /// Saves a place a person follows.\n\
             action save_place(\n    w: Locations,\n    name: Text,\n    \
             latitude: Float,\n    longitude: Float,\n) -> Place (icon: .plus, label: \"Save a place\") {\n    \
             w.places.insert(Place(name: name, latitude: latitude, longitude: longitude))\n}\n\n\
             action read_next(w: Locations) {\n    poll(w)\n}\n\n\
             action poll(w: Locations) {\n    read_next(w)\n}\n\n\
             trigger watch(w: Locations) every 1s: poll(w)\n",
        );
        let entry = fleet(dir.path(), &[])
            .expect("the package reads")
            .expect("it declares an app");
        let offered: Vec<(String, String)> = entry
            .offers
            .iter()
            .map(|offer| (offer.action.clone(), offer.label.clone()))
            .collect();
        assert!(
            offered.contains(&("save_place".to_owned(), "Save a place".to_owned())),
            "a command on the app needs no record at hand: {offered:?}"
        );
        assert_eq!(
            entry
                .offers
                .iter()
                .find(|offer| offer.action == "save_place")
                .expect("it offers saving a place")
                .asks
                .iter()
                .map(|asked| asked.name.as_str())
                .collect::<Vec<_>>(),
            ["name", "latitude", "longitude"],
            "the form asks what the action needs, and not its target"
        );
        assert!(
            !offered.iter().any(|(action, _)| action == "read_next"),
            "a step only the app's own code runs is no command: {offered:?}"
        );
        assert!(
            !offered.iter().any(|(action, _)| action == "poll"),
            "a trigger's handler runs on its clock, never from a menu: {offered:?}"
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
