//! A type's search (search.md, "What a type declares"): the filters its `search` block declares, in
//! order, its default order and the fields it keeps out of filters; or, with no block, the
//! defaults: every Bool, enum, Date, Time, number, reference, identifier and Set field a filter, in
//! field order, and the first Time the default order, newest first.
//!
//! A named filter (`filter "Open" = not done`) is a predicate over the resource's own fields,
//! checked here to be one the index can keep: it reads only the resource's own fields and
//! literals and calls nothing, so a change to one field changes it and nothing else does. One
//! may compare a field with the time it is asked at (`due <= today`, `due <= today + 7d`): that
//! is read when the filter is asked, and the index answers it from the field's column. Lambdas
//! of `filter` and `count` over a collection lower the same way, without the time.

use crate::ast::{self, BinOp, Expr, ExprKind, SearchBlock, SearchEntryKind, StrPart, UnOp};
use crate::diag::{Diagnostic, Span};
use crate::meta::{FilterKind, FilterMeta, SearchMeta, SortMeta, SortOrder};
use crate::pred::{CmpOp, Float, Lit, Operand, Pred};
use crate::resolve::{Env, FieldInfo, TypeInfo, sentence_case};
use crate::types::Ty;

/// The words of a typed search that a filter's key cannot be: `is:done`, `sort:-due`,
/// `in:list`.
pub const RESERVED_KEYS: &[&str] = &["is", "sort", "in", "text"];

/// What a filter on a field of type `ty` is: its kind, whether it may hold no value (an option
/// or a set) and whether it holds several (a set). `None` for a type no filter compares.
#[must_use]
pub fn filter_kind(ty: &Ty) -> Option<(FilterKind, bool, bool)> {
    match ty {
        Ty::Option(inner) => scalar_kind(inner).map(|kind| (kind, true, false)),
        Ty::Set(inner) if **inner == Ty::Text => Some((FilterKind::Text, true, true)),
        Ty::Set(inner) => scalar_kind(inner).map(|kind| (kind, true, true)),
        other => scalar_kind(other).map(|kind| (kind, false, false)),
    }
}

fn scalar_kind(ty: &Ty) -> Option<FilterKind> {
    Some(match ty {
        Ty::Bool => FilterKind::Bool,
        Ty::Enum(_) => FilterKind::Enum,
        Ty::Date => FilterKind::Date,
        Ty::Time => FilterKind::Time,
        Ty::Int | Ty::Float => FilterKind::Number,
        Ty::Record(key) if !crate::host::is_value(key) && key.app != crate::host::HOST_APP => {
            FilterKind::Reference
        }
        Ty::Identifier => FilterKind::Identifier,
        _ => return None,
    })
}

/// A named filter's key: its label in snake case, `Has attachment` as `has_attachment`.
#[must_use]
pub fn key_of_label(label: &str) -> String {
    let mut out = String::new();
    for c in label.trim().chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    out.trim_end_matches('_').to_owned()
}

/// The type named, with its doc comment's first line, as the errors about its search say it.
fn about(info: &TypeInfo) -> String {
    match info
        .doc
        .lines()
        .next()
        .map(str::trim)
        .filter(|l| !l.is_empty())
    {
        Some(line) => format!("`{}` ({line})", info.key.name),
        None => format!("`{}`", info.key.name),
    }
}

/// The fields of `info` a filter can be on, as the errors list them.
fn filterable(info: &TypeInfo) -> String {
    let names: Vec<&str> = info
        .fields
        .iter()
        .filter(|f| filter_kind(&f.ty).is_some())
        .map(|f| f.name.as_str())
        .collect();
    if names.is_empty() {
        "it has none; a named filter, `filter \"Label\" = ...`, is a predicate over its fields"
            .to_owned()
    } else {
        format!("its fields a filter compares: {}", names.join(", "))
    }
}

struct Ctx<'a> {
    info: &'a TypeInfo,
    env: &'a Env,
    diags: &'a mut Vec<Diagnostic>,
}

impl Ctx<'_> {
    fn err(&mut self, code: &str, span: Span, msg: impl Into<String>) {
        self.diags.push(Diagnostic::new(code, span, msg));
    }

    /// A filter's `(icon: .x)`, as an enum case's is written.
    fn icon(&mut self, opts: &[ast::ActionOpt]) -> Option<String> {
        let mut icon = None;
        for opt in opts {
            match (opt.name.name.as_str(), &opt.value.kind) {
                ("icon", _) if icon.is_some() => {
                    self.err("E090", opt.name.span, "`icon:` is given twice");
                }
                ("icon", ExprKind::Variant { name, args: None }) => {
                    match crate::prims::icon(&name.name) {
                        Some(kebab) => icon = Some(kebab.to_owned()),
                        None => self.err(
                            "E090",
                            name.span,
                            format!("`.{}` is not an icon of the platform's set", name.name),
                        ),
                    }
                }
                ("icon", _) => self.err(
                    "E090",
                    opt.value.span,
                    "`icon:` takes an icon written with a dot, such as `icon: .circle`",
                ),
                (other, _) => self.err(
                    "E090",
                    opt.name.span,
                    format!("a filter takes the option `icon:`; `{other}:` is not one"),
                ),
            }
        }
        icon
    }

    fn field(&mut self, name: &ast::Ident, what: &str) -> Option<&FieldInfo> {
        if self.info.field(&name.name).is_none() {
            let msg = format!(
                "{what}: {} has no field `{}`; its fields: {}",
                about(self.info),
                name.name,
                self.info.field_names()
            );
            self.err("E100", name.span, msg);
            return None;
        }
        self.info.field(&name.name)
    }

    fn field_filter(&mut self, name: &ast::Ident, label: Option<&str>) -> Option<FilterMeta> {
        let field = self.field(name, "`filter`")?.clone();
        let Some((kind, optional, many)) = filter_kind(&field.ty) else {
            let t = field.ty.display(&self.env.app);
            let why = if matches!(field.ty, Ty::Text) {
                "text is found by searching for it, not filtered"
            } else {
                "a filter compares a Bool, an enum, a Date or Time, a number, a reference, an identifier, an option of one, or a Set"
            };
            let msg = format!(
                "`{}` of {} is {t}, which no filter compares: {why}. {}",
                field.name,
                about(self.info),
                filterable(self.info)
            );
            self.err("E101", name.span, msg);
            return None;
        };
        Some(FilterMeta {
            key: field.name.clone(),
            label: label.map_or_else(|| sentence_case(&field.name), str::to_owned),
            doc: field
                .doc
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_owned(),
            kind,
            field: Some(field.id.clone()),
            ty: field.ty.meta_text(&self.env.app),
            pred: None,
            optional,
            many,
            icon: None,
        })
    }

    fn named_filter(&mut self, label: &str, span: Span, expr: &Expr) -> Option<FilterMeta> {
        let key = key_of_label(label);
        if !key.chars().next().is_some_and(char::is_alphabetic) {
            self.err(
                "E100",
                span,
                format!("a named filter's label starts with a letter; `{label}` does not"),
            );
            return None;
        }
        let lower = Lower {
            info: self.info,
            env: self.env,
            fields: Fields::Own,
            outer: None,
            captured: std::cell::RefCell::default(),
        };
        match lower.pred(expr) {
            Ok(pred) => Some(FilterMeta {
                key,
                label: label.to_owned(),
                doc: String::new(),
                kind: FilterKind::Predicate,
                field: None,
                ty: String::new(),
                pred: Some(pred),
                optional: false,
                many: false,
                icon: None,
            }),
            Err((at, why)) => {
                let msg = format!(
                    "the filter \"{label}\" is not a predicate the index can keep: {why}. A named filter of {} reads only its own fields and literals (`not done`, `due != .none`, `priority > 2`, `not attachments.is_empty()`)",
                    about(self.info)
                );
                self.err("E102", at, msg);
                None
            }
        }
    }

    fn sort(&mut self, field: &ast::Ident, order: &ast::Ident) -> Option<SortMeta> {
        let info = self.field(field, "`sort`")?.clone();
        let Some(word) = SortOrder::from_word(&order.name) else {
            self.err(
                "E103",
                order.span,
                format!(
                    "`{}` is not an order; write `newest`, `oldest`, `a_z` or `z_a`",
                    order.name
                ),
            );
            return None;
        };
        let held = match &info.ty {
            Ty::Option(inner) => (**inner).clone(),
            other => other.clone(),
        };
        let fits = match held {
            Ty::Date | Ty::Time | Ty::Int | Ty::Float => {
                matches!(word, SortOrder::Newest | SortOrder::Oldest)
            }
            Ty::Text | Ty::Identifier => matches!(word, SortOrder::AZ | SortOrder::ZA),
            _ => {
                let t = info.ty.display(&self.env.app);
                let msg = format!(
                    "`{}` of {} is {t}, which has no order: sort by a Date, a Time or a number (`newest`, `oldest`), or a Text (`a_z`, `z_a`)",
                    info.name,
                    about(self.info)
                );
                self.err("E103", field.span, msg);
                return None;
            }
        };
        if !fits {
            let words = if matches!(held, Ty::Text | Ty::Identifier) {
                "`a_z` or `z_a`"
            } else {
                "`newest` or `oldest`"
            };
            let t = held.display(&self.env.app);
            self.err(
                "E103",
                order.span,
                format!(
                    "`{}` is {t}: it sorts {words}, not `{}`",
                    info.name, order.name
                ),
            );
            return None;
        }
        Some(SortMeta {
            key: info.name.clone(),
            field: info.id,
            order: word,
        })
    }
}

/// The search of the type `info`: its `search` block's, checked (errors go to `diags`), or the
/// defaults when it has none.
pub fn resolve(
    info: &TypeInfo,
    block: Option<&SearchBlock>,
    env: &Env,
    diags: &mut Vec<Diagnostic>,
) -> SearchMeta {
    let Some(block) = block else {
        return defaults(info, env);
    };
    let mut ctx = Ctx { info, env, diags };
    let mut out = SearchMeta {
        declared: true,
        ..SearchMeta::default()
    };
    let mut hidden_names: Vec<(String, Span)> = Vec::new();
    for entry in &block.entries {
        match &entry.kind {
            SearchEntryKind::Field { field, label, opts } => {
                let icon = ctx.icon(opts);
                if let Some(mut filter) = ctx.field_filter(field, label.as_deref()) {
                    filter.icon = icon;
                    push_filter(&mut ctx, &mut out, filter, entry.span);
                }
            }
            SearchEntryKind::Named {
                label,
                label_span,
                expr,
                opts,
            } => {
                let icon = ctx.icon(opts);
                if let Some(mut filter) = ctx.named_filter(label, *label_span, expr) {
                    filter.icon = icon;
                    push_filter(&mut ctx, &mut out, filter, entry.span);
                }
            }
            SearchEntryKind::Sort { field, order } => {
                if out.sort.is_some() {
                    ctx.err(
                        "E104",
                        entry.span,
                        format!(
                            "{} already has a `sort`; a type has one default order",
                            about(info)
                        ),
                    );
                    continue;
                }
                out.sort = ctx.sort(field, order);
            }
            SearchEntryKind::Hide { field } => {
                if let Some(f) = ctx.field(field, "`hide`") {
                    let id = f.id.clone();
                    if out.hidden.contains(&id) {
                        ctx.err(
                            "E104",
                            field.span,
                            format!("`{}` is hidden already", field.name),
                        );
                    } else {
                        out.hidden.push(id);
                        hidden_names.push((field.name.clone(), field.span));
                    }
                }
            }
        }
    }
    for (name, span) in hidden_names {
        if out.filters.iter().any(|f| f.key == name) {
            ctx.err(
                "E104",
                span,
                format!(
                    "`{name}` is a filter of {} and hidden from filters; keep one",
                    about(info)
                ),
            );
        }
    }
    out
}

fn push_filter(ctx: &mut Ctx<'_>, out: &mut SearchMeta, filter: FilterMeta, span: Span) {
    if RESERVED_KEYS.contains(&filter.key.as_str()) {
        let msg = format!(
            "a filter named `{}` reads as search's own word (`is:`, `sort:`, `in:`, `text:`); label it otherwise",
            filter.key
        );
        ctx.err("E104", span, msg);
        return;
    }
    if out.filters.iter().any(|f| f.key == filter.key) {
        let msg = format!(
            "{} has a filter `{}` already; each filter is declared once",
            about(ctx.info),
            filter.key
        );
        ctx.err("E104", span, msg);
        return;
    }
    out.filters.push(filter);
}

/// The defaults of a type with no `search` block.
#[must_use]
pub fn defaults(info: &TypeInfo, env: &Env) -> SearchMeta {
    let mut out = SearchMeta::default();
    for field in &info.fields {
        if RESERVED_KEYS.contains(&field.name.as_str()) {
            continue;
        }
        let Some((kind, optional, many)) = filter_kind(&field.ty) else {
            continue;
        };
        out.filters.push(FilterMeta {
            key: field.name.clone(),
            label: sentence_case(&field.name),
            doc: field
                .doc
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_owned(),
            kind,
            field: Some(field.id.clone()),
            ty: field.ty.meta_text(&env.app),
            pred: None,
            optional,
            many,
            icon: None,
        });
    }
    out.sort = info
        .fields
        .iter()
        .find(|f| matches!(&f.ty, Ty::Time) || matches!(&f.ty, Ty::Option(t) if **t == Ty::Time))
        .map(|f| SortMeta {
            key: f.name.clone(),
            field: f.id.clone(),
            order: SortOrder::Newest,
        });
    out
}

/// How the fields of the resource a predicate is over are written.
#[derive(Clone, Copy)]
enum Fields<'a> {
    /// By name: a named filter's `not done`.
    Own,
    /// Through a lambda's parameter: `t -> not t.done`.
    Param(&'a str),
}

/// A predicate's lowering over the fields of one type.
struct Lower<'a> {
    info: &'a TypeInfo,
    env: &'a Env,
    fields: Fields<'a>,
    /// The type of a name the asking code holds (a local, a parameter), which a lambda's
    /// predicate may compare a field with: `None` for none.
    outer: Option<&'a Outer<'a>>,
    /// The names it compares, in the order of their [`Operand::Arg`]s.
    captured: std::cell::RefCell<Vec<String>>,
}

type Lowered<T> = Result<T, (Span, String)>;

/// The type of a name the code asking a predicate holds, `None` for one it does not.
type Outer<'a> = dyn Fn(&str) -> Option<Ty> + 'a;

/// `param -> body` as a predicate over the fields of `info`, when it is one the store can
/// answer: it reads only the item's fields and literals.
#[must_use]
pub fn lambda_pred(info: &TypeInfo, env: &Env, param: &str, body: &Expr) -> Option<Pred<String>> {
    Lower {
        info,
        env,
        fields: Fields::Param(param),
        outer: None,
        captured: std::cell::RefCell::default(),
    }
    .pred(body)
    .ok()
}

/// As [`lambda_pred`], the predicate also comparing fields with names the asking code holds
/// (`c -> c.column == col`), each of the type `outer` gives: the predicate and the names, in
/// the order of its [`Operand::Arg`]s, whose values are bound when it is asked.
#[must_use]
pub fn lambda_pred_with(
    info: &TypeInfo,
    env: &Env,
    param: &str,
    body: &Expr,
    outer: &dyn Fn(&str) -> Option<Ty>,
) -> Option<(Pred<String>, Vec<String>)> {
    let lower = Lower {
        info,
        env,
        fields: Fields::Param(param),
        outer: Some(outer),
        captured: std::cell::RefCell::default(),
    };
    let pred = lower.pred(body).ok()?;
    Some((pred, lower.captured.into_inner()))
}

impl Lower<'_> {
    /// The field `e` names, when it names one of the resource's own.
    fn own_field(&self, e: &Expr) -> Option<&FieldInfo> {
        match (&e.kind, self.fields) {
            (ExprKind::Ident(name), Fields::Own) => self.info.field(name),
            (ExprKind::Field { base, name }, Fields::Param(param)) if matches!(&base.kind, ExprKind::Ident(p) if p == param) => {
                self.info.field(&name.name)
            }
            _ => None,
        }
    }

    fn not_own(&self, e: &Expr) -> (Span, String) {
        let text = crate::print::expr_source(e);
        let why = match (&e.kind, self.fields) {
            (ExprKind::Ident(name), Fields::Own) => format!(
                "`{name}` is not a field of `{}`; its fields: {}",
                self.info.key.name,
                self.info.field_names()
            ),
            (ExprKind::Field { base, .. }, Fields::Own) if self.own_field(base).is_some() => {
                format!("`{text}` reads another resource through a reference")
            }
            (ExprKind::Call { .. }, _) => format!(
                "`{text}` calls something; a filter reads fields, and of them only `len()`, `is_empty()` and a set's `contains(value)`"
            ),
            _ => format!("`{text}` is not a field or a literal"),
        };
        (e.span, why)
    }

    fn pred(&self, e: &Expr) -> Lowered<Pred<String>> {
        match &e.kind {
            ExprKind::Bool(b) => Ok(Pred::Const(*b)),
            ExprKind::Unary {
                op: UnOp::Not,
                operand,
            } => Ok(Pred::Not(Box::new(self.pred(operand)?))),
            ExprKind::Binary {
                op: op @ (BinOp::And | BinOp::Or),
                lhs,
                rhs,
            } => {
                let both = vec![self.pred(lhs)?, self.pred(rhs)?];
                Ok(if *op == BinOp::And {
                    Pred::And(both)
                } else {
                    Pred::Or(both)
                })
            }
            ExprKind::Binary { op, lhs, rhs } => self.comparison(*op, lhs, rhs, e.span),
            ExprKind::Call { callee, args } => {
                let ExprKind::Field { base, name } = &callee.kind else {
                    return Err(self.not_own(e));
                };
                let Some(field) = self.own_field(base) else {
                    return Err(self.not_own(e));
                };
                match (name.name.as_str(), args.as_slice(), &field.ty) {
                    ("is_empty", [], Ty::Set(_) | Ty::Collection(_) | Ty::Text) => Ok(Pred::Cmp(
                        CmpOp::Eq,
                        Operand::Len(field.id.clone()),
                        Operand::Lit(Lit::Int(0)),
                    )),
                    // A set holding a value: the run of that value in the set's column.
                    ("contains", [arg], Ty::Set(item)) => {
                        let (lit, _) = self.literal(&arg.value, Some(item))?;
                        Ok(Pred::Has(field.id.clone(), lit))
                    }
                    _ => Err(self.not_own(e)),
                }
            }
            _ => match self.own_field(e) {
                Some(field) if field.ty == Ty::Bool => Ok(Pred::Cmp(
                    CmpOp::Eq,
                    Operand::Field(field.id.clone()),
                    Operand::Lit(Lit::Bool(true)),
                )),
                Some(field) => {
                    let t = field.ty.display(&self.env.app);
                    Err((
                        e.span,
                        format!("`{}` is {t}, not a Bool; compare it", field.name),
                    ))
                }
                None => Err(self.not_own(e)),
            },
        }
    }

    fn comparison(&self, op: BinOp, lhs: &Expr, rhs: &Expr, span: Span) -> Lowered<Pred<String>> {
        let cmp = match op {
            BinOp::Eq => CmpOp::Eq,
            BinOp::Ne => CmpOp::Ne,
            BinOp::Lt => CmpOp::Lt,
            BinOp::Le => CmpOp::Le,
            BinOp::Gt => CmpOp::Gt,
            BinOp::Ge => CmpOp::Ge,
            _ => {
                return Err((
                    span,
                    format!("`{}` is arithmetic, not a comparison", op.text()),
                ));
            }
        };
        // A literal that needs a type to read (`.open`, `.none`) takes the other side's.
        let lhs_first = !is_contextual(lhs) || is_contextual(rhs);
        let ((a, ta), (b, tb)) = if lhs_first {
            let left = self.operand(lhs, None)?;
            let right = self.operand(rhs, Some(&left.1))?;
            (left, right)
        } else {
            let right = self.operand(rhs, None)?;
            let left = self.operand(lhs, Some(&right.1))?;
            (left, right)
        };
        // `due == .none`, `due != .none`: whether an option holds a value.
        for (field, other) in [(&a, &b), (&b, &a)] {
            if let (Operand::Field(f), Operand::Lit(Lit::Absent)) = (field, other) {
                let present = Pred::Present(f.clone());
                return match cmp {
                    CmpOp::Eq => Ok(Pred::Not(Box::new(present))),
                    CmpOp::Ne => Ok(present),
                    _ => Err((
                        span,
                        "`.none` has no order; compare it with `==` or `!=`".into(),
                    )),
                };
            }
        }
        let (ha, hb) = (held(&ta), held(&tb));
        if !compatible(&ha, &hb) {
            let (x, y) = (ta.display(&self.env.app), tb.display(&self.env.app));
            return Err((span, format!("{x} and {y} do not compare")));
        }
        let ordered = !matches!(cmp, CmpOp::Eq | CmpOp::Ne);
        if ordered && !matches!(ha, Ty::Int | Ty::Float | Ty::Date | Ty::Time) {
            let x = ha.display(&self.env.app);
            return Err((
                span,
                format!(
                    "{x} has no order; `{}` compares numbers, dates and times",
                    op.text()
                ),
            ));
        }
        Ok(Pred::Cmp(cmp, a, b))
    }

    /// One side of a comparison: a field of the resource, how many one holds, or a literal.
    fn operand(&self, e: &Expr, want: Option<&Ty>) -> Lowered<(Operand<String>, Ty)> {
        if let Some(field) = self.own_field(e) {
            if matches!(
                field.ty,
                Ty::Set(_) | Ty::Map(_) | Ty::Collection(_) | Ty::List(_)
            ) {
                let t = field.ty.display(&self.env.app);
                return Err((
                    e.span,
                    format!(
                        "`{}` is {t}: ask `{}.is_empty()` or compare `{}.len()`",
                        field.name, field.name, field.name
                    ),
                ));
            }
            return Ok((Operand::Field(field.id.clone()), field.ty.clone()));
        }
        if let ExprKind::Call { callee, args } = &e.kind
            && let ExprKind::Field { base, name } = &callee.kind
            && name.name == "len"
            && args.is_empty()
            && let Some(field) = self.own_field(base)
            && matches!(field.ty, Ty::Set(_) | Ty::Collection(_) | Ty::Text)
        {
            return Ok((Operand::Len(field.id.clone()), Ty::Int));
        }
        // A value the asking code holds, compared as it is when the predicate is asked.
        if let (ExprKind::Ident(name), Fields::Param(param), Some(outer)) =
            (&e.kind, self.fields, self.outer)
            && name != param
            && let Some(ty) = outer(name)
            && matches!(
                held(&ty),
                Ty::Int
                    | Ty::Float
                    | Ty::Bool
                    | Ty::Text
                    | Ty::Date
                    | Ty::Time
                    | Ty::Identifier
                    | Ty::Record(_)
            )
        {
            let mut captured = self.captured.borrow_mut();
            let at = match captured.iter().position(|held| held == name) {
                Some(at) => at,
                None => {
                    captured.push(name.clone());
                    captured.len().saturating_sub(1)
                }
            };
            return Ok((Operand::Arg(u32::try_from(at).unwrap_or(u32::MAX)), ty));
        }
        if let Some(clock) = self.clock(e) {
            return clock;
        }
        let (lit, ty) = self.literal(e, want)?;
        Ok((Operand::Lit(lit), ty))
    }

    /// `today`, `now`, either plus or minus a duration (`today + 1d`, `now - 7d`), in a named
    /// filter: the time the filter is asked at ([`Operand::Clock`]). A lambda's predicate is
    /// answered where it is written, so there they stay a query's (`query(T, due: ..today)`).
    fn clock(&self, e: &Expr) -> Option<Lowered<(Operand<String>, Ty)>> {
        if !matches!(self.fields, Fields::Own) {
            return None;
        }
        let anchor = |e: &Expr| -> Option<bool> {
            let word = match &e.kind {
                ExprKind::Ident(w) => w.as_str(),
                ExprKind::Call { callee, args } if args.is_empty() => match &callee.kind {
                    ExprKind::Ident(w) => w.as_str(),
                    _ => return None,
                },
                _ => return None,
            };
            match word {
                "today" => Some(true),
                "now" => Some(false),
                _ => None,
            }
        };
        let (day, offset) = match &e.kind {
            ExprKind::Binary { op, lhs, rhs } if matches!(op, BinOp::Add | BinOp::Sub) => {
                let day = anchor(lhs)?;
                let ExprKind::Duration(n, unit) = &rhs.kind else {
                    return Some(Err((
                        rhs.span,
                        "a time a filter is asked at moves by a duration: `today + 1d`".into(),
                    )));
                };
                let Some(secs) = crate::duration::seconds(*n, unit) else {
                    return Some(Err((rhs.span, "the duration is too long".into())));
                };
                if day && secs.rem_euclid(crate::calendar::DAY) != 0 {
                    return Some(Err((
                        rhs.span,
                        "`today` moves by whole days or weeks (`today + 1d`); `now` by any duration"
                            .into(),
                    )));
                }
                (
                    day,
                    if matches!(op, BinOp::Sub) {
                        -secs
                    } else {
                        secs
                    },
                )
            }
            _ => (anchor(e)?, 0),
        };
        Some(Ok((
            Operand::Clock { day, offset },
            if day { Ty::Date } else { Ty::Time },
        )))
    }

    /// A literal, of the type `want` where it needs one to read (`.open`, `.none`).
    fn literal(&self, e: &Expr, want: Option<&Ty>) -> Lowered<(Lit, Ty)> {
        match &e.kind {
            ExprKind::Int(i) => Ok((Lit::Int(*i), Ty::Int)),
            ExprKind::Float(f) => Ok((Lit::Float(Float(*f)), Ty::Float)),
            ExprKind::Bool(b) => Ok((Lit::Bool(*b), Ty::Bool)),
            ExprKind::Unary {
                op: UnOp::Neg,
                operand,
            } => match &operand.kind {
                ExprKind::Int(i) => Ok((Lit::Int(i.checked_neg().unwrap_or(i64::MAX)), Ty::Int)),
                ExprKind::Float(f) => Ok((Lit::Float(Float(-*f)), Ty::Float)),
                _ => Err(self.not_own(e)),
            },
            ExprKind::Str(parts) => {
                let mut text = String::new();
                for part in parts {
                    match part {
                        StrPart::Lit(s) => text.push_str(s),
                        StrPart::Interp(_) => {
                            return Err((
                                e.span,
                                "interpolated text changes; a filter compares a literal".into(),
                            ));
                        }
                    }
                }
                Ok((Lit::Text(text), Ty::Text))
            }
            ExprKind::Call { callee, args } => {
                if let ExprKind::Ident(kind) = &callee.kind
                    && matches!(kind.as_str(), "Date" | "Time")
                {
                    let (ty, text) =
                        crate::resolve::date_value(kind, args).map_err(|why| (e.span, why))?;
                    let lit = if ty == Ty::Date {
                        Lit::Date(text)
                    } else {
                        Lit::Time(text)
                    };
                    return Ok((lit, ty));
                }
                if let Some(args) = crate::resolve::identifier_call(e) {
                    let text =
                        crate::resolve::identifier_value(args).map_err(|why| (e.span, why))?;
                    return Ok((Lit::Ident(text), Ty::Identifier));
                }
                if let ExprKind::Ident(name) = &callee.kind
                    && matches!(name.as_str(), "now" | "today")
                {
                    return Err((
                        e.span,
                        format!(
                            "`{name}()` changes with time, so the index cannot keep a filter on it; a query asks it (`query(T, due: today()..)`)"
                        ),
                    ));
                }
                Err(self.not_own(e))
            }
            ExprKind::Variant { name, args: None } => {
                let optional = want.is_none_or(|t| matches!(t, Ty::Option(_)));
                match (name.name.as_str(), want.map(held)) {
                    ("none", _) if optional => Ok((Lit::Absent, Ty::Option(Box::new(Ty::Error)))),
                    (_, Some(Ty::Enum(key))) => {
                        let info = self
                            .env
                            .enum_info(&key)
                            .ok_or_else(|| (e.span, "an unknown enum".to_owned()))?;
                        let variant = info.variant(&name.name).ok_or_else(|| {
                            (
                                name.span,
                                format!(
                                    "{} has no case `.{}`; its cases: {}",
                                    key.name,
                                    name.name,
                                    info.variant_names()
                                ),
                            )
                        })?;
                        if !variant.fields.is_empty() {
                            return Err((
                                name.span,
                                format!(
                                    "`.{}` carries a payload; a filter compares a case by itself",
                                    name.name
                                ),
                            ));
                        }
                        Ok((Lit::Variant(variant.id.clone()), Ty::Enum(key)))
                    }
                    _ => Err((
                        e.span,
                        format!("`.{}` needs a field of an enum to compare with", name.name),
                    )),
                }
            }
            _ => Err(self.not_own(e)),
        }
    }
}

/// Whether `e` is a literal that reads only against another type: `.open`, `.none`.
fn is_contextual(e: &Expr) -> bool {
    matches!(e.kind, ExprKind::Variant { .. })
}

/// The type a value of `ty` compares as: an option's inner type.
fn held(ty: &Ty) -> Ty {
    match ty {
        Ty::Option(inner) => (**inner).clone(),
        other => other.clone(),
    }
}

fn compatible(a: &Ty, b: &Ty) -> bool {
    // A time compares with a day by the day, an identifier with its text.
    match (a, b) {
        (Ty::Error, _)
        | (_, Ty::Error)
        | (Ty::Int | Ty::Float, Ty::Int | Ty::Float)
        | (Ty::Date | Ty::Time, Ty::Date | Ty::Time)
        | (Ty::Identifier | Ty::Text, Ty::Identifier)
        | (Ty::Identifier, Ty::Text) => true,
        (x, y) => x.same(y),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_label_is_a_key_in_snake_case() {
        assert_eq!(super::key_of_label("Has attachment"), "has_attachment");
        assert_eq!(super::key_of_label("  Due (soon)! "), "due_soon");
        assert_eq!(super::key_of_label("Open"), "open");
    }
}
