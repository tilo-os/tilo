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
        if let Some(c) = env.own_callable(name) {
            let c = c.clone();
            return self.call_callable(&c, args, span);
        }
        if let Some(t) = env.own_type(name) {
            let key = t.key.clone();
            return self.construct(&key, args);
        }
        match name {
            "outside" => return self.builtin_outside(args, span),
            "model_call" => return self.builtin_model(args, span),
            "run_block" => return self.builtin_run_block(args, span),
            "set" => return self.builtin_set(args, span),
            "open" => return self.builtin_open(args, span),
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
        self.err(
            "E014",
            name_span,
            format!(
                "unknown function `{name}`; declared here: {}",
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
                "package `{}` has no `{}`; it declares: {}",
                import.alias,
                name.name,
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
            let v = self.expr_as(&a.value, &p.ty);
            match slots.get_mut(i) {
                Some(slot @ None) => *slot = Some(v),
                _ => self.err(
                    "E022",
                    a.value.span,
                    format!("the parameter `{}` of `{name}` is given twice", p.name),
                ),
            }
        }
        let missing: Vec<&str> = params
            .iter()
            .zip(&slots)
            .filter(|(_, s)| s.is_none())
            .map(|(p, _)| p.name.as_str())
            .collect();
        if !missing.is_empty() {
            self.err(
                "E022",
                span,
                format!("`{name}` is missing {}; it is {sig}", missing.join(", ")),
            );
        }
        slots
            .into_iter()
            .map(|s| s.unwrap_or_else(Ex::error))
            .collect()
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
        let bound = self.bind_args(&c.name, &c.params, args, span, &sig);
        let ctx = self.st.ctx;
        match (c.kind, ctx) {
            (CKind::Fn, _) => self.call_own(c, bound, span, "fn"),
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
                self.call_own(c, bound, span, "action")
            }
            (CKind::Action, Ctx::Flow) => self.invoke(c, bound),
            (CKind::Action | CKind::Flow, Ctx::View | Ctx::Block) => self.action_value(c, bound),
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
        let v = self.expr_as(&value.value, &state.ty);
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
    /// (workspace-model.md section 3). Views only.
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
        for a in args.iter().skip(1) {
            let named_view = a.name.as_ref().is_some_and(|n| n.name == "view");
            if !named_view {
                self.err(
                    "E022",
                    a.value.span,
                    "`open` takes the resource and, optionally, `view: <view>`",
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
        Ex::host(Imp::ActOpen, vec![r, Ex::str(&view_id)], Ty::Action)
    }

    /// The id of the view `e` names (`editor` or `notes.editor`), when it shows a `key` alone.
    fn view_for(&self, e: &ast::Expr, key: &TypeKey) -> Option<String> {
        let path = dotted(e)?;
        let c = match path.as_slice() {
            [one] => self.env.own_callable(one)?,
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
            (Ty::Collection(_), "insert" | "remove" | "move")
                | (Ty::Set(_), "add" | "remove")
                | (Ty::Map(_), "put" | "remove")
        );
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
            )
        {
            let t = self.show(&f.ty);
            let ops = match &f.ty {
                Ty::Collection(_) => "insert, remove, move, len and the list methods",
                Ty::Set(_) => "add, remove, len and the list methods",
                _ => "put, remove, get (an Option of the value) and keys",
            };
            self.err(
                "E027",
                name.span,
                format!("`{}` is a {t} field; its methods are: {ops}", f.name),
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
                let v = self.expr_as(&a.value, t);
                Ex::host(Imp::CollInsert, vec![b, fid, v], (**t).clone())
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
            (Ty::Collection(t), "move") => {
                if !self.arity(name, args, 2, span) {
                    return Ex::error();
                }
                let (Some(a), Some(i)) = (args.first(), args.get(1)) else {
                    return Ex::error();
                };
                let item = self.expr_as(&a.value, t);
                let idx = self.expr_as(&i.value, &Ty::Int);
                Ex::host(Imp::CollMove, vec![b, fid, item, idx], Ty::Unit)
            }
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
        let ty = Ty::Option(Box::new(value.clone()));
        Ex::host(Imp::MapGetH, vec![b, fid, key], ty)
    }

    fn value_method(&mut self, r: Ex, name: &ast::Ident, args: &[ast::Arg], span: Span) -> Ex {
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
            (Ty::Principal, "name") if args.is_empty() => {
                Ex::host(Imp::PrincipalName, vec![r], Ty::Text)
            }
            (Ty::Int, "to_float") if args.is_empty() => {
                Ex::new(Ek::Conv(Conv::IntToFloat, Box::new(r)), Ty::Float)
            }
            (Ty::Float, "to_int") if args.is_empty() => {
                Ex::new(Ek::Conv(Conv::FloatToInt, Box::new(r)), Ty::Int)
            }
            (Ty::Int | Ty::Float | Ty::Bool, "to_text") if args.is_empty() => self.text_of(r, span),
            (Ty::Record(_), "id") if args.is_empty() => Ex::host(Imp::RefId, vec![r], Ty::Text),
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
            (Ty::List(t) | Ty::Set(t) | Ty::Collection(t), "sort_by") => {
                let elem = (**t).clone();
                self.sort_by(r, &elem, name, args, span)
            }
            (other, _) => {
                let t = self.show(other);
                let known = match other {
                    Ty::Text => "len",
                    Ty::Int => "to_float, to_text",
                    Ty::Float => "to_int, to_text",
                    Ty::Bool => "to_text",
                    Ty::Record(_) => "id (and fields)",
                    Ty::Option(_) | Ty::Result(..) | Ty::Enum(_) => {
                        "none; take it apart with `match`, such as `match x { .some(v) => v, .none => 0 }`"
                    }
                    Ty::List(_) | Ty::Set(_) | Ty::Collection(_) => {
                        "len, is_empty, map, filter, any, all, find, count"
                    }
                    _ => "none",
                };
                self.err(
                    "E027",
                    name.span,
                    format!(
                        "{t} has no method `{m}` taking {} argument(s); its methods: {known}",
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

    /// `map`, `filter`, `any`, `all`, `find` and `count` with a lambda, inlined as a loop
    /// (implementation.md section 3.6).
    fn higher_order(
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
            [a] if a.name.is_none() => match &a.value.kind {
                ExprKind::Lambda { param, body } => Some((param, body)),
                _ => None,
            },
            _ => None,
        };
        let Some((param, body)) = lambda else {
            self.err(
                "E051",
                span,
                format!("`.{m}` takes one lambda such as `.{m}(x -> x.done)`"),
            );
            for a in args {
                if !matches!(a.value.kind, ExprKind::Lambda { .. }) {
                    let _ = self.expr(&a.value, None);
                }
            }
            return Ex::error();
        };
        let mut fl = self.new_for(list, elem);
        let item = Ex::local(fl.item_local, elem.clone());
        self.push_sink();
        self.bind_local(&param.name, fl.item_local);
        let (result, result_ty) = if m == "map" {
            let v = self.expr(body, None);
            let out_ty = Ty::List(Box::new(v.ty.clone()));
            let out = self.fresh(out_ty.clone());
            self.emit(Stmt::Eval(Self::push_item(
                Ex::local(out, out_ty.clone()),
                v,
            )));
            (out, out_ty)
        } else {
            self.filter_like(m, body, &item, elem)
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
        Ex::new(
            Ek::Seq(stmts, Box::new(Ex::local(result, result_ty.clone()))),
            result_ty,
        )
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
        let lambda = match args {
            [a] if a.name.is_none() => match &a.value.kind {
                ExprKind::Lambda { param, body } => Some((param, body)),
                _ => None,
            },
            _ => None,
        };
        let Some((param, body)) = lambda else {
            self.err(
                "E051",
                span,
                "`.sort_by` takes one lambda such as `.sort_by(x -> x.title)`",
            );
            return Ex::error();
        };
        let mut fl = self.new_for(list, elem);
        self.push_sink();
        self.bind_local(&param.name, fl.item_local);
        let key = self.expr(body, None);
        if !matches!(
            key.ty,
            Ty::Int | Ty::Float | Ty::Text | Ty::Bool | Ty::Error
        ) {
            let t = self.show(&key.ty);
            self.err(
                "E020",
                name.span,
                format!("`sort_by` keys must be Int, Float, Text or Bool, found {t}"),
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
        let sorted = Ex::host(
            Imp::ListSortByKey,
            vec![Ex::local(list_local, Ty::Text), Ex::local(keys, keys_ty)],
            out_ty.clone(),
        );
        Ex::new(Ek::Seq(stmts, Box::new(sorted)), out_ty)
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
