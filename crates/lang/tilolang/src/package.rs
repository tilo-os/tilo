//! The pipeline: source to wasm and metadata, for packages and for agent blocks.

use crate::ast::{self, Item};
use crate::checker;
use crate::codegen;
use crate::diag::{Diagnostic, Placed, finish, finish_files};
use crate::effects::{self, Effects};
use crate::hash::sha256_hex;
use crate::ids::{Entropy, IdsLock, OsEntropy, SeededEntropy, declared_names};
use crate::ir;
use crate::meta::{
    ActionMeta, AnchorMeta, DefaultViewMeta, EnumMeta, FieldMeta, FnMeta, Line, PackageMeta,
    ParamMeta, ProposalMeta, ScenarioMeta, ShowMeta, StateMeta, TypeMeta, VariantMeta, ViewMeta,
};
use crate::parser::{ParsedFile, parse_expr_source, parse_file_at, parse_module, parse_ui_source};
use crate::prims;
use crate::resolve::{CKind, Env, ParamInfo, default_alias, resolve};
use crate::source::PackageSource;
use crate::types::Ty;

#[derive(Clone, Debug)]
pub struct Compiled {
    pub wasm: Vec<u8>,
    pub meta: PackageMeta,
    pub ids: IdsLock,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    View,
    Action,
    Data,
}

#[derive(Clone, Debug)]
pub struct CompiledBlock {
    pub wasm: Vec<u8>,
    pub value_type: String,
    pub kind: BlockKind,
    pub meta: PackageMeta,
    pub suggested: bool,
}

pub fn parse(source: &str) -> Result<ast::Module, Vec<Diagnostic>> {
    let (module, diags) = parse_module(source);
    match module {
        Some(m) if diags.is_empty() => Ok(m),
        _ => Err(finish(diags, source)),
    }
}

struct Analysis {
    module: ast::Module,
    env: Env,
    ir: ir::Module,
    ids: IdsLock,
    /// The package's templates as the source they were written in, by the module's own span space: what
    /// a package that imports this one expands where it calls them.
    templates: Vec<crate::meta::TemplateMeta>,
}

fn analyze(
    source: &str,
    ids: &IdsLock,
    imports: &[PackageMeta],
    rng: &mut dyn Entropy,
) -> Result<Analysis, Vec<Diagnostic>> {
    let module = parse(source)?;
    let files = [Placed {
        path: "app.tl",
        text: source,
        base: 0,
    }];
    analyze_module(module, ids, imports, rng, &files).map_err(|diags| finish(diags, source))
}

/// Resolves and checks a parsed module; the diagnostics are not yet located.
fn analyze_module(
    module: ast::Module,
    ids: &IdsLock,
    imports: &[PackageMeta],
    rng: &mut dyn Entropy,
    files: &[Placed<'_>],
) -> Result<Analysis, Vec<Diagnostic>> {
    let mut lock = ids.clone();
    lock.sync(&declared_names(&module), rng);
    let mut diags = Vec::new();
    let env = resolve(&module, &lock, imports, &mut diags);
    let ir = checker::check_module(&env, &module, &mut diags);
    if diags.is_empty() {
        Ok(Analysis {
            templates: template_meta(&module, files),
            module,
            env,
            ir,
            ids: lock,
        })
    } else {
        Err(diags)
    }
}

/// The files of a package in path order, each placed in the shared span space.
fn placed(source: &PackageSource) -> Vec<Placed<'_>> {
    let mut files: Vec<_> = source.files.iter().collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let mut base = 0;
    files
        .into_iter()
        .map(|f| {
            let p = Placed {
                path: &f.path,
                text: &f.text,
                base,
            };
            // One position past the end, so a file's end-of-input span stays inside it.
            base += f.text.len() + 1;
            p
        })
        .collect()
}

fn parse_placed(files: &[Placed<'_>]) -> (Vec<ParsedFile>, Vec<Diagnostic>) {
    let mut out = Vec::new();
    let mut diags = Vec::new();
    let mut node = 0;
    for f in files {
        let (parsed, mut d) = parse_file_at(f.text, f.base, node);
        node = parsed.next_node;
        diags.append(&mut d);
        out.push(parsed);
    }
    (out, diags)
}

/// Joins the files of a package into one module: exactly one of them names the app.
fn join_files(
    files: &[Placed<'_>],
    parsed: Vec<ParsedFile>,
    diags: &mut Vec<Diagnostic>,
) -> Option<ast::Module> {
    let mut header: Option<(ast::AppHeader, &str)> = None;
    let mut items = Vec::new();
    for (f, p) in files.iter().zip(parsed) {
        if let Some(h) = p.header {
            match &header {
                None => header = Some((h, f.path)),
                Some((_, first)) => diags.push(Diagnostic::new(
                    "E017",
                    h.span,
                    format!(
                        "the app is already named in `{first}`; a package names its app in exactly one file"
                    ),
                )),
            }
        }
        items.extend(p.items);
    }
    let Some((app, _)) = header else {
        let at = files.first().map_or(0, |f| f.base);
        diags.push(Diagnostic::new(
            "E010",
            crate::diag::Span::new(at, at),
            "one file of the package starts with `app <app id> \"<title>\"`, for example `app so.tilo.lang.counter \"Counter\"`",
        ));
        return None;
    };
    Some(ast::Module { app, items })
}

/// Parses a package of one or more files into one module. Diagnostics name their file.
pub fn parse_package(source: &PackageSource) -> Result<ast::Module, Vec<Diagnostic>> {
    let files = placed(source);
    let (parsed, mut diags) = parse_placed(&files);
    let module = join_files(&files, parsed, &mut diags);
    match module {
        Some(m) if diags.is_empty() => Ok(m),
        _ => Err(finish_files(diags, &files)),
    }
}

/// One file of a package, parsed on its own, for printing it back: `path`, its header, its items, and
/// the plain `//` comments printed among them.
#[derive(Clone, Debug)]
pub struct FileSyntax {
    pub path: String,
    pub header: Option<ast::AppHeader>,
    pub items: Vec<ast::Item>,
    pub comments: Vec<crate::lexer::Comment>,
}

/// Parses each file of a package for the canonical printer (`tilolang fmt`). A syntax error in any
/// file fails the whole package.
pub fn parse_package_files(source: &PackageSource) -> Result<Vec<FileSyntax>, Vec<Diagnostic>> {
    let files = placed(source);
    let (parsed, diags) = parse_placed(&files);
    if !diags.is_empty() {
        return Err(finish_files(diags, &files));
    }
    Ok(files
        .iter()
        .zip(parsed)
        .map(|(f, p)| FileSyntax {
            path: f.path.to_string(),
            header: p.header,
            items: p.items,
            comments: p.comments,
        })
        .collect())
}

fn analyze_package(
    source: &PackageSource,
    ids: &IdsLock,
    imports: &[PackageMeta],
    rng: &mut dyn Entropy,
) -> Result<Analysis, Vec<Diagnostic>> {
    let module = parse_package(source)?;
    let files = placed(source);
    analyze_module(module, ids, imports, rng, &files).map_err(|diags| finish_files(diags, &files))
}

/// Compiles a package of one or more files with ids drawn from the operating system's randomness.
pub fn compile_package_source(
    source: &PackageSource,
    ids: &IdsLock,
    imports: &[PackageMeta],
) -> Result<Compiled, Vec<Diagnostic>> {
    compile_package_source_with(source, ids, imports, &mut OsEntropy::default())
}

/// [`compile_package_source`]; new ids come from `rng`.
pub fn compile_package_source_with(
    source: &PackageSource,
    ids: &IdsLock,
    imports: &[PackageMeta],
    rng: &mut dyn Entropy,
) -> Result<Compiled, Vec<Diagnostic>> {
    let a = analyze_package(source, ids, imports, rng)?;
    let lines = lines_of(&a.ir, &placed(source));
    let mut compiled = emit_package(a);
    crate::meta::append_locales(&mut compiled.wasm, &locales_of(source, &compiled.meta));
    crate::meta::append_lines(&mut compiled.wasm, &lines);
    Ok(compiled)
}

/// Where each of `ir`'s functions is written, in the package source `files` placed in the shared span
/// space; a function no declaration is written for has no line.
fn lines_of(ir: &ir::Module, files: &[Placed<'_>]) -> Vec<Line> {
    ir.funcs
        .iter()
        .filter_map(|f| {
            let at = f.at?;
            let file = files.iter().rev().find(|file| {
                at >= file.base && at < file.base.saturating_add(file.text.len() + 1)
            })?;
            let before = file.text.get(..at.saturating_sub(file.base))?;
            Some(Line {
                export: f.export.clone(),
                file: file.path.to_owned(),
                line: line_number(before),
            })
        })
        .collect()
}

fn line_number(text: &str) -> u32 {
    u32::try_from(text.bytes().filter(|b| *b == b'\n').count() + 1).unwrap_or(u32::MAX)
}

/// The catalogues a module built from `source` carries: the source language's written from the compiled
/// sources, and every other language as its catalogue beside the sources says.
fn locales_of(source: &PackageSource, meta: &PackageMeta) -> Vec<(String, String)> {
    let mut locales = source.locales.clone();
    if let Some((_, text)) = locales
        .iter_mut()
        .find(|(locale, _)| locale == crate::messages::SOURCE_LOCALE)
    {
        *text = crate::messages::source_catalogue_keeping(meta, text);
    }
    locales
}

pub fn check_package(
    source: &PackageSource,
    ids: &IdsLock,
    imports: &[PackageMeta],
) -> Vec<Diagnostic> {
    analyze_package(source, ids, imports, &mut SeededEntropy(0))
        .err()
        .unwrap_or_default()
}

fn emit_package(a: Analysis) -> Compiled {
    let fx = effects::analyze(&a.ir, &a.env.app);
    let mut meta = build_meta(&a.module, &a.env, &fx, &a.ir.states, &a.ir.shows, &a.ir);
    meta.views
        .extend(row_views(&a.ir, &a.env.app, &fx, &a.ir.states));
    meta.words.clone_from(&a.ir.words);
    meta.at_event = at_event_fns(&a.ir, &a.env.app);
    // A template's body travels in the metadata JSON, which is how a `PackageMeta` handed to another
    // compiler reaches it; the module hash is taken over the code alone.
    meta.templates = a.templates;
    let bare = codegen::emit(&a.ir, None);
    meta.module_hash = sha256_hex(&bare);
    let wasm = codegen::emit(&a.ir, Some(&meta.to_json_bytes()));
    Compiled {
        wasm,
        meta,
        ids: a.ids,
    }
}

/// The package's `template view`s as the source they were written in: a caller elsewhere expands them
/// where it calls them, so their body is carried as text.
fn template_meta(module: &ast::Module, files: &[Placed<'_>]) -> Vec<crate::meta::TemplateMeta> {
    let mut out = Vec::new();
    for view in module.items.iter().filter_map(|item| match item {
        ast::Item::View(v) if crate::checker::is_template(v) => Some(v),
        _ => None,
    }) {
        let Some(file) = files.iter().rev().find(|file| {
            view.span.start >= file.base
                && view.span.start < file.base.saturating_add(file.text.len() + 1)
        }) else {
            continue;
        };
        let start = view.span.start.saturating_sub(file.base);
        let end = view.span.end.saturating_sub(file.base);
        let Some(source) = file.text.get(start..end) else {
            continue;
        };
        out.push(crate::meta::TemplateMeta {
            name: view.name.name.clone(),
            file: file.path.to_owned(),
            source: source.to_owned(),
        });
    }
    out
}

pub fn compile_package(
    source: &str,
    ids: &IdsLock,
    imports: &[PackageMeta],
) -> Result<Compiled, Vec<Diagnostic>> {
    compile_package_with(source, ids, imports, &mut OsEntropy::default())
}

pub fn compile_package_with(
    source: &str,
    ids: &IdsLock,
    imports: &[PackageMeta],
    rng: &mut dyn Entropy,
) -> Result<Compiled, Vec<Diagnostic>> {
    let a = analyze(source, ids, imports, rng)?;
    let lines: Vec<Line> =
        a.ir.funcs
            .iter()
            .filter_map(|f| {
                Some(Line {
                    export: f.export.clone(),
                    file: String::from("/"),
                    line: line_number(source.get(..f.at?)?),
                })
            })
            .collect();
    let mut compiled = emit_package(a);
    crate::meta::append_lines(&mut compiled.wasm, &lines);
    Ok(compiled)
}

pub fn check(source: &str, ids: &IdsLock, imports: &[PackageMeta]) -> Vec<Diagnostic> {
    analyze(source, ids, imports, &mut SeededEntropy(0))
        .err()
        .unwrap_or_default()
}

fn doc(lines: &[String]) -> String {
    lines.join("\n")
}

fn params_meta(params: &[ParamInfo], app: &str) -> Vec<ParamMeta> {
    params
        .iter()
        .map(|p| ParamMeta {
            name: p.name.clone(),
            ty: p.ty.meta_text(app),
            default: p.default.as_ref().map(|d| crate::meta::DefaultMeta {
                export: d.export.clone(),
                text: d.text.clone(),
            }),
            place: p.place,
            entry: p.entry,
            label: p.label.clone(),
        })
        .collect()
}

fn result_meta(ty: &Ty, app: &str) -> Option<String> {
    (!matches!(ty, Ty::Unit)).then(|| ty.meta_text(app))
}

fn uses_meta(uses: &std::collections::BTreeSet<crate::effects::Effect>) -> Vec<String> {
    uses.iter().map(|e| e.word().to_string()).collect()
}

fn field_meta(f: &crate::resolve::FieldInfo, app: &str) -> FieldMeta {
    FieldMeta {
        id: f.id.clone(),
        name: f.name.clone(),
        ty: f.ty.meta_text(app),
        merge: f.merge,
        doc: f.doc.clone(),
        default: f.default.clone(),
    }
}

/// The arguments the checker made functions of: the runtime runs them when an event fires.
fn at_event_fns(ir: &crate::ir::Module, app: &str) -> Vec<FnMeta> {
    ir.at_event
        .iter()
        .map(|a| FnMeta {
            id: a.id.clone(),
            name: a.name.clone(),
            doc: String::new(),
            params: params_meta(&a.params, app),
            result: a.result.meta_text(app),
        })
        .collect()
}

/// The `state` declarations a view holds, as the host reads them to answer a `bind`.
fn state_meta<'a>(
    states: impl Iterator<Item = &'a crate::resolve::StateInfo>,
    app: &str,
) -> Vec<StateMeta> {
    states
        .map(|s| StateMeta {
            id: s.id.clone(),
            name: s.name.clone(),
            ty: s.ty.meta_text(app),
        })
        .collect()
}

/// The views the checker made of UI loop bodies: the host calls them like declared views.
fn row_views(
    ir: &crate::ir::Module,
    app: &str,
    fx: &Effects,
    template_states: &[(String, crate::resolve::StateInfo)],
) -> Vec<ViewMeta> {
    ir.views
        .iter()
        .map(|v| ViewMeta {
            id: v.id.clone(),
            name: v.name.clone(),
            doc: String::new(),
            params: params_meta(&v.params, app),
            anchors: Vec::new(),
            reaches: fx.reaches.get(v.func).cloned().unwrap_or_default(),
            state: state_meta(
                template_states
                    .iter()
                    .filter(|(view, _)| *view == v.id)
                    .map(|(_, s)| s),
                app,
            ),
            presents: None,
            row_of: Some(v.owner.clone()),
            show: None,
        })
        .collect()
}

/// What the view `name` answers a request to show an item with, when it declares one.
fn show_of(shows: &[crate::ir::ViewShow], id: &str, name: &str, app: &str) -> Option<ShowMeta> {
    let show = shows.iter().find(|s| s.view == id)?;
    Some(ShowMeta {
        ty: show.ty.clone(),
        func: FnMeta {
            id: show.export.clone(),
            name: format!("{name} shown an item"),
            doc: String::new(),
            params: params_meta(std::slice::from_ref(&show.param), app),
            result: Ty::Action.meta_text(app),
        },
    })
}

fn build_meta(
    module: &ast::Module,
    env: &Env,
    fx: &Effects,
    template_states: &[(String, crate::resolve::StateInfo)],
    shows: &[crate::ir::ViewShow],
    ir: &crate::ir::Module,
) -> PackageMeta {
    let app = env.app.as_str();
    let mut meta = PackageMeta {
        app: app.to_string(),
        title: module.app.title.clone(),
        icon: module.app.icon.clone(),
        category: module.app.category.clone(),
        previously: module.app.previously.clone(),
        own_records: module.app.own_records,
        web: module.app.web.clone(),
        ..PackageMeta::default()
    };
    let type_id =
        |key: &crate::types::TypeKey| env.type_info(key).map(|t| t.id.clone()).unwrap_or_default();
    meta.roots = env.roots.iter().map(type_id).collect();
    // The apps it imports by their ids now: a `use` of an id a package had before names it.
    meta.uses = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Use(u) => Some(
                env.imports
                    .iter()
                    .find(|i| i.app == u.app || i.previously.contains(&u.app))
                    .map_or_else(|| u.app.clone(), |i| i.app.clone()),
            ),
            _ => None,
        })
        .collect();
    // Every module is compiled with the standard library, so its code may call into it: a bundle is
    // handed the library as the package it uses.
    if env.app != crate::stdlib::APP {
        meta.uses.push(crate::stdlib::APP.to_string());
    }
    meta.named_roots = env
        .named_roots
        .iter()
        .map(|(name, key)| crate::meta::NamedRootMeta {
            name: name.clone(),
            ty: type_id(key),
            published: None,
        })
        .collect();
    let views_meta = |views: &[(crate::types::TypeKey, String)]| -> Vec<DefaultViewMeta> {
        views
            .iter()
            .filter(|(key, _)| key.app == app)
            .filter_map(|(key, view)| {
                Some(DefaultViewMeta {
                    ty: type_id(key),
                    view: env.own_callable(view)?.id.clone(),
                })
            })
            .collect()
    };
    meta.default_views = views_meta(&env.default_views);
    meta.row_views = views_meta(&env.row_views);
    meta.triggers = env
        .triggers
        .iter()
        .map(|t| crate::meta::TriggerMeta {
            id: t.id.clone(),
            name: t.name.clone(),
            doc: t.doc.clone(),
            action: t.action.clone(),
            ty: t.ty.clone(),
            args: t
                .args
                .iter()
                .map(|arg| match arg {
                    crate::resolve::TriggerArg::Subject => crate::meta::TriggerArgMeta::Subject,
                    crate::resolve::TriggerArg::Root(ty) => {
                        crate::meta::TriggerArgMeta::Root(ty.clone())
                    }
                })
                .collect(),
            when: match &t.when {
                crate::resolve::TriggerWhen::Every(secs) => {
                    crate::meta::TriggerWhenMeta::Every(*secs)
                }
                crate::resolve::TriggerWhen::At(field) => {
                    crate::meta::TriggerWhenMeta::At(field.clone())
                }
            },
        })
        .collect();
    meta.views_of = env
        .views_of
        .iter()
        .filter(|(key, _, _)| key.app == app)
        .map(|(key, views, _)| crate::meta::ViewsMeta {
            ty: type_id(key),
            views: views
                .iter()
                .filter_map(|view| Some(env.own_callable(&view.name)?.id.clone()))
                .collect(),
            shown: views
                .iter()
                .filter_map(|view| {
                    let id = env.own_callable(&view.name)?.id.clone();
                    (view.label.is_some() || view.icon.is_some()).then(|| {
                        crate::meta::ViewShownMeta {
                            view: id,
                            label: view.label.clone(),
                            icon: view.icon.clone(),
                        }
                    })
                })
                .collect(),
        })
        .collect();
    let class_of = |idx: usize| {
        fx.class
            .get(idx)
            .copied()
            .unwrap_or(crate::types::EffectClass::Read)
    };
    for item in &module.items {
        match item {
            Item::Use(_)
            | Item::Root(_)
            | Item::DefaultView(_)
            | Item::RowView(_)
            | Item::Views(_)
            | Item::Trigger(_) => {}
            Item::Alias(a) => {
                let key = crate::types::TypeKey {
                    app: app.to_string(),
                    name: a.name.name.clone(),
                };
                if let Some((_, ty)) = env.aliases.iter().find(|(k, _)| *k == key) {
                    meta.aliases.push(crate::meta::AliasMeta {
                        name: key.name.clone(),
                        ty: ty.meta_text(app),
                    });
                }
            }
            Item::Scenario(sc) => {
                let Some(c) = env.scenario(&sc.name) else {
                    continue;
                };
                let Some(idx) = c.func else { continue };
                meta.actions.push(ActionMeta {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    doc: doc(&sc.doc),
                    params: Vec::new(),
                    result: result_meta(&c.result, app),
                    class: class_of(idx),
                    proposal: None,
                    reaches: fx.reaches.get(idx).cloned().unwrap_or_default(),
                    invokes: fx.invokes.get(idx).copied().unwrap_or(false),
                    label: c.present.label.clone(),
                    icon: None,
                    role: None,
                    scenario: true,
                    did: None,
                    uses: uses_meta(&c.uses),
                    present: None,
                    toggles: None,
                    key: None,
                    group: None,
                    guard: false,
                    applies: false,
                    outside: Vec::new(),
                });
                meta.scenarios.push(ScenarioMeta {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    action: c.id.clone(),
                });
            }
            Item::Enum(e) => {
                let Some(info) = env.own_enum(&e.name.name) else {
                    continue;
                };
                meta.enums.push(EnumMeta {
                    id: info.id.clone(),
                    name: info.key.name.clone(),
                    doc: info.doc.clone(),
                    variants: info
                        .variants
                        .iter()
                        .map(|v| VariantMeta {
                            id: v.id.clone(),
                            name: v.name.clone(),
                            doc: v.doc.clone(),
                            label: v.label.clone(),
                            icon: v.icon.clone(),
                            tone: v.tone.clone(),
                            fields: v.fields.iter().map(|f| field_meta(f, app)).collect(),
                        })
                        .collect(),
                });
            }
            Item::Type(t) => {
                let Some(info) = env.own_type(&t.name.name) else {
                    continue;
                };
                let fields = info.fields.iter().map(|f| field_meta(f, app)).collect();
                meta.types.push(TypeMeta {
                    id: info.id.clone(),
                    name: info.key.name.clone(),
                    doc: doc(&t.doc),
                    fields,
                    search: info.search.clone(),
                    origin: info.origin.clone(),
                    title: info.title.clone(),
                    untitled: info.untitled.clone(),
                });
            }
            Item::Action(a) => {
                let Some(c) = env.own_callable(&a.name.name) else {
                    continue;
                };
                let Some(idx) = c.func else { continue };
                let proposal = a.proposal.as_ref().and_then(|p| {
                    let v = env.own_callable(&p.view.name)?;
                    let args = match &p.args {
                        Some(names) => names.iter().map(|n| n.name.clone()).collect(),
                        None => c.params.iter().map(|x| x.name.clone()).collect(),
                    };
                    Some(ProposalMeta {
                        view: v.id.clone(),
                        name: v.name.clone(),
                        args,
                    })
                });
                meta.actions.push(ActionMeta {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    doc: doc(&a.doc),
                    params: params_meta(&c.params, app),
                    result: result_meta(&c.result, app),
                    class: class_of(idx),
                    proposal,
                    reaches: fx.reaches.get(idx).cloned().unwrap_or_default(),
                    invokes: fx.invokes.get(idx).copied().unwrap_or(false),
                    label: c.present.label.clone(),
                    icon: c.present.icon.clone(),
                    role: c.present.role.clone(),
                    scenario: false,
                    did: c.present.did.clone(),
                    uses: uses_meta(&c.uses),
                    present: c.present.view.clone(),
                    toggles: toggled_field(env, a, &c.params),
                    key: c.present.key.clone(),
                    group: c.present.group.clone(),
                    guard: a
                        .requires
                        .iter()
                        .any(|r| crate::checker::offer_decided(r, &c.params)),
                    applies: a
                        .applies
                        .iter()
                        .any(|r| crate::checker::offer_decided(r, &c.params)),
                    outside: ir
                        .funcs
                        .get(idx)
                        .map(|f| f.facts.outside_caps.iter().cloned().collect())
                        .unwrap_or_default(),
                });
                if let Some(v) = env.callables.iter().find(|v| {
                    v.own && v.kind == CKind::View && Some(&v.id) == c.present.view.as_ref()
                }) {
                    meta.views.push(ViewMeta {
                        id: v.id.clone(),
                        name: v.name.clone(),
                        doc: String::new(),
                        params: params_meta(&v.params, app),
                        anchors: Vec::new(),
                        reaches: v
                            .func
                            .and_then(|f| fx.reaches.get(f).cloned())
                            .unwrap_or_default(),
                        state: Vec::new(),
                        presents: Some(c.id.clone()),
                        row_of: None,
                        show: None,
                    });
                }
            }
            Item::View(v) => {
                let Some(c) = env.own_callable(&v.name.name) else {
                    continue;
                };
                let Some(idx) = c.func else { continue };
                let mut anchors: Vec<AnchorMeta> = env
                    .anchors
                    .iter()
                    .filter_map(|(k, id)| {
                        let (view, name) = k.split_once('#')?;
                        (view == v.name.name).then(|| AnchorMeta {
                            id: id.clone(),
                            name: name.to_string(),
                        })
                    })
                    .collect();
                anchors.sort_by(|a, b| a.name.cmp(&b.name));
                meta.views.push(ViewMeta {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    doc: doc(&v.doc),
                    params: params_meta(&c.params, app),
                    anchors,
                    reaches: fx.reaches.get(idx).cloned().unwrap_or_default(),
                    state: state_meta(
                        env.states.iter().filter(|s| s.view == v.name.name).chain(
                            template_states
                                .iter()
                                .filter(|(view, _)| *view == c.id)
                                .map(|(_, s)| s),
                        ),
                        app,
                    ),
                    presents: None,
                    row_of: None,
                    show: show_of(shows, &c.id, &c.name, app),
                });
            }
            Item::Fn(f) => {
                let Some(c) = env.own_callable(&f.name.name) else {
                    continue;
                };
                meta.fns.push(FnMeta {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    doc: doc(&f.doc),
                    params: params_meta(&c.params, app),
                    result: c.result.meta_text(app),
                });
            }
        }
    }
    meta
}

pub fn compile_block(
    source: &str,
    visible: &[PackageMeta],
) -> Result<CompiledBlock, Vec<Diagnostic>> {
    compile_block_for("main", source, visible)
}

/// Compiles a block: an expression, or a piece of view body, in the ad-hoc module `app agent.block.<run
/// id>` that implicitly imports every visible app.
pub fn compile_block_for(
    run_id: &str,
    source: &str,
    visible: &[PackageMeta],
) -> Result<CompiledBlock, Vec<Diagnostic>> {
    let app = format!("agent.block.{run_id}");
    let env = agent_env(&app, &crate::host::host_meta(), visible);
    let mut diags = Vec::new();
    let (expr, expr_diags) = parse_expr_source(source);
    let as_expr = expr.filter(|e| !is_ui_head(&env, e));
    let result = match as_expr {
        Some(e) => {
            diags.extend(expr_diags);
            checker::check_block_expr(&env, &e, &mut diags)
        }
        None => {
            let (stmts, ui_diags) = parse_ui_source(source);
            diags.extend(ui_diags);
            if !diags.is_empty() {
                return Err(finish(diags, source));
            }
            checker::check_block_ui(&env, &stmts, &mut diags)
        }
    };
    if !diags.is_empty() {
        return Err(finish(diags, source));
    }
    let mut meta = PackageMeta {
        app: app.clone(),
        title: format!("Block {run_id}"),
        ..PackageMeta::default()
    };
    let bare = codegen::emit(&result.module, None);
    meta.module_hash = sha256_hex(&bare);
    let wasm = codegen::emit(&result.module, Some(&meta.to_json_bytes()));
    let kind = match &result.ty {
        Ty::View => BlockKind::View,
        Ty::Action => BlockKind::Action,
        _ => BlockKind::Data,
    };
    Ok(CompiledBlock {
        wasm,
        value_type: result.ty.meta_text(&app),
        kind,
        suggested: result.suggested,
        meta,
    })
}

/// The names an agent's code sees: the host as `host`, the toolkit, and every visible app by the last
/// part of its id (by its whole id when two share that part).
fn agent_env(app: &str, host: &PackageMeta, visible: &[PackageMeta]) -> Env {
    let mut env = Env {
        app: app.to_owned(),
        ..Env::default()
    };
    env.add_import("host", host);
    env.add_toolkit();
    for m in visible.iter().filter(|m| m.app != crate::host::HOST_APP) {
        let alias = default_alias(&m.app);
        let alias = if env.imports.iter().any(|i| i.alias == alias) {
            m.app.clone()
        } else {
            alias
        };
        env.add_import(&alias, m);
    }
    env
}

/// What an agent's program is compiled into: `app agent.program.<hash>`.
pub const PROGRAM_APP_PREFIX: &str = "agent.program.";

/// A name a session bound, and its type as metadata writes it (`List<so.tilo.lang.notes.Note>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub name: String,
    pub ty: String,
}

/// A compiled program: a module whose one action takes the session's bindings, in order.
#[derive(Clone, Debug)]
pub struct CompiledProgram {
    pub wasm: Vec<u8>,
    pub meta: PackageMeta,
    pub action: String,
    pub lets: Vec<Binding>,
}

/// Compiles an agent's program (agents.md, "One language, one tool"): statements over every visible
/// app, run as an action whose parameters are the names a session bound before (`bound`).
pub fn compile_program(
    source: &str,
    visible: &[PackageMeta],
    bound: &[Binding],
) -> Result<CompiledProgram, Vec<Diagnostic>> {
    compile_program_reading(source, visible, bound, &[])
}

/// [`compile_program`] for a program that may also read the apps `sources` (each by the name programs
/// call it and its id): `grep(re, in: chat)`, `describe(chat)` and a reference to one of their
/// resources are read through the platform (agents.md, "Querying").
pub fn compile_program_reading(
    source: &str,
    visible: &[PackageMeta],
    bound: &[Binding],
    sources: &[(String, String)],
) -> Result<CompiledProgram, Vec<Diagnostic>> {
    let mut fingerprint = String::from(source);
    for (alias, app) in sources {
        fingerprint.push_str(&format!("\nsource {alias}:{app}"));
    }
    for b in bound {
        fingerprint.push_str(&format!("\n{}:{}", b.name, b.ty));
    }
    for m in visible {
        fingerprint.push_str(&format!("\n{}:{}", m.app, m.module_hash));
    }
    let key = sha256_hex(fingerprint.as_bytes());
    let short = key.get(..16).unwrap_or(&key);
    let app = format!("{PROGRAM_APP_PREFIX}{short}");
    let action = format!("program-{short}");
    let host = crate::host::host_meta();
    let mut env = agent_env(&app, &host, visible);
    env.add_agent();
    env.session = true;
    for (alias, source) in sources {
        if !env.sources.iter().any(|(_, known)| known == source) {
            env.sources.push((alias.clone(), source.clone()));
        }
    }
    let mut diags = Vec::new();
    let mut params = Vec::new();
    for b in bound {
        // A binding whose type no visible app declares any more (an app went away) cannot be seen:
        // the program reads as if it was never bound.
        if let Some(ty) = Ty::parse_meta(&b.ty, &app).filter(|ty| declared(&env, ty)) {
            params.push(ParamInfo {
                name: b.name.clone(),
                ty,
                default: None,
                place: false,
                label: None,
                entry: false,
            });
        }
    }
    let (stmts, parse_diags) = crate::parser::parse_program_source(source);
    if !parse_diags.is_empty() {
        return Err(finish(parse_diags, source));
    }
    let result = checker::check_program(&env, &stmts, &params, &format!("a_{action}"), &mut diags);
    if !diags.is_empty() {
        return Err(finish(diags, source));
    }
    let lets = result
        .lets
        .iter()
        .map(|(name, ty)| Binding {
            name: name.clone(),
            ty: ty.meta_text(&app),
        })
        .collect();
    let mut meta = PackageMeta {
        app: app.clone(),
        title: "Program".to_owned(),
        actions: vec![ActionMeta {
            id: action.clone(),
            name: "main".to_owned(),
            doc: String::new(),
            params: params_meta(&params, &app),
            result: None,
            class: crate::types::EffectClass::Undoable,
            proposal: None,
            reaches: Vec::new(),
            label: String::new(),
            icon: None,
            role: None,
            scenario: false,
            did: None,
            present: None,
            toggles: None,
            key: None,
            group: None,
            uses: Vec::new(),
            guard: false,
            applies: false,
            invokes: false,
            outside: Vec::new(),
        }],
        ..PackageMeta::default()
    };
    let bare = codegen::emit(&result.module, None);
    meta.module_hash = sha256_hex(&bare);
    let wasm = codegen::emit(&result.module, Some(&meta.to_json_bytes()));
    Ok(CompiledProgram {
        wasm,
        meta,
        action,
        lets,
    })
}

fn declared(env: &Env, ty: &Ty) -> bool {
    match ty {
        Ty::Record(key) => env.type_info(key).is_some(),
        Ty::Enum(key) => env.enum_info(key).is_some(),
        Ty::List(t) | Ty::Set(t) | Ty::Map(t) | Ty::Collection(t) | Ty::Option(t) => {
            declared(env, t)
        }
        Ty::Result(t, e) => declared(env, t) && declared(env, e),
        Ty::Tuple(ts) => ts.iter().all(|t| declared(env, t)),
        _ => true,
    }
}

/// Whether an expression is a UI primitive or a view applied, which the block treats as a piece of view
/// body.
fn is_ui_head(env: &Env, e: &ast::Expr) -> bool {
    let ast::ExprKind::Call { callee, .. } = &e.kind else {
        return matches!(&e.kind, ast::ExprKind::Ident(n) if prims::prim(n).is_some());
    };
    match &callee.kind {
        ast::ExprKind::Ident(n) => prims::prim(n).is_some(),
        ast::ExprKind::Field { base, name } => {
            let Some(path) = crate::check_path(base) else {
                return false;
            };
            let Some(import) = env.find_import(&path) else {
                return false;
            };
            env.foreign_callable(&import.app, &name.name)
                .is_some_and(|c| c.kind == CKind::View)
        }
        _ => false,
    }
}

/// The id of the subject's `Bool` field an action's `on:` names (`on: n.pinned`, `n` its first
/// parameter): the state it toggles.
fn toggled_field(env: &Env, action: &ast::ActionDecl, params: &[ParamInfo]) -> Option<String> {
    let on = action.opts.iter().find(|opt| opt.name.name == "on")?;
    let ast::ExprKind::Field { base, name } = &on.value.kind else {
        return None;
    };
    let ast::ExprKind::Ident(subject) = &base.kind else {
        return None;
    };
    let first = params.first().filter(|first| first.name == *subject)?;
    let Ty::Record(key) = &first.ty else {
        return None;
    };
    let field = env.type_info(key)?.field(&name.name)?;
    (field.ty == Ty::Bool).then(|| field.id.clone())
}

#[cfg(test)]
mod template_export_tests {
    use super::*;

    #[test]
    fn a_template_travels_in_the_packages_metadata() {
        let src = "app probe.lib \"Lib\"\n\n/// Chrome\ntemplate view wrap(body: View) {\n    stack {\n        body\n    }\n}\n";
        let compiled = compile_package_source(
            &PackageSource::single(src),
            &crate::ids::IdsLock::new(),
            &[],
        )
        .expect("lib compiles");
        assert!(
            compiled.meta.templates.iter().any(|t| t.name == "wrap"),
            "the metadata carries the template: {:?}",
            compiled.meta.templates
        );
        let back = PackageMeta::from_wasm(&compiled.wasm).expect("meta");
        assert!(
            back.templates.iter().any(|t| t.name == "wrap"),
            "the wasm's metadata carries it: {:?}",
            back.templates
        );
        let consumer = compile_package_source(
            &PackageSource::single(
                "app probe.c \"C\"\n\nuse probe.lib as lib\n\nview mine() {\n    text(\"x\")\n}\n\nview sheet() {\n    lib.wrap {\n        mine()\n    }\n}\n",
            ),
            &crate::ids::IdsLock::new(),
            &[back],
        );
        assert!(consumer.is_ok(), "{consumer:#?}");
    }
}
