//! Outcomes (convergence design 1.1): action parameter defaults, which an invocation that
//! leaves the argument out gets.

use super::native::mentions;
use super::{Checker, Ctx, Ex};
use crate::ast;
use crate::diag::Span;
use crate::ir::{self, Ek, FuncKind, Stmt};
use crate::resolve::Callable;
use crate::types::Ty;

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
}
