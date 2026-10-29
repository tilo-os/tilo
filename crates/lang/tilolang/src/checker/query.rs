//! `query(T, in: x, done: false, due: today..today + 7d, sort: .due_oldest)`, a list's
//! `items.matching("is:done")` and `filters(T)` (search.md, "A query" and "Where search appears").
//!
//! A query's arguments are the type's filters, checked against what each compares. Each is
//! lowered to the text a typed search writes for it (`done` and `false`, `due` and
//! `today..today+7d`), so a query in code and the same query typed (`is:done due:-7d`) are
//! one query: the runtime resolves both against the type's filters when it is answered, and the
//! index answers it.

use super::{Checker, Ex};
use crate::abi::Imp;
use crate::ast::{self, BinOp, ExprKind, UnOp};
use crate::diag::Span;
use crate::ir::{Ek, Stmt};
use crate::meta::{FilterKind, FilterMeta, SortOrder};
use crate::resolve::TypeInfo;
use crate::types::{Ty, TypeKey};

impl Checker<'_> {
    /// The record type a query's first argument names.
    fn queried(&mut self, what: &str, args: &[ast::Arg], span: Span) -> Option<TypeKey> {
        let Some(first) = args.first().filter(|a| a.name.is_none()) else {
            self.err(
                "E105",
                span,
                format!("`{what}` takes the type first: `{what}(Todo, ...)`"),
            );
            return None;
        };
        let named = self
            .type_named(&first.value)
            .or_else(|| self.program_type(&first.value));
        match named {
            Some(Ty::Record(key)) if self.env.type_info(&key).is_some() => Some(key),
            Some(Ty::Error) => None,
            other => {
                let shown = other.map_or_else(
                    || crate::print::expr_source(&first.value),
                    |t| self.show(&t),
                );
                self.err(
                    "E105",
                    first.value.span,
                    format!("`{what}` takes a record type first; `{shown}` is none"),
                );
                None
            }
        }
    }

    /// `filters(T)`: the filters of `T` as a list's places and search's chips show them, each
    /// a `host.Filter`.
    pub(super) fn builtin_filters(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let Some(key) = self.queried("filters", args, span) else {
            return Ex::error();
        };
        if args.len() > 1 {
            self.err(
                "E022",
                span,
                "`filters` takes the type alone: `filters(Todo)`",
            );
        }
        let id = self
            .env
            .type_info(&key)
            .map(|t| t.id.clone())
            .unwrap_or_default();
        Ex::host(
            Imp::Filters,
            vec![Ex::str(&id)],
            Ty::List(Box::new(super::platform::host_ty("Filter"))),
        )
    }

    /// `query(T, in: x, filter: value, ..., sort: .field_order)`: the resources of `T` the
    /// filters hold of, inside `x` (or anywhere in the workspace), as a lazy list the index
    /// answers.
    pub(super) fn builtin_query(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        self.query_named("query", args, span)
    }

    /// `query(...)`, or a program's typed `find(...)`, which is the same query.
    pub(super) fn query_named(&mut self, what: &str, args: &[ast::Arg], span: Span) -> Ex {
        let Some(key) = self.queried(what, args, span) else {
            for a in args.iter().skip(1) {
                if !matches!(
                    a.value.kind,
                    ExprKind::Range { .. } | ExprKind::Duration(..)
                ) {
                    let _ = self.expr(&a.value, None);
                }
            }
            return Ex::error();
        };
        let env = self.env;
        let Some(info) = env.type_info(&key) else {
            return Ex::error();
        };
        let list_ty = Ty::List(Box::new(Ty::Record(key.clone())));
        let mut scope: Option<Ex> = None;
        let mut terms: Vec<(String, Ex)> = Vec::new();
        for a in args.iter().skip(1) {
            let Some(name) = &a.name else {
                self.err(
                    "E105",
                    a.value.span,
                    format!(
                        "a query's arguments name the filter they set: `query({}, {}: ...)`",
                        key.name,
                        info.search.filters.first().map_or("in", |f| f.key.as_str())
                    ),
                );
                continue;
            };
            match name.name.as_str() {
                "in" => scope = self.query_scope(info, &a.value),
                "sort" => {
                    if let Some(text) = self.query_sort(info, &a.value) {
                        terms.push(("sort".to_owned(), Ex::str(&text)));
                    }
                }
                filter => {
                    let Some(f) = info.search.filter(filter).cloned() else {
                        let keys: Vec<&str> =
                            info.search.filters.iter().map(|f| f.key.as_str()).collect();
                        let keys = if keys.is_empty() {
                            "it has none".to_owned()
                        } else {
                            format!("its filters: {}", keys.join(", "))
                        };
                        let hint = crate::diag::suggestion(
                            filter,
                            info.search
                                .filters
                                .iter()
                                .map(|f| f.key.as_str())
                                .chain(["in", "sort"]),
                        );
                        self.err(
                            "E105",
                            name.span,
                            format!(
                                "`{}` has no filter `{filter}`{hint}; {keys}, and `in:` and `sort:`",
                                key.name
                            ),
                        );
                        continue;
                    };
                    if terms.iter().any(|(k, _)| k == filter) {
                        self.err(
                            "E105",
                            name.span,
                            format!("`{filter}` is set twice; a query sets each filter once"),
                        );
                        continue;
                    }
                    if let Some(value) = self.query_value(info, &f, &a.value) {
                        terms.push((filter.to_owned(), value));
                    }
                }
            }
        }
        // With no order asked, the type's own.
        if !terms.iter().any(|(k, _)| k == "sort")
            && let Some(sort) = &info.search.sort
        {
            let text = if sort.order.descending() {
                format!("-{}", sort.key)
            } else {
                sort.key.clone()
            };
            terms.push(("sort".to_owned(), Ex::str(&text)));
        }
        let in_view = matches!(self.st.ctx, super::Ctx::View);
        if scope.is_none() && in_view {
            self.err(
                "E105",
                span,
                format!(
                    "a query in a view looks inside a resource: `query({}, in: x, ...)`; one over the whole workspace is search's",
                    key.name
                ),
            );
        }
        // No scope: an Option the host reads as none (a handle, so the call has its argument).
        let scope = scope
            .unwrap_or_else(|| Ex::host(Imp::NoneV, Vec::new(), Ty::Option(Box::new(Ty::Text))));
        let pairs = self.text_pairs(terms);
        Ex::host(
            Imp::Query,
            vec![Ex::str(&info.id), scope, pairs, Ex::str("")],
            list_ty,
        )
    }

    /// `items.matching(q)`: the items of a list of resources that the query `q`, written as
    /// people type it in search (`is:done due:-7d`), holds of; all of them, as they were, for
    /// an empty query. A collection read in a view stays one, which the index answers.
    pub(super) fn matching(&mut self, list: Ex, elem: &Ty, args: &[ast::Arg], span: Span) -> Ex {
        let Ty::Record(key) = elem else {
            let t = self.show(elem);
            self.err(
                "E105",
                span,
                format!("`matching` asks a query of a list of resources; these are {t}"),
            );
            return Ex::error();
        };
        let [q] = args else {
            self.err(
                "E022",
                span,
                "`matching` takes the query: `items.matching(\"is:done\")`",
            );
            return Ex::error();
        };
        let text = self.expr_as(&q.value, &Ty::Text);
        let id = self
            .env
            .type_info(key)
            .map(|t| t.id.clone())
            .unwrap_or_default();
        let list_ty = list.ty.clone();
        let pairs = self.text_pairs(Vec::new());
        Ex::host(Imp::Query, vec![Ex::str(&id), list, pairs, text], list_ty)
    }

    /// A list of `(key, value)` text pairs, as the runtime reads a query's terms.
    fn text_pairs(&mut self, pairs: Vec<(String, Ex)>) -> Ex {
        let pair = Ty::Tuple(vec![Ty::Text, Ty::Text]);
        let list_ty = Ty::List(Box::new(pair.clone()));
        let list = self.fresh(list_ty.clone());
        let mut stmts = vec![Stmt::Set(
            list,
            Ex::host(Imp::ListNew, Vec::new(), list_ty.clone()),
        )];
        for (key, value) in pairs {
            let t = self.fresh(pair.clone());
            stmts.push(Stmt::Set(
                t,
                Ex::host(
                    Imp::TupleNew,
                    vec![Ex::new(Ek::I32(2), Ty::Bool)],
                    pair.clone(),
                ),
            ));
            for (index, item) in [Ex::str(&key), value].into_iter().enumerate() {
                stmts.push(Stmt::Eval(Ex::host(
                    Imp::TupleSetH,
                    vec![
                        Ex::local(t, pair.clone()),
                        Ex::new(Ek::I32(i32::try_from(index).unwrap_or(0)), Ty::Bool),
                        item,
                    ],
                    Ty::Unit,
                )));
            }
            stmts.push(Stmt::Eval(Ex::host(
                Imp::ListPushH,
                vec![Ex::local(list, list_ty.clone()), Ex::local(t, pair.clone())],
                Ty::Unit,
            )));
        }
        Ex::new(
            Ek::Seq(stmts, Box::new(Ex::local(list, list_ty.clone()))),
            list_ty,
        )
    }

    /// `in: x`: the resource whose collection of `T` the query looks in (its one collection of
    /// them), or such a collection.
    fn query_scope(&mut self, info: &TypeInfo, e: &ast::Expr) -> Option<Ex> {
        let item = Ty::Record(info.key.clone());
        let x = self.expr(e, None);
        match x.ty.clone() {
            Ty::Error => None,
            Ty::List(t) | Ty::Collection(t) if t.same(&item) => Some(x),
            Ty::Record(owner) => {
                let env = self.env;
                let holder = env.type_info(&owner)?;
                let colls: Vec<&crate::resolve::FieldInfo> = holder
                    .fields
                    .iter()
                    .filter(|f| matches!(&f.ty, Ty::Collection(t) if t.same(&item)))
                    .collect();
                match colls.as_slice() {
                    [one] => {
                        let name = ast::Ident {
                            name: one.name.clone(),
                            span: e.span,
                        };
                        Some(self.field_read(x, &name))
                    }
                    [] => {
                        self.err(
                            "E105",
                            e.span,
                            format!(
                                "`{}` holds no collection of {}; `in:` names a resource that does, or the collection",
                                owner.name, info.key.name
                            ),
                        );
                        None
                    }
                    several => {
                        let names: Vec<String> = several
                            .iter()
                            .map(|f| format!("`in: x.{}`", f.name))
                            .collect();
                        self.err(
                            "E105",
                            e.span,
                            format!(
                                "`{}` holds several collections of {}; name one: {}",
                                owner.name,
                                info.key.name,
                                names.join(" or ")
                            ),
                        );
                        None
                    }
                }
            }
            other => {
                let t = self.show(&other);
                self.err(
                    "E105",
                    e.span,
                    format!(
                        "`in:` takes a resource holding {}s, or a collection of them; this is {t}",
                        info.key.name
                    ),
                );
                None
            }
        }
    }

    /// `sort: .due_oldest`: the order as a typed search writes it (`due`, `-due`).
    fn query_sort(&mut self, info: &TypeInfo, e: &ast::Expr) -> Option<String> {
        let offered = sorts(info);
        let ExprKind::Variant { name, args: None } = &e.kind else {
            self.err(
                "E105",
                e.span,
                format!(
                    "`sort:` takes an order: {}",
                    offered
                        .iter()
                        .map(|o| format!("`.{o}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
            return None;
        };
        let found = SortOrder::ALL.into_iter().find_map(|order| {
            let field = name.name.strip_suffix(&format!("_{}", order.word()))?;
            let f = info.field(field)?;
            offered
                .contains(&name.name)
                .then(|| (f.name.clone(), order))
        });
        match found {
            Some((field, order)) => Some(if order.descending() {
                format!("-{field}")
            } else {
                field
            }),
            None => {
                let list = if offered.is_empty() {
                    format!("`{}` has no field to sort by", info.key.name)
                } else {
                    format!(
                        "its orders: {}",
                        offered
                            .iter()
                            .map(|o| format!("`.{o}`"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                self.err(
                    "E105",
                    name.span,
                    format!(
                        "`.{}` is not an order of `{}`; {list}",
                        name.name, info.key.name
                    ),
                );
                None
            }
        }
    }

    /// The value of the filter `f` a query sets, as a typed search writes it.
    fn query_value(&mut self, info: &TypeInfo, f: &FilterMeta, e: &ast::Expr) -> Option<Ex> {
        let field_ty = f
            .field
            .as_ref()
            .and_then(|id| info.fields.iter().find(|x| &x.id == id))
            .map(|x| x.ty.clone());
        // What one value of the filter is: a set's element, an option's inner value.
        let one = match &field_ty {
            Some(Ty::Set(t) | Ty::Option(t)) => (**t).clone(),
            Some(t) => t.clone(),
            None => Ty::Bool,
        };
        if let Some(word) = self.absence(e) {
            if f.optional {
                return Some(Ex::str(word));
            }
            self.err(
                "E105",
                e.span,
                format!(
                    "`{}` always holds a value; `{word}` is for a filter on an option or a set",
                    f.key
                ),
            );
            return None;
        }
        match f.kind {
            FilterKind::Bool | FilterKind::Predicate => {
                let v = self.expr_as(e, &Ty::Bool);
                Some(self.text_of(v, e.span))
            }
            FilterKind::Date | FilterKind::Time => self.dated_value(&one, e),
            FilterKind::Number => self.number_value(&one, e),
            FilterKind::Enum => self.enum_value(&one, e),
            FilterKind::Reference => {
                let v = self.expr_as(e, &one);
                if v.ty.is_error() {
                    return None;
                }
                Some(Ex::host(Imp::RefId, vec![v], Ty::Text))
            }
            FilterKind::Identifier => self.identifier_value(e),
            FilterKind::Text => Some(self.expr_as(e, &Ty::Text)),
        }
    }

    /// `none` or `any` (or `.none`) where a filter may hold no value.
    fn absence(&self, e: &ast::Expr) -> Option<&'static str> {
        match &e.kind {
            ExprKind::Ident(w) if self.lookup(w).is_none() && w == "none" => Some("none"),
            ExprKind::Ident(w) if self.lookup(w).is_none() && w == "any" => Some("any"),
            ExprKind::Variant { name, args: None } if name.name == "none" => Some("none"),
            _ => None,
        }
    }

    /// A Date or Time filter's value: a day or an instant, `today`, `now`, either plus or
    /// minus a duration, a duration from now (`-7d`), or a range of them.
    fn dated_value(&mut self, one: &Ty, e: &ast::Expr) -> Option<Ex> {
        if let ExprKind::Range { lo, hi } = &e.kind {
            let lo = match lo {
                Some(x) => self.dated_point(one, x)?,
                None => Ex::str(""),
            };
            let hi = match hi {
                Some(x) => self.dated_point(one, x)?,
                None => Ex::str(""),
            };
            return Some(concat(vec![lo, Ex::str(".."), hi]));
        }
        self.dated_point(one, e)
    }

    fn dated_point(&mut self, one: &Ty, e: &ast::Expr) -> Option<Ex> {
        if let Some(text) = self.relative(e) {
            return Some(Ex::str(&text));
        }
        // A month (`2026-08`) is all of its days, as typed search reads it.
        if let ExprKind::Calendar(text) = &e.kind
            && text.len() == 7
        {
            return Some(Ex::str(text));
        }
        let v = self.expr(e, Some(one));
        match &v.ty {
            Ty::Date | Ty::Time => Some(v.typed(Ty::Text)),
            Ty::Error => None,
            other => {
                let t = self.show(other);
                self.err(
                    "E105",
                    e.span,
                    format!("a {} filter takes a Date or a Time, `today`, `now`, a duration such as `-7d`, or a range of them; this is {t}", self.show(one)),
                );
                None
            }
        }
    }

    /// `today`, `now`, `today + 7d`, `-7d`: a time relative to when the query is answered, as
    /// a typed search writes it.
    fn relative(&self, e: &ast::Expr) -> Option<String> {
        let anchor = |e: &ast::Expr| -> Option<&'static str> {
            match &e.kind {
                ExprKind::Ident(w) if self.lookup(w).is_none() && w == "today" => Some("today"),
                ExprKind::Ident(w) if self.lookup(w).is_none() && w == "now" => Some("now"),
                ExprKind::Call { callee, args } if args.is_empty() => match &callee.kind {
                    ExprKind::Ident(w) if w == "today" => Some("today"),
                    _ => None,
                },
                _ => None,
            }
        };
        let duration = |e: &ast::Expr| -> Option<(i64, &'static str)> {
            match &e.kind {
                ExprKind::Duration(n, u) => Some((*n, *u)),
                ExprKind::Unary {
                    op: UnOp::Neg,
                    operand,
                } => match &operand.kind {
                    ExprKind::Duration(n, u) => Some((n.checked_neg()?, *u)),
                    _ => None,
                },
                _ => None,
            }
        };
        if let Some(word) = anchor(e) {
            return Some(word.to_owned());
        }
        if let Some((n, u)) = duration(e) {
            return Some(format!("{n:+}{u}"));
        }
        if let ExprKind::Binary {
            op: op @ (BinOp::Add | BinOp::Sub),
            lhs,
            rhs,
        } = &e.kind
            && let Some(word) = anchor(lhs)
            && let Some((n, u)) = duration(rhs)
        {
            let n = if *op == BinOp::Sub {
                n.checked_neg()?
            } else {
                n
            };
            return Some(format!("{word}{n:+}{u}"));
        }
        None
    }

    fn number_value(&mut self, one: &Ty, e: &ast::Expr) -> Option<Ex> {
        let point = |this: &mut Self, x: &ast::Expr| -> Option<Ex> {
            let v = this.expr(x, Some(one));
            match &v.ty {
                Ty::Int | Ty::Float => Some(this.text_of(v, x.span)),
                Ty::Error => None,
                other => {
                    let t = this.show(other);
                    this.err(
                        "E105",
                        x.span,
                        format!("a number filter takes a number or a range of them; this is {t}"),
                    );
                    None
                }
            }
        };
        if let ExprKind::Range { lo, hi } = &e.kind {
            let lo = match lo {
                Some(x) => point(self, x)?,
                None => Ex::str(""),
            };
            let hi = match hi {
                Some(x) => point(self, x)?,
                None => Ex::str(""),
            };
            return Some(concat(vec![lo, Ex::str(".."), hi]));
        }
        point(self, e)
    }

    /// An enum filter's value: a case, one of several (`[.open, .blocked]`), or an enum value.
    fn enum_value(&mut self, one: &Ty, e: &ast::Expr) -> Option<Ex> {
        let Ty::Enum(key) = one else {
            return None;
        };
        let env = self.env;
        let info = env.enum_info(key)?;
        let case = |this: &mut Self, x: &ast::Expr| -> Option<String> {
            let ExprKind::Variant { name, args: None } = &x.kind else {
                return None;
            };
            if info.variant(&name.name).is_none() {
                this.err(
                    "E105",
                    name.span,
                    format!(
                        "{} has no case `.{}`; its cases: {}",
                        key.name,
                        name.name,
                        info.variant_names()
                    ),
                );
            }
            Some(name.name.clone())
        };
        match &e.kind {
            ExprKind::List(items) => {
                let mut names = Vec::new();
                for item in items {
                    match case(self, item) {
                        Some(n) => names.push(n),
                        None => {
                            self.err(
                                "E105",
                                item.span,
                                format!("a list of cases of {} holds cases: `[.a, .b]`", key.name),
                            );
                            return None;
                        }
                    }
                }
                Some(Ex::str(&names.join(",")))
            }
            ExprKind::Variant { args: None, .. } => case(self, e).map(|n| Ex::str(&n)),
            _ => {
                let v = self.expr_as(e, one);
                if v.ty.is_error() {
                    return None;
                }
                self.enum_method(v, key, "name")
            }
        }
    }

    /// An identifier filter's value: an identifier, or a pattern of one (`*@x.com`), checked
    /// here when it is written out.
    fn identifier_value(&mut self, e: &ast::Expr) -> Option<Ex> {
        if let Some(text) = crate::resolve::plain_text(e) {
            return match crate::identifier::normalize_pattern(&text) {
                Ok(held) => Some(Ex::str(&held)),
                Err(why) => {
                    self.err("E105", e.span, why);
                    None
                }
            };
        }
        let v = self.expr(e, Some(&Ty::Identifier));
        match &v.ty {
            Ty::Identifier | Ty::Text => Some(v.typed(Ty::Text)),
            Ty::Error => None,
            other => {
                let t = self.show(other);
                self.err(
                    "E105",
                    e.span,
                    format!("an identifier filter takes an identifier or a pattern such as `\"*@example.com\"`; this is {t}"),
                );
                None
            }
        }
    }

    /// `.scheme` and `.value` of a `std.Identifier`.
    pub(super) fn identifier_part(&mut self, v: Ex, name: &ast::Ident) -> Ex {
        let part = match name.name.as_str() {
            "scheme" => 0,
            "value" => 1,
            other => {
                self.err(
                    "E021",
                    name.span,
                    format!("`std.Identifier` has the fields `scheme` and `value`, not `{other}`"),
                );
                return Ex::error();
            }
        };
        Ex::host(
            Imp::IdentPart,
            vec![v, Ex::new(Ek::I32(part), Ty::Bool)],
            Ty::Text,
        )
    }
}

impl Checker<'_> {
    /// `.excerpt`, `.text`, `.references` and `.people` of a `RichText`.
    pub(super) fn rich_part(&mut self, v: Ex, name: &ast::Ident) -> Ex {
        let (part, ty) = match name.name.as_str() {
            "excerpt" => (0, Ty::Text),
            "text" => (1, Ty::Text),
            "references" => (
                2,
                Ty::List(Box::new(Ty::Record(super::program::resource_key()))),
            ),
            "people" => (3, Ty::List(Box::new(Ty::Principal))),
            other => {
                self.err(
                    "E021",
                    name.span,
                    format!(
                        "`RichText` has the members `excerpt`, `text`, `references` and `people`, not `{other}`"
                    ),
                );
                return Ex::error();
            }
        };
        Ex::host(Imp::RichPart, vec![v, Ex::new(Ek::I32(part), Ty::Bool)], ty)
    }
}

/// The orders a query of `info` may ask for: `.field_order`, per field that has one.
fn sorts(info: &TypeInfo) -> Vec<String> {
    let mut out = Vec::new();
    for f in &info.fields {
        let held = match &f.ty {
            Ty::Option(t) => (**t).clone(),
            t => t.clone(),
        };
        let orders: &[SortOrder] = match held {
            Ty::Date | Ty::Time | Ty::Int | Ty::Float => &[SortOrder::Newest, SortOrder::Oldest],
            Ty::Text | Ty::Identifier => &[SortOrder::AZ, SortOrder::ZA],
            _ => &[],
        };
        for o in orders {
            out.push(format!("{}_{}", f.name, o.word()));
        }
    }
    out
}

/// Text pieces joined.
fn concat(parts: Vec<Ex>) -> Ex {
    let mut parts = parts.into_iter();
    let first = parts.next().unwrap_or_else(|| Ex::str(""));
    parts.fold(first, |acc, part| {
        Ex::host(Imp::StrConcat, vec![acc, part], Ty::Text)
    })
}
