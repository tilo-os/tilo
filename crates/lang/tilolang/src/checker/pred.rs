//! `filter` and `count` over a collection with a lambda the store can answer: the lambda as a
//! [`Pred`], handed to the host beside the loop (see [`crate::pred`]).

use super::{Checker, Ex};
use crate::abi::Imp;
use crate::ast::{self, BinOp, ExprKind, StrPart, UnOp};
use crate::ir::{Ek, Stmt};
use crate::pred::{CmpOp, Lit, Operand, Pred};
use crate::resolve::TypeInfo;
use crate::types::{MergeKind, Ty};

/// What a predicate operand holds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Bool,
    Int,
    Text,
}

impl Checker<'_> {
    /// The predicate `param -> body` is over items of the type `elem`, when it reads nothing
    /// but scalar fields of the item (registers, counters and texts) and literals.
    pub(super) fn native_pred(
        &self,
        elem: &Ty,
        param: &str,
        body: &ast::Expr,
    ) -> Option<Pred<String>> {
        let Ty::Record(key) = elem else {
            return None;
        };
        let info = self.env.type_info(key)?;
        pred_of(info, param, body)
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
        pred: &Pred<String>,
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
        let ask = Stmt::Set(
            answer,
            Ex::host(
                Imp::CollWhere,
                vec![Ex::local(list, list_ty.clone()), Ex::str(&text)],
                answer_ty.clone(),
            ),
        );
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

fn pred_of(info: &TypeInfo, param: &str, e: &ast::Expr) -> Option<Pred<String>> {
    match &e.kind {
        ExprKind::Bool(b) => Some(Pred::Const(*b)),
        ExprKind::Unary {
            op: UnOp::Not,
            operand,
        } => Some(Pred::Not(Box::new(pred_of(info, param, operand)?))),
        ExprKind::Binary {
            op: op @ (BinOp::And | BinOp::Or),
            lhs,
            rhs,
        } => {
            let both = vec![pred_of(info, param, lhs)?, pred_of(info, param, rhs)?];
            Some(if *op == BinOp::And {
                Pred::And(both)
            } else {
                Pred::Or(both)
            })
        }
        ExprKind::Binary { op, lhs, rhs } => {
            let op = match op {
                BinOp::Eq => CmpOp::Eq,
                BinOp::Ne => CmpOp::Ne,
                BinOp::Lt => CmpOp::Lt,
                BinOp::Le => CmpOp::Le,
                BinOp::Gt => CmpOp::Gt,
                BinOp::Ge => CmpOp::Ge,
                _ => return None,
            };
            let (a, ka) = operand(info, param, lhs)?;
            let (b, kb) = operand(info, param, rhs)?;
            let ordered = !matches!(op, CmpOp::Eq | CmpOp::Ne);
            (ka == kb && (!ordered || ka == Kind::Int)).then_some(Pred::Cmp(op, a, b))
        }
        _ => match operand(info, param, e)? {
            (field @ Operand::Field(_), Kind::Bool) => {
                Some(Pred::Cmp(CmpOp::Eq, field, Operand::Lit(Lit::Bool(true))))
            }
            _ => None,
        },
    }
}

fn operand(info: &TypeInfo, param: &str, e: &ast::Expr) -> Option<(Operand<String>, Kind)> {
    match &e.kind {
        ExprKind::Bool(b) => Some((Operand::Lit(Lit::Bool(*b)), Kind::Bool)),
        ExprKind::Int(i) => Some((Operand::Lit(Lit::Int(*i)), Kind::Int)),
        ExprKind::Unary {
            op: UnOp::Neg,
            operand: inner,
        } => match &inner.kind {
            ExprKind::Int(i) => Some((Operand::Lit(Lit::Int(i.checked_neg()?)), Kind::Int)),
            _ => None,
        },
        ExprKind::Str(parts) => {
            let mut text = String::new();
            for part in parts {
                match part {
                    StrPart::Lit(s) => text.push_str(s),
                    StrPart::Interp(_) => return None,
                }
            }
            Some((Operand::Lit(Lit::Text(text)), Kind::Text))
        }
        ExprKind::Field { base, name } => {
            if !matches!(&base.kind, ExprKind::Ident(p) if p == param) {
                return None;
            }
            let field = info.field(&name.name)?;
            let kind = match (field.merge, &field.ty) {
                (MergeKind::Register, Ty::Bool) => Kind::Bool,
                (MergeKind::Register | MergeKind::Counter, Ty::Int) => Kind::Int,
                (MergeKind::Register | MergeKind::Text, Ty::Text) => Kind::Text,
                _ => return None,
            };
            Some((Operand::Field(field.id.clone()), kind))
        }
        _ => None,
    }
}
