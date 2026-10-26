//! The checker: types (3.1), merge rules (3.2), effect discipline (3.3), own and foreign
//! (3.4), lambdas (3.6) and error reporting (3.7). It lowers well-typed code to the IR in the
//! same walk, so a program that passes is already in the form the backend consumes.

mod boundary;
mod call;
mod enums;
mod expr;
mod native;
mod pred;
#[cfg(test)]
mod tests;
mod ui;

use crate::abi::Imp;
use crate::ast::{self, Item, StmtKind};
use crate::diag::{Diagnostic, Span};
use crate::ir::{self, Conv, Ek, Facts, ForList, FuncKind, LocalId, Stmt};
use crate::resolve::{CKind, Callable, Env, ParamInfo, resolve_type_expr};
use crate::types::{MergeKind, Ty};
use std::collections::HashMap;

type Ex = ir::Expr;

/// What kind of code is being checked; each has its own effect discipline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Ctx {
    #[default]
    Fn,
    Action,
    Flow,
    View,
    /// An agent's block: reads and builds values, never changes anything.
    Block,
}

impl Ctx {
    const fn word(self) -> &'static str {
        match self {
            Self::Fn => "a fn",
            Self::Action => "an action",
            Self::Flow => "a flow",
            Self::View => "a view",
            Self::Block => "a block",
        }
    }
}

#[derive(Default)]
struct FnState {
    ctx: Ctx,
    locals: Vec<Ty>,
    scopes: Vec<HashMap<String, LocalId>>,
    sinks: Vec<Vec<Stmt>>,
    result: Ty,
    facts: Facts,
    view_id: String,
    /// Locals holding the key suffix of each enclosing UI loop, innermost last.
    keys: Vec<LocalId>,
    /// Set while checking the operand of `try`: the next `invoke` reports failure.
    try_invoke: bool,
    /// The view state the view declares, by name.
    states: HashMap<String, StateRef>,
    /// Set while checking a scenario, where `root T` names a root resource.
    scenario: bool,
    /// Where the UI being lowered stands.
    ui: UiPlace,
    /// For a view made of a loop body, the declared view it was written in (for anchors).
    owner_view: Option<String>,
    /// UI loops of this view lowered to loop nodes so far.
    fors: usize,
    /// Views made of this view's loop bodies so far.
    row_views: usize,
}

/// Where the UI being lowered stands, saved and restored around each block.
#[derive(Clone, Copy, Default)]
struct UiPlace {
    /// The primitive whose block is being lowered, empty at a view's top: `sidebar` and
    /// `inspector` sit directly in a frame, a fact directly in `facts`.
    parent: &'static str,
    /// Whether a list row `row(title, ...)` may stand here: directly in a list's item block or
    /// as a row view's body.
    list_row_ok: bool,
    /// Whether it is inside a `card`: a list there is the card's preview
    /// ([`crate::prims::PREVIEW_ROWS`]).
    in_card: bool,
}

/// A `window` or `lens` declaration in scope, for `bind:` and `set(...)`.
#[derive(Clone, Debug)]
pub(crate) struct StateRef {
    pub layer: ast::StateLayer,
    pub id: String,
    pub ty: Ty,
    pub local: LocalId,
}

pub(crate) struct Checker<'a> {
    env: &'a Env,
    diags: &'a mut Vec<Diagnostic>,
    st: FnState,
    /// Views made of UI loop bodies, in the order made; their functions follow the declared
    /// ones, from `synth_base` on.
    synth: Vec<(ir::Func, ir::SynthView)>,
    synth_base: usize,
}

/// What a checked agent block evaluates to.
pub struct BlockResult {
    pub module: ir::Module,
    pub ty: Ty,
}

/// Checks every body of the module and lowers it. Errors go to `diags`.
pub fn check_module(env: &Env, module: &ast::Module, diags: &mut Vec<Diagnostic>) -> ir::Module {
    let declared = env
        .callables
        .iter()
        .filter(|x| x.own)
        .filter_map(|x| x.func)
        .map(|f| f.saturating_add(1))
        .max()
        .unwrap_or(0);
    let mut c = Checker {
        env,
        diags,
        st: FnState::default(),
        synth: Vec::new(),
        synth_base: declared,
    };
    let mut funcs = Vec::new();
    for item in &module.items {
        let idx = funcs.len();
        let Some(callable) = env.callables.iter().find(|x| x.func == Some(idx)).cloned() else {
            continue;
        };
        let f = match item {
            Item::Action(a) => c.run_body(&callable, FuncKind::Action, Ctx::Action, &a.body),
            Item::Flow(f) => c.run_body(&callable, FuncKind::Flow, Ctx::Flow, &f.body),
            Item::Fn(f) => c.run_body(&callable, FuncKind::Fn, Ctx::Fn, &f.body),
            Item::Scenario(s) => c.run_body(&callable, FuncKind::Action, Ctx::Action, &s.body),
            Item::View(v) => c.run_view(&callable, v),
            Item::Use(_)
            | Item::Type(_)
            | Item::Enum(_)
            | Item::Root(_)
            | Item::DefaultView(_)
            | Item::RowView(_) => {
                continue;
            }
        };
        funcs.push(f);
    }
    // The presentation views of actions, in the order the resolver numbered them.
    for item in &module.items {
        let Item::Action(a) = item else { continue };
        let idx = funcs.len();
        let Some(view) = env.callables.iter().find(|x| x.func == Some(idx)).cloned() else {
            continue;
        };
        if view.name != crate::ids::present_key(&a.name.name) {
            continue;
        }
        let params: Vec<&str> = a.params.iter().map(|p| p.name.name.as_str()).collect();
        funcs.push(c.run_present(&view, &a.opts, &params));
    }
    c.check_proposals(module);
    let mut views = Vec::new();
    if funcs.len() == c.synth_base {
        for (f, v) in std::mem::take(&mut c.synth) {
            funcs.push(f);
            views.push(v);
        }
    } else if c.diags.is_empty() {
        c.err(
            "E000",
            Span::default(),
            "internal: the declared functions and their indexes disagree",
        );
    }
    ir::Module { funcs, views }
}

/// Checks a block that is one expression.
pub fn check_block_expr(env: &Env, e: &ast::Expr, diags: &mut Vec<Diagnostic>) -> BlockResult {
    let mut c = Checker {
        env,
        diags,
        st: FnState::default(),
        synth: Vec::new(),
        synth_base: 0,
    };
    c.begin(Ctx::Block, &[], Ty::Unit, "b_main");
    let ex = c.expr(e, None);
    let ty = ex.ty.clone();
    if ty == Ty::Unit {
        c.err("E072", e.span, "a block must produce a value (a view, an action application or data), but this expression has none");
    }
    c.st.result = ty.clone();
    c.emit(Stmt::Return(Some(ex)));
    let f = c.end(FuncKind::Block, "b_main", "b_main");
    BlockResult {
        module: ir::Module {
            funcs: vec![f],
            views: Vec::new(),
        },
        ty,
    }
}

/// Checks a block that is a piece of view body.
pub fn check_block_ui(env: &Env, body: &[ast::UiStmt], diags: &mut Vec<Diagnostic>) -> BlockResult {
    let mut c = Checker {
        env,
        diags,
        st: FnState::default(),
        synth: Vec::new(),
        synth_base: 0,
    };
    c.begin(Ctx::Block, &[], Ty::View, "block");
    c.view_body(body);
    let f = c.end(FuncKind::Block, "b_main", "b_main");
    BlockResult {
        module: ir::Module {
            funcs: vec![f],
            views: Vec::new(),
        },
        ty: Ty::View,
    }
}

fn always_returns(stmts: &[Stmt]) -> bool {
    match stmts.last() {
        Some(Stmt::Return(_)) => true,
        Some(Stmt::If(_, a, b)) => always_returns(a) && always_returns(b),
        _ => false,
    }
}

/// How a typed list element is read back from a host list, and converted.
pub(crate) fn load_op(ty: &Ty, i: Imp, f: Imp, h: Imp) -> (Imp, Option<Conv>) {
    match ty {
        Ty::Int => (i, None),
        Ty::Float => (f, None),
        Ty::Bool => (i, Some(Conv::IntToBool)),
        _ => (h, None),
    }
}

/// How a typed value is passed to a host store, and converted.
pub(crate) fn store_op(ty: &Ty, i: Imp, f: Imp, h: Imp) -> (Imp, Option<Conv>) {
    match ty {
        Ty::Int => (i, None),
        Ty::Float => (f, None),
        Ty::Bool => (i, Some(Conv::BoolToInt)),
        _ => (h, None),
    }
}

pub(crate) fn conv(c: Option<Conv>, e: Ex, ty: Ty) -> Ex {
    match c {
        Some(c) => Ex::new(Ek::Conv(c, Box::new(e)), ty),
        None => e,
    }
}

impl Checker<'_> {
    fn err(&mut self, code: &str, span: Span, msg: impl Into<String>) {
        self.diags.push(Diagnostic::new(code, span, msg));
    }

    fn show(&self, ty: &Ty) -> String {
        ty.display(&self.env.app)
    }

    // ---- function state -------------------------------------------------------------------

    fn begin(&mut self, ctx: Ctx, params: &[ParamInfo], result: Ty, view_id: &str) {
        self.st = FnState {
            ctx,
            result,
            view_id: view_id.to_string(),
            ..FnState::default()
        };
        self.st.scopes.push(HashMap::new());
        self.st.sinks.push(Vec::new());
        for p in params {
            self.declare(&p.name, p.ty.clone());
        }
    }

    fn end(&mut self, kind: FuncKind, name: &str, export: &str) -> ir::Func {
        let nparams = 0usize;
        let body = self.st.sinks.pop().unwrap_or_default();
        let st = std::mem::take(&mut self.st);
        ir::Func {
            kind,
            name: name.to_string(),
            export: export.to_string(),
            nparams,
            result: st.result,
            locals: st.locals,
            body,
            facts: st.facts,
        }
    }

    fn run_body(&mut self, c: &Callable, kind: FuncKind, ctx: Ctx, body: &ast::Block) -> ir::Func {
        self.begin(ctx, &c.params, c.result.clone(), &c.id);
        self.st.scenario = c.scenario;
        let valued = !matches!(c.result, Ty::Unit | Ty::Error);
        let stmts = match body.stmts.split_last() {
            // A body that ends in an expression returns its value.
            Some((
                ast::Stmt {
                    kind: StmtKind::Expr(last),
                    ..
                },
                rest,
            )) if valued => {
                self.push_sink();
                for s in rest {
                    self.stmt(s);
                }
                let v = self.expr_as(last, &c.result);
                self.emit(Stmt::Return(Some(v)));
                self.pop_sink()
            }
            _ => self.block_stmts(body),
        };
        if valued && !always_returns(&stmts) {
            let want = self.show(&c.result);
            self.err(
                "E029",
                body.span,
                format!("`{}` must return a {want} on every path; end the body with a {want} expression, or each path with `return <{want} value>`", c.name),
            );
        }
        if let Some(sink) = self.st.sinks.last_mut() {
            sink.extend(stmts);
        }
        let prefix = match kind {
            FuncKind::Action => "a_",
            FuncKind::Flow => "f_",
            FuncKind::Fn => "n_",
            FuncKind::View => "v_",
            FuncKind::Block => "b_",
        };
        let mut f = self.end(kind, &c.name, &format!("{prefix}{}", c.id));
        f.nparams = c.params.len();
        f
    }

    fn run_view(&mut self, c: &Callable, v: &ast::ViewDecl) -> ir::Func {
        self.begin(Ctx::View, &c.params, Ty::View, &c.id);
        let rest = self.view_state(&c.name, &v.body);
        let env = self.env;
        if let Some((key, _)) = env.default_views.iter().find(|(_, view)| *view == c.name) {
            self.window_frame(&c.name, &key.name, &rest, v.body_span);
        }
        let row_of = env
            .row_views
            .iter()
            .find(|(key, view)| key.app == env.app && *view == c.name);
        if let Some((key, _)) = row_of {
            self.row_view_body(&c.name, &key.name, &rest, v.body_span);
            self.st.ui.list_row_ok = true;
        }
        self.view_body(&rest);
        let mut f = self.end(FuncKind::View, &c.name, &format!("v_{}", c.id));
        f.nparams = c.params.len();
        f
    }

    // ---- locals and sinks -----------------------------------------------------------------

    fn fresh(&mut self, ty: Ty) -> LocalId {
        let id = self.st.locals.len() as LocalId;
        self.st.locals.push(ty);
        id
    }

    fn declare(&mut self, name: &str, ty: Ty) -> LocalId {
        let id = self.fresh(ty);
        self.bind_local(name, id);
        id
    }

    fn bind_local(&mut self, name: &str, id: LocalId) {
        if let Some(scope) = self.st.scopes.last_mut() {
            scope.insert(name.to_string(), id);
        }
    }

    fn lookup(&self, name: &str) -> Option<(LocalId, Ty)> {
        let id = self
            .st
            .scopes
            .iter()
            .rev()
            .find_map(|s| s.get(name).copied())?;
        let ty = self.st.locals.get(id as usize)?.clone();
        Some((id, ty))
    }

    fn locals_in_scope(&self) -> String {
        let mut names: Vec<&str> = self
            .st
            .scopes
            .iter()
            .flat_map(|s| s.keys().map(String::as_str))
            .collect();
        names.sort_unstable();
        names.dedup();
        if names.is_empty() {
            "none".to_string()
        } else {
            names.join(", ")
        }
    }

    fn emit(&mut self, s: Stmt) {
        if let Some(sink) = self.st.sinks.last_mut() {
            sink.push(s);
        }
    }

    fn push_sink(&mut self) {
        self.st.sinks.push(Vec::new());
        self.st.scopes.push(HashMap::new());
    }

    fn pop_sink(&mut self) -> Vec<Stmt> {
        self.st.scopes.pop();
        self.st.sinks.pop().unwrap_or_default()
    }

    fn block_stmts(&mut self, b: &ast::Block) -> Vec<Stmt> {
        self.push_sink();
        for s in &b.stmts {
            self.stmt(s);
        }
        self.pop_sink()
    }

    // ---- statements -----------------------------------------------------------------------

    fn stmt(&mut self, s: &ast::Stmt) {
        match &s.kind {
            StmtKind::Let { name, ty, value } => self.let_stmt(&name.name, ty.as_ref(), value),
            StmtKind::Assign { place, op, value } => self.assign(place, *op, value, s.span),
            StmtKind::For { var, iter, body } => {
                let list = self.expr(iter, None);
                let Some(elem) = self.element_of(&list.ty, iter.span) else {
                    return;
                };
                let mut fl = self.new_for(list, &elem);
                self.push_sink();
                self.bind_local(&var.name, fl.item_local);
                for st in &body.stmts {
                    self.stmt(st);
                }
                fl.body = self.pop_sink();
                self.emit(Stmt::ForList(Box::new(fl)));
            }
            StmtKind::Return(value) => self.return_stmt(value.as_ref(), s.span),
            StmtKind::As { who, body } => self.act_as(who, body, s.span),
            StmtKind::Expr(e) => self.expr_stmt(e),
        }
    }

    /// `as who { ... }` in a scenario: the block's action calls run with `invoker` = `who`.
    fn act_as(&mut self, who: &ast::Expr, body: &ast::Block, span: Span) {
        if !self.st.scenario {
            self.err("E094", span, "`as who { ... }` runs actions as a named person, which only a scenario does; elsewhere an action runs as whoever invokes it");
            let _ = self.expr(who, None);
            let _ = self.block_stmts(body);
            return;
        }
        let who = self.expr_as(who, &Ty::Principal);
        self.emit(Stmt::Eval(Ex::host(Imp::InvokerPush, vec![who], Ty::Unit)));
        for s in self.block_stmts(body) {
            self.emit(s);
        }
        self.emit(Stmt::Eval(Ex::host(Imp::InvokerPop, vec![], Ty::Unit)));
    }

    /// An expression whose value is not used: an `if`, `match` or block runs its branches as
    /// statements, so they need not agree on a type and an `if` needs no `else`.
    fn expr_stmt(&mut self, e: &ast::Expr) {
        match &e.kind {
            ast::ExprKind::If { cond, then, els } => {
                let cond = self.expr_as(cond, &Ty::Bool);
                let then = self.block_stmts(then);
                let els = match els {
                    None => Vec::new(),
                    Some(x) => {
                        self.push_sink();
                        self.expr_stmt(x);
                        self.pop_sink()
                    }
                };
                self.emit(Stmt::If(cond, then, els));
            }
            ast::ExprKind::Match { scrutinee, arms } => self.match_stmt(scrutinee, arms, e.span),
            ast::ExprKind::Block(b) => {
                let stmts = self.block_stmts(b);
                for s in stmts {
                    self.emit(s);
                }
            }
            _ => {
                let ex = self.expr(e, None);
                self.emit(Stmt::Eval(ex));
            }
        }
    }

    /// A block whose value is used: its last expression, checked against `exp`. A block that
    /// returns on every path has any type.
    fn block_value(&mut self, b: &ast::Block, exp: Option<&Ty>) -> Ex {
        self.push_sink();
        let value = match b.stmts.split_last() {
            Some((
                ast::Stmt {
                    kind: StmtKind::Expr(last),
                    ..
                },
                rest,
            )) => {
                for s in rest {
                    self.stmt(s);
                }
                Some(self.expr(last, exp))
            }
            _ => {
                for s in &b.stmts {
                    self.stmt(s);
                }
                None
            }
        };
        let stmts = self.pop_sink();
        let value = match value {
            Some(v) => v,
            None if always_returns(&stmts) => {
                Ex::new(Ek::Unreachable, exp.cloned().unwrap_or(Ty::Error))
            }
            None => Ex::new(Ek::Unit, Ty::Unit),
        };
        let ty = value.ty.clone();
        Ex::new(Ek::Seq(stmts, Box::new(value)), ty)
    }

    /// `if` whose value is used: both branches are required and agree on a type.
    fn if_value(
        &mut self,
        cond: &ast::Expr,
        then: &ast::Block,
        els: Option<&ast::Expr>,
        exp: Option<&Ty>,
        span: Span,
    ) -> Ex {
        let c = self.expr_as(cond, &Ty::Bool);
        let hint = exp.filter(|t| !t.is_error());
        let t = self.block_value(then, hint);
        let Some(els) = els else {
            if !matches!(t.ty, Ty::Unit | Ty::Error) {
                self.err("E038", span, "this `if` is used as a value, so it needs an `else` branch: `if cond { a } else { b }`");
                return Ex::error();
            }
            let none = Ex::new(Ek::Unit, Ty::Unit);
            return Ex::new(Ek::If(Box::new(c), Box::new(t), Box::new(none)), Ty::Unit);
        };
        let want = hint.cloned().unwrap_or_else(|| t.ty.clone());
        let t = self.coerce(t, &want, then.span);
        let f = if want.is_error() {
            self.expr(els, None)
        } else {
            self.expr_as(els, &want)
        };
        let want = if want.is_error() { f.ty.clone() } else { want };
        Ex::new(Ek::If(Box::new(c), Box::new(t), Box::new(f)), want)
    }

    fn let_stmt(&mut self, name: &str, ann: Option<&ast::TypeExpr>, value: &ast::Expr) {
        let want = ann.map(|t| resolve_type_expr(self.env, t, self.diags));
        let ex = match &want {
            Some(w) => self.expr_as(value, w),
            None => self.expr(value, None),
        };
        let mut ty = want.unwrap_or_else(|| ex.ty.clone());
        if ty == Ty::Unit {
            self.err(
                "E020",
                value.span,
                format!("this expression has no value, so `{name}` cannot hold it"),
            );
            ty = Ty::Error;
        }
        let id = self.declare(name, ty);
        self.emit(Stmt::Set(id, ex));
    }

    fn return_stmt(&mut self, value: Option<&ast::Expr>, span: Span) {
        let want = self.st.result.clone();
        match value {
            None => {
                if !matches!(want, Ty::Unit | Ty::Error) {
                    let w = self.show(&want);
                    self.err(
                        "E029",
                        span,
                        format!("this function returns {w}; write `return <{w} value>`"),
                    );
                }
                self.emit(Stmt::Return(None));
            }
            Some(e) => {
                if matches!(want, Ty::Unit) {
                    self.err("E029", span, "this function has no result type, so `return` takes no value; declare one with `-> Type`");
                    let _ = self.expr(e, None);
                    self.emit(Stmt::Return(None));
                } else {
                    let ex = self.expr_as(e, &want);
                    self.emit(Stmt::Return(Some(ex)));
                }
            }
        }
    }

    /// The element type a `for` loop walks, or an error.
    fn element_of(&mut self, ty: &Ty, span: Span) -> Option<Ty> {
        if ty.is_error() {
            return None;
        }
        if let Some(t) = ty.element() {
            return Some(t.clone());
        }
        if matches!(ty, Ty::Map(_)) {
            self.err(
                "E080",
                span,
                "looping over a Map is not supported in v1; keep the keys in a Set or a Collection",
            );
        } else {
            let t = self.show(ty);
            self.err(
                "E020",
                span,
                format!("a `for` loop walks a List, Set or Collection, but this is {t}"),
            );
        }
        None
    }

    /// Allocates the locals of a loop over a host list.
    fn new_for(&mut self, list: Ex, elem: &Ty) -> ForList {
        let (get, get_conv) = load_op(elem, Imp::ListGetI, Imp::ListGetF, Imp::ListGetH);
        ForList {
            list,
            list_local: self.fresh(Ty::Text),
            len_local: self.fresh(Ty::Int),
            idx_local: self.fresh(Ty::Int),
            item_local: self.fresh(elem.clone()),
            get,
            get_conv,
            body: Vec::new(),
        }
    }

    // ---- assignment and merge rules -------------------------------------------------------

    fn assign(&mut self, place: &ast::Place, op: ast::AssignOp, value: &ast::Expr, span: Span) {
        let Some((last, prefix)) = place.segs.split_last() else {
            return;
        };
        if prefix.is_empty() {
            self.assign_local(last, op, value);
            return;
        }
        let mut base = ast::Expr {
            kind: ast::ExprKind::Ident(prefix[0].name.clone()),
            span: prefix[0].span,
        };
        for seg in prefix.iter().skip(1) {
            let sp = base.span.to(seg.span);
            base = ast::Expr {
                kind: ast::ExprKind::Field {
                    base: Box::new(base),
                    name: seg.clone(),
                },
                span: sp,
            };
        }
        let base_ex = self.expr(&base, None);
        self.assign_field(base_ex, last, op, value, span);
    }

    fn assign_local(&mut self, name: &ast::Ident, op: ast::AssignOp, value: &ast::Expr) {
        let Some((id, ty)) = self.lookup(&name.name) else {
            let known = self.locals_in_scope();
            self.err(
                "E014",
                name.span,
                format!(
                    "unknown variable `{}`; variables in scope: {known}",
                    name.name
                ),
            );
            let _ = self.expr(value, None);
            return;
        };
        let cur = Ex::local(id, ty.clone());
        let rhs = self.expr_as(value, &ty);
        let new = match op {
            ast::AssignOp::Set => rhs,
            ast::AssignOp::Add | ast::AssignOp::Sub => {
                let bop = if op == ast::AssignOp::Add {
                    ir::BinOp::Add
                } else {
                    ir::BinOp::Sub
                };
                match &ty {
                    Ty::Int | Ty::Float => {
                        Ex::new(Ek::Bin(bop, Box::new(cur), Box::new(rhs)), ty.clone())
                    }
                    Ty::Text if op == ast::AssignOp::Add => {
                        Ex::host(Imp::StrConcat, vec![cur, rhs], Ty::Text)
                    }
                    Ty::Error => rhs,
                    other => {
                        let t = self.show(other);
                        self.err("E034", name.span, format!("`{}` on a {t} variable is not defined; `+=` and `-=` work on Int, Float and (for `+=`) Text", op.text()));
                        rhs
                    }
                }
            }
        };
        self.emit(Stmt::Set(id, new));
    }

    fn assign_field(
        &mut self,
        base: Ex,
        field: &ast::Ident,
        op: ast::AssignOp,
        value: &ast::Expr,
        span: Span,
    ) {
        let Ty::Record(key) = base.ty.clone() else {
            if !base.ty.is_error() {
                let t = self.show(&base.ty);
                self.err(
                    "E021",
                    field.span,
                    format!(
                        "cannot assign `{}`: the value has type {t}, which has no fields",
                        field.name
                    ),
                );
            }
            let _ = self.expr(value, None);
            return;
        };
        let Some(info) = self.env.type_info(&key) else {
            return;
        };
        let Some(f) = info.field(&field.name).cloned() else {
            let names = info.field_names();
            self.err(
                "E021",
                field.span,
                format!(
                    "`{}` has no field `{}`; fields are: {names}",
                    key.name, field.name
                ),
            );
            let _ = self.expr(value, None);
            return;
        };
        if self.st.ctx != Ctx::Action {
            let who = self.st.ctx.word();
            self.err("E040", span, format!("{who} cannot change `{}.{}`; only an action mutates fields. Write an action and apply it", key.name, f.name));
            let _ = self.expr(value, None);
            return;
        }
        if key.app != self.env.app {
            self.err("E045", span, format!("`{}` is declared by another app ({}); an action may only change its own module's types. Invoke that app's action from a flow instead", key.name, key.app));
            let _ = self.expr(value, None);
            return;
        }
        self.st.facts.mutates = true;
        if f.merge == MergeKind::Structural {
            self.err("E033", span, format!("`{}` is a {} field and cannot be assigned; change it with its methods (`.insert`, `.remove`, `.move`, `.add`, `.put`)", f.name, f.ty.display(&self.env.app)));
            let _ = self.expr(value, None);
            return;
        }
        let fid = Ex::str(&f.id);
        match (op, f.merge) {
            (ast::AssignOp::Add | ast::AssignOp::Sub, MergeKind::Counter) => {
                let mut d = self.expr_as(value, &f.ty);
                if op == ast::AssignOp::Sub {
                    d = Ex::new(Ek::Neg(Box::new(d)), f.ty.clone());
                }
                let imp = if f.ty == Ty::Float {
                    Imp::ResAddF
                } else {
                    Imp::ResAddI
                };
                self.emit(Stmt::Eval(Ex::host(imp, vec![base, fid, d], Ty::Unit)));
            }
            (ast::AssignOp::Add | ast::AssignOp::Sub, MergeKind::Text) => {
                self.err("E034", span, format!("`{}` on a text field is not defined; assign the whole text with `=` and the host splices the difference", op.text()));
                let _ = self.expr(value, None);
            }
            (ast::AssignOp::Add | ast::AssignOp::Sub, _) => {
                let ty = self.show(&f.ty);
                self.err(
                    "E030",
                    span,
                    format!(
                        "`{}` on a register field loses concurrent updates; declare `{name}: {ty} merge counter`, or write the read and the write out (`x.{name} = x.{name} + 1`)",
                        op.text(),
                        name = f.name,
                    ),
                );
                let _ = self.expr(value, None);
            }
            (ast::AssignOp::Set, MergeKind::Counter) => {
                self.err("E031", span, format!("`=` on a counter field overwrites concurrent increments; use `+=` or `-=` on `{}`, or declare it `merge register`", f.name));
                let _ = self.expr(value, None);
            }
            (ast::AssignOp::Set, _) => {
                let v = self.expr_as(value, &f.ty);
                let stmt = if f.merge == MergeKind::Text {
                    Ex::host(Imp::ResTextSet, vec![base, fid, v], Ty::Unit)
                } else {
                    let (imp, c) = store_op(&f.ty, Imp::ResSetI, Imp::ResSetF, Imp::ResSetH);
                    Ex::host(imp, vec![base, fid, conv(c, v, Ty::Int)], Ty::Unit)
                };
                self.emit(Stmt::Eval(stmt));
            }
        }
    }

    // ---- proposals ------------------------------------------------------------------------

    fn check_proposals(&mut self, module: &ast::Module) {
        for item in &module.items {
            let Item::Action(a) = item else { continue };
            let Some(p) = &a.proposal else { continue };
            let env = self.env;
            let Some(view) = env
                .own_callable(&p.view.name)
                .filter(|c| c.kind == CKind::View)
            else {
                let views: Vec<&str> = env
                    .callables
                    .iter()
                    .filter(|c| c.own && c.kind == CKind::View && !c.hidden())
                    .map(|c| c.name.as_str())
                    .collect();
                self.err(
                    "E014",
                    p.view.span,
                    format!(
                        "the proposal view `{}` does not exist; views: {}",
                        p.view.name,
                        views.join(", ")
                    ),
                );
                continue;
            };
            let Some(action) = env.own_callable(&a.name.name) else {
                continue;
            };
            let args: Vec<String> = match &p.args {
                Some(names) => names.iter().map(|n| n.name.clone()).collect(),
                None => action.params.iter().map(|x| x.name.clone()).collect(),
            };
            let mut types = Vec::new();
            for name in &args {
                match action.params.iter().find(|x| &x.name == name) {
                    Some(x) => types.push(x.ty.clone()),
                    None => self.err(
                        "E014",
                        p.view.span,
                        format!("`{name}` is not a parameter of action `{}`", a.name.name),
                    ),
                }
            }
            let ok = types.len() == view.params.len()
                && types.iter().zip(&view.params).all(|(t, v)| t.same(&v.ty));
            if !ok {
                self.err("E020", p.view.span, format!("the proposal view must take the action's arguments; `{}` is {}, the action passes ({})", p.view.name, view.signature(&env.app), args.join(", ")));
            }
        }
    }
}
