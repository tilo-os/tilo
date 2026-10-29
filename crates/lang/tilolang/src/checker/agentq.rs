//! The query words of agents' programs (agents.md, "Querying" and "Reading and describing"):
//! `grep` over the text of every app the program may read, `find` typed by a type and its
//! filters, `read` of what matches or of one page, `describe`, `print` with the fields to show,
//! `me`, and the chaining functions `count_by`, `group`, `take`, `each` and `single`.
//!
//! An app the program's host does not hold is read through the platform: the checker names it
//! by its id, from the apps the session says the program may read ([`crate::resolve::Env::sources`]),
//! and the runtime asks that app's host. Its resources are `host.Resource`s.

use serde_json::json;

use super::{Checker, Ex, conv, load_op};
use crate::abi::Imp;
use crate::ast::{self, BinOp, ExprKind};
use crate::diag::Span;
use crate::ir::{Ek, Stmt};
use crate::types::{Ty, TypeKey};

/// The kinds of block `grep(not: ...)` leaves out.
pub const BLOCK_KINDS: &[&str] = &["paragraph", "heading", "quote", "list", "code", "field"];

impl Checker<'_> {
    /// The apps a program may name as sources, each by the name it calls it and its id: the
    /// packages it holds, then those the session reads through the platform.
    pub(super) fn source_apps(&self) -> Vec<(String, String)> {
        let mut apps: Vec<(String, String)> = self
            .env
            .imports
            .iter()
            .filter(|i| {
                i.app != crate::host::HOST_APP
                    && i.app != crate::toolkit::UI_APP
                    && i.alias != "std"
            })
            .map(|i| (i.alias.clone(), i.app.clone()))
            .collect();
        for (alias, app) in &self.env.sources {
            if !apps.iter().any(|(_, known)| known == app) {
                apps.push((alias.clone(), app.clone()));
            }
        }
        apps
    }

    /// The app the name `name` calls, among the sources.
    fn source_app(&self, name: &str) -> Option<String> {
        self.source_apps()
            .into_iter()
            .find(|(alias, app)| alias == name || app == name)
            .map(|(_, app)| app)
    }

    /// An error naming the apps a program may read, for a name that is none of them.
    fn no_source(&mut self, name: &str, span: Span) {
        let apps = self.source_apps();
        let names: Vec<&str> = apps.iter().map(|(alias, _)| alias.as_str()).collect();
        let hint = crate::diag::suggestion(name, names.iter().copied());
        self.err(
            "E106",
            span,
            format!(
                "`{name}` is no app this program may read{hint}; it may read {}",
                if names.is_empty() {
                    "none".to_owned()
                } else {
                    names.join(", ")
                }
            ),
        );
    }

    /// `in: notes + chat`, `in: n3`, `in: [n3, n4]`: the apps named, and the resources whose
    /// text to search, added to `apps` and `scope`.
    fn grep_sources(&mut self, e: &ast::Expr, apps: &mut Vec<String>, scope: &mut Vec<Ex>) {
        match &e.kind {
            ExprKind::Binary {
                op: BinOp::Add,
                lhs,
                rhs,
            } => {
                self.grep_sources(lhs, apps, scope);
                self.grep_sources(rhs, apps, scope);
            }
            ExprKind::Ident(name) if self.lookup(name).is_none() => {
                // A root of the program's own scope (`notes` as a package's named root) is a
                // resource; an app's name is the app.
                match self.source_app(name) {
                    Some(app) => {
                        if !apps.contains(&app) {
                            apps.push(app);
                        }
                    }
                    None => self.no_source(name, e.span),
                }
            }
            ExprKind::List(items) => {
                for item in items {
                    self.grep_sources(item, apps, scope);
                }
            }
            _ => {
                let x = self.expr(e, None);
                match &x.ty {
                    Ty::Record(key) if !crate::host::is_value(key) => scope.push(x),
                    Ty::List(t) | Ty::Collection(t) if matches!(&**t, Ty::Record(key) if !crate::host::is_value(key)) =>
                    {
                        scope.push(x);
                    }
                    Ty::Error => {}
                    other => {
                        let t = self.show(other);
                        self.err(
                            "E106",
                            e.span,
                            format!(
                                "`in:` takes apps (`in: notes + chat`) or resources (`in: n3`); this is {t}"
                            ),
                        );
                    }
                }
            }
        }
    }

    /// A list of resources as one `List<host.Resource>` value, for the host.
    fn resource_list(&mut self, parts: Vec<Ex>) -> Ex {
        let list_ty = Ty::List(Box::new(Ty::Record(super::resource_key())));
        let list = self.fresh(list_ty.clone());
        let mut stmts = vec![Stmt::Set(
            list,
            Ex::host(Imp::ListNew, Vec::new(), list_ty.clone()),
        )];
        for part in parts {
            let pushed = match part.ty {
                Ty::List(_) | Ty::Collection(_) => {
                    let elem = Ty::Record(super::resource_key());
                    let mut fl = self.new_for(part, &elem);
                    fl.body.push(Stmt::Eval(Self::push_item(
                        Ex::local(list, list_ty.clone()),
                        Ex::local(fl.item_local, elem),
                    )));
                    Stmt::ForList(Box::new(fl))
                }
                _ => Stmt::Eval(Self::push_item(Ex::local(list, list_ty.clone()), part)),
            };
            stmts.push(pushed);
        }
        Ex::new(
            Ek::Seq(stmts, Box::new(Ex::local(list, list_ty.clone()))),
            list_ty,
        )
    }

    /// `grep(/re/, in: notes + chat, not: .quote, changed: -14d, mentions: me)`: the matches of
    /// the regular expression in the text of the resources named, each a `host.Match`; with no
    /// `in:`, every app the program may read.
    pub(super) fn builtin_grep(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let mut pattern: Option<Ex> = None;
        let mut apps: Vec<String> = Vec::new();
        let mut scope: Vec<Ex> = Vec::new();
        let mut named_in = false;
        let mut not: Vec<String> = Vec::new();
        let mut changed: Option<String> = None;
        let mut mentions: Option<String> = None;
        for a in args {
            let Some(name) = &a.name else {
                if pattern.is_some() {
                    self.err(
                        "E022",
                        a.value.span,
                        "`grep` takes one pattern; name the rest: `grep(/x/, in: notes, not: .quote)`",
                    );
                    continue;
                }
                pattern = Some(self.expr_as(&a.value, &Ty::Text));
                continue;
            };
            match name.name.as_str() {
                "in" => {
                    named_in = true;
                    self.grep_sources(&a.value, &mut apps, &mut scope);
                }
                "not" => self.block_kinds(&a.value, &mut not),
                "changed" => changed = self.when_text("changed", &a.value),
                "mentions" => match &a.value.kind {
                    ExprKind::Ident(w) if w == "me" => mentions = Some("me".to_owned()),
                    _ => self.err(
                        "E106",
                        a.value.span,
                        "`mentions:` takes `me`: the person this task acts for",
                    ),
                },
                other => {
                    let hint = crate::diag::suggestion(
                        other,
                        ["in", "not", "changed", "mentions"].iter().copied(),
                    );
                    self.err(
                        "E106",
                        name.span,
                        format!(
                            "`grep` has no argument `{other}`{hint}; it takes the pattern, then in:, not:, changed: and mentions:"
                        ),
                    );
                }
            }
        }
        let Some(pattern) = pattern else {
            self.err(
                "E022",
                span,
                "`grep` takes a regular expression first: `grep(/\\bTide\\b/, in: notes)`",
            );
            return Ex::error();
        };
        if !named_in {
            apps = self.source_apps().into_iter().map(|(_, app)| app).collect();
        }
        let options = json!({
            "apps": apps,
            "not": not,
            "changed": changed,
            "mentions": mentions,
        })
        .to_string();
        let scope = self.resource_list(scope);
        Ex::host(
            Imp::Grep,
            vec![pattern, Ex::str(&options), scope],
            Ty::List(Box::new(super::platform::host_ty("Match"))),
        )
    }

    /// `not: .quote` or `not: [.quote, .code]`: the kinds of block to leave out.
    fn block_kinds(&mut self, e: &ast::Expr, out: &mut Vec<String>) {
        match &e.kind {
            ExprKind::Variant { name, args: None } if BLOCK_KINDS.contains(&name.name.as_str()) => {
                out.push(name.name.clone());
            }
            ExprKind::List(items) => {
                for item in items {
                    self.block_kinds(item, out);
                }
            }
            ExprKind::Variant { name, .. } => {
                let hint = crate::diag::suggestion(&name.name, BLOCK_KINDS.iter().copied());
                self.err(
                    "E106",
                    name.span,
                    format!(
                        "`.{}` is no kind of block{hint}; kinds: .{}",
                        name.name,
                        BLOCK_KINDS.join(", .")
                    ),
                );
            }
            _ => self.err(
                "E106",
                e.span,
                format!(
                    "`not:` takes kinds of block: `not: .quote`, `not: [.quote, .code]`; kinds: .{}",
                    BLOCK_KINDS.join(", .")
                ),
            ),
        }
    }

    /// `changed: -14d` (or `2026-10-01..`, `today`): a time as search writes it, which the
    /// runtime resolves against the log.
    fn when_text(&mut self, what: &str, e: &ast::Expr) -> Option<String> {
        match &e.kind {
            ExprKind::Duration(..)
            | ExprKind::Calendar(_)
            | ExprKind::Range { .. }
            | ExprKind::Unary { .. } => Some(crate::print::expr_source(e).replace(' ', "")),
            ExprKind::Ident(w) if w == "today" => Some("today".to_owned()),
            _ => {
                self.err(
                    "E106",
                    e.span,
                    format!(
                        "`{what}:` takes a time as search writes it: `-14d`, `2026-10-01..`, `today`"
                    ),
                );
                None
            }
        }
    }

    /// `describe(notes)`, `describe(Note)`, `describe(notes.pin)`, `describe(Note.title)` or
    /// `describe(n3)`: declarations as tilolang, and what the program may do with them.
    pub(super) fn builtin_describe(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let [what] = args else {
            self.err(
                "E022",
                span,
                "`describe` takes one thing: an app (`describe(notes)`), a type, an action, a field or a resource",
            );
            return Ex::error();
        };
        if what.name.is_some() {
            self.err(
                "E022",
                span,
                "`describe` takes the thing itself: `describe(Note)`",
            );
            return Ex::error();
        }
        let e = &what.value;
        if let Some(path) = self.described_path(e) {
            return Ex::host(Imp::Describe, vec![Ex::str(&path.to_string())], Ty::Text);
        }
        let x = self.expr(e, None);
        match &x.ty {
            Ty::Record(key) if !crate::host::is_value(key) => {
                Ex::host(Imp::DescribeRef, vec![x], Ty::Text)
            }
            Ty::Error => Ex::error(),
            other => {
                let t = self.show(other);
                self.err(
                    "E106",
                    e.span,
                    format!(
                        "`describe` takes an app, a type, an action, a field or a resource; this is a {t}"
                    ),
                );
                Ex::error()
            }
        }
    }

    /// What `describe(e)` names when `e` is a declaration: `{app, name, field}`, by the app's id.
    fn described_path(&mut self, e: &ast::Expr) -> Option<serde_json::Value> {
        match &e.kind {
            ExprKind::Ident(name) if self.lookup(name).is_none() => {
                if let Some(app) = self.source_app(name) {
                    return Some(json!({"app": app}));
                }
                if let Some(info) = self.unique_type(name) {
                    return Some(json!({"app": info.0.app, "name": info.0.name}));
                }
                if name.chars().next().is_some_and(char::is_uppercase) {
                    let types = self.type_names();
                    let hint = crate::diag::suggestion(name, types.iter().map(String::as_str));
                    self.err(
                        "E106",
                        e.span,
                        format!("no type `{name}`{hint}; types: {}", types.join(", ")),
                    );
                    return Some(serde_json::Value::Null);
                }
                None
            }
            ExprKind::Field { base, name } => match &base.kind {
                ExprKind::Ident(first) if self.lookup(first).is_none() => {
                    if let Some(app) = self.source_app(first) {
                        return Some(json!({"app": app, "name": name.name}));
                    }
                    let (key, _) = self.unique_type(first)?;
                    Some(json!({"app": key.app, "name": key.name, "field": name.name}))
                }
                ExprKind::Field {
                    base: inner,
                    name: ty,
                } => {
                    let ExprKind::Ident(alias) = &inner.kind else {
                        return None;
                    };
                    if self.lookup(alias).is_some() {
                        return None;
                    }
                    let app = self.source_app(alias)?;
                    Some(json!({"app": app, "name": ty.name, "field": name.name}))
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// In a program, which has no types of its own, the type a bare name (`Note`) names: the
    /// one app's that has it.
    pub(super) fn program_type(&self, e: &ast::Expr) -> Option<Ty> {
        if !self.st.program {
            return None;
        }
        let ExprKind::Ident(name) = &e.kind else {
            return None;
        };
        self.unique_type(name).map(|(key, _)| Ty::Record(key))
    }

    /// The one type of any app the program holds named `name`, with its id.
    fn unique_type(&self, name: &str) -> Option<(TypeKey, String)> {
        let mut found = self
            .env
            .types
            .iter()
            .filter(|t| t.key.app != crate::host::HOST_APP && t.key.name == name);
        let one = found.next()?;
        found
            .next()
            .is_none()
            .then(|| (one.key.clone(), one.id.clone()))
    }

    fn type_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .env
            .types
            .iter()
            .filter(|t| t.key.app != crate::host::HOST_APP)
            .map(|t| t.key.name.clone())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// `read(r, matching: /re/, around: 1, page: 2)` or `read(r, around: "¶4")`: the resource's
    /// text with anchors, its size first, one page of it, only what matches when asked, with
    /// the paragraphs around.
    pub(super) fn read_with(&mut self, r: Ex, args: &[ast::Arg]) -> Ex {
        let mut matching = Ex::str("");
        let mut options = serde_json::Map::new();
        for a in args {
            let Some(name) = &a.name else {
                self.err(
                    "E022",
                    a.value.span,
                    "`read` takes one resource; name the rest: `read(n, matching: /x/, around: 1)`",
                );
                continue;
            };
            match name.name.as_str() {
                "matching" => matching = self.expr_as(&a.value, &Ty::Text),
                "around" => match &a.value.kind {
                    ExprKind::Int(n) => {
                        options.insert("around".into(), json!(n));
                    }
                    ExprKind::Str(parts) => match parts.as_slice() {
                        [ast::StrPart::Lit(anchor)] => {
                            options.insert("at".into(), json!(anchor));
                        }
                        _ => self.err(
                            "E106",
                            a.value.span,
                            "`around:` takes an anchor written out: `around: \"¶4\"`",
                        ),
                    },
                    _ => self.err(
                        "E106",
                        a.value.span,
                        "`around:` takes how many paragraphs to show around each match (`around: 1`), or an anchor (`around: \"¶4\"`)",
                    ),
                },
                "page" => match &a.value.kind {
                    ExprKind::Int(n) if *n >= 1 => {
                        options.insert("page".into(), json!(n));
                    }
                    _ => self.err(
                        "E106",
                        a.value.span,
                        "`page:` takes the page's number, from 1: `page: 2`",
                    ),
                },
                other => {
                    let hint = crate::diag::suggestion(
                        other,
                        ["matching", "around", "page"].iter().copied(),
                    );
                    self.err(
                        "E106",
                        name.span,
                        format!("`read` has no argument `{other}`{hint}; it takes matching:, around: and page:"),
                    );
                }
            }
        }
        let options = serde_json::Value::Object(options).to_string();
        Ex::host(
            Imp::ReadWith,
            vec![r, matching, Ex::str(&options)],
            Ty::Text,
        )
    }

    /// `print(xs, fields: [.a, .b])`, or `print(xs)` of a list of a type's resources: one row
    /// per resource, its reference first, then the fields (by default the type's filters).
    pub(super) fn print_rows(&mut self, list: Ex, fields: Option<&ast::Arg>) -> Option<Ex> {
        let (Ty::List(elem) | Ty::Collection(elem) | Ty::Set(elem)) = &list.ty else {
            if let Some(f) = fields {
                let t = self.show(&list.ty);
                self.err(
                    "E106",
                    f.value.span,
                    format!("`fields:` shows fields of a list's rows; this is a {t}"),
                );
            }
            return None;
        };
        let Ty::Record(key) = &**elem else {
            if fields.is_some() {
                let t = self.show(elem);
                self.err(
                    "E106",
                    list_span(fields),
                    format!("`fields:` shows fields of records; these are {t}"),
                );
            }
            return None;
        };
        let key = key.clone();
        let names: Vec<String> = if crate::host::is_value(&key) {
            let ty = crate::host::value_type(&key.name)?;
            ty.fields.iter().map(|(n, ..)| (*n).to_owned()).collect()
        } else {
            let info = self.env.type_info(&key)?;
            info.fields.iter().map(|f| f.name.clone()).collect()
        };
        let chosen: Vec<String> = match fields {
            Some(f) => {
                let ExprKind::List(items) = &f.value.kind else {
                    self.err(
                        "E106",
                        f.value.span,
                        "`fields:` takes the fields as key paths: `fields: [.title, .due]`",
                    );
                    return Some(Ex::error());
                };
                let mut chosen = Vec::new();
                for item in items {
                    let ExprKind::Variant { name, args: None } = &item.kind else {
                        self.err(
                            "E106",
                            item.span,
                            "a field to print is a key path: `.title`",
                        );
                        continue;
                    };
                    if !names.contains(&name.name) {
                        let hint =
                            crate::diag::suggestion(&name.name, names.iter().map(String::as_str));
                        self.err(
                            "E106",
                            name.span,
                            format!(
                                "`{}` has no field `{}`{hint}; fields: {}",
                                key.name,
                                name.name,
                                names.join(", ")
                            ),
                        );
                        continue;
                    }
                    chosen.push(name.name.clone());
                }
                chosen
            }
            None if crate::host::is_value(&key) || key.name == crate::host::RESOURCE_TYPE_NAME => {
                return None;
            }
            None => {
                let info = self.env.type_info(&key)?;
                info.search
                    .filters
                    .iter()
                    .map(|f| f.key.clone())
                    .filter(|k| names.contains(k))
                    .collect()
            }
        };
        let ty = elem.meta_text(&self.env.app);
        Some(Ex::host(
            Imp::PrintRows,
            vec![list, Ex::str(&json!(chosen).to_string()), Ex::str(&ty)],
            Ty::Unit,
        ))
    }

    /// The chaining functions of a list: `count_by(key)`, `group(key)`, `take(n)`,
    /// `each(f)` and `single()`; `None` for any other method.
    pub(super) fn list_chain(
        &mut self,
        list: Ex,
        elem: &Ty,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> Option<Ex> {
        let m = name.name.as_str();
        Some(match m {
            "count_by" | "group" => {
                let held_ty = Ty::List(Box::new(elem.clone()));
                let held = self.fresh(held_ty.clone());
                let hold = Stmt::Set(held, list);
                let map = ast::Ident {
                    name: "map".to_owned(),
                    span: name.span,
                };
                let keys =
                    self.higher_order(Ex::local(held, held_ty.clone()), elem, &map, args, span);
                let Ty::List(key) = keys.ty.clone() else {
                    return Some(Ex::error());
                };
                let (imp, row) = if m == "count_by" {
                    (Imp::CountBy, Ty::Tuple(vec![(*key).clone(), Ty::Int]))
                } else {
                    (
                        Imp::GroupBy,
                        Ty::Tuple(vec![(*key).clone(), held_ty.clone()]),
                    )
                };
                let out = Ty::List(Box::new(row));
                let call = Ex::host(imp, vec![keys, Ex::local(held, held_ty)], out.clone());
                Ex::new(Ek::Seq(vec![hold], Box::new(call)), out)
            }
            "take" => {
                let [n] = args else {
                    self.err("E022", span, "`take` takes how many: `take(5)`");
                    return Some(Ex::error());
                };
                let n = self.expr_as(&n.value, &Ty::Int);
                let ty = Ty::List(Box::new(elem.clone()));
                Ex::host(Imp::Take, vec![list, n], ty)
            }
            "single" => {
                if !args.is_empty() {
                    self.err(
                        "E022",
                        span,
                        "`single` takes nothing: `find(\"Note launch\").single()`",
                    );
                    return Some(Ex::error());
                }
                let one = Ex::host(Imp::Single, vec![list], Ty::List(Box::new(elem.clone())));
                let (get, c) = load_op(elem, Imp::ListGetI, Imp::ListGetF, Imp::ListGetH);
                conv(
                    c,
                    Ex::host(get, vec![one, Ex::int(0)], elem.clone()),
                    elem.clone(),
                )
            }
            "each" => {
                let lambda = match args {
                    [a] if a.name.is_none() => super::optional::lambda_of(&a.value),
                    _ => None,
                };
                let Some((param, body)) = lambda else {
                    self.err(
                        "E051",
                        span,
                        "`.each` takes one lambda: `.each(t -> todo.add(list, t.text))`",
                    );
                    return Some(Ex::error());
                };
                let mut fl = self.new_for(list, elem);
                self.push_sink();
                self.bind_local(&param.name, fl.item_local);
                let v = self.expr(&body, None);
                self.emit(Stmt::Eval(v));
                let mut inner = self.pop_sink();
                fl.body.append(&mut inner);
                Ex::new(
                    Ek::Seq(
                        vec![Stmt::ForList(Box::new(fl))],
                        Box::new(Ex::new(Ek::Unit, Ty::Unit)),
                    ),
                    Ty::Unit,
                )
            }
            _ => return None,
        })
    }

    /// `m.group(1)`: a group of a `host.Match`, the whole match for 0.
    pub(super) fn match_group(&mut self, m: Ex, args: &[ast::Arg], span: Span) -> Ex {
        let [n] = args else {
            self.err(
                "E022",
                span,
                "`group` takes the group's number: `m.group(1)`, 0 for the whole match",
            );
            return Ex::error();
        };
        let n = self.expr_as(&n.value, &Ty::Int);
        Ex::host(Imp::MatchGroup, vec![m, n], Ty::Text)
    }

    /// `@chat.Message:...` in a program whose host does not hold chat: a resource read through
    /// the platform, a `host.Resource`.
    pub(super) fn foreign_ref(&self, r: &ast::RefLit) -> Option<Ex> {
        if !self.st.program {
            return None;
        }
        let app = self.source_app(&r.app)?;
        if self.env.find_import(&r.app).is_some() {
            return None;
        }
        Some(Ex::host(
            Imp::ForeignRef,
            vec![Ex::str(&app), Ex::str(&r.ty), Ex::str(&r.id)],
            Ty::Record(super::resource_key()),
        ))
    }
}

fn list_span(fields: Option<&ast::Arg>) -> Span {
    fields.map_or_else(Span::default, |f| f.value.span)
}
