//! The checker: types, merge rules, what each kind of declaration may do, and lambdas. It
//! lowers well-typed code to the IR in the same walk, so a program that passes is already in the
//! form the backend consumes.
//!
//! What each kind of declaration may do:
//!
//! * a `fn` computes and calls `fn`s; it changes nothing and calls no action;
//! * a `view` reads resources, calls `fn`s and views and builds UI. Writing `act(args)` in a view
//!   makes an `Action` value; it doesn't run the action;
//! * an `action` reads and changes resources of its own package's types, and may call any action,
//!   its own or another package's: the call runs at its door, as a change of its own
//!   ([`docs/designs/app-language/action-system.md`]);
//! * an agent's program does what an action does, over every app it can see.
//!
//! Lambdas appear only as arguments of the built-in list methods and are inlined as loops, so
//! there are no closures at run time. Every error found is reported, not only the first
//! ([`crate::diag`]).

mod agentq;
mod boundary;
mod call;
mod component;
mod enums;
mod expr;
mod methods;
mod native;
mod optional;
mod order;
mod outcomes;
mod platform;
mod pred;
mod program;
mod props;
mod query;
mod template;
#[cfg(test)]
mod tests;
mod text_math;
mod ui;
mod union;
mod words;

pub(crate) use native::offer_decided;
pub use program::{RESOURCE_TYPE_NAME, check_program, resource_key};
pub use template::is_template;

use crate::abi::Imp;
use crate::ast::{self, Item, StmtKind};
use crate::diag::{Diagnostic, Span};
use crate::ir::{self, Conv, Ek, Facts, ForList, FuncKind, LocalId, Stmt};
use crate::resolve::{CKind, Callable, Env, ParamInfo, resolve_type_expr};
use crate::types::{MergeKind, Ty};
use std::collections::HashMap;

type Ex = ir::Expr;

/// How many people a rich input's `@` drawer offers from the conversation, unless a view asks
/// for another number (`speakers(items, max)`, chat.md "Completion drawers").
pub(crate) const DRAWER_SPEAKERS: i64 = 5;

/// What kind of code is being checked; each has its own effect discipline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Ctx {
    #[default]
    Fn,
    Action,
    /// An agent's program: statements over every app, which apply actions as their own steps
    /// and send field edits to the app that holds them.
    Program,
    View,
    /// An agent's block: reads and builds values, never changes anything.
    Block,
}

/// What the code being checked was written as.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Written {
    /// A declaration of the module.
    #[default]
    Declared,
    /// A scenario, where `root T` names a root resource.
    Scenario,
    /// An agent's program, which may `print`, `find` and `read`.
    Program,
}

/// Who `invoker` names outside an action.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Invoker {
    /// No one: only an action has an invoker.
    #[default]
    None,
    /// The person looking, where an action is presented: who would invoke it.
    Viewer,
}

impl Ctx {
    const fn word(self) -> &'static str {
        match self {
            Self::Fn => "a fn",
            Self::Action => "an action",
            Self::Program => "an agent's program",
            Self::View => "a view",
            Self::Block => "a block",
        }
    }
}

/// What the `invoke` call being checked is, if it is one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Invoked {
    /// Any other call.
    #[default]
    Plain,
    /// The operand of `try`: it reports failure rather than trapping.
    Try,
}

#[derive(Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each says one independent thing about the code being checked"
)]
struct FnState {
    ctx: Ctx,
    /// The declaration's name, as a message says it.
    decl: String,
    /// `uses web, time`: the effects the declaration says its body reads.
    uses: std::collections::BTreeSet<crate::effects::Effect>,
    /// The effects already reported as undeclared here: one message per missing word.
    reported: std::collections::BTreeSet<crate::effects::Effect>,
    locals: Vec<Ty>,
    scopes: Vec<HashMap<String, LocalId>>,
    sinks: Vec<Vec<Stmt>>,
    result: Ty,
    facts: Facts,
    view_id: String,
    /// The declaration whose message the words this code shows are attributes of, where it is
    /// not [`Self::view_id`]: an action's, for its label read where a view applies it and for a
    /// default an invocation computes.
    words_of: Option<String>,
    /// What a `let` kept, by the local it bound: the value as it was written, so a prop that
    /// shows words marks the literals inside it wherever the binding stands
    /// ([`words::Checker::mark_shown`]).
    lets: HashMap<LocalId, ast::Expr>,
    /// Locals holding the key suffix of each enclosing UI loop, innermost last.
    keys: Vec<LocalId>,
    /// What the `invoke` call being checked is: the operand of `try`, which reports failure
    /// rather than trapping.
    invoked: Invoked,
    /// Set while checking the operand of `suggest`: `Some(true)` when the call is the value an
    /// agent block proposes, `Some(false)` when it is a statement recording a proposal.
    suggest: Option<bool>,
    /// Where a `suggest` stood in the code being checked: a block proposes only as its whole
    /// expression, and says so where it stood elsewhere.
    suggested_at: Option<crate::diag::Span>,
    /// Whether the code being checked is an agent's block of expressions, whose own value its
    /// proposal may be. A block of view body builds a view, which proposes nothing.
    proposes: bool,
    /// The view state the view declares, by name.
    states: HashMap<String, StateRef>,
    /// What the code being checked was written as: a scenario or an agent's program
    /// allows what declared code does not.
    written: Written,
    /// The fields the code being checked writes from a parameter: a value it asks for, written
    /// where the type keeps it. What two actions offered on one kind of item must not both do.
    writes: Vec<(String, String, Ty)>,
    /// The locals bound to the parameters of what is being checked: a write is recorded only
    /// when the value written is one of them, never a literal or a local of its own.
    params: Vec<LocalId>,
    /// Whether the callable takes a resource: an outside effect is queued on the first one,
    /// so an action without one cannot hand any over.
    takes_resource: bool,
    /// The role of the action being checked (`primary`, `destructive`, `migration`).
    role: Option<String>,
    /// Who `invoker` is outside an action: while checking an action's requirements where it
    /// is presented, the person looking, who would invoke it.
    invoker: Invoker,
    /// Where the UI being lowered stands.
    ui: UiPlace,
    /// For a view made of a loop body, the declared view it was written in (for anchors).
    owner_view: Option<String>,
    /// UI loops of this view lowered to loop nodes so far.
    fors: usize,
    /// Views made of this view's loop bodies so far.
    row_views: usize,
    /// What the locals bound by `let` hold, in terms of places (see [`outcomes::Sym`]).
    syms: HashMap<LocalId, outcomes::Sym>,
    /// The parameters of the templates being expanded that are not values: view state,
    /// lambdas and blocks of the caller ([`template::Macro`]).
    macros: HashMap<String, template::Macro>,
    /// The package whose names the code being checked sees: `None` for the module's own.
    ns: Option<String>,
    /// The type parameters of the templates being expanded, bound.
    types: HashMap<String, Ty>,
    /// What node keys start with after the view's id: the templates being expanded, by call.
    prefix: String,
    /// The templates being expanded, outermost first, each by the package whose names its body
    /// sees and its name: two packages' views of the same name are not the same view.
    expanding: Vec<(Option<String>, String)>,
    /// The resource the view being lowered shows: its first parameter, when that is one. What
    /// the system's parts of its controls are about (a rich input's `@` drawer).
    subject: Option<(LocalId, Ty)>,
    /// The locals holding a scalar an event carries (`at` of `on drop(moved: T, at: Int)`),
    /// boxed until the event puts the value there.
    event_scalars: std::collections::HashSet<LocalId>,
    /// Whether the code being checked computes one argument of an action a view applies, which
    /// runs when that action is invoked (`Self::at_event`): it may read the clock, as the
    /// invocation it belongs to does, and what it reads is recorded there.
    press: bool,
    /// Whether the expression being checked is the action `file.choose` runs, whose file fills
    /// the parameter the action leaves out.
    choosing: bool,
}

/// Where the UI being lowered stands, saved and restored around each block.
#[derive(Clone, Copy, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each says one independent thing about where the UI stands"
)]
pub(crate) struct UiPlace {
    /// The primitive whose block is being lowered, empty at a view's top: `sidebar` and
    /// `inspector` sit directly in a frame, a fact directly in `facts`.
    parent: &'static str,
    /// Whether a list row `row(title, ...)` may stand here: directly in a list's item block,
    /// as a row view's body, or as the whole body of a view of the item it draws.
    list_row_ok: bool,
    /// Whether it is inside a `card`: a list there is the card's preview
    /// ([`crate::prims::PREVIEW_ROWS`]).
    in_card: bool,
    /// Whether it is in a named slot of a `ui.*` component: what a loop makes there are parts
    /// of the component (a menu's commands), so the loop lowers in place.
    parts: bool,
    /// Whether it is in a composer: text typed here is one person's until it is sent.
    composer: bool,
}

/// A `state` declaration in scope, for `bind:` and `set(...)`.
#[derive(Clone, Debug)]
pub(crate) struct StateRef {
    pub id: String,
    pub ty: Ty,
    pub local: LocalId,
}

pub(crate) struct Checker<'a> {
    env: &'a Env,
    diags: &'a mut Vec<Diagnostic>,
    st: FnState,
    /// Views made of UI loop bodies, in the order made; their functions follow the declared
    /// ones, from `synth_base` on.
    synth: Vec<(ir::Func, ir::SynthView)>,
    synth_base: usize,
    /// The view state templates declared where they expanded, by the view's id.
    states: Vec<(String, crate::resolve::StateInfo)>,
    /// The arguments computed from view state made so far ([`ir::AtEventFn`]); their
    /// functions follow the row views'.
    at_event: Vec<(ir::Func, ir::AtEventFn)>,
    /// What each view answers a request to show an item with ([`ir::ViewShow`]); their functions
    /// follow the arguments.
    shows: Vec<(ir::Func, ir::ViewShow)>,
    /// The words the views show as written, each once per view and key.
    words: Vec<crate::meta::WordMeta>,
    /// What each offered action of the module writes, with where it is declared: read once the
    /// bodies are checked, to find one command written twice.
    offers: Vec<Offer>,
    /// What the body checked last writes: `end` takes the state it was checked in, so the
    /// fields are kept here for the module to read.
    last_writes: Vec<FieldWritten>,
    /// Where the declaration the next function is lowered from starts, in the package source's
    /// span space: `end` takes it, so a function made in the middle of another's check (a row
    /// view, an argument computed from view state) is named by its own declaration.
    at: Option<usize>,
    /// Where the declaration being checked starts: what a function it makes in passing (a row
    /// view of its body, an argument computed from its view state) is named by.
    outer_at: Option<usize>,
    /// The string literals the module shows as words (by span): each is a message. A span is a
    /// place in the source, so one set says what every declaration of the module shows.
    shown: std::collections::HashSet<Span>,
}

/// A field an offered action writes from a value it asks for: the action, the field, its type.
type FieldWritten = (String, String, Ty);

/// What one offered action of the module writes, with the name and span it is declared at.
type Offer = (String, Span, Vec<FieldWritten>);

/// A condition an action keeps: which kind it is, the export that publishes it, and the lines
/// that make it.
type Condition<'a> = (FuncKind, fn(&str) -> String, &'a [ast::Require]);

/// What a checked agent block evaluates to.
pub struct BlockResult {
    pub module: ir::Module,
    pub ty: Ty,
    /// Whether the block's value is a proposal the host records instead of applying it.
    pub suggested: bool,
}

/// Checks every body of the module and lowers it. Errors go to `diags`.
pub fn check_module(env: &Env, module: &ast::Module, diags: &mut Vec<Diagnostic>) -> ir::Module {
    // Which literals are words is known before they are lowered: a literal a `let` keeps, or a
    // branch of one, is a message where a prop that shows words reaches it, and the body says
    // which of its literals those are. So the pass that says it runs first — its functions and
    // its diagnostics are thrown away — and the pass that lowers writes them as messages.
    let mut quiet = Vec::new();
    let shown = lower(env, module, &mut quiet, std::collections::HashSet::new()).1;
    lower(env, module, diags, shown).0
}

/// One pass over the module's bodies: the module, and the literals it shows as words.
fn lower(
    env: &Env,
    module: &ast::Module,
    diags: &mut Vec<Diagnostic>,
    shown: std::collections::HashSet<Span>,
) -> (ir::Module, std::collections::HashSet<Span>) {
    let declared = env
        .callables
        .iter()
        .filter(|x| x.own)
        .filter_map(|x| x.func)
        .chain(env.defaults.iter().map(|d| d.func))
        .map(|f| f.saturating_add(1))
        .max()
        .unwrap_or(0);
    let mut c = Checker {
        env,
        diags,
        st: FnState::default(),
        synth: Vec::new(),
        synth_base: declared,
        states: Vec::new(),
        at_event: Vec::new(),
        shows: Vec::new(),
        words: Vec::new(),
        offers: Vec::new(),
        last_writes: Vec::new(),
        at: None,
        outer_at: None,
        shown,
    };
    let mut funcs = Vec::new();
    // The actions, which declare what they read, and where their name is.
    let mut declaring: Vec<(usize, Span)> = Vec::new();
    for item in &module.items {
        let idx = funcs.len();
        let Some(callable) = env.callables.iter().find(|x| x.func == Some(idx)).cloned() else {
            continue;
        };
        c.at = Some(item.span().start);
        if let Item::Action(a) = item {
            declaring.push((idx, a.name.span));
        }
        let f = match item {
            Item::Action(a) => c.run_action(&callable, &a.requires, &a.applies, &a.body),
            Item::Fn(f) => c.run_body(&callable, FuncKind::Fn, Ctx::Fn, &f.body),
            Item::Scenario(s) => c.run_body(&callable, FuncKind::Action, Ctx::Action, &s.body),
            Item::View(v) if is_template(v) => continue,
            Item::View(v) => c.run_view(&callable, v),
            // A trigger declares no body: what it runs is the action it names.
            Item::Trigger(_) => continue,
            Item::Use(_)
            | Item::Type(_)
            | Item::Alias(_)
            | Item::Enum(_)
            | Item::Root(_)
            | Item::DefaultView(_)
            | Item::RowView(_)
            | Item::Views(_) => {
                continue;
            }
        };
        funcs.push(f);
        if let Item::Action(a) = item
            && !matches!(
                a.role.as_ref().map(|(role, _)| *role),
                Some(ast::ActionRole::Migration)
            )
            && !c.last_writes.is_empty()
        {
            let writes = std::mem::take(&mut c.last_writes);
            c.offers.push((a.name.name.clone(), a.name.span, writes));
        }
    }
    // The presentation views of actions, in the order the resolver numbered them.
    for item in &module.items {
        let Item::Action(a) = item else { continue };
        let idx = funcs.len();
        let Some(view) = env.callables.iter().find(|x| x.func == Some(idx)).cloned() else {
            continue;
        };
        if view.name != crate::ids::present_key(&a.name.name) {
            continue;
        }
        let Some(action) = env.own_callable(&a.name.name) else {
            continue;
        };
        c.at = Some(a.name.span.start);
        funcs.push(c.run_present(&view, &a.opts, action));
    }
    // The defaults of action parameters, in the order the resolver numbered them.
    for d in &env.defaults {
        if funcs.len() != d.func {
            break;
        }
        let written = module.items.iter().find_map(|item| match item {
            Item::Action(a) if a.name.name == d.action => a.params.get(d.param)?.default.as_ref(),
            _ => None,
        });
        let (Some(written), Some(action)) = (written, env.own_callable(&d.action)) else {
            break;
        };
        c.at = Some(written.span.start);
        funcs.push(c.run_default(action, d.param, written));
    }
    c.check_proposals(module);
    c.check_one_command();
    let mut views = Vec::new();
    let mut at_event = Vec::new();
    let mut shows = Vec::new();
    if funcs.len() == c.synth_base {
        for (f, v) in std::mem::take(&mut c.synth) {
            funcs.push(f);
            views.push(v);
        }
        for (f, mut a) in std::mem::take(&mut c.at_event) {
            a.func = funcs.len();
            funcs.push(f);
            at_event.push(a);
        }
        for (f, mut show) in std::mem::take(&mut c.shows) {
            show.func = funcs.len();
            funcs.push(f);
            shows.push(show);
        }
    } else if c.diags.is_empty() {
        c.err(
            "E000",
            Span::default(),
            "internal: the declared functions and their indexes disagree",
        );
    }
    // The guard and the `applies` conditions of each action that keeps one its subject and the
    // place it is offered in alone decide: what a surface offering it reads whether it can run
    // there, and whether it applies at all, from.
    for item in &module.items {
        let Item::Action(a) = item else { continue };
        let Some(action) = env.own_callable(&a.name.name) else {
            continue;
        };
        let decides = |lines: &[ast::Require]| {
            lines
                .iter()
                .any(|r| native::offer_decided(r, &action.params))
        };
        let conditions: [Condition; 2] = [
            (FuncKind::Guard, ir::guard_export, &a.requires),
            (FuncKind::Applies, ir::applies_export, &a.applies),
        ];
        for (kind, export, lines) in conditions {
            if !decides(lines) {
                continue;
            }
            c.at = lines.first().map(|line| line.span.start);
            funcs.push(c.run_guard(action, lines, &action.params, kind, export));
        }
    }
    // What each action reads, including through the fns and actions its body calls,
    // against what its `uses` clause declares. Its own reads are reported where they are
    // written; this reports the ones it reaches through a callee, at its name.
    let reached = crate::effects::reached(&funcs);
    for (idx, span) in declaring {
        let Some(callable) = env.callables.iter().find(|x| x.func == Some(idx)) else {
            continue;
        };
        let declared = &callable.uses;
        let own = funcs.get(idx).map(|f| &f.facts.effects);
        let word = match callable.kind {
            CKind::Action => "an action",
            _ => "a declaration",
        };
        for e in reached
            .get(idx)
            .into_iter()
            .flatten()
            .filter(|e| !declared.contains(e))
            .filter(|e| !own.is_some_and(|o| o.contains(e)))
        {
            c.err(
                "E121",
                span,
                format!(
                    "{word} `{}` reads {}, which a replay cannot compute again: declare `uses {}`",
                    callable.name,
                    e.what(),
                    e.word()
                ),
            );
        }
    }
    let module = ir::Module {
        funcs,
        views,
        states: c.states,
        at_event,
        shows,
        words: c.words,
    };
    let shown = c.shown;
    // Every prop a view gives a described component is one that component declares: a name the
    // catalogue does not know is refused by the window, so the compiler says it first.
    if diags.is_empty() {
        props::undeclared(&module, diags);
    }
    (module, shown)
}

/// Checks a block that is one expression.
pub fn check_block_expr(env: &Env, e: &ast::Expr, diags: &mut Vec<Diagnostic>) -> BlockResult {
    let mut c = Checker {
        env,
        diags,
        st: FnState::default(),
        synth: Vec::new(),
        synth_base: 0,
        states: Vec::new(),
        at_event: Vec::new(),
        shows: Vec::new(),
        words: Vec::new(),
        offers: Vec::new(),
        last_writes: Vec::new(),
        at: None,
        outer_at: None,
        shown: std::collections::HashSet::new(),
    };
    c.begin(Ctx::Block, &[], Ty::Unit, "b_main");
    c.st.proposes = true;
    let proposal = matches!(e.kind, ast::ExprKind::Suggest(_));
    let ex = c.expr(e, None);
    if let Some(at) = c.st.suggested_at.filter(|_| !proposal) {
        c.err(
            "E044",
            at,
            "`suggest` must be the whole expression of a block, or stand where an action runs",
        );
    }
    let ty = ex.ty.clone();
    if ty == Ty::Unit {
        c.err("E072", e.span, "a block must produce a value (a view, an action application or data), but this expression has none");
    }
    c.st.result = ty.clone();
    c.emit(Stmt::Return(Some(ex)));
    let f = c.end(FuncKind::Block, "b_main", "b_main");
    BlockResult {
        module: ir::Module {
            funcs: vec![f],
            views: Vec::new(),
            states: Vec::new(),
            at_event: Vec::new(),
            shows: Vec::new(),
            words: Vec::new(),
        },
        ty,
        // A block that is a `suggest` of an action is the proposal; the host records it where
        // it runs the block.
        suggested: proposal,
    }
}

/// Checks a block that is a piece of view body.
pub fn check_block_ui(env: &Env, body: &[ast::UiStmt], diags: &mut Vec<Diagnostic>) -> BlockResult {
    let mut c = Checker {
        env,
        diags,
        st: FnState::default(),
        synth: Vec::new(),
        synth_base: 0,
        states: Vec::new(),
        at_event: Vec::new(),
        shows: Vec::new(),
        words: Vec::new(),
        offers: Vec::new(),
        last_writes: Vec::new(),
        at: None,
        outer_at: None,
        shown: std::collections::HashSet::new(),
    };
    c.begin(Ctx::Block, &[], Ty::View, "block");
    c.view_body(body);
    let f = c.end(FuncKind::Block, "b_main", "b_main");
    BlockResult {
        module: ir::Module {
            funcs: vec![f],
            views: Vec::new(),
            states: Vec::new(),
            at_event: Vec::new(),
            shows: Vec::new(),
            words: Vec::new(),
        },
        ty: Ty::View,
        suggested: false,
    }
}

fn always_returns(stmts: &[Stmt]) -> bool {
    match stmts.last() {
        Some(Stmt::Return(_)) => true,
        Some(Stmt::If(_, a, b)) => always_returns(a) && always_returns(b),
        _ => false,
    }
}

/// How a typed list element is read back from a host list, and converted.
pub(crate) fn load_op(ty: &Ty, i: Imp, f: Imp, h: Imp) -> (Imp, Option<Conv>) {
    match ty {
        // A length of time is its seconds, as an Int is its number.
        Ty::Int | Ty::Duration => (i, None),
        Ty::Float => (f, None),
        Ty::Bool => (i, Some(Conv::IntToBool)),
        _ => (h, None),
    }
}

/// How a typed value is passed to a host store, and converted.
pub(crate) fn store_op(ty: &Ty, i: Imp, f: Imp, h: Imp) -> (Imp, Option<Conv>) {
    match ty {
        Ty::Int | Ty::Duration => (i, None),
        Ty::Float => (f, None),
        Ty::Bool => (i, Some(Conv::BoolToInt)),
        _ => (h, None),
    }
}

pub(crate) fn conv(c: Option<Conv>, e: Ex, ty: Ty) -> Ex {
    match c {
        Some(c) => Ex::new(Ek::Conv(c, Box::new(e)), ty),
        None => e,
    }
}

impl Checker<'_> {
    fn err(&mut self, code: &str, span: Span, msg: impl Into<String>) {
        self.diags.push(Diagnostic::new(code, span, msg));
    }

    fn show(&self, ty: &Ty) -> String {
        ty.display(&self.env.app)
    }

    /// Records that the code reads `effect`, written as `what` (`now`, `day_of(t)`, a time of no
    /// zone, `random.int`), and refuses it where it cannot be read: an action, and an agent's
    /// program, read only what its `uses` clause names (convergence.md, "Actions"); a view reads
    /// none but the zone it is drawn in; a `fn` and a block read what they are given and pass
    /// their reads to their caller.
    fn reads(&mut self, effect: crate::effects::Effect, span: Span, what: &str) {
        let refused = match self.st.ctx {
            Ctx::Action | Ctx::Program => !self.st.uses.contains(&effect),
            Ctx::View => effect != crate::effects::Effect::Time,
            _ => false,
        };
        if !refused {
            // A read that is allowed is the invocation's: what a `fn` reads is its caller's too.
            self.st.facts.effects.insert(effect);
            return;
        }
        if !self.st.reported.insert(effect) {
            return;
        }
        let what_it_reads = effect.what();
        let message = match self.st.ctx {
            Ctx::View => format!(
                "{what} reads {what_it_reads}, which a view cannot: a view is drawn again from what it is shown, where an action records it and is replayed from the record"
            ),
            Ctx::Block => format!(
                "{what} reads {what_it_reads}, which a block cannot: a block answers a question as it is asked, where an action records it and is replayed from the record"
            ),
            Ctx::Fn => format!(
                "{what} reads {what_it_reads}, which a fn cannot: a fn computes from the values it is given, so the read is written in the action that makes the invocation"
            ),
            _ => {
                let decl = self.st.decl.clone();
                format!(
                    "{what} reads {what_it_reads}, which a replay cannot compute again: {} `{decl}` declares it with `uses {}`",
                    self.st.ctx.word(),
                    effect.word()
                )
            }
        };
        self.err("E121", span, message);
    }

    // ---- function state -------------------------------------------------------------------

    fn begin(&mut self, ctx: Ctx, params: &[ParamInfo], result: Ty, view_id: &str) {
        self.st = FnState {
            ctx,
            result,
            view_id: view_id.to_string(),
            ..FnState::default()
        };
        self.st.scopes.push(HashMap::new());
        self.st.sinks.push(Vec::new());
        self.st.takes_resource = params.iter().any(|p| match &p.ty {
            Ty::Record(key) => !crate::host::is_value(key),
            Ty::Union(_) => true,
            _ => false,
        });
        for p in params {
            let local = self.declare(&p.name, p.ty.clone());
            self.st.params.push(local);
        }
    }

    fn end(&mut self, kind: FuncKind, name: &str, export: &str) -> ir::Func {
        let nparams = 0usize;
        let body = self.st.sinks.pop().unwrap_or_default();
        let st = std::mem::take(&mut self.st);
        ir::Func {
            kind,
            name: name.to_string(),
            export: export.to_string(),
            nparams,
            result: st.result,
            locals: st.locals,
            body,
            facts: st.facts,
            at: self.at.take(),
        }
    }

    /// An action's body, after its requirements: each refuses the invocation with its reason
    /// when its condition does not hold, before anything changes.
    fn run_action(
        &mut self,
        c: &Callable,
        requires: &[ast::Require],
        applies: &[ast::Require],
        body: &ast::Block,
    ) -> ir::Func {
        // What it applies to comes before what it can do: an invocation where it does not apply
        // is refused with the condition's reason, like one that does not meet a requirement.
        let guards: Vec<&ast::Require> = applies.iter().chain(requires).collect();
        self.run_guarded(c, FuncKind::Action, Ctx::Action, &guards, body)
    }

    fn run_body(&mut self, c: &Callable, kind: FuncKind, ctx: Ctx, body: &ast::Block) -> ir::Func {
        self.run_guarded(c, kind, ctx, &[], body)
    }

    fn run_guarded(
        &mut self,
        c: &Callable,
        kind: FuncKind,
        ctx: Ctx,
        guards: &[&ast::Require],
        body: &ast::Block,
    ) -> ir::Func {
        self.outer_at = self.at.or(self.outer_at);
        self.begin(ctx, &c.params, c.result.clone(), &c.id);
        self.st.decl.clone_from(&c.name);
        self.st.uses.clone_from(&c.uses);
        for r in guards {
            let cond = self.expr_as(&r.cond, &Ty::Bool);
            // Why it cannot run is words the person reads where it is refused.
            self.mark_shown(&r.reason);
            let reason = self.expr_as(&r.reason, &Ty::Text);
            let refuse = Stmt::Eval(Ex::host(Imp::Abort, vec![reason], Ty::Unit));
            self.emit(Stmt::If(cond, Vec::new(), vec![refuse]));
        }
        if c.scenario {
            self.st.written = Written::Scenario;
        }
        self.st.role.clone_from(&c.present.role);
        let valued = !matches!(c.result, Ty::Unit | Ty::Error);
        let stmts = match body.stmts.split_last() {
            // A body that ends in an expression returns its value.
            Some((
                ast::Stmt {
                    kind: StmtKind::Expr(last),
                    ..
                },
                rest,
            )) if valued => {
                self.push_sink();
                for s in rest {
                    self.stmt(s);
                }
                let v = self.expr_as(last, &c.result);
                self.emit(Stmt::Return(Some(v)));
                self.pop_sink()
            }
            _ => self.block_stmts(body),
        };
        if valued && !always_returns(&stmts) {
            let want = self.show(&c.result);
            self.err(
                "E029",
                body.span,
                format!("`{}` must return a {want} on every path; end the body with a {want} expression, or each path with `return <{want} value>`", c.name),
            );
        }
        if let Some(sink) = self.st.sinks.last_mut() {
            sink.extend(stmts);
        }
        let prefix = match kind {
            FuncKind::Action => "a_",
            FuncKind::Fn => "n_",
            FuncKind::View => "v_",
            FuncKind::Block => "b_",
            FuncKind::Guard => "q_",
            FuncKind::Applies => "p_",
        };
        // The fields it writes, kept for the module's own reading after `end` takes the rest.
        self.last_writes = std::mem::take(&mut self.st.writes);
        let mut f = self.end(kind, &c.name, &format!("{prefix}{}", c.id));
        f.nparams = c.params.len();
        f
    }

    fn run_view(&mut self, c: &Callable, v: &ast::ViewDecl) -> ir::Func {
        self.outer_at = self.at.or(self.outer_at);
        self.begin(Ctx::View, &c.params, Ty::View, &c.id);
        self.st.subject = c
            .params
            .first()
            .and_then(|p| self.lookup(&p.name))
            .filter(|(_, ty)| matches!(ty, Ty::Record(_)));
        let rest = self.view_state(&c.name, &v.body);
        // What the view answers when the window is asked to show an item of one of its types:
        // each a function of its own, run with the item, applied in the view's state.
        let (shows, rest) = Self::view_shows(&rest);
        let params: std::collections::BTreeSet<String> =
            c.params.iter().map(|p| p.name.clone()).collect();
        for show in &shows {
            self.run_show(&c.id, &c.name, show, &v.body, &params);
        }
        let rest = rest.as_slice();
        let env = self.env;
        let row_of = env
            .row_views
            .iter()
            .find(|(key, view)| key.app == env.app && *view == c.name);
        if let Some((key, _)) = row_of {
            self.row_view_body(&c.name, &key.name, rest, v.body_span);
            self.st.ui.list_row_ok = true;
        } else if self.st.subject.is_some() && self.body_is_row(rest) {
            // A view of one item whose body is its row is that item's row wherever it is
            // drawn: the parameters after the item are what the place around it states, which
            // is how a place states a fact once instead of every row repeating it.
            self.st.ui.list_row_ok = true;
        }
        self.view_body(rest);
        let mut f = self.end(FuncKind::View, &c.name, &format!("v_{}", c.id));
        f.nparams = c.params.len();
        f
    }

    // ---- locals and sinks -----------------------------------------------------------------

    fn fresh(&mut self, ty: Ty) -> LocalId {
        let id = self.st.locals.len() as LocalId;
        self.st.locals.push(ty);
        id
    }

    fn declare(&mut self, name: &str, ty: Ty) -> LocalId {
        let id = self.fresh(ty);
        self.bind_local(name, id);
        id
    }

    fn bind_local(&mut self, name: &str, id: LocalId) {
        if let Some(scope) = self.st.scopes.last_mut() {
            scope.insert(name.to_string(), id);
        }
    }

    fn lookup(&self, name: &str) -> Option<(LocalId, Ty)> {
        let id = self
            .st
            .scopes
            .iter()
            .rev()
            .find_map(|s| s.get(name).copied())?;
        let ty = self.st.locals.get(id as usize)?.clone();
        Some((id, ty))
    }

    fn locals_in_scope(&self) -> String {
        let mut names: Vec<&str> = self
            .st
            .scopes
            .iter()
            .flat_map(|s| s.keys().map(String::as_str))
            .collect();
        names.sort_unstable();
        names.dedup();
        if names.is_empty() {
            "none".to_string()
        } else {
            names.join(", ")
        }
    }

    fn emit(&mut self, s: Stmt) {
        if let Some(sink) = self.st.sinks.last_mut() {
            sink.push(s);
        }
    }

    fn push_sink(&mut self) {
        self.st.sinks.push(Vec::new());
        self.st.scopes.push(HashMap::new());
    }

    fn pop_sink(&mut self) -> Vec<Stmt> {
        self.st.scopes.pop();
        self.st.sinks.pop().unwrap_or_default()
    }

    fn block_stmts(&mut self, b: &ast::Block) -> Vec<Stmt> {
        self.push_sink();
        for s in &b.stmts {
            self.stmt(s);
        }
        self.pop_sink()
    }

    // ---- statements -----------------------------------------------------------------------

    fn stmt(&mut self, s: &ast::Stmt) {
        match &s.kind {
            StmtKind::Let { name, ty, value } => self.let_stmt(&name.name, ty.as_ref(), value),
            StmtKind::Assign { place, op, value } => self.assign(place, *op, value, s.span),
            StmtKind::For { var, iter, body } => {
                let list = self.expr(iter, None);
                let Some(elem) = self.element_of(&list.ty, iter.span) else {
                    return;
                };
                let mut fl = self.new_for(list, &elem);
                self.push_sink();
                self.bind_local(&var.name, fl.item_local);
                for st in &body.stmts {
                    self.stmt(st);
                }
                fl.body = self.pop_sink();
                self.emit(Stmt::ForList(Box::new(fl)));
            }
            StmtKind::Return(value) => self.return_stmt(value.as_ref(), s.span),
            StmtKind::Expr(e) => self.expr_stmt(e),
        }
    }

    /// `invoke(act(args), by: mia, at: 2026-06-29T07:02Z, from: msg)` in a scenario: runs what
    /// it names with `invoker` = `by` (the enclosing invoker where none is named), `now` = `at`
    /// where one is named, and what it creates recorded as made from `from` where one is given.
    /// The value is the expression's. Each invocation is a change of its own, so history and
    /// reflection show it as that person's at that time.
    fn builtin_invoke(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        if self.st.written != Written::Scenario {
            for a in args {
                let _ = self.expr(&a.value, None);
            }
            self.err("E094", span, "`invoke(...)` runs an action as a person at a time, which only a scenario does; elsewhere an action runs as whoever invokes it, now");
            return Ex::error();
        }
        let Some((first, rest)) = args.split_first() else {
            self.err(
                "E022",
                span,
                "`invoke` takes the action it runs: `invoke(send(t, \"Hi\"), by: mia)`",
            );
            return Ex::error();
        };
        if first.name.is_some() {
            self.err(
                "E022",
                first.value.span,
                "`invoke` takes the action it runs by position: `invoke(send(t, \"Hi\"), by: mia)`",
            );
            return Ex::error();
        }
        let mut by = None;
        let mut at = None;
        let mut from = None;
        for a in rest {
            let Some(name) = &a.name else {
                self.err(
                    "E022",
                    a.value.span,
                    "`invoke`'s context is named: `invoke(act(args), by: mia, at: 2026-06-29T07:02Z, from: r)`",
                );
                continue;
            };
            match name.name.as_str() {
                "by" => by = Some(self.expr_as(&a.value, &Ty::Principal)),
                "at" => at = Some(self.expr_as(&a.value, &Ty::Time)),
                "from" => {
                    let source = self.expr(&a.value, None);
                    match &source.ty {
                        Ty::Record(_) | Ty::Error => {}
                        other => {
                            let t = self.show(other);
                            self.err(
                                "E020",
                                a.value.span,
                                format!("`from` takes the resource what it runs was made from, a value of a record type; this is {t}"),
                            );
                        }
                    }
                    from = Some(source);
                }
                other => {
                    let _ = self.expr(&a.value, None);
                    self.err(
                        "E022",
                        name.span,
                        format!("`invoke` takes `by`, `at` and `from`; `{other}` is no part of an invocation"),
                    );
                }
            }
        }
        let value = self.expr(&first.value, None);
        let ty = value.ty.clone();
        let (mut stmts, last) = match value.kind {
            Ek::Seq(inner, last) => (inner, last),
            _ => (Vec::new(), Box::new(value)),
        };
        let opt_at = Ty::Option(Box::new(Ty::Time));
        let at = match at {
            Some(t) => Self::some(t, &opt_at),
            None => Ex::host(Imp::NoneV, Vec::new(), opt_at),
        };
        let by = by.unwrap_or_else(|| Ex::host(Imp::Invoker, Vec::new(), Ty::Principal));
        let opt_from = Ty::Option(Box::new(from.as_ref().map_or(Ty::Error, |f| f.ty.clone())));
        let from = match from {
            Some(f) => Self::some(f, &opt_from),
            None => Ex::host(Imp::NoneV, Vec::new(), opt_from),
        };
        let pop = Stmt::Eval(Ex::host(Imp::InvocationPop, Vec::new(), Ty::Unit));
        let mut out = vec![Stmt::Eval(Ex::host(
            Imp::InvocationPush,
            vec![by, at, from],
            Ty::Unit,
        ))];
        out.append(&mut stmts);
        let value = if ty.repr().is_some() {
            // What the call gave is held while the invocation is left, then handed back.
            let made = self.fresh(ty.clone());
            out.push(Stmt::Set(made, *last));
            out.push(pop);
            Ex::local(made, ty.clone())
        } else {
            // A call that gives nothing has nothing to hold.
            out.push(Stmt::Eval(*last));
            out.push(pop);
            Ex::new(Ek::Unit, Ty::Unit)
        };
        Ex::new(Ek::Seq(out, Box::new(value)), ty)
    }

    /// An expression whose value is not used: an `if`, `match` or block runs its branches as
    /// statements, so they need not agree on a type and an `if` needs no `else`.
    fn expr_stmt(&mut self, e: &ast::Expr) {
        match &e.kind {
            ast::ExprKind::If { cond, then, els } => {
                let cond = self.expr_as(cond, &Ty::Bool);
                let then = self.block_stmts(then);
                let els = match els {
                    None => Vec::new(),
                    Some(x) => {
                        self.push_sink();
                        self.expr_stmt(x);
                        self.pop_sink()
                    }
                };
                self.emit(Stmt::If(cond, then, els));
            }
            ast::ExprKind::Match { scrutinee, arms } => self.match_stmt(scrutinee, arms, e.span),
            ast::ExprKind::IfLet { binds, then, els } => {
                self.if_let_stmt(binds, then, els.as_deref());
            }
            ast::ExprKind::Block(b) => {
                let stmts = self.block_stmts(b);
                for s in stmts {
                    self.emit(s);
                }
            }
            _ => {
                let ex = self.expr(e, None);
                self.emit(Stmt::Eval(ex));
            }
        }
    }

    /// A block whose value is used: its last expression, checked against `exp`. A block that
    /// returns on every path has any type.
    fn block_value(&mut self, b: &ast::Block, exp: Option<&Ty>) -> Ex {
        self.push_sink();
        let value = match b.stmts.split_last() {
            Some((
                ast::Stmt {
                    kind: StmtKind::Expr(last),
                    ..
                },
                rest,
            )) => {
                for s in rest {
                    self.stmt(s);
                }
                Some(self.expr(last, exp))
            }
            _ => {
                for s in &b.stmts {
                    self.stmt(s);
                }
                None
            }
        };
        let stmts = self.pop_sink();
        let value = match value {
            Some(v) => v,
            None if always_returns(&stmts) => {
                Ex::new(Ek::Unreachable, exp.cloned().unwrap_or(Ty::Error))
            }
            None => Ex::new(Ek::Unit, Ty::Unit),
        };
        let ty = value.ty.clone();
        Ex::new(Ek::Seq(stmts, Box::new(value)), ty)
    }

    /// `if` whose value is used: both branches agree on a type, and one branch alone is the
    /// Option of it (`or_none`).
    fn if_value(
        &mut self,
        cond: &ast::Expr,
        then: &ast::Block,
        els: Option<&ast::Expr>,
        exp: Option<&Ty>,
    ) -> Ex {
        let c = self.expr_as(cond, &Ty::Bool);
        let hint = exp.filter(|t| !t.is_error());
        let t = self.block_value(then, hint);
        let Some(els) = els else {
            return Self::or_none(c, t);
        };
        let want = hint.cloned().unwrap_or_else(|| t.ty.clone());
        let t = self.coerce(t, &want, then.span);
        let f = if want.is_error() {
            self.expr(els, None)
        } else {
            self.expr_as(els, &want)
        };
        let want = if want.is_error() { f.ty.clone() } else { want };
        Ex::new(Ek::If(Box::new(c), Box::new(t), Box::new(f)), want)
    }

    fn let_stmt(&mut self, name: &str, ann: Option<&ast::TypeExpr>, value: &ast::Expr) {
        let want = ann.map(|t| resolve_type_expr(self.env, t, self.diags));
        let ex = match &want {
            Some(w) => self.expr_as(value, w),
            None => self.expr(value, None),
        };
        let sym = self.sym_of(value);
        let mut ty = want.unwrap_or_else(|| ex.ty.clone());
        if ty == Ty::Unit {
            self.err(
                "E020",
                value.span,
                format!("this expression has no value, so `{name}` cannot hold it"),
            );
            ty = Ty::Error;
        }
        let id = self.bind_let(name, ty, ex, value);
        if let Some(sym) = sym {
            self.st.syms.insert(id, sym);
        }
    }

    /// Bind the checked value `ex` to the new local `name`, of type `ty`: what a `let` holds.
    /// `written` is the value as the source wrote it, kept so a prop of words that shows the
    /// binding marks the literals inside it.
    fn bind_let(&mut self, name: &str, ty: Ty, ex: Ex, written: &ast::Expr) -> LocalId {
        let id = self.declare(name, ty);
        self.st.lets.insert(id, written.clone());
        self.emit(Stmt::Set(id, ex));
        id
    }

    fn return_stmt(&mut self, value: Option<&ast::Expr>, span: Span) {
        let want = self.st.result.clone();
        match value {
            None => {
                if !matches!(want, Ty::Unit | Ty::Error) {
                    let w = self.show(&want);
                    self.err(
                        "E029",
                        span,
                        format!("this function returns {w}; write `return <{w} value>`"),
                    );
                }
                self.emit(Stmt::Return(None));
            }
            Some(e) => {
                if matches!(want, Ty::Unit) {
                    self.err("E029", span, "this function has no result type, so `return` takes no value; declare one with `-> Type`");
                    let _ = self.expr(e, None);
                    self.emit(Stmt::Return(None));
                } else {
                    let ex = self.expr_as(e, &want);
                    self.emit(Stmt::Return(Some(ex)));
                }
            }
        }
    }

    /// The element type a `for` loop walks, or an error.
    fn element_of(&mut self, ty: &Ty, span: Span) -> Option<Ty> {
        if ty.is_error() {
            return None;
        }
        if let Some(t) = ty.element() {
            return Some(t.clone());
        }
        if matches!(ty, Ty::Map(_)) {
            self.err(
                "E080",
                span,
                "looping over a Map is not supported in v1; keep the keys in a Set or a Collection",
            );
        } else {
            let t = self.show(ty);
            self.err(
                "E020",
                span,
                format!("a `for` loop walks a List, Set or Collection, but this is {t}"),
            );
        }
        None
    }

    /// Allocates the locals of a loop over a host list.
    fn new_for(&mut self, list: Ex, elem: &Ty) -> ForList {
        let (get, get_conv) = load_op(elem, Imp::ListGetI, Imp::ListGetF, Imp::ListGetH);
        ForList {
            list,
            list_local: self.fresh(Ty::Text),
            len_local: self.fresh(Ty::Int),
            idx_local: self.fresh(Ty::Int),
            item_local: self.fresh(elem.clone()),
            get,
            get_conv,
            body: Vec::new(),
        }
    }

    // ---- assignment and merge rules -------------------------------------------------------

    fn assign(&mut self, place: &ast::Place, op: ast::AssignOp, value: &ast::Expr, span: Span) {
        let Some((last, prefix)) = place.segs.split_last() else {
            return;
        };
        if prefix.is_empty() {
            self.assign_local(last, op, value);
            return;
        }
        let mut base = ast::Expr {
            kind: ast::ExprKind::Ident(prefix[0].name.clone()),
            span: prefix[0].span,
        };
        for seg in prefix.iter().skip(1) {
            let sp = base.span.to(seg.span);
            base = ast::Expr {
                kind: ast::ExprKind::Field {
                    base: Box::new(base),
                    name: seg.clone(),
                },
                span: sp,
            };
        }
        let base_ex = self.expr(&base, None);
        let base_ty = base_ex.ty.clone();
        self.assign_field(base_ex, last, op, value, span);
        if op == ast::AssignOp::Set
            && self.st.ctx == Ctx::Action
            && let Ty::Record(key) = &base_ty
            && let Some(f) = self.env.type_info(key).and_then(|t| t.field(&last.name))
            && f.merge == MergeKind::Register
        {
            let ty = f.ty.clone();
            self.self_dependent_write(&base, last, &ty, value, span);
        }
    }

    fn assign_local(&mut self, name: &ast::Ident, op: ast::AssignOp, value: &ast::Expr) {
        let Some((id, ty)) = self.lookup(&name.name) else {
            let known = self.locals_in_scope();
            self.err(
                "E014",
                name.span,
                format!(
                    "unknown variable `{}`; variables in scope: {known}",
                    name.name
                ),
            );
            let _ = self.expr(value, None);
            return;
        };
        // What the variable holds now depends on the path taken here.
        self.st.syms.remove(&id);
        let cur = Ex::local(id, ty.clone());
        let rhs = self.expr_as(value, &ty);
        let new = match op {
            ast::AssignOp::Set => rhs,
            ast::AssignOp::Add | ast::AssignOp::Sub => {
                let bop = if op == ast::AssignOp::Add {
                    ir::BinOp::Add
                } else {
                    ir::BinOp::Sub
                };
                match &ty {
                    Ty::Int | Ty::Float => {
                        Ex::new(Ek::Bin(bop, Box::new(cur), Box::new(rhs)), ty.clone())
                    }
                    Ty::Text if op == ast::AssignOp::Add => {
                        Ex::host(Imp::StrConcat, vec![cur, rhs], Ty::Text)
                    }
                    Ty::Error => rhs,
                    other => {
                        let t = self.show(other);
                        self.err("E034", name.span, format!("`{}` on a {t} variable is not defined; `+=` and `-=` work on Int, Float and (for `+=`) Text", op.text()));
                        rhs
                    }
                }
            }
        };
        self.emit(Stmt::Set(id, new));
    }

    fn assign_field(
        &mut self,
        base: Ex,
        field: &ast::Ident,
        op: ast::AssignOp,
        value: &ast::Expr,
        span: Span,
    ) {
        let Ty::Record(key) = base.ty.clone() else {
            if !base.ty.is_error() {
                let t = self.show(&base.ty);
                self.err(
                    "E021",
                    field.span,
                    format!(
                        "cannot assign `{}`: the value has type {t}, which has no fields",
                        field.name
                    ),
                );
            }
            let _ = self.expr(value, None);
            return;
        };
        let Some(info) = self.env.type_info(&key) else {
            return;
        };
        let Some(f) = info.field(&field.name).cloned() else {
            let names = info.field_names();
            self.err(
                "E021",
                field.span,
                format!(
                    "`{}` has no field `{}`; fields are: {names}",
                    key.name, field.name
                ),
            );
            let _ = self.expr(value, None);
            return;
        };
        if let Some(kind) = info.origin.as_deref().and_then(crate::origin::kind)
            && let Some(of) = kind.field(&field.name)
            && !of.people
            && self.st.written != Written::Scenario
        {
            self.err(
                "E113",
                field.span,
                format!(
                    "`{}` comes from the service (`{}`): only it writes it, so a refresh never undoes what someone did; people change {}",
                    field.name,
                    kind.name,
                    {
                        let theirs: Vec<&str> =
                            kind.fields.iter().filter(|f| f.people).map(|f| f.name).collect();
                        if theirs.is_empty() { "none of its fields".to_owned() } else { theirs.join(", ") }
                    }
                ),
            );
        }
        if self.st.written == Written::Program {
            self.program_edit(base, &key, &f, op, value, span);
            return;
        }
        if self.st.ctx != Ctx::Action {
            let who = self.st.ctx.word();
            self.err("E040", span, format!("{who} cannot change `{}.{}`; only an action mutates fields. Write an action and apply it", key.name, f.name));
            let _ = self.expr(value, None);
            return;
        }
        if key.app != self.env.app {
            self.err("E045", span, format!("`{}` is declared by another app ({}); an action may only change its own module's types. Call that app's action instead", key.name, key.app));
            let _ = self.expr(value, None);
            return;
        }
        self.st.facts.mutates = true;
        if f.merge == MergeKind::Structural {
            self.err("E033", span, format!("`{}` is a {} field and cannot be assigned; change it with its methods (`.insert`, `.remove`, `.move`, `.add`, `.put`)", f.name, f.ty.display(&self.env.app)));
            let _ = self.expr(value, None);
            return;
        }
        // A field set from a value the action asks for is what a second action asking the same
        // value of the same type cannot also be for; a literal or a local of its own is not.
        let from_a_parameter = match &value.kind {
            ast::ExprKind::Ident(read) => self
                .lookup(read)
                .is_some_and(|(local, ty)| self.st.params.contains(&local) && ty.same(&f.ty)),
            _ => false,
        };
        if from_a_parameter {
            self.st
                .writes
                .push((info.id.clone(), f.name.clone(), f.ty.clone()));
        }
        let fid = Ex::str(&f.id);
        match (op, f.merge) {
            (ast::AssignOp::Add | ast::AssignOp::Sub, MergeKind::Counter) => {
                let mut d = self.expr_as(value, &f.ty);
                if op == ast::AssignOp::Sub {
                    d = Ex::new(Ek::Neg(Box::new(d)), f.ty.clone());
                }
                let imp = if f.ty == Ty::Float {
                    Imp::ResAddF
                } else {
                    Imp::ResAddI
                };
                self.emit(Stmt::Eval(Ex::host(imp, vec![base, fid, d], Ty::Unit)));
            }
            (ast::AssignOp::Add | ast::AssignOp::Sub, MergeKind::Text) => {
                self.err("E034", span, format!("`{}` on a text field is not defined; assign the whole text with `=` and the host splices the difference", op.text()));
                let _ = self.expr(value, None);
            }
            (ast::AssignOp::Add | ast::AssignOp::Sub, _) => {
                let ty = self.show(&f.ty);
                self.err(
                    "E030",
                    span,
                    format!(
                        "`{}` on a register field loses concurrent updates; declare `{name}: {ty} merge counter`, or take the new value as an argument: a parameter with a default the invoker gets when they leave it out (`{name}: {ty} = x.{name} + 1`, then `x.{name} = {name}`)",
                        op.text(),
                        name = f.name,
                    ),
                );
                let _ = self.expr(value, None);
            }
            (ast::AssignOp::Set, MergeKind::Counter) => {
                self.err("E031", span, format!("`=` on a counter field overwrites concurrent increments; use `+=` or `-=` on `{}`, or declare it `merge register`", f.name));
                let _ = self.expr(value, None);
            }
            (ast::AssignOp::Set, _) => {
                let v = self.expr_as(value, &f.ty);
                let stmt = if f.merge == MergeKind::Text {
                    Ex::host(Imp::ResTextSet, vec![base, fid, v], Ty::Unit)
                } else {
                    let (imp, c) = store_op(&f.ty, Imp::ResSetI, Imp::ResSetF, Imp::ResSetH);
                    Ex::host(imp, vec![base, fid, conv(c, v, Ty::Int)], Ty::Unit)
                };
                self.emit(Stmt::Eval(stmt));
            }
        }
    }

    // ---- proposals ------------------------------------------------------------------------

    fn check_proposals(&mut self, module: &ast::Module) {
        for item in &module.items {
            let Item::Action(a) = item else { continue };
            let Some(p) = &a.proposal else { continue };
            let env = self.env;
            let Some(view) = env
                .own_callable(&p.view.name)
                .filter(|c| c.kind == CKind::View)
            else {
                let views: Vec<&str> = env
                    .callables
                    .iter()
                    .filter(|c| c.own && c.kind == CKind::View && !c.hidden())
                    .map(|c| c.name.as_str())
                    .collect();
                self.err(
                    "E014",
                    p.view.span,
                    format!(
                        "the proposal view `{}` does not exist; views: {}",
                        p.view.name,
                        views.join(", ")
                    ),
                );
                continue;
            };
            let Some(action) = env.own_callable(&a.name.name) else {
                continue;
            };
            let args: Vec<String> = match &p.args {
                Some(names) => names.iter().map(|n| n.name.clone()).collect(),
                None => action.params.iter().map(|x| x.name.clone()).collect(),
            };
            let mut types = Vec::new();
            for name in &args {
                match action.params.iter().find(|x| &x.name == name) {
                    Some(x) => types.push(x.ty.clone()),
                    None => self.err(
                        "E014",
                        p.view.span,
                        format!("`{name}` is not a parameter of action `{}`", a.name.name),
                    ),
                }
            }
            let ok = types.len() == view.params.len()
                && types.iter().zip(&view.params).all(|(t, v)| t.same(&v.ty));
            if !ok {
                self.err("E020", p.view.span, format!("the proposal view must take the action's arguments; `{}` is {}, the action passes ({})", p.view.name, view.signature(&env.app), args.join(", ")));
            }
        }
    }

    /// Two actions offered on one kind of item that both ask for a value of the same type and
    /// write it to the same field are one command under two names: the item's menu offers both,
    /// and a person cannot tell which to pick. One goes, or the pair is one action with a
    /// choice of its own.
    fn check_one_command(&mut self) {
        let offers = std::mem::take(&mut self.offers);
        let mut found: Vec<(Span, String)> = Vec::new();
        for (index, (name, span, writes)) in offers.iter().enumerate() {
            for (other, _, theirs) in offers.iter().take(index) {
                for (kind, field, ty) in writes {
                    let both_write = theirs.iter().any(|(theirs_kind, theirs_field, theirs_ty)| {
                        theirs_kind == kind && theirs_field == field && theirs_ty.same(ty)
                    });
                    if !both_write {
                        continue;
                    }
                    let (Some(subject), Some(theirs_subject)) =
                        (self.takes(name, ty), self.takes(other, ty))
                    else {
                        continue;
                    };
                    if !subject.same(theirs_subject) {
                        continue;
                    }
                    let shown = ty.display(&self.env.app);
                    found.push((
                        *span,
                        format!(
                            "`{name}` and `{other}` both ask for a {shown} and write it to `{field}`: \
                             one command under two names. Keep one, or make them one action that \
                             asks for it once"
                        ),
                    ));
                }
            }
        }
        for (span, said) in found {
            self.err("E120", span, said);
        }
    }

    /// What the action `name` is offered on, when it also takes a value of `ty` to write: a
    /// parameter of that type that is not a place a view supplies.
    fn takes(&self, name: &str, ty: &Ty) -> Option<&Ty> {
        let c = self.env.own_callable(name)?;
        if !c.params.iter().any(|p| !p.place && p.ty.same(ty)) {
            return None;
        }
        c.params.first().map(|p| &p.ty)
    }
}
