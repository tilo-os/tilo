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
    ActionMeta, AnchorMeta, DefaultViewMeta, EnumMeta, FieldMeta, FlowMeta, FnMeta, PackageMeta,
    ParamMeta, ProposalMeta, ScenarioMeta, StateMeta, TypeMeta, VariantMeta, ViewMeta,
};
use crate::parser::{ParsedFile, parse_expr_source, parse_file_at, parse_module, parse_ui_source};
use crate::prims;
use crate::resolve::{CKind, Env, ParamInfo, default_alias, resolve};
use crate::source::PackageSource;
use crate::types::Ty;

/// A compiled app package.
#[derive(Clone, Debug)]
pub struct Compiled {
    pub wasm: Vec<u8>,
    pub meta: PackageMeta,
    /// The ids lock with every declaration assigned and gone ones retired; write it back.
    pub ids: IdsLock,
}

/// What an agent block evaluates to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    View,
    Action,
    Data,
}

/// A compiled agent block: a module with the export `b_main`.
#[derive(Clone, Debug)]
pub struct CompiledBlock {
    pub wasm: Vec<u8>,
    /// The type of the block's value, as written in source.
    pub value_type: String,
    pub kind: BlockKind,
    pub meta: PackageMeta,
}

/// Parses a module.
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
}

fn analyze(
    source: &str,
    ids: &IdsLock,
    imports: &[PackageMeta],
    rng: &mut dyn Entropy,
) -> Result<Analysis, Vec<Diagnostic>> {
    let module = parse(source)?;
    analyze_module(module, ids, imports, rng).map_err(|diags| finish(diags, source))
}

/// Resolves and checks a parsed module; the diagnostics are not yet located.
fn analyze_module(
    module: ast::Module,
    ids: &IdsLock,
    imports: &[PackageMeta],
    rng: &mut dyn Entropy,
) -> Result<Analysis, Vec<Diagnostic>> {
    let mut lock = ids.clone();
    lock.sync(&declared_names(&module), rng);
    let mut diags = Vec::new();
    let env = resolve(&module, &lock, imports, &mut diags);
    let ir = checker::check_module(&env, &module, &mut diags);
    if diags.is_empty() {
        Ok(Analysis {
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

/// Parses every file of a package, in path order, with spans in the shared span space.
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

/// One file of a package, parsed on its own, for printing it back: `path`, its header when it
/// has one, and its items.
#[derive(Clone, Debug)]
pub struct FileSyntax {
    pub path: String,
    pub header: Option<ast::AppHeader>,
    pub items: Vec<ast::Item>,
}

/// Parses each file of a package for the canonical printer (`tilolang fmt`). Syntax errors in
/// any file fail the whole package, as the printer would otherwise drop what it could not read.
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
    analyze_module(module, ids, imports, rng).map_err(|diags| finish_files(diags, &files))
}

/// Compiles a package of one or more files with ids drawn from the operating system's
/// randomness. Every `.tl` file is part of the one module; diagnostics name their file.
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
    Ok(emit_package(a))
}

/// Reports every error of a package of one or more files, or an empty list when it is fine.
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
    let mut meta = build_meta(&a.module, &a.env, &fx);
    meta.views.extend(row_views(&a.ir, &a.env.app, &fx));
    let bare = codegen::emit(&a.ir, None);
    meta.module_hash = sha256_hex(&bare);
    let wasm = codegen::emit(&a.ir, Some(&meta.to_json_bytes()));
    Compiled {
        wasm,
        meta,
        ids: a.ids,
    }
}

/// Compiles a package with ids drawn from the operating system's randomness.
pub fn compile_package(
    source: &str,
    ids: &IdsLock,
    imports: &[PackageMeta],
) -> Result<Compiled, Vec<Diagnostic>> {
    compile_package_with(source, ids, imports, &mut OsEntropy::default())
}

/// Compiles a package; new ids come from `rng`.
pub fn compile_package_with(
    source: &str,
    ids: &IdsLock,
    imports: &[PackageMeta],
    rng: &mut dyn Entropy,
) -> Result<Compiled, Vec<Diagnostic>> {
    let a = analyze(source, ids, imports, rng)?;
    Ok(emit_package(a))
}

/// Reports every error of a package, or an empty list when it is fine.
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
        })
        .collect()
}

fn result_meta(ty: &Ty, app: &str) -> Option<String> {
    (!matches!(ty, Ty::Unit)).then(|| ty.meta_text(app))
}

fn field_meta(f: &crate::resolve::FieldInfo, app: &str) -> FieldMeta {
    FieldMeta {
        id: f.id.clone(),
        name: f.name.clone(),
        ty: f.ty.meta_text(app),
        merge: f.merge,
        doc: f.doc.clone(),
        default: f.default.clone(),
        role: f.role.clone(),
    }
}

/// The views the checker made of UI loop bodies: the host calls them like declared views.
fn row_views(ir: &crate::ir::Module, app: &str, fx: &Effects) -> Vec<ViewMeta> {
    ir.views
        .iter()
        .map(|v| ViewMeta {
            id: v.id.clone(),
            name: v.name.clone(),
            doc: String::new(),
            params: params_meta(&v.params, app),
            anchors: Vec::new(),
            reaches: fx.reaches.get(v.func).cloned().unwrap_or_default(),
            state: Vec::new(),
            presents: None,
            row_of: Some(v.owner.clone()),
        })
        .collect()
}

fn build_meta(module: &ast::Module, env: &Env, fx: &Effects) -> PackageMeta {
    let app = env.app.as_str();
    let mut meta = PackageMeta {
        app: app.to_string(),
        title: module.app.title.clone(),
        icon: module.app.icon.clone(),
        category: module.app.category.clone(),
        ..PackageMeta::default()
    };
    let type_id =
        |key: &crate::types::TypeKey| env.type_info(key).map(|t| t.id.clone()).unwrap_or_default();
    meta.roots = env.roots.iter().map(type_id).collect();
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
    let class_of = |idx: usize| {
        fx.class
            .get(idx)
            .copied()
            .unwrap_or(crate::types::EffectClass::Read)
    };
    for item in &module.items {
        match item {
            Item::Use(_) | Item::Root(_) | Item::DefaultView(_) | Item::RowView(_) => {}
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
                    label: c.present.label.clone(),
                    icon: None,
                    role: None,
                    scenario: true,
                    present: None,
                    toggles: None,
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
                    label: c.present.label.clone(),
                    icon: c.present.icon.clone(),
                    role: c.present.role.clone(),
                    scenario: false,
                    present: c.present.view.clone(),
                    toggles: toggled_field(env, a, &c.params),
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
                    });
                }
            }
            Item::Flow(f) => {
                let Some(c) = env.own_callable(&f.name.name) else {
                    continue;
                };
                let Some(idx) = c.func else { continue };
                meta.flows.push(FlowMeta {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    doc: doc(&f.doc),
                    params: params_meta(&c.params, app),
                    result: result_meta(&c.result, app),
                    class: class_of(idx),
                    reaches: fx.reaches.get(idx).cloned().unwrap_or_default(),
                });
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
                    state: env
                        .states
                        .iter()
                        .filter(|s| s.view == v.name.name)
                        .map(|s| StateMeta {
                            id: s.id.clone(),
                            name: s.name.clone(),
                            layer: s.layer.word().to_string(),
                            ty: s.ty.meta_text(app),
                        })
                        .collect(),
                    presents: None,
                    row_of: None,
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

// ---- blocks -------------------------------------------------------------------------------

/// Compiles an agent block with the run id `main`. See [`compile_block_for`].
pub fn compile_block(
    source: &str,
    visible: &[PackageMeta],
) -> Result<CompiledBlock, Vec<Diagnostic>> {
    compile_block_for("main", source, visible)
}

/// Compiles a block: an expression, or a piece of view body, evaluated in the ad-hoc module
/// `app agent.block.<run id>` that implicitly imports every visible app.
pub fn compile_block_for(
    run_id: &str,
    source: &str,
    visible: &[PackageMeta],
) -> Result<CompiledBlock, Vec<Diagnostic>> {
    let app = format!("agent.block.{run_id}");
    let mut env = Env {
        app: app.clone(),
        ..Env::default()
    };
    env.add_import("host", &crate::host::host_meta());
    for m in visible.iter().filter(|m| m.app != crate::host::HOST_APP) {
        let alias = default_alias(&m.app);
        let alias = if env.imports.iter().any(|i| i.alias == alias) {
            m.app.clone()
        } else {
            alias
        };
        env.add_import(&alias, m);
    }
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
        meta,
    })
}

/// Whether an expression is a UI primitive or a view applied, which the block treats as a
/// piece of view body.
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
/// parameter): the state it toggles. `None` for any other `on:`, or none.
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
