//! Options taken apart without a `match`: `if let`, `x?.field`, `x ?? fallback`, and key paths
//! (`.field` where a lambda would only read that field). Each means the same in an action, a
//! function and a view.

use std::collections::HashMap;

use super::{Checker, Ex};
use crate::abi::Imp;
use crate::ast::{self, ExprKind, LetBind};
use crate::diag::Span;
use crate::ir::{Ek, Stmt};
use crate::types::Ty;

/// The name a key path's lambda binds its item to; no source can write it.
const KEY_ITEM: &str = "$item";

/// `.field` written where a lambda goes, as the lambda `x -> x.field`; a lambda as itself.
pub(super) fn lambda_of(e: &ast::Expr) -> Option<(ast::Ident, ast::Expr)> {
    match &e.kind {
        ExprKind::Lambda { param, body } => Some((param.clone(), (**body).clone())),
        ExprKind::Variant { name, args: None } => {
            let item = ast::Ident {
                name: KEY_ITEM.to_owned(),
                span: e.span,
            };
            let body = ast::Expr {
                kind: ExprKind::Field {
                    base: Box::new(ast::Expr {
                        kind: ExprKind::Ident(KEY_ITEM.to_owned()),
                        span: e.span,
                    }),
                    name: name.clone(),
                },
                span: e.span,
            };
            Some((item, body))
        }
        _ => None,
    }
}

/// What an `if let` lowered to: statements that set `cond` (true when every Option had a
/// value, binding each), and its two branches.
pub(super) struct IfLet<T> {
    pub prelude: Vec<Stmt>,
    pub cond: Ex,
    /// `None` when an Option is known to be absent where the code is expanded: the branch is
    /// never taken, and not checked.
    pub then: Option<T>,
    pub els: T,
}

impl Checker<'_> {
    /// Checks `if let a = x, b = y`: each value an Option, each later one seeing the names before
    /// it, then the branches. `then` runs with the names bound, `els` without them.
    pub(super) fn if_let<T>(
        &mut self,
        binds: &[LetBind],
        then: impl FnOnce(&mut Self) -> T,
        els: impl FnOnce(&mut Self) -> T,
    ) -> IfLet<T> {
        let ok = self.fresh(Ty::Bool);
        let saved = (self.st.macros.clone(), self.st.states.clone());
        self.st.scopes.push(HashMap::new());
        let scope = self.st.scopes.len() - 1;
        let mut levels = Vec::new();
        let mut absent = false;
        for b in binds {
            // An optional parameter of a template is known where it expands.
            if let Some(held) = self.static_option(&b.value) {
                match held.given() {
                    Some(m) => self.bind_static(&b.name.name, m),
                    None => absent = true,
                }
                continue;
            }
            self.push_sink();
            let v = self.expr(&b.value, None);
            let inner = match &v.ty {
                Ty::Option(t) => (**t).clone(),
                Ty::Error => Ty::Error,
                other => {
                    let t = self.show(other);
                    self.err(
                        "E106",
                        b.value.span,
                        format!(
                            "`if let` takes an Option, and this is {t}, which always has a value; write `let {} = ...` before the `if`",
                            b.name.name
                        ),
                    );
                    Ty::Error
                }
            };
            let opt = v.ty.clone();
            let tmp = self.fresh(opt.clone());
            self.emit(Stmt::Set(tmp, v));
            let bound = self.fresh(inner.clone());
            if let Some(s) = self.st.scopes.get_mut(scope) {
                s.insert(b.name.name.clone(), bound);
            }
            levels.push((tmp, bound, inner, opt));
        }
        let mut inner_stmts = vec![Stmt::Set(ok, Ex::new(Ek::Bool(true), Ty::Bool))];
        for (tmp, bound, inner, opt) in levels.into_iter().rev() {
            let mut stmts = self.pop_sink();
            let test = Ex::host(Imp::IsSome, vec![Ex::local(tmp, opt.clone())], Ty::Bool);
            let mut taken = vec![Stmt::Set(
                bound,
                Self::unwrap_opt(Ex::local(tmp, opt), &inner),
            )];
            taken.extend(inner_stmts);
            stmts.push(Stmt::If(test, taken, Vec::new()));
            inner_stmts = stmts;
        }
        let mut prelude = vec![Stmt::Set(ok, Ex::new(Ek::Bool(false), Ty::Bool))];
        prelude.extend(inner_stmts);
        let then = (!absent).then(|| then(self));
        self.st.scopes.pop();
        (self.st.macros, self.st.states) = saved;
        let els = els(self);
        let cond = if absent {
            Ex::new(Ek::Bool(false), Ty::Bool)
        } else {
            Ex::local(ok, Ty::Bool)
        };
        IfLet {
            prelude,
            cond,
            then,
            els,
        }
    }

    /// `if let` as a statement.
    pub(super) fn if_let_stmt(
        &mut self,
        binds: &[LetBind],
        then: &ast::Block,
        els: Option<&ast::Expr>,
    ) {
        let out = self.if_let(
            binds,
            |c| c.block_stmts(then),
            |c| match els {
                None => Vec::new(),
                Some(x) => {
                    c.push_sink();
                    c.expr_stmt(x);
                    c.pop_sink()
                }
            },
        );
        for s in out.prelude {
            self.emit(s);
        }
        self.emit(Stmt::If(out.cond, out.then.unwrap_or_default(), out.els));
    }

    /// `if let` whose value is used: both branches are required and agree on a type.
    pub(super) fn if_let_value(
        &mut self,
        binds: &[LetBind],
        then: &ast::Block,
        els: Option<&ast::Expr>,
        exp: Option<&Ty>,
        span: Span,
    ) -> Ex {
        let hint = exp.filter(|t| !t.is_error()).cloned();
        let Some(els) = els else {
            let out = self.if_let(
                binds,
                |c| c.block_value(then, hint.as_ref()),
                |_| Ex::new(Ek::Unit, Ty::Unit),
            );
            let t = out.then.unwrap_or_else(|| Ex::new(Ek::Unit, Ty::Unit));
            if !matches!(t.ty, Ty::Unit | Ty::Error) {
                self.err("E038", span, "this `if let` is used as a value, so it needs an `else` branch: `if let x = o { a } else { b }`");
                return Ex::error();
            }
            let none = Ex::new(Ek::Unit, Ty::Unit);
            let value = Ex::new(
                Ek::If(Box::new(out.cond), Box::new(t), Box::new(none)),
                Ty::Unit,
            );
            return Ex::new(Ek::Seq(out.prelude, Box::new(value)), Ty::Unit);
        };
        let want = std::cell::RefCell::new(hint);
        let out = self.if_let(
            binds,
            |c| {
                let t = c.block_value(then, want.borrow().as_ref());
                let w = want.borrow().clone().unwrap_or_else(|| t.ty.clone());
                let t = c.coerce(t, &w, then.span);
                if !w.is_error() {
                    *want.borrow_mut() = Some(w);
                }
                t
            },
            |c| match want.borrow().clone() {
                Some(w) => c.expr_as(els, &w),
                None => c.expr(els, None),
            },
        );
        let ty = match &out.then {
            Some(t) if !t.ty.is_error() => t.ty.clone(),
            _ => out.els.ty.clone(),
        };
        let value = match out.then {
            Some(t) => Ex::new(
                Ek::If(Box::new(out.cond), Box::new(t), Box::new(out.els)),
                ty.clone(),
            ),
            None => out.els,
        };
        Ex::new(Ek::Seq(out.prelude, Box::new(value)), ty)
    }

    /// `x?.name` or `x?.name(args)`: the field or method of the value of the Option `x`, an
    /// Option itself (not one inside another); `.none` when `x` has none.
    pub(super) fn opt_chain(
        &mut self,
        base: &ast::Expr,
        name: &ast::Ident,
        args: Option<&[ast::Arg]>,
        span: Span,
    ) -> Ex {
        let b = self.expr(base, None);
        let inner = match &b.ty {
            Ty::Option(t) => (**t).clone(),
            Ty::Error => return Ex::error(),
            other => {
                let t = self.show(other);
                let call = if args.is_some() { "(...)" } else { "" };
                self.err(
                    "E106",
                    span,
                    format!(
                        "`?.` reads through an Option, and this is {t}, which always has a value; write `.{}{call}`",
                        name.name
                    ),
                );
                return Ex::error();
            }
        };
        let opt = b.ty.clone();
        let tmp = self.fresh(opt.clone());
        let bound = self.fresh(inner.clone());
        self.push_sink();
        let item = Ex::local(bound, inner.clone());
        let v = match args {
            None => self.field_read(item, name),
            Some(args) => self.value_method(item, name, args, span),
        };
        let stmts = self.pop_sink();
        if v.ty.is_error() {
            return Ex::error();
        }
        let (value, ty) = match &v.ty {
            Ty::Option(_) => {
                let ty = v.ty.clone();
                (v, ty)
            }
            _ => {
                let ty = Ty::Option(Box::new(v.ty.clone()));
                (Self::some(v, &ty), ty)
            }
        };
        let mut taken = vec![Stmt::Set(
            bound,
            Self::unwrap_opt(Ex::local(tmp, opt.clone()), &inner),
        )];
        taken.extend(stmts);
        let some = Ex::new(Ek::Seq(taken, Box::new(value)), ty.clone());
        let none = Ex::host(Imp::NoneV, Vec::new(), ty.clone());
        let test = Ex::host(Imp::IsSome, vec![Ex::local(tmp, opt)], Ty::Bool);
        let pick = Ex::new(
            Ek::If(Box::new(test), Box::new(some), Box::new(none)),
            ty.clone(),
        );
        Ex::new(Ek::Seq(vec![Stmt::Set(tmp, b)], Box::new(pick)), ty)
    }

    /// `x ?? fallback`: the value of the Option `x`, or `fallback` when it has none. A fallback
    /// that is an Option itself makes the whole an Option.
    pub(super) fn coalesce(
        &mut self,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        exp: Option<&Ty>,
        span: Span,
    ) -> Ex {
        let hint = exp.map(|t| match t {
            Ty::Option(_) => t.clone(),
            _ => Ty::Option(Box::new(t.clone())),
        });
        let l = self.expr(lhs, hint.as_ref());
        let inner = match &l.ty {
            Ty::Option(t) => (**t).clone(),
            Ty::Error => {
                let _ = self.expr(rhs, None);
                return Ex::error();
            }
            other => {
                let t = self.show(other);
                self.err(
                    "E106",
                    span,
                    format!("`??` gives the fallback of an Option, and this is {t}, which always has a value; leave out `?? ...`"),
                );
                let _ = self.expr(rhs, None);
                return Ex::error();
            }
        };
        let opt = l.ty.clone();
        let tmp = self.fresh(opt.clone());
        let r = self.expr(rhs, Some(&inner));
        let test = Ex::host(Imp::IsSome, vec![Ex::local(tmp, opt.clone())], Ty::Bool);
        let (some, none, ty) = if r.ty.same(&opt) {
            (Ex::local(tmp, opt.clone()), r, opt)
        } else {
            let r = self.coerce(r, &inner, rhs.span);
            (Self::unwrap_opt(Ex::local(tmp, opt), &inner), r, inner)
        };
        let pick = Ex::new(
            Ek::If(Box::new(test), Box::new(some), Box::new(none)),
            ty.clone(),
        );
        Ex::new(Ek::Seq(vec![Stmt::Set(tmp, l)], Box::new(pick)), ty)
    }
}
