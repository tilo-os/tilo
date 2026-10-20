//! The canonical printer: syntax tree to the one fixed text projection.

use crate::ast::{
    Block, Else, Expr, ExprKind, FieldDecl, Item, Module, Param, Stmt, StmtKind, StrPart, TypeExpr,
    TypeExprKind, UiCall, UiKind, UiStmt, UnOp,
};

/// Prints a module in canonical form.
pub fn print_module(m: &Module) -> String {
    let mut p = Printer {
        out: String::new(),
        level: 0,
    };
    p.line(&format!("app {} {}", m.app.id, quote(&m.app.title)));
    let mut prev_use = false;
    for item in &m.items {
        let is_use = matches!(item, Item::Use(_));
        if !(is_use && prev_use) {
            p.out.push('\n');
        }
        prev_use = is_use;
        p.item(item);
    }
    p.out
}

/// Prints one expression in canonical form.
pub fn print_expr(e: &Expr) -> String {
    expr_text(e, 0)
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
        ExprKind::Lambda { .. } | ExprKind::IfExpr { .. } => 0,
        ExprKind::Binary { op, .. } => op.prec(),
        ExprKind::Unary { op: UnOp::Not, .. } => 3,
        ExprKind::Unary { op: UnOp::Neg, .. } | ExprKind::Do(_) => 7,
        _ => 9,
    }
}

fn expr_text(e: &Expr, min: u8) -> String {
    let body = match &e.kind {
        ExprKind::Int(v) => v.to_string(),
        ExprKind::Float(v) => float_text(*v),
        ExprKind::Bool(b) => b.to_string(),
        ExprKind::None => "none".into(),
        ExprKind::Str(parts) => {
            let mut out = String::from("\"");
            for part in parts {
                match part {
                    StrPart::Lit(s) => escape_into(s, &mut out),
                    StrPart::Interp(x) => {
                        out.push('{');
                        out.push_str(&expr_text(x, 0));
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
            let inner: Vec<String> = items.iter().map(|x| expr_text(x, 0)).collect();
            format!("({})", inner.join(", "))
        }
        ExprKind::List(items) => {
            let inner: Vec<String> = items.iter().map(|x| expr_text(x, 0)).collect();
            format!("[{}]", inner.join(", "))
        }
        ExprKind::IfExpr { cond, then, els } => {
            format!(
                "if {} then {} else {}",
                expr_text(cond, 0),
                expr_text(then, 0),
                expr_text(els, 0)
            )
        }
        ExprKind::Lambda { param, body } => format!("{} -> {}", param.name, expr_text(body, 0)),
        ExprKind::Unary {
            op: UnOp::Neg,
            operand,
        } => format!("-{}", expr_text(operand, 7)),
        ExprKind::Unary {
            op: UnOp::Not,
            operand,
        } => format!("not {}", expr_text(operand, 3)),
        ExprKind::Binary { op, lhs, rhs } => {
            let p = op.prec();
            // Comparisons do not chain: both sides must bind tighter.
            let left_min = if p == 4 { 5 } else { p };
            format!(
                "{} {} {}",
                expr_text(lhs, left_min),
                op.text(),
                expr_text(rhs, p.saturating_add(1))
            )
        }
        ExprKind::Field { base, name } => format!("{}.{}", expr_text(base, 8), name.name),
        ExprKind::Call { callee, args } => {
            let inner: Vec<String> = args
                .iter()
                .map(|a| match &a.name {
                    Some(n) => format!("{}: {}", n.name, expr_text(&a.value, 0)),
                    None => expr_text(&a.value, 0),
                })
                .collect();
            format!("{}({})", expr_text(callee, 8), inner.join(", "))
        }
        ExprKind::Do(x) => format!("do {}", expr_text(x, 7)),
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
            .map(|p| format!("{}: {}", p.name.name, print_type(&p.ty)))
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
                self.line(&format!("type {} {{", t.name.name));
                self.level += 1;
                for f in &t.fields {
                    self.field(f);
                }
                self.level -= 1;
                self.line("}");
            }
            Item::Action(a) => {
                self.docs(&a.doc);
                let mut head = format!("action {}{}", a.name.name, Self::params(&a.params));
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
                head.push_str(" {");
                self.line(&head);
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
                self.line(&format!(
                    "view {}{} {{",
                    v.name.name,
                    Self::params(&v.params)
                ));
                self.ui_body(&v.body);
                self.line("}");
            }
        }
    }

    fn field(&mut self, f: &FieldDecl) {
        self.docs(&f.doc);
        let mut text = format!("{}: {}", f.name.name, print_type(&f.ty));
        if let Some(m) = &f.merge {
            text.push_str(&format!(" merge {}", m.kind.name()));
        }
        if let Some(d) = &f.default {
            text.push_str(&format!(" = {}", print_expr(d)));
        }
        self.line(&text);
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
                self.line(&format!("let {}{ann} = {}", name.name, print_expr(value)));
            }
            StmtKind::Assign { place, op, value } => {
                let path: Vec<&str> = place.segs.iter().map(|i| i.name.as_str()).collect();
                self.line(&format!(
                    "{} {} {}",
                    path.join("."),
                    op.text(),
                    print_expr(value)
                ));
            }
            StmtKind::If { .. } => self.if_chain(s, "if"),
            StmtKind::For { var, iter, body } => {
                self.line(&format!("for {} in {} {{", var.name, print_expr(iter)));
                self.block_body(body);
                self.line("}");
            }
            StmtKind::Return(None) => self.line("return"),
            StmtKind::Return(Some(e)) => self.line(&format!("return {}", print_expr(e))),
            StmtKind::Expr(e) => self.line(&print_expr(e)),
        }
    }

    /// Prints `if`/`else if`/`else`; `lead` is the text before the condition.
    fn if_chain(&mut self, s: &Stmt, lead: &str) {
        let StmtKind::If { cond, then, els } = &s.kind else {
            return;
        };
        self.line(&format!("{lead} {} {{", print_expr(cond)));
        self.block_body(then);
        match els {
            None => self.line("}"),
            Some(Else::Block(b)) => {
                self.line("} else {");
                self.block_body(b);
                self.line("}");
            }
            Some(Else::If(next)) => {
                // Print the `else if` header on the closing line.
                let mut sub = Printer {
                    out: String::new(),
                    level: self.level,
                };
                sub.if_chain(next, "} else if");
                // The nested chain starts with the indent and the `}` we need here.
                self.out.push_str(&sub.out);
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
                self.line(&format!("let {} = {}", name.name, print_expr(value)))
            }
            UiKind::For { var, iter, body } => {
                self.line(&format!("for {} in {} {{", var.name, print_expr(iter)));
                self.ui_body(body);
                self.line("}");
            }
            UiKind::If { cond, then, els } => {
                self.line(&format!("if {} {{", print_expr(cond)));
                self.ui_body(then);
                match els {
                    Some(e) => {
                        self.line("} else {");
                        self.ui_body(e);
                        self.line("}");
                    }
                    None => self.line("}"),
                }
            }
            UiKind::Call(c) => self.ui_call(c),
        }
    }

    fn ui_call(&mut self, c: &UiCall) {
        let anchor = c
            .anchor
            .as_ref()
            .map(|a| format!(" #{}", a.name))
            .unwrap_or_default();
        let head = print_expr(&c.call);
        match &c.block {
            None => self.line(&format!("{head}{anchor}")),
            Some(stmts) => {
                self.line(&format!("{head} {{"));
                self.ui_body(stmts);
                self.line(&format!("}}{anchor}"));
            }
        }
    }
}
