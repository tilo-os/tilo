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
    "secondary:, meta:, mark:, icon:, avatar:, badge:, value: and checkbox:";

fn host(imp: Imp, args: Vec<Ex>) -> Stmt {
    Stmt::Eval(Ex::host(imp, args, Ty::Unit))
}

fn concat(a: Ex, b: Ex) -> Ex {
    Ex::host(Imp::StrConcat, vec![a, b], Ty::Text)
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
            "date"
                if !args
                    .iter()
                    .any(|a| a.name.as_ref().is_some_and(|n| n.name == "bind")) =>
            {
                self.err(
                    "E022",
                    c.call.span,
                    "`date` needs the Date it picks: `date(bind: task.due)`, a Date or Option<Date> field or view state",
                );
                for a in args {
                    let _ = self.expr(&a.value, None);
                }
                Some(self.dead_node())
            }
            _ => None,
        }
    }

    /// Whether a call has a positional argument that is not a flag: `row(n.title)` is a list
    /// row, `row(fill)` a layout row.
    fn titled(&self, args: &[ast::Arg]) -> bool {
        args.iter()
            .any(|a| a.name.is_none() && !self.is_flag(None, &a.value))
    }

    /// The arguments of primitives that take a value of one type; `true` when handled.
    pub(super) fn typed_arg(
        &mut self,
        prim: &str,
        n: LocalId,
        name: &str,
        value: &ast::Expr,
    ) -> bool {
        let node = Ex::local(n, Ty::View);
        let want = match (prim, name) {
            ("toggle", "bind") => {
                let _ = self.bind_typed(&node, value, &[Ty::Bool]);
                return true;
            }
            ("date", "bind") => {
                let day = [Ty::Date, Ty::Option(Box::new(Ty::Date))];
                // Only a date that may be none can be cleared.
                if let Some(Ty::Option(_)) = self.bind_typed(&node, value, &day) {
                    let yes = Ex::new(Ek::Bool(true), Ty::Bool);
                    self.prop(node, "optional", yes, value.span);
                }
                return true;
            }
            ("card", "item") => {
                let v = self.expr(value, None);
                if !matches!(v.ty, Ty::Record(_) | Ty::Error) {
                    let t = self.show(&v.ty);
                    self.err(
                        "E020",
                        value.span,
                        format!("`{prim}` shows a resource, a record; found {t}"),
                    );
                    return true;
                }
                // The card is named after its resource, as a native dashboard card is: the
                // resource's title heads it, beside the count.
                let r = self.fresh(v.ty.clone());
                self.emit(Stmt::Set(r, v.clone()));
                let item = Ex::local(r, v.ty);
                self.prop(node.clone(), name, item.clone(), value.span);
                let title = self.meta_field(item, "title", Ty::Text);
                self.prop(node, "title", title, value.span);
                return true;
            }
            ("avatar", "who") => Ty::Principal,
            ("rich_text", "value") => Ty::RichText,
            ("card", "count") => Ty::Int,
            ("section", "title") | ("field" | "toggle", "label") => Ty::Text,
            _ => return false,
        };
        let v = self.expr_as(value, &want);
        self.prop(node, name, v, value.span);
        true
    }

    /// A row view's body is one list row, after any `let`s it reads.
    pub(super) fn row_view_body(&mut self, view: &str, ty: &str, rest: &[UiStmt], span: Span) {
        let ok = match rest.split_last() {
            Some((
                UiStmt {
                    kind: UiKind::Call(c),
                    ..
                },
                lets,
            )) => {
                lets.iter().all(|s| matches!(s.kind, UiKind::Let { .. }))
                    && match &c.call.kind {
                        ExprKind::Call { callee, args } => match &callee.kind {
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
        };
        if !ok {
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
        let list = self.unhold(list, items.span);
        let Some(Ty::Record(key)) = list.ty.element().cloned() else {
            if !list.ty.is_error() {
                let t = self.show(&list.ty);
                self.err("E097", items.span, format!("`list` shows resources: its items are a List or Collection of a record type, and this is {t}. Show plain values with `for x in xs {{ ... }}` in a `stack`"));
            }
            return n;
        };
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
        let has_rows = match (&c.block, &c.item) {
            (Some(_), Some(_)) => true,
            (Some(_), None) => {
                self.err(
                    "E097",
                    c.call.span,
                    "name the item of a list's block: `list(xs) { x -> row(x.title) }`",
                );
                false
            }
            (None, _) if self.env.row_view(&key).is_some() => true,
            (None, _) => {
                self.err("E097", c.call.span, format!("a list of {0} needs its rows: write an item block, `list(xs) {{ x -> row(x.title) }}`, or declare the type's row, `row view <view> for {0}`", key.name));
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
        match (&c.block, &c.item, row_view) {
            (Some(block), Some(var), _) => {
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
            self.err("E097", value.span, format!("`{what}:` takes the name of a `window` or `lens` declaration of this view whose type is an enum, such as `{what}: order`; this view declares: {known}"));
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
            self.err("E097", span, "a list row `row(title, ...)` stands directly in a list's item block, `list(xs) { x -> row(x.title) }`, or is a row view's body; for a layout row write `row { ... }`");
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
                Some(name @ ("secondary" | "meta" | "badge")) => {
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
                Some(name @ ("mark" | "icon")) => {
                    if name == "icon" {
                        icon = Some(value);
                    }
                    self.icon_value(n, name, value);
                }
                Some("avatar") => avatar = Some(value),
                Some("checkbox") => {
                    if self.bind_typed(&node, value, &[Ty::Bool]).is_some() {
                        self.emit(host(
                            Imp::UiPropH,
                            vec![node.clone(), Ex::str("bind"), Ex::str("checkbox")],
                        ));
                    }
                }
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
    /// square (a group); one that names none keeps the room an avatar takes (`avatar_space`),
    /// in line with the rows of its list that show one; its list drops the room when no row
    /// shows one.
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
        // No participant this time.
        self.push_sink();
        match icon {
            Some(icon) => {
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
            None => {
                self.emit(host(
                    Imp::UiPropI,
                    vec![row, Ex::str("avatar_space"), Ex::int(1)],
                ));
            }
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

    /// The field `name` of what the platform shows of the resource `item` (its `host.Meta`).
    fn meta_field(&mut self, item: Ex, name: &str, ty: Ty) -> Ex {
        let meta_ty = super::platform::host_ty("Meta");
        let m = self.fresh(meta_ty.clone());
        self.emit(Stmt::Set(
            m,
            Ex::host(Imp::MetaOf, vec![item], meta_ty.clone()),
        ));
        Self::field_of_meta(Ex::local(m, meta_ty), name, ty)
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
                    Ty::Error => Some(Ex::error()),
                    _ => None,
                }
            }
            ExprKind::Call { callee, args } if args.is_empty() => {
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
                        self.err("E098", value.span, "choose a field's value with an action; `choice` binds view state, a `window` or `lens` declaration of an enum type");
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

    /// The declared action or flow a button's first argument applies, when it applies one.
    fn declared_action(&self, args: &[ast::Arg]) -> Option<Callable> {
        let first = args
            .iter()
            .find(|a| a.name.is_none() && !self.is_flag(None, &a.value))?;
        // `send(c, draft).then(set(draft, ""))` presents as `send`.
        let mut applied = &first.value;
        while let ExprKind::Call { callee, .. } = &applied.kind
            && let ExprKind::Field { base, name } = &callee.kind
            && name.name == "then"
        {
            applied = base;
        }
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
        matches!(found.kind, CKind::Action | CKind::Flow).then(|| found.clone())
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
        let mut call = applied;
        while let ExprKind::Call { callee: c, .. } = &call.kind
            && let ExprKind::Field { base, name } = &c.kind
            && name.name == "then"
        {
            call = base;
        }
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
        requires: &[ast::Require],
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
        // Why it is unavailable on the subject now: the reason of its first requirement on the
        // subject alone that does not hold, which is set last. A requirement that reads what a
        // caller gives (`to` in `move_to(t, to)`) cannot be decided before it is given: it is
        // checked when the action runs with it, and a form shows its reason as the refusal.
        for r in requires.iter().rev() {
            if mentions(&r.cond, others)
                .or_else(|| mentions(&r.reason, others))
                .is_some()
            {
                continue;
            }
            // A requirement may read `invoker`: here, the person looking.
            self.st.invoker = super::Invoker::Viewer;
            let cond = self.expr_as(&r.cond, &Ty::Bool);
            let reason = self.expr_as(&r.reason, &Ty::Text);
            self.st.invoker = super::Invoker::None;
            self.push_sink();
            self.prop(node.clone(), "unavailable", reason, r.span);
            let unmet = self.pop_sink();
            self.emit(Stmt::If(cond, Vec::new(), unmet));
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

    // ---- facts ----------------------------------------------------------------------------

    /// `"Due": d.due` in `facts { }`: a `fact` node with its `label` and `value`.
    pub(super) fn fact(
        &mut self,
        node: ast::NodeId,
        label: &ast::Expr,
        value: &ast::Expr,
        parent: LocalId,
        span: Span,
    ) {
        if self.st.ui.parent != "facts" {
            self.err(
                "E098",
                span,
                "a fact `\"Label\": value` stands in `facts { }`",
            );
            return;
        }
        let key = self.stmt_key(node);
        let n = self.new_node(&crate::toolkit::kind("Property"), key);
        let l = self.expr_as(label, &Ty::Text);
        self.prop(Ex::local(n, Ty::View), "label", l, label.span);
        let v = self.expr(value, None);
        let shown = match &v.ty {
            Ty::Text
            | Ty::Int
            | Ty::Float
            | Ty::Bool
            | Ty::Principal
            | Ty::Date
            | Ty::Time
            | Ty::Identifier
            | Ty::Error => true,
            Ty::Option(inner) => matches!(**inner, Ty::Date | Ty::Time),
            _ => false,
        };
        if shown {
            // A Date or a Time also sets `value:as` (see `prop`).
            self.prop(Ex::local(n, Ty::View), "value", v, value.span);
        } else {
            let t = self.show(&v.ty);
            self.err("E098", value.span, format!("a fact's value is Text, a number, a Bool, a Principal, a Date or a Time, and this is {t}; show it as text with `\"{{...}}\"`"));
        }
        self.emit(host(
            Imp::UiChild,
            vec![Ex::local(parent, Ty::View), Ex::local(n, Ty::View)],
        ));
    }
}

/// The first of `names` an expression reads as a variable, ignoring the inside of lambdas.
pub(super) fn mentions<'a>(e: &ast::Expr, names: &[&'a str]) -> Option<&'a str> {
    let block = |b: &ast::Block| {
        b.stmts.iter().find_map(|s| match &s.kind {
            StmtKind::Expr(x) | StmtKind::Let { value: x, .. } => mentions(x, names),
            StmtKind::Return(x) => x.as_ref().and_then(|x| mentions(x, names)),
            StmtKind::Assign { value, .. } => mentions(value, names),
            StmtKind::For { iter, .. } | StmtKind::As { who: iter, .. } => mentions(iter, names),
        })
    };
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
        ExprKind::Unary { operand, .. } | ExprKind::Try(operand) => mentions(operand, names),
        ExprKind::Binary { lhs, rhs, .. } => mentions(lhs, names).or_else(|| mentions(rhs, names)),
        ExprKind::Call { callee, args } => {
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
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Length(..)
        | ExprKind::Duration(..)
        | ExprKind::Calendar(_)
        | ExprKind::Bool(_)
        | ExprKind::Ref(_)
        | ExprKind::Lambda { .. }
        | ExprKind::Root(_) => None,
    }
}
