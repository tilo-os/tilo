//! The names a path may take: the address an action answers, the close
//! matches of an action nobody has, and the keys that tell colliding titles
//! apart.

use super::super::mcp_agent_paths::{CLOSE_MATCHES, close_matches};
use super::super::{Node, bound_key, pick};
use super::*;

/// An action nobody has is answered with what may have been meant, at most
/// [`CLOSE_MATCHES`] of them, never the app's whole list.
#[test]
fn an_action_no_resource_offers_names_at_most_five_close_matches() {
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

/// A title several resources share is refused with the key each one goes by:
/// the key is a name the next path can use.
#[test]
fn a_title_that_names_several_offers_the_keys_that_tell_them_apart() {
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

/// A resource is also named by the address an action answers it by
/// (`notes.notebook/page/<id>/body`): what a call said it changed is a path
/// the next call takes as it is.
#[test]
fn a_resource_is_named_by_the_address_an_action_answers() {
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

#[test]
fn bound_ids_decode_to_the_apps_own_name_and_nothing_else_does() {
    assert_eq!(
        bound_key(&bound("a", TODO_BODY)).as_deref(),
        Some(TODO_BODY)
    );
    assert_eq!(bound_key("notes.notebook"), None);
    assert_eq!(bound_key("tilo-ui-bound:v1:a:b:zz"), None);
    assert_eq!(bound_key("tilo-ui-bound:v1:a:b:abc"), None);
}
