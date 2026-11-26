//! The checker: types, merge rules, what each kind of declaration may do, and lambdas. It lowers
//! well-typed code to the IR in the same walk, so a program that passes is already in the form the
//! backend consumes. Every error found is reported, not only the first.

mod agentq;
mod boundary;
mod call;
mod component;
mod enums;
mod expr;
mod held;
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
use crate::resolve::{Access, CKind, Callable, Env, ParamInfo, resolve_type_expr};
use crate::types::{MergeKind, Ty, TypeKey};
use std::collections::{BTreeSet, HashMap};

type Ex = ir::Expr;

/// How many people a rich input's `@` drawer offers from the conversation, unless a view asks for
/// another number (`speakers(items, max)`).
pub(crate) const DRAWER_SPEAKERS: i64 = 5;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Ctx {
    #[default]
    Fn,
    Action,
    Program,
    View,
    Block,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Written {
    #[default]
    Declared,
    Scenario,
    Program,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Invoker {
    #[default]
    None,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Invoked {
    #[default]
    Plain,
    Try,
}

#[derive(Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each says one independent thing about the code being checked"
)]
struct FnState {
    ctx: Ctx,
    decl: String,
    uses: std::collections::BTreeSet<crate::effects::Effect>,
    /// The effects already reported as undeclared here: one message per missing word.
    reported: std::collections::BTreeSet<crate::effects::Effect>,
    locals: Vec<Ty>,
    scopes: Vec<HashMap<String, LocalId>>,
    sinks: Vec<Vec<Stmt>>,
    result: Ty,
    facts: Facts,
    view_id: String,
    /// The declaration whose message the words this code shows are attributes of, where it is not
    /// [`Self::view_id`].
    words_of: Option<String>,
    /// What each `let` bound, as it was written, so a prop of words marks the literals inside it
    /// ([`words::Checker::mark_shown`]).
    lets: HashMap<LocalId, ast::Expr>,
    /// The `let`s the words pass has followed into their written value here. A `let` whose value
    /// names the binding it makes (`let label = if let label = ... { label }`) would be followed
    /// forever: its inner name is the same binding, whose value is followed again.
    followed_lets: std::collections::HashSet<LocalId>,
    /// Locals holding the key suffix of each enclosing UI loop, innermost last.
    keys: Vec<LocalId>,
    /// What the `invoke` call being checked is: the operand of `try`, which reports failure rather
    /// than trapping.
    invoked: Invoked,
    /// Set while checking the operand of `suggest`: `Some(true)` when it is the value an agent block
    /// proposes, `Some(false)` for a statement recording a proposal.
    suggest: Option<bool>,
    /// Where a `suggest` stood: a block proposes only as its whole expression, and says so where it
    /// stood elsewhere.
    suggested_at: Option<crate::diag::Span>,
    proposes: bool,
    states: HashMap<String, StateRef>,
    written: Written,
    /// The fields the code writes from a parameter: what two actions offered on one kind of item
    /// must not both do.
    writes: Vec<(String, String, Ty)>,
    /// The locals bound to the parameters: a write is recorded only when the value written is one of
    /// them.
    params: Vec<LocalId>,
    /// Whether the callable takes a resource: an outside effect is queued on the first one.
    takes_resource: bool,
    role: Option<String>,
    /// Who `invoker` is outside an action: the person looking, who would invoke it.
    invoker: Invoker,
    ui: UiPlace,
    /// The arms of the branches the view being lowered stands in, innermost last: a key in one arm
    /// of an `if` or a `match` is not drawn beside a key in another.
    arms: Vec<(u32, usize)>,
    /// How many branch groups the declaration made.
    branches: u32,
    /// For a view made of a loop body, the declared view it was written in.
    owner_view: Option<String>,
    fors: usize,
    row_views: usize,
    syms: HashMap<LocalId, outcomes::Sym>,
    /// The parameters of the templates being expanded that are not values: view state, lambdas and
    /// blocks of the caller ([`template::Macro`]).
    macros: HashMap<String, template::Macro>,
    /// The package whose names the code sees: `None` for the module's own.
    ns: Option<String>,
    types: HashMap<String, Ty>,
    /// What node keys start with after the view's id: the templates being expanded, by call.
    prefix: String,
    /// The templates being expanded, outermost first.
    expanding: Vec<Expanding>,
    /// The resource the view shows, when its first parameter is one: what the system's parts of its
    /// controls are about (a rich input's `@` drawer).
    target: Option<(LocalId, Ty)>,
    /// The locals holding a scalar an event carries (`at` of `on drop(moved: T, at: Int)`), boxed
    /// until the event puts the value there.
    event_scalars: std::collections::HashSet<LocalId>,
    /// The locals holding what an event carries, scalar or not: an argument of `set` computed from
    /// one (`set(expr, x.expr)`) runs when the event fires, with the value in place.
    event_values: std::collections::HashSet<LocalId>,
    /// Whether the code computes one argument of an action a view applies, which runs when that
    /// action is invoked: it may read the clock, and what it reads is recorded there.
    press: bool,
    /// Whether the expression is the action `file.choose` runs, whose file fills the parameter the
    /// action leaves out.
    choosing: bool,
    /// The resource the call of an action at the span is made from (`ask(a).made_from(m)` in an
    /// action): the call it names runs as a change of its own, made from it.
    made_from: Option<(Span, Ex)>,
    /// What the action call just checked answers, when it is an action a view applies: the type a
    /// handler's binding takes ([`Checker::handler_block`]).
    last_action: Option<Ty>,
    /// The locals holding a record value — one built with its fields, read from text, or taken from
    /// a list of them — rather than a resource the store keeps: a write to one edits the value,
    /// never the store.
    values: std::collections::HashSet<LocalId>,
    /// The locals holding a `List` whose items are such record values: what `first` and a loop read
    /// out of it is a value too.
    value_lists: std::collections::HashSet<LocalId>,
}

/// The name a call names its action by, as source wrote it (`add_box`, or `notes.add`), for the
/// words a handler's binding is refused with.
fn called(e: &ast::Expr) -> String {
    let ast::ExprKind::Call { callee, .. } = &e.kind else {
        return "this action".to_owned();
    };
    match &callee.kind {
        ast::ExprKind::Ident(name) => name.clone(),
        ast::ExprKind::Field { base, name } => match &base.kind {
            ast::ExprKind::Ident(ns) => format!("{ns}.{}", name.name),
            _ => name.name.clone(),
        },
        _ => "this action".to_owned(),
    }
}

/// Refuse initial content that makes itself again: a type whose initial action inserts a
/// resource whose own initial content, at some depth, inserts one of the first type, so making
/// one would never end. `initials` holds each initial action's type, name and inserted types.
fn initial_cycle(
    diags: &mut Vec<Diagnostic>,
    initials: &[(TypeKey, ast::Ident, BTreeSet<TypeKey>)],
) {
    for (start, name, _) in initials {
        let mut path: Vec<&TypeKey> = vec![start];
        let mut seen: BTreeSet<&TypeKey> = BTreeSet::new();
        let mut found = None;
        // Depth first from the type, along what each initial action inserts.
        let mut stack: Vec<(usize, &TypeKey)> = vec![(0, start)];
        while let Some((depth, at)) = stack.pop() {
            path.truncate(depth);
            path.push(at);
            let Some((_, _, inserts)) = initials.iter().find(|(ty, _, _)| ty == at) else {
                continue;
            };
            for next in inserts {
                if next == start {
                    found = Some(path.clone());
                    break;
                }
                if seen.insert(next) {
                    stack.push((depth.saturating_add(1), next));
                }
            }
            if found.is_some() {
                break;
            }
        }
        let Some(path) = found else { continue };
        let starts: Vec<String> = path
            .iter()
            .skip(1)
            .chain(std::iter::once(&start))
            .map(|key| format!("`{}`", key.name))
            .collect();
        diags.push(Diagnostic::new(
            "E143",
            name.span,
            format!(
                "making a `{}` would never end: it starts with a {}. Insert the inner ones from an action a person invokes instead",
                start.name,
                starts.join(", which starts with a ")
            ),
        ));
    }
}

/// Where the UI being lowered stands, saved and restored around each block.
#[derive(Clone, Copy, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each says one independent thing about where the UI stands"
)]
pub(crate) struct UiPlace {
    /// The primitive whose block is being lowered, empty at a view's top: `sidebar` and `inspector`
    /// sit directly in a frame, a fact directly in `facts`.
    parent: &'static str,
    /// Whether a list row `row(title, ...)` may stand here: in a list's item block, as a row view's
    /// body, or as the whole body of a view of the item it draws.
    list_row_ok: bool,
    /// Whether it is inside a `card`: a list there is the card's preview
    /// ([`crate::prims::PREVIEW_ROWS`]).
    in_card: bool,
    /// Whether it is in a named slot of a `ui.*` component: a loop there makes parts of the
    /// component (a menu's commands), so it lowers in place.
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

/// A template being expanded: the package whose names its body sees, its name, and the id its
/// package declares it by (none for another package's template).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Expanding {
    pub ns: Option<String>,
    pub name: String,
    pub id: Option<String>,
}

/// One control's `key:`: the view it stands in, the key, where it is written, the action call its
/// press runs as it writes it (`copy_element(e)`) when it runs one, and the branch arms it stands
/// in.
type ControlKey = (String, String, Span, Option<String>, Vec<(u32, usize)>);

/// One key already seen in a view, with the branch arms it stands in and the action call the
/// control pressing it runs, when it runs one: two controls pressing one action the same way
/// share a key.
type SeenKey = (String, String, Vec<(u32, usize)>, Option<String>);

pub(crate) struct Checker<'a> {
    env: &'a Env,
    diags: &'a mut Vec<Diagnostic>,
    st: FnState,
    /// The types of what the declaration being checked inserts into a collection: what an initial
    /// action makes, each with its own initial content ([`initial_cycle`]).
    inserted: BTreeSet<TypeKey>,
    /// Views made of UI loop bodies, in the order made; their functions follow the declared ones,
    /// from `synth_base` on.
    synth: Vec<(ir::Func, ir::SynthView)>,
    synth_base: usize,
    /// The view state templates declared where they expanded, by the view's id.
    states: Vec<(String, crate::resolve::StateInfo)>,
    /// The arguments computed from view state ([`ir::AtEventFn`]); their functions follow the row
    /// views'.
    at_event: Vec<(ir::Func, ir::AtEventFn)>,
    /// What each view answers a request to show an item with ([`ir::ViewShow`]); their functions
    /// follow the arguments.
    shows: Vec<(ir::Func, ir::ViewShow)>,
    /// The places each view's window can be opened at ([`ir::ViewPlace`]); their functions follow
    /// the shows'.
    places: Vec<(ir::Func, ir::ViewPlace)>,
    /// How many handlers' steps after a binding were made: each is a function of its own, named by
    /// the view's id and this count ([`Checker::answer_steps`]).
    answer_n: usize,
    words: Vec<crate::meta::WordMeta>,
    /// What each offered action writes, with where it is declared: read once the bodies are checked,
    /// to find one command written twice.
    offers: Vec<Offer>,
    /// What the body checked last writes: `end` takes the state it was checked in, so the fields are
    /// kept here for the module to read.
    last_writes: Vec<FieldWritten>,
    /// Where the declaration the next function is lowered from starts: `end` takes it, so a function
    /// made in the middle of another's check is named by its own declaration.
    at: Option<usize>,
    /// Where the declaration being checked starts: what a function it makes in passing is named by.
    outer_at: Option<usize>,
    /// Whether the declaration being checked is a `migration action`: only a migration gives an
    /// item it inserts an id of its own.
    migrating: bool,
    /// The string literals the module shows as words, by span.
    shown: std::collections::HashSet<Span>,
    /// The functions of this package whose result a view shows, by index: what their bodies return
    /// is words the view shows too, each a message of that function's own declaration.
    shown_fns: std::collections::HashSet<usize>,
    /// Whether the function being checked is one whose result a view shows: what it returns is
    /// words the view shows, so its literals are its own declaration's messages.
    showing_result: bool,
    /// The views a capture or a stream leaves out, by id: those that declare `protect capture`,
    /// those a protected template expanded in, and those showing one of either
    /// ([`Self::view_edges`]).
    protected: BTreeSet<String>,
    /// (the view showing, the view it shows) for each view a view embeds.
    view_edges: Vec<(String, String)>,
    /// The module's functions whose body is one expression, by name: their parameters and that
    /// expression, which a lambda the store answers reads in place of a call to one
    /// ([`crate::search::lambda_pred_with`]).
    answers: std::collections::HashMap<String, (Vec<String>, ast::Expr)>,
    /// Each control's `key:` and where it is written, by the id of the view it stands in, with the
    /// action its press runs when it runs one and the branch arms it stands in: a view's controls
    /// answer the keys while it is shown, so two of them in one view, or a view-state control that
    /// an action of the window already answers, is a conflict (E090) — unless the two stand in
    /// branches no view draws together.
    control_keys: Vec<ControlKey>,
    /// The `key:` a control just read, with where it stands: what its block's press, lowered after
    /// its arguments, is matched to ([`Self::key_presses`]).
    last_shortcut: Option<(String, Span)>,
}

/// The functions of `module` whose body is one expression, by name: what a predicate reads in
/// place of a call to one.
fn answers(module: &ast::Module) -> std::collections::HashMap<String, (Vec<String>, ast::Expr)> {
    module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Fn(f) => match f.body.stmts.as_slice() {
                [
                    ast::Stmt {
                        kind: ast::StmtKind::Expr(answer),
                        ..
                    },
                ] => Some((
                    f.name.name.clone(),
                    (
                        f.params
                            .iter()
                            .map(|param| param.name.name.clone())
                            .collect(),
                        answer.clone(),
                    ),
                )),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

type FieldWritten = (String, String, Ty);

type Offer = (String, Span, Vec<FieldWritten>);

/// A condition an action keeps: which kind it is, the export that publishes it, and the lines that
/// make it.
type Condition<'a> = (FuncKind, fn(&str) -> String, &'a [ast::Require]);

pub struct BlockResult {
    pub module: ir::Module,
    pub ty: Ty,
    pub suggested: bool,
}

/// Checks every body of the module and lowers it. Errors go to `diags`.
pub fn check_module(env: &Env, module: &ast::Module, diags: &mut Vec<Diagnostic>) -> ir::Module {
    // Which literals are words is known before they are lowered, so the pass that says it runs first,
    // its functions and diagnostics thrown away, and the pass that lowers writes them as messages.
    let mut quiet = Vec::new();
    let shown = lower(
        env,
        module,
        &mut quiet,
        std::collections::HashSet::new(),
        true,
    )
    .1;
    lower(env, module, diags, shown, false).0
}

fn lower(
    env: &Env,
    module: &ast::Module,
    diags: &mut Vec<Diagnostic>,
    shown: std::collections::HashSet<Span>,
    collect: bool,
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
        inserted: BTreeSet::new(),
        synth: Vec::new(),
        synth_base: declared,
        states: Vec::new(),
        at_event: Vec::new(),
        shows: Vec::new(),
        places: Vec::new(),
        answer_n: 0,
        words: Vec::new(),
        offers: Vec::new(),
        last_writes: Vec::new(),
        at: None,
        outer_at: None,
        migrating: false,
        shown,
        shown_fns: std::collections::HashSet::new(),
        showing_result: false,
        protected: BTreeSet::new(),
        view_edges: Vec::new(),
        answers: answers(module),
        control_keys: Vec::new(),
        last_shortcut: None,
    };
    // A view's `protect capture`, before any body is read: what it shows may be left out of a
    // capture wherever it is.
    for item in &module.items {
        if let Item::View(v) = item {
            c.check_protect(v);
        }
    }
    let mut funcs = Vec::new();
    let mut declaring: Vec<(usize, Span)> = Vec::new();
    // The function of each action the module declares, by its declaration's node.
    let mut action_funcs: std::collections::HashMap<ast::NodeId, usize> =
        std::collections::HashMap::new();
    let mut initials = Vec::new();
    // The functions the pass that says which literals are words checked, by index: one whose result
    // a view shows is checked again, with what it returns marked as shown ([`Self::mark_shown`]).
    let mut fn_bodies: Vec<(usize, crate::resolve::Callable, ast::Block)> = Vec::new();
    for item in &module.items {
        let idx = funcs.len();
        let Some(callable) = env.callables.iter().find(|x| x.func == Some(idx)).cloned() else {
            continue;
        };
        c.at = Some(item.span().start);
        if let Item::Action(a) = item {
            declaring.push((idx, a.name.span));
            action_funcs.insert(a.node, idx);
        }
        c.migrating = matches!(
            item,
            Item::Action(a) if matches!(a.role, Some((ast::ActionRole::Migration, _)))
        );
        let f = match item {
            Item::Action(a) => c.run_action(&callable, &a.requires, &a.applies, &a.body),
            Item::Fn(f) => {
                if collect {
                    fn_bodies.push((idx, callable.clone(), f.body.clone()));
                }
                c.run_body(&callable, FuncKind::Fn, Ctx::Fn, &f.body)
            }
            Item::Scenario(s) => c.run_body(&callable, FuncKind::Action, Ctx::Action, &s.body),
            Item::View(v) if is_template(v) => continue,
            Item::View(v) => c.run_view(&callable, v),
            Item::Trigger(_) => continue,
            Item::Use(_)
            | Item::Type(_)
            | Item::Alias(_)
            | Item::Enum(_)
            | Item::Root(_)
            | Item::DefaultView(_)
            | Item::RowView(_)
            | Item::PlaceView(_)
            | Item::Views(_) => {
                continue;
            }
        };
        funcs.push(f);
        let inserted = std::mem::take(&mut c.inserted);
        if let Item::Action(a) = item
            && let Some((ast::ActionRole::Initial, _)) = a.role
            && let Some(Ty::Record(target)) = callable.params.first().map(|p| &p.ty)
        {
            initials.push((target.clone(), a.name.clone(), inserted));
        }
        if let Item::Action(a) = item
            && !matches!(
                a.role.as_ref().map(|(role, _)| *role),
                Some(
                    ast::ActionRole::Migration | ast::ActionRole::Initial | ast::ActionRole::Opened
                )
            )
            && !c.last_writes.is_empty()
        {
            let writes = std::mem::take(&mut c.last_writes);
            c.offers.push((a.name.name.clone(), a.name.span, writes));
        }
    }
    // A `fn` whose result a view shows answers words the view shows: the literals its body returns
    // are messages of that fn's own declaration. Which of them a view shows is known as each prop
    // is read, so the fns it names are checked again once every body has been, until no fn newly
    // shown calls another. What this pass makes is thrown away: the pass that lowers has it as the
    // spans it marks.
    if collect {
        let mut marked: std::collections::HashSet<usize> = std::collections::HashSet::new();
        while let Some((idx, callable, body)) = fn_bodies
            .iter()
            .find(|(idx, _, _)| c.shown_fns.contains(idx) && !marked.contains(idx))
            .cloned()
        {
            marked.insert(idx);
            c.at = Some(body.span.start);
            c.showing_result = true;
            c.run_body(&callable, FuncKind::Fn, Ctx::Fn, &body);
            c.showing_result = false;
        }
    }
    // The presentation views of actions, in the order the resolver numbered them.
    for item in &module.items {
        let Item::Action(a) = item else { continue };
        let idx = funcs.len();
        let Some(view) = env.callables.iter().find(|x| x.func == Some(idx)).cloned() else {
            continue;
        };
        if view.name != crate::ids::present_key(&crate::ids::action_key(module, a)) {
            continue;
        }
        let of = action_funcs.get(&a.node).copied();
        let Some(action) = env
            .callables
            .iter()
            .find(|x| x.own && x.func.is_some() && x.func == of)
        else {
            continue;
        };
        c.at = Some(a.name.span.start);
        funcs.push(c.run_present(&view, &a.opts, action));
    }
    for d in &env.defaults {
        if funcs.len() != d.func {
            break;
        }
        let written = module.items.iter().find_map(|item| match item {
            Item::Action(a) if action_funcs.get(&a.node) == Some(&d.of) => {
                a.params.get(d.param)?.default.as_ref()
            }
            _ => None,
        });
        let action = env.callables.iter().find(|x| x.own && x.func == Some(d.of));
        let (Some(written), Some(action)) = (written, action) else {
            break;
        };
        c.at = Some(written.span.start);
        funcs.push(c.run_default(action, d.param, written));
    }
    initial_cycle(c.diags, &initials);
    c.check_held_roots(module);
    c.check_proposals(module);
    c.check_one_command();
    let mut views = Vec::new();
    let mut at_event = Vec::new();
    let mut shows = Vec::new();
    let mut places = Vec::new();
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
        for (f, mut place) in std::mem::take(&mut c.places) {
            place.func = funcs.len();
            funcs.push(f);
            places.push(place);
        }
    } else if c.diags.is_empty() {
        c.err(
            "E000",
            Span::default(),
            "internal: the declared functions and their indexes disagree",
        );
    }
    // The guard and the `applies` conditions of each action that keeps one its target and the place
    // it is offered in alone decide.
    for item in &module.items {
        let Item::Action(a) = item else { continue };
        let Some(action) = env.own_action(a) else {
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
    // What each action reads through the fns and actions it calls, against what its `uses` clause
    // declares: reported at its name, since its own reads are reported where they are written.
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
    c.check_shortcuts();
    let capture_protected = c.protected_closure();
    let view_calls = c.view_edges;
    let module = ir::Module {
        funcs,
        views,
        states: c.states,
        at_event,
        shows,
        places,
        words: c.words,
        capture_protected,
        view_calls,
    };
    let shown = c.shown;
    // Every prop a view gives a described component is one that component declares.
    if diags.is_empty() {
        props::undeclared(&module, diags);
    }
    (module, shown)
}

pub fn check_block_expr(env: &Env, e: &ast::Expr, diags: &mut Vec<Diagnostic>) -> BlockResult {
    let mut c = Checker {
        env,
        diags,
        st: FnState::default(),
        inserted: BTreeSet::new(),
        synth: Vec::new(),
        synth_base: 0,
        states: Vec::new(),
        at_event: Vec::new(),
        shows: Vec::new(),
        places: Vec::new(),
        answer_n: 0,
        words: Vec::new(),
        protected: BTreeSet::new(),
        view_edges: Vec::new(),
        offers: Vec::new(),
        last_writes: Vec::new(),
        at: None,
        outer_at: None,
        migrating: false,
        shown: std::collections::HashSet::new(),
        shown_fns: std::collections::HashSet::new(),
        showing_result: false,
        answers: std::collections::HashMap::new(),
        control_keys: Vec::new(),
        last_shortcut: None,
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
            places: Vec::new(),
            words: Vec::new(),
            capture_protected: Vec::new(),
            view_calls: Vec::new(),
        },
        ty,
        suggested: proposal,
    }
}

pub fn check_block_ui(env: &Env, body: &[ast::UiStmt], diags: &mut Vec<Diagnostic>) -> BlockResult {
    let mut c = Checker {
        env,
        diags,
        st: FnState::default(),
        inserted: BTreeSet::new(),
        synth: Vec::new(),
        synth_base: 0,
        states: Vec::new(),
        at_event: Vec::new(),
        shows: Vec::new(),
        places: Vec::new(),
        answer_n: 0,
        words: Vec::new(),
        protected: BTreeSet::new(),
        view_edges: Vec::new(),
        offers: Vec::new(),
        last_writes: Vec::new(),
        at: None,
        outer_at: None,
        migrating: false,
        shown: std::collections::HashSet::new(),
        shown_fns: std::collections::HashSet::new(),
        showing_result: false,
        answers: std::collections::HashMap::new(),
        control_keys: Vec::new(),
        last_shortcut: None,
    };
    c.begin(Ctx::Block, &[], Ty::View, "block");
    c.view_body(body);
    c.check_shortcuts();
    let f = c.end(FuncKind::Block, "b_main", "b_main");
    BlockResult {
        module: ir::Module {
            funcs: vec![f],
            views: Vec::new(),
            states: Vec::new(),
            at_event: Vec::new(),
            shows: Vec::new(),
            places: Vec::new(),
            words: Vec::new(),
            capture_protected: Vec::new(),
            view_calls: Vec::new(),
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
        // A choice is its variant's number in wasm, as an `Int` is.
        Ty::Int | Ty::Duration | Ty::Choice(_) => (i, None),
        Ty::Float => (f, None),
        Ty::Bool => (i, Some(Conv::IntToBool)),
        _ => (h, None),
    }
}

/// How a typed value is passed to a host store, and converted.
pub(crate) fn store_op(ty: &Ty, i: Imp, f: Imp, h: Imp) -> (Imp, Option<Conv>) {
    match ty {
        Ty::Int | Ty::Duration | Ty::Choice(_) => (i, None),
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

/// Whether a view can draw both places: no branch group they share puts them in different arms, so
/// an `if`'s two sides, or a `match`'s two arms, never answer the same press together.
fn arms_together(a: &[(u32, usize)], b: &[(u32, usize)]) -> bool {
    a.iter().all(|(group, arm)| {
        b.iter()
            .find(|(held, _)| held == group)
            .is_none_or(|(_, other)| other == arm)
    })
}

impl Checker<'_> {
    fn err(&mut self, code: &str, span: Span, msg: impl Into<String>) {
        self.diags.push(Diagnostic::new(code, span, msg));
    }

    /// A control's `key:`: the shortcut its press answers while the view that writes it is shown.
    /// A key written as text is checked with the parser an action's `key:` uses and kept in one
    /// spelling for comparison, so two spellings of one press are one key; the control's prop keeps
    /// the key as the view wrote it, so a menu row shows `Ctrl+S` as the app declared it. `true`
    /// says it is handled; one computed where the view runs (`false`) is left to the caller, which
    /// types it as the prop it is. Where the key is written is kept for
    /// [`Self::check_shortcuts`], since a key answers a view, not a file.
    fn shortcut_prop(&mut self, n: LocalId, name: &str, value: &ast::Expr) -> bool {
        self.last_shortcut = None;
        let Some(text) = crate::resolve::plain_text(value) else {
            return false;
        };
        match crate::resolve::shortcut_key(&text) {
            Ok((key, shown)) => {
                self.control_keys.push((
                    self.st.view_id.clone(),
                    key.clone(),
                    value.span,
                    None,
                    self.st.arms.clone(),
                ));
                self.last_shortcut = Some((key, value.span));
                self.prop(Ex::local(n, Ty::View), name, Ex::str(&shown), value.span);
            }
            Err(said) => {
                self.err("E090", value.span, said);
                let _ = self.expr(value, None);
            }
        }
        true
    }

    /// What a control whose `key:` was just read presses: the first action of the app its handler
    /// calls, with the arguments it gives it, as it writes the call (`copy_element(e)`). A control
    /// that runs an action shows a command's shortcut, so a key an action already answers is its
    /// own, and two controls pressing one action the same way share a key: they mean one thing. A
    /// control whose press only sets view state has no such claim, and shadowing an action's key is
    /// ambiguous.
    fn key_presses(&mut self, key: &str, span: Span, press: Option<&ast::Expr>) {
        let action = press.and_then(|press| {
            let names: Vec<&str> = self
                .env
                .callables
                .iter()
                .filter(|c| c.own && c.kind == CKind::Action)
                .map(|c| c.name.as_str())
                .collect();
            native::action_call(press, &names).map(crate::print::expr_source)
        });
        if let Some(record) = self
            .control_keys
            .iter_mut()
            .find(|(_, held, at, _, _)| held == key && *at == span)
        {
            record.3 = action;
        }
    }

    /// One key one meaning: two controls of a view cannot answer different presses, and a control
    /// whose press only sets view state cannot answer one an action of the window already does. Two
    /// controls pressing one action the same way (`copy_element(e)` on a menu row and on a toolbar
    /// button) mean one thing, so one key serves them both. A control that runs an action shows that
    /// command's shortcut, as a menu row does. Reporting it here, where every view has been lowered,
    /// keeps the check in one place however a view was expanded.
    fn check_shortcuts(&mut self) {
        let action_keys: Vec<(String, String)> = self
            .env
            .callables
            .iter()
            .filter(|c| c.own && c.kind == CKind::Action)
            .filter_map(|c| c.present.key.clone().map(|key| (key, c.name.clone())))
            .collect();
        let mut seen: Vec<SeenKey> = Vec::new();
        for (view, key, span, presses, arms) in self.control_keys.clone() {
            if let Some((_, action)) = action_keys.iter().find(|(held, _)| *held == key)
                && presses.is_none()
            {
                self.err(
                    "E090",
                    span,
                    format!(
                        "`{key}` is already the action `{action}`'s key: a control that only changes view state cannot answer the same press"
                    ),
                );
                continue;
            }
            let held = seen.iter().find(|(held_view, held, held_arms, _)| {
                *held_view == view && *held == key && arms_together(held_arms, &arms)
            });
            if let Some((_, _, _, held_press)) = held {
                if held_press.is_some() && held_press == &presses {
                    continue;
                }
                self.err(
                    "E090",
                    span,
                    format!(
                        "`{key}` is already a control's key in this view: two controls cannot answer different presses on one key"
                    ),
                );
                continue;
            }
            seen.push((view, key, arms, presses));
        }
    }

    /// An action's write of `field` of a record of `key`: one that reaches the outside service
    /// its type is kept by makes the action reach outside ([`Self::reaches_origin`]).
    fn writes_field(&mut self, key: &TypeKey, field: &crate::resolve::FieldInfo) {
        if self.reaches_origin(key, field) {
            self.st.facts.outside = true;
        }
    }

    /// Whether writing `field` of a record of `key` changes what an outside service keeps: a
    /// field of the type's origin kind that people change, which the service is told, or a
    /// collection of records an origin keeps, whose insert or removal the service is told too.
    /// Such a change is an outside effect whoever makes it: an agent's asks its person, as any
    /// call reaching outside does. A kind its origin derives (the session's layout) stays on the
    /// device, and a scenario's records stand in for the service.
    fn reaches_origin(&self, key: &TypeKey, field: &crate::resolve::FieldInfo) -> bool {
        if self.st.written == Written::Scenario {
            return false;
        }
        let kept = |key: &TypeKey| {
            self.env
                .type_info(key)
                .and_then(|info| info.origin.as_deref())
                .and_then(crate::origin::kind)
                .filter(|kind| !kind.derived)
        };
        if kept(key)
            .and_then(|kind| kind.field(&field.name))
            .is_some_and(|of| of.people)
        {
            return true;
        }
        match &field.ty {
            Ty::Collection(item) | Ty::List(item) => match item.as_ref() {
                Ty::Record(item) => kept(item).is_some(),
                _ => false,
            },
            _ => false,
        }
    }

    fn show(&self, ty: &Ty) -> String {
        ty.display(&self.env.app)
    }

    /// Records that the code reads `effect`, written as `what` (`now`, `day_of(t)`, a time of no
    /// zone, `random.int`), and refuses it where it cannot be read: an action and an agent's program
    /// read only what its `uses` clause names; a view reads none but the zone it is drawn in; a `fn`
    /// and a block pass their reads to their caller.
    fn reads(&mut self, effect: crate::effects::Effect, span: Span, what: &str) {
        let refused = match self.st.ctx {
            Ctx::Action | Ctx::Program => !self.st.uses.contains(&effect),
            // A view reads the clock it is drawn at, and the code a vault item makes now: the
            // one secret it may show, which it keeps out of captures ([`protect capture`]).
            Ctx::View => !matches!(
                effect,
                crate::effects::Effect::Time | crate::effects::Effect::Secret
            ),
            _ => false,
        };
        if !refused {
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
            // A program declares nothing: it reads times through the filters that take them,
            // which the host answers against its clock as it reads.
            Ctx::Program if effect == crate::effects::Effect::Time => format!(
                "{what} reads {what_it_reads}, which a program cannot: it asks by time through filters, written as search writes a time: `find(Note, changed: -7d)` (changed in the last 7 days), `grep(/x/, in: notes, changed: today)`, `find(Task, due: today)`"
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

    /// An action's body, after its requirements: each refuses the invocation with its reason when its
    /// condition does not hold, before anything changes.
    fn run_action(
        &mut self,
        c: &Callable,
        requires: &[ast::Require],
        applies: &[ast::Require],
        body: &ast::Block,
    ) -> ir::Func {
        // What it applies to comes before what it can do: an invocation where it does not apply is
        // refused with the condition's reason.
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
            self.mark_shown(&r.reason);
            let reason = self.expr_as(&r.reason, &Ty::Text);
            let refuse = Stmt::Eval(Ex::host(Imp::Abort, vec![reason], Ty::Unit));
            self.emit(Stmt::If(cond, Vec::new(), vec![refuse]));
        }
        if c.access == Access::Scenario {
            self.st.written = Written::Scenario;
        }
        self.st.role.clone_from(&c.present.role);
        let valued = !matches!(c.result, Ty::Unit | Ty::Error);
        let stmts = match body.stmts.split_last() {
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
                // A fn whose result a view shows returns words the view shows.
                if self.showing_result {
                    self.mark_shown(last);
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
        self.last_writes = std::mem::take(&mut self.st.writes);
        let mut f = self.end(kind, &c.name, &format!("{prefix}{}", c.id));
        f.nparams = c.params.len();
        f
    }

    /// Reads a view's `protect capture` declaration: only `capture` may follow `protect`, and a
    /// view declares it once. The view it protects is recorded by id.
    fn check_protect(&mut self, view: &ast::ViewDecl) {
        let mut declared = false;
        let mut id = None;
        for protect in &view.protect {
            if protect.what.name != "capture" {
                self.err(
                    "E146",
                    protect.what.span,
                    format!(
                        "`protect` takes `capture`, not `{}`: a view that shows a secret or a code declares `protect capture`, and a capture or a stream leaves it out",
                        protect.what.name
                    ),
                );
                continue;
            }
            if declared {
                self.err(
                    "E147",
                    protect.span,
                    "a view declares `protect capture` once",
                );
                continue;
            }
            declared = true;
            id = self.env.own_callable(&view.name.name).map(|c| c.id.clone());
        }
        if let Some(id) = id {
            self.protected.insert(id);
        }
    }

    /// The views a capture or a stream leaves out, by id: each that declares `protect capture`,
    /// each a protected template expanded in, and, since a window showing one view shows its
    /// frame too, each showing another ([`Self::view_edges`]). A row view protects the view it is
    /// a row of.
    fn protected_closure(&self) -> Vec<String> {
        let mut out = self.protected.clone();
        loop {
            let mut grew = false;
            let owners: Vec<String> = out
                .iter()
                .filter_map(|id| id.split_once('~'))
                .map(|(owner, _)| owner.to_owned())
                .collect();
            for owner in owners {
                grew |= out.insert(owner);
            }
            for (caller, callee) in &self.view_edges {
                if out.contains(callee) {
                    grew |= out.insert(caller.clone());
                }
            }
            if !grew {
                break;
            }
        }
        out.into_iter().collect()
    }

    fn run_view(&mut self, c: &Callable, v: &ast::ViewDecl) -> ir::Func {
        self.outer_at = self.at.or(self.outer_at);
        self.begin(Ctx::View, &c.params, Ty::View, &c.id);
        self.st.target = c
            .params
            .first()
            .and_then(|p| self.lookup(&p.name))
            .filter(|(_, ty)| matches!(ty, Ty::Record(_)));
        let rest = self.view_state(&c.name, &v.body);
        // What the view answers when the window is asked to show an item: a function of its own, run
        // with the item, applied in the view's state.
        let (shows, rest) = Self::view_shows(&rest);
        let params: std::collections::BTreeSet<String> =
            c.params.iter().map(|p| p.name.clone()).collect();
        for show in &shows {
            match ast::place_of(show) {
                Some(place) => {
                    self.run_place(c, place, show, &v.body, &params);
                }
                None => self.run_show(&c.id, &c.name, show, &v.body, &params),
            }
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
        } else if self.st.target.is_some() && self.body_is_row(rest) {
            // A view of one item whose body is its row is that item's row wherever it is drawn: the
            // parameters after the item state the place around it.
            self.st.ui.list_row_ok = true;
        }
        self.view_body(rest);
        let mut f = self.end(FuncKind::View, &c.name, &format!("v_{}", c.id));
        f.nparams = c.params.len();
        f
    }

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

    /// Whether `ty` is a record type the store keeps: an action's parameter of it is a resource.
    fn resource_ty(ty: &Ty) -> bool {
        matches!(ty, Ty::Record(key) if !crate::host::is_value(key)) || matches!(ty, Ty::Union(_))
    }

    /// Whether `ex` is a record value rather than a resource: one built with its fields
    /// (`Note(title: "x")`), read from text (`json.decode<Note>(text)`) or taken from a list of
    /// such values. The checker learns this where a value is made and where it is bound, so a local
    /// it tied to one of those holds one.
    fn ex_is_record_value(&self, ex: &Ex) -> bool {
        match &ex.kind {
            Ek::Host(Imp::JsonDecode, _) => true,
            Ek::Local(id) => self.st.values.contains(id),
            // `xs.first()` of a list of values: the option of a value, which an arm binds.
            Ek::Host(Imp::ListEnd, args) => {
                args.first().is_some_and(|list| self.ex_is_value_list(list))
            }
            Ek::Seq(_, v) => self.ex_is_record_value(v),
            _ => false,
        }
    }

    /// Whether `ex` is a `List` whose items are record values, its item taken, not a resource.
    fn ex_is_value_list(&self, ex: &Ex) -> bool {
        match &ex.kind {
            Ek::Host(Imp::JsonDecode, _) => matches!(ex.ty, Ty::List(_)),
            Ek::Local(id) => self.st.value_lists.contains(id),
            Ek::Seq(_, v) => self.ex_is_value_list(v),
            _ => false,
        }
    }

    /// Records whether the local `id`, now holding `ex`, holds a record value or a list of them.
    fn learn_value(&mut self, id: LocalId, ex: &Ex) {
        if Self::resource_ty(&ex.ty) && self.ex_is_record_value(ex) {
            self.st.values.insert(id);
        } else {
            self.st.values.remove(&id);
        }
        let items_are_values = match &ex.ty {
            Ty::List(item) => Self::resource_ty(item) && self.ex_is_value_list(ex),
            _ => false,
        };
        if items_are_values {
            self.st.value_lists.insert(id);
        } else {
            self.st.value_lists.remove(&id);
        }
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

    /// `invoke(act(args), by: mia, at: 2026-06-29T07:02Z, from: msg)` in a scenario: runs what it
    /// names with `invoker` = `by`, `now` = `at` where one is named, and what it creates recorded as
    /// made from `from`. Each invocation is a change of its own. The runtime is handed the
    /// action's name too, for the words it refuses a step with.
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
                "by" => by = Some(self.expr_as(&a.value, &Ty::Actor)),
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
        let action = Ex::str(&called(&first.value));
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
        let by = by.unwrap_or_else(|| Ex::host(Imp::Invoker, Vec::new(), Ty::Actor));
        let opt_from = Ty::Option(Box::new(from.as_ref().map_or(Ty::Error, |f| f.ty.clone())));
        let from = match from {
            Some(f) => Self::some(f, &opt_from),
            None => Ex::host(Imp::NoneV, Vec::new(), opt_from),
        };
        let pop = Stmt::Eval(Ex::host(Imp::InvocationPop, Vec::new(), Ty::Unit));
        let mut out = vec![Stmt::Eval(Ex::host(
            Imp::InvocationPush,
            vec![by, at, from, action],
            Ty::Unit,
        ))];
        out.append(&mut stmts);
        let value = if ty.repr().is_some() {
            let made = self.fresh(ty.clone());
            out.push(Stmt::Set(made, *last));
            out.push(pop);
            Ex::local(made, ty.clone())
        } else {
            out.push(Stmt::Eval(*last));
            out.push(pop);
            Ex::new(Ek::Unit, Ty::Unit)
        };
        Ex::new(Ek::Seq(out, Box::new(value)), ty)
    }

    /// An expression whose value is not used: an `if` runs its branches as statements, so they need
    /// not agree on a type and an `if` needs no `else`.
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

    /// A block whose value is used: its last expression, checked against `exp`. A block that returns
    /// on every path has any type.
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

    /// `if` whose value is used: both branches agree on a type, and one branch alone is the Option of
    /// it (`or_none`).
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
        let ty = want.unwrap_or_else(|| ex.ty.clone());
        self.bind_written(name, ty, ex, value);
    }

    /// Binds the checked value `ex` to `name`, refusing one with no value to hold.
    fn bind_written(&mut self, name: &str, mut ty: Ty, ex: Ex, value: &ast::Expr) {
        let sym = self.sym_of(value);
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

    /// The action a view's handler applies when its event fires. A handler written as a block may
    /// bind the answer an action call gives (`let id = add_box(b)`): the steps after the binding
    /// are their own function, which the runtime runs once the action answers, with the answer
    /// where the name is. An action that is refused or fails answers nothing, and those steps do
    /// not run. Any other handler is one action, as it was.
    pub(super) fn handler(&mut self, e: &ast::Expr) -> Ex {
        match &e.kind {
            ast::ExprKind::Block(b) => self.handler_block(b),
            _ => self.expr_as(e, &Ty::Action),
        }
    }

    fn handler_block(&mut self, b: &ast::Block) -> Ex {
        self.push_sink();
        for (i, s) in b.stmts.iter().enumerate() {
            let last = b.stmts.get(i + 1).is_none();
            if last && let ast::StmtKind::Expr(expr) = &s.kind {
                let act = self.expr_as(expr, &Ty::Action);
                let stmts = self.pop_sink();
                return Ex::new(Ek::Seq(stmts, Box::new(act)), Ty::Action);
            }
            let ast::StmtKind::Let { name, ty, value } = &s.kind else {
                self.stmt(s);
                continue;
            };
            if ty.is_some() {
                self.let_stmt(&name.name, ty.as_ref(), value);
                continue;
            }
            self.st.last_action = None;
            let answer = self.expr(value, None);
            let declared = self.st.last_action.take();
            if answer.ty != Ty::Action {
                let ty = answer.ty.clone();
                self.bind_written(&name.name, ty, answer, value);
                continue;
            }
            let declared = declared.filter(|ty| !ty.is_error());
            let bound = match declared {
                Some(ty) if ty != Ty::Unit => Some(ty),
                other => {
                    if other.is_some() {
                        self.err(
                            "E148",
                            value.span,
                            format!(
                                "`{}` answers nothing, so `{}` has no answer to hold: an action a handler binds says what it answers, as `action {}(...) -> Text`",
                                called(value),
                                name.name,
                                called(value),
                            ),
                        );
                    } else {
                        self.err(
                            "E148",
                            value.span,
                            format!(
                                "`{}` takes the answer of an action's call, and this is an action value, not one: bind `act(args)` for an action that answers a value",
                                name.name,
                            ),
                        );
                    }
                    None
                }
            };
            let Some(answer_ty) = bound else {
                self.bind_written(&name.name, Ty::Error, answer, value);
                continue;
            };
            let (id, captures) =
                self.answer_steps(&name.name, &answer_ty, &b.stmts[i + 1..], s.span);
            let act = Ex::host(
                Imp::ActAnswer,
                vec![answer, Ex::str(&id), captures],
                Ty::Action,
            );
            let stmts = self.pop_sink();
            return Ex::new(Ek::Seq(stmts, Box::new(act)), Ty::Action);
        }
        self.err(
            "E063",
            b.span,
            "a handler ends with the action it applies: add its last step, such as `set(editing, id)`",
        );
        let _ = self.pop_sink();
        Ex::error()
    }

    /// The steps of a handler after it binds the action's `answer` to `bind`, as a function of the
    /// answer and of what else they read: a view's state, what the event carries, and the view's
    /// own values. Exported like an argument computed at the event, and the runtime runs it with
    /// the answer once the action gives one. Returns its id and the values it is handed besides
    /// the answer.
    fn answer_steps(
        &mut self,
        bind: &str,
        answer: &Ty,
        rest: &[ast::Stmt],
        span: Span,
    ) -> (String, Ex) {
        let rest = ast::Expr {
            kind: ast::ExprKind::Block(ast::Block {
                stmts: rest.to_vec(),
                span,
            }),
            span,
        };
        let mut names = BTreeSet::new();
        boundary::names_in(&rest, &mut names);
        names.remove(bind);
        let mut params = vec![ParamInfo {
            name: bind.to_owned(),
            ty: answer.clone(),
            default: None,
            place: false,
            label: None,
            entry: false,
            secret: false,
        }];
        let mut captures = Vec::new();
        let mut boxed = Vec::new();
        for name in &names {
            let (ty, arg) = if let Some(state) = self.state_named(name) {
                (state.ty.clone(), Self::state_at(&state))
            } else if let Some((local, ty)) = self.lookup(name) {
                if self.st.event_scalars.contains(&local) {
                    boxed.push(name.clone());
                }
                (ty.clone(), Ex::local(local, ty))
            } else {
                continue;
            };
            params.push(ParamInfo {
                name: name.clone(),
                ty,
                default: None,
                place: false,
                label: None,
                entry: false,
                secret: false,
            });
            captures.push(arg);
        }
        let n = self.answer_n;
        self.answer_n = self.answer_n.saturating_add(1);
        let id = format!("{}~answer{n}", self.st.view_id);
        let outer = std::mem::take(&mut self.st);
        self.begin(Ctx::View, &params, Ty::Action, &outer.view_id);
        self.st.ns.clone_from(&outer.ns);
        self.st.types.clone_from(&outer.types);
        self.st.press = true;
        // The state the steps read or set is the view's: `set(editing, id)` names it as the view
        // does, though the steps run in a frame of their own.
        for name in &names {
            let Some(declared) = outer.states.get(name) else {
                continue;
            };
            let Some((local, ty)) = self.lookup(name) else {
                continue;
            };
            self.st.states.insert(
                name.clone(),
                StateRef {
                    id: declared.id.clone(),
                    ty,
                    local,
                },
            );
        }
        // The scalars an event carries come boxed until it fires; the steps read them plain.
        for name in &boxed {
            if let Some((local, Ty::Option(inner))) = self.lookup(name) {
                let plain = self.declare(name, (*inner).clone());
                self.emit(Stmt::Set(
                    plain,
                    Self::unwrap_opt(Ex::local(local, Ty::Option(inner.clone())), &inner),
                ));
            }
        }
        let value = self.handler(&rest);
        self.emit(Stmt::Return(Some(value)));
        let name = format!("{} after an action's answer", outer.view_id);
        let export = ir::at_event_export(&id);
        self.at = self.outer_at;
        let mut f = self.end(ir::FuncKind::Fn, &name, &export);
        f.nparams = params.len();
        self.st = outer;
        self.at_event.push((
            f,
            ir::AtEventFn {
                func: 0,
                id: id.clone(),
                name,
                params,
                result: Ty::Action,
            },
        ));
        let captures = self.arg_list(captures);
        (id, captures)
    }

    /// Binds the checked value `ex` to the new local `name`; `written` is the value as source wrote
    /// it, kept so a prop of words that shows the binding marks the literals inside it.
    fn bind_let(&mut self, name: &str, ty: Ty, ex: Ex, written: &ast::Expr) -> LocalId {
        let id = self.declare(name, ty);
        self.st.lets.insert(id, written.clone());
        self.learn_value(id, &ex);
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
                    // A fn whose result a view shows returns words the view shows.
                    if self.showing_result {
                        self.mark_shown(e);
                    }
                    let ex = self.expr_as(e, &want);
                    self.emit(Stmt::Return(Some(ex)));
                }
            }
        }
    }

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

    fn new_for(&mut self, list: Ex, elem: &Ty) -> ForList {
        let (get, get_conv) = load_op(elem, Imp::ListGetI, Imp::ListGetF, Imp::ListGetH);
        // A loop over a list of record values binds the item as a value; a loop over a collection
        // binds the resource it holds.
        let item_is_value = Self::resource_ty(elem) && self.ex_is_value_list(&list);
        let fl = ForList {
            list,
            list_local: self.fresh(Ty::Text),
            len_local: self.fresh(Ty::Int),
            idx_local: self.fresh(Ty::Int),
            item_local: self.fresh(elem.clone()),
            get,
            get_conv,
            body: Vec::new(),
        };
        if item_is_value {
            self.st.values.insert(fl.item_local);
        }
        fl
    }

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
        // `x.f = v` on a local: `x` may hold a record value (one built with its fields, read from
        // text or taken out of a list), which the assignment changes as a value. The local takes a
        // copy of its own first, so another name for the same value keeps what it held; a stored
        // resource stays itself, and its field set writes the store. A parameter is no such local:
        // an action's record parameter is always a resource (an invocation refuses a record value
        // there, whose change would land in a copy that is dropped), and a fn's is what its caller
        // handed over, a resource as often as not, which only an action changes.
        let local = match prefix {
            [only] if self.st.written != Written::Program => self
                .lookup(&only.name)
                .filter(|(id, ty)| matches!(ty, Ty::Record(_)) && !self.st.params.contains(id)),
            _ => None,
        };
        let base_ex = match &local {
            Some((id, ty)) => {
                let own = Ex::host(Imp::RecOwn, vec![Ex::local(*id, ty.clone())], ty.clone());
                self.emit(Stmt::Set(*id, own));
                Ex::local(*id, ty.clone())
            }
            None => self.expr(&base, None),
        };
        let base_ty = base_ex.ty.clone();
        self.assign_field(base_ex, last, op, value, span, local.is_some());
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
        if op == ast::AssignOp::Set {
            self.learn_value(id, &new);
        }
        self.emit(Stmt::Set(id, new));
    }

    fn assign_field(
        &mut self,
        base: Ex,
        field: &ast::Ident,
        op: ast::AssignOp,
        value: &ast::Expr,
        span: Span,
        local: bool,
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
        // Outside an action nothing changes the store: a local's field set there edits the record
        // value it holds, which the store holds nothing of until an action puts it in a collection.
        let edits_value =
            local && self.st.ctx != Ctx::Action && self.st.written != Written::Program;
        if let Some(kind) = info.origin.as_deref().and_then(crate::origin::kind)
            && let Some(of) = kind.field(&field.name)
            && !of.people
            && self.st.written != Written::Scenario
            && !edits_value
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
        if edits_value {
            self.edit_value(base, &f, op, value, span);
            return;
        }
        if self.st.ctx != Ctx::Action {
            let who = self.st.ctx.word();
            self.err("E040", span, format!("{who} cannot change `{}.{}`; only an action mutates fields. Write an action and apply it; outside one, only a record value a local holds (one built with its fields, read from text or taken out of a list) has its fields set", key.name, f.name));
            let _ = self.expr(value, None);
            return;
        }
        if key.app != self.env.app {
            self.err("E045", span, format!("`{}` is declared by another app ({}); an action may only change its own module's types. Call that app's action instead", key.name, key.app));
            let _ = self.expr(value, None);
            return;
        }
        // The app's preferences are the settings service's record: each changes through its
        // own setter (`set-<field>`), which a control bound to it runs, so the change is
        // recorded in the settings' history and every device follows it.
        if self.env.prefs.contains(&key) {
            self.err(
                "E045",
                span,
                format!(
                    "`{}.{}` is one of the app's preferences: it changes through its setter, which a control bound to it runs (`bind: p.{}` where `p` is `root {}`), so the settings keep the change and every device follows it",
                    key.name, f.name, f.name, key.name
                ),
            );
            let _ = self.expr(value, None);
            return;
        }
        self.st.facts.mutates = true;
        self.writes_field(&key, &f);
        if f.merge == MergeKind::Structural {
            self.err("E033", span, format!("`{}` is a {} field and cannot be assigned; change it with its methods (`.insert`, `.remove`, `.move`, `.add`, `.put`)", f.name, f.ty.display(&self.env.app)));
            let _ = self.expr(value, None);
            return;
        }
        // A field set from a value the action asks for is what a second action asking the same value
        // of the same type cannot also be for.
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

    /// `p.f = v` outside an action, `p` a local holding a record value: the value's field
    /// changes, as building it set it. `+=` and `-=` add to a number it holds; no merge rule
    /// applies, since no two devices change one value.
    fn edit_value(
        &mut self,
        base: Ex,
        f: &crate::resolve::FieldInfo,
        op: ast::AssignOp,
        value: &ast::Expr,
        span: Span,
    ) {
        if f.merge == MergeKind::Structural {
            self.err("E033", span, format!("`{}` is a {} field and cannot be assigned; a record value starts it empty, and an action changes it with its methods once the value is stored", f.name, f.ty.display(&self.env.app)));
            let _ = self.expr(value, None);
            return;
        }
        let fid = Ex::str(&f.id);
        match op {
            ast::AssignOp::Set => {
                let v = self.expr_as(value, &f.ty);
                let (imp, c) = store_op(&f.ty, Imp::ResSetI, Imp::ResSetF, Imp::ResSetH);
                self.emit(Stmt::Eval(Ex::host(
                    imp,
                    vec![base, fid, conv(c, v, Ty::Int)],
                    Ty::Unit,
                )));
            }
            ast::AssignOp::Add | ast::AssignOp::Sub if matches!(f.ty, Ty::Int | Ty::Float) => {
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
            ast::AssignOp::Add | ast::AssignOp::Sub => {
                let t = self.show(&f.ty);
                self.err("E034", span, format!("`{}` on a {t} field is not defined; `+=` and `-=` add to an Int or a Float. Assign the whole value with `=`", op.text()));
                let _ = self.expr(value, None);
            }
        }
    }

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
            let Some(action) = env.own_action(a) else {
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

    /// Two actions offered on one kind of item that both ask for a value of the same type and write it
    /// to the same field are one command under two names: one goes, or the pair is one action with a
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
                    let (Some(target), Some(theirs_target)) =
                        (self.takes(name, ty), self.takes(other, ty))
                    else {
                        continue;
                    };
                    if !target.same(theirs_target) {
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

    /// What the action `name` is offered on, when it also takes a value of `ty` to write.
    fn takes(&self, name: &str, ty: &Ty) -> Option<&Ty> {
        let c = self.env.own_callable(name)?;
        if !c.params.iter().any(|p| !p.place && p.ty.same(ty)) {
            return None;
        }
        c.params.first().map(|p| &p.ty)
    }
}
