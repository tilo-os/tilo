//! Calls of the toolkit's components, the package `ui`: `ui.ListRow(title: n.title) { ... }`.
//!
//! Each call is checked against the component's description, which the toolkit generates from
//! the widget's Rust definition: its props by name or position and type, its bindings, its
//! slots and what they accept, and its events. A mistake is reported with the component's own
//! doc text. The call lowers to one node of kind `ui.<Name>` whose props are named as the
//! description names them; a named slot is a child `slot` node, and the block's other content
//! fills the default slot as the node's own children.

use std::collections::HashSet;

use super::{Checker, Ex};
use crate::abi::Imp;
use crate::ast::{self, ExprKind, UiCall, UiKind, UiStmt};
use crate::diag::Span;
use crate::ir::{LocalId, Stmt};
use crate::prims;
use crate::toolkit::{self, Description, Type};
use crate::types::Ty;

fn host(imp: Imp, args: Vec<Ex>) -> Stmt {
    Stmt::Eval(Ex::host(imp, args, Ty::Unit))
}

/// `ui.ListRow`.
fn shown(d: &Description) -> String {
    format!("`{}{}`", toolkit::KIND_PREFIX, d.name)
}

/// A part's doc text in a message: `: <first line>` or nothing.
fn doc_of(doc: &str) -> String {
    let line = tilo_ui_graph::component::first_line(doc);
    if line.is_empty() {
        String::new()
    } else {
        format!(" ({})", line.trim_end_matches('.'))
    }
}

/// The names of a description's arguments, for messages.
fn arguments(d: &Description) -> String {
    let mut names: Vec<String> = d.props.iter().map(|p| p.name.clone()).collect();
    names.extend(d.bindings.iter().map(|b| b.name.clone()));
    names.extend(d.state.iter().map(|s| s.name.clone()));
    if names.is_empty() {
        "none".into()
    } else {
        names.join(", ")
    }
}

fn listed(names: impl Iterator<Item = String>) -> String {
    let names: Vec<String> = names.collect();
    if names.is_empty() {
        "none".into()
    } else {
        names.join(", ")
    }
}

/// The toolkit's component a UI call names, when it names one: `ui.ListRow`.
pub(super) fn component_of(c: &UiCall) -> Option<(&ast::Expr, &str)> {
    let callee = match &c.call.kind {
        ExprKind::Call { callee, .. } => callee,
        _ => &c.call,
    };
    match &callee.kind {
        ExprKind::Field { base, name } => Some((base, name.name.as_str())),
        _ => None,
    }
}

impl Checker<'_> {
    /// The description a UI call of `ui.<name>` names, when `base` names the toolkit.
    pub(super) fn described(&self, base: &ast::Expr, name: &str) -> Option<&'static Description> {
        if !self.names_import(base) {
            return None;
        }
        let path = super::expr::dotted(base)?.join(".");
        self.env.component(&path, name)
    }

    /// Whether `base` names the toolkit's package.
    pub(super) fn names_toolkit(&self, base: &ast::Expr) -> bool {
        self.names_import(base)
            && super::expr::dotted(base)
                .and_then(|path| self.env.find_import(&path.join(".")))
                .is_some_and(|import| import.app == toolkit::UI_APP)
    }

    /// `ui.<name>` that the toolkit does not describe.
    pub(super) fn unknown_component(
        &mut self,
        name: &str,
        args: &[ast::Arg],
        span: Span,
    ) -> LocalId {
        let known = self
            .env
            .toolkit
            .map(|t| t.names().join(", "))
            .unwrap_or_default();
        self.err(
            "E060",
            span,
            format!("the toolkit has no component `{name}`; its components: {known}"),
        );
        for a in args {
            let _ = self.expr(&a.value, None);
        }
        self.dead_node()
    }

    /// A call of the component `d`: its node, with its props, bindings, slots and events.
    pub(super) fn component(
        &mut self,
        d: &'static Description,
        c: &UiCall,
        args: &[ast::Arg],
    ) -> LocalId {
        let span = c.call.span;
        let key = self.node_key(c);
        let n = self.new_node(&toolkit::kind(&d.name), key);
        let positional: Vec<&str> = d.positional().map(|p| p.name.as_str()).collect();
        let mut given: HashSet<String> = HashSet::new();
        let mut at = 0usize;
        for a in args {
            if a.name.is_none() && self.is_flag(None, &a.value) {
                self.flag_arg(n, &a.value);
                continue;
            }
            let name = match &a.name {
                Some(id) => id.name.clone(),
                None => {
                    let Some(name) = positional.get(at) else {
                        self.err(
                            "E022",
                            a.value.span,
                            format!(
                                "{} takes {} positional argument(s): {}; name the others (`name: value`)",
                                shown(d),
                                positional.len(),
                                listed(positional.iter().map(|p| format!("`{p}`")))
                            ),
                        );
                        let _ = self.expr(&a.value, None);
                        continue;
                    };
                    at = at.saturating_add(1);
                    (*name).to_string()
                }
            };
            if !given.insert(name.clone()) {
                self.err(
                    "E012",
                    a.value.span,
                    format!("{} is given `{name}` twice", shown(d)),
                );
                continue;
            }
            self.component_arg(n, d, &name, &a.value);
        }
        for p in d.props.iter().filter(|p| p.required) {
            if !given.contains(&p.name) {
                self.err(
                    "E022",
                    span,
                    format!(
                        "{} needs `{}`{}: {}",
                        shown(d),
                        p.name,
                        doc_of(&p.doc),
                        d.summary()
                    ),
                );
            }
        }
        // A rich input's `@` drawer offers the conversation's people however it is written.
        if d.name == "RichInput" && !given.contains("speakers") && !given.contains("people") {
            self.drawer_props(n);
        }
        if let Some(block) = &c.block {
            self.component_block(n, d, block, span);
        }
        n
    }

    /// One argument of a component call: a prop, a binding, a piece of state, or a sizing
    /// argument of the node.
    fn component_arg(
        &mut self,
        n: LocalId,
        d: &'static Description,
        name: &str,
        value: &ast::Expr,
    ) {
        let node = Ex::local(n, Ty::View);
        if let Some(p) = d.prop(name) {
            self.typed_prop(n, d, name, &p.ty, &p.doc, value);
        } else if let Some(b) = d.binding(name) {
            let want: Vec<Ty> = match &b.ty {
                Type::Text => vec![Ty::Text],
                Type::Bool => vec![Ty::Bool],
                Type::Int => vec![Ty::Int],
                Type::Float => vec![Ty::Float],
                Type::RichText => vec![Ty::RichText],
                _ => Vec::new(),
            };
            if self.bind_typed(&node, value, &want).is_some() {
                self.emit(host(
                    Imp::UiPropH,
                    vec![node, Ex::str("bind"), Ex::str(name)],
                ));
            }
        } else if let Some(s) = d.state(name) {
            self.typed_prop(n, d, name, &s.ty, &s.doc, value);
        } else if prims::NODE_ARGS.contains(&name) {
            self.apply_arg(n, "", name, value);
        } else {
            let event = d.event(name).map(|_| {
                format!("; `{name}` is an event, written in the block: `on {name}: action(...)`")
            });
            let slot = d.slot(name).map(|_| {
                format!("; `{name}` is a slot, written in the block: `{name}: {{ ... }}`")
            });
            self.err(
                "E063",
                value.span,
                format!(
                    "{} has no argument `{name}`; its arguments: {}{}",
                    shown(d),
                    arguments(d),
                    event.or(slot).unwrap_or_default()
                ),
            );
            let _ = self.expr(value, None);
        }
    }

    /// A value of type `ty` as the prop `name` of `n`.
    fn typed_prop(
        &mut self,
        n: LocalId,
        d: &Description,
        name: &str,
        ty: &Type,
        doc: &str,
        value: &ast::Expr,
    ) {
        let node = Ex::local(n, Ty::View);
        let mismatch = |this: &mut Self, found: &Ty| {
            let t = this.show(found);
            this.err(
                "E020",
                value.span,
                format!(
                    "{}'s `{name}` is {}{}; found {t}",
                    shown(d),
                    ty.show(),
                    doc_of(doc)
                ),
            );
        };
        match ty {
            Type::Icon => self.icon_value(n, name, value),
            Type::Choice {
                cases,
                name: choice,
            } => {
                self.case_value(n, name, value, cases, choice);
            }
            Type::Option(inner) if matches!(**inner, Type::Icon | Type::Choice { .. }) => {
                self.typed_prop(n, d, name, inner, doc, value);
            }
            Type::Option(inner) => {
                let held = lang_type(inner);
                let want = (!held.is_error()).then(|| Ty::Option(Box::new(held)));
                let v = self.expr(value, want.as_ref());
                match &v.ty {
                    Ty::Error => {}
                    Ty::Option(held) if fits(inner, held) => {
                        let held = (**held).clone();
                        self.set_when_some(&node, name, v, &held, value.span);
                    }
                    found if fits(inner, found) => self.prop(node, name, v, value.span),
                    found => {
                        let found = found.clone();
                        mismatch(self, &found);
                    }
                }
            }
            Type::Float => {
                let v = self.expr(value, None);
                match &v.ty {
                    Ty::Float | Ty::Error => self.prop(node, name, v, value.span),
                    Ty::Int => {
                        let v = Ex::new(
                            crate::ir::Ek::Conv(crate::ir::Conv::IntToFloat, Box::new(v)),
                            Ty::Float,
                        );
                        self.prop(node, name, v, value.span);
                    }
                    found => {
                        let found = found.clone();
                        mismatch(self, &found);
                    }
                }
            }
            Type::List(_) | Type::Set(_) | Type::Map { .. } => {
                let v = self.expr(value, None);
                if v.ty.is_error() {
                    return;
                }
                if matches!(v.ty, Ty::Collection(_)) {
                    // Not copied whole on every change, and not yet followed by the host
                    // item by item: a view takes it live instead.
                    let t = self.show(&v.ty);
                    self.err(
                        "E020",
                        value.span,
                        format!(
                            "{}'s `{name}` is {}; found {t}, a collection, which a component's prop does not take: hand it to a view's `Collection<T>` parameter, which gets it live, or loop over it in a slot",
                            shown(d),
                            ty.show()
                        ),
                    );
                } else if !fits(ty, &v.ty) {
                    mismatch(self, &v.ty);
                } else if offers_actions(ty) {
                    // Each action is shown by the component and run by an event of its node.
                    self.emit(host(Imp::UiActions, vec![node, Ex::str(name), v]));
                } else {
                    self.arg_prop(node, name, v);
                }
            }
            Type::Action => {
                let v = self.expr(value, None);
                if v.ty.is_error() {
                    return;
                }
                if fits(ty, &v.ty) {
                    let one = Ex::host(Imp::ListNew, Vec::new(), Ty::List(Box::new(v.ty.clone())));
                    let tmp = self.fresh(one.ty.clone());
                    self.emit(Stmt::Set(tmp, one));
                    let list = Ex::local(tmp, Ty::List(Box::new(v.ty.clone())));
                    self.emit(host(Imp::ListPushH, vec![list.clone(), v]));
                    self.emit(host(Imp::UiActions, vec![node, Ex::str(name), list]));
                } else {
                    mismatch(self, &v.ty);
                }
            }
            _ => {
                let v = self.expr(value, Some(&lang_type(ty)));
                if v.ty.is_error() {
                    return;
                }
                if fits(ty, &v.ty) {
                    self.prop(node, name, v, value.span);
                } else {
                    mismatch(self, &v.ty);
                }
            }
        }
    }

    /// Set the prop `name` to what the Option `v` holds, and leave it unset while it holds
    /// nothing.
    pub(super) fn set_when_some(&mut self, node: &Ex, name: &str, v: Ex, held: &Ty, span: Span) {
        let ty = v.ty.clone();
        let tmp = self.fresh(ty.clone());
        self.emit(Stmt::Set(tmp, v));
        let opt = Ex::local(tmp, ty);
        let test = Ex::host(Imp::IsSome, vec![opt.clone()], Ty::Bool);
        self.push_sink();
        let inner = Self::unwrap_opt(opt, held);
        self.prop(node.clone(), name, inner, span);
        let then = self.pop_sink();
        self.emit(Stmt::If(test, then, Vec::new()));
    }

    /// `role: .title` or `role: if c { .title } else { .body }`: one of `cases`, held by the
    /// prop as its name.
    pub(super) fn case_value(
        &mut self,
        n: LocalId,
        prop: &str,
        e: &ast::Expr,
        cases: &[String],
        choice: &str,
    ) {
        let all = || {
            cases
                .iter()
                .map(|c| format!(".{c}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        match &e.kind {
            ExprKind::Variant { name, args: None } => {
                if cases.contains(&name.name) {
                    self.emit(host(
                        Imp::UiPropH,
                        vec![Ex::local(n, Ty::View), Ex::str(prop), Ex::str(&name.name)],
                    ));
                } else {
                    self.err(
                        "E065",
                        name.span,
                        format!(
                            "{choice} has no case `.{}`; it is one of {}",
                            name.name,
                            all()
                        ),
                    );
                }
            }
            ExprKind::If { cond, then, els } => {
                let c = self.expr_as(cond, &Ty::Bool);
                self.push_sink();
                if let [
                    ast::Stmt {
                        kind: ast::StmtKind::Expr(x),
                        ..
                    },
                ] = then.stmts.as_slice()
                {
                    self.case_value(n, prop, x, cases, choice);
                }
                let t = self.pop_sink();
                self.push_sink();
                if let Some(els) = els {
                    self.case_value(n, prop, els, cases, choice);
                }
                let f = self.pop_sink();
                self.emit(Stmt::If(c, t, f));
            }
            ExprKind::Block(b) => {
                if let [
                    ast::Stmt {
                        kind: ast::StmtKind::Expr(x),
                        ..
                    },
                ] = b.stmts.as_slice()
                {
                    self.case_value(n, prop, x, cases, choice);
                }
            }
            _ => {
                self.err(
                    "E065",
                    e.span,
                    format!(
                        "`{prop}:` is one of {}; write it with a dot, such as `{prop}: .{}`",
                        all(),
                        cases.first().map_or("", String::as_str)
                    ),
                );
                let _ = self.expr(e, None);
            }
        }
    }

    /// `text(x, role: .caption)`: the language's text roles are the toolkit's `Text` roles,
    /// and `.numeric` is a title in tabular figures, so a changing number does not jitter.
    pub(super) fn text_role(&mut self, n: LocalId, e: &ast::Expr) {
        let roles = prims::choice("text_role").map_or(&[][..], |c| c.variants);
        let node = Ex::local(n, Ty::View);
        match &e.kind {
            ExprKind::Variant { name, args: None } if roles.contains(&name.name.as_str()) => {
                let role = if name.name == "numeric" {
                    self.emit(host(
                        Imp::UiPropI,
                        vec![node.clone(), Ex::str("numeric"), Ex::int(1)],
                    ));
                    "title"
                } else {
                    name.name.as_str()
                };
                self.emit(host(
                    Imp::UiPropH,
                    vec![node, Ex::str("role"), Ex::str(role)],
                ));
            }
            ExprKind::If { cond, then, els } => {
                let c = self.expr_as(cond, &Ty::Bool);
                self.push_sink();
                if let [
                    ast::Stmt {
                        kind: ast::StmtKind::Expr(x),
                        ..
                    },
                ] = then.stmts.as_slice()
                {
                    self.text_role(n, x);
                }
                let t = self.pop_sink();
                self.push_sink();
                if let Some(els) = els {
                    self.text_role(n, els);
                }
                let f = self.pop_sink();
                self.emit(Stmt::If(c, t, f));
            }
            ExprKind::Block(b) => {
                if let [
                    ast::Stmt {
                        kind: ast::StmtKind::Expr(x),
                        ..
                    },
                ] = b.stmts.as_slice()
                {
                    self.text_role(n, x);
                }
            }
            _ => self.apply_arg(n, "text", "role", e),
        }
    }

    /// The block of a component call: its named slots, its events, and the content of its
    /// default slot.
    fn component_block(
        &mut self,
        n: LocalId,
        d: &'static Description,
        block: &[UiStmt],
        span: Span,
    ) {
        let mut content: Vec<UiStmt> = Vec::new();
        for s in block {
            match &s.kind {
                UiKind::Slot { node, name, body } => self.slot_content(n, d, *node, name, body),
                UiKind::On {
                    event,
                    param,
                    at,
                    action,
                } => self.component_event(n, d, event, (param.as_ref(), at.as_ref()), action),
                _ => content.push(s.clone()),
            }
        }
        if content.is_empty() {
            return;
        }
        let Some(slot) = d.default_slot() else {
            self.err(
                "E063",
                span,
                format!(
                    "{} holds no content of its own; its slots: {}",
                    shown(d),
                    listed(d.slots.iter().map(|s| format!("`{}: {{ }}`", s.name)))
                ),
            );
            return;
        };
        self.slot_accepts(d, slot, &content);
        self.child_block("slot", &content, n);
    }

    /// `on <event>: <action>` in the block of the component `d`, whose node is `n`: the action
    /// runs when the event fires. A primitive the toolkit's component draws (`rich_input`,
    /// `button`) takes its component's events the same way, so both spellings share one
    /// contract.
    pub(super) fn component_event(
        &mut self,
        n: LocalId,
        d: &'static Description,
        event: &ast::Ident,
        params: (Option<&ast::EventParam>, Option<&ast::EventParam>),
        action: &ast::Expr,
    ) {
        let Some(e) = d.event(&event.name) else {
            self.err(
                "E063",
                event.span,
                format!(
                    "{} has no event `{}`; its events: {}",
                    shown(d),
                    event.name,
                    listed(
                        d.events
                            .iter()
                            .map(|e| format!("`{}`{}", e.name, doc_of(&e.doc)))
                    )
                ),
            );
            let _ = self.expr(action, None);
            return;
        };
        let act = self.event_action(d, e, params, action);
        if !act.ty.is_error() {
            self.emit(host(
                Imp::UiOn,
                vec![Ex::local(n, Ty::View), Ex::str(&event.name), act],
            ));
        }
    }

    /// What an event does: an action reference. An event that carries a value names it
    /// (`on select(x: Note): open(x)`), and the action takes it as one of its arguments as it
    /// is: the reference holds its place, and the host puts the value there when the event
    /// happens. An event that also says where the value lands names that second (`on
    /// drop(moved: Task, at: Int): place(moved, at)`). It is data still, never a closure.
    fn event_action(
        &mut self,
        d: &Description,
        e: &tilo_ui_graph::component::Event,
        (param, at): (Option<&ast::EventParam>, Option<&ast::EventParam>),
        action: &ast::Expr,
    ) -> Ex {
        let Some(param) = param else {
            return self.expr_as(action, &Ty::Action);
        };
        let Some(carried) = &e.value else {
            self.err(
                "E063",
                param.name.span,
                format!(
                    "{}'s `{}` carries no value; write `on {}: action(...)`",
                    shown(d),
                    e.name,
                    e.name
                ),
            );
            return Ex::error();
        };
        if let Some(at) = at
            && e.at.is_none()
        {
            self.err(
                "E063",
                at.name.span,
                format!(
                    "{}'s `{}` carries one value and says nothing of where it lands; write `on {}({}): ...`",
                    shown(d),
                    e.name,
                    e.name,
                    param.name.name
                ),
            );
            return Ex::error();
        }
        // An event that says where its value lands carries `[value, at]`: each name takes
        // its part. One that does not carries the value alone.
        let mut named = vec![(param, carried)];
        if let (Some(at), Some(where_)) = (at, &e.at) {
            named.push((at, where_));
        }
        let parts = e.at.is_some();
        self.push_sink();
        for (i, (p, carried)) in named.into_iter().enumerate() {
            let Some(ty) = self.event_param_ty(d, e, p, carried) else {
                self.pop_sink();
                return Ex::error();
            };
            if !passed_as_is(action, &p.name.name) {
                self.err(
                    "E063",
                    action.span,
                    format!(
                        "the value `{}` carries is passed to the action as it is, as one of its arguments: `{}(...)` with `{}` among them",
                        e.name, "action", p.name.name
                    ),
                );
                self.pop_sink();
                return Ex::error();
            }
            // A scalar is boxed until the event puts it in place, as view state read at the
            // event is (`Self::arg_at_press`).
            let scalar = ty.is_scalar();
            let ty = if scalar { Ty::Option(Box::new(ty)) } else { ty };
            let local = self.declare(&p.name.name, ty.clone());
            if scalar {
                self.st.event_scalars.insert(local);
            }
            let value = if parts {
                let part = i32::try_from(i).unwrap_or(0);
                Ex::host(
                    Imp::EventPart,
                    vec![Ex::new(crate::ir::Ek::I32(part), Ty::Bool)],
                    ty,
                )
            } else {
                Ex::host(Imp::EventValue, Vec::new(), ty)
            };
            self.emit(Stmt::Set(local, value));
        }
        let act = self.expr_as(action, &Ty::Action);
        let stmts = self.pop_sink();
        Ex::new(crate::ir::Ek::Seq(stmts, Box::new(act)), Ty::Action)
    }

    /// The type of the value `p` names, of the type `carried` the event gives: as written,
    /// or the event's own; `None` once an error is reported.
    fn event_param_ty(
        &mut self,
        d: &Description,
        e: &tilo_ui_graph::component::Event,
        p: &ast::EventParam,
        carried: &Type,
    ) -> Option<Ty> {
        let ty = match &p.ty {
            Some(te) => {
                let types = self.st.types.clone();
                self.type_in(te, &types)
            }
            None => lang_type(carried),
        };
        if ty.is_error() && p.ty.is_none() {
            self.err(
                "E063",
                p.name.span,
                format!(
                    "{}'s `{}` carries {}; name its type: `on {}({}: T): ...`",
                    shown(d),
                    e.name,
                    carried.show(),
                    e.name,
                    p.name.name
                ),
            );
            return None;
        }
        if !fits(carried, &ty) && !ty.is_error() {
            let t = self.show(&ty);
            self.err(
                "E020",
                p.name.span,
                format!(
                    "{}'s `{}` carries {}, not {t}",
                    shown(d),
                    e.name,
                    carried.show()
                ),
            );
            return None;
        }
        Some(ty)
    }

    /// `name: { ... }`: a `slot` node of `n`, named, holding the content.
    fn slot_content(
        &mut self,
        n: LocalId,
        d: &'static Description,
        node: ast::NodeId,
        name: &ast::Ident,
        body: &[UiStmt],
    ) {
        let Some(slot) = d.slot(&name.name) else {
            self.err(
                "E063",
                name.span,
                format!(
                    "{} has no slot `{}`; its slots: {}",
                    shown(d),
                    name.name,
                    listed(
                        d.slots
                            .iter()
                            .map(|s| format!("`{}`{}", s.name, doc_of(&s.doc)))
                    )
                ),
            );
            return;
        };
        self.slot_accepts(d, slot, body);
        let key = self.stmt_key(node);
        let s = self.new_node("slot", key);
        let s_ex = Ex::local(s, Ty::View);
        self.emit(host(
            Imp::UiPropH,
            vec![s_ex.clone(), Ex::str("name"), Ex::str(&slot.name)],
        ));
        self.child_block_of("slot", body, s, true);
        self.emit(host(Imp::UiChild, vec![Ex::local(n, Ty::View), s_ex]));
    }

    /// Check that `body` fits the slot: only the component it accepts, and one at most where
    /// it holds one.
    fn slot_accepts(
        &mut self,
        d: &Description,
        slot: &tilo_ui_graph::component::Slot,
        body: &[UiStmt],
    ) {
        let mut calls = 0usize;
        self.slot_calls(d, slot, body, &mut calls);
        if !slot.many
            && calls > 1
            && let Some(second) = body.get(1)
        {
            self.err(
                "E063",
                second.span,
                format!(
                    "{}'s `{}` holds one view{}",
                    shown(d),
                    slot.name,
                    doc_of(&slot.doc)
                ),
            );
        }
    }

    fn slot_calls(
        &mut self,
        d: &Description,
        slot: &tilo_ui_graph::component::Slot,
        body: &[UiStmt],
        calls: &mut usize,
    ) {
        for s in body {
            match &s.kind {
                UiKind::Call(c) => {
                    *calls = calls.saturating_add(1);
                    if !slot.accepts.reads_components() {
                        continue;
                    }
                    // A view of this package whose body is the component is one too.
                    let own_view = match &c.call.kind {
                        ExprKind::Call { callee, .. } => match &callee.kind {
                            ExprKind::Ident(v) => self
                                .env
                                .view_makes
                                .get(v)
                                .is_some_and(|made| slot.accepts.takes(made)),
                            _ => false,
                        },
                        _ => false,
                    };
                    let fits = own_view
                        || component_of(c).is_some_and(|(base, name)| {
                            slot.accepts.takes(name) && self.names_toolkit(base)
                        });
                    if !fits {
                        self.err(
                            "E063",
                            s.span,
                            format!(
                                "{}'s `{}` holds {} only{}",
                                shown(d),
                                slot.name,
                                slot.accepts.show(),
                                doc_of(&slot.doc)
                            ),
                        );
                    }
                }
                UiKind::For { body, .. } => self.slot_calls(d, slot, body, calls),
                UiKind::If { then, els, .. } | UiKind::IfLet { then, els, .. } => {
                    self.slot_calls(d, slot, then, calls);
                    if let Some(els) = els {
                        self.slot_calls(d, slot, els, calls);
                    }
                }
                UiKind::Match { arms, .. } => {
                    for arm in arms {
                        self.slot_calls(d, slot, &arm.body, calls);
                    }
                }
                _ => {}
            }
        }
    }
}

/// Whether the action `e` takes `name` only as one of its arguments, as it is: `open(x)`,
/// `check(item, x)`, `set(filter, x)`.
fn passed_as_is(e: &ast::Expr, name: &str) -> bool {
    let ExprKind::Call { callee, args } = &e.kind else {
        return false;
    };
    let names = [name];
    // `open(x).then(read(x))`: each step passes the value as it is, or does not use it, and
    // one of them passes it; the host puts it in every step's place.
    if let ExprKind::Field { base, name: then } = &callee.kind
        && then.name == "then"
        && let [next] = args.as_slice()
    {
        let step = |e: &ast::Expr| -> Option<bool> {
            if passed_as_is(e, name) {
                Some(true)
            } else if super::native::mentions(e, &names).is_none() {
                Some(false)
            } else {
                None
            }
        };
        return match (step(base), step(&next.value)) {
            (Some(a), Some(b)) => a || b,
            _ => false,
        };
    }
    if super::native::mentions(callee, &names).is_some() {
        return false;
    }
    let mut passed = false;
    for a in args {
        match &a.value.kind {
            ExprKind::Ident(n) if n == name => passed = true,
            _ if super::native::mentions(&a.value, &names).is_some() => return false,
            _ => {}
        }
    }
    passed
}

/// Whether values of `ty` hold actions offered to the person (`List<Action>`).
fn offers_actions(ty: &Type) -> bool {
    match ty {
        Type::Action => true,
        Type::Option(inner) | Type::List(inner) | Type::Set(inner) => offers_actions(inner),
        Type::Map { value, .. } => offers_actions(value),
        _ => false,
    }
}

/// The language's type of a description's scalar type, for checking literals against it.
fn lang_type(ty: &Type) -> Ty {
    match ty {
        Type::Text => Ty::Text,
        Type::Int => Ty::Int,
        Type::Float => Ty::Float,
        Type::Bool => Ty::Bool,
        Type::Principal => Ty::Principal,
        Type::Time => Ty::Time,
        Type::RichText => Ty::RichText,
        Type::Option(inner) => Ty::Option(Box::new(lang_type(inner))),
        Type::List(inner) | Type::Set(inner) => Ty::List(Box::new(lang_type(inner))),
        Type::Map { value, .. } => Ty::Map(Box::new(lang_type(value))),
        Type::Action => super::platform::host_ty("Action"),
        Type::Resource | Type::Icon | Type::Choice { .. } => Ty::Error,
    }
}

/// Whether a value of the language's type `found` fits a description's `ty`. Text takes a Date
/// or a Time too, which the host shows the person's way.
fn fits(ty: &Type, found: &Ty) -> bool {
    match (ty, found) {
        (Type::Option(inner), found) => fits(inner, found),
        // A list or a set takes either of the language's.
        (Type::List(inner) | Type::Set(inner), Ty::List(item) | Ty::Set(item)) => fits(inner, item),
        // The language's maps are keyed by text.
        (Type::Map { key, value }, Ty::Map(held)) => **key == Type::Text && fits(value, held),
        (Type::Action, Ty::Record(key)) => key.app == crate::host::HOST_APP && key.name == "Action",
        _ => matches!(
            (ty, found),
            (_, Ty::Error)
                | (Type::Text, Ty::Text | Ty::Date | Ty::Time)
                | (Type::Int, Ty::Int)
                | (Type::Bool, Ty::Bool)
                | (Type::Principal, Ty::Principal)
                | (Type::Time, Ty::Time)
                | (Type::RichText, Ty::RichText)
                | (Type::Float, Ty::Float | Ty::Int)
                | (Type::Resource, Ty::Record(_))
        ),
    }
}
