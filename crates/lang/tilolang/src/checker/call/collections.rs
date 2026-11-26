//! The methods a collection offers: moving, placing and removing items, and reading a map's
//! entries by key.

use super::{Checker, Ctx, Ek, Ex, ExprKind, FieldInfo, Imp, Span, Stmt, Ty, TypeKey, ast};

impl Checker<'_> {
    pub(super) fn container_method(
        &mut self,
        b: Ex,
        key: &TypeKey,
        f: &FieldInfo,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> Ex {
        // A value the host made (`host.Step`) is held whole, never a resource: its map or list is
        // read as the value it is, and nothing writes through it.
        if crate::host::is_value(key) {
            let r = self.field_read(
                b,
                &ast::Ident {
                    name: f.name.clone(),
                    span: name.span,
                    from_template_of: None,
                },
            );
            return self.value_method(r, name, args, span, None);
        }
        let m = name.name.as_str();
        if let (Ty::Map(t), "get" | "keys") = (&f.ty, m) {
            return self.map_read(b, f, t, name, args, span);
        }
        // Items under (above) one by a field of theirs is a query of the store's index; the field's
        // own read is the fallback for a list held anywhere else.
        if let (Ty::Collection(t), "descendants" | "ancestors") = (&f.ty, m) {
            let list = self.field_read(
                b.clone(),
                &ast::Ident {
                    name: f.name.clone(),
                    span: name.span,
                    from_template_of: None,
                },
            );
            let field = f.id.clone();
            return self.relatives(list, t, name, args, span, Some((b, field)));
        }
        // What a container field can be written through; its reads are its value's.
        let writes: &[&str] = match &f.ty {
            Ty::Collection(_) => &["insert", "remove", "remove_where", "move", "place"],
            Ty::Set(_) => &["add", "remove"],
            Ty::Map(v) if matches!(**v, Ty::Set(_)) => &["add", "remove", "put"],
            Ty::Map(_) => &["put", "remove"],
            _ => &[],
        };
        let valid = writes.contains(&m);
        let reads = super::super::methods::names(&f.ty).unwrap_or_default();
        if !valid && !reads.contains(&m) {
            let t = self.show(&f.ty);
            let ops = match &f.ty {
                Ty::Collection(_) | Ty::Set(_) => {
                    format!("{}, and what its items answer: {}", writes.join(", "), reads.join(", "))
                }
                Ty::Map(v) if matches!(**v, Ty::Set(_)) => {
                    "add(key, element), remove(key, element), remove(key), get (an Option of the set) and keys".to_owned()
                }
                _ => "put, remove, get (an Option of the value) and keys".to_owned(),
            };
            let known = writes.iter().chain(reads.iter()).copied();
            let hint = crate::diag::suggestion(m, known);
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
                    from_template_of: None,
                },
            );
            return self.value_method(r, name, args, span, None);
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
        self.writes_field(key, f);
        let fid = Ex::str(&f.id);
        match (&f.ty, m) {
            (Ty::Collection(t), "insert") => {
                // A migration keeps the id an earlier build gave what it carries over:
                // `.insert(item, id: e.id)`. A key names the item by what it is, so every device
                // that inserts it under that key inserts the same item:
                // `.insert(item, key: "{from.id()}|{mention}")`.
                let kept = match args {
                    [_, kept] if kept.name.as_ref().is_some_and(|n| n.name == "id") => {
                        if !self.migrating {
                            self.err(
                                "E022",
                                kept.value.span,
                                "only a `migration action` gives an item the id it had: an item anything else inserts takes a fresh one, or the one its `key` names",
                            );
                        }
                        Some((Imp::CollInsertAs, self.expr_as(&kept.value, &Ty::Text)))
                    }
                    [_, key] if key.name.as_ref().is_some_and(|n| n.name == "key") => {
                        Some((Imp::CollInsertKeyed, self.expr_as(&key.value, &Ty::Text)))
                    }
                    _ => None,
                };
                let first = if kept.is_some() { &args[..1] } else { args };
                if !self.arity(name, first, 1, span) {
                    return Ex::error();
                }
                let Some(a) = first.first() else {
                    return Ex::error();
                };
                let v = self.expr(&a.value, Some(t));
                let case = (matches!(**t, Ty::Union(_))
                    && !v.ty.is_error()
                    && !v.ty.same(t)
                    && v.ty.fits(t))
                .then(|| v.ty.clone());
                let v = self.coerce(v, t, a.value.span);
                let inserted = case.unwrap_or_else(|| (**t).clone());
                self.inserted.extend(inserted.records());
                match kept {
                    Some((imp, id)) => Ex::host(imp, vec![b, fid, v, id], inserted),
                    None => Ex::host(Imp::CollInsert, vec![b, fid, v], inserted),
                }
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
            (Ty::Collection(t), "place") => self.place_item(b, fid, t, args, span),
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
            // A map of sets: each element stored on its own, so concurrent additions all stay.
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
        // `to: i, among: run`: the place `i` among the items of `run` (any part of the collection
        // in its own order, such as the items one filter shows) other than the one moved.
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
            // An index, as a drag gives one: resolved, when the action runs, to the item it then
            // comes before, which is what is recorded.
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

    /// `items.place(item, .parent, inside: x)`, `before: x` or `after: x`: the item and
    /// everything under it (by the parent field `.parent` names) to one place of the tree — at
    /// the end of `x`'s own (`inside`, none for the end of the top level), just before `x`, or
    /// just after `x` and all it holds. One step, so a move is one change and one undo, and the
    /// subtree keeps its shape: what a view's `Tab`, `Shift-Tab`, `Alt-Up` and `Alt-Down` and its
    /// drags ask for, named by the neighbour the view shows.
    fn place_item(&mut self, b: Ex, fid: Ex, t: &Ty, args: &[ast::Arg], span: Span) -> Ex {
        let usage = "write `.place(item, .parent, inside: parent)`, `.place(item, .parent, before: other)`, `.place(item, .parent, after: other)`, `.place(item, .parent, under: over)` (indent), `.place(item, .parent, up: over)` or `.place(item, .parent, down: below)`";
        let (Some(item), Some(field), Some(place), 3) =
            (args.first(), args.get(1), args.get(2), args.len())
        else {
            self.err(
                "E022",
                span,
                format!(
                    "`.place` takes the item, the field its tree is by, and where it goes: {usage}"
                ),
            );
            for a in args {
                let _ = self.expr(&a.value, None);
            }
            return Ex::error();
        };
        let Some((param, body)) = super::super::optional::lambda_of(&field.value) else {
            self.err(
                "E022",
                field.value.span,
                "`.place` reads the tree by one field of its items: `.place(item, .parent, …)`",
            );
            let _ = self.expr(&field.value, None);
            return Ex::error();
        };
        let parent = match &body.kind {
            ExprKind::Field { base, name } if matches!(&base.kind, ExprKind::Ident(at) if at == &param.name) => {
                name.name.clone()
            }
            _ => {
                self.err(
                    "E022",
                    field.value.span,
                    "`.place` follows one parent field: write `.parent`",
                );
                return Ex::error();
            }
        };
        let item = self.expr_as(&item.value, t);
        let opt = Ty::Option(Box::new(t.clone()));
        let (mode, target) = match place.name.as_ref().map(|n| n.name.as_str()) {
            Some("inside") => (0, self.expr_as(&place.value, &opt)),
            Some("before") => (1, self.expr_as(&place.value, t)),
            Some("after") => (2, self.expr_as(&place.value, t)),
            Some("under") => (3, self.expr_as(&place.value, &opt)),
            Some("up") => (4, self.expr_as(&place.value, &opt)),
            Some("down") => (5, self.expr_as(&place.value, &opt)),
            _ => {
                self.err(
                    "E022",
                    place.value.span,
                    format!(".place names where the item goes: {usage}"),
                );
                let _ = self.expr(&place.value, None);
                return Ex::error();
            }
        };
        let mode = Ex::new(Ek::I32(mode), Ty::Bool);
        Ex::host(
            Imp::CollPlace,
            vec![b, fid, Ex::str(&parent), item, target, mode],
            Ty::Unit,
        )
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
                from_template_of: None,
            },
        );
        let filter = ast::Ident {
            name: "filter".into(),
            span: name.span,
            from_template_of: None,
        };
        let gone = self.higher_order(read, t, &filter, args, span, None);
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
        let value = match value {
            Ty::Set(element) => Ty::List(element.clone()),
            other => other.clone(),
        };
        let ty = Ty::Option(Box::new(value));
        Ex::host(Imp::MapGetH, vec![b, fid, key], ty)
    }

    /// `m.keys()` and `m.get(key)` of a map held as a value, not a field's.
    pub(super) fn map_value_method(
        &mut self,
        m: Ex,
        value: &Ty,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> Ex {
        if name.name == "keys" {
            if !self.arity(name, args, 0, span) {
                return Ex::error();
            }
            return Ex::host(Imp::MapKeysValue, vec![m], Ty::List(Box::new(Ty::Text)));
        }
        // `m.put(key, value)` and `m.remove(key)` fill the map; a view draws what a map holds, it
        // never fills one.
        if matches!(name.name.as_str(), "put" | "remove") {
            if !matches!(self.st.ctx, Ctx::Action | Ctx::Fn) {
                let who = self.st.ctx.word();
                self.err(
                    "E040",
                    span,
                    format!(
                        "{who} cannot change the entries of a map; only an action or a fn fills one"
                    ),
                );
                for a in args {
                    let _ = self.expr(&a.value, None);
                }
                return Ex::error();
            }
            let puts = name.name == "put";
            if !self.arity(name, args, if puts { 2 } else { 1 }, span) {
                return Ex::error();
            }
            let Some(k) = args.first() else {
                return Ex::error();
            };
            let key = self.expr_as(&k.value, &Ty::Text);
            if !puts {
                return Ex::host(Imp::MapRemoveValue, vec![m, key], Ty::Unit);
            }
            let Some(v) = args.get(1) else {
                return Ex::error();
            };
            let held = self.expr_as(&v.value, value);
            let held = Self::handle_of(held);
            return Ex::host(Imp::MapPutValue, vec![m, key, held], Ty::Unit);
        }
        if !self.arity(name, args, 1, span) {
            return Ex::error();
        }
        let Some(k) = args.first() else {
            return Ex::error();
        };
        let key = self.expr_as(&k.value, &Ty::Text);
        let value = match value {
            Ty::Set(element) => Ty::List(element.clone()),
            other => other.clone(),
        };
        Ex::host(Imp::MapGetValue, vec![m, key], Ty::Option(Box::new(value)))
    }
}
