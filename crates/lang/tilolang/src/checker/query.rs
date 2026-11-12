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

    /// `filters(T)`: the filters of `T` as a list's places and search's chips show them, each a
    /// `Filter<T>`: a value code names (`T.today`), compares and queries with.
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
            Ty::List(Box::new(Ty::Filter(key))),
        )
    }

    /// The filter `name` of the type `key`, with the value `args` give it, when `key` declares
    /// one by that name: `Task.today`, `Task.project(p)`, `Task.done(false)`. `None` when `key`
    /// is a type that declares no filters at all, so the name is something else.
    pub(super) fn filter_value(
        &mut self,
        key: &TypeKey,
        name: &ast::Ident,
        args: Option<&[ast::Arg]>,
        span: Span,
    ) -> Option<Ex> {
        let env = self.env;
        let info = env.type_info(key)?;
        if info.search.filters.is_empty() {
            return None;
        }
        let filters = &info.search.filters;
        let Some(f) = info.search.filter(&name.name).cloned() else {
            let keys: Vec<String> = filters
                .iter()
                .map(|f| format!("`{}.{}`", key.name, f.key))
                .collect();
            let words = filters.iter().map(|f| (f.key.as_str(), f.doc.as_str()));
            let hint = crate::diag::suggestion_about(&name.name, words);
            self.err(
                "E105",
                name.span,
                format!(
                    "`{}` declares no filter `{}`{hint}; its filters: {}",
                    key.name,
                    name.name,
                    keys.join(", ")
                ),
            );
            return Some(Ex::error());
        };
        let id = info.id.clone();
        let want = filter_value_ty(info, &f);
        let value = match (want, args) {
            (_, None) => Ex::host(Imp::NoneV, Vec::new(), Ty::Option(Box::new(Ty::Text))),
            (Some(want), Some([one])) if one.name.is_none() => {
                let v = self.expr_as(&one.value, &want);
                if v.ty.is_error() {
                    return Some(Ex::error());
                }
                Self::some(v, &Ty::Option(Box::new(want)))
            }
            (Some(want), Some(args)) => {
                let t = self.show(&want);
                self.err(
                    "E022",
                    span,
                    format!(
                        "`{}.{}` takes its value as one argument: `{}.{}({t})`",
                        key.name, f.key, key.name, f.key
                    ),
                );
                for a in args {
                    let _ = self.expr(&a.value, None);
                }
                return Some(Ex::error());
            }
            (None, Some(args)) => {
                self.err(
                    "E022",
                    span,
                    format!(
                        "`{}.{}` is a toggle: name it without a value, `{}.{}`",
                        key.name, f.key, key.name, f.key
                    ),
                );
                for a in args {
                    let _ = self.expr(&a.value, None);
                }
                return Some(Ex::error());
            }
        };
        Some(Ex::host(
            Imp::FilterLit,
            vec![Ex::str(&id), Ex::str(&f.key), value],
            Ty::Filter(key.clone()),
        ))
    }

    /// What choosing the filter value `v` asks, as search writes it: `is:today`,
    /// `project:<id>` (`host.Filter`'s `query`).
    pub(super) fn filter_query(v: Ex) -> Ex {
        let at = crate::host::value_field("Filter", "query")
            .map_or(3, |at| i32::try_from(at).unwrap_or(3));
        Ex::host(
            Imp::TupleGetH,
            vec![v, Ex::new(Ek::I32(at), Ty::Bool)],
            Ty::Text,
        )
    }

    /// The value `T.name` is where code reads a member of a type: the filter the type declares
    /// (`Task.today`), or, where a `Sort<T>` is asked, the order by one of its fields
    /// (`sorts: [Task.due]`).
    pub(super) fn type_member(
        &mut self,
        key: &TypeKey,
        name: &ast::Ident,
        exp: Option<&Ty>,
        span: Span,
    ) -> Option<Ex> {
        match exp {
            Some(Ty::Sort(of)) if of == key => Some(self.sort_value(key, name, span)),
            _ => self.filter_value(key, name, None, span),
        }
    }

    /// The order by the field `name` of `key`, as a `Sort<T>`: the menu entry's label comes
    /// from the field, and the order from its kind (`sort:title`, `sort:-received`).
    fn sort_value(&mut self, key: &TypeKey, name: &ast::Ident, span: Span) -> Ex {
        let env = self.env;
        let Some(info) = env.type_info(key) else {
            return Ex::error();
        };
        let ordered_fields: Vec<(&str, &str)> = info
            .fields
            .iter()
            .filter(|f| ordered(&f.ty))
            .map(|f| (f.name.as_str(), f.doc.lines().next().unwrap_or_default()))
            .collect();
        let Some(f) = info.field(&name.name) else {
            let hint = crate::diag::suggestion_about(&name.name, ordered_fields.clone());
            let names: Vec<String> = ordered_fields
                .iter()
                .map(|(name, _)| format!("`{}.{name}`", key.name))
                .collect();
            let known = if names.is_empty() {
                "it has no field with an order".to_owned()
            } else {
                format!("the fields of it with an order: {}", names.join(", "))
            };
            self.err(
                "E105",
                name.span,
                format!("`{}` has no field `{}`{hint}; {known}", key.name, name.name),
            );
            return Ex::error();
        };
        if !ordered(&f.ty) {
            let t = f.ty.display(&env.app);
            self.err(
                "E105",
                span,
                format!(
                    "`{}.{}` is {t}, which has no order: sort by a Date, a Time or a number, or a Text",
                    key.name, f.name
                ),
            );
            return Ex::error();
        }
        Ex::host(
            Imp::SortLit,
            vec![Ex::str(&info.id), Ex::str(&f.name)],
            Ty::Sort(key.clone()),
        )
    }

    /// The query text the filter values in `args` give, joined with spaces: a query asks all of
    /// them. With `text_too`, an argument may also be the query as people type it in search
    /// (`is:done`), which a list takes beside its filters.
    fn query_fragments(
        &mut self,
        what: &str,
        key: &TypeKey,
        args: &[ast::Arg],
        text_too: bool,
    ) -> Ex {
        let mut parts: Vec<Ex> = Vec::new();
        for a in args {
            let v = self.expr(&a.value, None);
            if v.ty.is_error() {
                continue;
            }
            match v.ty.clone() {
                Ty::Filter(of) if of == *key => parts.push(Self::filter_query(v)),
                Ty::Filter(of) => {
                    let (found, want) = (
                        self.show(&Ty::Filter(of)),
                        self.show(&Ty::Filter(key.clone())),
                    );
                    self.err(
                        "E105",
                        a.value.span,
                        format!(
                            "`{what}` takes the filters of {}; found {found}, a filter of {want}",
                            key.name
                        ),
                    );
                }
                Ty::Text | Ty::Date | Ty::Time if text_too => parts.push(v.typed(Ty::Text)),
                other => {
                    let t = self.show(&other);
                    let hint = if text_too {
                        "a filter of the type, or the query written as people type it in search (`is:done`)"
                    } else {
                        "a filter of the type"
                    };
                    self.err(
                        "E105",
                        a.value.span,
                        format!("`{what}` takes {hint}; found {t}"),
                    );
                }
            }
        }
        let mut parts = parts.into_iter();
        let Some(first) = parts.next() else {
            return Ex::str("");
        };
        parts.fold(first, |acc, part| {
            let joined = Ex::host(Imp::StrConcat, vec![acc, Ex::str(" ")], Ty::Text);
            Ex::host(Imp::StrConcat, vec![joined, part], Ty::Text)
        })
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
        let mut held: Vec<ast::Arg> = Vec::new();
        for a in args.iter().skip(1) {
            let Some(name) = &a.name else {
                // A filter given as a value: `query(Task, in: tasks, Task.today)`.
                held.push(a.clone());
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
                        let hint = crate::diag::suggestion_about(
                            filter,
                            info.search
                                .filters
                                .iter()
                                .map(|f| (f.key.as_str(), f.doc.as_str()))
                                .chain([("in", ""), ("sort", "")]),
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
        let text = self.query_fragments("query", &key, &held, false);
        Ex::host(
            Imp::Query,
            vec![Ex::str(&info.id), scope, pairs, text],
            list_ty,
        )
    }

    /// `items.matching(q)`: the items of a list of resources that `q` holds of, each argument a
    /// filter of their type (`Task.today`, `Task.project(p)`) or the query written as people
    /// type it in search (`is:done due:-7d`), all of them asked together; all of them, as they
    /// were, for none. A collection read in a view stays one, which the index answers.
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
        if args.is_empty() {
            self.err(
                "E022",
                span,
                "`matching` takes the query: `items.matching(Task.today)`",
            );
            return Ex::error();
        }
        let text = self.query_fragments("matching", key, args, true);
        let id = self
            .env
            .type_info(key)
            .map(|t| t.id.clone())
            .unwrap_or_default();
        let list_ty = list.ty.clone();
        let pairs = self.text_pairs(Vec::new());
        Ex::host(Imp::Query, vec![Ex::str(&id), list, pairs, text], list_ty)
    }

    /// A list of `(key, value)` text pairs, as the runtime reads a query's terms and a
    /// message's arguments.
    pub(super) fn text_pairs(&mut self, pairs: Vec<(String, Ex)>) -> Ex {
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
            // Who it names: `me` for the person looking, else a member by their id.
            FilterKind::Person => {
                if let ExprKind::Ident(name) = &e.kind
                    && name == "me"
                    && self.lookup(name).is_none()
                {
                    return Some(Ex::str("me"));
                }
                Some(self.expr_as(e, &Ty::Text))
            }
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
                ExprKind::Call { callee, args, .. } if args.is_empty() => match &callee.kind {
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
    /// `.name`, `.media`, `.size` and `.url` of a `File`: what it is called, the media type it
    /// holds, how many bytes it is, and where its bytes are (`.text()` reads them).
    pub(super) fn file_part(&mut self, v: Ex, name: &ast::Ident) -> Ex {
        if name.name == "size" {
            return Ex::host(Imp::FileSize, vec![v], Ty::Int);
        }
        let part = match name.name.as_str() {
            "name" => 0,
            "media" => 1,
            "url" => 2,
            other => {
                self.err(
                    "E021",
                    name.span,
                    format!(
                        "a `File` has the fields `name`, `media`, `size` and `url`, not `{other}`; `.text()` reads its content and `.files()` the files an archive holds"
                    ),
                );
                return Ex::error();
            }
        };
        Ex::host(
            Imp::FilePart,
            vec![v, Ex::new(Ek::I32(part), Ty::Bool)],
            Ty::Text,
        )
    }

    /// `.excerpt`, `.text`, `.markdown`, `.references` and `.people` of a `RichText`.
    pub(super) fn rich_part(&mut self, v: Ex, name: &ast::Ident) -> Ex {
        let (part, ty) = match name.name.as_str() {
            "excerpt" => (0, Ty::Text),
            "text" => (1, Ty::Text),
            "markdown" => (4, Ty::Text),
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
                        "`RichText` has the members `excerpt`, `text`, `markdown`, `references` and `people`, not `{other}`"
                    ),
                );
                return Ex::error();
            }
        };
        Ex::host(Imp::RichPart, vec![v, Ex::new(Ek::I32(part), Ty::Bool)], ty)
    }
}

/// Whether a field of this type has an order, and so is one a window may sort by: a Date, a
/// Time or a number, the latest or largest first, or a Text, A to Z.
fn ordered(ty: &Ty) -> bool {
    match ty {
        Ty::Date | Ty::Time | Ty::Int | Ty::Float | Ty::Text | Ty::Identifier => true,
        Ty::Option(inner) => ordered(inner),
        _ => false,
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

/// The type of the value a filter takes, when it takes one: its field's type without the
/// `Option` or `Set` around it — `Task.project(p)` takes a `Project`, `Task.done(false)` a
/// `Bool`, `Task.priority(.high)` a case of its enum. A named filter is on a field of its own
/// and takes none.
fn filter_value_ty(info: &TypeInfo, f: &FilterMeta) -> Option<Ty> {
    let id = f.field.as_ref()?;
    let field = info.fields.iter().find(|field| &field.id == id)?;
    let mut ty = field.ty.clone();
    loop {
        match ty {
            Ty::Option(inner) | Ty::Set(inner) => ty = *inner,
            held => return Some(held),
        }
    }
}
