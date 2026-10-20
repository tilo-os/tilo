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
    /// `(icon: .name)`: the app's icon, a dot literal of the platform's icon set.
    pub icon: Option<Ident>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Use(UseDecl),
    Type(TypeDecl),
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
    pub fields: Vec<FieldDecl>,
    pub span: Span,
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
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RootDecl {
    pub node: NodeId,
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
/// screenshots, tests and benchmarks (native-apps.md section 5).
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
    /// `(title)` or `(body)`: what the field is to the platform.
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
}

#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub node: NodeId,
    pub name: Ident,
    pub ty: TypeExpr,
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
}

impl ActionRole {
    pub const fn word(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Destructive => "destructive",
        }
    }
}

/// One option of an action's `(icon: .x, label: "y")` list.
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
    pub body: Block,
    pub span: Span,
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
        }
    }

    /// Binding strength, higher binds tighter.
    pub const fn prec(self) -> u8 {
        match self {
            Self::Or => 1,
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
    Call {
        callee: Box<Expr>,
        args: Vec<Arg>,
    },
    /// `try act(args)`: invokes in a flow; a `Result` of the action's result and the reason
    /// it failed.
    Try(Box<Expr>),
    /// `root T`: in a scenario, the workspace's resource of the root type `T`.
    Root(TypeExpr),
}

/// `.b(x, y)`: a variant and names for its payload fields, in order (`_` ignores one).
#[derive(Clone, Debug, PartialEq)]
pub struct Pattern {
    pub variant: Ident,
    pub binds: Vec<Ident>,
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
    /// `window name: T = default` or `lens name: T = default`.
    State(StateDecl),
    /// `"Due": d.due`: one fact of a `facts { }` block.
    Fact {
        node: NodeId,
        label: Expr,
        value: Expr,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct UiArm {
    pub pattern: Pattern,
    pub body: Vec<UiStmt>,
    /// Whether the body was written as a `{ }` block rather than one UI call.
    pub braced: bool,
}

/// Which layer of view state a declaration lives in (workspace-model.md section 1).
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
