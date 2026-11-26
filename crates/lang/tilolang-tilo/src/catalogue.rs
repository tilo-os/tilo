//! What an app publishes, read as a tilolang package.
//!
//! The catalogue is the one description of an app: its resources' surfaces
//! (`tilo_actions::surface::Surface`), whichever language the app is written in. Read as a
//! package, a surface's resource kinds are its types, the documents and enums its fields name are
//! types and enums, and its verbs are actions on their resource, each with an id naming that
//! type and the verb (`Banners/act`). That package is what another package `use`s (`use
//! so.tilo.chat as chat`), type-checked against at compile time, and what `tilolang describe`
//! prints, native and tilolang apps alike.
//!
//! A tilolang record type is a resource of its own, so a field holding a document of another
//! type refers to it ([`FieldType::Resource`]); a native document nested in a resource is part of
//! it ([`FieldType::Named`]). Both read as a type whose fields code reads. What the vocabulary
//! does not describe ([`FieldType::Any`]), and the platform's reactions and outboxes, are left
//! out: tilolang code does not read them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tilo_actions::ActionKind;
use tilo_actions::surface::{ADDRESS_KEY, FieldType, NamedType, Surface, VerbSurface};
use tilolang::meta::NamedRootMeta;
use tilolang::types::Ty;
use tilolang_runtime::meta::{
    ActionMeta, DefaultMeta, EffectClass, EnumMeta, FieldMeta, MergeKind, PackageMeta, ParamMeta,
    TypeMeta, VariantMeta,
};

use crate::action::{
    CREATE_ACTION, DELETE_BLOCK_ACTION, EDIT_BLOCK_ACTION, EDIT_FLAG_ACTION, EDIT_TEXT_ACTION,
    INSERT_BLOCK_ACTION, OPENED_ACTION, ORIGIN_ACTION, SCENARIO_ACTION,
};

/// The authored surface of an app directory, next to its manifest: the build tools compile it
/// ([`tilo_surface_compile`]).
pub const SURFACE_FILE: &str = "surface.toml";
/// The compiled surface of an app directory: the one form a receiver reads.
pub const COMPILED_SURFACE_FILE: &str = "surface.json";
/// The manifest of a native app directory.
pub const MANIFEST_FILE: &str = "tilo-app.toml";

#[derive(Deserialize)]
struct SurfaceFile {
    #[serde(default)]
    resources: Vec<Surface>,
}

#[derive(Deserialize)]
struct Manifest {
    package: ManifestPackage,
}

#[derive(Deserialize)]
struct ManifestPackage {
    id: String,
    #[serde(default)]
    name: String,
}

/// Where a resource a `surface.toml` lists is published, read beside its surface. A native app
/// leaves both out: its resources are its documents. A description of what a session publishes
/// says, of each resource kind, the address of the one resource of it (`id`), or that each
/// resource of it stands alone (`instances`): a window, a run.
#[derive(Deserialize)]
struct Published {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    instances: bool,
}

#[derive(Deserialize)]
struct PublishedFile {
    #[serde(default)]
    resources: Vec<Published>,
}

fn published(text: &str) -> Result<Vec<Published>, String> {
    let file: PublishedFile = serde_json::from_str(text)
        .map_err(|error| format!("not the compiled form of a surface: {error}"))?;
    Ok(file.resources)
}

/// The address each loose action of a `surface.toml` is published on, by the action's id
/// ([`verb_id`]): the resource whose `id` the file gives. An action of a resource kind without
/// one (a native app's, or a kind of `instances`) is sent to the resource it is called on
/// instead.
///
/// # Errors
/// The text is not a surface file, or two resources with an address publish one action, so a
/// call of it would name neither.
pub fn addresses(text: &str) -> Result<BTreeMap<String, String>, String> {
    Ok(loose(text)?
        .into_iter()
        .map(|(id, (address, _))| (id, address))
        .collect())
}

/// The loose actions of a `surface.toml` as it declares them, by id ([`verb_id`]): the verbs of
/// the resources whose `id` the file gives ([`addresses`]). A kind of `instances` offers its
/// verbs on each resource of it, so an app's `open` is no loose `open` and never stands for the
/// layout's.
///
/// # Errors
/// As [`addresses`].
pub fn loose_verbs(text: &str) -> Result<BTreeMap<String, VerbSurface>, String> {
    Ok(loose(text)?
        .into_iter()
        .map(|(id, (_, verb))| (id, verb))
        .collect())
}

/// The loose actions of a `surface.toml` by id, each with the address it is published on.
fn loose(text: &str) -> Result<BTreeMap<String, (String, VerbSurface)>, String> {
    let (surfaces, instances) = read_surfaces(text)?;
    let types = verb_types(&surfaces, &instances);
    let mut out: BTreeMap<String, (String, VerbSurface)> = BTreeMap::new();
    for (surface, published) in surfaces.iter().zip(published(text)?) {
        let Some(address) = published.id else {
            continue;
        };
        let ty = types.get(&surface.kind).map_or("", String::as_str);
        for verb in surface.verbs.iter().filter(|verb| !platform_verb(verb)) {
            let id = id_of(ty, verb);
            if let Some((other, _)) = out.insert(id.clone(), (address.clone(), verb.clone())) {
                return Err(format!(
                    "`{id}` is published on both `{other}` and `{address}`"
                ));
            }
        }
    }
    Ok(out)
}

/// The resources a compiled surface (`surface.json`) lists.
pub fn parse_surfaces(text: &str) -> Result<Vec<Surface>, String> {
    let file: SurfaceFile = serde_json::from_str(text)
        .map_err(|error| format!("not the compiled form of a surface: {error}"))?;
    // A resource written field by field publishes a setter of each field, which its file leaves
    // to the one rule that derives them.
    Ok(file
        .resources
        .into_iter()
        .map(|surface| {
            if surface.setters {
                surface.with_setters()
            } else {
                surface
            }
        })
        .collect())
}

/// One native app's published surface: its package id, the name people call it, and its compiled
/// surface. [`NativeSurface::package`] reads it as a tilolang package, whichever way it arrived:
/// from the app's directory ([`native_surface`]) for `tilolang check`, or from what a session
/// hands a task's host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeSurface {
    pub app: String,
    pub title: String,
    pub surface: String,
}

impl NativeSurface {
    /// The package this app publishes: its actions named by the catalogue ([`action_id`]), the
    /// root a program names its own document by ([`document_root`]), and a root for each
    /// resource it keeps beside that document ([`part_roots`]).
    pub fn package(&self) -> Result<PackageMeta, String> {
        let (surfaces, instances) = read_surfaces(&self.surface)?;
        Ok(native_package(
            &self.app,
            &self.title,
            &surfaces,
            &instances,
        ))
    }
}

/// [`NativeSurface::package`] of surfaces already read.
fn native_package(
    app: &str,
    title: &str,
    surfaces: &[Surface],
    instances: &[String],
) -> PackageMeta {
    let mut meta = package_with(app, title, surfaces, instances, Ids::Record);
    for action in &mut meta.actions {
        action.id = action_id(&meta.app, &action.id);
    }
    let kinds = kind_names(surfaces, instances);
    if let Some(root) = document_root(app, surfaces, &kinds, Ids::Record) {
        let beside = part_roots(app, surfaces, instances, &kinds, &root);
        meta.named_roots.push(root);
        meta.named_roots.extend(beside);
    }
    meta
}

/// The package a provider the device registered publishes, as code that uses it calls it, and
/// the address each of its actions is called at, by the action's id. The same package
/// `tilolang check` reads from the provider's committed description ([`NativeSurface`]): code
/// compiled against that is called here with the addresses this device's provider registered.
#[must_use]
pub fn provided(
    package: &tilo_actions::remote::ProvidedPackage,
) -> (PackageMeta, BTreeMap<String, String>) {
    let surfaces: Vec<Surface> = package
        .resources
        .iter()
        .map(|resource| resource.surface.clone())
        .collect();
    let meta = native_package(&package.app, &package.title, &surfaces, &[]);
    let types = verb_types(&surfaces, &[]);
    let addresses = package
        .resources
        .iter()
        .flat_map(|resource| {
            let ty = types.get(&resource.surface.kind).map_or("", String::as_str);
            resource
                .surface
                .verbs
                .iter()
                .filter(|verb| !platform_verb(verb))
                .map(move |verb| {
                    (
                        action_id(&package.app, &id_of(ty, verb)),
                        resource.address.clone(),
                    )
                })
        })
        .collect();
    (meta, addresses)
}

/// The id a catalogue gives the action `verb` (its [`verb_id`]) of the app `app`:
/// `so.tilo.chat#Messages/share-window`. A surface's verb ids are the app's own (`open`,
/// `share-window`), so several apps in one catalogue would otherwise be one action; its types
/// are named by their app the same way. The app's own api takes the verb back
/// ([`published_verb`]).
#[must_use]
pub fn action_id(app: &str, verb: &str) -> String {
    format!("{app}#{verb}")
}

/// The id of the verb `verb` of the resource type `ty` within its app: `Banners/act`. An
/// action belongs to its resource's type, and its id always names the type, never the verb
/// alone: were a verb offered on one type named by the verb, a second type adding the same verb
/// would rename it, and every grant, shortcut and invocation record naming the old id would
/// quietly stop matching.
#[must_use]
pub fn verb_id(ty: &str, verb: &str) -> String {
    format!("{ty}/{verb}")
}

/// The verbs every resource of a kind has, which no app declares: the system's Copy link, whose
/// link the host serving the resource mints. Their ids name the resource's type as an app's own
/// verbs do ([`verb_id`]), so a call chooses them the same way and the host answers them where the
/// resource is served.
pub const SYSTEM_VERBS: &[(&str, &str, &str)] = &[(
    tilo_actions::remote::SHARE_LINK_ACTION,
    "Copy link",
    "The link to this resource, which other devices and people open it by.",
)];

/// The id of the action `verb` of the resource type `ty` declares: its [`verb_id`], or, for a
/// tilolang package's verb (one offered by a name of its own), the id its declaration keeps
/// (`qymo5lg2`). That id is already the declaration's alone, whatever other type declares the
/// name, and it is what a change of the action claims.
fn id_of(ty: &str, verb: &VerbSurface) -> String {
    if verb.name.is_empty() {
        verb_id(ty, &verb.id)
    } else {
        verb.id.clone()
    }
}

/// The verb id the app's own api takes for the catalogue action `id` ([`action_id`]): the verb
/// after its resource type (`so.tilo.chat#Workspace/rename`, the session's `Shell/place`). A
/// tilolang package's action keeps the id its app gives it.
#[must_use]
pub fn published_verb<'a>(app: &str, id: &'a str) -> &'a str {
    let id = id
        .strip_prefix(app)
        .and_then(|rest| rest.strip_prefix('#'))
        .unwrap_or(id);
    id.rsplit_once('/').map_or(id, |(_, verb)| verb)
}

/// The surface a native app directory publishes: its manifest's id and name, and its compiled
/// surface, compiled here from the `surface.toml` beside its manifest. `None` for a directory
/// that is not a native app with an authored surface.
pub fn native_surface(dir: &Path) -> Result<Option<NativeSurface>, String> {
    let (manifest, surface) = (dir.join(MANIFEST_FILE), dir.join(SURFACE_FILE));
    if !manifest.is_file() || !surface.is_file() {
        return Ok(None);
    }
    let read = |path: &Path| {
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))
    };
    let compiled = tilo_surface_compile::compile_file(&surface)?;
    NativeSurface::read(&read(&manifest)?, compiled)
        .map(Some)
        .map_err(|error| format!("{}: {error}", dir.join(MANIFEST_FILE).display()))
}

impl NativeSurface {
    /// The native app whose `tilo-app.toml` is `manifest` and whose compiled surface is
    /// `surface`: [`native_surface`] over data already read, as a host with no disk holds it.
    ///
    /// # Errors
    /// The manifest is not one.
    pub fn read(manifest: &str, surface: String) -> Result<Self, String> {
        let manifest: Manifest = toml::from_str(manifest).map_err(|error| error.to_string())?;
        Ok(Self {
            app: manifest.package.id,
            title: manifest.package.name,
            surface,
        })
    }
}

/// The package a native app directory publishes: its manifest's id and name, and the
/// resources of its `surface.toml`. `None` for a directory that is not a native app with a
/// committed surface.
pub fn native_app(dir: &Path) -> Result<Option<PackageMeta>, String> {
    let Some(surface) = native_surface(dir)? else {
        return Ok(None);
    };
    surface
        .package()
        .map(Some)
        .map_err(|error| format!("{}: {error}", dir.join(SURFACE_FILE).display()))
}

/// The package of the app `app` named `title` whose compiled surface is `text`: a description a
/// binary carries, its actions named as the surface names them.
///
/// A task's host and `tilolang check` both read a native app through this one reader, so a
/// program compiles against the same package either way: [`NativeSurface::package`] adds what
/// only an app the session does not run has, its actions named by its app ([`action_id`]) and
/// its document a root a program names ([`document_root`]).
pub fn described(app: &str, title: &str, text: &str) -> Result<PackageMeta, String> {
    let (surfaces, instances) = read_surfaces(text)?;
    one_action_per_id(package_with(app, title, &surfaces, &instances, Ids::Record))
}

/// `meta`, refused where two of its actions have one id, so a call of it would name neither. An
/// action's name belongs to its resource type: two types offering one verb are two actions, each
/// with an id naming its type, so what is refused is one verb declared twice for one type, or
/// twice loose.
///
/// # Errors
/// The id that two actions of `meta` have.
pub fn one_action_per_id(meta: PackageMeta) -> Result<PackageMeta, String> {
    let mut seen = BTreeSet::new();
    for action in meta.actions.iter().filter(|action| !action.scenario) {
        if !seen.insert(action.id.as_str()) {
            return Err(format!(
                "{} declares the action `{}` twice",
                meta.app, action.id
            ));
        }
    }
    Ok(meta)
}

/// The resources `text` publishes, and the kinds `instances` marks: the ones whose verbs act on
/// the one resource they are called on.
fn read_surfaces(text: &str) -> Result<(Vec<Surface>, Vec<String>), String> {
    let surfaces = parse_surfaces(text)?;
    let instances = surfaces
        .iter()
        .zip(published(text)?)
        .filter(|(_, published)| published.instances)
        .map(|(surface, _)| surface.kind.clone())
        .collect();
    Ok((surfaces, instances))
}

/// The named root of a native app's own document: the first resource of its surface that stores
/// fields (`chat.workspace`), which its verbs are declared on and whose history its steps are
/// taken back with. A program names it where such a verb takes the document
/// (`chat.rename_channel(chat.workspace, ...)`), and reaches the app through it without reading
/// it. `None` for an app whose surface stores nothing, whose resource is no document a program
/// could name.
fn document_root(
    app: &str,
    surfaces: &[Surface],
    kinds: &BTreeMap<String, String>,
    ids: Ids,
) -> Option<NamedRootMeta> {
    let kind = surfaces
        .iter()
        .find(|surface| !surface.fields.is_empty())
        .map(|surface| surface.kind.as_str())?;
    let ty = kinds.get(kind)?;
    Some(NamedRootMeta {
        name: snake_case(ty),
        ty: ids.id(app, ty),
        published: Some(kind.to_owned()),
        device: false,
    })
}

/// The named roots of the resources a native app keeps beside its document, one of each kind:
/// the parts its notes or messages live in (`notes.pages`), whose verbs make and change what they
/// hold (`notes.new_note(notes.pages, title: "Groceries")`). A program reads one as it reads the
/// document, through the app's own read. Each resource of the surface besides `document` that
/// keeps a collection of its own and is not one of many (`instances`) is one: what a read of the
/// app lists beside its document ([`crate::native`]).
fn part_roots(
    app: &str,
    surfaces: &[Surface],
    instances: &[String],
    kinds: &BTreeMap<String, String>,
    document: &NamedRootMeta,
) -> Vec<NamedRootMeta> {
    surfaces
        .iter()
        .filter(|surface| {
            surface
                .fields
                .iter()
                .any(|field| field.stored == "collection" && !field.path.contains('.'))
                && !instances.contains(&surface.kind)
                && document.published.as_ref() != Some(&surface.kind)
        })
        .filter_map(|surface| {
            let ty = kinds.get(&surface.kind)?;
            Some(NamedRootMeta {
                name: snake_case(ty),
                ty: Ids::Record.id(app, ty),
                published: Some(surface.kind.clone()),
                device: false,
            })
        })
        .collect()
}

/// The native apps among `search`: each entry an app directory itself or a directory of them.
/// Directories that are not native apps are passed over.
pub fn native_apps(search: &[PathBuf]) -> Result<Vec<PackageMeta>, String> {
    let mut out = Vec::new();
    for entry in search {
        if let Some(meta) = native_app(entry)? {
            out.push(meta);
            continue;
        }
        let Ok(dirs) = std::fs::read_dir(entry) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = dirs
            .filter_map(Result::ok)
            .map(|dir| dir.path())
            .filter(|path| path.is_dir())
            .collect();
        dirs.sort();
        for dir in dirs {
            if let Some(meta) = native_app(&dir)? {
                out.push(meta);
            }
        }
    }
    Ok(out)
}

/// The type name of the resource kind `kind`: its last segment in upper camel case
/// (`chat.workspace` is `Workspace`, `tl.so.tilo.notes.Note` is `Note`).
#[must_use]
pub fn kind_type_name(kind: &str) -> String {
    let last = kind.rsplit('.').next().unwrap_or(kind);
    last.split(['-', '_'])
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        })
        .collect()
}

/// The type name of the resource kind `kind` from its last two segments: `tilo.workspace.emoji`
/// is `WorkspaceEmoji`.
fn longer_type_name(kind: &str) -> String {
    let mut segments = kind.rsplit('.');
    let last = segments.next().unwrap_or(kind);
    let before = segments.next().unwrap_or_default();
    format!("{}{}", kind_type_name(before), kind_type_name(last))
}

/// The app a tilolang resource kind belongs to and its type: `tl.<app>.<Type>`.
#[must_use]
pub fn tilolang_kind(kind: &str) -> Option<(&str, &str)> {
    let rest = kind.strip_prefix("tl.")?;
    let (app, name) = rest.rsplit_once('.')?;
    name.starts_with(|c: char| c.is_ascii_uppercase())
        .then_some((app, name))
}

/// `name` in snake case: `WorkspaceEmoji` is `workspace_emoji`.
fn snake_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len().saturating_add(4));
    for (index, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// The action name of a verb id: `begin-chat` is `begin_chat`.
fn action_name(id: &str) -> String {
    id.replace(['-', '.', ':'], "_")
}

/// The verbs only the platform invokes: creation, bound edits, the edits of one paragraph of a
/// rich text, what an outside origin writes and what a scenario seeds.
fn platform_verb(verb: &VerbSurface) -> bool {
    [
        CREATE_ACTION,
        ORIGIN_ACTION,
        SCENARIO_ACTION,
        OPENED_ACTION,
        EDIT_TEXT_ACTION,
        EDIT_FLAG_ACTION,
        EDIT_BLOCK_ACTION,
        INSERT_BLOCK_ACTION,
        DELETE_BLOCK_ACTION,
    ]
    .contains(&verb.id.as_str())
}

const fn effect_class(kind: ActionKind) -> EffectClass {
    match kind {
        ActionKind::Read => EffectClass::Read,
        ActionKind::Undoable => EffectClass::Undoable,
        ActionKind::Outside => EffectClass::Outside,
    }
}

/// The package of the app `app` named `title` that publishes `surfaces`: an app whose
/// resources the session publishes as readings (`so.tilo.device#Power`). See the module docs.
#[must_use]
pub fn package_of(app: &str, title: &str, surfaces: &[Surface]) -> PackageMeta {
    let mut meta = package_with(app, title, surfaces, &[], Ids::Readings);
    // Named by its app ([`action_id`]), as every catalogue names an app's actions: a verb is the
    // app's own word, and another app publishing the same one (`cancel`) loads beside it.
    for action in &mut meta.actions {
        action.id = action_id(app, &action.id);
    }
    meta
}

/// [`package_of`], where each resource of a kind of `instances` is a type too, though it
/// stores no fields: its verbs act on the one resource they are called on.
fn package_with(
    app: &str,
    title: &str,
    surfaces: &[Surface],
    instances: &[String],
    ids: Ids,
) -> PackageMeta {
    let mut reader = Reader {
        app,
        ids,
        kinds: kind_names(surfaces, instances),
        rows: row_kinds(surfaces),
        types: verb_types(surfaces, instances),
        declared: BTreeMap::new(),
        meta: PackageMeta {
            app: app.to_owned(),
            title: title.to_owned(),
            ..PackageMeta::default()
        },
    };
    for surface in surfaces {
        reader.surface(surface);
    }
    reader.meta
}

/// The type name of each resource kind of `surfaces`. A resource with stored fields is a type;
/// one without is where loose verbs are. A resource whose name a document or enum of the app
/// already has is named by more of its kind (`tilo.workspace.emoji`, holding `Emoji` documents, is
/// `WorkspaceEmoji`). A resource of a kind of `instances` is a type too, though it stores no
/// fields: its verbs act on the one resource they are called on.
fn kind_names(surfaces: &[Surface], instances: &[String]) -> BTreeMap<String, String> {
    surfaces
        .iter()
        .filter(|surface| !surface.fields.is_empty() || instances.contains(&surface.kind))
        .map(|surface| (surface.kind.clone(), type_name(surfaces, &surface.kind)))
        .collect()
}

/// The type name of the resource kind `kind` of `surfaces`: [`kind_type_name`], or
/// [`longer_type_name`] where a document or enum of the app already has that name.
fn type_name(surfaces: &[Surface], kind: &str) -> String {
    let short = kind_type_name(kind);
    let named = surfaces
        .iter()
        .flat_map(|surface| &surface.types)
        .any(|ty| ty.name == short);
    if named { longer_type_name(kind) } else { short }
}

/// The resource type each resource kind of `surfaces` offers its verbs as actions of, which
/// each action's id names ([`verb_id`]): the kind's type ([`kind_names`]), else the record type
/// a reading holds resources of it as ([`row_kinds`]). A resource that is neither (the shell's
/// layout, a workspace's members) stores nothing code reads, so its verbs take no resource
/// first; their ids still name the type of the resource they are published on.
fn verb_types(surfaces: &[Surface], instances: &[String]) -> BTreeMap<String, String> {
    let mut types = kind_names(surfaces, instances);
    for (kind, row) in row_kinds(surfaces) {
        types.entry(kind).or_insert(row);
    }
    for surface in surfaces {
        types
            .entry(surface.kind.clone())
            .or_insert_with(|| type_name(surfaces, &surface.kind));
    }
    types
}

/// The record types of `surfaces` that are resources of their own, by their kind: a record a
/// reading holds with its address ([`ADDRESS_KEY`], a Wi-Fi network in the network's reading).
/// The kind's own surface lists the verbs offered on it, which are actions on that record type.
fn row_kinds(surfaces: &[Surface]) -> BTreeMap<String, String> {
    surfaces
        .iter()
        .flat_map(|surface| &surface.types)
        .filter_map(|named| {
            named.fields.iter().find_map(|field| match &field.ty {
                FieldType::Resource(kind) if field.name == ADDRESS_KEY && !kind.is_empty() => {
                    Some((kind.clone(), named.name.clone()))
                }
                _ => None,
            })
        })
        .collect()
}

/// The type name each resource kind of `surfaces` reads as in [`package_of`] of them.
#[must_use]
pub fn kind_names_of(surfaces: &[Surface]) -> BTreeMap<String, String> {
    kind_names(surfaces, &[])
}

/// How a package spells the ids of its types.
#[derive(Clone, Copy)]
enum Ids {
    /// A document the app itself reads (`so.tilo.tasks.Issue`): a resource a program reads from
    /// the app's document, as it reads any app's the session holds.
    Record,
    /// A resource the session publishes as a reading (`so.tilo.device#Power`), which a view
    /// reads without a document.
    Readings,
}

impl Ids {
    /// The id of the type or enum `name` of the app `app`, qualified by the app so two apps'
    /// types of one name are two types wherever both are loaded. A document the app reads itself
    /// keeps them apart as `so.tilo.tasks.Issue`: an id with a `#` names a resource read from
    /// its published reading ([`tilolang_runtime::foreign`]), which an app whose document the
    /// session holds is not.
    fn id(self, app: &str, name: &str) -> String {
        match self {
            Self::Record => format!("{app}.{name}"),
            Self::Readings => format!("{app}#{name}"),
        }
    }
}

struct Reader<'a> {
    app: &'a str,
    ids: Ids,
    /// The type name of each resource kind of the app.
    kinds: BTreeMap<String, String>,
    /// The record types that are resources of their own ([`row_kinds`]), by kind: their kind's
    /// verbs are actions on them, and the record its reading declares is their type.
    rows: BTreeMap<String, String>,
    /// The resource type each kind's verbs are actions of ([`verb_types`]).
    types: BTreeMap<String, String>,
    /// The named types declared so far, by name, and whether each is an enum.
    declared: BTreeMap<String, (NamedType, bool)>,
    meta: PackageMeta,
}

impl Reader<'_> {
    /// The id of the app's type or enum `name`; see [`Ids::id`].
    fn type_id(&self, name: &str) -> String {
        self.ids.id(self.app, name)
    }

    /// The app's own type `name` as package metadata writes it. The language holds types of
    /// its own under some names (`View`, `Bytes`) and begins others with one (`List`), and an
    /// app names its types and its kinds without knowing which. Where the language reads the
    /// bare name as anything but this app's type, the type is written with its app before it,
    /// as the language itself writes a package's type of such a name ([`Ty::meta_text`]): it
    /// stays the app's, under the name the app gave it.
    fn own(&self, name: &str) -> String {
        let qualified = format!("{}.{name}", self.app);
        match (
            Ty::parse_meta(name, self.app),
            Ty::parse_meta(&qualified, self.app),
        ) {
            (bare, Some(own)) if bare.as_ref() != Some(&own) => qualified,
            _ => name.to_owned(),
        }
    }

    fn surface(&mut self, surface: &Surface) {
        let row = self.rows.get(&surface.kind).cloned();
        let own = self
            .kinds
            .get(&surface.kind)
            .cloned()
            .or_else(|| row.clone());
        let renamed = self.named_types(surface, own.as_deref());
        let rename = |name: &str| {
            renamed
                .get(name)
                .cloned()
                .unwrap_or_else(|| name.to_owned())
        };
        if let Some(own) = own.as_ref().filter(|_| row.is_none()) {
            self.own_type(surface, own, &rename);
        }
        let ty = self.types.get(&surface.kind).cloned().unwrap_or_default();
        for verb in surface.verbs.iter().filter(|verb| !platform_verb(verb)) {
            self.action(verb, &ty, own.as_deref(), &rename);
        }
        // A resource of a kind code reads has the system's verbs as well as the app's own: a
        // menu offers them on every resource, and a program calls them on one as it calls the
        // app's (`card.share_link()`).
        if let Some(own) = own.as_deref() {
            self.system_verbs(own, &ty);
        }
    }

    /// The verbs every resource has ([`SYSTEM_VERBS`]), declared on the type `own` as the surface
    /// resource kind's verbs are: id [`verb_id`] of `ty`, the resource taken first. A verb the
    /// app declares itself is kept as the app wrote it.
    fn system_verbs(&mut self, own: &str, ty: &str) {
        for &(verb, label, doc) in SYSTEM_VERBS {
            let id = verb_id(ty, verb);
            if self.meta.actions.iter().any(|action| action.id == id) {
                continue;
            }
            self.meta.actions.push(ActionMeta {
                id,
                verb: verb.to_owned(),
                name: action_name(verb),
                doc: doc.to_owned(),
                params: vec![ParamMeta {
                    name: snake_case(own),
                    ty: self.own(own),
                    default: None,
                    place: false,
                    entry: false,
                    label: None,
                    secret: false,
                }],
                result: Some("Text".to_owned()),
                class: EffectClass::Read,
                label: label.to_owned(),
                icon: None,
                role: None,
                scenario: false,
                did: None,
                did_many: None,
                keywords: Vec::new(),
                search: None,
                present: None,
                toggles: None,
                settles: None,
                field: None,
                key: None,
                group: None,
                export: None,
                guard: false,
                applies: false,
                uses: Vec::new(),
                outside: Vec::new(),
                notice: false,
                invokes: false,
                opens: Vec::new(),
                shows: Vec::new(),
                takes: Vec::new(),
                reaches: Vec::new(),
            });
        }
    }

    /// Declares the named types of `surface`, the resource type `own`'s, and gives the name each
    /// is declared under: a type another resource declared differently, or another resource's
    /// own type is called, is this resource's own, named after it. A kind of one segment
    /// (`network`) has no longer name to step aside to ([`type_name`]), so the system service's
    /// network reading in `processes` is `ProcessesNetwork` beside the device's `Network`.
    fn named_types(&mut self, surface: &Surface, own: Option<&str>) -> BTreeMap<String, String> {
        let mut renamed: BTreeMap<String, String> = BTreeMap::new();
        for named in &surface.types {
            let a_kind_is_called_so = self
                .kinds
                .values()
                .any(|kind| *kind == named.name && Some(kind.as_str()) != own);
            let name = match self.declared.get(&named.name) {
                Some((declared, _)) if declared == named => continue,
                Some(_) => format!("{}{}", own.unwrap_or_default(), named.name),
                None if a_kind_is_called_so => format!("{}{}", own.unwrap_or_default(), named.name),
                None => named.name.clone(),
            };
            renamed.insert(named.name.clone(), name.clone());
            self.declared.insert(name, (named.clone(), named.is_enum()));
        }
        let rename = |name: &str| {
            renamed
                .get(name)
                .cloned()
                .unwrap_or_else(|| name.to_owned())
        };
        for named in &surface.types {
            let Some(name) = renamed.get(&named.name).cloned() else {
                continue;
            };
            if named.is_enum() {
                let variants = named
                    .cases
                    .iter()
                    .map(|case| VariantMeta {
                        id: format!("{name}.{}", case.name),
                        name: case.name.clone(),
                        doc: String::new(),
                        label: case.label.clone(),
                        icon: None,
                        tone: None,
                        fields: case
                            .fields
                            .iter()
                            .filter_map(|field| {
                                self.field(&format!("{name}.{}", case.name), field, &rename)
                            })
                            .collect(),
                    })
                    .collect();
                self.meta.enums.push(EnumMeta {
                    id: self.type_id(&name),
                    name,
                    doc: named.doc.clone(),
                    tag: named.tag.clone(),
                    variants,
                });
            } else {
                let fields = named
                    .fields
                    .iter()
                    .filter_map(|field| self.field(&name, field, &rename))
                    .collect();
                self.meta.types.push(TypeMeta {
                    id: self.type_id(&name),
                    name,
                    doc: named.doc.clone(),
                    fields,
                    ..TypeMeta::default()
                });
            }
        }
        renamed
    }

    /// Declares the type `own` of the resource `surface` describes: its top-level fields.
    fn own_type(&mut self, surface: &Surface, own: &str, rename: &dyn Fn(&str) -> String) {
        let declared = surface
            .fields
            .iter()
            .filter(|field| !field.path.contains('.'))
            .map(|field| tilo_actions::surface::TypeField {
                name: field.path.clone(),
                ty: field.ty.clone(),
                title: field.title.clone(),
            })
            .collect::<Vec<_>>();
        let fields = declared
            .iter()
            .filter_map(|field| self.field(own, field, rename))
            .collect();
        // The resource's own type is a declared type like any other: a resource after it that
        // publishes a different type of the same name (`processes` reading a `Network` of its
        // own beside the connectivity `network`) is renamed after itself, so neither reads as
        // the other.
        self.declared.insert(
            own.to_owned(),
            (
                NamedType {
                    tag: None,
                    name: own.to_owned(),
                    doc: if surface.label == own {
                        String::new()
                    } else {
                        surface.label.clone()
                    },
                    fields: declared,
                    cases: Vec::new(),
                },
                false,
            ),
        );
        self.meta.types.push(TypeMeta {
            id: self.type_id(own),
            name: own.to_owned(),
            // What people call the resource, when it says more than the type's name.
            doc: if surface.label == own {
                String::new()
            } else {
                surface.label.clone()
            },
            fields,
            ..TypeMeta::default()
        });
    }

    /// Declares the verb `verb` of the resource type `ty` as an action, taking its resource first
    /// where code holds one of it (`own`; none for a resource that stores nothing code reads). A
    /// verb with a parameter of a type tilolang code cannot give (bytes, any JSON value) is left
    /// out: tilolang code cannot call it.
    fn action(
        &mut self,
        verb: &VerbSurface,
        ty: &str,
        own: Option<&str>,
        rename: &dyn Fn(&str) -> String,
    ) {
        let mut params = Vec::new();
        if let Some(own) = own {
            params.push(ParamMeta {
                name: snake_case(own),
                ty: self.own(own),
                default: None,
                place: false,
                entry: false,
                label: None,
                secret: false,
            });
        }
        for param in &verb.params {
            // What the code declared; a surface written before parameters had types says
            // what its schema does.
            let ty = if param.ty.is_any() {
                FieldType::from_schema(&param.schema)
            } else {
                param.ty.clone()
            };
            // A secret is text as tilolang holds it, and the parameter says so beside its type:
            // a record of a call of the action keeps its name alone.
            let secret = matches!(ty, FieldType::Secret)
                || matches!(&ty, FieldType::Option(inner) if matches!(**inner, FieldType::Secret));
            let ty = match ty {
                FieldType::Option(_) => ty,
                other if !param.required => FieldType::Option(Box::new(other)),
                other => other,
            };
            // A parameter tilolang code cannot give leaves the verb out when it must be given,
            // and is left to its default when it may be left out.
            let ty = if secret {
                match ty {
                    FieldType::Option(_) => "Option<Text>".to_owned(),
                    _ => "Text".to_owned(),
                }
            } else {
                let Some(ty) = self.ty(&ty, rename) else {
                    if param.required {
                        return;
                    }
                    continue;
                };
                ty
            };
            // A parameter the verb may go without is optional as a tilolang action's is:
            // `x: Option<Int> = .none`, left out of a call wherever it stands, and given as
            // `x: 80`. A `.none` reaches the app as no argument, so its own default holds.
            params.push(ParamMeta {
                name: action_name(&param.name),
                ty,
                default: (!param.required).then(DefaultMeta::none),
                place: false,
                entry: false,
                label: None,
                secret,
            });
        }
        // What the action answers with, as the surface types it: the value a
        // script reads back, `Unit` when the app declares no shape for it.
        let result = verb.returns.as_ref().and_then(|returns| {
            let ty = if returns.ty.is_any() {
                FieldType::from_schema(&returns.schema)
            } else {
                returns.ty.clone()
            };
            self.ty(&ty, rename)
        });
        // An action's name belongs to its resource type: `act` on `Banners` and `act` on
        // `Notifications` are two actions, each with an id naming its type (`Banners/act`), and
        // the app's own api takes it back by the verb alone.
        self.meta.actions.push(ActionMeta {
            id: id_of(ty, verb),
            verb: verb.id.clone(),
            // The name the action API offers it by, made a tilolang action's name: a tilolang
            // app's own (`pin`), else the id a native verb publishes it as
            // (`read-form-fields` is `read_form_fields`).
            name: action_name(verb.offered()),
            doc: verb.description.clone().unwrap_or_default(),
            params,
            result,
            class: effect_class(verb.kind),
            reaches: Vec::new(),
            label: verb.label.clone(),
            icon: None,
            role: verb.destructive.then(|| "destructive".to_owned()),
            scenario: false,
            did: None,
            did_many: None,
            keywords: Vec::new(),
            search: None,
            present: None,
            toggles: None,
            settles: None,
            // The field a native verb declares is the node its grant and its change are judged
            // at, as for the app's own.
            field: verb.field.clone(),
            key: None,
            group: None,
            export: None,
            guard: false,
            applies: false,
            uses: Vec::new(),
            outside: Vec::new(),
            notice: false,
            invokes: false,
            opens: Vec::new(),
            shows: Vec::new(),
            takes: Vec::new(),
        });
    }

    /// The field `field` of the type `owner`, or `None` when its type is not one tilolang code
    /// reads.
    fn field(
        &self,
        owner: &str,
        field: &tilo_actions::surface::TypeField,
        rename: &dyn Fn(&str) -> String,
    ) -> Option<FieldMeta> {
        // The address a record of a reading carries is what its actions are called on, not a
        // field code reads: the record is the resource.
        if field.name == ADDRESS_KEY {
            return None;
        }
        let ty = self.ty(&field.ty, rename)?;
        let merge = match field.ty {
            FieldType::Collection(_) | FieldType::Set(_) | FieldType::Map(_) => {
                MergeKind::Structural
            }
            FieldType::Text | FieldType::RichText => MergeKind::Text,
            _ => MergeKind::Register,
        };
        // The fields of a tuple variant are numbered, and a reading's own keys may be
        // kebab-cased (`user-switching`); tilolang names each as an identifier, and the id
        // keeps the key the reading holds it under.
        let (name, key) = match field.name.parse::<u32>() {
            Ok(0) => ("value".to_owned(), "value".to_owned()),
            Ok(index) => (format!("value{index}"), format!("value{index}")),
            // A service's camel-cased key (`workspaceName`) is named in snake case too.
            Err(_) => (
                snake_case(&field.name.replace('-', "_")),
                field.name.clone(),
            ),
        };
        Some(FieldMeta {
            id: format!("{owner}.{key}"),
            name,
            ty,
            merge,
            doc: field.title.clone(),
            default: None,
        })
    }

    /// `ty` as package metadata writes a type ([`tilolang::types::Ty::meta_text`]), or `None`
    /// when tilolang code does not read it.
    fn ty(&self, ty: &FieldType, rename: &dyn Fn(&str) -> String) -> Option<String> {
        let inner = |wrap: &str, item: &FieldType| {
            self.ty(item, rename).map(|item| format!("{wrap}<{item}>"))
        };
        Some(match ty {
            FieldType::Text => "Text".to_owned(),
            FieldType::RichText => "RichText".to_owned(),
            FieldType::Int => "Int".to_owned(),
            FieldType::Float => "Float".to_owned(),
            FieldType::Bool => "Bool".to_owned(),
            FieldType::Date => "Date".to_owned(),
            FieldType::Time => "Time".to_owned(),
            FieldType::Duration => "Duration".to_owned(),
            FieldType::Identifier => "std.Identifier".to_owned(),
            FieldType::Actor => "Actor".to_owned(),
            FieldType::Json => "json.Value".to_owned(),
            FieldType::Resource(kind) => self.resource(kind)?,
            FieldType::Named(name) => {
                let name = rename(name);
                match self.declared.get(&name) {
                    Some((_, true)) => format!("enum {name}"),
                    Some((_, false)) => self.own(&name),
                    None => return None,
                }
            }
            FieldType::Option(item) => inner("Option", item)?,
            FieldType::List(item) => inner("List", item)?,
            FieldType::Set(item) => inner("Set", item)?,
            FieldType::Collection(item) => inner("Collection", item)?,
            FieldType::Map(item) => inner("Map", item)?,
            FieldType::Union(cases) => {
                let cases: Option<Vec<String>> = cases
                    .iter()
                    .map(|case| match case {
                        FieldType::Resource(kind) => self.resource(kind),
                        _ => None,
                    })
                    .collect();
                cases?.join(" | ")
            }
            FieldType::Any | FieldType::Reactions | FieldType::Outbox(_) | FieldType::Secret => {
                return None;
            }
        })
    }

    /// The type of a resource of `kind`: this app's by name, a tilolang package's qualified by
    /// its app, a resource of any kind (`kind` empty) the host's `Resource`, which a value of
    /// every resource type is; `None` for another native app's.
    fn resource(&self, kind: &str) -> Option<String> {
        if kind.is_empty() {
            return Some(format!(
                "{}.{}",
                tilolang::HOST_APP,
                tilolang::RESOURCE_TYPE_NAME
            ));
        }
        if let Some(name) = self.kinds.get(kind).or_else(|| self.rows.get(kind)) {
            return Some(self.own(name));
        }
        let (app, name) = tilolang_kind(kind)?;
        Some(if app == self.app {
            name.to_owned()
        } else {
            format!("{app}.{name}")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{package_of, parse_surfaces};

    /// The connectivity resource's reading, and the system monitor's: each publishes a type of
    /// its own called `Network`, with different fields.
    const DEVICE: &str = r#"
[[resources]]
kind = "network"
label = "Network"
version = 1

[[resources.fields]]
path = "wifi"
type = { named = "Wifi" }

[[resources.types]]
name = "Wifi"
fields = [ { name = "on", type = "bool" } ]

[[resources]]
kind = "processes"
label = "Processes"
version = 1

[[resources.fields]]
path = "machine"
type = { named = "Machine" }

[[resources.types]]
name = "Machine"
fields = [ { name = "network", type = { named = "Network" } } ]

[[resources.types]]
name = "Network"
fields = [ { name = "rx_per_sec", type = "float" } ]
"#;

    /// A resource's own type is declared like any named type: the reading that publishes a
    /// different `Network` after the connectivity one gets its own, and the record naming it
    /// names that one.
    #[test]
    fn two_resources_publishing_one_type_name_keep_their_own() {
        let compiled = tilo_surface_compile::compile(DEVICE).expect("the surfaces compile");
        let surfaces = parse_surfaces(&compiled).expect("the surfaces read");
        let meta = package_of("so.tilo.device", "Device", &surfaces);
        let names: Vec<&str> = meta.types.iter().map(|ty| ty.name.as_str()).collect();
        assert!(names.contains(&"Network"), "{names:?}");
        assert!(names.contains(&"ProcessesNetwork"), "{names:?}");
        let machine = meta
            .types
            .iter()
            .find(|ty| ty.name == "Machine")
            .expect("the processes reading's Machine");
        let network = machine
            .fields
            .iter()
            .find(|field| field.name == "network")
            .expect("the machine's network");
        assert_eq!(network.ty, "ProcessesNetwork");
        let connectivity = meta
            .types
            .iter()
            .find(|ty| ty.name == "Network")
            .expect("the connectivity reading's Network");
        assert!(
            connectivity.fields.iter().any(|field| field.name == "wifi"),
            "the first Network stays the connectivity reading"
        );
    }

    /// A parameter the surface types a secret is text as tilolang gives it, and says beside its
    /// type that what is typed is handed over once and kept nowhere: a call of the action keeps
    /// its name alone.
    #[test]
    fn a_secret_parameter_reads_as_text_that_says_so() {
        let compiled = tilo_surface_compile::compile(
            r#"
[[resources]]
kind = "accounts"
label = "Accounts"
version = 0
fields = []

[[resources.verbs]]
id = "submit-sign-in-code"
kind = "outside"
params = [
  { name = "signIn", required = true, type = "text" },
  { name = "code", required = true, type = "secret" },
]
"#,
        )
        .expect("the surface compiles");
        let surfaces = parse_surfaces(&compiled).expect("the surface reads");
        let meta = package_of("tilo.accounts", "Accounts", &surfaces);
        let action = meta
            .actions
            .iter()
            .find(|action| action.verb == "submit-sign-in-code")
            .expect("the surface's verb");
        let code = action
            .params
            .iter()
            .find(|param| param.name == "code")
            .expect("the code parameter");
        assert!(code.secret, "the surface's type says it is a secret");
        assert_eq!(code.ty, "Text", "a secret is text to tilolang");
        let sign_in = action
            .params
            .iter()
            .find(|param| param.name == "sign_in")
            .expect("the sign-in parameter");
        assert!(!sign_in.secret, "a plain text parameter stays plain");
    }
}
