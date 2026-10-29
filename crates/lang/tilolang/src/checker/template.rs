//! Views a call expands where it stands: generic, higher-order views.
//!
//! A view whose parameters are written over type parameters (`view list<T>(items: List<T>)`),
//! or that takes view state (`filter: State<F>`), a function (`count: (F) -> Int`) or a view
//! of an item (`row: (T) -> View`), is a template. It has no function of its own: each call
//! checks its body where the call stands, its type parameters taken from what the call
//! passes, its state parameters naming the caller's view state, its function parameters the
//! caller's lambdas and its view parameters the caller's blocks, each checked in the caller's
//! scope when the body applies it. What it builds is the caller's: its reads, its view state
//! and its nodes, keyed under the call. The standard library (`std`) is written this way, and
//! an app's own views can be.

use std::collections::HashMap;
use std::rc::Rc;

use super::{Checker, Ex, StateRef, UiPlace};
use crate::abi::Imp;
use crate::ast::{self, ExprKind, TypeExpr, TypeExprKind, UiCall, UiKind, UiStmt};
use crate::diag::Span;
use crate::ir::{Ek, LocalId, Stmt};
use crate::resolve::{CKind, Callable, Template};
use crate::types::Ty;

/// What an optional template parameter holds where its uses are expanded.
pub(crate) enum Static {
    /// Left out.
    Absent,
    /// Given.
    Given(Macro),
}

impl Static {
    /// What was given, if anything.
    pub(crate) fn given(self) -> Option<Macro> {
        match self {
            Self::Given(m) => Some(m),
            Self::Absent => None,
        }
    }
}

/// What a template's parameter that is not a value is bound to where the template expands.
#[derive(Clone)]
pub(crate) enum Macro {
    /// A `State<E>` parameter: view state of the caller.
    State(StateRef),
    /// A function parameter, given a lambda.
    Lambda(Rc<Closure>),
    /// A view parameter (`View`, or `(T) -> View` with an item), given a block.
    Block(Rc<UiClosure>),
    /// A view-of-an-item parameter given a view of its own: the item type's row view.
    View(Rc<Callable>),
    /// An `Option<...>` parameter of one of the above, given one.
    Some(Box<Macro>),
    /// An `Option<...>` parameter left out.
    Absent,
}

/// Names as a piece of code sees them: what a lambda or a block given to a template reads
/// when the template applies it.
#[derive(Clone, Default)]
pub(crate) struct Scope {
    pub names: HashMap<String, LocalId>,
    pub states: HashMap<String, StateRef>,
    pub macros: HashMap<String, Macro>,
    /// The package whose names the code sees: `None` for the module's own, `std` for the
    /// standard library's.
    pub ns: Option<String>,
    pub types: HashMap<String, Ty>,
    /// What the keys of the nodes it makes start with, after the view's id.
    pub prefix: String,
    pub ui: UiPlace,
}

/// A lambda given to a template, with the scope it was written in.
pub(crate) struct Closure {
    pub params: Vec<(String, Ty)>,
    pub result: Ty,
    pub body: ast::Expr,
    pub scope: Scope,
}

/// A block given to a template, with the scope it was written in: its item, when it shows one.
pub(crate) struct UiClosure {
    pub item: Option<(String, Ty)>,
    pub body: Vec<UiStmt>,
    pub scope: Scope,
}

/// What a template's parameter takes.
enum Kind<'a> {
    Value(&'a TypeExpr),
    State(&'a TypeExpr),
    Function(&'a [TypeExpr], &'a TypeExpr),
    View,
    Optional(Box<Kind<'a>>),
}

/// What a parameter of a template takes, read off its written type.
fn kind_of(te: &TypeExpr) -> Kind<'_> {
    match &te.kind {
        TypeExprKind::Generic(name, args) if name.name == "State" && args.len() == 1 => {
            Kind::State(&args[0])
        }
        TypeExprKind::Generic(name, args) if name.name == "Option" && args.len() == 1 => {
            match kind_of(&args[0]) {
                Kind::Value(_) => Kind::Value(te),
                other => Kind::Optional(Box::new(other)),
            }
        }
        TypeExprKind::Fn(params, result) => Kind::Function(params, result),
        TypeExprKind::Named(path) if path.len() == 1 && path[0].name == "View" => Kind::View,
        _ => Kind::Value(te),
    }
}

/// Whether a view declared with these type parameters and parameters is a template.
#[must_use]
pub fn is_template(view: &ast::ViewDecl) -> bool {
    !view.type_params.is_empty()
        || view
            .params
            .iter()
            .any(|p| !matches!(kind_of(&p.ty), Kind::Value(_) | Kind::View))
}

/// What a scope saved while code of another scope is checked.
pub(crate) struct Saved {
    scopes: Vec<HashMap<String, LocalId>>,
    states: HashMap<String, StateRef>,
    macros: HashMap<String, Macro>,
    ns: Option<String>,
    types: HashMap<String, Ty>,
    prefix: String,
    ui: UiPlace,
}

impl Checker<'_> {
    /// The names as the code being checked sees them now.
    pub(crate) fn capture_scope(&self) -> Scope {
        let mut names = HashMap::new();
        for scope in &self.st.scopes {
            for (name, local) in scope {
                names.insert(name.clone(), *local);
            }
        }
        Scope {
            names,
            states: self.st.states.clone(),
            macros: self.st.macros.clone(),
            ns: self.st.ns.clone(),
            types: self.st.types.clone(),
            prefix: self.st.prefix.clone(),
            ui: self.st.ui,
        }
    }

    /// Check what follows as code written in `scope`, with `extra` names bound beside its
    /// own, until [`Self::leave`].
    pub(crate) fn enter(&mut self, scope: &Scope, extra: HashMap<String, LocalId>) -> Saved {
        let mut names = scope.names.clone();
        names.extend(extra);
        Saved {
            scopes: std::mem::replace(&mut self.st.scopes, vec![names]),
            states: std::mem::replace(&mut self.st.states, scope.states.clone()),
            macros: std::mem::replace(&mut self.st.macros, scope.macros.clone()),
            ns: std::mem::replace(&mut self.st.ns, scope.ns.clone()),
            types: std::mem::replace(&mut self.st.types, scope.types.clone()),
            prefix: std::mem::replace(&mut self.st.prefix, scope.prefix.clone()),
            ui: std::mem::replace(&mut self.st.ui, scope.ui),
        }
    }

    pub(crate) fn leave(&mut self, saved: Saved) {
        self.st.scopes = saved.scopes;
        self.st.states = saved.states;
        self.st.macros = saved.macros;
        self.st.ns = saved.ns;
        self.st.types = saved.types;
        self.st.prefix = saved.prefix;
        self.st.ui = saved.ui;
    }

    /// Whether the code being checked is the standard library's.
    pub(crate) fn in_std(&self) -> bool {
        self.st.ns.as_deref() == Some(crate::stdlib::NS)
    }

    /// The module's own callable `name`, when the code being checked sees the module's names.
    pub(crate) fn own_callable(&self, name: &str) -> Option<&Callable> {
        if self.in_std() {
            return None;
        }
        self.env.own_callable(name)
    }

    /// The template `name` the code being checked sees by its plain name: its own package's.
    pub(crate) fn template_named(&self, name: &str) -> Option<&Template> {
        self.env.template(self.st.ns.as_deref(), name)
    }

    /// The template `base.name`: `std.list`.
    pub(crate) fn template_at(&self, base: &ast::Expr, name: &str) -> Option<&Template> {
        let path = super::expr::dotted(base)?.join(".");
        if self.lookup(&path).is_some() {
            return None;
        }
        let import = self.env.find_import(&path)?;
        let ns = if import.app == self.env.app {
            None
        } else {
            Some(import.app.as_str())
        };
        self.env.template(ns, name)
    }

    /// The type a written type of a template names, with its type parameters as `types`
    /// binds them (an unbound one is an error type).
    pub(crate) fn type_in(&mut self, te: &TypeExpr, types: &HashMap<String, Ty>) -> Ty {
        match &te.kind {
            TypeExprKind::Named(path) if path.len() == 1 && types.contains_key(&path[0].name) => {
                types.get(&path[0].name).cloned().unwrap_or(Ty::Error)
            }
            TypeExprKind::Generic(name, args) if args.len() == 1 => {
                let inner = Box::new(self.type_in(&args[0], types));
                match name.name.as_str() {
                    "List" => Ty::List(inner),
                    "Option" => Ty::Option(inner),
                    "Set" => Ty::Set(inner),
                    "Map" => Ty::Map(inner),
                    "Collection" => Ty::Collection(inner),
                    _ => Ty::Error,
                }
            }
            TypeExprKind::Tuple(items) if !items.is_empty() => {
                Ty::Tuple(items.iter().map(|t| self.type_in(t, types)).collect())
            }
            TypeExprKind::Fn(..) => Ty::Error,
            _ => {
                let resolved = crate::resolve::resolve_type_expr(self.env, te, &mut Vec::new());
                if resolved.is_error() {
                    self.err(
                        "E013",
                        te.span,
                        format!("unknown type `{}`", crate::print::print_type(te)),
                    );
                }
                resolved
            }
        }
    }

    /// Bind the type parameters `te` mentions so that it names `ty`. Whether it can.
    fn unify(te: &TypeExpr, ty: &Ty, params: &[String], types: &mut HashMap<String, Ty>) -> bool {
        match (&te.kind, ty) {
            (_, Ty::Error) => true,
            (TypeExprKind::Named(path), _) if path.len() == 1 && params.contains(&path[0].name) => {
                match types.get(&path[0].name) {
                    Some(bound) if !bound.is_error() => bound.same(ty),
                    _ => {
                        types.insert(path[0].name.clone(), ty.clone());
                        true
                    }
                }
            }
            (TypeExprKind::Generic(name, args), _) if args.len() == 1 => {
                let (("List", Ty::List(inner) | Ty::Collection(inner) | Ty::Set(inner))
                | ("Option", Ty::Option(inner))
                | ("Set", Ty::Set(inner))
                | ("Collection", Ty::Collection(inner))) = (name.name.as_str(), ty)
                else {
                    return true;
                };
                Self::unify(&args[0], inner, params, types)
            }
            _ => true,
        }
    }

    /// Expand the template `t` where the UI call `c` stands: its nodes under `parent`, or,
    /// at a view's top, the one node it makes (a fragment of them when it makes several).
    pub(crate) fn expand(
        &mut self,
        t: &Template,
        c: &UiCall,
        args: &[ast::Arg],
        parent: Option<LocalId>,
    ) -> LocalId {
        let span = c.call.span;
        let view = &t.decl;
        if self.st.expanding.contains(&view.name.name) {
            self.err(
                "E061",
                span,
                format!(
                    "`{}` expands into itself; a view that takes view state, functions or views of an item cannot call itself",
                    view.name.name
                ),
            );
            return self.dead_node();
        }
        let caller = self.capture_scope();
        let params: Vec<String> = view.type_params.iter().map(|p| p.name.clone()).collect();
        // Each type parameter unbound until what a call passes binds it.
        let mut types: HashMap<String, Ty> =
            params.iter().map(|p| (p.clone(), Ty::Error)).collect();
        // What each parameter is given: by position, by name, and the call's block.
        let mut given: Vec<Option<&ast::Arg>> = vec![None; view.params.len()];
        let mut at = 0usize;
        for a in args {
            let index = match &a.name {
                Some(name) => view.params.iter().position(|p| p.name.name == name.name),
                None => {
                    let next = (at..view.params.len()).find(|i| given[*i].is_none());
                    at = next.map_or(at, |i| i + 1);
                    next
                }
            };
            match index {
                Some(i) if given[i].is_none() => given[i] = Some(a),
                Some(_) => self.err(
                    "E012",
                    a.value.span,
                    format!("`{}` is given one argument twice", view.name.name),
                ),
                None => {
                    let names: Vec<&str> =
                        view.params.iter().map(|p| p.name.name.as_str()).collect();
                    self.err(
                        "E022",
                        a.value.span,
                        format!(
                            "`{}` takes {}; this argument has no parameter",
                            view.name.name,
                            names.join(", ")
                        ),
                    );
                }
            }
        }
        // The block: named slots for the view parameters it names, the rest (or the item
        // block) for the view parameter of the content.
        let (slots, content) = split_block(c);
        let mut values: HashMap<String, LocalId> = HashMap::new();
        let mut states: HashMap<String, StateRef> = HashMap::new();
        let mut macros: HashMap<String, Macro> = HashMap::new();
        // A parameter of a template being expanded, passed on by its name: what it holds.
        let passed = |i: usize| -> Option<Macro> {
            let ExprKind::Ident(n) = &given[i]?.value.kind else {
                return None;
            };
            let m = caller.macros.get(n)?;
            (!matches!(m, Macro::State(_)) || !caller.names.contains_key(n)).then(|| m.clone())
        };
        // Values and view state first: they bind the type parameters lambdas are read with.
        for (i, p) in view.params.iter().enumerate() {
            if !matches!(kind_of(&p.ty), Kind::Value(_))
                && let Some(m) = passed(i)
            {
                let held = match &m {
                    Macro::Some(inner) => inner.as_ref(),
                    other => other,
                };
                if let Macro::State(state) = held {
                    let te = match kind_of(&p.ty) {
                        Kind::State(te) => Some(te),
                        Kind::Optional(inner) => match *inner {
                            Kind::State(te) => Some(te),
                            _ => None,
                        },
                        _ => None,
                    };
                    if let Some(te) = te {
                        Self::unify(te, &state.ty, &params, &mut types);
                    }
                    if !matches!(m, Macro::Some(_) | Macro::Absent) {
                        values.insert(p.name.name.clone(), state.local);
                        states.insert(p.name.name.clone(), state.clone());
                    }
                }
                macros.insert(p.name.name.clone(), m);
                continue;
            }
            match kind_of(&p.ty) {
                Kind::Value(te) => {
                    let Some(a) = given[i] else { continue };
                    let want = self.type_in(te, &types);
                    let v = self.expr(&a.value, (!want.is_error()).then_some(&want));
                    let v = if matches!(&te.kind, TypeExprKind::Generic(..)) {
                        self.unhold(v, a.value.span)
                    } else {
                        v
                    };
                    if !Self::unify(te, &v.ty, &params, &mut types) {
                        let (w, f) = (self.show(&want), self.show(&v.ty));
                        self.err(
                            "E020",
                            a.value.span,
                            format!(
                                "`{}` takes {w} as `{}`; found {f}",
                                view.name.name, p.name.name
                            ),
                        );
                    }
                    let want = self.type_in(te, &types);
                    let v = if want.is_error() || v.ty.same(&want) || coerces(&v.ty, &want) {
                        v
                    } else {
                        self.coerce(v, &want, a.value.span)
                    };
                    let local = self.fresh(v.ty.clone());
                    self.emit(Stmt::Set(local, v));
                    values.insert(p.name.name.clone(), local);
                }
                Kind::State(te) => match given[i] {
                    Some(a) => {
                        if let Some(state) = self.state_arg(&a.value, view, p) {
                            Self::unify(te, &state.ty, &params, &mut types);
                            values.insert(p.name.name.clone(), state.local);
                            states.insert(p.name.name.clone(), state.clone());
                            macros.insert(p.name.name.clone(), Macro::State(state));
                        }
                    }
                    None => self.err(
                        "E022",
                        span,
                        format!(
                            "`{}` needs `{}`: a `window` or `lens` declaration of this view",
                            view.name.name, p.name.name
                        ),
                    ),
                },
                Kind::Optional(inner) => match (inner.as_ref(), given[i]) {
                    (Kind::State(te), Some(a)) => {
                        if let Some(state) = self.state_arg(&a.value, view, p) {
                            Self::unify(te, &state.ty, &params, &mut types);
                            macros.insert(
                                p.name.name.clone(),
                                Macro::Some(Box::new(Macro::State(state))),
                            );
                        }
                    }
                    (Kind::State(_), None) => {
                        macros.insert(p.name.name.clone(), Macro::Absent);
                    }
                    _ => {}
                },
                Kind::Function(..) | Kind::View => {}
            }
        }
        // Functions and views, read with the type parameters bound.
        for (i, p) in view.params.iter().enumerate() {
            if macros.contains_key(&p.name.name) {
                continue;
            }
            let (optional, kind) = match kind_of(&p.ty) {
                Kind::Optional(inner) => (true, *inner),
                other => (false, other),
            };
            let made = match kind {
                Kind::Function(fparams, result) => {
                    let item_block = matches!(&result.kind, TypeExprKind::Named(path) if path.len() == 1 && path[0].name == "View");
                    self.function_arg(
                        given[i],
                        (fparams, result, item_block),
                        (&content, &caller, &types),
                        (view, p, span),
                    )
                }
                Kind::View => {
                    let body = slots
                        .iter()
                        .find(|(name, _)| name.name == p.name.name)
                        .map(|(_, body)| (*body).clone())
                        .or_else(|| {
                            (content.item.is_none() && !content.body.is_empty())
                                .then(|| content.body.clone())
                                .filter(|_| is_content_param(view, p))
                        });
                    body.map(|body| {
                        Macro::Block(Rc::new(UiClosure {
                            item: None,
                            body,
                            scope: caller.clone(),
                        }))
                    })
                }
                _ => continue,
            };
            match (made, optional) {
                (Some(m), true) => {
                    macros.insert(p.name.name.clone(), Macro::Some(Box::new(m)));
                }
                (Some(m), false) => {
                    macros.insert(p.name.name.clone(), m);
                }
                (None, true) => {
                    macros.insert(p.name.name.clone(), Macro::Absent);
                }
                (None, false) => {
                    if matches!(kind_of(&p.ty), Kind::View) {
                        // A view parameter left out holds nothing.
                        macros.insert(
                            p.name.name.clone(),
                            Macro::Block(Rc::new(UiClosure {
                                item: None,
                                body: Vec::new(),
                                scope: caller.clone(),
                            })),
                        );
                    } else {
                        self.err(
                            "E022",
                            span,
                            format!("`{}` needs `{}`", view.name.name, p.name.name),
                        );
                    }
                }
            }
        }
        for (name, _) in &slots {
            if !view.params.iter().any(|p| p.name.name == name.name) {
                self.err(
                    "E063",
                    name.span,
                    format!("`{}` takes no view `{}`", view.name.name, name.name),
                );
            }
        }
        // The template's own scope: its parameters, its package's names.
        let scope = Scope {
            names: values,
            states,
            macros,
            ns: t.ns.clone(),
            types,
            prefix: format!("{}t{}.", self.st.prefix, c.node),
            ui: self.st.ui,
        };
        let saved = self.enter(&scope, HashMap::new());
        self.st.expanding.push(view.name.name.clone());
        // The defaults of the values left out, read in the template's scope.
        for (i, p) in view.params.iter().enumerate() {
            let Kind::Value(te) = kind_of(&p.ty) else {
                continue;
            };
            if given[i].is_some() {
                continue;
            }
            let Some(default) = &p.default else {
                self.err(
                    "E022",
                    span,
                    format!("`{}` needs `{}`", view.name.name, p.name.name),
                );
                continue;
            };
            let types = self.st.types.clone();
            let want = self.type_in(te, &types);
            let v = self.expr_as(default, &want);
            let local = self.declare(&p.name.name, v.ty.clone());
            self.emit(Stmt::Set(local, v));
        }
        let body = self.template_state(view, &view.body);
        let node = self.template_body(&body, parent);
        self.st.expanding.pop();
        self.leave(saved);
        node
    }

    /// Lowers the `window` and `lens` declarations at the top of a template's body: state of
    /// the view it expands in, kept under that view's id and the template's name
    /// (`<view id>:list.query`), as the view's own is under its name. Returns the rest of the
    /// body.
    fn template_state(&mut self, view: &ast::ViewDecl, body: &[UiStmt]) -> Vec<UiStmt> {
        let mut rest = Vec::new();
        for s in body {
            let UiKind::State(d) = &s.kind else {
                rest.push(s.clone());
                continue;
            };
            let types = self.st.types.clone();
            let ty = self.type_in(&d.ty, &types);
            let base = format!("{}:{}.{}", self.st.view_id, view.name.name, d.name.name);
            let taken = |id: &str| self.states.iter().any(|(_, s)| s.id == id);
            let mut id = base.clone();
            let mut n = 2;
            while taken(&id) {
                id = format!("{base}#{n}");
                n += 1;
            }
            let fallback = match &d.default {
                Some(e) => self.expr_as(e, &ty),
                None => self.zero_of(&ty, d.name.span),
            };
            let layer = Ex::new(Ek::I32(d.layer.code()), Ty::Bool);
            let args = vec![layer, Ex::str(&id), Self::handle_of(fallback)];
            let value = if ty.is_scalar() {
                let boxed = Ty::Option(Box::new(ty.clone()));
                Self::unwrap_opt(Ex::host(Imp::StateGet, args, boxed), &ty)
            } else {
                Ex::host(Imp::StateGet, args, ty.clone())
            };
            let local = self.declare(&d.name.name, ty.clone());
            self.emit(Stmt::Set(local, value));
            self.st.states.insert(
                d.name.name.clone(),
                StateRef {
                    layer: d.layer,
                    id: id.clone(),
                    ty: ty.clone(),
                    local,
                },
            );
            self.states.push((
                self.st.view_id.clone(),
                crate::resolve::StateInfo {
                    view: self.st.view_id.clone(),
                    name: d.name.name.clone(),
                    id,
                    layer: d.layer,
                    ty,
                },
            ));
        }
        rest
    }

    /// The caller's view state a `State<E>` parameter is given: a plain name.
    fn state_arg(
        &mut self,
        value: &ast::Expr,
        view: &ast::ViewDecl,
        p: &ast::Param,
    ) -> Option<StateRef> {
        let state = match &value.kind {
            ExprKind::Ident(name) => self.state_named(name),
            _ => None,
        };
        if state.is_none() {
            let known = self.state_names();
            self.err(
                "E066",
                value.span,
                format!(
                    "`{}` of `{}` takes view state by its name, a `window` or `lens` declaration of this view; this view declares: {known}",
                    p.name.name, view.name.name
                ),
            );
        }
        state
    }

    /// What a function parameter is given: a lambda, the call's item block, a view's name, or
    /// for a view of an item left out, the item type's row view.
    fn function_arg(
        &mut self,
        arg: Option<&ast::Arg>,
        (fparams, result, item_block): (&[TypeExpr], &TypeExpr, bool),
        (content, caller, types): (&Content, &Scope, &HashMap<String, Ty>),
        (view, p, span): (&ast::ViewDecl, &ast::Param, Span),
    ) -> Option<Macro> {
        let param_tys: Vec<Ty> = fparams.iter().map(|t| self.type_in(t, types)).collect();
        if let Some(a) = arg {
            let lambda = super::optional::lambda_of(&a.value).filter(|_| !item_block);
            if let Some((param, body)) = lambda {
                if param_tys.len() != 1 {
                    self.err(
                        "E050",
                        a.value.span,
                        format!(
                            "`{}` takes a function of {} values; a lambda takes one",
                            p.name.name,
                            param_tys.len()
                        ),
                    );
                    return None;
                }
                let result = self.type_in(result, types);
                return Some(Macro::Lambda(Rc::new(Closure {
                    params: vec![(param.name, param_tys[0].clone())],
                    result,
                    body,
                    scope: caller.clone(),
                })));
            }
            return match &a.value.kind {
                ExprKind::Lambda { param, body } if !item_block => {
                    if param_tys.len() != 1 {
                        self.err(
                            "E050",
                            a.value.span,
                            format!(
                                "`{}` takes a function of {} values; a lambda takes one",
                                p.name.name,
                                param_tys.len()
                            ),
                        );
                        return None;
                    }
                    let result = self.type_in(result, types);
                    Some(Macro::Lambda(Rc::new(Closure {
                        params: vec![(param.name.clone(), param_tys[0].clone())],
                        result,
                        body: (**body).clone(),
                        scope: caller.clone(),
                    })))
                }
                ExprKind::Ident(name) if item_block => {
                    let found = self.own_callable(name).cloned();
                    match found {
                        Some(v) if v.kind == CKind::View && v.params.len() == 1 => {
                            Some(Macro::View(Rc::new(v)))
                        }
                        _ => {
                            self.err("E060", a.value.span, format!("`{}` takes a view of one item: a block `{{ x -> ... }}`, or the name of a view that takes the item", p.name.name));
                            None
                        }
                    }
                }
                _ => {
                    self.err(
                        "E050",
                        a.value.span,
                        format!(
                            "`{}` of `{}` takes a lambda, such as `x -> ...`",
                            p.name.name, view.name.name
                        ),
                    );
                    None
                }
            };
        }
        if !item_block {
            return None;
        }
        let item_ty = param_tys.first().cloned().unwrap_or(Ty::Error);
        if let Some((item, ())) = &content.item {
            return Some(Macro::Block(Rc::new(UiClosure {
                item: Some((item.name.clone(), item_ty)),
                body: content.body.clone(),
                scope: Scope {
                    ui: UiPlace {
                        list_row_ok: true,
                        ..caller.ui
                    },
                    ..caller.clone()
                },
            })));
        }
        // Left out: the item type's row view.
        if let Ty::Record(key) = &item_ty
            && let Some(v) = self.env.row_view(key)
        {
            return Some(Macro::View(Rc::new(v.clone())));
        }
        if !item_ty.is_error() {
            let t = self.show(&item_ty);
            self.err("E097", span, format!("`{}` needs the rows of its items: write an item block, `{{ x -> row(x.title) }}`, or declare the type's row, `row view <view> for {t}`", view.name.name));
        }
        None
    }

    /// A template's body where its call stands: its nodes under `parent`, or the one node it
    /// makes at a view's top.
    fn template_body(&mut self, body: &[UiStmt], parent: Option<LocalId>) -> LocalId {
        if let Some(p) = parent {
            self.ui_stmts(body, p);
            return p;
        }
        let one = match body.split_last() {
            Some((
                UiStmt {
                    kind: UiKind::Call(c),
                    ..
                },
                lets,
            )) if lets.iter().all(|s| matches!(s.kind, UiKind::Let { .. })) => Some((lets, c)),
            _ => None,
        };
        if let Some((lets, c)) = one {
            let dead = self.fresh(Ty::Error);
            self.ui_stmts(lets, dead);
            return self.ui_call(c, None);
        }
        let key = self.keyed(&format!("{}.{}body", self.st.view_id, self.st.prefix));
        let root = self.new_node("fragment", key);
        self.ui_stmts(body, root);
        root
    }

    /// Apply the macro `m`, named `name`, as a UI statement under `parent`: a view parameter's
    /// block, or a view, given `args`.
    pub(crate) fn apply_ui(
        &mut self,
        name: &str,
        m: &Macro,
        c: &UiCall,
        args: &[ast::Arg],
        parent: Option<LocalId>,
    ) -> LocalId {
        let span = c.call.span;
        let parent = match parent {
            Some(p) => p,
            None => {
                let key = self.node_key(c);
                self.new_node("fragment", key)
            }
        };
        match m {
            Macro::Block(block) => {
                let mut extra = HashMap::new();
                match (&block.item, args) {
                    (Some((item, ty)), [a]) => {
                        let v = self.expr_as(&a.value, ty);
                        let local = self.fresh(ty.clone());
                        self.emit(Stmt::Set(local, v));
                        extra.insert(item.clone(), local);
                    }
                    (None, []) => {}
                    _ => {
                        self.err(
                            "E022",
                            span,
                            format!(
                                "`{name}` takes {}",
                                if block.item.is_some() {
                                    "one item"
                                } else {
                                    "no arguments"
                                }
                            ),
                        );
                        return parent;
                    }
                }
                let saved = self.enter(&block.scope, extra);
                if name == "composer" {
                    self.st.ui.composer = true;
                }
                self.ui_stmts(&block.body, parent);
                self.leave(saved);
            }
            // An optional view renders when it was given, and nothing when it was left out.
            Macro::Some(inner) => {
                return self.apply_ui(name, inner, c, args, Some(parent));
            }
            Macro::Absent => {}
            Macro::View(v) => {
                let [a] = args else {
                    self.err("E022", span, format!("`{name}` takes one item"));
                    return parent;
                };
                let ty = v.params.first().map_or(Ty::Error, |p| p.ty.clone());
                let item = self.expr_as(&a.value, &ty);
                let key = self.node_key(c);
                let row = self.view_node(v, vec![item], key, span);
                self.emit(Stmt::Eval(Ex::host(
                    Imp::UiChild,
                    vec![Ex::local(parent, Ty::View), Ex::local(row, Ty::View)],
                    Ty::Unit,
                )));
            }
            _ => {
                self.err(
                    "E060",
                    span,
                    format!("`{name}` is not a view; place a view parameter, a block or a view"),
                );
            }
        }
        parent
    }

    /// Apply the lambda `name` to `args`, as an expression.
    pub(crate) fn apply_lambda(&mut self, name: &str, args: &[ast::Arg], span: Span) -> Ex {
        let Some(Macro::Lambda(closure)) = self.st.macros.get(name).cloned() else {
            self.err("E026", span, format!("`{name}` is not a function"));
            return Ex::error();
        };
        if args.len() != closure.params.len() {
            self.err(
                "E022",
                span,
                format!("`{name}` takes {} argument(s)", closure.params.len()),
            );
            return Ex::error();
        }
        let mut extra = HashMap::new();
        for ((pname, pty), a) in closure.params.iter().zip(args) {
            let v = self.expr_as(&a.value, pty);
            let local = self.fresh(pty.clone());
            self.emit(Stmt::Set(local, v));
            extra.insert(pname.clone(), local);
        }
        let saved = self.enter(&closure.scope, extra);
        let v = self.expr_as(&closure.body, &closure.result);
        self.leave(saved);
        v
    }

    /// What an optional parameter holds, when `e` names one.
    pub(crate) fn static_option(&self, e: &ast::Expr) -> Option<Static> {
        let ExprKind::Ident(name) = &e.kind else {
            return None;
        };
        if self.lookup(name).is_some() && !matches!(self.st.macros.get(name), Some(Macro::State(_)))
        {
            return None;
        }
        match self.st.macros.get(name)? {
            Macro::Absent => Some(Static::Absent),
            Macro::Some(inner) => Some(Static::Given((**inner).clone())),
            _ => None,
        }
    }

    /// Bind a pattern's one binder to what a static option holds, in the current scope.
    pub(crate) fn bind_static(&mut self, binder: &str, m: Macro) {
        if binder == "_" {
            return;
        }
        if let Macro::State(state) = &m {
            self.bind_local(binder, state.local);
            self.st.states.insert(binder.to_owned(), state.clone());
        }
        self.st.macros.insert(binder.to_owned(), m);
    }

    /// The arm of a `match` over a static option to check: the index of the arm and its binder.
    pub(crate) fn static_arm<'p>(
        &mut self,
        held: &Static,
        patterns: &[&'p ast::Pattern],
        span: Span,
    ) -> Option<(usize, Option<&'p str>)> {
        let want = match held {
            Static::Given(_) => "some",
            Static::Absent => "none",
        };
        let found = patterns.iter().position(|p| p.variant.name == want);
        let Some(index) = found else {
            self.err("E035", span, format!("this match does not cover `.{want}`"));
            return None;
        };
        let binder = patterns[index].binds.first().map(|b| b.name.as_str());
        Some((index, binder))
    }

    /// `for c in cases(E)`: the body once per case, at compile time, each keyed by its case.
    /// Whether `iter` is such a list.
    pub(crate) fn unrolled_for(
        &mut self,
        var: &str,
        iter: &ast::Expr,
        body: &[UiStmt],
        parent: LocalId,
    ) -> bool {
        let ExprKind::Call { callee, args } = &iter.kind else {
            return false;
        };
        if !matches!(&callee.kind, ExprKind::Ident(n) if n == "cases" && self.lookup(n).is_none()) {
            return false;
        }
        let Some(key) = self.cases_of(args, iter.span) else {
            return true;
        };
        let ty = Ty::Enum(key.clone());
        let Some(info) = self.env.enum_info(&key).cloned() else {
            return true;
        };
        for v in &info.variants {
            let value = self.variant_named(&ty, &v.name, iter.span);
            self.push_sink();
            let local = self.declare(var, ty.clone());
            self.emit(Stmt::Set(local, value));
            let suffix = Ex::str(&format!("/{}", v.name));
            let suffix = match self.st.keys.last() {
                Some(k) => Ex::host(
                    Imp::StrConcat,
                    vec![Ex::local(*k, Ty::Text), suffix],
                    Ty::Text,
                ),
                None => suffix,
            };
            let key_local = self.fresh(Ty::Text);
            self.emit(Stmt::Set(key_local, suffix));
            self.st.keys.push(key_local);
            self.ui_stmts(body, parent);
            self.st.keys.pop();
            let stmts = self.pop_sink();
            for s in stmts {
                self.emit(s);
            }
        }
        true
    }

    /// The enum `cases(E)` names.
    pub(crate) fn cases_of(
        &mut self,
        args: &[ast::Arg],
        span: Span,
    ) -> Option<crate::types::TypeKey> {
        let [a] = args else {
            self.err("E022", span, "`cases` takes one enum type: `cases(Show)`");
            return None;
        };
        match self.type_named(&a.value) {
            Some(Ty::Enum(key)) => Some(key),
            Some(Ty::Error) => None,
            _ => {
                self.err(
                    "E022",
                    a.value.span,
                    "`cases` takes an enum type, such as `cases(Show)`",
                );
                None
            }
        }
    }

    /// The type a plain name or dotted path names in an expression: a type parameter, a type
    /// of the module or one imported.
    pub(crate) fn type_named(&self, e: &ast::Expr) -> Option<Ty> {
        let path = super::expr::dotted(e)?;
        if let [single] = path.as_slice() {
            if self.lookup(single).is_some() {
                return None;
            }
            if let Some(ty) = self.st.types.get(single) {
                return Some(ty.clone());
            }
            if self.in_std() {
                return None;
            }
            if let Some(info) = self.env.own_type(single) {
                return Some(Ty::Record(info.key.clone()));
            }
            if let Some(info) = self.env.own_enum(single) {
                return Some(Ty::Enum(info.key.clone()));
            }
            return None;
        }
        let (prefix, name) = path.split_at(path.len().saturating_sub(1));
        let import = self.env.find_import(&prefix.join("."))?;
        let key = crate::types::TypeKey {
            app: import.app.clone(),
            name: name.first()?.clone(),
        };
        if self.env.enum_info(&key).is_some() {
            return Some(Ty::Enum(key));
        }
        self.env.type_info(&key).map(|_| Ty::Record(key))
    }

    /// The value of the variant `name` of the enum `ty`, which carries no payload.
    pub(crate) fn variant_named(&mut self, ty: &Ty, name: &str, span: Span) -> Ex {
        let Some(shape) = super::enums::Shape::of(self, ty) else {
            return Ex::error();
        };
        let ident = ast::Ident {
            name: name.to_owned(),
            span,
        };
        self.construct_variant(&shape, ty, &ident, None, span)
    }

    /// What a value of the enum `key` shows: per variant, `pick` of it, chosen at run time.
    pub(crate) fn per_variant(
        &mut self,
        value: Ex,
        key: &crate::types::TypeKey,
        out: Ty,
        pick: impl Fn(&crate::resolve::VariantInfo) -> Ex,
    ) -> Ex {
        let Some(info) = self.env.enum_info(key).cloned() else {
            return Ex::error();
        };
        let ty = value.ty.clone();
        let tmp = self.fresh(ty.clone());
        let mut rev = info.variants.iter().rev();
        let mut acc = rev.next().map_or_else(Ex::error, &pick);
        for v in rev {
            let test = Ex::host(
                Imp::EnumIs,
                vec![Ex::local(tmp, ty.clone()), Ex::str(&v.id)],
                Ty::Bool,
            );
            acc = Ex::new(
                Ek::If(Box::new(test), Box::new(pick(v)), Box::new(acc)),
                out.clone(),
            );
        }
        Ex::new(Ek::Seq(vec![Stmt::Set(tmp, value)], Box::new(acc)), out)
    }
}

/// Whether a value of `found` is taken where `want` is written without a conversion: a
/// collection read where a list is.
fn coerces(found: &Ty, want: &Ty) -> bool {
    match (found, want) {
        (Ty::Collection(a) | Ty::Set(a), Ty::List(b)) => a.same(b),
        _ => false,
    }
}

/// The block of a template call taken apart: its named slots, and the rest. An item block
/// names slots too: `std.transcript(c, c.messages) { m -> message(m) composer: { ... } }` is the
/// row of each item and, beside it, the composer, which does not see the item.
struct Content {
    item: Option<(ast::Ident, ())>,
    body: Vec<UiStmt>,
}

fn split_block(c: &UiCall) -> (Vec<(&ast::Ident, &Vec<UiStmt>)>, Content) {
    let mut slots = Vec::new();
    let mut body = Vec::new();
    for s in c.block.iter().flatten() {
        match &s.kind {
            UiKind::Slot {
                name, body: inner, ..
            } => slots.push((name, inner)),
            _ => body.push(s.clone()),
        }
    }
    let content = Content {
        item: c.item.clone().map(|item| (item, ())),
        body,
    };
    (slots, content)
}

/// Whether `p` is the view parameter a template call's block fills when it names no slot:
/// the first `View` parameter, or the one called `content`.
fn is_content_param(view: &ast::ViewDecl, p: &ast::Param) -> bool {
    let views: Vec<&ast::Param> = view
        .params
        .iter()
        .filter(|q| matches!(kind_of(&q.ty), Kind::View))
        .collect();
    views
        .iter()
        .find(|q| q.name.name == "content")
        .or_else(|| views.first())
        .is_some_and(|q| q.name.name == p.name.name)
}
