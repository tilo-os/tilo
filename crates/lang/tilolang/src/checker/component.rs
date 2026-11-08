//! Calls of the toolkit's components, the package `ui`: `ui.ListRow(title: n.title) { ... }`.
//!
//! Each call is checked against the component's description, which the toolkit generates from
//! the widget's Rust definition: its props by name or position and type, its bindings, its
//! slots and what they accept, and its events. A mistake is reported with the component's own
//! doc text. The call lowers to one node of kind `ui.<Name>` whose props are named as the
//! description names them; a named slot is a child `slot` node, and the block's other content
//! fills the default slot as the node's own children.

use std::collections::{HashMap, HashSet};

use super::{Checker, Ctx, Ex};
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
    listed(
        d.props
            .iter()
            .map(|p| p.name.clone())
            .chain(d.bindings.iter().map(|b| b.name.clone()))
            .chain(d.state.iter().map(|s| s.name.clone())),
    )
}

fn listed(names: impl Iterator<Item = String>) -> String {
    let names: Vec<String> = names.collect();
    if names.is_empty() {
        "none".into()
    } else {
        names.join(", ")
    }
}

/// The components whose actions a view places itself: what an item shows of them (its menu and
/// its swipe) on an item's node, and the window's bar on its shell. The host puts what the view
/// names where it names it, in that order, and everything else behind its own "More"; the
/// palette, an agent and a program still offer every action however a view places them.
const PLACES: [&str; 4] = ["ListItem", "TreeViewNode", "BoardCard", "NavShell"];

/// The argument a view places actions with.
const ACTIONS: &str = "actions";

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

    /// A primitive that is a component of the toolkit under a shorter name (`text` is
    /// `ui.Text`): its call is that component's call, exactly.
    pub(super) fn component_prim(
        &mut self,
        component: &str,
        c: &UiCall,
        args: &[ast::Arg],
    ) -> LocalId {
        match self.env.toolkit.and_then(|toolkit| toolkit.get(component)) {
            Some(d) => self.component(d, c, args),
            None => {
                self.err(
                    "E060",
                    c.call.span,
                    format!("the toolkit describes no component `{component}`"),
                );
                for a in args {
                    let _ = self.expr(&a.value, None);
                }
                self.dead_node()
            }
        }
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
        // The actions a view places itself (`actions:`): read once the item they stand on is
        // lowered, as what that item is is what the names are checked against.
        let places = PLACES.iter().any(|kind| *kind == d.name);
        let mut placement: Option<&ast::Expr> = None;
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
            if places && name == ACTIONS {
                placement = Some(&a.value);
                continue;
            }
            self.component_arg(n, d, &name, &a.value);
        }
        if let Some(value) = placement {
            self.placement_prop(n, d, value, args);
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

    /// `actions:` on a node that places actions: the view names, in its order, the actions it
    /// puts on what it shows — an item's menu and swipe, or the window's bar. The host puts each
    /// where the view names it, in that order, with the group and the primacy the view gives it,
    /// and everything else behind its own "More". A name the app does not declare, or one that
    /// does not apply to what the node shows, is reported here, where the type is known.
    fn placement_prop(
        &mut self,
        n: LocalId,
        d: &Description,
        value: &ast::Expr,
        args: &[ast::Arg],
    ) {
        let item = self.placed_item(d, args);
        let items = match &value.kind {
            ExprKind::List(items) => items.as_slice(),
            _ => {
                self.err(
                    "E137",
                    value.span,
                    format!(
                        "{} places the actions it names, in order: `{ACTIONS}: [name, ...]`",
                        shown(d)
                    ),
                );
                return;
            }
        };
        let mut entries = Vec::new();
        for at in items {
            if let Some(entry) = self.placed_entry(at, item.as_ref()) {
                entries.push(entry);
            }
        }
        let list = self.placed_list(entries);
        let node = Ex::local(n, Ty::View);
        self.prop(node, ACTIONS, list, value.span);
    }

    /// What a node that places actions shows: the item of a row, a card or a tree node, or the
    /// resource the view is of for a window's bar. `None` when it is not a local this can read
    /// the type of, which places the actions by name alone.
    fn placed_item(&self, d: &Description, args: &[ast::Arg]) -> Option<Ty> {
        if d.name == "NavShell" {
            return self.st.subject.as_ref().map(|(_, ty)| ty.clone());
        }
        let value = args
            .iter()
            .find(|a| a.name.as_ref().is_some_and(|id| id.name == "item"))
            .or_else(|| {
                args.iter()
                    .find(|a| a.name.is_none() && !self.is_flag(None, &a.value))
            })?
            .value
            .clone();
        match &value.kind {
            ExprKind::Ident(name) => self.lookup(name).map(|(_, ty)| ty),
            _ => None,
        }
    }

    /// One action a view places: the name it writes, with what it says of the action
    /// (`group: "Due"`, `primary: true`), as the id the host matches its offers by.
    fn placed_entry(
        &mut self,
        at: &ast::Expr,
        item: Option<&Ty>,
    ) -> Option<(String, Option<String>, bool)> {
        let (name, given) = match &at.kind {
            ExprKind::Ident(name) => (name, &[][..]),
            ExprKind::Call { callee, args, .. } => match &callee.kind {
                ExprKind::Ident(name) if args.iter().all(|a| a.name.is_some()) => {
                    (name, args.as_slice())
                }
                _ => {
                    self.err(
                        "E137",
                        at.span,
                        "an action a view places is named: `check`, or `schedule (group: \"Due\", \
                         primary: true)`",
                    );
                    return None;
                }
            },
            _ => {
                self.err(
                    "E137",
                    at.span,
                    "an action a view places is named: `check`, or `schedule (group: \"Due\", \
                     primary: true)`",
                );
                return None;
            }
        };
        let named = self
            .own_callable(name)
            .filter(|c| matches!(c.kind, crate::resolve::CKind::Action))
            .map(|c| (c.id.clone(), c.params.clone()));
        let Some((id, params)) = named else {
            self.err(
                "E137",
                at.span,
                format!(
                    "`{}` is not one of the app's actions{}; what it places: {}",
                    name,
                    self.near_action(name, item),
                    self.placing(item)
                ),
            );
            return None;
        };
        if let Some(item) = item
            && let Ty::Record(key) = item
            && !params
                .iter()
                .take(2)
                .any(|p| matches!(&p.ty, Ty::Record(held) if held == key))
        {
            self.err(
                "E138",
                at.span,
                format!(
                    "`{}` does not apply to a {}; what it places: {}",
                    name,
                    self.show(item),
                    self.placing(Some(item))
                ),
            );
            return None;
        }
        let mut group: Option<String> = None;
        let mut primary = false;
        for a in given {
            let Some(named) = &a.name else { continue };
            match named.name.as_str() {
                "group" => match &a.value.kind {
                    ExprKind::Str(parts) => match parts.as_slice() {
                        [ast::StrPart::Lit(text)] => group = Some(text.clone()),
                        _ => {
                            self.err("E137", a.value.span, "`group:` is text: `group: \"Due\"`");
                        }
                    },
                    _ => {
                        self.err("E137", a.value.span, "`group:` names the run of commands this one stands in, as text: `group: \"Due\"`");
                    }
                },
                "primary" => match &a.value.kind {
                    ExprKind::Bool(flag) => primary = *flag,
                    _ => {
                        self.err(
                            "E137",
                            a.value.span,
                            "`primary:` says whether it is the primary one: `primary: true`",
                        );
                    }
                },
                other => {
                    let lone = format!(
                        "an action a view places takes `group:` and `primary:`; `{other}` is neither"
                    );
                    self.err("E137", a.value.span, lone);
                }
            }
        }
        Some((id, group, primary))
    }

    /// The names of the actions that apply to what `item` holds, for a message: the ones the
    /// view could have placed.
    fn placing(&self, item: Option<&Ty>) -> String {
        listed(self.applicable(item).map(|c| c.name.clone()))
    }

    /// The app's actions that apply to what `item` holds: the ones the view could have placed,
    /// in the order the app declares them.
    fn applicable<'a>(
        &'a self,
        item: Option<&'a Ty>,
    ) -> impl Iterator<Item = &'a crate::resolve::Callable> {
        self.env.callables.iter().filter(move |c| {
            c.app == self.env.app
                && matches!(c.kind, crate::resolve::CKind::Action)
                && !c.hidden()
                && item.is_none_or(|item| match item {
                    Ty::Record(key) => c
                        .params
                        .iter()
                        .take(2)
                        .any(|p| matches!(&p.ty, Ty::Record(held) if held == key)),
                    _ => true,
                })
        })
    }

    /// `; did you mean `check`?` for a name that is not an action.
    fn near_action(&self, name: &str, item: Option<&Ty>) -> String {
        crate::diag::suggestion(name, self.applicable(item).map(|c| c.name.as_str()))
    }

    /// The placement as the node holds it: each action's id, the group the view puts it in (none
    /// for the action's own), and whether the view makes it the primary one.
    fn placed_list(&mut self, entries: Vec<(String, Option<String>, bool)>) -> Ex {
        let opt = Ty::Option(Box::new(Ty::Text));
        let entry = Ty::Tuple(vec![Ty::Text, opt.clone(), Ty::Bool]);
        let list = Ty::List(Box::new(entry.clone()));
        let held = self.fresh(list.clone());
        let mut stmts = vec![Stmt::Set(
            held,
            Ex::host(Imp::ListNew, Vec::new(), list.clone()),
        )];
        for (id, group, primary) in entries {
            let one = self.fresh(entry.clone());
            stmts.push(Stmt::Set(
                one,
                Ex::host(
                    Imp::TupleNew,
                    vec![Ex::new(crate::ir::Ek::I32(3), Ty::Bool)],
                    entry.clone(),
                ),
            ));
            let group = match group {
                Some(group) => Ex::host(Imp::SomeH, vec![Ex::str(&group)], opt.clone()),
                None => Ex::host(Imp::NoneV, Vec::new(), opt.clone()),
            };
            for (field, value) in [
                Ex::str(&id),
                group,
                Ex::new(crate::ir::Ek::Bool(primary), Ty::Bool),
            ]
            .into_iter()
            .enumerate()
            {
                let (imp, c) =
                    super::store_op(&value.ty, Imp::TupleSetI, Imp::TupleSetF, Imp::TupleSetH);
                stmts.push(host(
                    imp,
                    vec![
                        Ex::local(one, entry.clone()),
                        Ex::new(
                            crate::ir::Ek::I32(i32::try_from(field).unwrap_or(0)),
                            Ty::Bool,
                        ),
                        super::conv(c, value, Ty::Int),
                    ],
                ));
            }
            stmts.push(host(
                Imp::ListPushH,
                vec![Ex::local(held, list.clone()), Ex::local(one, entry.clone())],
            ));
        }
        Ex::new(
            crate::ir::Ek::Seq(stmts, Box::new(Ex::local(held, list.clone()))),
            list,
        )
    }

    /// One argument of a component call: a prop, a value the control shows given to it, a piece
    /// of state, or a sizing argument of the node. The place a control edits is the `bind:` of
    /// the call, never an argument of its own name.
    fn component_arg(
        &mut self,
        n: LocalId,
        d: &'static Description,
        name: &str,
        value: &ast::Expr,
    ) {
        if let Some(msg) = super::ui::removed_arg(&d.name, name) {
            self.err("E096", value.span, msg);
            return;
        }
        // `bind:` names the component's bindable value whatever it is called: the binding is the
        // description's, its type and its merge. A component with several offers the one whose
        // type the bound place has.
        if name == "bind" && d.binding(name).is_none() {
            if d.bindings.is_empty() {
                self.err(
                    "E063",
                    value.span,
                    format!(
                        "{} binds nothing; `bind:` names a component's bindable value",
                        shown(d)
                    ),
                );
            } else {
                self.binding_prop(n, d, value);
            }
            return;
        }
        if let Some(p) = d.prop(name) {
            // The literals a prop of words shows are the view's messages (i18n.md §2).
            if !p.data && self.st.ctx == Ctx::View && shows_words(&p.ty) {
                self.mark_shown(value);
            }
            self.typed_prop(n, d, name, &p.ty, &p.doc, value, p.identity);
        } else if let Some(b) = d.binding(name) {
            // A value the view does not hold (a device's volume, read from its reading) is
            // given, not bound: the control shows it, and the person's change goes to the
            // view's `on change(v)`, which asks whoever holds the value to change it. A place
            // is never an argument: a control edits it through `bind:`, the one spelling.
            if d.event("change").is_some() && !self.is_place(value) {
                self.typed_prop(n, d, name, &b.ty, &b.doc, value, false);
            } else {
                self.err(
                    "E063",
                    value.span,
                    format!(
                        "{} has no argument `{name}`: a control edits the place it binds, so \
                         write `bind: {}`",
                        shown(d),
                        crate::print::expr_source(value)
                    ),
                );
                let _ = self.expr(value, None);
            }
        } else if let Some(s) = d.state(name) {
            self.typed_prop(n, d, name, &s.ty, &s.doc, value, false);
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

    /// `bind:` of the component, bound to what `value` names: checked against its bindings' own
    /// types, and the node carries the name of the one the bound place fits, which the host
    /// merges under.
    pub(super) fn binding_prop(&mut self, n: LocalId, d: &'static Description, value: &ast::Expr) {
        let bindings = &d.bindings;
        let node = Ex::local(n, Ty::View);
        let want: Vec<Ty> = bindings.iter().flat_map(|b| binding_types(&b.ty)).collect();
        let enum_case = bindings.iter().any(|b| b.ty == Type::Enum);
        // A control that declares `title` draws the resource's title when it edits it, as a
        // document's large title is drawn.
        let titled = d.prop("title").is_some_and(|title| title.ty == Type::Bool);
        let Some(ty) = self.bind_typed(&node, value, &want, enum_case, titled) else {
            return;
        };
        let Some(b) = bindings.iter().find(|b| binds(&b.ty, &ty)) else {
            return;
        };
        self.emit(host(
            Imp::UiPropH,
            vec![node, Ex::str("bind"), Ex::str(&b.name)],
        ));
    }

    /// Whether `value` names somewhere a control can be bound to: a field, or a piece of the
    /// view's state. Anything else is a value computed for the control to show.
    fn is_place(&self, value: &ast::Expr) -> bool {
        match &value.kind {
            ExprKind::Ident(name) => self.state_named(name).is_some(),
            ExprKind::Field { .. } => true,
            _ => false,
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
        identity: bool,
    ) {
        let node = Ex::local(n, Ty::View);
        // A filter names the item it is: the host compares the prop's text with its siblings'
        // keys, so the query the filter asks is what it is given.
        let keyed =
            matches!(ty, Type::Text) || matches!(ty, Type::Option(inner) if **inner == Type::Text);
        if identity
            && keyed
            && let Some(filter) = self.filter_shaped(value)
        {
            let text = Self::filter_query(filter);
            self.prop(node, name, text, value.span);
            return;
        }
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
            Type::Picture => self.picture_value(n, name, value),
            Type::Choice {
                cases,
                name: choice,
            } => {
                self.case_value(n, name, value, cases, choice);
            }
            Type::Option(inner)
                if matches!(**inner, Type::Icon | Type::Choice { .. } | Type::Picture) =>
            {
                self.typed_prop(n, d, name, inner, doc, value, identity);
            }
            Type::Option(inner) if **inner == Type::Action => {
                self.action_prop(n, name, value, &mismatch);
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
                // Cases are written `.arc(...)`: the elements are told their enum.
                let want = names_cases(ty).then(|| lang_type(ty));
                let v = self.expr(value, want.as_ref());
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
            Type::Action => self.action_prop(n, name, value, &mismatch),
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

    /// The value of an identity prop, when it is a filter: `selected: place`, `ui.NavItem(Task.today)`.
    /// A filter typed by name, or one a view state or a parameter holds. A value that is not one
    /// takes the prop's own path, unchanged.
    fn filter_shaped(&mut self, value: &ast::Expr) -> Option<Ex> {
        let named = |this: &Self, base: &ast::Expr, name: &ast::Ident| {
            let Some(Ty::Record(key)) = this.type_named(base) else {
                return false;
            };
            this.env
                .type_info(&key)
                .is_some_and(|info| info.search.filter(&name.name).is_some())
        };
        let filters = match &value.kind {
            ExprKind::Field { base, name } => named(self, base, name),
            ExprKind::Call { callee, .. } => match &callee.kind {
                ExprKind::Field { base, name } => named(self, base, name),
                _ => false,
            },
            ExprKind::Ident(name) => self
                .lookup(name)
                .and_then(|(local, _)| self.st.locals.get(local as usize))
                .is_some_and(|ty| matches!(ty, Ty::Filter(_))),
            _ => false,
        };
        filters.then(|| self.expr(value, None))
    }

    /// A prop that is one action offered to the person (`ui.NavSection(add: g.add)`): the node
    /// holds it as its control shows it, and the node's event `name` runs it.
    fn action_prop(
        &mut self,
        node: LocalId,
        name: &str,
        value: &ast::Expr,
        mismatch: &impl Fn(&mut Self, &Ty),
    ) {
        fn offered(ty: &Ty) -> bool {
            match ty {
                Ty::Action => true,
                Ty::Option(inner) => offered(inner),
                Ty::Record(key) => key.app == crate::host::HOST_APP && key.name == "Action",
                _ => false,
            }
        }
        let v = self.expr(value, None);
        if v.ty.is_error() {
            return;
        }
        if !offered(&v.ty) {
            mismatch(self, &v.ty);
            return;
        }
        let n = Ex::local(node, Ty::View);
        self.emit(host(Imp::UiAction, vec![n, Ex::str(name), v]));
    }

    /// `image: "tilo-blob://<hash>"` or `image: a.photo`: a picture by where its bytes are, as
    /// the node's prop of that name.
    fn picture_value(&mut self, n: LocalId, prop: &str, e: &ast::Expr) {
        let v = self.expr_as(e, &Ty::Text);
        if v.ty.is_error() {
            return;
        }
        self.emit(host(
            Imp::UiPropPic,
            vec![Ex::local(n, Ty::View), Ex::str(prop), v],
        ));
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
                // A tone the view states is the tone its case's declaration gives it
                // (`high (icon: .flag, tone: .warning)`): an enum value names the tone it
                // holds, as a row's mark does.
                let given = self.expr(e, None);
                if choice == "Tone"
                    && matches!(e.kind, ExprKind::Ident(_) | ExprKind::Field { .. })
                    && self.enum_tone(&Ex::local(n, Ty::View), prop, given, e.span)
                {
                    return;
                }
                self.err(
                    "E065",
                    e.span,
                    format!(
                        "`{prop}:` is one of {}; write it with a dot, such as `{prop}: .{}`",
                        all(),
                        cases.first().map_or("", String::as_str)
                    ),
                );
            }
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
        // A list inside a card is the card's summary of its resource: it shows the first rows.
        let outer = self.st.ui.in_card;
        self.st.ui.in_card |= d.name == "Card";
        self.child_block("slot", &content, n);
        self.st.ui.in_card = outer;
    }

    /// `on <event>: <action>` in the block of the component `d`, whose node is `n`: the action
    /// runs when the event fires. A primitive that is a component (`rich_input`) or builds one
    /// (`button`) takes its events the same way: `ui.*` written out and the shorter name share
    /// one contract.
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
                    // What the caller of a template gives the slot — its block or a view of
                    // its own — is the component where it is written.
                    let from_caller = match &c.call.kind {
                        ExprKind::Call { callee, .. } => match &callee.kind {
                            ExprKind::Ident(v) => self.macro_slot(d, slot, v, calls),
                            _ => false,
                        },
                        _ => false,
                    };
                    // A template expanded here is the component its body makes, as a view is.
                    let from_template = match &c.call.kind {
                        ExprKind::Call { callee, .. } => match &callee.kind {
                            ExprKind::Field { base, name } => self
                                .template_at(base, &name.name)
                                .and_then(|t| crate::resolve::view_root_component(&t.decl.body))
                                .is_some_and(|made| slot.accepts.takes(&made)),
                            _ => false,
                        },
                        _ => false,
                    };
                    let fits = own_view
                        || from_caller
                        || from_template
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

    /// Whether `name`, a parameter of the template being expanded, fills `slot`: its block is
    /// checked against the slot as if it stood there, and a view by what its body makes.
    fn macro_slot(
        &mut self,
        d: &Description,
        slot: &tilo_ui_graph::component::Slot,
        name: &str,
        calls: &mut usize,
    ) -> bool {
        match self.st.macros.get(name).cloned() {
            Some(super::template::Macro::Some(inner)) => match &*inner {
                super::template::Macro::View(v) => self
                    .env
                    .view_makes
                    .get(&v.name)
                    .is_some_and(|made| slot.accepts.takes(made)),
                super::template::Macro::Block(block) => self.slot_block(d, slot, block, calls),
                _ => false,
            },
            Some(super::template::Macro::View(v)) => self
                .env
                .view_makes
                .get(&v.name)
                .is_some_and(|made| slot.accepts.takes(made)),
            Some(super::template::Macro::Block(block)) => self.slot_block(d, slot, &block, calls),
            _ => false,
        }
    }

    /// Whether the caller's block fills `slot`: its body is written in the caller's scope, so
    /// it is read there, as [`Self::apply_ui`] reads it.
    fn slot_block(
        &mut self,
        d: &Description,
        slot: &tilo_ui_graph::component::Slot,
        block: &super::template::UiClosure,
        calls: &mut usize,
    ) -> bool {
        let saved = self.enter(&block.scope, HashMap::new());
        self.slot_calls(d, slot, &block.body, calls);
        self.leave(saved);
        true
    }
}

/// Whether the action `e` takes `name` only as one of its arguments, as it is: `open(x)`,
/// `check(item, x)`, `set(filter, x)`.
fn passed_as_is(e: &ast::Expr, name: &str) -> bool {
    let ExprKind::Call { callee, args, .. } = &e.kind else {
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

/// Whether a value of `ty` shows text: text, or an option or list of it.
pub(super) fn shows_text(ty: &Ty) -> bool {
    match ty {
        Ty::Text => true,
        Ty::Option(inner) | Ty::List(inner) => shows_text(inner),
        _ => false,
    }
}

/// Whether a prop of `ty` shows text: text, or an option or list of it.
pub(super) fn shows_words(ty: &Type) -> bool {
    match ty {
        Type::Text => true,
        Type::Option(inner) | Type::List(inner) => shows_words(inner),
        _ => false,
    }
}

/// Whether `ty` holds one of the toolkit's closed sets with fields.
fn names_cases(ty: &Type) -> bool {
    match ty {
        Type::Cases { .. } => true,
        Type::Option(inner) | Type::List(inner) | Type::Set(inner) => names_cases(inner),
        Type::Map { value, .. } => names_cases(value),
        _ => false,
    }
}

/// The language's types a binding of the description's `ty` takes: a moment is a Date, a Time,
/// or one that may be none.
pub(super) fn binding_types(ty: &Type) -> Vec<Ty> {
    match ty {
        Type::Text => vec![Ty::Text],
        Type::Bool => vec![Ty::Bool],
        Type::Int => vec![Ty::Int],
        Type::Float => vec![Ty::Float],
        Type::Time => vec![
            Ty::Date,
            Ty::Time,
            Ty::Option(Box::new(Ty::Date)),
            Ty::Option(Box::new(Ty::Time)),
        ],
        Type::RichText => vec![Ty::RichText],
        // A person, or nobody: the field holds the identity, and may be cleared.
        Type::Principal => vec![Ty::Principal, Ty::Option(Box::new(Ty::Principal))],
        _ => Vec::new(),
    }
}

/// Whether a binding described as `ty` takes a field of the language type `field`: a case
/// binding takes a field of whichever enum the app declares, whose cases are the field's.
fn binds(ty: &Type, field: &Ty) -> bool {
    match ty {
        Type::Enum => matches!(field, Ty::Enum(_)),
        other => binding_types(other).contains(field),
    }
}

/// The language's type of a description's scalar type, for checking literals against it.
fn lang_type(ty: &Type) -> Ty {
    match ty {
        // A picture is named by where its bytes are, which is text.
        Type::Text | Type::Picture => Ty::Text,
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
        // A `Cases` set is the `ui` package's enum of that name (components.md §4.1).
        Type::Cases { name } => Ty::Enum(toolkit::key(name)),
        // A case is whichever enum the bound field declares, which a description does not name.
        Type::Resource | Type::Icon | Type::Choice { .. } | Type::Enum => Ty::Error,
    }
}

/// Whether a value of the language's type `found` fits a description's `ty`. Text takes a Date
/// or a Time too, which the host shows the person's way.
fn fits(ty: &Type, found: &Ty) -> bool {
    match (ty, found) {
        // An option prop takes an optional value of its own too: the item a window holds open,
        // or the selection it keeps.
        (Type::Option(inner), Ty::Option(held)) => fits(inner, held),
        (Type::Option(inner), found) => fits(inner, found),
        // A list or a set takes either of the language's.
        (Type::List(inner) | Type::Set(inner), Ty::List(item) | Ty::Set(item)) => fits(inner, item),
        // The language's maps are keyed by text.
        (Type::Map { key, value }, Ty::Map(held)) => **key == Type::Text && fits(value, held),
        (Type::Action, Ty::Record(key)) => key.app == crate::host::HOST_APP && key.name == "Action",
        (Type::Cases { name }, Ty::Enum(key)) => *key == toolkit::key(name),
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
                | (Type::Picture, Ty::Text)
                | (Type::Resource, Ty::Record(_))
        ),
    }
}
