//! Calls: functions, actions, flows, record construction, built-ins and methods.

use super::expr::dotted;
use super::{Checker, Ctx, Ex, conv, store_op};
use crate::abi::Imp;
use crate::ast::{self, ExprKind};
use crate::diag::Span;
use crate::ir::{self, ActionRef, Conv, Ek, Stmt};
use crate::prims;
use crate::resolve::{CKind, Callable, FieldInfo, Import};
use crate::types::{Ty, TypeKey};

impl Checker<'_> {
    pub(super) fn call(&mut self, callee: &ast::Expr, args: &[ast::Arg], span: Span) -> Ex {
        match &callee.kind {
            ExprKind::Ident(name) => self.call_named(name, callee.span, args, span),
            ExprKind::Field { base, name } => {
                if matches!(&base.kind, ExprKind::Ident(ns) if ns == crate::stdlib::NS)
                    && name.name == "RichText"
                    && self.lookup(crate::stdlib::NS).is_none()
                {
                    return self.builtin_rich_text(args, span);
                }
                if matches!(&base.kind, ExprKind::Ident(ns) if ns == crate::stdlib::NS)
                    && name.name == "Identifier"
                    && self.lookup(crate::stdlib::NS).is_none()
                {
                    return match crate::resolve::identifier_value(args) {
                        Ok(text) => Ex::str(&text).typed(Ty::Identifier),
                        Err(msg) => {
                            self.err("E099", span, msg);
                            Ex::error()
                        }
                    };
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
        if let Some(ex) = self.program_builtin(name, args, span) {
            return ex;
        }
        match name {
            "outside" => return self.builtin_outside(args, span),
            "model_call" => return self.builtin_model(args, span),
            "run_block" => return self.builtin_run_block(args, span),
            "set" => return self.builtin_set(args, span),
            "open" => return self.builtin_open(args, span),
            "person" => return self.builtin_person(args, span),
            "actions" => return self.builtin_actions(args, span),
            "lenses" => return self.builtin_lenses(args, span),
            "delivery" => return self.builtin_delivery(args, span),
            "members" => return self.builtin_members(args, span),
            "created" | "updated" | "history" => {
                return self.builtin_versions(name, args, span);
            }
            "unseen" => return self.builtin_unseen(args, span),
            "undo" => return self.builtin_undo(args, span),
            "steps" => return self.builtin_steps(args, span),
            "changes" => return self.builtin_changes(args, span),
            "meta" => return self.builtin_meta(args, span),
            "cases" => return self.builtin_cases(args, span),
            "phrase" => return self.builtin_phrase(args, span),
            "dated" => return self.builtin_dated(args, span),
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
        let mut known = names.clone();
        for word in prims::BUILTINS
            .iter()
            .copied()
            .chain(self.program_builtins())
        {
            known.push(word);
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
        // form, which asks the person for them (forms-and-diagrams.md §1).
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
        if !missing.is_empty() {
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
        let layer = Ex::new(Ek::I32(state.layer.code()), Ty::Bool);
        let boxed = Ex::new(Ek::I32(i32::from(scalar)), Ty::Bool);
        let ty = if scalar {
            Ty::Option(Box::new(state.ty.clone()))
        } else {
            state.ty.clone()
        };
        Ex::host(Imp::StateAt, vec![layer, Ex::str(&state.id), boxed], ty)
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
        let at_press = matches!(c.kind, CKind::Action | CKind::Flow) && self.st.ctx == Ctx::View;
        let bound = self.bind_args_or_default(&c.name, &c.params, args, span, &sig, at_press);
        let ctx = self.st.ctx;
        match (c.kind, ctx) {
            (CKind::Fn, _) => {
                let bound = bound
                    .into_iter()
                    .map(|s| s.unwrap_or_else(Ex::error))
                    .collect();
                self.call_own(c, bound, span, "fn")
            }
            (CKind::Action, Ctx::Fn) => {
                self.err("E043", span, format!("a fn computes values and cannot use actions; `{}` is an action. Move this into an action or a flow", c.name));
                Ex::error()
            }
            (CKind::Action, Ctx::Action) => {
                if !c.own {
                    self.err("E041", span, format!("an action cannot call the foreign action `{}`: one change cannot span apps. Call it from a flow with `{}(...)`", c.name, c.name));
                    return Ex::error();
                }
                if let Some(f) = c.func {
                    self.st.facts.inline_actions.insert(f);
                }
                let (stmts, bound) = self.inline_args(c, bound);
                let call = self.call_own(c, bound, span, "action");
                if stmts.is_empty() {
                    call
                } else {
                    let ty = call.ty.clone();
                    Ex::new(Ek::Seq(stmts, Box::new(call)), ty)
                }
            }
            (CKind::Action, Ctx::Flow) => {
                let bound = self.applied_args(c, bound, span);
                self.invoke(c, bound)
            }
            (CKind::Action | CKind::Flow, Ctx::View | Ctx::Block) => {
                let bound = self.applied_args(c, bound, span);
                self.action_value(c, bound)
            }
            (CKind::Flow, other) => {
                let who = other.word();
                self.err("E046", span, format!("{who} cannot call the flow `{}`; flows run on their own and are applied from a view or a block as an action", c.name));
                Ex::error()
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

    fn call_own(&mut self, c: &Callable, bound: Vec<Ex>, span: Span, what: &str) -> Ex {
        match c.func {
            Some(f) if c.own => Ex::new(Ek::Call(f, bound), c.result.clone()),
            _ => {
                self.err("E081", span, format!("calling the {what} `{}` of another package is not supported in v1: the host ABI has no cross-module call. Only actions of other packages can be applied", c.name));
                Ex::error()
            }
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
        Ex::host(Imp::ActRef, vec![Ex::str(&c.id), list], Ty::Action)
    }

    fn invoke(&mut self, c: &Callable, bound: Vec<Ex>) -> Ex {
        self.st.facts.invokes.insert(Self::action_ref(c));
        let list = self.arg_list(bound);
        let act = Ex::host(Imp::ActRef, vec![Ex::str(&c.id), list], Ty::Action);
        if std::mem::take(&mut self.st.try_invoke) {
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
            if f.role.as_deref() == Some("previously") {
                self.err(
                    "E115",
                    a.value.span,
                    format!(
                        "`{}` is what `{}` held previously: nothing writes it any more; give the value where the type keeps it now",
                        f.name, key.name
                    ),
                );
                let _ = self.expr(&a.value, None);
                continue;
            }
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
        if !self.st.scenario {
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

    fn builtin_outside(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        if self.st.ctx != Ctx::Action {
            self.err(
                "E047",
                span,
                "`outside(capability, payload)` leaves the system and only an action may use it",
            );
            return Ex::error();
        }
        let (Some(cap), Some(payload), 2) = (args.first(), args.get(1), args.len()) else {
            self.err(
                "E022",
                span,
                "`outside` takes a capability name and a payload: `outside(\"post\", payload)`",
            );
            return Ex::error();
        };
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
        Ex::host(
            Imp::Outside,
            vec![Ex::str(name), Self::handle_of(p)],
            Ty::Unit,
        )
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
                format!(
                    "`set(name, value)` changes a view's `window` or `lens` state; {who} has none"
                ),
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
            self.err("E066", target.value.span, format!("the first argument of `set` names a `window` or `lens` declaration of this view; this view declares: {known}"));
            let _ = self.expr(&value.value, None);
            return Ex::error();
        };
        let v = self.arg_at_press(&value.value, &state.ty);
        Self::set_action(&state, v)
    }

    /// The action that sets the view state `state` to `v`.
    pub(super) fn set_action(state: &super::StateRef, v: Ex) -> Ex {
        let layer = Ex::new(Ek::I32(state.layer.code()), Ty::Bool);
        Ex::host(
            Imp::ActSet,
            vec![layer, Ex::str(&state.id), Self::handle_of(v)],
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

    fn builtin_model(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        if self.st.ctx != Ctx::Flow {
            self.err(
                "E047",
                span,
                "`model_call(kind, input)` is nondeterministic and only a flow may use it",
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
        // `notes.insert(n)` of a resource that names a container: that container's method.
        if let Some((key, f)) = self.held_of(&r.ty) {
            return self.container_method(r, &key, &f, name, args, span);
        }
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

    fn arity(&mut self, name: &ast::Ident, args: &[ast::Arg], n: usize, span: Span) -> bool {
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
        let valid = matches!(
            (&f.ty, m),
            (
                Ty::Collection(_),
                "insert" | "remove" | "move" | "remove_where"
            ) | (Ty::Set(_), "add" | "remove")
                | (Ty::Map(_), "put" | "remove")
        ) || (m == "add" && matches!(&f.ty, Ty::Map(v) if matches!(**v, Ty::Set(_))));
        if !valid
            && !matches!(
                m,
                "len"
                    | "is_empty"
                    | "map"
                    | "filter"
                    | "any"
                    | "all"
                    | "find"
                    | "count"
                    | "sort_by"
                    | "first"
                    | "last"
                    | "matching"
                    | "count_by"
                    | "group"
                    | "take"
                    | "each"
                    | "single"
            )
        {
            let t = self.show(&f.ty);
            let ops = match &f.ty {
                Ty::Collection(_) => {
                    "insert, remove, remove_where, move, len, matching and the list methods"
                }
                Ty::Set(_) => "add, remove, len and the list methods",
                Ty::Map(v) if matches!(**v, Ty::Set(_)) => {
                    "add(key, element), remove(key, element), remove(key), get (an Option of the set) and keys"
                }
                _ => "put, remove, get (an Option of the value) and keys",
            };
            let methods = [
                "insert",
                "remove",
                "remove_where",
                "move",
                "add",
                "put",
                "get",
                "keys",
                "len",
                "is_empty",
                "map",
                "filter",
                "any",
                "all",
                "find",
                "count",
                "sort_by",
                "first",
                "last",
                "matching",
                "count_by",
                "group",
                "take",
                "each",
                "single",
            ];
            let hint = crate::diag::suggestion(m, methods);
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

    pub(super) fn value_method(
        &mut self,
        r: Ex,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> Ex {
        let m = name.name.as_str();
        match (&r.ty.clone(), m) {
            (Ty::Error, _) => {
                for a in args {
                    if !matches!(a.value.kind, ExprKind::Lambda { .. }) {
                        let _ = self.expr(&a.value, None);
                    }
                }
                Ex::error()
            }
            (Ty::Text, "len") if args.is_empty() => Ex::host(Imp::StrLen, vec![r], Ty::Int),
            (Ty::RichText, "is_empty") if args.is_empty() => {
                Ex::host(Imp::RichEmpty, vec![r], Ty::Bool)
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
            (Ty::Int, "to_float") if args.is_empty() => {
                Ex::new(Ek::Conv(Conv::IntToFloat, Box::new(r)), Ty::Float)
            }
            (Ty::Float, "to_int") if args.is_empty() => {
                Ex::new(Ek::Conv(Conv::FloatToInt, Box::new(r)), Ty::Int)
            }
            (Ty::Int | Ty::Float | Ty::Bool, "to_text") if args.is_empty() => self.text_of(r, span),
            (Ty::Record(_), "id") if args.is_empty() => Ex::host(Imp::RefId, vec![r], Ty::Text),
            // `a.then(b)`: an action that applies `a`, then `b` once `a` is done.
            (Ty::Action, "then") => {
                let [next] = args else {
                    self.err("E022", span, "`.then` takes the action to apply next: `send(c, draft).then(set(draft, \"\"))`");
                    return Ex::error();
                };
                let next = self.expr_as(&next.value, &Ty::Action);
                Ex::host(Imp::ActThen, vec![r, next], Ty::Action)
            }
            (Ty::Enum(key), "label" | "icon" | "name") if args.is_empty() => {
                let key = key.clone();
                self.enum_method(r, &key, m).unwrap_or_else(Ex::error)
            }
            (Ty::List(_) | Ty::Set(_) | Ty::Collection(_), "len") if args.is_empty() => {
                Ex::host(Imp::ListLen, vec![r], Ty::Int)
            }
            (Ty::List(_) | Ty::Set(_) | Ty::Collection(_), "is_empty") if args.is_empty() => {
                let len = Ex::host(Imp::ListLen, vec![r], Ty::Int);
                Ex::new(
                    Ek::Bin(ir::BinOp::Eq, Box::new(len), Box::new(Ex::int(0))),
                    Ty::Bool,
                )
            }
            (
                Ty::List(t) | Ty::Set(t) | Ty::Collection(t),
                "map" | "filter" | "any" | "all" | "find" | "count",
            ) => {
                let elem = (**t).clone();
                self.higher_order(r, &elem, name, args, span)
            }
            (Ty::List(t) | Ty::Set(t) | Ty::Collection(t), "first" | "last") if args.is_empty() => {
                let elem = (**t).clone();
                Self::end_item(r, &elem, m == "last")
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
                    Ty::Text => "len",
                    Ty::RichText => "is_empty (and the members excerpt, text, references, people)",
                    Ty::Int => "to_float, to_text",
                    Ty::Float => "to_int, to_text",
                    Ty::Bool => "to_text",
                    Ty::Record(_) => "id (and fields)",
                    Ty::Option(_) | Ty::Result(..) | Ty::Enum(_) => {
                        "none; take it apart with `match`, such as `match x { .some(v) => v, .none => 0 }`"
                    }
                    Ty::List(_) | Ty::Set(_) | Ty::Collection(_) => {
                        "len, is_empty, first, last, map, filter, any, all, find, count, sort_by, count_by, group, take, each, single"
                    }
                    _ => "none",
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

    /// `map`, `filter`, `any`, `all`, `find` and `count` with a lambda, inlined as a loop: a
    /// lambda is only ever a direct argument of these, so there are no closures at run time.
    pub(super) fn higher_order(
        &mut self,
        list: Ex,
        elem: &Ty,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> Ex {
        let m = name.name.as_str();
        if m == "count" && args.is_empty() {
            return Ex::host(Imp::ListLen, vec![list], Ty::Int);
        }
        let lambda = match args {
            [a] if a.name.is_none() => super::optional::lambda_of(&a.value),
            _ => None,
        };
        let Some((param, body)) = lambda else {
            self.err(
                "E051",
                span,
                format!("`.{m}` takes one lambda such as `.{m}(x -> x.done)`, or a key path such as `.{m}(.done)`"),
            );
            for a in args {
                if !matches!(a.value.kind, ExprKind::Lambda { .. }) {
                    let _ = self.expr(&a.value, None);
                }
            }
            return Ex::error();
        };
        // Over a collection, a lambda the store can answer is asked of it first; the collection
        // is held in a local for that, which the loop reads too.
        let pred = match (&list.ty, m) {
            (Ty::Collection(_), "filter" | "count") => self.native_pred(elem, &param.name, &body),
            _ => None,
        };
        let (list, native) = match pred {
            Some(pred) => {
                let ty = list.ty.clone();
                let local = self.fresh(ty.clone());
                (Ex::local(local, ty), Some((local, list, pred)))
            }
            None => (list, None),
        };
        let mut fl = self.new_for(list, elem);
        let item = Ex::local(fl.item_local, elem.clone());
        self.push_sink();
        self.bind_local(&param.name, fl.item_local);
        let (result, result_ty) = if m == "map" {
            let v = self.expr(&body, None);
            let out_ty = Ty::List(Box::new(v.ty.clone()));
            let out = self.fresh(out_ty.clone());
            self.emit(Stmt::Eval(Self::push_item(
                Ex::local(out, out_ty.clone()),
                v,
            )));
            (out, out_ty)
        } else {
            self.filter_like(m, &body, &item, elem)
        };
        let mut inner = self.pop_sink();
        // Hoisted `Set` of the accumulator is emitted before the loop by the caller below.
        let init = match m {
            "map" | "filter" => Some(Ex::host(Imp::ListNew, Vec::new(), result_ty.clone())),
            "any" => Some(Ex::new(Ek::Bool(false), Ty::Bool)),
            "all" => Some(Ex::new(Ek::Bool(true), Ty::Bool)),
            "count" => Some(Ex::int(0)),
            _ => Some(Ex::host(Imp::NoneV, Vec::new(), result_ty.clone())),
        };
        fl.body.append(&mut inner);
        let mut stmts = Vec::new();
        if let Some(i) = init {
            stmts.push(Stmt::Set(result, i));
        }
        stmts.push(Stmt::ForList(Box::new(fl)));
        let looped = Ex::new(
            Ek::Seq(stmts, Box::new(Ex::local(result, result_ty.clone()))),
            result_ty,
        );
        match native {
            Some((local, held, pred)) => self.where_or_loop(local, held, m, &pred, looped),
            None => looped,
        }
    }

    /// `first()` or `last()`: the host reads the end of the list (`list_end`), so a view over a
    /// collection depends on that item alone, not on its length or the items before it.
    fn end_item(list: Ex, elem: &Ty, last: bool) -> Ex {
        let out = Ty::Option(Box::new(elem.clone()));
        let which = Ex::new(Ek::I32(i32::from(last)), Ty::Bool);
        Ex::host(Imp::ListEnd, vec![list, which], out)
    }

    /// `sort_by(x -> key)`: collects the keys in a parallel list, then asks the host for the
    /// items ordered by them (ascending, stable). Keys are Int, Float, Text or Bool.
    fn sort_by(
        &mut self,
        list: Ex,
        elem: &Ty,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> Ex {
        // `sort_by(key)` or `sort_by(key, desc)`: latest, largest or last first.
        let (key_arg, desc) = match args {
            [a] if a.name.is_none() => (Some(a), false),
            [a, d]
                if a.name.is_none()
                    && matches!(&d.value.kind, ExprKind::Ident(w) if w == "desc" || w == "asc") =>
            {
                (
                    Some(a),
                    matches!(&d.value.kind, ExprKind::Ident(w) if w == "desc"),
                )
            }
            _ => (None, false),
        };
        let lambda = key_arg.and_then(|a| super::optional::lambda_of(&a.value));
        let Some((param, body)) = lambda else {
            self.err(
                "E051",
                span,
                "`.sort_by` takes one lambda such as `.sort_by(x -> x.title)`, or a key path such as `.sort_by(.title)`, and `desc` for the largest or latest first: `.sort_by(.due, desc)`",
            );
            return Ex::error();
        };
        // Over a collection, a key the store can order by is asked of it first; the collection
        // is held in a local for that, which the loop reads too.
        let native = match &list.ty {
            Ty::Collection(_) => self.sort_key(elem, &param.name, &body),
            _ => None,
        };
        let (list, held) = match native {
            Some(_) => {
                let ty = list.ty.clone();
                let local = self.fresh(ty.clone());
                (Ex::local(local, ty), Some((local, list)))
            }
            None => (list, None),
        };
        let mut fl = self.new_for(list, elem);
        self.push_sink();
        self.bind_local(&param.name, fl.item_local);
        let key = match &native {
            // A version sorts by its time; none comes first.
            Some(super::order::SortKey::Version { last }) => {
                self.version_time(Ex::local(fl.item_local, elem.clone()), *last)
            }
            _ => self.expr(&body, None),
        };
        // A Date or a Time is ISO text, which sorts in time order.
        let key = match key.ty {
            Ty::Date | Ty::Time => key.typed(Ty::Text),
            _ => key,
        };
        if !matches!(
            key.ty,
            Ty::Int | Ty::Float | Ty::Text | Ty::Bool | Ty::Error
        ) {
            let t = self.show(&key.ty);
            self.err(
                "E020",
                name.span,
                format!("`sort_by` keys must be Int, Float, Text, Bool, Date or Time, found {t}"),
            );
        }
        let keys_ty = Ty::List(Box::new(key.ty.clone()));
        let keys = self.fresh(keys_ty.clone());
        self.emit(Stmt::Eval(Self::push_item(
            Ex::local(keys, keys_ty.clone()),
            key,
        )));
        let mut inner = self.pop_sink();
        fl.body.append(&mut inner);
        let out_ty = Ty::List(Box::new(elem.clone()));
        let list_local = fl.list_local;
        let stmts = vec![
            Stmt::Set(keys, Ex::host(Imp::ListNew, Vec::new(), keys_ty.clone())),
            Stmt::ForList(Box::new(fl)),
        ];
        let mut sorted = Ex::host(
            Imp::ListSortByKey,
            vec![Ex::local(list_local, Ty::Text), Ex::local(keys, keys_ty)],
            out_ty.clone(),
        );
        if desc {
            sorted = Ex::host(Imp::ListReverse, vec![sorted], out_ty.clone());
        }
        let looped = Ex::new(Ek::Seq(stmts, Box::new(sorted)), out_ty);
        match (native, held) {
            (Some(key), Some((local, held))) => {
                self.ordered_or_loop(local, held, &key, desc, looped)
            }
            _ => looped,
        }
    }

    /// The body of `filter`, `any`, `all`, `find` and `count`: tests the lambda on the item.
    fn filter_like(&mut self, m: &str, body: &ast::Expr, item: &Ex, elem: &Ty) -> (u32, Ty) {
        let pred = self.expr_as(body, &Ty::Bool);
        let (out_ty, on_true, on_false): (Ty, Vec<Stmt>, Vec<Stmt>);
        let out;
        match m {
            "filter" => {
                out_ty = Ty::List(Box::new(elem.clone()));
                out = self.fresh(out_ty.clone());
                on_true = vec![Stmt::Eval(Self::push_item(
                    Ex::local(out, out_ty.clone()),
                    item.clone(),
                ))];
                on_false = Vec::new();
            }
            "any" => {
                out_ty = Ty::Bool;
                out = self.fresh(Ty::Bool);
                on_true = vec![
                    Stmt::Set(out, Ex::new(Ek::Bool(true), Ty::Bool)),
                    Stmt::Break,
                ];
                on_false = Vec::new();
            }
            "all" => {
                out_ty = Ty::Bool;
                out = self.fresh(Ty::Bool);
                on_true = Vec::new();
                on_false = vec![
                    Stmt::Set(out, Ex::new(Ek::Bool(false), Ty::Bool)),
                    Stmt::Break,
                ];
            }
            "count" => {
                out_ty = Ty::Int;
                out = self.fresh(Ty::Int);
                let next = Ex::new(
                    Ek::Bin(
                        ir::BinOp::Add,
                        Box::new(Ex::local(out, Ty::Int)),
                        Box::new(Ex::int(1)),
                    ),
                    Ty::Int,
                );
                on_true = vec![Stmt::Set(out, next)];
                on_false = Vec::new();
            }
            _ => {
                out_ty = Ty::Option(Box::new(elem.clone()));
                out = self.fresh(out_ty.clone());
                on_true = vec![
                    Stmt::Set(out, Self::some(item.clone(), &out_ty)),
                    Stmt::Break,
                ];
                on_false = Vec::new();
            }
        }
        self.emit(Stmt::If(pred, on_true, on_false));
        (out, out_ty)
    }
}
