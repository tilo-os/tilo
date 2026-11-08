//! Calls: functions, actions, record construction, built-ins and methods.

mod lists;

use super::expr::dotted;
use super::{Checker, Ctx, Ex, Invoked, Written, conv, store_op};
use crate::abi::Imp;
use crate::ast::{self, ExprKind};
use crate::diag::Span;
use crate::ir::{self, ActionRef, Ek, Stmt};
use crate::prims;
use crate::resolve::{CKind, Callable, FieldInfo, Import};
use crate::types::{Ty, TypeKey};

/// The module a draw is asked of: `random.int(lo, hi)`.
const RANDOM: &str = "random";

impl Checker<'_> {
    pub(super) fn call(
        &mut self,
        callee: &ast::Expr,
        types: &[ast::TypeExpr],
        args: &[ast::Arg],
        span: Span,
    ) -> Ex {
        if !types.is_empty() && !names_json(callee) {
            self.err(
                "E022",
                span,
                "only `json.decode<T>(text)` takes a type between `<` and `>`",
            );
        }
        match &callee.kind {
            ExprKind::Ident(name) => self.call_named(name, callee.span, args, span),
            ExprKind::Field { base, name } => {
                if matches!(&base.kind, ExprKind::Ident(ns) if ns == crate::stdlib::NS)
                    && self.lookup(crate::stdlib::NS).is_none()
                {
                    if name.name == "RichText" {
                        return self.builtin_rich_text(args, span);
                    }
                    if let Some(held) =
                        crate::resolve::std_text_ty(&[crate::stdlib::NS, name.name.as_str()])
                    {
                        return match crate::resolve::std_text_value(&held, args) {
                            Ok(text) => Ex::str(&text).typed(held),
                            Err(msg) => {
                                self.err("E099", span, msg);
                                Ex::error()
                            }
                        };
                    }
                }
                // `random.int(lo, hi)`: a draw, which the invocation records (effects.md).
                if matches!(&base.kind, ExprKind::Ident(ns) if ns == RANDOM)
                    && self.lookup(RANDOM).is_none()
                    && self.env.find_import(RANDOM).is_none()
                {
                    return self.builtin_random(name, args, span);
                }
                // `web.get` and `web.post`: a request to a service outside Tilo, which the
                // invocation makes once and records (effects.md).
                if matches!(&base.kind, ExprKind::Ident(ns) if ns == "web")
                    && self.lookup("web").is_none()
                    && self.env.find_import("web").is_none()
                {
                    return self.builtin_web(name, args, span);
                }
                // `json.parse`, `json.decode<T>` and `json.encode`: reading and writing JSON
                // text, which is no effect: it reaches nothing outside the values it is given.
                if matches!(&base.kind, ExprKind::Ident(ns) if ns == "json")
                    && self.lookup("json").is_none()
                    && self.env.find_import("json").is_none()
                {
                    return self.builtin_json(name, types, args, span);
                }
                if let Some(key) = self.enum_path(base) {
                    let ty = Ty::Enum(key);
                    return match super::enums::Shape::of(self, &ty) {
                        Some(shape) => self.construct_variant(&shape, &ty, name, Some(args), span),
                        None => Ex::error(),
                    };
                }
                if self.names_import(base) {
                    let path = dotted(base).unwrap_or_default().join(".");
                    if let Some(import) = self.env.find_import(&path).cloned() {
                        return self.call_foreign(&import, name, args, span);
                    }
                }
                // `Task.project(p)`: a filter of the type with the value it asks of.
                if let Some(Ty::Record(key)) = self.type_named(base)
                    && let Some(ex) = self.filter_value(&key, name, Some(args), span)
                {
                    return ex;
                }
                self.method(base, name, args, span)
            }
            _ => {
                self.err("E026", callee.span, "only a name or a method can be called");
                Ex::error()
            }
        }
    }

    fn call_named(&mut self, name: &str, name_span: Span, args: &[ast::Arg], span: Span) -> Ex {
        let env = self.env;
        if matches!(
            self.st.macros.get(name),
            Some(super::template::Macro::Lambda(_))
        ) {
            return self.apply_lambda(name, args, span);
        }
        if self.lookup(name).is_some() {
            self.err(
                "E026",
                name_span,
                format!(
                    "`{name}` is a variable, not a function; lambdas cannot be stored or called"
                ),
            );
            return Ex::error();
        }
        if let Some(c) = self.own_callable(name) {
            let c = c.clone();
            return self.call_callable(&c, args, span);
        }
        if let Some(t) = env.own_type(name).filter(|_| !self.in_std()) {
            let key = t.key.clone();
            return self.construct(&key, args);
        }
        if name == "RichText" {
            return self.builtin_rich_text(args, span);
        }
        if matches!(name, "Date" | "Time") {
            return match crate::resolve::date_value(name, args) {
                Ok((ty, text)) => Ex::str(&text).typed(ty),
                Err(msg) => {
                    self.err("E099", span, msg);
                    Ex::error()
                }
            };
        }
        // `Duration(n)`: a length of time of `n` seconds, for one an app keeps as a number.
        if name == "Duration" {
            let [n] = args else {
                self.err(
                    "E022",
                    span,
                    "`Duration` takes one number, its seconds: `Duration(hold)`",
                );
                return Ex::error();
            };
            if n.name.is_some() {
                self.err(
                    "E022",
                    n.value.span,
                    "`Duration` takes its seconds by position: `Duration(hold)`",
                );
            }
            return self.expr_as(&n.value, &Ty::Int).typed(Ty::Duration);
        }
        // A session's code has the agent library's words in scope, its default import.
        if env.session
            && let Some(ex) = self.agent_call(name, args, span)
        {
            return ex;
        }
        match name {
            "outside" => return self.builtin_outside(args, span),
            "withdraw" => return self.builtin_withdraw(args, span),
            "model_call" => return self.builtin_model(args, span),
            "run_block" => return self.builtin_run_block(args, span),
            "set" => return self.builtin_set(args, span),
            "open" => return self.builtin_open(args, span),
            "notify" => return self.builtin_notify(args, span),
            "resource" => return self.builtin_resource(args, span),
            "person" => return self.builtin_person(args, span),
            "agent" => return self.builtin_agent(args, span),
            "actions" => return self.builtin_actions(args, span),
            "lenses" => return self.builtin_lenses(args, span),
            "delivery" => return self.builtin_delivery(args, span),
            "members" => return self.builtin_members(args, span),
            "created" | "updated" | "history" => {
                return self.builtin_versions(name, args, span);
            }
            "changed" => return self.builtin_changed(args, span),
            "unseen" => return self.builtin_unseen(args, span),
            "linked" => return self.builtin_links(args, span),
            "proposal_note" => return self.builtin_proposal_note(args, span),
            "first_unseen" => return self.builtin_first_unseen(args, span),
            "mentions" => return self.builtin_mentions(args, span),
            "undo" => return self.builtin_undo(args, span),
            "steps" => return self.builtin_steps(args, span),
            "changes" => return self.builtin_changes(args, span),
            "meta" => return self.builtin_meta(args, span),
            "cases" => return self.builtin_cases(args, span),
            "phrase" => return self.builtin_phrase(args, span),
            "preview_rows" => return self.builtin_preview_rows(args, span),
            "entry_parts" => return self.builtin_entry_parts(args, span),
            "invoke" => return self.builtin_invoke(args, span),
            "query" => return self.builtin_query(args, span),
            "filters" => return self.builtin_filters(args, span),
            _ => {}
        }
        if prims::prim(name).is_some() {
            self.err("E061", name_span, format!("`{name}` is a UI primitive; it builds UI and can only be written as a statement of a view"));
            return Ex::error();
        }
        let mut names: Vec<&str> = env
            .callables
            .iter()
            .filter(|c| c.own)
            .map(|c| c.name.as_str())
            .collect();
        names.sort_unstable();
        let mut known: Vec<&str> = names.clone();
        known.extend(prims::BUILTINS.iter().copied());
        if self.env.session {
            for word in crate::agent::FUNCTIONS {
                known.push(word.name);
            }
        }
        let hint = crate::diag::suggestion(name, known);
        self.err(
            "E014",
            name_span,
            format!(
                "unknown function `{name}`{hint}; declared here: {}",
                names.join(", ")
            ),
        );
        Ex::error()
    }

    fn call_foreign(
        &mut self,
        import: &Import,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> Ex {
        // The agent library's words are host imports the checker lowers rather than calls into
        // a module: `agent.grep(...)` in a package is the code a session's `grep` is.
        if import.app == crate::agent::APP
            && let Some(ex) = self.agent_call(&name.name, args, span)
        {
            return ex;
        }
        let env = self.env;
        if let Some(c) = env.foreign_callable(&import.app, &name.name) {
            let c = c.clone();
            return self.call_callable(&c, args, span);
        }
        let key = TypeKey {
            app: import.app.clone(),
            name: name.name.clone(),
        };
        if env.type_info(&key).is_some() {
            return self.construct(&key, args);
        }
        let names: Vec<&str> = env
            .callables
            .iter()
            .filter(|c| c.app == import.app)
            .map(|c| c.name.as_str())
            .collect();
        self.err(
            "E014",
            name.span,
            format!(
                "package `{}` has no `{}`{}; it declares: {}",
                import.alias,
                name.name,
                crate::diag::suggestion(&name.name, names.iter().copied()),
                names.join(", ")
            ),
        );
        Ex::error()
    }

    /// Matches arguments to parameters; positional first, then named.
    pub(super) fn bind_args(
        &mut self,
        name: &str,
        params: &[crate::resolve::ParamInfo],
        args: &[ast::Arg],
        span: Span,
        sig: &str,
    ) -> Vec<Ex> {
        self.bind_args_or_default(name, params, args, span, sig, false)
            .into_iter()
            .map(|s| s.unwrap_or_else(Ex::error))
            .collect()
    }

    /// As [`Self::bind_args`]; a parameter with a default may be left out, as `None`. With
    /// `at_press`, the arguments are those of an action a view applies ([`Self::arg_at_press`]).
    pub(super) fn bind_args_or_default(
        &mut self,
        name: &str,
        params: &[crate::resolve::ParamInfo],
        args: &[ast::Arg],
        span: Span,
        sig: &str,
        at_press: bool,
    ) -> Vec<Option<Ex>> {
        let mut slots: Vec<Option<Ex>> = vec![None; params.len()];
        let mut pos = 0usize;
        let mut named = false;
        for a in args {
            let index = match &a.name {
                None => {
                    if named {
                        self.err(
                            "E022",
                            a.value.span,
                            format!(
                                "positional arguments come before named ones in a call of `{name}`"
                            ),
                        );
                    }
                    pos += 1;
                    Some(pos - 1)
                }
                Some(n) => {
                    named = true;
                    let found = params.iter().position(|p| p.name == n.name);
                    if found.is_none() {
                        let names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
                        self.err(
                            "E022",
                            n.span,
                            format!(
                                "`{name}` has no parameter `{}`; parameters are: {}",
                                n.name,
                                names.join(", ")
                            ),
                        );
                    }
                    found
                }
            };
            let Some(i) = index else {
                let _ = self.expr(&a.value, None);
                continue;
            };
            let Some(p) = params.get(i) else {
                self.err(
                    "E022",
                    a.value.span,
                    format!("too many arguments for `{name}`; it is {sig}"),
                );
                let _ = self.expr(&a.value, None);
                continue;
            };
            let v = if at_press {
                self.arg_at_press(&a.value, &p.ty)
            } else {
                self.expr_as(&a.value, &p.ty)
            };
            match slots.get_mut(i) {
                Some(slot @ None) => *slot = Some(v),
                _ => self.err(
                    "E022",
                    a.value.span,
                    format!("the parameter `{}` of `{name}` is given twice", p.name),
                ),
            }
        }
        // An action a view applies may leave out its last parameters: running it opens its
        // form, which asks the person for them.
        let asked = |at: usize| {
            at_press
                && slots
                    .get(at..)
                    .is_some_and(|rest| rest.iter().all(Option::is_none))
                && params.get(at..).is_some_and(|rest| {
                    rest.iter()
                        .all(|p| p.default.is_some() || p.ty.asked_by_form())
                })
        };
        let missing: Vec<&str> = params
            .iter()
            .zip(&slots)
            .enumerate()
            .filter(|(at, (p, s))| s.is_none() && p.default.is_none() && !asked(*at))
            .map(|(_, (p, _))| p.name.as_str())
            .collect();
        // A parameter a form cannot ask for says so, naming the action, the parameter and its
        // type, rather than reading as one the person simply forgot: the action stays a command
        // only an argument given here or a default reaches.
        let unaskable: Vec<_> = params
            .iter()
            .zip(&slots)
            .enumerate()
            .filter(|(at, (p, s))| {
                at_press
                    && s.is_none()
                    && p.default.is_none()
                    && slots
                        .get(*at..)
                        .is_some_and(|rest| rest.iter().all(Option::is_none))
                    && !p.ty.asked_by_form()
            })
            .map(|(_, (p, _))| p)
            .collect();
        if let Some(param) = unaskable.first() {
            self.err(
                "E022",
                span,
                format!(
                    "`{name}` cannot be asked for `{}` of type `{}`: a form asks for text,                      numbers, flags, days, moments, lengths of time, accounts, people, resources                      and enum cases, so give it here or give the parameter a default",
                    param.name,
                    param.ty.display(&self.env.app)
                ),
            );
        } else if !missing.is_empty() {
            self.err(
                "E022",
                span,
                format!("`{name}` is missing {}; it is {sig}", missing.join(", ")),
            );
        }
        let given = slots
            .iter()
            .rposition(Option::is_some)
            .map_or(0, |at| at + 1);
        let asked_from: Vec<bool> = (0..params.len()).map(asked).collect();
        slots
            .into_iter()
            .zip(params)
            .enumerate()
            .map(|(at, (s, p))| match s {
                None if p.default.is_none()
                    && !(at >= given && asked_from.get(at).copied().unwrap_or(false)) =>
                {
                    Some(Ex::error())
                }
                other => other,
            })
            .collect()
    }

    /// An argument of an action a view applies, of type `want`. An action's arguments are
    /// evaluated when it is invoked, on what the invoker sees, as its parameter defaults are:
    /// view state given as itself is read when the event fires, and an argument that computes
    /// from view state (`set(open, not open)`, `send(c, "{draft}!")`) is a function the
    /// runtime runs then ([`Self::at_event`]), so a press right after a keystroke, or a second
    /// press before the view drew again, sees what came before it. Any other argument is the
    /// value the view drew with.
    pub(super) fn arg_at_press(&mut self, e: &ast::Expr, want: &Ty) -> Ex {
        if let ExprKind::Ident(name) = &e.kind
            && let Some(state) = self.state_named(name)
            && (state.ty == *want || want.is_error())
        {
            return Self::state_at(&state);
        }
        // A scalar an event carries goes to the action boxed, as its place until it fires.
        if let ExprKind::Ident(name) = &e.kind
            && let Some((local, ty)) = self.lookup(name)
            && self.st.event_scalars.contains(&local)
            && (ty == Ty::Option(Box::new(want.clone())) || want.is_error())
        {
            return Ex::local(local, ty);
        }
        let mut names = std::collections::BTreeSet::new();
        super::boundary::names_in(e, &mut names);
        // An action value computes nothing from view state itself: the applications in it
        // take their own arguments at the press.
        let reads_state = names.iter().any(|n| self.state_named(n).is_some());
        if reads_state && !matches!(want, Ty::Action) {
            return self.at_event(e, want, &names);
        }
        self.expr_as(e, want)
    }

    /// The argument `e` of type `want`, which reads view state among `names`, as a function
    /// of its own the runtime runs when the event fires ([`ir::AtEventFn`]): it takes each view
    /// state `e` reads, read then, and each value from around it, as the view drew it. It is
    /// checked in the discipline of a `fn`, as a parameter default is.
    fn at_event(
        &mut self,
        e: &ast::Expr,
        want: &Ty,
        names: &std::collections::BTreeSet<String>,
    ) -> Ex {
        let mut params = Vec::new();
        let mut args = Vec::new();
        for name in names {
            let (ty, arg) = if let Some(state) = self.state_named(name) {
                (state.ty.clone(), Self::state_at(&state))
            } else if let Some((local, ty)) = self.lookup(name) {
                (ty.clone(), Ex::local(local, ty))
            } else {
                // A name of the package (a fn, a type, an enum), seen the same from anywhere.
                continue;
            };
            params.push(crate::resolve::ParamInfo {
                name: name.clone(),
                ty,
                default: None,
                place: false,
                entry: false,
            });
            args.push(arg);
        }
        let n = self.at_event.len();
        let id = format!("{}~arg{n}", self.st.view_id);
        let outer = std::mem::take(&mut self.st);
        self.begin(Ctx::Fn, &params, want.clone(), "");
        self.st.ns.clone_from(&outer.ns);
        self.st.types.clone_from(&outer.types);
        let value = self.expr_as(e, want);
        self.emit(Stmt::Return(Some(value)));
        let name = format!("{} at the event", outer.view_id);
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
                result: want.clone(),
            },
        ));
        let list = self.arg_list(args);
        let scalar = Ex::new(Ek::I32(i32::from(want.is_scalar())), Ty::Bool);
        let ty = if want.is_scalar() {
            Ty::Option(Box::new(want.clone()))
        } else {
            want.clone()
        };
        Ex::host(Imp::ArgAt, vec![Ex::str(&id), list, scalar], ty)
    }

    /// The view state `state` as an action's argument, read when the event fires: a handle
    /// until then, a scalar's boxed as its slot holds it.
    pub(super) fn state_at(state: &super::StateRef) -> Ex {
        let scalar = state.ty.is_scalar();
        let boxed = Ex::new(Ek::I32(i32::from(scalar)), Ty::Bool);
        let ty = if scalar {
            Ty::Option(Box::new(state.ty.clone()))
        } else {
            state.ty.clone()
        };
        Ex::host(Imp::StateAt, vec![Ex::str(&state.id), boxed], ty)
    }

    fn action_ref(c: &Callable) -> ActionRef {
        ActionRef {
            app: c.app.clone(),
            id: c.id.clone(),
            name: c.name.clone(),
            own: c.own,
            func: c.func,
            foreign_class: c.class,
        }
    }

    fn call_callable(&mut self, c: &Callable, args: &[ast::Arg], span: Span) -> Ex {
        let sig = c.signature(&self.env.app);
        // An action a view applies runs when its event fires: the view state it takes is read
        // then, not when the view drew.
        let at_press = c.kind == CKind::Action && self.st.ctx == Ctx::View;
        let bound = self.bind_args_or_default(&c.name, &c.params, args, span, &sig, at_press);
        let ctx = self.st.ctx;
        match (c.kind, ctx) {
            (CKind::Fn, _) => {
                let bound = bound
                    .into_iter()
                    .map(|s| s.unwrap_or_else(Ex::error))
                    .collect();
                self.call_fn(c, bound, span)
            }
            (CKind::Action, Ctx::Fn) => {
                self.err("E043", span, format!("a fn computes values and cannot use actions; `{}` is an action. Move this into an action", c.name));
                Ex::error()
            }
            (CKind::Action, Ctx::Action | Ctx::Program) => {
                // A call runs the action at its own door, as a change of its own: the
                // authority decides it, and the caller suspends around it and goes on from
                // its journaled result. `suggest act(args)` is the same call, recorded as a
                // proposal instead of applied.
                let bound = self.applied_args(c, bound, span);
                self.invoke(c, bound)
            }
            (CKind::Action, Ctx::View | Ctx::Block) => {
                let bound = self.applied_args(c, bound, span);
                self.action_value(c, bound)
            }
            (CKind::View, _) => {
                self.err(
                    "E061",
                    span,
                    format!(
                        "`{}` is a view; views are UI statements of a view body, not expressions",
                        c.name
                    ),
                );
                Ex::error()
            }
        }
    }

    /// A `fn`: the module's own is a call, another package's one runs there, as that package's
    /// own code, over the values it is given. A fn changes nothing — it cannot use actions
    /// (E043) or change fields (E040) — so a call of one asks nobody, as reading that
    /// package's resources asks nobody. The host boxes a scalar result like an action's, so it
    /// unboxes as one.
    fn call_fn(&mut self, c: &Callable, bound: Vec<Ex>, span: Span) -> Ex {
        if c.own {
            return match c.func {
                Some(f) => {
                    self.st.facts.calls.insert(f);
                    Ex::new(Ek::Call(f, bound), c.result.clone())
                }
                None => {
                    self.err("E081", span, format!("`{}` has no code to call", c.name));
                    Ex::error()
                }
            };
        }
        let call = Ex::host(
            Imp::CallFn,
            vec![Ex::str(&c.id), self.arg_list(bound)],
            Ty::Text,
        );
        match &c.result {
            Ty::Unit => call.typed(Ty::Unit),
            Ty::Error => Ex::error(),
            t if t.is_scalar() => Self::unwrap_opt(call, t),
            t => call.typed(t.clone()),
        }
    }

    /// A list handle holding the arguments, in parameter order.
    fn arg_list(&mut self, args: Vec<Ex>) -> Ex {
        let ty = Ty::List(Box::new(Ty::Error));
        let tmp = self.fresh(ty.clone());
        let mut stmts = vec![Stmt::Set(
            tmp,
            Ex::host(Imp::ListNew, Vec::new(), ty.clone()),
        )];
        for a in args {
            stmts.push(Stmt::Eval(Self::push_item(Ex::local(tmp, ty.clone()), a)));
        }
        Ex::new(Ek::Seq(stmts, Box::new(Ex::local(tmp, ty.clone()))), ty)
    }

    fn action_value(&mut self, c: &Callable, bound: Vec<Ex>) -> Ex {
        self.st.facts.action_values.insert(Self::action_ref(c));
        let list = self.arg_list(bound);
        // A block's whole expression is the proposal, and its value the action: the host
        // records it as one where it runs the block, instead of applying it.
        self.st.suggest = None;
        Ex::host(Imp::ActRef, vec![Ex::str(&c.id), list], Ty::Action)
    }

    fn invoke(&mut self, c: &Callable, bound: Vec<Ex>) -> Ex {
        self.st.facts.invokes.insert(Self::action_ref(c));
        let list = self.arg_list(bound);
        let act = Ex::host(Imp::ActRef, vec![Ex::str(&c.id), list], Ty::Action);
        if let Some(value) = self.st.suggest.take() {
            // The host records the proposal where it runs; a block's value is the proposal
            // itself, which the host records where it runs the block.
            if value {
                return act.typed(Ty::Action);
            }
            return Ex::host(Imp::Suggest, vec![act], Ty::Unit);
        }
        if std::mem::replace(&mut self.st.invoked, Invoked::Plain) == Invoked::Try {
            // The host builds the `Result`: `.ok(result)`, or `.err(reason)`.
            let ty = Ty::Result(Box::new(c.result.clone()), Box::new(Ty::Text));
            return Ex::host(Imp::TryInvoke, vec![act], ty);
        }
        match &c.result {
            Ty::Unit => Ex::host(Imp::Invoke, vec![act], Ty::Unit),
            Ty::Error => Ex::error(),
            t => {
                let h = Ex::host(Imp::Invoke, vec![act], Ty::Text);
                if t.is_scalar() {
                    Self::unwrap_opt(h, t)
                } else {
                    Ex { ty: t.clone(), ..h }
                }
            }
        }
    }

    /// Whether a field of type `ty` holds a resource: any resource (`host.Resource`), one
    /// resource type, or any of a union's.
    fn is_any_resource_field(ty: &Ty) -> bool {
        matches!(ty, Ty::Record(_) | Ty::Union(_))
    }

    /// Whether `e` writes a record out here (`Note(title: ...)`) rather than naming one the
    /// store holds (an action's result, a root, a reference).
    fn constructs(&self, e: &ast::Expr) -> bool {
        match &e.kind {
            ast::ExprKind::Call { callee, .. } => {
                let ExprKind::Ident(name) = &callee.kind else {
                    return false;
                };
                !self.in_std() && self.env.own_type(name).is_some()
            }
            ast::ExprKind::If { then, els, .. } => {
                let last = then.stmts.last();
                let Some(ast::Stmt {
                    kind: ast::StmtKind::Expr(t),
                    ..
                }) = last
                else {
                    return false;
                };
                els.as_ref()
                    .is_some_and(|els| self.constructs(t) && self.constructs(els))
            }
            _ => false,
        }
    }

    /// `Todo(title: "x")`: a record value, not yet a resource.
    fn construct(&mut self, key: &TypeKey, args: &[ast::Arg]) -> Ex {
        let env = self.env;
        let Some(info) = env.type_info(key) else {
            return Ex::error();
        };
        let ty = Ty::Record(key.clone());
        let tmp = self.fresh(ty.clone());
        let mut stmts = vec![Stmt::Set(
            tmp,
            Ex::host(Imp::RecNew, vec![Ex::str(&info.id)], ty.clone()),
        )];
        let mut given: Vec<&str> = Vec::new();
        for (i, a) in args.iter().enumerate() {
            let field: Option<&FieldInfo> = match &a.name {
                Some(n) => {
                    let f = info.field(&n.name);
                    if f.is_none() {
                        self.err(
                            "E021",
                            n.span,
                            format!(
                                "`{}` has no field `{}`; fields are: {}",
                                key.name,
                                n.name,
                                info.field_names()
                            ),
                        );
                    }
                    f
                }
                None => {
                    let f = info.fields.get(i);
                    if f.is_none() {
                        self.err(
                            "E022",
                            a.value.span,
                            format!(
                                "`{}` has only {} fields: {}",
                                key.name,
                                info.fields.len(),
                                info.field_names()
                            ),
                        );
                    }
                    f
                }
            };
            let Some(f) = field else {
                let _ = self.expr(&a.value, None);
                continue;
            };
            if given.contains(&f.name.as_str()) {
                self.err(
                    "E022",
                    a.value.span,
                    format!("the field `{}` is given twice", f.name),
                );
            }
            given.push(&f.name);
            if f.ty.is_container() {
                self.err(
                    "E033",
                    a.value.span,
                    format!(
                        "`{}` is a {} field; it starts empty and is changed with its methods",
                        f.name,
                        self.show(&f.ty)
                    ),
                );
                continue;
            }
            // A field holding a resource takes one the store has: an action's result, a root,
            // a reference (`@app.Type:id`). A record written out here is not one: it has no id
            // until the store makes it, so nothing can hold it, and the change would be rolled
            // back when it runs. Make it first (`let n = notes.new_note(...)`) and give that.
            if Self::is_any_resource_field(&f.ty) && self.constructs(&a.value) {
                self.err(
                    "E020",
                    a.value.span,
                    format!(
                        "`{}` holds a resource, and a `{}` written out here is not one yet: it has no id until it is made. Make it first and give what the store made (an action's result, a root, or `@app.Type:id`)",
                        f.name,
                        self.show(&f.ty)
                    ),
                );
            }
            let v = self.expr_as(&a.value, &f.ty);
            let (imp, c) = store_op(&f.ty, Imp::RecSetI, Imp::RecSetF, Imp::RecSetH);
            stmts.push(Stmt::Eval(Ex::host(
                imp,
                vec![
                    Ex::local(tmp, ty.clone()),
                    Ex::str(&f.id),
                    conv(c, v, Ty::Int),
                ],
                Ty::Unit,
            )));
        }
        Ex::new(Ek::Seq(stmts, Box::new(Ex::local(tmp, ty.clone()))), ty)
    }

    /// `person("Mia")` in a scenario: a principal named for the sample data, whose id is
    /// `scenario:Mia`. Nothing else can make a principal from text.
    /// `RichText("Ask ", ada, " about ", task)`: text as it is, a run naming each person
    /// (`@Ada`) or resource (its title) that refers to it, and each time as a run every reader
    /// reads in their own zone and language; `RichText()` is empty.
    fn builtin_rich_text(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let list_ty = Ty::List(Box::new(Ty::Text));
        let tmp = self.fresh(list_ty.clone());
        let mut stmts = vec![Stmt::Set(
            tmp,
            Ex::host(Imp::ListNew, Vec::new(), list_ty.clone()),
        )];
        for a in args {
            if let Some(n) = &a.name {
                self.err(
                    "E022",
                    n.span,
                    "`RichText(...)` takes its parts in order, unnamed: `RichText(\"Ask \", ada)`",
                );
            }
            let mut v = self.expr(&a.value, None);
            // A moment is a run every reader reads in their own zone and language.
            if v.ty == Ty::Time {
                v = Ex::host(Imp::RichTime, vec![v], Ty::RichText);
            }
            let fits = matches!(
                v.ty,
                Ty::Text | Ty::RichText | Ty::Principal | Ty::Record(_) | Ty::Error
            );
            if !fits {
                let t = self.show(&v.ty);
                self.err(
                    "E023",
                    a.value.span,
                    format!(
                        "a part of rich text is text, rich text, a time, a person or a resource, not {t}"
                    ),
                );
                continue;
            }
            stmts.push(Stmt::Eval(Ex::host(
                Imp::ListPushH,
                vec![Ex::local(tmp, list_ty.clone()), v],
                Ty::Unit,
            )));
        }
        let _ = span;
        let list = Ex::new(
            Ek::Seq(stmts, Box::new(Ex::local(tmp, list_ty.clone()))),
            list_ty,
        );
        Ex::host(Imp::RichNew, vec![list], Ty::RichText)
    }

    fn builtin_person(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        if self.st.written != Written::Scenario {
            self.err("E094", span, "`person(name)` makes a sample person for a scenario; elsewhere a principal comes only from `invoker` or a stored field");
            for a in args {
                let _ = self.expr(&a.value, None);
            }
            return Ex::error();
        }
        let [ast::Arg { name: None, value }] = args else {
            self.err(
                "E022",
                span,
                "`person` takes the person's name: `person(\"Mia\")`",
            );
            return Ex::error();
        };
        let name = self.expr_as(value, &Ty::Text);
        Ex::host(Imp::PrincipalNamed, vec![name], Ty::Principal)
    }

    /// `agent(name)`: software in the workspace a scenario stages, offered beside its people.
    fn builtin_agent(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        if self.st.written != Written::Scenario {
            self.err("E094", span, "`agent(name)` makes sample software for a scenario; elsewhere a principal comes only from `invoker` or a stored field");
            for a in args {
                let _ = self.expr(&a.value, None);
            }
            return Ex::error();
        }
        let [ast::Arg { name: None, value }] = args else {
            self.err(
                "E022",
                span,
                "`agent` takes the agent's name: `agent(\"Scout\")`",
            );
            return Ex::error();
        };
        let name = self.expr_as(value, &Ty::Text);
        Ex::host(Imp::AgentNamed, vec![name], Ty::Principal)
    }

    fn builtin_outside(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        if self.st.ctx != Ctx::Action {
            self.err(
                "E047",
                span,
                "`outside(capability, payload)` leaves the system and only an action may use it",
            );
            return Ex::error();
        }
        if !self.st.takes_resource {
            self.err(
                "E047",
                span,
                "`outside` queues its effect on the action's resource, and this action takes none: \
                 give it the resource the effect is about (`action go(d: Desk, …)`)",
            );
            return Ex::error();
        }
        // `outside(capability, payload)` or, held back until a moment, `outside(capability,
        // payload, at: <a Time>)`: the device carries it out only once the clock reaches it.
        let (Some(cap), Some(payload)) = (args.first(), args.get(1)) else {
            self.err(
                "E022",
                span,
                "`outside` takes a capability name and a payload: `outside(\"post\", payload)`",
            );
            return Ex::error();
        };
        let held = match (args.get(2), args.len()) {
            (None, 2) => None,
            (Some(third), 3) if third.name.as_ref().is_some_and(|n| n.name == "at") => Some(third),
            (Some(third), 3) => {
                self.err(
                    "E022",
                    third.value.span,
                    "`outside` holds an effect back by naming the moment: `outside(\"smtp.send\", payload, at: now + 10s)`",
                );
                return Ex::error();
            }
            _ => {
                self.err(
                    "E022",
                    span,
                    "`outside` takes a capability name, a payload, and at most the moment it is held until: `outside(\"post\", payload)`",
                );
                return Ex::error();
            }
        };
        let cap = cap.clone();
        let payload = payload.clone();
        let ExprKind::Str(parts) = &cap.value.kind else {
            self.err(
                "E022",
                cap.value.span,
                "the capability of `outside` must be a string literal such as \"post\"",
            );
            return Ex::error();
        };
        let [ast::StrPart::Lit(name)] = parts.as_slice() else {
            self.err(
                "E022",
                cap.value.span,
                "the capability of `outside` must be a plain string literal",
            );
            return Ex::error();
        };
        if !crate::origin::is_capability(name) {
            let known: Vec<&str> = crate::origin::CAPABILITIES
                .iter()
                .map(|(n, _)| *n)
                .collect();
            self.err(
                "E022",
                cap.value.span,
                format!(
                    "no capability `{name}` takes outside effects; the capabilities are: {}",
                    known.join(", ")
                ),
            );
        }
        let p = self.expr(&payload.value, None);
        self.st.facts.outside = true;
        match held {
            Some(third) => {
                let at = self.expr_as(&third.value, &Ty::Time);
                Ex::host(
                    Imp::OutsideAt,
                    vec![Ex::str(name), Self::handle_of(p), Self::handle_of(at)],
                    Ty::Unit,
                )
            }
            None => Ex::host(
                Imp::Outside,
                vec![Ex::str(name), Self::handle_of(p)],
                Ty::Unit,
            ),
        }
    }

    /// `withdraw(r, reason)`: take back the outside effects `r`'s actions queued that nothing
    /// carried out yet, settling each as withdrawn for `reason`, so they never happen. A person
    /// takes back what they wrote before it went (undo send); an agent does the same.
    fn builtin_withdraw(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        if self.st.ctx != Ctx::Action {
            self.err(
                "E047",
                span,
                "`withdraw(r, reason)` takes back an outside effect and only an action may use it",
            );
            return Ex::error();
        }
        let (Some(r), Some(reason), 2) = (args.first(), args.get(1), args.len()) else {
            self.err(
                "E022",
                span,
                "`withdraw` takes the resource and why: `withdraw(d, \"Taken back\")`",
            );
            for a in args {
                let _ = self.expr(&a.value, None);
            }
            return Ex::error();
        };
        let r = self.expr(&r.value, None);
        let primary = match &r.ty {
            Ty::Record(_) => Self::handle_of(r),
            Ty::Error => return Ex::error(),
            other => {
                let t = self.show(other);
                self.err(
                    "E020",
                    span,
                    format!("`withdraw` takes a resource, a value of a record type; this is {t}"),
                );
                return Ex::error();
            }
        };
        let why = self.expr_as(&reason.value, &Ty::Text);
        self.st.facts.outside = true;
        Ex::host(Imp::Withdraw, vec![primary, why], Ty::Unit)
    }

    /// `run_block(part, source)`: after this action commits, the host evaluates `source` as a
    /// block written by the invoker and, when it applies an action, records what happened as
    /// an `Invocation` stored in the part's `invocation` field.
    fn builtin_run_block(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        if self.st.ctx != Ctx::Action {
            self.err(
                "E047",
                span,
                "`run_block(part, source)` queues a step for after the action commits, and only an action may use it",
            );
            return Ex::error();
        }
        let (Some(part), Some(source), 2) = (args.first(), args.get(1), args.len()) else {
            self.err(
                "E022",
                span,
                "`run_block` takes a part and the block's source: `run_block(part, source)`",
            );
            return Ex::error();
        };
        let p = self.expr(&part.value, None);
        let src = self.expr_as(&source.value, &Ty::Text);
        let want = Ty::Option(Box::new(Ty::Record(TypeKey {
            app: crate::host::HOST_APP.to_string(),
            name: "Invocation".to_string(),
        })));
        let Ty::Record(key) = p.ty.clone() else {
            if !p.ty.is_error() {
                self.err("E020", part.value.span, "`run_block` needs a resource of this module's types that has a field `invocation: Option<host.Invocation>`");
            }
            return Ex::error();
        };
        let field = self
            .env
            .type_info(&key)
            .and_then(|t| t.field("invocation"))
            .filter(|f| f.ty.same(&want))
            .cloned();
        let Some(f) = field else {
            self.err("E020", part.value.span, format!("`{}` has no field `invocation: Option<host.Invocation>`, which is where the host records the invocation", key.name));
            return Ex::error();
        };
        if key.app != self.env.app {
            self.err("E045", part.value.span, format!("`{}` is declared by another app ({}); an action may only change its own module's types", key.name, key.app));
            return Ex::error();
        }
        self.st.facts.mutates = true;
        Ex::host(Imp::QueueBlock, vec![p, Ex::str(&f.id), src], Ty::Unit)
    }

    /// `set(name, value)`: an action reference that changes the view state `name` when the
    /// control it is on is used. Views only.
    fn builtin_set(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        if self.st.ctx != Ctx::View {
            let who = self.st.ctx.word();
            self.err(
                "E066",
                span,
                format!("`set(name, value)` changes a view's state; {who} has none"),
            );
            return Ex::error();
        }
        let (Some(target), Some(value), 2) = (args.first(), args.get(1), args.len()) else {
            self.err(
                "E022",
                span,
                "`set` takes the view state and its new value: `set(sort, .title)`",
            );
            return Ex::error();
        };
        let state = match &target.value.kind {
            ExprKind::Ident(n) if target.name.is_none() => self.state_named(n),
            _ => None,
        };
        let Some(state) = state else {
            let known = self.state_names();
            self.err("E066", target.value.span, format!("the first argument of `set` names a `state` declaration of this view; this view declares: {known}"));
            let _ = self.expr(&value.value, None);
            return Ex::error();
        };
        let v = self.arg_at_press(&value.value, &state.ty);
        Self::set_action(&state, v)
    }

    /// The action that sets the view state `state` to `v`.
    pub(super) fn set_action(state: &super::StateRef, v: Ex) -> Ex {
        Ex::host(
            Imp::ActSet,
            vec![Ex::str(&state.id), Self::handle_of(v)],
            Ty::Action,
        )
    }

    /// `open(r)` or `open(r, view: v)`: an action reference that goes to the window of `r`
    /// (workspace-model.md, "Windows"). Views only.
    fn builtin_open(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        if self.st.ctx != Ctx::View {
            let who = self.st.ctx.word();
            self.err(
                "E067",
                span,
                format!(
                    "`open(r)` is applied from a view, on a control; {who} cannot open a window"
                ),
            );
            return Ex::error();
        }
        let Some(target) = args.first().filter(|a| a.name.is_none()) else {
            self.err(
                "E022",
                span,
                "`open` takes the resource to open: `open(note)`",
            );
            return Ex::error();
        };
        let r = self.expr(&target.value, None);
        let Ty::Record(key) = r.ty.clone() else {
            if !r.ty.is_error() {
                let t = self.show(&r.ty);
                self.err(
                    "E067",
                    target.value.span,
                    format!("`open` opens a resource, a value of a record type; this is {t}"),
                );
            }
            return Ex::error();
        };
        let mut view_id = String::new();
        let mut front = Ex::new(Ek::I32(1), Ty::Bool);
        for a in args.iter().skip(1) {
            if a.name.as_ref().is_some_and(|n| n.name == "front") {
                // `front: false`: shown in the next column, not brought in front of a narrow
                // window (a selection the keyboard moves).
                front = self.expr_as(&a.value, &Ty::Bool);
                continue;
            }
            let named_view = a.name.as_ref().is_some_and(|n| n.name == "view");
            if !named_view {
                self.err(
                    "E022",
                    a.value.span,
                    "`open` takes the resource and, optionally, `view: <view>` and `front: false`",
                );
                continue;
            }
            match self.view_for(&a.value, &key) {
                Some(id) => view_id = id,
                None => {
                    self.err(
                        "E067",
                        a.value.span,
                        format!(
                            "`view:` names a view that takes a `{}` and nothing else",
                            key.name
                        ),
                    );
                }
            }
        }
        Ex::host(Imp::ActOpen, vec![r, Ex::str(&view_id), front], Ty::Action)
    }

    /// The id of the view `e` names (`editor` or `notes.editor`), when it shows a `key` alone.
    fn view_for(&self, e: &ast::Expr, key: &TypeKey) -> Option<String> {
        let path = dotted(e)?;
        let c = match path.as_slice() {
            [one] => self.own_callable(one)?,
            [prefix @ .., last] => {
                let import = self.env.find_import(&prefix.join("."))?;
                self.env.foreign_callable(&import.app, last)?
            }
            [] => return None,
        };
        let fits = c.kind == CKind::View
            && matches!(c.params.as_slice(), [p] if p.ty == Ty::Record(key.clone()));
        fits.then(|| c.id.clone())
    }

    /// `notify(to: who, title: text, body: text, opens: r)`: tell the person `to` something on
    /// their device, and open the resource `r` when they choose it. The invocation records it,
    /// so a run replayed after waiting tells them no more than the first time.
    fn builtin_notify(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let mut to = None;
        let mut title = None;
        let mut body = None;
        let mut opens = None;
        for arg in args {
            let Some(named) = &arg.name else {
                self.err(
                    "E022",
                    arg.value.span,
                    "`notify` takes what it says by name: `notify(to:, title:, body:, opens:)`",
                );
                let _ = self.expr(&arg.value, None);
                continue;
            };
            match named.name.as_str() {
                "to" => to = Some(self.expr_as(&arg.value, &Ty::Principal)),
                "title" | "body" => {
                    // What the notification shows is words a person reads: its literal is a
                    // message of the action, as the text of a component is one of the view.
                    self.mark_shown(&arg.value);
                    let said = self.expr_as(&arg.value, &Ty::Text);
                    if named.name == "title" {
                        title = Some(said);
                    } else {
                        body = Some(said);
                    }
                }
                "opens" => {
                    let r = self.expr(&arg.value, None);
                    match r.ty.clone() {
                        Ty::Record(_) => opens = Some(r),
                        ty if ty.is_error() => {}
                        ty => {
                            let t = self.show(&ty);
                            self.err(
                                "E022",
                                arg.value.span,
                                format!(
                                    "`opens:` is the resource choosing the notification opens; this is {t}"
                                ),
                            );
                        }
                    }
                }
                other => {
                    self.err(
                        "E022",
                        arg.value.span,
                        format!(
                            "`notify` says `to:`, `title:`, `body:` and `opens:`; not `{other}:`"
                        ),
                    );
                    let _ = self.expr(&arg.value, None);
                }
            }
        }
        let mut missing = Vec::new();
        for (given, what) in [
            (to.is_some(), "to"),
            (title.is_some(), "title"),
            (body.is_some(), "body"),
            (opens.is_some(), "opens"),
        ] {
            if !given {
                missing.push(what);
            }
        }
        if !missing.is_empty() {
            self.err(
                "E022",
                span,
                format!(
                    "`notify` also needs {}: `notify(to: who, title: ..., body: ..., opens: r)`",
                    missing.join(", ")
                ),
            );
        }
        self.reads(crate::effects::Effect::Notify, span, "`notify`");
        Ex::host(
            Imp::Notify,
            vec![
                to.unwrap_or_else(Ex::error),
                title.unwrap_or_else(Ex::error),
                body.unwrap_or_else(Ex::error),
                opens.unwrap_or_else(Ex::error),
            ],
            Ty::Unit,
        )
    }

    fn builtin_model(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        if !matches!(self.st.ctx, Ctx::Action | Ctx::Program) {
            self.err(
                "E047",
                span,
                "`model_call(kind, input)` is nondeterministic and only an action may use it",
            );
            return Ex::error();
        }
        let (Some(kind), Some(input), 2) = (args.first(), args.get(1), args.len()) else {
            self.err(
                "E022",
                span,
                "`model_call` takes a kind and an input: `model_call(\"classify\", text)`",
            );
            return Ex::error();
        };
        let k = self.expr_as(&kind.value, &Ty::Text);
        let i = self.expr(&input.value, None);
        Ex::host(Imp::ModelCall, vec![k, Self::handle_of(i)], Ty::Text)
    }

    // ---- methods --------------------------------------------------------------------------

    pub(super) fn method(
        &mut self,
        recv: &ast::Expr,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> Ex {
        // `Date.parse(text, near: now)`: a day a text names, read at a moment. It stands on the
        // type, which is not a value, so it is read before the receiver.
        if let ExprKind::Ident(n) = &recv.kind
            && self.lookup(n).is_none()
            && !self.names_import(recv)
            && let Some(ty) = parse_ty(n)
        {
            return self.type_method(ty, name, args, span);
        }
        // `a.items.insert(x)` needs the record and the field, not the field's value.
        if let ExprKind::Field { base, name: fname } = &recv.kind
            && !self.names_import(base)
            && !self.names_platform(base)
        {
            let b = self.expr(base, None);
            if let Some((key, f)) = self.container_field(&b, fname) {
                return self.container_method(b, &key, &f, name, args, span);
            }
            let r = self.field_read(b, fname);
            return self.value_method(r, name, args, span);
        }
        let r = self.expr(recv, None);
        self.value_method(r, name, args, span)
    }

    /// Whether `e` is `space` or `workspace`, read only through their fields (`space.width`,
    /// `workspace.members`), and not a variable of that name.
    fn names_platform(&self, e: &ast::Expr) -> bool {
        matches!(&e.kind, ExprKind::Ident(n) if (n == "space" || n == "workspace") && self.lookup(n).is_none())
    }

    fn container_field(&self, b: &Ex, fname: &ast::Ident) -> Option<(TypeKey, FieldInfo)> {
        let Ty::Record(key) = &b.ty else { return None };
        let f = self.env.type_info(key)?.field(&fname.name)?;
        f.ty.is_container().then(|| (key.clone(), f.clone()))
    }

    pub(super) fn arity(
        &mut self,
        name: &ast::Ident,
        args: &[ast::Arg],
        n: usize,
        span: Span,
    ) -> bool {
        if args.len() == n && args.iter().all(|a| a.name.is_none()) {
            return true;
        }
        let word = if n == 1 { "argument" } else { "arguments" };
        self.err("E022", span, format!("`.{}` takes {n} {word}", name.name));
        for a in args {
            let _ = self.expr(&a.value, None);
        }
        false
    }

    fn container_method(
        &mut self,
        b: Ex,
        key: &TypeKey,
        f: &FieldInfo,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> Ex {
        let m = name.name.as_str();
        if let (Ty::Map(t), "get" | "keys") = (&f.ty, m) {
            return self.map_read(b, f, t, name, args, span);
        }
        // The items under (above) one by a field of theirs is a query of the store's index: the
        // field it follows is a column of the collection, and the runs of one value are a
        // node's children. The field's own read stays the answer's fallback, and the one a list
        // held anywhere else takes.
        if let (Ty::Collection(t), "descendants" | "ancestors") = (&f.ty, m) {
            let list = self.field_read(
                b.clone(),
                &ast::Ident {
                    name: f.name.clone(),
                    span: name.span,
                },
            );
            let field = f.id.clone();
            return self.relatives(list, t, name, args, span, Some((b, field)));
        }
        // What a container field can be written through, and what it answers besides: the
        // reads are its value's (`methods::names`), so a field accepts what a list or set does.
        let writes: &[&str] = match &f.ty {
            Ty::Collection(_) => &["insert", "remove", "remove_where", "move"],
            Ty::Set(_) => &["add", "remove"],
            Ty::Map(v) if matches!(**v, Ty::Set(_)) => &["add", "remove", "put"],
            Ty::Map(_) => &["put", "remove"],
            _ => &[],
        };
        let valid = writes.contains(&m);
        let reads = super::methods::names(&f.ty).unwrap_or_default();
        if !valid && !reads.contains(&m) {
            let t = self.show(&f.ty);
            let ops = match &f.ty {
                Ty::Collection(_) | Ty::Set(_) => {
                    format!("{}, and what its items answer: {}", writes.join(", "), reads.join(", "))
                }
                Ty::Map(v) if matches!(**v, Ty::Set(_)) => {
                    "add(key, element), remove(key, element), remove(key), get (an Option of the set) and keys".to_owned()
                }
                _ => "put, remove, get (an Option of the value) and keys".to_owned(),
            };
            let known = writes.iter().chain(reads.iter()).copied();
            let hint = crate::diag::suggestion(m, known);
            self.err(
                "E027",
                name.span,
                format!("`{}` is a {t} field{hint}; its methods are: {ops}", f.name),
            );
            return Ex::error();
        }
        if !valid {
            let r = self.field_read(
                b,
                &ast::Ident {
                    name: f.name.clone(),
                    span: name.span,
                },
            );
            return self.value_method(r, name, args, span);
        }
        if self.st.ctx != Ctx::Action {
            let who = self.st.ctx.word();
            self.err("E040", span, format!("{who} cannot change `{}.{}`; only an action mutates fields. Write an action and apply it", key.name, f.name));
            return Ex::error();
        }
        if key.app != self.env.app {
            self.err("E045", span, format!("`{}` is declared by another app ({}); an action may only change its own module's types", key.name, key.app));
            return Ex::error();
        }
        self.st.facts.mutates = true;
        let fid = Ex::str(&f.id);
        match (&f.ty, m) {
            (Ty::Collection(t), "insert") => {
                if !self.arity(name, args, 1, span) {
                    return Ex::error();
                }
                let Some(a) = args.first() else {
                    return Ex::error();
                };
                // Into a collection of a union, an item of one of its types is inserted as
                // that type: `channels.insert(Category(...))` gives the Category.
                let v = self.expr(&a.value, Some(t));
                let case = (matches!(**t, Ty::Union(_))
                    && !v.ty.is_error()
                    && !v.ty.same(t)
                    && v.ty.fits(t))
                .then(|| v.ty.clone());
                let v = self.coerce(v, t, a.value.span);
                Ex::host(
                    Imp::CollInsert,
                    vec![b, fid, v],
                    case.unwrap_or_else(|| (**t).clone()),
                )
            }
            (Ty::Collection(t), "remove") => {
                if !self.arity(name, args, 1, span) {
                    return Ex::error();
                }
                let Some(a) = args.first() else {
                    return Ex::error();
                };
                let v = self.expr_as(&a.value, t);
                Ex::host(Imp::CollRemove, vec![b, fid, v], Ty::Unit)
            }
            (Ty::Collection(t), "move") => self.move_item(b, fid, t, args, span),
            (Ty::Collection(t), "remove_where") => self.remove_where(b, f, t, name, args, span),
            (Ty::Set(t), "add" | "remove") => {
                if !self.arity(name, args, 1, span) {
                    return Ex::error();
                }
                let Some(a) = args.first() else {
                    return Ex::error();
                };
                let v = Self::handle_of(self.expr_as(&a.value, t));
                let imp = if m == "add" {
                    Imp::SetAdd
                } else {
                    Imp::SetRemove
                };
                Ex::host(imp, vec![b, fid, v], Ty::Unit)
            }
            // A map of sets: an element added to or taken from one key's set, each element
            // stored on its own so concurrent additions all stay.
            (Ty::Map(t), "add" | "remove")
                if matches!(**t, Ty::Set(_)) && (m == "add" || args.len() == 2) =>
            {
                if !self.arity(name, args, 2, span) {
                    return Ex::error();
                }
                let (Some(k), Some(e), Ty::Set(element)) = (args.first(), args.get(1), &**t) else {
                    return Ex::error();
                };
                let k = self.expr_as(&k.value, &Ty::Text);
                let e = Self::handle_of(self.expr_as(&e.value, element));
                let imp = if m == "add" {
                    Imp::MapSetAdd
                } else {
                    Imp::MapSetRemove
                };
                Ex::host(imp, vec![b, fid, k, e], Ty::Unit)
            }
            (Ty::Map(t), "put") if matches!(**t, Ty::Set(_)) => {
                self.err(
                    "E027",
                    name.span,
                    format!(
                        "`{}` is a map of sets: add and remove its elements (`.add(key, element)`), so changes made at once all stay",
                        f.name
                    ),
                );
                Ex::error()
            }
            (Ty::Map(t), "put") => {
                if !self.arity(name, args, 2, span) {
                    return Ex::error();
                }
                let (Some(k), Some(v)) = (args.first(), args.get(1)) else {
                    return Ex::error();
                };
                let k = self.expr_as(&k.value, &Ty::Text);
                let v = Self::handle_of(self.expr_as(&v.value, t));
                Ex::host(Imp::MapPut, vec![b, fid, k, v], Ty::Unit)
            }
            (Ty::Map(_), "remove") => {
                if !self.arity(name, args, 1, span) {
                    return Ex::error();
                }
                let Some(k) = args.first() else {
                    return Ex::error();
                };
                let k = self.expr_as(&k.value, &Ty::Text);
                Ex::host(Imp::MapRemove, vec![b, fid, k], Ty::Unit)
            }
            _ => Ex::error(),
        }
    }

    /// `items.move(item, after: other)` or `before: other`: a place named by a neighbour, never
    /// counted (convergence.md, "Actions"), so concurrent moves land where each person meant.
    fn move_item(&mut self, b: Ex, fid: Ex, t: &Ty, args: &[ast::Arg], span: Span) -> Ex {
        let usage = "write `.move(item, after: other)`, `.move(item, before: other)`, `.move(item, to: index)` or `.move(item, to: index, among: run)`";
        // `to: i, among: run`: the place `i` among the items of `run` (a lane of a board, the
        // items of one section) other than the one moved.
        if let [item, place, among] = args
            && place.name.as_ref().is_some_and(|n| n.name == "to")
            && among.name.as_ref().is_some_and(|n| n.name == "among")
        {
            let item = self.expr_as(&item.value, t);
            let index = self.expr_as(&place.value, &Ty::Int);
            let run = self.expr_as(&among.value, &Ty::List(Box::new(t.clone())));
            return Ex::host(Imp::CollMoveAmong, vec![b, fid, item, index, run], Ty::Unit);
        }
        let (Some(item), Some(place), 2) = (args.first(), args.get(1), args.len()) else {
            self.err(
                "E022",
                span,
                format!("`.move` takes the item and where it goes: {usage}"),
            );
            return Ex::error();
        };
        let side = place.name.as_ref().map(|n| n.name.as_str());
        let after = match side {
            Some("after") => true,
            Some("before") => false,
            // An index, as a drag gives one: resolved, when the action runs, to the item it
            // then comes before, which is what is recorded (convergence.md, "Actions").
            Some("to") => {
                let item = self.expr_as(&item.value, t);
                let index = self.expr_as(&place.value, &Ty::Int);
                return Ex::host(Imp::CollMoveTo, vec![b, fid, item, index], Ty::Unit);
            }
            _ => {
                self.err(
                    "E022",
                    place.value.span,
                    format!("`.move` names where the item goes: {usage}"),
                );
                let _ = self.expr(&place.value, None);
                return Ex::error();
            }
        };
        let item = self.expr_as(&item.value, t);
        let neighbour = self.expr_as(&place.value, t);
        let side = Ex::new(Ek::I32(i32::from(after)), Ty::Bool);
        Ex::host(Imp::CollMove, vec![b, fid, item, neighbour, side], Ty::Unit)
    }

    /// `items.remove_where(x -> x.done)`: removes every item the predicate holds of and gives how
    /// many went.
    fn remove_where(
        &mut self,
        b: Ex,
        f: &FieldInfo,
        t: &Ty,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> Ex {
        let owner = self.fresh(b.ty.clone());
        let read = self.field_read(
            Ex::local(owner, b.ty.clone()),
            &ast::Ident {
                name: f.name.clone(),
                span: name.span,
            },
        );
        let filter = ast::Ident {
            name: "filter".into(),
            span: name.span,
        };
        let gone = self.higher_order(read, t, &filter, args, span);
        if gone.ty.is_error() {
            return Ex::error();
        }
        let list_ty = gone.ty.clone();
        let list = self.fresh(list_ty.clone());
        let mut fl = self.new_for(Ex::local(list, list_ty.clone()), t);
        fl.body.push(Stmt::Eval(Ex::host(
            Imp::CollRemove,
            vec![
                Ex::local(owner, b.ty.clone()),
                Ex::str(&f.id),
                Ex::local(fl.item_local, t.clone()),
            ],
            Ty::Unit,
        )));
        let count = Ex::host(Imp::ListLen, vec![Ex::local(list, list_ty)], Ty::Int);
        Ex::new(
            Ek::Seq(
                vec![
                    Stmt::Set(owner, b),
                    Stmt::Set(list, gone),
                    Stmt::ForList(Box::new(fl)),
                ],
                Box::new(count),
            ),
            Ty::Int,
        )
    }

    /// `m.get(key)` and `m.keys()` of a `Map` field: reads, allowed wherever fields can be read.
    fn map_read(
        &mut self,
        b: Ex,
        f: &FieldInfo,
        value: &Ty,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> Ex {
        let fid = Ex::str(&f.id);
        if name.name == "keys" {
            if !self.arity(name, args, 0, span) {
                return Ex::error();
            }
            let ty = Ty::List(Box::new(Ty::Text));
            return Ex::host(Imp::MapKeys, vec![b, fid], ty);
        }
        if !self.arity(name, args, 1, span) {
            return Ex::error();
        }
        let Some(k) = args.first() else {
            return Ex::error();
        };
        let key = self.expr_as(&k.value, &Ty::Text);
        // A key's set reads as the list of its elements, a value to pass around.
        let value = match value {
            Ty::Set(element) => Ty::List(element.clone()),
            other => other.clone(),
        };
        let ty = Ty::Option(Box::new(value));
        Ex::host(Imp::MapGetH, vec![b, fid, key], ty)
    }

    /// `m.keys()` and `m.get(key)` of a map held as a value, not a field's.
    fn map_value_method(
        &mut self,
        m: Ex,
        value: &Ty,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> Ex {
        if name.name == "keys" {
            if !self.arity(name, args, 0, span) {
                return Ex::error();
            }
            return Ex::host(Imp::MapKeysValue, vec![m], Ty::List(Box::new(Ty::Text)));
        }
        if !self.arity(name, args, 1, span) {
            return Ex::error();
        }
        let Some(k) = args.first() else {
            return Ex::error();
        };
        let key = self.expr_as(&k.value, &Ty::Text);
        let value = match value {
            Ty::Set(element) => Ty::List(element.clone()),
            other => other.clone(),
        };
        Ex::host(Imp::MapGetValue, vec![m, key], Ty::Option(Box::new(value)))
    }

    /// The words of `random`, for a message that names them.
    const DRAWS: &'static str = "`random.int(lo, hi)`, `random.float()`, `random.choice(xs)`, `random.shuffle(xs)` and `random.id()`";

    /// `random.int(lo, hi)`, `random.float()`, `random.choice(xs)`, `random.shuffle(xs)` and
    /// `random.id()`: a draw. Only an action makes one, declaring `uses random`, and
    /// its invocation keeps each value drawn, so a replay reads the same values (effects.md).
    fn builtin_random(&mut self, name: &ast::Ident, args: &[ast::Arg], span: Span) -> Ex {
        let word = name.name.as_str();
        let what = format!("`random.{word}`");
        match word {
            "int" => {
                if args.len() != 2 || args.iter().any(|a| a.name.is_some()) {
                    self.err(
                        "E022",
                        span,
                        "`random.int` takes the lowest and highest number to draw, by position: `random.int(1, 6)`",
                    );
                    for a in args {
                        let _ = self.expr(&a.value, None);
                    }
                    return Ex::error();
                }
                let (Some(lo), Some(hi)) = (args.first(), args.get(1)) else {
                    return Ex::error();
                };
                let lo = self.expr_as(&lo.value, &Ty::Int);
                let hi = self.expr_as(&hi.value, &Ty::Int);
                self.reads(crate::effects::Effect::Random, span, "`random.int`");
                Ex::host(Imp::RandomInt, vec![lo, hi], Ty::Int)
            }
            "float" | "id" => {
                if !args.is_empty() {
                    self.err(
                        "E022",
                        span,
                        format!("{what} takes no arguments: it draws one value"),
                    );
                    for a in args {
                        let _ = self.expr(&a.value, None);
                    }
                    return Ex::error();
                }
                self.reads(crate::effects::Effect::Random, span, &what);
                if word == "float" {
                    Ex::host(Imp::RandomFloat, vec![], Ty::Float)
                } else {
                    Ex::host(Imp::RandomId, vec![], Ty::Text)
                }
            }
            "choice" | "shuffle" => {
                let [xs] = args else {
                    self.err(
                        "E022",
                        span,
                        format!(
                            "{what} takes the list to draw from, by position: `random.{word}(items)`"
                        ),
                    );
                    for a in args {
                        let _ = self.expr(&a.value, None);
                    }
                    return Ex::error();
                };
                if xs.name.is_some() {
                    self.err(
                        "E022",
                        xs.value.span,
                        format!("{what} takes its list by position: `random.{word}(items)`"),
                    );
                }
                let got = self.expr(&xs.value, None);
                // A collection or a set reads as the list of what it holds.
                let element = match &got.ty {
                    Ty::List(t) | Ty::Collection(t) | Ty::Set(t) => (**t).clone(),
                    Ty::Error => return Ex::error(),
                    other => {
                        let found = self.show(other);
                        self.err(
                            "E020",
                            xs.value.span,
                            format!("{what} draws from a list; this is {found}"),
                        );
                        return Ex::error();
                    }
                };
                let list = self.coerce(got, &Ty::List(Box::new(element.clone())), xs.value.span);
                self.reads(crate::effects::Effect::Random, span, &what);
                if word == "choice" {
                    Ex::host(Imp::RandomChoice, vec![list], Ty::Option(Box::new(element)))
                } else {
                    Ex::host(Imp::RandomShuffle, vec![list], Ty::List(Box::new(element)))
                }
            }
            _ => {
                self.err(
                    "E014",
                    name.span,
                    format!("`random` has no `{word}`; it draws {}", Self::DRAWS),
                );
                for a in args {
                    let _ = self.expr(&a.value, None);
                }
                Ex::error()
            }
        }
    }

    /// `web.get(url, headers:, if_none_match:)` and `web.post(url, body:, headers:)`: a request
    /// to a service outside Tilo, which the invocation makes once, records, and replays from the
    /// record (effects.md).
    fn builtin_web(&mut self, name: &ast::Ident, args: &[ast::Arg], span: Span) -> Ex {
        let get = match name.name.as_str() {
            "get" => true,
            "post" => false,
            other => {
                self.err(
                    "E022",
                    name.span,
                    format!(
                        "`web` has `get` and `post`; not `{other}`. `web.get(url, headers:, if_none_match:)` fetches, `web.post(url, body:, headers:)` sends"
                    ),
                );
                for a in args {
                    let _ = self.expr(&a.value, None);
                }
                return Ex::error();
            }
        };
        let what = if get { "`web.get`" } else { "`web.post`" };
        let Some(url) = args.first().filter(|a| a.name.is_none()) else {
            self.err(
                "E022",
                span,
                format!(
                    "{what} takes the url first: `web.get(\"{}\")`",
                    "https://example.com/"
                ),
            );
            for a in args {
                let _ = self.expr(&a.value, None);
            }
            return Ex::error();
        };
        let url = self.expr_as(&url.value, &Ty::Text);
        let headers = Ty::List(Box::new(Ty::Text));
        let mut headers_arg = None;
        let mut if_none_match = None;
        let mut body = None;
        for arg in &args[1..] {
            let Some(named) = &arg.name else {
                self.err(
                    "E022",
                    arg.value.span,
                    format!(
                        "{what} takes the url by position; the rest by name: {}",
                        if get {
                            "`headers:`, `if_none_match:`"
                        } else {
                            "`body:`, `headers:`"
                        }
                    ),
                );
                let _ = self.expr(&arg.value, None);
                continue;
            };
            match (get, named.name.as_str()) {
                (_, "headers") => {
                    headers_arg = Some(self.expr_as(&arg.value, &headers));
                }
                (true, "if_none_match") => {
                    let value = self.expr(&arg.value, None);
                    if_none_match = Some(if matches!(value.ty, Ty::Text) {
                        Ex::host(Imp::SomeH, vec![value], Ty::Option(Box::new(Ty::Text)))
                    } else if matches!(&value.ty, Ty::Option(inner) if **inner == Ty::Text)
                        || value.ty.is_error()
                    {
                        value
                    } else {
                        let t = self.show(&value.ty);
                        self.err(
                            "E022",
                            arg.value.span,
                            format!("`if_none_match:` takes the ETag a service gave, as Text or an `Option<Text>`; this is {t}"),
                        );
                        Ex::error()
                    });
                }
                (false, "body") => {
                    body = Some(self.expr_as(&arg.value, &Ty::Text));
                }
                (_, other) => {
                    self.err(
                        "E022",
                        named.span,
                        format!(
                            "`{other}:` is no argument of {}",
                            if get { "`web.get`" } else { "`web.post`" }
                        ),
                    );
                    let _ = self.expr(&arg.value, None);
                }
            }
        }
        let headers_arg = headers_arg.unwrap_or_else(|| Ex::host(Imp::ListNew, vec![], headers));
        self.reads(crate::effects::Effect::Web, span, what);
        if get {
            let if_none_match = if_none_match
                .unwrap_or_else(|| Ex::host(Imp::NoneV, vec![], Ty::Option(Box::new(Ty::Text))));
            Ex::host(
                Imp::WebGet,
                vec![url, headers_arg, if_none_match],
                Ty::Response,
            )
        } else {
            let Some(body) = body else {
                self.err(
                    "E022",
                    span,
                    "`web.post` sends a body, named `body:`: `web.post(\"https://example.com/\", body: text)`",
                );
                return Ex::error();
            };
            Ex::host(Imp::WebPost, vec![url, body, headers_arg], Ty::Response)
        }
    }

    /// `json.parse(text)`, `json.decode<T>(text)` and `json.encode(value)`: JSON text read into
    /// a value of a type the caller names, and a value written as text. No effect: it reads no
    /// clock, draws nothing and reaches no service, so an action that only reads text it was
    /// given needs no `uses`.
    fn builtin_json(
        &mut self,
        name: &ast::Ident,
        types: &[ast::TypeExpr],
        args: &[ast::Arg],
        span: Span,
    ) -> Ex {
        let result = |held: Ty| Ty::Result(Box::new(held), Box::new(Ty::Text));
        match name.name.as_str() {
            "parse" => {
                if let Some(written) = types.first() {
                    self.err(
                        "E022",
                        written.span,
                        "`json.parse` decides nothing about the shape: it answers a `json.Value`, and `json.decode<T>(text)` reads text into a `T`",
                    );
                }
                let Some(text) = self.json_text_arg("json.parse(text)", args, span) else {
                    return Ex::error();
                };
                Ex::host(Imp::JsonParse, vec![text], result(Ty::Json))
            }
            "decode" => {
                let Some(written) = types.first() else {
                    self.err(
                        "E022",
                        span,
                        "`json.decode` says what it reads: `json.decode<Item>(text)`, with a record, an enum, a list, an option, a day or a moment",
                    );
                    for a in args {
                        let _ = self.expr(&a.value, None);
                    }
                    return Ex::error();
                };
                let mut diags = Vec::new();
                let held = crate::resolve::resolve_type_expr(self.env, written, &mut diags);
                for diagnostic in diags {
                    self.err(&diagnostic.code, diagnostic.span, diagnostic.message);
                }
                if !held.is_error() && !held.reads_json() {
                    let t = self.show(&held);
                    self.err(
                        "E020",
                        written.span,
                        format!(
                            "`json.decode` reads a value text can make: Int, Float, Bool, Text, Date, Time, a list, an option, an enum or a record; `{t}` is none of them"
                        ),
                    );
                    return Ex::error();
                }
                let Some(text) = self.json_text_arg("json.decode<T>(text)", args, span) else {
                    return Ex::error();
                };
                let app = self.env.app.clone();
                Ex::host(
                    Imp::JsonDecode,
                    vec![text, Ex::str(&held.meta_text(&app)), Ex::str(&app)],
                    result(held),
                )
            }
            "encode" => {
                if let Some(written) = types.first() {
                    self.err(
                        "E022",
                        written.span,
                        "`json.encode` writes what its value is; it takes no type",
                    );
                }
                let Some(arg) = args.first().filter(|a| a.name.is_none()) else {
                    self.err(
                        "E022",
                        span,
                        "`json.encode` writes one value: `json.encode(item)`",
                    );
                    for a in args {
                        let _ = self.expr(&a.value, None);
                    }
                    return Ex::error();
                };
                let value = self.expr(&arg.value, None);
                if args.len() > 1 {
                    for a in &args[1..] {
                        self.err("E022", a.value.span, "`json.encode` writes one value");
                        let _ = self.expr(&a.value, None);
                    }
                }
                if !value.ty.is_error() && !value.ty.writes_json() {
                    let t = self.show(&value.ty);
                    self.err(
                        "E020",
                        arg.value.span,
                        format!(
                            "`json.encode` writes a value JSON has: Int, Float, Bool, Text, Date, Time, a list, an option, a map, an enum, a record or a `json.Value`; `{t}` is none of them"
                        ),
                    );
                    return Ex::error();
                }
                Ex::host(Imp::JsonEncode, vec![value], Ty::Text)
            }
            other => {
                self.err(
                    "E022",
                    name.span,
                    format!(
                        "`json` has `parse`, `decode` and `encode`; not `{other}`. `json.parse(text)` reads any shape, `json.decode<T>(text)` reads a `T`, `json.encode(value)` writes text"
                    ),
                );
                for a in args {
                    let _ = self.expr(&a.value, None);
                }
                Ex::error()
            }
        }
    }

    /// The one text argument of `json.parse` and `json.decode<T>`.
    fn json_text_arg(&mut self, what: &str, args: &[ast::Arg], span: Span) -> Option<Ex> {
        if args.len() != 1 || args.first().is_some_and(|a| a.name.is_some()) {
            self.err("E022", span, format!("{what} takes one text: `{what}`"));
            for a in args {
                let _ = self.expr(&a.value, None);
            }
            return None;
        }
        let text = self.expr_as(&args[0].value, &Ty::Text);
        Some(text)
    }

    /// A method that stands on a type instead of a value: `Date.parse(text, near: now)` and
    /// `Time.parse(text, near: now)`, the day or the moment a text names when people type it
    /// ("tomorrow", "fri 10am", "in 2 weeks"), read at a moment in the person's zone.
    fn type_method(&mut self, ty: Ty, name: &ast::Ident, args: &[ast::Arg], span: Span) -> Ex {
        let t = self.show(&ty);
        if name.name != "parse" {
            self.err(
                "E027",
                name.span,
                format!("`{t}` is a type, not a value; it has one method of its own, `.parse(text, near: at)`, and builds a value with `{t}(text)`"),
            );
            for a in args {
                let _ = self.expr(&a.value, None);
            }
            return Ex::error();
        }
        let [text, near] = args else {
            self.err(
                "E022",
                span,
                format!("`.parse` takes the text and the moment it is read at: `{t}.parse(\"tomorrow 9am\", near: now)`"),
            );
            for a in args {
                let _ = self.expr(&a.value, None);
            }
            return Ex::error();
        };
        if text.name.is_some() {
            self.err(
                "E022",
                text.value.span,
                "`.parse` takes the text by position: `.parse(\"tomorrow 9am\", near: now)`",
            );
        }
        // The moment is named, as the text it is read at: a bare second argument reads as a
        // count of something other than what it is.
        match &near.name {
            Some(named) if named.name == "near" => {}
            Some(named) => self.err(
                "E022",
                named.span,
                "`.parse` reads the text at the moment named `near:`",
            ),
            None => self.err(
                "E022",
                near.value.span,
                "`.parse` reads the text at the moment named `near:`, such as `near: now`",
            ),
        }
        let text = self.expr_as(&text.value, &Ty::Text);
        let near = self.expr_as(&near.value, &Ty::Time);
        self.reads(crate::effects::Effect::Time, span, "`parse`");
        let imp = if ty == Ty::Date {
            Imp::DayParse
        } else {
            Imp::TimeParse
        };
        Ex::host(imp, vec![text, near], Ty::Option(Box::new(ty)))
    }

    pub(super) fn value_method(
        &mut self,
        r: Ex,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> Ex {
        let m = name.name.as_str();
        // The text and number surface first: it holds every method those values have (and the
        // text and number methods of a list), and its names are `methods`'.
        if let Some(ex) = self.text_math(r.clone(), name, args, span) {
            return ex;
        }
        match (&r.ty.clone(), m) {
            (Ty::Error, _) => {
                for a in args {
                    if !matches!(a.value.kind, ExprKind::Lambda { .. }) {
                        let _ = self.expr(&a.value, None);
                    }
                }
                Ex::error()
            }
            (Ty::RichText, "is_empty") if args.is_empty() => {
                Ex::host(Imp::RichEmpty, vec![r], Ty::Bool)
            }
            // The body of a response, and bytes, read as the text they decode as; bytes counted.
            (Ty::Response | Ty::Bytes, "text") if args.is_empty() => {
                Ex::host(Imp::WebText, vec![r], Ty::Text)
            }
            (Ty::Bytes, "len") if args.is_empty() => Ex::host(Imp::BytesLength, vec![r], Ty::Int),
            // A map read as a value (a `json.Value`'s object, or one `json.decode` read): its
            // keys, and the value one key holds.
            (Ty::Map(t), "get" | "keys") => {
                let value = t.clone();
                self.map_value_method(r, &value, name, args, span)
            }
            // A day read as people read it: the day of the week it is, the day, month and year
            // it names, and the first day of the week it stands in.
            (Ty::Date, "weekday") if args.is_empty() => {
                Ex::host(Imp::DayWeekday, vec![r], Ty::Choice("weekday"))
            }
            (Ty::Date, "day" | "month" | "year") if args.is_empty() => {
                let part = match m {
                    "day" => 0,
                    "month" => 1,
                    _ => 2,
                };
                let part = Ex::new(Ek::I32(part), Ty::Bool);
                Ex::host(Imp::DayPart, vec![r, part], Ty::Int)
            }
            (Ty::Date, "start_of_week") if args.is_empty() => {
                Ex::host(Imp::DayStartOfWeek, vec![r], Ty::Date)
            }
            // A moment read as people read it: the day it falls on for the person, in their
            // zone, and the time of day it is then.
            (Ty::Time, "day") if args.is_empty() => {
                self.reads(crate::effects::Effect::Time, span, "`day`");
                Ex::host(Imp::TimeDay, vec![r], Ty::Date)
            }
            (Ty::Time, "time_of_day") if args.is_empty() => {
                self.reads(crate::effects::Effect::Time, span, "`time_of_day`");
                Ex::host(Imp::TimeOfDay, vec![r], Ty::Duration)
            }
            // A length of time as the seconds it is, which is how a record keeps one.
            (Ty::Duration, "seconds") if args.is_empty() => r.typed(Ty::Int),
            // `r.next(from, done)`: the day the item comes round again. Counting from the due
            // day it steps on by the rule; counting from completion it steps from the day it was
            // done. Returns the day to be due next.
            (Ty::Repeat, "next") => {
                if !self.arity(name, args, 2, span) {
                    return Ex::error();
                }
                let (Some(from), Some(done)) = (args.first(), args.get(1)) else {
                    return Ex::error();
                };
                let from = self.expr_as(&from.value, &Ty::Date);
                let done = self.expr_as(&done.value, &Ty::Date);
                Ex::host(Imp::RepeatNext, vec![r, from, done], Ty::Date)
            }
            // `r.next_at(at, day)`: the moment on `day` at the same local time of day as `at`,
            // so a standup at 09:30 stays at 09:30 across a change of clocks.
            (Ty::Repeat, "next_at") => {
                if !self.arity(name, args, 2, span) {
                    return Ex::error();
                }
                let (Some(at), Some(day)) = (args.first(), args.get(1)) else {
                    return Ex::error();
                };
                let at = self.expr_as(&at.value, &Ty::Time);
                let day = self.expr_as(&day.value, &Ty::Date);
                self.reads(crate::effects::Effect::Time, span, "`next_at`");
                Ex::host(Imp::RepeatNextAt, vec![r, at, day], Ty::Time)
            }
            (Ty::Repeat, "label") if args.is_empty() => {
                Ex::host(Imp::RepeatLabel, vec![r], Ty::Text)
            }
            // `m.at(due, at)`: the moment to remind, from the due day and its time of day: an
            // item without one is timed from the start of the day.
            (Ty::Reminder, "at") => {
                if !self.arity(name, args, 2, span) {
                    return Ex::error();
                }
                let (Some(due), Some(at)) = (args.first(), args.get(1)) else {
                    return Ex::error();
                };
                let due = self.expr_as(&due.value, &Ty::Date);
                let at = self.expr_as(&at.value, &Ty::Option(Box::new(Ty::Time)));
                self.reads(crate::effects::Effect::Time, span, "`at`");
                Ex::host(Imp::ReminderAt, vec![r, due, at], Ty::Time)
            }
            (Ty::Reminder, "label") if args.is_empty() => {
                Ex::host(Imp::ReminderLabel, vec![r], Ty::Text)
            }
            (Ty::Principal, "name") if args.is_empty() => {
                Ex::host(Imp::PrincipalName, vec![r], Ty::Text)
            }
            (Ty::Principal, "kind") if args.is_empty() => {
                Ex::host(Imp::PrincipalKind, vec![r], Ty::Choice("principal_kind"))
            }
            (Ty::Principal, "role") if args.is_empty() => {
                Ex::host(Imp::PrincipalRole, vec![r], Ty::Option(Box::new(Ty::Text)))
            }
            (Ty::Record(_), "id") if args.is_empty() => Ex::host(Imp::RefId, vec![r], Ty::Text),
            // `as.many()`: an action that applies each of `as` to its own item, all at once:
            // one change, one undo.
            (Ty::List(inner), "many") if args.is_empty() && matches!(**inner, Ty::Action) => {
                Ex::host(Imp::ActMany, vec![r], Ty::Action)
            }
            // `a.then(b)`: an action that applies `a`, then `b` once `a` is done.
            (Ty::Action, "then") => {
                let [next] = args else {
                    self.err("E022", span, "`.then` takes the action to apply next: `send(c, draft).then(set(draft, \"\"))`");
                    return Ex::error();
                };
                let next = self.expr_as(&next.value, &Ty::Action);
                Ex::host(Imp::ActThen, vec![r, next], Ty::Action)
            }
            // `a.made_from(r)`: the action `a`, invoked made from the resource `r`: what the
            // resources it makes record as what they were made from (`created(r)?.from`).
            (Ty::Action, "made_from") => {
                let [from] = args else {
                    self.err("E022", span, "`.made_from` takes the resource the action is made from: `comment(comments).made_from(t)`");
                    return Ex::error();
                };
                if let Some(name) = &from.name {
                    self.err(
                        "E022",
                        name.span,
                        "`.made_from` takes its resource by position: `comment(comments).made_from(t)`",
                    );
                }
                let from = self.expr_as(&from.value, &Ty::Record(super::resource_key()));
                Ex::host(Imp::ActMadeFrom, vec![r, from], Ty::Action)
            }
            (Ty::Enum(key), "label" | "icon" | "tone" | "name") if args.is_empty() => {
                let key = key.clone();
                self.enum_method(r, &key, m).unwrap_or_else(Ex::error)
            }
            (Ty::List(_) | Ty::Set(_) | Ty::Collection(_), "len") if args.is_empty() => {
                Ex::host(Imp::ListLen, vec![r], Ty::Int)
            }
            // How many it holds, once the store has read the whole workspace: none until then,
            // so a window that counts what it shows never shows a part as the whole
            // ([`Imp::CollCounted`]).
            (Ty::List(_) | Ty::Set(_) | Ty::Collection(_), "counted") if args.is_empty() => {
                Ex::host(Imp::CollCounted, vec![r], Ty::Option(Box::new(Ty::Int)))
            }
            (Ty::List(_) | Ty::Set(_) | Ty::Collection(_), "is_empty") if args.is_empty() => {
                // Whether it holds nothing reads whether it shows a first item: a filtered
                // collection's length is only the most it can hold, so measuring it said an
                // empty one held something ([`Imp::CollEmpty`]), and counting it read the run.
                Ex::host(Imp::CollEmpty, vec![r], Ty::Bool)
            }
            (
                Ty::List(t) | Ty::Set(t) | Ty::Collection(t),
                "map" | "flat_map" | "filter" | "any" | "all" | "find" | "count",
            ) => {
                let elem = (**t).clone();
                self.higher_order(r, &elem, name, args, span)
            }
            (Ty::List(t) | Ty::Set(t) | Ty::Collection(t), "first" | "last") if args.is_empty() => {
                let elem = (**t).clone();
                Self::end_item(r, &elem, m == "last")
            }
            (Ty::List(t) | Ty::Collection(t), "descendants" | "ancestors") => {
                let elem = (**t).clone();
                self.relatives(r, &elem, name, args, span, None)
            }
            // `items.index_of(x)`: where it stands among the items, or none when it is not one.
            (Ty::List(t) | Ty::Set(t) | Ty::Collection(t), "index_of") => {
                let elem = (**t).clone();
                if !self.arity(name, args, 1, span) {
                    return Ex::error();
                }
                let Some(a) = args.first() else {
                    return Ex::error();
                };
                let what = Self::handle_of(self.expr_as(&a.value, &elem));
                Ex::host(
                    Imp::ListIndexOf,
                    vec![r, what],
                    Ty::Option(Box::new(Ty::Int)),
                )
            }
            (Ty::List(t) | Ty::Collection(t), "at") => {
                let elem = (**t).clone();
                if !self.arity(name, args, 1, span) {
                    return Ex::error();
                }
                let Some(a) = args.first() else {
                    return Ex::error();
                };
                self.item_at(r, &elem, a)
            }
            // `items.previous(x)`: the item just before `x` in the list as a view shows it —
            // the row over the one a list is rendering — or none for the first. A row's place
            // is the host's, so a row tells what stands over it without walking the list.
            (Ty::List(t) | Ty::Collection(t), "previous") => {
                let elem = (**t).clone();
                if !self.arity(name, args, 1, span) {
                    return Ex::error();
                }
                let Some(a) = args.first() else {
                    return Ex::error();
                };
                let what = self.expr_as(&a.value, &elem);
                Ex::host(Imp::ListPrevious, vec![r, what], Ty::Option(Box::new(elem)))
            }
            (Ty::List(_), "of") => self.list_of(r, args, span),
            (Ty::List(t) | Ty::Collection(t), "matching") => {
                let elem = (**t).clone();
                self.matching(r, &elem, args, span)
            }
            (Ty::List(t) | Ty::Set(t) | Ty::Collection(t), "sort_by") => {
                let elem = (**t).clone();
                self.sort_by(r, &elem, name, args, span)
            }
            (
                Ty::List(t) | Ty::Set(t) | Ty::Collection(t),
                "count_by" | "group" | "take" | "each" | "single",
            ) => {
                let elem = (**t).clone();
                self.list_chain(r, &elem, name, args, span)
                    .unwrap_or_else(Ex::error)
            }
            (Ty::Record(k), "group") if k.app == crate::host::HOST_APP && k.name == "Match" => {
                self.match_group(r, args, span)
            }
            (Ty::Record(k), "is")
                if k.app == crate::host::HOST_APP
                    && crate::host::NAMES_AN_ACTION.contains(&k.name.as_str()) =>
            {
                let name = k.name.clone();
                self.names_action(r, &name, args, span)
            }
            (other, _) => {
                let t = self.show(other);
                let known = match other {
                    Ty::Repeat => "next, next_at, label".to_owned(),
                    Ty::Reminder => "at, label".to_owned(),
                    Ty::Principal => "name, kind, role".to_owned(),
                    Ty::Action => "then, made_from".to_owned(),
                    // Every other type's methods are in one table, whose names the check of a
                    // container field reads too.
                    other => super::methods::described(other),
                };
                let words = known.split([',', ' ', '(', ')']).filter(|w| {
                    !w.is_empty() && w.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                });
                let hint = crate::diag::suggestion(m, words);
                self.err(
                    "E027",
                    name.span,
                    format!(
                        "{t} has no method `{m}` taking {} argument(s){hint}; its methods: {known}",
                        args.len()
                    ),
                );
                for a in args {
                    if !matches!(a.value.kind, ExprKind::Lambda { .. }) {
                        let _ = self.expr(&a.value, None);
                    }
                }
                Ex::error()
            }
        }
    }
}

/// The type a text names, where it is written before a method instead of a value:
/// `Date.parse(...)`, `Time.parse(...)`.
fn parse_ty(name: &str) -> Option<Ty> {
    match name {
        "Date" => Some(Ty::Date),
        "Time" => Some(Ty::Time),
        _ => None,
    }
}

/// Whether the callee is `json.X`: the one module whose calls take a type argument.
fn names_json(callee: &ast::Expr) -> bool {
    matches!(&callee.kind, ExprKind::Field { base, .. } if matches!(&base.kind, ExprKind::Ident(ns) if ns == "json"))
}
