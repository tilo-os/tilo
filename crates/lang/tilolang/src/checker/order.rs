//! `sort_by` over a collection, answered by the store: by a field of the items (`.due`,
//! `x -> x.title`) or by their versions (`x -> updated(x)`, `x -> created(x)`, ordered by their
//! time), oldest or smallest first, or with `desc` the latest or largest first.
//!
//! A store with an index keeps a column per such field and per version in each collection, so
//! a list in that order opens by reading one page. The order is handed to the host beside the
//! loop that sorts the items itself, which runs where the host cannot answer (a collection
//! read whole in an action, a union's items).

use super::{Checker, Ex};
use crate::abi::Imp;
use crate::ast::{self, ExprKind};
use crate::ir::{self, Ek, Stmt};
use crate::types::Ty;

/// The pseudo-fields naming a resource's versions in an order the store answers.
pub(super) const CREATED: &str = "$created";
pub(super) const UPDATED: &str = "$updated";

/// What a `sort_by` key reads of an item.
pub(super) enum SortKey {
    /// A field, by id.
    Field(String),
    /// Its first (`created`) or last (`updated`) version.
    Version { last: bool },
}

impl SortKey {
    fn field(&self) -> &str {
        match self {
            Self::Field(id) => id,
            Self::Version { last: true } => UPDATED,
            Self::Version { last: false } => CREATED,
        }
    }
}

impl Checker<'_> {
    /// What the key `param -> body` reads of an item of the type `elem`, when the store can
    /// order by it: a field of the item that holds a value (a register or a counter), or
    /// `created(param)` / `updated(param)`.
    pub(super) fn sort_key(&self, elem: &Ty, param: &str, body: &ast::Expr) -> Option<SortKey> {
        let is_param = |e: &ast::Expr| matches!(&e.kind, ExprKind::Ident(p) if p == param);
        match &body.kind {
            ExprKind::Call { callee, args } => {
                let ExprKind::Ident(name) = &callee.kind else {
                    return None;
                };
                let last = match name.as_str() {
                    "updated" => true,
                    "created" => false,
                    _ => return None,
                };
                match args.as_slice() {
                    [a] if a.name.is_none() && is_param(&a.value) => {
                        Some(SortKey::Version { last })
                    }
                    _ => None,
                }
            }
            ExprKind::Field { base, name } if is_param(base) => {
                let Ty::Record(key) = elem else {
                    return None;
                };
                let field = self.env.type_info(key)?.field(&name.name)?;
                matches!(
                    field.ty,
                    Ty::Int | Ty::Float | Ty::Text | Ty::Bool | Ty::Date | Ty::Time
                )
                .then(|| SortKey::Field(field.id.clone()))
            }
            _ => None,
        }
    }

    /// The key of the loop that sorts by a version itself: its time, the empty text for none
    /// (first, as the store orders an absent version).
    pub(super) fn version_time(&mut self, item: Ex, last: bool) -> Ex {
        let version = super::platform::host_ty("Version");
        let opt_ty = Ty::Option(Box::new(version.clone()));
        let end = self.fresh(opt_ty.clone());
        let set = Stmt::Set(
            end,
            Ex::host(
                Imp::VersionEnd,
                vec![item, Ex::new(Ek::I32(i32::from(last)), Ty::Bool)],
                opt_ty.clone(),
            ),
        );
        let found = Ex::host(Imp::IsSome, vec![Ex::local(end, opt_ty.clone())], Ty::Bool);
        let at = crate::host::value_field("Version", "at")
            .and_then(|at| i32::try_from(at).ok())
            .unwrap_or(1);
        let time = Ex::host(
            Imp::TupleGetH,
            vec![
                Ex::host(Imp::UnwrapH, vec![Ex::local(end, opt_ty)], version),
                Ex::new(Ek::I32(at), Ty::Bool),
            ],
            Ty::Text,
        );
        let chosen = Ex::new(
            Ek::If(Box::new(found), Box::new(time), Box::new(Ex::str(""))),
            Ty::Text,
        );
        Ex::new(Ek::Seq(vec![set], Box::new(chosen)), Ty::Text)
    }

    /// `looped`, the loop of a `sort_by` over the collection `held` (kept in the local `list`,
    /// which the loop reads), behind the store's answer to the order `key`: `coll_ordered`
    /// hands back the collection in that order when the host can list it so, and the loop runs
    /// when it cannot.
    pub(super) fn ordered_or_loop(
        &mut self,
        list: u32,
        held: Ex,
        key: &SortKey,
        descending: bool,
        looped: Ex,
    ) -> Ex {
        let text =
            serde_json::json!({ "field": key.field(), "descending": descending }).to_string();
        let ty = looped.ty.clone();
        let list_ty = held.ty.clone();
        let hold = Stmt::Set(list, held);
        let answer_ty = Ty::Option(Box::new(ty.clone()));
        let answer = self.fresh(answer_ty.clone());
        let ask = Stmt::Set(
            answer,
            Ex::host(
                Imp::CollOrdered,
                vec![Ex::local(list, list_ty), Ex::str(&text)],
                answer_ty.clone(),
            ),
        );
        let found = Ex::host(
            Imp::IsSome,
            vec![Ex::local(answer, answer_ty.clone())],
            Ty::Bool,
        );
        let items = Ex::host(Imp::UnwrapH, vec![Ex::local(answer, answer_ty)], ty.clone());
        let chosen = Ex::new(
            Ek::If(Box::new(found), Box::new(items), Box::new(looped)),
            ty.clone(),
        );
        Ex::new(Ek::Seq(vec![hold, ask], Box::new(chosen)), ty)
    }
}
