//! Code authorship: the code between a person and an action, as one more
//! party bounding them.
//!
//! A program a person (or their agent) runs acts with that person's
//! authority, and never with more than its authors may: each author in the
//! chain of code that led to an action is a delegation bounding the person
//! ([`crate::Authority::bounded_by`]), and the minimum wins. Code an
//! application's package wrote is that application ([`Grantee::app`]): the
//! workspace's grants naming the application bound it as they bound the
//! application anywhere, a denial of it holding over everything more
//! specific. Code a person or an agent wrote (a block in a conversation) is
//! that person or agent.
//!
//! What code may do on its own account depends on whether it is trusted
//! (an installed or built-in package, an agent's code its person admitted):
//! trusted code reads and makes undoable changes and asks before reaching
//! outside; code nobody trusts asks before everything. An application's own
//! code is an author of its own records ([`crate::roles::app_authorship`]),
//! trusted or not.

use crate::{Answer, Grant, Grantee, KindSet, Party, TreePath};

/// The grants code written by `author` holds in `workspace` as a party of its
/// own: [`trusted`](self) code reads and makes undoable changes and asks
/// before reaching outside, other code asks before everything.
#[must_use]
pub fn code_grants(workspace: &str, author: &Grantee, trusted: bool) -> Vec<Grant> {
    let grant = |suffix: &str, kinds: KindSet, answer: Answer| {
        Grant::new(
            format!("code-author:{}:{suffix}", subject_key(author)),
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
/// [`code_grants`], and an application's [`crate::roles::app_authorship`]
/// of its own records) and the grants of `workspace` naming it. The workspace's
/// denial of the author holds over every more specific grant.
#[must_use]
pub fn code_party<'g>(author: &Grantee, own: &'g [Grant], workspace: &'g [Grant]) -> Party<'g> {
    code_party_in(author, own, workspace, None)
}

/// [`code_party`], where the apps `earlier` names were renamed: the workspace's grants to an app under
/// an id it had before name it too.
#[must_use]
pub fn code_party_in<'g>(
    author: &Grantee,
    own: &'g [Grant],
    workspace: &'g [Grant],
    earlier: Option<&crate::EarlierIds>,
) -> Party<'g> {
    let author_alone = std::slice::from_ref(author);
    let named: Vec<&'g Grant> = workspace
        .iter()
        .filter(|grant| match earlier {
            Some(earlier) => earlier.subject_matches(&grant.subject, author_alone),
            None => {
                &grant.subject == author
                    || crate::earlier::with_held(|held| {
                        held.subject_matches(&grant.subject, author_alone)
                    })
                    .unwrap_or(false)
            }
        })
        .collect();
    Party::new(vec![author.clone()], own)
        .with_grants(named.iter().copied())
        .held_off_by(named)
}

fn subject_key(subject: &Grantee) -> String {
    match subject {
        Grantee::Member(crate::MemberId::Person(id)) => format!("person:{id}"),
        Grantee::Member(crate::MemberId::Agent(id)) => format!("agent:{id}"),
        Grantee::App(id) => format!("app:{id}"),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActionKind, Authority, Context, roles};

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
        let author = Grantee::app("so.tilo.notes");
        let own = code_grants(WORKSPACE, &author, true);
        let party = code_party(&author, &own, &[]);
        assert_eq!(
            answer(&party, &node("x"), ActionKind::Undoable),
            Answer::Allow
        );
        assert_eq!(answer(&party, &node("x"), ActionKind::Outside), Answer::Ask);
    }

    #[test]
    fn untrusted_code_asks_except_for_its_own_records() {
        let author = Grantee::app("so.tilo.notes");
        let mut own = code_grants(WORKSPACE, &author, false);
        own.push(roles::app_authorship(
            WORKSPACE,
            "so.tilo.notes",
            author.clone(),
        ));
        let party = code_party(&author, &own, &[]);
        assert_eq!(
            answer(&party, &node("so.tilo.other"), ActionKind::Undoable),
            Answer::Ask
        );
        assert_eq!(
            answer(&party, &node("so.tilo.notes"), ActionKind::Undoable),
            Answer::Allow
        );
        assert_eq!(
            answer(&party, &node("so.tilo.notes"), ActionKind::Outside),
            Answer::Ask
        );
    }

    #[test]
    fn the_workspaces_denial_of_an_author_holds() {
        let author = Grantee::app("so.tilo.notes");
        let mut own = code_grants(WORKSPACE, &author, true);
        own.push(roles::app_authorship(
            WORKSPACE,
            "so.tilo.notes",
            author.clone(),
        ));
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
