//! The decision rules, over generated grant sets rather than one arrangement
//! each: the most specific node wins, the stricter answer wins at one node,
//! and someone acting for a person never gets past what the person holds.

use proptest::prelude::*;
use tilo_action_policy::{
    ActionKind, Answer, Context, Grant, KindSet, Level, Lifetime, Subject, TreePath, decide,
    decide_delegated, decide_subject, explain,
};

const ALICE: &str = "a11ce";
const AGENT: &str = "ag";

fn alice() -> Subject {
    Subject::person(ALICE)
}

fn cx() -> Context<'static> {
    Context::at(1_000_000)
}

/// A room, then each level named (`a0`), open (`*`) or any depth (`**`), in
/// tree order: a node of the tree, or a pattern that covers some node.
fn node() -> impl Strategy<Value = TreePath> {
    proptest::collection::vec(
        prop_oneof![
            Just(0u8), // the level's own name: a0, app0, ...
            Just(1),   // every one of that level
            Just(2),   // any depth
        ],
        1..6,
    )
    .prop_map(|levels| {
        let mut path = TreePath::room("r");
        // Past the room the path starts at.
        let mut at = 1usize;
        for shape in levels {
            let Some(level) = Level::ALL.get(at) else {
                break;
            };
            at = at.saturating_add(1);
            path = match shape {
                1 => path.any(*level),
                2 => path.rest(),
                _ => path.push(*level, format!("{}{}", level.tag(), at.saturating_sub(1))),
            };
        }
        path
    })
}

fn answer() -> impl Strategy<Value = Answer> {
    prop_oneof![Just(Answer::Deny), Just(Answer::Ask), Just(Answer::Allow)]
}

fn kinds() -> impl Strategy<Value = KindSet> {
    (any::<bool>(), any::<bool>(), any::<bool>()).prop_map(|(read, undoable, outside)| {
        [
            (read, ActionKind::Read),
            (undoable, ActionKind::Undoable),
            (outside, ActionKind::Outside),
        ]
        .into_iter()
        .filter_map(|(wanted, kind)| wanted.then_some(kind))
        .collect()
    })
}

fn lifetime() -> impl Strategy<Value = Lifetime> {
    prop_oneof![
        Just(Lifetime::UntilRevoked),
        Just(Lifetime::Once),
        // Before and after the context's clock.
        (0u64..2_000_000).prop_map(Lifetime::Until),
    ]
}

/// A grant set of one subject, `subject`, plus a node to decide and a kind.
fn grants_of(
    subject: Subject,
) -> impl Strategy<Value = (Vec<Grant>, TreePath, ActionKind, Context<'static>)> {
    (
        proptest::collection::vec((node(), kinds(), answer(), lifetime()), 0..6),
        node(),
        prop_oneof![
            Just(ActionKind::Read),
            Just(ActionKind::Undoable),
            Just(ActionKind::Outside)
        ],
    )
        .prop_map(move |(specs, node, kind)| {
            let grants = specs
                .into_iter()
                .enumerate()
                .map(|(index, (at, kinds, answer, lifetime))| {
                    Grant::new(
                        format!("g{index}"),
                        subject.clone(),
                        at,
                        kinds,
                        answer,
                        Subject::person("ca401"),
                    )
                    .with_lifetime(lifetime)
                })
                .collect();
            (grants, node, kind, cx())
        })
}

#[test]
fn the_generated_nodes_cover_themselves_every_time() {
    // The properties below lean on every grant matching at least the node it
    // names; a generator that produced a node its own pattern misses would
    // make them weak rather than wrong.
    proptest!(|(node in node())| {
        prop_assert!(node.covers(&node), "{node} does not cover itself");
    });
}

#[test]
fn the_generated_kinds_include_what_they_name() {
    proptest!(|(kinds in kinds())| {
        for kind in kinds.iter() {
            prop_assert!(kinds.contains(kind));
        }
    });
}

/// A grant for one use.
fn once(node: &TreePath) -> Grant {
    Grant::new(
        "once",
        alice(),
        node.clone(),
        KindSet::ALL,
        Answer::Allow,
        alice(),
    )
    .with_lifetime(Lifetime::Once)
}

/// An action below `node`.
fn under(node: &TreePath) -> TreePath {
    node.clone().push(Level::Action, "run")
}

/// A grant the room's owner gives the person.
fn alice_grant(node: TreePath, kinds: KindSet, answer: Answer) -> Grant {
    Grant::new(
        "person",
        alice(),
        node,
        kinds,
        answer,
        Subject::person("ca401"),
    )
}

/// The stricter of two answers: the person's answer is the ceiling a
/// delegation cannot pass.
fn stricter(one: Answer, other: Answer) -> Answer {
    one.min(other)
}

/// What the rules say, read off the definition rather than off [`decide`]:
/// among the live covering grants of the kind, the most specific node's
/// strictest answer; no such grant denies.
fn expected(grants: &[Grant], subjects: &[Subject], node: &TreePath, kind: ActionKind) -> Answer {
    let covering: Vec<&Grant> = grants
        .iter()
        .filter(|grant| {
            grant.is_live(&cx())
                && grant.kinds.contains(kind)
                && subjects.contains(&grant.subject)
                && grant.node.covers(node)
        })
        .collect();
    let Some(most) = covering.iter().map(|grant| grant.node.specificity()).max() else {
        return Answer::Deny;
    };
    covering
        .iter()
        .filter(|grant| grant.node.specificity() == most)
        .map(|grant| grant.answer)
        .min()
        .unwrap_or(Answer::Deny)
}

proptest! {
    #[test]
    fn decided_answers_follow_the_rules(mut set in grants_of(alice())) {
        let (grants, node, kind) = (std::mem::take(&mut set.0), set.1.clone(), set.2);
        let decision = decide(&grants, &[alice()], &node, kind, &set.3);
        prop_assert_eq!(decision.answer, expected(&grants, &[alice()], &node, kind));
        // The deciding grant is one the rules allow to decide, and covers
        // what was asked.
        if let Some(grant) = decision.grant() {
            prop_assert!(grant.node.covers(&node));
            prop_assert!(grant.kinds.contains(kind));
            prop_assert_eq!(grant.answer, decision.answer);
        } else {
            prop_assert_eq!(decision.answer, Answer::Deny);
        }
        // And it says its answer either way.
        let opening = match decision.answer {
            Answer::Allow => "Allowed",
            Answer::Ask => "Asks first",
            Answer::Deny => "Not allowed",
        };
        let why = explain(&decision, &node, kind);
        prop_assert!(why.starts_with(opening), "{why}");
    }

    #[test]
    fn a_narrower_grant_never_loses_to_a_wider_one(
        wider_answer in answer(),
        narrower_answer in answer(),
        open in any::<bool>(),
    ) {
        // Every grant covers the node, so node and kind never let a grant
        // through: the only question is which of overlapping nodes wins.
        let node = TreePath::room("r").app("mail").kind("mail.message").resource("m").action("send");
        let wider = TreePath::room("r").app("mail");
        let narrower = if open {
            wider.clone().any(Level::Kind)
        } else {
            wider.clone().kind("mail.message")
        };
        let grants = [
            Grant::new("wider", alice(), wider, KindSet::ALL, wider_answer, alice()),
            Grant::new("narrower", alice(), narrower, KindSet::ALL, narrower_answer, alice()),
        ];
        let decision = decide(&grants, &[alice()], &node, ActionKind::Outside, &cx());
        // A name beats an open level, and either beats the wider node.
        prop_assert_eq!(decision.answer, narrower_answer);
        prop_assert_eq!(decision.grant().map(|grant| grant.id.as_str()), Some("narrower"));

        // With the narrower one gone, the wider one decides on its own node.
        let alone = [grants[0].clone()];
        prop_assert_eq!(
            decide(&alone, &[alice()], &node, ActionKind::Outside, &cx()).answer,
            wider_answer
        );
    }

    #[test]
    fn at_one_node_deny_beats_ask_beats_allow(
        answers in proptest::collection::vec(answer(), 1..6),
    ) {
        let node = TreePath::room("r").app("mail");
        let action = node.clone().kind("mail.message").action("archive");
        let grants: Vec<Grant> = answers
            .iter()
            .enumerate()
            .map(|(index, answer)| {
                Grant::new(
                    format!("g{index}"),
                    alice(),
                    node.clone(),
                    KindSet::ALL,
                    *answer,
                    alice(),
                )
            })
            .collect();
        let expected = answers.iter().copied().min().unwrap_or(Answer::Deny);
        prop_assert_eq!(
            decide(&grants, &[alice()], &action, ActionKind::Undoable, &cx()).answer,
            expected
        );
    }

    #[test]
    fn no_grant_set_answers_more_than_its_grants(mut set in grants_of(alice())) {
        let (grants, node, kind) = (std::mem::take(&mut set.0), set.1.clone(), set.2);
        let decision = decide(&grants, &[alice()], &node, kind, &set.3);
        // Allowed or asked means a grant decided it; a deny may be one too,
        // since a grant can refuse.
        if decision.answer != Answer::Deny {
            prop_assert!(decision.grant().is_some());
        }
        // Another grant on the very node decided never lifts the answer: at
        // one node the strictest of the grants decides.
        if let Some(deciding) = decision.grant() {
            for extra in [Answer::Allow, Answer::Ask, Answer::Deny] {
                let mut more = grants.clone();
                let widest = Grant::new(
                    "extra",
                    alice(),
                    deciding.node.clone(),
                    KindSet::ALL,
                    extra,
                    alice(),
                );
                more.push(widest);
                let after = decide(&more, &[alice()], &node, kind, &cx());
                prop_assert!(after.answer <= decision.answer, "{after:?} lifts {decision:?}");
            }
        }
    }

    #[test]
    fn a_kind_a_grant_does_not_name_is_not_covered(mut set in grants_of(alice())) {
        let (grants, node, _) = (std::mem::take(&mut set.0), set.1.clone(), set.2);
        for kind in ActionKind::ALL {
            let decision = decide(&grants, &[alice()], &node, kind, &cx());
            if let Some(grant) = decision.grant() {
                prop_assert!(grant.kinds.contains(kind));
            }
            // Reads of a grant set follow the same rule for every kind.
            prop_assert_eq!(decision.answer, expected(&grants, &[alice()], &node, kind));
        }
    }

    #[test]
    fn grants_of_another_subject_decide_nothing(mut set in grants_of(Subject::agent(AGENT))) {
        let (grants, node, kind) = (std::mem::take(&mut set.0), set.1.clone(), set.2);
        let decision = decide_subject(&grants, &alice(), &node, kind, &set.3);
        prop_assert_eq!(decision.answer, Answer::Deny);
        prop_assert!(decision.grant().is_none());
    }

    #[test]
    fn delegation_never_exceeds_the_person(
        person in grants_of(alice()),
        delegation in grants_of(Subject::agent(AGENT)),
    ) {
        let mut grants = person.0.clone();
        grants.extend(delegation.0.iter().cloned());
        let node = person.1.clone();
        let kind = person.2;
        let decision = decide_delegated(&grants, &alice(), &Subject::agent(AGENT), &node, kind, &cx());
        let own = decide_subject(&grants, &alice(), &node, kind, &cx()).answer;
        let delegate = decide_subject(&grants, &Subject::agent(AGENT), &node, kind, &cx()).answer;
        prop_assert_eq!(decision.answer, stricter(own, delegate));
        prop_assert!(decision.answer <= own);
        prop_assert!(decision.answer <= delegate);
        // What the person may not do, neither may their agent.
        if own == Answer::Deny {
            prop_assert_eq!(decision.answer, Answer::Deny);
        }
        // A delegation allowed somewhere the person is refused never lifts
        // the person's refusal: `min` reads the person, not the pair.
        let mut refused = grants.clone();
        refused.push(alice_grant(node.clone(), KindSet::of(kind), Answer::Deny));
        let mut both = refused.clone();
        both.push(Grant::new(
            "wide",
            Subject::agent(AGENT),
            node.parent().unwrap_or_else(TreePath::root),
            KindSet::ALL,
            Answer::Allow,
            alice(),
        ));
        prop_assert_eq!(
            decide_delegated(&both, &alice(), &Subject::agent(AGENT), &node, kind, &cx()).answer,
            Answer::Deny
        );
    }

    #[test]
    fn a_delegation_never_widens_a_restriction_specific_to_the_person(
        person_node in node(),
        delegation_node in node(),
        person_answer in answer(),
        delegation_answer in answer(),
    ) {
        // Whatever the two hold, the combined answer is never above either.
        let grants = [
            Grant::new("person", alice(), person_node, KindSet::ALL, person_answer, alice()),
            Grant::new(
                "agent",
                Subject::agent(AGENT),
                delegation_node,
                KindSet::ALL,
                delegation_answer,
                alice(),
            ),
        ];
        let node = TreePath::room("r").app("mail").kind("mail.message").resource("m").action("send");
        let decision = decide_delegated(&grants, &alice(), &Subject::agent(AGENT), &node, ActionKind::Read, &cx());
        prop_assert!(decision.answer <= person_answer);
        prop_assert!(decision.answer <= delegation_answer);
    }

    #[test]
    fn expired_grants_do_not_decide(
        at in node(),
        answer in answer(),
        until in 0u64..2_000_000,
    ) {
        // The action is on the grant's own node, so liveness is the only
        // thing that can keep it from deciding.
        let grant = Grant::new("g", alice(), at.clone(), KindSet::ALL, answer, alice())
            .with_lifetime(Lifetime::Until(until));
        let held = [grant];
        let decision = decide(&held, &[alice()], &at, ActionKind::Read, &cx());
        if until > 1_000_000 {
            prop_assert_eq!(decision.answer, answer);
        } else {
            prop_assert_eq!(decision.answer, Answer::Deny);
            prop_assert!(decision.grant().is_none());
        }
    }

    #[test]
    fn a_once_grant_decides_nothing_after_the_use_that_spent_it(
        at in node(),
        answer in answer(),
    ) {
        // The action is left out, so any node's grant covers it: the answer is
        // the strictest of what the set holds while the `Once` grant is live.
        let held = [once(&at), alice_grant(at.clone(), KindSet::ALL, answer)];
        let decision = decide(&held, &[alice()], &under(&at), ActionKind::Read, &cx());
        prop_assert_eq!(decision.answer, stricter(Answer::Allow, answer));

        // After the door that ran on it revokes it, the set decides as it
        // would have without it.
        let spent = [alice_grant(at.clone(), KindSet::ALL, answer)];
        let after = decide(&spent, &[alice()], &under(&at), ActionKind::Read, &cx());
        prop_assert_eq!(after.answer, answer);
        prop_assert!(after.answer <= decision.answer);
        let without_either: [Grant; 0] = [];
        prop_assert_eq!(
            decide(&without_either, &[alice()], &under(&at), ActionKind::Read, &cx()).answer,
            Answer::Deny
        );
    }
}
