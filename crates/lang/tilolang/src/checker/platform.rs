//! What code reads of the platform: the actions offered on a resource, who else is at it, what
//! it is called, an enum's cases, and the platform's own phrases. Each is a value of the
//! language, so a view composes headers, menus and presence from them as `std` does.

use super::{Checker, Ex};
use crate::abi::Imp;
use crate::ast;
use crate::diag::Span;
use crate::ir::{Ek, Stmt};
use crate::resolve::CKind;
use crate::types::{Ty, TypeKey};

/// A value type of the host: `host.Action`.
pub(crate) fn host_ty(name: &str) -> Ty {
    Ty::Record(TypeKey {
        app: crate::host::HOST_APP.to_string(),
        name: name.to_string(),
    })
}

impl Checker<'_> {
    /// The one positional argument of a built-in, checked as a resource.
    fn resource_arg(&mut self, what: &str, args: &[ast::Arg], span: Span) -> Option<Ex> {
        let [a] = args else {
            self.err(
                "E022",
                span,
                format!("`{what}` takes one resource: `{what}(r)`"),
            );
            return None;
        };
        if a.name.is_some() {
            self.err(
                "E022",
                a.value.span,
                format!("`{what}` takes the resource by position: `{what}(r)`"),
            );
        }
        let r = self.expr(&a.value, None);
        match &r.ty {
            Ty::Record(_) => Some(r),
            Ty::Error => None,
            other => {
                let t = self.show(other);
                self.err(
                    "E020",
                    a.value.span,
                    format!("`{what}` takes a resource, a value of a record type; this is {t}"),
                );
                None
            }
        }
    }

    /// `actions(r)`: the actions offered on `r` to the person looking, under the authority
    /// that decides what they may do: `r`'s own, and those of the resource holding it that
    /// take it (`delete(nb, n)` on a note). Each is a `host.Action`: label, icon, role,
    /// whether it is the primary one, its parameters with their defaults, and `run`, the
    /// action applied to `r`, which a control runs.
    ///
    /// `actions(r, above)` offers them where the view shows `r` under `above`: the item just
    /// above it in its group, or `.none` for the first of its group. An action that reads a
    /// place (`above: Option<T> (above)`) is offered with it, and one whose `applies` does not
    /// hold there is left out.
    pub(super) fn builtin_actions(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let Some(r) = self.resource_arg("actions", args.get(..1).unwrap_or_default(), span) else {
            return Ex::error();
        };
        let subject = r.ty.clone();
        let above = match args.get(1) {
            Some(a) => {
                if a.name.is_some() {
                    self.err(
                        "E022",
                        a.value.span,
                        "`actions` takes the place it offers the item in by position: `actions(r, above)`",
                    );
                }
                let want = Ty::Option(Box::new(subject));
                self.expr_as(&a.value, &want)
            }
            None => Ex::host(Imp::NoneV, Vec::new(), Ty::Option(Box::new(subject))),
        };
        if let Some(extra) = args.get(2) {
            self.err(
                "E022",
                extra.value.span,
                "`actions` takes the item and where the view shows it: `actions(r)` or `actions(r, above)`",
            );
            let _ = self.expr(&extra.value, None);
        }
        Ex::host(
            Imp::Actions,
            vec![r, above],
            Ty::List(Box::new(host_ty("Action"))),
        )
    }

    /// `lenses(r)`: the lenses on `r` now, the person's own and everyone else's: the people
    /// and agents whose windows show it. Presence on any component reads it.
    /// `created(r)`, `updated(r)` and `history(r)`: the versions of `r` the log holds (who made
    /// each, when, by which action), read-only. `created` is the first, `updated` the last;
    /// both are `.none` while the log of `r` is not at hand.
    pub(super) fn builtin_versions(&mut self, which: &str, args: &[ast::Arg], span: Span) -> Ex {
        let Some(r) = self.resource_arg(which, args, span) else {
            return Ex::error();
        };
        let version = host_ty("Version");
        match which {
            "history" => Ex::host(Imp::Versions, vec![r], Ty::List(Box::new(version))),
            // The ends alone, which a store with an index keeps per resource: no history read.
            _ => {
                let last = Ex::new(Ek::I32(i32::from(which == "updated")), Ty::Bool);
                Ex::host(
                    Imp::VersionEnd,
                    vec![r, last],
                    Ty::Option(Box::new(version)),
                )
            }
        }
    }

    /// `steps(r)`: the history of `r` as people read it, newest first: consecutive versions by
    /// the same person, or by the same task run, folded into one `host.Step`. Which versions
    /// fold is the platform's rule, the same in every app's history, so no view writes it.
    pub(super) fn builtin_steps(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let Some(r) = self.resource_arg("steps", args, span) else {
            return Ex::error();
        };
        Ex::host(Imp::Steps, vec![r], Ty::List(Box::new(host_ty("Step"))))
    }

    /// `unseen(r)`: the versions of `r` the person looking has not seen, oldest first: those a
    /// task run or an agent made after the person's seen mark of `r` and after their own last
    /// change of it (task-legibility.md §2, §4). A view that reads it runs again when either
    /// moves.
    pub(super) fn builtin_unseen(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let Some(r) = self.resource_arg("unseen", args, span) else {
            return Ex::error();
        };
        Ex::host(Imp::Unseen, vec![r], Ty::List(Box::new(host_ty("Version"))))
    }

    /// `changes(of: run)`: every version the task run made, of every resource, oldest first.
    pub(super) fn builtin_changes(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let [a] = args else {
            self.err(
                "E022",
                span,
                "`changes` takes one task run: `changes(of: run)`",
            );
            return Ex::error();
        };
        if a.name.as_ref().is_some_and(|n| n.name != "of") {
            self.err(
                "E022",
                a.value.span,
                "`changes` takes the run as `of`: `changes(of: run)`",
            );
        }
        let run_ty = host_ty("TaskRun");
        let run = self.expr(&a.value, Some(&run_ty));
        match &run.ty {
            Ty::Error => return Ex::error(),
            ty if *ty == run_ty => {}
            other => {
                let t = self.show(other);
                self.err(
                    "E020",
                    a.value.span,
                    format!("`changes` takes a `host.TaskRun` (a version's `task`); this is {t}"),
                );
                return Ex::error();
            }
        }
        Ex::host(
            Imp::RunChanges,
            vec![run],
            Ty::List(Box::new(host_ty("Version"))),
        )
    }

    pub(super) fn builtin_lenses(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let Some(r) = self.resource_arg("lenses", args, span) else {
            return Ex::error();
        };
        Ex::host(Imp::Lenses, vec![r], Ty::List(Box::new(host_ty("Lens"))))
    }

    /// The collection `c` holds and the type of its items, which a fact about a conversation
    /// reads: a channel and its messages.
    fn conversation_of(&mut self, what: &str, c: &Ex, span: Span) -> Option<(String, TypeKey)> {
        let Ty::Record(key) = &c.ty else {
            return None;
        };
        let env = self.env;
        let found = env.type_info(key).and_then(|info| {
            info.fields.iter().find_map(|f| match &f.ty {
                Ty::Collection(item) => match &**item {
                    Ty::Record(item) => Some((f.clone(), item.clone())),
                    _ => None,
                },
                _ => None,
            })
        });
        let Some((field, item)) = found else {
            let name = key.name.clone();
            self.err(
                "E020",
                span,
                format!(
                    "`{what}` takes a resource holding a collection of records; `{name}` holds none"
                ),
            );
            return None;
        };
        Some((field.name, item))
    }

    /// `first_unseen(c)`: the first item of `c`'s own collection that the person looking has
    /// not seen, oldest first (a conversation's first unread message), or `.none` when nothing
    /// of it is unseen. Read from `c`'s versions and the person's seen mark of it, so a
    /// transcript draws its "new messages" divider where reading left off without counting
    /// anything. A view that reads it runs again when `c` moves or its mark does.
    pub(super) fn builtin_first_unseen(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let Some(c) = self.resource_arg("first_unseen", args, span) else {
            return Ex::error();
        };
        let Some((field, item)) = self.conversation_of("first_unseen", &c, span) else {
            return Ex::error();
        };
        let id = self
            .env
            .type_info(&item)
            .map(|info| info.id.clone())
            .unwrap_or_default();
        Ex::host(
            Imp::FirstUnseen,
            vec![c, Ex::str(&field), Ex::str(&id)],
            Ty::Option(Box::new(Ty::Record(item))),
        )
    }

    /// `mentions(c)`: how many of `c`'s items name the person looking, that they have not seen:
    /// a channel's mention count beside its unread count (`unseen(c)`), from the same versions
    /// and seen mark, so a caught-up channel counts none without reading a message.
    pub(super) fn builtin_mentions(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let Some(c) = self.resource_arg("mentions", args, span) else {
            return Ex::error();
        };
        let Some((field, item)) = self.conversation_of("mentions", &c, span) else {
            return Ex::error();
        };
        let id = self
            .env
            .type_info(&item)
            .map(|info| info.id.clone())
            .unwrap_or_default();
        Ex::host(
            Imp::Mentions,
            vec![c, Ex::str(&field), Ex::str(&id)],
            Ty::Int,
        )
    }

    /// `resource(id)`: the resource the store keeps under the id text, read as its own type
    /// (`host.Resource`, any resource). A place names what it shows by the id alone, so its
    /// shell reads that resource — its actions, its title — without knowing the type ahead.
    /// `.none` for text that names no resource this store holds. A view that reads it runs
    /// again when the resource changes.
    ///
    /// `resource(id, of: alias.T)`: the resource of another package's type `T` at that id, as a
    /// `T` — what an id another app gave names, read as the type its app says it is (a window
    /// of the session's, by its address). Its fields read from what that app publishes, and its
    /// actions go to that app, which refuses an id it holds no such resource at.
    pub(super) fn builtin_resource(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        if let [a, of] = args
            && a.name.is_none()
            && of.name.as_ref().is_some_and(|n| n.name == "of")
        {
            return self.resource_of(a, of);
        }
        let [a] = args else {
            self.err(
                "E022",
                span,
                "`resource` takes the id of one resource: `resource(p.record)`, \
                 or `resource(id, of: alias.T)` for one of another package's type",
            );
            return Ex::error();
        };
        if a.name.is_some() {
            self.err(
                "E022",
                a.value.span,
                "`resource` takes the id by position: `resource(p.record)`",
            );
        }
        let id = self.expr_as(&a.value, &Ty::Text);
        Ex::host(
            Imp::ResourceRef,
            vec![id],
            Ty::Option(Box::new(Ty::Record(super::resource_key()))),
        )
    }

    /// `resource(id, of: alias.T)` ([`Self::builtin_resource`]).
    fn resource_of(&mut self, a: &ast::Arg, of: &ast::Arg) -> Ex {
        let key = match self.type_named(&of.value) {
            Some(Ty::Record(key)) if key.app != self.env.app => key,
            _ => {
                self.err(
                    "E022",
                    of.value.span,
                    "`of:` names a type of a package this one uses (`of: session.Window`); \
                     a resource of this package's own is read from the record that holds it",
                );
                return Ex::error();
            }
        };
        let id = self.expr_as(&a.value, &Ty::Text);
        Ex::host(
            Imp::RefLit,
            vec![
                Ex::str(&key.app),
                Ex::str(&format!("{}#{}", key.app, key.name)),
                id,
            ],
            Ty::Record(key),
        )
    }

    /// `delivery(r)`: how the outside effect `r`'s actions queued last stands (sending a mail),
    /// `.none` when they queued none: waiting to go online, sending, sent, rejected with the
    /// service's reason, or outcome unknown. A view that reads it runs again when it moves.
    pub(super) fn builtin_delivery(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let Some(r) = self.resource_arg("delivery", args, span) else {
            return Ex::error();
        };
        Ex::host(
            Imp::Delivery,
            vec![r],
            Ty::Option(Box::new(host_ty("Delivery"))),
        )
    }

    /// `members(r)`: the workspace's members who can read `r` now, as the platform's
    /// membership and grants decide, each a `Principal` (`name()`, `kind()`, `role()`). A view
    /// that reads it runs again when membership or grants change.
    pub(super) fn builtin_members(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let Some(r) = self.resource_arg("members", args, span) else {
            return Ex::error();
        };
        Ex::host(Imp::Members, vec![r], Ty::List(Box::new(Ty::Principal)))
    }

    /// `workspace.members`: everyone in the workspace now, people and agents, read from the
    /// platform's membership, never a copy the app keeps.
    ///
    /// `workspace.me`: the person the view is shown to, as the runtime knows them, read where
    /// the view runs, so it names the viewer of each window and each embedded view alike.
    pub(super) fn workspace_field(&mut self, name: &ast::Ident) -> Ex {
        if name.name == "members" {
            return Ex::host(
                Imp::WorkspaceMembers,
                Vec::new(),
                Ty::List(Box::new(Ty::Principal)),
            );
        }
        if name.name == "me" {
            // Who is shown a view: an action has an `invoker` instead, it acts for someone.
            if self.st.ctx != super::Ctx::View {
                let who = self.st.ctx.word();
                self.err(
                    "E064",
                    name.span,
                    format!(
                        "`workspace.me` is the person a view is shown to; {who} cannot read it"
                    ),
                );
                return Ex::error();
            }
            return Ex::host(Imp::Viewer, Vec::new(), Ty::Principal);
        }
        self.err(
            "E021",
            name.span,
            format!(
                "`workspace` has no field `{}`; fields are: {}",
                name.name,
                crate::prims::WORKSPACE_FIELDS
            ),
        );
        Ex::error()
    }

    /// The people a rich input's `@` drawer offers, which the system gives every rich input
    /// (chat.md, "Completion drawers"): who spoke last in `subject` (the resource the view
    /// shows) and its members; without a subject, nobody first and the workspace's members.
    pub(super) fn drawer_people(subject: Option<Ex>) -> (Ex, Ex) {
        let people = Ty::List(Box::new(Ty::Principal));
        match subject {
            Some(r) => (
                Ex::host(Imp::Speakers, vec![r.clone()], people.clone()),
                Ex::host(Imp::Members, vec![r], people),
            ),
            None => (
                Ex::host(Imp::ListNew, Vec::new(), people.clone()),
                Ex::host(Imp::WorkspaceMembers, Vec::new(), people),
            ),
        }
    }

    /// `meta(r)` or `meta(T)`: what the platform shows of a resource or its type, a
    /// `host.Meta`: its title, its type's name, the noun a sentence calls it by, its app's
    /// icon, and whether it opens in a window of its own.
    pub(super) fn builtin_meta(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let [a] = args else {
            self.err(
                "E022",
                span,
                "`meta` takes a resource or a type: `meta(note)`, `meta(Note)`",
            );
            return Ex::error();
        };
        if let Some(ty) = self.type_named(&a.value) {
            return match ty {
                Ty::Record(key) => {
                    let id = self
                        .env
                        .type_info(&key)
                        .map(|t| t.id.clone())
                        .unwrap_or_default();
                    Ex::host(Imp::MetaOf, vec![Ex::str(&id)], host_ty("Meta"))
                }
                Ty::Error => Ex::error(),
                other => {
                    let t = self.show(&other);
                    self.err(
                        "E020",
                        a.value.span,
                        format!("`meta` takes a resource or a record type; {t} is neither"),
                    );
                    Ex::error()
                }
            };
        }
        let Some(r) = self.resource_arg("meta", args, span) else {
            return Ex::error();
        };
        Ex::host(Imp::MetaOf, vec![r], host_ty("Meta"))
    }

    /// `cases(E)`: every case of the enum `E`, in declaration order. Each case carries no
    /// payload.
    pub(super) fn builtin_cases(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let Some(key) = self.cases_of(args, span) else {
            return Ex::error();
        };
        let ty = Ty::Enum(key.clone());
        let Some(info) = self.env.enum_info(&key).cloned() else {
            return Ex::error();
        };
        if let Some(v) = info.variants.iter().find(|v| !v.fields.is_empty()) {
            self.err(
                "E097",
                span,
                format!(
                    "`cases` lists the cases of an enum whose cases carry nothing; `.{}` of {} carries a payload",
                    v.name, key.name
                ),
            );
            return Ex::error();
        }
        let list_ty = Ty::List(Box::new(ty.clone()));
        let list = self.fresh(list_ty.clone());
        let mut stmts = vec![Stmt::Set(
            list,
            Ex::host(Imp::ListNew, Vec::new(), list_ty.clone()),
        )];
        for v in &info.variants {
            let value = self.variant_named(&ty, &v.name, span);
            stmts.push(Stmt::Eval(Ex::host(
                Imp::ListPushH,
                vec![Ex::local(list, list_ty.clone()), value],
                Ty::Unit,
            )));
        }
        Ex::new(
            Ek::Seq(stmts, Box::new(Ex::local(list, list_ty.clone()))),
            list_ty,
        )
    }

    /// `dated(text, at)`: when `text` says something is due, as people type it into a quick
    /// add ("Call the landlord tomorrow 9am"), read against the time `at` (in an action, `now`)
    /// in the zone of who runs it: a `host.Dated` holding the text without the phrase that said
    /// when, the day it named and the moment, when it named a time (`crate::natural`).
    pub(super) fn builtin_dated(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let [text, at] = args else {
            self.err(
                "E022",
                span,
                "`dated` takes the text and the time it is read at: `dated(entry, now)`",
            );
            return Ex::error();
        };
        if let Some(a) = [text, at].into_iter().find(|a| a.name.is_some()) {
            self.err(
                "E022",
                a.value.span,
                "`dated` takes its text and time by position: `dated(entry, now)`",
            );
        }
        let text = self.expr_as(&text.value, &Ty::Text);
        let at = self.expr_as(&at.value, &Ty::Time);
        Ex::host(Imp::Dated, vec![text, at], host_ty("Dated"))
    }

    /// `phrase("tilolang-empty")` or `phrase("tilolang-none-selected", kind: noun)`: one of the
    /// platform's phrases, in the person's language, with its arguments.
    pub(super) fn builtin_phrase(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let Some((first, rest)) = args.split_first() else {
            self.err(
                "E022",
                span,
                "`phrase` takes the phrase's id: `phrase(\"tilolang-empty\")`",
            );
            return Ex::error();
        };
        let id = self.expr_as(&first.value, &Ty::Text);
        let mut pairs = Vec::new();
        for a in rest {
            let Some(name) = &a.name else {
                self.err(
                    "E022",
                    a.value.span,
                    "a phrase's arguments are named: `phrase(\"id\", kind: noun)`",
                );
                continue;
            };
            let v = self.expr(&a.value, Some(&Ty::Text));
            let v = match v.ty {
                Ty::Text | Ty::Error => v,
                _ => self.text_of(v, a.value.span),
            };
            pairs.push((name.name.clone(), v));
        }
        let args = self.text_pairs(pairs);
        Ex::host(Imp::Phrase, vec![id, args], Ty::Text)
    }

    /// `v.is(edit)` of a `host.Version` or a `host.Action`: whether the action it names is the
    /// declared action `edit`, named by the compiler, so a renamed or relabelled action is still
    /// found and a misspelt one is an error.
    pub(super) fn names_action(&mut self, r: Ex, ty: &str, args: &[ast::Arg], span: Span) -> Ex {
        let named = match args {
            [a] if a.name.is_none() => match &a.value.kind {
                ast::ExprKind::Ident(name) if self.lookup(name).is_none() => self
                    .own_callable(name)
                    .filter(|c| matches!(c.kind, CKind::Action | CKind::Flow))
                    .map(|c| c.id.clone()),
                ast::ExprKind::Field { base, name } if self.names_import(base) => {
                    super::expr::dotted(base).and_then(|path| {
                        let import = self.env.find_import(&path.join("."))?;
                        self.env
                            .foreign_callable(&import.app, &name.name)
                            .filter(|c| matches!(c.kind, CKind::Action | CKind::Flow))
                            .map(|c| c.id.clone())
                    })
                }
                _ => None,
            },
            _ => None,
        };
        let (Some(id), Some(at)) = (named, crate::host::action_id_at(ty)) else {
            self.err(
                "E022",
                span,
                format!("`.is` takes one of the app's actions by its name: `history(m).any(v -> v.is(edit))`; a `host.{ty}` names the action, not its label"),
            );
            return Ex::error();
        };
        let at = Ex::new(Ek::I32(i32::try_from(at).unwrap_or(0)), Ty::Bool);
        let held = Ex::host(Imp::TupleGetH, vec![r, at], Ty::Text);
        Ex::host(Imp::StrEq, vec![held, Ex::str(&id)], Ty::Bool)
    }

    /// A field of a value of a host value type (`a.label` of a `host.Action`): read from the
    /// tuple it is held as.
    pub(super) fn value_field(&mut self, b: Ex, key: &TypeKey, name: &ast::Ident) -> Option<Ex> {
        if key.app != crate::host::HOST_APP {
            return None;
        }
        let value = crate::host::value_type(&key.name)?;
        let Some(index) = crate::host::value_field(&key.name, &name.name) else {
            let names: Vec<&str> = value.fields.iter().map(|(n, ..)| *n).collect();
            self.err(
                "E021",
                name.span,
                format!(
                    "`host.{}` has no field `{}`; fields are: {}",
                    key.name,
                    name.name,
                    names.join(", ")
                ),
            );
            return Some(Ex::error());
        };
        let ty = self
            .env
            .type_info(key)
            .and_then(|info| info.field(&name.name))
            .map_or(Ty::Error, |f| f.ty.clone());
        if ty == Ty::View {
            return Some(self.shown_value(b, &key.name, name));
        }
        let at = Ex::new(Ek::I32(i32::try_from(index).unwrap_or(0)), Ty::Bool);
        let (imp, c) = super::load_op(&ty, Imp::TupleGetI, Imp::TupleGetF, Imp::TupleGetH);
        let raw = if c.is_some() { Ty::Int } else { ty.clone() };
        Some(super::conv(c, Ex::host(imp, vec![b, at], raw), ty))
    }

    /// `c.before` or `c.after` of a `host.Change`: what the field held, shown as its type shows
    /// it, a node of the view placed where the expression stands. Its key is the place in the
    /// source with the identity of the enclosing items, as every node's is.
    fn shown_value(&mut self, b: Ex, ty: &str, name: &ast::Ident) -> Ex {
        if self.st.ctx != super::Ctx::View {
            let who = self.st.ctx.word();
            self.err(
                "E061",
                name.span,
                format!(
                    "`{}` of a `host.{ty}` is shown in a view; {who} reads `field` and the version's `fields`",
                    name.name
                ),
            );
            return Ex::error();
        }
        let side = Ex::new(Ek::I32(i32::from(name.name == "after")), Ty::Bool);
        let key = self.keyed(&format!(
            "{}.{}~{}{}",
            self.st.view_id, self.st.prefix, name.name, name.span.start
        ));
        Ex::host(Imp::ChangeView, vec![b, side, key], Ty::View)
    }

    /// `undo(v)` of a `host.Version`, or `undo(run)` of a `host.TaskRun`: the action that takes
    /// the step back, or every step of the run, through the platform's history (`undo`), as
    /// the person who applies it. Applied from a view, on a control: `button(undo(v))`.
    pub(super) fn builtin_undo(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        if self.st.ctx != super::Ctx::View {
            let who = self.st.ctx.word();
            self.err(
                "E067",
                span,
                format!("`undo(v)` is applied from a view, on a control; {who} cannot undo"),
            );
            return Ex::error();
        }
        let [a] = args else {
            self.err(
                "E022",
                span,
                "`undo` takes one version or task run: `undo(v)`, `undo(v.task)`",
            );
            return Ex::error();
        };
        if a.name.is_some() {
            self.err(
                "E022",
                a.value.span,
                "`undo` takes the version or run by position: `undo(v)`",
            );
        }
        let what = self.expr(&a.value, None);
        let (version, run) = (host_ty("Version"), host_ty("TaskRun"));
        let of = match &what.ty {
            Ty::Error => return Ex::error(),
            ty if *ty == version => 0,
            ty if *ty == run => 1,
            Ty::Option(inner) if **inner == run => {
                self.err(
                    "E020",
                    a.value.span,
                    "a version's `task` is `.none` for a change made outside a run: `if let run = v.task { button(undo(run)) }`",
                );
                return Ex::error();
            }
            other => {
                let t = self.show(other);
                self.err(
                    "E020",
                    a.value.span,
                    format!("`undo` takes a `host.Version` or a `host.TaskRun`; this is {t}"),
                );
                return Ex::error();
            }
        };
        let of = Ex::new(Ek::I32(of), Ty::Bool);
        Ex::host(Imp::ActUndo, vec![what, of], Ty::Action)
    }

    /// `e.label()`, `e.icon()`, `e.tone()` or `e.name()` of a value of a declared enum: what a
    /// control shows for its case, the name of its icon, the tone it reads in and the case's
    /// name.
    pub(super) fn enum_method(&mut self, r: Ex, key: &TypeKey, m: &str) -> Option<Ex> {
        self.env.enum_info(key)?;
        match m {
            "label" => Some(self.per_variant(r, key, Ty::Text, |v| Ex::str(&v.label))),
            "name" => Some(self.per_variant(r, key, Ty::Text, |v| Ex::str(&v.name))),
            "icon" => {
                let opt = Ty::Option(Box::new(Ty::Text));
                Some(self.per_variant(r, key, opt.clone(), |v| match &v.icon {
                    Some(icon) => Ex::host(Imp::SomeH, vec![Ex::str(icon)], opt.clone()),
                    None => Ex::host(Imp::NoneV, Vec::new(), opt.clone()),
                }))
            }
            // The tone a case reads in, as the tone case name a component's `tone:` prop
            // holds; a case that names none is neutral.
            "tone" => Some(self.per_variant(r, key, Ty::Text, |v| {
                Ex::str(v.tone.as_deref().unwrap_or("neutral"))
            })),
            _ => None,
        }
    }
}
