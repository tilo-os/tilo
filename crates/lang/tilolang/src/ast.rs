//! The syntax tree. Every declaration, field, parameter and UI call carries a [`NodeId`], and
//! every node carries its span.

use crate::diag::Span;
use crate::types::MergeKind;

/// Identity of a syntax node within one parsed module.
pub type NodeId = u32;

#[derive(Clone, Debug, PartialEq)]
pub struct Ident {
    pub name: String,
    pub span: Span,
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
    /// `(icon: .note)`: the platform icon the app is shown with.
    pub icon: Option<String>,
    /// `(category: .office)`: the launcher category the app is listed under.
    pub category: Option<String>,
    /// `(previously: so.tilo.old)`: the ids the app was published under before. Its records,
    /// grants and windows under them are the app's (the platform checks that the same publisher
    /// signed both).
    pub previously: Vec<String>,
    /// `(records: .own)`: each member of a workspace keeps the app's records in their own
    /// record there, which only that person's devices read; `false` (`.shared`, the default)
    /// keeps them in the workspace's record of the app, which every member reads.
    pub own_records: bool,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Use(UseDecl),
    Type(TypeDecl),
    /// `type Name = T`: a name for a type. A container (`Collection<Note>`) named so is a
    /// resource holding it; any other type is another name for it.
    Alias(AliasDecl),
    Enum(EnumDecl),
    /// `root T`: a type with one resource per workspace, which the launcher opens.
    Root(RootDecl),
    /// `default view v for T`: the view a window of a `T` uses unless changed.
    DefaultView(DefaultViewDecl),
    /// `row view v for T`: the row a list of `T`s shows for each item.
    RowView(DefaultViewDecl),
    /// `scenario "name" { ... }`: named seed data, run as a hidden action.
    Scenario(ScenarioDecl),
    Action(ActionDecl),
    Flow(FlowDecl),
    View(ViewDecl),
    Fn(FnDecl),
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
    /// `(origin: mail.message)`: the outside service whose resources the type holds
    /// (convergence.md, "Outside services"), as the dotted name of one of its kinds.
    pub origin: Option<Ident>,
    pub fields: Vec<FieldDecl>,
    /// `search { ... }`: what search, a list's filters and agents offer of the type.
    pub search: Option<SearchBlock>,
    pub span: Span,
}

/// `search { filter done; sort due oldest; hide notes }` in a type (search.md, "What a type
/// declares"): the filters people and agents find its resources by, in order, its default order,
/// and fields kept out of filters.
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
    /// `filter unread "Unread" (icon: .mail)`: a filter on a field, its control derived from
    /// the field's type.
    Field {
        field: Ident,
        label: Option<String>,
        opts: Vec<ActionOpt>,
    },
    /// `filter "Open" = not done (icon: .circle)`: a named predicate over the resource's own
    /// fields, shown as a toggle.
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

/// `enum Name { a, b(x: T, y: U), c }`: a sum type. Values are written `.a`, `.b(x: 1, y: 2)`.
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
    /// The payload fields, in order; empty for a variant without payload.
    pub fields: Vec<Param>,
    /// `(icon: .pin)` after the name and payload, as written.
    pub opts: Vec<ActionOpt>,
    pub span: Span,
}

/// `type Notes = Collection<Note>`, `type Channel = TextChannel | Thread`.
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

/// `scenario "a week of notes" { ... }`: statements that fill a workspace with sample data for
/// screenshots, tests and benchmarks. It never touches a person's data.
#[derive(Clone, Debug, PartialEq)]
pub struct ScenarioDecl {
    pub node: NodeId,
    pub doc: Vec<String>,
    pub name: String,
    pub name_span: Span,
    /// `scenario "editing a note" -> Note { ... }`: what the scenario returns, which the window
    /// shows once it is staged, as it shows what a primary action made.
    pub result: Option<TypeExpr>,
    pub body: Block,
    pub span: Span,
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
    /// `(title)` or `(body)`: what the field is to the platform; `(previously)`: what the type
    /// held before, which only its migrations read.
    pub marker: Option<Ident>,
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
    /// `Int`, `Album`, `todo.Item`.
    Named(Vec<Ident>),
    /// `List<T>`, `Collection<T>`, ...
    Generic(Ident, Vec<TypeExpr>),
    Tuple(Vec<TypeExpr>),
    /// `(T) -> View`: what a parameter of a view takes when it is given a lambda or a block
    /// with an item.
    Fn(Vec<TypeExpr>, Box<TypeExpr>),
    /// `TextChannel | Thread`: a resource of any of these types.
    Union(Vec<TypeExpr>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub node: NodeId,
    pub name: Ident,
    pub ty: TypeExpr,
    /// `done: Bool = not item.done`: an action parameter's default, evaluated when the action is
    /// invoked without it, over the parameters before it, and recorded as the argument.
    pub default: Option<Expr>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Proposal {
    pub view: Ident,
    /// The action parameters passed to the view, when written out.
    pub args: Option<Vec<Ident>>,
}

/// `primary` or `destructive` before `action`: how the platform presents the action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionRole {
    /// The one main action on its subject: the header's Primary button and the empty state's
    /// action.
    Primary,
    /// Drawn as destructive; undone rather than confirmed.
    Destructive,
    /// `migration action`: never offered; the platform runs it once on each resource of its
    /// subject's type that a version before it wrote, as an ordinary action, to bring what that
    /// version stored to where this one keeps it. Only a migration reads `(previously)` fields.
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
    pub proposal: Option<Proposal>,
    /// `(icon: .plus, label: "New")`, as written.
    pub opts: Vec<ActionOpt>,
    /// The `require` lines heading the body: when the action is available on its subject.
    pub requires: Vec<Require>,
    pub body: Block,
    pub span: Span,
}

/// `require not d.sent "{d.subject} went already"`, heading an action's body: a condition on the
/// action's subject (its first parameter) the action needs, and why it is unavailable when the
/// condition does not hold. Where the action would be offered on the subject (its window's
/// header, a menu, an agent's choices) it is not offered then; invoked anyway, it is refused
/// with the reason before it changes anything. The condition is pure: it reads only the
/// subject, as the action's presentation does.
#[derive(Clone, Debug, PartialEq)]
pub struct Require {
    pub cond: Expr,
    /// Text: why the action is unavailable.
    pub reason: Expr,
    pub span: Span,
}

impl ActionDecl {
    /// Whether the action's presentation follows its subject: a `label:` or `icon:` written
    /// as an `if`, an `on:` state, or a `require` on it. It is then lowered to a hidden view of
    /// the subject.
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

    /// Whether `label` is text that interpolates the subject, and nothing else: it follows the
    /// subject wherever the action is offered on it.
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
pub struct FlowDecl {
    pub node: NodeId,
    pub doc: Vec<String>,
    pub name: Ident,
    pub params: Vec<Param>,
    pub result: Option<TypeExpr>,
    pub body: Block,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ViewDecl {
    pub node: NodeId,
    pub doc: Vec<String>,
    pub name: Ident,
    /// `view list<T>(...)`: the types its parameters are written over, each taken from what a
    /// call passes.
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
    /// `as who { ... }`: in a scenario, the actions the block calls run with `invoker` = `who`;
    /// `as who at time { ... }` also with `now` = `time`.
    As {
        who: Expr,
        at: Option<Expr>,
        body: Block,
    },
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
    /// `??`: the Option's value, or the fallback.
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

#[derive(Clone, Debug, PartialEq)]
pub struct RefLit {
    /// The app part as written (an app id or an alias); empty for `@kind:id`.
    pub app: String,
    pub ty: String,
    pub id: String,
    pub version: Option<String>,
}

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
    /// `11rem`, `4dp`, `1fr`.
    Length(f64, crate::types::LengthUnit),
    /// `7d`, `5m`: a length of time, the value of a filter (`received: -7d`).
    Duration(i64, &'static str),
    /// `2026-06-29` (a Date), `2026-06-29T07:02:00Z` (a Time) or `2026-08` (a month, the value
    /// of a filter), in canonical ISO text.
    Calendar(String),
    /// `a..b`, `a..`, `..b`: a range, the value of a filter (`due: today()..today() + 7d`).
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
    /// `if cond { ... } else { ... }`: the value of the branch taken, or a statement.
    If {
        cond: Box<Expr>,
        then: Block,
        els: Option<Box<Expr>>,
    },
    /// `if let a = x, b = y { ... } else { ... }`: the first block with each Option's value
    /// bound, when every one has one; else the `else` branch.
    IfLet {
        binds: Vec<LetBind>,
        then: Block,
        els: Option<Box<Expr>>,
    },
    /// `{ stmts; expr }`: the value of the last expression.
    Block(Block),
    /// `match e { .a => x, .b(p, q) => y }`.
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    /// `.name` or `.name(field: value, ...)`: a variant of the enum the context expects.
    Variant {
        name: Ident,
        args: Option<Vec<Arg>>,
    },
    Lambda {
        param: Ident,
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
    /// `x?.name` or `x?.name(args)`: the field or method of the Option `x`'s value, as an
    /// Option; `.none` when `x` is.
    OptChain {
        base: Box<Expr>,
        name: Ident,
        args: Option<Vec<Arg>>,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Arg>,
    },
    /// `try act(args)`: invokes in a flow; a `Result` of the action's result and the reason
    /// it failed.
    Try(Box<Expr>),
    /// `root T`: the workspace's resource of the root type `T`.
    Root(TypeExpr),
}

/// `name = value` of an `if let`: `value` is an Option, `name` its value.
#[derive(Clone, Debug, PartialEq)]
pub struct LetBind {
    pub name: Ident,
    pub value: Expr,
}

/// `.b(x, y)`: a variant and names for its payload fields, in order (`_` ignores one).
#[derive(Clone, Debug, PartialEq)]
pub struct Pattern {
    /// The variant (`.some`), or for a type pattern the name the value is bound to.
    pub variant: Ident,
    pub binds: Vec<Ident>,
    /// `t: TextChannel`: the case of a union, by its type.
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
        value: Expr,
    },
    /// `if let a = x, b = y { ... } else { ... }` in a view body.
    IfLet {
        binds: Vec<LetBind>,
        then: Vec<UiStmt>,
        els: Option<Vec<UiStmt>>,
    },
    /// `window name: T = default` or `lens name: T = default`.
    State(StateDecl),
    /// `"Due": d.due`: one fact of a `facts { }` block.
    Fact {
        node: NodeId,
        label: Expr,
        value: Expr,
    },
    /// `trailing: { ... }`: the content of the slot `trailing` of a `ui.*` component.
    Slot {
        node: NodeId,
        name: Ident,
        body: Vec<UiStmt>,
    },
    /// `on activate: open(c)`: what the event `activate` of a `ui.*` component does. An event
    /// that carries a value names it, `on select(x: Note): open(x)`, and passes it on as an
    /// argument of the action; one that also says where it lands names that second,
    /// `on drop(moved: Task, at: Int): place(moved, at)`.
    On {
        event: Ident,
        param: Option<EventParam>,
        at: Option<EventParam>,
        action: Expr,
    },
}

/// The value an event carries, as `on select(x: Note): ...` names it.
#[derive(Clone, Debug, PartialEq)]
pub struct EventParam {
    pub name: Ident,
    /// Its type, when written; else the type the component's description gives.
    pub ty: Option<TypeExpr>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UiArm {
    pub pattern: Pattern,
    pub body: Vec<UiStmt>,
    /// Whether the body was written as a `{ }` block rather than one UI call.
    pub braced: bool,
}

/// Which layer of view state a declaration lives in (workspace-model.md, "The pieces").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StateLayer {
    /// Shared by everyone looking at the window.
    Window,
    /// One per person per window.
    Lens,
}

impl StateLayer {
    pub const fn word(self) -> &'static str {
        match self {
            Self::Window => "window",
            Self::Lens => "lens",
        }
    }

    /// The layer as the host ABI passes it.
    pub const fn code(self) -> i32 {
        match self {
            Self::Window => 0,
            Self::Lens => 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct StateDecl {
    pub node: NodeId,
    pub layer: StateLayer,
    pub name: Ident,
    pub ty: TypeExpr,
    pub default: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UiCall {
    pub node: NodeId,
    /// A call of a UI primitive or of a view, or a bare name.
    pub call: Expr,
    /// The item name of a list's item block: `list(xs) { x -> ... }`.
    pub item: Option<Ident>,
    pub block: Option<Vec<UiStmt>>,
    pub anchor: Option<Ident>,
}
