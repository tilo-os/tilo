use tilo_action_policy::{
    ActionKind, Answer, Authority, Basis, Change, Context, Grant, GrantId, Ids, KindSet, Labels,
    Level, Lifetime, Party, Preset, PresetError, Refusal, Role, RoomPolicy, Rule, Subject,
    TreePath, decide, describe, describe_node,
    roles::{self, governance},
};

const NOW: u64 = 1_000_000;
const ALICE: &str = "a11ce";
const BOB: &str = "b0b";
const CAROL: &str = "ca401";

fn cx() -> Context<'static> {
    Context::at(NOW)
}

fn alice() -> Subject {
    Subject::person(ALICE)
}

fn mail() -> TreePath {
    TreePath::room("r").app("mail")
}

fn send() -> TreePath {
    mail().kind("mail.message").resource("m1").action("send")
}

fn archive() -> TreePath {
    mail().kind("mail.message").resource("m1").action("archive")
}

fn grant(id: &str, node: TreePath, kinds: KindSet, answer: Answer) -> Grant {
    Grant::new(id, alice(), node, kinds, answer, Subject::person(CAROL))
}

fn answer(grants: &[Grant], node: &TreePath, kind: ActionKind) -> Answer {
    decide(grants, &[alice()], node, kind, &cx()).answer
}

const ANSWERS: [Answer; 3] = [Answer::Deny, Answer::Ask, Answer::Allow];

#[test]
fn no_grant_denies() {
    for kind in ActionKind::ALL {
        let decision = decide(&[], &[alice()], &send(), kind, &cx());
        assert_eq!(decision.answer, Answer::Deny);
        assert_eq!(decision.basis, Basis::NoGrant);
    }
}

#[test]
fn answers_are_ordered_deny_ask_allow() {
    assert!(Answer::Deny < Answer::Ask && Answer::Ask < Answer::Allow);
    assert_eq!(Answer::Ask.min(Answer::Allow), Answer::Ask);
}

#[test]
fn a_grant_covers_its_node_and_everything_below() {
    let grants = [grant("g", mail(), KindSet::ALL, Answer::Allow)];
    assert_eq!(answer(&grants, &mail(), ActionKind::Read), Answer::Allow);
    assert_eq!(answer(&grants, &send(), ActionKind::Outside), Answer::Allow);
    let field = mail()
        .kind("mail.message")
        .resource("m1")
        .field("flags.seen")
        .action("set");
    assert_eq!(answer(&grants, &field, ActionKind::Undoable), Answer::Allow);
    let sibling = TreePath::room("r").app("notes").action("send");
    assert_eq!(answer(&grants, &sibling, ActionKind::Outside), Answer::Deny);
    let other_room = TreePath::room("s").app("mail");
    assert_eq!(answer(&grants, &other_room, ActionKind::Read), Answer::Deny);
    let parent = TreePath::room("r");
    assert_eq!(answer(&grants, &parent, ActionKind::Read), Answer::Deny);
}

#[test]
fn a_root_grant_covers_every_room() {
    let grants = [grant("g", TreePath::root(), KindSet::READ, Answer::Allow)];
    assert_eq!(answer(&grants, &send(), ActionKind::Read), Answer::Allow);
    assert_eq!(
        answer(&grants, &TreePath::room("x"), ActionKind::Read),
        Answer::Allow
    );
}

#[test]
fn grants_only_cover_their_kinds() {
    for granted in 0u8..8 {
        let kinds: KindSet = ActionKind::ALL
            .into_iter()
            .enumerate()
            .filter(|(bit, _)| granted & (1 << bit) != 0)
            .map(|(_, kind)| kind)
            .collect();
        let grants = [grant("g", mail(), kinds, Answer::Allow)];
        for kind in ActionKind::ALL {
            let expected = if kinds.contains(kind) {
                Answer::Allow
            } else {
                Answer::Deny
            };
            assert_eq!(answer(&grants, &send(), kind), expected, "{kinds:?} {kind}");
        }
    }
}

#[test]
fn a_new_outside_action_does_not_slip_under_an_undoable_grant() {
    let grants = [grant("g", mail(), KindSet::READ_UNDOABLE, Answer::Allow)];
    let added_later = mail()
        .kind("mail.message")
        .resource("m1")
        .action("forward-to-fax");
    assert_eq!(
        answer(&grants, &added_later, ActionKind::Undoable),
        Answer::Allow
    );
    assert_eq!(
        answer(&grants, &added_later, ActionKind::Outside),
        Answer::Deny
    );
}

#[test]
fn the_most_specific_node_wins() {
    // "Mail: Allow; Mail › Send: Ask" asks before sending and allows the rest.
    let grants = [
        grant("mail", mail(), KindSet::ALL, Answer::Allow),
        grant(
            "send",
            mail().rest().action("send"),
            KindSet::ALL,
            Answer::Ask,
        ),
    ];
    assert_eq!(answer(&grants, &send(), ActionKind::Outside), Answer::Ask);
    assert_eq!(
        answer(&grants, &archive(), ActionKind::Undoable),
        Answer::Allow
    );

    // A narrow Allow beats a broad Deny, and the other way round.
    for (broad, narrow) in [(Answer::Deny, Answer::Allow), (Answer::Allow, Answer::Deny)] {
        let grants = [
            grant("broad", TreePath::room("r"), KindSet::ALL, broad),
            grant("narrow", send(), KindSet::ALL, narrow),
        ];
        assert_eq!(answer(&grants, &send(), ActionKind::Outside), narrow);
        assert_eq!(answer(&grants, &archive(), ActionKind::Outside), broad);
    }
}

#[test]
fn deeper_beats_shallower_for_every_pair_of_answers() {
    let chain = [
        TreePath::root(),
        TreePath::room("r"),
        mail(),
        mail().kind("mail.message"),
        mail().kind("mail.message").resource("m1"),
        send(),
    ];
    for (shallow_index, shallow) in chain.iter().enumerate() {
        for deep in &chain[shallow_index + 1..] {
            for shallow_answer in ANSWERS {
                for deep_answer in ANSWERS {
                    let grants = [
                        grant("deep", deep.clone(), KindSet::ALL, deep_answer),
                        grant("shallow", shallow.clone(), KindSet::ALL, shallow_answer),
                    ];
                    assert_eq!(
                        answer(&grants, &send(), ActionKind::Outside),
                        deep_answer,
                        "{shallow} {shallow_answer} vs {deep} {deep_answer}"
                    );
                }
            }
        }
    }
}

#[test]
fn at_the_same_node_deny_beats_ask_beats_allow() {
    for first in ANSWERS {
        for second in ANSWERS {
            for third in ANSWERS {
                let grants = [
                    grant("1", mail(), KindSet::ALL, first),
                    grant("2", mail(), KindSet::ALL, second),
                    grant("3", mail(), KindSet::ALL, third),
                ];
                let expected = first.min(second).min(third);
                let decision = decide(&grants, &[alice()], &send(), ActionKind::Outside, &cx());
                assert_eq!(decision.answer, expected);
                assert_eq!(decision.grant().map(|grant| grant.answer), Some(expected));
            }
        }
    }
}

#[test]
fn a_named_level_beats_an_open_one_before_depth_counts() {
    // "This message: Deny" beats "every message › Send: Allow".
    let grants = [
        grant(
            "any-send",
            mail()
                .kind("mail.message")
                .any(Level::Resource)
                .action("send"),
            KindSet::ALL,
            Answer::Allow,
        ),
        grant(
            "m1",
            mail().kind("mail.message").resource("m1"),
            KindSet::ALL,
            Answer::Deny,
        ),
    ];
    assert_eq!(answer(&grants, &send(), ActionKind::Outside), Answer::Deny);
    let other = mail().kind("mail.message").resource("m2").action("send");
    assert_eq!(answer(&grants, &other, ActionKind::Outside), Answer::Allow);
    let other_action = mail().kind("mail.message").resource("m2").action("archive");
    assert_eq!(
        answer(&grants, &other_action, ActionKind::Undoable),
        Answer::Deny
    );
}

#[test]
fn any_depth_matches_zero_or_more_segments() {
    let pattern = TreePath::room("r").rest().push(Level::Field, "comments");
    assert!(pattern.covers(&TreePath::room("r").field("comments")));
    assert!(
        pattern.covers(
            &mail()
                .resource("x")
                .field("comments")
                .item("c1")
                .action("set")
        )
    );
    assert!(!pattern.covers(&mail().resource("x").field("body")));
    assert!(!pattern.covers(&TreePath::room("s").field("comments")));
    assert!(TreePath::room("r").rest().covers(&TreePath::room("r")));
    assert_eq!(
        TreePath::room("r").rest().specificity(),
        TreePath::room("r").specificity()
    );
}

#[test]
fn overlapping_nodes_share_a_descendant() {
    let app = mail();
    let comments = TreePath::room("r").rest().push(Level::Field, "comments");
    assert!(comments.overlaps(&app));
    assert!(app.overlaps(&comments));
    assert!(app.overlaps(&TreePath::room("r")));
    assert!(TreePath::room("r").overlaps(&app));
    assert!(app.overlaps(&send()));
    assert!(TreePath::root().overlaps(&app));
    assert!(!app.overlaps(&TreePath::room("r").app("notes")));
    assert!(!app.overlaps(&TreePath::room("s")));
    assert!(!app.overlaps(&roles::room_resource("r")));
    let any_send = TreePath::room("r").any(Level::App).rest().action("send");
    assert!(any_send.overlaps(&app));
    assert!(any_send.overlaps(&send()));
    assert!(!any_send.overlaps(&roles::room_resource("r")));
}

#[test]
fn open_levels_only_match_their_level() {
    let pattern = TreePath::room("r").any(Level::App);
    assert!(pattern.covers(&mail()));
    assert!(!pattern.covers(&TreePath::room("r").kind(roles::ROOM_KIND)));
    assert!(pattern.covers(&TreePath::room("r").any(Level::App)));
    assert!(!mail().covers(&TreePath::room("r").any(Level::App)));
}

#[test]
fn grants_apply_only_to_their_subject() {
    let grants = [
        Grant::new(
            "bob",
            Subject::person(BOB),
            mail(),
            KindSet::ALL,
            Answer::Allow,
            alice(),
        ),
        Grant::new(
            "editor",
            Subject::role(Role::Editor),
            mail(),
            KindSet::READ,
            Answer::Allow,
            alice(),
        ),
    ];
    assert_eq!(answer(&grants, &send(), ActionKind::Read), Answer::Deny);
    let as_editor = decide(
        &grants,
        &[alice(), Subject::role(Role::Editor)],
        &send(),
        ActionKind::Read,
        &cx(),
    );
    assert_eq!(as_editor.answer, Answer::Allow);
    let agent = decide(
        &grants,
        &[Subject::agent(BOB)],
        &send(),
        ActionKind::Read,
        &cx(),
    );
    assert_eq!(agent.answer, Answer::Deny);
}

#[test]
fn expired_grants_are_not_live() {
    let grants = [
        grant("broad", mail(), KindSet::ALL, Answer::Allow),
        grant("until", send(), KindSet::ALL, Answer::Deny).with_lifetime(Lifetime::Until(NOW)),
    ];
    assert_eq!(answer(&grants, &send(), ActionKind::Outside), Answer::Allow);
    let earlier = Context::at(NOW - 1);
    let decision = decide(&grants, &[alice()], &send(), ActionKind::Outside, &earlier);
    assert_eq!(decision.answer, Answer::Deny);
}

#[test]
fn session_grants_live_only_in_their_session() {
    let grants = [grant("s", mail(), KindSet::ALL, Answer::Allow)
        .with_lifetime(Lifetime::Session("s1".into()))];
    let here = cx().in_session("s1");
    let elsewhere = cx().in_session("s2");
    assert_eq!(
        decide(&grants, &[alice()], &send(), ActionKind::Read, &here).answer,
        Answer::Allow
    );
    assert_eq!(
        decide(&grants, &[alice()], &send(), ActionKind::Read, &elsewhere).answer,
        Answer::Deny
    );
    // A receiver that does not know sessions leaves the refusal to the device.
    assert_eq!(answer(&grants, &send(), ActionKind::Read), Answer::Allow);
}

#[test]
fn task_grants_end_with_the_task() {
    let grants =
        [grant("t", mail(), KindSet::ALL, Answer::Allow)
            .with_lifetime(Lifetime::Task("t1".into()))];
    let open = |task: &str| task == "t1";
    let closed = |_: &str| false;
    let with_open = cx().with_tasks(&open);
    let with_closed = cx().with_tasks(&closed);
    assert_eq!(
        decide(&grants, &[alice()], &send(), ActionKind::Read, &with_open).answer,
        Answer::Allow
    );
    assert_eq!(
        decide(&grants, &[alice()], &send(), ActionKind::Read, &with_closed).answer,
        Answer::Deny
    );
}

#[test]
fn once_grants_are_reported_for_consumption() {
    let grants = [
        grant("mail", mail(), KindSet::ALL, Answer::Ask),
        grant("once", send(), KindSet::OUTSIDE, Answer::Allow).with_lifetime(Lifetime::Once),
    ];
    let authority = Authority::person(Party::new(vec![alice()], &grants));
    let verdict = authority.decide(&send(), ActionKind::Outside, &cx());
    assert_eq!(verdict.answer, Answer::Allow);
    let once: Vec<_> = verdict
        .once_grants()
        .map(|grant| grant.id.clone())
        .collect();
    assert_eq!(once, vec![GrantId::new("once")]);
    let after: Vec<Grant> = grants
        .iter()
        .filter(|grant| !once.contains(&grant.id))
        .cloned()
        .collect();
    let authority = Authority::person(Party::new(vec![alice()], &after));
    assert_eq!(
        authority.decide(&send(), ActionKind::Outside, &cx()).answer,
        Answer::Ask
    );
}

#[test]
fn delegation_is_the_minimum_of_person_and_delegation() {
    for person_answer in ANSWERS {
        for agent_answer in ANSWERS {
            let grants = [
                grant("person", mail(), KindSet::ALL, person_answer),
                Grant::new(
                    "agent",
                    Subject::agent("ag"),
                    mail(),
                    KindSet::ALL,
                    agent_answer,
                    alice(),
                ),
            ];
            let authority = Authority::person(Party::new(vec![alice()], &grants))
                .bounded_by(Party::new(vec![Subject::agent("ag")], &grants));
            let verdict = authority.decide(&send(), ActionKind::Outside, &cx());
            assert_eq!(verdict.answer, person_answer.min(agent_answer));
            assert_eq!(verdict.parts.len(), 2);
        }
    }
}

#[test]
fn an_agent_never_does_more_than_its_person() {
    let grants = [
        grant("person", mail(), KindSet::READ, Answer::Allow),
        Grant::new(
            "agent",
            Subject::agent("ag"),
            TreePath::root(),
            KindSet::ALL,
            Answer::Allow,
            alice(),
        ),
    ];
    let authority = Authority::person(Party::new(vec![alice()], &grants))
        .bounded_by(Party::new(vec![Subject::agent("ag")], &grants));
    assert_eq!(
        authority.decide(&send(), ActionKind::Read, &cx()).answer,
        Answer::Allow
    );
    assert_eq!(
        authority.decide(&send(), ActionKind::Outside, &cx()).answer,
        Answer::Deny
    );
    // A restriction added on the person reaches the agent.
    let mut restricted = grants.to_vec();
    restricted.push(grant("no-send", send(), KindSet::ALL, Answer::Deny));
    let authority = Authority::person(Party::new(vec![alice()], &restricted))
        .bounded_by(Party::new(vec![Subject::agent("ag")], &restricted));
    assert_eq!(
        authority.decide(&send(), ActionKind::Read, &cx()).answer,
        Answer::Deny
    );
}

#[test]
fn every_bound_applies() {
    let grants = [
        grant("person", mail(), KindSet::ALL, Answer::Allow),
        Grant::new(
            "agent",
            Subject::agent("ag"),
            mail(),
            KindSet::ALL,
            Answer::Allow,
            alice(),
        ),
        Grant::new(
            "task",
            Subject::task("t"),
            mail(),
            KindSet::READ_UNDOABLE,
            Answer::Allow,
            alice(),
        ),
    ];
    let authority = Authority::person(Party::new(vec![alice()], &grants))
        .bounded_by(Party::new(vec![Subject::agent("ag")], &grants))
        .bounded_by(Party::new(vec![Subject::task("t")], &grants));
    assert_eq!(
        authority
            .decide(&archive(), ActionKind::Undoable, &cx())
            .answer,
        Answer::Allow
    );
    let verdict = authority.decide(&send(), ActionKind::Outside, &cx());
    assert_eq!(verdict.answer, Answer::Deny);
    assert_eq!(
        verdict.deciding().map(|part| part.basis),
        Some(Basis::NoGrant)
    );
    assert!(
        verdict
            .explain()
            .starts_with("Not allowed: the delegation's grants"),
        "{}",
        verdict.explain()
    );
}

#[test]
fn the_owner_is_structural() {
    let mut room = RoomPolicy::new("r", ALICE);
    room.grants.push(grant(
        "deny-owner",
        TreePath::room("r"),
        KindSet::ALL,
        Answer::Deny,
    ));
    let authority = Authority::person(room.party(ALICE));
    for kind in ActionKind::ALL {
        let verdict = authority.decide(&send(), kind, &cx());
        assert_eq!(verdict.answer, Answer::Allow);
        assert_eq!(verdict.parts[0].basis, Basis::Owner);
    }
    // The owner's delegations still bound what acts for them.
    let agent =
        Authority::person(room.party(ALICE)).bounded_by(room.party_of(Subject::agent("ag")));
    assert_eq!(
        agent.decide(&send(), ActionKind::Read, &cx()).answer,
        Answer::Deny
    );
    // Outside the room the owner holds nothing structural.
    assert_eq!(
        authority
            .decide(&TreePath::room("s"), ActionKind::Read, &cx())
            .answer,
        Answer::Deny
    );

    let owner = Authority::person(room.party(ALICE));
    let remove = Change::RemoveMember {
        person: ALICE.into(),
    };
    assert_eq!(
        room.clone().apply(ALICE, &owner, remove, &cx(), false),
        Err(Refusal::Owner)
    );
    let demote = Change::SetRole {
        person: ALICE.into(),
        role: Role::Viewer,
    };
    assert_eq!(
        room.clone().apply(ALICE, &owner, demote, &cx(), false),
        Err(Refusal::Owner)
    );
}

fn room_with(members: &[(&str, Role)]) -> RoomPolicy {
    let mut room = RoomPolicy::new("r", ALICE);
    for (person, role) in members {
        room.members.insert((*person).to_owned(), *role);
    }
    room
}

#[test]
fn role_presets_grant_what_the_table_says() {
    use ActionKind::{Outside, Read, Undoable};
    use Answer::{Allow, Ask, Deny};
    let room = room_with(&[]);
    let content = mail().kind("mail.message").resource("m1").action("archive");
    let comment = mail()
        .kind("mail.message")
        .resource("m1")
        .field("comments")
        .action("insert");
    let governance = roles::governance_action("r", governance::INVITE);
    let room_settings = roles::room_resource("r");
    let table: [(Role, [Answer; 6]); 4] = [
        // content Read, Undoable, Outside; comment Undoable; governance Undoable; room Read
        (Role::Viewer, [Allow, Deny, Deny, Deny, Deny, Allow]),
        (Role::Commenter, [Allow, Deny, Deny, Allow, Deny, Allow]),
        (Role::Editor, [Allow, Allow, Ask, Allow, Deny, Allow]),
        (Role::Owner, [Allow, Allow, Allow, Allow, Allow, Allow]),
    ];
    for (role, expected) in table {
        let subjects = [Subject::person("someone"), Subject::role(role)];
        let at = |node: &TreePath, kind| decide(&room.grants, &subjects, node, kind, &cx()).answer;
        let got = [
            at(&content, Read),
            at(&content, Undoable),
            at(&content, Outside),
            at(&comment, Undoable),
            at(&governance, Outside),
            at(&room_settings, Read),
        ];
        assert_eq!(got, expected, "{role}");
    }
}

#[test]
fn non_members_hold_nothing() {
    let room = room_with(&[(BOB, Role::Editor)]);
    let stranger = Authority::person(room.party(CAROL));
    assert_eq!(
        stranger.decide(&mail(), ActionKind::Read, &cx()).answer,
        Answer::Deny
    );
    assert_eq!(room.role_of(CAROL), None);
    assert_eq!(room.role_of(BOB), Some(Role::Editor));
    assert_eq!(room.role_of(ALICE), Some(Role::Owner));
}

#[test]
fn only_governors_change_membership() {
    let mut room = room_with(&[(BOB, Role::Editor)]);
    let bob = room.party(BOB);
    let bob = Authority::person(bob);
    let add = Change::AddMember {
        person: CAROL.into(),
        role: Role::Viewer,
    };
    let refused = room.clone().apply(BOB, &bob, add.clone(), &cx(), false);
    assert!(
        matches!(refused, Err(Refusal::NotPermitted(_))),
        "{refused:?}"
    );

    let owner = Authority::person(room.party(ALICE));
    room.clone()
        .apply(ALICE, &owner, add.clone(), &cx(), false)
        .expect("owner adds");
    let mut added = room.clone();
    added
        .apply(ALICE, &owner, add.clone(), &cx(), false)
        .expect("owner adds");
    assert_eq!(
        added.clone().apply(ALICE, &owner, add, &cx(), false),
        Err(Refusal::AlreadyAMember(CAROL.into()))
    );
    let owner_role = Change::AddMember {
        person: "dave".into(),
        role: Role::Owner,
    };
    assert_eq!(
        room.clone().apply(ALICE, &owner, owner_role, &cx(), false),
        Err(Refusal::OwnerRole)
    );

    // The owner hands Bob the invite and add-member actions; Bob can then add
    // people up to his own role and no further.
    for action in [governance::INVITE, governance::ADD_MEMBER] {
        let delegated = Grant::new(
            format!("bob-{action}"),
            Subject::person(BOB),
            roles::governance_action("r", action),
            KindSet::OUTSIDE,
            Answer::Allow,
            Subject::person(ALICE),
        );
        let owner = Authority::person(room.party(ALICE));
        let mut next = room.clone();
        next.apply(ALICE, &owner, Change::Grant(delegated), &cx(), false)
            .expect("owner grants");
        room = next;
    }
    let bob = Authority::person(room.party(BOB));
    room.check_invite(BOB, &bob, Role::Editor, &cx(), false)
        .expect("editor invites editor");
    let refused = room.check_invite(BOB, &bob, Role::Owner, &cx(), false);
    assert_eq!(refused, Err(Refusal::OwnerRole));
    let add = Change::AddMember {
        person: CAROL.into(),
        role: Role::Commenter,
    };
    let mut next = room.clone();
    next.apply(BOB, &bob, add, &cx(), false).expect("bob adds");
    assert_eq!(next.role_of(CAROL), Some(Role::Commenter));
    let bob = Authority::person(next.party(BOB));
    let remove = Change::RemoveMember {
        person: CAROL.into(),
    };
    assert!(matches!(
        next.clone().apply(BOB, &bob, remove, &cx(), false),
        Err(Refusal::NotPermitted(_))
    ));

    // A commenter given the invite action still cannot invite an editor.
    let mut room = room_with(&[(CAROL, Role::Commenter)]);
    room.grants.push(Grant::new(
        "carol-invite",
        Subject::person(CAROL),
        roles::governance_action("r", governance::INVITE),
        KindSet::OUTSIDE,
        Answer::Allow,
        Subject::person(ALICE),
    ));
    let carol = Authority::person(room.party(CAROL));
    assert_eq!(
        room.check_invite(CAROL, &carol, Role::Editor, &cx(), false),
        Err(Refusal::AboveActorRole {
            actor: Some(Role::Commenter),
            wanted: Role::Editor
        })
    );
    room.check_invite(CAROL, &carol, Role::Viewer, &cx(), false)
        .expect("commenter invites viewer");
}

#[test]
fn ask_needs_a_witnessed_approval() {
    let mut room = room_with(&[(BOB, Role::Editor)]);
    room.grants.push(Grant::new(
        "bob-invite",
        Subject::person(BOB),
        roles::governance_action("r", governance::INVITE),
        KindSet::OUTSIDE,
        Answer::Ask,
        Subject::person(ALICE),
    ));
    let bob = Authority::person(room.party(BOB));
    let refused = room.check_invite(BOB, &bob, Role::Viewer, &cx(), false);
    assert!(
        matches!(refused, Err(Refusal::NeedsApproval(_))),
        "{refused:?}"
    );
    room.check_invite(BOB, &bob, Role::Viewer, &cx(), true)
        .expect("approved");
}

#[test]
fn nobody_grants_more_than_they_hold() {
    let mut room = room_with(&[(BOB, Role::Editor)]);
    room.grants.push(Grant::new(
        "bob-grant",
        Subject::person(BOB),
        roles::governance_action("r", governance::GRANT),
        KindSet::OUTSIDE,
        Answer::Allow,
        Subject::person(ALICE),
    ));
    let bob = Authority::person(room.party(BOB));
    let to_carol = |kinds, answer| {
        Change::Grant(Grant::new(
            "to-carol",
            Subject::person(CAROL),
            mail(),
            kinds,
            answer,
            Subject::person(BOB),
        ))
    };
    // Bob, an editor, holds Ask for outside effects.
    assert_eq!(
        room.clone().apply(
            BOB,
            &bob,
            to_carol(KindSet::OUTSIDE, Answer::Allow),
            &cx(),
            false
        ),
        Err(Refusal::AboveActorGrant {
            kind: ActionKind::Outside,
            held: Answer::Ask,
            wanted: Answer::Allow
        })
    );
    room.clone()
        .apply(
            BOB,
            &bob,
            to_carol(KindSet::OUTSIDE, Answer::Ask),
            &cx(),
            false,
        )
        .expect("ask is within ask");
    room.clone()
        .apply(
            BOB,
            &bob,
            to_carol(KindSet::ALL, Answer::Deny),
            &cx(),
            false,
        )
        .expect("a deny never exceeds");
    let mut granted = room.clone();
    granted
        .apply(
            BOB,
            &bob,
            to_carol(KindSet::READ_UNDOABLE, Answer::Allow),
            &cx(),
            false,
        )
        .expect("within editor");
    let bob = Authority::person(granted.party(BOB));
    let again = to_carol(KindSet::READ, Answer::Allow);
    assert_eq!(
        granted.clone().apply(BOB, &bob, again, &cx(), false),
        Err(Refusal::DuplicateGrant(GrantId::new("to-carol")))
    );
    let elsewhere = Change::Grant(Grant::new(
        "elsewhere",
        Subject::person(CAROL),
        TreePath::room("s"),
        KindSet::READ,
        Answer::Allow,
        Subject::person(BOB),
    ));
    assert_eq!(
        granted.clone().apply(BOB, &bob, elsewhere, &cx(), false),
        Err(Refusal::OutsideRoom(TreePath::room("s")))
    );
}

#[test]
fn grant_and_revoke_are_actions() {
    let mut room = room_with(&[(BOB, Role::Viewer)]);
    let owner = Authority::person(room.party(ALICE));
    let edit = Grant::new(
        "bob-mail",
        Subject::person(BOB),
        mail(),
        KindSet::UNDOABLE,
        Answer::Allow,
        Subject::person(CAROL),
    );
    let mut next = room.clone();
    next.apply(ALICE, &owner, Change::Grant(edit), &cx(), false)
        .expect("grant");
    room = next;
    let bob = Authority::person(room.party(BOB));
    assert_eq!(
        bob.decide(&archive(), ActionKind::Undoable, &cx()).answer,
        Answer::Allow
    );
    let owner = Authority::person(room.party(ALICE));
    let revoke = Change::Revoke(GrantId::new("bob-mail"));
    room.clone()
        .apply(ALICE, &owner, revoke.clone(), &cx(), false)
        .expect("revoke");
    let mut revoked = room.clone();
    revoked
        .apply(ALICE, &owner, revoke.clone(), &cx(), false)
        .expect("revoke");
    let bob = Authority::person(revoked.party(BOB));
    assert_eq!(
        bob.decide(&archive(), ActionKind::Undoable, &cx()).answer,
        Answer::Deny
    );
    assert_eq!(
        revoked.clone().apply(
            ALICE,
            &Authority::person(revoked.party(ALICE)),
            revoke,
            &cx(),
            false
        ),
        Err(Refusal::UnknownGrant(GrantId::new("bob-mail")))
    );
    // A viewer cannot revoke.
    let bob = Authority::person(room.party(BOB));
    let refused = room.clone().apply(
        BOB,
        &bob,
        Change::Revoke(GrantId::new("bob-mail")),
        &cx(),
        false,
    );
    assert!(matches!(refused, Err(Refusal::NotPermitted(_))));
}

#[test]
fn a_policy_moves_to_another_room_id() {
    let mut room = room_with(&[(BOB, Role::Editor)]);
    room.grants.push(Grant::new(
        "elsewhere",
        alice(),
        TreePath::room("s"),
        KindSet::ALL,
        Answer::Allow,
        alice(),
    ));
    room.rehome("q");
    assert_eq!(room.room, "q");
    let bob = Authority::person(room.party(BOB));
    let moved = TreePath::room("q").app("mail").action("archive");
    assert_eq!(
        bob.decide(&moved, ActionKind::Undoable, &cx()).answer,
        Answer::Allow
    );
    assert_eq!(
        bob.decide(&archive(), ActionKind::Undoable, &cx()).answer,
        Answer::Deny
    );
    assert!(
        room.grants
            .iter()
            .any(|grant| grant.node == TreePath::room("s"))
    );
    let unowned = RoomPolicy::unowned("u", &alice());
    assert_eq!(unowned.owner, None);
    assert_eq!(
        unowned.grants.len(),
        roles::room_role_grants("u", &alice()).len()
    );
}

#[test]
fn a_blocked_member_holds_nothing_but_stays_on_record() {
    let mut room = room_with(&[(BOB, Role::Editor)]);
    assert_eq!(room.active_role(BOB), Some(Role::Editor));
    let block = room.block(BOB, alice());
    room.grants.push(block);
    assert!(room.is_blocked(BOB));
    assert_eq!(room.role_of(BOB), Some(Role::Editor));
    assert_eq!(room.active_role(BOB), None);
    let bob = Authority::person(room.party(BOB));
    for kind in ActionKind::ALL {
        assert_eq!(bob.decide(&send(), kind, &cx()).answer, Answer::Deny);
    }
    let owner_block = room.block(ALICE, alice());
    room.grants.push(owner_block);
    assert!(!room.is_blocked(ALICE), "the owner is never blocked");
}

#[test]
fn a_standing_carries_the_person_and_every_bound() {
    use tilo_action_policy::Standing;
    let room = room_with(&[(BOB, Role::Editor)]);
    let relative = TreePath::root().app("mail").action("send");
    let bob = Standing::person(room.clone(), BOB);
    assert_eq!(bob.node(&relative), mail().action("send"));
    assert_eq!(
        bob.answer(&bob.node(&relative), ActionKind::Outside, &cx()),
        Answer::Ask
    );
    let agent = Standing::person(room.clone(), BOB).bounded_by(
        Subject::agent("ag"),
        roles::agent_default(&[mail()]).assign(
            "ag",
            &Subject::agent("ag"),
            &alice(),
            &Lifetime::UntilRevoked,
        ),
    );
    assert!(agent.is_bounded_by(&Subject::agent("ag")));
    assert_eq!(
        agent.answer(&archive(), ActionKind::Undoable, &cx()),
        Answer::Allow
    );
    let notes = TreePath::room("r").app("notes").action("append");
    assert_eq!(
        agent.answer(&notes, ActionKind::Undoable, &cx()),
        Answer::Deny
    );
    let owner = Standing::person(room, ALICE).on_foreign_device();
    let invite = roles::governance_action("r", governance::INVITE);
    assert_eq!(
        owner.answer(&invite, roles::GOVERNANCE_KIND, &cx()),
        Answer::Deny
    );
    assert_eq!(
        owner.answer(&send(), ActionKind::Outside, &cx()),
        Answer::Ask
    );
    let json = serde_json::to_string(&owner).unwrap();
    assert_eq!(serde_json::from_str::<Standing>(&json).unwrap(), owner);
}

#[test]
fn removing_a_member_drops_their_grants() {
    let mut room = room_with(&[(BOB, Role::Viewer)]);
    room.grants.push(Grant::new(
        "bob-mail",
        Subject::person(BOB),
        mail(),
        KindSet::ALL,
        Answer::Allow,
        alice(),
    ));
    let owner = Authority::person(room.party(ALICE));
    let mut next = room.clone();
    next.apply(
        ALICE,
        &owner,
        Change::RemoveMember { person: BOB.into() },
        &cx(),
        false,
    )
    .expect("remove");
    room = next;
    assert!(!room.is_member(BOB));
    assert!(
        room.grants
            .iter()
            .all(|grant| grant.subject != Subject::person(BOB))
    );
}

#[test]
fn the_agent_default_reads_and_changes_but_asks_outside() {
    let preset = roles::agent_default(&[mail()]);
    preset.validate().expect("valid");
    let grants = preset.assign(
        "agent:ag",
        &Subject::agent("ag"),
        &alice(),
        &Lifetime::UntilRevoked,
    );
    let room = room_with(&[(BOB, Role::Editor)]);
    let authority = Authority::person(room.party(ALICE))
        .bounded_by(Party::new(vec![Subject::agent("ag")], &grants));
    assert_eq!(
        authority
            .decide(&archive(), ActionKind::Undoable, &cx())
            .answer,
        Answer::Allow
    );
    assert_eq!(
        authority.decide(&send(), ActionKind::Read, &cx()).answer,
        Answer::Allow
    );
    assert_eq!(
        authority.decide(&send(), ActionKind::Outside, &cx()).answer,
        Answer::Ask
    );
    let notes = TreePath::room("r").app("notes").action("append");
    assert_eq!(
        authority.decide(&notes, ActionKind::Undoable, &cx()).answer,
        Answer::Deny
    );
    // The agent of an editor asks no more than the editor does: Outside is Ask either way.
    let bob = Authority::person(room.party(BOB))
        .bounded_by(Party::new(vec![Subject::agent("ag")], &grants));
    assert_eq!(
        bob.decide(&send(), ActionKind::Outside, &cx()).answer,
        Answer::Ask
    );
}

#[test]
fn a_foreign_device_never_governs() {
    let room = room_with(&[]);
    let bound = roles::foreign_device_bound();
    let authority = Authority::person(room.party(ALICE))
        .bounded_by(Party::new(vec![Subject::ForeignDevice], &bound));
    assert_eq!(
        authority.decide(&send(), ActionKind::Outside, &cx()).answer,
        Answer::Ask
    );
    assert_eq!(
        authority
            .decide(&send(), ActionKind::Undoable, &cx())
            .answer,
        Answer::Allow
    );
    let invite = roles::governance_action("r", governance::INVITE);
    assert_eq!(
        authority
            .decide(&invite, roles::GOVERNANCE_KIND, &cx())
            .answer,
        Answer::Deny
    );
    assert_eq!(
        authority
            .decide(&roles::room_resource("r"), ActionKind::Read, &cx())
            .answer,
        Answer::Allow
    );
}

#[test]
fn presets_validate() {
    let rule = Rule::new(mail(), KindSet::READ, Answer::Allow);
    assert_eq!(
        Preset::new(" ", "", vec![rule.clone()]).validate(),
        Err(PresetError::Unnamed)
    );
    assert_eq!(
        Preset::new("p", "", vec![]).validate(),
        Err(PresetError::Empty)
    );
    assert_eq!(
        Preset::new(
            "p",
            "",
            vec![Rule::new(mail(), KindSet::NONE, Answer::Allow)]
        )
        .validate(),
        Err(PresetError::NoKinds(0))
    );
    let contradicting = Rule::new(mail(), KindSet::ALL, Answer::Deny);
    assert_eq!(
        Preset::new("p", "", vec![rule.clone(), contradicting]).validate(),
        Err(PresetError::Contradicts(0, 1))
    );
    let disjoint = Rule::new(mail(), KindSet::OUTSIDE, Answer::Deny);
    Preset::new("p", "", vec![rule, disjoint])
        .validate()
        .expect("disjoint kinds");
    for role in Role::ALL {
        roles::role_preset(role, "r")
            .validate()
            .expect("role presets are valid");
    }
}

#[test]
fn paths_round_trip_through_text() {
    let paths = [
        TreePath::root(),
        send(),
        TreePath::room("r/1%").app("*").kind("a:b"),
        mail()
            .any(Level::Kind)
            .rest()
            .field("comments.thread")
            .action("set"),
    ];
    for path in paths {
        let text = path.to_string();
        let parsed: TreePath = text.parse().expect("parses");
        assert_eq!(parsed, path, "{text}");
    }
    assert_eq!(
        send().to_string(),
        "room:r/app:mail/kind:mail.message/resource:m1/action:send"
    );
    assert_eq!(TreePath::room("r/1%").to_string(), "room:r%2F1%25");
    assert!("room".parse::<TreePath>().is_err());
    assert!("planet:x".parse::<TreePath>().is_err());
    assert!("room:".parse::<TreePath>().is_err());
    assert!("room:a%zz".parse::<TreePath>().is_err());
}

#[test]
fn grants_round_trip_through_json() {
    let grant = grant(
        "g",
        mail().any(Level::Resource),
        KindSet::READ_UNDOABLE,
        Answer::Ask,
    )
    .with_lifetime(Lifetime::Task("t1".into()));
    let json = serde_json::to_value(&grant).expect("serialises");
    assert_eq!(json["node"], "room:r/app:mail/resource:*");
    assert_eq!(json["kinds"], serde_json::json!(["read", "undoable"]));
    assert_eq!(json["answer"], "ask");
    let back: Grant = serde_json::from_value(json).expect("deserialises");
    assert_eq!(back, grant);
    let room = RoomPolicy::new("r", ALICE);
    let text = serde_json::to_string(&room).expect("serialises");
    assert_eq!(
        serde_json::from_str::<RoomPolicy>(&text).expect("deserialises"),
        room
    );
}

struct Names;

impl Labels for Names {
    fn label(&self, level: Level, id: &str) -> Option<String> {
        match (level, id) {
            (Level::App, "mail") => Some("Mail".into()),
            (Level::Action, "send") => Some("Send".into()),
            (Level::Room, "r") => Some("Project room".into()),
            _ => None,
        }
    }
}

#[test]
fn grant_sets_read_as_sentences() {
    let grants = [
        grant("mail-read", mail(), KindSet::READ_UNDOABLE, Answer::Allow),
        grant("mail-outside", mail(), KindSet::OUTSIDE, Answer::Ask),
        grant(
            "send",
            mail().rest().action("send"),
            KindSet::ALL,
            Answer::Ask,
        ),
        grant(
            "snippets",
            TreePath::room("r").app("snippets"),
            KindSet::ALL,
            Answer::Allow,
        ),
        grant("room", TreePath::room("r"), KindSet::READ, Answer::Allow)
            .with_lifetime(Lifetime::Session("s".into())),
    ];
    assert_eq!(
        describe(&grants, &Names),
        "Project room (this session): reading allowed. \
         Project room › Mail: reading and undoable changes allowed, outside effects ask first. \
         Project room › Mail › anywhere › Send: everything asks first. \
         Project room › snippets: everything allowed."
    );
    assert_eq!(describe(&[], &Ids), "Nothing is granted.");
    assert_eq!(describe_node(&TreePath::root(), &Ids), "Everything");
    assert_eq!(
        describe_node(&mail().any(Level::Resource), &Ids),
        "room r › mail › every resource"
    );
    let preset = roles::role_preset(Role::Editor, "r");
    let editor = preset.assign(
        "e",
        &Subject::role(Role::Editor),
        &alice(),
        &Lifetime::UntilRevoked,
    );
    assert_eq!(
        describe(&editor, &Ids),
        "room r: reading and undoable changes allowed, outside effects ask first. \
         room r › tilo.room: undoable changes and outside effects not allowed."
    );
}

#[test]
fn verdicts_explain_themselves() {
    let grants = [grant("mail", mail(), KindSet::ALL, Answer::Ask)];
    let verdict = Authority::person(Party::new(vec![alice()], &grants)).decide(
        &send(),
        ActionKind::Outside,
        &cx(),
    );
    assert_eq!(
        verdict.explain(),
        "Asks first: the person's grants give outside effects on room:r/app:mail the answer \"ask\" (grant mail)."
    );
    let room = RoomPolicy::new("r", ALICE);
    let verdict = Authority::person(room.party(ALICE)).decide(&send(), ActionKind::Outside, &cx());
    assert_eq!(verdict.explain(), "Allowed: the person owns the room.");
}

#[test]
fn a_privileged_action_needs_the_persons_own_allow() {
    let agent = || Subject::agent("ag");
    let asks = [grant("asks", mail(), KindSet::ALL, Answer::Ask)];
    let alone = Authority::person(Party::new(vec![alice()], &asks));
    assert_eq!(
        alone.decide(&send(), ActionKind::Outside, &cx()).answer,
        Answer::Ask
    );
    // Confirming is no authority the person lacks.
    assert_eq!(
        alone
            .decide(&send(), ActionKind::Outside, &cx())
            .privileged()
            .answer,
        Answer::Deny
    );

    // A person allowed it still answers their agent's Ask.
    let allows = [
        grant("allows", mail(), KindSet::ALL, Answer::Allow),
        Grant::new(
            "agent-asks",
            agent(),
            mail(),
            KindSet::ALL,
            Answer::Ask,
            alice(),
        ),
    ];
    let delegated = Authority::person(Party::new(vec![alice()], &allows))
        .bounded_by(Party::new(vec![agent()], &allows));
    assert_eq!(
        delegated
            .decide(&send(), ActionKind::Outside, &cx())
            .privileged()
            .answer,
        Answer::Ask
    );
    let allowed = Authority::person(Party::new(vec![alice()], &allows));
    assert_eq!(
        allowed
            .decide(&send(), ActionKind::Outside, &cx())
            .privileged()
            .answer,
        Answer::Allow
    );
}
