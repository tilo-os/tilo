//! Calls: functions, actions, record construction, built-ins and methods.

mod collections;
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

const RANDOM: &str = "random";

const FILE: &str = "file";

const CLIPBOARD: &str = "clipboard";

const PAGE: &str = "page";

const PDF: &str = "pdf";

/// The device's own work, asked by name (`engine.call`, `engine.start`, `engine.status`,
/// `engine.cancel`).
const ENGINE: &str = "engine";

impl Checker<'_> {
    pub(super) fn call(
        &mut self,
        callee: &ast::Expr,
        types: &[ast::TypeExpr],
        args: &[ast::Arg],
        span: Span,
    ) -> Ex {
        if let ExprKind::Ident(name) = &callee.kind
            && name == "Map"
            && self.lookup(name).is_none()
            && self.own_callable(name).is_none()
            && self.env.own_type(name).is_none()
        {
            return self.builtin_map(types, args, span);
        }
        if !types.is_empty() && !names_json(callee) {
            self.err(
                "E022",
                span,
                "only `json.decode<T>(text)` and `Map<T>()` take a type between `<` and `>`",
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
                    if name.name == "Identifier" {
                        return match crate::resolve::std_identifier_value(args) {
                            Ok(text) => Ex::str(&text).typed(Ty::Identifier),
                            Err(msg) => {
                                self.err("E099", span, msg);
                                Ex::error()
                            }
                        };
                    }
                    // `std.say(...)`: words the code writes for a person, not a bare `say`, which
                    // a package's own action takes.
                    if name.name == "say" {
                        return self.builtin_say(args, span);
                    }
                    if name.name == "may_read" {
                        return self.builtin_may_read(args, span);
                    }
                    // `std.occurrences(rule, start, timezone, exdates, from, to)`: the moments a
                    // repeat falls on in a window, read from the rule alone.
                    if name.name == "occurrences" {
                        return self.builtin_occurrences(args, span);
                    }
                }
                if matches!(&base.kind, ExprKind::Ident(ns) if ns == RANDOM)
                    && self.lookup(RANDOM).is_none()
                    && self.env.find_import(RANDOM).is_none()
                {
                    return self.builtin_random(name, args, span);
                }
                if matches!(&base.kind, ExprKind::Ident(ns) if ns == "web")
                    && self.lookup("web").is_none()
                    && self.env.find_import("web").is_none()
                {
                    return self.builtin_web(name, args, span);
                }
                // `page.call(request)`: the page the window shows, which only an action acts on.
                if matches!(&base.kind, ExprKind::Ident(ns) if ns == PAGE)
                    && self.lookup(PAGE).is_none()
                    && self.env.find_import(PAGE).is_none()
                {
                    return self.builtin_page(name, args, span);
                }
                // `json.parse`, `json.decode<T>`, `json.encode`: JSON text, which is no effect.
                if matches!(&base.kind, ExprKind::Ident(ns) if ns == "json")
                    && self.lookup("json").is_none()
                    && self.env.find_import("json").is_none()
                {
                    return self.builtin_json(name, types, args, span);
                }
                if matches!(&base.kind, ExprKind::Ident(ns) if ns == FILE)
                    && self.lookup(FILE).is_none()
                    && self.env.find_import(FILE).is_none()
                {
                    return self.builtin_file(name, args, span);
                }
                if matches!(&base.kind, ExprKind::Ident(ns) if ns == CLIPBOARD)
                    && self.lookup(CLIPBOARD).is_none()
                    && self.env.find_import(CLIPBOARD).is_none()
                {
                    return self.builtin_clipboard(name, args, span);
                }
                if matches!(&base.kind, ExprKind::Ident(ns) if ns == PDF)
                    && self.lookup(PDF).is_none()
                    && self.env.find_import(PDF).is_none()
                {
                    return self.builtin_pdf(name, args, span);
                }
                // `engine.call`, `engine.start`, `engine.status`, `engine.cancel`: the device's
                // own work, asked by name. A view never asks: it is drawn again from what it is
                // shown, and the device's work is not what it was shown.
                if matches!(&base.kind, ExprKind::Ident(ns) if ns == ENGINE)
                    && self.lookup(ENGINE).is_none()
                    && self.env.find_import(ENGINE).is_none()
                {
                    return self.builtin_engine(name, args, span);
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
            return self.construct(&key, args, span);
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
        // `Duration(n)`: `n` seconds, for a length of time an app keeps as a number.
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
            "call_members" => return self.builtin_call_members(args, span),
            "invoked" => return self.builtin_invoked(args, span),
            "lines_run" => return self.builtin_lines_run(args, span),
            "run_line" => return self.builtin_run_line(args, span),
            "join_call" | "leave_call" | "mute_call" | "deafen_call" | "camera_call"
            | "screen_call" => return self.builtin_call(name, args, span),
            "delivery" => return self.builtin_delivery(args, span),
            "reach" => return self.builtin_reach(args, span),
            "members" => return self.builtin_members(args, span),
            "speakers" => return self.builtin_speakers(args, span),
            "created" | "updated" | "history" => {
                return self.builtin_versions(name, args, span);
            }
            "changed" => return self.builtin_changed(args, span),
            "unseen" => return self.builtin_unseen(args, span),
            "linked" => return self.builtin_links(args, span),
            "earlier" => return self.builtin_earlier(args, span),
            "proposal_note" => return self.builtin_proposal_note(args, span),
            "first_unseen" => return self.builtin_first_unseen(args, span),
            "seen_by" => return self.builtin_seen_by(args, span),
            "mentions" => return self.builtin_mentions(args, span),
            "undo" => return self.builtin_undo(args, span),
            "steps" => return self.builtin_steps(args, span),
            "changes" => return self.builtin_changes(args, span),
            "meta" => return self.builtin_meta(args, span),
            "cases" => return self.builtin_cases(args, span),
            "phrase" => return self.builtin_phrase(args, span),
            // `fonts()`: the families this device sets text in, as a reading face's picker
            // offers them.
            "fonts" => {
                if !args.is_empty() {
                    self.err(
                        "E022",
                        span,
                        "`fonts()` takes nothing: the families this device has",
                    );
                }
                for a in args {
                    let _ = self.expr(&a.value, None);
                }
                return Ex::host(Imp::FontFamilies, Vec::new(), Ty::List(Box::new(Ty::Text)));
            }
            "preview_rows" => return self.builtin_preview_rows(args, span),
            "entry_parts" => return self.builtin_entry_parts(args, span),
            "invoke" => return self.builtin_invoke(args, span),
            "stage" => return self.builtin_stage(args, span),
            "query" => return self.builtin_query(args, span),
            "filters" => return self.builtin_filters(args, span),
            _ => {}
        }
        if prims::prim(name).is_some() {
            // A program asks for what it reads: what a primitive would show is read or printed.
            let instead = if self.st.ctx == Ctx::Program {
                "; a program reads a resource's text with `read(r)`, a field by its name \
                 (`r.body`), and shows values with `print(...)`"
            } else {
                ""
            };
            self.err("E061", name_span, format!("`{name}` is a UI primitive; it builds UI and can only be written as a statement of a view{instead}"));
            return Ex::error();
        }
        if name == "say" {
            self.err(
                "E014",
                name_span,
                "the words marker is `std.say(\"Morning: 3 done\")`, in the library's namespace, since a bare `say` is a name a package can take for an action of its own",
            );
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
        // The agent library's words are host imports the checker lowers, not calls into a module.
        if import.app == crate::agent::APP
            && let Some(ex) = self.agent_call(&name.name, args, span)
        {
            return ex;
        }
        let env = self.env;
        if let Some(c) =
            env.foreign_callable_marked(&import.app, &name.name, name.from_template_of.as_deref())
        {
            let c = c.clone();
            return self.call_callable(&c, args, span);
        }
        let key = TypeKey {
            app: import.app.clone(),
            name: name.name.clone(),
        };
        if env.type_info(&key).is_some() {
            return self.construct(&key, args, span);
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

    /// As [`Self::bind_args`], but a parameter with a default may be left out, as `None`.
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
        // An action a view applies may leave out its last parameters: running it opens its form,
        // which asks for them. A `File` parameter is no form's: a chooser fills it.
        let choosing = self.st.choosing;
        let trailing_none = |rest: &[Option<Ex>]| rest.iter().all(Option::is_none);
        let filled_by_a_chooser = |at: usize| {
            choosing
                && params
                    .get(at)
                    .is_some_and(|p| p.ty == Ty::File || p.ty == Ty::List(Box::new(Ty::File)))
                && slots.get(at..).is_some_and(trailing_none)
        };
        let asked = |at: usize| {
            (at_press || filled_by_a_chooser(at))
                && slots
                    .get(at..)
                    .is_some_and(|rest| rest.iter().all(Option::is_none))
                && params.get(at..).is_some_and(|rest| {
                    rest.iter().all(|p| {
                        p.default.is_some()
                            || p.ty.asked_by_form()
                            || p.ty == Ty::File
                            || p.ty == Ty::List(Box::new(Ty::File))
                    })
                })
        };
        let missing: Vec<&str> = params
            .iter()
            .zip(&slots)
            .enumerate()
            .filter(|(at, (p, s))| s.is_none() && p.default.is_none() && !asked(*at))
            .map(|(_, (p, _))| p.name.as_str())
            .collect();
        // A parameter no form can ask for says so, rather than reading as one the person forgot.
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
                    && !filled_by_a_chooser(*at)
            })
            .map(|(_, (p, _))| p)
            .collect();
        if let Some(param) = unaskable.first() {
            let said = if param.ty == Ty::File || param.ty == Ty::List(Box::new(Ty::File)) {
                format!(
                    "`{name}` cannot be asked for `{}`: a file is the person's to choose, so the action run from a control with `file.choose({name}(…))` (or `file.choose_folder(…)` for the files of a folder) gets it, and a call written as it is must give it one",
                    param.name
                )
            } else {
                format!(
                    "`{name}` cannot be asked for `{}` of type `{}`: a form asks for text,                      numbers, flags, days, moments, lengths of time, accounts, people, resources                      and enum cases, so give it here or give the parameter a default",
                    param.name,
                    param.ty.display(&self.env.app)
                )
            };
            self.err("E022", span, said);
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

    /// An argument of an action a view applies, of type `want`. View state given as itself is read
    /// when the event fires, and an argument that computes from view state (`set(open, not open)`)
    /// is a function the runtime runs then ([`Self::at_event`]). Any other argument is the value
    /// the view drew with.
    pub(super) fn arg_at_press(&mut self, e: &ast::Expr, want: &Ty) -> Ex {
        if let ExprKind::Ident(name) = &e.kind
            && let Some(state) = self.state_named(name)
            && (state.ty == *want || want.is_error())
        {
            return Self::state_at(&state);
        }
        if let ExprKind::Ident(name) = &e.kind
            && let Some((local, ty)) = self.lookup(name)
            && self.st.event_scalars.contains(&local)
            && (ty == Ty::Option(Box::new(want.clone())) || want.is_error())
        {
            return Ex::local(local, ty);
        }
        let mut names = std::collections::BTreeSet::new();
        super::boundary::names_in(e, &mut names);
        // An action value computes nothing from view state itself: the applications in it take
        // their own arguments at the press. The clock is read then too.
        let reads_state = names
            .iter()
            .any(|n| self.state_named(n).is_some() || self.clock_named(n));
        // What an event carries is in place only when it fires: a value computed from it
        // (`set(expr, x.expr)`) is computed then. Given as itself, it is filled in place.
        let reads_event = !matches!(e.kind, ExprKind::Ident(_))
            && names.iter().any(|n| {
                self.lookup(n)
                    .is_some_and(|(local, _)| self.st.event_values.contains(&local))
            });
        if (reads_state || reads_event) && !matches!(want, Ty::Action) {
            return self.at_event(e, want, &names);
        }
        self.expr_as(e, want)
    }

    /// Whether `name` in an action's argument is the clock (`today`, `now`), read when the action
    /// is invoked.
    fn clock_named(&self, name: &str) -> bool {
        matches!(name, "today" | "now")
            && self.lookup(name).is_none()
            && self.state_named(name).is_none()
    }

    /// The argument `e` of type `want`, which reads view state among `names`, as a function the
    /// runtime runs when the event fires ([`ir::AtEventFn`]). It is checked as a parameter default
    /// is, but for the clock: it reads `today` and `now` then, recorded with the invocation's
    /// arguments.
    fn at_event(
        &mut self,
        e: &ast::Expr,
        want: &Ty,
        names: &std::collections::BTreeSet<String>,
    ) -> Ex {
        let mut params = Vec::new();
        let mut args = Vec::new();
        // The scalars an event carries come boxed until it fires; the function reads them plain.
        let mut boxed = Vec::new();
        for name in names {
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
            params.push(crate::resolve::ParamInfo {
                name: name.clone(),
                ty,
                default: None,
                place: false,
                label: None,
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
        self.st.press = true;
        for name in &boxed {
            if let Some((local, Ty::Option(inner))) = self.lookup(name) {
                let plain = self.declare(name, (*inner).clone());
                self.emit(Stmt::Set(
                    plain,
                    Self::unwrap_opt(Ex::local(local, Ty::Option(inner.clone())), &inner),
                ));
            }
        }
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
        // An action a view applies runs when its event fires: the view state it takes is read then.
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
            (CKind::Action, _) if c.present.role.as_deref() == Some("initial") => {
                let target = c
                    .params
                    .first()
                    .map_or_else(String::new, |p| p.ty.to_string());
                self.err("E143", span, format!("`{}` is what a new `{target}` starts with: the platform writes it as each one is made, and nothing invokes it. Insert the `{target}` where it belongs", c.name));
                Ex::error()
            }
            (CKind::Action, _) if c.present.role.as_deref() == Some("opened") => {
                let target = c
                    .params
                    .first()
                    .map_or_else(String::new, |p| p.ty.to_string());
                self.err("E143", span, format!("`{}` is what opening a `{target}` does: the platform runs it as the person opens one, wherever from, and nothing invokes it. Open the `{target}` instead (`open(x)`)", c.name));
                Ex::error()
            }
            (CKind::Action, Ctx::Action) if self.st.role.as_deref() == Some("initial") => {
                self.err("E143", span, format!("an initial action is part of making its resource, one change, and `{}` is an action, a change of its own. Write what it does in the initial action itself", c.name));
                Ex::error()
            }
            (CKind::Action, Ctx::Action | Ctx::Program) => {
                // A call runs the action at its own action API, as a change of its own, and goes on from
                // its journaled result. `suggest act(args)` is the same call, recorded as a
                // proposal instead of applied.
                let bound = Self::applied_args(c, bound);
                let from = self
                    .st
                    .made_from
                    .take_if(|(at, _)| *at == span)
                    .map(|(_, from)| from);
                self.invoke(c, bound, from)
            }
            (CKind::Action, Ctx::View | Ctx::Block) => {
                let bound = Self::applied_args(c, bound);
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

    /// A `fn`: the module's own is a call, another package's runs there as its own code. A fn
    /// changes nothing, so a call of one asks nobody; the host boxes a scalar result like an
    /// action's.
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
        // A block's whole expression is the proposal: the host records it as one instead of
        // applying it.
        self.st.suggest = None;
        Ex::host(Imp::ActRef, vec![Ex::str(&c.id), list], Ty::Action)
    }

    fn invoke(&mut self, c: &Callable, bound: Vec<Ex>, from: Option<Ex>) -> Ex {
        self.st.facts.invokes.insert(Self::action_ref(c));
        let list = self.arg_list(bound);
        let mut act = Ex::host(Imp::ActRef, vec![Ex::str(&c.id), list], Ty::Action);
        if let Some(from) = from {
            act = Ex::host(Imp::ActMadeFrom, vec![act, from], Ty::Action);
        }
        if let Some(value) = self.st.suggest.take() {
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

    /// Whether `e` writes a record here (`Note(title: ...)`) rather than naming one the store
    /// holds.
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
    fn construct(&mut self, key: &TypeKey, args: &[ast::Arg], span: Span) -> Ex {
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
            // A field holding a resource takes one the store has: an action's result, a root, a
            // reference. A record written here has no id until the store makes it, so nothing can
            // hold it: make it first (`let n = notes.new_note(...)`) and give that.
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
        // A record field it leaves out starts holding a record of its own when it is made.
        self.check_held(key, &given, span);
        Ex::new(Ek::Seq(stmts, Box::new(Ex::local(tmp, ty.clone()))), ty)
    }

    /// `person("Mia")` in a scenario: an actor whose id is `scenario:Mia`; nothing else makes
    /// one from text. `RichText("Ask ", ada, " about ", task)`: text with a run naming each person
    /// or resource it refers to, and each time read in the reader's own zone and language.
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
            if v.ty == Ty::Time {
                v = Ex::host(Imp::RichTime, vec![v], Ty::RichText);
            }
            let fits = matches!(
                v.ty,
                Ty::Text | Ty::RichText | Ty::Actor | Ty::Record(_) | Ty::Error
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

    /// `stage(device.sound, device.Sound(volume: 0.4))` in a scenario: the reading another app
    /// publishes of its resource, as the scenario stands it in for that app.
    fn builtin_stage(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        if self.st.written != Written::Scenario {
            self.err("E094", span, "`stage(resource, reading)` stands a reading in for the app that publishes it, which only a scenario does");
            for a in args {
                let _ = self.expr(&a.value, None);
            }
            return Ex::error();
        }
        let [
            ast::Arg {
                name: None,
                value: target,
            },
            ast::Arg {
                name: None,
                value: reading,
            },
        ] = args
        else {
            self.err(
                "E022",
                span,
                "`stage` takes a resource of another app and its reading: `stage(device.sound, device.Sound(volume: 0.4))`",
            );
            return Ex::error();
        };
        let target = self.expr(target, None);
        let Ty::Record(key) = &target.ty else {
            if !target.ty.is_error() {
                self.err(
                    "E020",
                    span,
                    "`stage` takes a resource of another app first",
                );
            }
            return Ex::error();
        };
        if key.app == self.env.app {
            self.err(
                "E020",
                span,
                format!(
                    "`{}` is this app's own: a scenario writes its fields",
                    key.name
                ),
            );
            return Ex::error();
        }
        let ty = target.ty.clone();
        let reading = self.expr_as(reading, &ty);
        Ex::host(Imp::StageReading, vec![target, reading], Ty::Unit)
    }

    fn builtin_person(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        if self.st.written != Written::Scenario {
            self.err("E094", span, "`person(name)` makes a sample person for a scenario; elsewhere an actor comes only from `invoker` or a stored field");
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
        Ex::host(Imp::PersonNamed, vec![name], Ty::Actor)
    }

    /// `agent(name)`: software in the workspace a scenario stages, offered beside its people.
    fn builtin_agent(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        if self.st.written != Written::Scenario {
            self.err("E094", span, "`agent(name)` makes sample software for a scenario; elsewhere an actor comes only from `invoker` or a stored field");
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
        Ex::host(Imp::AgentNamed, vec![name], Ty::Actor)
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
                 give it the resource the effect is about (`action go(d: Shell, …)`)",
            );
            return Ex::error();
        }
        // `outside(capability, payload)`, or `outside(capability, payload, at: <a Time>)`: the
        // device carries it out once the clock reaches it.
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
        self.st.facts.outside_caps.insert(name.clone());
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
    /// carried out yet, settling each as withdrawn for `reason`.
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

    /// `run_block(part, source)`: after this action commits, the host evaluates `source` as a block
    /// the invoker wrote, and records what it applies as an `Invocation` in the part's `invocation`
    /// field.
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

    /// `set(name, value)`: an action reference that changes the view state `name`. Views only.
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

    pub(super) fn set_action(state: &super::StateRef, v: Ex) -> Ex {
        Ex::host(
            Imp::ActSet,
            vec![Ex::str(&state.id), Self::handle_of(v)],
            Ty::Action,
        )
    }

    /// `open(r)`, or `open(r, view: v)`: an action reference that goes to the window of `r`. Views
    /// only.
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
                // window.
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
        // A type the package declares no default view of: opening one of its resources shows the
        // platform's page of its fields, and the app cannot say what that page should hold.
        if view_id.is_empty()
            && key.app == self.env.app
            && !self
                .env
                .default_views
                .iter()
                .any(|(declared, _)| *declared == key)
        {
            self.err(
                "E141",
                target.value.span,
                format!(
                    "`{}` declares no default view, so opening one of its resources shows the \
                     platform's page of its fields; add `default view <view> for {0}`, or open a \
                     view of it here (`open(r, view: <view>)`)",
                    key.name
                ),
            );
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
                self.env.foreign_callable_marked(&import.app, last, None)?
            }
            [] => return None,
        };
        let fits = c.kind == CKind::View
            && matches!(c.params.as_slice(), [p] if p.ty == Ty::Record(key.clone()));
        fits.then(|| c.id.clone())
    }

    /// `file.choose(a, accept: [...])`: the action `a` run with the file the person picks.
    /// `file.save(a, name: "...")`: the action run, and what it made put in a file the person
    /// picks. Both stand on a control, as `open` does.
    fn builtin_file(&mut self, name: &ast::Ident, args: &[ast::Arg], span: Span) -> Ex {
        if self.st.ctx != Ctx::View {
            let who = self.st.ctx.word();
            self.err(
                "E067",
                span,
                format!(
                    "`file.{}` is applied from a view, on a control: it asks the person at the window; {who} cannot",
                    name.name
                ),
            );
            return Ex::error();
        }
        let choosing = match name.name.as_str() {
            "choose" | "choose_folder" => true,
            "save" => false,
            other => {
                self.err(
                    "E026",
                    name.span,
                    format!("`file` has `choose`, `choose_folder` and `save`, not `{other}`"),
                );
                return Ex::error();
            }
        };
        let folder = name.name == "choose_folder";
        let mut action = None;
        let mut accept = None;
        let mut called = None;
        for a in args {
            match a.name.as_ref().map(|n| n.name.as_str()) {
                None if action.is_none() => {
                    // The action the chooser runs is applied with the file it fills: its first
                    // parameter left out that is one.
                    self.st.choosing |= choosing;
                    let applied = self.expr_as(&a.value, &Ty::Action);
                    self.st.choosing = false;
                    action = Some(applied);
                }
                None => {
                    self.err(
                        "E022",
                        a.value.span,
                        format!(
                            "`file.{}` takes one action, the one it runs: `file.{}({})`",
                            name.name,
                            name.name,
                            if choosing {
                                "insert_image(n)"
                            } else {
                                "export(n)"
                            }
                        ),
                    );
                }
                Some(kind @ ("accept" | "name")) if (kind == "accept") == choosing => {
                    if choosing {
                        accept = Some(self.expr_as(&a.value, &Ty::List(Box::new(Ty::Text))));
                    } else {
                        called = Some(self.expr_as(&a.value, &Ty::Text));
                    }
                }
                Some(other) => {
                    self.err(
                        "E022",
                        a.value.span,
                        format!(
                            "`file.{}` takes {}; not `{other}:`",
                            name.name,
                            if choosing { "`accept:`" } else { "`name:`" }
                        ),
                    );
                }
            }
        }
        let Some(action) = action else {
            self.err(
                "E022",
                span,
                format!(
                    "`file.{}` takes the action it runs: `file.{}({})`",
                    name.name,
                    name.name,
                    if choosing {
                        "insert_image(n)"
                    } else {
                        "export(n)"
                    }
                ),
            );
            return Ex::error();
        };
        if choosing {
            let accept = accept.unwrap_or_else(|| {
                Ex::host(Imp::ListNew, Vec::new(), Ty::List(Box::new(Ty::Text)))
            });
            let imp = if folder {
                Imp::ActChooseFolder
            } else {
                Imp::ActChoose
            };
            Ex::host(imp, vec![action, accept], Ty::Action)
        } else {
            let called = match called {
                Some(called) => called,
                None => {
                    self.err(
                        "E022",
                        span,
                        "`file.save` says what the file is called: `file.save(export(n), name: \"Notes.md\")`",
                    );
                    Ex::str("")
                }
            };
            Ex::host(Imp::ActSave, vec![action, called], Ty::Action)
        }
    }

    /// `notify(to: who, title: text, body: text, opens: r)`: tell the person `to` something on
    /// their device, and open `r` when they choose it. The invocation records it.
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
                "to" => to = Some(self.expr_as(&arg.value, &Ty::Actor)),
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

    pub(super) fn method(
        &mut self,
        recv: &ast::Expr,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> Ex {
        // `Date.parse(text, near: now)` stands on the type, which is not a value, so it is read
        // before the receiver.
        if let ExprKind::Ident(n) = &recv.kind
            && self.lookup(n).is_none()
            && !self.names_import(recv)
            && let Some(ty) = parse_ty(n)
        {
            return self.type_method(ty, name, args, span);
        }
        // `act(args).made_from(r)` in an action: the call runs as a change of its own, made from
        // `r`, which what it makes records (`created(x)?.from`).
        if name.name == "made_from"
            && matches!(self.st.ctx, Ctx::Action | Ctx::Program)
            && matches!(recv.kind, ExprKind::Call { .. })
        {
            return self.invoke_made_from(recv, args, span);
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

    /// `act(args).made_from(r)` in an action or a program: the call of `act`, made from `r`.
    fn invoke_made_from(&mut self, call: &ast::Expr, args: &[ast::Arg], span: Span) -> Ex {
        let [from] = args else {
            self.err(
                "E022",
                span,
                "`.made_from` takes the resource the action is made from: `agent.ask(all, who, said).made_from(m)`",
            );
            return Ex::error();
        };
        if let Some(name) = &from.name {
            self.err(
                "E022",
                name.span,
                "`.made_from` takes its resource by position: `agent.ask(all, who, said).made_from(m)`",
            );
        }
        let from = self.expr_as(&from.value, &Ty::Record(super::resource_key()));
        let outer = self.st.made_from.replace((call.span, from));
        let made = self.expr(call, None);
        if std::mem::replace(&mut self.st.made_from, outer).is_some() {
            self.err(
                "E022",
                call.span,
                "`.made_from` follows the call of an action: `agent.ask(all, who, said).made_from(m)`",
            );
            return Ex::error();
        }
        made
    }

    /// Whether `e` is `space` or `workspace`, read only through their fields, and not a variable of
    /// that name.
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

    /// The arguments `wanted` of a call that names them, each once (`r.markdown(links: pages, from:
    /// folder)`).
    fn named_args<'a>(
        &mut self,
        args: &'a [ast::Arg],
        wanted: &[&str],
        form: &str,
        span: Span,
    ) -> Option<Vec<&'a ast::Arg>> {
        let mut held: Vec<Option<&ast::Arg>> = vec![None; wanted.len()];
        let mut whole = args.len() == wanted.len();
        for a in args {
            match a
                .name
                .as_ref()
                .and_then(|n| wanted.iter().position(|w| *w == n.name))
            {
                Some(at) if held[at].is_none() => held[at] = Some(a),
                _ => whole = false,
            }
        }
        if whole && held.iter().all(Option::is_some) {
            return held.into_iter().collect();
        }
        self.err("E022", span, format!("`{form}`"));
        for a in args {
            let _ = self.expr(&a.value, None);
        }
        None
    }

    /// `Map<Text>()`: a map the code fills while it runs, unlike one a record keeps as a field.
    fn builtin_map(&mut self, types: &[ast::TypeExpr], args: &[ast::Arg], span: Span) -> Ex {
        if !args.is_empty() {
            self.err(
                "E022",
                span,
                "`Map<T>()` starts empty and takes nothing: `Map<Text>()`",
            );
            for a in args {
                let _ = self.expr(&a.value, None);
            }
            return Ex::error();
        }
        let Some(written) = types.first() else {
            self.err(
                "E022",
                span,
                "`Map()` says what its keys lead to: `Map<Text>()`, a text by each key",
            );
            return Ex::error();
        };
        let mut diags = Vec::new();
        let held = crate::resolve::resolve_type_expr(self.env, written, &mut diags);
        for diagnostic in diags {
            self.err(&diagnostic.code, diagnostic.span, diagnostic.message);
        }
        if held.is_error() {
            return Ex::error();
        }
        Ex::host(Imp::MapNew, Vec::new(), Ty::Map(Box::new(held)))
    }

    const DRAWS: &'static str = "`random.int(lo, hi)`, `random.float()`, `random.choice(xs)`, `random.shuffle(xs)` and `random.id()`";

    /// `random.int(lo, hi)`, `random.float()`, `random.choice(xs)`, `random.shuffle(xs)` and
    /// `random.id()`: only an action makes a draw, declaring `uses random`, and its invocation
    /// keeps each value, so a replay reads the same.
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

    /// `web.get(url, headers:, if_none_match:)` and `web.post(url, body:, headers:)`: a request to
    /// a service outside Tilo, which the invocation makes once, records, and replays from the
    /// record.
    /// `page.call(request)`: the page the app's window shows does what the request says, and
    /// answers what it found as JSON text. Only an action acts on the page: a view is drawn
    /// again from what it is shown, and reading a page is not that.
    fn builtin_page(&mut self, name: &ast::Ident, args: &[ast::Arg], span: Span) -> Ex {
        if name.name != "call" {
            self.err(
                "E022",
                name.span,
                format!("`page` has `call`; not `{}`", name.name),
            );
            for a in args {
                let _ = self.expr(&a.value, None);
            }
            return Ex::error();
        }
        if self.st.ctx != Ctx::Action {
            let who = self.st.ctx.word();
            self.err(
                "E047",
                span,
                format!("`page.call` acts on the page, and only an action may; {who} may not"),
            );
            return Ex::error();
        }
        let Some(request) = args.first().filter(|a| a.name.is_none()) else {
            self.err(
                "E022",
                span,
                "`page.call` takes the request first: `page.call(\"{\\\"request\\\": \\\"outline\\\"}\")`",
            );
            for a in args {
                let _ = self.expr(&a.value, None);
            }
            return Ex::error();
        };
        if args.len() > 1 {
            self.err(
                "E022",
                span,
                "`page.call` takes one argument, the request as JSON text",
            );
        }
        let request = self.expr_as(&request.value, &Ty::Text);
        self.reads(crate::effects::Effect::Page, span, "`page.call`");
        Ex::host(Imp::PageCall, vec![Self::handle_of(request)], Ty::Text)
    }

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

    /// `json.parse(text)`, `json.decode<T>(text)` and `json.encode(value)`. No effect: it reaches no
    /// service and reads no clock, so an action that only reads text it was given needs no `uses`.
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

    /// A method that stands on a type instead of a value: `Date.parse` and `Time.parse` (the day or
    /// the moment a text names, read at a moment in the person's zone), `RichText.from_markdown`,
    /// and `File`'s.
    fn type_method(&mut self, ty: Ty, name: &ast::Ident, args: &[ast::Arg], span: Span) -> Ex {
        let t = self.show(&ty);
        if ty == Ty::RichText {
            return self.rich_type_method(name, args, span);
        }
        if ty == Ty::File {
            return self.file_type_method(name, args, span);
        }
        // `Time.from_wall(zone, text)`: the moment a wall-clock time names where people in `zone`
        // are, a pure reading of the zone's rules, so a view draws it and an action that imports
        // a calendar needs no clock to say when its events are.
        if ty == Ty::Time && name.name == "from_wall" {
            let [zone, text] = args else {
                self.err(
                    "E022",
                    span,
                    "`Time.from_wall` takes the zone and the wall-clock time: `Time.from_wall(\"Europe/Berlin\", \"2026-11-24T09:00\")`",
                );
                for a in args {
                    let _ = self.expr(&a.value, None);
                }
                return Ex::error();
            };
            for arg in [zone, text] {
                if let Some(named) = &arg.name {
                    self.err(
                        "E022",
                        named.span,
                        "`Time.from_wall` takes the zone and the time by position",
                    );
                }
            }
            let zone = self.expr_as(&zone.value, &Ty::Text);
            let text = self.expr_as(&text.value, &Ty::Text);
            return Ex::host(
                Imp::TimeFromWall,
                vec![zone, text],
                Ty::Option(Box::new(Ty::Time)),
            );
        }
        if ty == Ty::Bytes {
            return self.bytes_type_method(name, args, span);
        }
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
        // The moment is named as the text it is read at.
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

    /// `RichText.from_markdown(text)` and `RichText.embed(r)`: pure, as a value's method is.
    fn rich_type_method(&mut self, name: &ast::Ident, args: &[ast::Arg], span: Span) -> Ex {
        if name.name == "embed" {
            let Some(r) = self.resource_arg("RichText.embed", args, span) else {
                return Ex::error();
            };
            return Ex::host(Imp::RichEmbed, vec![r], Ty::RichText);
        }
        if name.name != "from_markdown" {
            self.err(
                "E027",
                name.span,
                "`RichText` is a type, not a value; its methods of its own are `.from_markdown(text)`, the document a Markdown text says, and `.embed(r)`, the text projecting the resource `r`, and `RichText(…)` builds a value",
            );
            for a in args {
                let _ = self.expr(&a.value, None);
            }
            return Ex::error();
        }
        // The text it reads, then the address each page stands at by the note it is, and the folder
        // this one is written in: a link to a page reads as the reference to it.
        let mut text: Option<Ex> = None;
        let mut links: Option<Ex> = None;
        let mut from: Option<Ex> = None;
        for a in args {
            match a.name.as_ref().map(|n| n.name.as_str()) {
                None => {
                    if text.replace(self.expr_as(&a.value, &Ty::Text)).is_some() {
                        self.err(
                            "E022",
                            span,
                            "`.from_markdown` reads one text, the first it takes: \
                             `RichText.from_markdown(text)`",
                        );
                    }
                }
                Some("links") => {
                    let held = self.expr_as(&a.value, &Ty::Map(Box::new(Ty::Text)));
                    links = Some(held);
                }
                Some("from") => from = Some(self.expr_as(&a.value, &Ty::Text)),
                Some(other) => self.err(
                    "E022",
                    a.value.span,
                    format!(
                        "`.from_markdown` takes the text, `links: pages` and `from: folder`; not \
                         `{other}:`"
                    ),
                ),
            }
        }
        let Some(text) = text else {
            self.err(
                "E022",
                span,
                "`.from_markdown` takes the text it reads: `RichText.from_markdown(text)`",
            );
            return Ex::error();
        };
        let links =
            links.unwrap_or_else(|| Ex::host(Imp::MapNew, Vec::new(), Ty::Map(Box::new(Ty::Text))));
        let from = from.unwrap_or_else(|| Ex::str(""));
        let app = Ex::str(&self.env.app);
        Ex::host(
            Imp::RichFromMarkdown,
            vec![text, links, from, app],
            Ty::RichText,
        )
    }
    /// `engine.call(op, input)`, `engine.start(op, input)`, `engine.status(job)` and
    /// `engine.cancel(job)`: the device's own work — a decode, a render, a raster — asked by a
    /// name the host answers for this app. A call answers now; a start answers a job id, and the
    /// job's progress is asked for by that id. Both carry JSON, which the app reads into its own
    /// records with `json.decode<T>`. An action asks; a view is drawn again from what it is
    /// shown, so it never does.
    fn builtin_engine(&mut self, name: &ast::Ident, args: &[ast::Arg], span: Span) -> Ex {
        let result = || Ty::Result(Box::new(Ty::Text), Box::new(Ty::Text));
        let asks_input = match name.name.as_str() {
            "call" | "start" => true,
            "status" | "cancel" => false,
            other => {
                self.err(
                    "E022",
                    name.span,
                    format!(
                        "`engine` has `call`, `start`, `status` and `cancel`, not `{other}`: \
                         `engine.call(op, input)`, `engine.start(op, input)`, \
                         `engine.status(job)` and `engine.cancel(job)`"
                    ),
                );
                for a in args {
                    let _ = self.expr(&a.value, None);
                }
                return Ex::error();
            }
        };
        let what = format!("`engine.{}`", name.name);
        if self.st.ctx == Ctx::View {
            self.err(
                "E067",
                span,
                format!(
                    "{what} is the device's work, which a view never asks: it is drawn again \
                     from what it is shown, so what it asks would run on every draw. Ask it from \
                     an action"
                ),
            );
        }
        let mut held: Option<Ex> = None;
        let mut input: Option<Ex> = None;
        for a in args {
            if let Some(named) = &a.name {
                self.err(
                    "E022",
                    named.span,
                    format!("{what} takes what it reads by position"),
                );
            }
            if asks_input && held.is_none() {
                held = Some(self.expr_as(&a.value, &Ty::Text));
            } else if asks_input && input.is_none() {
                input = Some(self.expr_as(&a.value, &Ty::Text));
            } else if !asks_input && held.is_none() {
                held = Some(self.expr_as(&a.value, &Ty::Text));
            } else {
                self.err(
                    "E022",
                    a.value.span,
                    if asks_input {
                        format!(
                            "{what} takes the op and its input, and nothing else: \
                             `engine.{}(\"{}\u{2026}\", input)",
                            name.name,
                            if name.name == "start" {
                                "paint"
                            } else {
                                "decode"
                            }
                        )
                    } else {
                        format!(
                            "{what} takes the job, and nothing else: `engine.{}(job)",
                            name.name
                        )
                    },
                );
                let _ = self.expr(&a.value, None);
            }
        }
        let Some(held) = held else {
            self.err(
                "E022",
                span,
                if asks_input {
                    format!(
                        "{what} takes the op it asks for: `engine.{}(op, input)",
                        name.name
                    )
                } else {
                    format!("{what} takes the job: `engine.{}(job)", name.name)
                },
            );
            return Ex::error();
        };
        if asks_input && input.is_none() {
            self.err(
                "E022",
                span,
                format!(
                    "{what} takes its input after the op: `engine.{}(op, input)`; `\"\"` for \
                     work that reads nothing",
                    name.name
                ),
            );
            return Ex::error();
        }
        let app = Ex::str(&self.env.app);
        match name.name.as_str() {
            "call" => {
                let input = input.unwrap_or_else(Ex::error);
                Ex::host(Imp::EngineCall, vec![app, held, input], result())
            }
            "start" => {
                let input = input.unwrap_or_else(Ex::error);
                Ex::host(Imp::EngineStart, vec![app, held, input], result())
            }
            "status" => Ex::host(Imp::EngineStatus, vec![held], result()),
            _ => Ex::host(Imp::EngineCancel, vec![held], Ty::Bool),
        }
    }

    /// `clipboard.copy(text)`, `clipboard.copy_image(file)`, `clipboard.paste()` and
    /// `clipboard.paste_image()`: the person's clipboard. A copy reaches their device and a paste
    /// reads what is on it now, so an action that does either declares `uses clipboard`, and
    /// neither is written in a view, which is drawn again from what it is shown.
    fn builtin_pdf(&mut self, name: &ast::Ident, args: &[ast::Arg], span: Span) -> Ex {
        let result = |held: Ty| Ty::Result(Box::new(held), Box::new(Ty::Text));
        let what = format!("`pdf.{}`", name.name);
        let (imp, held, takes) = match name.name.as_str() {
            "pages" => (Imp::PdfPages, result(Ty::Text), "the file it reads"),
            "outline" => (Imp::PdfOutline, result(Ty::Text), "the file it reads"),
            "fields" => (Imp::PdfFields, result(Ty::Text), "the file it reads"),
            "markup" => (Imp::PdfMarkup, result(Ty::Text), "the file it reads"),
            "import" => (
                Imp::PdfImport,
                result(Ty::Text),
                "the file it takes pages from",
            ),
            "text" => (
                Imp::PdfText,
                result(Ty::Text),
                "the file and the page, counted from 1",
            ),
            "links" => (
                Imp::PdfLinks,
                result(Ty::Text),
                "the file and the page, counted from 1",
            ),
            "picture" => (
                Imp::PdfPicture,
                result(Ty::File),
                "the file, the page counted from 1, and the picture's width in pixels",
            ),
            "save" => (
                Imp::PdfSave,
                result(Ty::File),
                "the files, the document's first, and the save request as JSON",
            ),
            other => {
                self.err(
                    "E022",
                    name.span,
                    format!(
                        "`pdf` has `pages`, `outline`, `text`, `links`, `fields`, `markup`, \
                         `import`, `picture` and `save`, not `{other}`: \
                         `pdf.pages(file)` answers a document's pages"
                    ),
                );
                for a in args {
                    let _ = self.expr(&a.value, None);
                }
                return Ex::error();
            }
        };
        // The file first, by position; then the numbers `text`, `links` and `picture` take.
        let wanted = match name.name.as_str() {
            "save" | "text" | "links" => 2,
            "picture" => 3,
            _ => 1,
        };
        if args.len() != wanted {
            self.err(
                "E022",
                span,
                format!("{what} takes {takes}: `pdf.{}(\u{2026})`", name.name),
            );
            for a in args {
                let _ = self.expr(&a.value, None);
            }
            return Ex::error();
        }
        let mut given: Vec<Ex> = Vec::with_capacity(wanted);
        for (at, arg) in args.iter().enumerate() {
            if let Some(named) = &arg.name {
                self.err(
                    "E022",
                    named.span,
                    format!("{what} takes {takes} by position"),
                );
            }
            if at == 0 {
                let held = if name.name == "save" {
                    Ty::List(Box::new(Ty::File))
                } else {
                    Ty::File
                };
                given.push(self.expr_as(&arg.value, &held));
            } else if name.name == "save" {
                given.push(self.expr_as(&arg.value, &Ty::Text));
            } else {
                given.push(self.expr_as(&arg.value, &Ty::Int));
            }
        }
        Ex::host(imp, given, held)
    }

    fn builtin_clipboard(&mut self, name: &ast::Ident, args: &[ast::Arg], span: Span) -> Ex {
        let (imp, ty) = match name.name.as_str() {
            "copy" => (Imp::ClipboardCopyText, Ty::Bool),
            "copy_image" => (Imp::ClipboardCopyImage, Ty::Bool),
            "paste" => (Imp::ClipboardPasteText, Ty::Option(Box::new(Ty::Text))),
            "paste_image" => (Imp::ClipboardPasteImage, Ty::Option(Box::new(Ty::File))),
            other => {
                self.err(
                    "E022",
                    name.span,
                    format!(
                        "`clipboard` has `copy`, `copy_image`, `paste` and `paste_image`, not \
                         `{other}`: `clipboard.copy(text)`, `clipboard.copy_image(file)`, \
                         `clipboard.paste()` and `clipboard.paste_image()`"
                    ),
                );
                for a in args {
                    let _ = self.expr(&a.value, None);
                }
                return Ex::error();
            }
        };
        let what = format!("`clipboard.{}`", name.name);
        self.reads(crate::effects::Effect::Clipboard, span, &what);
        let (held, said) = match name.name.as_str() {
            "copy" => (Some(Ty::Text), "the text to copy"),
            "copy_image" => (Some(Ty::File), "the image file to copy"),
            _ => (None, ""),
        };
        let Some(held) = held else {
            if let Some(extra) = args.first() {
                self.err(
                    "E022",
                    extra.value.span,
                    format!("{what} takes nothing: it answers what is on the clipboard"),
                );
                let _ = self.expr(&extra.value, None);
            }
            return Ex::host(imp, Vec::new(), ty);
        };
        let [value] = args else {
            self.err(
                "E022",
                span,
                format!(
                    "{what} takes {said} and nothing else: `clipboard.{}(\u{2026})`",
                    name.name
                ),
            );
            for a in args {
                let _ = self.expr(&a.value, None);
            }
            return Ex::error();
        };
        if let Some(named) = &value.name {
            self.err(
                "E022",
                named.span,
                format!("{what} takes {said} by position"),
            );
        }
        let value = self.expr_as(&value.value, &held);
        // A copy changes the device: the action that makes it reaches outside Tilo, and its
        // invocation records it.
        self.st.facts.outside = true;
        Ex::host(imp, vec![value], ty)
    }

    /// `Bytes`'s method: `Bytes.from_base64(text)`, the bytes a base64 text holds — how a picture
    /// or another file an action came by as text becomes bytes it can keep.
    fn bytes_type_method(&mut self, name: &ast::Ident, args: &[ast::Arg], span: Span) -> Ex {
        if name.name != "from_base64" {
            self.err(
                "E027",
                name.span,
                format!(
                    "`Bytes` has `from_base64`, not `{}`: `Bytes.from_base64(text)` reads the bytes a base64 text holds (a vCard's PHOTO, a reply's body)",
                    name.name
                ),
            );
            for a in args {
                let _ = self.expr(&a.value, None);
            }
            return Ex::error();
        }
        let [text] = args else {
            self.err(
                "E022",
                span,
                "`Bytes.from_base64` takes the text: `Bytes.from_base64(text)`",
            );
            for a in args {
                let _ = self.expr(&a.value, None);
            }
            return Ex::error();
        };
        let text = self.expr_as(&text.value, &Ty::Text);
        Ex::host(Imp::Base64Decode, vec![text], Ty::Bytes)
    }

    /// `File`'s methods: `File.of_text(name, text)`, `File.of_bytes(name, bytes)`, `File.at(name,
    /// address)` and `File.zip(name, files)`.
    fn file_type_method(&mut self, name: &ast::Ident, args: &[ast::Arg], span: Span) -> Ex {
        if name.name == "of_document" {
            let [named, markdown, format] = args else {
                self.err(
                    "E022",
                    span,
                    "`File.of_document` takes the file's name, the document as Markdown and the format: `File.of_document(\"Notes.docx\", markdown, \"docx\")`",
                );
                for a in args {
                    let _ = self.expr(&a.value, None);
                }
                return Ex::error();
            };
            let named = self.expr_as(&named.value, &Ty::Text);
            let markdown = self.expr_as(&markdown.value, &Ty::Text);
            let format = self.expr_as(&format.value, &Ty::Text);
            return Ex::host(Imp::FileOfDocument, vec![named, markdown, format], Ty::File);
        }
        if name.name == "of_sheets" {
            let [named, workbook, format] = args else {
                self.err(
                    "E022",
                    span,
                    "`File.of_sheets` takes the file's name, the workbook as text and the format: `File.of_sheets(\"Budget.xlsx\", workbook, \"xlsx\")`",
                );
                for a in args {
                    let _ = self.expr(&a.value, None);
                }
                return Ex::error();
            };
            let named = self.expr_as(&named.value, &Ty::Text);
            let workbook = self.expr_as(&workbook.value, &Ty::Text);
            let format = self.expr_as(&format.value, &Ty::Text);
            return Ex::host(Imp::SheetOfFile, vec![named, workbook, format], Ty::File);
        }
        // A folder on this device, within what the app was granted: its entries, one entry, the
        // bytes a path holds, a watch that runs an action when the folder changes, the open of a
        // file in the app that opens its kind, and the writes a Files window makes. They read or
        // change the disk, so an action declares `uses files` and a view calls none of them.
        if matches!(
            name.name.as_str(),
            "list"
                | "stat"
                | "read"
                | "watch"
                | "open"
                | "mkdir"
                | "write"
                | "rename"
                | "move"
                | "copy"
                | "remove"
                | "trash"
                | "trash_entries"
                | "restore"
        ) {
            return self.file_device_method(name, args, span);
        }
        let (held, imp, what, of) = match name.name.as_str() {
            "of_text" => (Ty::Text, Imp::FileOfText, "the text it holds", "text"),
            "of_bytes" => (Ty::Bytes, Imp::FileOfBytes, "the bytes it holds", "bytes"),
            "at" => (
                Ty::Text,
                Imp::FileAt,
                "the address of the content it holds",
                "a `tilo-blob://` address",
            ),
            "zip" => (
                Ty::List(Box::new(Ty::File)),
                Imp::FileZip,
                "the files it holds",
                "files",
            ),
            other => {
                self.err(
                    "E027",
                    name.span,
                    format!(
                        "`File` has `of_text`, `of_bytes`, `of_sheets`, `of_document`, `at`, `zip` and `open`, not `{other}`: a file is made of what an action holds, as `File.of_text(\"Notes.md\", text)`"
                    ),
                );
                for a in args {
                    let _ = self.expr(&a.value, None);
                }
                return Ex::error();
            }
        };
        let [named, content] = args else {
            self.err(
                "E022",
                span,
                format!(
                    "`.{}` takes the file's name and {}: `File.{}(\"{}\", {})`",
                    name.name,
                    what,
                    name.name,
                    if name.name == "zip" {
                        "Notes.zip"
                    } else {
                        "Notes.md"
                    },
                    of
                ),
            );
            for a in args {
                let _ = self.expr(&a.value, None);
            }
            return Ex::error();
        };
        for arg in [named, content] {
            if let Some(named) = &arg.name {
                self.err(
                    "E022",
                    named.span,
                    format!(
                        "`.{}` takes the name and {} by position: `File.{}(…, …)`",
                        name.name, what, name.name
                    ),
                );
            }
        }
        let named = self.expr_as(&named.value, &Ty::Text);
        let content = self.expr_as(&content.value, &held);
        Ex::host(imp, vec![named, content], Ty::File)
    }

    /// The `File` calls over a device folder: `File.list(path)`, `File.stat(path)`,
    /// `File.read(path)`, `File.watch(path, on: action)` and `File.open(path)`/`File.open(file)`.
    /// Each reads the disk, so the action declaring it says `uses files`; a view calls none.
    fn file_device_method(&mut self, name: &ast::Ident, args: &[ast::Arg], span: Span) -> Ex {
        let method = name.name.as_str();
        let form = match method {
            "list" => "`File.list(path)`: a folder's entries, within what the app was granted",
            "stat" => "`File.stat(path)`: one entry, or none when nothing is there",
            "read" => "`File.read(path)`: the bytes a path holds",
            "watch" => {
                "`File.watch(path, run)`: one of this package's actions, run when the folder changes"
            }
            "open" => {
                "`File.open(path)` or `File.open(file)`: the file at a path, or one an action \
                 holds, in the app that opens its kind"
            }
            "mkdir" => "`File.mkdir(path)`: a folder made, within what the app was granted",
            "write" => "`File.write(path, bytes)`: a new file holding those bytes",
            "rename" => "`File.rename(path, name)`: the entry renamed in its own folder",
            "move" => "`File.move(path, folder)`: the entry moved into that folder",
            "copy" => "`File.copy(path, folder)`: a copy of the entry in that folder",
            "remove" => "`File.remove(path)`: the entry removed for good, folder and all",
            "trash" => "`File.trash(path)`: the entry put in this device's trash",
            "trash_entries" => "`File.trash_entries()`: what this device's trash holds",
            _ => "`File.restore(name)`: the trashed entry named, put back where it came from",
        };
        let wanted = match method {
            "trash_entries" => 0,
            "watch" | "write" | "rename" | "move" | "copy" => 2,
            _ => 1,
        };
        if args.len() != wanted {
            self.err("E022", span, form.to_owned());
            for a in args {
                let _ = self.expr(&a.value, None);
            }
            return Ex::error();
        }
        for arg in args {
            if let Some(named) = &arg.name {
                self.err(
                    "E022",
                    named.span,
                    format!("{form}; its arguments stand by position"),
                );
            }
        }
        self.reads(
            crate::effects::Effect::Files,
            span,
            &format!("`File.{method}`"),
        );
        // `File.open` names a path the app was granted, or the file an action holds: both are one
        // open, decided by the shell from the file's bytes and its name.
        if method == "open" {
            let opened = self.expr(&args[0].value, None);
            let (imp, opened) = if opened.ty.same(&Ty::File) {
                (Imp::FileOpen, opened)
            } else {
                (
                    Imp::FileOpenPath,
                    self.coerce(opened, &Ty::Text, args[0].value.span),
                )
            };
            return Ex::host(
                imp,
                vec![opened],
                Ty::Result(Box::new(Ty::Unit), Box::new(Ty::Text)),
            );
        }
        let entry = || super::platform::host_ty("FileEntry");
        let trashed = || super::platform::host_ty("TrashEntry");
        let result = |ok: Ty| Ty::Result(Box::new(ok), Box::new(Ty::Text));
        if method == "trash_entries" {
            return Ex::host(
                Imp::FileTrashEntries,
                Vec::new(),
                result(Ty::List(Box::new(trashed()))),
            );
        }
        let path = self.expr_as(&args[0].value, &Ty::Text);
        match method {
            "list" => Ex::host(
                Imp::FileList,
                vec![path],
                result(Ty::List(Box::new(entry()))),
            ),
            "stat" => Ex::host(
                Imp::FileStat,
                vec![path],
                result(Ty::Option(Box::new(entry()))),
            ),
            "read" => Ex::host(Imp::FileReadPath, vec![path], result(Ty::Bytes)),
            "watch" => {
                let run = self.expr_as(&args[1].value, &Ty::Text);
                Ex::host(Imp::FileWatch, vec![path, run], result(Ty::Text))
            }
            "mkdir" => Ex::host(Imp::FileMkdir, vec![path], result(entry())),
            "write" => {
                let bytes = self.expr_as(&args[1].value, &Ty::Bytes);
                Ex::host(Imp::FileWrite, vec![path, bytes], result(entry()))
            }
            "rename" => {
                let name = self.expr_as(&args[1].value, &Ty::Text);
                Ex::host(Imp::FileRename, vec![path, name], result(entry()))
            }
            "move" => {
                let folder = self.expr_as(&args[1].value, &Ty::Text);
                Ex::host(Imp::FileMove, vec![path, folder], result(entry()))
            }
            "copy" => {
                let folder = self.expr_as(&args[1].value, &Ty::Text);
                Ex::host(Imp::FileCopy, vec![path, folder], result(entry()))
            }
            "remove" => Ex::host(Imp::FileRemove, vec![path], result(Ty::Bool)),
            "trash" => Ex::host(Imp::FileTrash, vec![path], result(trashed())),
            _ => Ex::host(Imp::FileRestore, vec![path], result(entry())),
        }
    }

    pub(super) fn value_method(
        &mut self,
        r: Ex,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> Ex {
        let m = name.name.as_str();
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
            // `r.appended(other)`: this document with `other`'s paragraphs following as
            // paragraphs of their own, never running on into the last.
            (Ty::RichText, "appended") => {
                let [other] = args else {
                    self.err(
                        "E022",
                        span,
                        "`.appended` takes the text that follows it: `r.appended(RichText.from_markdown(text))`",
                    );
                    return Ex::error();
                };
                if let Some(named) = &other.name {
                    self.err(
                        "E022",
                        named.span,
                        "`.appended` takes the text by position: `r.appended(other)`",
                    );
                }
                let other = self.expr_as(&other.value, &Ty::RichText);
                Ex::host(Imp::RichAppended, vec![r, other], Ty::RichText)
            }
            // `r.markdown(links: pages, from: folder)`: each reference to a page of `links` a link
            // to that page, at the path from the folder the page is written in.
            (Ty::RichText, "markdown") => {
                let form = "markdown(links: pages, from: folder)`: the document as Markdown, a \
                            reference to a page of `links` (an address by each resource) written \
                            as the link to it from the folder `from";
                let Some(named) = self.named_args(args, &["links", "from"], form, span) else {
                    return Ex::error();
                };
                let links = self.expr_as(&named[0].value, &Ty::Map(Box::new(Ty::Text)));
                let from = self.expr_as(&named[1].value, &Ty::Text);
                Ex::host(Imp::RichMarkdown, vec![r, links, from], Ty::Text)
            }
            // `r.relinked(links: pages, sources: sources)`: the document with each link to a page
            // of `links` the reference to it, as `RichText.from_markdown` reads one, and each
            // picture whose source `sources` names showing the source it gives.
            (Ty::RichText, "relinked") => {
                let form = "relinked(links: pages, sources: sources)`: the document with each link \
                            to a page of `links` (a resource by the address it stands at) the \
                            reference to it, and each picture of a source `sources` names showing \
                            the source it gives";
                let Some(named) = self.named_args(args, &["links", "sources"], form, span) else {
                    return Ex::error();
                };
                let links = self.expr_as(&named[0].value, &Ty::Map(Box::new(Ty::Text)));
                let sources = self.expr_as(&named[1].value, &Ty::Map(Box::new(Ty::Text)));
                let app = Ex::str(&self.env.app);
                Ex::host(
                    Imp::RichRelinked,
                    vec![r, links, sources, app],
                    Ty::RichText,
                )
            }
            // The body of a response, and bytes, read as their text; bytes counted.
            (Ty::Response | Ty::Bytes, "text") if args.is_empty() => {
                Ex::host(Imp::WebText, vec![r], Ty::Text)
            }
            (Ty::Bytes, "len") if args.is_empty() => Ex::host(Imp::BytesLength, vec![r], Ty::Int),
            // A file: what it is called, its media type, its size, where its bytes are, and its
            // content as text.
            (Ty::File, "name" | "media" | "url") if args.is_empty() => {
                let part = match m {
                    "name" => 0,
                    "media" => 1,
                    _ => 2,
                };
                let part = Ex::new(Ek::I32(part), Ty::Bool);
                Ex::host(Imp::FilePart, vec![r, part], Ty::Text)
            }
            (Ty::File, "size") if args.is_empty() => Ex::host(Imp::FileSize, vec![r], Ty::Int),
            // A spreadsheet file's sheets, as the text `json.decode<List<T>>` reads: each
            // sheet's name, whether its tab is hidden, and its cells by A1 with the input,
            // format and bold each held.
            (Ty::File, "sheets") if args.is_empty() => Ex::host(
                Imp::SheetFile,
                vec![r],
                Ty::Result(Box::new(Ty::Text), Box::new(Ty::Text)),
            ),
            // The workbook a text describes (the JSON `File.of_sheets` writes), evaluated: each
            // of its sheets with the extent a window shows and what each cell shows there.
            (Ty::Text, "sheet_values") if args.is_empty() => Ex::host(
                Imp::SheetValues,
                vec![r],
                Ty::Result(Box::new(Ty::Text), Box::new(Ty::Text)),
            ),
            // The sheets a structural change of a workbook's text leaves (rows or columns
            // inserted or removed, a range sorted, a fill extended, a sheet renamed), with the
            // formulas the change moves rewritten.
            (Ty::Text, "sheet_edit") => {
                let [change] = args else {
                    self.err(
                        "E022",
                        span,
                        "`sheet_edit` takes the change, as text: `book.sheet_edit(change)`",
                    );
                    for a in args {
                        let _ = self.expr(&a.value, None);
                    }
                    return Ex::error();
                };
                let change = self.expr_as(&change.value, &Ty::Text);
                Ex::host(
                    Imp::SheetEdit,
                    vec![r, change],
                    Ty::Result(Box::new(Ty::Text), Box::new(Ty::Text)),
                )
            }
            // The numbers a range of a sheet shows, row by row: a chart's readings.
            (Ty::Text, "sheet_numbers") => {
                let [sheet, range] = args else {
                    self.err("E022", span, "`sheet_numbers` takes the sheet's name and the range: `book.sheet_numbers(sheet, range)`");
                    for a in args {
                        let _ = self.expr(&a.value, None);
                    }
                    return Ex::error();
                };
                let sheet = self.expr_as(&sheet.value, &Ty::Text);
                let range = self.expr_as(&range.value, &Ty::Text);
                Ex::host(
                    Imp::SheetNumbers,
                    vec![r, sheet, range],
                    Ty::Result(Box::new(Ty::Text), Box::new(Ty::Text)),
                )
            }
            // The ranges of a sheet as a structural change leaves them, each empty where the
            // change removes every cell it names.
            (Ty::Text, "sheet_ranges") => {
                let [sheet, ranges] = args else {
                    self.err("E022", span, "`sheet_ranges` takes the sheet and the ranges: `change.sheet_ranges(sheet, ranges)`");
                    for a in args {
                        let _ = self.expr(&a.value, None);
                    }
                    return Ex::error();
                };
                let sheet = self.expr_as(&sheet.value, &Ty::Text);
                let ranges = self.expr_as(&ranges.value, &Ty::List(Box::new(Ty::Text)));
                Ex::host(
                    Imp::SheetRanges,
                    vec![r, sheet, ranges],
                    Ty::Result(Box::new(Ty::Text), Box::new(Ty::Text)),
                )
            }
            // The cells a record action writes into a sheet: `set-records` by column header, or
            // `upsert-item` by the values that identify its row.
            (Ty::Text, "sheet_records") => {
                let [request] = args else {
                    self.err(
                        "E022",
                        span,
                        "`sheet_records` takes the records as text: `sheet.sheet_records(records)`",
                    );
                    for a in args {
                        let _ = self.expr(&a.value, None);
                    }
                    return Ex::error();
                };
                let request = self.expr_as(&request.value, &Ty::Text);
                Ex::host(
                    Imp::SheetRecords,
                    vec![r, request],
                    Ty::Result(Box::new(Ty::Text), Box::new(Ty::Text)),
                )
            }
            // The input to store for a typed entry and the format it shows in, for the currency
            // a bare money symbol means.
            (Ty::Text, "sheet_entry") => {
                let [currency] = args else {
                    self.err("E022", span, "`sheet_entry` takes the workbook's currency: `input.sheet_entry(currency)`");
                    for a in args {
                        let _ = self.expr(&a.value, None);
                    }
                    return Ex::error();
                };
                let currency = self.expr_as(&currency.value, &Ty::Text);
                Ex::host(
                    Imp::SheetEntry,
                    vec![r, currency],
                    Ty::Result(Box::new(Ty::Text), Box::new(Ty::Text)),
                )
            }
            (Ty::File, "text") if args.is_empty() => Ex::host(
                Imp::FileText,
                vec![r],
                Ty::Result(Box::new(Ty::Text), Box::new(Ty::Text)),
            ),
            // A document file as the Markdown an editor shows: by the format its name ends in,
            // a Word, OpenDocument, RTF, EPUB, HTML or Markdown file; plain text as itself.
            (Ty::File, "markdown") if args.is_empty() => Ex::host(
                Imp::FileMarkdown,
                vec![r],
                Ty::Result(Box::new(Ty::Text), Box::new(Ty::Text)),
            ),
            // The files an archive holds, each kept in the content store under the name it has
            // inside.
            (Ty::File, "files") if args.is_empty() => Ex::host(
                Imp::FileFiles,
                vec![r],
                Ty::Result(Box::new(Ty::List(Box::new(Ty::File))), Box::new(Ty::Text)),
            ),
            // A file's pixels: what its container says, and the crop, turn, resize and encode a
            // program applies. All of them reach the device's codec, so the action declares
            // `uses image`; a view, drawn again from what it is shown, works on no pixels.
            (Ty::File, "image_info") if args.is_empty() => {
                self.reads(crate::effects::Effect::Image, span, "`image_info`");
                Ex::host(
                    Imp::FileImageInfo,
                    vec![r],
                    Ty::Option(Box::new(super::platform::host_ty("ImageInfo"))),
                )
            }
            (Ty::File, "cropped") => {
                self.reads(crate::effects::Effect::Image, span, "`cropped`");
                let form = "cropped(x, y, width, height)`: the region of the picture kept, each \
                            a fraction of it from 0 to 1";
                let [x, y, width, height] = args else {
                    self.err("E022", span, format!("`.{form}`"));
                    for a in args {
                        let _ = self.expr(&a.value, None);
                    }
                    return Ex::error();
                };
                let mut parts = Vec::with_capacity(4);
                for argument in [x, y, width, height] {
                    if let Some(named) = &argument.name {
                        self.err(
                            "E022",
                            named.span,
                            format!("`.{form}` takes its region by position"),
                        );
                    }
                    parts.push(self.expr_as(&argument.value, &Ty::Float));
                }
                let files = Ty::Result(Box::new(Ty::File), Box::new(Ty::Text));
                Ex::host(
                    Imp::FileCrop,
                    std::iter::once(r).chain(parts).collect(),
                    files,
                )
            }
            (Ty::File, "turned") => {
                self.reads(crate::effects::Effect::Image, span, "`turned`");
                let [quarters] = args else {
                    self.err(
                        "E022",
                        span,
                        "`.turned(quarters)`: how many times a quarter turn clockwise the picture \
                         is turned",
                    );
                    for a in args {
                        let _ = self.expr(&a.value, None);
                    }
                    return Ex::error();
                };
                if let Some(named) = &quarters.name {
                    self.err(
                        "E022",
                        named.span,
                        "`.turned` takes its quarter turns by position: `.turned(1)`",
                    );
                }
                let quarters = self.expr_as(&quarters.value, &Ty::Int);
                let files = Ty::Result(Box::new(Ty::File), Box::new(Ty::Text));
                Ex::host(Imp::FileTurn, vec![r, quarters], files)
            }
            (Ty::File, "resized") => {
                self.reads(crate::effects::Effect::Image, span, "`resized`");
                let form = "resized(width, height)`: the pixels the picture is scaled to";
                let [width, height] = args else {
                    self.err("E022", span, format!("`.{form}`"));
                    for a in args {
                        let _ = self.expr(&a.value, None);
                    }
                    return Ex::error();
                };
                let mut parts = Vec::with_capacity(2);
                for argument in [width, height] {
                    if let Some(named) = &argument.name {
                        self.err(
                            "E022",
                            named.span,
                            format!("`.{form}` takes its size by position"),
                        );
                    }
                    parts.push(self.expr_as(&argument.value, &Ty::Int));
                }
                let files = Ty::Result(Box::new(Ty::File), Box::new(Ty::Text));
                Ex::host(
                    Imp::FileResize,
                    std::iter::once(r).chain(parts).collect(),
                    files,
                )
            }
            (Ty::File, "thumbnail") => {
                self.reads(crate::effects::Effect::Image, span, "`thumbnail`");
                let [size] = args else {
                    self.err(
                        "E022",
                        span,
                        "`.thumbnail(size)` takes how large a side of the preview may be, in pixels",
                    );
                    for a in args {
                        let _ = self.expr(&a.value, None);
                    }
                    return Ex::error();
                };
                if let Some(named) = &size.name {
                    self.err(
                        "E022",
                        named.span,
                        "`.thumbnail` takes its size by position: `.thumbnail(512)`",
                    );
                }
                let size = self.expr_as(&size.value, &Ty::Int);
                Ex::host(
                    Imp::FileThumbnail,
                    vec![r, size],
                    Ty::Option(Box::new(Ty::File)),
                )
            }
            (Ty::File, "encoded") => {
                self.reads(crate::effects::Effect::Image, span, "`encoded`");
                let mut format = None;
                let mut quality = None;
                for argument in args {
                    match argument.name.as_ref().map(|name| name.name.as_str()) {
                        Some("format") => {
                            format = Some(self.expr_as(&argument.value, &Ty::Text));
                        }
                        Some("quality") => {
                            quality = Some(self.expr_as(&argument.value, &Ty::Int));
                        }
                        _ => {
                            self.err(
                                "E022",
                                argument.value.span,
                                "`.encoded(format: \"png\", quality: 90)` names its format, and \
                                 its quality for a lossy one",
                            );
                            let _ = self.expr(&argument.value, None);
                            return Ex::error();
                        }
                    }
                }
                let Some(format) = format else {
                    self.err(
                        "E022",
                        span,
                        "`.encoded` says what it writes: `.encoded(format: \"png\")`, or \
                         `\"jpeg\"` or `\"webp\"`",
                    );
                    return Ex::error();
                };
                let quality = quality.unwrap_or_else(|| Ex::new(Ek::Int(90), Ty::Int));
                let files = Ty::Result(Box::new(Ty::File), Box::new(Ty::Text));
                Ex::host(Imp::FileEncode, vec![r, format, quality], files)
            }
            // A book the file is, read from an EPUB container: its chapters in reading order,
            // each beginning with its title as a heading, and what its package says about it
            // (title, author, language, subject). A file that is not a book, or one damaged or
            // too long, fails the action that reads it, saying so.
            (Ty::File, "book") if args.is_empty() => {
                Ex::host(Imp::FileBook, vec![r], Ty::List(Box::new(Ty::RichText)))
            }
            (Ty::File, "book_info") if args.is_empty() => {
                Ex::host(Imp::FileBookInfo, vec![r], Ty::Map(Box::new(Ty::Text)))
            }
            // The cover picture the package carries, kept as a file of this device, when it has
            // one.
            (Ty::File, "book_cover") if args.is_empty() => {
                Ex::host(Imp::FileBookCover, vec![r], Ty::Option(Box::new(Ty::File)))
            }
            // A map read as a value: its keys, the value one key holds, and — in the code that
            // runs — the entry put there or taken out.
            (Ty::Map(t), "get" | "keys" | "put" | "remove") => {
                let value = t.clone();
                self.map_value_method(r, &value, name, args, span)
            }
            // A day as people read it: the day of the week, the day, month and year, and the first
            // day of its week.
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
            // A moment read as people read it: the day it falls on for the person, and its time.
            (Ty::Time, "day") if args.is_empty() => {
                self.reads(crate::effects::Effect::Time, span, "`day`");
                Ex::host(Imp::TimeDay, vec![r], Ty::Date)
            }
            // The day a moment falls on in the named zone, or in `floating`'s (its wall time read
            // as UTC): a pure reading of the zone's rules, which a view draws as it draws a day.
            (Ty::Time, "day_in")
                if args.len() == 1 && args.first().is_some_and(|a| a.name.is_none()) =>
            {
                let Some(a) = args.first() else {
                    return Ex::error();
                };
                let zone = self.expr_as(&a.value, &Ty::Text);
                Ex::host(Imp::TimeDayIn, vec![r, zone], Ty::Date)
            }
            // The wall time a moment is in a named zone, as a calendar stores one.
            (Ty::Time, "wall")
                if args.len() == 1 && args.first().is_some_and(|a| a.name.is_none()) =>
            {
                let Some(one) = args.first() else {
                    return Ex::error();
                };
                let zone = self.expr_as(&one.value, &Ty::Text);
                Ex::host(Imp::TimeWall, vec![r, zone], Ty::Option(Box::new(Ty::Text)))
            }
            (Ty::Time, "time_of_day") if args.is_empty() => {
                self.reads(crate::effects::Effect::Time, span, "`time_of_day`");
                Ex::host(Imp::TimeOfDay, vec![r], Ty::Duration)
            }
            // The time of day, as the person's clock style writes it: `10:41` or `10:41 AM`.
            (Ty::Time, "clock") if args.is_empty() => {
                self.reads(crate::effects::Effect::Time, span, "`clock`");
                Ex::host(Imp::TimeClock, vec![r], Ty::Text)
            }
            // A day or a moment as a machine reads it (`2026-06-29`, `2026-06-29T16:00:00Z`): the
            // stored text, which sorts in time order and parses back with `Date` or `Time`.
            (Ty::Date | Ty::Time, "iso") if args.is_empty() => r.typed(Ty::Text),
            // A length of time as the seconds it is, which is how a record keeps one.
            (Ty::Duration, "seconds") if args.is_empty() => r.typed(Ty::Int),
            (Ty::Actor, "name") if args.is_empty() => Ex::host(Imp::ActorName, vec![r], Ty::Text),
            // What names them whatever they are called: a key every device makes the same.
            (Ty::Actor, "id") if args.is_empty() => Ex::host(Imp::ActorId, vec![r], Ty::Text),
            (Ty::Actor, "kind") if args.is_empty() => {
                Ex::host(Imp::ActorKind, vec![r], Ty::Choice("actor_kind"))
            }
            (Ty::Actor, "role") if args.is_empty() => {
                Ex::host(Imp::ActorRole, vec![r], Ty::Option(Box::new(Ty::Text)))
            }
            (Ty::Record(_), "id") if args.is_empty() => Ex::host(Imp::RefId, vec![r], Ty::Text),
            // `as.many()`: an action that applies each of `as` to its own item, all at once: one
            // change, one undo.
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
            // `a.made_from(r)`: the action invoked made from the resource `r`, which what it makes
            // records (`created(r)?.from`).
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
            // How many it holds, once the store has read the whole workspace: none until then
            // ([`Imp::CollCounted`]).
            (Ty::List(_) | Ty::Set(_) | Ty::Collection(_), "counted") if args.is_empty() => {
                Ex::host(Imp::CollCounted, vec![r], Ty::Option(Box::new(Ty::Int)))
            }
            (Ty::List(_) | Ty::Set(_) | Ty::Collection(_), "is_empty") if args.is_empty() => {
                // Whether it holds nothing reads whether it shows a first item: a filtered
                // collection's length is only the most it can hold ([`Imp::CollEmpty`]).
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
            // `items.previous(x)`: the item just before `x` in the list as a view shows it, or none
            // for the first. A row's place is the host's.
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
                    Ty::Actor => "name, kind, role".to_owned(),
                    Ty::Action => "then, made_from".to_owned(),
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

/// The type a text names, where it is written before a method instead of a value: `Date.parse(...)`,
/// `Time.parse(...)`, `RichText.from_markdown(...)`, `File.of_text(...)`.
fn parse_ty(name: &str) -> Option<Ty> {
    match name {
        "Date" => Some(Ty::Date),
        "Time" => Some(Ty::Time),
        "RichText" => Some(Ty::RichText),
        "File" => Some(Ty::File),
        "Bytes" => Some(Ty::Bytes),
        _ => None,
    }
}

/// Whether the callee is `json.X`: the one module whose calls take a type argument.
fn names_json(callee: &ast::Expr) -> bool {
    matches!(&callee.kind, ExprKind::Field { base, .. } if matches!(&base.kind, ExprKind::Ident(ns) if ns == "json"))
}
