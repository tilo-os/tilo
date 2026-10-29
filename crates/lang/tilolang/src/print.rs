//! The canonical printer: syntax tree to the one fixed text projection.

use crate::ast::{
    ActionOpt, AppHeader, Arg, Block, Expr, ExprKind, FieldDecl, Item, Module, Param, Pattern,
    SearchBlock, SearchEntryKind, Stmt, StmtKind, StrPart, TypeExpr, TypeExprKind, UiCall, UiKind,
    UiStmt, UnOp,
};

/// A block of one expression whose text is at most this long prints on one line: `{ x }`.
const INLINE_BLOCK: usize = 40;

/// Prints a module in canonical form.
pub fn print_module(m: &Module) -> String {
    print_file(Some(&m.app), &m.items)
}

/// Prints one file of a package in canonical form: the `app` header when the file has it, then
/// its items, a blank line between items (consecutive `use`s stay together).
pub fn print_file(header: Option<&AppHeader>, items: &[Item]) -> String {
    let mut p = Printer {
        out: String::new(),
        level: 0,
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
    p.out
}

/// Prints one expression in canonical form.
pub fn print_expr(e: &Expr) -> String {
    expr_text(e, 0, 0)
}

fn indent(level: usize) -> String {
    "    ".repeat(level)
}

/// A block as text: on one line when it is one short expression, otherwise one statement per
/// line, indented one level deeper than `level`.
fn block_text(b: &Block, level: usize) -> String {
    if let [
        Stmt {
            kind: StmtKind::Expr(e),
            ..
        },
    ] = b.stmts.as_slice()
    {
        let one = expr_text(e, 0, level);
        if !one.contains('\n') && one.len() <= INLINE_BLOCK {
            return format!("{{ {one} }}");
        }
    }
    if b.stmts.is_empty() {
        return "{}".to_owned();
    }
    let mut sub = Printer {
        out: String::new(),
        level: level + 1,
    };
    for s in &b.stmts {
        sub.stmt(s);
    }
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

fn args_text(args: &[Arg], level: usize) -> String {
    let inner: Vec<String> = args
        .iter()
        .map(|a| match &a.name {
            Some(n) => format!("{}: {}", n.name, expr_text(&a.value, 0, level)),
            None => expr_text(&a.value, 0, level),
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
fn binds_text(binds: &[crate::ast::LetBind], level: usize) -> String {
    let parts: Vec<String> = binds
        .iter()
        .map(|b| format!("{} = {}", b.name.name, expr_text(&b.value, 0, level)))
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
    let body = match &e.kind {
        ExprKind::Int(v) => v.to_string(),
        ExprKind::Float(v) => float_text(*v),
        ExprKind::Length(v, u) => format!("{v}{}", u.text()),
        ExprKind::Duration(v, u) => format!("{v}{u}"),
        ExprKind::Calendar(text) => calendar_text(text),
        ExprKind::Range { lo, hi } => {
            let end = |x: &Option<Box<Expr>>| {
                x.as_deref()
                    .map(|x| expr_text(x, 1, level))
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
                        out.push_str(&expr_text(x, 0, level));
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
            let inner: Vec<String> = items.iter().map(|x| expr_text(x, 0, level)).collect();
            format!("({})", inner.join(", "))
        }
        ExprKind::List(items) => {
            let inner: Vec<String> = items.iter().map(|x| expr_text(x, 0, level)).collect();
            format!("[{}]", inner.join(", "))
        }
        ExprKind::If { cond, then, els } => {
            let mut out = format!(
                "if {} {}",
                expr_text(cond, 0, level),
                block_text(then, level)
            );
            match els.as_deref() {
                None => {}
                Some(Expr {
                    kind: ExprKind::Block(b),
                    ..
                }) => out.push_str(&format!(" else {}", block_text(b, level))),
                Some(other) => out.push_str(&format!(" else {}", expr_text(other, 0, level))),
            }
            out
        }
        ExprKind::IfLet { binds, then, els } => {
            let mut out = format!(
                "if {} {}",
                binds_text(binds, level),
                block_text(then, level)
            );
            match els.as_deref() {
                None => {}
                Some(Expr {
                    kind: ExprKind::Block(b),
                    ..
                }) => out.push_str(&format!(" else {}", block_text(b, level))),
                Some(other) => out.push_str(&format!(" else {}", expr_text(other, 0, level))),
            }
            out
        }
        ExprKind::Block(b) => block_text(b, level),
        ExprKind::Match { scrutinee, arms } => {
            let mut out = format!("match {} {{\n", expr_text(scrutinee, 0, level));
            for arm in arms {
                let body = expr_text(&arm.body, 0, level + 1);
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
            out.push_str(&indent(level));
            out.push('}');
            out
        }
        ExprKind::Variant { name, args } => match args {
            None => format!(".{}", name.name),
            Some(args) => format!(".{}({})", name.name, args_text(args, level)),
        },
        ExprKind::Lambda { param, body } => {
            format!("{} -> {}", param.name, expr_text(body, 0, level))
        }
        ExprKind::Unary {
            op: UnOp::Neg,
            operand,
        } => format!("-{}", expr_text(operand, 7, level)),
        ExprKind::Unary {
            op: UnOp::Not,
            operand,
        } => format!("not {}", expr_text(operand, 3, level)),
        ExprKind::Binary { op, lhs, rhs } => {
            let p = op.prec();
            // Comparisons do not chain: both sides must bind tighter.
            let left_min = if p == 4 { 5 } else { p };
            format!(
                "{} {} {}",
                expr_text(lhs, left_min, level),
                op.text(),
                expr_text(rhs, p.saturating_add(1), level)
            )
        }
        ExprKind::Field { base, name } => format!("{}.{}", expr_text(base, 8, level), name.name),
        ExprKind::OptChain { base, name, args } => {
            let call = args
                .as_ref()
                .map(|a| format!("({})", args_text(a, level)))
                .unwrap_or_default();
            format!("{}?.{}{call}", expr_text(base, 8, level), name.name)
        }
        ExprKind::Call { callee, args } => {
            format!(
                "{}({})",
                expr_text(callee, 8, level),
                args_text(args, level)
            )
        }
        ExprKind::Try(x) => format!("try {}", expr_text(x, 7, level)),
        ExprKind::Root(t) => format!("root {}", print_type(t)),
    };
    if prec_of(e) < min {
        format!("({body})")
    } else {
        body
    }
}

struct Printer {
    out: String,
    level: usize,
}

impl Printer {
    /// An expression printed at this printer's level.
    fn ex(&self, e: &Expr) -> String {
        expr_text(e, 0, self.level)
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
                self.docs(&t.doc);
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
                self.docs(&e.doc);
                self.line(&format!("enum {} {{", e.name.name));
                self.level += 1;
                for v in &e.variants {
                    self.docs(&v.doc);
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
                self.docs(&a.doc);
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
                self.docs(&sc.doc);
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
                self.docs(&a.doc);
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
                self.docs(&f.doc);
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
                self.docs(&f.doc);
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
                self.docs(&v.doc);
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
    fn opts(&self, opts: &[ActionOpt]) -> String {
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
        self.level -= 1;
    }

    fn stmt(&mut self, s: &Stmt) {
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
        match &s.kind {
            UiKind::Let { name, value } => {
                let v = self.ex(value);
                self.line(&format!("let {} = {v}", name.name));
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
            UiKind::IfLet { binds, then, els } => (binds_text(binds, self.level), then, els),
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
