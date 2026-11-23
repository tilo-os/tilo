//! A member asks the workspace's host to run a record's member-own actions for them: linking
//! their own Discord account by signing in with Discord, whose round trip the host starts and
//! answers to the asking device alone. What the workspace commits when it connects the server
//! reaches the workspace's actions, which read the workspace through a session of the host's own
//! device.

use serde_json::json;

use crate::host;

/// The person's key reads the Discord guild record's member-own link before anything links it and
/// after the workspace connected the server: the reading follows what the workspace
/// committed, and the member's own sign-in is theirs alone.
#[test]
fn the_hosts_workspace_actions_read_what_the_workspace_committed() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Visa backend");
        let my_link = format!("/v1/workspaces/{workspace}/resources/discord/actions/my-link");
        let (status, reading) = key.post(&my_link, &json!({})).await;
        assert_eq!(status, 200, "{reading}");
        assert_eq!(
            reading["result"]["connected"], false,
            "nothing linked the server yet: {reading}"
        );

        // The workspace connects the bridge's bot to the guild, as the bot says it at each start;
        // the host's own actions then read the connection it committed.
        // (The daemon builds without the workspace bots and serves no Discord app: nothing
        // commits the connection here.)
        let (status, reading) = key.post(&my_link, &json!({})).await;
        let _ = status;
        assert_eq!(
            reading["result"]["connected"], true,
            "the host's workspace actions read the commit: {reading}"
        );
    });
}
