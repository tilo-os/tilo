//! `tilo sdk gen`: the SDK's types for a workspace's apps, from their surfaces.
//!
//! An app publishes its resource kinds, their fields and its actions in its `surface.toml`
//! (`tilo_actions::surface::Surface`). A generated type is that surface in the SDK's language:
//! one class per resource kind, whose attributes are the kind's fields (a read-only field is
//! one with no setter) and whose methods are its actions, taking the action's parameters and
//! answering the value the surface says it answers. A method is the action itself: the class
//! is the SDK's `Resource`, so `document.read_form_fields()` runs `read-form-fields` there.
//!
//! Which apps' types are written: `-w` names a hosted workspace, whose own read of an app
//! answers its surface; without it, every package this build ships that publishes a surface —
//! native and tilolang apps, and the services whose actions are the workspace's too — from the
//! fleet's compiled-in manifests (`fleet_catalogue`), the list the desktop's search and the Store read,
//! so the SDK, those apps and a workspace's programs agree on one surface.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, Result, anyhow};
use clap::{Subcommand, ValueEnum};
use collab_types::surface::SurfaceFile;
use tilo_actions::ActionKind;
use tilo_actions::field_type::{FieldType, NamedType, TypeField};
use tilo_actions::surface::{Surface, VerbSurface};

pub const PYTHON_OUT: &str = "tilo_types";
pub const TYPESCRIPT_OUT: &str = "tilo-types";
pub const OPENAPI_OUT: &str = ".";
const OPENAPI_FILE: &str = "openapi.json";
const SURFACE_FILE: &str = "surface.json";

/// The names the SDK's own resource answers to, which an app's field or action of the same
/// name cannot be reached by: `document.path` is the resource's path whatever field the app
/// declares.
const OWN: [&str; 17] = [
    "workspace",
    "path",
    "kind",
    "fields",
    "children",
    "cursor",
    "raw",
    "load",
    "refresh",
    "flush",
    "do",
    "edit",
    "remove",
    "watch",
    "action",
    "has",
    "many",
];

const PYTHON_WORDS: [&str; 36] = [
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif",
    "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda",
    "nonlocal", "not", "or", "pass", "raise", "return", "self", "try", "while", "with", "yield",
    "False", "None", "True",
];

#[derive(Subcommand)]
pub enum SdkCmd {
    /// Write the types of a workspace's apps, for Python or TypeScript.
    #[command(
        after_help = "Examples:\n  tilo sdk gen --lang python              tilo_types/, from this checkout's apps\n  \
                            tilo sdk gen --lang ts -w jane-doe      tilo-types/, from that workspace's apps\n  \
                            tilo sdk gen --lang python --check      Say whether the types are current\n  \
                            tilo sdk gen --lang openapi --out api   The API's description"
    )]
    Gen {
        /// The language the types are written in, or `openapi` for the API's description.
        #[arg(long)]
        lang: Lang,
        /// The directory to write them in.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Write nothing: say whether what is there is what would be written.
        #[arg(long)]
        check: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Lang {
    Python,
    Ts,
    /// The API's description (`OpenAPI`), with these apps' actions.
    Openapi,
}

#[derive(Clone, Debug)]
pub struct App {
    pub id: String,
    pub title: String,
    /// Its surface, in the form it is kept by
    /// (`collab_types::surface::canonical`).
    pub surface: String,
}

pub fn generate(reach: crate::api::Reach<'_>, workspace: Option<&str>, cmd: SdkCmd) -> ExitCode {
    let SdkCmd::Gen { lang, out, check } = cmd;
    let apps = match surfaces(reach, workspace) {
        Ok(apps) => apps,
        Err(error) => {
            eprintln!("tilo: {error:#}");
            return ExitCode::FAILURE;
        }
    };
    if apps.is_empty() {
        eprintln!(
            "tilo: no app's surface is here: {}",
            workspace.map_or(
                "this build ships no app whose surface it can write types from",
                |_| "no app installed in that workspace has published its surface there",
            )
        );
        return ExitCode::FAILURE;
    }
    let files = match lang {
        Lang::Python => python_files(&apps),
        Lang::Ts => typescript_files(&apps),
        Lang::Openapi => openapi_files(&apps),
    };
    let files = match files {
        Ok(files) => files,
        Err(error) => {
            eprintln!("tilo: {error:#}");
            return ExitCode::FAILURE;
        }
    };
    let out = out.unwrap_or_else(|| {
        PathBuf::from(match lang {
            Lang::Python => PYTHON_OUT,
            Lang::Ts => TYPESCRIPT_OUT,
            Lang::Openapi => OPENAPI_OUT,
        })
    });
    match land(&out, &files, check) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("tilo: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn surfaces(reach: crate::api::Reach<'_>, workspace: Option<&str>) -> Result<Vec<App>> {
    match workspace {
        Some(workspace) => crate::hosted::surfaces(reach, workspace),
        None => fleet(),
    }
}

/// Every package this build ships that publishes a surface — native apps, tilolang packages and
/// the services whose actions a workspace holds (the on-screen keyboard's) — from the fleet's
/// compiled-in manifests (`fleet_catalogue`): its id, its name and the surface its package publishes.
fn fleet() -> Result<Vec<App>> {
    fleet_catalogue::surfaced()
        .map(|(id, name, file)| {
            SurfaceFile::from_json(file).map_err(|error| anyhow!("{id}: {error}"))?;
            Ok(App {
                id: id.to_owned(),
                title: name.to_owned(),
                surface: file.to_owned(),
            })
        })
        .collect()
}

fn land(out: &Path, files: &[(PathBuf, String)], check: bool) -> Result<ExitCode> {
    let mut changed = Vec::new();
    for (name, text) in files {
        let path = out.join(name);
        let current = std::fs::read_to_string(&path).ok();
        if current.as_deref() == Some(text.as_str()) {
            continue;
        }
        changed.push(path.clone());
        if !check {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("making {}", parent.display()))?;
            }
            std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        }
    }
    if check {
        if changed.is_empty() {
            println!("the types are current");
            return Ok(ExitCode::SUCCESS);
        }
        for path in &changed {
            eprintln!("not current: {}", path.display());
        }
        eprintln!("tilo: write them with `tilo sdk gen`");
        return Ok(ExitCode::FAILURE);
    }
    println!("wrote {} files under {}", files.len(), out.display());
    Ok(ExitCode::SUCCESS)
}

struct Types {
    declarations: Vec<Declaration>,
    classes: Vec<Class>,
}

enum Declaration {
    Record {
        name: String,
        doc: String,
        fields: Vec<Member>,
    },
    Cases {
        name: String,
        doc: String,
        cases: Vec<(String, Vec<Member>)>,
    },
}

struct Class {
    name: String,
    kind: String,
    label: String,
    fields: Vec<Member>,
    actions: Vec<Action>,
    /// Each field is written by its own setter, `set-<field>`, not by a field edit
    /// (`Surface::setters`).
    setters: bool,
}

struct Member {
    name: String,
    wire: String,
    ty: Ty,
    title: String,
    readonly: bool,
}

struct Action {
    /// The action's id, as the wire takes it (`read-form-fields`), its name in the SDK's
    /// language (`read_form_fields`, `readFormFields`), and what the app says it does.
    id: String,
    snake: String,
    camel: String,
    params: Vec<Param>,
    result: Option<Ty>,
    kind: ActionKind,
    doc: String,
    /// The named arguments type of its bulk call (`create_many`, `createMany`),
    /// when it has one.
    many: Option<Many>,
}

/// A bulk call's generated arguments type: `CreateArgs`, the list's item.
struct Many {
    args: String,
}

struct Param {
    name: String,
    wire: String,
    ty: Ty,
    required: bool,
}

enum Ty {
    Any,
    Text,
    Int,
    Number,
    Bool,
    List(Box<Self>),
    /// A collection's items, as its read lists them: each with the id
    /// an action takes it by, and its value.
    Collection(Box<Self>),
    Map(Box<Self>),
    Option(Box<Self>),
    Union(Vec<Self>),
    Named(String),
}

type Names = BTreeMap<String, String>;

fn types_of(app: &App) -> Result<Types> {
    let file =
        SurfaceFile::from_json(&app.surface).map_err(|error| anyhow!("{}: {error}", app.id))?;
    let surfaces: Vec<&Surface> = file
        .resources
        .iter()
        .filter(|surface| !tilo_actions::surface::platform_kind(&surface.kind))
        .collect();
    let kinds = class_names(&surfaces);
    let mut types = Types {
        declarations: Vec::new(),
        classes: Vec::new(),
    };
    let mut declared: BTreeMap<String, NamedType> = BTreeMap::new();
    for surface in surfaces {
        let own = kinds
            .get(&surface.kind)
            .cloned()
            .unwrap_or_else(|| camel(&surface.kind));
        let names = types.declare(surface, &own, &kinds, &mut declared);
        let resolver = Resolver {
            names: &names,
            kinds: &kinds,
        };
        let fields = surface
            .fields
            .iter()
            .filter(|field| !field.path.contains('.'))
            .filter(|field| !matches!(field.ty, FieldType::Reactions | FieldType::Outbox(_)))
            .map(|field| {
                let (name, wire) = member(&field.path);
                Member {
                    name,
                    wire,
                    ty: resolver.ty(&field.ty),
                    title: field.title.clone(),
                    readonly: field.readonly,
                }
            })
            .collect();
        let actions = surface
            .verbs
            .iter()
            .filter(|verb| !verb.is_platform())
            .map(|verb| action_of(verb, &resolver))
            .collect();
        types.classes.push(Class {
            name: own,
            kind: surface.kind.clone(),
            label: surface.label.clone(),
            fields,
            actions,
            setters: surface.setters,
        });
    }
    types.name_many_types();
    Ok(types)
}

impl Types {
    /// Declare the named types `surface` lists, answering the name each is written under, by
    /// the name the surface gives it.
    fn declare(
        &mut self,
        surface: &Surface,
        own: &str,
        kinds: &BTreeMap<String, String>,
        declared: &mut BTreeMap<String, NamedType>,
    ) -> Names {
        let mut names = Names::new();
        let mut fresh: Vec<(String, NamedType)> = Vec::new();
        for named in &surface.types {
            let name = match names_of(declared, named) {
                Some((name, true)) => {
                    names.insert(named.name.clone(), name);
                    continue;
                }
                Some((_, false)) => format!("{own}{}", named.name),
                None => named.name.clone(),
            };
            names.insert(named.name.clone(), name.clone());
            declared.insert(name.clone(), named.clone());
            fresh.push((name, named.clone()));
        }
        let resolver = Resolver {
            names: &names,
            kinds,
        };
        for (name, named) in fresh {
            let members = |fields: &[TypeField]| -> Vec<Member> {
                fields
                    .iter()
                    .map(|field| {
                        let (member, wire) = member(&field.name);
                        Member {
                            name: member,
                            wire,
                            ty: resolver.ty(&field.ty),
                            title: field.title.clone(),
                            readonly: false,
                        }
                    })
                    .collect()
            };
            let doc = named.doc.clone();
            if named.is_enum() {
                let cases = named
                    .cases
                    .iter()
                    .map(|case| (case.name.clone(), members(&case.fields)))
                    .collect();
                self.declarations
                    .push(Declaration::Cases { name, doc, cases });
            } else {
                let fields = members(&named.fields);
                self.declarations
                    .push(Declaration::Record { name, doc, fields });
            }
        }
        names
    }
}

impl Types {
    /// Give every action a bulk call can carry a name for its arguments type:
    /// `create` is `CreateArgs`. A group reports no member's value, so only an
    /// undoable action that answers none has one.
    fn name_many_types(&mut self) {
        let mut taken: Vec<String> = self
            .declarations
            .iter()
            .map(|declaration| match declaration {
                Declaration::Record { name, .. } | Declaration::Cases { name, .. } => name.clone(),
            })
            .chain(self.classes.iter().map(|class| class.name.clone()))
            .collect();
        for class in &mut self.classes {
            for action in &mut class.actions {
                if action.kind != ActionKind::Undoable
                    || action.result.is_some()
                    || OWN.contains(&action.id.as_str())
                {
                    continue;
                }
                let base = format!("{}Args", camel(&action.id));
                let mut name = if taken.contains(&base) {
                    format!("{}{base}", class.name)
                } else {
                    base.clone()
                };
                let mut count = 2_u32;
                while taken.contains(&name) {
                    name = format!("{base}{count}");
                    count = count.saturating_add(1);
                }
                taken.push(name.clone());
                action.many = Some(Many { args: name });
            }
        }
    }
}

fn names_of(declared: &BTreeMap<String, NamedType>, named: &NamedType) -> Option<(String, bool)> {
    declared
        .iter()
        .find(|(_, known)| known.name == named.name)
        .map(|(name, known)| (name.clone(), known == named))
}

struct Resolver<'a> {
    names: &'a Names,
    kinds: &'a BTreeMap<String, String>,
}

impl Resolver<'_> {
    fn ty(&self, ty: &FieldType) -> Ty {
        let inner = |ty: &FieldType| Box::new(self.ty(ty));
        match ty {
            FieldType::Text
            | FieldType::Secret
            | FieldType::RichText
            | FieldType::Date
            | FieldType::Time
            | FieldType::Identifier
            | FieldType::Actor => Ty::Text,
            FieldType::Int | FieldType::Duration => Ty::Int,
            FieldType::Float => Ty::Number,
            FieldType::Bool => Ty::Bool,
            FieldType::Collection(item) => Ty::Collection(inner(item)),
            FieldType::List(item) | FieldType::Set(item) => Ty::List(inner(item)),
            FieldType::Map(item) => Ty::Map(inner(item)),
            FieldType::Option(item) => Ty::Option(inner(item)),
            FieldType::Union(cases) => Ty::Union(cases.iter().map(|case| self.ty(case)).collect()),
            FieldType::Named(name) => self.names.get(name).cloned().map_or(Ty::Any, Ty::Named),
            FieldType::Resource(kind) => self.kinds.get(kind).cloned().map_or(Ty::Any, Ty::Named),
            FieldType::Any | FieldType::Json | FieldType::Reactions | FieldType::Outbox(_) => {
                Ty::Any
            }
        }
    }
}

/// The type name of a resource kind: its last segment in upper camel case (`chat.workspace`
/// is `Workspace`), or its last two when a record or enum of the app already has that name
/// (`tilo.workspace.emoji` is `WorkspaceEmoji`).
fn class_names(surfaces: &[&Surface]) -> BTreeMap<String, String> {
    let declared: Vec<&str> = surfaces
        .iter()
        .flat_map(|surface| surface.types.iter())
        .map(|named| named.name.as_str())
        .collect();
    let mut names: BTreeMap<String, String> = BTreeMap::new();
    let mut taken: Vec<String> = Vec::new();
    for surface in surfaces {
        // How many of the kind's own segments the name is written with: one (`Document`)
        // while nothing else of the app has it, then two (`ChannelInfo`), then all of them.
        let segments: Vec<&str> = surface.kind.split('.').collect();
        let mut written = None;
        for count in 1..=segments.len() {
            let from = segments.len().saturating_sub(count);
            let name: String = segments
                .get(from..)
                .unwrap_or_default()
                .iter()
                .map(|segment| camel(segment))
                .collect();
            if !declared.contains(&name.as_str()) && !taken.contains(&name) {
                written = Some(name);
                break;
            }
        }
        // Every name of the kind is one the app declares: the last resort is its whole kind,
        // which no record is named.
        let name = written.unwrap_or_else(|| camel(&surface.kind.replace('.', "-")));
        if !taken.contains(&name) {
            taken.push(name.clone());
        }
        names.insert(surface.kind.clone(), name);
    }
    names
}

/// `name` in upper camel case: `pdf-document` is `PdfDocument`.
fn camel(name: &str) -> String {
    name.split(['-', '_', '.'])
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        })
        .collect()
}

fn action_of(verb: &VerbSurface, resolver: &Resolver<'_>) -> Action {
    // What the action API admits: the name it offers the action by, which is the verb's own id
    // for a native app and the name the app declares (`pin`) for a tilolang one.
    let called = verb.offered();
    let params = verb
        .params
        .iter()
        .map(|param| {
            let (name, wire) = member(&param.name);
            let ty = if param.ty.is_any() {
                FieldType::from_schema(&param.schema)
            } else {
                param.ty.clone()
            };
            Param {
                name,
                wire,
                ty: match ty {
                    FieldType::Option(_) => resolver.ty(&ty),
                    other if !param.required => Ty::Option(Box::new(resolver.ty(&other))),
                    other => resolver.ty(&other),
                },
                required: param.required,
            }
        })
        .collect();
    Action {
        id: called.to_owned(),
        snake: snake(called),
        camel: camel_method(called),
        params,
        result: verb.returns.as_ref().map(|returns| {
            let ty = if returns.ty.is_any() {
                FieldType::from_schema(&returns.schema)
            } else {
                returns.ty.clone()
            };
            resolver.ty(&ty)
        }),
        kind: verb.kind,
        doc: verb.description.clone().unwrap_or_default(),
        many: None,
    }
}

/// A field's, a parameter's or a record field's name in the SDK's language, and the app's own
/// name for it.
fn member(wire: &str) -> (String, String) {
    (wire.replace('-', "_"), wire.to_owned())
}

fn snake(id: &str) -> String {
    id.replace(['-', '.', ':'], "_")
}

fn camel_method(id: &str) -> String {
    let mut out = String::new();
    for (index, word) in id.split(['-', '.', ':']).enumerate() {
        let mut chars = word.chars();
        if index == 0 {
            out.push_str(word);
            continue;
        }
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

fn module_name(id: &str) -> String {
    let last = id.rsplit('.').next().unwrap_or(id);
    let name: String = last
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    if name.starts_with(|c: char| c.is_ascii_digit()) {
        format!("_{name}")
    } else {
        name
    }
}

/// Each app's module in the SDK, by its id: its last id segment (`notes`), and for an app whose
/// last segment another app's id also ends with, the segments before it that tell them apart
/// (`so.tilo.lang.notes` would be `lang_notes` beside `so.tilo.notes`, which stays `notes`). Two
/// modules of one name would be one file, so the shorter id keeps the plain name.
fn module_names(apps: &[App]) -> BTreeMap<String, String> {
    let mut order: Vec<&App> = apps.iter().collect();
    // The shorter id first, so it keeps the plain name; the id breaks a tie, so every run writes
    // the same names.
    order.sort_by(|left, right| {
        left.id
            .split_terminator('.')
            .count()
            .cmp(&right.id.split_terminator('.').count())
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut names: BTreeMap<String, String> = BTreeMap::new();
    for app in order {
        let segments: Vec<String> = app.id.rsplit('.').map(module_name).collect();
        let mut parts = segments.iter();
        let mut name = parts.next().cloned().unwrap_or_default();
        // Grow the name towards the left until no app before it holds it.
        for part in parts {
            if !names.values().any(|used| used == &name) {
                break;
            }
            name = format!("{part}_{name}");
        }
        names.insert(app.id.clone(), name);
    }
    names
}

fn app_path(app: &App) -> &str {
    app.id.rsplit('.').next().unwrap_or(&app.id)
}

fn title_of(app: &App) -> String {
    if app.title.is_empty() {
        app.id.clone()
    } else {
        format!("{} (`{}`)", app.title, app.id)
    }
}

fn python_type(ty: &Ty) -> String {
    match ty {
        Ty::Any => "Any".to_owned(),
        Ty::Text => "str".to_owned(),
        Ty::Int => "int".to_owned(),
        Ty::Number => "float".to_owned(),
        Ty::Bool => "bool".to_owned(),
        Ty::List(item) => format!("list[{}]", python_type(item)),
        Ty::Collection(item) => format!("list[Entry[{}]]", python_type(item)),
        Ty::Map(item) => format!("dict[str, {}]", python_type(item)),
        Ty::Option(item) => format!("{} | None", python_type(item)),
        Ty::Union(cases) => {
            if cases.is_empty() {
                return "Any".to_owned();
            }
            let cases: Vec<String> = cases.iter().map(python_type).collect();
            cases.join(" | ")
        }
        Ty::Named(name) => name.clone(),
    }
}

fn typescript_type(ty: &Ty) -> String {
    match ty {
        Ty::Any => "unknown".to_owned(),
        Ty::Text => "string".to_owned(),
        Ty::Int | Ty::Number => "number".to_owned(),
        Ty::Bool => "boolean".to_owned(),
        Ty::List(item) => {
            let item = typescript_type(item);
            if item.contains(' ') {
                format!("Array<{item}>")
            } else {
                format!("{item}[]")
            }
        }
        Ty::Collection(item) => format!("Array<tilo.Entry<{}>>", typescript_type(item)),
        Ty::Map(item) => format!("Record<string, {}>", typescript_type(item)),
        Ty::Option(item) => format!("{} | null", typescript_type(item)),
        Ty::Union(cases) => {
            if cases.is_empty() {
                return "unknown".to_owned();
            }
            let cases: Vec<String> = cases.iter().map(typescript_type).collect();
            cases.join(" | ")
        }
        Ty::Named(name) => name.clone(),
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('"', "'")
        .replace("*/", "* /")
        .replace('\\', "/")
}

fn comment(title: &str) -> String {
    let title = one_line(title);
    if title.is_empty() {
        String::new()
    } else {
        format!("  # {title}")
    }
}

/// [`comment`] as TypeScript writes one: `//` after a field, never Python's `#`.
fn typescript_comment(title: &str) -> String {
    let title = one_line(title);
    if title.is_empty() {
        String::new()
    } else {
        format!("  // {title}")
    }
}

fn doc(out: &mut String, text: &str, indent: usize) {
    let text = one_line(text);
    if text.is_empty() {
        return;
    }
    let pad = "    ".repeat(indent);
    let _ = writeln!(out, "{pad}\"\"\"{text}\"\"\"");
}

fn comment_block(out: &mut String, lines: &[&str]) {
    out.push_str("/**\n");
    for line in lines {
        if line.is_empty() {
            out.push_str(" *\n");
        } else {
            let _ = writeln!(out, " * {line}");
        }
    }
    out.push_str(" */\n");
}

/// `name` as a TypeScript property may be written: an identifier as it is, and one that is not
/// (a tilolang record's field id, `2od7mwir`) as the quoted string it is on the wire.
fn typescript_name(name: &str) -> String {
    let identifier = name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    if identifier {
        name.to_owned()
    } else {
        format!("\"{name}\"")
    }
}

fn python_name(name: &str) -> String {
    let mut name = name.to_owned();
    if PYTHON_WORDS.contains(&name.as_str()) {
        name.push('_');
    }
    if name.starts_with(|c: char| c.is_ascii_digit()) {
        name.insert(0, '_');
    }
    name
}

fn distinct(fields: &[Member]) -> Vec<(&Member, bool)> {
    let mut seen: Vec<String> = Vec::new();
    fields
        .iter()
        .map(|field| {
            if seen.contains(&field.name) {
                return (field, false);
            }
            seen.push(field.name.clone());
            (field, true)
        })
        .collect()
}

// -- Python -----------------------------------------------------------------

/// The API's description (`openapi.json`), its `runAction` typed per action of `apps`.
///
/// # Errors
///
/// An app's surface is not one.
pub fn openapi_files(apps: &[App]) -> Result<Vec<(PathBuf, String)>> {
    let mut text = serde_json::to_string_pretty(&described(apps)?)?;
    text.push('\n');
    Ok(vec![(PathBuf::from(OPENAPI_FILE), text)])
}

/// The API's description with the actions of `apps`: what `openapi.json` is, and what the
/// TypeScript client is written from.
fn described(apps: &[App]) -> Result<serde_json::Value> {
    let apps: Vec<tilo_api::v1::openapi::App> = apps
        .iter()
        .map(|app| tilo_api::v1::openapi::App {
            id: app.id.clone(),
            title: app.title.clone(),
            surface: app.surface.clone(),
        })
        .collect();
    tilo_api::v1::openapi::document(&apps).map_err(|error| anyhow!(error))
}

/// The Python types of `apps`: a module per app, their package, and the client of the API's
/// operations.
pub fn python_files(apps: &[App]) -> Result<Vec<(PathBuf, String)>> {
    let mut files = Vec::new();
    let modules = module_names(apps);
    for app in apps {
        let types = types_of(app)?;
        let module = modules
            .get(&app.id)
            .cloned()
            .unwrap_or_else(|| module_name(&app.id));
        files.push((
            PathBuf::from(format!("{module}.py")),
            python_module(app, &types),
        ));
    }
    files.push((PathBuf::from("__init__.py"), python_package()));
    files.push((
        PathBuf::from(PYTHON_CLIENT),
        python_client(&described(apps)?),
    ));
    Ok(files)
}

/// The package that holds the apps' modules: one module per app, each naming its own types
/// (`from tilo.types.pdf_viewer import Document`). Two apps may both call a type `Person`, so
/// none of them is re-exported here.
fn python_package() -> String {
    "\"\"\"Types for the apps this workspace has, written by `tilo sdk gen`.\n\
     \n\
     `tilo sdk gen --lang python -w <workspace>` writes them from the apps installed\n\
     there; without `-w`, every app this build ships, native and tilolang. Each app has a module\n\
     of its own, holding a class per resource kind: a `tilo.Resource` named by the\n\
     resource's path, whose fields are the app's fields and whose methods are its actions.\n\
     \n\
     ```python\n\
     from tilo.types.pdf_viewer import Document\n\
     \n\
     form = await Document(ws, \"pdf-viewer/application.pdf\").load()\n\
     fields = await form.read_form_fields()\n\
     ```\n\
     \"\"\"\n"
        .to_owned()
}

/// One app's module: its records and enums, then a class per resource kind.
fn python_module(app: &App, types: &Types) -> String {
    let mut out = String::new();
    let first = types
        .classes
        .first()
        .map_or("Resource", |class| class.name.as_str());
    let kind = types
        .classes
        .first()
        .map_or("", |class| class.kind.as_str());
    let head = [
        format!("Types for {}, from the app's surface.", title_of(app)),
        String::new(),
        format!("`tilo sdk gen --lang python` writes this file from the app's `{SURFACE_FILE}`;"),
        "write it again rather than editing it.".to_owned(),
        String::new(),
        format!(
            "`{first}` is the app's `{kind}` at `{}/<resource>`, read with",
            app_path(app)
        ),
        "`await ….load()`: its fields are the app's fields (read-only ones without a setter)"
            .to_owned(),
        "and its methods are its actions. A method answers what the app declares; `run` follows"
            .to_owned(),
        "one that takes longer. A record is the dict the wire carries, and an".to_owned(),
        "enum whose cases carry fields is an object whose tag the app's schema names. An"
            .to_owned(),
        "action whose name is one the class already has is run with `action(\"…\")`, which"
            .to_owned(),
        "the file says where it is. An undoable action that answers nothing also has a".to_owned(),
        "`<action>_many`, which applies it to every argument object of a list in a few".to_owned(),
        "all-or-nothing steps, all of them one invocation group.".to_owned(),
    ];
    out.push_str("\"\"\"");
    for (index, line) in head.iter().enumerate() {
        if index == 0 {
            let _ = writeln!(out, "{line}");
        } else if line.is_empty() {
            out.push('\n');
        } else {
            let _ = writeln!(out, "{line}");
        }
    }
    out.push_str("\"\"\"\n\n");
    out.push_str("from __future__ import annotations\n\n");
    out.push_str("from typing import Any, Literal, NotRequired, TypedDict\n");
    let _ = writeln!(out, "from tilo import Entry, Resource\n");
    let mut all: Vec<String> = types
        .declarations
        .iter()
        .map(|declaration| match declaration {
            Declaration::Record { name, .. } | Declaration::Cases { name, .. } => name.clone(),
        })
        .collect();
    all.extend(types.classes.iter().map(|class| class.name.clone()));
    all.extend(
        types
            .classes
            .iter()
            .flat_map(|class| class.actions.iter())
            .filter_map(|action| action.many.as_ref().map(|many| many.args.clone())),
    );
    out.push_str("__all__ = [\n");
    for name in &all {
        let _ = writeln!(out, "    \"{name}\",");
    }
    out.push_str("]\n\n");
    for declaration in &types.declarations {
        python_declaration(&mut out, declaration);
    }
    for class in &types.classes {
        for action in &class.actions {
            if let Some(many) = &action.many {
                python_args(&mut out, &many.args, action);
            }
        }
    }
    let surface = tilo_api::v1::surface_version(&app.id, &app.surface);
    for class in &types.classes {
        python_class(&mut out, class, &surface);
    }
    out
}

fn python_declaration(out: &mut String, declaration: &Declaration) {
    match declaration {
        Declaration::Record {
            name,
            doc: text,
            fields,
        } => {
            // A field the app names with a Python word (`from`) keeps that name, written as the
            // mapping form of a TypedDict. Its types are strings: that form is read as the file
            // runs, and the types it names are declared later in it; the rest read as a class
            // of annotated fields.
            let written = fields
                .iter()
                .all(|field| python_name(&field.name) == field.name);
            if !written {
                let entries: Vec<String> = fields
                    .iter()
                    .map(|field| {
                        format!(
                            "    \"{}\": \"{}\"{}",
                            field.name,
                            python_type(&field.ty),
                            comment(&field.title)
                        )
                    })
                    .collect();
                let _ = writeln!(out, "{name} = TypedDict(\"{name}\", {{");
                for entry in entries {
                    let _ = writeln!(out, "{entry},");
                }
                out.push_str("})\n");
                doc(out, text, 0);
                out.push('\n');
                return;
            }
            let _ = writeln!(out, "class {name}(TypedDict):");
            doc(out, text, 1);
            if fields.is_empty() {
                out.push_str("    pass\n\n");
                return;
            }
            for (field, first) in distinct(fields) {
                if !first {
                    let _ = writeln!(
                        out,
                        "    # the app declares `{}` twice: the first is the one here.",
                        field.name
                    );
                    continue;
                }
                let _ = writeln!(
                    out,
                    "    {}: {}{}",
                    field.name,
                    python_type(&field.ty),
                    comment(&field.title)
                );
            }
            out.push('\n');
        }
        Declaration::Cases {
            name,
            doc: text,
            cases,
        } => {
            if cases.iter().all(|(_, fields)| fields.is_empty()) {
                let values: Vec<String> = cases
                    .iter()
                    .map(|(case, _)| format!("\"{case}\""))
                    .collect();
                let _ = writeln!(out, "{name} = Literal[{}]", values.join(", "));
                doc(out, text, 0);
                out.push('\n');
                return;
            }
            let names: Vec<&str> = cases.iter().map(|(case, _)| case.as_str()).collect();
            let text = with_said(
                &one_line(text),
                format!(
                    "One of {}: an object whose case the app tags it with.",
                    names.join(", ")
                ),
            );
            let _ = writeln!(out, "{name} = dict[str, Any]");
            doc(out, &text, 0);
            out.push('\n');
        }
    }
}

/// `class` as a Python class, made from the surface whose version is `surface`: its actions
/// name that version, so a workspace whose app publishes another refuses them instead of
/// running what these types no longer describe.
fn python_class(out: &mut String, class: &Class, surface: &str) {
    let _ = writeln!(out, "class {}(Resource):", class.name);
    let mut text = one_line(&class.label);
    if !text.is_empty() {
        text.push_str(" — ");
    }
    let _ = write!(text, "`{}`.", class.kind);
    doc(out, &text, 1);
    let _ = writeln!(out, "    __surface__ = \"{surface}\"");
    if class.setters {
        let _ = writeln!(out, "    __setters__ = True");
    }
    if class.fields.is_empty() && class.actions.is_empty() {
        out.push('\n');
        return;
    }
    let mut taken = python_fields(out, class);
    let actions = python_actions(out, class, &mut taken);
    if !actions.is_empty() {
        let names: Vec<String> = actions.iter().map(|name| format!("\"{name}\"")).collect();
        let _ = writeln!(
            out,
            "    # The actions a call reaches before the resource is read: `await tracker.create(...)`\n\
             \x20   # needs no `load()` first.\n\
             \x20   __actions__ = frozenset({{{}}})",
            names.join(", ")
        );
        out.push('\n');
    }
    out.push('\n');
}

/// `class`'s fields as Python attributes and properties; answers the names they take.
fn python_fields(out: &mut String, class: &Class) -> Vec<String> {
    let mut taken: Vec<String> = Vec::new();
    for (field, first) in distinct(&class.fields) {
        let name = python_name(&field.name);
        if !first {
            let _ = writeln!(
                out,
                "    # the app declares `{}` twice: the first is the one here.",
                field.wire
            );
            continue;
        }
        if OWN.contains(&field.wire.as_str()) {
            let _ = writeln!(
                out,
                "    # the app's `{wire}` field is the SDK's own `{wire}`: read it with\n\
                 \x20   # `fields[\"{wire}\"]`.",
                wire = field.wire
            );
            continue;
        }
        taken.push(name.clone());
        if field.readonly || name != field.wire {
            // A read-only field is a property; so is one whose wire name Python cannot name (a
            // tilolang record keeps its fields under ids, some of them starting with a digit):
            // the property reads the key the wire has, so `note.updated` and `note._2od7mwir`
            // both reach the field the app keeps.
            let _ = writeln!(out, "    @property");
            let _ = writeln!(out, "    def {name}(self) -> {}:", python_type(&field.ty));
            let said = if field.title.is_empty() {
                "Read only.".to_owned()
            } else if name == field.wire {
                field.title.clone()
            } else {
                format!("{} (`{}` on the wire).", field.title, field.wire)
            };
            doc(out, &said, 2);
            let _ = writeln!(out, "        return self.fields[\"{}\"]", field.wire);
            if !field.readonly {
                let _ = writeln!(out);
                let _ = writeln!(out, "    @{name}.setter");
                let _ = writeln!(
                    out,
                    "    def {name}(self, value: {}) -> None:",
                    python_type(&field.ty)
                );
                let _ = writeln!(out, "        self.edit(\"{}\", value=value)", field.wire);
            }
        } else {
            let _ = writeln!(
                out,
                "    {name}: {}{}",
                python_type(&field.ty),
                comment(&field.title)
            );
        }
    }
    if !class.fields.is_empty() {
        out.push('\n');
    }
    taken
}

/// `class`'s actions as Python methods, each named apart from the names `taken` already;
/// answers the methods written.
fn python_actions(out: &mut String, class: &Class, taken: &mut Vec<String>) -> Vec<String> {
    let mut actions: Vec<String> = Vec::new();
    for action in &class.actions {
        let name = python_name(&action.snake);
        if OWN.contains(&action.id.as_str()) {
            let _ = writeln!(
                out,
                "    # the app's `{id}` action is the SDK's own: run it with `action(\"{id}\")`.",
                id = action.id
            );
            continue;
        }
        if taken.contains(&name) {
            let _ = writeln!(
                out,
                "    # the app's `{id}` action is named as a field of this kind: run it with\n\
                 \x20   # `action(\"{id}\")`.",
                id = action.id
            );
            continue;
        }
        taken.push(name.clone());
        actions.push(name);
        python_action(out, action);
        out.push('\n');
        if let Some(many) = &action.many {
            let bulk = python_name(&format!("{}_many", action.snake));
            if taken.contains(&bulk) {
                let _ = writeln!(
                    out,
                    "    # the app's `{id}` has a bulk call named as a field of this kind:\n\
                     \x20   # run each with `action(\"{id}\")`.",
                    id = action.id
                );
                continue;
            }
            taken.push(bulk.clone());
            actions.push(bulk);
            python_many(out, action, many);
            out.push('\n');
        }
    }
    actions
}

fn python_action(out: &mut String, action: &Action) {
    let name = python_name(&action.snake);
    let mut params: Vec<&Param> = action
        .params
        .iter()
        .filter(|param| param.required)
        .collect();
    params.extend(action.params.iter().filter(|param| !param.required));
    let _ = write!(out, "    def {name}(self");
    if params.is_empty() {
        out.push(')');
    } else {
        let one = params.len() == 1
            && params.iter().all(|param| param.required)
            && params
                .iter()
                .all(|param| python_type(&param.ty).len() <= 40);
        if one {
            let line = params.first().map_or_else(String::new, |param| {
                format!(
                    ", *, {}: {}",
                    python_name(&param.name),
                    python_type(&param.ty)
                )
            });
            let _ = write!(out, "{line})");
        } else {
            out.push_str(",\n        *,\n");
            for param in &params {
                let _ = writeln!(
                    out,
                    "        {}: {}{},",
                    python_name(&param.name),
                    python_type(&param.ty),
                    if param.required { "" } else { " = None" }
                );
            }
            out.push_str("    )");
        }
    }
    let result = python_result(action);
    let _ = writeln!(out, " -> {result}:");
    if action.doc.is_empty() {
        doc(out, "Runs the app's action.", 2);
    } else {
        doc(out, &action.doc, 2);
    }
    if params.is_empty() {
        let _ = writeln!(out, "        return self.action(\"{}\")", action.id);
        return;
    }
    // The arguments as keywords, when each is one the app's own name for it may be written as:
    // an action whose name is a Python word (`from`) takes the dict form instead.
    if action.params.iter().all(|param| param.required)
        && action
            .params
            .iter()
            .all(|param| python_name(&param.wire) == param.wire)
    {
        let arguments: Vec<String> = action
            .params
            .iter()
            .map(|param| format!("{}={}", param.wire, python_name(&param.name)))
            .collect();
        let _ = writeln!(
            out,
            "        return self.action(\"{}\", {})",
            action.id,
            arguments.join(", ")
        );
        return;
    }
    out.push_str("        args: dict[str, Any] = {}\n");
    for param in &action.params {
        if param.required {
            let _ = writeln!(
                out,
                "        args[\"{}\"] = {}",
                param.wire,
                python_name(&param.name)
            );
        } else {
            let _ = writeln!(out, "        if {} is not None:", python_name(&param.name));
            let _ = writeln!(
                out,
                "            args[\"{}\"] = {}",
                param.wire,
                python_name(&param.name)
            );
        }
    }
    let _ = writeln!(out, "        return self.action(\"{}\", **args)", action.id);
}

/// The arguments of a bulk call, as the wire takes them: each parameter under its own
/// name, `NotRequired` when it may be left out.
fn python_args(out: &mut String, name: &str, action: &Action) {
    // The mapping form, since `from __future__ import annotations` leaves a class form's
    // `NotRequired` unread: a TypedDict made here knows its optional keys.
    let _ = writeln!(out, "{name} = TypedDict(\"{name}\", {{");
    for param in &action.params {
        let ty = python_type(&param.ty);
        let value = if param.required {
            format!("\"{ty}\"")
        } else {
            format!("NotRequired[\"{ty}\"]")
        };
        let _ = writeln!(out, "    \"{}\": {value},", param.wire);
    }
    out.push_str("})\n");
    let text = format!("The arguments of `{}`.", action.id);
    doc(out, &text, 0);
    out.push('\n');
}

/// The bulk call of `action`: `items`, one all-or-nothing step per call, all one
/// invocation group.
fn python_many(out: &mut String, action: &Action, many: &Many) {
    let name = python_name(&format!("{}_many", action.snake));
    let _ = writeln!(
        out,
        "    def {name}(self, items: list[{}]) -> list[dict[str, Any]]:",
        many.args
    );
    let text = format!(
        "Runs `{}` for each of `items`: one all-or-nothing step per call, all of them one \
         invocation group that History lists once and one undo takes back.",
        action.id
    );
    doc(out, &text, 2);
    let _ = writeln!(out, "        return self.many(\"{}\", items)", action.id);
}

/// What an action answers: what the app declares, and the `Invocation` that
/// follows one still running.
fn python_result(action: &Action) -> String {
    // An action call waits for its invocation and answers its result: a caller
    // never gets an `Invocation` back from a typed method, only what the action
    // produced.
    action
        .result
        .as_ref()
        .map_or_else(|| "None".to_owned(), python_type)
}

// -- The Python client of `/v1` --------------------------------------------

/// The module the Python client is written to. No app's module can be named so: an app's is
/// written with no dot at all ([`module_name`]).
const PYTHON_CLIENT: &str = "api_v1.py";

/// The Python client of `described`, the API's description: its schemas as typed dicts, and an
/// operation each as a method, in two classes: the account's operations, and a workspace's,
/// which name it themselves. The SDK's own `Tilo` and `Workspace` are those classes, carried by
/// the transport the SDK was given.
#[expect(
    clippy::too_many_lines,
    reason = "one client, from its schemas to its operations, in the order the description gives them"
)]
fn python_client(described: &serde_json::Value) -> String {
    let mut out = String::new();
    out.push_str(
        "\"\"\"The client of Tilo's API, `/v1`, written by `tilo sdk gen` from the API's\n\
         description: its schemas as types, and each of its operations as a method. Write it\n\
         again rather than editing it.\n\
         \"\"\"\n\n\
         from __future__ import annotations\n\n\
         from collections.abc import AsyncIterator, Mapping\n\
         from dataclasses import dataclass\n\
         from typing import Any, Literal, NotRequired, TypeAlias, TypedDict\n\n",
    );
    let empty = serde_json::Map::new();
    let schemas = at(described, "/components/schemas")
        .as_object()
        .unwrap_or(&empty);
    for (name, schema) in schemas {
        // An app action's own schema (`notes.notebook.rename.Arguments`): its app's module
        // types it.
        if name.contains('.') {
            continue;
        }
        let said = at(schema, "/description").as_str().unwrap_or_default();
        if said.trim().is_empty() {
            let _ = writeln!(
                out,
                "#: The `{name}` schema, as the API's requests and answers carry it."
            );
        } else {
            let _ = writeln!(out, "#: {}", one_line(said));
        }
        match at(schema, "/properties").as_object() {
            Some(properties) => {
                let required: Vec<&str> = at(schema, "/required")
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
                    .collect();
                let _ = writeln!(out, "{name} = TypedDict(\"{name}\", {{");
                for (property, shape) in properties {
                    let ty = py_of(shape);
                    if required.contains(&property.as_str()) {
                        let _ = writeln!(out, "    {property:?}: \"{ty}\",");
                    } else {
                        let _ = writeln!(out, "    {property:?}: NotRequired[\"{ty}\"],");
                    }
                }
                out.push_str("})\n\n");
            }
            None => {
                let _ = writeln!(out, "{name}: TypeAlias = {}\n", py_of(schema));
            }
        }
    }
    out.push_str(
        "\n@dataclass(frozen=True)\n\
         class Operation:\n\
         \x20   \"\"\"One operation of the API: how a request for it is written, and what it answers.\"\"\"\n\
         \x20   method: str\n\
         \x20   path: str\n\
         \x20   query: tuple[str, ...]\n\
         \x20   body: str | None\n\
         \x20   answer: str\n\
         \x20   mutating: bool\n\n",
    );
    let operations = operations_of(described);
    out.push_str("OPERATIONS: dict[str, Operation] = {\n");
    for operation in &operations {
        let query: Vec<String> = operation
            .query
            .iter()
            .map(|name| format!("{name:?}"))
            .collect();
        let _ = writeln!(
            out,
            "    {:?}: Operation(\n        method={:?},\n        path={:?},\n        query={},\n        body={},\n        answer={:?},\n        mutating={},\n    ),",
            operation.id,
            operation.method,
            operation.path,
            if query.is_empty() {
                "()".to_owned()
            } else {
                format!("({},)", query.join(", "))
            },
            match &operation.body {
                None => "None".to_owned(),
                Some(Body::Json(_)) => "\"json\"".to_owned(),
                Some(Body::Bytes) => "\"bytes\"".to_owned(),
            },
            match operation.answer {
                Answer::Json(_) => "json",
                Answer::Events => "events",
                Answer::Bytes => "bytes",
            },
            if operation.mutating { "True" } else { "False" }
        );
    }
    out.push_str("}\n\n");
    for (class, said, workspace) in [
        (
            "AccountOperations",
            "The operations of the API outside every workspace: the API itself, and the key's account.",
            false,
        ),
        (
            "WorkspaceOperations",
            "The operations of one workspace, which names it in each request itself.",
            true,
        ),
    ] {
        let _ = writeln!(out, "class {class}:");
        let _ = writeln!(out, "    \"\"\"{said}\"\"\"\n");
        out.push_str(
            "    async def _send(\n\
             \x20       self,\n\
             \x20       operation: Operation,\n\
             \x20       params: Mapping[str, Any],\n\
             \x20       body: Any,\n\
             \x20       headers: Mapping[str, str],\n\
             \x20   ) -> Any:\n\
             \x20       \"\"\"One request for `operation`, its answer read as the operation answers it.\"\"\"\n\
             \x20       raise NotImplementedError\n\n\
             \x20   def _stream(\n\
             \x20       self,\n\
             \x20       operation: Operation,\n\
             \x20       params: Mapping[str, Any],\n\
             \x20       body: Any,\n\
             \x20       headers: Mapping[str, str],\n\
             \x20   ) -> AsyncIterator[dict[str, Any]]:\n\
             \x20       \"\"\"One request for `operation`, its answer's events as they come.\"\"\"\n\
             \x20       raise NotImplementedError\n\n",
        );
        for operation in operations
            .iter()
            .filter(|operation| operation.path.starts_with(WORKSPACE_PATH) == workspace)
        {
            python_operation(&mut out, operation, workspace);
        }
        out.push('\n');
    }
    out
}

/// One operation of the client as a Python method: its path parameters, its query and its body,
/// and the answer the description names.
fn python_operation(out: &mut String, operation: &Operation<'_>, workspace: bool) {
    let name = method_snake(operation.id);
    let path: Vec<(&str, bool)> = operation
        .params
        .iter()
        .copied()
        .filter(|(name, _)| !(workspace && *name == "workspace"))
        .collect();
    let mut arguments: Vec<String> = Vec::new();
    for (param, required) in &path {
        if *required {
            arguments.push(format!("{param}: str"));
        } else {
            arguments.push(format!("{param}: str | None = None"));
        }
    }
    let body = operation.py_body.as_deref();
    let body_expression = match body {
        Some("bytes") => {
            arguments.push("body: bytes".to_owned());
            "body"
        }
        Some("dict[str, Any]") => {
            arguments.push("body: dict[str, Any] | None = None".to_owned());
            "body"
        }
        Some(ty) => {
            arguments.push(format!("body: {ty}"));
            "body"
        }
        None => "None",
    };
    let keyword: Vec<String> = operation
        .query
        .iter()
        .map(|name| format!("{name}: str | None = None"))
        .chain(["headers: Mapping[str, str] | None = None".to_owned()])
        .collect();
    arguments.push("*".to_owned());
    arguments.extend(keyword);
    let mut params: Vec<String> = path
        .iter()
        .map(|(name, _)| format!("{name:?}: {name}"))
        .collect();
    params.extend(
        operation
            .query
            .iter()
            .map(|name| format!("{name:?}: {name}")),
    );
    let params = format!("{{{}}}", params.join(", "));
    let answer = match &operation.answer {
        Answer::Events => "AsyncIterator[dict[str, Any]]",
        Answer::Bytes => "bytes",
        Answer::Json(_) => operation.py_answer.as_str(),
    };
    let declaration = if matches!(operation.answer, Answer::Events) {
        "def"
    } else {
        "async def"
    };
    let _ = writeln!(
        out,
        "    {declaration} {name}(self, {}) -> {answer}:",
        arguments.join(", ")
    );
    doc(out, operation.summary, 2);
    let call = format!(
        "OPERATIONS[{:?}], {params}, {body_expression}, headers or {{}}",
        operation.id
    );
    match operation.answer {
        Answer::Events => {
            let _ = writeln!(out, "        return self._stream({call})\n");
        }
        _ => {
            let _ = writeln!(out, "        return await self._send({call})\n");
        }
    }
}

/// `id` as a Python method name: `getApi` is `get_api`.
fn method_snake(id: &str) -> String {
    let characters: Vec<char> = id.chars().collect();
    let mut out = String::new();
    for (index, character) in characters.iter().enumerate() {
        if !character.is_uppercase() {
            out.push(*character);
            continue;
        }
        let previous = index.checked_sub(1).and_then(|at| characters.get(at));
        let following = index.checked_add(1).and_then(|at| characters.get(at));
        let boundary = previous
            .is_some_and(|before| before.is_lowercase() || before.is_ascii_digit())
            || (previous.is_some_and(|before| before.is_uppercase())
                && following.is_some_and(|after| after.is_lowercase()));
        if boundary && !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
        out.extend(character.to_lowercase());
    }
    out
}

/// The Python type of a JSON schema of the description.
fn py_of(schema: &serde_json::Value) -> String {
    use serde_json::Value;
    let Some(map) = schema.as_object() else {
        return "Any".to_owned();
    };
    if let Some(reference) = map.get("$ref").and_then(Value::as_str) {
        let name = reference.rsplit('/').next().unwrap_or_default();
        // An app action's own schema is its app module's to type.
        return if name.contains('.') {
            "Any".to_owned()
        } else {
            name.to_owned()
        };
    }
    if let Some(value) = map.get("const") {
        return match value {
            Value::String(_) => "str".to_owned(),
            Value::Bool(_) => "bool".to_owned(),
            Value::Number(number) if number.is_f64() => "float".to_owned(),
            Value::Number(_) => "int".to_owned(),
            _ => "Any".to_owned(),
        };
    }
    if let Some(values) = map.get("enum").and_then(Value::as_array) {
        let literals: Vec<String> = values
            .iter()
            .filter_map(Value::as_str)
            .map(|value| format!("{value:?}"))
            .collect();
        if literals.len() == values.len() {
            return format!("Literal[{}]", literals.join(", "));
        }
        return union_py(values.iter().map(py_of).collect());
    }
    for cases in ["anyOf", "oneOf"] {
        if let Some(cases) = map.get(cases).and_then(Value::as_array) {
            return union_py(cases.iter().map(py_of).collect());
        }
    }
    match map.get("type") {
        Some(Value::Array(types)) => union_py(
            types
                .iter()
                .filter_map(Value::as_str)
                .map(|ty| py_of_type(ty, schema))
                .collect(),
        ),
        Some(Value::String(ty)) => py_of_type(ty, schema),
        _ if map.contains_key("properties") => py_of_type("object", schema),
        _ => "Any".to_owned(),
    }
}

fn py_of_type(ty: &str, schema: &serde_json::Value) -> String {
    match ty {
        "string" => "str".to_owned(),
        "integer" => "int".to_owned(),
        "number" => "float".to_owned(),
        "boolean" => "bool".to_owned(),
        "null" => "None".to_owned(),
        "array" => {
            if let Some(items) = at(schema, "/prefixItems").as_array() {
                let items: Vec<String> = items.iter().map(py_of).collect();
                format!("tuple[{}]", items.join(", "))
            } else {
                format!("list[{}]", py_of(at(schema, "/items")))
            }
        }
        "object" if at(schema, "/properties").is_object() => "dict[str, Any]".to_owned(),
        "object" if at(schema, "/additionalProperties").is_object() => {
            format!("dict[str, {}]", py_of(at(schema, "/additionalProperties")))
        }
        "object" => "dict[str, Any]".to_owned(),
        _ => "Any".to_owned(),
    }
}

/// `types` as one: each once, `Any` for none.
fn union_py(types: Vec<String>) -> String {
    let mut once: Vec<String> = Vec::new();
    for ty in types {
        if !once.contains(&ty) {
            once.push(ty);
        }
    }
    if once.is_empty() {
        return "Any".to_owned();
    }
    once.join(" | ")
}

// -- TypeScript -------------------------------------------------------------

/// The words a TypeScript file may not give a function of its own: the page's own `document`,
/// `window` and the like, which a module-level name would shadow.
const TS_GLOBALS: [&str; 14] = [
    "document",
    "window",
    "history",
    "location",
    "navigator",
    "screen",
    "status",
    "name",
    "self",
    "parent",
    "top",
    "event",
    "origin",
    "print",
];

/// The TypeScript types of `apps`: a module per app, and the one that names them all.
pub fn typescript_files(apps: &[App]) -> Result<Vec<(PathBuf, String)>> {
    let mut files = Vec::new();
    let mut modules = Vec::new();
    let names = module_names(apps);
    for app in apps {
        let types = types_of(app)?;
        let module = names
            .get(&app.id)
            .cloned()
            .unwrap_or_else(|| module_name(&app.id));
        files.push((
            PathBuf::from(format!("{module}.ts")),
            typescript_module(app, &types),
        ));
        modules.push(module);
    }
    files.push((PathBuf::from("index.ts"), typescript_package(&modules)));
    files.push((
        PathBuf::from(TS_CLIENT),
        typescript_client(&described(apps)?),
    ));
    Ok(files)
}

/// The module that names every app's: `pdfViewer.Document`, `pdfViewer.pdfDocument`. Two apps
/// may both call a type `Person`, so each app's module is exported as its own namespace.
fn typescript_package(modules: &[String]) -> String {
    let mut out = String::new();
    comment_block(
        &mut out,
        &[
            "Types for the apps this workspace has, written by `tilo sdk gen`.",
            "",
            "`tilo sdk gen --lang ts -w <workspace>` writes them from the apps installed",
            "there; without `-w`, every app this build ships, native and tilolang. Each app has a",
            "module of its own, exported here as a namespace: a type is the SDK's `Resource`",
            "of one resource kind, made by its factory with the resource's path.",
            "",
            "```ts",
            "import { pdfViewer } from \"./tilo-types/index.ts\";",
            "",
            "const form: pdfViewer.Document = pdfViewer.pdfDocument(ws, \"pdf-viewer/app.pdf\");",
            "await form.load();",
            "```",
        ],
    );
    out.push('\n');
    for module in modules {
        // The module is named as its file is (`pdf_viewer`); the namespace a caller writes is
        // camel case (`pdfViewer.Document`).
        let _ = writeln!(
            out,
            "export * as {} from \"./{module}.ts\";",
            camel_method(&module.replace('_', "-"))
        );
    }
    out
}

/// One app's module: its records and enums, then an interface per resource kind with the
/// factory that makes one at a path.
fn typescript_module(app: &App, types: &Types) -> String {
    let mut out = String::new();
    let first = types
        .classes
        .first()
        .map_or("Resource", |class| class.name.as_str());
    let kind = types
        .classes
        .first()
        .map_or("", |class| class.kind.as_str());
    let head = [
        format!("Types for {}, from the app's surface.", title_of(app)),
        String::new(),
        format!("`tilo sdk gen --lang ts` writes this file from the app's `{SURFACE_FILE}`; write"),
        "it again rather than editing it.".to_owned(),
        String::new(),
        format!("`{first}` is a `Resource` of the app's `{kind}`, made by its factory with the"),
        format!(
            "resource's path (`{}/<resource>`): its attributes are the app's fields",
            app_path(app)
        ),
        "(read-only ones `readonly`) and its methods are the app's actions, named in camel"
            .to_owned(),
        "case and taking the arguments the wire takes them under, each answering what the".to_owned(),
        "action produced once it finished. A record is the object the wire carries, and an enum whose"
            .to_owned(),
        "cases carry fields is an object whose tag the app's schema names. An action whose"
            .to_owned(),
        "name is one the interface already has is run with `action(\"…\")`, which the file"
            .to_owned(),
        "says where it is. An undoable action that answers nothing also has a `<action>Many`,\n\
         which applies it to every argument object of a list in a few all-or-nothing steps,\n\
         all of them one invocation group."
            .to_owned(),
    ];
    let head: Vec<&str> = head.iter().map(String::as_str).collect();
    comment_block(&mut out, &head);
    out.push('\n');
    out.push_str("import { resourceProxy } from \"tilo-sdk\";\n");
    out.push_str("import type * as tilo from \"tilo-sdk\";\n\n");
    let _ = writeln!(
        out,
        "/** The version of the surface these types were made from: a workspace whose app\n\
         \x20* publishes another refuses their actions rather than run what they no longer\n\
         \x20* describe. */\n\
         export const SURFACE = \"{}\";\n",
        tilo_api::v1::surface_version(&app.id, &app.surface)
    );
    for declaration in &types.declarations {
        typescript_declaration(&mut out, declaration);
    }
    for class in &types.classes {
        for action in &class.actions {
            if let Some(many) = &action.many {
                typescript_args_decl(&mut out, &many.args, action);
            }
        }
    }
    for class in &types.classes {
        typescript_class(&mut out, class);
    }
    out
}

fn typescript_declaration(out: &mut String, declaration: &Declaration) {
    match declaration {
        Declaration::Record {
            name,
            doc: text,
            fields,
        } => {
            let text = one_line(text);
            if !text.is_empty() {
                let _ = writeln!(out, "/** {text} */");
            }
            let _ = writeln!(out, "export interface {name} {{");
            for (field, first) in distinct(fields) {
                if !first {
                    let _ = writeln!(
                        out,
                        "  // the app declares `{}` twice: the first is the one here.",
                        field.name
                    );
                    continue;
                }
                let _ = writeln!(
                    out,
                    "  {}: {};{}",
                    typescript_name(&field.name),
                    typescript_type(&field.ty),
                    typescript_comment(&field.title)
                );
            }
            out.push_str("}\n\n");
        }
        Declaration::Cases {
            name,
            doc: text,
            cases,
        } => {
            let said = one_line(text);
            if cases.iter().all(|(_, fields)| fields.is_empty()) {
                let values: Vec<String> = cases
                    .iter()
                    .map(|(case, _)| format!("\"{case}\""))
                    .collect();
                let text = with_said(&said, format!("One of {}.", values.join(", ")));
                let _ = writeln!(out, "/** {text} */");
                let _ = writeln!(out, "export type {name} = {};\n", values.join(" | "));
                return;
            }
            let names: Vec<&str> = cases.iter().map(|(case, _)| case.as_str()).collect();
            let text = with_said(
                &said,
                format!(
                    "One of {}: an object whose case the app tags it with.",
                    names.join(", ")
                ),
            );
            let _ = writeln!(out, "/** {text} */");
            let _ = writeln!(out, "export type {name} = Record<string, unknown>;\n");
        }
    }
}

/// `said` with `more` after it, one of them alone when the other is empty.
fn with_said(said: &str, more: String) -> String {
    if said.is_empty() {
        return more;
    }
    format!("{said} {more}")
}

fn typescript_class(out: &mut String, class: &Class) {
    let mut text = one_line(&class.label);
    if !text.is_empty() {
        text.push_str(" — ");
    }
    let _ = write!(text, "`{}`.", class.kind);
    let _ = writeln!(out, "/** {text} */");
    let _ = writeln!(
        out,
        "export interface {} extends tilo.Resource {{",
        class.name
    );
    let mut taken = typescript_fields(out, class);
    let actions = typescript_actions(out, class, &mut taken);
    out.push_str("}\n\n");
    let listed: Vec<String> = actions.iter().map(|name| format!("\"{name}\"")).collect();
    let _ = writeln!(
        out,
        "/** The actions `{class}` reaches before it is read: `ws` calls them without a\n\
         \x20 `load()` first. */",
        class = class.name
    );
    let _ = writeln!(
        out,
        "export const {}_ACTIONS = [{}] as const;\n",
        class.name.to_uppercase(),
        listed.join(", ")
    );
    let made = factory(class);
    let _ = write!(
        out,
        "/** The `{}` kind at `path`. */\n\
         export function {made}(ws: tilo.Workspace, path: string): {} {{\n\
         \x20 return resourceProxy(ws, path, SURFACE, {}_ACTIONS{}) as unknown as {};\n\
         }}\n\n",
        class.kind,
        class.name,
        class.name.to_uppercase(),
        if class.setters { ", true" } else { "" },
        class.name,
    );
}

/// `class`'s fields as interface members; answers the names they take.
fn typescript_fields(out: &mut String, class: &Class) -> Vec<String> {
    let mut taken: Vec<String> = Vec::new();
    for (field, first) in distinct(&class.fields) {
        if !first {
            let _ = writeln!(
                out,
                "  // the app declares `{}` twice: the first is the one here.",
                field.wire
            );
            continue;
        }
        if OWN.contains(&field.wire.as_str()) {
            let _ = writeln!(
                out,
                "  // the app's `{wire}` field is the SDK's own `{wire}`.",
                wire = field.wire
            );
            continue;
        }
        taken.push(field.name.clone());
        let _ = writeln!(
            out,
            "  {}{}: {};{}",
            if field.readonly { "readonly " } else { "" },
            typescript_name(&field.name),
            typescript_type(&field.ty),
            typescript_comment(&field.title)
        );
    }
    taken
}

/// `class`'s actions as interface methods, each named apart from the names `taken` already;
/// answers the methods written.
fn typescript_actions(out: &mut String, class: &Class, taken: &mut Vec<String>) -> Vec<String> {
    let mut actions: Vec<String> = Vec::new();
    for action in &class.actions {
        if OWN.contains(&action.id.as_str()) {
            let _ = writeln!(
                out,
                "  // the app's `{id}` action is the SDK's own: run it with `action(\"{id}\")`.",
                id = action.id
            );
            continue;
        }
        // A name ending in `Many` is how a bulk call of the action before it is reached.
        if action.camel.ends_with("Many") {
            let _ = writeln!(
                out,
                "  // the app's `{id}` action is named as the SDK's bulk calls (`<action>Many`):\n\
                 \x20 // run it with `action(\"{id}\")`.",
                id = action.id
            );
            continue;
        }
        if taken.contains(&action.camel) {
            let _ = writeln!(
                out,
                "  // the app's `{id}` action is named as a field of this kind: run it with\n\
                 \x20 // `action(\"{id}\")`.",
                id = action.id
            );
            continue;
        }
        taken.push(action.camel.clone());
        actions.push(action.camel.clone());
        typescript_action(out, action);
        if let Some(many) = &action.many {
            let bulk = format!("{}Many", action.camel);
            if taken.contains(&bulk) {
                let _ = writeln!(
                    out,
                    "  // the app's `{id}` has a bulk call named as a field of this kind: run\n\
                     \x20 // each with `action(\"{id}\")`.",
                    id = action.id
                );
                continue;
            }
            taken.push(bulk.clone());
            actions.push(bulk);
            typescript_many(out, action, many);
        }
    }
    actions
}

/// The function that makes a `PdfDocument` at a path: `pdfDocument`, or `notesPage` where the
/// plain name is the page's own (`document`).
/// The words a module-level function may not be named in TypeScript: a factory named for a kind
/// whose type is one of them (`Default`) takes its app's name before it (`viewsDefault`).
const TS_RESERVED: &[&str] = &[
    "arguments",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "eval",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "interface",
    "let",
    "new",
    "null",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "undefined",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

fn factory(class: &Class) -> String {
    let mut chars = class.name.chars();
    let plain: String = chars.next().map_or_else(String::new, |first| {
        first.to_lowercase().chain(chars).collect()
    });
    if !TS_GLOBALS.contains(&plain.as_str()) && !TS_RESERVED.contains(&plain.as_str()) {
        return plain;
    }
    // The app the kind belongs to: the segment before its type (`pdf`, `views`).
    let prefix = class
        .kind
        .rsplit_once('.')
        .map_or(class.kind.as_str(), |(prefix, _)| prefix);
    let app = prefix.rsplit('.').next().unwrap_or(prefix);
    let named = format!("{}{}", camel(app), class.name);
    let mut chars = named.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_lowercase().chain(chars).collect()
    })
}

fn typescript_action(out: &mut String, action: &Action) {
    let text = one_line(&action.doc);
    if !text.is_empty() {
        let _ = writeln!(out, "  /** {text} */");
    }
    let required: Vec<&Param> = action
        .params
        .iter()
        .filter(|param| param.required)
        .collect();
    let optional: Vec<&Param> = action
        .params
        .iter()
        .filter(|param| !param.required)
        .collect();
    out.push_str("  ");
    let _ = write!(out, "{}(", action.camel);
    if !required.is_empty() {
        let _ = write!(out, "args: {}", typescript_args(&required, &optional));
    } else if !optional.is_empty() {
        let _ = write!(out, "args?: {}", typescript_args(&required, &optional));
    }
    let _ = writeln!(out, "): Promise<{}>;", typescript_result(action));
}

/// What an action's arguments object is: each parameter under the name the wire takes it by.
fn typescript_args(required: &[&Param], optional: &[&Param]) -> String {
    let mut out = String::from("{\n");
    for line in typescript_arg_lines(required, optional) {
        let _ = writeln!(out, "    {line}");
    }
    out.push_str("  }");
    out
}

/// Each of an action's arguments as an object's field: the wire's name, optional ones
/// absent rather than null.
fn typescript_arg_lines(required: &[&Param], optional: &[&Param]) -> Vec<String> {
    let mut lines = Vec::new();
    for param in required {
        lines.push(format!("{}: {};", param.wire, typescript_type(&param.ty)));
    }
    for param in optional {
        let inner = match &param.ty {
            Ty::Option(item) => typescript_type(item),
            other => typescript_type(other),
        };
        lines.push(format!("{}?: {inner};", param.wire));
    }
    lines
}

/// The arguments of a bulk call, as the wire takes them.
fn typescript_args_decl(out: &mut String, name: &str, action: &Action) {
    let text = format!("The arguments of `{}`.", action.id);
    let _ = writeln!(out, "/** {text} */");
    let _ = writeln!(out, "export interface {name} {{");
    let required: Vec<&Param> = action
        .params
        .iter()
        .filter(|param| param.required)
        .collect();
    let optional: Vec<&Param> = action
        .params
        .iter()
        .filter(|param| !param.required)
        .collect();
    for line in typescript_arg_lines(&required, &optional) {
        let _ = writeln!(out, "  {line}");
    }
    out.push_str("}\n\n");
}

/// The bulk call of `action`: `items`, one all-or-nothing step per call, all one
/// invocation group.
fn typescript_many(out: &mut String, action: &Action, many: &Many) {
    let text = one_line(&format!(
        "Runs `{}` for each of `items`: one all-or-nothing step per call, all of them one invocation group \
         that History lists once and one undo takes back.",
        action.id
    ));
    let _ = writeln!(out, "  /** {text} */");
    let _ = writeln!(
        out,
        "  {}Many(items: {}[]): Promise<tilo.ActionAnswer[]>;",
        action.camel, many.args
    );
}

/// What an action answers: what the app declares, and the `Invocation` that
/// follows one still running.
fn typescript_result(action: &Action) -> String {
    // An action call waits for its invocation and answers its result: a caller
    // never gets an `Invocation` back from a typed method, only what the action
    // produced.
    action
        .result
        .as_ref()
        .map_or_else(|| "void".to_owned(), typescript_type)
}
// -- The TypeScript client of `/v1` ----------------------------------------

/// The module the TypeScript client is written to. No app's module can be named so: an app's
/// is written with underscores alone ([`module_name`]).
const TS_CLIENT: &str = "api-v1.ts";
/// The schema each event of a stream is read as, as the description names it.
const STREAM_EVENT: &str = "Event";

/// One operation of the description, as the client calls it.
struct Operation<'a> {
    id: &'a str,
    method: String,
    path: &'a str,
    summary: &'a str,
    /// Its path parameters, then its query parameters, each with whether it is required.
    params: Vec<(&'a str, bool)>,
    query: Vec<&'a str>,
    /// What it takes: JSON of a type, or bytes (`None` for nothing).
    body: Option<Body>,
    /// What it takes, as the Python client writes it.
    py_body: Option<String>,
    /// What it answers on success.
    answer: Answer,
    /// What it answers, as the Python client writes it.
    py_answer: String,
    mutating: bool,
}

enum Body {
    Json(String),
    Bytes,
}

enum Answer {
    Json(String),
    Events,
    Bytes,
}

/// The TypeScript client of `described`, the API's description: its schemas as types, and an
/// operation each as a method, in two classes: the account's operations, and a workspace's,
/// which name it themselves. The SDK's own `Tilo` and `Workspace` are those classes, carried by
/// the transport the SDK was given.
fn typescript_client(described: &serde_json::Value) -> String {
    let mut out = String::new();
    comment_block(
        &mut out,
        &[
            "The client of Tilo's API, `/v1`, written by `tilo sdk gen` from the API's",
            "description: its schemas as types, and each of its operations as a method. Write it",
            "again rather than editing it.",
        ],
    );
    out.push('\n');
    let empty = serde_json::Map::new();
    let schemas = at(described, "/components/schemas")
        .as_object()
        .unwrap_or(&empty);
    for (name, schema) in schemas {
        // An app action's own schema (`notes.notebook.rename.Arguments`): its app's module
        // types it.
        if name.contains('.') {
            continue;
        }
        // Every type the client exports says what it is, the description's words or its name.
        if at(schema, "/description")
            .as_str()
            .is_none_or(|said| said.trim().is_empty())
        {
            let _ = writeln!(
                out,
                "/** A `{name}`, as the API's requests and answers carry one. */"
            );
        } else {
            typescript_doc(&mut out, schema, "");
        }
        match schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
        {
            Some(_) => {
                let _ = writeln!(out, "export interface {name} {{");
                for line in object_fields(schema, "  ") {
                    let _ = writeln!(out, "{line}");
                }
                out.push_str("}\n\n");
            }
            None => {
                let _ = writeln!(out, "export type {name} = {};\n", ts_of(schema));
            }
        }
    }
    let operations = operations_of(described);
    typescript_operations(&mut out, &operations);
    for (class, doc, workspace) in [
        (
            "AccountOperations",
            "The operations of the API outside every workspace: the API itself, and the key's account.",
            false,
        ),
        (
            "WorkspaceOperations",
            "The operations of one workspace, which names it in each request itself.",
            true,
        ),
    ] {
        let _ = writeln!(out, "/** {doc} */\nexport abstract class {class} {{");
        out.push_str(
            "  /** @internal One request for `operation`, its answer read as the operation answers. */\n\
             \x20 protected abstract send(\n\
             \x20   operation: Operation,\n\
             \x20   params: Record<string, string | undefined>,\n\
             \x20   body: unknown,\n\
             \x20   headers: Record<string, string>,\n\
             \x20 ): Promise<unknown>;\n\
             \x20 /** @internal One request for `operation`, its answer's events as they come. */\n\
             \x20 protected abstract stream(\n\
             \x20   operation: Operation,\n\
             \x20   params: Record<string, string | undefined>,\n\
             \x20   body: unknown,\n\
             \x20   headers: Record<string, string>,\n\
             \x20 ): AsyncIterableIterator<",
        );
        let _ = writeln!(out, "{STREAM_EVENT}>;\n");
        for operation in operations
            .iter()
            .filter(|operation| operation.path.starts_with(WORKSPACE_PATH) == workspace)
        {
            typescript_method(&mut out, operation, workspace);
        }
        out.pop();
        out.push_str("}\n\n");
    }
    out
}

/// What each of `operations` is, as `OPERATIONS` tells the client: how a request for it is
/// written, and what it answers.
fn typescript_operations(out: &mut String, operations: &[Operation<'_>]) {
    out.push_str(
        "/** One operation of the API: how a request for it is written, and what it answers. */\n\
         export interface Operation {\n\
         \x20 readonly method: string;\n\
         \x20 /** Its path, its parameters in braces. */\n\
         \x20 readonly path: string;\n\
         \x20 /** The query parameters it reads. */\n\
         \x20 readonly query: readonly string[];\n\
         \x20 readonly body: \"json\" | \"bytes\" | null;\n\
         \x20 readonly answer: \"json\" | \"events\" | \"bytes\";\n\
         \x20 /** Whether it changes something, so it takes an `Idempotency-Key`. */\n\
         \x20 readonly mutating: boolean;\n\
         }\n\n\
         /** The API's operations, by their `operationId`. */\n\
         export const OPERATIONS = {\n",
    );
    for operation in operations {
        let query: Vec<String> = operation
            .query
            .iter()
            .map(|name| format!("\"{name}\""))
            .collect();
        let _ = writeln!(
            out,
            "  {}: {{ method: \"{}\", path: \"{}\", query: [{}], body: {}, answer: \"{}\", mutating: {} }},",
            operation.id,
            operation.method,
            operation.path,
            query.join(", "),
            match operation.body {
                None => "null",
                Some(Body::Json(_)) => "\"json\"",
                Some(Body::Bytes) => "\"bytes\"",
            },
            match operation.answer {
                Answer::Json(_) => "json",
                Answer::Events => "events",
                Answer::Bytes => "bytes",
            },
            operation.mutating
        );
    }
    out.push_str("} as const satisfies Record<string, Operation>;\n\n");
}

/// What `pointer` names in `value`, `null` where nothing is.
fn at<'a>(value: &'a serde_json::Value, pointer: &str) -> &'a serde_json::Value {
    static NULL: serde_json::Value = serde_json::Value::Null;
    value.pointer(pointer).unwrap_or(&NULL)
}

/// Where a workspace's operations are: it names the workspace itself.
const WORKSPACE_PATH: &str = "/v1/workspaces/{workspace}";

fn typescript_method(out: &mut String, operation: &Operation<'_>, workspace: bool) {
    let params: Vec<String> = operation
        .params
        .iter()
        .filter(|(name, _)| !(workspace && *name == "workspace"))
        .map(|(name, required)| format!("{}{}: string", name, if *required { "" } else { "?" }))
        .chain(
            operation
                .query
                .iter()
                .map(|name| format!("{name}?: string")),
        )
        .collect();
    // A default stands only where every argument after it has one too.
    let required = operation
        .params
        .iter()
        .any(|(name, required)| *required && !(workspace && *name == "workspace"))
        || match &operation.body {
            Some(Body::Json(ty)) => ty != "Record<string, unknown>",
            Some(Body::Bytes) => true,
            None => false,
        };
    let mut arguments = if params.is_empty() {
        "params: Record<string, never>".to_owned()
    } else {
        format!("params: {{ {} }}", params.join("; "))
    };
    if !required {
        arguments.push_str(" = {}");
    }
    let body = match &operation.body {
        None => "undefined",
        Some(Body::Json(ty)) => {
            let _ = write!(arguments, ", body: {ty}");
            if ty == "Record<string, unknown>" {
                arguments.push_str(" = {}");
            }
            "body"
        }
        Some(Body::Bytes) => {
            arguments.push_str(", body: Uint8Array");
            "body"
        }
    };
    arguments.push_str(", headers: Record<string, string> = {}");
    let _ = writeln!(out, "  /** {} */", one_line(operation.summary));
    let call = format!("(OPERATIONS.{}, params, {body}, headers)", operation.id);
    let ty = match &operation.answer {
        Answer::Events => {
            let _ = writeln!(
                out,
                "  {}({arguments}): AsyncIterableIterator<{STREAM_EVENT}> {{\n    return this.stream{call};\n  }}\n",
                operation.id
            );
            return;
        }
        Answer::Json(ty) => ty.as_str(),
        Answer::Bytes => "Uint8Array",
    };
    let _ = writeln!(
        out,
        "  {}({arguments}): Promise<{ty}> {{\n    return this.send{call} as Promise<{ty}>;\n  }}\n",
        operation.id
    );
}

/// Every operation `described` lists, in its order.
fn operations_of(described: &serde_json::Value) -> Vec<Operation<'_>> {
    let mut operations = Vec::new();
    let Some(paths) = at(described, "/paths").as_object() else {
        return operations;
    };
    for (path, methods) in paths {
        let Some(methods) = methods.as_object() else {
            continue;
        };
        for (method, operation) in methods {
            let parameters = at(operation, "/parameters")
                .as_array()
                .map_or(&[][..], Vec::as_slice);
            let named = |place: &str| -> Vec<(&str, bool)> {
                parameters
                    .iter()
                    .filter(|parameter| at(parameter, "/in").as_str() == Some(place))
                    .filter_map(|parameter| {
                        Some((
                            at(parameter, "/name").as_str()?,
                            at(parameter, "/required").as_bool() == Some(true),
                        ))
                    })
                    .collect()
            };
            let body = at(operation, "/requestBody/content")
                .as_object()
                .and_then(|content| {
                    content.get("application/json").map_or_else(
                        || (!content.is_empty()).then_some(Body::Bytes),
                        |json| Some(Body::Json(ts_of(at(json, "/schema")))),
                    )
                });
            let py_body = at(operation, "/requestBody/content")
                .as_object()
                .and_then(|content| {
                    content.get("application/json").map_or_else(
                        || (!content.is_empty()).then_some("bytes".to_owned()),
                        |json| Some(py_of(at(json, "/schema"))),
                    )
                });
            let mut answers: Vec<String> = Vec::new();
            let mut py_answers: Vec<String> = Vec::new();
            let mut answer = None;
            for (status, response) in at(operation, "/responses")
                .as_object()
                .into_iter()
                .flatten()
            {
                if !status.starts_with('2') {
                    continue;
                }
                let content = at(response, "/content").as_object();
                if let Some(json) = content.and_then(|content| content.get("application/json")) {
                    let ty = ts_of(at(json, "/schema"));
                    if !answers.contains(&ty) {
                        answers.push(ty);
                    }
                    let py = py_of(at(json, "/schema"));
                    if !py_answers.contains(&py) {
                        py_answers.push(py);
                    }
                } else if content.is_some_and(|content| content.contains_key("text/event-stream")) {
                    answer = Some(Answer::Events);
                } else {
                    answer = Some(Answer::Bytes);
                }
            }
            operations.push(Operation {
                id: at(operation, "/operationId").as_str().unwrap_or_default(),
                method: method.to_uppercase(),
                path,
                summary: at(operation, "/summary").as_str().unwrap_or_default(),
                params: named("path"),
                query: named("query").into_iter().map(|(name, _)| name).collect(),
                body,
                py_body,
                answer: answer.unwrap_or_else(|| Answer::Json(union_of(answers))),
                py_answer: union_py(py_answers),
                mutating: parameters.iter().any(|parameter| {
                    at(parameter, "/$ref")
                        .as_str()
                        .is_some_and(|reference| reference.ends_with("/IdempotencyKey"))
                }),
            });
        }
    }
    operations
}

/// The TypeScript type of a JSON schema of the description.
fn ts_of(schema: &serde_json::Value) -> String {
    use serde_json::Value;
    let Some(map) = schema.as_object() else {
        return "unknown".to_owned();
    };
    if let Some(reference) = map.get("$ref").and_then(Value::as_str) {
        let name = reference.rsplit('/').next().unwrap_or_default();
        // An app action's own schema is its app module's to type.
        return if name.contains('.') { "unknown" } else { name }.to_owned();
    }
    if let Some(value) = map.get("const") {
        return value.to_string();
    }
    if let Some(values) = map.get("enum").and_then(Value::as_array) {
        return union_of(values.iter().map(Value::to_string).collect());
    }
    for cases in ["anyOf", "oneOf"] {
        if let Some(cases) = map.get(cases).and_then(Value::as_array) {
            return union_of(cases.iter().map(ts_of).collect());
        }
    }
    match map.get("type") {
        Some(Value::Array(types)) => union_of(
            types
                .iter()
                .filter_map(Value::as_str)
                .map(|ty| ts_of_type(ty, schema))
                .collect(),
        ),
        Some(Value::String(ty)) => ts_of_type(ty, schema),
        _ if map.contains_key("properties") => ts_of_type("object", schema),
        _ => "unknown".to_owned(),
    }
}

fn ts_of_type(ty: &str, schema: &serde_json::Value) -> String {
    match ty {
        "string" => "string".to_owned(),
        "integer" | "number" => "number".to_owned(),
        "boolean" => "boolean".to_owned(),
        "null" => "null".to_owned(),
        "array" => {
            if let Some(items) = at(schema, "/prefixItems").as_array() {
                let items: Vec<String> = items.iter().map(ts_of).collect();
                format!("[{}]", items.join(", "))
            } else {
                let item = ts_of(at(schema, "/items"));
                if item.contains(' ') {
                    format!("Array<{item}>")
                } else {
                    format!("{item}[]")
                }
            }
        }
        "object" if at(schema, "/properties").is_object() => {
            format!("{{ {} }}", object_fields(schema, "").join(" "))
        }
        "object" if at(schema, "/additionalProperties").is_object() => {
            format!(
                "Record<string, {}>",
                ts_of(at(schema, "/additionalProperties"))
            )
        }
        "object" => "Record<string, unknown>".to_owned(),
        _ => "unknown".to_owned(),
    }
}

/// `types` as one: each once, `unknown` for none.
fn union_of(types: Vec<String>) -> String {
    let mut once: Vec<String> = Vec::new();
    for ty in types {
        if !once.contains(&ty) {
            once.push(ty);
        }
    }
    if once.is_empty() {
        return "unknown".to_owned();
    }
    once.join(" | ")
}

/// An object schema's properties as an interface's fields, each with what it is.
fn object_fields(schema: &serde_json::Value, indent: &str) -> Vec<String> {
    let required: Vec<&str> = at(schema, "/required")
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect();
    let mut lines = Vec::new();
    for (name, property) in at(schema, "/properties").as_object().into_iter().flatten() {
        if !indent.is_empty() {
            let mut said = String::new();
            typescript_doc(&mut said, property, indent);
            lines.extend(said.lines().map(str::to_owned));
        }
        lines.push(format!(
            "{indent}{}{}: {};",
            typescript_name(name),
            if required.contains(&name.as_str()) {
                ""
            } else {
                "?"
            },
            ts_of(property)
        ));
    }
    lines
}

/// What `schema` says it is, as a doc comment at `indent`.
fn typescript_doc(out: &mut String, schema: &serde_json::Value, indent: &str) {
    let text = one_line(at(schema, "/description").as_str().unwrap_or_default());
    if !text.is_empty() {
        let _ = writeln!(out, "{indent}/** {text} */");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two apps whose ids end in the same word would write one module file: the shorter id keeps
    /// the plain name, the longer one takes the segment before it, and either alone has the plain
    /// name. What the generator writes otherwise is what the SDK packages ship, in both
    /// languages, and the `OpenAPI` document.
    #[test]
    fn two_apps_of_one_name_get_a_module_each() {
        let short = fleet()
            .expect("the fleet's surfaces read")
            .into_iter()
            .find(|app| app.id == "so.tilo.notes")
            .expect("the fleet ships notes");
        let mut long = short.clone();
        long.id = "so.tilo.lang.notes".to_owned();
        let names = module_names(&[long.clone(), short.clone()]);
        assert_eq!(names[&short.id], "notes");
        assert_eq!(names[&long.id], "lang_notes");
        let names = module_names(std::slice::from_ref(&long));
        assert_eq!(names[&long.id], "notes");
    }

    /// The TypeScript client is the API's description in TypeScript: a method for each of its
    /// operations, in the class of the account's or of a workspace's, which binds the workspace
    /// itself; its answers are the description's schemas, and an app action's own schema is
    /// left to the app's module.
    #[test]
    fn the_typescript_client_has_a_method_per_operation() {
        let notes = fleet()
            .expect("the fleet's surfaces read")
            .into_iter()
            .find(|app| app.id == "so.tilo.notes")
            .expect("the fleet ships notes");
        let files = typescript_files(std::slice::from_ref(&notes)).expect("ts generates");
        let client = &files
            .iter()
            .find(|(name, _)| name == Path::new(TS_CLIENT))
            .expect("the client is written")
            .1;
        for operation in tilo_api::v1::operations::OPERATIONS.iter() {
            assert!(
                client.contains(&format!("\n  {}(params", operation.id)),
                "{} has no method:\n{client}",
                operation.id
            );
        }
        for written in [
            "export interface ActionAnswer {",
            "runAction(params: { path: string; action: string }, body: Record<string, unknown> = {}, headers: Record<string, string> = {}): Promise<ActionAnswer>",
            "putContent(params: { hash: string }, body: Uint8Array, headers: Record<string, string> = {}): Promise<PutContent>",
            "events(params: { path?: string } = {}, headers: Record<string, string> = {}): AsyncIterableIterator<Event>",
            "getContent(params: { hash: string }, headers: Record<string, string> = {}): Promise<Uint8Array>",
            "listAccountResources(params: { path?: string; cursor?: string; limit?: string; kind?: string } = {}",
        ] {
            assert!(client.contains(written), "{written}:\n{client}");
        }
        assert!(!client.contains(".Arguments"), "{client}");
    }

    /// The Python client is the API's description in Python: a method for each of its operations,
    /// in the class of the account's or of a workspace's, its answers the description's schemas,
    /// and an app action's own schema left to the app's module.
    #[test]
    fn the_python_client_has_a_method_per_operation() {
        let notes = fleet()
            .expect("the fleet's surfaces read")
            .into_iter()
            .find(|app| app.id == "so.tilo.notes")
            .expect("the fleet ships notes");
        let files = python_files(std::slice::from_ref(&notes)).expect("python generates");
        let client = &files
            .iter()
            .find(|(name, _)| name == Path::new(PYTHON_CLIENT))
            .expect("the client is written")
            .1;
        for operation in tilo_api::v1::operations::OPERATIONS.iter() {
            let name = method_snake(operation.id);
            assert!(
                client.contains(&format!("def {name}(self")),
                "{} has no method:\n{client}",
                operation.id
            );
        }
        for written in [
            "class AccountOperations:",
            "class WorkspaceOperations:",
            "async def run_action(self, path: str, action: str, body: dict[str, Any] | None = None, *, headers: Mapping[str, str] | None = None) -> ActionAnswer:",
            "async def put_content(self, hash: str, body: bytes, *, headers: Mapping[str, str] | None = None) -> PutContent:",
            "def events(self, *, path: str | None = None, headers: Mapping[str, str] | None = None) -> AsyncIterator[dict[str, Any]]:",
            "async def get_content(self, hash: str, *, headers: Mapping[str, str] | None = None) -> bytes:",
            "async def list_account_resources(self, *, path: str | None = None, cursor: str | None = None, limit: str | None = None, kind: str | None = None, headers: Mapping[str, str] | None = None) -> Listing:",
            "Listing = TypedDict(\"Listing\", {",
        ] {
            assert!(client.contains(written), "{written}:\n{client}");
        }
        assert!(!client.contains(".Arguments"), "{client}");
    }

    /// The kinds and verbs the platform publishes on every app's surface are no part of the app's
    /// types: an effect the app queues (`tilolang.effect`, whose `$send` is the device's) is no
    /// class of its module, and the stores' `$` verbs are no methods of any class, in either
    /// language or in the API's description. The app's own kind and action stay.
    #[test]
    fn platform_kinds_and_dollar_verbs_are_no_types() {
        let toml = r#"
schema = 1

[[resources]]
kind = "tilolang.effect"
label = "A queued effect"
version = 1

[[resources.verbs]]
id = "$send"
label = "Hand the effect over"
kind = "undoable"

[[resources]]
kind = "tl.so.tilo.timer.Board"
label = "Board"
version = 1

[[resources.verbs]]
id = "$create"
label = "Create"
kind = "undoable"

[[resources.verbs]]
id = "migrate"
label = "Migrate"
kind = "undoable"

[[resources.verbs]]
id = "extend"
name = "extend"
label = "Extend"
kind = "undoable"
"#;
        let surface = tilo_surface_compile::compile(toml).expect("timer surface parses");
        let timer = App {
            id: "so.tilo.timer".to_owned(),
            title: "Timer".to_owned(),
            surface,
        };

        let py_files = python_files(std::slice::from_ref(&timer)).expect("python generates");
        let python = &py_files
            .iter()
            .find(|(name, _)| name == Path::new("timer.py"))
            .expect("timer.py is written")
            .1;
        assert!(python.contains("class Board"), "{python}");
        assert!(python.contains("def extend("), "{python}");
        for platform in [
            "class Effect",
            "tilolang.effect",
            "$send",
            "$create",
            "migrate",
        ] {
            assert!(!python.contains(platform), "{platform} leaked:\n{python}");
        }

        let ts_files = typescript_files(std::slice::from_ref(&timer)).expect("ts generates");
        let ts = &ts_files
            .iter()
            .find(|(name, _)| name == Path::new("timer.ts"))
            .expect("timer.ts is written")
            .1;
        assert!(ts.contains("export interface Board"), "{ts}");
        for platform in [
            "interface Effect",
            "tilolang.effect",
            "$send",
            "$create",
            "migrate",
        ] {
            assert!(!ts.contains(platform), "{platform} leaked:\n{ts}");
        }

        let openapi = openapi_files(std::slice::from_ref(&timer)).expect("openapi generates");
        let doc: serde_json::Value =
            serde_json::from_str(&openapi[0].1).expect("openapi JSON parses");
        let text = doc.to_string();
        for platform in ["tilolang.effect", "$send", "migrate"] {
            assert!(!text.contains(platform), "{platform} leaked:\n{text}");
        }
        assert!(text.contains("tl.so.tilo.timer.Board.extend"), "{text}");
        // The API still describes a field edit a caller runs, at the app's own kind.
        assert!(text.contains("tl.so.tilo.timer.Board.$create"), "{text}");
    }

    /// `FieldType::Duration` travels as whole seconds (an integer), so Python renders `int`
    /// (never `float`, which the binder refuses), TypeScript renders `number`, and `OpenAPI`
    /// keeps the integer schema and unit description for both parameters and results.
    #[test]
    fn duration_parameter_and_result_generate_integer_types() {
        let toml = r#"
schema = 1

[[resources]]
kind = "tl.so.tilo.timer.Board"
label = "Board"
version = 1

[[resources.verbs]]
id = "extend"
name = "extend"
label = "Extend"
kind = "undoable"
returns = { type = "duration", schema = { type = "integer", description = "Duration in whole seconds" } }

[[resources.verbs.params]]
name = "span"
required = true
type = "duration"
schema = { type = "integer", description = "Duration in whole seconds" }

[[resources.verbs.params]]
name = "extra"
required = false
type = { option = "duration" }
schema = { type = "integer", description = "Duration in whole seconds" }
"#;
        let surface = tilo_surface_compile::compile(toml).expect("timer surface parses");
        let timer = App {
            id: "so.tilo.timer".to_owned(),
            title: "Timer".to_owned(),
            surface,
        };

        let py_files = python_files(std::slice::from_ref(&timer)).expect("python generates");
        let python = &py_files
            .iter()
            .find(|(name, _)| name == Path::new("timer.py"))
            .expect("timer.py is written")
            .1;
        let expected_python_sig = concat!(
            "    def extend(self,\n",
            "        *,\n",
            "        span: int,\n",
            "        extra: int | None = None,\n",
            "    ) -> int:\n",
        );
        assert!(python.contains(expected_python_sig), "{python}");
        assert!(
            !python.contains("float"),
            "Duration must not render as float in Python:\n{python}"
        );

        let ts_files = typescript_files(std::slice::from_ref(&timer)).expect("ts generates");
        let ts = &ts_files
            .iter()
            .find(|(name, _)| name == Path::new("timer.ts"))
            .expect("timer.ts is written")
            .1;
        assert!(ts.contains("  extend("), "{ts}");
        assert!(ts.contains("span: number"), "{ts}");
        assert!(ts.contains("extra?: number"), "{ts}");
        assert!(ts.contains("): Promise<number>;"), "{ts}");

        let openapi = openapi_files(std::slice::from_ref(&timer)).expect("openapi generates");
        let doc: serde_json::Value =
            serde_json::from_str(&openapi[0].1).expect("openapi JSON parses");
        let schemas = &doc["components"]["schemas"];
        let args = &schemas["tl.so.tilo.timer.Board.extend.Arguments"]["properties"];
        assert_eq!(args["span"]["type"], "integer");
        assert!(
            args["span"]["description"]
                .as_str()
                .is_some_and(|d| d.contains("whole seconds")),
            "{args:#}"
        );
        let result = &schemas["tl.so.tilo.timer.Board.extend.Result"];
        assert_eq!(result["type"], "integer");
        assert!(
            result["description"]
                .as_str()
                .is_some_and(|d| d.contains("whole seconds")),
            "{result:#}"
        );
    }
}
