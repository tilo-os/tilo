//! The native vocabulary: lists and their rows, view state as
//! native controls, facts, and buttons for declared actions. Each lowers to the same `ui_*`
//! host calls as every other node. A window's frame is no part of it: the standard library
//! (`std.list`, `std.document`, `std.page`) composes frames from the toolkit's components.

use super::enums::Shape;
use super::expr::dotted;
use super::{Checker, Ex, StateRef};
use crate::abi::Imp;
use crate::ast::{self, ExprKind, StmtKind, UiCall, UiKind, UiStmt};
use crate::diag::Span;
use crate::ir::{Ek, LocalId, Stmt};
use crate::prims::{self, Prim};
use crate::resolve::{CKind, Callable, EnumInfo};
use crate::types::Ty;

/// The named arguments a list row takes, for messages.
const ROW_ARGS_TEXT: &str =
    "secondary:, note:, meta:, mark:, icon:, avatar:, badge:, value:, checkbox: and check:";

fn host(imp: Imp, args: Vec<Ex>) -> Stmt {
    Stmt::Eval(Ex::host(imp, args, Ty::Unit))
}

/// Whether `a` is a call's `bind:` argument.
fn is_bind(a: &ast::Arg) -> bool {
    a.name.as_ref().is_some_and(|name| name.name == "bind")
}

fn concat(a: Ex, b: Ex) -> Ex {
    Ex::host(Imp::StrConcat, vec![a, b], Ty::Text)
}

/// What an application applies, once the steps that carry it are stripped:
/// `send(c, d).then(set(d, ""))` is `send`, and `act(x).made_from(r)` is `act` — the button
/// takes its look and the arguments it reads from it, while what it runs keeps `r` as what it
/// is made from.
fn applied_action(e: &ast::Expr) -> &ast::Expr {
    let mut applied = e;
    while let ExprKind::Call { callee, .. } = &applied.kind
        && let ExprKind::Field { base, name } = &callee.kind
        && matches!(name.name.as_str(), "then" | "made_from")
    {
        applied = base;
    }
    applied
}

impl Checker<'_> {
    /// Lowers the primitives whose lowering is their own; `None` for the rest.
    pub(super) fn native(&mut self, p: &Prim, c: &UiCall, args: &[ast::Arg]) -> Option<LocalId> {
        match p.name {
            "list" => Some(self.list_frame(c, args)),
            "row" if self.titled(args) => Some(self.list_row(c, args)),
            "button" => {
                let callee = self.declared_action(args)?;
                Some(self.action_button(&callee, c, args))
            }
            "choice" => Some(self.choice(c, args)),
            // `field(label, bind: t.assignee)`: the form's labelled row, its control the
            // toolkit's field for the bound value's type. A person is picked with `PersonField`.
            "field" if args.iter().any(is_bind) => Some(self.bound_field(p, c, args)),
            "facts" => Some(self.facts(c, args)),
            _ => None,
        }
    }

    /// Whether a call has a positional argument that is not a flag: `row(n.title)` is a list
    /// row, `row(fill)` a layout row.
    fn titled(&self, args: &[ast::Arg]) -> bool {
        args.iter()
            .any(|a| a.name.is_none() && !self.is_flag(None, &a.value))
    }

    /// Whether `rest` is one list row, after any `let`s it reads: what a view draws when it
    /// draws one item's row.
    pub(super) fn body_is_row(&self, rest: &[UiStmt]) -> bool {
        match rest.split_last() {
            Some((
                UiStmt {
                    kind: UiKind::Call(c),
                    ..
                },
                lets,
            )) => {
                lets.iter().all(|s| matches!(s.kind, UiKind::Let { .. }))
                    && match &c.call.kind {
                        ExprKind::Call { callee, args, .. } => match &callee.kind {
                            ExprKind::Ident(n) => {
                                n == "row" && self.titled(args) && c.block.is_none()
                            }
                            // The toolkit's row itself: what `row(...)` builds.
                            ExprKind::Field { base, name } => {
                                name.name == "ListRow" && self.names_toolkit(base)
                            }
                            _ => false,
                        },
                        _ => false,
                    }
            }
            _ => false,
        }
    }

    /// `preview_rows()`: how many rows a `list` inside a card shows
    /// ([`crate::prims::PREVIEW_ROWS`]), for a view composing a card that says how many of its
    /// items the card leaves out.
    pub(super) fn builtin_preview_rows(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        if !args.is_empty() {
            self.err(
                "E022",
                span,
                "`preview_rows` takes nothing: it is how many rows a card's list shows",
            );
        }
        Ex::new(Ek::Int(prims::PREVIEW_ROWS), Ty::Int)
    }

    /// A row view's body is one list row, after any `let`s it reads.
    pub(super) fn row_view_body(&mut self, view: &str, ty: &str, rest: &[UiStmt], span: Span) {
        if !self.body_is_row(rest) {
            self.err("E093", span, format!("`{view}` is the row view of {ty}, so its body is one list row, after any `let`s: `row(x.title, secondary: ...)` or `ui.ListRow(title: x.title, ...)`"));
        }
    }

    // ---- lists ----------------------------------------------------------------------------

    /// `list(items, sort: s, filter: f, empty: "...") { x -> ... }`: a `list` node with one
    /// `item` child per resource, each holding the item block's nodes or the type's row view.
    /// `list(items) { x -> ... }`: a list of rows in the content (a card's summary, the rows of
    /// a page): a `list` node with one `item` child per resource, each holding the item
    /// block's nodes or the type's row view. A window's own list is `std.list`.
    fn list_frame(&mut self, c: &UiCall, args: &[ast::Arg]) -> LocalId {
        let key = self.node_key(c);
        let list_key = self.fresh(Ty::Text);
        self.emit(Stmt::Set(list_key, key));
        let n = self.new_node("list", Ex::local(list_key, Ty::Text));
        let node = Ex::local(n, Ty::View);
        let mut items = None;
        for a in args {
            match a.name.as_ref().map(|i| i.name.as_str()) {
                None if self.is_flag(None, &a.value) => self.flag_arg(n, &a.value),
                None | Some("items") if items.is_none() => items = Some(&a.value),
                None => {
                    self.err(
                        "E022",
                        a.value.span,
                        "`list` takes one positional argument, its items",
                    );
                    let _ = self.expr(&a.value, None);
                }
                Some(name @ ("sort" | "filter" | "empty")) => {
                    self.err("E096", a.value.span, format!("`list` in a view's content takes no `{name}:`; a window that lists resources is `std.list(of, items, {name}: ...)`"));
                    let _ = self.expr(&a.value, None);
                }
                Some(name) if prims::NODE_ARGS.contains(&name) => {
                    self.apply_arg(n, "list", name, &a.value);
                }
                Some(name) => {
                    self.err(
                        "E063",
                        a.value.span,
                        format!(
                            "`list` has no argument `{name}`; arguments: items, {}",
                            prims::NODE_ARGS.join(", ")
                        ),
                    );
                    let _ = self.expr(&a.value, None);
                }
            }
        }
        let Some(items) = items else {
            self.err(
                "E022",
                c.call.span,
                "`list` needs its items: `list(nb.notes) { n -> row(n.title) }`",
            );
            return n;
        };
        let list = self.expr(items, None);
        let elem = list.ty.element().cloned().unwrap_or(Ty::Error);
        // A list draws resources: a record type's items, or a union's, each by the row view of
        // the type its item is (the categories, channels and direct conversations of one page).
        match &elem {
            Ty::Record(_) | Ty::Error => {}
            Ty::Union(keys) if keys.iter().all(|key| self.env.row_view(key).is_some()) => {}
            Ty::Union(_) => {
                let t = self.show(&list.ty);
                self.err("E097", items.span, format!("`list` draws each item of a union by the row view of its own type, and {t} has a type without one: declare `row view <view> for <type>` for each"));
                return n;
            }
            _ => {
                let t = self.show(&list.ty);
                self.err("E097", items.span, format!("`list` shows resources: its items are a List or Collection of a record type, and this is {t}. Show plain values with `for x in xs {{ ... }}` in a `stack`"));
                return n;
            }
        }
        // A list in a card is the card's summary: it shows its first rows, and the card opens
        // the resource for the rest.
        if self.st.ui.in_card {
            self.emit(host(
                Imp::UiPropI,
                vec![
                    node,
                    Ex::str("preview"),
                    Ex::new(Ek::Int(prims::PREVIEW_ROWS), Ty::Int),
                ],
            ));
        }
        let has_rows = match (&c.block, c.params.as_slice()) {
            (Some(_), [_]) => true,
            (Some(_), _) => {
                self.err(
                    "E097",
                    c.call.span,
                    "name the item of a list's block: `list(xs) { x -> row(x.title) }`",
                );
                false
            }
            (None, _) if matches!(&elem, Ty::Union(_)) => {
                self.err("E097", c.call.span, "a list of a union writes its rows, each by the row view of its item's own type: `list(xs) { x -> show(x, row: true) }`");
                false
            }
            (None, _) if matches!(&elem, Ty::Record(key) if self.env.row_view(key).is_some()) => {
                true
            }
            (None, _) => {
                let name = match &elem {
                    Ty::Record(key) => key.name.clone(),
                    _ => "the type".to_owned(),
                };
                self.err("E097", c.call.span, format!("a list of {name} needs its rows: write an item block, `list(xs) {{ x -> row(x.title) }}`, or declare the type's row, `row view <view> for {name}`"));
                false
            }
        };
        if has_rows {
            self.list_items(n, list_key, list, c);
        }
        n
    }

    /// One `item` node per resource, with the item block's nodes or the row view as its
    /// children: in a view, each the row of a loop node, which the host renders when it shows
    /// the row ([`Self::list_rows`]); elsewhere keyed `<list key>/<ref id>` in place.
    fn list_items(&mut self, n: LocalId, list_key: LocalId, list: Ex, c: &UiCall) {
        let span = c.call.span;
        let elem = list.ty.element().cloned().unwrap_or(Ty::Error);
        let row_view = match (&c.block, &elem) {
            (None, Ty::Record(key)) => self.env.row_view(key).cloned(),
            _ => None,
        };
        let Some(list) = self.list_rows(n, list, c, row_view.clone()) else {
            return;
        };
        // A card shows its list's first rows and says how many it has
        // ([`prims::PREVIEW_ROWS`]): where the rows are built in place, the list reads those
        // rows of what it lists and no more, and carries the count of the whole, as a list
        // whose rows render as the host shows them does.
        let list = if self.st.ui.in_card {
            let ty = list.ty.clone();
            let held = self.fresh(ty.clone());
            self.emit(Stmt::Set(held, list));
            let held = Ex::local(held, ty);
            self.emit(host(
                Imp::UiPropI,
                vec![
                    Ex::local(n, Ty::View),
                    Ex::str("len"),
                    Ex::host(Imp::ListLen, vec![held.clone()], Ty::Int),
                ],
            ));
            Ex::host(
                Imp::Take,
                vec![held, Ex::int(prims::PREVIEW_ROWS)],
                Ty::List(Box::new(elem.clone())),
            )
        } else {
            list
        };
        let mut fl = self.new_for(list, &elem);
        let item = Ex::local(fl.item_local, elem.clone());
        self.push_sink();
        let id = self.fresh(Ty::Text);
        self.emit(Stmt::Set(
            id,
            concat(
                Ex::str("/"),
                Ex::host(Imp::RefId, vec![item.clone()], Ty::Text),
            ),
        ));
        let suffix = match self.st.keys.last() {
            Some(k) => concat(Ex::local(*k, Ty::Text), Ex::local(id, Ty::Text)),
            None => Ex::local(id, Ty::Text),
        };
        let key_local = self.fresh(Ty::Text);
        self.emit(Stmt::Set(key_local, suffix));
        let item_key = self.fresh(Ty::Text);
        self.emit(Stmt::Set(
            item_key,
            concat(Ex::local(list_key, Ty::Text), Ex::local(id, Ty::Text)),
        ));
        let it = self.new_node("item", Ex::local(item_key, Ty::Text));
        let it_ex = Ex::local(it, Ty::View);
        self.emit(host(
            Imp::UiPropH,
            vec![it_ex.clone(), Ex::str("item"), item.clone()],
        ));
        self.st.keys.push(key_local);
        match (&c.block, c.params.as_slice(), row_view) {
            (Some(block), [var], _) => {
                self.bind_local(&var.name, fl.item_local);
                let saved = self.st.ui;
                self.st.ui.parent = "item";
                self.st.ui.list_row_ok = true;
                self.ui_stmts(block, it);
                self.st.ui = saved;
            }
            (None, _, Some(v)) => {
                let key = concat(Ex::local(item_key, Ty::Text), Ex::str("/row"));
                let row = self.view_node(&v, vec![item], key, span);
                self.emit(host(Imp::UiChild, vec![it_ex, Ex::local(row, Ty::View)]));
            }
            _ => {}
        }
        self.st.keys.pop();
        self.emit(host(
            Imp::UiChild,
            vec![Ex::local(n, Ty::View), Ex::local(it, Ty::View)],
        ));
        fl.body = self.pop_sink();
        self.emit(Stmt::ForList(Box::new(fl)));
    }

    /// The view state an option names, when it is an enum whose variants carry no payload:
    /// what a native control (a sort menu, a filter list, a choice) can offer.
    fn enum_state(&mut self, what: &str, value: &ast::Expr) -> Option<(StateRef, EnumInfo)> {
        let name = match &value.kind {
            ExprKind::Ident(n) => Some(n.as_str()),
            _ => None,
        };
        let Some((name, state)) = name.and_then(|n| Some((n, self.state_named(n)?))) else {
            let known = self.state_names();
            self.err("E097", value.span, format!("`{what}:` takes the name of a `state` declaration of this view whose type is an enum, such as `{what}: order`; this view declares: {known}"));
            return None;
        };
        let info = match &state.ty {
            Ty::Enum(key) => self.env.enum_info(key).cloned(),
            _ => None,
        };
        let Some(info) = info.filter(|i| i.variants.iter().all(|v| v.fields.is_empty())) else {
            let t = self.show(&state.ty);
            self.err("E097", value.span, format!("`{what}:` makes view state a native control, so it takes an enum whose variants carry no payload; `{name}` is {t}"));
            return None;
        };
        Some((state, info))
    }

    /// The enum, the selected variant's name and one event per variant that selects it:
    /// `<props>enum`, `<props>selected` and `<events><variant>`.
    fn state_control(
        &mut self,
        n: LocalId,
        (props, events): (&str, &str),
        state: &StateRef,
        info: &EnumInfo,
        span: Span,
    ) {
        let node = Ex::local(n, Ty::View);
        self.emit(host(
            Imp::UiPropH,
            vec![
                node.clone(),
                Ex::str(&format!("{props}enum")),
                Ex::str(&info.id),
            ],
        ));
        let selected = Self::variant_name(state, info);
        self.emit(host(
            Imp::UiPropH,
            vec![node.clone(), Ex::str(&format!("{props}selected")), selected],
        ));
        for v in &info.variants {
            let value = self.variant_value(state, &v.name, span);
            let act = Self::set_action(state, value);
            self.emit(host(
                Imp::UiOn,
                vec![node.clone(), Ex::str(&format!("{events}{}", v.name)), act],
            ));
        }
    }

    /// The name of the variant the view state holds.
    fn variant_name(state: &StateRef, info: &EnumInfo) -> Ex {
        let cur = Ex::local(state.local, state.ty.clone());
        let mut rev = info.variants.iter().rev();
        let mut acc = Ex::str(rev.next().map_or("", |v| v.name.as_str()));
        for v in rev {
            let test = Ex::host(Imp::EnumIs, vec![cur.clone(), Ex::str(&v.id)], Ty::Bool);
            acc = Ex::new(
                Ek::If(Box::new(test), Box::new(Ex::str(&v.name)), Box::new(acc)),
                Ty::Text,
            );
        }
        acc
    }

    fn variant_value(&mut self, state: &StateRef, name: &str, span: Span) -> Ex {
        let Some(shape) = Shape::of(self, &state.ty) else {
            return Ex::error();
        };
        let ident = ast::Ident {
            name: name.to_string(),
            span,
        };
        self.construct_variant(&shape, &state.ty, &ident, None, span)
    }

    /// `row(title, secondary: ..., ...)`: a `list_row`.
    fn list_row(&mut self, c: &UiCall, args: &[ast::Arg]) -> LocalId {
        let span = c.call.span;
        if c.block.is_some() {
            self.err("E097", span, "a row with a title is a list row and takes no block; for a layout row write `row { ... }` with the title inside it as `text(...)`");
        }
        if !self.st.ui.list_row_ok {
            self.err("E097", span, "a list row `row(title, ...)` stands directly in a list's item block, `list(xs) { x -> row(x.title) }`, is a row view's body, or is the whole body of a view of the item it draws; for a layout row write `row { ... }`");
        }
        let key = self.node_key(c);
        let row_key = self.fresh(Ty::Text);
        self.emit(Stmt::Set(row_key, key));
        let n = self.new_node(
            &crate::toolkit::kind("ListRow"),
            Ex::local(row_key, Ty::Text),
        );
        let node = Ex::local(n, Ty::View);
        let mut titled = false;
        let mut title = None;
        let mut avatar = None;
        let mut icon = None;
        for a in args {
            let value = &a.value;
            match a.name.as_ref().map(|i| i.name.as_str()) {
                None if self.is_flag(None, value) => self.flag_arg(n, value),
                None if !titled => {
                    titled = true;
                    title = self.row_subject(&node, value);
                }
                None => {
                    self.err("E022", value.span, format!("a list row takes one positional argument, its title; name the others: {ROW_ARGS_TEXT}"));
                    let _ = self.expr(value, None);
                }
                Some("meta") => self.meta_arg(&node, value),
                Some(name @ ("secondary" | "badge" | "note")) => {
                    self.text_arg(&node, name, value);
                }
                Some("value") => {
                    let v = self.expr(value, None);
                    if matches!(v.ty, Ty::Text | Ty::Int | Ty::Error) {
                        self.prop(node.clone(), "value", v, value.span);
                    } else {
                        let t = self.show(&v.ty);
                        self.err(
                            "E020",
                            value.span,
                            format!("a row's `value:` is Text or an Int; found {t}"),
                        );
                    }
                }
                Some("icon") => {
                    icon = Some(value);
                    self.icon_value(n, "icon", value);
                }
                Some("mark") => self.mark_arg(n, value),
                Some("avatar") => avatar = Some(value),
                Some("checkbox") => {
                    if self
                        .bind_typed(&node, value, &[Ty::Bool], false, false)
                        .is_some()
                    {
                        self.emit(host(
                            Imp::UiPropH,
                            vec![node.clone(), Ex::str("bind"), Ex::str("checkbox")],
                        ));
                    }
                }
                // What a press on the box does: the app's, so a check can carry more than the
                // one field the box shows.
                Some("check") => self.apply_arg(n, "row", "on_check", value),
                Some(name) if prims::NODE_ARGS.contains(&name) => {
                    self.apply_arg(n, "row", name, value);
                }
                Some(name) if prims::LAYOUT_ROW_ARGS.contains(&name) => {
                    self.err("E063", value.span, format!("`{name}:` belongs to a layout row, `row({name}: ...) {{ ... }}`; a list row takes {ROW_ARGS_TEXT}"));
                    let _ = self.expr(value, None);
                }
                Some(name) => {
                    self.err(
                        "E063",
                        value.span,
                        format!("a list row has no argument `{name}`; it takes {ROW_ARGS_TEXT}"),
                    );
                    let _ = self.expr(value, None);
                }
            }
        }
        if let Some(value) = avatar {
            self.avatar_arg(n, row_key, title, icon, value);
        }
        n
    }

    /// A row's `avatar:`: a Principal, or an Option of one, as the row's `avatar` slot holding a
    /// `ui.Avatar`. At `.none` (the last message's author of a conversation that has none yet) a
    /// row that names an `icon:` shows the item itself as its avatar, that glyph on a neutral
    /// square (a group); one that names none shows no avatar, and its facts run on without one.
    fn avatar_arg(
        &mut self,
        n: LocalId,
        row_key: LocalId,
        title: Option<LocalId>,
        icon: Option<&ast::Expr>,
        value: &ast::Expr,
    ) {
        let v = self.expr(value, Some(&Ty::Principal));
        let optional = match &v.ty {
            Ty::Option(inner) if matches!(**inner, Ty::Principal) => true,
            Ty::Error => return,
            _ => false,
        };
        let v = if optional {
            v
        } else {
            self.coerce(v, &Ty::Principal, value.span)
        };
        if v.ty.is_error() {
            return;
        }
        let ty = v.ty.clone();
        let tmp = self.fresh(ty.clone());
        self.emit(Stmt::Set(tmp, v));
        let row = Ex::local(n, Ty::View);
        let slot_key = concat(Ex::local(row_key, Ty::Text), Ex::str("#avatar"));
        let slot = self.new_node("slot", slot_key);
        let slot_ex = Ex::local(slot, Ty::View);
        self.emit(host(
            Imp::UiPropH,
            vec![slot_ex.clone(), Ex::str("name"), Ex::str("avatar")],
        ));
        let avatar_kind = crate::toolkit::kind("Avatar");
        // The participant's avatar.
        self.push_sink();
        let who_key = concat(Ex::local(row_key, Ty::Text), Ex::str("#avatar/who"));
        let who = self.new_node(&avatar_kind, who_key);
        let principal = if optional {
            Ex::host(
                Imp::UnwrapH,
                vec![Ex::local(tmp, ty.clone())],
                Ty::Principal,
            )
        } else {
            Ex::local(tmp, ty.clone())
        };
        self.emit(host(
            Imp::UiPropH,
            vec![Ex::local(who, Ty::View), Ex::str("who"), principal],
        ));
        self.emit(host(
            Imp::UiChild,
            vec![slot_ex.clone(), Ex::local(who, Ty::View)],
        ));
        self.emit(host(Imp::UiChild, vec![row.clone(), slot_ex.clone()]));
        let some = self.pop_sink();
        if !optional {
            for s in some {
                self.emit(s);
            }
            return;
        }
        // No participant this time: a row that names an icon shows the item itself; one that
        // names none shows no avatar, and its other facts run on without an empty slot.
        self.push_sink();
        if let Some(icon) = icon {
            let glyph_key = concat(Ex::local(row_key, Ty::Text), Ex::str("#avatar/glyph"));
            let glyph = self.new_node(&avatar_kind, glyph_key);
            if let Some(title) = title {
                self.emit(host(
                    Imp::UiPropH,
                    vec![
                        Ex::local(glyph, Ty::View),
                        Ex::str("name"),
                        Ex::local(title, Ty::Text),
                    ],
                ));
            }
            self.icon_value(glyph, "glyph", icon);
            self.emit(host(
                Imp::UiChild,
                vec![slot_ex.clone(), Ex::local(glyph, Ty::View)],
            ));
            self.emit(host(Imp::UiChild, vec![row, slot_ex]));
        }
        let none = self.pop_sink();
        let test = Ex::host(Imp::IsSome, vec![Ex::local(tmp, ty)], Ty::Bool);
        self.emit(Stmt::If(test, some, none));
    }

    /// A row's text: Text, a Date or a Time, or an Option of one (`.none` is the empty text).
    /// A Date or a Time is marked for the host to format (see `prop`).
    /// A list row's positional: its title, or the resource it shows (a `resource` prop the
    /// host renders as that resource's row).
    fn row_subject(&mut self, node: &Ex, value: &ast::Expr) -> Option<LocalId> {
        let v = self.expr(value, Some(&Ty::Text));
        if matches!(v.ty, Ty::Record(_)) {
            // The resource itself: its row shows its title and its app's icon, as a native
            // row of a resource does.
            let r = self.fresh(v.ty.clone());
            self.emit(Stmt::Set(r, v.clone()));
            let item = Ex::local(r, v.ty);
            self.prop(node.clone(), "item", item.clone(), value.span);
            let meta_ty = super::platform::host_ty("Meta");
            let m = self.fresh(meta_ty.clone());
            self.emit(Stmt::Set(
                m,
                Ex::host(Imp::MetaOf, vec![item], meta_ty.clone()),
            ));
            let meta = Ex::local(m, meta_ty);
            let title = Self::field_of_meta(meta.clone(), "title", Ty::Text);
            self.prop(node.clone(), "title", title, value.span);
            let icon_ty = Ty::Option(Box::new(Ty::Text));
            let icon = Self::field_of_meta(meta, "icon", icon_ty);
            self.set_when_some(node, "icon", icon, &Ty::Text, value.span);
            return None;
        }
        if v.ty != Ty::Text {
            self.text_prop(node, "title", v, value.span);
            return None;
        }
        let title = self.fresh(Ty::Text);
        self.emit(Stmt::Set(title, v));
        self.prop(
            node.clone(),
            "title",
            Ex::local(title, Ty::Text),
            value.span,
        );
        Some(title)
    }

    /// The field `name` of the `host.Meta` `meta`.
    fn field_of_meta(meta: Ex, name: &str, ty: Ty) -> Ex {
        let at = crate::host::value_field("Meta", name)
            .and_then(|at| i32::try_from(at).ok())
            .unwrap_or(0);
        Ex::host(
            Imp::TupleGetH,
            vec![meta, Ex::new(Ek::I32(at), Ty::Bool)],
            ty,
        )
    }

    /// A row's `meta:`: when it happened, or the day an item is due.
    fn meta_arg(&mut self, node: &Ex, value: &ast::Expr) {
        let v = self.expr(value, None);
        self.text_prop(node, "meta", v, value.span);
    }

    /// A row's `mark:`: an icon (`mark: .pin`), the name of one (`mark: a.icon`), or an enum
    /// value whose case names one (`mark: t.priority`), which also gives the row the tone the
    /// case reads in: its mark and its checkbox ring show the item's priority. A case that
    /// reads in no tone says the item has no importance to show, and draws no mark.
    fn mark_arg(&mut self, n: LocalId, e: &ast::Expr) {
        let node = Ex::local(n, Ty::View);
        if !matches!(e.kind, ExprKind::Ident(_) | ExprKind::Field { .. }) {
            self.icon_value(n, "mark", e);
            return;
        }
        let v = self.expr(e, None);
        match v.ty.clone() {
            Ty::Enum(key) => {
                if self.env.enum_info(&key).is_some() {
                    let opt = Ty::Option(Box::new(Ty::Text));
                    let mark = self.per_variant(v.clone(), &key, opt.clone(), |case| {
                        match (&case.icon, &case.tone) {
                            (Some(icon), Some(_)) => {
                                Ex::host(Imp::SomeH, vec![Ex::str(icon)], opt.clone())
                            }
                            _ => Ex::host(Imp::NoneV, Vec::new(), opt.clone()),
                        }
                    });
                    self.set_when_some(&node, "mark", mark, &Ty::Text, e.span);
                }
                self.enum_tone(&node, "tone", v, e.span);
            }
            Ty::Text => self.prop(node, "mark", v, e.span),
            Ty::Option(inner) if *inner == Ty::Text => {
                self.set_when_some(&node, "mark", v, &Ty::Text, e.span);
            }
            Ty::Error => {}
            _ => {
                let t = self.show(&v.ty);
                self.err(
                    "E097",
                    e.span,
                    format!("`mark:` takes an icon (`mark: .pin`), the name of one (`mark: a.icon`, `mark: c.icon()`) or an enum value whose cases name them and the tone they read in (`mark: t.priority`); found {t}"),
                );
            }
        }
    }

    /// The tone the case `value` holds, as the prop `prop` of `node`: a case reads in the tone
    /// its declaration gives it (`high (icon: .flag, tone: .warning)`), and in no tone when it
    /// names none. `false` when `value` is not an enum value, so the caller says what it takes.
    pub(super) fn enum_tone(&mut self, node: &Ex, prop: &str, value: Ex, span: Span) -> bool {
        let Ty::Enum(key) = value.ty.clone() else {
            return false;
        };
        if let Some(tone) = self.enum_method(value, &key, "tone") {
            self.prop(node.clone(), prop, tone, span);
        }
        true
    }

    fn text_arg(&mut self, node: &Ex, name: &str, value: &ast::Expr) {
        let v = self.expr(value, Some(&Ty::Text));
        self.text_prop(node, name, v, value.span);
    }

    fn text_prop(&mut self, node: &Ex, name: &str, v: Ex, span: Span) {
        let v = match &v.ty {
            Ty::Text | Ty::Date | Ty::Time | Ty::Identifier | Ty::Error => v,
            Ty::Option(inner)
                if matches!(**inner, Ty::Text | Ty::Date | Ty::Time | Ty::Identifier) =>
            {
                let held = (**inner).clone();
                self.set_when_some(node, name, v, &held, span);
                return;
            }
            other => {
                let t = self.show(other);
                let what = if name == "title" {
                    "Text, a Date or a Time, an Option of one, or a resource"
                } else {
                    "Text, a Date or a Time, or an Option of one"
                };
                self.err(
                    "E020",
                    span,
                    format!("a row's `{name}` is {what}; found {t}"),
                );
                return;
            }
        };
        self.prop(node.clone(), name, v, span);
    }

    /// The icon name `e` computes, when the platform names it: the `icon` of a host value (an
    /// action, a type's metadata) or of an enum case. Text an app writes is no icon; `std`'s
    /// own parameters carry the platform's.
    fn platform_icon(&mut self, e: &ast::Expr) -> Option<Ex> {
        match &e.kind {
            ExprKind::Field { base, name } if name.name == "icon" => {
                let b = self.expr(base, None);
                match b.ty.clone() {
                    Ty::Record(key) if crate::host::is_value(&key) => {
                        self.value_field(b, &key, name)
                    }
                    // A filter's icon is a name the platform knows, as a host value's is.
                    Ty::Filter(_) => {
                        let Ty::Record(key) = super::platform::host_ty("Filter") else {
                            return None;
                        };
                        self.value_field(b, &key, name)
                    }
                    // A resource's own `icon` field names the glyph it shows, as a
                    // host value's does: an app, an entry of the launcher, an item of a
                    // dock. Read as a field, its type tells whether it is a glyph or
                    // something else.
                    Ty::Record(_) => {
                        let icon = self.field_read(b, name);
                        (icon.ty == Ty::Text).then_some(icon)
                    }
                    Ty::Error => Some(Ex::error()),
                    _ => None,
                }
            }
            ExprKind::Call { callee, args, .. } if args.is_empty() => {
                let ExprKind::Field { base, name } = &callee.kind else {
                    return None;
                };
                if name.name != "icon" {
                    return None;
                }
                let b = self.expr(base, None);
                match b.ty.clone() {
                    Ty::Enum(key) => self.enum_method(b, &key, "icon"),
                    Ty::Error => Some(Ex::error()),
                    _ => None,
                }
            }
            _ if self.in_std() => Some(self.expr(e, None)),
            _ => None,
        }
    }

    /// `mark: .pin` or `mark: if cond { .pin }`: the prop holds the icon's kebab-case name, set
    /// only on the branches that name one.
    pub(super) fn icon_value(&mut self, n: LocalId, prop: &str, e: &ast::Expr) {
        match &e.kind {
            ExprKind::Variant { name, args: None } => {
                if let Some(icon) = self.icon_named(name) {
                    self.emit(host(
                        Imp::UiPropH,
                        vec![Ex::local(n, Ty::View), Ex::str(prop), Ex::str(icon)],
                    ));
                }
            }
            ExprKind::If { cond, then, els } => {
                let c = self.expr_as(cond, &Ty::Bool);
                self.push_sink();
                self.icon_block(n, prop, then);
                let t = self.pop_sink();
                self.push_sink();
                if let Some(els) = els {
                    self.icon_value(n, prop, els);
                }
                let f = self.pop_sink();
                self.emit(Stmt::If(c, t, f));
            }
            ExprKind::Block(b) => self.icon_block(n, prop, b),
            _ => {
                // An icon the platform names at run time: an action's, a type's or an enum
                // case's (`a.icon`, `meta(T).icon`, `c.icon()`); `std` passes such names on.
                let Some(v) = self.platform_icon(e) else {
                    self.err(
                        "E097",
                        e.span,
                        format!("`{prop}:` takes an icon such as `.pin`, `if cond {{ .pin }}`, or one the platform names (`a.icon`, `c.icon()`)"),
                    );
                    return;
                };
                let node = Ex::local(n, Ty::View);
                match &v.ty {
                    Ty::Text => self.prop(node, prop, v, e.span),
                    Ty::Option(inner) if **inner == Ty::Text => {
                        self.set_when_some(&node, prop, v, &Ty::Text, e.span);
                    }
                    Ty::Error => {}
                    _ => self.err(
                        "E097",
                        e.span,
                        format!("`{prop}:` takes an icon such as `.pin`, `if cond {{ .pin }}`, or the name of one"),
                    ),
                }
            }
        }
    }

    fn icon_block(&mut self, n: LocalId, prop: &str, b: &ast::Block) {
        match b.stmts.as_slice() {
            [
                ast::Stmt {
                    kind: StmtKind::Expr(e),
                    ..
                },
            ] => self.icon_value(n, prop, e),
            _ => self.err(
                "E097",
                b.span,
                format!("`{prop}:` takes one icon in each branch, such as `if cond {{ .pin }}`"),
            ),
        }
    }

    /// The kebab-case name of the icon `.name`, or an error.
    pub(super) fn icon_named(&mut self, name: &ast::Ident) -> Option<&'static str> {
        let found = prims::icon(&name.name);
        if found.is_none() {
            self.err("E091", name.span, prims::unknown_icon(&name.name));
        }
        found
    }

    // ---- controls -------------------------------------------------------------------------

    /// `field(label, bind: t.assignee)`: a form's field, its label on the row and its control
    /// the toolkit's component for the bound value's type — a person is the toolkit's
    /// `PersonField`, offered the workspace's members and the person the view is shown to.
    /// The app writes the binding once and gets the control the type takes.
    fn bound_field(&mut self, p: &Prim, c: &UiCall, args: &[ast::Arg]) -> LocalId {
        let holder = self.primitive(
            p,
            c,
            &args
                .iter()
                .filter(|a| !is_bind(a))
                .cloned()
                .collect::<Vec<_>>(),
        );
        let Some(d) = self
            .env
            .toolkit
            .and_then(|toolkit| toolkit.get("PersonField"))
        else {
            self.err(
                "E060",
                c.call.span,
                "the toolkit describes no `PersonField`, which a field bound to a person is",
            );
            return holder;
        };
        let key = concat(self.node_key(c), Ex::str("#person"));
        let n = self.new_node(&crate::toolkit::kind(&d.name), key);
        for a in args.iter().filter(|a| is_bind(a)) {
            self.binding_prop(n, d, &a.value);
        }
        // Who may be picked, and which of them is "Me": the workspace's, never a copy the app
        // keeps.
        let people = Ty::List(Box::new(Ty::Principal));
        self.prop(
            Ex::local(n, Ty::View),
            "people",
            Ex::host(Imp::WorkspaceMembers, Vec::new(), people),
            c.call.span,
        );
        self.prop(
            Ex::local(n, Ty::View),
            "me",
            Ex::host(Imp::Viewer, Vec::new(), Ty::Principal),
            c.call.span,
        );
        self.emit(host(
            Imp::UiChild,
            vec![Ex::local(holder, Ty::View), Ex::local(n, Ty::View)],
        ));
        holder
    }

    /// `choice(bind: s)`: the enum of the view state `s`, its selected variant and a
    /// `pick.<variant>` event per variant.
    fn choice(&mut self, c: &UiCall, args: &[ast::Arg]) -> LocalId {
        let key = self.node_key(c);
        let n = self.new_node("choice", key);
        let mut bound = false;
        for a in args {
            let value = &a.value;
            match a.name.as_ref().map(|i| i.name.as_str()) {
                None if self.is_flag(None, value) => self.flag_arg(n, value),
                Some("bind") => {
                    bound = true;
                    if matches!(value.kind, ExprKind::Field { .. }) {
                        self.err("E098", value.span, "choose a field's value with an action; `choice` binds view state, a `state` declaration of an enum type");
                    } else if let Some((state, info)) = self.enum_state("bind", value) {
                        self.state_control(n, ("", "pick."), &state, &info, value.span);
                    }
                }
                Some(name) if prims::NODE_ARGS.contains(&name) => {
                    self.apply_arg(n, "choice", name, value);
                }
                other => {
                    let what = other.map_or_else(
                        || "a positional argument".to_string(),
                        |n| format!("an argument `{n}`"),
                    );
                    self.err("E063", value.span, format!("`choice` has no {what}; it takes `bind:`, the view state it picks, and the sizing arguments"));
                    let _ = self.expr(value, None);
                }
            }
        }
        if !bound {
            self.err(
                "E022",
                c.call.span,
                "`choice` needs the view state it picks: `choice(bind: filter)`",
            );
        }
        if c.block.is_some() {
            self.err(
                "E063",
                c.call.span,
                "`choice` takes no block; its cases come from the enum",
            );
        }
        n
    }

    /// The declared action a button's first argument applies, when it applies one.
    fn declared_action(&self, args: &[ast::Arg]) -> Option<Callable> {
        let first = args
            .iter()
            .find(|a| a.name.is_none() && !self.is_flag(None, &a.value))?;
        let applied = applied_action(&first.value);
        let ExprKind::Call { callee, .. } = &applied.kind else {
            return None;
        };
        let found = match &callee.kind {
            ExprKind::Ident(name) if self.lookup(name).is_none() => self.own_callable(name),
            ExprKind::Field { base, name } if self.names_import(base) => {
                let path = dotted(base)?.join(".");
                let import = self.env.find_import(&path)?;
                self.env.foreign_callable(&import.app, &name.name)
            }
            _ => None,
        }?;
        matches!(found.kind, CKind::Action).then(|| found.clone())
    }

    /// What a button for the application `applied` (`pin(m)`, `move_to(b, card, other)`) shows
    /// of the action's presentation that follows its arguments: its `label:` written with
    /// them or as an `if`, its `icon:` as an `if`, and `on:`, each read with those arguments
    /// as the view draws, so the button changes with them (Pin turns into Unpin). What is not
    /// written so stays the declaration's, which the caller set before.
    fn applied_presentation(&mut self, callee: &Callable, applied: &ast::Expr, n: LocalId) {
        let p = &callee.present;
        let (label, icon, on) = (p.label_text.clone(), p.icon_when.clone(), p.on.clone());
        if label.is_none() && icon.is_none() && on.is_none() {
            return;
        }
        let call = applied_action(applied);
        let ExprKind::Call { args, .. } = &call.kind else {
            return;
        };
        // An application that leaves a parameter for its form to ask (`button(check())`) has
        // nothing to read it with: the button shows what the declaration says.
        let positional = args.iter().filter(|a| a.name.is_none()).count();
        let complete = callee.params.iter().enumerate().all(|(at, p)| {
            at < positional
                || p.default.is_some()
                || args
                    .iter()
                    .any(|a| a.name.as_ref().is_some_and(|name| name.name == p.name))
        });
        if !complete {
            return;
        }
        let sig = callee.signature(&self.env.app);
        let bound =
            self.bind_args_or_default(&callee.name, &callee.params, args, call.span, &sig, false);
        let node = Ex::local(n, Ty::View);
        self.push_sink();
        // Each parameter as the application gives it, or as its default computes it from those
        // before it (`pinned = not m.pinned`), as an invocation would.
        let mut given: Vec<Ex> = Vec::new();
        let mut unbound: Vec<&str> = Vec::new();
        for (p, v) in callee.params.iter().zip(bound) {
            let value = match (v, p.default.as_ref().and_then(|d| d.func)) {
                (Some(v), _) => Some(v),
                (None, Some(f)) if unbound.is_empty() => {
                    Some(Ex::new(crate::ir::Ek::Call(f, given.clone()), p.ty.clone()))
                }
                (None, _) => None,
            };
            match value {
                Some(v) if unbound.is_empty() => {
                    let id = self.declare(&p.name, p.ty.clone());
                    self.emit(Stmt::Set(id, v));
                    given.push(Ex::local(id, p.ty.clone()));
                }
                _ => unbound.push(p.name.as_str()),
            }
        }
        // What reads an argument the application leaves for a form to ask stays as declared.
        let readable =
            |e: &Option<ast::Expr>| e.clone().filter(|e| mentions(e, &unbound).is_none());
        let (label, icon, on) = (readable(&label), readable(&icon), readable(&on));
        if let Some(text) = &label {
            let v = self.expr_as(text, &Ty::Text);
            self.prop(node.clone(), "label", v, text.span);
        }
        if let Some(when) = &icon {
            self.icon_value(n, "icon", when);
        }
        if let Some(on) = &on {
            let v = self.expr_as(on, &Ty::Bool);
            self.prop(node, "toggled", v, on.span);
        }
        for stmt in self.pop_sink() {
            self.emit(stmt);
        }
    }

    /// `button(act(x))`: a button whose label, icon and role are the action's, and whose
    /// `press` applies it.
    fn action_button(&mut self, callee: &Callable, c: &UiCall, args: &[ast::Arg]) -> LocalId {
        let span = c.call.span;
        let key = self.node_key(c);
        let n = self.new_node(&crate::toolkit::kind("Button"), key);
        let node = Ex::local(n, Ty::View);
        let mut applied = false;
        for a in args {
            let value = &a.value;
            match a.name.as_ref().map(|i| i.name.as_str()) {
                None if self.is_flag(None, value) => self.flag_arg(n, value),
                None if !applied => {
                    applied = true;
                    let act = self.expr(value, None);
                    let p = &callee.present;
                    self.prop(node.clone(), "label", Ex::str(&p.label), span);
                    if let Some(icon) = &p.icon {
                        self.prop(node.clone(), "icon", Ex::str(icon), span);
                    }
                    // A button in content stands on its own: `secondary` unless the action says
                    // (an icon-only toolkit button is `plain`, as a header's are).
                    let role = p.role.clone();
                    let role = role.as_deref().unwrap_or("secondary");
                    self.prop(node.clone(), "role", Ex::str(role), span);
                    self.applied_presentation(callee, value, n);
                    if !act.ty.is_error() {
                        self.emit(host(Imp::UiOn, vec![node.clone(), Ex::str("press"), act]));
                    }
                }
                None => {
                    self.err("E022", value.span, format!("`button({}(...))` takes the action only; its label, icon and role come from the declaration of `{}`", callee.name, callee.name));
                    let _ = self.expr(value, None);
                }
                Some(name) => {
                    if let Some(msg) = super::ui::removed_arg("button", name) {
                        self.err("E096", value.span, msg);
                        continue;
                    }
                    if name == "on" {
                        self.err("E098", value.span, format!("this button already applies `{}`; remove `on:`. A button for `set(...)` or `open(...)` is written `button(\"Label\", on: ...)`", callee.name));
                    } else if name == "icon_only" {
                        // Only the action's icon shows, its label naming the button to
                        // assistive technology: a composer's Send.
                        let v = self.expr_as(value, &Ty::Bool);
                        self.prop(node.clone(), "icon_only", v, value.span);
                        continue;
                    } else if name == "enabled" || prims::NODE_ARGS.contains(&name) {
                        self.apply_arg(n, "button", name, value);
                        continue;
                    } else {
                        self.err("E063", value.span, format!("`button` has no argument `{name}`; a button for an action takes `enabled:`, `icon_only:` and the sizing arguments"));
                    }
                    let _ = self.expr(value, None);
                }
            }
        }
        if c.block.is_some() {
            self.err("E063", span, "`button` takes no block");
        }
        n
    }

    // ---- presentation -----------------------------------------------------------------------

    /// The hidden view of an action whose `label:` or `icon:` is an `if`, or that declares
    /// `on:`: a view of the subject (the action's first parameter) building one `present` node
    /// with those props. `params` are the action's parameters, the subject first; the view reads
    /// only the subject.
    pub(super) fn run_present(
        &mut self,
        view: &Callable,
        opts: &[ast::ActionOpt],
        params: &[crate::resolve::ParamInfo],
    ) -> crate::ir::Func {
        self.begin(super::Ctx::View, &view.params, Ty::View, &view.id);
        // The parameters whose defaults follow from the subject are what an invocation from
        // here would pass: `done` with `done: Bool = not item.done`. The options read them.
        let mut given: Vec<Ex> = view
            .params
            .first()
            .map(|subject| Ex::local(0, subject.ty.clone()))
            .into_iter()
            .collect();
        let mut others: Vec<&str> = Vec::new();
        for p in params.iter().skip(1) {
            match p.default.as_ref().and_then(|d| d.func) {
                Some(f) if others.is_empty() && !given.is_empty() => {
                    let value = Ex::new(crate::ir::Ek::Call(f, given.clone()), p.ty.clone());
                    let id = self.declare(&p.name, p.ty.clone());
                    self.emit(Stmt::Set(id, value));
                    given.push(Ex::local(id, p.ty.clone()));
                }
                _ => others.push(p.name.as_str()),
            }
        }
        let n = self.new_node("present", Ex::str(&view.id));
        let node = Ex::local(n, Ty::View);
        let others = others.as_slice();
        for opt in opts {
            let name = opt.name.name.as_str();
            let dynamic = name == "on"
                || matches!(opt.value.kind, ExprKind::If { .. })
                || (name == "label"
                    && matches!(&opt.value.kind, ExprKind::Str(parts) if parts.iter().any(|p| matches!(p, ast::StrPart::Interp(_)))));
            if !dynamic || !matches!(name, "label" | "icon" | "on") {
                continue;
            }
            // A label that reads other arguments is read where the action is applied with them.
            if name == "label"
                && matches!(opt.value.kind, ExprKind::Str(_))
                && mentions(&opt.value, others).is_some()
            {
                continue;
            }
            if let Some(other) = mentions(&opt.value, others) {
                let subject = params.first().map_or("", |p| p.name.as_str());
                self.err("E090", opt.value.span, format!("`{name}:` is shown wherever the action is offered on `{subject}`, before anyone gives `{other}`; it reads only the subject, `{subject}`"));
                continue;
            }
            match name {
                "label" => {
                    let v = self.expr_as(&opt.value, &Ty::Text);
                    self.prop(node.clone(), "label", v, opt.value.span);
                }
                "icon" => self.icon_value(n, "icon", &opt.value),
                _ => {
                    let v = self.expr_as(&opt.value, &Ty::Bool);
                    self.prop(node.clone(), "on", v, opt.value.span);
                }
            }
        }
        self.emit(Stmt::Return(Some(node)));
        let mut f = self.end(
            crate::ir::FuncKind::View,
            &view.name,
            &format!("v_{}", view.id),
        );
        f.nparams = view.params.len();
        f
    }

    /// The guard of the action `c` (or its `applies` conditions): the requirements its subject
    /// and the place it is offered in alone decide, as one function answering the first reason
    /// the action cannot run on the subject now, or the empty text while it can
    /// ([`crate::ir::FuncKind::Guard`], [`crate::ir::FuncKind::Applies`]). A requirement that
    /// reads what a caller gives (`to` in `move_to(t, to)`) cannot be decided before it is
    /// given: it is checked when the action runs with it, and a form shows its reason as the
    /// refusal.
    pub(super) fn run_guard(
        &mut self,
        c: &Callable,
        requires: &[ast::Require],
        params: &[crate::resolve::ParamInfo],
        kind: crate::ir::FuncKind,
        export: fn(&str) -> String,
    ) -> crate::ir::Func {
        // The subject, then the places a view supplies: everything an offer knows before it is
        // made.
        let decided = 1 + params.iter().skip(1).take_while(|p| p.place).count();
        let subject = params.get(..decided).unwrap_or_default();
        self.begin(super::Ctx::View, subject, Ty::Text, &c.id);
        for r in requires.iter().filter(|r| offer_decided(r, params)) {
            // A requirement may read `invoker`: here, the person looking, who would run it.
            self.st.invoker = super::Invoker::Viewer;
            let cond = self.expr_as(&r.cond, &Ty::Bool);
            let reason = self.expr_as(&r.reason, &Ty::Text);
            self.st.invoker = super::Invoker::None;
            self.emit(Stmt::If(cond, Vec::new(), vec![Stmt::Return(Some(reason))]));
        }
        self.emit(Stmt::Return(Some(Ex::str(""))));
        let mut f = self.end(kind, &c.name, &export(&c.id));
        f.nparams = subject.len();
        f
    }

    // ---- a resource's page ----------------------------------------------------------------

    /// The record a page part is about: `of` as the record it names, with its type.
    fn page_of(
        &mut self,
        prim: &str,
        args: &[ast::Arg],
        span: Span,
    ) -> Option<(Ex, crate::resolve::TypeInfo)> {
        let mut of = None;
        for a in args {
            if a.name.is_none() && self.is_flag(None, &a.value) {
                continue;
            }
            if a.name.is_none() && of.is_none() {
                of = Some(&a.value);
                continue;
            }
            let name = a.name.as_ref().map_or_else(String::new, |n| n.name.clone());
            self.err(
                "E063",
                a.value.span,
                if name.is_empty() {
                    format!("`{prim}` takes one resource, the one whose page it is part of")
                } else {
                    format!("`{prim}` has no argument `{name}`: it takes the resource alone")
                },
            );
            let _ = self.expr(&a.value, None);
        }
        let Some(of) = of else {
            self.err(
                "E022",
                span,
                format!("`{prim}` takes the resource whose page it is part of: `{prim}(t)`"),
            );
            return None;
        };
        let b = self.expr(of, None);
        let Ty::Record(key) = b.ty.clone() else {
            if !b.ty.is_error() {
                let t = self.show(&b.ty);
                self.err(
                    "E063",
                    of.span,
                    format!("`{prim}` takes a resource, whose page it is part of; this is {t}"),
                );
            }
            return None;
        };
        let info = self.env.type_info(&key)?.clone();
        Some((b, info))
    }

    /// Bind `node` to the field `field` of the record `b`, as `bind: b.field` does: the
    /// component's binding, and the field the host merges it into.
    fn bind_part(
        &mut self,
        node: &Ex,
        d: &crate::toolkit::Description,
        b: &Ex,
        field: &crate::resolve::FieldInfo,
        span: Span,
    ) {
        let Some(binding) = d.bindings.first() else {
            return;
        };
        if !super::component::binding_types(&binding.ty).contains(&field.ty) {
            let t = self.show(&field.ty);
            self.err(
                "E062",
                span,
                format!(
                    "`{}` is a field of type {t}, which no control binds",
                    field.name
                ),
            );
            return;
        }
        self.emit(host(
            Imp::UiBind,
            vec![node.clone(), b.clone(), Ex::str(&field.id)],
        ));
        self.emit(host(
            Imp::UiPropH,
            vec![node.clone(), Ex::str("bind"), Ex::str(&binding.name)],
        ));
    }

    /// `facts(of) { ... }`: the facts of the resource as one row of chips — the same height,
    /// the same chevron, no labels, wrapping — each derived from what the field is: the day a
    /// date field names, the person a principal field names, and the case an enum field holds,
    /// in the case's glyph and tone, opening the cases to pick another. The block adds the
    /// chips only the view can offer (the project's), in the same row.
    fn facts(&mut self, c: &UiCall, args: &[ast::Arg]) -> LocalId {
        let key = self.node_key(c);
        let row = self.new_node("row", key.clone());
        let node = Ex::local(row, Ty::View);
        self.prop_text(node.clone(), "gap", Ex::str("tight"));
        self.prop_text(node.clone(), "align", Ex::str("center"));
        self.emit(host(
            Imp::UiPropI,
            vec![node.clone(), Ex::str("wrap"), Ex::int(1)],
        ));
        if let Some((b, info)) = self.page_of("facts", args, c.call.span) {
            for field in &info.fields {
                let chip = match &field.ty {
                    Ty::Date => self.date_chip(&key, &b, field, c.call.span),
                    Ty::Option(inner) if **inner == Ty::Date => {
                        self.date_chip(&key, &b, field, c.call.span)
                    }
                    Ty::Principal => self.person_chip(&key, &b, field, c.call.span),
                    Ty::Option(inner) if **inner == Ty::Principal => {
                        self.person_chip(&key, &b, field, c.call.span)
                    }
                    Ty::Enum(enum_key) => self.enum_chip(&key, &b, field, enum_key),
                    _ => None,
                };
                if let Some(chip) = chip {
                    self.emit(host(Imp::UiChild, vec![node.clone(), chip]));
                }
            }
        }
        if let Some(block) = &c.block {
            self.child_block("facts", block, row);
        }
        row
    }

    /// The chip that shows the day `field` names.
    fn date_chip(
        &mut self,
        key: &Ex,
        b: &Ex,
        field: &crate::resolve::FieldInfo,
        span: Span,
    ) -> Option<Ex> {
        let env = self.env;
        let d = env.toolkit.and_then(|toolkit| toolkit.get("DateField"))?;
        // One chip per field: a type with two days puts two chips in the row, each keyed by
        // the field it shows.
        let n = self.new_node(
            &crate::toolkit::kind(&d.name),
            concat(key.clone(), Ex::str(&format!("#date-{}", field.name))),
        );
        let view = Ex::local(n, Ty::View);
        self.bind_part(&view, d, b, field, span);
        // A date that may be empty can be cleared.
        if matches!(field.ty, Ty::Option(_)) {
            self.emit(host(
                Imp::UiPropI,
                vec![view.clone(), Ex::str("optional"), Ex::int(1)],
            ));
        }
        Some(view)
    }

    /// The chip that shows the person `field` names: the toolkit's person field, which offers
    /// the workspace's people and the person looking, never a copy an app keeps.
    fn person_chip(
        &mut self,
        key: &Ex,
        b: &Ex,
        field: &crate::resolve::FieldInfo,
        span: Span,
    ) -> Option<Ex> {
        let env = self.env;
        let d = env.toolkit.and_then(|toolkit| toolkit.get("PersonField"))?;
        let n = self.new_node(
            &crate::toolkit::kind(&d.name),
            concat(key.clone(), Ex::str(&format!("#person-{}", field.name))),
        );
        let view = Ex::local(n, Ty::View);
        self.bind_part(&view, d, b, field, span);
        let people = Ty::List(Box::new(Ty::Principal));
        self.prop(
            view.clone(),
            "people",
            Ex::host(Imp::WorkspaceMembers, Vec::new(), people),
            span,
        );
        self.prop(
            view.clone(),
            "me",
            Ex::host(Imp::Viewer, Vec::new(), Ty::Principal),
            span,
        );
        Some(view)
    }

    /// The chip that shows the case `field` holds: the toolkit's enum field, showing the
    /// case's own glyph, label and tone, and opening the enum's cases to pick another — one
    /// per case, each opening with the case's glyph and reading in its tone. A case is picked
    /// by its name, which the field takes as that case, so an edit goes through the field
    /// itself, never through a text the case would have to be read out of.
    ///
    /// A field whose cases carry values has no chip: a case is then a value with fields, and
    /// one name does not say which.
    fn enum_chip(
        &mut self,
        key: &Ex,
        b: &Ex,
        field: &crate::resolve::FieldInfo,
        enum_key: &crate::types::TypeKey,
    ) -> Option<Ex> {
        let info = self.env.enum_info(enum_key)?.clone();
        if info.variants.iter().any(|case| !case.fields.is_empty()) {
            return None;
        }
        let env = self.env;
        let d = env.toolkit.and_then(|toolkit| toolkit.get("EnumField"))?;
        let binding = d.bindings.first()?;
        let n = self.new_node(
            &crate::toolkit::kind(&d.name),
            concat(key.clone(), Ex::str(&format!("#enum-{}", field.name))),
        );
        let view = Ex::local(n, Ty::View);
        // The chip names what it chooses from, for whoever reads it without seeing it.
        self.emit(host(
            Imp::UiPropH,
            vec![
                view.clone(),
                Ex::str("label"),
                Ex::str(&crate::resolve::variant_label(&field.name, &field.doc)),
            ],
        ));
        self.emit(host(
            Imp::UiBind,
            vec![view.clone(), b.clone(), Ex::str(&field.id)],
        ));
        self.emit(host(
            Imp::UiPropH,
            vec![view.clone(), Ex::str("bind"), Ex::str(&binding.name)],
        ));
        for case in &info.variants {
            let n = self.new_node(
                &crate::toolkit::kind("EnumOption"),
                concat(
                    key.clone(),
                    Ex::str(&format!("#enum-{}-{}", field.name, case.id)),
                ),
            );
            let option = Ex::local(n, Ty::View);
            let prop = |name: &str, value: &str| {
                host(
                    Imp::UiPropH,
                    vec![option.clone(), Ex::str(name), Ex::str(value)],
                )
            };
            self.emit(prop("name", &case.id));
            self.emit(prop("label", &case.label));
            if let Some(icon) = &case.icon {
                self.emit(prop("icon", icon));
            }
            if let Some(tone) = &case.tone {
                self.emit(prop("tone", tone));
            }
            self.emit(host(Imp::UiChild, vec![view.clone(), option]));
        }
        Some(view)
    }
}

/// Whether the requirement `r` reads nothing a caller gives beyond the subject: what the subject
/// alone decides, so a surface offering the action can say whether it can run before anyone gives
/// the rest. One that reads another argument (`to` in `move_to(t, to)`) is checked when the action
/// runs with it.
pub(crate) fn offer_decided(r: &ast::Require, params: &[crate::resolve::ParamInfo]) -> bool {
    let others: Vec<&str> = params
        .iter()
        .skip(1)
        .filter(|p| !p.place)
        .map(|p| p.name.as_str())
        .collect();
    mentions(&r.cond, &others).is_none() && mentions(&r.reason, &others).is_none()
}

/// The first of `names` an expression reads as a variable, as the caller wrote them: a lambda
/// body counts, read without the names the lambda binds itself, since what it reads of the
/// caller's it still reads when it runs.
pub(super) fn mentions<'a>(e: &ast::Expr, names: &[&'a str]) -> Option<&'a str> {
    let block = |b: &ast::Block| mentions_block(b, names);
    match &e.kind {
        ExprKind::Ident(n) => names.iter().find(|x| **x == n).copied(),
        ExprKind::Field { base, .. } => mentions(base, names),
        ExprKind::OptChain { base, args, .. } => mentions(base, names).or_else(|| {
            args.iter()
                .flatten()
                .find_map(|a| mentions(&a.value, names))
        }),
        ExprKind::IfLet { binds, then, els } => binds
            .iter()
            .find_map(|b| mentions(&b.value, names))
            .or_else(|| block(then))
            .or_else(|| els.as_deref().and_then(|x| mentions(x, names))),
        ExprKind::Unary { operand, .. } | ExprKind::Try(operand) | ExprKind::Suggest(operand) => {
            mentions(operand, names)
        }
        ExprKind::Binary { lhs, rhs, .. } => mentions(lhs, names).or_else(|| mentions(rhs, names)),
        ExprKind::Call { callee, args, .. } => {
            mentions(callee, names).or_else(|| args.iter().find_map(|a| mentions(&a.value, names)))
        }
        ExprKind::Variant { args, .. } => args
            .as_ref()
            .and_then(|args| args.iter().find_map(|a| mentions(&a.value, names))),
        ExprKind::Tuple(items) | ExprKind::List(items) => {
            items.iter().find_map(|x| mentions(x, names))
        }
        ExprKind::If { cond, then, els } => mentions(cond, names)
            .or_else(|| block(then))
            .or_else(|| els.as_deref().and_then(|x| mentions(x, names))),
        ExprKind::Block(b) => block(b),
        ExprKind::Match { scrutinee, arms } => mentions(scrutinee, names)
            .or_else(|| arms.iter().find_map(|a| mentions(&a.body, names))),
        ExprKind::Str(parts) => parts.iter().find_map(|p| match p {
            ast::StrPart::Interp(x) => mentions(x, names),
            ast::StrPart::Lit(_) => None,
        }),
        ExprKind::Range { lo, hi } => [lo, hi]
            .into_iter()
            .flatten()
            .find_map(|end| mentions(end, names)),
        ExprKind::Lambda { params, body } => {
            let own: Vec<&str> = names
                .iter()
                .copied()
                .filter(|n| !params.iter().any(|p| p.name.as_str() == *n))
                .collect();
            mentions(body, &own)
        }
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Length(..)
        | ExprKind::Duration(..)
        | ExprKind::Calendar(_)
        | ExprKind::Bool(_)
        | ExprKind::Ref(_)
        | ExprKind::Root(_) => None,
    }
}

/// [`mentions`] over the statements of a block, the names a `for` binds itself left out as a
/// lambda's parameters are.
fn mentions_block<'a>(b: &ast::Block, names: &[&'a str]) -> Option<&'a str> {
    b.stmts.iter().find_map(|s| match &s.kind {
        StmtKind::Expr(x) | StmtKind::Let { value: x, .. } => mentions(x, names),
        StmtKind::Return(x) => x.as_ref().and_then(|x| mentions(x, names)),
        StmtKind::Assign { value, .. } => mentions(value, names),
        StmtKind::For { var, iter, body } => {
            let own: Vec<&str> = names
                .iter()
                .copied()
                .filter(|n| var.name.as_str() != *n)
                .collect();
            mentions(iter, names).or_else(|| mentions_block(body, &own))
        }
    })
}
