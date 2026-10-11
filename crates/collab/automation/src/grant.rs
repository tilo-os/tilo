//! The authority a shortcut is allowed to exercise.
//!
//! A shortcut's grant is a set of rules over the published tree, the same
//! `(node, kinds, answer)` a person grants to a collaborator or an agent. It
//! bounds the shortcut: the host still decides, from the invoking person's
//! own grants, whether that person may take the action at all. The effective
//! answer is the stricter of the two.

use tilo_action_policy::{
    ActionKind, Answer, Context, Grant, GrantId, Level, Lifetime, Rule, Segment, Subject, TreePath,
    decide,
};
use tilo_actions::door::ActionAddress;

/// The node a step's action has in the tree: the room the run acts in (or
/// any room, for a run confined to none), the app, the resource kind, the
/// resource and the action.
#[must_use]
pub fn step_node(
    room: Option<&str>,
    app_id: &str,
    resource_kind: &str,
    address: &ActionAddress,
) -> TreePath {
    let base = match room {
        Some(room) => TreePath::room(room),
        None => TreePath::root().any(Level::Room),
    };
    base.app(app_id)
        .kind(resource_kind)
        .resource(address.resource.clone())
        .action(address.action.clone())
}

/// A rule naming one action on every room's matching resource, as a
/// shortcut's grant usually does.
#[must_use]
pub fn action_rule(
    app_id: &str,
    resource_kind: &str,
    resource: Option<&str>,
    action: &str,
    kinds: tilo_action_policy::KindSet,
    answer: Answer,
) -> Rule {
    let node = TreePath::root()
        .any(Level::Room)
        .app(app_id)
        .kind(resource_kind);
    let node = match resource {
        Some(resource) => node.resource(resource),
        None => node.any(Level::Resource),
    };
    Rule::new(node.action(action), kinds, answer)
}

/// The confirmations a grant gives in advance: each allowing rule that
/// names one action, as (resource, action) — the resource `*` where the
/// rule covers every resource of its kind.
#[must_use]
pub fn advance_consents(grant: &[Rule]) -> Vec<(String, String)> {
    grant
        .iter()
        .filter(|rule| rule.answer == Answer::Allow)
        .filter_map(|rule| {
            let segments = rule.node.segments();
            let Some(Segment::Name(Level::Action, action)) = segments.last() else {
                return None;
            };
            let resource = segments.iter().rev().find_map(|segment| match segment {
                Segment::Name(Level::Resource, name) => Some(name.clone()),
                Segment::Any(Level::Resource) => Some("*".to_owned()),
                _ => None,
            })?;
            Some((resource, action.clone()))
        })
        .collect()
}

/// What a shortcut's grant answers for one step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepGrant {
    pub answer: Answer,
    /// The deciding rule names this very action (not an app or a kind of
    /// resource above it): an advance consent an unattended run may use in
    /// place of a person's confirmation.
    pub names_action: bool,
}

/// What `grant` answers for the action at `node` of `kind`.
#[must_use]
pub fn grant_answer(grant: &[Rule], node: &TreePath, kind: ActionKind) -> StepGrant {
    let subject = Subject::Shortcut(String::new());
    let grants: Vec<Grant> = grant
        .iter()
        .enumerate()
        .map(|(index, rule)| Grant {
            id: GrantId::new(index.to_string()),
            subject: subject.clone(),
            node: rule.node.clone(),
            kinds: rule.kinds,
            answer: rule.answer,
            lifetime: Lifetime::UntilRevoked,
            granted_by: subject.clone(),
        })
        .collect();
    let decision = decide(&grants, &[subject], node, kind, &Context::at(0));
    StepGrant {
        answer: decision.answer,
        names_action: decision.grant().is_some_and(|grant| {
            matches!(
                grant.node.segments().last(),
                Some(Segment::Name(Level::Action, _))
            )
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_action_policy::KindSet;

    const NOTES: &str = "so.tilo.notes";

    fn answer(grant: &[Rule], resource: &str, kind: &str, action: &str) -> Answer {
        let node = step_node(
            Some("room"),
            NOTES,
            kind,
            &ActionAddress::new(resource, action),
        );
        grant_answer(grant, &node, ActionKind::Undoable).answer
    }

    #[test]
    fn a_shortcut_grant_reaches_its_resource_or_exactly_its_kind() {
        // One resource's action: not the same action on another resource.
        let exact = [action_rule(
            NOTES,
            "tilo.rich-text",
            Some("notes.notebook/page/1/body"),
            "apply",
            KindSet::ALL,
            Answer::Allow,
        )];
        assert_eq!(
            answer(
                &exact,
                "notes.notebook/page/1/body",
                "tilo.rich-text",
                "apply"
            ),
            Answer::Allow
        );
        assert_eq!(
            answer(&exact, "notes.notebook", "notes.notebook", "apply"),
            Answer::Deny
        );
        assert_eq!(
            answer(
                &exact,
                "notes.notebook/page/2/body",
                "tilo.rich-text",
                "apply"
            ),
            Answer::Deny
        );

        // Every resource of one kind: that kind exactly, in that app, for that
        // action. Neither a sub-kind nor a kind sharing a prefix.
        let kind = [action_rule(
            NOTES,
            "notes.page",
            None,
            "apply",
            KindSet::ALL,
            Answer::Allow,
        )];
        assert_eq!(
            answer(&kind, "notes.page:1", "notes.page", "apply"),
            Answer::Allow
        );
        assert_eq!(
            answer(&kind, "notes.page:2", "notes.page", "apply"),
            Answer::Allow
        );
        assert_eq!(
            answer(&kind, "notes.page:1/x", "notes.page.special", "apply"),
            Answer::Deny
        );
        assert_eq!(
            answer(&kind, "notes.pages:1", "notes.pages", "apply"),
            Answer::Deny
        );
        assert_eq!(
            answer(&kind, "notes.page:1", "notes.page", "delete"),
            Answer::Deny
        );
        let chat = step_node(
            Some("room"),
            "so.tilo.chat",
            "notes.page",
            &ActionAddress::new("notes.page:1", "apply"),
        );
        assert_eq!(
            grant_answer(&kind, &chat, ActionKind::Undoable).answer,
            Answer::Deny
        );

        // A narrower deny on the kind's undoable changes caps it.
        let mut restricted = kind.to_vec();
        restricted.push(action_rule(
            NOTES,
            "notes.page",
            Some("notes.page:1"),
            "apply",
            KindSet::UNDOABLE,
            Answer::Deny,
        ));
        assert_eq!(
            answer(&restricted, "notes.page:1", "notes.page", "apply"),
            Answer::Deny
        );
        assert_eq!(
            answer(&restricted, "notes.page:2", "notes.page", "apply"),
            Answer::Allow
        );
    }

    #[test]
    fn only_a_rule_naming_the_action_is_an_advance_consent() {
        let node = step_node(
            None,
            NOTES,
            "notes.note",
            &ActionAddress::new("notes.note:1", "delete"),
        );
        let app = [Rule::new(
            TreePath::root().any(Level::Room).app(NOTES),
            KindSet::ALL,
            Answer::Allow,
        )];
        let decided = grant_answer(&app, &node, ActionKind::Undoable);
        assert_eq!(decided.answer, Answer::Allow);
        assert!(!decided.names_action);
        let consent = [action_rule(
            NOTES,
            "notes.note",
            Some("notes.note:1"),
            "delete",
            KindSet::ALL,
            Answer::Allow,
        )];
        assert!(grant_answer(&consent, &node, ActionKind::Undoable).names_action);
    }
}
