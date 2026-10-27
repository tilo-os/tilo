//! Every row a boundary: the body of a UI `for` becomes a view of its own, called once per item
//! with the item and the values the body uses from around the loop. The loop itself lowers to
//! one node, `for:<app>:<view id>`, holding the list (`items`) and those values (`arg.<name>`);
//! the host makes one boundary per item of it, keyed by the item, and renders a row only when
//! it is shown (a row of a `list`) and again only when what that row read changed. Nobody has
//! to factor a row out into a view to get there.

use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use super::template::{Closure, Macro, Scope, UiClosure};
use super::{Checker, Ctx, Ex, StateRef, UiPlace};
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
        // A loop in a named slot of a component makes parts of it (the commands of a menu, the
        // actions of a header): it lowers in place.
        if self.st.ctx != Ctx::View || !passable(elem) || self.st.ui.parts {
            return Some(list);
        }
        let (body, hoisted) = self.hoist_layout(body, var);
        let body = body.as_slice();
        let mut names = BTreeSet::new();
        names_in_ui(body, &mut names);
        names.remove(var);
        if let Some(direct) = self
            .direct_row(var, elem, body)
            .filter(|_| self.st.macros.is_empty())
        {
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
        // The lambdas and blocks of a template's caller the body applies, and what they read.
        let macros: Vec<(String, Macro)> = names
            .iter()
            .filter_map(|name| Some((name.clone(), self.st.macros.get(name)?.clone())))
            .filter(|(name, m)| !matches!(m, Macro::State(_)) || self.lookup(name).is_none())
            .collect();
        let mut read: Vec<LocalId> = Vec::new();
        for (_, m) in &macros {
            macro_reads(m, &mut read);
        }
        read.sort_unstable();
        read.dedup();
        let Some(mut captures) = self.captures(names) else {
            return Some(list);
        };
        let mut remap: Vec<(LocalId, String)> = Vec::new();
        for local in read {
            if captures.iter().any(|(_, held, _)| *held == local) {
                if let Some((name, ..)) = captures.iter().find(|(_, held, _)| *held == local) {
                    remap.push((local, name.clone()));
                }
                continue;
            }
            let Some(ty) = self.st.locals.get(local as usize).cloned() else {
                continue;
            };
            if !passable(&ty) {
                return Some(list);
            }
            let name = format!("$m{local}");
            remap.push((local, name.clone()));
            captures.push((name, local, ty));
        }
        let id = format!("{}~row{}", self.st.view_id, self.st.row_views);
        self.st.row_views = self.st.row_views.saturating_add(1);
        let owner = self.view_name();
        let (params, states) = self.row_params(var, elem, &captures);
        let inherited = Inherited {
            ns: self.st.ns.clone(),
            types: self.st.types.clone(),
            prefix: self.st.prefix.clone(),
            ui: self.st.ui,
            macros,
            remap,
        };
        let func = self.row_view(
            RowView {
                id: &id,
                owner: &owner,
                var,
                params: &params,
                states: &states,
            },
            |this| {
                this.inherit(inherited);
                this.view_body(body);
            },
        );
        self.st.facts.views.insert(func);
        let around = captures
            .into_iter()
            .map(|(name, local, ty)| (name, Ex::local(local, ty)))
            .collect();
        self.loop_node(&id, list, var, &hoisted, around, parent);
        None
    }

    /// Take on, in a row view made of a loop body, what the loop saw of the template being
    /// expanded: its package's names, its type parameters, its keys, and its caller's lambdas
    /// and blocks, which now read the row view's parameters.
    fn inherit(&mut self, inherited: Inherited) {
        let Inherited {
            ns,
            types,
            prefix,
            ui,
            macros,
            remap,
        } = inherited;
        self.st.ns = ns;
        self.st.types = types;
        self.st.prefix = prefix;
        self.st.ui = UiPlace {
            parent: self.st.ui.parent,
            ..ui
        };
        let map: HashMap<LocalId, LocalId> = remap
            .into_iter()
            .filter_map(|(old, name)| Some((old, self.lookup(&name)?.0)))
            .collect();
        for (name, m) in macros {
            let m = remap_macro(&m, &map);
            if let Macro::State(state) = &m {
                self.bind_local(&name, state.local);
                self.st.states.insert(name.clone(), state.clone());
            }
            self.st.macros.insert(name, m);
        }
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
        if names.iter().any(|name| self.st.macros.contains_key(name)) {
            return Some(list);
        }
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
                        this.st.ui.parent = "item";
                        this.st.ui.list_row_ok = true;
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
            default: None,
        }];
        params.extend(captures.iter().map(|(name, _, ty)| ParamInfo {
            name: name.clone(),
            ty: ty.clone(),
            default: None,
        }));
        params.push(ParamInfo {
            name: ROW_KEY.to_owned(),
            ty: Ty::Text,
            default: None,
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
        let view = self.own_callable(name)?.clone();
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

/// What a row view made inside a template's expansion takes on from it.
struct Inherited {
    ns: Option<String>,
    types: HashMap<String, Ty>,
    prefix: String,
    ui: UiPlace,
    /// The caller's lambdas and blocks the body applies, by name.
    macros: Vec<(String, Macro)>,
    /// Each local they read, and the row view's parameter that holds it.
    remap: Vec<(LocalId, String)>,
}

/// Every local a lambda or block of a template's caller reads, its nested ones included.
fn macro_reads(m: &Macro, out: &mut Vec<LocalId>) {
    match m {
        Macro::State(state) => out.push(state.local),
        Macro::Lambda(closure) => {
            let mut names = BTreeSet::new();
            names_in(&closure.body, &mut names);
            for (param, _) in &closure.params {
                names.remove(param);
            }
            scope_reads(&closure.scope, &names, out);
        }
        Macro::Block(block) => {
            let mut names = BTreeSet::new();
            names_in_ui(&block.body, &mut names);
            if let Some((item, _)) = &block.item {
                names.remove(item);
            }
            scope_reads(&block.scope, &names, out);
        }
        Macro::Some(inner) => macro_reads(inner, out),
        Macro::View(_) | Macro::Absent => {}
    }
}

/// The locals `names` read in `scope`: its variables, its view state and its own lambdas'.
fn scope_reads(scope: &Scope, names: &BTreeSet<String>, out: &mut Vec<LocalId>) {
    for name in names {
        if let Some(local) = scope.names.get(name) {
            out.push(*local);
        }
        if let Some(state) = scope.states.get(name) {
            out.push(state.local);
        }
        if let Some(m) = scope.macros.get(name) {
            macro_reads(m, out);
        }
    }
}

/// `m` reading the locals of another function: each old local the new one `map` gives.
fn remap_macro(m: &Macro, map: &HashMap<LocalId, LocalId>) -> Macro {
    match m {
        Macro::State(state) => Macro::State(StateRef {
            local: map.get(&state.local).copied().unwrap_or(state.local),
            ..state.clone()
        }),
        Macro::Lambda(closure) => Macro::Lambda(Rc::new(Closure {
            params: closure.params.clone(),
            result: closure.result.clone(),
            body: closure.body.clone(),
            scope: remap_scope(&closure.scope, map),
        })),
        Macro::Block(block) => Macro::Block(Rc::new(UiClosure {
            item: block.item.clone(),
            body: block.body.clone(),
            scope: remap_scope(&block.scope, map),
        })),
        Macro::View(v) => Macro::View(Rc::clone(v)),
        Macro::Some(inner) => Macro::Some(Box::new(remap_macro(inner, map))),
        Macro::Absent => Macro::Absent,
    }
}

/// `scope` in another function: the locals it can still read, as `map` moves them.
fn remap_scope(scope: &Scope, map: &HashMap<LocalId, LocalId>) -> Scope {
    Scope {
        names: scope
            .names
            .iter()
            .filter_map(|(name, local)| Some((name.clone(), *map.get(local)?)))
            .collect(),
        states: scope
            .states
            .iter()
            .filter_map(|(name, state)| {
                Some((
                    name.clone(),
                    StateRef {
                        local: *map.get(&state.local)?,
                        ..state.clone()
                    },
                ))
            })
            .collect(),
        macros: scope
            .macros
            .iter()
            .map(|(name, m)| (name.clone(), remap_macro(m, map)))
            .collect(),
        ns: scope.ns.clone(),
        types: scope.types.clone(),
        prefix: scope.prefix.clone(),
        ui: scope.ui,
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
            UiKind::Slot { body, .. } => names_in_ui(body, out),
            UiKind::On { action, .. } => names_in(action, out),
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
