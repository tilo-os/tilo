//! Every row a boundary: the body of a UI `for` becomes a view of its own, called once per item
//! with the item and the values the body uses from around the loop. The loop itself lowers to
//! one node, `for:<app>:<view id>`, holding the list (`items`) and those values (`arg.<name>`);
//! the host makes one boundary per item of it, keyed by the item, and renders a row only when
//! it is shown (a row of a `list`) and again only when what that row read changed. Nobody has
//! to factor a row out into a view to get there.

use std::collections::BTreeSet;

use super::{Checker, Ctx, Ex, StateRef};
use crate::abi::Imp;
use crate::ast::{self, ExprKind, StmtKind, StrPart, UiCall, UiKind, UiStmt};
use crate::ir::{self, FuncKind, LocalId, Stmt};
use crate::prims;
use crate::resolve::{Callable, ParamInfo};
use crate::types::Ty;

/// The prefix of the kind of the node a UI loop lowers to.
pub(crate) const FOR_EACH: &str = "for:";
/// The last parameter of a row view: the host passes the row's key suffix, `/<item>` after
/// the enclosing rows' (the loop node's `suffix`), the item a resource's id or else its place.
const ROW_KEY: &str = "$key";
/// The item parameter of the rows of a list that names no item: the rows of its type's
/// `row view`.
const ROW_ITEM: &str = "$item";

impl Checker<'_> {
    /// Lower `for var in list { body }` (in a view) to one node whose rows the host renders
    /// as boundaries of their own. Returns `list` back when the loop must be lowered in place
    /// instead: in a block, or when the body uses a value that cannot be passed to a view
    /// (a `View` the caller built).
    pub(super) fn rows_of(
        &mut self,
        var: &str,
        list: Ex,
        elem: &Ty,
        body: &[UiStmt],
        parent: LocalId,
    ) -> Option<Ex> {
        if self.st.ctx != Ctx::View || !passable(elem) {
            return Some(list);
        }
        let (body, hoisted) = self.hoist_layout(body, var);
        let body = body.as_slice();
        let mut names = BTreeSet::new();
        names_in_ui(body, &mut names);
        names.remove(var);
        if let Some(direct) = self.direct_row(var, elem, body) {
            self.loop_node(
                &direct.target,
                list,
                &direct.item,
                &hoisted,
                direct.around,
                parent,
            );
            return None;
        }
        let Some(captures) = self.captures(names) else {
            return Some(list);
        };
        let id = format!("{}~row{}", self.st.view_id, self.st.row_views);
        self.st.row_views = self.st.row_views.saturating_add(1);
        let owner = self.view_name();
        let (params, states) = self.row_params(var, elem, &captures);
        let func = self.row_view(
            RowView {
                id: &id,
                owner: &owner,
                var,
                params: &params,
                states: &states,
            },
            |this| this.view_body(body),
        );
        self.st.facts.views.insert(func);
        let around = captures
            .into_iter()
            .map(|(name, local, ty)| (name, Ex::local(local, ty)))
            .collect();
        self.loop_node(&id, list, var, &hoisted, around, parent);
        None
    }

    /// The rows of a `list` (`list(xs) { x -> row(x.title) }`, or the type's `row view`): one
    /// loop node under the list, whose rows each make the `item` node of one resource, holding
    /// the item block's nodes or the row view's. Rows of a list render only when the host shows
    /// them. Returns `list` back when the rows must be lowered in place instead: in a block, or
    /// when the item block uses a value that cannot be passed to a view.
    pub(super) fn list_rows(
        &mut self,
        list_node: LocalId,
        list: Ex,
        c: &UiCall,
        row_view: Option<Callable>,
    ) -> Option<Ex> {
        let elem = list.ty.element().cloned().unwrap_or(Ty::Error);
        if self.st.ctx != Ctx::View || !passable(&elem) {
            return Some(list);
        }
        let block = c.block.as_deref().unwrap_or_default();
        let var = c.item.as_ref().map_or(ROW_ITEM, |item| item.name.as_str());
        let mut names = BTreeSet::new();
        names_in_ui(block, &mut names);
        names.remove(var);
        let Some(captures) = self.captures(names) else {
            return Some(list);
        };
        let id = format!("{}~row{}", self.st.view_id, self.st.row_views);
        self.st.row_views = self.st.row_views.saturating_add(1);
        let owner = self.view_name();
        let (params, states) = self.row_params(var, &elem, &captures);
        let span = c.call.span;
        let func = self.row_view(
            RowView {
                id: &id,
                owner: &owner,
                var,
                params: &params,
                states: &states,
            },
            |this| {
                let Some((item, _)) = this.lookup(var) else {
                    return;
                };
                let item = Ex::local(item, elem.clone());
                let key = this.keyed(&format!("{id}.item"));
                let it = this.new_node("item", key);
                let at = || Ex::local(it, Ty::View);
                this.emit(Stmt::Eval(Ex::host(
                    Imp::UiPropH,
                    vec![at(), Ex::str("item"), item.clone()],
                    Ty::Unit,
                )));
                match row_view {
                    Some(v) if c.block.is_none() => {
                        let key = this.keyed(&format!("{id}.row"));
                        let row = this.view_node(&v, vec![item], key, span);
                        this.emit(Stmt::Eval(Ex::host(
                            Imp::UiChild,
                            vec![at(), Ex::local(row, Ty::View)],
                            Ty::Unit,
                        )));
                    }
                    _ => {
                        this.st.ui_parent = "item";
                        this.st.list_row_ok = true;
                        this.ui_stmts(block, it);
                    }
                }
                this.emit(Stmt::Return(Some(at())));
            },
        );
        self.st.facts.views.insert(func);
        let around = captures
            .into_iter()
            .map(|(name, local, ty)| (name, Ex::local(local, ty)))
            .collect();
        self.loop_node(&id, list, var, &[], around, list_node);
        None
    }

    /// The values a row body uses from around its loop, by name; `None` when one of them
    /// cannot be passed to a view.
    fn captures(&self, names: BTreeSet<String>) -> Option<Vec<(String, LocalId, Ty)>> {
        let mut captures = Vec::new();
        for name in names {
            if let Some((local, ty)) = self.lookup(&name) {
                if !passable(&ty) {
                    return None;
                }
                captures.push((name, local, ty));
            }
        }
        Some(captures)
    }

    /// The parameters of a row view (the item, the captures, the row's key) and the view state
    /// among the captures.
    fn row_params(
        &self,
        var: &str,
        elem: &Ty,
        captures: &[(String, LocalId, Ty)],
    ) -> (Vec<ParamInfo>, Vec<(String, StateRef)>) {
        let mut params = vec![ParamInfo {
            name: var.to_owned(),
            ty: elem.clone(),
        }];
        params.extend(captures.iter().map(|(name, _, ty)| ParamInfo {
            name: name.clone(),
            ty: ty.clone(),
        }));
        params.push(ParamInfo {
            name: ROW_KEY.to_owned(),
            ty: Ty::Text,
        });
        // View state the body uses is passed by value; `set(...)` and `bind:` still name the
        // slot, which is the view's, not the row's.
        let states = captures
            .iter()
            .filter_map(|(name, local, _)| {
                let state = self.st.states.get(name)?;
                (state.local == *local).then(|| (name.clone(), state.clone()))
            })
            .collect();
        (params, states)
    }

    /// The node of a loop whose rows call the view `target` of this package: on each item, as
    /// its parameter `item`, with the values `around` as the parameters of their names.
    fn loop_node(
        &mut self,
        target: &str,
        list: Ex,
        item: &str,
        hoisted: &[ast::Arg],
        around: Vec<(String, Ex)>,
        parent: LocalId,
    ) {
        let n = self.st.fors;
        self.st.fors = n.saturating_add(1);
        let key = self.loop_key(n);
        let node = self.new_node(&format!("{FOR_EACH}{}:{target}", self.env.app), key);
        let at = || Ex::local(node, Ty::View);
        self.arg_prop(at(), "items", list);
        self.arg_prop(at(), "each", Ex::str(item));
        // The row stands where the body's node stood: it takes that node's sizing and
        // placement, as the node a view call is wrapped in does.
        for arg in hoisted {
            match &arg.name {
                Some(name) => self.apply_arg(node, "", &name.name, &arg.value),
                None => self.flag_arg(node, &arg.value),
            }
        }
        if let Some(outer) = self.st.keys.last() {
            let outer = Ex::local(*outer, Ty::Text);
            self.arg_prop(at(), "suffix", outer);
        }
        for (name, value) in around {
            self.arg_prop(at(), &format!("arg.{name}"), value);
        }
        self.emit(Stmt::Eval(Ex::host(
            Imp::UiChild,
            vec![Ex::local(parent, Ty::View), at()],
            Ty::Unit,
        )));
    }

    /// When the body is one call of a view of this package whose arguments are the item and
    /// names from around the loop, the rows call that view: no view is made of the body.
    fn direct_row(&mut self, var: &str, elem: &Ty, body: &[UiStmt]) -> Option<DirectRow> {
        let [only] = body else { return None };
        let UiKind::Call(call) = &only.kind else {
            return None;
        };
        if call.block.is_some() || call.anchor.is_some() {
            return None;
        }
        let ExprKind::Call { callee, args } = &call.call.kind else {
            return None;
        };
        let ExprKind::Ident(name) = &callee.kind else {
            return None;
        };
        if prims::prim(name).is_some() || self.lookup(name).is_some() {
            return None;
        }
        let view = self.env.own_callable(name)?.clone();
        let func = view.func?;
        if view.kind != crate::resolve::CKind::View
            || args.len() != view.params.len()
            || view.params.iter().any(|p| !passable(&p.ty))
        {
            return None;
        }
        let mut item = None;
        let mut around = Vec::new();
        let mut named = false;
        for (pos, arg) in args.iter().enumerate() {
            let param = match &arg.name {
                Some(n) => {
                    named = true;
                    view.params.iter().find(|p| p.name == n.name)?
                }
                None if !named => view.params.get(pos)?,
                None => return None,
            };
            let ExprKind::Ident(value) = &arg.value.kind else {
                return None;
            };
            if value == var {
                if item.is_some() || !param.ty.same(elem) || elem.is_error() {
                    return None;
                }
                item = Some(param.name.clone());
                continue;
            }
            let (local, ty) = self.lookup(value)?;
            if !param.ty.same(&ty) || ty.is_error() || around.iter().any(|(p, _)| p == &param.name)
            {
                return None;
            }
            around.push((param.name.clone(), Ex::local(local, ty)));
        }
        let item = item?;
        self.st.facts.views.insert(func);
        Some(DirectRow {
            target: view.id,
            item,
            around,
        })
    }

    /// Check the body `body` lowers as the view `row.id`, its parameters `row.params` (the
    /// item first). Returns the index of its function.
    fn row_view(&mut self, row: RowView<'_>, body: impl FnOnce(&mut Self)) -> usize {
        let RowView {
            id,
            owner,
            var,
            params,
            states,
        } = row;
        let outer = std::mem::take(&mut self.st);
        self.begin(Ctx::View, params, Ty::View, id);
        self.st.owner_view = Some(owner.to_owned());
        // The row's identity, the enclosing rows' included: what makes an anchored node's key
        // name one node among the rows, as in a loop lowered in place.
        if let Some((key, _)) = self.lookup(ROW_KEY) {
            self.st.keys.push(key);
        }
        for (name, state) in states {
            if let Some((local, _)) = self.lookup(name) {
                self.st.states.insert(
                    name.clone(),
                    StateRef {
                        local,
                        ..state.clone()
                    },
                );
            }
        }
        body(self);
        let mut f = self.end(
            FuncKind::View,
            &format!("{owner} for {var}"),
            &format!("v_{id}"),
        );
        f.nparams = params.len();
        self.st = outer;
        let func = self.synth_base.saturating_add(self.synth.len());
        self.synth.push((
            f,
            ir::SynthView {
                func,
                // A row view's id is the declared view's, then `~row<n>` per loop around it.
                owner: id.split('~').next().unwrap_or(id).to_owned(),
                id: id.to_owned(),
                name: format!("{owner} for {var}"),
                params: params.to_vec(),
            },
        ));
        func
    }

    /// When the body is one UI call, its sizing and placement arguments that do not depend on
    /// the item, taken out of it: they go on the row. Returns the body without them.
    fn hoist_layout(&self, body: &[UiStmt], var: &str) -> (Vec<UiStmt>, Vec<ast::Arg>) {
        let [only] = body else {
            return (body.to_vec(), Vec::new());
        };
        let UiKind::Call(call) = &only.kind else {
            return (body.to_vec(), Vec::new());
        };
        let ExprKind::Call { callee, args } = &call.call.kind else {
            return (body.to_vec(), Vec::new());
        };
        let view_params: Option<Vec<String>> = match &callee.kind {
            ExprKind::Ident(name) if prims::prim(name).is_some() => None,
            ExprKind::Ident(name) => self
                .env
                .own_callable(name)
                .map(|v| v.params.iter().map(|p| p.name.clone()).collect()),
            _ => return (body.to_vec(), Vec::new()),
        };
        let layout = |name: &str| match &view_params {
            Some(params) => prims::NODE_ARGS.contains(&name) && !params.iter().any(|p| p == name),
            None => ROW_LAYOUT.contains(&name),
        };
        let mut kept = Vec::new();
        let mut hoisted = Vec::new();
        for arg in args {
            let mut used = BTreeSet::new();
            names_in(&arg.value, &mut used);
            let movable = !used.contains(var)
                && match &arg.name {
                    Some(name) => layout(&name.name),
                    None => self.is_flag(None, &arg.value) && flag_name(&arg.value) == Some("fill"),
                };
            if movable {
                hoisted.push(arg.clone());
            } else {
                kept.push(arg.clone());
            }
        }
        if hoisted.is_empty() {
            return (body.to_vec(), Vec::new());
        }
        let mut call = call.clone();
        call.call.kind = ExprKind::Call {
            callee: callee.clone(),
            args: kept,
        };
        let stmt = UiStmt {
            kind: UiKind::Call(call),
            span: only.span,
        };
        (vec![stmt], hoisted)
    }

    /// The key of the `n`th loop of this view, with the identity of the enclosing items.
    fn loop_key(&self, n: usize) -> Ex {
        let base = format!("{}.for{n}", self.st.view_id);
        match self.st.keys.last() {
            Some(k) => Ex::host(
                Imp::StrConcat,
                vec![Ex::str(&base), Ex::local(*k, Ty::Text)],
                Ty::Text,
            ),
            None => Ex::str(&base),
        }
    }
}

/// A view made of a row body: its id, the declared view it was written in (by name), the
/// item's name, its parameters and the view state among them.
#[derive(Clone, Copy)]
struct RowView<'a> {
    id: &'a str,
    owner: &'a str,
    var: &'a str,
    params: &'a [ParamInfo],
    states: &'a [(String, StateRef)],
}

/// A loop body that is one call of a view on the item: what the rows call.
struct DirectRow {
    target: String,
    /// The parameter that takes the item.
    item: String,
    around: Vec<(String, Ex)>,
}

/// The arguments of a primitive that size and place it among its siblings, which a row takes
/// on when the primitive is all its body is.
const ROW_LAYOUT: &[&str] = &["fill", "min", "max", "aspect", "span", "at"];

/// The name of a bare flag argument (`fill`, `fill(2)`).
fn flag_name(e: &ast::Expr) -> Option<&str> {
    match &e.kind {
        ExprKind::Ident(name) => Some(name),
        ExprKind::Call { callee, .. } => match &callee.kind {
            ExprKind::Ident(name) => Some(name),
            _ => None,
        },
        _ => None,
    }
}

/// Whether a value of this type can be an argument of a view: a node handle does not outlive
/// the run that built it.
fn passable(ty: &Ty) -> bool {
    !matches!(ty, Ty::View | Ty::Unit | Ty::Error)
}

/// Every name the UI statements use. A name a statement binds is collected too when it is
/// used: such a name is passed in needlessly when the loop's scope also has it, never missed.
fn names_in_ui(stmts: &[UiStmt], out: &mut BTreeSet<String>) {
    for s in stmts {
        match &s.kind {
            UiKind::Call(c) => {
                names_in(&c.call, out);
                if let Some(block) = &c.block {
                    names_in_ui(block, out);
                }
            }
            UiKind::For { iter, body, .. } => {
                names_in(iter, out);
                names_in_ui(body, out);
            }
            UiKind::If { cond, then, els } => {
                names_in(cond, out);
                names_in_ui(then, out);
                if let Some(els) = els {
                    names_in_ui(els, out);
                }
            }
            UiKind::Match { scrutinee, arms } => {
                names_in(scrutinee, out);
                for arm in arms {
                    names_in_ui(&arm.body, out);
                }
            }
            UiKind::Let { value, .. } => names_in(value, out),
            UiKind::Fact { label, value, .. } => {
                names_in(label, out);
                names_in(value, out);
            }
            UiKind::State(d) => {
                if let Some(default) = &d.default {
                    names_in(default, out);
                }
            }
        }
    }
}

fn names_in(e: &ast::Expr, out: &mut BTreeSet<String>) {
    match &e.kind {
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Length(..)
        | ExprKind::Bool(_)
        | ExprKind::Ref(_)
        | ExprKind::Root(_) => {}
        ExprKind::Ident(name) => {
            out.insert(name.clone());
        }
        ExprKind::Str(parts) => {
            for part in parts {
                if let StrPart::Interp(e) = part {
                    names_in(e, out);
                }
            }
        }
        ExprKind::Tuple(items) | ExprKind::List(items) => {
            for item in items {
                names_in(item, out);
            }
        }
        ExprKind::If { cond, then, els } => {
            names_in(cond, out);
            names_in_block(then, out);
            if let Some(els) = els {
                names_in(els, out);
            }
        }
        ExprKind::Block(b) => names_in_block(b, out),
        ExprKind::Match { scrutinee, arms } => {
            names_in(scrutinee, out);
            for arm in arms {
                names_in(&arm.body, out);
            }
        }
        ExprKind::Variant { args, .. } => {
            for arg in args.iter().flatten() {
                names_in(&arg.value, out);
            }
        }
        ExprKind::Lambda { body, .. } => names_in(body, out),
        ExprKind::Unary { operand, .. } => names_in(operand, out),
        ExprKind::Binary { lhs, rhs, .. } => {
            names_in(lhs, out);
            names_in(rhs, out);
        }
        ExprKind::Field { base, .. } => names_in(base, out),
        ExprKind::Call { callee, args } => {
            names_in(callee, out);
            for arg in args {
                names_in(&arg.value, out);
            }
        }
        ExprKind::Try(inner) => names_in(inner, out),
    }
}

fn names_in_block(b: &ast::Block, out: &mut BTreeSet<String>) {
    for s in &b.stmts {
        match &s.kind {
            StmtKind::Let { value, .. } => names_in(value, out),
            StmtKind::Assign { place, value, .. } => {
                if let Some(first) = place.segs.first() {
                    out.insert(first.name.clone());
                }
                names_in(value, out);
            }
            StmtKind::For { iter, body, .. } => {
                names_in(iter, out);
                names_in_block(body, out);
            }
            StmtKind::As { who, body } => {
                names_in(who, out);
                names_in_block(body, out);
            }
            StmtKind::Return(value) => {
                if let Some(value) = value {
                    names_in(value, out);
                }
            }
            StmtKind::Expr(e) => names_in(e, out),
        }
    }
}
