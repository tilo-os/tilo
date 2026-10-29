//! What an app publishes, read as a tilolang package.
//!
//! The catalogue is the one description of an app: its resources' surfaces
//! (`tilo_actions::surface::Surface`), whichever language the app is written in. Read as a
//! package, a surface's resource kinds are its types, the records and enums its fields name are
//! types and enums, and its verbs are actions on their resource. That package is what another
//! package `use`s (`use so.tilo.chat as chat`), type-checked against at compile time, and what
//! `tilolang describe` prints, native and tilolang apps alike.
//!
//! A tilolang record type is a resource of its own, so a field holding a record of another
//! type refers to it ([`FieldType::Resource`]); a native record nested in a resource is part of
//! it ([`FieldType::Named`]). Both read as a type whose fields code reads. What the vocabulary
//! does not describe ([`FieldType::Any`]), and the platform's reactions and outboxes, are left
//! out: tilolang code does not read them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tilo_actions::ActionKind;
use tilo_actions::surface::{FieldType, NamedType, Surface, VerbSurface};
use tilolang::meta::SearchMeta;
use tilolang_runtime::meta::{
    ActionMeta, EffectClass, EnumMeta, FieldMeta, MergeKind, PackageMeta, ParamMeta, TypeMeta,
    VariantMeta,
};

use crate::action::{CREATE_ACTION, EDIT_FLAG_ACTION, EDIT_TEXT_ACTION};

/// The surface file of an app directory, next to its manifest.
pub const SURFACE_FILE: &str = "surface.toml";
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

/// The resources a `surface.toml` lists.
///
/// # Errors
/// The text is not a surface file.
pub fn parse_surfaces(text: &str) -> Result<Vec<Surface>, String> {
    // Through a JSON value, as receivers read it (`collab_types::surface::SurfaceFile`).
    let value: serde_json::Value = toml::from_str(text).map_err(|error| error.to_string())?;
    let file: SurfaceFile = serde_json::from_value(value).map_err(|error| error.to_string())?;
    Ok(file.resources)
}

/// The package a native app directory publishes: its manifest's id and name, and the
/// resources of its `surface.toml`. `None` for a directory that is not a native app with a
/// committed surface.
///
/// # Errors
/// The manifest or the surface file does not read.
pub fn native_app(dir: &Path) -> Result<Option<PackageMeta>, String> {
    let (manifest, surface) = (dir.join(MANIFEST_FILE), dir.join(SURFACE_FILE));
    if !manifest.is_file() || !surface.is_file() {
        return Ok(None);
    }
    let read = |path: &Path| {
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))
    };
    let manifest: Manifest = toml::from_str(&read(&manifest)?)
        .map_err(|error| format!("{}: {error}", dir.join(MANIFEST_FILE).display()))?;
    let surfaces = parse_surfaces(&read(&surface)?)
        .map_err(|error| format!("{}: {error}", dir.join(SURFACE_FILE).display()))?;
    Ok(Some(package_of(
        &manifest.package.id,
        &manifest.package.name,
        &surfaces,
    )))
}

/// The native apps among `search`: each entry an app directory itself or a directory of them.
/// Directories that are not native apps are passed over.
///
/// # Errors
/// An app's manifest or surface file does not read.
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
fn tilolang_kind(kind: &str) -> Option<(&str, &str)> {
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

/// The verbs only the platform invokes: a tilolang host's own commit, creation and bound edits.
fn platform_verb(verb: &VerbSurface) -> bool {
    [
        CREATE_ACTION,
        EDIT_TEXT_ACTION,
        EDIT_FLAG_ACTION,
        crate::surface::COMMIT,
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

/// The package of the app `app` named `title` that publishes `surfaces`. See the module docs.
#[must_use]
pub fn package_of(app: &str, title: &str, surfaces: &[Surface]) -> PackageMeta {
    let mut reader = Reader {
        app,
        kinds: BTreeMap::new(),
        declared: BTreeMap::new(),
        meta: PackageMeta {
            app: app.to_owned(),
            title: title.to_owned(),
            ..PackageMeta::default()
        },
    };
    // A resource with stored fields is a type; one without is where loose verbs are. A
    // resource whose name a record or enum of the app already has is named by more of its kind
    // (`tilo.workspace.emoji`, holding `Emoji` records, is `WorkspaceEmoji`).
    let named: Vec<&str> = surfaces
        .iter()
        .flat_map(|surface| surface.types.iter().map(|ty| ty.name.as_str()))
        .collect();
    for surface in surfaces {
        if !surface.fields.is_empty() {
            let short = kind_type_name(&surface.kind);
            let name = if named.contains(&short.as_str()) {
                longer_type_name(&surface.kind)
            } else {
                short
            };
            reader.kinds.insert(surface.kind.clone(), name);
        }
    }
    for surface in surfaces {
        reader.surface(surface);
    }
    reader.meta
}

struct Reader<'a> {
    app: &'a str,
    /// The type name of each resource kind of the app.
    kinds: BTreeMap<String, String>,
    /// The named types declared so far, by name, and whether each is an enum.
    declared: BTreeMap<String, (NamedType, bool)>,
    meta: PackageMeta,
}

impl Reader<'_> {
    fn surface(&mut self, surface: &Surface) {
        let own = self.kinds.get(&surface.kind).cloned();
        let renamed = self.named_types(surface, own.as_deref());
        let rename = |name: &str| {
            renamed
                .get(name)
                .cloned()
                .unwrap_or_else(|| name.to_owned())
        };
        if let Some(own) = &own {
            self.own_type(surface, own, &rename);
        }
        for verb in surface.verbs.iter().filter(|verb| !platform_verb(verb)) {
            self.action(verb, own.as_deref(), &rename);
        }
    }

    /// Declares the named types of `surface`, the resource type `own`'s, and gives the name each
    /// is declared under: a type another resource declared differently is this resource's own,
    /// named after it.
    fn named_types(&mut self, surface: &Surface, own: Option<&str>) -> BTreeMap<String, String> {
        let mut renamed: BTreeMap<String, String> = BTreeMap::new();
        for named in &surface.types {
            let name = match self.declared.get(&named.name) {
                Some((declared, _)) if declared == named => continue,
                Some(_) => format!("{}{}", own.unwrap_or_default(), named.name),
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
                    id: name.clone(),
                    name,
                    doc: named.doc.clone(),
                    variants,
                });
            } else {
                let fields = named
                    .fields
                    .iter()
                    .filter_map(|field| self.field(&name, field, &rename))
                    .collect();
                self.meta.types.push(TypeMeta {
                    id: name.clone(),
                    name,
                    doc: named.doc.clone(),
                    fields,
                    search: SearchMeta::default(),
                    holder: false,
                    origin: None,
                });
            }
        }
        renamed
    }

    /// Declares the type `own` of the resource `surface` describes: its top-level fields.
    fn own_type(&mut self, surface: &Surface, own: &str, rename: &dyn Fn(&str) -> String) {
        let fields = surface
            .fields
            .iter()
            .filter(|field| !field.path.contains('.'))
            .filter_map(|field| {
                let typed = tilo_actions::surface::TypeField {
                    name: field.path.clone(),
                    ty: field.ty.clone(),
                    title: field.title.clone(),
                };
                self.field(own, &typed, rename)
            })
            .collect();
        self.meta.types.push(TypeMeta {
            id: own.to_owned(),
            name: own.to_owned(),
            // What people call the resource, when it says more than the type's name.
            doc: if surface.label == own {
                String::new()
            } else {
                surface.label.clone()
            },
            fields,
            search: SearchMeta::default(),
            holder: false,
            origin: None,
        });
    }

    /// Declares the verb `verb` of the resource type `own` (none: a loose verb) as an action
    /// taking its resource first. A verb with a parameter of a type tilolang code cannot give
    /// (bytes, any JSON value) is left out: tilolang code cannot call it.
    fn action(&mut self, verb: &VerbSurface, own: Option<&str>, rename: &dyn Fn(&str) -> String) {
        let mut params = Vec::new();
        if let Some(own) = own {
            params.push(ParamMeta {
                name: snake_case(own),
                ty: own.to_owned(),
                default: None,
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
            let ty = match ty {
                FieldType::Option(_) => ty,
                other if !param.required => FieldType::Option(Box::new(other)),
                other => other,
            };
            let Some(ty) = self.ty(&ty, rename) else {
                return;
            };
            params.push(ParamMeta {
                name: action_name(&param.name),
                ty,
                default: None,
            });
        }
        self.meta.actions.push(ActionMeta {
            id: verb.id.clone(),
            name: action_name(&verb.id),
            doc: verb.description.clone().unwrap_or_default(),
            params,
            result: None,
            class: effect_class(verb.kind),
            proposal: None,
            reaches: Vec::new(),
            label: verb.label.clone(),
            icon: None,
            role: verb.destructive.then(|| "destructive".to_owned()),
            scenario: false,
            present: None,
            toggles: None,
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
        let ty = self.ty(&field.ty, rename)?;
        let merge = match field.ty {
            FieldType::Collection(_) | FieldType::Set(_) | FieldType::Map(_) => {
                MergeKind::Structural
            }
            FieldType::Text | FieldType::RichText => MergeKind::Text,
            _ => MergeKind::Register,
        };
        // The fields of a tuple variant are numbered; tilolang names them.
        let name = match field.name.parse::<u32>() {
            Ok(0) => "value".to_owned(),
            Ok(index) => format!("value{index}"),
            Err(_) => field.name.clone(),
        };
        Some(FieldMeta {
            id: format!("{owner}.{name}"),
            name,
            ty,
            merge,
            doc: field.title.clone(),
            default: None,
            role: None,
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
            FieldType::Identifier => "std.Identifier".to_owned(),
            FieldType::Principal => "Principal".to_owned(),
            FieldType::Resource(kind) => self.resource(kind)?,
            FieldType::Named(name) => {
                let name = rename(name);
                match self.declared.get(&name) {
                    Some((_, true)) => format!("enum {name}"),
                    Some((_, false)) => name,
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
            FieldType::Any | FieldType::Reactions | FieldType::Outbox(_) => return None,
        })
    }

    /// The type of a resource of `kind`: this app's by name, a tilolang package's qualified by
    /// its app; `None` for any kind or another native app's.
    fn resource(&self, kind: &str) -> Option<String> {
        if let Some(name) = self.kinds.get(kind) {
            return Some(name.clone());
        }
        let (app, name) = tilolang_kind(kind)?;
        Some(if app == self.app {
            name.to_owned()
        } else {
            format!("{app}.{name}")
        })
    }
}
