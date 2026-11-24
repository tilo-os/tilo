//! What a declaration says it reads: the `uses` clause of an action, checked and turned into the effects it
//! names ([`Effect`]).

use std::collections::BTreeSet;

use crate::ast::Item;
use crate::effects::Effect;

use super::Resolver;

pub(super) fn declared(names: &[String]) -> BTreeSet<Effect> {
    names.iter().filter_map(|n| Effect::parse(n)).collect()
}

impl Resolver<'_> {
    /// The effects a declaration's `uses` clause names, each once, checked: an action declares the ones its
    /// body reads ([`Effect`]); a view and a `fn` read none and take no clause.
    pub(super) fn uses_of(&mut self, item: &Item) -> BTreeSet<Effect> {
        let written = match item {
            Item::Action(a) => &a.uses,
            // Seed data runs where a developer runs it, and is never replayed.
            Item::Scenario(_) => return Effect::ALL.iter().copied().collect(),
            _ => return BTreeSet::new(),
        };
        let mut out = BTreeSet::new();
        for u in written {
            match Effect::parse(&u.name) {
                Some(e) if out.insert(e) => {}
                Some(_) => self.err("E121", u.span, format!("`{}` is in `uses` twice", u.name)),
                None => {
                    let effects: Vec<&str> = Effect::ALL.iter().map(|e| e.word()).collect();
                    self.err(
                        "E121",
                        u.span,
                        format!(
                            "`{}` is not an effect; `uses` names what a replay cannot compute again: {}",
                            u.name,
                            effects.join(", ")
                        ),
                    );
                    out.extend(Effect::ALL.iter().copied());
                }
            }
        }
        if let Item::Action(a) = item
            && out.contains(&Effect::Check)
            && !takes_a_check_id(a)
            && let Some(u) = written.iter().find(|u| u.name == Effect::Check.word())
        {
            self.err(
                "E121",
                u.span,
                format!(
                    "`{}` runs a check the project's host approved, so it is the app's \
                     `run-approved-check`: it takes the resource it runs on and the check's id, \
                     and nothing else: `action {}(r: Repository, check_id: Text) uses check`",
                    a.name.name, a.name.name
                ),
            );
        }
        out
    }
}

/// Whether `a` takes what the workspace's `run-approved-check` takes: the resource it runs on,
/// then the check's id as text, which the project's host reads by that name.
fn takes_a_check_id(a: &crate::ast::ActionDecl) -> bool {
    let [_, id] = a.params.as_slice() else {
        return false;
    };
    id.name.name == "check_id"
        && id.default.is_none()
        && matches!(&id.ty.kind, crate::ast::TypeExprKind::Named(path)
            if path.len() == 1 && path.first().is_some_and(|ty| ty.name == "Text"))
}
