//! The pipeline: source to wasm and metadata, for packages and for agent blocks.

use crate::ast::{self, Item};
use crate::checker;
use crate::codegen;
use crate::diag::{Diagnostic, finish};
use crate::effects::{self, Effects};
use crate::hash::sha256_hex;
use crate::ids::{Entropy, IdsLock, OsEntropy, SeededEntropy, declared_names};
use crate::ir;
use crate::meta::{
    ActionMeta, AnchorMeta, FieldMeta, FlowMeta, FnMeta, PackageMeta, ParamMeta, ProposalMeta,
    TypeMeta, ViewMeta,
};
use crate::parser::{parse_expr_source, parse_module, parse_ui_source};
use crate::prims;
use crate::resolve::{CKind, Env, ParamInfo, default_alias, resolve};
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
        Err(finish(diags, source))
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
    let fx = effects::analyze(&a.ir, &a.env.app);
    let mut meta = build_meta(&a.module, &a.env, &fx);
    let bare = codegen::emit(&a.ir, None);
    meta.module_hash = sha256_hex(&bare);
    let wasm = codegen::emit(&a.ir, Some(&meta.to_json_bytes()));
    Ok(Compiled {
        wasm,
        meta,
        ids: a.ids,
    })
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
            ty: p.ty.display(app),
        })
        .collect()
}

fn result_meta(ty: &Ty, app: &str) -> Option<String> {
    (!matches!(ty, Ty::Unit)).then(|| ty.display(app))
}

fn build_meta(module: &ast::Module, env: &Env, fx: &Effects) -> PackageMeta {
    let app = env.app.as_str();
    let mut meta = PackageMeta {
        app: app.to_string(),
        title: module.app.title.clone(),
        ..PackageMeta::default()
    };
    for item in &module.items {
        match item {
            Item::Use(_) => {}
            Item::Type(t) => {
                let Some(info) = env.own_type(&t.name.name) else {
                    continue;
                };
                let fields = info
                    .fields
                    .iter()
                    .map(|f| FieldMeta {
                        id: f.id.clone(),
                        name: f.name.clone(),
                        ty: f.ty.display(app),
                        merge: f.merge,
                        doc: f.doc.clone(),
                        default: f.default.clone(),
                    })
                    .collect();
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
                    class: fx
                        .class
                        .get(idx)
                        .copied()
                        .unwrap_or(crate::types::EffectClass::Read),
                    proposal,
                    reaches: fx.reaches.get(idx).cloned().unwrap_or_default(),
                });
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
                    class: fx
                        .class
                        .get(idx)
                        .copied()
                        .unwrap_or(crate::types::EffectClass::Read),
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
                    result: c.result.display(app),
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
        value_type: result.ty.display(&app),
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
