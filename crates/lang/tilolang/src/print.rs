//! The canonical printer: syntax tree to the one fixed text projection.

use crate::ast::{
    ActionOpt, AppHeader, Arg, BinOp, Block, Expr, ExprKind, FieldDecl, Ident, Item, Module, Param,
    Pattern, SearchBlock, SearchEntryKind, Stmt, StmtKind, StrPart, TypeExpr, TypeExprKind, UiCall,
    UiKind, UiStmt, UnOp,
};
use crate::diag::Span;
use crate::lexer::Comment;

/// A block of one expression whose text is at most this long prints on one line: `{ x }`.
const INLINE_BLOCK: usize = 40;

/// The widest line the printer makes, the width rustfmt writes this repository's Rust at: a
/// signature, a call, a chain or a list that does not fit breaks over its lines, one item per
/// line, so what an author wrapped for reading keeps its shape.
const PAGE: usize = 100;

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
        ctx: Ctx {
            comments,
            seen: 0,
            page: Some(PAGE),
        },
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
        if !app.web.is_empty() {
            let hosts: Vec<String> = app.web.iter().map(|host| quote(host)).collect();
            options.push(format!("web: [{}]", hosts.join(", ")));
        }
        let one = format!(
            "app {} {}{}",
            app.id,
            quote(&app.title),
            opts_text(&options)
        );
        if options.is_empty() || fits(&one, 0, &p.ctx) {
            p.line(&one);
        } else {
            // An app with many options breaks them over their own lines.
            p.line(&format!("app {} {} (", app.id, quote(&app.title)));
            p.level += 1;
            for option in &options {
                p.line(&format!("{option},"));
            }
            p.level -= 1;
            p.line(")");
        }
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

/// Prints one expression in canonical form, on one line.
pub fn print_expr(e: &Expr) -> String {
    expr_text(e, 0, 0)
}

fn indent(level: usize) -> String {
    "    ".repeat(level)
}

/// The state printing text shares: the file's `//` comments still to place, and the widest line
/// the text may make.
#[derive(Clone, Copy)]
struct Ctx<'a> {
    /// The file's plain `//` comments, in source order.
    comments: &'a [Comment],
    /// How many of them have been printed: the printer writes nodes in source order, so a
    /// comment is printed above the first node that starts after it.
    seen: usize,
    /// The widest line text may make, or `None` where it cannot take a newline at all: inside a
    /// string literal's `{}`, and in a description quoted on one line.
    page: Option<usize>,
}

impl Ctx<'_> {
    /// A copy whose text is one line: what an item's text is, to measure it against the page.
    ///
    /// It shares the comments still to place, so what it prints keeps their lines; the printing
    /// that follows keeps them only if the one-line text is the one taken.
    fn one_line(&self) -> Self {
        Self {
            comments: self.comments,
            seen: self.seen,
            page: None,
        }
    }
}

/// Whether `text`, written from column `col`, keeps the page: one line no wider than the page.
fn fits(text: &str, col: usize, ctx: &Ctx) -> bool {
    !text.contains('\n') && ctx.page.is_none_or(|page| col + text.len() <= page)
}

/// ` (icon: .x, label: "y")`, the options of an action or an enum variant after its head, or
/// nothing without options.
fn opts_text(opts: &[String]) -> String {
    if opts.is_empty() {
        String::new()
    } else {
        format!(" ({})", opts.join(", "))
    }
}

/// Writes into `out` the plain `//` comments written above the node that starts at `start`,
/// each on a line of its own indented to `level`; the context's `seen` moves past them.
fn comments_into(out: &mut String, level: usize, ctx: &mut Ctx, start: usize) {
    while let Some(text) = ctx
        .comments
        .get(ctx.seen)
        .filter(|c| c.span.start < start)
        .map(|c| c.text.clone())
    {
        ctx.seen += 1;
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
fn holds_comment(ctx: &Ctx, span: Span) -> bool {
    ctx.comments
        .get(ctx.seen..)
        .unwrap_or_default()
        .iter()
        .any(|c| c.span.start >= span.start && c.span.start < span.end)
}

/// `open`, the `items` separated by commas and `close`: on one line when that fits the page from
/// `col`, otherwise one item per line, one level deeper than `level`, each with a trailing comma.
///
/// `item` prints one item at the column it starts at and the level it is written at; an item too
/// long for its line breaks its own text the same way.
fn comma_group<'e, T>(
    items: &'e [T],
    col: usize,
    level: usize,
    ctx: &mut Ctx,
    open: &str,
    close: &str,
    item: impl Fn(&'e T, usize, usize, &mut Ctx) -> String,
) -> String {
    let mut probe = ctx.one_line();
    let flat: Vec<String> = items
        .iter()
        .map(|i| item(i, col, level, &mut probe))
        .collect();
    let one = format!("{open}{}{close}", flat.join(", "));
    if fits(&one, col, ctx) {
        ctx.seen = probe.seen;
        return one;
    }
    let inner = level + 1;
    // The comma that follows each item is part of its line: an item is measured one column
    // shorter, so the line it ends keeps the page.
    let page = ctx.page;
    if let Some(p) = page {
        ctx.page = Some(p.saturating_sub(1));
    }
    let mut out = String::from(open);
    out.push('\n');
    for i in items {
        out.push_str(&indent(inner));
        out.push_str(&item(i, inner * 4, inner, ctx));
        out.push_str(",\n");
    }
    ctx.page = page;
    out.push_str(&indent(level));
    out.push_str(close);
    out
}

/// A block as text: on one line when it is one short expression, otherwise one statement per
/// line, indented one level deeper than `level`. A block whose statements carry a `//` comment
/// prints on several lines, so the comment has a line of its own. `col` is where its `{` goes;
/// `inline` is off when the block around it takes lines of its own anyway.
fn block_text(b: &Block, col: usize, level: usize, ctx: &mut Ctx, inline: bool) -> String {
    if b.stmts.is_empty() && !holds_comment(ctx, b.span) {
        return "{}".to_owned();
    }
    if inline
        && let [
            Stmt {
                kind: StmtKind::Expr(e),
                ..
            },
        ] = b.stmts.as_slice()
        && !holds_comment(ctx, b.span)
    {
        let mut probe = ctx.one_line();
        let one = expr_with(e, 0, col + 2, level, &mut probe);
        if !one.contains('\n')
            && one.len() <= INLINE_BLOCK
            && fits(&format!("{{ {one} }}"), col, ctx)
        {
            ctx.seen = probe.seen;
            return format!("{{ {one} }}");
        }
    }
    let mut sub = Printer {
        out: String::new(),
        level: level + 1,
        ctx: Ctx {
            comments: ctx.comments,
            seen: ctx.seen,
            page: ctx.page,
        },
    };
    for s in &b.stmts {
        sub.stmt(s);
    }
    // A comment after the last statement is the block's, not the next one's.
    sub.comments_before(b.span.end);
    ctx.seen = sub.ctx.seen;
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

fn arg_text(a: &Arg, col: usize, level: usize, ctx: &mut Ctx) -> String {
    match &a.name {
        Some(n) => {
            let value = expr_with(&a.value, 0, col + n.name.len() + 2, level, ctx);
            format!("{}: {value}", n.name)
        }
        None => expr_with(&a.value, 0, col, level, ctx),
    }
}

fn param_text(p: &Param, col: usize, level: usize, ctx: &mut Ctx) -> String {
    let ty = print_type(&p.ty);
    let mut head = format!("{}: {ty}", p.name.name);
    if let Some(place) = &p.place {
        head = format!("{head} ({})", place.name);
    }
    if let Some(label) = &p.label {
        let mut quoted = String::new();
        escape_into(label, &mut quoted);
        head = format!("{head} (label: \"{quoted}\")");
    }
    match &p.default {
        Some(default) => {
            let value = expr_with(default, 0, col + head.len() + 3, level, ctx);
            format!("{head} = {value}")
        }
        None => head,
    }
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

/// ` uses web, time`, or nothing for a declaration that uses no effect.
fn uses_text(uses: &[Ident]) -> String {
    if uses.is_empty() {
        return String::new();
    }
    let names: Vec<&str> = uses.iter().map(|u| u.name.as_str()).collect();
    format!(" uses {}", names.join(", "))
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

/// Whether a long expression may break the line before this operator: the lexer reads a line
/// that starts with one as the line before it continuing. `-`, `not` and `/` may not, since each
/// can begin an expression or a literal of its own.
fn leads_line(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::Or
            | BinOp::And
            | BinOp::Eq
            | BinOp::Ne
            | BinOp::Lt
            | BinOp::Le
            | BinOp::Gt
            | BinOp::Ge
            | BinOp::Add
            | BinOp::Mul
            | BinOp::Rem
            | BinOp::Coalesce
    )
}

/// Binding strength used to decide parentheses; see the grammar's levels.
fn prec_of(e: &Expr) -> u8 {
    match &e.kind {
        ExprKind::Lambda { .. } | ExprKind::Range { .. } => 0,
        ExprKind::Binary { op, .. } => op.prec(),
        ExprKind::Unary { op: UnOp::Not, .. } => 3,
        ExprKind::Unary { op: UnOp::Neg, .. } | ExprKind::Try(_) | ExprKind::Suggest(_) => 7,
        _ => 9,
    }
}

/// `let a = x, b = y` of an `if let`.
fn binds_text(binds: &[crate::ast::LetBind], col: usize, level: usize, ctx: &mut Ctx) -> String {
    let mut probe = ctx.one_line();
    let flat: Vec<String> = binds
        .iter()
        .map(|b| bind_text(b, col + 4, level, &mut probe))
        .collect();
    let one = format!("let {}", flat.join(", "));
    if fits(&one, col, ctx) {
        ctx.seen = probe.seen;
        return one;
    }
    // The binds break after their commas, one level in, each on a line of its own.
    let inner = level + 1;
    let page = ctx.page;
    if let Some(p) = page {
        ctx.page = Some(p.saturating_sub(1));
    }
    let mut out = String::from("let ");
    for (i, b) in binds.iter().enumerate() {
        if i > 0 {
            out.push_str(",\n");
            out.push_str(&indent(inner));
        }
        let at = if i == 0 { col + 4 } else { inner * 4 };
        out.push_str(&bind_text(b, at, inner, ctx));
    }
    ctx.page = page;
    out
}

/// One `name = value` of an `if let`.
fn bind_text(b: &crate::ast::LetBind, col: usize, level: usize, ctx: &mut Ctx) -> String {
    let head = format!("{} = ", b.name.name);
    let value = expr_with(&b.value, 0, col + head.len(), level, ctx);
    format!("{head}{value}")
}

/// A calendar literal as written: an instant without its seconds when they are zero.
fn calendar_text(text: &str) -> String {
    // The canonical text carries the seconds a time may be written without.
    match text.strip_suffix(":00Z") {
        Some(minutes) if text.len() == 20 => format!("{minutes}Z"),
        _ => match text.strip_suffix(":00") {
            Some(minutes) if text.len() == 19 => minutes.to_owned(),
            _ => text.to_owned(),
        },
    }
}

/// An expression in its canonical text, on one line where it fits: how a description quotes
/// what the source wrote (an action parameter's default).
pub(crate) fn expr_source(e: &Expr) -> String {
    expr_text(e, 0, 0)
}

fn expr_text(e: &Expr, min: u8, level: usize) -> String {
    let mut ctx = Ctx {
        comments: &[],
        seen: 0,
        page: None,
    };
    expr_with(e, min, 0, level, &mut ctx)
}

/// One step of an access chain: a name after a `.`, or the arguments that follow one.
enum Seg<'a> {
    /// `.name`
    Field(&'a Ident),
    /// `?.name`
    Optional(&'a Ident),
    /// The `(args)` that follow the step before it.
    Args(&'a [Arg]),
    /// The `<types>` a call takes before its arguments.
    Types(&'a [TypeExpr]),
}

fn seg_text(seg: &Seg<'_>, col: usize, level: usize, ctx: &mut Ctx) -> String {
    match seg {
        Seg::Field(name) => format!(".{}", name.name),
        Seg::Optional(name) => format!("?.{}", name.name),
        Seg::Args(args) => comma_group(args, col, level, ctx, "(", ")", arg_text),
        Seg::Types(types) => {
            let written: Vec<String> = types.iter().map(print_type).collect();
            format!("<{}>", written.join(", "))
        }
    }
}

/// The base of an access chain and its steps, outermost last: `a.b(x)?.c` is `a` then `.b`, the
/// arguments `(x)`, `?.c`.
fn chain_of(e: &Expr) -> (&Expr, Vec<Seg<'_>>) {
    let mut segs = Vec::new();
    let mut cur = e;
    loop {
        match &cur.kind {
            ExprKind::Call {
                callee,
                types,
                args,
                ..
            } => {
                segs.push(Seg::Args(args));
                if !types.is_empty() {
                    segs.push(Seg::Types(types));
                }
                cur = callee;
            }
            ExprKind::OptChain { base, name, args } => {
                if let Some(a) = args {
                    segs.push(Seg::Args(a));
                }
                segs.push(Seg::Optional(name));
                cur = base;
            }
            ExprKind::Field { base, name } => {
                segs.push(Seg::Field(name));
                cur = base;
            }
            _ => break,
        }
    }
    segs.reverse();
    (cur, segs)
}

/// A name with its accesses: `a.b`, `f(x).g(y)`, `a?.b(x)`.
///
/// On one line when it fits the page. Otherwise a name's arguments go one per line — the call
/// keeps its name and breaks its `(args)` — and a chain that reads a call's result breaks before
/// each `.`, the calls inside it breaking only when their own line is still too long.
fn postfix(e: &Expr, col: usize, level: usize, ctx: &mut Ctx) -> String {
    let (base, segs) = chain_of(e);
    let mut probe = ctx.one_line();
    let mut one = expr_with(base, 8, col, level, &mut probe);
    for seg in &segs {
        let at = col + one.len();
        one.push_str(&seg_text(seg, at, level, &mut probe));
    }
    if fits(&one, col, ctx) {
        ctx.seen = probe.seen;
        return one;
    }
    // A name's call is a call: `.rows(..)` keeps its name and breaks its arguments.
    let last = segs.len().saturating_sub(1);
    if matches!(base.kind, ExprKind::Ident(_))
        && matches!(segs.get(last), Some(Seg::Args(_)))
        && segs[..last].iter().all(|s| !matches!(s, Seg::Args(_)))
        && let [.., Seg::Args(args)] = segs.as_slice()
    {
        let mut name = expr_with(base, 8, col, level, ctx);
        for seg in &segs[..last] {
            name.push_str(&seg_text(seg, col + name.len(), level, ctx));
        }
        let open = col + name.len();
        let args = comma_group(args, open, level, ctx, "(", ")", arg_text);
        return format!("{name}{args}");
    }
    // A chain over a call's result breaks before each `.`: the leading fields stay on the first
    // line, the calls after them each take one.
    let inner = level + 1;
    let mut out = expr_with(base, 8, col, level, ctx);
    let mut broken = false;
    for (i, seg) in segs.iter().enumerate() {
        let line = if broken { inner * 4 } else { col };
        let at = line + out.len() - out.rfind('\n').map_or(0, |i| i + 1);
        match seg {
            Seg::Args(_) | Seg::Types(_) => out.push_str(&seg_text(seg, at, inner, ctx)),
            Seg::Field(_) | Seg::Optional(_) => {
                let text = seg_text(seg, at, inner, ctx);
                let call = matches!(segs.get(i + 1), Some(Seg::Args(_)));
                if !broken && !call && fits(&text, at, ctx) {
                    out.push_str(&text);
                } else {
                    broken = true;
                    out.push('\n');
                    out.push_str(&indent(inner));
                    let at = inner * 4 + seg_text_len(seg);
                    out.push_str(&seg_text(seg, at, inner, ctx));
                }
            }
        }
    }
    out
}

/// How wide a chain step's name is, before the arguments that may follow it.
fn seg_text_len(seg: &Seg<'_>) -> usize {
    match seg {
        Seg::Field(name) => name.name.len() + 1,
        Seg::Optional(name) => name.name.len() + 2,
        // The types a call takes fit on the line their arguments do.
        Seg::Args(_) | Seg::Types(_) => 0,
    }
}

/// An expression in its canonical text from column `col`, printing the `//` comments inside its
/// blocks and match arms above the statement or arm they were written over, as the printer does
/// for statements.
fn expr_with(e: &Expr, min: u8, col: usize, level: usize, ctx: &mut Ctx) -> String {
    let body = match &e.kind {
        ExprKind::Int(v) => v.to_string(),
        ExprKind::Float(v) => float_text(*v),
        ExprKind::Length(v, u) => format!("{v}{}", u.text()),
        ExprKind::Duration(v, u) => format!("{v}{u}"),
        ExprKind::Calendar(text) => calendar_text(text),
        ExprKind::Range { lo, hi } => {
            let mut end = |x: &Option<Box<Expr>>| {
                x.as_deref()
                    .map(|x| expr_with(x, 1, col, level, ctx))
                    .unwrap_or_default()
            };
            format!("{}..{}", end(lo), end(hi))
        }
        ExprKind::Bool(b) => b.to_string(),
        ExprKind::Str(parts) => {
            // A string literal cannot take a newline: what its `{}` prints stays on one line, so
            // a long call inside one stays long rather than breaking the literal.
            let page = ctx.page.take();
            let mut out = String::from("\"");
            for part in parts {
                match part {
                    StrPart::Lit(s) => escape_into(s, &mut out),
                    StrPart::Interp(x) => {
                        out.push('{');
                        out.push_str(&expr_with(x, 0, col, level, ctx));
                        out.push('}');
                    }
                }
            }
            ctx.page = page;
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
        ExprKind::Tuple(items) => comma_group(items, col, level, ctx, "(", ")", |x, c, l, ctx| {
            expr_with(x, 0, c, l, ctx)
        }),
        ExprKind::List(items) => comma_group(items, col, level, ctx, "[", "]", |x, c, l, ctx| {
            expr_with(x, 0, c, l, ctx)
        }),
        ExprKind::If { cond, then, els } => {
            let cond = expr_with(cond, 0, col + 3, level, ctx);
            if_text(&format!("if {cond}"), then, els.as_deref(), col, level, ctx)
        }
        ExprKind::IfLet { binds, then, els } => {
            let binds = binds_text(binds, col + 3, level, ctx);
            if_text(
                &format!("if {binds}"),
                then,
                els.as_deref(),
                col,
                level,
                ctx,
            )
        }
        ExprKind::Block(b) => block_text(b, col, level, ctx, true),
        ExprKind::Match { scrutinee, arms } => {
            let mut out = format!(
                "match {} {{\n",
                expr_with(scrutinee, 0, col + 6, level, ctx)
            );
            for arm in arms {
                comments_into(&mut out, level + 1, ctx, arm.pattern.span.start);
                let pattern = pattern_text(&arm.pattern);
                let body_col = (level + 1) * 4 + pattern.len() + 4;
                let body = expr_with(&arm.body, 0, body_col, level + 1, ctx);
                let comma = if matches!(arm.body.kind, ExprKind::Block(_)) {
                    ""
                } else {
                    ","
                };
                out.push_str(&format!(
                    "{}{pattern} => {body}{comma}\n",
                    indent(level + 1)
                ));
            }
            // A comment after the last arm is the match's, not the next statement's.
            comments_into(&mut out, level + 1, ctx, e.span.end);
            out.push_str(&indent(level));
            out.push('}');
            out
        }
        ExprKind::Variant { name, args } => match args {
            None => format!(".{}", name.name),
            Some(args) => {
                let lead = format!(".{}", name.name);
                let open = col + lead.len();
                let args = comma_group(args, open, level, ctx, "(", ")", arg_text);
                format!("{lead}{args}")
            }
        },
        ExprKind::Lambda { params, body } => {
            let names: Vec<&str> = params.iter().map(|param| param.name.as_str()).collect();
            let head = if names.len() == 1 {
                names[0].to_owned()
            } else {
                format!("({})", names.join(", "))
            };
            let body = expr_with(body, 0, col + head.len() + 4, level, ctx);
            format!("{head} -> {body}")
        }
        ExprKind::Unary {
            op: UnOp::Neg,
            operand,
        } => format!("-{}", expr_with(operand, 7, col + 1, level, ctx)),
        ExprKind::Unary {
            op: UnOp::Not,
            operand,
        } => format!("not {}", expr_with(operand, 3, col + 4, level, ctx)),
        ExprKind::Binary { op, lhs, rhs } => {
            let p = op.prec();
            // Comparisons do not chain: both sides must bind tighter.
            let left_min = if p == 4 { 5 } else { p };
            let mut probe = ctx.one_line();
            let left = expr_with(lhs, left_min, col, level, &mut probe);
            let right = expr_with(
                rhs,
                p.saturating_add(1),
                col + left.len() + op.text().len() + 2,
                level,
                &mut probe,
            );
            let one = format!("{left} {} {right}", op.text());
            if fits(&one, col, ctx) || !leads_line(*op) || left.ends_with('}') {
                ctx.seen = probe.seen;
                one
            } else {
                // Every operator of the chain leads a line of its own, one level in: how a long
                // condition or fallback reads down the page.
                let inner = level + 1;
                let mut spine = vec![(*op, rhs.as_ref())];
                let mut base = lhs.as_ref();
                while let ExprKind::Binary {
                    op: below,
                    lhs: l,
                    rhs: r,
                } = &base.kind
                {
                    if below.prec() != p || !leads_line(*below) {
                        break;
                    }
                    spine.push((*below, r.as_ref()));
                    base = l.as_ref();
                }
                spine.reverse();
                let mut trial = *ctx;
                let mut out = expr_with(base, left_min, col, level, &mut trial);
                let mut broke = true;
                for (below, x) in &spine {
                    // A line that ends a block ends a statement too: it cannot be continued.
                    if out.ends_with('}') {
                        broke = false;
                        break;
                    }
                    let at = inner * 4 + below.text().len() + 1;
                    let text = expr_with(x, p.saturating_add(1), at, inner, &mut trial);
                    out.push_str(&format!("\n{}{} {text}", indent(inner), below.text()));
                }
                if broke {
                    ctx.seen = trial.seen;
                    out
                } else {
                    ctx.seen = probe.seen;
                    one
                }
            }
        }
        ExprKind::Field { .. } | ExprKind::OptChain { .. } | ExprKind::Call { .. } => {
            postfix(e, col, level, ctx)
        }
        ExprKind::Try(x) => format!("try {}", expr_with(x, 7, col + 4, level, ctx)),
        ExprKind::Suggest(x) => format!("suggest {}", expr_with(x, 7, col + 8, level, ctx)),
        ExprKind::Root(t) => format!("root {}", print_type(t)),
    };
    if prec_of(e) < min {
        format!("({body})")
    } else {
        body
    }
}

/// `if` and its branches: `head {..}`, `head {..} else {..}`, `head {..} else if ..`.
///
/// The whole branch prints on one line when it fits, its blocks one expression each; otherwise
/// every block takes lines of its own, so a long condition or a chain of `else if`s reads down
/// the page rather than across it.
fn if_text(
    head: &str,
    then: &Block,
    els: Option<&Expr>,
    col: usize,
    level: usize,
    ctx: &mut Ctx,
) -> String {
    let mut probe = ctx.one_line();
    let one = branch(head, then, els, col, level, &mut probe, true);
    if fits(&one, col, ctx) {
        ctx.seen = probe.seen;
        return one;
    }
    branch(head, then, els, col, level, ctx, false)
}

/// A branch with its blocks on one line (`inline`) or over their lines. The `else` decides for
/// itself: it lands on a line of its own, so a short one stays one line even beside a block that
/// took several.
fn branch(
    head: &str,
    then: &Block,
    els: Option<&Expr>,
    col: usize,
    level: usize,
    ctx: &mut Ctx,
    inline: bool,
) -> String {
    // The block opens after the head's last line, `col` when the head is one line.
    let open = match head.rfind('\n') {
        Some(i) => head.len() - i,
        None => col + head.len() + 1,
    };
    let mut out = format!("{head} {}", block_text(then, open, level, ctx, inline));
    if let Some(els) = els {
        // The `else` follows the closing `}` on its line, or the one-line block.
        let at = match out.rfind('\n') {
            Some(i) => out.len() - i - 1 + 6,
            None => col + out.len() + 6,
        };
        match &els.kind {
            ExprKind::Block(b) => {
                let block = block_text(b, at, level, ctx, true);
                out.push_str(&format!(" else {block}"));
            }
            _ => {
                let text = expr_with(els, 0, at, level, ctx);
                out.push_str(&format!(" else {text}"));
            }
        }
    }
    out
}

struct Printer<'a> {
    out: String,
    level: usize,
    ctx: Ctx<'a>,
}

impl Printer<'_> {
    /// An expression printed at this printer's level, its comments placed as they are written.
    fn ex(&mut self, e: &Expr) -> String {
        let col = self.level * 4;
        expr_with(e, 0, col, self.level, &mut self.ctx)
    }

    /// An expression that starts at column `col` rather than at this level's indentation, as one
    /// does after `let x = ` or `return `.
    fn ex_at(&mut self, e: &Expr, col: usize) -> String {
        expr_with(e, 0, col, self.level, &mut self.ctx)
    }

    /// An expression that must keep `after` more columns free on its last line.
    fn ex_before(&mut self, e: &Expr, after: usize) -> String {
        let page = self.ctx.page;
        if let Some(p) = page {
            self.ctx.page = Some(p.saturating_sub(after));
        }
        let text = self.ex(e);
        self.ctx.page = page;
        text
    }

    /// Whether `text` from column `col` keeps the page.
    fn fits(&self, col: usize, text: &str) -> bool {
        fits(text, col, &self.ctx)
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
        comments_into(&mut self.out, self.level, &mut self.ctx, start);
    }

    /// A declaration's head: `lead(items)tail` and then the parenthesised `opts` and `close`.
    ///
    /// It prints on one line when it fits the page. When it does not, the part too long for the
    /// page breaks: the `opts` go one per line after the `(` when the items fit, and otherwise
    /// the `items` go one per line with a trailing comma, indented one level deeper, the line
    /// that closes them keeping the `tail` — the result type or the proposal — before the
    /// `opts`, which break that way too when that line is still too wide.
    fn head<T>(
        &mut self,
        lead: &str,
        items: &[T],
        tail: &str,
        opts: &[String],
        close: &str,
        item: impl Fn(&T, usize, usize, &mut Ctx) -> String,
    ) {
        let col = self.level * 4;
        let opts_one = opts_text(opts);
        if items.is_empty() {
            let head = format!("{lead}(){tail}");
            let one = format!("{head}{opts_one}{close}");
            if opts.is_empty() || self.fits(col, &one) {
                self.line(&one);
                return;
            }
            self.line(&format!("{head} ("));
            self.level += 1;
            for o in opts {
                self.line(&format!("{o},"));
            }
            self.level -= 1;
            self.line(&format!("){close}"));
            return;
        }
        let mut probe = self.ctx.one_line();
        let flat: Vec<String> = items
            .iter()
            .map(|i| item(i, col, self.level, &mut probe))
            .collect();
        let one = format!("{lead}({}){tail}{opts_one}{close}", flat.join(", "));
        if fits(&one, col, &self.ctx) {
            self.ctx.seen = probe.seen;
            self.line(&one);
            return;
        }
        // Options too long for the page break while the items stay on the head, which is the
        // shorter of the two.
        let opens = format!("{lead}({}){tail} (", flat.join(", "));
        if !opts.is_empty() && self.fits(col, &opens) {
            self.ctx.seen = probe.seen;
            self.line(&opens);
            self.level += 1;
            for o in opts {
                self.line(&format!("{o},"));
            }
            self.level -= 1;
            self.line(&format!("){close}"));
            return;
        }
        self.line(&format!("{lead}("));
        self.level += 1;
        let inner = self.level;
        // Each item's line ends with its comma, which the page has to hold too.
        let page = self.ctx.page;
        if let Some(p) = page {
            self.ctx.page = Some(p.saturating_sub(1));
        }
        for i in items {
            let text = item(i, inner * 4, inner, &mut self.ctx);
            self.line(&format!("{text},"));
        }
        self.ctx.page = page;
        self.level -= 1;
        let closes = format!("){tail}{opts_one}{close}");
        if opts.is_empty() || self.fits(col, &closes) {
            self.line(&closes);
            return;
        }
        self.line(&format!("){tail} ("));
        self.level += 1;
        for o in opts {
            self.line(&format!("{o},"));
        }
        self.level -= 1;
        self.line(&format!("){close}"));
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
                let mut options = Vec::new();
                if let Some(origin) = &t.origin {
                    options.push(format!("origin: {}", origin.name));
                }
                if let Some(title) = &t.title {
                    options.push(format!("title: {}", title.field.name));
                }
                if let Some(untitled) = &t.untitled {
                    options.push(format!("untitled: {}", quote(&untitled.words)));
                }
                let options = if options.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", options.join(", "))
                };
                self.line(&format!("type {}{options} {{", t.name.name));
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
                    let opts = self.opts(&v.opts);
                    if v.fields.is_empty() {
                        // A variant without fields is its name alone, then its options.
                        let text = format!("{}{}", v.name.name, opts_text(&opts));
                        self.line(&text);
                    } else {
                        self.head(&v.name.name, &v.fields, "", &opts, "", param_text);
                    }
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
            Item::Views(d) => {
                let views = d
                    .views
                    .iter()
                    .map(|view| {
                        let opts = self.opts(&view.opts);
                        format!("{}{}", view.view.name, opts_text(&opts))
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                self.line(&format!("views {views} for {}", print_type(&d.ty)));
            }
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
            Item::Trigger(t) => {
                let params = t.param.as_ref().map_or_else(String::new, |p| {
                    format!("({}: {})", p.name.name, print_type(&p.ty))
                });
                let when = match &t.when {
                    crate::ast::TriggerWhen::Every { amount, unit, .. } => {
                        format!("every {amount}{unit}")
                    }
                    crate::ast::TriggerWhen::At { expr, .. } => {
                        format!("at {}", self.ex(expr))
                    }
                };
                let call = self.ex(&t.call);
                self.line(&format!("trigger {}{params} {when}: {call}", t.name.name));
            }
            Item::Action(a) => {
                let role = a
                    .role
                    .map(|(r, _)| format!("{} ", r.word()))
                    .unwrap_or_default();
                let mut tail = String::new();
                if let Some(r) = &a.result {
                    tail.push_str(&format!(" -> {}", print_type(r)));
                }
                tail.push_str(&uses_text(&a.uses));
                if let Some(p) = &a.proposal {
                    tail.push_str(&format!(" proposal {}", p.view.name));
                    if let Some(args) = &p.args {
                        let names: Vec<&str> = args.iter().map(|i| i.name.as_str()).collect();
                        tail.push_str(&format!("({})", names.join(", ")));
                    }
                }
                let opts = self.opts(&a.opts);
                let lead = format!("{role}action {}", a.name.name);
                self.head(&lead, &a.params, &tail, &opts, " {", param_text);
                self.level += 1;
                for (word, lines) in [("require", &a.requires), ("applies", &a.applies)] {
                    for r in lines {
                        self.comments_before(r.span.start);
                        let col = self.level * 4;
                        let cond = self.ex_at(&r.cond, col + 8);
                        let reason = self.ex_at(&r.reason, col + 8 + cond.len() + 1);
                        self.line(&format!("{word} {cond} {reason}"));
                    }
                }
                self.level -= 1;
                self.block_body(&a.body);
                self.line("}");
            }
            Item::Fn(f) => {
                let tail = format!(" -> {}", print_type(&f.result));
                let lead = format!("fn {}", f.name.name);
                self.head(&lead, &f.params, &tail, &[], " {", param_text);
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
                let lead = format!(
                    "{}view {}{types}",
                    if v.template { "template " } else { "" },
                    v.name.name
                );
                self.head(&lead, &v.params, "", &[], " {", param_text);
                self.ui_body(&v.body);
                self.line("}");
            }
        }
    }

    /// `icon: .x, label: "y"`, the options of an action or an enum variant, in canonical text.
    fn opts(&mut self, opts: &[ActionOpt]) -> Vec<String> {
        opts.iter()
            .map(|o| {
                let value = self.ex(&o.value);
                format!("{}: {value}", o.name.name)
            })
            .collect()
    }

    fn field(&mut self, f: &FieldDecl) {
        self.docs(&f.doc);
        self.comments_before(f.span.start);
        let mut text = format!("{}: {}", f.name.name, print_type(&f.ty));
        if let Some(m) = &f.merge {
            text.push_str(&format!(" merge {}", m.kind.name()));
        }
        if let Some(d) = &f.default {
            let col = self.level * 4 + text.len() + 3;
            let value = self.ex_at(d, col);
            text.push_str(&format!(" = {value}"));
        }
        self.line(&text);
    }

    fn search(&mut self, search: &SearchBlock) {
        self.line("search {");
        self.level += 1;
        for entry in &search.entries {
            self.comments_before(entry.span.start);
            let text = match &entry.kind {
                SearchEntryKind::Field { field, label, opts } => {
                    let opts = opts_text(&self.opts(opts));
                    match label {
                        Some(label) => format!("filter {} {}{opts}", field.name, quote(label)),
                        None => format!("filter {}{opts}", field.name),
                    }
                }
                SearchEntryKind::Named {
                    label, expr, opts, ..
                } => {
                    let label = quote(label);
                    let opts = opts_text(&self.opts(opts));
                    let head = format!("filter {label} = ");
                    // The options come after the condition on its last line.
                    let page = self.ctx.page;
                    if let Some(p) = page {
                        self.ctx.page = Some(p.saturating_sub(opts.len()));
                    }
                    let value = self.ex_at(expr, self.level * 4 + head.len());
                    self.ctx.page = page;
                    format!("{head}{value}{opts}")
                }
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
                let head = format!("let {}{ann} = ", name.name);
                let col = self.level * 4 + head.len();
                let v = self.ex_at(value, col);
                self.line(&format!("{head}{v}"));
            }
            StmtKind::Assign { place, op, value } => {
                let path: Vec<&str> = place.segs.iter().map(|i| i.name.as_str()).collect();
                let head = format!("{} {} ", path.join("."), op.text());
                let col = self.level * 4 + head.len();
                let v = self.ex_at(value, col);
                self.line(&format!("{head}{v}"));
            }
            StmtKind::For { var, iter, body } => {
                let head = format!("for {} in ", var.name);
                let col = self.level * 4 + head.len();
                let it = self.ex_at(iter, col);
                self.line(&format!("{head}{it} {{"));
                self.block_body(body);
                self.line("}");
            }
            StmtKind::Return(None) => self.line("return"),
            StmtKind::Return(Some(e)) => {
                let col = self.level * 4 + 7;
                let v = self.ex_at(e, col);
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
                let ty = ty
                    .as_ref()
                    .map(|t| format!(": {}", print_type(t)))
                    .unwrap_or_default();
                let head = format!("let {}{ty} = ", name.name);
                let col = self.level * 4 + head.len();
                let v = self.ex_at(value, col);
                self.line(&format!("{head}{v}"));
            }
            UiKind::State(d) => {
                let head = format!("state {}: {}", d.name.name, print_type(&d.ty));
                let default = match &d.default {
                    Some(e) => {
                        let col = self.level * 4 + head.len() + 3;
                        format!(" = {}", self.ex_at(e, col))
                    }
                    None => String::new(),
                };
                self.line(&format!("{head}{default}"));
            }
            UiKind::For { var, iter, body } => {
                let head = format!("for {} in ", var.name);
                let col = self.level * 4 + head.len();
                let it = self.ex_at(iter, col);
                self.line(&format!("{head}{it} {{"));
                self.ui_body(body);
                self.line("}");
            }
            UiKind::If { .. } | UiKind::IfLet { .. } => self.ui_if(s, "if"),
            UiKind::Match { scrutinee, arms } => {
                let head = "match ";
                let col = self.level * 4 + head.len();
                let m = self.ex_at(scrutinee, col);
                self.line(&format!("{head}{m} {{"));
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
                    // The arm's body stands after `{pat} => `: what it prints keeps that much
                    // room free on its first line.
                    let page = self.ctx.page;
                    let mut sub = Printer {
                        out: String::new(),
                        level: self.level,
                        ctx: Ctx {
                            comments: &[],
                            seen: 0,
                            page: page.map(|p| p.saturating_sub(pat.len() + 4)),
                        },
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
                let col = self.level * 4;
                let l = self.ex_at(label, col);
                let v = self.ex_at(value, col + l.len() + 2);
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
                let named = |p: &crate::ast::EventParam| match &p.ty {
                    Some(ty) => format!("{}: {}", p.name.name, print_type(ty)),
                    None => p.name.name.clone(),
                };
                let carried = match (param, at) {
                    (Some(p), Some(at)) => format!("({}, {})", named(p), named(at)),
                    (Some(p), None) => format!("({})", named(p)),
                    _ => String::new(),
                };
                let head = format!("on {}{carried}: ", event.name);
                let col = self.level * 4 + head.len();
                let a = self.ex_at(action, col);
                self.line(&format!("{head}{a}"));
            }
            UiKind::Call(c) => self.ui_call(c),
        }
    }

    /// `if`, `else if` and `else` of a view body; `lead` is the text before the condition.
    fn ui_if(&mut self, s: &UiStmt, lead: &str) {
        let (c, then, els) = match &s.kind {
            UiKind::If { cond, then, els } => {
                let col = self.level * 4 + lead.len() + 1;
                (self.ex_at(cond, col), then, els)
            }
            UiKind::IfLet { binds, then, els } => {
                let col = self.level * 4 + lead.len() + 1;
                (binds_text(binds, col, self.level, &mut self.ctx), then, els)
            }
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
        let item = match c.params.as_slice() {
            [] => String::new(),
            [one] => format!(" {} ->", one.name),
            many => {
                let names: Vec<&str> = many.iter().map(|p| p.name.as_str()).collect();
                format!(" ({}) ->", names.join(", "))
            }
        };
        // What follows the call on its line: the block it opens, or the name it is anchored to.
        let after = match &c.block {
            None => anchor.clone(),
            Some(_) => format!(" {{{item}"),
        };
        let head = self.ex_before(&c.call, after.len());
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
