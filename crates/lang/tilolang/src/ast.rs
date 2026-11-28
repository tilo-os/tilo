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
    /// (`shell.style_shell`): the resolver admits such a plain name, since the template's body is
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
    /// `(tagline: "...")`: what the app does, in one plain phrase, as the Store and search show it
    /// under its name.
    pub tagline: Option<String>,
    /// `(keywords: ["note", "memo"])`: the other words people know the app by, which search
    /// matches.
    pub keywords: Vec<String>,
    /// `(previously: so.tilo.old)`: the ids the app was published under before; its resources,
    /// grants and windows under them are the app's, when the same publisher signed both.
    pub previously: Vec<String>,
    /// `(splash_budget: 250)`: the milliseconds a person may wait between asking for the app's
    /// window and its first frame. It becomes the manifest's `[entrypoint] splash-budget`, which
    /// the session holds the app's open to.
    pub splash_budget: Option<u32>,
    /// Each member keeps the app's records in their own workspace (`resources: .own`).
    pub own_resources: bool,
    /// Each window is the person's own who opened it, and never stands on a workspace's shared
    /// desktop (`windows: .personal`).
    pub personal_windows: bool,
    /// What each window of the app shows one of: `(document: workbook)`, a named root the app
    /// keeps one of for each document and none for the workspace, or `(document:
    /// library.documents)`, one item of a collection the workspace's one root holds.
    pub document: Option<WindowDocument>,
    /// `(web: ["example.com", "*.rss.org"])`: the hosts `uses web` requests may reach; any other is
    /// refused. `*.rss.org` covers subdomains, not `rss.org`.
    pub web: Vec<String>,
    /// `(connections: ["lsp"])`: the kinds of held connection the app keeps open in a process of
    /// its own ([`crate::connections`]), which `connection.open` names.
    pub connections: Vec<String>,
    /// `(schemes: ["tube"])`: the URI schemes the app opens, as a native app's manifest claims
    /// them: a link of one of them starts this app, which shows what the link names.
    pub schemes: Vec<String>,
    /// `(engine: "tilo-runtime/2026.10.23-chromium")`: the runtime bundle the app shows a web
    /// page with. The package's manifest requires it, and the launcher runs the app in the
    /// web engine's sandbox.
    pub engine: Option<String>,
    pub span: Span,
}

/// `document: root` or `document: root.items` in an app's header.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowDocument {
    /// The named root.
    pub root: Ident,
    /// The collection field of the root each window shows one item of; none when each window is
    /// a root of its own.
    pub items: Option<Ident>,
}

impl std::fmt::Display for WindowDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.items {
            Some(items) => write!(f, "{}.{}", self.root.name, items.name),
            None => f.write_str(&self.root.name),
        }
    }
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
    /// `place view v for T`: what a place of a `T` shows, as content on its own (a
    /// place on the canvas, an answer's view), when its default view is the app's window.
    PlaceView(DefaultViewDecl),
    Views(ViewsDecl),
    Scenario(ScenarioDecl),
    /// `op "delete_branch" changes device`: work the device's engine does for the app, by the
    /// name `engine.call` and `engine.start` ask it by, with what it does beyond answering.
    Op(OpDecl),
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
            Self::Op(d) => &d.doc,
            Self::Trigger(d) => &d.doc,
            Self::Action(d) => &d.doc,
            Self::View(d) => &d.doc,
            Self::Fn(d) => &d.doc,
            Self::Use(d) => &d.doc,
            Self::Root(d) => &d.doc,
            Self::DefaultView(d) | Self::RowView(d) | Self::PlaceView(d) => &d.doc,
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
            Self::DefaultView(d) | Self::RowView(d) | Self::PlaceView(d) => d.span,
            Self::Views(d) => d.span,
            Self::Scenario(d) => d.span,
            Self::Op(d) => d.span,
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
    pub doc: Vec<String>,
    pub app: String,
    pub alias: Option<Ident>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypeDecl {
    pub node: NodeId,
    pub doc: Vec<String>,
    pub name: Ident,
    /// `prefs Name { ... }`: the app's preferences, read and set through the settings service
    /// (`settings/apps/<app>`). A prefs declaration is a record type plus a root the host reads
    /// from the settings service.
    pub prefs: bool,
    /// `(origin: mail.message)`: the outside service kind whose resources the type holds
    /// (convergence.md, "Outside services").
    pub origin: Option<Ident>,
    /// `(title: subject)`: the field a record is named by ([`crate::resolve::TypeInfo::name_field`]).
    pub title: Option<TitleDecl>,
    /// `(untitled: "Untitled")`: what a record is called while its title is empty.
    pub untitled: Option<UntitledDecl>,
    /// `(label: "Notes")`: what a person reads the type as, where that is not its name in words.
    pub label: Option<String>,
    /// `(noun: "notebook")`: what one of the type is called in a sentence, where its name in words
    /// says something else.
    pub noun: Option<String>,
    /// `(readers: author)`: who reads a record of the type, when not every member.
    pub readers: Option<ReadersDecl>,
    /// `(sealed: true)`: a root type whose record has keys of its own from its first write.
    pub sealed: bool,
    pub fields: Vec<FieldDecl>,
    pub search: Option<SearchBlock>,
    /// `live when state == .playing`: when a resource of the type is live, a predicate over its own
    /// fields as a named filter is. The type's `view live(x: T)` is what the shell shows of it.
    pub live: Option<LiveDecl>,
    pub span: Span,
}

/// `live when <expr>` in a type: when an instance is a live activity (playing, running, on a call),
/// read from its own fields, which the resource index keeps.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveDecl {
    pub when: Expr,
    pub span: Span,
}

/// Who reads a type's records (`readers: author until sent`, `readers: members`), and where it
/// says so.
#[derive(Clone, Debug, PartialEq)]
pub struct ReadersDecl {
    pub rule: crate::meta::ReaderRule,
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
    /// `conceal secret`: a field kept out of the search index and out of filters, so search never
    /// holds or finds what it says. A credential, a note kept for one person.
    Conceal { field: Ident },
    /// `kind conversation`: what a resource of the type is to the system's search, which draws
    /// it as one and sends to it (`conversation`, `message`, `person` or `agent`).
    Kind { kind: Ident },
    /// `people person`: a field naming who the resource is with, an `Actor` or a set of them.
    People { field: Ident },
    /// `identity number`: the `Int` field a person names a resource by, as `#12`. Search indexes
    /// both `#12` and `12`, so typing either finds the resource and opens it (search.md, "A number
    /// a resource is named by").
    Identity { field: Ident },
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
    pub doc: Vec<String>,
    /// `root notes: Notes`: the name code reaches the root by (`notes`, `alias.notes`).
    pub name: Option<Ident>,
    /// `device root readings: Readings`: one root for each device (`devices/<device>/<app>/…`)
    /// rather than one for the workspace. What a device reads and keeps of its own goes here;
    /// what people share goes in an ordinary root.
    pub device: bool,
    pub ty: TypeExpr,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DefaultViewDecl {
    pub node: NodeId,
    pub doc: Vec<String>,
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

/// `op "push" leaves`: an op of the device's engine the package asks (`engine.call`,
/// `engine.start`, `engine.stream`, `engine.picture`), declared with what it does besides
/// answering, so every action that asks it is classed by it and none can leave it out.
#[derive(Clone, Debug, PartialEq)]
pub struct OpDecl {
    pub node: NodeId,
    pub doc: Vec<String>,
    /// The name the engine knows the op by.
    pub name: String,
    pub name_span: Span,
    pub facts: Vec<(OpFact, Span)>,
    pub span: Span,
}

/// What an op does besides answering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpFact {
    /// `changes device`: it changes the device (a repository on its disk, a running shell),
    /// which the workspace's undo cannot take back, so an action that asks it is never undoable.
    ChangesDevice,
    /// `reaches outside`: it reaches a service outside Tilo (a message to someone on Signal):
    /// an action that asks it is an outside effect, and a caller other than the person is
    /// asked first.
    ReachesOutside,
    /// `leaves`: what it does leaves the system for good (that message, sent): it reaches
    /// outside too, and a person's own press of an action that asks it always confirms first.
    Leaves,
}

impl OpFact {
    /// The words a declaration says it by.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Self::ChangesDevice => "changes device",
            Self::ReachesOutside => "reaches outside",
            Self::Leaves => "leaves",
        }
    }
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
    /// `on t.device`: the field naming the device the action's effect lands on, which a device
    /// reads to arm its own wakeup for the instant (`at` only).
    pub device: Option<Expr>,
    /// The action it runs: the target, and any roots it passes.
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
    /// `password: Text (secret)`: a value history never shows.
    pub secret: bool,
    /// `saved: Bool (label: "Kept in your places")`: what a person reads it as.
    pub label: Option<String>,
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
    /// `token: Text (secret)`: the value is handed over once and kept nowhere, so no record of a
    /// call keeps it.
    pub secret: bool,
}

/// `primary`, `destructive`, `migration`, `initial` or `opened` before `action`: how the action is,
/// not where a surface shows it. A surface decides placement from what the action offers a person,
/// never from its role; only the ones the platform applies (a migration, an initial and an opened
/// action) are left out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionRole {
    /// The one main action on its target: the header's Primary button and the empty state's
    /// action.
    Primary,
    /// Drawn as destructive; undone rather than confirmed.
    Destructive,
    /// `migration action`: never offered; the platform runs it once on each resource an earlier
    /// version wrote.
    Migration,
    /// `initial action`: never offered; what each new resource of its type starts with, which
    /// the platform writes in the change that makes the resource.
    Initial,
    /// `opened action`: never offered; what opening a resource of its type does, wherever it is
    /// opened from, run as the person who opened it.
    Opened,
    /// `closed action`: never offered; what closing a window of a resource of its type does, run
    /// as the person whose window it was.
    Closed,
    /// `agents action`: offered to agents and code, never on a person's lists: a step a view
    /// runs itself, which is no command of a person's.
    Agents,
}

impl ActionRole {
    pub const fn word(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Destructive => "destructive",
            Self::Migration => "migration",
            Self::Initial => "initial",
            Self::Opened => "opened",
            Self::Closed => "closed",
            Self::Agents => "agents",
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
    pub opts: Vec<ActionOpt>,
    /// The `require` lines heading the body: when the action is available on its target.
    pub requires: Vec<Require>,
    /// The `applies` lines heading the body: when the action can apply where it is offered (`applies
    /// above != .none`). A surface then leaves it out of an item's menu rather than showing a row
    /// that can never work.
    pub applies: Vec<Require>,
    pub body: Block,
    pub span: Span,
}

/// `require not d.sent "{d.subject} went already"`: a condition on the target the action needs, and
/// why it is unavailable without it. Not offered where it would be; invoked anyway, it is refused
/// with the reason before it changes anything. The condition reads only the target.
#[derive(Clone, Debug, PartialEq)]
pub struct Require {
    pub cond: Expr,
    /// Text: why the action is unavailable.
    pub reason: Expr,
    pub span: Span,
}

impl ActionDecl {
    /// Whether the presentation follows the target: a `label:` or `icon:` written as an `if`, an
    /// `on:` state, or a `require` on it. It is then lowered to a hidden view of the target.
    pub fn presents_state(&self) -> bool {
        !self.requires.is_empty()
            || self.opts.iter().any(|o| match o.name.name.as_str() {
                "on" => true,
                "label" => {
                    matches!(o.value.kind, ExprKind::If { .. }) || self.label_reads_target(&o.value)
                }
                "icon" => matches!(o.value.kind, ExprKind::If { .. }),
                _ => false,
            })
    }

    /// Whether `label` is text interpolating the target and nothing else: it then follows the
    /// target wherever the action is offered.
    pub fn label_reads_target(&self, label: &Expr) -> bool {
        let ExprKind::Str(parts) = &label.kind else {
            return false;
        };
        let Some(target) = self.params.first() else {
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
                    ExprKind::Ident(n) if *n == target.name.name => any = true,
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
    /// `window "Desktop shell"`: what assistive technology knows the window this view fills by,
    /// when it is the view a whole window shows. A view that names none is known by its name.
    pub window: Option<String>,
    /// `protect capture`, heading the view's body: what a capture or a stream leaves out
    /// wherever the view is shown.
    pub protect: Vec<Protect>,
    pub body: Vec<UiStmt>,
    pub body_span: Span,
    pub span: Span,
}

/// `protect capture`: a view-level declaration that a capture or a stream leaves the view out
/// of, wherever it is shown.
#[derive(Clone, Debug, PartialEq)]
pub struct Protect {
    /// What is protected. Only `capture` is declared today.
    pub what: Ident,
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
    /// `suggest act(args)`: applies the action only once a person says yes to it, asking them even
    /// where it may run without; an agent block's value.
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
    Part {
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
        /// `binds .title`: the field of the record a `begin` handler's action makes that the
        /// field edits from then on. The handler is refused without it.
        binds: Option<Ident>,
    },
}

/// The place a view statement declares, when it is one: `on show week: ...` names a place of the
/// window by `week`, where `on show(x: Task): ...` takes an item.
#[must_use]
pub fn place_of(s: &UiStmt) -> Option<&EventParam> {
    match &s.kind {
        UiKind::On {
            event,
            param: Some(param),
            at: None,
            ..
        } if event.name == "show" && param.ty.is_none() => Some(param),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EventParam {
    pub name: Ident,
    /// Its type when written; else the type the component's description gives. A view's `on show`
    /// without one names a place of the window (`on show week: ...`), not an item.
    pub ty: Option<TypeExpr>,
    /// The words a place is called by when its name does not say them (`on show inbox "Task
    /// inbox": ...`).
    pub label: Option<Box<str>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UiArm {
    pub pattern: Pattern,
    pub body: Vec<UiStmt>,
    /// Whether the body was written as a `{ }` block rather than one UI call.
    pub braced: bool,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StateDecl {
    pub node: NodeId,
    pub name: Ident,
    pub ty: TypeExpr,
    pub default: Option<Expr>,
    /// `state pointer: List<Float> = [] kept`: where the person is (a pointer, a selection),
    /// which the platform keeps in the window and never in their record.
    pub kept: bool,
    pub span: Span,
}

/// How a view's clock is written: which of `every` and `after` it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clock {
    /// `every 1s: ...`: again and again while the view is shown.
    Every,
    /// `after 3s: ...`: once, that long after the view first showed it.
    After,
}

impl Clock {
    /// The word it is written with, which is also the `ui.Timer` prop it sets.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Every => "every",
            Self::After => "after",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct UiCall {
    pub node: NodeId,
    pub call: Expr,
    /// Written as the view's own clock, `every 1s: tick(c)` or `after 3s: shoot(c)`: the call is
    /// the toolkit's `ui.Timer` it stands for, and the source keeps the short form.
    pub clock: Option<Clock>,
    /// The names a block opens with: `list(xs) { x -> ... }`, or `(x, under)` for a template's
    /// function of several values.
    pub params: Vec<Ident>,
    pub block: Option<Vec<UiStmt>>,
    pub anchor: Option<Ident>,
}
