//! Code authorship: the code between a person and an action, as one more
//! party bounding them.
//!
//! A program a person (or their agent) runs acts with that person's
//! authority, and never with more than its authors may: each author in the
//! chain of code that led to an action is a delegation bounding the person
//! ([`crate::Authority::bounded_by`]), and the minimum wins. Code a person or
//! an agent wrote (a block in a conversation) is that person or agent; an
//! app's code is the person running it, and reaches as far as the kinds the
//! app declares ([`crate::Authority::in_app`]).
//!
//! What code may do on its own account depends on whether it is trusted
//! (an agent's code its person admitted): trusted code reads and makes
//! undoable changes and asks before reaching outside; code nobody trusts asks
//! before everything.

use crate::{Answer, Grant, Grantee, KindSet, Party, TreePath};

/// The grants code written by `author` holds in `workspace` as a party of its
/// own: [`trusted`](self) code reads and makes undoable changes and asks
/// before reaching outside, other code asks before everything.
#[must_use]
pub fn code_grants(workspace: &str, author: &Grantee, trusted: bool) -> Vec<Grant> {
    let grant = |suffix: &str, kinds: KindSet, answer: Answer| {
        Grant::new(
            format!("code-author:{}:{suffix}", grantee_key(author)),
            author.clone(),
            TreePath::workspace(workspace),
            kinds,
            answer,
            author.clone(),
        )
    };
    if trusted {
        vec![
            grant("content", KindSet::READ_UNDOABLE, Answer::Allow),
            grant("outside", KindSet::OUTSIDE, Answer::Ask),
        ]
    } else {
        vec![grant("untrusted", KindSet::ALL, Answer::Ask)]
    }
}

/// The party code written by `author` is, holding `own` (its
/// [`code_grants`]) and the grants of `workspace` naming it. The workspace's
/// denial of the author holds over every more specific grant.
#[must_use]
pub fn code_party<'g>(author: &Grantee, own: &'g [Grant], workspace: &'g [Grant]) -> Party<'g> {
    let named: Vec<&'g Grant> = workspace
        .iter()
        .filter(|grant| &grant.grantee == author)
        .collect();
    Party::new(vec![author.clone()], own)
        .with_grants(named.iter().copied())
        .held_off_by(named)
}

fn grantee_key(grantee: &Grantee) -> String {
    match grantee {
        Grantee::Member(crate::MemberId::Person(id)) => format!("person:{id}"),
        Grantee::Member(crate::MemberId::Agent(id)) => format!("agent:{id}"),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActionKind, Authority, Context};

    const WORKSPACE: &str = "workspace";

    fn node(app: &str) -> TreePath {
        TreePath::workspace(WORKSPACE)
            .app(app)
            .kind("k")
            .action("a")
    }

    fn answer(party: &Party<'_>, at: &TreePath, kind: ActionKind) -> Answer {
        Authority::default()
            .bounded_by(party.clone())
            .decide(at, kind, &Context::at(0))
            .parts[1]
            .answer
    }

    #[test]
    fn trusted_code_asks_only_before_reaching_outside() {
        let author = Grantee::agent("helper");
        let own = code_grants(WORKSPACE, &author, true);
        let party = code_party(&author, &own, &[]);
        assert_eq!(
            answer(&party, &node("x"), ActionKind::Undoable),
            Answer::Allow
        );
        assert_eq!(answer(&party, &node("x"), ActionKind::Outside), Answer::Ask);
    }

    #[test]
    fn untrusted_code_asks_for_everything() {
        let author = Grantee::agent("helper");
        let own = code_grants(WORKSPACE, &author, false);
        let party = code_party(&author, &own, &[]);
        for kind in [ActionKind::Undoable, ActionKind::Outside] {
            assert_eq!(answer(&party, &node("so.tilo.notes"), kind), Answer::Ask);
        }
    }

    #[test]
    fn the_workspaces_denial_of_an_author_holds() {
        let author = Grantee::agent("helper");
        let own = code_grants(WORKSPACE, &author, true);
        let workspace = [Grant::new(
            "deny-notes",
            author.clone(),
            TreePath::workspace(WORKSPACE),
            KindSet::ALL,
            Answer::Deny,
            Grantee::person("owner"),
        )];
        let party = code_party(&author, &own, &workspace);
        assert_eq!(
            answer(&party, &node("so.tilo.notes"), ActionKind::Read),
            Answer::Deny
        );
    }
}
