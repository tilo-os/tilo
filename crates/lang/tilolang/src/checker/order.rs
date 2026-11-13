//! `sort_by` over a collection, answered by the store: by a field of the items (`.due`, `x -> x.title`), by
//! their versions (`x -> updated(x)`), or by when one of their fields last changed
//! (`x -> changed(x, .done)?.at`), oldest or smallest first, or with `desc` the latest or largest first.
//!
//! A store with an index keeps a column per such field and per version in each collection, so a list in
//! that order opens by reading one page. The order is handed to the host beside the loop that sorts the
//! items itself, which runs where the host cannot answer. A field an item may have no value of orders with
//! those items last, and an order over a list already in one keeps that order where this one holds two
//! items equal.

use super::{Checker, Ex};
use crate::abi::Imp;
use crate::ast::{self, ExprKind};
use crate::ir::{Ek, Stmt};
use crate::types::Ty;

/// The pseudo-fields naming a resource's versions in an order the store answers.
pub(super) const CREATED: &str = "$created";
pub(super) const UPDATED: &str = "$updated";
/// What the pseudo-field holding when a field last changed begins with.
pub(super) const CHANGED: &str = "$changed";

pub(super) enum SortKey {
    /// A field, by id, and whether an item may have no value of it.
    Field {
        id: String,
        optional: bool,
    },
    Version {
        last: bool,
    },
    /// When a field of it last changed (`changed(x, .field)?.at`), under the pseudo-field the index keeps it
    /// at. An item that never changed it has no moment of one, which stands before every moment.
    Changed {
        id: String,
    },
}

impl SortKey {
    fn field(&self) -> &str {
        match self {
            Self::Field { id, .. } | Self::Changed { id } => id,
            Self::Version { last: true } => UPDATED,
            Self::Version { last: false } => CREATED,
        }
    }

    /// Whether the items without a value of the field stand after those with one, in both directions: a task
    /// nobody gave a day is no day's work, not the first one's.
    pub(super) fn absent_last(&self) -> bool {
        matches!(self, Self::Field { optional: true, .. })
    }
}

impl Checker<'_> {
    /// What the key `param -> body` reads of an item of the type `elem`, when the store can order by it: a
    /// field of the item that holds a value (a register or a counter, a day or a time, though one it may not
    /// have), or `created(param)` / `updated(param)`. A key that names a function a template was given
    /// (`x -> closed(x)`) reads what that function reads.
    pub(super) fn sort_key(&self, elem: &Ty, param: &str, body: &ast::Expr) -> Option<SortKey> {
        let is_param = |e: &ast::Expr| matches!(&e.kind, ExprKind::Ident(p) if p == param);
        match &body.kind {
            ExprKind::Call { callee, args, .. } => {
                let ExprKind::Ident(name) = &callee.kind else {
                    return None;
                };
                if let [a] = args.as_slice()
                    && a.name.is_none()
                    && is_param(&a.value)
                {
                    if let "created" | "updated" = name.as_str() {
                        return Some(SortKey::Version {
                            last: name.as_str() == "updated",
                        });
                    }
                    // The function the template was given for this name: its own parameter stands where the item
                    // does, so its body is the key.
                    let closure = self.lambda_named(name)?;
                    let [(inner, _)] = closure.params.as_slice() else {
                        return None;
                    };
                    return self.sort_key(elem, inner, &closure.body);
                }
                None
            }
            // When a field last changed, which the index keeps by its time.
            ExprKind::OptChain { base, name, .. } if name.name == "at" => {
                let ExprKind::Call { callee, args, .. } = &base.kind else {
                    return None;
                };
                let ExprKind::Ident(callee) = &callee.kind else {
                    return None;
                };
                let [r, f] = args.as_slice() else {
                    return None;
                };
                if callee != "changed" || r.name.is_some() || !is_param(&r.value) {
                    return None;
                }
                let ExprKind::Variant {
                    name: field,
                    args: None,
                } = &f.value.kind
                else {
                    return None;
                };
                let Ty::Record(key) = elem else {
                    return None;
                };
                let field = self.env.type_info(key)?.field(&field.name)?;
                Some(SortKey::Changed {
                    id: format!("{CHANGED}.{}", field.id),
                })
            }
            ExprKind::Field { base, name } if is_param(base) => {
                let Ty::Record(key) = elem else {
                    return None;
                };
                let field = self.env.type_info(key)?.field(&name.name)?;
                match &field.ty {
                    Ty::Int | Ty::Float | Ty::Text | Ty::Bool | Ty::Date | Ty::Time => {
                        Some(SortKey::Field {
                            id: field.id.clone(),
                            optional: false,
                        })
                    }
                    // A day or a time an item may not have.
                    Ty::Option(inner) if matches!(**inner, Ty::Date | Ty::Time) => {
                        Some(SortKey::Field {
                            id: field.id.clone(),
                            optional: true,
                        })
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// The lambda the name `name` holds where the code being checked sees it: a parameter of the template being
    /// expanded, given a function or nothing.
    fn lambda_named(&self, name: &str) -> Option<std::rc::Rc<super::template::Closure>> {
        use super::template::Macro;
        match self.st.macros.get(name)? {
            Macro::Lambda(closure) => Some(std::rc::Rc::clone(closure)),
            Macro::Some(inner) => match inner.as_ref() {
                Macro::Lambda(closure) => Some(std::rc::Rc::clone(closure)),
                _ => None,
            },
            _ => None,
        }
    }

    /// The key of the loop that sorts by a version itself: its time, the empty text for none (first, as the
    /// store orders an absent version).
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

    /// `looped`, the loop of a `sort_by` over the collection `held` (kept in the local `list`, which the loop
    /// reads), behind the store's answer to the order `key`: `coll_ordered` hands back the collection in that
    /// order when the host can list it so, and the loop runs when it cannot.
    pub(super) fn ordered_or_loop(
        &mut self,
        list: u32,
        held: Ex,
        key: &SortKey,
        descending: bool,
        looped: Ex,
    ) -> Ex {
        let text = serde_json::json!({
            "field": key.field(),
            "descending": descending,
            "absent_last": key.absent_last(),
        })
        .to_string();
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
