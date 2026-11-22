//! Code authorship: the code between a person and an action, as one more
//! party bounding them.
//!
//! A program a person (or their agent) runs acts with that person's
//! authority, and never with more than its authors may: each author in the
//! chain of code that led to an action is a delegation bounding the person
//! ([`crate::Authority::bounded_by`]), and the minimum wins. Code a person or
//! an agent wrote (a block in a conversation) is that person or agent; an
//! app's code is the person running it.
//!
//! What code may do on its own account depends on whether it is trusted
//! (an installed or built-in package, an agent's code its person admitted):
//! trusted code reads and makes
//! undoable changes and asks before reaching outside; code nobody trusts asks
//! before everything. A package reaches the outside services the workspace
//! granted its app ([`service_grant`]) through its own actions without asking.

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

/// The workspace's grant letting `app`'s own code reach the outside service `service`
/// (`youtube`) through the app's own actions, given by `granted_by`: once given, every member's
/// use of the app reaches that service without asking, as far as each member may change the
/// workspace. It names the app's code, never a person, so it lets nobody do anything else, and
/// another app's actions, or another service, still ask. Revoking it ends it at once.
#[must_use]
pub fn service_grant(workspace: &str, app: &str, service: &str, granted_by: Grantee) -> Grant {
    Grant::new(
        format!("app-service:{app}:{service}"),
        Grantee::App(app.to_owned()),
        TreePath::workspace(workspace).app(app.to_owned()),
        KindSet::OUTSIDE,
        Answer::Allow,
        granted_by,
    )
    .for_services([service])
}

/// Whether `grant` is a grant of an outside service to an app's code ([`service_grant`]).
#[must_use]
pub fn is_service_grant(grant: &Grant) -> bool {
    matches!(grant.grantee, Grantee::App(_)) && !grant.services.is_empty()
}

/// The party a package's code is when `runner` runs `app`'s package: what it holds of its own
/// (`own`, its [`code_grants`]), the workspace's grants to the runner, and the outside services
/// the workspace granted the app ([`service_grant`]). The workspace's denial of the runner holds
/// over every more specific grant.
#[must_use]
pub fn package_party<'g>(
    runner: &Grantee,
    app: &str,
    own: &'g [Grant],
    workspace: &'g [Grant],
) -> Party<'g> {
    let app_grantee = Grantee::App(app.to_owned());
    let services = workspace
        .iter()
        .filter(|grant| grant.grantee == app_grantee && is_service_grant(grant));
    code_party(runner, own, workspace)
        .also_as(app_grantee.clone())
        .with_grants(services)
}

fn grantee_key(grantee: &Grantee) -> String {
    match grantee {
        Grantee::Member(crate::MemberId::Person(id)) => id.as_str().to_owned(),
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

    tilo_test_env::scenario! {
        fn authored_code_asks_by_how_far_it_is_trusted;

        phase trusted_code_asks_only_before_reaching_outside;
        {
            let author = Grantee::agent("helper");
            let own = code_grants(WORKSPACE, &author, true);
            let party = code_party(&author, &own, &[]);
            assert_eq!(
                answer(&party, &node("x"), ActionKind::Undoable),
                Answer::Allow
            );
            assert_eq!(answer(&party, &node("x"), ActionKind::Outside), Answer::Ask);
        }

        phase untrusted_code_asks_for_everything;
        {
            let author = Grantee::agent("helper");
            let own = code_grants(WORKSPACE, &author, false);
            let party = code_party(&author, &own, &[]);
            for kind in [ActionKind::Undoable, ActionKind::Outside] {
                assert_eq!(answer(&party, &node("so.tilo.notes"), kind), Answer::Ask);
            }
        }

        phase the_workspaces_denial_of_an_author_holds;
        {
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

        phase a_package_reaches_only_the_services_its_app_was_granted;
        {
            const TUBE: &str = "so.tilo.tube";
            let runner = Grantee::person("ada");
            let own = code_grants(WORKSPACE, &runner, true);
            let granted = [service_grant(WORKSPACE, TUBE, "youtube", Grantee::person("owner"))];
            let reaching = |workspace: &[Grant], app: &str, services: &[&str]| {
                let services: Vec<String> = services.iter().map(|s| (*s).to_owned()).collect();
                let party = package_party(&runner, TUBE, &own, workspace);
                Authority::default()
                    .bounded_by(party)
                    .decide(
                        &node(app),
                        ActionKind::Outside,
                        &Context::at(0).with_services(&services),
                    )
                    .parts[1]
                    .answer
            };
            assert_eq!(reaching(&granted, TUBE, &["youtube"]), Answer::Allow);
            assert_eq!(
                reaching(&[], TUBE, &["youtube"]),
                Answer::Ask,
                "without the workspace's grant it asks"
            );
            assert_eq!(
                reaching(&granted, TUBE, &["youtube", "imap"]),
                Answer::Ask,
                "a service nobody granted asks, alongside one granted too"
            );
            assert_eq!(
                reaching(&granted, TUBE, &[]),
                Answer::Ask,
                "an outside effect that names no service is not the granted one"
            );
            assert_eq!(
                reaching(&granted, "so.tilo.mail", &["youtube"]),
                Answer::Ask,
                "another app's action is not the app's own"
            );
        }
    }
}
