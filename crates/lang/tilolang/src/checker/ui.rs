//! View bodies: UI statements lowered to `ui_*` host calls (implementation.md section 4.4).

use super::expr::dotted;
use super::{Checker, Ex, StateRef, conv, store_op};
use crate::abi::Imp;
use crate::ast::{self, ExprKind, UiCall, UiKind, UiStmt};
use crate::diag::Span;
use crate::ir::{Conv, Ek, LocalId, Stmt};
use crate::prims::{self, Prim};
use crate::resolve::{CKind, Callable, TypeInfo};
use crate::types::Ty;

impl Checker<'_> {
    /// Lowers the `window` and `lens` declarations at the top of a view body: each reads its
    /// slot, or its default while the slot is empty, into a local of its name. Returns the rest
    /// of the body.
    pub(super) fn view_state(&mut self, view: &str, body: &[UiStmt]) -> Vec<UiStmt> {
        let mut rest = Vec::new();
        for s in body {
            let UiKind::State(d) = &s.kind else {
                rest.push(s.clone());
                continue;
            };
            let Some(info) = self
                .env
                .states
                .iter()
                .find(|x| x.view == view && x.name == d.name.name)
                .cloned()
            else {
                continue;
            };
            let fallback = match &d.default {
                Some(e) => self.expr_as(e, &info.ty),
                None => self.zero_of(&info.ty, d.name.span),
            };
            // The slot's value, or the default while it is empty; scalars travel boxed.
            let layer = Ex::new(Ek::I32(info.layer.code()), Ty::Bool);
            let args = vec![layer, Ex::str(&info.id), Self::handle_of(fallback)];
            let value = if info.ty.is_scalar() {
                let boxed = Ty::Option(Box::new(info.ty.clone()));
                Self::unwrap_opt(Ex::host(Imp::StateGet, args, boxed), &info.ty)
            } else {
                Ex::host(Imp::StateGet, args, info.ty.clone())
            };
            let local = self.declare(&info.name, info.ty.clone());
            self.emit(Stmt::Set(local, value));
            self.st.states.insert(
                info.name.clone(),
                StateRef {
                    layer: info.layer,
                    id: info.id.clone(),
                    ty: info.ty.clone(),
                    local,
                },
            );
        }
        rest
    }

    /// The value view state of type `ty` starts with when it declares no default.
    fn zero_of(&mut self, ty: &Ty, span: Span) -> Ex {
        match ty {
            Ty::Int => Ex::int(0),
            Ty::Float => Ex::new(Ek::Float(0.0), Ty::Float),
            Ty::Bool => Ex::new(Ek::Bool(false), Ty::Bool),
            Ty::Text => Ex::str(""),
            Ty::Option(_) => Ex::host(Imp::NoneV, Vec::new(), ty.clone()),
            Ty::List(_) => Ex::host(Imp::ListNew, Vec::new(), ty.clone()),
            Ty::Choice(_) => Ex::new(Ek::Int(0), ty.clone()),
            Ty::Error => Ex::error(),
            Ty::Enum(_) => {
                let shape = super::enums::Shape::of(self, ty);
                let first = shape
                    .as_ref()
                    .and_then(|s| s.variants().into_iter().next())
                    .filter(|v| v.fields.is_empty());
                match (shape, first) {
                    (Some(shape), Some(v)) => {
                        let name = ast::Ident { name: v.name, span };
                        self.construct_variant(&shape, ty, &name, None, span)
                    }
                    _ => {
                        let t = self.show(ty);
                        self.err("E066", span, format!("this view state needs a default: the first variant of {t} carries a payload; write `= .<variant>(...)`"));
                        Ex::error()
                    }
                }
            }
            other => {
                let t = self.show(other);
                self.err(
                    "E066",
                    span,
                    format!("view state of type {t} needs a default: write `= <value>`"),
                );
                Ex::error()
            }
        }
    }

    /// The view state `name` names here, unless a variable of that name hides it.
    pub(super) fn state_named(&self, name: &str) -> Option<StateRef> {
        let state = self.st.states.get(name)?;
        let (local, _) = self.lookup(name)?;
        (local == state.local).then(|| state.clone())
    }

    /// The names of the view state in scope, for messages.
    pub(super) fn state_names(&self) -> String {
        let mut names: Vec<&str> = self.st.states.keys().map(String::as_str).collect();
        names.sort_unstable();
        if names.is_empty() {
            "nothing".to_string()
        } else {
            names.join(", ")
        }
    }

    /// Lowers a whole view body and returns its root node. A body of `let`s and one call has
    /// that call as its root.
    pub(super) fn view_body(&mut self, body: &[UiStmt]) {
        let lets_then_call = match body.split_last() {
            Some((
                UiStmt {
                    kind: UiKind::Call(c),
                    ..
                },
                lets,
            )) if lets.iter().all(|s| matches!(s.kind, UiKind::Let { .. })) => Some((lets, c)),
            _ => None,
        };
        let root = match lets_then_call {
            Some((lets, c)) => {
                let dead = self.fresh(Ty::Error);
                self.ui_stmts(lets, dead);
                self.ui_call(c, None)
            }
            None => {
                let root = self.fresh(Ty::View);
                let key = Ex::str(&self.st.view_id.clone());
                self.emit(Stmt::Set(
                    root,
                    Ex::host(Imp::UiNode, vec![Ex::str("fragment"), key], Ty::View),
                ));
                self.ui_stmts(body, root);
                root
            }
        };
        self.emit(Stmt::Return(Some(Ex::local(root, Ty::View))));
    }

    pub(super) fn ui_stmts(&mut self, stmts: &[UiStmt], parent: LocalId) {
        for s in stmts {
            match &s.kind {
                UiKind::Call(c) => {
                    self.ui_call(c, Some(parent));
                }
                UiKind::Let { name, value } => {
                    let v = self.expr(value, None);
                    let id = self.declare(&name.name, v.ty.clone());
                    self.emit(Stmt::Set(id, v));
                }
                UiKind::If { cond, then, els } => {
                    let c = self.expr_as(cond, &Ty::Bool);
                    self.push_sink();
                    self.ui_stmts(then, parent);
                    let t = self.pop_sink();
                    self.push_sink();
                    if let Some(e) = els {
                        self.ui_stmts(e, parent);
                    }
                    let f = self.pop_sink();
                    self.emit(Stmt::If(c, t, f));
                }
                UiKind::For { var, iter, body } => self.ui_for(&var.name, iter, body, parent),
                UiKind::Match { scrutinee, arms } => {
                    self.match_ui(scrutinee, arms, parent, s.span);
                }
                UiKind::Fact { node, label, value } => {
                    self.fact(*node, label, value, parent, s.span);
                }
                UiKind::State(d) => {
                    self.err(
                        "E066",
                        d.span,
                        format!(
                            "declare `{} {}` at the top of the view's body, not inside a block",
                            d.layer.word(),
                            d.name.name
                        ),
                    );
                }
            }
        }
    }

    /// A loop whose items get keys built from the item's identity (a record's reference id,
    /// or the index for plain values).
    fn ui_for(&mut self, var: &str, iter: &ast::Expr, body: &[UiStmt], parent: LocalId) {
        let list = self.expr(iter, None);
        let Some(elem) = self.element_of(&list.ty, iter.span) else {
            return;
        };
        let Some(list) = self.rows_of(var, list, &elem, body, parent) else {
            return;
        };
        let mut fl = self.new_for(list, &elem);
        let part = if matches!(elem, Ty::Record(_)) {
            Ex::host(
                Imp::RefId,
                vec![Ex::local(fl.item_local, elem.clone())],
                Ty::Text,
            )
        } else {
            Ex::host(
                Imp::IntToStr,
                vec![Ex::local(fl.idx_local, Ty::Int)],
                Ty::Text,
            )
        };
        let sep = Ex::host(Imp::StrConcat, vec![Ex::str("/"), part], Ty::Text);
        let suffix = match self.st.keys.last() {
            Some(k) => Ex::host(Imp::StrConcat, vec![Ex::local(*k, Ty::Text), sep], Ty::Text),
            None => sep,
        };
        let key_local = self.fresh(Ty::Text);
        self.push_sink();
        self.bind_local(var, fl.item_local);
        self.emit(Stmt::Set(key_local, suffix));
        self.st.keys.push(key_local);
        self.ui_stmts(body, parent);
        self.st.keys.pop();
        fl.body = self.pop_sink();
        self.emit(Stmt::ForList(Box::new(fl)));
    }

    /// The key of a node: its call id, combined with the identity of the enclosing items.
    pub(super) fn node_key(&self, c: &UiCall) -> Ex {
        let base = match &c.anchor {
            Some(a) => {
                let id = self
                    .env
                    .anchors
                    .get(&format!("{}#{}", self.view_name(), a.name))
                    .cloned()
                    .unwrap_or_default();
                if id.is_empty() {
                    format!("{}.{}", self.st.view_id, c.node)
                } else {
                    id
                }
            }
            None => format!("{}.{}", self.st.view_id, c.node),
        };
        self.keyed(&base)
    }

    /// The key of the statement `node` that is not a call (a fact).
    pub(super) fn stmt_key(&self, node: ast::NodeId) -> Ex {
        self.keyed(&format!("{}.{}", self.st.view_id, node))
    }

    /// `base` combined with the identity of the enclosing items.
    pub(super) fn keyed(&self, base: &str) -> Ex {
        match self.st.keys.last() {
            Some(k) => Ex::host(
                Imp::StrConcat,
                vec![Ex::str(base), Ex::local(*k, Ty::Text)],
                Ty::Text,
            ),
            None => Ex::str(base),
        }
    }

    /// The name of the view being checked, for anchor lookups.
    pub(super) fn view_name(&self) -> String {
        if let Some(owner) = &self.st.owner_view {
            return owner.clone();
        }
        self.env
            .callables
            .iter()
            .find(|c| c.own && c.id == self.st.view_id)
            .map(|c| c.name.clone())
            .unwrap_or_default()
    }

    fn ui_call(&mut self, c: &UiCall, parent: Option<LocalId>) -> LocalId {
        let env = self.env;
        let (callee, args): (&ast::Expr, &[ast::Arg]) = match &c.call.kind {
            ExprKind::Call { callee, args } => (callee, args),
            _ => (&c.call, &[]),
        };
        let span = c.call.span;
        let node = match &callee.kind {
            ExprKind::Ident(name) => {
                if let Some((id, ty)) = self.lookup(name) {
                    if ty == Ty::View && args.is_empty() && c.block.is_none() {
                        if let Some(p) = parent {
                            self.emit(Stmt::Eval(Ex::host(
                                Imp::UiChild,
                                vec![Ex::local(p, Ty::View), Ex::local(id, Ty::View)],
                                Ty::Unit,
                            )));
                        }
                        return id;
                    }
                    self.err("E060", span, format!("`{name}` is a variable of type {}; only a View variable can be placed in a view", self.show(&ty)));
                    return self.dead_node();
                }
                if self.st.ui.parent == "facts" {
                    self.err("E098", span, "`facts { }` holds facts, one `\"Label\": value` per line; put other content outside it");
                    return self.dead_node();
                }
                if let Some(msg) = removed_prim(name) {
                    self.err("E096", span, msg);
                    for a in args {
                        let _ = self.expr(&a.value, None);
                    }
                    return self.dead_node();
                }
                if let Some(p) = prims::prim(name) {
                    match self.native(p, c, args) {
                        Some(n) => n,
                        None => self.primitive(p, c, args),
                    }
                } else if let Some(v) = env.own_callable(name) {
                    let v = v.clone();
                    self.view_call(&v, c, args, span)
                } else {
                    let mut views: Vec<&str> = env
                        .callables
                        .iter()
                        .filter(|x| x.kind == CKind::View && !x.hidden())
                        .map(|x| x.name.as_str())
                        .collect();
                    views.sort_unstable();
                    self.err(
                        "E060",
                        span,
                        format!(
                            "unknown UI primitive or view `{name}`; primitives: {}; views: {}",
                            prims::prim_names(),
                            views.join(", ")
                        ),
                    );
                    return self.dead_node();
                }
            }
            ExprKind::Field { base, name } if self.names_import(base) => {
                let path = dotted(base).unwrap_or_default().join(".");
                let found = env
                    .find_import(&path)
                    .and_then(|i| env.foreign_callable(&i.app, &name.name))
                    .cloned();
                match found {
                    Some(v) => self.view_call(&v, c, args, span),
                    None => {
                        self.err(
                            "E060",
                            span,
                            format!("`{path}` declares no view `{}`", name.name),
                        );
                        return self.dead_node();
                    }
                }
            }
            _ => {
                self.err(
                    "E060",
                    span,
                    "a UI statement is a primitive such as `text(...)` or a view call",
                );
                return self.dead_node();
            }
        };
        if let Some(a) = &c.anchor {
            let key = format!("{}#{}", self.view_name(), a.name);
            let id = env.anchors.get(&key).cloned().unwrap_or_default();
            self.emit(Stmt::Eval(Ex::host(
                Imp::UiAnchor,
                vec![Ex::local(node, Ty::View), Ex::str(&id)],
                Ty::Unit,
            )));
        }
        if let Some(p) = parent {
            self.emit(Stmt::Eval(Ex::host(
                Imp::UiChild,
                vec![Ex::local(p, Ty::View), Ex::local(node, Ty::View)],
                Ty::Unit,
            )));
        }
        node
    }

    /// A node local for statements that failed to check, so lowering can go on.
    pub(super) fn dead_node(&mut self) -> LocalId {
        self.fresh(Ty::Error)
    }

    pub(super) fn new_node(&mut self, kind: &str, key: Ex) -> LocalId {
        let n = self.fresh(Ty::View);
        self.emit(Stmt::Set(
            n,
            Ex::host(Imp::UiNode, vec![Ex::str(kind), key], Ty::View),
        ));
        n
    }

    pub(super) fn primitive(&mut self, p: &Prim, c: &UiCall, args: &[ast::Arg]) -> LocalId {
        self.placement(p.name, c.call.span);
        let key = self.node_key(c);
        let n = self.new_node(p.name, key);
        let mut pos = 0usize;
        for a in args {
            if a.name.is_none() && self.is_flag(Some(p), &a.value) {
                self.flag_arg(n, &a.value);
                continue;
            }
            self.prim_arg(p, n, pos, a);
            if a.name.is_none() {
                pos += 1;
            }
        }
        if p.name == "embed" {
            self.embed_complete(c, args);
        }
        if let Some(block) = &c.block {
            self.child_block(p.name, block, n);
        }
        n
    }

    /// The statements of a primitive's block, as children of `n`.
    pub(super) fn child_block(&mut self, prim: &'static str, block: &[UiStmt], n: LocalId) {
        let saved = self.st.ui;
        self.st.ui.parent = prim;
        self.st.ui.list_row_ok = false;
        self.st.ui.in_card |= prim == "card";
        self.ui_stmts(block, n);
        self.st.ui = saved;
    }

    /// `embed` renders a block with its author as the code author, so both are required.
    fn embed_complete(&mut self, c: &UiCall, args: &[ast::Arg]) {
        let named = |want: &str| {
            args.iter()
                .any(|a| a.name.as_ref().is_some_and(|n| n.name == want))
        };
        let unnamed = args.iter().filter(|a| a.name.is_none()).count();
        if !named("source") && unnamed == 0 {
            self.err(
                "E022",
                c.call.span,
                "`embed` needs the block's source: `embed(source, author: who)`",
            );
        }
        if !named("author") {
            self.err(
                "E022",
                c.call.span,
                "`embed` needs the code author of the block, a Principal: `embed(source, author: who)`",
            );
        }
    }

    /// Whether an unnamed argument is a bare flag: `truncate`, `fill`, `fill(2)`, or one of
    /// the primitive's own (`page(full)`).
    pub(super) fn is_flag(&self, p: Option<&Prim>, e: &ast::Expr) -> bool {
        let name = match &e.kind {
            ExprKind::Ident(n) => n,
            ExprKind::Call { callee, .. } => match &callee.kind {
                ExprKind::Ident(n) if n == "fill" => n,
                _ => return false,
            },
            _ => return false,
        };
        if self.lookup(name).is_some() {
            return false;
        }
        prims::NODE_FLAGS.contains(&name.as_str())
            || p.is_some_and(|p| p.flags.contains(&name.as_str()))
    }

    pub(super) fn flag_arg(&mut self, n: LocalId, e: &ast::Expr) {
        let node = Ex::local(n, Ty::View);
        match &e.kind {
            ExprKind::Ident(name) if name == "fill" => {
                let one = Ex::new(Ek::Float(1.0), Ty::Float);
                self.emit(Stmt::Eval(Ex::host(
                    Imp::UiPropF,
                    vec![node, Ex::str("fill"), one],
                    Ty::Unit,
                )));
            }
            ExprKind::Ident(name) => {
                let on = Ex::new(Ek::Int(1), Ty::Int);
                self.emit(Stmt::Eval(Ex::host(
                    Imp::UiPropI,
                    vec![node, Ex::str(name), on],
                    Ty::Unit,
                )));
            }
            ExprKind::Call { args, .. } => {
                let [a] = args.as_slice() else {
                    self.err(
                        "E022",
                        e.span,
                        "`fill(n)` takes one weight, such as `fill(2)`",
                    );
                    return;
                };
                let w = self.float_value(&a.value);
                self.emit(Stmt::Eval(Ex::host(
                    Imp::UiPropF,
                    vec![node, Ex::str("fill"), w],
                    Ty::Unit,
                )));
            }
            _ => {}
        }
    }

    /// A number as a Float: an Int is converted, and `16/9` of two Int literals is the ratio.
    fn float_value(&mut self, e: &ast::Expr) -> Ex {
        if let ExprKind::Binary {
            op: ast::BinOp::Div,
            lhs,
            rhs,
        } = &e.kind
            && let (ExprKind::Int(a), ExprKind::Int(b)) = (&lhs.kind, &rhs.kind)
            && *b != 0
        {
            return Ex::new(Ek::Float(*a as f64 / *b as f64), Ty::Float);
        }
        let v = self.expr(e, None);
        match &v.ty {
            Ty::Float | Ty::Error => v,
            Ty::Int => Ex::new(
                Ek::Conv(crate::ir::Conv::IntToFloat, Box::new(v)),
                Ty::Float,
            ),
            other => {
                let t = self.show(other);
                self.err("E020", e.span, format!("expected a number, found {t}"));
                Ex::error()
            }
        }
    }

    fn prim_arg(&mut self, p: &Prim, n: LocalId, index: usize, a: &ast::Arg) {
        let name = match &a.name {
            Some(id) => id.name.clone(),
            None => match p.positional.get(index) {
                Some(n) => (*n).to_string(),
                None => {
                    self.err("E022", a.value.span, format!("`{}` takes {} positional argument(s): {}; name the others (`name: value`)", p.name, p.positional.len(), p.positional.join(", ")));
                    let _ = self.expr(&a.value, None);
                    return;
                }
            },
        };
        let allowed = p.positional.contains(&name.as_str())
            || p.named.contains(&name.as_str())
            || prims::NODE_ARGS.contains(&name.as_str());
        if let Some(msg) = removed_arg(p.name, &name) {
            self.err("E096", a.value.span, msg);
            return;
        }
        if !allowed {
            let props: Vec<&str> = p
                .positional
                .iter()
                .chain(p.named.iter())
                .chain(prims::NODE_ARGS.iter())
                .copied()
                .collect();
            self.err(
                "E063",
                a.value.span,
                format!(
                    "`{}` has no argument `{name}`; arguments: {}; flags: {}",
                    p.name,
                    props.join(", "),
                    prims::NODE_FLAGS
                        .iter()
                        .chain(p.flags.iter())
                        .copied()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
            let _ = self.expr(&a.value, None);
            return;
        }
        if p.name == "embed" && matches!(name.as_str(), "source" | "author" | "invocation") {
            let want = match name.as_str() {
                "source" => Ty::Text,
                "author" => Ty::Principal,
                _ => Ty::Option(Box::new(Ty::Record(crate::types::TypeKey {
                    app: crate::host::HOST_APP.to_string(),
                    name: "Invocation".to_string(),
                }))),
            };
            let v = self.expr_as(&a.value, &want);
            self.prop(Ex::local(n, Ty::View), &name, v, a.value.span);
            return;
        }
        if self.typed_arg(p.name, n, &name, &a.value) {
            return;
        }
        self.apply_arg(n, p.name, &name, &a.value);
    }

    /// Lowers one named argument of the UI call `prim` (empty for a view call) to its host
    /// call.
    pub(super) fn apply_arg(&mut self, n: LocalId, prim: &str, name: &str, value: &ast::Expr) {
        let node = Ex::local(n, Ty::View);
        if let Some(kind) = prims::arg_choice(prim, name) {
            self.choice_arg(node, kind, name, value);
        } else if name == "bind" {
            self.bind_arg(&node, value);
        } else if name == "on" || name.starts_with("on_") {
            let act = self.expr_as(value, &Ty::Action);
            let event = if name == "on" {
                "press"
            } else {
                name.trim_start_matches("on_")
            };
            self.emit(Stmt::Eval(Ex::host(
                Imp::UiOn,
                vec![node, Ex::str(event), act],
                Ty::Unit,
            )));
        } else if name == "aspect" {
            let v = self.float_value(value);
            self.emit(Stmt::Eval(Ex::host(
                Imp::UiPropF,
                vec![node, Ex::str(name), v],
                Ty::Unit,
            )));
        } else {
            let v = self.expr(value, None);
            if matches!(name, "min" | "max") && !matches!(v.ty, Ty::Length(_) | Ty::Error) {
                self.err(
                    "E063",
                    value.span,
                    format!(
                        "`{name}` is a length with a unit: write `{name}: 11rem` or `{name}: 4dp`"
                    ),
                );
                return;
            }
            self.prop(node, name, v, value.span);
        }
    }

    /// An option of a UI call that takes a built-in enum (`gap: .section`): the property holds
    /// the variant's name.
    fn choice_arg(&mut self, node: Ex, kind: &'static str, name: &str, value: &ast::Expr) {
        if let ExprKind::Ident(word) = &value.kind
            && self.lookup(word).is_none()
        {
            let c = prims::choice(kind);
            let variants = c.map_or(&[][..], |c| c.variants);
            let msg = if variants.contains(&word.as_str()) {
                format!("options are written with a dot: `{name}: .{word}`")
            } else {
                format!(
                    "`{name}` is one of {}; write it with a dot, such as `{name}: .{}`",
                    variants
                        .iter()
                        .map(|v| format!(".{v}"))
                        .collect::<Vec<_>>()
                        .join(", "),
                    variants.first().copied().unwrap_or("")
                )
            };
            self.err("E065", value.span, msg);
            return;
        }
        let v = self.expr_as(value, &Ty::Choice(kind));
        if v.ty.is_error() {
            return;
        }
        let text = self.choice_text(kind, v);
        self.emit(Stmt::Eval(Ex::host(
            Imp::UiPropH,
            vec![node, Ex::str(name), text],
            Ty::Unit,
        )));
    }

    /// A Date or a Time, or an Option of one, as the ISO text a UI prop holds (empty for
    /// `.none`), and `"date"` or `"time"`; `None` for any other value.
    pub(super) fn dated(&mut self, v: Ex) -> Result<(Ex, &'static str), Ex> {
        let kind = |t: &Ty| match t {
            Ty::Date => Some("date"),
            Ty::Time => Some("time"),
            _ => None,
        };
        if let Some(k) = kind(&v.ty) {
            return Ok((v.typed(Ty::Text), k));
        }
        let Ty::Option(inner) = &v.ty else {
            return Err(v);
        };
        let Some(k) = kind(inner) else {
            return Err(v);
        };
        Ok((self.text_or_empty(v), k))
    }

    /// An `Option<Text>` (or of a Date or Time) as its text, or the empty text for `.none`.
    pub(super) fn text_or_empty(&mut self, v: Ex) -> Ex {
        let tmp = self.fresh(v.ty.clone());
        let opt = Ex::local(tmp, v.ty.clone());
        let test = Ex::host(Imp::IsSome, vec![opt.clone()], Ty::Bool);
        let value = Ex::host(Imp::UnwrapH, vec![opt], Ty::Text);
        let pick = Ex::new(
            Ek::If(Box::new(test), Box::new(value), Box::new(Ex::str(""))),
            Ty::Text,
        );
        Ex::new(Ek::Seq(vec![Stmt::Set(tmp, v)], Box::new(pick)), Ty::Text)
    }

    /// Sets the prop `name`. A Date or a Time is its ISO text, and the sibling prop
    /// `<name>:as` says which (`"date"` or `"time"`), for the host to format it.
    pub(super) fn prop(&mut self, node: Ex, name: &str, v: Ex, span: Span) {
        let v = match self.dated(v) {
            Ok((text, kind)) => {
                self.emit(Stmt::Eval(Ex::host(
                    Imp::UiPropH,
                    vec![node.clone(), Ex::str(name), text],
                    Ty::Unit,
                )));
                self.emit(Stmt::Eval(Ex::host(
                    Imp::UiPropH,
                    vec![node, Ex::str(&format!("{name}:as")), Ex::str(kind)],
                    Ty::Unit,
                )));
                return;
            }
            Err(v) => v,
        };
        match &v.ty {
            Ty::Error => {}
            Ty::Length(u) => {
                let prop = format!("{name}.{}", u.text());
                self.emit(Stmt::Eval(Ex::host(
                    Imp::UiPropF,
                    vec![node, Ex::str(&prop), v],
                    Ty::Unit,
                )));
            }
            Ty::Action | Ty::View | Ty::Unit | Ty::Choice(_) | Ty::Enum(_) | Ty::Result(..) => {
                let t = self.show(&v.ty);
                self.err("E063", span, format!("the property `{name}` cannot hold {t}; events are written `on: action(...)`"));
            }
            ty => {
                let (imp, c) = store_op(ty, Imp::UiPropI, Imp::UiPropF, Imp::UiPropH);
                let val = conv(c, v, Ty::Int);
                self.emit(Stmt::Eval(Ex::host(
                    imp,
                    vec![node, Ex::str(name), val],
                    Ty::Unit,
                )));
            }
        }
    }

    pub(super) fn bind_arg(&mut self, node: &Ex, value: &ast::Expr) {
        let _ = self.bind_typed(node, value, &[Ty::Int, Ty::Float, Ty::Bool, Ty::Text]);
    }

    /// `bind:` to a field or view state whose type is one of `allowed`.
    pub(super) fn bind_typed(
        &mut self,
        node: &Ex,
        value: &ast::Expr,
        allowed: &[Ty],
    ) -> Option<Ty> {
        let names = allowed
            .iter()
            .map(|t| self.show(t))
            .collect::<Vec<_>>()
            .join(", ");
        if let ExprKind::Ident(n) = &value.kind
            && let Some(state) = self.state_named(n)
        {
            if !allowed.contains(&state.ty) {
                let t = self.show(&state.ty);
                self.err("E062", value.span, format!("this binds view state of type {names}; `{n}` is {t}; change it with `on: set({n}, value)`"));
                return None;
            }
            let layer = Ex::new(Ek::I32(state.layer.code()), Ty::Bool);
            self.emit(Stmt::Eval(Ex::host(
                Imp::UiBindState,
                vec![node.clone(), layer, Ex::str(&state.id)],
                Ty::Unit,
            )));
            return Some(state.ty);
        }
        let ExprKind::Field { base, name } = &value.kind else {
            self.err(
                "E062",
                value.span,
                "`bind:` takes a field of a resource, such as `bind: item.done`, or a `window` or `lens` declaration of the view",
            );
            let _ = self.expr(value, None);
            return None;
        };
        let b = self.expr(base, None);
        let Ty::Record(key) = b.ty.clone() else {
            if !b.ty.is_error() {
                self.err(
                    "E062",
                    value.span,
                    "`bind:` takes a field of a resource, such as `bind: item.done`",
                );
            }
            return None;
        };
        let env = self.env;
        let Some(f) = env.type_info(&key).and_then(|t| t.field(&name.name)) else {
            let names = env
                .type_info(&key)
                .map(TypeInfo::field_names)
                .unwrap_or_default();
            self.err(
                "E021",
                name.span,
                format!(
                    "`{}` has no field `{}`; fields are: {names}",
                    key.name, name.name
                ),
            );
            return None;
        };
        if f.ty.is_container() || !allowed.contains(&f.ty) {
            let t = self.show(&f.ty);
            self.err(
                "E062",
                value.span,
                format!("this binds a field of type {names}; `{}` is {t}", f.name),
            );
            return None;
        }
        self.emit(Stmt::Eval(Ex::host(
            Imp::UiBind,
            vec![node.clone(), b, Ex::str(&f.id)],
            Ty::Unit,
        )));
        Some(f.ty.clone())
    }

    /// A call of a view: the caller wraps the result in a node keyed by the call site, so two
    /// calls of the same view never share keys.
    fn view_call(&mut self, v: &Callable, c: &UiCall, args: &[ast::Arg], span: Span) -> LocalId {
        if v.kind != CKind::View {
            self.err("E061", span, format!("`{}` is {} {}, not a view or a UI primitive; an action goes in an event: `on: {}(...)`", v.name, super::expr::article(v.kind.word()), v.kind.word(), v.name));
            return self.dead_node();
        }
        if c.block.is_some() {
            self.err(
                "E063",
                span,
                format!(
                    "`{}` is a view and takes no child block; pass children as a View parameter",
                    v.name
                ),
            );
        }
        let sig = v.signature(&self.env.app);
        // Sizing and placement arguments (`fill`, `min:`, `at:`, ...) belong to the node the
        // call is wrapped in; the rest are the view's parameters.
        let is_node_arg = |this: &Self, a: &ast::Arg| match &a.name {
            Some(n) => {
                prims::NODE_ARGS.contains(&n.name.as_str())
                    && !v.params.iter().any(|p| p.name == n.name)
            }
            None => this.is_flag(None, &a.value),
        };
        let (node_args, param_args): (Vec<&ast::Arg>, Vec<&ast::Arg>) =
            args.iter().partition(|a| is_node_arg(self, a));
        let param_args: Vec<ast::Arg> = param_args.into_iter().cloned().collect();
        let bound = self.bind_args(&v.name, &v.params, &param_args, span, &sig);
        let key = self.node_key(c);
        let node = self.view_node(v, bound, key, span);
        for a in node_args {
            match &a.name {
                Some(n) => self.apply_arg(node, "", &n.name, &a.value),
                None => self.flag_arg(node, &a.value),
            }
        }
        node
    }

    /// A view call is its own boundary: a node of kind `view:<app>:<view id>` whose arguments
    /// are properties named `arg.<name>`, which the host renders by calling the view, and renders
    /// again only when what that view read changed or its arguments did. A view that takes a
    /// `View` (content built by the caller) is inlined instead: a node handle does not outlive
    /// the caller's run.
    pub(super) fn view_node(
        &mut self,
        v: &Callable,
        bound: Vec<Ex>,
        key: Ex,
        _span: Span,
    ) -> LocalId {
        let takes_content = v.params.iter().any(|p| matches!(p.ty, Ty::View));
        if let (true, Some(f)) = (v.own, v.func) {
            self.st.facts.views.insert(f);
            if takes_content {
                let wrapper = self.new_node("fragment", key);
                let inner = Ex::new(Ek::Call(f, bound), Ty::View);
                self.emit(Stmt::Eval(Ex::host(
                    Imp::UiChild,
                    vec![Ex::local(wrapper, Ty::View), inner],
                    Ty::Unit,
                )));
                return wrapper;
            }
        }
        let n = self.new_node(&format!("view:{}:{}", v.app, v.id), key);
        for (p, val) in v.params.iter().zip(bound) {
            let prop = format!("arg.{}", p.name);
            self.arg_prop(Ex::local(n, Ty::View), &prop, val);
        }
        n
    }

    /// An argument of a view call, as a property of its node: any value a parameter takes.
    pub(super) fn arg_prop(&mut self, node: Ex, name: &str, v: Ex) {
        let imp = match v.ty.repr() {
            None => return,
            Some(crate::types::Vt::I64) => Imp::UiPropI,
            Some(crate::types::Vt::F64) => Imp::UiPropF,
            Some(crate::types::Vt::I32) if v.ty == Ty::Bool => Imp::UiPropI,
            Some(crate::types::Vt::I32) => Imp::UiPropH,
        };
        let v = if v.ty == Ty::Bool {
            conv(Some(Conv::BoolToInt), v, Ty::Int)
        } else {
            v
        };
        self.emit(Stmt::Eval(Ex::host(
            imp,
            vec![node, Ex::str(name), v],
            Ty::Unit,
        )));
    }
}

/// What replaced a primitive that is gone, for its error.
fn removed_prim(name: &str) -> Option<&'static str> {
    Some(match name {
        "heading" => {
            "`heading` is gone: the window's title is the root resource's `(title)` field, and the platform draws it in the header. Group content with `section(\"Title\") { }`, or write `text(x, role: .title)`"
        }
        "list_row" => {
            "`list_row` is now `row(title, secondary: ...)`, inside a list's item block (`list(xs) { x -> row(x.title) }`) or as a `row view`"
        }
        _ => return None,
    })
}

/// What replaced an argument that is gone, for its error.
pub(super) fn removed_arg(prim: &str, arg: &str) -> Option<&'static str> {
    Some(match (prim, arg) {
        ("text", "style") => {
            "text has a role, not a style: `text(x, role: .title)`; the roles are .body, .title, .caption and .numeric"
        }
        ("button", "style" | "size") => {
            "a button takes its look from its action: mark the action `primary action` or `destructive action` and write `button(act(x))`; there is no `style:` or `size:`"
        }
        _ => return None,
    })
}
