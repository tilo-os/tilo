//! The syntax tree. Every declaration, field, parameter and UI call carries a [`NodeId`], and
//! every node carries its span.

use crate::diag::Span;
use crate::types::MergeKind;

pub type NodeId = u32;

#[derive(Clone, Debug, PartialEq)]
pub struct Ident {
    pub name: String,
    pub span: Span,
    /// The package a `template view` came from, when import qualified this name with it
    /// (`shell.desk_style`): the resolver admits such a plain name, since the template's body is
    /// the one place that may call the package's unoffered helpers.
    pub from_template_of: Option<String>,
}

impl Ident {
    #[must_use]
    pub fn plain(name: impl Into<String>, span: Span) -> Self {
        Self {
            name: name.into(),
            span,
            from_template_of: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Module {
    pub app: AppHeader,
    pub items: Vec<Item>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AppHeader {
    pub id: String,
    pub title: String,
    pub icon: Option<String>,
    pub category: Option<String>,
    /// `(previously: so.tilo.old)`: the ids the app was published under before; its records,
    /// grants and windows under them are the app's, when the same publisher signed both.
    pub previously: Vec<String>,
    /// `(records: .own)`: each member keeps the app's records in their own record there, which only
    /// that person's devices read; the default `.shared` keeps them where every member reads.
    pub own_records: bool,
    /// `(web: ["example.com", "*.rss.org"])`: the hosts `uses web` requests may reach; any other is
    /// refused. `*.rss.org` covers subdomains, not `rss.org`.
    pub web: Vec<String>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Use(UseDecl),
    Type(TypeDecl),
    /// `type Name = T`: a name for a type. A container (`Collection<Note>`) named so is a resource
    /// holding it.
    Alias(AliasDecl),
    Enum(EnumDecl),
    /// `root T`: one resource per workspace, which the launcher opens.
    Root(RootDecl),
    DefaultView(DefaultViewDecl),
    RowView(DefaultViewDecl),
    Views(ViewsDecl),
    Scenario(ScenarioDecl),
    Trigger(TriggerDecl),
    Action(ActionDecl),
    View(ViewDecl),
    Fn(FnDecl),
}

impl Item {
    pub fn doc(&self) -> &[String] {
        match self {
            Self::Type(d) => &d.doc,
            Self::Alias(d) => &d.doc,
            Self::Enum(d) => &d.doc,
            Self::Scenario(d) => &d.doc,
            Self::Trigger(d) => &d.doc,
            Self::Action(d) => &d.doc,
            Self::View(d) => &d.doc,
            Self::Fn(d) => &d.doc,
            Self::Use(_) | Self::Root(_) | Self::DefaultView(_) | Self::RowView(_) => &[],
            Self::Views(d) => &d.doc,
        }
    }

    pub fn span(&self) -> Span {
        match self {
            Self::Use(d) => d.span,
            Self::Type(d) => d.span,
            Self::Alias(d) => d.span,
            Self::Enum(d) => d.span,
            Self::Root(d) => d.span,
            Self::DefaultView(d) | Self::RowView(d) => d.span,
            Self::Views(d) => d.span,
            Self::Scenario(d) => d.span,
            Self::Trigger(d) => d.span,
            Self::Action(d) => d.span,
            Self::View(d) => d.span,
            Self::Fn(d) => d.span,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct UseDecl {
    pub node: NodeId,
    pub app: String,
    pub alias: Option<Ident>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypeDecl {
    pub node: NodeId,
    pub doc: Vec<String>,
    pub name: Ident,
    /// `(origin: mail.message)`: the outside service kind whose resources the type holds
    /// (convergence.md, "Outside services").
    pub origin: Option<Ident>,
    /// `(title: subject)`: the field a record is named by ([`crate::resolve::TypeInfo::name_field`]).
    pub title: Option<TitleDecl>,
    /// `(untitled: "Untitled")`: what a record is called while its title is empty.
    pub untitled: Option<UntitledDecl>,
    pub fields: Vec<FieldDecl>,
    pub search: Option<SearchBlock>,
    pub span: Span,
}

/// The field a record is named by: a text field of the type, read wherever one is named (a row, a
/// window's title, a reference, history).
#[derive(Clone, Debug, PartialEq)]
pub struct TitleDecl {
    pub field: Ident,
    pub span: Span,
}

/// What a type is called while its title is empty: what surfaces fall back to, and the placeholder
/// its title field edits with.
#[derive(Clone, Debug, PartialEq)]
pub struct UntitledDecl {
    pub words: String,
    pub span: Span,
}

/// `search { filter done; sort due oldest; hide notes }` in a type (search.md, "What a type
/// declares"): its filters in order, its default order and fields kept out of filters.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchBlock {
    pub entries: Vec<SearchEntry>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchEntry {
    pub kind: SearchEntryKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SearchEntryKind {
    /// `filter unread "Unread" (icon: .mail)`: a filter on a field, its control derived from the
    /// field's type.
    Field {
        field: Ident,
        label: Option<String>,
        opts: Vec<ActionOpt>,
    },
    /// `filter "Open" = not done (icon: .circle)`: a named predicate over the resource's fields,
    /// shown as a toggle.
    Named {
        label: String,
        label_span: Span,
        expr: Expr,
        opts: Vec<ActionOpt>,
    },
    /// `sort received newest`: the default order, and an order offered.
    Sort { field: Ident, order: Ident },
    /// `hide to`: a field kept out of filters.
    Hide { field: Ident },
}

/// `enum Name { a, b(x: T, y: U), c }`: values are written `.a`, `.b(x: 1, y: 2)`.
#[derive(Clone, Debug, PartialEq)]
pub struct EnumDecl {
    pub node: NodeId,
    pub doc: Vec<String>,
    pub name: Ident,
    pub variants: Vec<VariantDecl>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VariantDecl {
    pub node: NodeId,
    pub doc: Vec<String>,
    pub name: Ident,
    pub fields: Vec<Param>,
    pub opts: Vec<ActionOpt>,
    pub span: Span,
}

/// `type Notes = List<Note>`, `type Channel = TextChannel | Thread`.
#[derive(Clone, Debug, PartialEq)]
pub struct AliasDecl {
    pub node: NodeId,
    pub doc: Vec<String>,
    pub name: Ident,
    pub ty: TypeExpr,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RootDecl {
    pub node: NodeId,
    /// `root notes: Notes`: the name code reaches the root by (`notes`, `alias.notes`).
    pub name: Option<Ident>,
    pub ty: TypeExpr,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DefaultViewDecl {
    pub node: NodeId,
    pub view: Ident,
    pub ty: TypeExpr,
    pub span: Span,
}

/// `views project_list, project_board for Project`: what a window of the type switches between. Any
/// other view of the type is the app's own helper.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewsDecl {
    pub node: NodeId,
    pub doc: Vec<String>,
    pub views: Vec<ViewsEntry>,
    pub ty: TypeExpr,
    pub span: Span,
}

/// One view of a `views` declaration, with the label and icon it is offered by: `project_board
/// (icon: .columns)`.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewsEntry {
    pub view: Ident,
    pub opts: Vec<ActionOpt>,
}

/// `scenario "a week of notes" { ... }`: statements that fill a workspace with sample data for
/// screenshots, tests and benchmarks. It never touches a person's data.
#[derive(Clone, Debug, PartialEq)]
pub struct ScenarioDecl {
    pub node: NodeId,
    pub doc: Vec<String>,
    pub name: String,
    pub name_span: Span,
    /// `scenario "editing a note" -> Note { ... }`: what the scenario returns, which the window shows
    /// once staged.
    pub result: Option<TypeExpr>,
    pub body: Block,
    pub span: Span,
}

/// `trigger due(t: Task) at t.at: notify_due(t)`: when one of the package's actions runs by itself,
/// run for each resource of the parameter's type as the app's own invocation.
#[derive(Clone, Debug, PartialEq)]
pub struct TriggerDecl {
    pub node: NodeId,
    pub doc: Vec<String>,
    pub name: Ident,
    /// The record the trigger runs for: the resources of its type are the ones it runs on.
    pub param: Option<Param>,
    pub when: TriggerWhen,
    /// The action it runs: the subject, and any roots it passes.
    pub call: Expr,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TriggerWhen {
    /// `every 30m`: at each multiple of the length, counted from the epoch, so every device agrees.
    Every {
        amount: i64,
        unit: &'static str,
        span: Span,
    },
    /// `at t.at`: once at the instant the resource's field holds, and again when it changes.
    At { expr: Expr, span: Span },
}

#[derive(Clone, Debug, PartialEq)]
pub struct MergeSpec {
    pub kind: MergeKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldDecl {
    pub node: NodeId,
    pub doc: Vec<String>,
    pub name: Ident,
    pub ty: TypeExpr,
    pub merge: Option<MergeSpec>,
    pub default: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypeExpr {
    pub kind: TypeExprKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TypeExprKind {
    Named(Vec<Ident>),
    Generic(Ident, Vec<TypeExpr>),
    Tuple(Vec<TypeExpr>),
    /// `(T) -> View`: what a parameter of a view takes when it is given a lambda or a block with an
    /// item.
    Fn(Vec<TypeExpr>, Box<TypeExpr>),
    /// `TextChannel | Thread`: a resource of any of these types.
    Union(Vec<TypeExpr>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub node: NodeId,
    pub name: Ident,
    pub ty: TypeExpr,
    /// `done: Bool = not item.done`: a default evaluated when the action is invoked without it, over
    /// the parameters before it, and recorded as the argument. Boxed: most parameters have none.
    pub default: Option<Box<Expr>>,
    /// `above: Option<Task> (above)`: the place the view offers the action in supplies it, not an
    /// invocation.
    pub place: Option<Ident>,
    /// `saved: Bool (label: "On the Watch later list")`: what the parameter is called where a person
    /// gives it.
    pub label: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Proposal {
    pub view: Ident,
    pub args: Option<Vec<Ident>>,
}

/// `primary`, `destructive` or `migration` before `action`: how the action is, not where a surface
/// shows it. A surface decides placement from what the action offers a person, never from its role;
/// only a migration is left out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionRole {
    /// The one main action on its subject: the header's Primary button and the empty state's
    /// action.
    Primary,
    /// Drawn as destructive; undone rather than confirmed.
    Destructive,
    /// `migration action`: never offered; the platform runs it once on each resource an earlier
    /// version wrote.
    Migration,
}

impl ActionRole {
    pub const fn word(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Destructive => "destructive",
            Self::Migration => "migration",
        }
    }
}

/// One option of an action's `(icon: .x, label: "y")` list, or of an enum variant's `(icon: .x)`.
#[derive(Clone, Debug, PartialEq)]
pub struct ActionOpt {
    pub name: Ident,
    pub value: Expr,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ActionDecl {
    pub node: NodeId,
    pub doc: Vec<String>,
    pub role: Option<(ActionRole, Span)>,
    pub name: Ident,
    pub params: Vec<Param>,
    pub result: Option<TypeExpr>,
    /// `uses web, time`: the effects the body has, which it records and replays. A body that reads
    /// one it does not declare is refused.
    pub uses: Vec<Ident>,
    pub proposal: Option<Proposal>,
    pub opts: Vec<ActionOpt>,
    /// The `require` lines heading the body: when the action is available on its subject.
    pub requires: Vec<Require>,
    /// The `applies` lines heading the body: when the action can apply where it is offered (`applies
    /// above != .none`). A surface then leaves it out of an item's menu rather than showing a row
    /// that can never work.
    pub applies: Vec<Require>,
    pub body: Block,
    pub span: Span,
}

/// `require not d.sent "{d.subject} went already"`: a condition on the subject the action needs, and
/// why it is unavailable without it. Not offered where it would be; invoked anyway, it is refused
/// with the reason before it changes anything. The condition reads only the subject.
#[derive(Clone, Debug, PartialEq)]
pub struct Require {
    pub cond: Expr,
    /// Text: why the action is unavailable.
    pub reason: Expr,
    pub span: Span,
}

impl ActionDecl {
    /// Whether the presentation follows the subject: a `label:` or `icon:` written as an `if`, an
    /// `on:` state, or a `require` on it. It is then lowered to a hidden view of the subject.
    pub fn presents_state(&self) -> bool {
        !self.requires.is_empty()
            || self.opts.iter().any(|o| match o.name.name.as_str() {
                "on" => true,
                "label" => {
                    matches!(o.value.kind, ExprKind::If { .. })
                        || self.label_reads_subject(&o.value)
                }
                "icon" => matches!(o.value.kind, ExprKind::If { .. }),
                _ => false,
            })
    }

    /// Whether `label` is text interpolating the subject and nothing else: it then follows the
    /// subject wherever the action is offered.
    pub fn label_reads_subject(&self, label: &Expr) -> bool {
        let ExprKind::Str(parts) = &label.kind else {
            return false;
        };
        let Some(subject) = self.params.first() else {
            return false;
        };
        let mut any = false;
        for part in parts {
            if let StrPart::Interp(e) = part {
                let mut root = &**e;
                while let ExprKind::Field { base, .. } | ExprKind::Call { callee: base, .. } =
                    &root.kind
                {
                    root = base;
                }
                match &root.kind {
                    ExprKind::Ident(n) if *n == subject.name.name => any = true,
                    _ => return false,
                }
            }
        }
        any
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ViewDecl {
    pub node: NodeId,
    pub doc: Vec<String>,
    pub name: Ident,
    /// `template view body(content: View)`: a view that expands where it is called, its nodes the
    /// caller's, whatever its parameters are. A view whose parameters are over types, state,
    /// functions or views is one anyway.
    pub template: bool,
    /// `view list<T>(...)`: the types its parameters are written over, each taken from what a call
    /// passes.
    pub type_params: Vec<Ident>,
    pub params: Vec<Param>,
    pub body: Vec<UiStmt>,
    pub body_span: Span,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FnDecl {
    pub node: NodeId,
    pub doc: Vec<String>,
    pub name: Ident,
    pub params: Vec<Param>,
    pub result: TypeExpr,
    pub body: Block,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignOp {
    Set,
    Add,
    Sub,
}

impl AssignOp {
    pub const fn text(self) -> &'static str {
        match self {
            Self::Set => "=",
            Self::Add => "+=",
            Self::Sub => "-=",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub segs: Vec<Ident>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StmtKind {
    Let {
        name: Ident,
        ty: Option<TypeExpr>,
        value: Expr,
    },
    Assign {
        place: Place,
        op: AssignOp,
        value: Expr,
    },
    For {
        var: Ident,
        iter: Expr,
        body: Block,
    },
    Return(Option<Expr>),
    Expr(Expr),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Coalesce,
}

impl BinOp {
    pub const fn text(self) -> &'static str {
        match self {
            Self::Or => "or",
            Self::And => "and",
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Rem => "%",
            Self::Coalesce => "??",
        }
    }

    /// Binding strength, higher binds tighter.
    pub const fn prec(self) -> u8 {
        match self {
            Self::Or | Self::Coalesce => 1,
            Self::And => 2,
            Self::Eq | Self::Ne | Self::Lt | Self::Le | Self::Gt | Self::Ge => 4,
            Self::Add | Self::Sub => 5,
            Self::Mul | Self::Div | Self::Rem => 6,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

// The reference literal is the lexer's: it reads `@app.Type:id` as one token.
pub use tilolang_lex::RefLit;

#[derive(Clone, Debug, PartialEq)]
pub enum StrPart {
    Lit(String),
    Interp(Box<Expr>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Arg {
    pub name: Option<Ident>,
    pub value: Expr,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Length(f64, crate::types::LengthUnit),
    Duration(i64, &'static str),
    /// `2026-06-29`, `2026-06-29T07:02:00Z` or `2026-08`, in canonical ISO text.
    Calendar(String),
    Range {
        lo: Option<Box<Expr>>,
        hi: Option<Box<Expr>>,
    },
    Bool(bool),
    Str(Vec<StrPart>),
    Ref(RefLit),
    Ident(String),
    Tuple(Vec<Expr>),
    List(Vec<Expr>),
    If {
        cond: Box<Expr>,
        then: Block,
        els: Option<Box<Expr>>,
    },
    IfLet {
        binds: Vec<LetBind>,
        then: Block,
        els: Option<Box<Expr>>,
    },
    Block(Block),
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    Variant {
        name: Ident,
        args: Option<Vec<Arg>>,
    },
    Lambda {
        params: Vec<Ident>,
        body: Box<Expr>,
    },
    Unary {
        op: UnOp,
        operand: Box<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Field {
        base: Box<Expr>,
        name: Ident,
    },
    OptChain {
        base: Box<Expr>,
        name: Ident,
        args: Option<Vec<Arg>>,
    },
    Call {
        callee: Box<Expr>,
        types: Vec<TypeExpr>,
        args: Vec<Arg>,
    },
    /// `try act(args)`: invokes in an action; a `Result` of the action's result and the reason it
    /// failed.
    Try(Box<Expr>),
    /// `suggest act(args)`: proposes the action instead of applying it; an agent block's value.
    Suggest(Box<Expr>),
    Root(TypeExpr),
}

#[derive(Clone, Debug, PartialEq)]
pub struct LetBind {
    pub name: Ident,
    pub value: Expr,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pattern {
    pub variant: Ident,
    pub binds: Vec<Ident>,
    pub ty: Option<TypeExpr>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub body: Expr,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UiStmt {
    pub kind: UiKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum UiKind {
    Call(UiCall),
    For {
        var: Ident,
        iter: Expr,
        body: Vec<UiStmt>,
    },
    If {
        cond: Expr,
        then: Vec<UiStmt>,
        /// The `else` branch; an `else if` is a branch holding one `If`.
        els: Option<Vec<UiStmt>>,
    },
    Match {
        scrutinee: Expr,
        arms: Vec<UiArm>,
    },
    Let {
        name: Ident,
        ty: Option<TypeExpr>,
        value: Expr,
    },
    IfLet {
        binds: Vec<LetBind>,
        then: Vec<UiStmt>,
        els: Option<Vec<UiStmt>>,
    },
    State(StateDecl),
    /// `"Due": d.due`: a labelled fact, which E098 rejects: a fact is a chip derived from the field
    /// it shows (`facts(of)`).
    Fact {
        node: NodeId,
        label: Expr,
        value: Expr,
    },
    Slot {
        node: NodeId,
        name: Ident,
        body: Vec<UiStmt>,
    },
    /// `on activate: open(c)`: what the component's event does. An event that carries a value names
    /// it (`on select(x: Note): open(x)`); one that also says where it lands names that second.
    On {
        event: Ident,
        param: Option<EventParam>,
        at: Option<EventParam>,
        /// The handler, boxed as an expression's own operands are: a statement copies 520 bytes
        /// around otherwise.
        action: Box<Expr>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct EventParam {
    pub name: Ident,
    /// Its type when written; else the type the component's description gives.
    pub ty: Option<TypeExpr>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UiArm {
    pub pattern: Pattern,
    pub body: Vec<UiStmt>,
    /// Whether the body was written as a `{ }` block rather than one UI call.
    pub braced: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StateDecl {
    pub node: NodeId,
    pub name: Ident,
    pub ty: TypeExpr,
    pub default: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UiCall {
    pub node: NodeId,
    pub call: Expr,
    /// The names a block opens with: `list(xs) { x -> ... }`, or `(x, under)` for a template's
    /// function of several values.
    pub params: Vec<Ident>,
    pub block: Option<Vec<UiStmt>>,
    pub anchor: Option<Ident>,
}
