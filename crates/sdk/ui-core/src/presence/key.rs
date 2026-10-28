//! Presence keys are wire types and live in `collab_types::presence`.

pub use collab_types::presence::{
    DIGEST_KEY_FIELD, PathSegment, PointerSpace, PresenceKey, SelectionRange,
};

#[cfg(test)]
mod view_key_tests {
    use super::*;
    use crate::presence::local_workspace_key;
    use crate::view::{ViewRect, ViewSubset};

    #[test]
    fn view_keys_round_trip_their_subset_and_nothing_else() {
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

    #[test]
    fn the_presence_scope_is_the_workspace_room_or_nothing() {
        // phase: the_presence_scope_is_the_room_the_documents_go_to
        // The presence scope is the room, and it is the *same* room the
        // collaboration address uses — deriving the two separately is how a
        // window ends up announcing into one room while addressing documents to
        // another. It's also what makes a collaborator's cursor follow them from
        // the editor into the file manager: neither app names itself.
        {
            let room = "40fc5b85-22d7-4294-a9df-ecc8a391cfce";
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_WORKSPACE_ID", room);
            assert_eq!(
                local_workspace_key(),
                Some(PresenceKey::Workspace {
                    id: room.to_string()
                })
            );
            assert_eq!(
                crate::collaboration::local_cluster_id().to_string(),
                room,
                "presence must be scoped from the same value the routing uses"
            );
        }
        // phase: no_workspace_means_no_scope
        // The nil uuid addresses nobody, so no workspace means no scope.
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.remove("TILO_WORKSPACE_ID");
            assert!(crate::collaboration::local_cluster_id().is_nil());
            assert_eq!(local_workspace_key(), None);
        }
        // phase: empty_workspace_id_is_treated_as_unset
        // An env var set to the empty string is what a shell script produces
        // when the variable it meant to forward was itself unset. Taking it
        // literally would put every such app in one room.
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_WORKSPACE_ID", "");
            assert_eq!(local_workspace_key(), None);
        }
    }

    #[test]
    fn pointer_keys_pin_their_ordinals_and_carry_their_space() {
        // phase: the_pointer_key_ordinals_are_the_ones_the_bridge_mirrors
        // The ordinals in the postcard key wire contract are written
        // against, pinned as bytes.
        //
        // Postcard tags variants by ordinal, so inserting `Pointer` anywhere but
        // last, or swapping `Desktop` and `Surface`, decodes on the bridge side as
        // a different variant or not at all — and a pointer publish that silently
        // stops arriving looks exactly like a peer who is not moving. The two
        // enums are separate declarations in separate crates and neither mentions
        // the other, so this is the only guard.
        {
            assert_eq!(
                postcard::to_stdvec(&PresenceKey::desktop_pointer("r")).unwrap(),
                vec![5u8, 0, 1, b'r'],
                "PresenceKey::Pointer must stay variant 5 and PointerSpace::Desktop variant 0",
            );
            assert_eq!(
                postcard::to_stdvec(&PresenceKey::surface_pointer("s")).unwrap(),
                vec![5u8, 1, 1, b's'],
                "PointerSpace::Surface must stay variant 1",
            );
            assert_eq!(
                postcard::to_stdvec(&PresenceKey::tile_pointer("r", 7)).unwrap(),
                vec![5u8, 2, 1, b'r', 7],
                "PointerSpace::Tile must stay variant 2",
            );
        }
        // phase: a_pointer_key_carries_its_space_across_the_wire_encoding
        // The space is what a pointer key is *for*, and it has to survive the
        // encoding the wire uses — the whole failure was a space that existed at
        // one end and not the other.
        {
            for key in [
                PresenceKey::desktop_pointer("room-7"),
                PresenceKey::surface_pointer("so.tilo.word-processor"),
                PresenceKey::tile_pointer("room-7", 0x1234_5678_9abc),
            ] {
                let bytes = postcard::to_stdvec(&key).expect("a pointer key encodes");
                let back: PresenceKey =
                    postcard::from_bytes(&bytes).expect("and decodes to the same key");
                assert_eq!(back, key, "the space must survive the round trip");
                assert!(
                    back.pointer_space().is_some(),
                    "a pointer key with no readable space is the bug this variant exists to stop",
                );
            }
        }
        // phase: two_surfaces_are_two_spaces
        // Two surfaces are two spaces. A file manager cannot answer for a
        // position measured in a word processor, and this is where that is
        // decided — before anything has been drawn, from the key alone.
        {
            let wp = PresenceKey::surface_pointer("so.tilo.word-processor");
            let fm = PresenceKey::surface_pointer("so.tilo.files");
            assert_ne!(wp.pointer_space(), fm.pointer_space());
            assert!(!wp.scopes_a_desktop_pointer());
            assert!(!fm.scopes_a_desktop_pointer());
        }
        // phase: every_spelling_of_the_desktop_reads_as_the_desktop
        // The two older spellings of the desktop still mean it: a session's
        // room-scoped republish, and the tag the keyless capnp cursor path was
        // re-keyed to.
        {
            assert!(PresenceKey::desktop_pointer("r").scopes_a_desktop_pointer());
            assert!(PresenceKey::Workspace { id: "r".into() }.scopes_a_desktop_pointer());
            assert!(PresenceKey::pointer_default().scopes_a_desktop_pointer());
            // A pointer placed over a tab's tile is drawn by the compositor
            // above every window, like any desktop pointer.
            assert!(PresenceKey::tile_pointer("r", 1).scopes_a_desktop_pointer());
            // A position inside a document is not a position on a screen.
            assert!(
                !PresenceKey::Widget {
                    path: vec![PathSegment::Index(0)]
                }
                .scopes_a_desktop_pointer()
            );
            assert_eq!(
                PresenceKey::Tab { id: "t".into() }.pointer_space(),
                None,
                "a tab is an object, not a coordinate space",
            );
        }
    }
}

#[cfg(test)]
mod scope_digest_tests {
    use super::*;

    #[test]
    fn digest_form_is_stable_distinct_and_self_describing() {
        let key = |id: &str| PresenceKey::Widget {
            path: vec![
                PathSegment::Field("wp_block".into()),
                PathSegment::Field(id.into()),
            ],
        };
        let long = format!("{}.7", "ab".repeat(64));
        let a = key(&long);
        assert_eq!(a.scope_digest(), key(&long).scope_digest());
        assert_ne!(a.scope_digest(), key("other").scope_digest());
        // Known vector shared with the LIVE_V1 anchor tests (host + browser).
        assert_eq!(
            hex::encode(key("b1").scope_digest().unwrap()),
            SCOPE_DIGEST_B1
        );
        let PresenceKey::Widget { path } = a.digest_form().unwrap() else {
            panic!()
        };
        assert_eq!(path[0], PathSegment::Field(DIGEST_KEY_FIELD.into()));
        assert_eq!(
            a.digest_form(),
            Some(PresenceKey::from_scope_digest(a.scope_digest().unwrap()))
        );
    }

    /// `hex(scope_digest(Widget{[wp_block, "b1"]}))`.
    const SCOPE_DIGEST_B1: &str = "9c59c4ded04a69ccf5b2ac58797c3b77";
}
