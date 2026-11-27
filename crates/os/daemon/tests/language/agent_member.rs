//! A member asks the workspace's host to run a record's member-own actions for them: linking
//! their own Discord account by signing in with Discord, whose round trip the host starts and
//! answers to the asking device alone. What the workspace commits when it connects the server
//! reaches the workspace's actions, which read the workspace through a session of the host's own
//! device.

use serde_json::json;
use tilo_actions::remote::{ActionCallError, application_action_target};

use crate::language::host;

/// The person's key reads the Discord guild record's member-own link before anything links it and
/// after the workspace connected the server: the reading follows what the workspace
/// committed, and the member's own sign-in is theirs alone.
#[test]
fn the_hosts_workspace_actions_read_what_the_workspace_committed() {
    // The Discord connector is the workspace's host app, a package of this tree's `apps/`: the
    // device installs it and the daemon starts it as a person's computer does, so `my-link` and
    // `connect` are the app's own actions.
    host::run_with_packages(&[], &[("discord", "discord")], |host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Visa backend");
        let at =
            |action: &str| format!("/v1/workspaces/{workspace}/resources/discord/actions/{action}");
        let (status, reading) = key.post(&at("my-link"), &json!({})).await;
        assert_eq!(status, 200, "{reading}");
        assert_eq!(
            reading["result"]["connected"], false,
            "nothing linked the server yet: {reading}"
        );

        // The workspace connects the bridge's bot to the guild, as the bot says it at each start;
        // the host's own actions then read the connection it committed.
        let (status, connected) = key
            .post(
                &at("connect"),
                &json!({"guild_id": "123456789012345678", "name": "Ada's server"}),
            )
            .await;
        assert_eq!(status, 200, "the workspace connects the guild: {connected}");
        let (status, reading) = key.post(&at("my-link"), &json!({})).await;
        assert_eq!(status, 200, "{reading}");
        assert_eq!(
            reading["result"]["connected"], true,
            "the host's workspace actions read the commit: {reading}"
        );
    });
}

/// Publishing a member-own action offers it; admission still decides each call: another member of
/// the workspace is offered it and refused nothing at the door, while a device the workspace never
/// admitted is refused every one of them.
#[test]
fn a_member_is_offered_the_member_own_actions_and_a_stranger_is_refused() {
    host::run_with_packages(&[], &[("discord", "discord")], |host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Visa backend");
        let at =
            |action: &str| format!("/v1/workspaces/{workspace}/resources/discord/actions/{action}");
        // Reading the guild starts the app, as a member's first look at it does; the host then
        // offers the record's own actions.
        let (status, reading) = key.post(&at("my-link"), &json!({})).await;
        assert_eq!(status, 200, "{reading}");
        let (status, connected) = key
            .post(
                &at("connect"),
                &json!({"guild_id": "123456789012345678", "name": "Ada's server"}),
            )
            .await;
        assert_eq!(status, 200, "the workspace connects the guild: {connected}");

        // Another member of the workspace reads their own link through the host: the action is
        // offered to them too, though they are no owner of the workspace.
        let (other, _) = host.other_person("Bo", &workspace).await;
        let target =
            application_action_target(&workspace, "so.tilo.discord", "discord.guild", "my-link");
        let read = other
            .call(&target.provider, target.address.clone(), &json!({}))
            .await
            .expect("the device link answers");
        assert!(
            matches!(read, tilo_actions::InvocationUpdate::Succeeded { .. }),
            "another member reads their own link: {read:?}"
        );

        // A key the workspace has no certificate for asks for the same actions and is refused:
        // being offered them is not being admitted to them.
        let stranger = host.visitor(&workspace).await;
        let refused = stranger
            .call(&target.provider, target.address.clone(), &json!({}))
            .await;
        assert!(
            matches!(
                refused,
                Err(ActionCallError::NotPermitted | ActionCallError::Refused { .. })
            ),
            "a stranger is refused the member's own actions: {refused:?}"
        );
    });
}
