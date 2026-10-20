//! View bodies: UI statements lowered to `ui_*` host calls (implementation.md section 4.4).

use super::expr::dotted;
use super::{Checker, Ex, conv, store_op};
use crate::abi::Imp;
use crate::ast::{self, ExprKind, UiCall, UiKind, UiStmt};
use crate::diag::Span;
use crate::ir::{Ek, LocalId, Stmt};
use crate::prims::{self, Prim};
use crate::resolve::{CKind, Callable, TypeInfo};
use crate::types::Ty;

impl Checker<'_> {
    /// Lowers a whole view body and returns its root node.
    pub(super) fn view_body(&mut self, body: &[UiStmt]) {
        let root = match body {
            [
                UiStmt {
                    kind: UiKind::Call(c),
                    ..
                },
            ] => self.ui_call(c, None),
            _ => {
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

    fn ui_stmts(&mut self, stmts: &[UiStmt], parent: LocalId) {
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
    fn node_key(&self, c: &UiCall) -> Ex {
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
        match self.st.keys.last() {
            Some(k) => Ex::host(
                Imp::StrConcat,
                vec![Ex::str(&base), Ex::local(*k, Ty::Text)],
                Ty::Text,
            ),
            None => Ex::str(&base),
        }
    }

    /// The name of the view being checked, for anchor lookups.
    fn view_name(&self) -> String {
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
                if let Some(p) = prims::prim(name) {
                    self.primitive(p, c, args)
                } else if let Some(v) = env.own_callable(name) {
                    let v = v.clone();
                    self.view_call(&v, c, args, span)
                } else {
                    let mut views: Vec<&str> = env
                        .callables
                        .iter()
                        .filter(|x| x.kind == CKind::View)
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
    fn dead_node(&mut self) -> LocalId {
        self.fresh(Ty::Error)
    }

    fn new_node(&mut self, kind: &str, key: Ex) -> LocalId {
        let n = self.fresh(Ty::View);
        self.emit(Stmt::Set(
            n,
            Ex::host(Imp::UiNode, vec![Ex::str(kind), key], Ty::View),
        ));
        n
    }

    fn primitive(&mut self, p: &Prim, c: &UiCall, args: &[ast::Arg]) -> LocalId {
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
            self.ui_stmts(block, n);
        }
        n
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
    fn is_flag(&self, p: Option<&Prim>, e: &ast::Expr) -> bool {
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

    fn flag_arg(&mut self, n: LocalId, e: &ast::Expr) {
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
        self.apply_arg(n, &name, &a.value);
    }

    /// Lowers one named argument of a UI call to its host call.
    fn apply_arg(&mut self, n: LocalId, name: &str, value: &ast::Expr) {
        let node = Ex::local(n, Ty::View);
        if name == "bind" {
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
        } else if let Some(token) = self.token_of(value) {
            self.emit(Stmt::Eval(Ex::host(
                Imp::UiPropH,
                vec![node, Ex::str(name), Ex::str(&token)],
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

    /// A bare layout token (`related`, `center`, `bottom_trailing`) given as an argument value.
    fn token_of(&self, e: &ast::Expr) -> Option<String> {
        match &e.kind {
            ExprKind::Ident(t)
                if self.lookup(t).is_none() && prims::TOKENS.contains(&t.as_str()) =>
            {
                Some(t.clone())
            }
            _ => None,
        }
    }

    fn prop(&mut self, node: Ex, name: &str, v: Ex, span: Span) {
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
            Ty::Action | Ty::View | Ty::Unit | Ty::Enum(_) => {
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

    fn bind_arg(&mut self, node: &Ex, value: &ast::Expr) {
        let ExprKind::Field { base, name } = &value.kind else {
            self.err(
                "E062",
                value.span,
                "`bind:` takes a field of a resource, such as `bind: item.done`",
            );
            let _ = self.expr(value, None);
            return;
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
            return;
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
            return;
        };
        if f.ty.is_container() || !matches!(f.ty, Ty::Int | Ty::Float | Ty::Bool | Ty::Text) {
            let t = self.show(&f.ty);
            self.err(
                "E062",
                value.span,
                format!(
                    "`bind:` edits a field of type Int, Float, Bool or Text; `{}` is {t}",
                    f.name
                ),
            );
            return;
        }
        self.emit(Stmt::Eval(Ex::host(
            Imp::UiBind,
            vec![node.clone(), b, Ex::str(&f.id)],
            Ty::Unit,
        )));
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
                Some(n) => self.apply_arg(node, &n.name, &a.value),
                None => self.flag_arg(node, &a.value),
            }
        }
        node
    }

    fn view_node(&mut self, v: &Callable, bound: Vec<Ex>, key: Ex, span: Span) -> LocalId {
        if let (true, Some(f)) = (v.own, v.func) {
            let wrapper = self.new_node("fragment", key);
            self.st.facts.views.insert(f);
            let inner = Ex::new(Ek::Call(f, bound), Ty::View);
            self.emit(Stmt::Eval(Ex::host(
                Imp::UiChild,
                vec![Ex::local(wrapper, Ty::View), inner],
                Ty::Unit,
            )));
            return wrapper;
        }
        // A view of another package is applied by the host: a node of kind
        // `view:<app>:<view id>` whose parameters are properties named `arg.<name>`.
        let n = self.new_node(&format!("view:{}:{}", v.app, v.id), key);
        for (p, val) in v.params.iter().zip(bound) {
            let prop = format!("arg.{}", p.name);
            self.prop(Ex::local(n, Ty::View), &prop, val, span);
        }
        n
    }
}
