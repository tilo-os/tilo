//! The methods of a list: a lambda's inline loop, sorting, the ends, an index, and the items
//! under a node by a field of theirs.

use super::super::{Checker, Ex};
use crate::abi::Imp;
use crate::ast::{self, ExprKind};
use crate::diag::Span;
use crate::ir::{self, Ek, Stmt};
use crate::types::Ty;

impl Checker<'_> {
    /// `map`, `flat_map`, `filter`, `any`, `all`, `find` and `count` with a lambda, inlined as a
    /// loop: a lambda is only ever a direct argument of these, so there are no closures at run
    /// time. `flat_map` gives the lists its lambda finds, joined in the order they come.
    pub(in crate::checker) fn higher_order(
        &mut self,
        list: Ex,
        elem: &Ty,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> Ex {
        let m = name.name.as_str();
        // A count says how many items there are: a collection the store lists as it reads it
        // is read to its end where the store cannot say otherwise, which is what counting it
        // costs ([`Imp::CollCount`]). Length is what the store knows without that
        // ([`Imp::ListLen`]); a list holds a collection too, so the host tells them apart.
        if m == "count" && args.is_empty() {
            return Ex::host(Imp::CollCount, vec![list], Ty::Int);
        }
        let lambda = match args {
            [a] if a.name.is_none() => super::super::optional::lambda_of(&a.value),
            _ => None,
        };
        let Some((param, body)) = lambda else {
            self.err(
                "E051",
                span,
                format!("`.{m}` takes one lambda such as `.{m}(x -> x.done)`, or a key path such as `.{m}(.done)`"),
            );
            for a in args {
                if !matches!(a.value.kind, ExprKind::Lambda { .. }) {
                    let _ = self.expr(&a.value, None);
                }
            }
            return Ex::error();
        };
        // Over a collection, a lambda the store can answer is asked of it first: over a
        // collection, and over a list that may be one (a parameter taking either keeps the
        // collection it is given). The list is held in a local for the asking, which the loop
        // reads too.
        let pred = match (&list.ty, m) {
            (Ty::Collection(_) | Ty::List(_), "filter" | "count") => {
                self.native_pred(elem, &param.name, &body)
            }
            _ => None,
        };
        let (list, native) = match pred {
            Some(pred) => {
                let ty = list.ty.clone();
                let local = self.fresh(ty.clone());
                (Ex::local(local, ty), Some((local, list, pred)))
            }
            None => (list, None),
        };
        let mut fl = self.new_for(list, elem);
        let item = Ex::local(fl.item_local, elem.clone());
        self.push_sink();
        self.bind_local(&param.name, fl.item_local);
        let (result, result_ty) = if m == "map" {
            let v = self.expr(&body, None);
            let out_ty = Ty::List(Box::new(v.ty.clone()));
            let out = self.fresh(out_ty.clone());
            self.emit(Stmt::Eval(Self::push_item(
                Ex::local(out, out_ty.clone()),
                v,
            )));
            (out, out_ty)
        } else if m == "flat_map" {
            // What the lambda gives for each item, joined in the order the items and their own
            // lists keep: one loop inside the other, pushing what it holds.
            let v = self.expr(&body, None);
            let each = match v.ty.clone() {
                Ty::List(each) => *each,
                Ty::Error => Ty::Error,
                other => {
                    let shown = self.show(&other);
                    self.err(
                        "E020",
                        name.span,
                        format!("`flat_map`'s lambda gives a list to join, found {shown}: `all.flat_map(c -> c.channels)`"),
                    );
                    Ty::Error
                }
            };
            let out_ty = Ty::List(Box::new(each.clone()));
            let out = self.fresh(out_ty.clone());
            if !each.is_error() {
                let mut each_loop = self.new_for(v, &each);
                let inside = Ex::local(each_loop.item_local, each.clone());
                each_loop.body.push(Stmt::Eval(Self::push_item(
                    Ex::local(out, out_ty.clone()),
                    inside,
                )));
                self.emit(Stmt::ForList(Box::new(each_loop)));
            }
            (out, out_ty)
        } else {
            self.filter_like(m, &body, &item, elem)
        };
        let mut inner = self.pop_sink();
        // Hoisted `Set` of the accumulator is emitted before the loop by the caller below.
        let init = match m {
            "map" | "flat_map" | "filter" => {
                Some(Ex::host(Imp::ListNew, Vec::new(), result_ty.clone()))
            }
            "any" => Some(Ex::new(Ek::Bool(false), Ty::Bool)),
            "all" => Some(Ex::new(Ek::Bool(true), Ty::Bool)),
            "count" => Some(Ex::int(0)),
            _ => Some(Ex::host(Imp::NoneV, Vec::new(), result_ty.clone())),
        };
        fl.body.append(&mut inner);
        let mut stmts = Vec::new();
        if let Some(i) = init {
            stmts.push(Stmt::Set(result, i));
        }
        stmts.push(Stmt::ForList(Box::new(fl)));
        let looped = Ex::new(
            Ek::Seq(stmts, Box::new(Ex::local(result, result_ty.clone()))),
            result_ty,
        );
        match native {
            Some((local, held, pred)) => self.where_or_loop(local, held, m, &pred, looped),
            None => looped,
        }
    }

    /// `items.descendants(t, x -> x.parent)` and `items.ancestors(t, x -> x.parent)`: the items
    /// under `t`, and the ones it is under, following the parent each item names. The items
    /// under it come in the order the list keeps them, the ones above it from the nearest;
    /// `t` itself is neither.
    ///
    /// `collection`, when the list is a collection field of a resource, is that resource and the
    /// field's id: the store answers the walk from the index's column of the parent field, and
    /// only where it cannot the walk runs, reading the parent one item at a time and stopping at
    /// the first item without one, so a chain that loops still ends there.
    pub(super) fn relatives(
        &mut self,
        list: Ex,
        elem: &Ty,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
        collection: Option<(Ex, String)>,
    ) -> Ex {
        let m = name.name.as_str();
        let lambda = match args {
            [t, l] if t.name.is_none() && l.name.is_none() => {
                Some((t, super::super::optional::lambda_of(&l.value)))
            }
            _ => None,
        };
        let Some((target, Some((param, body)))) = lambda else {
            self.err(
                "E051",
                span,
                format!(
                    "`.{m}` takes one of the items and how to read an item's parent: `.{m}(t, x -> x.parent)`, or `.{m}(t, .parent)`"
                ),
            );
            for a in args {
                if !matches!(a.value.kind, ExprKind::Lambda { .. }) {
                    let _ = self.expr(&a.value, None);
                }
            }
            return Ex::error();
        };
        // The field the walk follows, when the lambda only reads one: a column of the
        // collection answers a walk over it, and a parent read another way is the walk's alone.
        let parent_field = match &body.kind {
            ExprKind::Field { base, name } if matches!(&base.kind, ExprKind::Ident(at) if at == &param.name) => {
                Some(name.name.clone())
            }
            _ => None,
        };
        let opt = Ty::Option(Box::new(elem.clone()));
        let out_ty = Ty::List(Box::new(elem.clone()));
        let target = self.expr_as(&target.value, elem);
        // The parent of `at`, with the lambda's parameter bound to it.
        let at = self.fresh(elem.clone());
        self.push_sink();
        self.bind_local(&param.name, at);
        let parent = self.expr(&body, None);
        let parent = self.coerce(parent, &opt, body.span);
        let step = self.pop_sink();
        if parent.ty.is_error() {
            return Ex::error();
        }
        let held_ty = list.ty.clone();
        let held = self.fresh(held_ty.clone());
        let target_local = self.fresh(elem.clone());
        let cur = self.fresh(opt.clone());
        let out = self.fresh(out_ty.clone());
        let mut stmts = vec![
            Stmt::Set(held, list),
            Stmt::Set(out, Ex::host(Imp::ListNew, Vec::new(), out_ty.clone())),
        ];
        // What the parent of the item held in `at` is, left in `cur`.
        let parent_step = {
            let mut s = step;
            s.push(Stmt::Set(cur, parent));
            s
        };
        let has_a_parent = || Ex::host(Imp::IsSome, vec![Ex::local(cur, opt.clone())], Ty::Bool);
        if m == "descendants" {
            let found = self.fresh(Ty::Bool);
            let mut outer = self.new_for(Ex::local(held, held_ty.clone()), elem);
            let item = Ex::local(outer.item_local, elem.clone());
            let mut body = vec![Stmt::Set(at, item.clone())];
            body.extend(parent_step.clone());
            body.push(Stmt::Set(found, Ex::new(Ek::Bool(false), Ty::Bool)));
            let mut walk = self.new_for(Ex::local(held, held_ty), elem);
            let under = self.same(
                Ex::local(at, elem.clone()),
                Ex::local(target_local, elem.clone()),
                span,
            );
            let mut reached = vec![
                Stmt::Set(at, Self::unwrap_opt(Ex::local(cur, opt.clone()), elem)),
                Stmt::If(
                    under,
                    vec![
                        Stmt::Set(found, Ex::new(Ek::Bool(true), Ty::Bool)),
                        Stmt::Break,
                    ],
                    Vec::new(),
                ),
            ];
            reached.extend(parent_step);
            walk.body
                .push(Stmt::If(has_a_parent(), reached, vec![Stmt::Break]));
            body.push(Stmt::ForList(Box::new(walk)));
            body.push(Stmt::If(
                Ex::local(found, Ty::Bool),
                vec![Stmt::Eval(Self::push_item(
                    Ex::local(out, out_ty.clone()),
                    item,
                ))],
                Vec::new(),
            ));
            outer.body = body;
            stmts.push(Stmt::ForList(Box::new(outer)));
        } else {
            stmts.push(Stmt::Set(
                cur,
                Self::some(Ex::local(target_local, elem.clone()), &opt),
            ));
            let mut walk = self.new_for(Ex::local(held, held_ty), elem);
            let mut up = vec![Stmt::Set(
                at,
                Self::unwrap_opt(Ex::local(cur, opt.clone()), elem),
            )];
            up.extend(parent_step);
            up.push(Stmt::If(
                has_a_parent(),
                vec![Stmt::Eval(Self::push_item(
                    Ex::local(out, out_ty.clone()),
                    Self::unwrap_opt(Ex::local(cur, opt.clone()), elem),
                ))],
                vec![Stmt::Break],
            ));
            walk.body
                .push(Stmt::If(has_a_parent(), up, vec![Stmt::Break]));
            stmts.push(Stmt::ForList(Box::new(walk)));
        }
        let looped = Ex::new(
            Ek::Seq(stmts, Box::new(Ex::local(out, out_ty.clone()))),
            out_ty.clone(),
        );
        let asked = match (collection, parent_field) {
            (Some((holder, field)), Some(parent)) => {
                let answer_ty = Ty::Option(Box::new(out_ty.clone()));
                let answer = self.fresh(answer_ty.clone());
                let down = Ex::new(Ek::I32(i32::from(m == "descendants")), Ty::Bool);
                let asked = Ex::host(
                    Imp::CollRelatives,
                    vec![
                        holder,
                        Ex::str(&field),
                        Ex::str(&parent),
                        Ex::local(target_local, elem.clone()),
                        down,
                    ],
                    answer_ty.clone(),
                );
                let found = Ex::host(
                    Imp::IsSome,
                    vec![Ex::local(answer, answer_ty.clone())],
                    Ty::Bool,
                );
                let items = Ex::host(
                    Imp::UnwrapH,
                    vec![Ex::local(answer, answer_ty)],
                    out_ty.clone(),
                );
                Ek::Seq(
                    vec![Stmt::Set(target_local, target), Stmt::Set(answer, asked)],
                    Box::new(Ex::new(
                        Ek::If(Box::new(found), Box::new(items), Box::new(looped)),
                        out_ty.clone(),
                    )),
                )
            }
            _ => Ek::Seq(vec![Stmt::Set(target_local, target)], Box::new(looped)),
        };
        Ex::new(asked, out_ty)
    }

    /// `first()` or `last()`: the host reads the end of the list (`list_end`), so a view over a
    /// collection depends on that item alone, not on its length or the items before it.
    pub(super) fn end_item(list: Ex, elem: &Ty, last: bool) -> Ex {
        let out = Ty::Option(Box::new(elem.clone()));
        let which = Ex::new(Ek::I32(i32::from(last)), Ty::Bool);
        Ex::host(Imp::ListEnd, vec![list, which], out)
    }

    /// `xs.at(i)`: the item at the place `i` counts from the start, the place `index_of` answers
    /// for it, and none where the list ends before that place — so a copy can be read where the
    /// item it copies stands. It is the first `i + 1` items' last, asked only where the list is
    /// that long.
    pub(super) fn item_at(&mut self, list: Ex, elem: &Ty, place: &ast::Arg) -> Ex {
        let list_ty = list.ty.clone();
        let held = self.fresh(list_ty.clone());
        let at = self.fresh(Ty::Int);
        let up_to = Ex::new(
            Ek::Bin(
                ir::BinOp::Add,
                Box::new(Ex::local(at, Ty::Int)),
                Box::new(Ex::int(1)),
            ),
            Ty::Int,
        );
        let items_ty = Ty::List(Box::new(elem.clone()));
        let taken = self.fresh(items_ty.clone());
        let out = Ty::Option(Box::new(elem.clone()));
        let found = self.fresh(out.clone());
        let enough = Ex::new(
            Ek::Bin(
                ir::BinOp::Eq,
                Box::new(Ex::host(
                    Imp::ListLen,
                    vec![Ex::local(taken, items_ty.clone())],
                    Ty::Int,
                )),
                Box::new(up_to.clone()),
            ),
            Ty::Bool,
        );
        let place = self.expr_as(&place.value, &Ty::Int);
        let stmts = vec![
            Stmt::Set(held, list),
            Stmt::Set(at, place),
            Stmt::Set(
                taken,
                Ex::host(
                    Imp::Take,
                    vec![Ex::local(held, list_ty), up_to],
                    items_ty.clone(),
                ),
            ),
            Stmt::Set(found, Ex::host(Imp::NoneV, Vec::new(), out.clone())),
            Stmt::If(
                enough,
                vec![Stmt::Set(
                    found,
                    Self::end_item(Ex::local(taken, items_ty), elem, true),
                )],
                Vec::new(),
            ),
        ];
        Ex::new(Ek::Seq(stmts, Box::new(Ex::local(found, out.clone()))), out)
    }

    /// `sort_by(x -> key)`: collects the keys in a parallel list, then asks the host for the
    /// items ordered by them (ascending, stable). Keys are Int, Float, Text or Bool; a Date or
    /// a Time sorts in time order, and one that may be absent after every item that has one.
    pub(super) fn sort_by(
        &mut self,
        list: Ex,
        elem: &Ty,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> Ex {
        // `sort_by(key)` or `sort_by(key, desc)`: latest, largest or last first.
        let (key_arg, desc) = match args {
            [a] if a.name.is_none() => (Some(a), false),
            [a, d]
                if a.name.is_none()
                    && matches!(&d.value.kind, ExprKind::Ident(w) if w == "desc" || w == "asc") =>
            {
                (
                    Some(a),
                    matches!(&d.value.kind, ExprKind::Ident(w) if w == "desc"),
                )
            }
            _ => (None, false),
        };
        let lambda = key_arg.and_then(|a| super::super::optional::lambda_of(&a.value));
        let Some((param, body)) = lambda else {
            self.err(
                "E051",
                span,
                "`.sort_by` takes one lambda such as `.sort_by(x -> x.title)`, or a key path such as `.sort_by(.title)`, and `desc` for the largest or latest first: `.sort_by(.due, desc)`",
            );
            return Ex::error();
        };
        // A key the store can order by is asked of it first: over a collection, and over a list
        // that may be one (a parameter taking either keeps the collection it is given), which
        // is held in a local for the asking, and that the loop reads too.
        let native = match &list.ty {
            Ty::Collection(_) | Ty::List(_) => self.sort_key(elem, &param.name, &body),
            _ => None,
        };
        let (list, held) = match native {
            Some(_) => {
                let ty = list.ty.clone();
                let local = self.fresh(ty.clone());
                (Ex::local(local, ty), Some((local, list)))
            }
            None => (list, None),
        };
        let mut fl = self.new_for(list, elem);
        self.push_sink();
        self.bind_local(&param.name, fl.item_local);
        let key = match &native {
            // A version sorts by its time; none comes first.
            Some(super::super::order::SortKey::Version { last }) => {
                self.version_time(Ex::local(fl.item_local, elem.clone()), *last)
            }
            _ => self.expr(&body, None),
        };
        // A Date or a Time is ISO text, which sorts in time order; one that may be absent is
        // read once, its presence and its value telling where the item stands: the items
        // without one stand after those with one, whatever the direction. When a field last
        // changed and one never did, it stands where no moment does: first, and last with
        // `desc`, as the index keeps it.
        let changed = matches!(&native, Some(super::super::order::SortKey::Changed { .. }));
        let (key, present) = match key.ty.clone() {
            Ty::Date | Ty::Time => (key.typed(Ty::Text), None),
            Ty::Option(ref inner) if matches!(**inner, Ty::Date | Ty::Time) => {
                let inner = (**inner).clone();
                let ty = key.ty.clone();
                let held = self.fresh(ty.clone());
                self.emit(Stmt::Set(held, key));
                let present = Ex::host(Imp::IsSome, vec![Ex::local(held, ty.clone())], Ty::Bool);
                let value =
                    Ex::host(Imp::UnwrapH, vec![Ex::local(held, ty)], inner).typed(Ty::Text);
                let key = Ex::new(
                    Ek::If(
                        Box::new(present.clone()),
                        Box::new(value),
                        Box::new(Ex::str("")),
                    ),
                    Ty::Text,
                );
                (key, if changed { None } else { Some(present) })
            }
            _ => (key, None),
        };
        if !matches!(
            key.ty,
            Ty::Int | Ty::Float | Ty::Text | Ty::Bool | Ty::Error
        ) {
            let t = self.show(&key.ty);
            self.err(
                "E020",
                name.span,
                format!("`sort_by` keys must be Int, Float, Text, Bool, Date or Time, found {t}"),
            );
        }
        let out_ty = Ty::List(Box::new(elem.clone()));
        let keys_ty = Ty::List(Box::new(key.ty.clone()));
        let keys = self.fresh(keys_ty.clone());
        // The items that have a value go to a list of their own, sorted there, and those
        // without one follow them in the order the list holds them, whichever direction: a task
        // nobody gave a day is no day's work, not the first one's.
        let kept = self.fresh(out_ty.clone());
        let missing = self.fresh(out_ty.clone());
        if let Some(present) = &present {
            let item = || Ex::local(fl.item_local, elem.clone());
            let push_keys = Stmt::Eval(Self::push_item(Ex::local(keys, keys_ty.clone()), key));
            let push_kept = Self::push_item(Ex::local(kept, out_ty.clone()), item());
            self.emit(Stmt::Eval(Ex::new(
                Ek::If(
                    Box::new(present.clone()),
                    Box::new(Ex::new(
                        Ek::Seq(vec![push_keys], Box::new(push_kept)),
                        Ty::Unit,
                    )),
                    Box::new(Self::push_item(Ex::local(missing, out_ty.clone()), item())),
                ),
                Ty::Unit,
            )));
        } else {
            self.emit(Stmt::Eval(Self::push_item(
                Ex::local(keys, keys_ty.clone()),
                key,
            )));
        }
        let mut inner = self.pop_sink();
        fl.body.append(&mut inner);
        let list_local = fl.list_local;
        let mut stmts = vec![Stmt::Set(
            keys,
            Ex::host(Imp::ListNew, Vec::new(), keys_ty.clone()),
        )];
        if present.is_some() {
            for keep in [kept, missing] {
                stmts.push(Stmt::Set(
                    keep,
                    Ex::host(Imp::ListNew, Vec::new(), out_ty.clone()),
                ));
            }
        }
        stmts.push(Stmt::ForList(Box::new(fl)));
        let source = if present.is_some() { kept } else { list_local };
        let mut sorted = Ex::host(
            Imp::ListSortByKey,
            vec![Ex::local(source, Ty::Text), Ex::local(keys, keys_ty)],
            out_ty.clone(),
        );
        if desc {
            sorted = Ex::host(Imp::ListReverse, vec![sorted], out_ty.clone());
        }
        let ordered = self.fresh(out_ty.clone());
        stmts.push(Stmt::Set(ordered, sorted));
        if present.is_some() {
            let mut tail = self.new_for(Ex::local(missing, out_ty.clone()), elem);
            tail.body.push(Stmt::Eval(Self::push_item(
                Ex::local(ordered, out_ty.clone()),
                Ex::local(tail.item_local, elem.clone()),
            )));
            stmts.push(Stmt::ForList(Box::new(tail)));
        }
        let looped = Ex::new(
            Ek::Seq(stmts, Box::new(Ex::local(ordered, out_ty.clone()))),
            out_ty,
        );
        match (native, held) {
            (Some(key), Some((local, held))) => {
                self.ordered_or_loop(local, held, &key, desc, looped)
            }
            _ => looped,
        }
    }

    /// The body of `filter`, `any`, `all`, `find` and `count`: tests the lambda on the item.
    fn filter_like(&mut self, m: &str, body: &ast::Expr, item: &Ex, elem: &Ty) -> (u32, Ty) {
        let pred = self.expr_as(body, &Ty::Bool);
        let (out_ty, on_true, on_false): (Ty, Vec<Stmt>, Vec<Stmt>);
        let out;
        match m {
            "filter" => {
                out_ty = Ty::List(Box::new(elem.clone()));
                out = self.fresh(out_ty.clone());
                on_true = vec![Stmt::Eval(Self::push_item(
                    Ex::local(out, out_ty.clone()),
                    item.clone(),
                ))];
                on_false = Vec::new();
            }
            "any" => {
                out_ty = Ty::Bool;
                out = self.fresh(Ty::Bool);
                on_true = vec![
                    Stmt::Set(out, Ex::new(Ek::Bool(true), Ty::Bool)),
                    Stmt::Break,
                ];
                on_false = Vec::new();
            }
            "all" => {
                out_ty = Ty::Bool;
                out = self.fresh(Ty::Bool);
                on_true = Vec::new();
                on_false = vec![
                    Stmt::Set(out, Ex::new(Ek::Bool(false), Ty::Bool)),
                    Stmt::Break,
                ];
            }
            "count" => {
                out_ty = Ty::Int;
                out = self.fresh(Ty::Int);
                let next = Ex::new(
                    Ek::Bin(
                        ir::BinOp::Add,
                        Box::new(Ex::local(out, Ty::Int)),
                        Box::new(Ex::int(1)),
                    ),
                    Ty::Int,
                );
                on_true = vec![Stmt::Set(out, next)];
                on_false = Vec::new();
            }
            _ => {
                out_ty = Ty::Option(Box::new(elem.clone()));
                out = self.fresh(out_ty.clone());
                on_true = vec![
                    Stmt::Set(out, Self::some(item.clone(), &out_ty)),
                    Stmt::Break,
                ];
                on_false = Vec::new();
            }
        }
        self.emit(Stmt::If(pred, on_true, on_false));
        (out, out_ty)
    }
}
