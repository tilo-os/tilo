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
    pub(super) fn builtin_actions(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let Some(r) = self.resource_arg("actions", args, span) else {
            return Ex::error();
        };
        Ex::host(Imp::Actions, vec![r], Ty::List(Box::new(host_ty("Action"))))
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
    pub(super) fn workspace_field(&mut self, name: &ast::Ident) -> Ex {
        if name.name == "members" {
            return Ex::host(
                Imp::WorkspaceMembers,
                Vec::new(),
                Ty::List(Box::new(Ty::Principal)),
            );
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
        let pair = Ty::Tuple(vec![Ty::Text, Ty::Text]);
        let list_ty = Ty::List(Box::new(pair.clone()));
        let list = self.fresh(list_ty.clone());
        let mut stmts = vec![Stmt::Set(
            list,
            Ex::host(Imp::ListNew, Vec::new(), list_ty.clone()),
        )];
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
            let t = self.fresh(pair.clone());
            stmts.push(Stmt::Set(
                t,
                Ex::host(
                    Imp::TupleNew,
                    vec![Ex::new(Ek::I32(2), Ty::Bool)],
                    pair.clone(),
                ),
            ));
            for (index, item) in [Ex::str(&name.name), v].into_iter().enumerate() {
                stmts.push(Stmt::Eval(Ex::host(
                    Imp::TupleSetH,
                    vec![
                        Ex::local(t, pair.clone()),
                        Ex::new(Ek::I32(i32::try_from(index).unwrap_or(0)), Ty::Bool),
                        item,
                    ],
                    Ty::Unit,
                )));
            }
            stmts.push(Stmt::Eval(Ex::host(
                Imp::ListPushH,
                vec![Ex::local(list, list_ty.clone()), Ex::local(t, pair.clone())],
                Ty::Unit,
            )));
        }
        let call = Ex::host(Imp::Phrase, vec![id, Ex::local(list, list_ty)], Ty::Text);
        Ex::new(Ek::Seq(stmts, Box::new(call)), Ty::Text)
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
        let at = Ex::new(Ek::I32(i32::try_from(index).unwrap_or(0)), Ty::Bool);
        let (imp, c) = super::load_op(&ty, Imp::TupleGetI, Imp::TupleGetF, Imp::TupleGetH);
        let raw = if c.is_some() { Ty::Int } else { ty.clone() };
        Some(super::conv(c, Ex::host(imp, vec![b, at], raw), ty))
    }

    /// `e.label()`, `e.icon()` or `e.name()` of a value of a declared enum: what a control shows
    /// for its case, the name of its icon, and the case's name.
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
            _ => None,
        }
    }
}
