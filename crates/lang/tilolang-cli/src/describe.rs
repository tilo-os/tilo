//! `tilolang describe`: an app's catalogue entry printed as tilolang, native and tilolang apps
//! alike.
//!
//! A native app is read from the `surface.toml` it commits next to its manifest, compiled; a
//! tilolang
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
    /// The compiled surface its package ships, as generated from the same metadata.
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
    // A secret is shown as a field that keeps what is typed out of every record, whatever it is
    // held as.
    let kind = if param.secret {
        "secret"
    } else {
        match inner_type(&param.ty) {
            "Int" | "Duration" => "integer",
            "Float" => "number",
            "Bool" => "boolean",
            "Time" => "datetime",
            _ => "text",
        }
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

/// The file an app commits its catalogue entry in, beside its manifest or its `app.tl`: what
/// search, the Store and a server running none of the app's own programs show for it. The fleet
/// crate gathers them at build time, one per app, so a change to one app writes one file.
pub const FLEET_FILE: &str = "fleet.json";
/// A native app's compiled surface, beside its manifest.
const NATIVE_SURFACE_FILE: &str = "surface.json";
/// Where a native app's manifest names no `platforms`.
const NATIVE_PLATFORMS: [&str; 3] = ["linux", "macos", "web"];
/// The declarations an app's sources make about its windows, read from its `ResourceApp`.
const PERSONAL_WINDOWS: &str = "constWINDOWS_ARE_PERSONAL:bool=true;";
const DOCUMENT_WINDOWS: &str = "constWINDOW_RESOURCE:bool=true;";

/// The catalogue entry of the app in `dir`: a native app's or a tilolang package's.
///
/// # Errors
/// As [`fleet`] and [`native_fleet`].
pub fn fleet_entry(dir: &Path, search: &[PathBuf]) -> Result<Option<FleetEntry>, String> {
    if let Some(native) = native_fleet(dir)? {
        return Ok(Some(native.entry));
    }
    fleet(dir, search)
}

/// An app's catalogue entry as the app commits it ([`FLEET_FILE`]): what [`fleet`] prints, with
/// whether search lists it and where its compiled surface lives. A tilolang package's surface is
/// generated from its own source, so it travels in the file; a native app commits its compiled
/// surface beside its manifest, and the entry names that file instead.
#[derive(Debug, Serialize)]
pub struct FleetFile {
    pub id: String,
    pub name: String,
    /// Its menu category ("Office"); empty when it declares none.
    pub category: String,
    pub platforms: Vec<String>,
    pub tagline: String,
    pub names: Vec<String>,
    pub keywords: Vec<String>,
    /// `(kind, label)` of each kind of record it creates.
    pub resource_types: Vec<(String, String)>,
    pub offers: Vec<FleetOffer>,
    pub commands: Vec<FleetCommand>,
    pub windows_personal: bool,
    pub window_documents: bool,
    /// The kinds of record each member of a workspace keeps one of (`[resources] private`).
    pub private: Vec<String>,
    pub previously: Vec<String>,
    /// Whether search and the Store offer the app; a service publishes a surface without it.
    pub listed: bool,
    /// Its compiled surface, when it is generated from the app's own source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
    /// The file beside the app that holds its compiled surface.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface_file: Option<String>,
}

impl FleetFile {
    /// `entry` as a tilolang package commits it: its surface is generated from its source, so it
    /// travels in the file, and search offers it wherever the runtime runs.
    fn of_package(entry: FleetEntry, platforms: Vec<String>) -> Self {
        Self {
            id: entry.id,
            name: entry.name,
            category: entry.category,
            platforms,
            tagline: entry.tagline,
            names: entry.names,
            keywords: entry.keywords,
            resource_types: entry.resource_types,
            offers: entry.offers,
            commands: entry.commands,
            windows_personal: entry.windows_personal,
            window_documents: entry.window_documents,
            private: Vec::new(),
            previously: entry.previously,
            listed: true,
            surface: Some(entry.surface),
            surface_file: None,
        }
    }

    /// `entry` as a native app commits it: its compiled surface is the file beside it.
    fn of_native(entry: NativeEntry) -> Self {
        Self {
            listed: entry.listed,
            private: entry.private,
            surface: None,
            surface_file: Some(NATIVE_SURFACE_FILE.to_owned()),
            ..Self::of_package(entry.entry, entry.platforms)
        }
    }
}

/// Write every app's catalogue entry under `root`: one [`FLEET_FILE`] beside each native app's
/// manifest and each tilolang package of `apps/tl` that declares an app, and none for a package
/// that no longer declares one. Answers how many it wrote.
pub fn write_fleet(root: &Path, search: &[PathBuf]) -> Result<usize, String> {
    let files = fleet_files(root, search)?;
    for (path, text) in &files {
        std::fs::write(path, text).map_err(|error| format!("{}: {error}", path.display()))?;
    }
    for stale in committed(&root.join("apps")) {
        if !files.iter().any(|(path, _)| *path == stale) {
            std::fs::remove_file(&stale)
                .map_err(|error| format!("{}: {error}", stale.display()))?;
        }
    }
    Ok(files.len())
}

/// Say whether every app's committed catalogue entry is the one its files say.
///
/// # Errors
/// Says which entries are stale or missing, and the command that writes them.
pub fn check_fleet(root: &Path, search: &[PathBuf]) -> Result<(), String> {
    let files = fleet_files(root, search)?;
    let relative = |path: &Path| {
        path.strip_prefix(root)
            .unwrap_or(path)
            .display()
            .to_string()
    };
    let mut stale: Vec<String> = Vec::new();
    for (path, text) in &files {
        match std::fs::read_to_string(path) {
            Ok(current) if current == *text => {}
            Ok(_) => stale.push(format!("{} is stale", relative(path))),
            Err(_) => stale.push(format!("{} is missing", relative(path))),
        }
    }
    for path in committed(&root.join("apps")) {
        if !files.iter().any(|(known, _)| *known == path) {
            stale.push(format!("{} names no app any more", relative(&path)));
        }
    }
    if stale.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{}\nwrite them with `tilolang describe --fleet --write`",
        stale.join("\n")
    ))
}

/// The committed catalogue entry of every app under `root`, by the file it is written to.
fn fleet_files(root: &Path, search: &[PathBuf]) -> Result<Vec<(PathBuf, String)>, String> {
    let apps = root.join("apps");
    if !apps.is_dir() {
        return Err(format!(
            "{}: no apps/ directory: name the root of a Tilo checkout",
            root.display()
        ));
    }
    let platforms = runtime_platforms(root)?;
    let mut files = Vec::new();
    for dir in sorted_dirs(&apps) {
        if let Some(entry) = native_fleet(&dir)? {
            files.push((dir.join(FLEET_FILE), json(&FleetFile::of_native(entry))?));
        }
    }
    for dir in sorted_dirs(&apps.join("tl")) {
        if !dir.join("app.tl").is_file() {
            continue;
        }
        let Some(entry) = fleet(&dir, search)? else {
            continue;
        };
        files.push((
            dir.join(FLEET_FILE),
            json(&FleetFile::of_package(entry, platforms.clone()))?,
        ));
    }
    Ok(files)
}

/// The `fleet.json` files the tree commits, an app that no longer declares one included.
fn committed(apps: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for dir in sorted_dirs(apps)
        .into_iter()
        .chain(sorted_dirs(&apps.join("tl")))
    {
        let file = dir.join(FLEET_FILE);
        if file.is_file() {
            found.push(file);
        }
    }
    found
}

/// The directories under `dir`, in the order files sort; none when `dir` does not exist.
fn sorted_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// Where a tilolang app runs: where the runtime bundle it runs on does.
fn runtime_platforms(root: &Path) -> Result<Vec<String>, String> {
    let path = root.join("crates/lang/tilolang-process/tilo-runtime.toml");
    let text =
        std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let manifest = text
        .parse::<toml::Table>()
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let platforms = manifest
        .get("package")
        .and_then(toml::Value::as_table)
        .and_then(|package| package.get("platforms"))
        .map(strings)
        .unwrap_or_default();
    if platforms.is_empty() {
        return Err(format!("{}: names no platforms", path.display()));
    }
    Ok(platforms)
}

fn json(file: &FleetFile) -> Result<String, String> {
    let mut text = serde_json::to_string_pretty(file).map_err(|error| error.to_string())?;
    text.push('\n');
    Ok(text)
}

/// The catalogue entry of the native app in `dir`, read from the files the app commits: its
/// manifest `tilo-app.toml`, its compiled `surface.json`, the words in its `search/*.ftl` and two
/// declarations in its sources. None of them is compiled. `None` when `dir` holds no native app,
/// or one that commits no surface.
fn native_fleet(dir: &Path) -> Result<Option<NativeEntry>, String> {
    let manifest_path = dir.join("tilo-app.toml");
    let surface_path = dir.join(NATIVE_SURFACE_FILE);
    if !manifest_path.is_file() || !surface_path.is_file() {
        return Ok(None);
    }
    let source = std::fs::read_to_string(&manifest_path)
        .map_err(|error| format!("{}: {error}", manifest_path.display()))?;
    let manifest = source
        .parse::<toml::Table>()
        .map_err(|error| format!("{}: {error}", manifest_path.display()))?;
    let surface = std::fs::read_to_string(&surface_path)
        .map_err(|error| format!("{}: {error}", surface_path.display()))?;
    let package = manifest.get("package").and_then(toml::Value::as_table);
    let id = package
        .and_then(|package| package.get("id"))
        .and_then(toml::Value::as_str)
        .ok_or_else(|| format!("{}: no [package] id", manifest_path.display()))?
        .to_owned();
    let name = package
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str)
        .map_or_else(|| id.clone(), ToOwned::to_owned);
    let category = package
        .and_then(|package| package.get("category"))
        .and_then(toml::Value::as_str)
        .map_or_else(String::new, ToOwned::to_owned);
    let platforms = package
        .and_then(|package| package.get("platforms"))
        .map(strings)
        .filter(|declared| !declared.is_empty())
        .unwrap_or_else(|| NATIVE_PLATFORMS.map(ToOwned::to_owned).to_vec());
    let previously = package
        .and_then(|package| package.get("previously"))
        .map(strings)
        .unwrap_or_default();
    let names = package
        .map(|package| other_names(package, &name))
        .unwrap_or_default();
    // Search and the Store offer an app showing a window; a service publishes a surface and is
    // reached by the actions of the workspace instead.
    let kind = package
        .and_then(|package| package.get("kind"))
        .and_then(toml::Value::as_str)
        .unwrap_or("app");
    let entry_type = manifest
        .get("entrypoint")
        .and_then(toml::Value::as_table)
        .and_then(|entrypoint| entrypoint.get("type"))
        .and_then(toml::Value::as_str)
        .unwrap_or("gui");
    let resources = manifest.get("resources").and_then(toml::Value::as_table);
    let private = resources
        .and_then(|resources| resources.get("private"))
        .map(strings)
        .unwrap_or_default();
    let resource_types = document_types(dir, &manifest, &id, &name, &surface);
    let tagline = native_tagline(dir);
    Ok(Some(NativeEntry {
        platforms,
        entry: FleetEntry {
            id,
            name,
            category,
            tagline,
            names,
            keywords: search_words(dir)?,
            resource_types,
            offers: manifest_offers(&manifest),
            commands: Vec::new(),
            // A window of the app opens the first resource its surface names.
            window_kind: first_kind(&surface),
            windows_personal: declares(dir, PERSONAL_WINDOWS),
            window_documents: declares(dir, DOCUMENT_WINDOWS),
            surface,
            previously,
        },
        private,
        listed: kind == "app" && entry_type == "gui",
    }))
}

/// A native app's catalogue entry, with the facts only its manifest carries.
struct NativeEntry {
    entry: FleetEntry,
    /// Where the app runs, as its manifest declares.
    platforms: Vec<String>,
    /// The kinds of record each member of a workspace keeps one of.
    private: Vec<String>,
    listed: bool,
}

/// The words an app's searches match it by: every language's `search-keywords`, English first,
/// each word once.
fn search_words(dir: &Path) -> Result<Vec<String>, String> {
    let search = dir.join("search");
    let Ok(entries) = std::fs::read_dir(&search) else {
        return Ok(Vec::new());
    };
    let mut sources: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "ftl"))
        .collect();
    sources.sort_by_key(|path| {
        (
            path.file_stem().and_then(|stem| stem.to_str()) != Some("en"),
            path.clone(),
        )
    });
    let mut words: Vec<String> = Vec::new();
    for source in sources {
        let text = std::fs::read_to_string(&source)
            .map_err(|error| format!("{}: {error}", source.display()))?;
        let found = text
            .lines()
            .find_map(|line| message(line, "search-keywords"))
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("{}: no `search-keywords` message", source.display()))?;
        for word in found.split(',') {
            let word = word.trim();
            if !word.is_empty() && !words.iter().any(|known| known.eq_ignore_ascii_case(word)) {
                words.push(word.to_owned());
            }
        }
    }
    Ok(words)
}

/// The app's English tag line, as the packer signs it into its manifest.
fn native_tagline(dir: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(dir.join("search/en.ftl")) else {
        return String::new();
    };
    text.lines()
        .find_map(|line| message(line, "app-tagline"))
        .map_or_else(String::new, ToOwned::to_owned)
}

/// The value of the FTL message `name` on one line, when the line declares it.
fn message<'line>(line: &'line str, name: &str) -> Option<&'line str> {
    line.strip_prefix(name)?
        .trim_start()
        .strip_prefix('=')
        .map(str::trim)
}

/// Its name in the other languages its catalogues translate it in.
fn other_names(package: &toml::Table, name: &str) -> Vec<String> {
    let Some(names) = package.get("names").and_then(toml::Value::as_table) else {
        return Vec::new();
    };
    let mut localized: Vec<&str> = names.values().filter_map(toml::Value::as_str).collect();
    localized.sort_unstable();
    let mut others: Vec<String> = Vec::new();
    for known in localized {
        if !known.eq_ignore_ascii_case(name)
            && !others.iter().any(|other| other.eq_ignore_ascii_case(known))
        {
            others.push((*known).to_owned());
        }
    }
    others
}

/// The kinds of document the app creates, with their labels.
///
/// Only an app that opens files makes documents. Its kind is its own when it lives in the app's
/// namespace (`sheets.workbook` for Sheets); a kind it only uses (`settings.locale`) is another
/// app's. A per-person kind (`person = true`: a watch history, a settings pane) exists once and
/// is never new, and a kind labelled with the app's own name is the app itself.
fn document_types(
    dir: &Path,
    manifest: &toml::Table,
    id: &str,
    name: &str,
    surface: &str,
) -> Vec<(String, String)> {
    let providers = manifest.get("providers").and_then(toml::Value::as_table);
    let opens_files = providers.is_some_and(|providers| {
        ["ext", "mime"]
            .iter()
            .any(|key| providers.get(*key).is_some_and(declared))
    });
    if !opens_files {
        return Vec::new();
    }
    let labels = surface_labels(surface);
    let kinds = manifest
        .get("resources")
        .and_then(toml::Value::as_table)
        .and_then(|resources| resources.get("kinds"))
        .map(strings)
        .unwrap_or_default();
    let namespaces = [
        dir.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned(),
        id.rsplit('.').next().unwrap_or_default().to_owned(),
    ];
    let own = name.to_lowercase();
    let mut types: Vec<(String, String)> = Vec::new();
    for kind in kinds {
        let label = labels
            .iter()
            .rev()
            .find(|(labelled, _)| *labelled == kind)
            .map_or("", |(_, label)| label.as_str());
        let namespace = kind.split('.').next().unwrap_or_default();
        if label.is_empty()
            || label.to_lowercase() == own
            || !namespaces.iter().any(|known| known == namespace)
            || types.iter().any(|(known, _)| *known == kind)
        {
            continue;
        }
        types.push((kind, label.to_owned()));
    }
    types
}

/// `(kind, label)` of each resource a committed surface declares that is not the person's own.
fn surface_labels(surface: &str) -> Vec<(String, String)> {
    let Some(resources) = serde_json::from_str::<serde_json::Value>(surface)
        .ok()
        .and_then(|surface| {
            surface
                .get("resources")
                .and_then(serde_json::Value::as_array)
                .cloned()
        })
    else {
        return Vec::new();
    };
    resources
        .iter()
        .filter(|resource| {
            resource
                .get("person")
                .is_none_or(|person| person.as_bool() != Some(true))
        })
        .filter_map(|resource| {
            Some((
                resource.get("kind")?.as_str()?.to_owned(),
                resource
                    .get("label")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            ))
        })
        .collect()
}

/// The first kind of record a committed surface names: the one a window of the app opens.
fn first_kind(surface: &str) -> String {
    serde_json::from_str::<serde_json::Value>(surface)
        .ok()
        .and_then(|surface| {
            surface
                .get("resources")
                .and_then(serde_json::Value::as_array)
                .and_then(|resources| {
                    resources
                        .iter()
                        .find_map(|resource| resource.get("kind")?.as_str().map(ToOwned::to_owned))
                })
        })
        .unwrap_or_default()
}

/// Whether the app's sources declare `declaration`, however they space it.
fn declares(dir: &Path, declaration: &str) -> bool {
    let mut pending = vec![dir.join("src")];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                if text.lines().any(|line| {
                    line.chars()
                        .filter(|c| !c.is_whitespace())
                        .eq(declaration.chars())
                }) {
                    return true;
                }
            }
        }
    }
    false
}

/// The actions the app's manifest offers while it is closed (`[[offers]]`).
fn manifest_offers(manifest: &toml::Table) -> Vec<FleetOffer> {
    let Some(offers) = manifest.get("offers").and_then(toml::Value::as_array) else {
        return Vec::new();
    };
    offers
        .iter()
        .filter_map(|offer| {
            let offer = offer.as_table()?;
            let field = |key: &str| offer.get(key).and_then(toml::Value::as_str);
            Some(FleetOffer {
                resource: field("resource")?.to_owned(),
                resource_kind: field("resource-kind")
                    .or_else(|| field("resource_kind"))
                    .unwrap_or_default()
                    .to_owned(),
                action: field("action")?.to_owned(),
                label: field("label")?.to_owned(),
                description: field("description").unwrap_or_default().to_owned(),
                kind: field("kind").unwrap_or("undoable").to_owned(),
                operation: field("operation").unwrap_or("effect").to_owned(),
                confirm: offer
                    .get("confirm")
                    .and_then(toml::Value::as_bool)
                    .unwrap_or(false),
                asks: manifest_asks(offer.get("asks")),
                keywords: offer.get("keywords").map(strings).unwrap_or_default(),
                icon: field("icon").unwrap_or_default().to_owned(),
            })
        })
        .collect()
}

/// The values an offer of a native manifest asks for: a manifest written before they carried
/// several names one, read as text.
fn manifest_asks(asks: Option<&toml::Value>) -> Vec<FleetAsked> {
    match asks {
        Some(toml::Value::String(name)) => vec![FleetAsked {
            name: name.clone(),
            ty: "text".to_owned(),
            description: String::new(),
        }],
        Some(toml::Value::Array(asked)) => asked
            .iter()
            .filter_map(|asked| {
                let asked = asked.as_table()?;
                Some(FleetAsked {
                    name: asked.get("name").and_then(toml::Value::as_str)?.to_owned(),
                    ty: asked
                        .get("type")
                        .and_then(toml::Value::as_str)
                        .unwrap_or("text")
                        .to_owned(),
                    description: asked
                        .get("description")
                        .and_then(toml::Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                })
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Whether a manifest value declares something: a list with a member, or words.
fn declared(value: &toml::Value) -> bool {
    match value {
        toml::Value::Array(items) => !items.is_empty(),
        toml::Value::String(words) => !words.is_empty(),
        toml::Value::Boolean(declared) => *declared,
        _ => false,
    }
}

/// The strings of a manifest list, or none when it is not one.
fn strings(value: &toml::Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(toml::Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
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

    /// A native app's directory as it commits it: its manifest, surface, search words and the
    /// declaration of its windows in its sources.
    fn native_app(root: &Path) -> PathBuf {
        let dir = root.join("apps/leaf");
        std::fs::create_dir_all(dir.join("search")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("tilo-app.toml"),
            "[package]\nid = \"t.leaf\"\nname = \"Leaf\"\ncategory = \"Office\"\n\
             platforms = [\"linux\"]\n\n[package.names]\nen = \"Leaf\"\nde = \"Blatt\"\n\n\
             [providers]\next = [\"leaf\"]\n\n[resources]\nkinds = [\"leaf.note\", \"other.thing\", \"leaf.history\"]\n\
             private = [\"leaf.history\"]\n\n\
             [[offers]]\nresource = \"leaf.note\"\naction = \"new\"\nlabel = \"New leaf\"\nasks = \"title\"\n\n\
             [[offers]]\nresource = \"leaf.note\"\naction = \"note\"\nlabel = \"Note it\"\n\
             asks = [{ name = \"words\", type = \"text\", description = \"What to write\" }]\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("surface.json"),
            r#"{"resources":[{"kind":"leaf.note","label":"Note"},{"kind":"leaf.history","label":"Your history","person":true},{"kind":"other.thing","label":"Thing"}]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("search/en.ftl"),
            "app-tagline = Leaves you write together\nsearch-keywords = leaf, note\n",
        )
        .unwrap();
        std::fs::write(dir.join("search/de.ftl"), "search-keywords = Blatt, note\n").unwrap();
        std::fs::write(
            dir.join("src/main.rs"),
            "impl ResourceApp for Leaf {\n    const WINDOW_RESOURCE: bool = true;\n}\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn a_native_apps_entry_is_what_its_own_files_say() {
        let root = tempfile::tempdir().unwrap();
        let dir = native_app(root.path());
        let entry = native_fleet(&dir)
            .expect("the app reads")
            .expect("it is an app");
        assert_eq!(entry.entry.id, "t.leaf");
        assert_eq!(entry.entry.name, "Leaf");
        assert_eq!(entry.entry.category, "Office");
        assert_eq!(entry.platforms, ["linux"]);
        assert_eq!(entry.entry.tagline, "Leaves you write together");
        assert_eq!(entry.entry.names, ["Blatt"]);
        assert_eq!(entry.entry.keywords, ["leaf", "note", "Blatt"]);
        // Its kind is its own and labelled; the kind another app owns and the person's own are
        // no document it creates.
        assert_eq!(
            entry.entry.resource_types,
            [("leaf.note".to_owned(), "Note".to_owned())]
        );
        assert_eq!(entry.private, ["leaf.history"]);
        assert!(entry.listed);
        assert!(entry.entry.window_documents);
        assert!(!entry.entry.windows_personal);
        let offers = &entry.entry.offers;
        assert_eq!(offers.len(), 2);
        assert_eq!(
            offers[0].asks,
            [FleetAsked {
                name: "title".to_owned(),
                ty: "text".to_owned(),
                description: String::new()
            }]
        );
        assert_eq!(offers[1].asks[0].description, "What to write");
    }

    #[test]
    fn every_app_commits_its_entry_beside_it() {
        let root = tempfile::tempdir().unwrap();
        let dir = native_app(root.path());
        // A tilolang package, and the runtime where the platforms it runs on are declared.
        let runtime = root.path().join("crates/lang/tilolang-process");
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::write(
            runtime.join("tilo-runtime.toml"),
            "[package]\nplatforms = [\"linux\", \"web\"]\n",
        )
        .unwrap();
        let package = root.path().join("apps/tl/thing");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(
            package.join("app.tl"),
            "app t.thing \"Thing\" (icon: .box)\n\ntype Thing {\n    name: Text\n}\n\n\
             root thing: Thing\n\ndefault view thing for Thing\n\n\
             view thing(t: Thing) {\n    text(\"Thing\")\n}\n",
        )
        .unwrap();

        assert_eq!(write_fleet(root.path(), &[]).expect("the entries write"), 2);
        assert!(dir.join(FLEET_FILE).is_file());
        assert!(package.join(FLEET_FILE).is_file());
        check_fleet(root.path(), &[]).expect("the committed entries are current");

        // The app whose search words changed is the one the check names, and writing it back
        // leaves the other app's entry as it was.
        std::fs::write(
            dir.join("search/de.ftl"),
            "search-keywords = Blatt, note, Laub\n",
        )
        .unwrap();
        let stale = check_fleet(root.path(), &[]).expect_err("the changed app's entry is stale");
        assert!(stale.contains("apps/leaf/fleet.json is stale"), "{stale}");
        assert!(!stale.contains("apps/tl/thing/fleet.json"), "{stale}");
        write_fleet(root.path(), &[]).expect("the entry writes");
        check_fleet(root.path(), &[]).expect("the committed entries are current");

        // A package that stops declaring an app loses the entry it committed.
        std::fs::write(package.join("app.tl"), "type Thing {\n    name: Text\n}\n").unwrap();
        write_fleet(root.path(), &[]).expect("the entry goes");
        assert!(!package.join(FLEET_FILE).is_file());
    }
}
