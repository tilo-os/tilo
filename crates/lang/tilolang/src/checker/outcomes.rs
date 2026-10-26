//! Outcomes and deltas (convergence design 1.1): action parameter defaults, which an invocation
//! that leaves the argument out gets, and the register writes whose meaning depends on where
//! the change is applied.

use super::native::mentions;
use super::{Checker, Ctx, Ex};
use crate::ast::{self, BinOp, ExprKind, UnOp};
use crate::diag::Span;
use crate::ir::{self, Ek, FuncKind, LocalId, Stmt};
use crate::resolve::Callable;
use crate::types::Ty;

/// What a value in an action body is, as far as a register write needs to know: a place (a
/// local or parameter and the fields read below it), `not` of one, or one plus or minus an
/// amount.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Sym {
    Place(LocalId, Vec<String>),
    Not(LocalId, Vec<String>),
    Shifted(LocalId, Vec<String>),
}

impl Checker<'_> {
    /// The function computing the default of the parameter `index` of `action`: a pure
    /// function of the parameters before it.
    pub(super) fn run_default(
        &mut self,
        action: &Callable,
        index: usize,
        written: &ast::Expr,
    ) -> ir::Func {
        let before = action.params.get(..index).unwrap_or_default();
        let (name, ty, export) = match action.params.get(index) {
            Some(p) => (
                p.name.clone(),
                p.ty.clone(),
                p.default
                    .as_ref()
                    .map(|d| d.export.clone())
                    .unwrap_or_default(),
            ),
            None => (String::new(), Ty::Error, String::new()),
        };
        self.begin(Ctx::Fn, before, ty.clone(), "");
        let later: Vec<&str> = action
            .params
            .get(index..)
            .unwrap_or_default()
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        let value = if let Some(other) = mentions(written, &later) {
            let what = if other == name {
                "itself".to_owned()
            } else {
                format!("`{other}`, which comes after it")
            };
            self.err("E028", written.span, format!("the default of `{name}` reads {what}; a default is computed from the parameters before it, so put the ones it reads first"));
            Ex::error()
        } else {
            self.expr_as(written, &ty)
        };
        self.emit(Stmt::Return(Some(value)));
        let mut f = self.end(FuncKind::Fn, &format!("{}~{name}", action.name), &export);
        f.nparams = before.len();
        f
    }

    /// The arguments of an action inlined into another action, the defaults of those left out
    /// computed there and then: each argument is held in a local, so none is evaluated twice.
    pub(super) fn inline_args(
        &mut self,
        c: &Callable,
        bound: Vec<Option<Ex>>,
    ) -> (Vec<Stmt>, Vec<Ex>) {
        if bound.iter().all(Option::is_some) {
            return (Vec::new(), bound.into_iter().flatten().collect());
        }
        let mut stmts = Vec::new();
        let mut args: Vec<Ex> = Vec::new();
        for (slot, p) in bound.into_iter().zip(&c.params) {
            let value = match (slot, p.default.as_ref().and_then(|d| d.func)) {
                (Some(v), _) => v,
                (None, Some(f)) => Ex::new(Ek::Call(f, args.clone()), p.ty.clone()),
                (None, None) => Ex::error(),
            };
            let tmp = self.fresh(p.ty.clone());
            stmts.push(Stmt::Set(tmp, value));
            args.push(Ex::local(tmp, p.ty.clone()));
        }
        (stmts, args)
    }

    /// The arguments of an action applied as a value (from a view, a flow or a block): those
    /// left out at the end are computed by the runtime when it is invoked, on the invoker's
    /// state. A gap before a given argument is refused.
    pub(super) fn applied_args(
        &mut self,
        c: &Callable,
        bound: Vec<Option<Ex>>,
        span: Span,
    ) -> Vec<Ex> {
        let given = bound.iter().rposition(Option::is_some).map_or(0, |i| i + 1);
        let mut out = Vec::new();
        for (i, slot) in bound.into_iter().take(given).enumerate() {
            match slot {
                Some(v) => out.push(v),
                None => {
                    let name = c.params.get(i).map_or("", |p| p.name.as_str());
                    self.err("E028", span, format!("this leaves out `{name}` of `{}` but gives a parameter after it; an action applied from a view, a flow or a block leaves out only its last parameters, so give `{name}` too", c.name));
                    out.push(Ex::error());
                }
            }
        }
        out
    }

    // ---- register writes ------------------------------------------------------------------

    /// What `e` is in terms of places, when it is a place, `not` of one, or one plus or minus
    /// an amount; `let`s are followed.
    pub(super) fn sym_of(&self, e: &ast::Expr) -> Option<Sym> {
        match &e.kind {
            ExprKind::Ident(name) => {
                let (id, _) = self.lookup(name)?;
                Some(
                    self.st
                        .syms
                        .get(&id)
                        .cloned()
                        .unwrap_or(Sym::Place(id, Vec::new())),
                )
            }
            ExprKind::Field { base, name } => match self.sym_of(base)? {
                Sym::Place(id, mut path) => {
                    path.push(name.name.clone());
                    Some(Sym::Place(id, path))
                }
                Sym::Not(..) | Sym::Shifted(..) => None,
            },
            ExprKind::Unary {
                op: UnOp::Not,
                operand,
            } => match self.sym_of(operand)? {
                Sym::Place(id, path) => Some(Sym::Not(id, path)),
                Sym::Not(id, path) => Some(Sym::Place(id, path)),
                Sym::Shifted(..) => None,
            },
            ExprKind::Binary {
                op: op @ (BinOp::Add | BinOp::Sub),
                lhs,
                rhs,
            } => {
                let shifted = |s: Option<Sym>| match s {
                    Some(Sym::Place(id, path) | Sym::Shifted(id, path)) => {
                        Some(Sym::Shifted(id, path))
                    }
                    _ => None,
                };
                shifted(self.sym_of(lhs)).or_else(|| {
                    if *op == BinOp::Add {
                        shifted(self.sym_of(rhs))
                    } else {
                        None
                    }
                })
            }
            ExprKind::Block(b) => match b.stmts.as_slice() {
                [
                    ast::Stmt {
                        kind: ast::StmtKind::Expr(x),
                        ..
                    },
                ] => self.sym_of(x),
                _ => None,
            },
            _ => None,
        }
    }

    /// Refuses `x.f = not x.f` and `x.f = x.f + n`: a write of a register computed from that
    /// same register, which recomputes wherever the change is applied. `place` is the written
    /// place as source text, `base` the expression it is a field of.
    pub(super) fn self_dependent_write(
        &mut self,
        base: &ast::Expr,
        field: &ast::Ident,
        ty: &Ty,
        value: &ast::Expr,
        span: Span,
    ) {
        let Some(Sym::Place(id, mut path)) = self.sym_of(base) else {
            return;
        };
        path.push(field.name.clone());
        let Some(written) = self.sym_of(value) else {
            return;
        };
        let place = format!(
            "{}.{}",
            super::expr::dotted(base).unwrap_or_default().join("."),
            field.name
        );
        let name = &field.name;
        let shown = self.show(ty);
        match written {
            Sym::Not(at, from) if at == id && from == path && *ty == Ty::Bool => self.err(
                "E039",
                span,
                format!("`{place} = not {place}` is computed again wherever the change is applied, so two people doing it at once cancel each other out. Record what the invoker saw: an outcome, as a parameter with a default the invoker gets when they leave it out (`{name}: Bool = not {place}`, then `{place} = {name}`); or a delta, by making `{name}` a counter (`{name}: Int merge counter`) changed with `+=` and `-=`"),
            ),
            Sym::Shifted(at, from) if at == id && from == path && matches!(ty, Ty::Int | Ty::Float) => self.err(
                "E039",
                span,
                format!("`{place} = {place} + ...` is computed again wherever the change is applied, so of two people changing it at once one change is lost. Record what the invoker meant: a delta, by making `{name}` a counter (`{name}: {shown} merge counter`) changed with `+=` and `-=`; or an outcome, as a parameter with a default the invoker gets when they leave it out (`{name}: {shown} = {place} + 1`, then `{place} = {name}`)"),
            ),
            _ => {}
        }
    }
}
