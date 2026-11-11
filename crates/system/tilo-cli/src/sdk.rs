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
//! answers its surface; without it, the apps committed in this checkout (`apps/*`).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, Result, anyhow};
use clap::{Subcommand, ValueEnum};
use collab_types::surface::SurfaceFile;
use serde::Deserialize;
use tilo_actions::ActionKind;
use tilo_actions::field_type::{FieldType, NamedType, TypeField};
use tilo_actions::surface::{Surface, VerbSurface};

/// Where `--lang python` writes its types without `--out`.
pub const PYTHON_OUT: &str = "tilo_types";
/// Where `--lang ts` writes its types without `--out`.
pub const TYPESCRIPT_OUT: &str = "tilo-types";
/// An app's surface, and its manifest, in the app's directory.
const SURFACE_FILE: &str = "surface.toml";
const MANIFEST_FILE: &str = "tilo-app.toml";

/// The names the SDK's own resource answers to, which an app's field or action of the same
/// name cannot be reached by: `document.path` is the resource's path whatever field the app
/// declares.
const OWN: [&str; 16] = [
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
];

/// The words a Python name may not be.
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
                            tilo sdk gen --lang python --check      Say whether the types are current"
    )]
    Gen {
        /// The language the types are written in.
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
}

/// One app whose types are generated: its id, what people call it, and the bytes of its
/// `surface.toml`.
#[derive(Clone, Debug)]
pub struct App {
    pub id: String,
    pub title: String,
    pub surface: String,
}

/// `tilo sdk gen`.
pub fn generate(door: Option<&str>, workspace: Option<&str>, cmd: SdkCmd) -> ExitCode {
    let SdkCmd::Gen { lang, out, check } = cmd;
    let apps = match surfaces(door, workspace) {
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
                "run this in a Tilo checkout, whose apps commit theirs (apps/*/surface.toml)",
                |_| "no app installed in that workspace has published its surface there",
            )
        );
        return ExitCode::FAILURE;
    }
    let files = match lang {
        Lang::Python => python_files(&apps),
        Lang::Ts => typescript_files(&apps),
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

/// The apps whose types are generated: the ones installed in `workspace`, or else the ones of
/// the checkout this runs in.
fn surfaces(door: Option<&str>, workspace: Option<&str>) -> Result<Vec<App>> {
    match workspace {
        Some(workspace) => crate::hosted::surfaces(door, workspace),
        None => committed(Path::new("apps")),
    }
}

/// The apps of a checkout's `apps` directory: each with a manifest and a committed surface.
fn committed(apps: &Path) -> Result<Vec<App>> {
    let dirs = std::fs::read_dir(apps).with_context(|| {
        format!(
            "reading {}: name a workspace with -w to write a hosted workspace's types",
            apps.display()
        )
    })?;
    let mut dirs: Vec<PathBuf> = dirs
        .filter_map(Result::ok)
        .map(|dir| dir.path())
        .filter(|dir| dir.join(SURFACE_FILE).is_file() && dir.join(MANIFEST_FILE).is_file())
        .collect();
    dirs.sort();
    dirs.iter().map(|dir| app_of(dir)).collect()
}

#[derive(Deserialize)]
struct Manifest {
    package: Package,
}

#[derive(Deserialize)]
struct Package {
    id: String,
    #[serde(default)]
    name: String,
}

/// The app of the directory `dir`: its manifest's id and name, and its `surface.toml`.
fn app_of(dir: &Path) -> Result<App> {
    let read = |name: &str| {
        let path = dir.join(name);
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))
    };
    let manifest: Manifest = toml::from_str(&read(MANIFEST_FILE)?)
        .with_context(|| format!("{}", dir.join(MANIFEST_FILE).display()))?;
    Ok(App {
        id: manifest.package.id,
        title: manifest.package.name,
        surface: read(SURFACE_FILE)?,
    })
}

/// Write `files` under `out`, or with `check` say whether what is there already is them.
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

// -- what a surface says ----------------------------------------------------

/// One app's types: the records and enums its fields name, and a class per resource kind.
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
    /// An enum: the cases it has, with the fields of each.
    Cases {
        name: String,
        doc: String,
        cases: Vec<(String, Vec<Member>)>,
    },
}

/// One resource kind, as the SDK's class.
struct Class {
    /// Its type name (`PdfDocument`), and the kind it is (`pdf.document`).
    name: String,
    kind: String,
    label: String,
    fields: Vec<Member>,
    actions: Vec<Action>,
}

/// A field, or a field of a record: its name, type, and what the app calls it.
struct Member {
    /// Its name in the SDK's language, and the app's own when they differ.
    name: String,
    wire: String,
    ty: Ty,
    title: String,
    /// A read-only field of a resource: the SDK reads it and has no setter for it.
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
}

struct Param {
    name: String,
    wire: String,
    ty: Ty,
    required: bool,
}

/// A type as the SDK's languages write it; a named one is a record or enum of the app's.
enum Ty {
    Any,
    Text,
    Int,
    Number,
    Bool,
    List(Box<Self>),
    Map(Box<Self>),
    Option(Box<Self>),
    Union(Vec<Self>),
    Named(String),
}

/// How a surface names the types it declares: each name is written under a name of its own
/// when another surface of the app declares it differently.
type Names = BTreeMap<String, String>;

/// The types one app publishes.
fn types_of(app: &App) -> Result<Types> {
    let file = SurfaceFile::parse(app.surface.as_bytes())
        .map_err(|error| anyhow!("{}: {error}", app.id))?;
    let surfaces = &file.resources;
    let kinds = class_names(surfaces);
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
            .filter(|verb| !platform_verb(verb))
            .map(|verb| action_of(verb, &resolver))
            .collect();
        types.classes.push(Class {
            name: own,
            kind: surface.kind.clone(),
            label: surface.label.clone(),
            fields,
            actions,
        });
    }
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

/// The name `named` is already declared under, and whether it is declared the same.
fn names_of(declared: &BTreeMap<String, NamedType>, named: &NamedType) -> Option<(String, bool)> {
    declared
        .iter()
        .find(|(_, known)| known.name == named.name)
        .map(|(name, known)| (name.clone(), known == named))
}

/// The types of a surface, read as the SDK writes them.
struct Resolver<'a> {
    names: &'a Names,
    kinds: &'a BTreeMap<String, String>,
}

impl Resolver<'_> {
    fn ty(&self, ty: &FieldType) -> Ty {
        let inner = |ty: &FieldType| Box::new(self.ty(ty));
        match ty {
            FieldType::Text
            | FieldType::RichText
            | FieldType::Date
            | FieldType::Time
            | FieldType::Identifier
            | FieldType::Principal => Ty::Text,
            FieldType::Int => Ty::Int,
            FieldType::Float | FieldType::Duration => Ty::Number,
            FieldType::Bool => Ty::Bool,
            FieldType::List(item) | FieldType::Set(item) | FieldType::Collection(item) => {
                Ty::List(inner(item))
            }
            FieldType::Map(item) => Ty::Map(inner(item)),
            FieldType::Option(item) => Ty::Option(inner(item)),
            FieldType::Union(cases) => Ty::Union(cases.iter().map(|case| self.ty(case)).collect()),
            FieldType::Named(name) => self.names.get(name).cloned().map_or(Ty::Any, Ty::Named),
            FieldType::Resource(kind) => self.kinds.get(kind).cloned().map_or(Ty::Any, Ty::Named),
            FieldType::Any | FieldType::Reactions | FieldType::Outbox(_) => Ty::Any,
        }
    }
}

/// The type name of a resource kind: its last segment in upper camel case (`chat.workspace`
/// is `Workspace`), or its last two when a record or enum of the app already has that name
/// (`tilo.workspace.emoji` is `WorkspaceEmoji`).
fn class_names(surfaces: &[Surface]) -> BTreeMap<String, String> {
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

/// The verbs only the platform invokes: a host's creation, and a bound control's edits.
fn platform_verb(verb: &VerbSurface) -> bool {
    matches!(
        verb.id.as_str(),
        "$create" | "$edit-text" | "$edit-flag" | "$commit"
    )
}

fn action_of(verb: &VerbSurface, resolver: &Resolver<'_>) -> Action {
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
        id: verb.id.clone(),
        snake: snake(&verb.id),
        camel: camel_method(&verb.id),
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
    }
}

/// A field's, a parameter's or a record field's name in the SDK's language, and the app's own
/// name for it.
fn member(wire: &str) -> (String, String) {
    (wire.replace('-', "_"), wire.to_owned())
}

/// `read-form-fields` as Python names it: `read_form_fields`.
fn snake(id: &str) -> String {
    id.replace(['-', '.', ':'], "_")
}

/// `read-form-fields` as TypeScript names it: `readFormFields`.
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

// -- the two languages ------------------------------------------------------

/// One app's files: the module its types are written in (`pdf-viewer` is `pdf_viewer`).
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

/// The name an app goes by in a path: its id's last segment (`pdf-viewer`).
fn app_path(app: &App) -> &str {
    app.id.rsplit('.').next().unwrap_or(&app.id)
}

/// What people call the app, with its id.
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

/// `text` as one line a docstring or comment holds: its words, with the marks that would end
/// the comment left out.
fn one_line(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('"', "'")
        .replace("*/", "* /")
        .replace('\\', "/")
}

/// A field's own words, as a trailing comment.
fn comment(title: &str) -> String {
    let title = one_line(title);
    if title.is_empty() {
        String::new()
    } else {
        format!("  # {title}")
    }
}

/// A docstring, indented by `indent`, when there is something to say.
fn doc(out: &mut String, text: &str, indent: usize) {
    let text = one_line(text);
    if text.is_empty() {
        return;
    }
    let pad = "    ".repeat(indent);
    let _ = writeln!(out, "{pad}\"\"\"{text}\"\"\"");
}

/// A comment block: each line of `lines` after ` * `, an empty line as ` *`.
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

/// A Python name for `name`: the words a name may not be take a trailing `_`.
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

/// The fields of a class or record, as the SDK writes them: the first of a name, and a note
/// for each one after it that the app declares differently.
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

/// The Python types of `apps`: a module per app, and the package that holds them.
pub fn python_files(apps: &[App]) -> Result<Vec<(PathBuf, String)>> {
    let mut files = Vec::new();
    for app in apps {
        let types = types_of(app)?;
        let module = module_name(&app.id);
        files.push((
            PathBuf::from(format!("{module}.py")),
            python_module(app, &types),
        ));
    }
    files.push((PathBuf::from("__init__.py"), python_package()));
    Ok(files)
}

/// The package that holds the apps' modules: one module per app, each naming its own types
/// (`from tilo.types.pdf_viewer import Document`). Two apps may both call a type `Person`, so
/// none of them is re-exported here.
fn python_package() -> String {
    "\"\"\"Types for the apps this workspace has, written by `tilo sdk gen`.\n\
     \n\
     `tilo sdk gen --lang python -w <workspace>` writes them from the apps installed\n\
     there; without `-w`, from the apps of the checkout it runs in. Each app has a module\n\
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
        "and its methods are its actions. A method answers what the app declares; an action"
            .to_owned(),
        "that takes longer answers a `Run`. A record is the dict the wire carries, and an"
            .to_owned(),
        "enum whose cases carry fields is an object whose tag the app's schema names. An"
            .to_owned(),
        "action whose name is one the class already has is run with `action(\"…\")`, which"
            .to_owned(),
        "the file says where it is.".to_owned(),
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
    out.push_str("from typing import Any, Literal, TypedDict\n");
    let _ = writeln!(out, "from tilo import Resource, Run\n");
    let mut all: Vec<String> = types
        .declarations
        .iter()
        .map(|declaration| match declaration {
            Declaration::Record { name, .. } | Declaration::Cases { name, .. } => name.clone(),
        })
        .collect();
    all.extend(types.classes.iter().map(|class| class.name.clone()));
    out.push_str("__all__ = [\n");
    for name in &all {
        let _ = writeln!(out, "    \"{name}\",");
    }
    out.push_str("]\n\n");
    for declaration in &types.declarations {
        python_declaration(&mut out, declaration);
    }
    for class in &types.classes {
        python_class(&mut out, class);
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

fn python_class(out: &mut String, class: &Class) {
    let _ = writeln!(out, "class {}(Resource):", class.name);
    let mut text = one_line(&class.label);
    if !text.is_empty() {
        text.push_str(" — ");
    }
    let _ = write!(text, "`{}`.", class.kind);
    doc(out, &text, 1);
    if class.fields.is_empty() && class.actions.is_empty() {
        out.push_str("    pass\n\n");
        return;
    }
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
        if field.readonly {
            let _ = writeln!(out, "    @property");
            let _ = writeln!(out, "    def {name}(self) -> {}:", python_type(&field.ty));
            if field.title.is_empty() {
                doc(out, "Read only.", 2);
            } else {
                doc(out, &field.title, 2);
            }
            let _ = writeln!(out, "        return self.fields[\"{}\"]", field.wire);
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
        taken.push(name);
        python_action(out, action);
        out.push('\n');
    }
    out.push('\n');
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

/// What an action answers: what the app declares, and the `Run` that follows one still
/// running.
fn python_result(action: &Action) -> String {
    let declared = action.result.as_ref().map(python_type);
    match (action.kind, declared) {
        (ActionKind::Read, Some(ty)) => ty,
        (ActionKind::Read, None) => "None".to_owned(),
        (_, Some(ty)) => format!("{ty} | Run"),
        (_, None) => "Run | None".to_owned(),
    }
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
    for app in apps {
        let types = types_of(app)?;
        let module = module_name(&app.id);
        files.push((
            PathBuf::from(format!("{module}.ts")),
            typescript_module(app, &types),
        ));
        modules.push(module);
    }
    files.push((PathBuf::from("index.ts"), typescript_package(&modules)));
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
            "there; without `-w`, from the apps of the checkout it runs in. Each app has a",
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
        "case and taking the arguments the wire takes them under. An action that takes".to_owned(),
        "longer answers a `Run`. A record is the object the wire carries, and an enum whose"
            .to_owned(),
        "cases carry fields is an object whose tag the app's schema names. An action whose"
            .to_owned(),
        "name is one the interface already has is run with `action(\"…\")`, which the file"
            .to_owned(),
        "says where it is.".to_owned(),
    ];
    let head: Vec<&str> = head.iter().map(String::as_str).collect();
    comment_block(&mut out, &head);
    out.push('\n');
    out.push_str("import { resourceProxy } from \"tilo-sdk\";\n");
    out.push_str("import type * as tilo from \"tilo-sdk\";\n\n");
    for declaration in &types.declarations {
        typescript_declaration(&mut out, declaration);
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
                    field.name,
                    typescript_type(&field.ty),
                    comment(&field.title)
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
            field.name,
            typescript_type(&field.ty),
            comment(&field.title)
        );
    }
    for action in &class.actions {
        if OWN.contains(&action.id.as_str()) {
            let _ = writeln!(
                out,
                "  // the app's `{id}` action is the SDK's own: run it with `action(\"{id}\")`.",
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
        typescript_action(out, action);
    }
    out.push_str("}\n\n");
    let made = factory(class);
    let _ = write!(
        out,
        "/** The `{}` kind at `path`. */\n\
         export function {made}(ws: tilo.Workspace, path: string): {} {{\n\
         \x20 return resourceProxy(ws, path) as unknown as {};\n\
         }}\n\n",
        class.kind, class.name, class.name,
    );
}

/// The function that makes a `PdfDocument` at a path: `pdfDocument`, or `notesPage` where the
/// plain name is the page's own (`document`).
fn factory(class: &Class) -> String {
    let mut chars = class.name.chars();
    let plain: String = chars.next().map_or_else(String::new, |first| {
        first.to_lowercase().chain(chars).collect()
    });
    if !TS_GLOBALS.contains(&plain.as_str()) {
        return plain;
    }
    let app = camel(class.kind.split('.').next().unwrap_or(&class.kind));
    let named = format!("{app}{}", class.name);
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
    for param in required {
        let _ = writeln!(out, "    {}: {};", param.wire, typescript_type(&param.ty));
    }
    for param in optional {
        let inner = match &param.ty {
            Ty::Option(item) => typescript_type(item),
            other => typescript_type(other),
        };
        let _ = writeln!(out, "    {}?: {inner};", param.wire);
    }
    out.push_str("  }");
    out
}

/// What an action answers: what the app declares, and the `Run` that follows one still
/// running.
fn typescript_result(action: &Action) -> String {
    let declared = action.result.as_ref().map(typescript_type);
    match (action.kind, declared) {
        (ActionKind::Read, Some(ty)) => ty,
        (ActionKind::Read, None) => "void".to_owned(),
        (_, Some(ty)) => format!("{ty} | tilo.Run"),
        (_, None) => "tilo.Run | void".to_owned(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    /// The PDF Viewer, as this checkout commits it: the app whose surface reads a document's
    /// form fields.
    fn pdf_viewer() -> App {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/pdf-viewer");
        app_of(&dir).expect("the PDF Viewer's surface reads")
    }

    fn python_of(app: &App) -> String {
        let files = python_files(std::slice::from_ref(app)).expect("the types are written");
        let (_, text) = files
            .iter()
            .find(|(name, _)| name == Path::new(&format!("{}.py", module_name(&app.id))))
            .expect("the app's module is written");
        text.clone()
    }

    fn typescript_of(app: &App) -> String {
        let files = typescript_files(std::slice::from_ref(app)).expect("the types are written");
        let (_, text) = files
            .iter()
            .find(|(name, _)| name == Path::new(&format!("{}.ts", module_name(&app.id))))
            .expect("the app's module is written");
        text.clone()
    }

    /// The read action's value is a type of the surface's own: the class that reads the form
    /// answers a `FormFields`, not a bare dict.
    #[test]
    fn a_python_read_action_answers_the_type_its_surface_declares() {
        let python = python_of(&pdf_viewer());
        assert!(
            python.contains("    def read_form_fields(self) -> FormFields:\n"),
            "{python}"
        );
        assert!(
            python.contains("        return self.action(\"read-form-fields\")\n"),
            "{python}"
        );
        assert!(
            python.contains(
                "class FormFields(TypedDict):\n    file: str\n    fields: list[FormFieldRow]\n"
            ),
            "{python}"
        );
        // A read-only field is a property: read, and never assigned.
        assert!(
            python.contains(
                "    @property\n    def source(self) -> Any:\n        \"\"\"Read only.\"\"\"\n\
                 \x20       return self.fields[\"source\"]\n"
            ),
            "{python}"
        );
        // An enum of cases without fields is the literals the wire carries.
        assert!(
            python.contains(
                "MarkupStyle = Literal[\"highlight\", \"underline\", \"strikeout\", \"squiggly\"]"
            ),
            "{python}"
        );
    }

    /// The same in TypeScript: the action is camel case, and its arguments keep the names the
    /// wire takes them under.
    #[test]
    fn a_typescript_read_action_answers_the_type_its_surface_declares() {
        let ts = typescript_of(&pdf_viewer());
        assert!(
            ts.contains("  readFormFields(): Promise<FormFields>;\n"),
            "{ts}"
        );
        assert!(
            ts.contains("    from_page?: number;\n    to_page?: number;\n"),
            "{ts}"
        );
        assert!(
            ts.contains(
                "export interface FormFields {\n  file: string;\n  fields: FormFieldRow[];\n}\n"
            ),
            "{ts}"
        );
        assert!(ts.contains("  readonly source: unknown;\n"), "{ts}");
        // `document` is the page's own: the factory is named for the app.
        assert!(
            ts.contains(
                "export function pdfDocument(ws: tilo.Workspace, path: string): Document {"
            ),
            "{ts}"
        );
    }

    /// An action of the app whose name the SDK's own resource answers to is said so, rather
    /// than shadowing it.
    #[test]
    fn an_action_the_sdk_owns_is_named_rather_than_shadowed() {
        let ts = typescript_of(&pdf_viewer());
        assert!(!ts.contains("  refresh("), "{ts}");
    }
}
