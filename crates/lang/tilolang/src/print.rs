//! The canonical printer: syntax tree to the one fixed text projection.

use crate::ast::{
    ActionOpt, AppHeader, Arg, Block, Expr, ExprKind, FieldDecl, Item, Module, Param, Pattern,
    SearchBlock, SearchEntryKind, Stmt, StmtKind, StrPart, TypeExpr, TypeExprKind, UiCall, UiKind,
    UiStmt, UnOp,
};
use crate::diag::Span;
use crate::lexer::Comment;

/// A block of one expression whose text is at most this long prints on one line: `{ x }`.
const INLINE_BLOCK: usize = 40;

/// Prints a module in canonical form.
pub fn print_module(m: &Module) -> String {
    print_file(Some(&m.app), &m.items, &[])
}

/// Prints one file of a package in canonical form: the `app` header when the file has it, then
/// its items, a blank line between items (consecutive `use`s stay together).
///
/// `comments` are the file's plain `//` comments ([`crate::lexer::plain_comments`]): each prints
/// just above the declaration, field, statement or match arm it was written over, after that
/// node's `///` lines, inside an expression's blocks and match arms too. A comment written inside
/// a declaration's head has no line of its own to keep, so it prints above the next statement.
pub fn print_file(header: Option<&AppHeader>, items: &[Item], comments: &[Comment]) -> String {
    let mut p = Printer {
        out: String::new(),
        level: 0,
        comments,
        seen: 0,
    };
    if let Some(app) = header {
        let mut options = Vec::new();
        if let Some(icon) = &app.icon {
            options.push(format!("icon: .{icon}"));
        }
        if let Some(category) = &app.category {
            options.push(format!("category: .{category}"));
        }
        match app.previously.as_slice() {
            [] => {}
            [one] => options.push(format!("previously: {one}")),
            many => options.push(format!("previously: [{}]", many.join(", "))),
        }
        if app.own_records {
            options.push("records: .own".to_owned());
        }
        let options = if options.is_empty() {
            String::new()
        } else {
            format!(" ({})", options.join(", "))
        };
        p.line(&format!("app {} {}{options}", app.id, quote(&app.title)));
    }
    let mut prev_use = false;
    for (index, item) in items.iter().enumerate() {
        let is_use = matches!(item, Item::Use(_));
        if !(is_use && prev_use) && (header.is_some() || index > 0) {
            p.out.push('\n');
        }
        prev_use = is_use;
        p.item(item);
    }
    // Comments after the last item, which belong to no declaration.
    p.comments_before(usize::MAX);
    p.out
}

/// Prints one expression in canonical form.
pub fn print_expr(e: &Expr) -> String {
    expr_text(e, 0, 0)
}

fn indent(level: usize) -> String {
    "    ".repeat(level)
}

/// Writes into `out` the plain `//` comments written above the node that starts at `start`,
/// each on a line of its own indented to `level`; `seen` moves past them.
fn comments_into(
    out: &mut String,
    level: usize,
    comments: &[Comment],
    seen: &mut usize,
    start: usize,
) {
    while let Some(text) = comments
        .get(*seen)
        .filter(|c| c.span.start < start)
        .map(|c| c.text.clone())
    {
        *seen += 1;
        out.push_str(&indent(level));
        if text.is_empty() {
            out.push_str("//\n");
        } else {
            out.push_str("// ");
            out.push_str(&text);
            out.push('\n');
        }
    }
}

/// Whether a comment still to print is written inside `span`.
fn holds_comment(comments: &[Comment], seen: usize, span: Span) -> bool {
    comments
        .get(seen..)
        .unwrap_or_default()
        .iter()
        .any(|c| c.span.start >= span.start && c.span.start < span.end)
}

/// A block as text: on one line when it is one short expression, otherwise one statement per
/// line, indented one level deeper than `level`. A block whose statements carry a `//` comment
/// prints on several lines, so the comment has a line of its own.
fn block_text(b: &Block, level: usize, comments: &[Comment], seen: &mut usize) -> String {
    if b.stmts.is_empty() && !holds_comment(comments, *seen, b.span) {
        return "{}".to_owned();
    }
    if let [
        Stmt {
            kind: StmtKind::Expr(e),
            ..
        },
    ] = b.stmts.as_slice()
        && !holds_comment(comments, *seen, b.span)
    {
        let one = expr_text(e, 0, level);
        if !one.contains('\n') && one.len() <= INLINE_BLOCK {
            return format!("{{ {one} }}");
        }
    }
    let mut sub = Printer {
        out: String::new(),
        level: level + 1,
        comments,
        seen: *seen,
    };
    for s in &b.stmts {
        sub.stmt(s);
    }
    // A comment after the last statement is the block's, not the next one's.
    sub.comments_before(b.span.end);
    *seen = sub.seen;
    format!("{{\n{}{}}}", sub.out, indent(level))
}

fn pattern_text(p: &Pattern) -> String {
    if let Some(ty) = &p.ty {
        return format!("{}: {}", p.variant.name, print_type(ty));
    }
    if p.binds.is_empty() {
        format!(".{}", p.variant.name)
    } else {
        let names: Vec<&str> = p.binds.iter().map(|b| b.name.as_str()).collect();
        format!(".{}({})", p.variant.name, names.join(", "))
    }
}

fn args_text(args: &[Arg], level: usize, comments: &[Comment], seen: &mut usize) -> String {
    let inner: Vec<String> = args
        .iter()
        .map(|a| {
            let value = expr_with(&a.value, 0, level, comments, seen);
            match &a.name {
                Some(n) => format!("{}: {value}", n.name),
                None => value,
            }
        })
        .collect();
    inner.join(", ")
}

/// Prints a type in canonical form.
pub fn print_type(t: &TypeExpr) -> String {
    match &t.kind {
        TypeExprKind::Named(path) => path
            .iter()
            .map(|i| i.name.as_str())
            .collect::<Vec<_>>()
            .join("."),
        TypeExprKind::Generic(name, args) => {
            let inner: Vec<String> = args.iter().map(print_type).collect();
            format!("{}<{}>", name.name, inner.join(", "))
        }
        TypeExprKind::Tuple(items) => {
            let inner: Vec<String> = items.iter().map(print_type).collect();
            format!("({})", inner.join(", "))
        }
        TypeExprKind::Fn(params, result) => {
            let inner: Vec<String> = params.iter().map(print_type).collect();
            format!("({}) -> {}", inner.join(", "), print_type(result))
        }
        TypeExprKind::Union(cases) => {
            let inner: Vec<String> = cases.iter().map(print_type).collect();
            inner.join(" | ")
        }
    }
}

/// A string literal with escapes.
pub fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    escape_into(s, &mut out);
    out.push('"');
    out
}

fn escape_into(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '{' => out.push_str("\\{"),
            '}' => out.push_str("\\}"),
            c => out.push(c),
        }
    }
}

fn float_text(v: f64) -> String {
    let s = format!("{v:?}");
    if s.contains('e') && !s.contains('.') {
        // `1e300` lexes as a float already.
        return s;
    }
    s
}

/// Binding strength used to decide parentheses; see the grammar's levels.
fn prec_of(e: &Expr) -> u8 {
    match &e.kind {
        ExprKind::Lambda { .. } | ExprKind::Range { .. } => 0,
        ExprKind::Binary { op, .. } => op.prec(),
        ExprKind::Unary { op: UnOp::Not, .. } => 3,
        ExprKind::Unary { op: UnOp::Neg, .. } | ExprKind::Try(_) => 7,
        _ => 9,
    }
}

/// `let a = x, b = y` of an `if let`.
fn binds_text(
    binds: &[crate::ast::LetBind],
    level: usize,
    comments: &[Comment],
    seen: &mut usize,
) -> String {
    let parts: Vec<String> = binds
        .iter()
        .map(|b| {
            let value = expr_with(&b.value, 0, level, comments, seen);
            format!("{} = {value}", b.name.name)
        })
        .collect();
    format!("let {}", parts.join(", "))
}

/// A calendar literal as written: an instant without its seconds when they are zero.
fn calendar_text(text: &str) -> String {
    match text.strip_suffix(":00Z") {
        Some(minutes) if text.len() == 20 => format!("{minutes}Z"),
        _ => text.to_owned(),
    }
}

/// An expression in its canonical text, on one line where it fits: how a description quotes
/// what the source wrote (an action parameter's default).
pub(crate) fn expr_source(e: &Expr) -> String {
    expr_text(e, 0, 0)
}

fn expr_text(e: &Expr, min: u8, level: usize) -> String {
    let mut seen = 0;
    expr_with(e, min, level, &[], &mut seen)
}

/// An expression in its canonical text, printing the `//` comments inside its blocks and match
/// arms above the statement or arm they were written over, as the printer does for statements.
fn expr_with(e: &Expr, min: u8, level: usize, comments: &[Comment], seen: &mut usize) -> String {
    let body = match &e.kind {
        ExprKind::Int(v) => v.to_string(),
        ExprKind::Float(v) => float_text(*v),
        ExprKind::Length(v, u) => format!("{v}{}", u.text()),
        ExprKind::Duration(v, u) => format!("{v}{u}"),
        ExprKind::Calendar(text) => calendar_text(text),
        ExprKind::Range { lo, hi } => {
            let mut end = |x: &Option<Box<Expr>>| {
                x.as_deref()
                    .map(|x| expr_with(x, 1, level, comments, seen))
                    .unwrap_or_default()
            };
            format!("{}..{}", end(lo), end(hi))
        }
        ExprKind::Bool(b) => b.to_string(),
        ExprKind::Str(parts) => {
            let mut out = String::from("\"");
            for part in parts {
                match part {
                    StrPart::Lit(s) => escape_into(s, &mut out),
                    StrPart::Interp(x) => {
                        out.push('{');
                        out.push_str(&expr_with(x, 0, level, comments, seen));
                        out.push('}');
                    }
                }
            }
            out.push('"');
            out
        }
        ExprKind::Ref(r) => {
            let app = if r.app.is_empty() {
                String::new()
            } else {
                format!("{}.", r.app)
            };
            let ver = r
                .version
                .as_ref()
                .map(|v| format!("@{v}"))
                .unwrap_or_default();
            format!("@{app}{}:{}{ver}", r.ty, r.id)
        }
        ExprKind::Ident(n) => n.clone(),
        ExprKind::Tuple(items) => {
            let inner: Vec<String> = items
                .iter()
                .map(|x| expr_with(x, 0, level, comments, seen))
                .collect();
            format!("({})", inner.join(", "))
        }
        ExprKind::List(items) => {
            let inner: Vec<String> = items
                .iter()
                .map(|x| expr_with(x, 0, level, comments, seen))
                .collect();
            format!("[{}]", inner.join(", "))
        }
        ExprKind::If { cond, then, els } => {
            let mut out = format!(
                "if {} {}",
                expr_with(cond, 0, level, comments, seen),
                block_text(then, level, comments, seen)
            );
            match els.as_deref() {
                None => {}
                Some(Expr {
                    kind: ExprKind::Block(b),
                    ..
                }) => out.push_str(&format!(" else {}", block_text(b, level, comments, seen))),
                Some(other) => out.push_str(&format!(
                    " else {}",
                    expr_with(other, 0, level, comments, seen)
                )),
            }
            out
        }
        ExprKind::IfLet { binds, then, els } => {
            let mut out = format!(
                "if {} {}",
                binds_text(binds, level, comments, seen),
                block_text(then, level, comments, seen)
            );
            match els.as_deref() {
                None => {}
                Some(Expr {
                    kind: ExprKind::Block(b),
                    ..
                }) => out.push_str(&format!(" else {}", block_text(b, level, comments, seen))),
                Some(other) => out.push_str(&format!(
                    " else {}",
                    expr_with(other, 0, level, comments, seen)
                )),
            }
            out
        }
        ExprKind::Block(b) => block_text(b, level, comments, seen),
        ExprKind::Match { scrutinee, arms } => {
            let mut out = format!(
                "match {} {{\n",
                expr_with(scrutinee, 0, level, comments, seen)
            );
            for arm in arms {
                comments_into(&mut out, level + 1, comments, seen, arm.pattern.span.start);
                let body = expr_with(&arm.body, 0, level + 1, comments, seen);
                let comma = if matches!(arm.body.kind, ExprKind::Block(_)) {
                    ""
                } else {
                    ","
                };
                out.push_str(&format!(
                    "{}{} => {body}{comma}\n",
                    indent(level + 1),
                    pattern_text(&arm.pattern)
                ));
            }
            // A comment after the last arm is the match's, not the next statement's.
            comments_into(&mut out, level + 1, comments, seen, e.span.end);
            out.push_str(&indent(level));
            out.push('}');
            out
        }
        ExprKind::Variant { name, args } => match args {
            None => format!(".{}", name.name),
            Some(args) => format!(".{}({})", name.name, args_text(args, level, comments, seen)),
        },
        ExprKind::Lambda { params, body } => {
            let names: Vec<&str> = params.iter().map(|param| param.name.as_str()).collect();
            let body = expr_with(body, 0, level, comments, seen);
            if names.len() == 1 {
                format!("{} -> {body}", names[0])
            } else {
                format!("({}) -> {body}", names.join(", "))
            }
        }
        ExprKind::Unary {
            op: UnOp::Neg,
            operand,
        } => format!("-{}", expr_with(operand, 7, level, comments, seen)),
        ExprKind::Unary {
            op: UnOp::Not,
            operand,
        } => format!("not {}", expr_with(operand, 3, level, comments, seen)),
        ExprKind::Binary { op, lhs, rhs } => {
            let p = op.prec();
            // Comparisons do not chain: both sides must bind tighter.
            let left_min = if p == 4 { 5 } else { p };
            format!(
                "{} {} {}",
                expr_with(lhs, left_min, level, comments, seen),
                op.text(),
                expr_with(rhs, p.saturating_add(1), level, comments, seen)
            )
        }
        ExprKind::Field { base, name } => format!(
            "{}.{}",
            expr_with(base, 8, level, comments, seen),
            name.name
        ),
        ExprKind::OptChain { base, name, args } => {
            let call = args
                .as_ref()
                .map(|a| format!("({})", args_text(a, level, comments, seen)))
                .unwrap_or_default();
            format!(
                "{}?.{}{call}",
                expr_with(base, 8, level, comments, seen),
                name.name
            )
        }
        ExprKind::Call { callee, args } => {
            format!(
                "{}({})",
                expr_with(callee, 8, level, comments, seen),
                args_text(args, level, comments, seen)
            )
        }
        ExprKind::Try(x) => format!("try {}", expr_with(x, 7, level, comments, seen)),
        ExprKind::Root(t) => format!("root {}", print_type(t)),
    };
    if prec_of(e) < min {
        format!("({body})")
    } else {
        body
    }
}

struct Printer<'a> {
    out: String,
    level: usize,
    /// The file's plain `//` comments, in source order.
    comments: &'a [Comment],
    /// How many of them have been printed: the printer writes nodes in source order, so a
    /// comment is printed above the first node that starts after it.
    seen: usize,
}

impl Printer<'_> {
    /// An expression printed at this printer's level, its comments placed as they are written.
    fn ex(&mut self, e: &Expr) -> String {
        expr_with(e, 0, self.level, self.comments, &mut self.seen)
    }

    fn line(&mut self, text: &str) {
        for _ in 0..self.level {
            self.out.push_str("    ");
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn docs(&mut self, doc: &[String]) {
        for d in doc {
            if d.is_empty() {
                self.line("///");
            } else {
                self.line(&format!("/// {d}"));
            }
        }
    }

    /// The plain `//` comments written above the node that starts at `start`.
    fn comments_before(&mut self, start: usize) {
        comments_into(
            &mut self.out,
            self.level,
            self.comments,
            &mut self.seen,
            start,
        );
    }

    fn params(params: &[Param]) -> String {
        let inner: Vec<String> = params
            .iter()
            .map(|p| {
                let default = p
                    .default
                    .as_ref()
                    .map(|d| format!(" = {}", expr_text(d, 0, 0)))
                    .unwrap_or_default();
                format!("{}: {}{default}", p.name.name, print_type(&p.ty))
            })
            .collect();
        format!("({})", inner.join(", "))
    }

    fn item(&mut self, item: &Item) {
        self.docs(item.doc());
        self.comments_before(item.span().start);
        match item {
            Item::Use(u) => {
                let alias = u
                    .alias
                    .as_ref()
                    .map(|a| format!(" as {}", a.name))
                    .unwrap_or_default();
                self.line(&format!("use {}{alias}", u.app));
            }
            Item::Type(t) => {
                match &t.origin {
                    Some(origin) => {
                        self.line(&format!(
                            "type {} (origin: {}) {{",
                            t.name.name, origin.name
                        ));
                    }
                    None => self.line(&format!("type {} {{", t.name.name)),
                }
                self.level += 1;
                for f in &t.fields {
                    self.field(f);
                }
                if let Some(search) = &t.search {
                    if !t.fields.is_empty() {
                        self.out.push('\n');
                    }
                    self.search(search);
                }
                self.level -= 1;
                self.line("}");
            }
            Item::Enum(e) => {
                self.line(&format!("enum {} {{", e.name.name));
                self.level += 1;
                for v in &e.variants {
                    self.docs(&v.doc);
                    self.comments_before(v.span.start);
                    let mut text = v.name.name.clone();
                    if !v.fields.is_empty() {
                        text.push_str(&Self::params(&v.fields));
                    }
                    text.push_str(&self.opts(&v.opts));
                    self.line(&text);
                }
                self.level -= 1;
                self.line("}");
            }
            Item::Root(r) => match &r.name {
                Some(name) => self.line(&format!("root {}: {}", name.name, print_type(&r.ty))),
                None => self.line(&format!("root {}", print_type(&r.ty))),
            },
            Item::Alias(a) => {
                self.line(&format!("type {} = {}", a.name.name, print_type(&a.ty)));
            }
            Item::DefaultView(d) => self.line(&format!(
                "default view {} for {}",
                d.view.name,
                print_type(&d.ty)
            )),
            Item::RowView(d) => self.line(&format!(
                "row view {} for {}",
                d.view.name,
                print_type(&d.ty)
            )),
            Item::Scenario(sc) => {
                let result = sc
                    .result
                    .as_ref()
                    .map(|r| format!(" -> {}", print_type(r)))
                    .unwrap_or_default();
                self.line(&format!("scenario {}{result} {{", quote(&sc.name)));
                self.block_body(&sc.body);
                self.line("}");
            }
            Item::Action(a) => {
                let role = a
                    .role
                    .map(|(r, _)| format!("{} ", r.word()))
                    .unwrap_or_default();
                let mut head = format!("{role}action {}{}", a.name.name, Self::params(&a.params));
                if let Some(r) = &a.result {
                    head.push_str(&format!(" -> {}", print_type(r)));
                }
                if let Some(p) = &a.proposal {
                    head.push_str(&format!(" proposal {}", p.view.name));
                    if let Some(args) = &p.args {
                        let names: Vec<&str> = args.iter().map(|i| i.name.as_str()).collect();
                        head.push_str(&format!("({})", names.join(", ")));
                    }
                }
                head.push_str(&self.opts(&a.opts));
                head.push_str(" {");
                self.line(&head);
                self.level += 1;
                for r in &a.requires {
                    let (cond, reason) = (self.ex(&r.cond), self.ex(&r.reason));
                    self.line(&format!("require {cond} {reason}"));
                }
                self.level -= 1;
                self.block_body(&a.body);
                self.line("}");
            }
            Item::Flow(f) => {
                let mut head = format!("flow {}{}", f.name.name, Self::params(&f.params));
                if let Some(r) = &f.result {
                    head.push_str(&format!(" -> {}", print_type(r)));
                }
                head.push_str(" {");
                self.line(&head);
                self.block_body(&f.body);
                self.line("}");
            }
            Item::Fn(f) => {
                self.line(&format!(
                    "fn {}{} -> {} {{",
                    f.name.name,
                    Self::params(&f.params),
                    print_type(&f.result)
                ));
                self.block_body(&f.body);
                self.line("}");
            }
            Item::View(v) => {
                let types = if v.type_params.is_empty() {
                    String::new()
                } else {
                    let names: Vec<&str> = v.type_params.iter().map(|t| t.name.as_str()).collect();
                    format!("<{}>", names.join(", "))
                };
                self.line(&format!(
                    "view {}{types}{} {{",
                    v.name.name,
                    Self::params(&v.params)
                ));
                self.ui_body(&v.body);
                self.line("}");
            }
        }
    }

    /// ` (icon: .x, label: "y")`, or nothing without options.
    fn opts(&mut self, opts: &[ActionOpt]) -> String {
        if opts.is_empty() {
            return String::new();
        }
        let opts: Vec<String> = opts
            .iter()
            .map(|o| format!("{}: {}", o.name.name, self.ex(&o.value)))
            .collect();
        format!(" ({})", opts.join(", "))
    }

    fn field(&mut self, f: &FieldDecl) {
        self.docs(&f.doc);
        self.comments_before(f.span.start);
        let mut text = format!("{}: {}", f.name.name, print_type(&f.ty));
        if let Some(m) = &f.merge {
            text.push_str(&format!(" merge {}", m.kind.name()));
        }
        if let Some(m) = &f.marker {
            text.push_str(&format!(" ({})", m.name));
        }
        if let Some(d) = &f.default {
            text.push_str(&format!(" = {}", self.ex(d)));
        }
        self.line(&text);
    }

    fn search(&mut self, search: &SearchBlock) {
        self.line("search {");
        self.level += 1;
        for entry in &search.entries {
            self.comments_before(entry.span.start);
            let text = match &entry.kind {
                SearchEntryKind::Field { field, label, opts } => match label {
                    Some(label) => {
                        format!("filter {} {}{}", field.name, quote(label), self.opts(opts))
                    }
                    None => format!("filter {}{}", field.name, self.opts(opts)),
                },
                SearchEntryKind::Named {
                    label, expr, opts, ..
                } => format!(
                    "filter {} = {}{}",
                    quote(label),
                    self.ex(expr),
                    self.opts(opts)
                ),
                SearchEntryKind::Sort { field, order } => {
                    format!("sort {} {}", field.name, order.name)
                }
                SearchEntryKind::Hide { field } => format!("hide {}", field.name),
                SearchEntryKind::MarkOthers => "mark others".to_owned(),
            };
            self.line(&text);
        }
        self.level -= 1;
        self.line("}");
    }

    fn block_body(&mut self, b: &Block) {
        self.level += 1;
        for s in &b.stmts {
            self.stmt(s);
        }
        // A comment after the last statement is the block's, not the next one's.
        self.comments_before(b.span.end);
        self.level -= 1;
    }

    fn stmt(&mut self, s: &Stmt) {
        self.comments_before(s.span.start);
        match &s.kind {
            StmtKind::Let { name, ty, value } => {
                let ann = ty
                    .as_ref()
                    .map(|t| format!(": {}", print_type(t)))
                    .unwrap_or_default();
                let v = self.ex(value);
                self.line(&format!("let {}{ann} = {v}", name.name));
            }
            StmtKind::Assign { place, op, value } => {
                let path: Vec<&str> = place.segs.iter().map(|i| i.name.as_str()).collect();
                let v = self.ex(value);
                self.line(&format!("{} {} {v}", path.join("."), op.text()));
            }
            StmtKind::For { var, iter, body } => {
                let it = self.ex(iter);
                self.line(&format!("for {} in {it} {{", var.name));
                self.block_body(body);
                self.line("}");
            }
            StmtKind::As { who, at, body } => {
                let w = self.ex(who);
                let at = at
                    .as_ref()
                    .map(|t| format!(" at {}", self.ex(t)))
                    .unwrap_or_default();
                self.line(&format!("as {w}{at} {{"));
                self.block_body(body);
                self.line("}");
            }
            StmtKind::Return(None) => self.line("return"),
            StmtKind::Return(Some(e)) => {
                let v = self.ex(e);
                self.line(&format!("return {v}"));
            }
            StmtKind::Expr(e) => {
                let v = self.ex(e);
                self.line(&v);
            }
        }
    }

    fn ui_body(&mut self, stmts: &[UiStmt]) {
        self.level += 1;
        for s in stmts {
            self.ui_stmt(s);
        }
        self.level -= 1;
    }

    fn ui_stmt(&mut self, s: &UiStmt) {
        self.comments_before(s.span.start);
        match &s.kind {
            UiKind::Let { name, ty, value } => {
                let v = self.ex(value);
                let ty = ty
                    .as_ref()
                    .map(|t| format!(": {}", print_type(t)))
                    .unwrap_or_default();
                self.line(&format!("let {}{ty} = {v}", name.name));
            }
            UiKind::State(d) => {
                let default = d
                    .default
                    .as_ref()
                    .map(|e| format!(" = {}", self.ex(e)))
                    .unwrap_or_default();
                self.line(&format!(
                    "{} {}: {}{default}",
                    d.layer.word(),
                    d.name.name,
                    print_type(&d.ty)
                ));
            }
            UiKind::For { var, iter, body } => {
                let it = self.ex(iter);
                self.line(&format!("for {} in {it} {{", var.name));
                self.ui_body(body);
                self.line("}");
            }
            UiKind::If { .. } | UiKind::IfLet { .. } => self.ui_if(s, "if"),
            UiKind::Match { scrutinee, arms } => {
                let m = self.ex(scrutinee);
                self.line(&format!("match {m} {{"));
                self.level += 1;
                for arm in arms {
                    self.comments_before(arm.pattern.span.start);
                    let pat = pattern_text(&arm.pattern);
                    if arm.braced {
                        if arm.body.is_empty() {
                            self.line(&format!("{pat} => {{}}"));
                        } else {
                            self.line(&format!("{pat} => {{"));
                            self.ui_body(&arm.body);
                            self.line("}");
                        }
                        continue;
                    }
                    let mut sub = Printer {
                        out: String::new(),
                        level: self.level,
                        comments: &[],
                        seen: 0,
                    };
                    for st in &arm.body {
                        sub.ui_stmt(st);
                    }
                    let text = sub.out.trim_start().trim_end_matches('\n').to_owned();
                    self.line(&format!("{pat} => {text},"));
                }
                self.level -= 1;
                self.line("}");
            }
            UiKind::Fact { label, value, .. } => {
                let l = self.ex(label);
                let v = self.ex(value);
                self.line(&format!("{l}: {v}"));
            }
            UiKind::Slot { name, body, .. } => {
                self.line(&format!("{}: {{", name.name));
                self.level += 1;
                for st in body {
                    self.ui_stmt(st);
                }
                self.level -= 1;
                self.line("}");
            }
            UiKind::On {
                event,
                param,
                at,
                action,
            } => {
                let a = self.ex(action);
                let named = |p: &crate::ast::EventParam| match &p.ty {
                    Some(ty) => format!("{}: {}", p.name.name, print_type(ty)),
                    None => p.name.name.clone(),
                };
                let carried = match (param, at) {
                    (Some(p), Some(at)) => format!("({}, {})", named(p), named(at)),
                    (Some(p), None) => format!("({})", named(p)),
                    _ => String::new(),
                };
                self.line(&format!("on {}{carried}: {a}", event.name));
            }
            UiKind::Call(c) => self.ui_call(c),
        }
    }

    /// `if`, `else if` and `else` of a view body; `lead` is the text before the condition.
    fn ui_if(&mut self, s: &UiStmt, lead: &str) {
        let (c, then, els) = match &s.kind {
            UiKind::If { cond, then, els } => (self.ex(cond), then, els),
            UiKind::IfLet { binds, then, els } => (
                binds_text(binds, self.level, self.comments, &mut self.seen),
                then,
                els,
            ),
            _ => return,
        };
        self.line(&format!("{lead} {c} {{"));
        self.ui_body(then);
        match els.as_deref() {
            None => self.line("}"),
            Some(
                [
                    next @ UiStmt {
                        kind: UiKind::If { .. } | UiKind::IfLet { .. },
                        ..
                    },
                ],
            ) => self.ui_if(next, "} else if"),
            Some(e) => {
                self.line("} else {");
                self.ui_body(e);
                self.line("}");
            }
        }
    }

    fn ui_call(&mut self, c: &UiCall) {
        let anchor = c
            .anchor
            .as_ref()
            .map(|a| format!(" as {}", a.name))
            .unwrap_or_default();
        let head = self.ex(&c.call);
        let item = c
            .item
            .as_ref()
            .map(|i| format!(" {} ->", i.name))
            .unwrap_or_default();
        match &c.block {
            None => self.line(&format!("{head}{anchor}")),
            Some(stmts) => {
                self.line(&format!("{head} {{{item}"));
                self.ui_body(stmts);
                self.line(&format!("}}{anchor}"));
            }
        }
    }
}
