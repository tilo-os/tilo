//! The native vocabulary (native-apps.md section 3): frames and where their columns stand,
//! lists and their rows, view state as native controls, facts, and buttons for declared
//! actions. Each lowers to the same `ui_*` host calls as every other node.

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
            "page"
                if args.iter().any(|a| {
                    a.name.is_none() && matches!(&a.value.kind, ExprKind::Ident(n) if n == "full")
                }) =>
            {
                self.err(
                    "E095",
                    c.call.span,
                    "`page(full)` is now `canvas { }`: free content without a readable width",
                );
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

    /// Checks that a primitive stands where it may: a frame's columns and its composer
    /// directly in a frame.
    pub(super) fn placement(&mut self, prim: &str, span: Span) {
        if prims::FRAMES.contains(&self.st.ui_parent) {
            return;
        }
        if matches!(prim, "sidebar" | "inspector") {
            self.err("E095", span, format!("`{prim}` is a column of the window's frame, so it stands directly in `document {{ }}`, `page {{ }}` or `canvas {{ }}`"));
        } else if prim == "composer" {
            self.err("E095", span, "`composer` is the entry bar along the foot of the window's frame, so it stands directly in `document { }`, `page { }` or `canvas { }`");
        }
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
                self.bind_typed(&node, value, &[Ty::Bool]);
                return true;
            }
            ("date", "bind") => {
                let day = [Ty::Date, Ty::Option(Box::new(Ty::Date))];
                self.bind_typed(&node, value, &day);
                return true;
            }
            ("inspector" | "card", "item") => {
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
                self.prop(node, name, v, value.span);
                return true;
            }
            ("avatar", "who") => Ty::Principal,
            ("card", "count") => Ty::Int,
            ("section", "title") | ("field" | "toggle", "label") => Ty::Text,
            _ => return false,
        };
        let v = self.expr_as(value, &want);
        self.prop(node, name, v, value.span);
        true
    }

    // ---- frames ---------------------------------------------------------------------------

    /// A window's view is one frame, after its view state and `let`s.
    pub(super) fn window_frame(&mut self, view: &str, ty: &str, rest: &[UiStmt], span: Span) {
        let framed = match rest.split_last() {
            Some((
                UiStmt {
                    kind: UiKind::Call(c),
                    ..
                },
                lets,
            )) => lets.iter().all(|s| matches!(s.kind, UiKind::Let { .. })) && self.is_frame(c),
            _ => false,
        };
        if !framed {
            self.err("E095", span, format!("`{view}` is the window of a {ty}, so its view is one frame: `list(...) {{ x -> ... }}`, `document {{ }}`, `page {{ }}` or `canvas {{ }}`, after any `window`, `lens` and `let`. Put the content inside the frame"));
        }
    }

    fn is_frame(&self, c: &UiCall) -> bool {
        let callee = match &c.call.kind {
            ExprKind::Call { callee, .. } => callee,
            _ => &c.call,
        };
        matches!(&callee.kind, ExprKind::Ident(n) if prims::FRAMES.contains(&n.as_str()) && self.lookup(n).is_none())
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
                        ExprKind::Call { callee, args } => {
                            matches!(&callee.kind, ExprKind::Ident(n) if n == "row")
                                && self.titled(args)
                                && c.block.is_none()
                        }
                        _ => false,
                    }
            }
            _ => false,
        };
        if !ok {
            self.err("E093", span, format!("`{view}` is the row view of {ty}, so its body is one list row, after any `let`s: `row(x.title, secondary: ...)`"));
        }
    }

    // ---- lists ----------------------------------------------------------------------------

    /// `list(items, sort: s, filter: f, empty: "...") { x -> ... }`: a `list` node with one
    /// `item` child per resource, each holding the item block's nodes or the type's row view.
    fn list_frame(&mut self, c: &UiCall, args: &[ast::Arg]) -> LocalId {
        let key = self.node_key(c);
        let list_key = self.fresh(Ty::Text);
        self.emit(Stmt::Set(list_key, key));
        let n = self.new_node("list", Ex::local(list_key, Ty::Text));
        let node = Ex::local(n, Ty::View);
        let mut items = None;
        let mut sort = None;
        let mut filter = None;
        for a in args {
            match a.name.as_ref().map(|i| i.name.as_str()) {
                None if self.is_flag(None, &a.value) => self.flag_arg(n, &a.value),
                None | Some("items") if items.is_none() => items = Some(&a.value),
                None => {
                    self.err("E022", a.value.span, "`list` takes one positional argument, its items; name the others: `sort:`, `filter:`, `empty:`");
                    let _ = self.expr(&a.value, None);
                }
                Some("sort") => sort = Some(&a.value),
                Some("filter") => filter = Some(&a.value),
                Some("empty") => {
                    let v = self.expr_as(&a.value, &Ty::Text);
                    self.prop(node.clone(), "empty", v, a.value.span);
                }
                Some(name) if prims::NODE_ARGS.contains(&name) => {
                    self.apply_arg(n, "list", name, &a.value);
                }
                Some(name) => {
                    self.err("E063", a.value.span, format!("`list` has no argument `{name}`; arguments: items, sort, filter, empty, {}", prims::NODE_ARGS.join(", ")));
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
        let before = self.diags.len();
        let list = self.expr(items, None);
        let clean = self.diags.len() == before;
        let Some(Ty::Record(key)) = list.ty.element().cloned() else {
            if !list.ty.is_error() {
                let t = self.show(&list.ty);
                self.err("E097", items.span, format!("`list` shows resources: its items are a List or Collection of a record type, and this is {t}. Show plain values with `for x in xs {{ ... }}` in a `stack`"));
            }
            return n;
        };
        let type_id = self
            .env
            .type_info(&key)
            .map(|t| t.id.clone())
            .unwrap_or_default();
        self.emit(host(
            Imp::UiPropH,
            vec![node, Ex::str("item_type"), Ex::str(&type_id)],
        ));
        if let Some(v) = sort
            && let Some((state, info)) = self.enum_state("sort", v)
        {
            self.state_control(n, ("sort.", "sort."), &state, &info, v.span);
        }
        if let Some(v) = filter
            && let Some((state, info)) = self.enum_state("filter", v)
        {
            self.state_control(n, ("filter.", "filter."), &state, &info, v.span);
            if clean {
                self.filter_counts(n, items, &state, &info);
            }
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

    /// One `item` node per resource, keyed `<list key>/<ref id>`, with the item block's nodes
    /// (keyed like a `for` loop's) or the row view as its children.
    fn list_items(&mut self, n: LocalId, list_key: LocalId, list: Ex, c: &UiCall) {
        let span = c.call.span;
        let elem = list.ty.element().cloned().unwrap_or(Ty::Error);
        let row_view = match (&c.block, &elem) {
            (None, Ty::Record(key)) => self.env.row_view(key).cloned(),
            _ => None,
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
                let saved = (self.st.ui_parent, self.st.list_row_ok);
                self.st.ui_parent = "item";
                self.st.list_row_ok = true;
                self.ui_stmts(block, it);
                (self.st.ui_parent, self.st.list_row_ok) = saved;
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

    /// `filter.count.<variant>`: how many items the list would show with the filter on that
    /// variant. The items are evaluated once per variant with the state's local set to it, then
    /// the local is restored.
    fn filter_counts(&mut self, n: LocalId, items: &ast::Expr, state: &StateRef, info: &EnumInfo) {
        let ty = state.ty.clone();
        for v in &info.variants {
            let saved = self.fresh(ty.clone());
            self.emit(Stmt::Set(saved, Ex::local(state.local, ty.clone())));
            let value = self.variant_value(state, &v.name, items.span);
            self.emit(Stmt::Set(state.local, value));
            let list = self.expr(items, None);
            let len = Ex::host(Imp::ListLen, vec![list], Ty::Int);
            self.emit(host(
                Imp::UiPropI,
                vec![
                    Ex::local(n, Ty::View),
                    Ex::str(&format!("filter.count.{}", v.name)),
                    len,
                ],
            ));
            self.emit(Stmt::Set(state.local, Ex::local(saved, ty.clone())));
        }
    }

    /// `row(title, secondary: ..., ...)`: a `list_row`.
    fn list_row(&mut self, c: &UiCall, args: &[ast::Arg]) -> LocalId {
        let span = c.call.span;
        if c.block.is_some() {
            self.err("E097", span, "a row with a title is a list row and takes no block; for a layout row write `row { ... }` with the title inside it as `text(...)`");
        }
        if !self.st.list_row_ok {
            self.err("E097", span, "a list row `row(title, ...)` stands directly in a list's item block, `list(xs) { x -> row(x.title) }`, or is a row view's body; for a layout row write `row { ... }`");
        }
        let key = self.node_key(c);
        let n = self.new_node("list_row", key);
        let node = Ex::local(n, Ty::View);
        let mut titled = false;
        for a in args {
            let value = &a.value;
            match a.name.as_ref().map(|i| i.name.as_str()) {
                None if self.is_flag(None, value) => self.flag_arg(n, value),
                None if !titled => {
                    titled = true;
                    self.row_subject(&node, value);
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
                Some(name @ ("mark" | "icon")) => self.icon_value(n, name, value),
                Some("avatar") => {
                    let v = self.expr_as(value, &Ty::Principal);
                    self.prop(node.clone(), "avatar", v, value.span);
                }
                Some("checkbox") => self.bind_typed(&node, value, &[Ty::Bool]),
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
        n
    }

    /// A row's text: Text, a Date or a Time, or an Option of one (`.none` is the empty text).
    /// A Date or a Time is marked for the host to format (see `prop`).
    /// A list row's positional: its title, or the resource it shows (a `resource` prop the
    /// host renders as that resource's row).
    fn row_subject(&mut self, node: &Ex, value: &ast::Expr) {
        let v = self.expr(value, Some(&Ty::Text));
        if matches!(v.ty, Ty::Record(_)) {
            self.prop(node.clone(), "resource", v, value.span);
        } else {
            self.text_prop(node, "title", v, value.span);
        }
    }

    fn text_arg(&mut self, node: &Ex, name: &str, value: &ast::Expr) {
        let v = self.expr(value, Some(&Ty::Text));
        self.text_prop(node, name, v, value.span);
    }

    fn text_prop(&mut self, node: &Ex, name: &str, v: Ex, span: Span) {
        let v = match &v.ty {
            Ty::Text | Ty::Date | Ty::Time | Ty::Error => v,
            Ty::Option(inner) if **inner == Ty::Text => self.text_or_empty(v),
            Ty::Option(inner) if matches!(**inner, Ty::Date | Ty::Time) => v,
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

    /// `mark: .pin` or `mark: if cond { .pin }`: the prop holds the icon's kebab-case name, set
    /// only on the branches that name one.
    fn icon_value(&mut self, n: LocalId, prop: &str, e: &ast::Expr) {
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
                self.err(
                    "E097",
                    e.span,
                    format!("`{prop}:` takes an icon such as `.pin`, or `if cond {{ .pin }}`"),
                );
                let _ = self.expr(e, None);
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
        let ExprKind::Call { callee, .. } = &first.value.kind else {
            return None;
        };
        let found = match &callee.kind {
            ExprKind::Ident(name) if self.lookup(name).is_none() => self.env.own_callable(name),
            ExprKind::Field { base, name } if self.names_import(base) => {
                let path = dotted(base)?.join(".");
                let import = self.env.find_import(&path)?;
                self.env.foreign_callable(&import.app, &name.name)
            }
            _ => None,
        }?;
        matches!(found.kind, CKind::Action | CKind::Flow).then(|| found.clone())
    }

    /// `button(act(x))`: a button whose label, icon and role are the action's, and whose
    /// `press` applies it.
    fn action_button(&mut self, callee: &Callable, c: &UiCall, args: &[ast::Arg]) -> LocalId {
        let span = c.call.span;
        let key = self.node_key(c);
        let n = self.new_node("button", key);
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
                    if let Some(role) = &p.role {
                        self.prop(node.clone(), "role", Ex::str(role), span);
                    }
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
                    } else if name == "enabled" || prims::NODE_ARGS.contains(&name) {
                        self.apply_arg(n, "button", name, value);
                        continue;
                    } else {
                        self.err("E063", value.span, format!("`button` has no argument `{name}`; a button for an action takes `enabled:` and the sizing arguments"));
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
        params: &[&str],
    ) -> crate::ir::Func {
        self.begin(super::Ctx::View, &view.params, Ty::View, &view.id);
        let n = self.new_node("present", Ex::str(&view.id));
        let node = Ex::local(n, Ty::View);
        let others = params.get(1..).unwrap_or_default();
        for opt in opts {
            let name = opt.name.name.as_str();
            let dynamic = name == "on" || matches!(opt.value.kind, ExprKind::If { .. });
            if !dynamic || !matches!(name, "label" | "icon" | "on") {
                continue;
            }
            if let Some(other) = mentions(&opt.value, others) {
                let subject = params.first().copied().unwrap_or_default();
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
        if self.st.ui_parent != "facts" {
            self.err(
                "E098",
                span,
                "a fact `\"Label\": value` stands in `facts { }`",
            );
            return;
        }
        let key = self.stmt_key(node);
        let n = self.new_node("fact", key);
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
fn mentions<'a>(e: &ast::Expr, names: &[&'a str]) -> Option<&'a str> {
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
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Length(..)
        | ExprKind::Bool(_)
        | ExprKind::Ref(_)
        | ExprKind::Lambda { .. }
        | ExprKind::Root(_) => None,
    }
}
