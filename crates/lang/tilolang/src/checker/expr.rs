//! Expressions: literals, operators, fields, strings and coercions.

use super::{Checker, Ctx, Ex, conv, load_op, store_op};
use crate::abi::Imp;
use crate::ast::{self, BinOp, ExprKind, StrPart, UnOp};
use crate::diag::Span;
use crate::ir::{self, Conv, Ek};
use crate::types::Ty;

fn is_none_lit(e: &Ex) -> bool {
    matches!(&e.kind, Ek::Host(Imp::NoneV, _))
}

impl Checker<'_> {
    /// Checks `e` against an expected type and coerces the result.
    pub(super) fn expr_as(&mut self, e: &ast::Expr, want: &Ty) -> Ex {
        let ex = self.expr(e, Some(want));
        self.coerce(ex, want, e.span)
    }

    /// Wraps a value in `some` when an Option is expected, and reports a mismatch otherwise.
    pub(super) fn coerce(&mut self, ex: Ex, want: &Ty, span: Span) -> Ex {
        if ex.ty.is_error() || want.is_error() || ex.ty.same(want) {
            return ex;
        }
        if let Ty::Option(inner) = want {
            if ex.ty.same(inner) {
                return Self::some(ex, want);
            }
        }
        let (w, f) = (self.show(want), self.show(&ex.ty));
        let hint = match (want, &ex.ty) {
            (Ty::Float, Ty::Int) => "; convert with `.to_float()`",
            (Ty::Int, Ty::Float) => "; convert with `.to_int()`",
            _ => "",
        };
        self.err("E020", span, format!("expected {w}, found {f}{hint}"));
        Ex::error()
    }

    pub(super) fn some(ex: Ex, ty: &Ty) -> Ex {
        let inner = ex.ty.clone();
        let (imp, c) = store_op(&inner, Imp::SomeI, Imp::SomeF, Imp::SomeH);
        Ex::host(imp, vec![conv(c, ex, Ty::Int)], ty.clone())
    }

    /// Reads the value out of an Option handle.
    pub(super) fn unwrap_opt(h: Ex, inner: &Ty) -> Ex {
        let (imp, c) = load_op(inner, Imp::UnwrapI, Imp::UnwrapF, Imp::UnwrapH);
        let raw = Ex::host(
            imp,
            vec![h],
            if c.is_some() { Ty::Int } else { inner.clone() },
        );
        conv(c, raw, inner.clone())
    }

    /// Turns a value into a host handle: scalars are boxed like an `Option`.
    pub(super) fn handle_of(ex: Ex) -> Ex {
        if ex.ty.is_scalar() {
            let t = Ty::Option(Box::new(ex.ty.clone()));
            Self::some(ex, &t)
        } else {
            ex
        }
    }

    pub(super) fn expr(&mut self, e: &ast::Expr, exp: Option<&Ty>) -> Ex {
        match &e.kind {
            ExprKind::Int(v) => Ex::int(*v),
            ExprKind::Float(v) => Ex::new(Ek::Float(*v), Ty::Float),
            ExprKind::Bool(b) => Ex::new(Ek::Bool(*b), Ty::Bool),
            ExprKind::None => match exp {
                Some(t @ Ty::Option(_)) => Ex::host(Imp::NoneV, Vec::new(), t.clone()),
                Some(Ty::Error) => Ex::error(),
                _ => {
                    self.err("E025", e.span, "cannot tell which Option type `none` is here; annotate the variable (`let x: Option<Int> = none`) or use it where an Option is expected");
                    Ex::error()
                }
            },
            ExprKind::Str(parts) => self.string(parts),
            ExprKind::Ref(r) => self.ref_lit(r, e.span),
            ExprKind::Ident(name) => self.ident(name, e.span),
            ExprKind::Tuple(items) => self.tuple(items, exp),
            ExprKind::List(items) => self.list(items, exp, e.span),
            ExprKind::IfExpr { cond, then, els } => {
                let c = self.expr_as(cond, &Ty::Bool);
                let t = self.expr(then, exp);
                let want = exp
                    .filter(|w| !matches!(w, Ty::Error))
                    .cloned()
                    .unwrap_or_else(|| t.ty.clone());
                let t = self.coerce(t, &want, then.span);
                let f = self.expr_as(els, &want);
                Ex::new(Ek::If(Box::new(c), Box::new(t), Box::new(f)), want)
            }
            ExprKind::Lambda { .. } => {
                self.err("E050", e.span, "a lambda `x -> ...` can only be a direct argument of map, filter, any, all, find, count or sort_by; it cannot be stored or passed around");
                Ex::error()
            }
            ExprKind::Unary { op, operand } => self.unary(*op, operand, e.span),
            ExprKind::Binary { op, lhs, rhs } => self.binary(*op, lhs, rhs, e.span),
            ExprKind::Field { base, name } => {
                if self.names_import(base) {
                    self.err(
                        "E026",
                        e.span,
                        format!(
                            "`{}` is declared by an imported package; call it as `{}(...)`",
                            name.name, name.name
                        ),
                    );
                    return Ex::error();
                }
                let b = self.expr(base, None);
                self.field_read(b, name)
            }
            ExprKind::Call { callee, args } => self.call(callee, args, e.span),
            ExprKind::Do(inner) => {
                if self.st.ctx != Ctx::Flow {
                    let who = self.st.ctx.word();
                    self.err("E044", e.span, format!("`do` invokes an action and only a flow may do that; {who} builds a reference instead: write the action call without `do`"));
                    return Ex::error();
                }
                if !matches!(inner.kind, ExprKind::Call { .. }) {
                    self.err(
                        "E044",
                        e.span,
                        "`do` must be followed by an action call, such as `do archive(m)`",
                    );
                    return Ex::error();
                }
                self.expr(inner, exp)
            }
        }
    }

    /// Whether `e` is a dotted path naming an imported package (and not a variable).
    pub(super) fn names_import(&self, e: &ast::Expr) -> bool {
        let Some(path) = dotted(e) else { return false };
        if path.first().is_some_and(|f| self.lookup(f).is_some()) {
            return false;
        }
        self.env.find_import(&path.join(".")).is_some()
    }

    fn ident(&mut self, name: &str, span: Span) -> Ex {
        if let Some((id, ty)) = self.lookup(name) {
            return Ex::local(id, ty);
        }
        if let Some(c) = self.env.own_callable(name) {
            self.err(
                "E026",
                span,
                format!(
                    "`{name}` is {} {}, not a value; apply it: `{}`",
                    article(c.kind.word()),
                    c.kind.word(),
                    c.signature(&self.env.app)
                ),
            );
        } else if self.env.own_type(name).is_some() {
            self.err(
                "E026",
                span,
                format!("`{name}` is a type; construct a value with `{name}(field: value)`"),
            );
        } else if self.env.find_import(name).is_some() {
            self.err(
                "E026",
                span,
                format!("`{name}` is an imported package; use `{name}.<name>`"),
            );
        } else {
            let known = self.locals_in_scope();
            self.err(
                "E014",
                span,
                format!("unknown name `{name}`; variables in scope: {known}"),
            );
        }
        Ex::error()
    }

    fn string(&mut self, parts: &[StrPart]) -> Ex {
        let mut acc: Option<Ex> = None;
        for part in parts {
            let piece = match part {
                StrPart::Lit(s) => Ex::str(s),
                StrPart::Interp(x) => {
                    let v = self.expr(x, None);
                    self.to_text(v, x.span)
                }
            };
            acc = Some(match acc {
                None => piece,
                Some(a) => Ex::host(Imp::StrConcat, vec![a, piece], Ty::Text),
            });
        }
        acc.unwrap_or_else(|| Ex::str(""))
    }

    pub(super) fn to_text(&mut self, v: Ex, span: Span) -> Ex {
        match &v.ty {
            Ty::Text | Ty::Error => v,
            Ty::Int => Ex::host(Imp::IntToStr, vec![v], Ty::Text),
            Ty::Float => Ex::host(Imp::FloatToStr, vec![v], Ty::Text),
            Ty::Bool => Ex::new(
                Ek::If(
                    Box::new(v),
                    Box::new(Ex::str("true")),
                    Box::new(Ex::str("false")),
                ),
                Ty::Text,
            ),
            Ty::Record(_) => Ex::host(Imp::RefId, vec![v], Ty::Text),
            other => {
                let t = self.show(other);
                self.err("E020", span, format!("a value of type {t} cannot be shown as text; interpolate Text, Int, Float, Bool or a record"));
                Ex::error()
            }
        }
    }

    fn ref_lit(&mut self, r: &ast::RefLit, span: Span) -> Ex {
        if r.version.is_some() {
            self.err("E070", span, "pinned references (`@kind:id@version`) are not supported in v1; write the live reference `@kind:id`");
            return Ex::error();
        }
        let env = self.env;
        let Some(info) = env.resolve_ref(&r.app, &r.ty) else {
            let known: Vec<String> = env
                .types
                .iter()
                .map(|t| format!("{}.{}", t.key.app, t.key.name))
                .collect();
            self.err(
                "E071",
                span,
                format!(
                    "no record type matches `@{}{}{}:...`; known types: {}",
                    r.app,
                    if r.app.is_empty() { "" } else { "." },
                    r.ty,
                    known.join(", ")
                ),
            );
            return Ex::error();
        };
        Ex::host(
            Imp::RefLit,
            vec![Ex::str(&info.key.app), Ex::str(&info.id), Ex::str(&r.id)],
            Ty::Record(info.key.clone()),
        )
    }

    fn tuple(&mut self, items: &[ast::Expr], exp: Option<&Ty>) -> Ex {
        let hints: Vec<Option<Ty>> = match exp {
            Some(Ty::Tuple(ts)) if ts.len() == items.len() => {
                ts.iter().cloned().map(Some).collect()
            }
            _ => vec![None; items.len()],
        };
        let tmp = self.fresh(Ty::Error);
        let mut stmts = vec![ir::Stmt::Set(
            tmp,
            Ex::host(
                Imp::TupleNew,
                vec![Ex::new(Ek::I32(items.len() as i32), Ty::Bool)],
                Ty::Text,
            ),
        )];
        let mut tys = Vec::new();
        for (i, (item, hint)) in items.iter().zip(hints).enumerate() {
            let v = match &hint {
                Some(h) => self.expr_as(item, h),
                None => self.expr(item, None),
            };
            tys.push(v.ty.clone());
            let (imp, c) = store_op(&v.ty, Imp::TupleSetI, Imp::TupleSetF, Imp::TupleSetH);
            let idx = Ex::new(Ek::I32(i as i32), Ty::Bool);
            stmts.push(ir::Stmt::Eval(Ex::host(
                imp,
                vec![Ex::local(tmp, Ty::Text), idx, conv(c, v, Ty::Int)],
                Ty::Unit,
            )));
        }
        let ty = Ty::Tuple(tys);
        if let Some(l) = self.st.locals.get_mut(tmp as usize) {
            *l = ty.clone();
        }
        Ex::new(Ek::Seq(stmts, Box::new(Ex::local(tmp, ty.clone()))), ty)
    }

    fn list(&mut self, items: &[ast::Expr], exp: Option<&Ty>, span: Span) -> Ex {
        let hint = match exp {
            Some(Ty::List(t)) => Some((**t).clone()),
            _ => None,
        };
        let mut values: Vec<Ex> = Vec::new();
        let mut elem = hint;
        for item in items {
            let v = match &elem {
                Some(t) => self.expr_as(item, t),
                None => self.expr(item, None),
            };
            if elem.is_none() {
                elem = Some(v.ty.clone());
            }
            values.push(v);
        }
        let Some(elem) = elem else {
            self.err("E025", span, "cannot tell what an empty list holds; annotate the variable (`let xs: List<Int> = []`)");
            return Ex::error();
        };
        let ty = Ty::List(Box::new(elem));
        let tmp = self.fresh(ty.clone());
        let mut stmts = vec![ir::Stmt::Set(
            tmp,
            Ex::host(Imp::ListNew, Vec::new(), ty.clone()),
        )];
        for v in values {
            stmts.push(ir::Stmt::Eval(Self::push_item(
                Ex::local(tmp, ty.clone()),
                v,
            )));
        }
        Ex::new(Ek::Seq(stmts, Box::new(Ex::local(tmp, ty.clone()))), ty)
    }

    /// `list_push_*` of one item.
    pub(super) fn push_item(list: Ex, item: Ex) -> Ex {
        let (imp, c) = store_op(&item.ty, Imp::ListPushI, Imp::ListPushF, Imp::ListPushH);
        Ex::host(imp, vec![list, conv(c, item, Ty::Int)], Ty::Unit)
    }

    fn unary(&mut self, op: UnOp, operand: &ast::Expr, span: Span) -> Ex {
        match op {
            UnOp::Not => {
                let v = self.expr_as(operand, &Ty::Bool);
                Ex::new(Ek::Not(Box::new(v)), Ty::Bool)
            }
            UnOp::Neg => {
                let v = self.expr(operand, None);
                match &v.ty {
                    Ty::Int | Ty::Float => {
                        let ty = v.ty.clone();
                        Ex::new(Ek::Neg(Box::new(v)), ty)
                    }
                    Ty::Error => v,
                    other => {
                        let t = self.show(other);
                        self.err(
                            "E024",
                            span,
                            format!("`-` negates an Int or a Float, not {t}"),
                        );
                        Ex::error()
                    }
                }
            }
        }
    }

    fn binary(&mut self, op: BinOp, lhs: &ast::Expr, rhs: &ast::Expr, span: Span) -> Ex {
        if matches!(op, BinOp::And | BinOp::Or) {
            let l = self.expr_as(lhs, &Ty::Bool);
            let r = self.expr_as(rhs, &Ty::Bool);
            let t = Ex::new(Ek::Bool(true), Ty::Bool);
            let f = Ex::new(Ek::Bool(false), Ty::Bool);
            let (then, els) = if op == BinOp::And { (r, f) } else { (t, r) };
            return Ex::new(Ek::If(Box::new(l), Box::new(then), Box::new(els)), Ty::Bool);
        }
        let l = self.expr(lhs, None);
        let hint = l.ty.clone();
        let r = self.expr(rhs, Some(&hint));
        if l.ty.is_error() || r.ty.is_error() {
            return Ex::error();
        }
        match op {
            BinOp::Eq | BinOp::Ne => self.equality(op == BinOp::Eq, l, r, span),
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => self.ordering(op, l, r, span),
            _ => self.arithmetic(op, l, r, span),
        }
    }

    fn equality(&mut self, eq: bool, l: Ex, r: Ex, span: Span) -> Ex {
        let negate = |e: Ex| Ex::new(Ek::Not(Box::new(e)), Ty::Bool);
        if matches!(l.ty, Ty::Option(_)) && is_none_lit(&r) {
            let some = Ex::host(Imp::IsSome, vec![l], Ty::Bool);
            return if eq { negate(some) } else { some };
        }
        let r = self.coerce(r, &l.ty.clone(), span);
        let (lt, base) = (l.ty.clone(), if eq { ir::BinOp::Eq } else { ir::BinOp::Ne });
        match lt {
            Ty::Int | Ty::Float | Ty::Bool => {
                Ex::new(Ek::Bin(base, Box::new(l), Box::new(r)), Ty::Bool)
            }
            Ty::Text => {
                let e = Ex::host(Imp::StrEq, vec![l, r], Ty::Bool);
                if eq { e } else { negate(e) }
            }
            Ty::Record(_) => {
                let e = Ex::host(Imp::RefEq, vec![l, r], Ty::Bool);
                if eq { e } else { negate(e) }
            }
            Ty::Error => Ex::error(),
            other => {
                let t = self.show(&other);
                self.err("E024", span, format!("{t} values cannot be compared with `==`; compare Int, Float, Bool, Text or records (for an Option, compare with `none` or unwrap it)"));
                Ex::error()
            }
        }
    }

    fn ordering(&mut self, op: BinOp, l: Ex, r: Ex, span: Span) -> Ex {
        let bop = match op {
            BinOp::Lt => ir::BinOp::Lt,
            BinOp::Le => ir::BinOp::Le,
            BinOp::Gt => ir::BinOp::Gt,
            _ => ir::BinOp::Ge,
        };
        let r = self.coerce(r, &l.ty.clone(), span);
        match l.ty.clone() {
            Ty::Int | Ty::Float => Ex::new(Ek::Bin(bop, Box::new(l), Box::new(r)), Ty::Bool),
            Ty::Text => {
                let cmp = Ex::host(Imp::StrCmp, vec![l, r], Ty::Bool);
                let cmp = Ex::new(Ek::Conv(Conv::SignExt, Box::new(cmp)), Ty::Int);
                Ex::new(Ek::Bin(bop, Box::new(cmp), Box::new(Ex::int(0))), Ty::Bool)
            }
            other => {
                let t = self.show(&other);
                self.err(
                    "E024",
                    span,
                    format!("`{}` orders Int, Float or Text, not {t}", op.text()),
                );
                Ex::error()
            }
        }
    }

    fn arithmetic(&mut self, op: BinOp, l: Ex, r: Ex, span: Span) -> Ex {
        let bop = match op {
            BinOp::Add => ir::BinOp::Add,
            BinOp::Sub => ir::BinOp::Sub,
            BinOp::Mul => ir::BinOp::Mul,
            BinOp::Div => ir::BinOp::Div,
            _ => ir::BinOp::Rem,
        };
        match (&l.ty, &r.ty) {
            (Ty::Int, Ty::Int) => Ex::new(Ek::Bin(bop, Box::new(l), Box::new(r)), Ty::Int),
            (Ty::Float, Ty::Float) if op != BinOp::Rem => {
                Ex::new(Ek::Bin(bop, Box::new(l), Box::new(r)), Ty::Float)
            }
            (Ty::Text, Ty::Text) if op == BinOp::Add => {
                Ex::host(Imp::StrConcat, vec![l, r], Ty::Text)
            }
            _ => {
                let (a, b) = (self.show(&l.ty), self.show(&r.ty));
                let hint = match (&l.ty, &r.ty) {
                    (Ty::Int, Ty::Float) | (Ty::Float, Ty::Int) => {
                        "; convert the Int with `.to_float()` first"
                    }
                    _ => "",
                };
                self.err("E024", span, format!("`{}` needs two Int or two Float operands (or two Text for `+`); found {a} and {b}{hint}", op.text()));
                Ex::error()
            }
        }
    }

    /// Reads field `name` of the record value `b`.
    pub(super) fn field_read(&mut self, b: Ex, name: &ast::Ident) -> Ex {
        match b.ty.clone() {
            Ty::Error => Ex::error(),
            Ty::Record(key) => {
                let env = self.env;
                let Some(info) = env.type_info(&key) else {
                    return Ex::error();
                };
                let Some(f) = info.field(&name.name) else {
                    self.err(
                        "E021",
                        name.span,
                        format!(
                            "`{}` has no field `{}`; fields are: {}",
                            key.name,
                            name.name,
                            info.field_names()
                        ),
                    );
                    return Ex::error();
                };
                if matches!(f.ty, Ty::Map(_)) {
                    self.err("E080", name.span, format!("reading the Map field `{}` is not supported in v1; change it with `.put` and `.remove`", f.name));
                    return Ex::error();
                }
                let (imp, c) = load_op(&f.ty, Imp::ResGetI, Imp::ResGetF, Imp::ResGetH);
                let raw_ty = if c.is_some() { Ty::Int } else { f.ty.clone() };
                conv(
                    c,
                    Ex::host(imp, vec![b, Ex::str(&f.id)], raw_ty),
                    f.ty.clone(),
                )
            }
            other => {
                let t = self.show(&other);
                self.err("E021", name.span, format!("`.{}` needs a record, but the value has type {t}, which has no fields; methods are called `.{}(...)`", name.name, name.name));
                Ex::error()
            }
        }
    }
}

/// `a.b.c` as `["a", "b", "c"]`.
pub(super) fn dotted(e: &ast::Expr) -> Option<Vec<String>> {
    match &e.kind {
        ExprKind::Ident(n) => Some(vec![n.clone()]),
        ExprKind::Field { base, name } => {
            let mut p = dotted(base)?;
            p.push(name.name.clone());
            Some(p)
        }
        _ => None,
    }
}

pub(super) fn article(word: &str) -> &'static str {
    if word.starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an"
    } else {
        "a"
    }
}
