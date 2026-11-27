use tilo_action_policy::ActingPolicy as _;
use tilo_action_policy::{
    Answer, Authority, Basis, Change, Context, Grant, GrantId, Grantee, Ids, Labels, Level, Party,
    Preset, PresetError, Refusal, Role, Rule, TreePath, WorkspacePolicy, decide, describe,
    describe_node,
    roles::{self, governance},
    sentences,
};
use tilo_action_types::{ActionKind, KindSet, Lifetime};
use tilo_member_types::Acting;

const NOW: u64 = 1_000_000;
const ALICE: &str = "a11ce";
const BOB: &str = "b0b";
const CAROL: &str = "ca401";
/// The app whose records keep a workspace's layout and each person's view of it.
const LAYOUT: &str = "so.tilo.browser-desktop";

fn cx() -> Context<'static> {
    Context::at(NOW)
}

fn alice() -> Grantee {
    Grantee::person(ALICE)
}

fn mail() -> TreePath {
    TreePath::workspace("r").app("mail")
}

fn send() -> TreePath {
    mail().kind("mail.message").resource("m1").action("send")
}

fn archive() -> TreePath {
    mail().kind("mail.message").resource("m1").action("archive")
}

fn grant(id: &str, node: TreePath, kinds: KindSet, answer: Answer) -> Grant {
    Grant::new(id, alice(), node, kinds, answer, Grantee::person(CAROL))
}

fn answer(grants: &[Grant], node: &TreePath, kind: ActionKind) -> Answer {
    decide(grants, &[alice()], node, kind, &cx()).answer
}

/// The workspace vault's record.
const VAULT: &str = "uuid:56daeee1-adbe-4b63-b0cc-5fd549e93077";

/// The workspace's mail and vault resources, at the nodes every reader decides them on.
fn workspace_mail_and_vault() -> Vec<TreePath> {
    let mail = TreePath::workspace("r").app("so.tilo.mail");
    vec![
        mail.clone().resource(""),
        mail.kind("tl.so.tilo.mail").resource(""),
        TreePath::workspace("r")
            .app(tilo_app_id::apps::PASSWORDS)
            .kind("passwords.workspace-vault")
            .resource(VAULT),
    ]
}

#[test]
fn members_reach_the_workspace_mail_and_vault_through_their_role() {
    let grants = roles::workspace_role_grants("r", &alice());
    let editor = [Grantee::person(BOB), Grantee::role(Role::Editor)];
    let viewer = [Grantee::person(CAROL), Grantee::role(Role::Viewer)];
    for node in workspace_mail_and_vault() {
        for kind in [ActionKind::Read, ActionKind::Undoable] {
            assert_eq!(
                decide(&grants, &editor, &node, kind, &cx()).answer,
                Answer::Allow,
                "an Editor: {node} {kind:?}"
            );
        }
        assert_eq!(
            decide(&grants, &viewer, &node, ActionKind::Read, &cx()).answer,
            Answer::Allow,
            "a Viewer reads {node}"
        );
        assert_eq!(
            decide(&grants, &viewer, &node, ActionKind::Undoable, &cx()).answer,
            Answer::Deny,
            "a Viewer changes nothing in {node}"
        );
        // Sending mail out reaches past the workspace: the role asks it, as everywhere.
        assert_eq!(
            decide(&grants, &editor, &node, ActionKind::Outside, &cx()).answer,
            Answer::Ask
        );
    }
}

#[test]
fn an_owner_closes_the_workspace_vault_and_shares_it_with_one_member() {
    let vault = TreePath::workspace("r")
        .app(tilo_app_id::apps::PASSWORDS)
        .resource(VAULT);
    let mut grants = roles::workspace_role_grants("r", &alice());
    grants.push(Grant::new(
        "vault-closed",
        Grantee::role(Role::Viewer),
        vault.clone(),
        KindSet::READ,
        Answer::Deny,
        alice(),
    ));
    // Shared with one member: a personal grant comes before the role's.
    grants.push(Grant::new(
        "vault-bob",
        Grantee::person(BOB),
        vault.clone(),
        KindSet::READ,
        Answer::Allow,
        alice(),
    ));
    let editor = [Grantee::person(BOB), Grantee::role(Role::Editor)];
    let other = [Grantee::person(CAROL), Grantee::role(Role::Editor)];
    assert_eq!(
        decide(&grants, &editor, &vault, ActionKind::Read, &cx()).answer,
        Answer::Allow
    );
    assert_eq!(
        decide(&grants, &other, &vault, ActionKind::Read, &cx()).answer,
        Answer::Deny
    );
}

#[test]
fn an_agent_reaches_workspace_mail_through_a_grant_scoped_to_mail() {
    let mail = TreePath::workspace("r").app("so.tilo.mail");
    let key = Grantee::member(tilo_member_types::MemberId::agent("key", ALICE));
    let mut grants = roles::workspace_role_grants("r", &alice());
    let mailbox = mail.clone().resource("");
    assert_eq!(
        decide(
            &grants,
            std::slice::from_ref(&key),
            &mailbox,
            ActionKind::Read,
            &cx()
        )
        .answer,
        Answer::Deny,
        "nothing granted, nothing read"
    );
    grants.push(Grant::new(
        "key-mail",
        key.clone(),
        mail,
        KindSet::READ,
        Answer::Allow,
        alice(),
    ));
    for node in workspace_mail_and_vault().into_iter().take(2) {
        assert_eq!(
            decide(
                &grants,
                std::slice::from_ref(&key),
                &node,
                ActionKind::Read,
                &cx()
            )
            .answer,
            Answer::Allow,
            "{node}"
        );
    }
    assert_eq!(
        decide(
            &grants,
            &[key],
            &TreePath::workspace("r")
                .app(tilo_app_id::apps::PASSWORDS)
                .resource(VAULT),
            ActionKind::Read,
            &cx()
        )
        .answer,
        Answer::Deny,
        "a Mail grant opens no vault"
    );
}

const ANSWERS: [Answer; 3] = [Answer::Deny, Answer::Ask, Answer::Allow];

tilo_test_env::scenario! {
    fn the_most_specific_and_strictest_grant_wins;

    phase answers_are_ordered_deny_ask_allow;
    {
        assert!(Answer::Deny < Answer::Ask && Answer::Ask < Answer::Allow);
        assert_eq!(Answer::Ask.min(Answer::Allow), Answer::Ask);
    }

    phase the_most_specific_node_wins;
    {
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
                grant("broad", TreePath::workspace("r"), KindSet::ALL, broad),
                grant("narrow", send(), KindSet::ALL, narrow),
            ];
            assert_eq!(answer(&grants, &send(), ActionKind::Outside), narrow);
            assert_eq!(answer(&grants, &archive(), ActionKind::Outside), broad);
        }
    }

    phase deeper_beats_shallower_for_every_pair_of_answers;
    {
        let chain = [
            TreePath::root(),
            TreePath::workspace("r"),
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

    phase at_the_same_node_deny_beats_ask_beats_allow;
    {
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

    phase a_named_level_beats_an_open_one_before_depth_counts;
    {
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

    phase a_personal_override_wins_at_the_same_scope;
    {
        let viewers = Grantee::Role(Role::Viewer);
        for (role_answer, personal_answer) in
            [(Answer::Allow, Answer::Deny), (Answer::Deny, Answer::Allow)]
        {
            let role = Grant::new(
                "viewers",
                viewers.clone(),
                mail(),
                KindSet::ALL,
                role_answer,
                Grantee::person(CAROL),
            );
            let personal = grant("alice", mail(), KindSet::ALL, personal_answer);
            for rules in [[&role, &personal], [&personal, &role]] {
                let decision = decide(
                    rules,
                    &[alice(), viewers.clone()],
                    &send(),
                    ActionKind::Read,
                    &cx(),
                );
                assert_eq!(decision.answer, personal_answer);
                assert_eq!(decision.grant(), Some(&personal));

                let other = decide(
                    rules,
                    &[Grantee::person(BOB), viewers.clone()],
                    &send(),
                    ActionKind::Read,
                    &cx(),
                );
                assert_eq!(other.answer, role_answer);
                assert_eq!(other.grant(), Some(&role));
            }
        }
    }
}

tilo_test_env::scenario! {
    fn a_grant_covers_its_node_kinds_and_levels;

    phase no_grant_denies;
    {
        for kind in ActionKind::ALL {
            let decision = decide(&[], &[alice()], &send(), kind, &cx());
            assert_eq!(decision.answer, Answer::Deny);
            assert_eq!(decision.basis, Basis::NoGrant);
        }
    }

    phase a_grant_covers_its_node_and_everything_below;
    {
        let grants = [grant("g", mail(), KindSet::ALL, Answer::Allow)];
        assert_eq!(answer(&grants, &mail(), ActionKind::Read), Answer::Allow);
        assert_eq!(answer(&grants, &send(), ActionKind::Outside), Answer::Allow);
        let field = mail()
            .kind("mail.message")
            .resource("m1")
            .field("flags.seen")
            .action("set");
        assert_eq!(answer(&grants, &field, ActionKind::Undoable), Answer::Allow);
        let sibling = TreePath::workspace("r").app("notes").action("send");
        assert_eq!(answer(&grants, &sibling, ActionKind::Outside), Answer::Deny);
        let other_workspace = TreePath::workspace("s").app("mail");
        assert_eq!(
            answer(&grants, &other_workspace, ActionKind::Read),
            Answer::Deny
        );
        let parent = TreePath::workspace("r");
        assert_eq!(answer(&grants, &parent, ActionKind::Read), Answer::Deny);
    }

    phase a_root_grant_covers_every_workspace;
    {
        let grants = [grant("g", TreePath::root(), KindSet::READ, Answer::Allow)];
        assert_eq!(answer(&grants, &send(), ActionKind::Read), Answer::Allow);
        assert_eq!(
            answer(&grants, &TreePath::workspace("x"), ActionKind::Read),
            Answer::Allow
        );
    }

    phase grants_only_cover_their_kinds;
    {
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

    phase a_new_outside_action_does_not_slip_under_an_undoable_grant;
    {
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

    phase any_depth_matches_zero_or_more_segments;
    {
        let pattern = TreePath::workspace("r")
            .rest()
            .push(Level::Field, "comments");
        assert!(pattern.covers(&TreePath::workspace("r").field("comments")));
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
        assert!(!pattern.covers(&TreePath::workspace("s").field("comments")));
        assert!(
            TreePath::workspace("r")
                .rest()
                .covers(&TreePath::workspace("r"))
        );
        assert_eq!(
            TreePath::workspace("r").rest().specificity(),
            TreePath::workspace("r").specificity()
        );
    }

    phase overlapping_nodes_share_a_descendant;
    {
        let app = mail();
        let comments = TreePath::workspace("r")
            .rest()
            .push(Level::Field, "comments");
        assert!(comments.overlaps(&app));
        assert!(app.overlaps(&comments));
        assert!(app.overlaps(&TreePath::workspace("r")));
        assert!(TreePath::workspace("r").overlaps(&app));
        assert!(app.overlaps(&send()));
        assert!(TreePath::root().overlaps(&app));
        assert!(!app.overlaps(&TreePath::workspace("r").app("notes")));
        assert!(!app.overlaps(&TreePath::workspace("s")));
        assert!(!app.overlaps(&roles::workspace_resource("r")));
        let any_send = TreePath::workspace("r")
            .any(Level::App)
            .rest()
            .action("send");
        assert!(any_send.overlaps(&app));
        assert!(any_send.overlaps(&send()));
        assert!(!any_send.overlaps(&roles::workspace_resource("r")));
    }

    phase open_levels_only_match_their_level;
    {
        let pattern = TreePath::workspace("r").any(Level::App);
        assert!(pattern.covers(&mail()));
        assert!(!pattern.covers(&TreePath::workspace("r").kind(roles::WORKSPACE_KIND)));
        assert!(pattern.covers(&TreePath::workspace("r").any(Level::App)));
        assert!(!mail().covers(&TreePath::workspace("r").any(Level::App)));
    }
}

tilo_test_env::scenario! {
    fn a_grant_reaches_only_its_grantee_and_higher_roles;

    phase grants_apply_only_to_their_grantee;
    {
        let grants = [
            Grant::new(
                "bob",
                Grantee::person(BOB),
                mail(),
                KindSet::ALL,
                Answer::Allow,
                alice(),
            ),
            Grant::new(
                "editor",
                Grantee::role(Role::Editor),
                mail(),
                KindSet::READ,
                Answer::Allow,
                alice(),
            ),
        ];
        assert_eq!(answer(&grants, &send(), ActionKind::Read), Answer::Deny);
        let as_editor = decide(
            &grants,
            &[alice(), Grantee::role(Role::Editor)],
            &send(),
            ActionKind::Read,
            &cx(),
        );
        assert_eq!(as_editor.answer, Answer::Allow);
        let agent = decide(
            &grants,
            &[Grantee::agent(BOB, ALICE)],
            &send(),
            ActionKind::Read,
            &cx(),
        );
        assert_eq!(agent.answer, Answer::Deny);
    }

    phase a_grant_to_a_role_reaches_every_higher_role;
    // A grant to a role names everyone holding it and everyone above it, so a
    // workspace shares something with `Role::Viewer` and reaches every member.
    // A built-in role's own grants name that role alone: they are its rules.
    {
        let workspace = workspace_with_members(&[
            (BOB, Role::Editor),
            (CAROL, Role::Moderator),
            ("v", Role::Viewer),
        ]);
        let shared = Grant::new(
            "share:m1:viewer",
            Grantee::role(Role::Viewer),
            mail().kind("mail.message").resource("m1"),
            KindSet::READ_UNDOABLE,
            Answer::Allow,
            alice(),
        );
        let mut policy = workspace;
        policy.grants.push(shared);
        let edits = |person: &str| {
            Authority::person(policy.party(person))
                .decide(
                    &mail().kind("mail.message").resource("m1").action("archive"),
                    ActionKind::Undoable,
                    &cx(),
                )
                .answer
        };
        for person in [ALICE, CAROL, BOB, "v"] {
            assert_eq!(edits(person), Answer::Allow, "{person}");
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn wall_clock_context_accepts_native_system_time() {
    let now: std::time::SystemTime = std::time::UNIX_EPOCH + std::time::Duration::from_millis(NOW);
    assert_eq!(Context::at_time(now).now_unix_ms, NOW);
}

tilo_test_env::scenario! {
    fn a_grant_lives_as_long_as_its_lifetime;

    phase wall_clock_context_preserves_epoch_and_expiry;
    {
        use tilo_clock::time::{Duration, UNIX_EPOCH};

        assert_eq!(Context::at_time(UNIX_EPOCH).now_unix_ms, 0);
        assert_eq!(
            Context::at_time(UNIX_EPOCH - Duration::from_millis(1)).now_unix_ms,
            0
        );
        let grants =
            [grant("until", send(), KindSet::ALL, Answer::Allow).with_lifetime(Lifetime::Until(NOW))];
        for (millis, expected) in [
            (NOW - 1, Answer::Allow),
            (NOW, Answer::Deny),
            (NOW + 1, Answer::Deny),
        ] {
            let cx = Context::at_time(UNIX_EPOCH + Duration::from_millis(millis));
            assert_eq!(cx.now_unix_ms, millis);
            assert_eq!(
                decide(&grants, &[alice()], &send(), ActionKind::Outside, &cx).answer,
                expected
            );
        }
    }

    phase every_prompt_offers_the_same_lifetimes;
    {
        let offered = Lifetime::offered(NOW);
        assert_eq!(
            offered,
            [
                Lifetime::Once,
                Lifetime::Session,
                Lifetime::Until(NOW + tilo_action_types::OFFERED_SPAN_MS),
                Lifetime::UntilRevoked,
            ]
        );
        for lifetime in offered {
            assert_eq!(Lifetime::chosen(lifetime.choice(), NOW), Some(lifetime));
        }
        assert_eq!(Lifetime::chosen(4, NOW), None);
        let keys: Vec<_> = offered.iter().map(|lifetime| lifetime.key()).collect();
        assert_eq!(keys, ["once", "session", "until", "always"]);
        let durable: Vec<_> = offered.iter().map(|lifetime| lifetime.is_durable()).collect();
        assert_eq!(durable, [false, false, true, true]);
        assert!(Lifetime::Until(NOW).is_live_at(Some(NOW - 1)));
        assert!(!Lifetime::Until(NOW).is_live_at(Some(NOW)));
        assert!(Lifetime::Until(NOW).is_live_at(None));
        // Ordered by how long each lives.
        let by_length = offered.map(|lifetime| lifetime.max(Lifetime::Until(NOW)));
        assert_eq!(by_length[..2], [Lifetime::Until(NOW); 2]);
        assert_eq!(by_length[2..], offered[2..]);
    }

    phase expired_grants_are_not_live;
    {
        let grants = [
            grant("broad", mail(), KindSet::ALL, Answer::Allow),
            grant("until", send(), KindSet::ALL, Answer::Deny).with_lifetime(Lifetime::Until(NOW)),
        ];
        assert_eq!(answer(&grants, &send(), ActionKind::Outside), Answer::Allow);
        let earlier = Context::at(NOW - 1);
        let decision = decide(&grants, &[alice()], &send(), ActionKind::Outside, &earlier);
        assert_eq!(decision.answer, Answer::Deny);
    }

    phase once_grants_are_reported_for_consumption;
    {
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
}

fn workspace_with_members(members: &[(&str, Role)]) -> WorkspacePolicy {
    let mut workspace = WorkspacePolicy::new("r", ALICE);
    for (person, role) in members {
        workspace.members.insert((*person).to_owned(), *role);
    }
    workspace.with_current_roles()
}

#[test]
fn custom_roles_follow_governance_and_compose_with_personal_overrides() {
    let mut workspace = workspace_with_members(&[(BOB, Role::Viewer), (CAROL, Role::Viewer)]);
    let document = mail().resource("review");
    let apply = |workspace: &mut WorkspacePolicy, change| {
        let held = workspace.clone();
        workspace
            .apply(
                ALICE,
                &held.person_on_device(ALICE, false),
                change,
                &cx(),
                false,
            )
            .expect("the owner can manage roles");
    };
    let read = |workspace: &WorkspacePolicy, person| {
        workspace
            .party(person)
            .decide(&document, ActionKind::Read, &cx())
            .answer
    };
    for (id, name, answer) in [
        ("guests", "Guests", Answer::Deny),
        ("reviewers", "Reviewers", Answer::Allow),
    ] {
        apply(
            &mut workspace,
            Change::DefineRole {
                id: id.into(),
                name: name.into(),
            },
        );
        apply(
            &mut workspace,
            Change::Grant(Grant::new(
                id,
                Grantee::CustomRole(id.into()),
                document.clone(),
                KindSet::READ,
                answer,
                alice(),
            )),
        );
    }
    for (person, role) in [(BOB, "guests"), (BOB, "reviewers"), (CAROL, "guests")] {
        apply(
            &mut workspace,
            Change::SetMemberCustomRole {
                person: person.into(),
                role: role.into(),
                assigned: true,
            },
        );
    }
    assert_eq!(read(&workspace, BOB), Answer::Allow);
    assert_eq!(read(&workspace, CAROL), Answer::Deny);

    apply(
        &mut workspace,
        Change::Grant(Grant::new(
            "bob-exception",
            Grantee::person(BOB),
            document.clone(),
            KindSet::READ,
            Answer::Deny,
            alice(),
        )),
    );
    assert_eq!(read(&workspace, BOB), Answer::Deny);
    apply(
        &mut workspace,
        Change::Revoke(GrantId::new("bob-exception")),
    );
    assert_eq!(read(&workspace, BOB), Answer::Allow);

    let loaded: WorkspacePolicy =
        serde_json::from_value(serde_json::to_value(&workspace).unwrap()).unwrap();
    assert_eq!(read(&loaded, BOB), Answer::Allow);
    assert_eq!(read(&loaded, CAROL), Answer::Deny);

    apply(
        &mut workspace,
        Change::RemoveRole {
            id: "reviewers".into(),
        },
    );
    assert_eq!(read(&workspace, BOB), Answer::Deny);
    assert!(
        !workspace
            .grantees_of(BOB)
            .contains(&Grantee::CustomRole("reviewers".into()))
    );
    assert!(
        !workspace
            .grants
            .iter()
            .any(|grant| grant.id.as_str() == "reviewers")
    );
}

tilo_test_env::scenario! {
    fn membership_decides_who_holds_anything;

    phase non_members_hold_nothing;
    {
        let workspace = workspace_with_members(&[(BOB, Role::Editor)]);
        let stranger = Authority::person(workspace.party(CAROL));
        assert_eq!(
            stranger.decide(&mail(), ActionKind::Read, &cx()).answer,
            Answer::Deny
        );
        assert_eq!(workspace.role_of(CAROL), None);
        assert_eq!(workspace.role_of(BOB), Some(Role::Editor));
        assert_eq!(workspace.role_of(ALICE), Some(Role::Owner));
    }

    phase admission_records_when_a_member_joined_until_they_leave;
    {
        fn by_owner(workspace: &WorkspacePolicy, change: Change, now: u64) -> WorkspacePolicy {
            let owner = Authority::person(workspace.party(ALICE));
            let mut next = workspace.clone();
            next.apply(ALICE, &owner, change, &Context::at(now), false)
                .expect("the owner administers");
            next
        }
        let workspace = by_owner(
            &workspace_with_members(&[]),
            Change::AddMember {
                person: CAROL.into(),
                role: Role::Viewer,
            },
            NOW,
        );
        assert_eq!(workspace.joined_at(CAROL), Some(NOW));
        assert_eq!(
            workspace.joined_at(ALICE),
            Some(0),
            "the owner holds it from the start"
        );
        assert_eq!(workspace.joined_at(BOB), None);

        let workspace = by_owner(
            &workspace,
            Change::SetRole {
                person: CAROL.into(),
                role: Role::Editor,
            },
            NOW + 5_000,
        );
        assert_eq!(
            workspace.joined_at(CAROL),
            Some(NOW),
            "a role change keeps it"
        );

        let workspace = by_owner(
            &workspace,
            Change::RemoveMember {
                person: CAROL.into(),
            },
            NOW + 5_000,
        );
        assert_eq!(workspace.joined_at(CAROL), None);
    }

    phase only_administrators_change_membership;
    {
        let mut workspace = workspace_with_members(&[(BOB, Role::Editor)]);
        let bob = workspace.party(BOB);
        let bob = Authority::person(bob);
        let add = Change::AddMember {
            person: CAROL.into(),
            role: Role::Viewer,
        };
        let refused = workspace
            .clone()
            .apply(BOB, &bob, add.clone(), &cx(), false);
        assert!(
            matches!(refused, Err(Refusal::NotPermitted(_))),
            "{refused:?}"
        );

        let owner = Authority::person(workspace.party(ALICE));
        workspace
            .clone()
            .apply(ALICE, &owner, add.clone(), &cx(), false)
            .expect("owner adds");
        let mut added = workspace.clone();
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
            workspace
                .clone()
                .apply(ALICE, &owner, owner_role, &cx(), false),
            Err(Refusal::OwnerRole)
        );

        // The owner hands Bob the add-member action; Bob can then add
        // people up to his own role and no further.
        {
            let action = governance::ADD_MEMBER;
            let delegated = Grant::new(
                format!("bob-{action}"),
                Grantee::person(BOB),
                roles::governance_action("r", action),
                KindSet::OUTSIDE,
                Answer::Allow,
                Grantee::person(ALICE),
            );
            let owner = Authority::person(workspace.party(ALICE));
            let mut next = workspace.clone();
            next.apply(ALICE, &owner, Change::Grant(delegated), &cx(), false)
                .expect("owner grants");
            workspace = next;
        }
        let bob = Authority::person(workspace.party(BOB));
        workspace
            .check_admission(&bob, Role::Editor, &cx(), false)
            .expect("editor invites editor");
        let refused = workspace.check_admission(&bob, Role::Owner, &cx(), false);
        assert_eq!(refused, Err(Refusal::OwnerRole));
        let add = Change::AddMember {
            person: CAROL.into(),
            role: Role::Commenter,
        };
        let mut next = workspace.clone();
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

        // A commenter given add-member still cannot invite an editor.
        let mut workspace = workspace_with_members(&[(CAROL, Role::Commenter)]);
        workspace.grants.push(Grant::new(
            "carol-invite",
            Grantee::person(CAROL),
            roles::governance_action("r", governance::ADD_MEMBER),
            KindSet::OUTSIDE,
            Answer::Allow,
            Grantee::person(ALICE),
        ));
        let carol = Authority::person(workspace.party(CAROL));
        assert_eq!(
            workspace.check_admission(&carol, Role::Editor, &cx(), false),
            Err(Refusal::AboveActorRole(Role::Editor))
        );
        workspace
            .check_admission(&carol, Role::Viewer, &cx(), false)
            .expect("commenter invites viewer");

        // A workspace that denies its Editors outside effects denies them to
        // whoever is handed Editor too: an Editor there still invites one,
        // and no Moderator.
        let mut workspace = workspace_with_members(&[(BOB, Role::Editor)]);
        workspace.grants.extend([
            Grant::new(
                "editors-no-outside",
                Grantee::role(Role::Editor),
                TreePath::workspace("r"),
                KindSet::OUTSIDE,
                Answer::Deny,
                Grantee::person(ALICE),
            ),
            Grant::new(
                "editors-invite",
                Grantee::role(Role::Editor),
                roles::governance_action("r", governance::ADD_MEMBER),
                KindSet::OUTSIDE,
                Answer::Allow,
                Grantee::person(ALICE),
            ),
        ]);
        let bob = Authority::person(workspace.party(BOB));
        workspace
            .check_admission(&bob, Role::Editor, &cx(), false)
            .expect("an Editor held to the workspace's Deny invites an Editor");
        assert_eq!(
            workspace.check_admission(&bob, Role::Moderator, &cx(), false),
            Err(Refusal::AboveActorRole(Role::Moderator))
        );
    }

    phase a_policy_moves_to_another_workspace_id;
    {
        let mut workspace = workspace_with_members(&[(BOB, Role::Editor)]);
        workspace.grants.push(Grant::new(
            "elsewhere",
            alice(),
            TreePath::workspace("s"),
            KindSet::ALL,
            Answer::Allow,
            alice(),
        ));
        workspace.rehome("q");
        assert_eq!(workspace.workspace, "q");
        let bob = Authority::person(workspace.party(BOB));
        let moved = TreePath::workspace("q").app("mail").action("archive");
        assert_eq!(
            bob.decide(&moved, ActionKind::Undoable, &cx()).answer,
            Answer::Allow
        );
        assert_eq!(
            bob.decide(&archive(), ActionKind::Undoable, &cx()).answer,
            Answer::Deny
        );
        assert!(
            workspace
                .grants
                .iter()
                .any(|grant| grant.node == TreePath::workspace("s"))
        );
        let unowned = WorkspacePolicy::unowned("u", &alice());
        assert_eq!(unowned.owner, None);
        assert_eq!(
            unowned.grants.len(),
            roles::workspace_role_grants("u", &alice()).len()
        );
    }

    phase a_blocked_member_holds_nothing_but_stays_on_record;
    {
        let mut workspace = workspace_with_members(&[(BOB, Role::Editor)]);
        assert_eq!(workspace.active_role(BOB), Some(Role::Editor));
        let block = workspace.block(BOB, alice());
        workspace.grants.push(block);
        assert!(workspace.is_blocked(BOB));
        assert_eq!(workspace.role_of(BOB), Some(Role::Editor));
        assert_eq!(workspace.active_role(BOB), None);
        let bob = Authority::person(workspace.party(BOB));
        for kind in ActionKind::ALL {
            assert_eq!(bob.decide(&send(), kind, &cx()).answer, Answer::Deny);
        }
        let owner_block = workspace.block(ALICE, alice());
        workspace.grants.push(owner_block);
        assert!(!workspace.is_blocked(ALICE), "the owner is never blocked");
    }

    phase an_access_carries_the_person_and_every_bound;
    {
        use tilo_action_policy::MemberAccess;
        let workspace = workspace_with_members(&[(BOB, Role::Editor)]);
        let relative = TreePath::root().app("mail").action("send");
        let bob = MemberAccess::person(workspace.clone(), BOB);
        assert_eq!(bob.node(&relative), mail().action("send"));
        assert_eq!(
            bob.answer(&bob.node(&relative), ActionKind::Outside, &cx()),
            Answer::Ask
        );
        let agent = MemberAccess::new(
            workspace.clone(),
            Acting::agent("ag", BOB).acting_for(Acting::person(BOB)),
        )
        .granted(roles::agent_default(&[mail()]).assign(
            "ag",
            &Grantee::agent("ag", BOB),
            &alice(),
            &Lifetime::UntilRevoked,
        ));
        assert_eq!(agent.people(), vec![BOB]);
        assert_eq!(
            agent.answer(&archive(), ActionKind::Undoable, &cx()),
            Answer::Allow
        );
        let notes = TreePath::workspace("r").app("notes").action("append");
        assert_eq!(
            agent.answer(&notes, ActionKind::Undoable, &cx()),
            Answer::Deny
        );
        let owner = MemberAccess::person(workspace, ALICE).on_foreign_device();
        let invite = roles::governance_action("r", governance::ADD_MEMBER);
        assert_eq!(
            owner.answer(&invite, roles::GOVERNANCE_KIND, &cx()),
            Answer::Deny
        );
        assert_eq!(
            owner.answer(&send(), ActionKind::Outside, &cx()),
            Answer::Ask
        );
        let json = serde_json::to_string(&owner).unwrap();
        assert_eq!(serde_json::from_str::<MemberAccess>(&json).unwrap(), owner);
    }

    phase a_member_leaves_their_workspace_without_being_able_to_remove_anyone;
    // Leaving is the one membership change no grant checks: whoever removes
    // themselves does it by being who they are, even while restricted, and they
    // reach nobody else by it.
    {
        let mut workspace = workspace_with_members(&[(BOB, Role::Viewer), (CAROL, Role::Editor)]);
        workspace.grants.push(roles::restriction_grant(
            "r",
            BOB,
            NOW + 1_000,
            Grantee::person(ALICE),
        ));
        let bob = Authority::person(workspace.party(BOB).clone());
        let leave = Change::RemoveMember { person: BOB.into() };
        assert_eq!(workspace.check(BOB, &bob, &leave, &cx(), false), Ok(()));
        assert!(workspace.is_member(BOB), "check writes nothing");
        let mut left = workspace.clone();
        left.apply(BOB, &bob, leave, &cx(), false)
            .expect("a restricted viewer leaves");
        assert!(!left.is_member(BOB));
        assert!(
            matches!(
                left.apply(
                    BOB,
                    &bob,
                    Change::RemoveMember {
                        person: CAROL.into()
                    },
                    &cx(),
                    false
                ),
                Err(Refusal::NotPermitted(_))
            ),
            "leaving reaches no one else"
        );
    }

    phase removing_a_member_drops_their_grants;
    {
        let mut workspace = workspace_with_members(&[(BOB, Role::Viewer)]);
        workspace.grants.push(Grant::new(
            "bob-mail",
            Grantee::person(BOB),
            mail(),
            KindSet::ALL,
            Answer::Allow,
            alice(),
        ));
        let owner = Authority::person(workspace.party(ALICE));
        let mut next = workspace.clone();
        next.apply(
            ALICE,
            &owner,
            Change::RemoveMember { person: BOB.into() },
            &cx(),
            false,
        )
        .expect("remove");
        workspace = next;
        assert!(!workspace.is_member(BOB));
        assert!(
            workspace
                .grants
                .iter()
                .all(|grant| grant.grantee != Grantee::person(BOB))
        );
    }
}

tilo_test_env::scenario! {
    fn policies_and_grants_read_back_as_written;

    phase paths_round_trip_through_text;
    {
        let paths = [
            TreePath::root(),
            send(),
            TreePath::workspace("r/1%").app("*").kind("a:b"),
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
            "workspace:r/app:mail/kind:mail.message/resource:m1/action:send"
        );
        assert_eq!(TreePath::workspace("r/1%").to_string(), "workspace:r%2F1%25");
        assert!("workspace".parse::<TreePath>().is_err());
        assert!("planet:x".parse::<TreePath>().is_err());
        assert!("workspace:".parse::<TreePath>().is_err());
        let primary: TreePath = "workspace:r/app:mail/resource:".parse().unwrap();
        assert_eq!(primary, mail().resource(""));
        assert!(!primary.covers(&mail().resource("other")));
        assert!(!primary.covers(&mail().any(Level::Resource)));
        assert_eq!(primary.to_string(), "workspace:r/app:mail/resource:");
        assert!("workspace:a%zz".parse::<TreePath>().is_err());
    }

    phase grants_round_trip_through_json;
    {
        let grant = grant(
            "g",
            mail().any(Level::Resource),
            KindSet::READ_UNDOABLE,
            Answer::Ask,
        )
        .with_lifetime(Lifetime::Until(5));
        let json = serde_json::to_value(&grant).expect("serialises");
        assert_eq!(json["node"], "workspace:r/app:mail/resource:*");
        assert_eq!(json["kinds"], serde_json::json!(["read", "undoable"]));
        assert_eq!(json["answer"], "ask");
        let back: Grant = serde_json::from_value(json).expect("deserialises");
        assert_eq!(back, grant);
        let workspace = WorkspacePolicy::new("r", ALICE);
        let text = serde_json::to_string(&workspace).expect("serialises");
        assert_eq!(
            serde_json::from_str::<WorkspacePolicy>(&text).expect("deserialises"),
            workspace
        );
    }
}

struct Names;

impl Labels for Names {
    fn label(&self, level: Level, id: &str) -> Option<String> {
        match (level, id) {
            (Level::App, "mail") => Some("Mail".into()),
            (Level::Action, "send") => Some("Send".into()),
            (Level::Workspace, "r") => Some("Project workspace".into()),
            _ => None,
        }
    }

    fn everything(&self) -> String {
        "Everything".to_owned()
    }
}

tilo_test_env::scenario! {
    fn grants_and_verdicts_read_as_sentences;

    phase grant_sets_read_as_sentences;
    {
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
                TreePath::workspace("r").app("snippets"),
                KindSet::ALL,
                Answer::Allow,
            ),
            grant(
                "workspace",
                TreePath::workspace("r"),
                KindSet::READ,
                Answer::Allow,
            )
            .with_lifetime(Lifetime::Session),
        ];
        assert_eq!(
            describe(&grants, &Names),
            "Project workspace (this session): reading allowed. \
         Project workspace › Mail: reading and undoable changes allowed, outside effects ask first. \
         Project workspace › Mail › anywhere › Send: everything asks first. \
         Project workspace › snippets: everything allowed."
        );
        assert_eq!(describe(&[], &Ids), "Nothing is granted.");
        assert_eq!(describe_node(&TreePath::root(), &Ids), "Everything");
        assert_eq!(
            describe_node(&mail().any(Level::Resource), &Ids),
            "workspace r › mail › every resource"
        );
        let preset = roles::role_preset(Role::Editor, "r");
        let editor = preset.assign(
            "e",
            &Grantee::role(Role::Editor),
            &alice(),
            &Lifetime::UntilRevoked,
        );
        assert_eq!(
            describe(&editor, &Ids),
            "workspace r: reading and undoable changes allowed, outside effects ask first. \
         workspace r › so.tilo.browser-desktop › desktop.person-view: undoable changes not allowed. \
         workspace r › so.tilo.ownership: everything not allowed. \
         workspace r › tilo.workspace: undoable changes and outside effects not allowed. \
         workspace r › anywhere › moderation: undoable changes and outside effects not allowed."
        );
    }

    phase a_grant_set_reads_as_a_sentence_a_person_can_check;
    {
        let grants = [
            grant(
                "workspace-read",
                TreePath::workspace("r"),
                KindSet::READ,
                Answer::Allow,
            ),
            grant("mail", mail(), KindSet::READ_UNDOABLE, Answer::Allow),
            grant(
                "mail-send",
                mail().rest().action("send"),
                KindSet::OUTSIDE,
                Answer::Ask,
            ),
            grant(
                "finance",
                TreePath::workspace("r").app("finance"),
                KindSet::ALL,
                Answer::Deny,
            ),
        ];
        assert_eq!(
            sentences(&grants, &Names),
            "Project workspace › Mail › anywhere › Send: outside effects ask first. \
         Project workspace › Mail: reading and undoable changes allowed. \
         Project workspace › finance: everything not allowed. Tilo keeps it from them, but it is \
         not encrypted apart: their devices hold the workspace's keys, so only the workspace \
         itself keeps content from someone. \
         Project workspace: reading allowed."
        );
        assert!(
            !grant("w", TreePath::workspace("r"), KindSet::READ, Answer::Deny)
                .denies_reading_within_workspace(),
            "refusing the whole workspace is its boundary"
        );
        assert!(
            !grant("o", mail(), KindSet::OUTSIDE, Answer::Deny).denies_reading_within_workspace(),
            "only refusing to read is told apart"
        );
        assert_eq!(sentences(&[], &Ids), "Nothing is granted.");
        let viewer = roles::role_preset(Role::Viewer, "r").assign(
            "v",
            &Grantee::role(Role::Viewer),
            &alice(),
            &Lifetime::UntilRevoked,
        );
        assert_eq!(
            sentences(&viewer, &Ids),
            "workspace r › so.tilo.ownership: everything not allowed. Tilo keeps it from them, \
         but it is not encrypted apart: their devices hold the workspace's keys, so only the \
         workspace itself keeps content from someone. workspace r: reading allowed."
        );
        let owner = roles::role_preset(Role::Owner, "r").assign(
            "o",
            &Grantee::role(Role::Owner),
            &alice(),
            &Lifetime::UntilRevoked,
        );
        assert_eq!(sentences(&owner, &Ids), "workspace r: everything allowed.");
    }

    phase verdicts_explain_themselves;
    {
        let grants = [grant("mail", mail(), KindSet::ALL, Answer::Ask)];
        let verdict = Authority::person(Party::new(vec![alice()], &grants)).decide(
            &send(),
            ActionKind::Outside,
            &cx(),
        );
        assert_eq!(
            verdict.explain(),
            "Asks first: the person's grants give outside effects on workspace:r/app:mail the answer \"ask\" (grant mail)."
        );
        let workspace = WorkspacePolicy::new("r", ALICE);
        let verdict =
            Authority::person(workspace.party(ALICE)).decide(&send(), ActionKind::Outside, &cx());
        assert_eq!(verdict.explain(), "Allowed: the person owns the workspace.");
    }
}

tilo_test_env::scenario! {
    fn granting_is_an_administrators_action;

    phase grant_requires_grant_action;
    // Granting is a governance action on the workspace's own resource: whoever is
    // not granted it grants nothing, and whoever is may give no more than they
    // hold.
    {
        let workspace = workspace_with_members(&[(BOB, Role::Editor)]);
        let edit = Grant::new(
            "carol-mail",
            Grantee::person(CAROL),
            mail(),
            KindSet::UNDOABLE,
            Answer::Allow,
            alice(),
        );
        let grant_node = roles::governance_action("r", governance::GRANT);
        let bob = Authority::person(workspace.party(BOB));
        assert_eq!(
            bob.decide(&grant_node, roles::GOVERNANCE_KIND, &cx())
                .answer,
            Answer::Deny
        );
        let refused = workspace
            .clone()
            .apply(BOB, &bob, Change::Grant(edit.clone()), &cx(), false);
        assert!(
            matches!(refused, Err(Refusal::NotPermitted(_))),
            "{refused:?}"
        );
        assert!(
            workspace.grants.iter().all(|held| held.id != edit.id),
            "a refused grant writes nothing"
        );

        // The owner may grant, and the grant is signed by whoever gave it.
        let owner = Authority::person(workspace.party(ALICE));
        let mut granted = workspace.clone();
        granted
            .apply(ALICE, &owner, Change::Grant(edit.clone()), &cx(), false)
            .expect("the owner grants");
        let recorded = granted
            .grants
            .iter()
            .find(|held| held.id == edit.id)
            .expect("recorded");
        assert_eq!(recorded.granted_by, Grantee::person(ALICE));
        assert_eq!(
            Authority::person(granted.party(CAROL))
                .decide(&archive(), ActionKind::Undoable, &cx())
                .answer,
            Answer::Allow
        );

        // Granted the workspace's governance alone, a viewer still hands out no more
        // than they hold: a governance grant is not a grant of the content.
        let mut narrow = workspace_with_members(&[(BOB, Role::Viewer)]);
        narrow
            .grants
            .push(roles::administrator_grant("r", BOB, alice()));
        let bob = Authority::person(narrow.party(BOB));
        assert_eq!(
            bob.decide(&grant_node, roles::GOVERNANCE_KIND, &cx())
                .answer,
            Answer::Allow
        );
        let refused = narrow.clone().apply(
            BOB,
            &bob,
            Change::Grant(Grant::new(
                "bob-gives-edit",
                Grantee::person(CAROL),
                mail(),
                KindSet::UNDOABLE,
                Answer::Allow,
                alice(),
            )),
            &cx(),
            false,
        );
        assert!(
            matches!(refused, Err(Refusal::AboveActorGrant { .. })),
            "{refused:?}"
        );
    }

    phase ask_needs_a_approved_approval;
    {
        let mut workspace = workspace_with_members(&[(BOB, Role::Editor)]);
        workspace.grants.push(Grant::new(
            "bob-invite",
            Grantee::person(BOB),
            roles::governance_action("r", governance::ADD_MEMBER),
            KindSet::OUTSIDE,
            Answer::Ask,
            Grantee::person(ALICE),
        ));
        let bob = Authority::person(workspace.party(BOB));
        let refused = workspace.check_admission(&bob, Role::Viewer, &cx(), false);
        assert!(
            matches!(refused, Err(Refusal::NeedsApproval(_))),
            "{refused:?}"
        );
        workspace
            .check_admission(&bob, Role::Viewer, &cx(), true)
            .expect("approved");
    }

    phase nobody_grants_more_than_they_hold;
    {
        let mut workspace = workspace_with_members(&[(BOB, Role::Editor)]);
        workspace.grants.push(Grant::new(
            "bob-grant",
            Grantee::person(BOB),
            roles::governance_action("r", governance::GRANT),
            KindSet::OUTSIDE,
            Answer::Allow,
            Grantee::person(ALICE),
        ));
        let bob = Authority::person(workspace.party(BOB));
        let to_carol = |kinds, answer| {
            Change::Grant(Grant::new(
                "to-carol",
                Grantee::person(CAROL),
                mail(),
                kinds,
                answer,
                Grantee::person(BOB),
            ))
        };
        // Bob, an editor, holds Ask for outside effects.
        assert_eq!(
            workspace.clone().apply(
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
        workspace
            .clone()
            .apply(
                BOB,
                &bob,
                to_carol(KindSet::OUTSIDE, Answer::Ask),
                &cx(),
                false,
            )
            .expect("ask is within ask");
        workspace
            .clone()
            .apply(
                BOB,
                &bob,
                to_carol(KindSet::ALL, Answer::Deny),
                &cx(),
                false,
            )
            .expect("a deny never exceeds");
        let mut granted = workspace.clone();
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
        // Writing their grant again with other terms takes the one they gave back, which Bob,
        // holding no revoke, may not.
        let again = to_carol(KindSet::READ, Answer::Allow);
        let refused = granted.clone().apply(BOB, &bob, again, &cx(), false);
        assert!(matches!(refused, Err(Refusal::NotPermitted(_))), "{refused:?}");
        // Nobody writes over a grant someone else gave.
        let over = Change::Grant(Grant::new(
            "to-carol",
            Grantee::person(CAROL),
            mail(),
            KindSet::READ,
            Answer::Allow,
            Grantee::person(ALICE),
        ));
        let alice = Authority::person(granted.party(ALICE));
        assert_eq!(
            granted.clone().apply(ALICE, &alice, over, &cx(), false),
            Err(Refusal::DuplicateGrant(GrantId::new("to-carol")))
        );
        let elsewhere = Change::Grant(Grant::new(
            "elsewhere",
            Grantee::person(CAROL),
            TreePath::workspace("s"),
            KindSet::READ,
            Answer::Allow,
            Grantee::person(BOB),
        ));
        assert_eq!(
            granted.clone().apply(BOB, &bob, elsewhere, &cx(), false),
            Err(Refusal::OutsideWorkspace(TreePath::workspace("s")))
        );
    }

    phase a_giver_changing_their_mind_replaces_their_grant_in_one_change;
    {
        let workspace = workspace_with_members(&[(BOB, Role::Editor)]);
        let owner = Authority::person(workspace.party(ALICE));
        let to_app = |answer, lifetime| {
            Change::Grant(
                Grant::new(
                    "mail-app",
                    Grantee::app("mail"),
                    mail(),
                    KindSet::ALL,
                    answer,
                    Grantee::person(ALICE),
                )
                .with_lifetime(lifetime),
            )
        };
        let held = |workspace: &WorkspacePolicy| {
            workspace
                .grants
                .iter()
                .filter(|grant| grant.id.as_str() == "mail-app")
                .map(|grant| (grant.answer, grant.lifetime))
                .collect::<Vec<_>>()
        };
        // Declined, then allowed where the app's permissions are.
        let mut changed = workspace.clone();
        changed
            .apply(ALICE, &owner, to_app(Answer::Deny, Lifetime::UntilRevoked), &cx(), false)
            .expect("the decline");
        changed
            .apply(ALICE, &owner, to_app(Answer::Allow, Lifetime::UntilRevoked), &cx(), false)
            .expect("the decline replaced");
        assert_eq!(held(&changed), vec![(Answer::Allow, Lifetime::UntilRevoked)]);
        // Allowed once, and before it is used, allowed for good.
        changed
            .apply(ALICE, &owner, Change::Revoke(GrantId::new("mail-app")), &cx(), false)
            .expect("taken back");
        changed
            .apply(ALICE, &owner, to_app(Answer::Allow, Lifetime::Once), &cx(), false)
            .expect("allowed once");
        changed
            .apply(ALICE, &owner, to_app(Answer::Allow, Lifetime::UntilRevoked), &cx(), false)
            .expect("the once replaced");
        assert_eq!(held(&changed), vec![(Answer::Allow, Lifetime::UntilRevoked)]);
        // The same grant again changes nothing.
        let before = changed.clone();
        changed
            .apply(ALICE, &owner, to_app(Answer::Allow, Lifetime::UntilRevoked), &cx(), false)
            .expect("the same grant converges");
        assert_eq!(changed, before);
    }

    phase grant_and_revoke_are_actions;
    {
        let mut workspace = workspace_with_members(&[(BOB, Role::Viewer)]);
        let owner = Authority::person(workspace.party(ALICE));
        let edit = Grant::new(
            "bob-mail",
            Grantee::person(BOB),
            mail(),
            KindSet::UNDOABLE,
            Answer::Allow,
            Grantee::person(CAROL),
        );
        let mut next = workspace.clone();
        next.apply(ALICE, &owner, Change::Grant(edit), &cx(), false)
            .expect("grant");
        workspace = next;
        let bob = Authority::person(workspace.party(BOB));
        assert_eq!(
            bob.decide(&archive(), ActionKind::Undoable, &cx()).answer,
            Answer::Allow
        );
        let owner = Authority::person(workspace.party(ALICE));
        let revoke = Change::Revoke(GrantId::new("bob-mail"));
        workspace
            .clone()
            .apply(ALICE, &owner, revoke.clone(), &cx(), false)
            .expect("revoke");
        let mut revoked = workspace.clone();
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
        let bob = Authority::person(workspace.party(BOB));
        let refused = workspace.clone().apply(
            BOB,
            &bob,
            Change::Revoke(GrantId::new("bob-mail")),
            &cx(),
            false,
        );
        assert!(matches!(refused, Err(Refusal::NotPermitted(_))));
    }

    phase a_governance_change_spends_the_once_grant_it_ran_on;
    {
        let mut workspace = WorkspacePolicy::new("r", CAROL);
        workspace.members.insert(ALICE.into(), Role::Editor);
        let once = Grant::new(
            "grant-once",
            alice(),
            roles::governance_action("r", governance::GRANT),
            KindSet::of(roles::GOVERNANCE_KIND),
            Answer::Allow,
            Grantee::person(CAROL),
        )
        .with_lifetime(Lifetime::Once);
        workspace.grants.push(once);
        let denial = |id: &str| {
            Change::Grant(Grant::new(
                id,
                Grantee::agent("ag", ALICE),
                mail(),
                KindSet::ALL,
                Answer::Deny,
                alice(),
            ))
        };
        let before = workspace.clone();
        let authority = Authority::person(before.party(ALICE));
        workspace
            .apply(ALICE, &authority, denial("first"), &cx(), false)
            .expect("allowed once");
        assert!(
            workspace
                .grants
                .iter()
                .all(|grant| grant.id.as_str() != "grant-once"),
            "the grant given for one use is spent"
        );
        let after = workspace.clone();
        let authority = Authority::person(after.party(ALICE));
        assert!(matches!(
            workspace.apply(ALICE, &authority, denial("second"), &cx(), false),
            Err(Refusal::NotPermitted(_))
        ));
    }

    phase a_refused_run_spends_nothing;
    {
        let grants =
            [grant("once", send(), KindSet::READ, Answer::Allow).with_lifetime(Lifetime::Once)];
        let authority = Authority::person(Party::new(vec![alice()], &grants));
        assert!(
            authority
                .decide(&send(), ActionKind::Outside, &cx())
                .spent()
                .is_empty()
        );
        assert_eq!(
            authority.decide(&send(), ActionKind::Read, &cx()).spent(),
            vec![GrantId::new("once")]
        );
    }
}

tilo_test_env::scenario! {
    fn an_agent_or_device_is_bounded_by_its_person;

    phase delegation_is_the_minimum_of_person_and_delegation;
    {
        for person_answer in ANSWERS {
            for agent_answer in ANSWERS {
                let grants = [
                    grant("person", mail(), KindSet::ALL, person_answer),
                    Grant::new(
                        "agent",
                        Grantee::agent("ag", ALICE),
                        mail(),
                        KindSet::ALL,
                        agent_answer,
                        alice(),
                    ),
                ];
                let authority = Authority::person(Party::new(vec![alice()], &grants))
                    .bounded_by(Party::new(vec![Grantee::agent("ag", ALICE)], &grants));
                let verdict = authority.decide(&send(), ActionKind::Outside, &cx());
                assert_eq!(verdict.answer, person_answer.min(agent_answer));
                assert_eq!(verdict.parts.len(), 2);
            }
        }
    }

    phase an_agent_never_does_more_than_its_person;
    {
        let grants = [
            grant("person", mail(), KindSet::READ, Answer::Allow),
            Grant::new(
                "agent",
                Grantee::agent("ag", ALICE),
                TreePath::root(),
                KindSet::ALL,
                Answer::Allow,
                alice(),
            ),
        ];
        let authority = Authority::person(Party::new(vec![alice()], &grants))
            .bounded_by(Party::new(vec![Grantee::agent("ag", ALICE)], &grants));
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
            .bounded_by(Party::new(vec![Grantee::agent("ag", ALICE)], &restricted));
        assert_eq!(
            authority.decide(&send(), ActionKind::Read, &cx()).answer,
            Answer::Deny
        );
    }

    phase every_bound_applies;
    {
        let grants = [
            grant("person", mail(), KindSet::ALL, Answer::Allow),
            Grant::new(
                "agent",
                Grantee::agent("ag", ALICE),
                mail(),
                KindSet::READ_UNDOABLE,
                Answer::Allow,
                alice(),
            ),
        ];
        let authority = Authority::person(Party::new(vec![alice()], &grants))
            .bounded_by(Party::new(vec![Grantee::agent("ag", ALICE)], &grants));
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
                .starts_with("Not allowed: the grants of those acting for them"),
            "{}",
            verdict.explain()
        );
    }

    phase the_agent_default_reads_and_changes_but_asks_outside;
    {
        let preset = roles::agent_default(&[mail()]);
        preset.validate().expect("valid");
        let grants = preset.assign(
            "agent:ag",
            &Grantee::agent("ag", ALICE),
            &alice(),
            &Lifetime::UntilRevoked,
        );
        let workspace = workspace_with_members(&[(BOB, Role::Editor)]);
        let authority = Authority::person(workspace.party(ALICE))
            .bounded_by(Party::new(vec![Grantee::agent("ag", ALICE)], &grants));
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
        let notes = TreePath::workspace("r").app("notes").action("append");
        assert_eq!(
            authority.decide(&notes, ActionKind::Undoable, &cx()).answer,
            Answer::Deny
        );
        // The agent of an editor asks no more than the editor does: Outside is Ask either way.
        let bob = Authority::person(workspace.party(BOB))
            .bounded_by(Party::new(vec![Grantee::agent("ag", ALICE)], &grants));
        assert_eq!(
            bob.decide(&send(), ActionKind::Outside, &cx()).answer,
            Answer::Ask
        );
    }

    phase a_turn_for_another_member_and_its_sub_agent_do_what_both_people_may;
    {
        // Bob's agent answers Carol: Bob may not touch mail, Carol may.
        let mut workspace = workspace_with_members(&[(BOB, Role::Editor), (CAROL, Role::Editor)]);
        workspace.grants.push(Grant::new(
            "bob-no-mail",
            Grantee::person(BOB),
            mail(),
            KindSet::ALL,
            Answer::Deny,
            alice(),
        ));
        let decided = |acting: &Acting, node: &TreePath| {
            let granted = acting.handed_preset("r");
            Authority::of_acting(acting, &workspace, &granted)
                .decide(node, ActionKind::Undoable, &cx())
                .answer
        };
        let for_carol = Acting::handed_agent("scout", CAROL);
        let answering_carol = for_carol.clone().agent_of(BOB);
        let sub_agent = answering_carol.clone().handed_on("helper").expect("an agent starts it");
        assert_eq!(decided(&for_carol, &archive()), Answer::Allow);
        for (who, acting) in [("the turn", &answering_carol), ("its sub-agent", &sub_agent)] {
            assert_eq!(
                decided(acting, &archive()),
                Answer::Deny,
                "{who} does nothing its agent's person may not: {acting:?}"
            );
            let notes = TreePath::workspace("r").app("notes").action("append");
            assert_eq!(
                decided(acting, &notes),
                Answer::Allow,
                "and is not refused for being Bob's agent: {acting:?}"
            );
        }
    }

    phase a_foreign_device_never_governs;
    {
        let workspace = workspace_with_members(&[]);
        let authority = Authority::person(
            workspace
                .party(ALICE)
                .capped(tilo_action_policy::Cap::FOREIGN_DEVICE),
        );
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
        let invite = roles::governance_action("r", governance::ADD_MEMBER);
        let refused = authority.decide(&invite, roles::GOVERNANCE_KIND, &cx());
        assert_eq!(refused.answer, Answer::Deny);
        assert!(
            refused
                .explain()
                .starts_with("Not allowed: this computer is not one of the person's own"),
            "a refusal says why in words: {}",
            refused.explain()
        );
        assert_eq!(
            authority
                .decide(&roles::workspace_resource("r"), ActionKind::Read, &cx())
                .answer,
            Answer::Allow
        );
    }

    phase a_privileged_action_needs_the_persons_own_allow;
    {
        let agent = || Grantee::agent("ag", ALICE);
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

    phase what_a_receiver_admits_is_allowed_by_the_persons_own_grants;
    // Receivers admit only what the author's grants allow outright, so the
    // person's own grants allow it too: a privileged action is never admitted
    // on receipt on an Ask, whatever the delegations say.
    {
        for person in ANSWERS {
            for delegation in ANSWERS {
                let grants = [
                    grant("person", mail(), KindSet::ALL, person),
                    Grant::new(
                        "agent",
                        Grantee::agent("ag", ALICE),
                        mail(),
                        KindSet::ALL,
                        delegation,
                        alice(),
                    ),
                ];
                let verdict = Authority::person(Party::new(vec![alice()], &grants))
                    .bounded_by(Party::new(vec![Grantee::agent("ag", ALICE)], &grants))
                    .decide(&send(), ActionKind::Outside, &cx());
                if verdict.answer == Answer::Allow {
                    assert_eq!(verdict.clone().privileged().answer, Answer::Allow);
                }
            }
        }
    }

    phase an_agent_delegation_bounds_its_person_by_the_editor_preset;
    // A delegated agent acts with its person's grants bounded by its
    // delegation, the Editor preset: an owner's agent changes the workspace but does
    // not govern it, and a viewer's agent changes nothing.
    {
        use tilo_action_policy::MemberAccess;
        let workspace = workspace_with_members(&[(BOB, Role::Viewer)]);
        let resources = TreePath::workspace("r").app("notes").action("append");
        let set_role = roles::governance_action("r", governance::SET_ROLE);
        let owner_agent = MemberAccess::new(
            workspace.clone(),
            Acting::agent("scribe", ALICE).acting_for(Acting::person(ALICE)),
        )
        .granted(tilo_member_types::Acting::handed_agent("scribe", ALICE).handed_preset("r"));
        assert_eq!(
            owner_agent.answer(&resources, ActionKind::Undoable, &cx()),
            Answer::Allow
        );
        assert_eq!(
            owner_agent.answer(&set_role, roles::GOVERNANCE_KIND, &cx()),
            Answer::Deny
        );
        assert_eq!(
            MemberAccess::person(workspace.clone(), ALICE).answer(&set_role, roles::GOVERNANCE_KIND, &cx()),
            Answer::Allow,
            "the owner administers"
        );
        let viewer_agent = MemberAccess::new(
            workspace,
            Acting::agent("scribe", BOB).acting_for(Acting::person(BOB)),
        )
        .granted(tilo_member_types::Acting::handed_agent("scribe", BOB).handed_preset("r"));
        assert_eq!(
            viewer_agent.answer(&resources, ActionKind::Undoable, &cx()),
            Answer::Deny
        );
        assert_eq!(
            viewer_agent.answer(&resources, ActionKind::Read, &cx()),
            Answer::Allow
        );
    }

    phase each_role_holds_the_grants_of_those_it_hands_out;
    // A role hands out another when each of that role's grants is within its
    // own: the built-in roles nest, Viewer to Owner, and none holds a wider one.
    {
        for (index, held) in Role::ALL.into_iter().enumerate() {
            for (other, wanted) in Role::ALL.into_iter().enumerate() {
                assert_eq!(held.includes(wanted), other <= index, "{held} / {wanted}");
            }
        }
    }

    phase an_agents_role_never_exceeds_its_person_or_the_cap;
    // An agent acts as its person, within the agent cap, and never administers.
    {
        use roles::agent_policy;
        for person in Role::ALL {
            let role = agent_policy::role(person);
            assert!(person.includes(role), "{person} gave {role}");
            assert!(agent_policy::ROLE_CAP.includes(role), "{person} gave {role}");
        }
        assert_eq!(agent_policy::role(Role::Owner), Role::Editor);
        assert_eq!(agent_policy::role(Role::Commenter), Role::Commenter);
    }
}

fn remove_message() -> TreePath {
    TreePath::workspace("r")
        .app("chat")
        .kind("chat.workspace")
        .resource("c1")
        .field(roles::MODERATION_FIELD)
        .action("remove-message")
}

tilo_test_env::scenario! {
    fn moderation_reaches_only_below_the_moderator;

    phase only_moderators_and_the_owner_reach_the_moderation_node;
    {
        let workspace = workspace_with_members(&[
            (BOB, Role::Editor),
            (CAROL, Role::Moderator),
            ("v", Role::Viewer),
        ]);
        let answer_of = |person: &str| {
            Authority::person(workspace.party(person))
                .decide(&remove_message(), ActionKind::Undoable, &cx())
                .answer
        };
        assert_eq!(answer_of(ALICE), Answer::Allow);
        assert_eq!(answer_of(CAROL), Answer::Allow);
        assert_eq!(answer_of(BOB), Answer::Deny);
        assert_eq!(answer_of("v"), Answer::Deny);
        let content = mail().kind("mail.message").resource("m1").action("archive");
        let carol = Authority::person(workspace.party(CAROL));
        assert_eq!(
            carol.decide(&content, ActionKind::Undoable, &cx()).answer,
            Answer::Allow,
            "a Moderator edits like an Editor"
        );
        for action in [
            governance::SET_ROLE,
            governance::GRANT,
            governance::SETTINGS,
        ] {
            assert!(
                workspace.check_action(&carol, action, &cx(), true).is_err(),
                "a Moderator does not govern beyond membership: {action}"
            );
        }
        for action in roles::MODERATOR_GOVERNANCE {
            assert_eq!(workspace.check_action(&carol, action, &cx(), false), Ok(()));
        }
    }

    phase membership_changes_are_decided_by_grants_and_keep_the_owner;
    {
        let mut workspace = workspace_with_members(&[
            (BOB, Role::Editor),
            (CAROL, Role::Moderator),
            ("d0d", Role::Moderator),
        ]);
        let policy = workspace.clone();
        let carol = Authority::person(policy.party(CAROL));
        // The workspace keeps its owner, whoever asks.
        for change in [
            Change::RemoveMember {
                person: ALICE.into(),
            },
            Change::Ban {
                person: ALICE.into(),
            },
        ] {
            let before = workspace.clone();
            assert_eq!(
                workspace.apply(CAROL, &carol, change, &cx(), false),
                Err(Refusal::Owner)
            );
            assert_eq!(workspace, before, "nothing is written");
        }
        assert_eq!(
            workspace.apply(
                CAROL,
                &carol,
                Change::Ban {
                    person: CAROL.into()
                },
                &cx(),
                false
            ),
            Err(Refusal::Themselves)
        );
        // The Moderator role's grant on removing reaches another Moderator as
        // it reaches an Editor: no role ranks a member out of reach.
        workspace
            .apply(
                CAROL,
                &carol,
                Change::RemoveMember {
                    person: "d0d".into(),
                },
                &cx(),
                false,
            )
            .unwrap();
        assert!(!workspace.is_member("d0d"));

        // An Editor holds no grant on removing, so removes nobody.
        let bob = Authority::person(workspace.party(BOB));
        let remove_carol = Change::RemoveMember {
            person: CAROL.into(),
        };
        assert!(matches!(
            workspace.check(BOB, &bob, &remove_carol, &cx(), true),
            Err(Refusal::NotPermitted(_))
        ));
        // Given an Ask grant on it, the Editor's removal asks first, and an
        // approved one goes ahead, a Moderator's included.
        let owner = workspace.clone();
        let owner = Authority::person(owner.party(ALICE));
        workspace
            .apply(
                ALICE,
                &owner,
                Change::Grant(Grant::new(
                    "bob-remove",
                    Grantee::person(BOB),
                    roles::governance_action("r", governance::REMOVE_MEMBER),
                    KindSet::OUTSIDE,
                    Answer::Ask,
                    Grantee::person(ALICE),
                )),
                &cx(),
                false,
            )
            .unwrap();
        let policy = workspace.clone();
        let bob = Authority::person(policy.party(BOB));
        assert!(matches!(
            workspace.check(BOB, &bob, &remove_carol, &cx(), false),
            Err(Refusal::NeedsApproval(_))
        ));
        workspace
            .apply(BOB, &bob, remove_carol, &cx(), true)
            .unwrap();
        assert!(!workspace.is_member(CAROL));
    }

    phase a_change_naming_software_as_a_person_is_refused;
    {
        // A change arrives from any device: one naming an agent's key, or the
        // retired `person:` spelling, as a person is refused, not applied.
        let mut workspace = workspace_with_members(&[(BOB, Role::Editor), (CAROL, Role::Moderator)]);
        let policy = workspace.clone();
        let carol = Authority::person(policy.party(CAROL));
        for named in ["agent:scout", "person:bob"] {
            let before = workspace.clone();
            assert_eq!(
                workspace.apply(
                    CAROL,
                    &carol,
                    Change::Ban {
                        person: named.into()
                    },
                    &cx(),
                    false
                ),
                Err(Refusal::NotAPerson(named.to_owned()))
            );
            assert_eq!(workspace, before, "nothing is written");
            assert!(!workspace.is_blocked(named));
            assert!(workspace.grantees_of(named).is_empty());
        }
    }

    phase a_ban_removes_and_blocks_until_unban;
    {
        let mut workspace = workspace_with_members(&[(BOB, Role::Editor), (CAROL, Role::Moderator)]);
        let policy = workspace.clone();
        let carol = Authority::person(policy.party(CAROL));
        let ban = Change::Ban { person: BOB.into() };
        assert_eq!(workspace.check(CAROL, &carol, &ban, &cx(), false), Ok(()));
        assert!(workspace.is_member(BOB), "check writes nothing");
        workspace
            .apply(CAROL, &carol, ban.clone(), &cx(), false)
            .unwrap();
        assert!(!workspace.is_member(BOB));
        assert!(workspace.is_banned(BOB));
        assert!(workspace.is_blocked(BOB));
        assert_eq!(
            workspace.apply(CAROL, &carol, ban, &cx(), false),
            Err(Refusal::Banned(BOB.into()))
        );
        let owner_policy = workspace.clone();
        let owner = Authority::person(owner_policy.party(ALICE));
        assert_eq!(
            workspace.apply(
                ALICE,
                &owner,
                Change::AddMember {
                    person: BOB.into(),
                    role: Role::Editor
                },
                &cx(),
                false
            ),
            Err(Refusal::Banned(BOB.into())),
            "a banned person cannot be added back"
        );
        workspace
            .apply(
                CAROL,
                &carol,
                Change::Unban { person: BOB.into() },
                &cx(),
                false,
            )
            .unwrap();
        assert!(!workspace.is_banned(BOB));
        assert_eq!(
            workspace.apply(
                CAROL,
                &carol,
                Change::Unban { person: BOB.into() },
                &cx(),
                false
            ),
            Err(Refusal::NotBanned(BOB.into()))
        );
    }

    phase editors_cannot_ban;
    {
        let workspace = workspace_with_members(&[(BOB, Role::Editor), (CAROL, Role::Viewer)]);
        let bob = Authority::person(workspace.party(BOB));
        let ban = Change::Ban {
            person: CAROL.into(),
        };
        assert!(matches!(
            workspace.check(BOB, &bob, &ban, &cx(), true),
            Err(Refusal::NotPermitted(_))
        ));
    }

    phase a_restriction_stops_changes_until_it_lapses_and_leaves_reading;
    {
        let mut workspace = workspace_with_members(&[(BOB, Role::Editor), (CAROL, Role::Moderator)]);
        let policy = workspace.clone();
        let carol = Authority::person(policy.party(CAROL));
        let until = NOW + 60_000;
        let restrict = Change::Restrict {
            person: BOB.into(),
            until_ms: until,
        };
        workspace
            .apply(CAROL, &carol, restrict, &cx(), false)
            .unwrap();
        assert!(workspace.is_member(BOB), "a restriction is not a removal");
        assert!(!workspace.is_blocked(BOB), "nor a ban");
        assert_eq!(workspace.restricted_until(BOB, &cx()), Some(until));
        let bob = Authority::person(workspace.party(BOB));
        let decide = |at: u64, kind| bob.decide(&archive(), kind, &Context::at(at)).answer;
        assert_eq!(decide(NOW, ActionKind::Undoable), Answer::Deny);
        assert_eq!(decide(NOW, ActionKind::Read), Answer::Allow);
        assert_eq!(decide(until, ActionKind::Undoable), Answer::Allow, "lapsed");
        assert_eq!(workspace.restricted_until(BOB, &Context::at(until)), None);
    }

    phase a_restriction_keeps_the_owner_and_lifts_once;
    {
        let mut workspace = workspace_with_members(&[(BOB, Role::Editor), (CAROL, Role::Moderator)]);
        let policy = workspace.clone();
        let carol = Authority::person(policy.party(CAROL));
        let bob = Authority::person(policy.party(BOB));
        let on = |person: &str| Change::Restrict {
            person: person.into(),
            until_ms: NOW + 1,
        };
        assert!(matches!(
            workspace.check(BOB, &bob, &on(CAROL), &cx(), true),
            Err(Refusal::NotPermitted(_))
        ));
        assert_eq!(
            workspace.check(CAROL, &carol, &on(ALICE), &cx(), false),
            Err(Refusal::Owner)
        );
        assert_eq!(
            workspace.check(CAROL, &carol, &on(CAROL), &cx(), false),
            Err(Refusal::Themselves)
        );
        assert_eq!(
            workspace.check(
                CAROL,
                &carol,
                &Change::Restrict {
                    person: BOB.into(),
                    until_ms: NOW
                },
                &cx(),
                false
            ),
            Err(Refusal::InThePast(NOW))
        );
        workspace
            .apply(CAROL, &carol, on(BOB), &cx(), false)
            .unwrap();
        let lift = Change::Unrestrict { person: BOB.into() };
        workspace
            .apply(CAROL, &carol, lift.clone(), &cx(), false)
            .unwrap();
        assert_eq!(workspace.restricted_until(BOB, &cx()), None);
        assert_eq!(
            workspace.apply(CAROL, &carol, lift, &cx(), false),
            Err(Refusal::NotRestricted(BOB.into()))
        );
    }
}

tilo_test_env::scenario! {
    fn roles_grant_what_the_presets_say;

    phase the_owner_is_structural;
    {
        let mut workspace = WorkspacePolicy::new("r", ALICE);
        workspace.grants.push(grant(
            "deny-owner",
            TreePath::workspace("r"),
            KindSet::ALL,
            Answer::Deny,
        ));
        let authority = Authority::person(workspace.party(ALICE));
        for kind in ActionKind::ALL {
            let verdict = authority.decide(&send(), kind, &cx());
            assert_eq!(verdict.answer, Answer::Allow);
            assert_eq!(verdict.parts[0].basis, Basis::Owner);
        }
        // The owner's delegations still bound what acts for them.
        let agent = Authority::person(workspace.party(ALICE))
            .bounded_by(workspace.party_of_agent("ag", ALICE));
        assert_eq!(
            agent.decide(&send(), ActionKind::Read, &cx()).answer,
            Answer::Deny
        );
        // Outside the workspace the owner holds nothing structural.
        assert_eq!(
            authority
                .decide(&TreePath::workspace("s"), ActionKind::Read, &cx())
                .answer,
            Answer::Deny
        );

        let owner = Authority::person(workspace.party(ALICE));
        let remove = Change::RemoveMember {
            person: ALICE.into(),
        };
        assert_eq!(
            workspace.clone().apply(ALICE, &owner, remove, &cx(), false),
            Err(Refusal::Owner)
        );
        let demote = Change::SetRole {
            person: ALICE.into(),
            role: Role::Viewer,
        };
        assert_eq!(
            workspace.clone().apply(ALICE, &owner, demote, &cx(), false),
            Err(Refusal::Owner)
        );
    }

    phase role_presets_grant_what_the_table_says;
    {
        use ActionKind::{Outside, Read, Undoable};
        use Answer::{Allow, Ask, Deny};
        let workspace = workspace_with_members(&[]);
        let content = mail().kind("mail.message").resource("m1").action("archive");
        let comment = mail()
            .kind("mail.message")
            .resource("m1")
            .field("comments")
            .action("insert");
        let governance = roles::governance_action("r", governance::ADD_MEMBER);
        let workspace_settings = roles::workspace_resource("r");
        let table: [(Role, [Answer; 6]); 4] = [
            // content Read, Undoable, Outside; comment Undoable; governance Undoable; workspace Read
            (Role::Viewer, [Allow, Deny, Deny, Deny, Deny, Allow]),
            (Role::Commenter, [Allow, Deny, Deny, Allow, Deny, Allow]),
            (Role::Editor, [Allow, Allow, Ask, Allow, Deny, Allow]),
            (Role::Owner, [Allow, Allow, Allow, Allow, Allow, Allow]),
        ];
        for (role, expected) in table {
            let grantees = [Grantee::person("someone"), Grantee::role(role)];
            let at =
                |node: &TreePath, kind| decide(&workspace.grants, &grantees, node, kind, &cx()).answer;
            let got = [
                at(&content, Read),
                at(&content, Undoable),
                at(&content, Outside),
                at(&comment, Undoable),
                at(&governance, Outside),
                at(&workspace_settings, Read),
            ];
            assert_eq!(got, expected, "{role}");
        }
    }

    phase editor_preset_asks_for_outside;
    // The Editor preset grants the workspace's content and not the workspace's own
    // resource, so a change that reaches outside asks, and changing who may do
    // what is refused: the role is a preset of grants like any other.
    {
        let workspace = workspace_with_members(&[(BOB, Role::Editor)]);
        let bob = Authority::person(workspace.party(BOB));
        assert_eq!(
            bob.decide(&send(), ActionKind::Outside, &cx()).answer,
            Answer::Ask
        );
        assert_eq!(
            bob.decide(&archive(), ActionKind::Undoable, &cx()).answer,
            Answer::Allow
        );
        let settings = roles::workspace_resource("r");
        assert_eq!(
            bob.decide(&settings, ActionKind::Undoable, &cx()).answer,
            Answer::Deny,
            "an editor does not change the workspace's own settings"
        );
        let grant_node = roles::governance_action("r", governance::GRANT);
        assert_eq!(
            bob.decide(&grant_node, roles::GOVERNANCE_KIND, &cx())
                .answer,
            Answer::Deny
        );
        assert_eq!(
            workspace.clone().apply(
                BOB,
                &bob,
                Change::SetRole {
                    person: BOB.into(),
                    role: Role::Editor,
                },
                &cx(),
                false
            ),
            Err(Refusal::NotPermitted(
                bob.decide(&grant_node, roles::GOVERNANCE_KIND, &cx())
                    .explain()
            ))
        );
    }

    phase owner_cannot_be_removed_by_grant;
    // Ownership is structural and sits outside the grants: no grant the workspace
    // holds, not even one denying everything on the person, removes or demotes
    // the owner.
    {
        let mut workspace = workspace_with_members(&[(BOB, Role::Editor)]);
        // The roster says why the recorded owner was grandfathered in; the
        // recorded owner is what counts, and no grant reaches over them.
        assert!(workspace.is_owner(ALICE));
        let deny = Grant::new(
            "bob-no-mail",
            Grantee::person(BOB),
            mail(),
            KindSet::ALL,
            Answer::Deny,
            Grantee::person(BOB),
        );
        workspace.grants.push(deny);
        assert!(!workspace.is_member(BOB) || workspace.role_of(BOB) == Some(Role::Editor));
        assert_eq!(workspace.role_of(ALICE), Some(Role::Owner));
        for kind in ActionKind::ALL {
            let verdict = Authority::person(workspace.party(ALICE)).decide(&send(), kind, &cx());
            assert_eq!(verdict.answer, Answer::Allow, "{kind}");
            assert_eq!(verdict.parts[0].basis, Basis::Owner);
        }
        for change in [
            Change::RemoveMember {
                person: ALICE.into(),
            },
            Change::SetRole {
                person: ALICE.into(),
                role: Role::Viewer,
            },
            Change::Ban {
                person: ALICE.into(),
            },
            Change::Restrict {
                person: ALICE.into(),
                until_ms: cx().now_unix_ms + 60_000,
            },
        ] {
            let owner = Authority::person(workspace.party(ALICE));
            let refused = workspace
                .clone()
                .apply(ALICE, &owner, change.clone(), &cx(), false);
            assert!(refused.is_err(), "{change:?} -> {refused:?}");
            assert_eq!(workspace.role_of(ALICE), Some(Role::Owner), "{change:?}");
            assert!(!workspace.is_banned(ALICE), "{change:?}");
            assert_eq!(workspace.restricted_until(ALICE, &cx()), None, "{change:?}");
        }
        // Handing the workspace's own ownership out is not a governance action.
        let owner = Authority::person(workspace.party(ALICE));
        for change in [
            Change::SetRole {
                person: BOB.into(),
                role: Role::Owner,
            },
            Change::AddMember {
                person: CAROL.into(),
                role: Role::Owner,
            },
        ] {
            let refused = workspace
                .clone()
                .apply(ALICE, &owner, change.clone(), &cx(), false);
            assert!(
                matches!(refused, Err(Refusal::OwnerRole)),
                "{change:?} -> {refused:?}"
            );
        }
        // The owner's own grant set still bounds what acts for them.
        let agent = Authority::person(workspace.party(ALICE))
            .bounded_by(workspace.party_of_agent("ag", ALICE));
        assert_eq!(
            agent.decide(&send(), ActionKind::Read, &cx()).answer,
            Answer::Deny
        );
    }

    phase presets_validate;
    {
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

    phase the_workspaces_answers_name_the_role_it_gave;
    {
        let workspace = workspace_with_members(&[
            (BOB, Role::Editor),
            (CAROL, Role::Moderator),
            ("v", Role::Viewer),
            ("c", Role::Commenter),
        ]);
        for (person, role) in [
            (ALICE, Role::Owner),
            (CAROL, Role::Moderator),
            (BOB, Role::Editor),
            ("v", Role::Viewer),
            ("c", Role::Viewer),
        ] {
            let authority = Authority::person(workspace.party(person));
            let answers = roles::telling_nodes("r").map(|node| {
                authority.decide(&node, ActionKind::Undoable, &cx()).answer == Answer::Allow
            });
            assert_eq!(roles::role_answered(answers), role, "{person}: {answers:?}");
        }
    }

    phase built_in_roles_load_as_the_code_defines_them_and_a_workspace_keeps_its_own_grants;
    {
        // A workspace written when the Commenter's rules were fewer, with a stale
        // copy of a Viewer rule and an adjustment of its own to the Editor role.
        let mut stored = workspace_with_members(&[(BOB, Role::Commenter)]);
        let last = format!(
            "role:commenter:{}",
            roles::role_rules(Role::Commenter, "r").len() - 1
        );
        stored.grants.retain(|grant| grant.id.as_str() != last);
        for grant in &mut stored.grants {
            if grant.id.as_str() == "role:viewer:0" {
                grant.answer = Answer::Deny;
            }
        }
        let adjustment = Grant::new(
            "editors-no-mail",
            Grantee::role(Role::Editor),
            mail(),
            KindSet::UNDOABLE,
            Answer::Deny,
            alice(),
        );
        stored.grants.insert(0, adjustment.clone());
        let custom = Grant::new(
            "role:editor:x",
            Grantee::role(Role::Editor),
            mail(),
            KindSet::READ,
            Answer::Allow,
            alice(),
        );
        stored.grants.push(custom.clone());

        let loaded: WorkspacePolicy =
            serde_json::from_value(serde_json::to_value(&stored).unwrap()).unwrap();
        let mut expected = vec![adjustment];
        expected.extend(roles::workspace_role_grants("r", &alice()));
        expected.push(custom);
        assert_eq!(loaded.grants, expected);
        assert_eq!(stored.clone().with_current_roles().grants, expected);

        let reactions = mail()
            .kind("mail.message")
            .resource("m1")
            .field(roles::REACTIONS_FIELD);
        let bob = Authority::person(loaded.party(BOB));
        assert_eq!(
            bob.decide(&reactions, ActionKind::Undoable, &cx()).answer,
            Answer::Allow
        );

        // A workspace that holds none of a role's grants does not get them back.
        let mut bare = workspace_with_members(&[]);
        bare.grants
            .retain(|grant| grant.grantee != Grantee::role(Role::Viewer));
        assert_eq!(bare.clone().with_current_roles(), bare);

        // A workspace already current comes back as stored, in its own order.
        let mut current = workspace_with_members(&[]);
        current.grants.reverse();
        assert_eq!(current.clone().with_current_roles(), current);
    }

    phase a_workspace_neither_revokes_nor_gives_a_built_in_role_grant;
    {
        let workspace = workspace_with_members(&[]);
        let owner = Authority::person(workspace.party(ALICE));
        assert_eq!(
            workspace.clone().apply(
                ALICE,
                &owner,
                Change::Revoke(GrantId::new("role:commenter:1")),
                &cx(),
                false
            ),
            Err(Refusal::BuiltInRole(GrantId::new("role:commenter:1")))
        );
        let given = Grant::new(
            "role:viewer:1",
            Grantee::role(Role::Viewer),
            mail(),
            KindSet::UNDOABLE,
            Answer::Allow,
            alice(),
        );
        assert_eq!(
            workspace
                .clone()
                .apply(ALICE, &owner, Change::Grant(given), &cx(), false),
            Err(Refusal::BuiltInRole(GrantId::new("role:viewer:1")))
        );
    }
}

#[test]
fn each_person_changes_their_own_view_of_the_layout_and_no_one_elses() {
    let mut workspace = WorkspacePolicy::new("r", ALICE);
    for (person, role) in [(BOB, Role::Editor), (CAROL, Role::Viewer)] {
        let held = workspace.clone();
        workspace
            .apply(
                ALICE,
                &held.person_on_device(ALICE, false),
                Change::AddMember {
                    person: person.into(),
                    role,
                },
                &cx(),
                false,
            )
            .expect("the owner admits members");
    }
    let answer = |workspace: &WorkspacePolicy, person, of: &str, kind| {
        let node = TreePath::workspace("r")
            .app(LAYOUT)
            .resource(format!("view-of-{of}"));
        Authority::person(workspace.party(person))
            .decide(&node, kind, &cx())
            .answer
    };
    for person in [BOB, CAROL] {
        assert_eq!(
            answer(&workspace, person, person, ActionKind::Undoable),
            Answer::Allow,
            "{person} changes their own view, whatever their role"
        );
        assert_eq!(
            answer(&workspace, person, ALICE, ActionKind::Read),
            Answer::Allow
        );
    }
    assert_eq!(
        answer(&workspace, BOB, CAROL, ActionKind::Undoable),
        Answer::Deny,
        "an Editor changes no one else's view"
    );
    assert_eq!(
        answer(&workspace, CAROL, BOB, ActionKind::Undoable),
        Answer::Deny
    );
    let layout = TreePath::workspace("r")
        .app(LAYOUT)
        .kind("tilo.desktop")
        .resource("desktop");
    assert_eq!(
        Authority::person(workspace.party(BOB))
            .decide(&layout, ActionKind::Undoable, &cx())
            .answer,
        Answer::Allow,
        "the layout itself stays every Editor's to change"
    );
    assert_eq!(
        workspace.extra_grants(BOB),
        0,
        "the view is no grant beyond the role"
    );

    let held = workspace.clone();
    workspace
        .apply(
            ALICE,
            &held.person_on_device(ALICE, false),
            Change::Restrict {
                person: BOB.into(),
                until_ms: NOW + 1,
            },
            &cx(),
            false,
        )
        .unwrap();
    assert_eq!(
        answer(&workspace, BOB, BOB, ActionKind::Undoable),
        Answer::Deny,
        "a restriction stops their view too"
    );

    let held = workspace.clone();
    workspace
        .apply(
            ALICE,
            &held.person_on_device(ALICE, false),
            Change::RemoveMember { person: BOB.into() },
            &cx(),
            false,
        )
        .unwrap();
    assert_eq!(
        answer(&workspace, BOB, BOB, ActionKind::Undoable),
        Answer::Deny,
        "one who leaves no longer changes their view"
    );
    let stored: WorkspacePolicy =
        serde_json::from_value(serde_json::to_value(&workspace).unwrap()).unwrap();
    assert_eq!(
        stored, workspace,
        "a stored policy loads with the same views"
    );
}
