//! The parser: tokens to the syntax tree. `grammar.md` next to the crate's README writes out
//! the grammar it reads.
//!
//! Syntax errors are recovered at statement and item boundaries, so one run reports several.

use crate::ast::{
    ActionDecl, ActionOpt, ActionRole, AliasDecl, AppHeader, Arg, AssignOp, BinOp, Block,
    DefaultViewDecl, EnumDecl, Expr, ExprKind, FieldDecl, FlowDecl, FnDecl, Ident, Item, LetBind,
    MatchArm, MergeSpec, Module, NodeId, Param, Pattern, Place, Proposal, RootDecl, ScenarioDecl,
    SearchBlock, SearchEntry, SearchEntryKind, StateDecl, StateLayer, Stmt, StmtKind, StrPart,
    TypeDecl, TypeExpr, TypeExprKind, UiArm, UiCall, UiKind, UiStmt, UnOp, UseDecl, VariantDecl,
    ViewDecl,
};
use crate::diag::{Diagnostic, Span};
use crate::lexer::{StrPiece, Tok, Token, lex, lex_at};
use crate::types::MergeKind;

/// Whether call arguments name only a filter's options: `(icon: .circle)`.
fn only_opts(args: &[Arg]) -> bool {
    !args.is_empty()
        && args
            .iter()
            .all(|a| a.name.as_ref().is_some_and(|n| n.name == "icon"))
}

/// The arguments as a filter's options.
fn opts_of(args: Vec<Arg>) -> Vec<ActionOpt> {
    args.into_iter()
        .filter_map(|a| {
            Some(ActionOpt {
                name: a.name?,
                value: a.value,
            })
        })
        .collect()
}

/// A named filter's predicate and the options written after it: `not done (icon: .circle)`
/// parses as a call of `done`, and `project == .none (icon: .inbox)` as a case with fields,
/// each told from what it reads like by naming only options.
fn split_opts(e: Expr) -> (Expr, Vec<ActionOpt>) {
    let span = e.span;
    match e.kind {
        ExprKind::Variant {
            name,
            args: Some(args),
        } if only_opts(&args) => {
            let span = Span::new(span.start, name.span.end);
            let kind = ExprKind::Variant { name, args: None };
            (Expr { kind, span }, opts_of(args))
        }
        ExprKind::Call { callee, args } if only_opts(&args) => (*callee, opts_of(args)),
        ExprKind::Unary { op, operand } => {
            let (inner, opts) = split_opts(*operand);
            let span = Span::new(span.start, inner.span.end);
            let kind = ExprKind::Unary {
                op,
                operand: Box::new(inner),
            };
            (Expr { kind, span }, opts)
        }
        ExprKind::Binary { op, lhs, rhs } => {
            let (inner, opts) = split_opts(*rhs);
            let span = Span::new(span.start, inner.span.end);
            let kind = ExprKind::Binary {
                op,
                lhs,
                rhs: Box::new(inner),
            };
            (Expr { kind, span }, opts)
        }
        kind => (Expr { kind, span }, Vec::new()),
    }
}

/// A syntax error that was already reported.
struct Abort;

type PResult<T> = Result<T, Abort>;

/// Parses a whole `app.tl` file.
pub fn parse_module(src: &str) -> (Option<Module>, Vec<Diagnostic>) {
    let (toks, mut diags) = lex(src);
    let mut p = Parser::new(toks, src);
    let module = p.module();
    diags.append(&mut p.diags);
    (module, diags)
}

/// One file of a package, parsed: its `app` header when it has one, and its items.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedFile {
    pub header: Option<AppHeader>,
    pub items: Vec<Item>,
    /// The first node id after this file's: the next file of the package starts there.
    pub next_node: NodeId,
}

/// Parses one file of a package whose first byte sits at `base` in the package's span space
/// and whose first node id is `first_node`. The `app` header is optional here; the package
/// decides which file must hold it.
pub fn parse_file_at(src: &str, base: usize, first_node: NodeId) -> (ParsedFile, Vec<Diagnostic>) {
    let (toks, mut diags) = lex_at(src, base);
    let mut p = Parser::new_at(toks, src, base);
    p.next_node = first_node;
    let (header, items) = p.file();
    diags.append(&mut p.diags);
    let parsed = ParsedFile {
        header,
        items,
        next_node: p.next_node,
    };
    (parsed, diags)
}

/// Parses one expression (a block of an agent answer). Returns the expression, or `None` when
/// the text is not a single expression; the second value tells whether errors were reported.
pub fn parse_expr_source(src: &str) -> (Option<Expr>, Vec<Diagnostic>) {
    let (toks, mut diags) = lex(src);
    let mut p = Parser::new(toks, src);
    p.skip_nl();
    let expr = p.expr().ok();
    p.skip_nl();
    let complete = matches!(p.peek(), Tok::Eof);
    diags.append(&mut p.diags);
    (if complete { expr } else { None }, diags)
}

/// Parses the text as the statements of an agent's program: `let`, `for`, assignments and
/// expressions, one per line.
pub fn parse_program_source(src: &str) -> (Vec<Stmt>, Vec<Diagnostic>) {
    let (toks, mut diags) = lex(src);
    let mut p = Parser::new(toks, src);
    let mut out = Vec::new();
    p.skip_nl();
    while !matches!(p.peek(), Tok::Eof) {
        match p.stmt() {
            Ok(s) => out.push(s),
            Err(Abort) => p.recover_stmt(),
        }
        p.skip_nl();
        if matches!(p.peek(), Tok::RBrace) {
            p.unexpected::<()>("a statement").ok();
            p.bump();
        }
    }
    diags.append(&mut p.diags);
    (out, diags)
}

/// Parses the text as the statements of a view body.
pub fn parse_ui_source(src: &str) -> (Vec<UiStmt>, Vec<Diagnostic>) {
    let (toks, mut diags) = lex(src);
    let mut p = Parser::new(toks, src);
    let mut out = Vec::new();
    p.skip_nl();
    while !matches!(p.peek(), Tok::Eof) {
        match p.ui_stmt() {
            Ok(s) => out.push(s),
            Err(Abort) => p.recover_stmt(),
        }
        p.skip_nl();
    }
    diags.append(&mut p.diags);
    (out, diags)
}

fn bin(op: BinOp, lhs: Expr, rhs: Expr) -> Expr {
    let span = lhs.span.to(rhs.span);
    Expr {
        kind: ExprKind::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        },
        span,
    }
}

struct Parser<'a> {
    toks: Vec<Token>,
    pos: usize,
    src: &'a str,
    /// Where `src` starts in the span space: spans are `base` + the offset in `src`.
    base: usize,
    diags: Vec<Diagnostic>,
    next_node: NodeId,
}

/// What the options after an app's title declare.
#[derive(Default)]
struct AppOptions {
    icon: Option<String>,
    category: Option<String>,
    previously: Vec<String>,
    own_records: bool,
}

/// The launcher categories an app declares (`category: .office`), as the platform's menu
/// categories name them in lower case.
pub const APP_CATEGORIES: &[&str] = &[
    "office",
    "development",
    "graphics",
    "multimedia",
    "network",
    "education",
    "science",
    "game",
    "utility",
    "system",
];
const ITEM_KWS: &[&str] = &["use", "type", "enum", "action", "flow", "view", "fn"];
const TYPE_ARGS: &[&str] = &[
    "List",
    "Set",
    "Map",
    "Collection",
    "Option",
    "Result",
    "State",
];
const ITEM_EXPECTED: &str = "`use`, `type`, `enum`, `root`, `default view`, `row view`, `action`, `flow`, `view`, `fn` or `scenario`";

impl<'a> Parser<'a> {
    fn new(toks: Vec<Token>, src: &'a str) -> Self {
        Self::new_at(toks, src, 0)
    }

    fn new_at(mut toks: Vec<Token>, src: &'a str, base: usize) -> Self {
        if toks.last().is_none_or(|t| t.tok != Tok::Eof) {
            let end = base + src.len();
            toks.push(Token {
                tok: Tok::Eof,
                span: Span::new(end, end),
            });
        }
        Self {
            toks,
            pos: 0,
            src,
            base,
            diags: Vec::new(),
            next_node: 0,
        }
    }

    fn node(&mut self) -> NodeId {
        let n = self.next_node;
        self.next_node += 1;
        n
    }

    fn peek(&self) -> &Tok {
        self.toks.get(self.pos).map_or(&Tok::Eof, |t| &t.tok)
    }

    fn peek_at(&self, n: usize) -> &Tok {
        self.toks.get(self.pos + n).map_or(&Tok::Eof, |t| &t.tok)
    }

    fn span(&self) -> Span {
        self.toks
            .get(self.pos)
            .map_or_else(Span::default, |t| t.span)
    }

    fn prev_end(&self) -> usize {
        self.pos
            .checked_sub(1)
            .and_then(|i| self.toks.get(i))
            .map_or(0, |t| t.span.end)
    }

    fn bump(&mut self) -> Token {
        let t = self.toks.get(self.pos).cloned().unwrap_or_else(|| Token {
            tok: Tok::Eof,
            span: Span::default(),
        });
        if !matches!(t.tok, Tok::Eof) {
            self.pos += 1;
        }
        t
    }

    fn is(&self, t: &Tok) -> bool {
        self.peek() == t
    }

    fn is_kw(&self, k: &str) -> bool {
        matches!(self.peek(), Tok::Kw(x) if *x == k)
    }

    fn eat(&mut self, t: &Tok) -> bool {
        if self.is(t) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn eat_kw(&mut self, k: &str) -> bool {
        if self.is_kw(k) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn skip_nl(&mut self) {
        while matches!(self.peek(), Tok::Newline) {
            self.bump();
        }
    }

    fn fail<T>(&mut self, span: Span, msg: impl Into<String>) -> PResult<T> {
        self.diags.push(Diagnostic::new("E003", span, msg));
        Err(Abort)
    }

    fn unexpected<T>(&mut self, expected: &str) -> PResult<T> {
        let found = self.peek().describe();
        let span = self.span();
        self.fail(span, format!("expected {expected}, found {found}"))
    }

    fn expect(&mut self, t: &Tok) -> PResult<Span> {
        if self.is(t) {
            Ok(self.bump().span)
        } else {
            self.unexpected(&t.describe())
        }
    }

    fn expect_kw(&mut self, k: &str) -> PResult<Span> {
        if self.is_kw(k) {
            Ok(self.bump().span)
        } else {
            self.unexpected(&format!("`{k}`"))
        }
    }

    fn ident(&mut self, what: &str) -> PResult<Ident> {
        match self.peek().clone() {
            Tok::Ident(name) => {
                let span = self.bump().span;
                Ok(Ident { name, span })
            }
            Tok::Kw(k) => {
                let span = self.span();
                self.fail(
                    span,
                    format!(
                        "expected {what}, found the keyword `{k}`, which cannot be used as a name"
                    ),
                )
            }
            _ => self.unexpected(what),
        }
    }

    fn app_id(&mut self) -> PResult<String> {
        let mut out = self.ident("an app id such as `so.tilo.counter`")?.name;
        while self.is(&Tok::Dot) && matches!(self.peek_at(1), Tok::Ident(_)) {
            self.bump();
            out.push('.');
            out.push_str(&self.ident("an app id segment")?.name);
        }
        Ok(out)
    }

    /// The options after the app's title: `(icon: .note, category: .office, previously:
    /// so.tilo.old, records: .own)`, each at most once. `previously:` takes an app id, or several
    /// in brackets.
    fn app_options(&mut self) -> PResult<AppOptions> {
        let mut out = AppOptions::default();
        let mut seen_previously = false;
        let mut records: Option<String> = None;
        if !self.eat(&Tok::LParen) {
            return Ok(out);
        }
        loop {
            self.skip_nl();
            if self.eat(&Tok::RParen) {
                break;
            }
            let key = self.ident("`icon`, `category`, `previously` or `records`")?;
            self.expect(&Tok::Colon)?;
            if key.name == "previously" {
                if std::mem::replace(&mut seen_previously, true) {
                    return self.fail(key.span, "`previously:` is given twice".to_owned());
                }
                if self.eat(&Tok::LBracket) {
                    loop {
                        self.skip_nl();
                        if self.eat(&Tok::RBracket) {
                            break;
                        }
                        out.previously.push(self.app_id()?);
                        self.skip_nl();
                        if !self.eat(&Tok::Comma) {
                            self.skip_nl();
                            self.expect(&Tok::RBracket)?;
                            break;
                        }
                    }
                } else {
                    out.previously.push(self.app_id()?);
                }
            } else {
                self.expect(&Tok::Dot)?;
                let value = self.ident("a name after the dot, such as `.note`")?;
                let slot = match key.name.as_str() {
                    "icon" => &mut out.icon,
                    "category" => {
                        if !APP_CATEGORIES.contains(&value.name.as_str()) {
                            return self.fail(
                                value.span,
                                format!(
                                    "`.{}` is not a category; an app is one of {}",
                                    value.name,
                                    APP_CATEGORIES
                                        .iter()
                                        .map(|c| format!("`.{c}`"))
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                ),
                            );
                        }
                        &mut out.category
                    }
                    "records" => {
                        if !matches!(value.name.as_str(), "own" | "shared") {
                            return self.fail(
                                value.span,
                                format!(
                                    "`.{}` is not where records are kept; an app's records are \
                                     `.shared` with the workspace or each member's `.own`",
                                    value.name
                                ),
                            );
                        }
                        &mut records
                    }
                    other => {
                        return self.fail(
                            key.span,
                            format!(
                                "an app has an `icon:`, a `category:`, `previously:` and \
                                 `records:`, not `{other}:`"
                            ),
                        );
                    }
                };
                if slot.replace(value.name).is_some() {
                    return self.fail(key.span, format!("`{}:` is given twice", key.name));
                }
            }
            self.skip_nl();
            if !self.eat(&Tok::Comma) {
                self.skip_nl();
                self.expect(&Tok::RParen)?;
                break;
            }
        }
        out.own_records = records.as_deref() == Some("own");
        Ok(out)
    }

    fn plain_string(&mut self, what: &str) -> PResult<String> {
        if let Tok::Str(pieces) = self.peek().clone() {
            let span = self.bump().span;
            let mut out = String::new();
            for piece in pieces {
                match piece {
                    StrPiece::Lit(s) => out.push_str(&s),
                    StrPiece::Interp { .. } => {
                        return self
                            .fail(span, format!("{what} cannot contain `{{ }}` interpolation"));
                    }
                }
            }
            Ok(out)
        } else {
            self.unexpected(what)
        }
    }

    // ---- recovery -------------------------------------------------------------------------

    fn recover_stmt(&mut self) {
        let mut depth = 0usize;
        loop {
            match self.peek() {
                Tok::Eof => return,
                Tok::Newline if depth == 0 => return,
                Tok::LBrace => depth += 1,
                Tok::RBrace => {
                    if depth == 0 {
                        return;
                    }
                    depth -= 1;
                }
                _ => {}
            }
            self.bump();
        }
    }

    fn at_item_start(&self) -> bool {
        matches!(self.peek(), Tok::Doc(_))
            || ITEM_KWS.iter().any(|k| self.is_kw(k))
            || self.at_root_decl()
            || self.at_default_view()
            || self.at_row_view()
            || self.at_scenario()
            || self.at_action_role().is_some()
    }

    /// `row view v for T` at the start of an item.
    fn at_row_view(&self) -> bool {
        matches!(self.peek(), Tok::Ident(n) if n == "row")
            && matches!(self.peek_at(1), Tok::Kw("view"))
    }

    /// `scenario "name" { ... }` at the start of an item.
    fn at_scenario(&self) -> bool {
        matches!(self.peek(), Tok::Ident(n) if n == "scenario")
            && matches!(self.peek_at(1), Tok::Str(_))
    }

    /// `primary` or `destructive` before an action, or before another such word.
    fn at_action_role(&self) -> Option<ActionRole> {
        let role = match self.peek() {
            Tok::Ident(n) if n == "primary" => ActionRole::Primary,
            Tok::Ident(n) if n == "destructive" => ActionRole::Destructive,
            Tok::Ident(n) if n == "migration" => ActionRole::Migration,
            _ => return None,
        };
        let next_is_role = matches!(self.peek_at(1), Tok::Ident(n) if n == "primary" || n == "destructive" || n == "migration");
        (matches!(self.peek_at(1), Tok::Kw("action" | "flow")) || next_is_role).then_some(role)
    }

    /// `root T` at the start of an item.
    fn at_root_decl(&self) -> bool {
        matches!(self.peek(), Tok::Ident(n) if n == "root")
            && matches!(self.peek_at(1), Tok::Ident(_))
    }

    /// `default view v for T` at the start of an item.
    fn at_default_view(&self) -> bool {
        matches!(self.peek(), Tok::Ident(n) if n == "default")
            && matches!(self.peek_at(1), Tok::Kw("view"))
    }

    /// `x` or `x: T`: a value an event carries, named, with its type when written.
    fn event_param(&mut self, what: &str) -> Result<crate::ast::EventParam, Abort> {
        let name = self.ident(what)?;
        let ty = if self.eat(&Tok::Colon) {
            Some(self.type_expr()?)
        } else {
            None
        };
        Ok(crate::ast::EventParam { name, ty })
    }

    fn recover_item(&mut self) {
        let mut depth = 0usize;
        let mut first = true;
        loop {
            match self.peek() {
                Tok::Eof => return,
                Tok::LBrace => depth += 1,
                Tok::RBrace => depth = depth.saturating_sub(1),
                Tok::Newline if depth == 0 => {
                    self.skip_nl();
                    if self.at_item_start() {
                        return;
                    }
                    continue;
                }
                _ => {
                    if depth == 0 && !first && self.at_item_start() && self.pos > 0 {
                        return;
                    }
                }
            }
            first = false;
            self.bump();
        }
    }

    // ---- items ----------------------------------------------------------------------------

    fn module(&mut self) -> Option<Module> {
        self.skip_nl();
        let start = self.span();
        let header = if self.is_kw("app") {
            self.bump();
            let id = self.app_id().ok()?;
            let title = self.plain_string("the app title in quotes").ok()?;
            let options = self.app_options().ok()?;
            AppHeader {
                id,
                title,
                icon: options.icon,
                category: options.category,
                previously: options.previously,
                own_records: options.own_records,
                span: start.to(Span::new(self.prev_end(), self.prev_end())),
            }
        } else {
            self.diags.push(Diagnostic::new(
                "E010",
                start,
                "a module starts with `app <app id> \"<title>\"`, for example `app so.tilo.counter \"Counter\"`",
            ));
            return None;
        };
        let mut items = Vec::new();
        loop {
            self.skip_nl();
            if matches!(self.peek(), Tok::Eof) {
                break;
            }
            match self.item() {
                Ok(item) => items.push(item),
                Err(Abort) => self.recover_item(),
            }
        }
        Some(Module { app: header, items })
    }

    /// A file of a multi-file package: an optional header, then items.
    fn file(&mut self) -> (Option<AppHeader>, Vec<Item>) {
        self.skip_nl();
        let start = self.span();
        let mut header = None;
        if self.is_kw("app") {
            self.bump();
            let id = self.app_id();
            let title = id
                .and_then(|id| {
                    let title = self.plain_string("the app title in quotes")?;
                    Ok((id, title, self.app_options()?))
                })
                .ok();
            match title {
                Some((id, title, options)) => {
                    header = Some(AppHeader {
                        id,
                        title,
                        icon: options.icon,
                        category: options.category,
                        previously: options.previously,
                        own_records: options.own_records,
                        span: start.to(Span::new(self.prev_end(), self.prev_end())),
                    });
                }
                None => self.recover_item(),
            }
        }
        let mut items = Vec::new();
        loop {
            self.skip_nl();
            if matches!(self.peek(), Tok::Eof) {
                break;
            }
            if self.is_kw("app") {
                let span = self.span();
                self.diags.push(Diagnostic::new(
                    "E017",
                    span,
                    "the `app` header comes first in its file, and only once in a package",
                ));
                self.bump();
                self.recover_item();
                continue;
            }
            match self.item() {
                Ok(item) => items.push(item),
                Err(Abort) => self.recover_item(),
            }
        }
        (header, items)
    }

    fn docs(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        while let Tok::Doc(text) = self.peek().clone() {
            self.bump();
            out.push(text);
            self.skip_nl();
        }
        out
    }

    fn item(&mut self) -> PResult<Item> {
        let doc = self.docs();
        let start = self.span();
        if self.at_root_decl() {
            self.bump();
            let node = self.node();
            // `root notes: Notes` names it; `root Notebook` does not.
            let name = if matches!(self.peek_at(1), Tok::Colon) {
                let name = self.ident("the root's name")?;
                self.bump();
                Some(name)
            } else {
                None
            };
            let ty = self.type_expr()?;
            return Ok(Item::Root(RootDecl {
                node,
                name,
                ty,
                span: start.to(Span::new(self.prev_end(), self.prev_end())),
            }));
        }
        if self.at_default_view() {
            self.bump();
            self.bump();
            let node = self.node();
            let view = self.ident("the name of the default view")?;
            self.expect_kw("for")?;
            let ty = self.type_expr()?;
            return Ok(Item::DefaultView(DefaultViewDecl {
                node,
                view,
                ty,
                span: start.to(Span::new(self.prev_end(), self.prev_end())),
            }));
        }
        if self.at_row_view() {
            self.bump();
            self.bump();
            let node = self.node();
            let view = self.ident("the name of the row view")?;
            self.expect_kw("for")?;
            let ty = self.type_expr()?;
            return Ok(Item::RowView(DefaultViewDecl {
                node,
                view,
                ty,
                span: start.to(Span::new(self.prev_end(), self.prev_end())),
            }));
        }
        if self.at_scenario() {
            self.bump();
            let node = self.node();
            let name_span = self.span();
            let name = self.plain_string("the scenario's name in quotes")?;
            let result = if self.eat(&Tok::Arrow) {
                Some(self.type_expr()?)
            } else {
                None
            };
            let body = self.block()?;
            return Ok(Item::Scenario(ScenarioDecl {
                node,
                doc,
                name,
                name_span,
                result,
                body,
                span: start.to(Span::new(self.prev_end(), self.prev_end())),
            }));
        }
        let mut role = None;
        while let Some(r) = self.at_action_role() {
            let span = self.bump().span;
            if role.is_some() {
                self.diags.push(Diagnostic::new(
                    "E090",
                    span,
                    "an action is `primary`, `destructive` or a `migration`, one of them: the primary action is the one main thing to do, a destructive one never is, and a migration is never offered. Keep one of the words",
                ));
                continue;
            }
            role = Some((r, span));
        }
        if role.is_some() && self.is_kw("flow") {
            let span = self.span();
            return self.fail(span, "`primary` and `destructive` mark actions; a flow is offered as it is. Write the flow without the word, or make it an action");
        }
        let Tok::Kw(kw) = self.peek().clone() else {
            return self.unexpected(ITEM_EXPECTED);
        };
        match kw {
            "use" => self.use_decl(start).map(Item::Use),
            "type" if matches!(self.peek_at(2), Tok::Assign) => {
                self.alias_decl(doc, start).map(Item::Alias)
            }
            "type" => self.type_decl(doc, start).map(Item::Type),
            "enum" => self.enum_decl(doc, start).map(Item::Enum),
            "action" => self.action(doc, role, start).map(Item::Action),
            "flow" => self.flow(doc, start).map(Item::Flow),
            "view" => self.view(doc, start).map(Item::View),
            "fn" => self.func(doc, start).map(Item::Fn),
            _ => self.unexpected(ITEM_EXPECTED),
        }
    }

    fn enum_decl(&mut self, doc: Vec<String>, start: Span) -> PResult<EnumDecl> {
        self.expect_kw("enum")?;
        let node = self.node();
        let name = self.ident("an enum name")?;
        self.expect(&Tok::LBrace)?;
        let mut variants = Vec::new();
        loop {
            self.skip_nl();
            if self.eat(&Tok::RBrace) {
                break;
            }
            if matches!(self.peek(), Tok::Eof) {
                return self.unexpected("`}` to close the enum");
            }
            let vdoc = self.docs();
            let vstart = self.span();
            let vnode = self.node();
            let vname = match self.ident("a variant name") {
                Ok(n) => n,
                Err(Abort) => {
                    self.recover_stmt();
                    continue;
                }
            };
            let fields = if self.is(&Tok::LParen) && !self.at_variant_opts() {
                self.params()?
            } else {
                Vec::new()
            };
            let opts = if self.at_variant_opts() {
                self.action_opts()?
            } else {
                Vec::new()
            };
            variants.push(VariantDecl {
                node: vnode,
                doc: vdoc,
                name: vname,
                fields,
                opts,
                span: vstart.to(Span::new(self.prev_end(), self.prev_end())),
            });
            if !self.eat(&Tok::Comma) && !matches!(self.peek(), Tok::Newline | Tok::RBrace) {
                let _: PResult<()> = self.unexpected("a new line, `,` or `}` after the variant");
                self.recover_stmt();
            }
        }
        Ok(EnumDecl {
            node,
            doc,
            name,
            variants,
            span: start.to(Span::new(self.prev_end(), self.prev_end())),
        })
    }

    /// `(icon: .pin)` after a variant: options, told from a payload `(x: T)` by what follows
    /// the colon, which for a payload field is a type.
    fn at_variant_opts(&self) -> bool {
        self.is(&Tok::LParen)
            && matches!(self.peek_at(1), Tok::Ident(_))
            && matches!(self.peek_at(2), Tok::Colon)
            && !matches!(self.peek_at(3), Tok::Ident(_) | Tok::LParen)
    }

    fn use_decl(&mut self, start: Span) -> PResult<UseDecl> {
        self.expect_kw("use")?;
        let node = self.node();
        let app = self.app_id()?;
        let alias = if self.eat_kw("as") {
            Some(self.ident("an alias")?)
        } else {
            None
        };
        Ok(UseDecl {
            node,
            app,
            alias,
            span: start.to(Span::new(self.prev_end(), self.prev_end())),
        })
    }

    /// `type Name = T`.
    fn alias_decl(&mut self, doc: Vec<String>, start: Span) -> PResult<AliasDecl> {
        self.expect_kw("type")?;
        let node = self.node();
        let name = self.ident("a type name")?;
        self.expect(&Tok::Assign)?;
        let ty = self.type_expr()?;
        Ok(AliasDecl {
            node,
            doc,
            name,
            ty,
            span: start.to(Span::new(self.prev_end(), self.prev_end())),
        })
    }

    fn type_decl(&mut self, doc: Vec<String>, start: Span) -> PResult<TypeDecl> {
        self.expect_kw("type")?;
        let node = self.node();
        let name = self.ident("a type name")?;
        let origin = if self.eat(&Tok::LParen) {
            let origin = self.type_origin()?;
            self.expect(&Tok::RParen)?;
            Some(origin)
        } else {
            None
        };
        self.expect(&Tok::LBrace)?;
        let mut fields = Vec::new();
        let mut search: Option<SearchBlock> = None;
        loop {
            self.skip_nl();
            if self.eat(&Tok::RBrace) {
                break;
            }
            if matches!(self.peek(), Tok::Eof) {
                return self.unexpected("`}` to close the type");
            }
            if matches!(self.peek(), Tok::Ident(w) if w == "search")
                && matches!(self.peek_at(1), Tok::LBrace)
            {
                match self.search_block() {
                    Ok(block) => {
                        if search.is_some() {
                            let _: PResult<()> = self.fail(
                                block.span,
                                "a type has one `search` block; put every filter, sort and hide in it",
                            );
                        } else {
                            search = Some(block);
                        }
                    }
                    Err(Abort) => self.recover_stmt(),
                }
                continue;
            }
            match self.field() {
                Ok(f) => {
                    fields.push(f);
                    if !self.eat(&Tok::Comma) && !matches!(self.peek(), Tok::Newline | Tok::RBrace)
                    {
                        let _: PResult<()> =
                            self.unexpected("a new line, `,` or `}` after the field");
                        self.recover_stmt();
                    }
                }
                Err(Abort) => self.recover_stmt(),
            }
        }
        Ok(TypeDecl {
            node,
            doc,
            name,
            origin,
            fields,
            search,
            span: start.to(Span::new(self.prev_end(), self.prev_end())),
        })
    }

    /// `origin: imap.message` in a type's options: the kind of an outside service's resource,
    /// one dotted name.
    fn type_origin(&mut self) -> PResult<Ident> {
        let key = self.ident("`origin`")?;
        if key.name != "origin" {
            return self.fail(
                key.span,
                "a type takes one option, `origin:` (`type Message (origin: imap.message) { ... }`)",
            );
        }
        self.expect(&Tok::Colon)?;
        let first = self.ident("the origin's kind, such as `imap.message`")?;
        let mut name = first.name.clone();
        let mut span = first.span;
        while self.eat(&Tok::Dot) {
            let part = self.ident("the rest of the origin's kind")?;
            name.push('.');
            name.push_str(&part.name);
            span = span.to(part.span);
        }
        Ok(Ident { name, span })
    }

    /// `search { ... }`: one `filter`, `sort` or `hide` per line (search.md, "What a type
    /// declares").
    fn search_block(&mut self) -> PResult<SearchBlock> {
        let start = self.bump().span;
        self.expect(&Tok::LBrace)?;
        let mut entries = Vec::new();
        loop {
            self.skip_nl();
            if self.eat(&Tok::RBrace) {
                break;
            }
            if matches!(self.peek(), Tok::Eof) {
                return self.unexpected("`}` to close the search block");
            }
            match self.search_entry() {
                Ok(entry) => {
                    entries.push(entry);
                    if !self.eat(&Tok::Comma) && !matches!(self.peek(), Tok::Newline | Tok::RBrace)
                    {
                        let _: PResult<()> =
                            self.unexpected("a new line or `}` after the search line");
                        self.recover_stmt();
                    }
                }
                Err(Abort) => self.recover_stmt(),
            }
        }
        Ok(SearchBlock {
            entries,
            span: start.to(Span::new(self.prev_end(), self.prev_end())),
        })
    }

    fn search_entry(&mut self) -> PResult<SearchEntry> {
        let start = self.span();
        let word = self.ident("`filter`, `sort` or `hide`")?;
        let kind = match word.name.as_str() {
            "filter" => {
                if matches!(self.peek(), Tok::Str(_)) {
                    let label_span = self.span();
                    let label = self.plain_string("a filter's label")?;
                    if !self.eat(&Tok::Assign) {
                        return self.unexpected(
                            "`=` and what the filter holds of: `filter \"Open\" = not done`",
                        );
                    }
                    let (expr, opts) = split_opts(self.expr()?);
                    SearchEntryKind::Named {
                        label,
                        label_span,
                        expr,
                        opts,
                    }
                } else {
                    let field = self.ident("the field a filter is on")?;
                    let label = if matches!(self.peek(), Tok::Str(_)) {
                        Some(self.plain_string("a filter's label")?)
                    } else {
                        None
                    };
                    let opts = self.action_opts()?;
                    SearchEntryKind::Field { field, label, opts }
                }
            }
            "sort" => {
                let field = self.ident("the field to sort by")?;
                let order = self.ident("`newest`, `oldest`, `a_z` or `z_a`")?;
                SearchEntryKind::Sort { field, order }
            }
            "hide" => SearchEntryKind::Hide {
                field: self.ident("the field to keep out of filters")?,
            },
            other => {
                return self.fail(
                    word.span,
                    format!(
                        "a search block holds `filter`, `sort` and `hide` lines; `{other}` is none of them"
                    ),
                );
            }
        };
        Ok(SearchEntry {
            kind,
            span: start.to(Span::new(self.prev_end(), self.prev_end())),
        })
    }

    fn field(&mut self) -> PResult<FieldDecl> {
        let doc = self.docs();
        let start = self.span();
        let node = self.node();
        let name = self.ident("a field name")?;
        self.expect(&Tok::Colon)?;
        let ty = self.type_expr()?;
        let merge = if self.is_kw("merge") {
            let s = self.bump().span;
            let kind_ident = self.ident("`register`, `counter` or `text`")?;
            let kind = match kind_ident.name.as_str() {
                "register" => MergeKind::Register,
                "counter" => MergeKind::Counter,
                "text" => MergeKind::Text,
                other => {
                    return self.fail(kind_ident.span, format!("unknown merge kind `{other}`; the merge kinds are register, counter and text"));
                }
            };
            Some(MergeSpec {
                kind,
                span: s.to(kind_ident.span),
            })
        } else {
            None
        };
        let marker = if self.eat(&Tok::LParen) {
            let m = self.ident("`title` or `body`")?;
            self.expect(&Tok::RParen)?;
            Some(m)
        } else {
            None
        };
        let default = if self.eat(&Tok::Assign) {
            Some(self.expr()?)
        } else {
            None
        };
        Ok(FieldDecl {
            node,
            doc,
            name,
            ty,
            merge,
            marker,
            default,
            span: start.to(Span::new(self.prev_end(), self.prev_end())),
        })
    }

    /// A type, or several joined by `|`: a union.
    fn type_expr(&mut self) -> PResult<TypeExpr> {
        let start = self.span();
        let first = self.one_type()?;
        if !self.is(&Tok::Pipe) {
            return Ok(first);
        }
        let mut cases = vec![first];
        while self.eat(&Tok::Pipe) {
            self.skip_nl();
            cases.push(self.one_type()?);
        }
        Ok(TypeExpr {
            kind: TypeExprKind::Union(cases),
            span: start.to(Span::new(self.prev_end(), self.prev_end())),
        })
    }

    fn one_type(&mut self) -> PResult<TypeExpr> {
        let start = self.span();
        if self.eat(&Tok::LParen) {
            if self.is(&Tok::RParen) {
                // `()`: no value, as in `Result<(), Text>`.
                let close = self.bump().span;
                return Ok(TypeExpr {
                    kind: TypeExprKind::Tuple(Vec::new()),
                    span: start.to(close),
                });
            }
            let mut items = vec![self.type_expr()?];
            while self.eat(&Tok::Comma) {
                items.push(self.type_expr()?);
            }
            self.expect(&Tok::RParen)?;
            // `(T) -> View`: a function of the items in parentheses.
            if self.eat(&Tok::Arrow) {
                let result = self.type_expr()?;
                let span = start.to(Span::new(self.prev_end(), self.prev_end()));
                return Ok(TypeExpr {
                    kind: TypeExprKind::Fn(items, Box::new(result)),
                    span,
                });
            }
            let span = start.to(Span::new(self.prev_end(), self.prev_end()));
            return Ok(TypeExpr {
                kind: TypeExprKind::Tuple(items),
                span,
            });
        }
        let first = self.ident("a type")?;
        if TYPE_ARGS.contains(&first.name.as_str()) && self.is(&Tok::Lt) {
            self.bump();
            let mut args = vec![self.type_expr()?];
            while self.eat(&Tok::Comma) {
                args.push(self.type_expr()?);
            }
            self.expect(&Tok::Gt)?;
            let span = start.to(Span::new(self.prev_end(), self.prev_end()));
            return Ok(TypeExpr {
                kind: TypeExprKind::Generic(first, args),
                span,
            });
        }
        let mut path = vec![first];
        while self.is(&Tok::Dot) && matches!(self.peek_at(1), Tok::Ident(_)) {
            self.bump();
            path.push(self.ident("a type name")?);
        }
        let span = start.to(Span::new(self.prev_end(), self.prev_end()));
        Ok(TypeExpr {
            kind: TypeExprKind::Named(path),
            span,
        })
    }

    fn params(&mut self) -> PResult<Vec<Param>> {
        self.expect(&Tok::LParen)?;
        let mut out = Vec::new();
        while !self.is(&Tok::RParen) {
            let node = self.node();
            let name = self.ident("a parameter name")?;
            self.expect(&Tok::Colon)?;
            let ty = self.type_expr()?;
            let default = if self.eat(&Tok::Assign) {
                Some(self.expr()?)
            } else {
                None
            };
            out.push(Param {
                node,
                name,
                ty,
                default,
            });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RParen)?;
        Ok(out)
    }

    fn action(
        &mut self,
        doc: Vec<String>,
        role: Option<(ActionRole, Span)>,
        start: Span,
    ) -> PResult<ActionDecl> {
        self.expect_kw("action")?;
        let node = self.node();
        let name = self.ident("an action name")?;
        let params = self.params()?;
        let result = if self.eat(&Tok::Arrow) {
            Some(self.type_expr()?)
        } else {
            None
        };
        let proposal = if self.eat_kw("proposal") {
            let view = self.ident("the name of the proposal view")?;
            // `(icon: ...)` after the view is the action's options, not the view's arguments.
            let options_follow = matches!(self.peek_at(2), Tok::Colon);
            let args = if !options_follow && self.eat(&Tok::LParen) {
                let mut names = Vec::new();
                while !self.is(&Tok::RParen) {
                    names.push(self.ident("an action parameter")?);
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.expect(&Tok::RParen)?;
                Some(names)
            } else {
                None
            };
            Some(Proposal { view, args })
        } else {
            None
        };
        let opts = self.action_opts()?;
        let (requires, body) = self.action_body()?;
        Ok(ActionDecl {
            node,
            doc,
            role,
            name,
            params,
            result,
            proposal,
            opts,
            requires,
            body,
            span: start.to(Span::new(self.prev_end(), self.prev_end())),
        })
    }

    /// `(icon: .plus, label: "New")` before an action's body.
    fn action_opts(&mut self) -> PResult<Vec<ActionOpt>> {
        let mut out = Vec::new();
        if !self.eat(&Tok::LParen) {
            return Ok(out);
        }
        while !self.is(&Tok::RParen) {
            let name = self.ident("an option of the action: `icon` or `label`")?;
            self.expect(&Tok::Colon)?;
            let value = self.expr()?;
            out.push(ActionOpt { name, value });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RParen)?;
        Ok(out)
    }

    fn flow(&mut self, doc: Vec<String>, start: Span) -> PResult<FlowDecl> {
        self.expect_kw("flow")?;
        let node = self.node();
        let name = self.ident("a flow name")?;
        let params = self.params()?;
        let result = if self.eat(&Tok::Arrow) {
            Some(self.type_expr()?)
        } else {
            None
        };
        let body = self.block()?;
        Ok(FlowDecl {
            node,
            doc,
            name,
            params,
            result,
            body,
            span: start.to(Span::new(self.prev_end(), self.prev_end())),
        })
    }

    fn func(&mut self, doc: Vec<String>, start: Span) -> PResult<FnDecl> {
        self.expect_kw("fn")?;
        let node = self.node();
        let name = self.ident("a function name")?;
        let params = self.params()?;
        self.expect(&Tok::Arrow)?;
        let result = self.type_expr()?;
        let body = self.block()?;
        Ok(FnDecl {
            node,
            doc,
            name,
            params,
            result,
            body,
            span: start.to(Span::new(self.prev_end(), self.prev_end())),
        })
    }

    fn view(&mut self, doc: Vec<String>, start: Span) -> PResult<ViewDecl> {
        self.expect_kw("view")?;
        let node = self.node();
        let name = self.ident("a view name")?;
        let mut type_params = Vec::new();
        if self.eat(&Tok::Lt) {
            loop {
                type_params.push(self.ident("a type parameter, such as `T`")?);
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(&Tok::Gt)?;
        }
        let params = self.params()?;
        let body_start = self.span();
        let body = self.ui_block()?;
        let end = Span::new(self.prev_end(), self.prev_end());
        Ok(ViewDecl {
            node,
            doc,
            name,
            type_params,
            params,
            body,
            body_span: body_start.to(end),
            span: start.to(end),
        })
    }

    // ---- statements -----------------------------------------------------------------------

    /// An action's body: the `require` lines heading it, then its statements.
    fn action_body(&mut self) -> PResult<(Vec<crate::ast::Require>, Block)> {
        let start = self.expect(&Tok::LBrace)?;
        let mut requires = Vec::new();
        loop {
            self.skip_nl();
            // `require` is a word of its own only where it heads a line of an action's body
            // and a condition follows; elsewhere it is an ordinary name.
            let heads = matches!(self.peek(), Tok::Ident(word) if word == "require")
                && !matches!(
                    self.peek_at(1),
                    Tok::Assign | Tok::Dot | Tok::LParen | Tok::Newline | Tok::RBrace
                );
            if !heads {
                break;
            }
            let at = self.span();
            self.bump();
            let cond = self.expr()?;
            let reason = self.expr()?;
            requires.push(crate::ast::Require {
                cond,
                span: at.to(reason.span),
                reason,
            });
            if !matches!(self.peek(), Tok::Newline | Tok::RBrace) {
                return self.unexpected("a new line after the requirement's reason");
            }
        }
        Ok((requires, self.block_after(start)?))
    }

    fn block(&mut self) -> PResult<Block> {
        let start = self.expect(&Tok::LBrace)?;
        self.block_after(start)
    }

    /// The rest of a block whose `{` is at `start`.
    fn block_after(&mut self, start: Span) -> PResult<Block> {
        let mut stmts = Vec::new();
        loop {
            self.skip_nl();
            if self.is(&Tok::RBrace) {
                break;
            }
            if matches!(self.peek(), Tok::Eof) {
                return self.unexpected("`}` to close the block");
            }
            match self.stmt() {
                Ok(s) => {
                    stmts.push(s);
                    if !matches!(self.peek(), Tok::Newline | Tok::RBrace) {
                        let _: PResult<()> =
                            self.unexpected("a new line or `}` after the statement");
                        self.recover_stmt();
                    }
                }
                Err(Abort) => self.recover_stmt(),
            }
        }
        let end = self.expect(&Tok::RBrace)?;
        Ok(Block {
            stmts,
            span: start.to(end),
        })
    }

    fn stmt(&mut self) -> PResult<Stmt> {
        let start = self.span();
        let kind = match self.peek().clone() {
            Tok::Kw("let") => {
                self.bump();
                let name = self.ident("a variable name")?;
                let ty = if self.eat(&Tok::Colon) {
                    Some(self.type_expr()?)
                } else {
                    None
                };
                self.expect(&Tok::Assign)?;
                let value = self.expr()?;
                StmtKind::Let { name, ty, value }
            }
            Tok::Kw("for") => {
                self.bump();
                let var = self.ident("a loop variable")?;
                self.expect_kw("in")?;
                let iter = self.expr()?;
                let body = self.block()?;
                StmtKind::For { var, iter, body }
            }
            Tok::Kw("as") => {
                self.bump();
                let who = self.expr()?;
                let at = if matches!(self.peek(), Tok::Ident(w) if w == "at") {
                    self.bump();
                    Some(self.expr()?)
                } else {
                    None
                };
                let body = self.block()?;
                StmtKind::As { who, at, body }
            }
            Tok::Kw("return") => {
                self.bump();
                if matches!(self.peek(), Tok::Newline | Tok::RBrace | Tok::Eof) {
                    StmtKind::Return(None)
                } else {
                    StmtKind::Return(Some(self.expr()?))
                }
            }
            _ => {
                let e = self.expr()?;
                let op = match self.peek() {
                    Tok::Assign => Some(AssignOp::Set),
                    Tok::PlusAssign => Some(AssignOp::Add),
                    Tok::MinusAssign => Some(AssignOp::Sub),
                    _ => None,
                };
                match op {
                    Some(op) => {
                        self.bump();
                        let place = self.place_of(&e)?;
                        let value = self.expr()?;
                        StmtKind::Assign { place, op, value }
                    }
                    None => StmtKind::Expr(e),
                }
            }
        };
        Ok(Stmt {
            kind,
            span: start.to(Span::new(self.prev_end(), self.prev_end())),
        })
    }

    fn place_of(&mut self, e: &Expr) -> PResult<Place> {
        fn walk(e: &Expr, out: &mut Vec<Ident>) -> bool {
            match &e.kind {
                ExprKind::Ident(name) => {
                    out.push(Ident {
                        name: name.clone(),
                        span: e.span,
                    });
                    true
                }
                ExprKind::Field { base, name } => {
                    let ok = walk(base, out);
                    out.push(name.clone());
                    ok
                }
                _ => false,
            }
        }
        let mut segs = Vec::new();
        if walk(e, &mut segs) {
            Ok(Place { segs, span: e.span })
        } else {
            self.fail(
                e.span,
                "the left side of an assignment must be a variable or a field, such as `c.value`",
            )
        }
    }

    // ---- expressions ----------------------------------------------------------------------

    fn expr(&mut self) -> PResult<Expr> {
        if matches!(self.peek(), Tok::Ident(_)) && matches!(self.peek_at(1), Tok::Arrow) {
            let param = self.ident("a lambda parameter")?;
            self.bump();
            self.skip_nl();
            let body = self.expr()?;
            let span = param.span.to(body.span);
            return Ok(Expr {
                kind: ExprKind::Lambda {
                    param,
                    body: Box::new(body),
                },
                span,
            });
        }
        if self.is(&Tok::DotDot) {
            let start = self.bump().span;
            let hi = self.or_expr()?;
            let span = start.to(hi.span);
            return Ok(Expr {
                kind: ExprKind::Range {
                    lo: None,
                    hi: Some(Box::new(hi)),
                },
                span,
            });
        }
        let lo = self.or_expr()?;
        if !self.is(&Tok::DotDot) {
            return Ok(lo);
        }
        let dots = self.bump().span;
        let hi = if matches!(
            self.peek(),
            Tok::RParen | Tok::RBracket | Tok::RBrace | Tok::Comma | Tok::Newline | Tok::Eof
        ) {
            None
        } else {
            Some(Box::new(self.or_expr()?))
        };
        let span = lo.span.to(hi.as_ref().map_or(dots, |hi| hi.span));
        Ok(Expr {
            kind: ExprKind::Range {
                lo: Some(Box::new(lo)),
                hi,
            },
            span,
        })
    }

    fn or_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.and_expr()?;
        loop {
            let op = if self.eat_kw("or") {
                BinOp::Or
            } else if self.eat(&Tok::QQ) {
                BinOp::Coalesce
            } else {
                return Ok(lhs);
            };
            self.skip_nl();
            let rhs = self.and_expr()?;
            lhs = bin(op, lhs, rhs);
        }
    }

    fn and_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.not_expr()?;
        while self.eat_kw("and") {
            self.skip_nl();
            let rhs = self.not_expr()?;
            lhs = bin(BinOp::And, lhs, rhs);
        }
        Ok(lhs)
    }

    fn not_expr(&mut self) -> PResult<Expr> {
        if self.is_kw("not") {
            let start = self.bump().span;
            let operand = self.not_expr()?;
            let span = start.to(operand.span);
            return Ok(Expr {
                kind: ExprKind::Unary {
                    op: UnOp::Not,
                    operand: Box::new(operand),
                },
                span,
            });
        }
        self.cmp_expr()
    }

    fn cmp_expr(&mut self) -> PResult<Expr> {
        let lhs = self.add_expr()?;
        let op = match self.peek() {
            Tok::EqEq => BinOp::Eq,
            Tok::NotEq => BinOp::Ne,
            Tok::Lt => BinOp::Lt,
            Tok::Le => BinOp::Le,
            Tok::Gt => BinOp::Gt,
            Tok::Ge => BinOp::Ge,
            _ => return Ok(lhs),
        };
        self.bump();
        self.skip_nl();
        let rhs = self.add_expr()?;
        Ok(bin(op, lhs, rhs))
    }

    fn add_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.mul_expr()?;
        loop {
            let op = match self.peek() {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                _ => return Ok(lhs),
            };
            self.bump();
            self.skip_nl();
            let rhs = self.mul_expr()?;
            lhs = bin(op, lhs, rhs);
        }
    }

    fn mul_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.unary()?;
        loop {
            let op = match self.peek() {
                Tok::Star => BinOp::Mul,
                Tok::Slash => BinOp::Div,
                Tok::Percent => BinOp::Rem,
                _ => return Ok(lhs),
            };
            self.bump();
            self.skip_nl();
            let rhs = self.unary()?;
            lhs = bin(op, lhs, rhs);
        }
    }

    fn unary(&mut self) -> PResult<Expr> {
        if self.is(&Tok::Minus) {
            let start = self.bump().span;
            let operand = self.unary()?;
            let span = start.to(operand.span);
            return Ok(Expr {
                kind: ExprKind::Unary {
                    op: UnOp::Neg,
                    operand: Box::new(operand),
                },
                span,
            });
        }
        if self.is_kw("try") {
            let start = self.bump().span;
            let operand = self.unary()?;
            let span = start.to(operand.span);
            return Ok(Expr {
                kind: ExprKind::Try(Box::new(operand)),
                span,
            });
        }
        self.postfix()
    }

    fn postfix(&mut self) -> PResult<Expr> {
        let mut e = self.primary()?;
        loop {
            if self.is(&Tok::Dot) {
                self.bump();
                let name = self.ident("a field or method name after `.`")?;
                let span = e.span.to(name.span);
                e = Expr {
                    kind: ExprKind::Field {
                        base: Box::new(e),
                        name,
                    },
                    span,
                };
            } else if self.is(&Tok::QDot) {
                self.bump();
                let name = self.ident("a field or method name after `?.`")?;
                let mut end = name.span;
                let args = if self.is(&Tok::LParen) {
                    self.bump();
                    let args = self.args()?;
                    end = self.expect(&Tok::RParen)?;
                    Some(args)
                } else {
                    None
                };
                let span = e.span.to(end);
                e = Expr {
                    kind: ExprKind::OptChain {
                        base: Box::new(e),
                        name,
                        args,
                    },
                    span,
                };
            } else if self.is(&Tok::LParen) {
                self.bump();
                let args = self.args()?;
                let close = self.expect(&Tok::RParen)?;
                let span = e.span.to(close);
                e = Expr {
                    kind: ExprKind::Call {
                        callee: Box::new(e),
                        args,
                    },
                    span,
                };
            } else {
                return Ok(e);
            }
        }
    }

    fn args(&mut self) -> PResult<Vec<Arg>> {
        let mut out = Vec::new();
        while !self.is(&Tok::RParen) {
            let name = match (self.peek().clone(), self.peek_at(1)) {
                (Tok::Ident(_), Tok::Colon) => {
                    let n = self.ident("an argument name")?;
                    self.bump();
                    Some(n)
                }
                // A keyword names an argument too: `open(r, view: v)`.
                (Tok::Kw(k), Tok::Colon) => {
                    let span = self.bump().span;
                    self.bump();
                    Some(Ident {
                        name: k.to_string(),
                        span,
                    })
                }
                _ => None,
            };
            let value = self.expr()?;
            out.push(Arg { name, value });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        Ok(out)
    }

    fn primary(&mut self) -> PResult<Expr> {
        let start = self.span();
        let kind = match self.peek().clone() {
            Tok::Int(v) => {
                self.bump();
                ExprKind::Int(v)
            }
            Tok::Float(v) => {
                self.bump();
                ExprKind::Float(v)
            }
            Tok::Length(v, u) => {
                self.bump();
                let unit = match u {
                    "rem" => crate::types::LengthUnit::Rem,
                    "dp" => crate::types::LengthUnit::Dp,
                    _ => crate::types::LengthUnit::Fr,
                };
                ExprKind::Length(v, unit)
            }
            Tok::Duration(v, u) => {
                self.bump();
                ExprKind::Duration(v, u)
            }
            Tok::Calendar(text) => {
                self.bump();
                ExprKind::Calendar(text)
            }
            Tok::Kw("true") => {
                self.bump();
                ExprKind::Bool(true)
            }
            Tok::Kw("false") => {
                self.bump();
                ExprKind::Bool(false)
            }
            Tok::Ident(name) if name == "root" && matches!(self.peek_at(1), Tok::Ident(_)) => {
                self.bump();
                let ty = self.type_expr()?;
                let span = start.to(ty.span);
                return Ok(Expr {
                    kind: ExprKind::Root(ty),
                    span,
                });
            }
            Tok::Ident(name) if name == "do" && matches!(self.peek_at(1), Tok::Ident(_)) => {
                let span = self.span();
                return self.fail(
                    span,
                    "flows invoke an action by calling it: write `act(args)`, without `do`",
                );
            }
            Tok::Dot if matches!(self.peek_at(1), Tok::Ident(_)) => {
                self.bump();
                let name = self.ident("a variant name")?;
                let args = if self.is(&Tok::LParen) {
                    self.bump();
                    let args = self.args()?;
                    self.expect(&Tok::RParen)?;
                    Some(args)
                } else {
                    None
                };
                ExprKind::Variant { name, args }
            }
            Tok::LBrace => {
                let block = self.block()?;
                let span = block.span;
                return Ok(Expr {
                    kind: ExprKind::Block(block),
                    span,
                });
            }
            Tok::Kw("match") => return self.match_expr(),
            Tok::Ref(r) => {
                self.bump();
                ExprKind::Ref(r)
            }
            Tok::Ident(name) => {
                self.bump();
                ExprKind::Ident(name)
            }
            Tok::Str(pieces) => {
                self.bump();
                ExprKind::Str(self.str_parts(pieces)?)
            }
            Tok::LParen => {
                self.bump();
                let first = self.expr()?;
                if self.is(&Tok::Comma) {
                    let mut items = vec![first];
                    while self.eat(&Tok::Comma) {
                        if self.is(&Tok::RParen) {
                            break;
                        }
                        items.push(self.expr()?);
                    }
                    let close = self.expect(&Tok::RParen)?;
                    return Ok(Expr {
                        kind: ExprKind::Tuple(items),
                        span: start.to(close),
                    });
                }
                self.expect(&Tok::RParen)?;
                return Ok(first);
            }
            Tok::LBracket => {
                self.bump();
                let mut items = Vec::new();
                while !self.is(&Tok::RBracket) {
                    items.push(self.expr()?);
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                let close = self.expect(&Tok::RBracket)?;
                return Ok(Expr {
                    kind: ExprKind::List(items),
                    span: start.to(close),
                });
            }
            Tok::Kw("if") => return self.if_expr(),
            _ => return self.unexpected("an expression"),
        };
        Ok(Expr {
            kind,
            span: start.to(Span::new(self.prev_end(), self.prev_end())),
        })
    }

    /// `if cond { ... } [else (if ... | { ... })]`.
    fn if_expr(&mut self) -> PResult<Expr> {
        let start = self.expect_kw("if")?;
        if self.is_kw("let") {
            let binds = self.let_binds()?;
            let then = self.block()?;
            let els = self.else_expr()?;
            return Ok(Expr {
                kind: ExprKind::IfLet { binds, then, els },
                span: start.to(Span::new(self.prev_end(), self.prev_end())),
            });
        }
        let cond = self.expr()?;
        if matches!(self.peek(), Tok::Ident(n) if n == "then") {
            let span = self.span();
            return self.fail(span, "`if` takes blocks: write `if cond { a } else { b }`");
        }
        let then = self.block()?;
        let els = if self.eat_kw("else") {
            if self.is_kw("if") {
                Some(Box::new(self.if_expr()?))
            } else {
                let b = self.block()?;
                let span = b.span;
                Some(Box::new(Expr {
                    kind: ExprKind::Block(b),
                    span,
                }))
            }
        } else {
            None
        };
        Ok(Expr {
            kind: ExprKind::If {
                cond: Box::new(cond),
                then,
                els,
            },
            span: start.to(Span::new(self.prev_end(), self.prev_end())),
        })
    }

    /// `let a = x, b = y` after `if`.
    fn let_binds(&mut self) -> PResult<Vec<LetBind>> {
        self.expect_kw("let")?;
        let mut binds = Vec::new();
        loop {
            let name = self.ident("a name for the Option's value")?;
            if !self.is(&Tok::Assign) {
                let span = self.span();
                return self.fail(
                    span,
                    "`if let` binds a name to an Option's value: `if let m = c.messages.last() { ... }`",
                );
            }
            self.bump();
            let value = self.expr()?;
            binds.push(LetBind { name, value });
            if !self.eat(&Tok::Comma) {
                return Ok(binds);
            }
            // `if let a = x, let b = y` reads as well; the second `let` is optional.
            self.eat_kw("let");
        }
    }

    /// An `else` branch: another `if`, or a block.
    fn else_expr(&mut self) -> PResult<Option<Box<Expr>>> {
        if !self.eat_kw("else") {
            return Ok(None);
        }
        if self.is_kw("if") {
            return Ok(Some(Box::new(self.if_expr()?)));
        }
        let b = self.block()?;
        let span = b.span;
        Ok(Some(Box::new(Expr {
            kind: ExprKind::Block(b),
            span,
        })))
    }

    /// `.name` or `.name(a, b)` before the `=>` of a match arm.
    fn pattern(&mut self) -> PResult<Pattern> {
        let start = self.span();
        // `t: TextChannel`: a case of a union, by its type.
        if matches!(self.peek(), Tok::Ident(_)) && matches!(self.peek_at(1), Tok::Colon) {
            let variant = self.ident("a name for the value")?;
            self.bump();
            let ty = self.type_expr()?;
            return Ok(Pattern {
                variant,
                binds: Vec::new(),
                ty: Some(ty),
                span: start.to(Span::new(self.prev_end(), self.prev_end())),
            });
        }
        if !self.eat(&Tok::Dot) {
            return self.unexpected("a pattern such as `.some(x)`, `.none` or `t: TextChannel`");
        }
        let variant = self.ident("a variant name")?;
        let mut binds = Vec::new();
        if self.eat(&Tok::LParen) {
            while !self.is(&Tok::RParen) {
                binds.push(self.ident("a name for the payload field")?);
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(&Tok::RParen)?;
        }
        Ok(Pattern {
            variant,
            binds,
            ty: None,
            span: start.to(Span::new(self.prev_end(), self.prev_end())),
        })
    }

    /// After a match arm: a `,`, a new line, or the closing `}`.
    fn arm_end(&mut self) -> PResult<()> {
        if self.eat(&Tok::Comma) || matches!(self.peek(), Tok::Newline | Tok::RBrace) {
            return Ok(());
        }
        self.unexpected("`,`, a new line or `}` after the match arm")
    }

    fn match_expr(&mut self) -> PResult<Expr> {
        let start = self.expect_kw("match")?;
        let scrutinee = self.expr()?;
        self.expect(&Tok::LBrace)?;
        let mut arms = Vec::new();
        loop {
            self.skip_nl();
            if self.is(&Tok::RBrace) {
                break;
            }
            if matches!(self.peek(), Tok::Eof) {
                return self.unexpected("`}` to close the match");
            }
            let pattern = self.pattern()?;
            self.expect(&Tok::FatArrow)?;
            self.skip_nl();
            let body = self.expr()?;
            arms.push(MatchArm { pattern, body });
            self.arm_end()?;
        }
        let end = self.expect(&Tok::RBrace)?;
        Ok(Expr {
            kind: ExprKind::Match {
                scrutinee: Box::new(scrutinee),
                arms,
            },
            span: start.to(end),
        })
    }

    fn str_parts(&mut self, pieces: Vec<StrPiece>) -> PResult<Vec<StrPart>> {
        let mut out = Vec::new();
        for piece in pieces {
            match piece {
                StrPiece::Lit(s) => out.push(StrPart::Lit(s)),
                StrPiece::Interp { start, end } => {
                    let local = start.checked_sub(self.base).zip(end.checked_sub(self.base));
                    let Some(text) = local.and_then(|(from, to)| self.src.get(from..to)) else {
                        return self.fail(Span::new(start, end), "invalid interpolation");
                    };
                    let (toks, mut diags) = lex_at(text, start);
                    self.diags.append(&mut diags);
                    let mut sub = Parser::new_at(toks, self.src, self.base);
                    sub.skip_nl();
                    let expr = sub.expr();
                    sub.skip_nl();
                    let complete = matches!(sub.peek(), Tok::Eof);
                    self.diags.append(&mut sub.diags);
                    let Ok(expr) = expr else { return Err(Abort) };
                    if !complete {
                        return self.fail(
                            Span::new(start, end),
                            "an interpolation `{ }` holds a single expression",
                        );
                    }
                    out.push(StrPart::Interp(Box::new(expr)));
                }
            }
        }
        Ok(out)
    }

    // ---- views ----------------------------------------------------------------------------

    fn ui_block(&mut self) -> PResult<Vec<UiStmt>> {
        self.expect(&Tok::LBrace)?;
        self.ui_block_rest()
    }

    /// The statements of a view block after its `{` (and a list's `x ->`), and the `}`.
    fn ui_block_rest(&mut self) -> PResult<Vec<UiStmt>> {
        let mut out = Vec::new();
        loop {
            self.skip_nl();
            if self.is(&Tok::RBrace) {
                break;
            }
            if matches!(self.peek(), Tok::Eof) {
                return self.unexpected("`}` to close the block");
            }
            match self.ui_stmt() {
                Ok(s) => {
                    out.push(s);
                    if !matches!(self.peek(), Tok::Newline | Tok::RBrace) {
                        let _: PResult<()> =
                            self.unexpected("a new line or `}` after the UI statement");
                        self.recover_stmt();
                    }
                }
                Err(Abort) => self.recover_stmt(),
            }
        }
        self.expect(&Tok::RBrace)?;
        Ok(out)
    }

    fn ui_stmt(&mut self) -> PResult<UiStmt> {
        let start = self.span();
        let kind = match self.peek().clone() {
            Tok::Kw("for") => {
                self.bump();
                let var = self.ident("a loop variable")?;
                self.expect_kw("in")?;
                let iter = self.expr()?;
                let body = self.ui_block()?;
                UiKind::For { var, iter, body }
            }
            Tok::Kw("if") => return self.ui_if(),
            Tok::Kw("match") => {
                self.bump();
                let scrutinee = self.expr()?;
                self.expect(&Tok::LBrace)?;
                let mut arms = Vec::new();
                loop {
                    self.skip_nl();
                    if self.is(&Tok::RBrace) {
                        break;
                    }
                    if matches!(self.peek(), Tok::Eof) {
                        return self.unexpected("`}` to close the match");
                    }
                    let pattern = self.pattern()?;
                    self.expect(&Tok::FatArrow)?;
                    self.skip_nl();
                    let (body, braced) = if self.is(&Tok::LBrace) {
                        (self.ui_block()?, true)
                    } else {
                        (vec![self.ui_stmt()?], false)
                    };
                    arms.push(UiArm {
                        pattern,
                        body,
                        braced,
                    });
                    self.arm_end()?;
                }
                self.expect(&Tok::RBrace)?;
                UiKind::Match { scrutinee, arms }
            }
            Tok::Ident(word)
                if (word == "window" || word == "lens")
                    && matches!(self.peek_at(1), Tok::Ident(_))
                    && matches!(self.peek_at(2), Tok::Colon) =>
            {
                self.bump();
                let layer = if word == "window" {
                    StateLayer::Window
                } else {
                    StateLayer::Lens
                };
                let node = self.node();
                let name = self.ident("a state name")?;
                self.expect(&Tok::Colon)?;
                let ty = self.type_expr()?;
                let default = if self.eat(&Tok::Assign) {
                    Some(self.expr()?)
                } else {
                    None
                };
                UiKind::State(StateDecl {
                    node,
                    layer,
                    name,
                    ty,
                    default,
                    span: start.to(Span::new(self.prev_end(), self.prev_end())),
                })
            }
            Tok::Kw("let") => {
                self.bump();
                let name = self.ident("a variable name")?;
                self.expect(&Tok::Assign)?;
                let value = self.expr()?;
                UiKind::Let { name, value }
            }
            Tok::Ident(word)
                if word == "on"
                    && matches!(self.peek_at(1), Tok::Ident(_))
                    && matches!(self.peek_at(2), Tok::Colon | Tok::LParen) =>
            {
                self.bump();
                let event = self.ident("an event name")?;
                // `on select(x: Note): ...`: the value the event carries, named; and where it
                // lands, when the event says (`on drop(moved: Task, at: Int): ...`).
                let (mut param, mut at) = (None, None);
                if self.eat(&Tok::LParen) {
                    param = Some(self.event_param("the name of the value the event carries")?);
                    if self.eat(&Tok::Comma) {
                        at = Some(self.event_param("the name of where the value lands")?);
                    }
                    self.expect(&Tok::RParen)?;
                }
                self.expect(&Tok::Colon)?;
                let action = self.expr()?;
                UiKind::On {
                    event,
                    param,
                    at,
                    action,
                }
            }
            Tok::Ident(_)
                if matches!(self.peek_at(1), Tok::Colon)
                    && matches!(self.peek_at(2), Tok::LBrace) =>
            {
                let node = self.node();
                let name = self.ident("a slot name")?;
                self.expect(&Tok::Colon)?;
                let body = self.ui_block()?;
                UiKind::Slot { node, name, body }
            }
            Tok::Str(_) if matches!(self.peek_at(1), Tok::Colon) => {
                let node = self.node();
                let label = self.primary()?;
                self.expect(&Tok::Colon)?;
                let value = self.expr()?;
                UiKind::Fact { node, label, value }
            }
            _ => UiKind::Call(self.ui_call()?),
        };
        Ok(UiStmt {
            kind,
            span: start.to(Span::new(self.prev_end(), self.prev_end())),
        })
    }

    /// `if cond { ui } [else (if ... | { ui })]` in a view body.
    fn ui_if(&mut self) -> PResult<UiStmt> {
        let start = self.expect_kw("if")?;
        if self.is_kw("let") {
            let binds = self.let_binds()?;
            let then = self.ui_block()?;
            let els = self.ui_else()?;
            return Ok(UiStmt {
                kind: UiKind::IfLet { binds, then, els },
                span: start.to(Span::new(self.prev_end(), self.prev_end())),
            });
        }
        let cond = self.expr()?;
        let then = self.ui_block()?;
        let els = if self.eat_kw("else") {
            if self.is_kw("if") {
                Some(vec![self.ui_if()?])
            } else {
                Some(self.ui_block()?)
            }
        } else {
            None
        };
        Ok(UiStmt {
            kind: UiKind::If { cond, then, els },
            span: start.to(Span::new(self.prev_end(), self.prev_end())),
        })
    }

    fn ui_else(&mut self) -> PResult<Option<Vec<UiStmt>>> {
        if !self.eat_kw("else") {
            return Ok(None);
        }
        if self.is_kw("if") {
            return Ok(Some(vec![self.ui_if()?]));
        }
        Ok(Some(self.ui_block()?))
    }

    fn ui_call(&mut self) -> PResult<UiCall> {
        let node = self.node();
        let call = self.postfix()?;
        let mut block = None;
        let mut item = None;
        if self.is(&Tok::LBrace) {
            if matches!(self.peek_at(1), Tok::Ident(_)) && matches!(self.peek_at(2), Tok::Arrow) {
                // `list(items) { x -> ... }`, or a view that takes a view of an item:
                // `std.list(nb, nb.notes) { n -> row(n.title) }`.
                self.bump();
                item = Some(self.ident("the item's name")?);
                self.expect(&Tok::Arrow)?;
                block = Some(self.ui_block_rest()?);
            } else {
                block = Some(self.ui_block()?);
            }
        }
        if self.is(&Tok::Hash) {
            let span = self.span();
            return self.fail(span, "an anchor is written after the call as `as name`, for example `button(\"+1\", on: increment(c)) as plus`");
        }
        let anchor = if self.eat_kw("as") {
            Some(self.ident("an anchor name after `as`")?)
        } else {
            None
        };
        Ok(UiCall {
            node,
            call,
            item,
            block,
            anchor,
        })
    }
}
