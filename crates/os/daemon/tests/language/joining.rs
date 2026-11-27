//! A device whose first request goes out as it joins. A host takes no request from a device it
//! has not admitted to the workspace yet, so a request sent in the moment between asking to join
//! and being admitted is kept until it is answered, never refused as not admitted.

use crate::language::guests::guest;
use crate::language::host;

/// A person's device joins the workspace by its invitation and asks for the workspace's people at
/// once: the host answers it, and a key's first ask is answered too.
#[test]
fn a_request_sent_as_a_device_joins_is_answered_once_it_is_admitted() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let owner_device = host.member("its owner's computer", &workspace).await;
        // The guest's device joins, and its first request is the workspace's people.
        let joined = guest(&host, &owner_device, "editor", "Grace").await;
        let answered = joined.members(&workspace).await;
        assert!(
            answered.is_ok(),
            "the host answers the joining device's first request: {answered:?}"
        );

        // A key that has just been made asks at once, and is answered rather than turned away.
        let key = host.key("Visa backend");
        let (status, listed) = key
            .get(&format!("/v1/workspaces/{workspace}/resources"))
            .await;
        assert_eq!(status, 200, "the key's first ask is answered: {listed}");
    });
}
