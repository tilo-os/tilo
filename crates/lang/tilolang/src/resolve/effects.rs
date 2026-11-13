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
        out
    }
}
