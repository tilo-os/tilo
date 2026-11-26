use crate::view::{ViewRect, ViewSubset};
use tilo_presence::PresenceKey;
use tilo_ui_base::presence::local_workspace_key;

tilo_test_env::scenario! {
    fn presence_keys_carry_views_scopes_and_pointer_spaces;

    phase view_keys_round_trip_their_subset_and_nothing_else;
    {
        // phase: view_key_round_trips_the_subset
        {
            let subset = ViewSubset {
                scroll: vec![12.0, 34.0],
                open_resource: Some("file:/notes.md".into()),
                viewport: ViewRect {
                    x: 0.0,
                    y: 0.0,
                    width: 800.0,
                    height: 600.0,
                },
                view_mode: Some("list".into()),
            };
            let key = PresenceKey::view(&subset);
            assert!(matches!(key, PresenceKey::View { .. }));
            assert_eq!(
                key.as_view()
                    .expect("a View key decodes back to its subset"),
                subset
            );
        }
        // phase: as_view_is_none_for_other_key_variants
        {
            assert!(PresenceKey::Tab { id: "t".into() }.as_view().is_none());
            assert!(
                PresenceKey::Workspace { id: "w".into() }
                    .as_view()
                    .is_none()
            );
            assert!(PresenceKey::pointer_default().as_view().is_none());
            // Garbage bytes in a View key decode to None rather than panicking.
            assert!(
                PresenceKey::View {
                    data: vec![0xff, 0x00, 0x13]
                }
                .as_view()
                .is_none()
            );
        }
    }

    phase the_presence_scope_is_the_workspace_or_nothing;
    {
        // phase: the_presence_scope_is_the_workspace_the_documents_go_to
        // The presence scope is the workspace, and it is the *same* workspace the
        // collaboration address uses — deriving the two separately is how a
        // window ends up announcing into one workspace while addressing documents to
        // another. It's also what makes a member's cursor follow them from
        // the editor into the file manager: neither app names itself.
        {
            let workspace = "40fc5b85-22d7-4294-a9df-ecc8a391cfce";
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_WORKSPACE_ID", workspace);
            assert_eq!(
                local_workspace_key(),
                Some(PresenceKey::Workspace {
                    id: workspace.to_string()
                })
            );
            assert_eq!(
                tilo_ui_base::collaboration::local_cluster_id().to_string(),
                workspace,
                "presence must be scoped from the same value the routing uses"
            );
        }
        // phase: no_workspace_means_no_scope
        // The nil uuid addresses nobody, so no workspace means no scope.
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.remove("TILO_WORKSPACE_ID");
            assert!(tilo_ui_base::collaboration::local_cluster_id().is_nil());
            assert_eq!(local_workspace_key(), None);
        }
        // phase: empty_workspace_id_is_treated_as_unset
        // An env var set to the empty string is what a shell script produces
        // when the variable it meant to forward was itself unset. Taking it
        // literally would put every such app in one workspace.
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_WORKSPACE_ID", "");
            assert_eq!(local_workspace_key(), None);
        }
    }
}
