//! `filter` and `count` over a collection with a lambda the store can answer: the lambda as a
//! [`Pred`], handed to the host beside the loop (see [`crate::pred`]).

use super::{Checker, Ex};
use crate::abi::Imp;
use crate::ast;
use crate::ir::{Ek, Stmt};
use crate::pred::Pred;
use crate::types::Ty;

impl Checker<'_> {
    /// The predicate `param -> body` is over items of the type `elem`, when it reads nothing
    /// but fields of the item and literals ([`crate::search::lambda_pred`]).
    ///
    /// It may compare a field with a local or parameter the code holds (`c -> c.column ==
    /// col`): those names come back beside it, in the order of its `Arg`s, and their values are
    /// handed to the host with it.
    pub(super) fn native_pred(
        &self,
        elem: &Ty,
        param: &str,
        body: &ast::Expr,
    ) -> Option<(Pred<String>, Vec<String>)> {
        let Ty::Record(key) = elem else {
            return None;
        };
        let info = self.env.type_info(key)?;
        let outer = |name: &str| self.lookup(name).map(|(_, ty)| ty);
        crate::search::lambda_pred_with(info, self.env, param, body, &outer)
    }

    /// `looped`, the loop of a `filter` or `count` over the collection `held` (kept in the local
    /// `list`, which the loop reads), behind the store's answer to `pred`: `coll_where` hands
    /// back the collection's matching items when the host can list them (a collection read in a
    /// view), and the loop runs when it cannot.
    pub(super) fn where_or_loop(
        &mut self,
        list: u32,
        held: Ex,
        m: &str,
        (pred, captured): &(Pred<String>, Vec<String>),
        looped: Ex,
    ) -> Ex {
        // Text the host cannot read is a question it does not answer: the loop runs.
        let text = serde_json::to_string(pred).unwrap_or_default();
        let ty = looped.ty.clone();
        let list_ty = &held.ty.clone();
        let hold = Stmt::Set(list, held);
        let filtered_ty = match list_ty {
            Ty::Collection(elem) => Ty::List(elem.clone()),
            other => other.clone(),
        };
        let answer_ty = Ty::Option(Box::new(filtered_ty.clone()));
        let answer = self.fresh(answer_ty.clone());
        let asked = if captured.is_empty() {
            Ex::host(
                Imp::CollWhere,
                vec![Ex::local(list, list_ty.clone()), Ex::str(&text)],
                answer_ty.clone(),
            )
        } else {
            // The values it compares with, as they are now, in the order of its `Arg`s.
            let values: Vec<ast::Expr> = captured
                .iter()
                .map(|name| ast::Expr {
                    kind: ast::ExprKind::Ident(name.clone()),
                    span: crate::diag::Span::default(),
                })
                .collect();
            let tuple = ast::Expr {
                kind: ast::ExprKind::Tuple(values),
                span: crate::diag::Span::default(),
            };
            let args = self.expr(&tuple, None);
            Ex::host(
                Imp::CollWhereWith,
                vec![Ex::local(list, list_ty.clone()), Ex::str(&text), args],
                answer_ty.clone(),
            )
        };
        let ask = Stmt::Set(answer, asked);
        let found = Ex::host(
            Imp::IsSome,
            vec![Ex::local(answer, answer_ty.clone())],
            Ty::Bool,
        );
        let items = Ex::host(
            Imp::UnwrapH,
            vec![Ex::local(answer, answer_ty)],
            filtered_ty,
        );
        let native = if m == "count" {
            Ex::host(Imp::ListLen, vec![items], Ty::Int)
        } else {
            items
        };
        let chosen = Ex::new(
            Ek::If(Box::new(found), Box::new(native), Box::new(looped)),
            ty.clone(),
        );
        Ex::new(Ek::Seq(vec![hold, ask], Box::new(chosen)), ty)
    }
}
