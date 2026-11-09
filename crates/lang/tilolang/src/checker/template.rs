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

use super::{Checker, Ctx, Ex, StateRef, UiPlace};
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

/// A block given to a template, with the scope it was written in: the values it names, each
/// with its type — an item block's one item, or the several values of a function's block.
pub(crate) struct UiClosure {
    pub params: Vec<(String, Ty)>,
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

/// The `state` declaration a parameter's default names (`opened: State<Option<T>> = open_item`),
/// when its default is a name.
fn default_state_name(p: &ast::Param) -> Option<&str> {
    match &p.default.as_ref()?.kind {
        ExprKind::Ident(name) => Some(name.as_str()),
        _ => None,
    }
}

/// Whether a view declared with these type parameters and parameters is a template.
#[must_use]
pub fn is_template(view: &ast::ViewDecl) -> bool {
    view.template
        || !view.type_params.is_empty()
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
        self.st.ns.as_deref() == Some(crate::stdlib::APP)
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
                    // `Filter<T>`: a filter of the type `T` names, as outside a template.
                    "Filter" => match *inner {
                        Ty::Record(key) => Ty::Filter(key),
                        other if other.is_error() => Ty::Error,
                        other => {
                            let t = other.display(&self.env.app);
                            self.err(
                                "E013",
                                te.span,
                                format!(
                                    "`Filter<T>` is the filters of a type's items; `{t}` is no resource type"
                                ),
                            );
                            Ty::Error
                        }
                    },
                    // `Sort<T>`: an order by a field of the type `T` names.
                    "Sort" => match *inner {
                        Ty::Record(key) => Ty::Sort(key),
                        other if other.is_error() => Ty::Error,
                        other => {
                            let t = other.display(&self.env.app);
                            self.err(
                                "E013",
                                te.span,
                                format!(
                                    "`Sort<T>` is an order by a field of a type's items; `{t}` is no resource type"
                                ),
                            );
                            Ty::Error
                        }
                    },
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
                if let ("Sort", Ty::Sort(key)) = (name.name.as_str(), ty) {
                    return Self::unify(&args[0], &Ty::Record(key.clone()), params, types);
                }
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

    /// `entry: .some(new_task(t))`: the action a view's maybe-action is, which the entry fills.
    /// An entry fills its action's one `Text` parameter with the words nothing else takes, so
    /// an action without one (`add(t, done: Bool)`), or with several, is no entry's action:
    /// E068 where the call that makes the action is written.
    fn entry_action(&mut self, span: Span, e: &ast::Expr) {
        let held = match &e.kind {
            ExprKind::Variant { name, args } if name.name == "some" => match args.as_deref() {
                Some([one]) => &one.value,
                _ => return,
            },
            _ => e,
        };
        let ExprKind::Call { callee, .. } = &held.kind else {
            return;
        };
        let ExprKind::Ident(name) = &callee.kind else {
            return;
        };
        let Some(c) = self.own_callable(name).cloned() else {
            return;
        };
        if c.kind != CKind::Action {
            return;
        }
        let texts = c.params.iter().filter(|p| p.ty == Ty::Text).count();
        if texts == 1 {
            return;
        }
        let what = if texts == 0 {
            "no `Text` parameter".to_owned()
        } else {
            format!("{texts} `Text` parameters")
        };
        self.err("E068", span, format!("`{}` has {what}: an entry fills the action's one `Text` parameter with the words nothing else takes, so the entry needs an action with exactly one", c.name));
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
        let itself = (t.ns.clone(), view.name.name.clone());
        if self.st.expanding.contains(&itself) {
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
        // The states a template keeps itself: a `State<T>` parameter a call leaves out, or
        // leaves to the template by passing on an absent `Option<State<T>>`, is the state its
        // default names, one the template declares in its body (`opened: State<Option<T>> =
        // open_item`). Read once the template's own states are lowered: a caller that wants the
        // state passes its own. A parameter with no default still needs one.
        let mut own: Vec<(&ast::Param, &TypeExpr, &ast::Expr)> = Vec::new();
        // The state a parameter's default names, when the call gave the parameter one of its
        // own: the template keeps nothing of it then.
        let mut kept: Vec<&str> = Vec::new();
        // Values and view state first: they bind the type parameters lambdas are read with.
        for (i, p) in view.params.iter().enumerate() {
            if !matches!(kind_of(&p.ty), Kind::Value(_))
                && let Some(m) = passed(i)
            {
                if matches!(m, Macro::Absent)
                    && let Kind::State(te) = kind_of(&p.ty)
                    && let Some(default) = &p.default
                {
                    own.push((p, te, default));
                    continue;
                }
                if matches!(m, Macro::Some(_) | Macro::State(_))
                    && matches!(kind_of(&p.ty), Kind::State(_))
                    && let Some(name) = default_state_name(p)
                {
                    kept.push(name);
                }
                let held = match &m {
                    Macro::Some(inner) => inner.as_ref(),
                    other => other,
                };
                match held {
                    Macro::State(state) => {
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
                    // A function passed on by the name its caller gave it binds the type
                    // parameters its result names, as the function a caller writes does.
                    Macro::Lambda(closure) => {
                        if let Some(result) = function_result(&p.ty) {
                            Self::unify(result, &closure.result, &params, &mut types);
                        }
                    }
                    Macro::View(_) | Macro::Block(_) | Macro::Absent | Macro::Some(_) => {}
                }
                macros.insert(p.name.name.clone(), m);
                continue;
            }
            match kind_of(&p.ty) {
                Kind::Value(te) => {
                    let Some(a) = given[i] else { continue };
                    let want = self.type_in(te, &types);
                    // A text parameter is words the view shows, as a component's prop of words
                    // is: the literal given to it is this view's message (i18n.md §2), so
                    // `std.composer(draft, "Comment on this {noun}", send)`'s sentence is
                    // translated with the view that writes it.
                    if self.st.ctx == Ctx::View
                        && !want.is_error()
                        && super::component::shows_text(&want)
                    {
                        self.mark_shown(&a.value);
                    }
                    if matches!(&want, Ty::Option(inner) if **inner == Ty::Action) {
                        self.entry_action(a.value.span, &a.value);
                    }
                    let v = self.expr(&a.value, (!want.is_error()).then_some(&want));
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
                    let v = if want.is_error() || v.ty.same(&want) || coerces(&v.ty, &want) {
                        v
                    } else {
                        self.coerce(v, &want, a.value.span)
                    };
                    let local = self.fresh(v.ty.clone());
                    self.emit(Stmt::Set(local, v));
                    values.insert(p.name.name.clone(), local);
                }
                Kind::State(te) => match (given[i], &p.default) {
                    (Some(a), _) => {
                        if let Some(state) = self.state_arg(&a.value, view, p) {
                            Self::unify(te, &state.ty, &params, &mut types);
                            values.insert(p.name.name.clone(), state.local);
                            states.insert(p.name.name.clone(), state.clone());
                            macros.insert(p.name.name.clone(), Macro::State(state));
                            kept.extend(default_state_name(p));
                        }
                    }
                    (None, Some(default)) => own.push((p, te, default)),
                    (None, None) => self.err(
                        "E022",
                        span,
                        format!(
                            "`{}` needs `{}`: a `state` declaration of this view",
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
                        (&content, &caller, &mut types, &params),
                        (view, p, span, optional),
                    )
                }
                Kind::View => {
                    let body = slots
                        .iter()
                        .find(|(name, _)| name.name == p.name.name)
                        .map(|(_, body)| (*body).clone())
                        .or_else(|| {
                            (content.params.is_empty() && !content.body.is_empty())
                                .then(|| content.body.clone())
                                .filter(|_| is_content_param(view, p))
                        });
                    body.map(|body| {
                        Macro::Block(Rc::new(UiClosure {
                            params: Vec::new(),
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
                                params: Vec::new(),
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
        self.st.expanding.push(itself);
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
        let body = self.template_state(view, &view.body, &kept);
        // The state each parameter left to the template keeps, now that its own are declared.
        for (p, te, default) in own {
            let Some(state) = self.own_state(view, p, default) else {
                continue;
            };
            Self::unify(te, &state.ty, &params, &mut self.st.types);
            let types = self.st.types.clone();
            let want = self.type_in(te, &types);
            if !want.is_error() && !state.ty.same(&want) {
                let (w, f) = (self.show(&want), self.show(&state.ty));
                self.err(
                    "E020",
                    default.span,
                    format!(
                        "`{}` of `{}` takes {w}; the state it names is {f}",
                        p.name.name, view.name.name
                    ),
                );
            }
            self.bind_local(&p.name.name, state.local);
            self.st.states.insert(p.name.name.clone(), state.clone());
            self.st
                .macros
                .insert(p.name.name.clone(), Macro::State(state));
        }
        let node = self.template_body(&body, parent);
        self.st.expanding.pop();
        self.leave(saved);
        node
    }

    /// The state a template's `State<T>` parameter names as its default (`opened:
    /// State<Option<T>> = open_item`): the one the template declares, read in its own scope.
    fn own_state(
        &mut self,
        view: &ast::ViewDecl,
        p: &ast::Param,
        default: &ast::Expr,
    ) -> Option<StateRef> {
        let state = match &default.kind {
            ExprKind::Ident(name) => self.state_named(name),
            _ => None,
        };
        if state.is_none() {
            let known = self.state_names();
            self.err(
                "E066",
                default.span,
                format!(
                    "`{}` of `{}` takes a `state` declaration of the template it names, and the template declares: {known}",
                    p.name.name, view.name.name
                ),
            );
        }
        state
    }

    /// Lowers the `state` declarations at the top of a template's body: state of
    /// the view it expands in, kept under that view's id and the template's name
    /// (`<view id>:list.query`), as the view's own is under its name. One a parameter names as
    /// its default is the caller's instead, and left undeclared. Returns the rest of the body.
    fn template_state(
        &mut self,
        view: &ast::ViewDecl,
        body: &[UiStmt],
        kept: &[&str],
    ) -> Vec<UiStmt> {
        let mut rest = Vec::new();
        for s in body {
            let UiKind::State(d) = &s.kind else {
                rest.push(s.clone());
                continue;
            };
            if kept.contains(&d.name.name.as_str()) {
                continue;
            }
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
            let args = vec![Ex::str(&id), Self::handle_of(fallback)];
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
                    "`{}` of `{}` takes view state by its name, a `state` declaration of this view; this view declares: {known}",
                    p.name.name, view.name.name
                ),
            );
        }
        state
    }

    /// What a function parameter is given: a lambda, the call's item block, a view's name, or
    /// for a view of an item left out, the item type's row view. `.none` for a parameter a call
    /// may leave out is nothing, as leaving it out is.
    fn function_arg(
        &mut self,
        arg: Option<&ast::Arg>,
        (fparams, result, item_block): (&[TypeExpr], &TypeExpr, bool),
        (content, caller, types, names): (&Content, &Scope, &mut HashMap<String, Ty>, &[String]),
        (view, p, span, optional): (&ast::ViewDecl, &ast::Param, Span, bool),
    ) -> Option<Macro> {
        let param_tys: Vec<Ty> = fparams.iter().map(|t| self.type_in(t, types)).collect();
        if let Some(a) = arg {
            if optional && is_none(&a.value) {
                return None;
            }
            // A key path or a lambda, as the function of every value the parameter takes. A
            // function of a view is given its rows by the call's block, never by a lambda: the
            // block names the values it draws with, and the item is one of them.
            let bound: Option<(Vec<ast::Ident>, ast::Expr)> = if item_block {
                None
            } else if let Some((param, body)) = super::optional::lambda_of(&a.value) {
                Some((vec![param], body))
            } else if let ExprKind::Lambda { params, body } = &a.value.kind {
                Some((params.clone(), (**body).clone()))
            } else {
                None
            };
            if let Some((params, body)) = bound {
                if params.len() != param_tys.len() {
                    self.err(
                        "E050",
                        a.value.span,
                        format!(
                            "`{}` takes a function of {}; a lambda takes that many values",
                            p.name.name,
                            param_tys.len()
                        ),
                    );
                    return None;
                }
                let held: Vec<(String, Ty)> = params
                    .iter()
                    .map(|param| param.name.clone())
                    .zip(param_tys)
                    .collect();
                // What the function is worth binds the type parameters its result names:
                // `group: .due` makes a list's heading a day, and anyone reading the list
                // keeps it a day.
                let named = type_params_in(result, names);
                let unbound = |types: &HashMap<String, Ty>| {
                    named
                        .iter()
                        .any(|name| types.get(*name).is_none_or(Ty::is_error))
                };
                let result_ty = if unbound(types) {
                    let worth = self.body_type(&held, &body, caller);
                    Self::unify(result, &worth, names, types);
                    if !worth.is_error() && unbound(types) {
                        let (want, found) = (crate::print::print_type(result), self.show(&worth));
                        self.err(
                            "E020",
                            a.value.span,
                            format!(
                                "`{}` of `{}` is a function that makes {want}; this one makes {found}",
                                p.name.name, view.name.name
                            ),
                        );
                    }
                    self.type_in(result, types)
                } else {
                    self.type_in(result, types)
                };
                return Some(Macro::Lambda(Rc::new(Closure {
                    params: held,
                    result: result_ty,
                    body,
                    scope: caller.clone(),
                })));
            }
            return match &a.value.kind {
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
        // The block names the values its rows are drawn with: the item, and at the top of a
        // nested list the item it stands under.
        if !content.params.is_empty() {
            if content.params.len() != param_tys.len() {
                self.err(
                    "E022",
                    content.params[0].span,
                    format!(
                        "`{}` takes a block naming {} value(s); this block names {}",
                        p.name.name,
                        param_tys.len(),
                        content.params.len()
                    ),
                );
                return None;
            }
            let params = content
                .params
                .iter()
                .zip(param_tys.iter())
                .map(|(param, ty)| (param.name.clone(), ty.clone()))
                .collect();
            return Some(Macro::Block(Rc::new(UiClosure {
                params,
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

    /// What a function parameter's body is worth on its own: its values are bound, its body
    /// read, and what it lowers to dropped. The body is read here for its type alone and read
    /// again where the template calls the function, which is where an error in it is reported.
    fn body_type(&mut self, params: &[(String, Ty)], body: &ast::Expr, scope: &Scope) -> Ty {
        let mut extra = HashMap::new();
        for (name, ty) in params {
            extra.insert(name.clone(), self.fresh(ty.clone()));
        }
        let saved = self.enter(scope, extra);
        let known = self.diags.len();
        self.push_sink();
        let worth = self.expr(body, None);
        self.pop_sink();
        self.diags.truncate(known);
        self.leave(saved);
        worth.ty
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
                if block.params.len() == args.len() {
                    for ((name, ty), a) in block.params.iter().zip(args) {
                        let v = self.expr_as(&a.value, ty);
                        let local = self.fresh(ty.clone());
                        self.emit(Stmt::Set(local, v));
                        extra.insert(name.clone(), local);
                    }
                } else {
                    self.err(
                        "E022",
                        span,
                        format!("`{name}` takes {} value(s)", block.params.len()),
                    );
                    return parent;
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
        let ExprKind::Call { callee, args, .. } = &iter.kind else {
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

/// Whether `e` is the `.none` a call leaves an optional parameter out with.
fn is_none(e: &ast::Expr) -> bool {
    matches!(&e.kind, ExprKind::Variant { name, args: None } if name.name == "none")
}

/// The type parameters `names` that the written type `te` mentions, each once: what a
/// function's result binds where a call passes it.
fn type_params_in<'a>(te: &'a TypeExpr, names: &'a [String]) -> Vec<&'a String> {
    let mut found: Vec<&String> = Vec::new();
    collect_type_params(te, names, &mut found);
    found
}

fn collect_type_params<'a>(te: &'a TypeExpr, names: &'a [String], found: &mut Vec<&'a String>) {
    match &te.kind {
        TypeExprKind::Named(path) => {
            if path.len() == 1
                && let Some(name) = names.iter().find(|name| **name == path[0].name)
                && !found.contains(&name)
            {
                found.push(name);
            }
        }
        TypeExprKind::Generic(_, args) | TypeExprKind::Tuple(args) | TypeExprKind::Union(args) => {
            for arg in args {
                collect_type_params(arg, names, found);
            }
        }
        TypeExprKind::Fn(params, result) => {
            for param in params {
                collect_type_params(param, names, found);
            }
            collect_type_params(result, names, found);
        }
    }
}

/// The result a written function type names: `(T) -> Option<K>`, plain or optional.
fn function_result(te: &TypeExpr) -> Option<&TypeExpr> {
    if let TypeExprKind::Fn(_, result) = &te.kind {
        return Some(result);
    }
    let TypeExprKind::Generic(name, args) = &te.kind else {
        return None;
    };
    match (name.name.as_str(), args.as_slice()) {
        (
            "Option",
            [
                TypeExpr {
                    kind: TypeExprKind::Fn(_, result),
                    ..
                },
            ],
        ) => Some(result),
        _ => None,
    }
}

/// The block of a template call taken apart: its named slots, and the rest. An item block
/// names slots too: `std.transcript(c, c.messages) { m -> message(m) composer: { ... } }` is the
/// row of each item and, beside it, the composer, which does not see the item.
struct Content {
    params: Vec<ast::Ident>,
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
        params: c.params.clone(),
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
