#![allow(
    clippy::similar_names,
    reason = "sk/pk and owner/editor pairings are the conventional, intentional names for a keypair and a two-role fixture"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable,
    clippy::panic_in_result_fn,
    clippy::arithmetic_side_effects,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::wildcard_imports,
    clippy::used_underscore_binding,
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    clippy::items_after_statements,
    clippy::significant_drop_tightening,
    clippy::future_not_send,
    clippy::large_futures,
    clippy::await_holding_lock,
    clippy::await_holding_refcell_ref,
    reason = "test scenarios and fixtures: a failed unwrap is the assertion, and each scenario keeps its setup, guards and awaits in one readable body"
)]

use super::*;

#[test]
fn a_join_names_a_valid_record_kind_or_none() {
    let joined = |kind: &str| super::super::joined_resource_type(kind);
    assert_eq!(joined("").unwrap(), None);
    assert_eq!(
        joined("notes.notebook").unwrap().as_deref(),
        Some("notes.notebook")
    );
    for invalid in ["1note", "note book", "note/page"] {
        assert!(joined(invalid).is_err(), "{invalid}");
    }
}

#[cfg(test)]
#[tokio::test(flavor = "current_thread")]
async fn native_media_raw_admitted_channel_has_no_local_process_authority() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fixture = local_directory_fixture();
            let channel = fixture.invocation_channel();
            for command in [
                runtime_collab::native_media::Command::Join {
                    channel_id: "voice".into(),
                },
                runtime_collab::native_media::Command::SetMuted { muted: false },
            ] {
                let mut request = runtime_collab::native_media::Request::new(command);
                request.intent = 1;
                let error = channel
                    .native_media_command(&request)
                    .await
                    .expect_err("ordinary admitted channel cannot mint local process ownership");
                assert!(error.to_string().contains("launcher-attested"), "{error}");
            }
        })
        .await;
}

#[cfg(test)]
#[tokio::test(flavor = "current_thread")]
async fn file_clipboard_raw_admitted_channel_cannot_mint_local_ownership() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fixture = local_directory_fixture();
            let channel = fixture.invocation_channel();
            for command in [
                runtime_collab::file_clipboard::Command::Snapshot,
                runtime_collab::file_clipboard::Command::Publish {
                    sources: vec![uuid::Uuid::new_v4().to_string()],
                    cut: true,
                },
            ] {
                let error = channel
                    .local_file_clipboard_command(&command, None)
                    .await
                    .expect_err("unattested channel must be refused");
                assert!(error.to_string().contains("launcher-attested"), "{error}");
            }
        })
        .await;
}

#[cfg(test)]
#[tokio::test(flavor = "current_thread")]
async fn audio_output_raw_admitted_channel_cannot_open_or_write_host_pcm() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fixture = local_directory_fixture();
            let channel = fixture.invocation_channel();
            for command in [
                runtime_collab::audio_output::Command::Open {},
                runtime_collab::audio_output::Command::Write {
                    generation: 1,
                    sequence: 1,
                    samples: vec![123; 1920],
                },
            ] {
                let error = channel.audio_output_command(&command).await.expect_err(
                    "an admitted remote/raw route cannot manufacture launcher process ownership",
                );
                assert!(error.to_string().contains("launcher-attested"), "{error}");
            }
        })
        .await;
}

tilo_test_env::scenario! {
    fn a_certified_join_is_verified_and_admitted_by_its_workspace;

    phase certified_join_verifies_root_device_and_workspace_chain;
    {
        let root = SigningKey::from_bytes(&[1; 32]);
        let device = SigningKey::from_bytes(&[2; 32]);
        let route = CrdtRoute::cluster("workspace-a");
        let presentation = certified_join("workspace-a", &root, &device);
        let verified = verify_join_presentation(
            &route,
            &presentation,
            &root.verifying_key().to_bytes(),
            2,
            false,
            true,
        )
        .expect("valid certified join");
        assert_eq!(verified.user_public_key, root.verifying_key().to_bytes());
        assert_eq!(
            verified.device_public_key,
            device.verifying_key().to_bytes()
        );
    }

    phase certified_join_rejects_unknown_proof_signer_and_tampered_cert;
    {
        let root = SigningKey::from_bytes(&[3; 32]);
        let device = SigningKey::from_bytes(&[4; 32]);
        let stranger = SigningKey::from_bytes(&[5; 32]);
        let route = CrdtRoute::cluster("workspace-a");
        let mut presentation = certified_join("workspace-a", &root, &device);
        presentation.proof = stranger
            .sign(&tilo_identity_client::collaboration_join_digest(
                "workspace-a",
            ).unwrap())
            .to_bytes()
            .to_vec();
        assert!(
            verify_join_presentation(
                &route,
                &presentation,
                &root.verifying_key().to_bytes(),
                2,
                false,
                true,
            )
            .is_err()
        );

        let mut presentation = certified_join("workspace-a", &root, &device);
        let mut cert: tilo_user_identity::DeviceCert =
            serde_json::from_slice(&presentation.device_certificate).expect("certificate");
        cert.device_label.push_str(" (tampered)");
        presentation.device_certificate = serde_json::to_vec(&cert).expect("certificate JSON");
        assert!(
            verify_join_presentation(
                &route,
                &presentation,
                &root.verifying_key().to_bytes(),
                2,
                false,
                true,
            )
            .is_err()
        );
    }

    phase certified_join_rejects_other_workspace_revoked_and_unenrolled_device;
    {
        let root = SigningKey::from_bytes(&[6; 32]);
        let device = SigningKey::from_bytes(&[7; 32]);
        let presentation = certified_join("workspace-b", &root, &device);
        let expected_root = root.verifying_key().to_bytes();
        assert!(
            verify_join_presentation(
                &CrdtRoute::cluster("workspace-a"),
                &presentation,
                &expected_root,
                2,
                false,
                true,
            )
            .is_err()
        );
        let route = CrdtRoute::cluster("workspace-b");
        let other_root = SigningKey::from_bytes(&[8; 32]);
        assert!(
            verify_join_presentation(
                &route,
                &presentation,
                &other_root.verifying_key().to_bytes(),
                2,
                false,
                true,
            )
            .is_err(),
            "a certificate selected for another workspace identity is rejected"
        );
        assert!(
            verify_join_presentation(&route, &presentation, &expected_root, 2, true, true).is_err()
        );
        assert!(
            verify_join_presentation(&route, &presentation, &expected_root, 2, false, false)
                .is_err()
        );
    }

    phase certified_join_admission_uses_workspace_membership_and_role;
    {
        let root = SigningKey::from_bytes(&[20; 32]);
        let actor = person_id_for_root(root.verifying_key().to_bytes());
        let mut workspace = crate::apps::test_support::Workspace::new(
            crate::apps::test_support::WorkspaceId::from_u128(1),
            "Member workspace",
        );
        let route = CrdtRoute::cluster(workspace.id().to_string());
        workspace.policy.add_member(actor.clone(), crate::apps::test_support::Role::Editor);
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);

        let admitted = admit_verified_join(&tree, &route, verified_device(&root))
            .expect("a certified editor is admitted");
        assert_eq!(admitted.actor_key, actor);
        assert_eq!(admitted.role, CapRole::Editor);
    }

    phase certified_join_admission_rejects_wrong_or_removed_root;
    {
        let member = SigningKey::from_bytes(&[21; 32]);
        let stranger = SigningKey::from_bytes(&[22; 32]);
        let member_id = person_id_for_root(member.verifying_key().to_bytes());
        let mut workspace = crate::apps::test_support::Workspace::new(
            crate::apps::test_support::WorkspaceId::from_u128(2),
            "Revocation workspace",
        );
        let route = CrdtRoute::cluster(workspace.id().to_string());
        workspace.policy.add_member(member_id.clone(), crate::apps::test_support::Role::Viewer);
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);

        assert!(admit_verified_join(&tree, &route, verified_device(&stranger)).is_err());
        let admitted = admit_verified_join(&tree, &route, verified_device(&member))
            .expect("member starts admitted");
        let membership_tree = tree.shared();
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = AppBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(membership_tree.clone()),
            None,
        );
        assert!(membership_tree.write().workspaces[0].policy.dismiss(&member_id));
        assert!(
            state.revalidate_admission(&route, &admitted).is_err(),
            "removing membership rejects the certified session's next operation"
        );
    }

    phase certified_join_admission_rejects_cross_workspace_replay;
    {
        let root = SigningKey::from_bytes(&[23; 32]);
        let actor = person_id_for_root(root.verifying_key().to_bytes());
        let mut member_workspace = crate::apps::test_support::Workspace::new(
            crate::apps::test_support::WorkspaceId::from_u128(3),
            "Member workspace",
        );
        member_workspace.policy.set_owner(&actor);
        let other_workspace = crate::apps::test_support::Workspace::new(
            crate::apps::test_support::WorkspaceId::from_u128(4),
            "Other workspace",
        );
        let replay_route = CrdtRoute::cluster(other_workspace.id().to_string());
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(member_workspace);
        tree.workspaces.push(other_workspace);

        assert!(admit_verified_join(&tree, &replay_route, verified_device(&root)).is_err());
    }
}

#[test]
fn workspace_control_requires_editor_authority() {
    assert!(workspace_control_role_allowed(Role::Owner));
    assert!(workspace_control_role_allowed(Role::Editor));
    assert!(!workspace_control_role_allowed(Role::Commenter));
    assert!(!workspace_control_role_allowed(Role::Viewer));
}

tilo_test_env::scenario! {
    fn a_foreign_device_is_held_to_the_workspace_cap;

    phase a_foreign_device_is_refused_actions_above_the_workspace_cap;
    {
        use tilo_actions::ActionKind::{Outside, Undoable};
        let root = SigningKey::from_bytes(&[24; 32]);
        let mut workspace =
            crate::apps::test_support::Workspace::new(crate::apps::test_support::WorkspaceId::from_u128(4), "Actions");
        workspace.policy.set_owner(person_id_for_root(root.verifying_key().to_bytes()));
        let workspace_id = workspace.id().to_string();
        let route = CrdtRoute::cluster(&workspace_id);
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let native = admit_verified_join(&tree, &route, verified_device(&root)).unwrap();
        let foreign = VerifiedJoinIdentity {
            capped: true,
            ..verified_device(&root)
        };
        let foreign = admit_verified_join(&tree, &route, foreign).unwrap();
        let (state, _bridge_rx) = AppBrokerState::new(
            &Arc::new(FanoutState::new()),
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(tree.shared()),
            None,
        );
        let authorize = |admitted: &AdmittedJoinIdentity, kind| {
            state.authorize_invocation_permission(
                &workspace_id,
                (
                    "so.tilo.settings",
                    "settings.workspace",
                    "settings.workspace",
                    "workspace.rename",
                ),
                &tilo_actions::ActionNeed::of(kind),
                &admitted.broker_actor(),
                0,
            )
        };

        for kind in [Outside, Undoable] {
            authorize(&native, kind).unwrap();
        }
        assert!(
            authorize(&foreign, Outside).is_err(),
            "reaching outside asks first on a device not theirs, and nobody answered"
        );
        authorize(&foreign, Undoable).unwrap();
    }

    phase a_foreign_device_joins_with_at_most_the_workspace_cap;
    {
        let root = SigningKey::from_bytes(&[23; 32]);
        let mut workspace =
            crate::apps::test_support::Workspace::new(crate::apps::test_support::WorkspaceId::from_u128(3), "Foreign");
        workspace.policy.set_owner(person_id_for_root(root.verifying_key().to_bytes()));
        workspace.allow_agent("scribe", crate::apps::test_support::KindSet::READ);
        let route = CrdtRoute::cluster(workspace.id().to_string());
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let foreign = VerifiedJoinIdentity {
            capped: true,
            ..verified_device(&root)
        };

        let workspace_id = tree.workspaces[0].id().to_string();
        let kept = crate::apps::test_support::KeptAgents::default().listing(
            &workspace_id,
            ("scribe", &person_id_for_root(root.verifying_key().to_bytes())),
        );

        let mut admitted = admit_verified_join(&tree, &route, verified_device(&root)).unwrap();
        assert_eq!(admitted.role, CapRole::Owner);
        admit_listed_agent(&kept, &workspace_id, &mut admitted, "scribe").unwrap();

        let mut admitted = admit_verified_join(&tree, &route, foreign.clone()).unwrap();
        assert!(
            admit_listed_agent(&kept, &workspace_id, &mut admitted, "scribe").is_err(),
            "an agent runs on a member's own device, not on one the workspace caps"
        );
        let changes = |admitted: &AdmittedJoinIdentity, tree: &WorkspaceTree| {
            admitted
                .with_authority(Some(&tree.desktop), &route.cluster_id, |access| {
                    access
                        .authority()
                        .decide(
                            &tree.workspaces[0].policy.node().app("so.tilo.notes"),
                            ActionKind::Undoable,
                            &tilo_action_policy::Context::at_secs(0),
                        )
                        .answer
                })
                .unwrap()
        };
        assert_eq!(changes(&admitted, &tree), Answer::Allow);

        let admitted = admit_verified_join(&tree, &route, foreign).unwrap();
        assert_eq!(
            changes(&admitted, &tree),
            Answer::Allow,
            "a device that is not the person's still changes the workspace's content"
        );
    }
}

tilo_test_env::scenario! {
    fn an_agent_acts_only_with_the_authority_its_person_holds;

    phase a_delegated_agent_acts_with_its_persons_authority_and_ends_with_them;
    {
        let root = SigningKey::from_bytes(&[44; 32]);
        let actor = person_id_for_root(root.verifying_key().to_bytes());
        let mut workspace = crate::apps::test_support::Workspace::new(
            crate::apps::test_support::WorkspaceId::from_u128(44),
            "Agent workspace",
        );
        let owner =
            person_id_for_root(SigningKey::from_bytes(&[43; 32]).verifying_key().to_bytes());
        workspace.policy.set_owner(owner.clone());
        workspace.policy.add_member(actor.clone(), Role::Moderator);
        workspace.policy.grant(tilo_action_policy::Grant::new(
            "fixture:launches-agents",
            crate::apps::test_support::Grantee::person(actor.clone()),
            tilo_action_policy::roles::governance_action(
                &workspace.id().to_string(),
                tilo_action_policy::roles::governance::GRANT,
            ),
            crate::apps::test_support::KindSet::OUTSIDE,
            Answer::Allow,
            crate::apps::test_support::Grantee::person(owner),
        ));
        let route = CrdtRoute::cluster(workspace.id().to_string());
        workspace.allow_agent("scribe", crate::apps::test_support::KindSet::READ);
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);

        let workspace_id = tree.workspaces[0].id().to_string();
        let kept =
            crate::apps::test_support::KeptAgents::default().listing(&workspace_id, ("scribe", &actor));
        let mut admitted = admit_verified_join(&tree, &route, verified_device(&root))
            .expect("the member is admitted before the agent they run is");
        admit_listed_agent(&kept, &workspace_id, &mut admitted, "scribe")
            .expect("the record lists the agent the member runs");
        assert_eq!(
            admitted.broker_actor().actor(),
            &tilo_actions::Actor::from(tilo_action_policy::MemberId::agent("scribe", actor.clone()))
        );
        assert!(admitted.broker_actor().is_agent());
        let answer = |kind| {
            admitted
                .with_authority(Some(&tree.desktop), &route.cluster_id, |access| {
                    access
                        .authority()
                        .decide(
                            &tree.workspaces[0].policy.node().app("so.tilo.notes"),
                            kind,
                            &tilo_action_policy::Context::at_secs(0),
                        )
                        .answer
                })
                .unwrap()
        };
        assert_eq!(answer(ActionKind::Read), Answer::Allow, "the agent reads");
        assert_eq!(
            answer(ActionKind::Undoable),
            Answer::Allow,
            "and changes what its person's turn allows it"
        );
        assert_ne!(
            answer(roles::GOVERNANCE_KIND),
            Answer::Allow,
            "never the workspace's governance"
        );

        let membership = tree.shared();
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = AppBrokerState::new_configured(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(membership.authority()),
            None,
            None,
            crate::apps::services::ProcessServices {
                agents: Some(Arc::new(kept.clone())),
                ..membership.services()
            },
        );
        assert!(
            state.revalidate_admission(&route, &admitted).is_ok(),
            "a going agent session is admitted while its person is"
        );
        assert!(membership.write().workspaces[0].policy.dismiss(&actor));
        assert!(
            state.revalidate_admission(&route, &admitted).is_err(),
            "removing the person invalidates an already-open agent channel"
        );
    }

    phase a_hosted_agent_takes_its_persons_role_capped_and_ends_with_them;
    // In a workspace this desktop hosts itself, a delegated agent takes part
    // with its person's role, capped at the editor preset, and removing the
    // person cuts an open session off.
    {
        use tilo_action_policy::roles::governance;
        let owner_root = SigningKey::from_bytes(&[45; 32]);
        let owner = person_id_for_root(owner_root.verifying_key().to_bytes());
        let bob_root = SigningKey::from_bytes(&[46; 32]);
        let bob = person_id_for_root(bob_root.verifying_key().to_bytes());
        let mut workspace =
            crate::apps::test_support::Workspace::new(crate::apps::test_support::WorkspaceId::from_u128(45), "Hosted");
        workspace.policy.set_owner(owner.clone());
        let workspace_id = workspace.policy.workspace.clone();
        // Bob comments, and may launch the workspace's agents.
        workspace.policy.add_member(bob.clone(), Role::Commenter);
        workspace.policy.grant(tilo_action_policy::Grant::new(
            "fixture:bob-launches-agents",
            crate::apps::test_support::Grantee::person(bob.clone()),
            roles::governance_action(&workspace_id, governance::GRANT),
            crate::apps::test_support::KindSet::OUTSIDE,
            Answer::Allow,
            crate::apps::test_support::Grantee::person(owner.clone()),
        ));
        // The bounds the agents' own records keep: scribe may change what is
        // there, reader may only read.
        use crate::apps::test_support::{Grants as _, KindSet, TreePath};
        workspace.allow(
            Grantee::agent("scribe", owner.clone()),
            &TreePath::root(),
            KindSet::READ_UNDOABLE,
        );
        workspace.allow(Grantee::agent("reader", owner.clone()), &TreePath::root(), KindSet::READ);
        let route = CrdtRoute::cluster(workspace.id().to_string());
        let notes = workspace.policy.node().app("so.tilo.notes");
        let set_role = roles::governance_action(&workspace_id, governance::SET_ROLE);
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);

        let workspace_id = tree.workspaces[0].id().to_string();
        let kept = crate::apps::test_support::KeptAgents::default()
            .listing(&workspace_id, ("scribe", &owner))
            .listing(&workspace_id, ("reader", &bob))
            .listing(&workspace_id, ("bobs-scribe", &bob));
        let delegate = |root: &SigningKey, agent: &str, tree: &WorkspaceTree| {
            let mut admitted = admit_verified_join(tree, &route, verified_device(root)).unwrap();
            admit_listed_agent(&kept, &workspace_id, &mut admitted, agent).map(|()| admitted)
        };
        let answer = |admitted: &AdmittedJoinIdentity,
                      tree: &WorkspaceTree,
                      node: &TreePath,
                      kind: ActionKind| {
            admitted
                .with_authority(Some(&tree.desktop), &route.cluster_id, |access| {
                    access
                        .authority()
                        .decide(node, kind, &tilo_action_policy::Context::at_secs(0))
                        .answer
                })
                .unwrap()
        };

        // The owner's agent edits as their owner may, held to an Editor, and
        // never governs.
        let scribe = delegate(&owner_root, "scribe", &tree).unwrap();
        assert_eq!(scribe.role, Role::Editor, "its owner's role, capped");
        assert!(workspace_control_role_allowed(scribe.role));
        assert_eq!(
            answer(&scribe, &tree, &notes, ActionKind::Undoable),
            Answer::Allow
        );
        assert_eq!(
            answer(&scribe, &tree, &set_role, roles::GOVERNANCE_KIND),
            Answer::Deny,
            "the owner's agent does not govern the workspace"
        );
        // Bob's session holds Bob's role, whatever it is called.
        let reader = delegate(&bob_root, "reader", &tree).unwrap();
        assert_eq!(reader.role, Role::Commenter, "its person's role, capped");
        assert!(!workspace_control_role_allowed(reader.role));
        assert_eq!(
            answer(&reader, &tree, &notes, ActionKind::Read),
            Answer::Allow
        );
        assert_eq!(
            answer(&reader, &tree, &notes, ActionKind::Undoable),
            Answer::Deny,
            "a commenter's agent changes nothing"
        );
        // An agent is its person's: Bob running `scribe` would be Bob's scribe, an agent the
        // record names for nobody, never the owner's of the same id.
        assert!(
            delegate(&bob_root, "scribe", &tree).is_err(),
            "the record lists scribe for the owner, not for Bob"
        );
        let bobs = delegate(&bob_root, "bobs-scribe", &tree).unwrap();
        assert_eq!(bobs.role, Role::Commenter, "capped by its person's role");
        assert!(!workspace_control_role_allowed(bobs.role));
        assert_eq!(
            answer(&bobs, &tree, &notes, ActionKind::Undoable),
            Answer::Deny,
            "the agent changes nothing its person may not"
        );

        let membership = tree.shared();
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = AppBrokerState::new_configured(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(membership.authority()),
            None,
            None,
            crate::apps::services::ProcessServices {
                agents: Some(Arc::new(kept.clone())),
                ..membership.services()
            },
        );
        assert_eq!(
            state.revalidate_admission(&route, &bobs).unwrap().role,
            Role::Commenter,
            "revalidation resolves the same role"
        );
        assert!(state.revalidate_admission(&route, &scribe).is_ok());

        // Removing the person ends their agent's session at once.
        assert!(membership.write().workspaces[0].policy.dismiss(&bob));
        assert!(
            state.revalidate_admission(&route, &bobs).is_err(),
            "revoking the person cuts their agent off"
        );
        assert!(
            state.revalidate_admission(&route, &reader).is_err(),
            "removing the person ends every agent of theirs at once"
        );
        assert!(state.revalidate_admission(&route, &scribe).is_ok());
    }

    phase non_owner_cannot_claim_an_agent_actor;
    {
        let member = SigningKey::from_bytes(&[45; 32]);
        let member_id = person_id_for_root(member.verifying_key().to_bytes());
        let mut workspace = crate::apps::test_support::Workspace::new(
            crate::apps::test_support::WorkspaceId::from_u128(45),
            "Delegation workspace",
        );
        let route = CrdtRoute::cluster(workspace.id().to_string());
        workspace.policy.add_member(member_id, crate::apps::test_support::Role::Editor);
        workspace.allow_agent("operator", crate::apps::test_support::KindSet::READ);
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let workspace_id = tree.workspaces[0].id().to_string();
        let mut admitted = admit_verified_join(&tree, &route, verified_device(&member))
            .expect("the person is a member");
        assert!(
            admit_listed_agent(
                &crate::apps::test_support::KeptAgents::default(),
                &workspace_id,
                &mut admitted,
                "operator"
            )
            .is_err(),
            "the workspace lists no agent of that id"
        );
    }
}

/// The person's own workspace is admitted from the joiner's own root alone, with no
/// workspace in the tree, and stays admitted without one. Nobody else's root enters it, and
/// nobody enters another person's workspace.
#[test]
fn the_persons_workspace_admits_only_its_own_roots_devices_as_owner() {
    let root = SigningKey::from_bytes(&[26; 32]);
    let device = SigningKey::from_bytes(&[27; 32]);
    let stranger = SigningKey::from_bytes(&[28; 32]);
    let own_workspace = collab_types::person_workspace(&root.verifying_key().to_bytes());
    let route = CrdtRoute::record("so.tilo.tube", own_workspace.to_string(), "watch-history");
    let membership = WorkspaceTree::default().shared();
    let own_device = VerifiedJoinIdentity {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        capped: false,
    };

    let admitted = admit_verified_join(&membership.read(), &route, own_device.clone())
        .expect("the root's own device joins their own workspace");
    assert_eq!(admitted.role, CapRole::Owner);
    assert_eq!(
        admitted.actor_key,
        person_id_for_root(root.verifying_key().to_bytes())
    );
    assert!(
        admit_verified_join(&membership.read(), &route, verified_device(&stranger)).is_err(),
        "another root's device never joins this person's workspace"
    );
    let foreign = admit_verified_join(
        &membership.read(),
        &route,
        VerifiedJoinIdentity {
            capped: true,
            ..own_device
        },
    )
    .expect("the root's foreign device joins their own workspace");
    let governs = foreign
        .with_authority(None, &route.cluster_id, |access| {
            access
                .authority()
                .decide(
                    &roles::governance_action(&route.cluster_id, roles::governance::ADD_MEMBER),
                    roles::GOVERNANCE_KIND,
                    &tilo_action_policy::Context::at_secs(0),
                )
                .answer
        })
        .unwrap();
    assert_eq!(
        governs,
        Answer::Deny,
        "a foreign device keeps the foreign-device bound in the person's own workspace too"
    );
    let strangers_workspace = CrdtRoute::record(
        "so.tilo.tube",
        collab_types::person_workspace(&stranger.verifying_key().to_bytes()).to_string(),
        "watch-history",
    );
    assert!(
        admit_verified_join(&membership.read(), &strangers_workspace, own_device).is_err(),
        "a device never joins another person's workspace"
    );

    let fanout = Arc::new(FanoutState::new());
    let (state, _bridge_rx) = AppBrokerState::new(
        &fanout,
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        Some(membership),
        None,
    );
    let current = state
        .revalidate_admission(&route, &admitted)
        .expect("the own workspace stays admitted without a workspace");
    assert_eq!(current.role, CapRole::Owner);
}

/// A device a harness lists is still refused once the workspace's authority
/// revokes it: the list never stands in for, or overrides, revocation.
#[test]
fn a_revoked_device_is_refused_even_when_it_is_listed() {
    let root = SigningKey::from_bytes(&[11; 32]);
    let route = CrdtRoute::cluster(uuid::Uuid::from_u128(11).to_string());
    let device = |seed| {
        super::signed_worker::signed_device(
            &root,
            &SigningKey::from_bytes(&[seed; 32]),
            "laptop",
            tilo_user_identity::DeviceCapabilities::primary(),
            None,
        )
    };
    let (current, revoked) = (device(12), device(13));
    let host = super::signed_worker::CurrentHost::new(&route, &current, None);
    let listed = [current.device_public_key, revoked.device_public_key];
    let authority: &dyn crate::apps::certificate_authority::CertificateAuthority = host.as_ref();

    let refused = authorize_join_certificate(&route, &revoked, Some(&listed), Some(authority));
    assert_eq!(refused.unwrap_err(), "host certificate changed or revoked");
    assert_eq!(
        authorize_join_certificate(&route, &current, Some(&listed), Some(authority)),
        Ok((root.verifying_key().to_bytes(), false, true)),
    );
}
