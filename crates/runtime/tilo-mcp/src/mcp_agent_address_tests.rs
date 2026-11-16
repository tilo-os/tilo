//! The names a path may take: the address an action answers, the close
//! matches of an action nobody has, and the keys that tell colliding titles
//! apart.

use super::super::mcp_agent_paths::{CLOSE_MATCHES, close_matches};
use super::super::{Node, bound_key, pick};
use super::*;

tilo_test_env::scenario! {
    fn a_path_a_name_or_an_address_leads_to_one_resource_or_says_what_was_meant;

    phase an_action_no_resource_offers_names_at_most_five_close_matches;
    // An action nobody has is answered with what may have been meant, at most
    // [`CLOSE_MATCHES`] of them, never the app's whole list.
    {
        let ids: Vec<String> = [
            "mark-read",
            "read-aloud",
            "read",
            "archive",
            "reply",
            "remove",
            "rename",
            "restore",
            "reload",
            "read-more",
            "read-later",
            "react",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let close = close_matches("read", &ids, CLOSE_MATCHES);
        assert_eq!(close.len(), CLOSE_MATCHES, "{close:?}");
        assert!(close.iter().all(|id| id.contains("read")), "{close:?}");
        assert_eq!(close_matches("arch", &ids, CLOSE_MATCHES), ["archive"]);
        assert!(close_matches("explode", &ids, CLOSE_MATCHES).is_empty());
    }

    phase a_title_that_names_several_offers_the_keys_that_tell_them_apart;
    // A title several resources share is refused with the key each one goes by:
    // the key is a name the next path can use.
    {
        let node = |key: &str| Node {
            id: format!("notes.page:{key}"),
            kind: "notes.page".into(),
            label: "README".into(),
            key: format!("notes.page:{key}"),
            parent: "notes.notebook".into(),
            container: false,
            actions: Vec::new(),
        };
        let nodes = vec![node("aaaaaaaa"), node("bbbbbbbb")];
        let error = pick("README", &nodes, None).unwrap_err();
        assert_eq!(
            error,
            "README names 2 resources: README (notes.page:aaaaaaaa), README (notes.page:bbbbbbbb); \
         name one by its key"
        );
    }

    phase a_resource_is_named_by_the_address_an_action_answers;
    // A resource is also named by the address an action answers it by
    // (`notes.notebook/page/<id>/body`): what a call said it changed is a path
    // the next call takes as it is.
    {
        let host = Notes::new(true);
        run(async {
            let address = format!("{TODO_BODY}#append-paragraph");
            let (text, failed) = tool(
                &agent(&host),
                "tilo_do",
                json!({"path": address, "args": {"text": "x"}}),
            )
            .await;
            assert!(!failed, "{text}");
            let invoked = host.invoked();
            let (resource, action, _) = invoked.last().unwrap();
            assert_eq!(*resource, host.id(TODO_BODY));
            assert_eq!(action, "append-paragraph");

            // Without an action, the same address lists the same resource.
            let (text, failed) = tool(&agent(&host), "tilo_ls", json!({"path": TODO_BODY})).await;
            assert!(!failed, "{text}");
            assert!(text.contains("#append-paragraph(text)"), "{text}");
        });
    }

    phase bound_ids_decode_to_the_apps_own_name_and_nothing_else_does;
    {
        assert_eq!(
            bound_key(&bound("a", TODO_BODY)).as_deref(),
            Some(TODO_BODY)
        );
        assert_eq!(bound_key("notes.notebook"), None);
        assert_eq!(bound_key("tilo-ui-bound:v1:a:b:zz"), None);
        assert_eq!(bound_key("tilo-ui-bound:v1:a:b:abc"), None);
    }

    phase a_path_is_app_names_and_an_action;
    {
        assert_eq!(
            parse_path("notes/Todo/body#append-paragraph").unwrap(),
            ParsedPath {
                app: "notes".into(),
                segments: vec!["Todo".into(), "body".into()],
                action: Some("append-paragraph".into()),
            }
        );
        assert_eq!(
            parse_path("  notes ").unwrap(),
            ParsedPath {
                app: "notes".into(),
                segments: Vec::new(),
                action: None,
            }
        );
        // A title with a slash or a hash writes it escaped.
        let parsed = parse_path(r"notes/Q1\/Q2 plan\#1#rename").unwrap();
        assert_eq!(parsed.segments, vec!["Q1/Q2 plan#1".to_owned()]);
        assert_eq!(parsed.action.as_deref(), Some("rename"));
        assert_eq!(path_name("Q1/Q2 plan#1"), r"Q1\/Q2 plan\#1");
        assert!(parse_path("").is_err());
        assert!(parse_path("#rename").is_err());
    }

    phase a_path_means_the_same_resource_after_the_desktop_restarts;
    {
        let before = Notes::new(true);
        let after = before.restarted();
        assert_ne!(
            before.id(TODO_BODY),
            after.id(TODO_BODY),
            "the broker's ids moved"
        );
        run(async {
            for host in [&before, &after] {
                let (text, failed) = tool(
                    &agent(host),
                    "tilo_do",
                    json!({"path": "notes/Todo/body#append-paragraph", "args": {"text": "x"}}),
                )
                .await;
                assert!(!failed, "{text}");
                let invoked = host.invoked();
                let (resource, action, _) = invoked.last().unwrap();
                assert_eq!(*resource, host.id(TODO_BODY));
                assert_eq!(action, "append-paragraph");
            }
        });
    }

    phase the_app_alone_names_an_action_it_publishes_on_a_resource_it_lists;
    // The live Settings publishes `add-email-account` on the root resource it
    // lists, not on the app's own root: `settings#add-email-account` names it
    // as surely as `settings/settings.root#add-email-account` does.
    {
        let host = Notes::new(true).live();
        run(async {
            let (text, failed) = tool(&agent(&host), "tilo_do", json!({"path": "notes#archive"})).await;
            assert!(!failed, "{text}");
            assert_eq!(text, "ok", "{text}");
            let (resource, action, _) = host.invoked().last().cloned().unwrap();
            assert_eq!(resource, host.id(&Notes::live_key(TODO)), "{resource}");
            assert_eq!(action, "archive");
            let (text, failed) = tool(&agent(&host), "tilo_ls", json!({"path": "notes#archive"})).await;
            assert!(!failed, "{text}");
            assert!(
                text.starts_with("notes/Notebook/Todo#archive  Archive note"),
                "{text}"
            );
        });
    }

    phase an_app_action_on_two_resources_is_refused_naming_both;
    // Two resources carrying the same action is not one action: the app alone
    // names neither, and says which it would have to be told.
    {
        let host = Notes::new(true).live().archiving_both_notes();
        run(async {
            let (text, failed) = tool(&agent(&host), "tilo_do", json!({"path": "notes#archive"})).await;
            assert!(failed, "{text}");
            assert!(
                text.contains("names 2 actions")
                    && text.contains("notes/Notebook/Todo#archive")
                    && text.contains("notes/Notebook/Groceries#archive"),
                "{text}"
            );
            assert!(text.contains("#archive"), "{text}");
            assert!(host.invoked().is_empty(), "nothing ran: {text}");
        });
    }
}
