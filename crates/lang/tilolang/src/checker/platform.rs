//! What code reads of the platform: the actions offered on a resource, who else is at it, what it is
//! called, an enum's cases, and the platform's own phrases.

use super::{CKind, Checker, Ctx, Ex};
use crate::abi::Imp;
use crate::ast::{self, ExprKind};
use crate::diag::Span;
use crate::ir::{Ek, Stmt};
use crate::types::{Ty, TypeKey};

/// A value type of the host: `host.Action`.
pub(crate) fn host_ty(name: &str) -> Ty {
    Ty::Record(TypeKey {
        app: crate::host::HOST_APP.to_string(),
        name: name.to_string(),
    })
}

impl Checker<'_> {
    pub(super) fn resource_arg(&mut self, what: &str, args: &[ast::Arg], span: Span) -> Option<Ex> {
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
            // A union of record types is a resource too: whichever record it holds.
            Ty::Record(_) | Ty::Union(_) => Some(r),
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

    /// `actions(r)`: the actions offered on `r` to the person looking, each a `host.Action` with its
    /// label, icon, role, parameters and `run`. `actions(r, above)` offers them where the view shows
    /// `r` under `above`. Only the host draws them: a view that read them back would be reflecting
    /// over the declarations the host already places (E047).
    pub(super) fn builtin_actions(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        if self.st.ctx == Ctx::View {
            self.err(
                "E047",
                span,
                "a view never reads a resource's actions back to draw them: declare them on its \
                 type, place the affordances of the thing itself, and the host puts the rest in \
                 the window's bar and on the item's menu",
            );
            return Ex::error();
        }
        let Some(r) = self.resource_arg("actions", args.get(..1).unwrap_or_default(), span) else {
            return Ex::error();
        };
        let target = r.ty.clone();
        let above = match args.get(1) {
            Some(a) => {
                if a.name.is_some() {
                    self.err(
                        "E022",
                        a.value.span,
                        "`actions` takes the place it offers the item in by position: `actions(r, above)`",
                    );
                }
                let want = Ty::Option(Box::new(target));
                self.expr_as(&a.value, &want)
            }
            None => Ex::host(Imp::NoneV, Vec::new(), Ty::Option(Box::new(target))),
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

    /// `lenses(r)`: who has `r` open now, the person's own and everyone else's. `created(r)`,
    /// `updated(r)` and `history(r)`: the versions the log holds (who, when, by which action);
    /// `.none` while its log is not at hand.
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

    /// `changed(r, .field)`: the version that last changed the field of `r`, or the one that made
    /// `r` when none after it did. A store with an index answers it without reading any history.
    pub(super) fn builtin_changed(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let [r, f] = args else {
            self.err(
                "E022",
                span,
                "`changed` takes a resource and one of its fields: `changed(r, .field)`",
            );
            return Ex::error();
        };
        if r.name.is_some() || f.name.is_some() {
            self.err(
                "E022",
                span,
                "`changed` takes its arguments by position: `changed(r, .field)`",
            );
        }
        let r = self.expr(&r.value, None);
        let field = match &f.value.kind {
            ExprKind::Variant { name, args: None } => name.clone(),
            _ => {
                self.err(
                    "E022",
                    f.value.span,
                    "`changed` reads a field, written `.field`: `changed(r, .done)`",
                );
                return Ex::error();
            }
        };
        if r.ty.is_error() {
            return Ex::error();
        }
        let Ty::Record(key) = &r.ty else {
            let t = self.show(&r.ty);
            self.err(
                "E020",
                f.value.span,
                format!("`changed` takes a resource, a value of a record type; this is {t}"),
            );
            return Ex::error();
        };
        let env = self.env;
        let Some(info) = env.type_info(key) else {
            return Ex::error();
        };
        let Some(held) = info.field(&field.name) else {
            self.err(
                "E021",
                field.span,
                format!(
                    "`{}` has no field `{}`; fields are: {}",
                    key.name,
                    field.name,
                    info.field_names()
                ),
            );
            return Ex::error();
        };
        let id = held.id.clone();
        Ex::host(
            Imp::FieldChanged,
            vec![r, Ex::str(&id)],
            Ty::Option(Box::new(host_ty("Version"))),
        )
    }

    /// `linked(r)`: the resources linked to `r`, each a `host.Link` with how it links: made from it,
    /// naming it in a field, or mentioning it in its rich text.
    pub(super) fn builtin_links(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let Some(r) = self.resource_arg("linked", args, span) else {
            return Ex::error();
        };
        Ex::host(Imp::Links, vec![r], Ty::List(Box::new(host_ty("Link"))))
    }

    /// `earlier(r)`: what an earlier build of `r`'s app kept in documents of its own, each item a
    /// `host.Earlier`: what a `migration action` carries into the package's own resources. Only a
    /// migration reads it, since only a migration runs once over what that build left.
    pub(super) fn builtin_earlier(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        if !self.migrating {
            self.err(
                "E022",
                span,
                "`earlier(r)` reads what an earlier build of the app kept, which only a `migration action` carries over",
            );
        }
        let Some(r) = self.resource_arg("earlier", args, span) else {
            return Ex::error();
        };
        self.st.facts.earlier = true;
        Ex::host(
            Imp::Earlier,
            vec![r],
            Ty::List(Box::new(host_ty("Earlier"))),
        )
    }

    /// `proposal_note(r)`: what waits to be proposed of `r`, as one line of text; empty when nothing
    /// waits.
    pub(super) fn builtin_proposal_note(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let Some(r) = self.resource_arg("proposal_note", args, span) else {
            return Ex::error();
        };
        Ex::host(Imp::ProposalNote, vec![r], Ty::Text)
    }

    /// `steps(r)`: the history of `r` as people read it, newest first: consecutive versions by one
    /// person or one run folded into one `host.Step`, which says where it stands among the visits
    /// history reads.
    pub(super) fn builtin_steps(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let Some(r) = self.resource_arg("steps", args, span) else {
            return Ex::error();
        };
        Ex::host(Imp::Steps, vec![r], Ty::List(Box::new(host_ty("Step"))))
    }

    /// `unseen(r)`: the versions of `r` the person looking has not seen, oldest first: those someone
    /// else made after their seen mark and their own last change.
    pub(super) fn builtin_unseen(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let Some(r) = self.resource_arg("unseen", args, span) else {
            return Ex::error();
        };
        Ex::host(Imp::Unseen, vec![r], Ty::List(Box::new(host_ty("Version"))))
    }

    pub(super) fn builtin_changes(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let [a] = args else {
            self.err("E022", span, "`changes` takes one run: `changes(of: run)`");
            return Ex::error();
        };
        if a.name.as_ref().is_some_and(|n| n.name != "of") {
            self.err(
                "E022",
                a.value.span,
                "`changes` takes the run as `of`: `changes(of: run)`",
            );
        }
        let run_ty = host_ty("Run");
        let run = self.expr(&a.value, Some(&run_ty));
        match &run.ty {
            Ty::Error => return Ex::error(),
            ty if *ty == run_ty => {}
            other => {
                let t = self.show(other);
                self.err(
                    "E020",
                    a.value.span,
                    format!("`changes` takes a `host.Run` (a version's `run`); this is {t}"),
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

    /// `call_members(r)`: who is in the system's call held on `r` now, the person looking among
    /// them while this device is in it. It is the call service's presence, never stored: a view
    /// that read it runs again when someone joins, leaves, talks or mutes.
    pub(super) fn builtin_call_members(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let Some(r) = self.resource_arg("call_members", args, span) else {
            return Ex::error();
        };
        Ex::host(
            Imp::CallMembers,
            vec![r],
            Ty::List(Box::new(host_ty("CallMember"))),
        )
    }

    /// `call_inputs(r)`: the microphones and cameras this device's call held on `r` can send,
    /// each with whether it is one the call sends from now: what the person chooses between.
    pub(super) fn builtin_call_inputs(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let Some(r) = self.resource_arg("call_inputs", args, span) else {
            return Ex::error();
        };
        Ex::host(
            Imp::CallInputs,
            vec![r],
            Ty::List(Box::new(host_ty("CallInput"))),
        )
    }

    /// `select_call_input(r, id)` and `set_call_volume(r, member, percent)`: which microphone or
    /// camera this device sends from in the system's call held on `r`, and how loud it plays one of the
    /// call's members (a person, 0 to 200 percent of as they arrive, on this device only).
    pub(super) fn builtin_call(&mut self, name: &str, args: &[ast::Arg], span: Span) -> Ex {
        let switch = name.strip_suffix("_call").unwrap_or(name);
        let takes_switch = !matches!(switch, "join" | "leave");
        let usage = if takes_switch {
            format!("`{name}` takes the resource and whether it is on: `{name}(r, true)`")
        } else {
            format!("`{name}` takes one resource: `{name}(r)`")
        };
        let (target, on) = match (args, takes_switch) {
            ([r], false) => (r, None),
            ([r, on], true) => (r, Some(on)),
            _ => {
                self.err("E022", span, usage);
                for a in args {
                    let _ = self.expr(&a.value, None);
                }
                return Ex::error();
            }
        };
        if args.iter().any(|a| a.name.is_some()) {
            self.err("E022", span, usage);
        }
        let r = self.expr(&target.value, None);
        match &r.ty {
            Ty::Record(_) => {}
            Ty::Error => return Ex::error(),
            other => {
                let t = self.show(other);
                self.err(
                    "E020",
                    target.value.span,
                    format!("`{name}` takes a resource, a value of a record type; this is {t}"),
                );
                return Ex::error();
            }
        }
        let on = match on {
            Some(on) => self.expr_as(&on.value, &Ty::Bool),
            None => Ex::new(Ek::I32(i32::from(switch == "join")), Ty::Bool),
        };
        Ex::host(Imp::ActCall, vec![r, Ex::str(switch), on], Ty::Action)
    }

    pub(super) fn builtin_call_value(&mut self, name: &str, args: &[ast::Arg], span: Span) -> Ex {
        let resource = |this: &mut Self, args: &[ast::Arg], usage: &str| {
            let Some(first) = args.first() else {
                this.err("E022", span, usage);
                return None;
            };
            let r = this.expr(&first.value, None);
            match &r.ty {
                Ty::Record(_) => Some(r),
                Ty::Error => None,
                other => {
                    let t = this.show(other);
                    this.err(
                        "E020",
                        first.value.span,
                        format!("`{name}` takes a resource first; this is {t}"),
                    );
                    None
                }
            }
        };
        if name == "select_call_input" {
            let usage = "`select_call_input` takes the resource and the microphone's or camera's id: \
                         `select_call_input(r, input.id)`";
            if args.len() != 2 {
                self.err("E022", span, usage);
                for a in args {
                    let _ = self.expr(&a.value, None);
                }
                return Ex::error();
            }
            let Some(r) = resource(self, args, usage) else {
                return Ex::error();
            };
            let id = self.expr_as(&args[1].value, &Ty::Text);
            return Ex::host(Imp::ActCallInput, vec![r, id], Ty::Action);
        }
        let usage = "`set_call_volume` takes the resource, the member and a percent (0 to 200): \
                     `set_call_volume(r, m.who, 50)`";
        if args.len() != 3 {
            self.err("E022", span, usage);
            for a in args {
                let _ = self.expr(&a.value, None);
            }
            return Ex::error();
        }
        let Some(r) = resource(self, args, usage) else {
            return Ex::error();
        };
        let member = self.expr_as(&args[1].value, &Ty::Actor);
        let percent = self.expr_as(&args[2].value, &Ty::Float);
        Ex::host(Imp::ActCallVolume, vec![r, member, percent], Ty::Action)
    }

    /// `invoked(id)`: the invocation the system's dispatch knows by `id` (the address a field
    /// keeps, as a reply keeps the turn that wrote it), as its record reads now: `.none` until
    /// the record is read, and for an id that names nothing. It is the dispatch's record, never
    /// stored: a view that read it runs again as the invocation moves.
    pub(super) fn builtin_invoked(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let [a] = args else {
            self.err(
                "E022",
                span,
                "`invoked` takes the id of one invocation: `invoked(m.turn)`",
            );
            for a in args {
                let _ = self.expr(&a.value, None);
            }
            return Ex::error();
        };
        if a.name.is_some() {
            self.err(
                "E022",
                a.value.span,
                "`invoked` takes the id by position: `invoked(m.turn)`",
            );
        }
        let id = self.expr_as(&a.value, &Ty::Text);
        Ex::host(
            Imp::Invoked,
            vec![id],
            Ty::Option(Box::new(host_ty("Invoked"))),
        )
    }

    /// `lines_run(r)`: the tilolang lines the person ran from this window in the conversation
    /// `r` (`run_line`), oldest first, each with what it answered. They are this window's, never
    /// stored: what a line changed is in the records it changed, credited to the person.
    pub(super) fn builtin_lines_run(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let Some(r) = self.resource_arg("lines_run", args, span) else {
            return Ex::error();
        };
        Ex::host(
            Imp::LinesRun,
            vec![r],
            Ty::List(Box::new(host_ty("LineRun"))),
        )
    }

    /// `run_line(r, line)`: run the tilolang `line` as the person's own step, through the
    /// session's program host (`session:programs#run-line`), from the conversation `r`. Every call
    /// it makes is the person's, decided where it lands; what it answers reads in `lines_run(r)`.
    pub(super) fn builtin_run_line(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let [target, line] = args else {
            self.err(
                "E022",
                span,
                "`run_line` takes the conversation and the line: `run_line(c, line)`",
            );
            for a in args {
                let _ = self.expr(&a.value, None);
            }
            return Ex::error();
        };
        if args.iter().any(|a| a.name.is_some()) {
            self.err(
                "E022",
                span,
                "`run_line` takes its arguments by position: `run_line(c, line)`",
            );
        }
        let r = self.expr(&target.value, None);
        match &r.ty {
            Ty::Record(_) | Ty::Union(_) => {}
            Ty::Error => return Ex::error(),
            other => {
                let t = self.show(other);
                self.err(
                    "E020",
                    target.value.span,
                    format!("`run_line` runs a line from a resource, a value of a record type; this is {t}"),
                );
                return Ex::error();
            }
        }
        let line = self.expr_as(&line.value, &Ty::Text);
        Ex::host(Imp::ActRunLine, vec![r, line], Ty::Action)
    }

    /// The collection `c` holds and the type of its items, which a fact about a conversation reads.
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

    /// `first_unseen(c)`: the first item of `c`'s collection the person looking has not seen, or
    /// `.none`. Read from `c`'s versions and their seen mark, so a transcript draws its "new
    /// messages" divider without counting anything.
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

    /// `seen_by(c, m)`: the members whose reading of `c` ends at `m`.
    pub(super) fn builtin_seen_by(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let [c, item] = args else {
            self.err(
                "E022",
                span,
                "`seen_by` takes a conversation and one of its items: `seen_by(c, m)`",
            );
            return Ex::error();
        };
        if item.name.is_some() {
            self.err(
                "E022",
                item.value.span,
                "`seen_by` takes its arguments by position: `seen_by(c, m)`",
            );
        }
        let Some(c) = self.resource_arg("seen_by", std::slice::from_ref(c), span) else {
            return Ex::error();
        };
        let Some((_, item_ty)) = self.conversation_of("seen_by", &c, span) else {
            return Ex::error();
        };
        let item = self.expr_as(&item.value, &Ty::Record(item_ty));
        Ex::host(Imp::SeenBy, vec![c, item], Ty::List(Box::new(Ty::Actor)))
    }

    /// `mentions(c)`: how many of `c`'s items name the person looking that they have not seen, from
    /// the same versions and seen mark.
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

    /// `resource(id)`: the resource the store keeps under the id text, as `host.Resource`, whatever
    /// its type. `resource(id, of: alias.T)`: the same, as that package's type, for an id another
    /// app gave.
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

    /// `delivery(r)`: how the outside effect `r`'s actions queued last stands, or `.none` when they
    /// queued none.
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

    /// `reach(r)`: how far this device's own changes of `r` have gone, as a `host.Reach`: pending
    /// until another device that keeps the workspace has taken them in, then reached, or refused
    /// by the workspace. Of an item, the changes that made and edited it; of a whole record, any
    /// change of it. A view that read it runs again when it moves.
    pub(super) fn builtin_reach(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let Some(r) = self.resource_arg("reach", args, span) else {
            return Ex::error();
        };
        Ex::host(Imp::Reach, vec![r], host_ty("Reach"))
    }

    /// `members(r)`: the workspace's members who can read `r` now, as membership and grants decide.
    pub(super) fn builtin_members(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let Some(r) = self.resource_arg("members", args, span) else {
            return Ex::error();
        };
        Ex::host(Imp::Members, vec![r], Ty::List(Box::new(Ty::Actor)))
    }

    /// `std.may_read(who, r)`: whether the member `who` may read `r` now, as the workspace's
    /// grants decide a read of theirs. Someone who is no member reads nothing.
    pub(super) fn builtin_may_read(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let [who, r] = args else {
            self.err(
                "E022",
                span,
                "`std.may_read` takes a member and a resource: `std.may_read(who, r)`",
            );
            return Ex::error();
        };
        if let Some(name) = who.name.as_ref().or(r.name.as_ref()) {
            self.err(
                "E022",
                name.span,
                "`std.may_read` takes its arguments by position: `std.may_read(who, r)`",
            );
        }
        let who = self.expr_as(&who.value, &Ty::Actor);
        let r = self.expr_as(&r.value, &Ty::Record(super::resource_key()));
        Ex::host(Imp::MayRead, vec![who, r], Ty::Bool)
    }

    /// `std.occurrences(rule, start, timezone, exdates, from, to)`: the moments the series of a
    /// repeat's rule starts at, from the day `from` to the day `to`, its wall time read in
    /// `timezone` and the days `exdates` left out. Read from the rule alone, so a view may call it.
    pub(super) fn builtin_occurrences(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let some_left_out = "`std.occurrences` takes the rule, the start, the time zone, the days \
                             left out, and the window's days: `std.occurrences(rule, start, \
                             timezone, exdates, from, to)`";
        if args.len() != 6 {
            self.err("E022", span, some_left_out);
            for a in args {
                let _ = self.expr(&a.value, None);
            }
            return Ex::error();
        }
        for a in args {
            if let Some(name) = &a.name {
                self.err(
                    "E022",
                    name.span,
                    "`std.occurrences` takes its arguments by position: `std.occurrences(rule, \
                     start, timezone, exdates, from, to)`",
                );
            }
        }
        let wanted = [
            Ty::Text,
            Ty::Text,
            Ty::Text,
            Ty::Set(Box::new(Ty::Text)),
            Ty::Date,
            Ty::Date,
        ];
        let given: Vec<Ex> = args
            .iter()
            .zip(wanted)
            .map(|(a, ty)| self.expr_as(&a.value, &ty))
            .collect();
        Ex::host(Imp::Occurrences, given, Ty::List(Box::new(Ty::Time)))
    }

    /// Everyone in the workspace now, which a person picker and a rich input's `@` drawer offer:
    /// the `people` of the workspace's `members` root, read as the module reads it once it uses
    /// the workspace's package (`use tilo.workspace as workspace`, then `workspace.members`).
    pub(super) fn workspace_people(&mut self, span: Span) -> Ex {
        let app = crate::prims::WORKSPACE_APP;
        if !self.env.imports.iter().any(|import| import.app == app) {
            self.err(
                "E064",
                span,
                format!(
                    "a person picker offers the workspace's members, which a package reads by \
                     using the workspace's package: `use {app} as workspace`"
                ),
            );
            return Ex::error();
        }
        let at = |kind| ast::Expr { kind, span };
        let name = |name: &str| ast::Ident {
            name: name.to_owned(),
            span,
            from_template_of: None,
        };
        let mut read = at(ExprKind::Ident("tilo".to_owned()));
        for step in ["workspace", "members", "people"] {
            read = at(ExprKind::Field {
                base: Box::new(read),
                name: name(step),
            });
        }
        self.expr(&read, None)
    }

    /// `speakers(items)`: the people who spoke in the conversation of `items`, most recent first,
    /// each once, at most five (or `max`). A composer's `@` drawer offers them.
    pub(super) fn builtin_speakers(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let people = Ty::List(Box::new(Ty::Actor));
        let (items, max) = match args {
            [a] => (a, None),
            [a, m] => (a, Some(m)),
            _ => {
                self.err(
                    "E022",
                    span,
                    "`speakers` takes the items of a conversation, and at most how many people: \
                     `speakers(said)`",
                );
                return Ex::error();
            }
        };
        if items.name.is_some() || max.is_some_and(|m| m.name.is_some()) {
            self.err(
                "E022",
                span,
                "`speakers` takes its items by position: `speakers(said)`",
            );
        }
        let list = self.expr(&items.value, None);
        match &list.ty {
            Ty::List(t) | Ty::Collection(t) if matches!(**t, Ty::Record(_)) => {}
            Ty::Error => return Ex::error(),
            other => {
                let t = self.show(other);
                self.err(
                    "E020",
                    items.value.span,
                    format!(
                        "`speakers` takes the items of a conversation, resources of one record \
                         type; this is {t}"
                    ),
                );
                return Ex::error();
            }
        }
        let max = match max {
            Some(m) => self.expr_as(&m.value, &Ty::Int),
            None => Ex::new(Ek::Int(crate::checker::DRAWER_SPEAKERS), Ty::Int),
        };
        Ex::host(Imp::Speakers, vec![list, max], people)
    }

    /// Who a rich input's `@` drawer offers after the conversation's own: the members who can read
    /// what a mention names (`target`), or the workspace's members.
    pub(super) fn drawer_people(&mut self, target: Option<Ex>, span: Span) -> Ex {
        match target {
            Some(r) => Ex::host(Imp::Members, vec![r], Ty::List(Box::new(Ty::Actor))),
            None => self.workspace_people(span),
        }
    }

    /// `meta(r)` or `meta(T)`: a `host.Meta`: its title, its type's name, the noun a sentence calls
    /// it by, its app's icon, and whether it opens in a window of its own.
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

    /// `cases(E)`: every case of the enum `E`, in declaration order.
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

    fn entry_args(&mut self, what: &str, args: &[ast::Arg], span: Span) -> Option<(Ex, Ex)> {
        let [action, text] = args else {
            self.err(
                "E022",
                span,
                format!("`{what}` takes the action the entry runs and the text typed in it: `{what}(action, typed)`"),
            );
            return None;
        };
        if let Some(a) = [action, text].into_iter().find(|a| a.name.is_some()) {
            self.err(
                "E022",
                a.value.span,
                format!("`{what}` takes its action and text by position: `{what}(action, typed)`"),
            );
        }
        let action = self.expr_as(&action.value, &Ty::Action);
        let text = self.expr_as(&text.value, &Ty::Text);
        Some((action, text))
    }

    /// `entry_parts(action, text)`: what the text typed in an entry will make of the action it runs,
    /// as the words it takes out of the title and leaves to the action (`crate::entry`).
    pub(super) fn builtin_entry_parts(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let Some((action, text)) = self.entry_args("entry_parts", args, span) else {
            return Ex::error();
        };
        Ex::host(
            Imp::EntryParts,
            vec![action, text],
            Ty::List(Box::new(host_ty("EntryPart"))),
        )
    }

    /// `phrase("tilolang-empty")`: one of the platform's phrases, in the person's language, with its
    /// arguments.
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

    /// `v.is(edit)` of a `host.Version` or a `host.Action`: whether it names the declared action
    /// `edit`, named by the compiler, so a misspelt one is an error.
    pub(super) fn names_action(&mut self, r: Ex, ty: &str, args: &[ast::Arg], span: Span) -> Ex {
        let named = match args {
            [a] if a.name.is_none() => match &a.value.kind {
                ast::ExprKind::Ident(name) if self.lookup(name).is_none() => self
                    .own_callable(name)
                    .filter(|c| matches!(c.kind, CKind::Action))
                    .map(|c| c.id.clone()),
                ast::ExprKind::Field { base, name } if self.names_import(base) => {
                    super::expr::dotted(base).and_then(|path| {
                        let import = self.env.find_import(&path.join("."))?;
                        self.env
                            .foreign_callable_marked(
                                &import.app,
                                &name.name,
                                name.from_template_of.as_deref(),
                            )
                            .filter(|c| matches!(c.kind, CKind::Action))
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

    /// A field of a host value (`a.label` of a `host.Action`): read from the tuple it is held as.
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

    /// `c.before` or `c.after` of a `host.Change`: what the field held, shown as its type shows it,
    /// as a node of the view placed where the expression stands.
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

    /// `undo(v)` of a `host.Version`, or `undo(run)` of a `host.Run`: the action that takes the step
    /// back, or every step of the run, through the platform's history.
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
                "`undo` takes one version or run: `undo(v)`, `undo(v.run)`",
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
        let (version, run) = (host_ty("Version"), host_ty("Run"));
        let of = match &what.ty {
            Ty::Error => return Ex::error(),
            ty if *ty == version => 0,
            ty if *ty == run => 1,
            Ty::Option(inner) if **inner == run => {
                self.err(
                    "E020",
                    a.value.span,
                    "a version's `run` is `.none` for a change made outside a run: `if let run = v.run { button(undo(run)) }`",
                );
                return Ex::error();
            }
            other => {
                let t = self.show(other);
                self.err(
                    "E020",
                    a.value.span,
                    format!("`undo` takes a `host.Version` or a `host.Run`; this is {t}"),
                );
                return Ex::error();
            }
        };
        let of = Ex::new(Ek::I32(of), Ty::Bool);
        Ex::host(Imp::ActUndo, vec![what, of], Ty::Action)
    }

    /// `e.label()`, `e.icon()`, `e.tone()` or `e.name()` of a declared enum: what a control shows for
    /// its case.
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
            "tone" => Some(self.per_variant(r, key, Ty::Text, |v| {
                Ex::str(v.tone.as_deref().unwrap_or("neutral"))
            })),
            _ => None,
        }
    }
}
