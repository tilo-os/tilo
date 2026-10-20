//! The parser: tokens to the syntax tree of implementation.md section 2.
//!
//! Syntax errors are recovered at statement and item boundaries, so one run reports several.

use crate::ast::{
    ActionDecl, ActionOpt, ActionRole, AppHeader, Arg, AssignOp, BinOp, Block, DefaultViewDecl,
    EnumDecl, Expr, ExprKind, FieldDecl, FlowDecl, FnDecl, Ident, Item, MatchArm, MergeSpec,
    Module, NodeId, Param, Pattern, Place, Proposal, RootDecl, ScenarioDecl, StateDecl, StateLayer,
    Stmt, StmtKind, StrPart, TypeDecl, TypeExpr, TypeExprKind, UiArm, UiCall, UiKind, UiStmt, UnOp,
    UseDecl, VariantDecl, ViewDecl,
};
use crate::diag::{Diagnostic, Span};
use crate::lexer::{StrPiece, Tok, Token, lex, lex_at};
use crate::types::MergeKind;

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

const ITEM_KWS: &[&str] = &["use", "type", "enum", "action", "flow", "view", "fn"];
const TYPE_ARGS: &[&str] = &["List", "Set", "Map", "Collection", "Option", "Result"];
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
            _ => return None,
        };
        let next_is_role =
            matches!(self.peek_at(1), Tok::Ident(n) if n == "primary" || n == "destructive");
        (matches!(self.peek_at(1), Tok::Kw("action" | "flow")) || next_is_role).then_some(role)
    }

    /// `(icon: .name)` after the app's title.
    fn header_opts(&mut self) -> PResult<Option<Ident>> {
        if !self.eat(&Tok::LParen) {
            return Ok(None);
        }
        let mut icon = None;
        while !self.is(&Tok::RParen) {
            let name = self.ident("`icon`")?;
            if name.name != "icon" {
                return self.fail(
                    name.span,
                    format!(
                        "the app header takes one option, `icon`: `(icon: .notebook)`; `{}` is not one",
                        name.name
                    ),
                );
            }
            self.expect(&Tok::Colon)?;
            icon = Some(self.icon_lit()?);
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RParen)?;
        Ok(icon)
    }

    /// `.name`: an icon of the platform's set.
    fn icon_lit(&mut self) -> PResult<Ident> {
        if !self.eat(&Tok::Dot) {
            return self.unexpected("an icon written with a dot, such as `.plus`");
        }
        let mut name = self.ident("an icon name")?;
        // `.settings_2` lexes as the name `settings_2`; nothing else to join.
        name.span = Span::new(name.span.start.saturating_sub(1), name.span.end);
        Ok(name)
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
            let icon = self.header_opts().ok()?;
            AppHeader {
                id,
                title,
                icon,
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
                    let icon = self.header_opts()?;
                    Ok((id, title, icon))
                })
                .ok();
            match title {
                Some((id, title, icon)) => {
                    header = Some(AppHeader {
                        id,
                        title,
                        icon,
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
            let ty = self.type_expr()?;
            return Ok(Item::Root(RootDecl {
                node,
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
            let body = self.block()?;
            return Ok(Item::Scenario(ScenarioDecl {
                node,
                doc,
                name,
                name_span,
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
                    "an action is `primary` or `destructive`, not both: the primary action is the one main thing to do, and a destructive one never is. Keep one of the words",
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
            let fields = if self.is(&Tok::LParen) {
                self.params()?
            } else {
                Vec::new()
            };
            variants.push(VariantDecl {
                node: vnode,
                doc: vdoc,
                name: vname,
                fields,
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

    fn type_decl(&mut self, doc: Vec<String>, start: Span) -> PResult<TypeDecl> {
        self.expect_kw("type")?;
        let node = self.node();
        let name = self.ident("a type name")?;
        self.expect(&Tok::LBrace)?;
        let mut fields = Vec::new();
        loop {
            self.skip_nl();
            if self.eat(&Tok::RBrace) {
                break;
            }
            if matches!(self.peek(), Tok::Eof) {
                return self.unexpected("`}` to close the type");
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
            fields,
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

    fn type_expr(&mut self) -> PResult<TypeExpr> {
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
            out.push(Param { node, name, ty });
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
        let body = self.block()?;
        Ok(ActionDecl {
            node,
            doc,
            role,
            name,
            params,
            result,
            proposal,
            opts,
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
        let params = self.params()?;
        let body_start = self.span();
        let body = self.ui_block()?;
        let end = Span::new(self.prev_end(), self.prev_end());
        Ok(ViewDecl {
            node,
            doc,
            name,
            params,
            body,
            body_span: body_start.to(end),
            span: start.to(end),
        })
    }

    // ---- statements -----------------------------------------------------------------------

    fn block(&mut self) -> PResult<Block> {
        let start = self.expect(&Tok::LBrace)?;
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
        self.or_expr()
    }

    fn or_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.and_expr()?;
        while self.eat_kw("or") {
            self.skip_nl();
            let rhs = self.and_expr()?;
            lhs = bin(BinOp::Or, lhs, rhs);
        }
        Ok(lhs)
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

    /// `.name` or `.name(a, b)` before the `=>` of a match arm.
    fn pattern(&mut self) -> PResult<Pattern> {
        let start = self.span();
        if !self.eat(&Tok::Dot) {
            return self.unexpected("a pattern such as `.some(x)` or `.none`");
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

    fn ui_call(&mut self) -> PResult<UiCall> {
        let node = self.node();
        let call = self.postfix()?;
        let mut block = None;
        let mut item = None;
        if self.is(&Tok::LBrace) {
            if matches!(self.peek_at(1), Tok::Ident(_)) && matches!(self.peek_at(2), Tok::Arrow) {
                let is_list = match &call.kind {
                    ExprKind::Call { callee, .. } => {
                        matches!(&callee.kind, ExprKind::Ident(n) if n == "list")
                    }
                    _ => false,
                };
                if !is_list {
                    return self.fail(call.span, "`name { x -> ... }` is only available as `list(items) { x -> ... }`; write `for x in items { ... }` instead");
                }
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
