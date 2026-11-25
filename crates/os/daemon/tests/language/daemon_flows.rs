//! What reaches a device's one daemon from outside it, each through the path production uses:
//! a key of the person's over `/v1` and MCP, an agent run on the device asking its person first,
//! and another device of theirs, or a key nobody certified, over the device link.

use serde_json::{Value, json};
use tilo_actions::remote::ActionProvider as _;
use tilo_actions::remote::device_tickets;

use crate::language::host;

/// The text of an MCP tool's result, and whether it failed.
fn said(result: &Value) -> (String, bool) {
    let text = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (text, result["isError"].as_bool().unwrap_or(false))
}

/// A backend's key lists what the workspace publishes on the daemon, runs an action and follows the invocation over `/v1`, and its agent lists and acts over the
/// workspace's MCP endpoint: every call answered by the daemon's dispatch.
#[test]
fn a_key_lists_reads_runs_and_follows_on_the_daemon() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Visa backend");

        let (status, listed) = key
            .get(&format!("/v1/workspaces/{workspace}/resources"))
            .await;
        assert_eq!(status, 200, "{listed}");
        let rows = listed["rows"].as_array().cloned().unwrap_or_default();
        assert!(
            rows.iter().any(|row| row["path"] == "devices"),
            "the device's own actions are listed: {listed}"
        );

        let (status, ran) = key
            .post(
                &format!("/v1/workspaces/{workspace}/resources/devices/actions/info"),
                &json!({}),
            )
            .await;
        assert_eq!(status, 200, "{ran}");
        assert_eq!(ran["status"], "ok", "{ran}");
        let invocation = ran["invocation"]
            .as_str()
            .expect("the invocation")
            .to_owned();
        let (status, followed) = key
            .get(&format!(
                "/v1/workspaces/{workspace}/invocations/{invocation}"
            ))
            .await;
        assert_eq!(status, 200, "{followed}");
        assert_eq!(followed["status"], "ok", "{followed}");
        assert_eq!(followed["invocation"], invocation.as_str(), "{followed}");
        assert_eq!(followed["result"], ran["result"], "{followed}");

        // The person on the daemon itself is answered the same.
        let here = host
            .dispatch()
            .invoke(
                &host.caller(),
                host::call(
                    device_tickets::PROVIDER,
                    tilo_actions::ActionAddress::new(device_tickets::RESOURCE, "info"),
                    &json!({}),
                ),
            )
            .await
            .expect("the dispatch answers")
            .unwrap();
        let tilo_actions::remote::ReceiptStatus::Completed { output } = &here.status else {
            panic!("the person reads the device: {here:?}");
        };
        assert_eq!(output.value.as_ref(), Some(&ran["result"]), "{ran}");

        let (listed, failed) = said(&key.tool("tilo_ls", &json!({"workspace": workspace})).await);
        assert!(!failed, "{listed}");
        assert!(listed.contains("devices"), "{listed}");
        let (done, failed) = said(
            &key.tool(
                "tilo_do",
                &json!({"workspace": workspace, "path": "devices#info"}),
            )
            .await,
        );
        assert!(!failed, "{done}");
        assert!(
            done.contains(&hex::encode(host.daemon().endpoint_addr().id.as_bytes())),
            "it answers which device it is: {done}"
        );
    });
}

/// An agent the person runs on this device (`tilo mcp`) lists what the device publishes, and is
/// refused adding a device, which only the person does: no ticket is served for it.
#[test]
fn an_agent_on_the_device_is_refused_what_only_its_person_does() {
    host::run(|host| async move {
        let agent = host.agent();
        let (listed, failed) = said(&agent.tool("tilo_ls", &json!({})).await);
        assert!(!failed, "{listed}");
        assert!(listed.contains("devices"), "{listed}");
        let (refused, failed) = said(
            &agent
                .tool("tilo_do", &json!({"path": "devices#issue"}))
                .await,
        );
        assert!(failed, "an agent adds no device: {refused}");
        assert!(
            !refused.contains("tilo login"),
            "no ticket is printed: {refused}"
        );
    });
}

/// Another device of the person's asks the daemon over the device link and is answered as the
/// person; a key the workspace has no certificate for is refused what the workspace opens to
/// nobody.
#[test]
fn a_device_of_the_person_is_answered_and_a_visitor_is_refused_over_the_device_link() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let laptop = host.member("Ada's laptop", &workspace).await;
        let governance = tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER;
        let at =
            |operation| tilo_actions::remote::workspace_governance_address(&workspace, operation);
        let listed = laptop
            .call(governance, at("invitation.list"), &json!({}))
            .await;

        assert!(
            matches!(listed, Ok(tilo_actions::InvocationUpdate::Succeeded { .. })),
            "the person's laptop lists the invitations: {listed:?}"
        );

        let visitor = host.visitor(&workspace).await;
        let refused = visitor
            .call(governance, at("invite"), &json!({"role": "editor"}))
            .await;
        assert!(
            matches!(
                refused,
                Err(tilo_actions::remote::ActionCallError::NotPermitted)
            ),
            "{refused:?}"
        );
    });
}

/// Whether a signed address names a relay: what reaches a device from another network.
fn relayed(hint: &tilo_peer_state::join::TransportHint) -> bool {
    let named: Value = serde_json::from_str(&hint.opaque_json).unwrap();
    named["endpoint_addr"]["addrs"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|addr| addr.get("Relay").is_some())
}

/// The daemon serves before its endpoint reaches a relay. An invitation made at once waits for
/// the relay and names it, so another person's computer joins through it, and the address the
/// workspace names for the daemon carries the relay too.
#[test]
fn an_invitation_made_at_start_names_the_relay_and_another_person_joins_with_it() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let laptop = host.member("Ada's laptop", &workspace).await;
        let invited = laptop
            .call(
                tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER,
                tilo_actions::remote::workspace_governance_address(&workspace, "invite"),
                &json!({"role": "editor"}),
            )
            .await;
        let Ok(tilo_actions::InvocationUpdate::Succeeded { output, .. }) = &invited else {
            panic!("the person's laptop invites: {invited:?}");
        };
        let answer = output.value.as_ref().expect("the invitation");
        let signed = hex::decode(answer["invitation"].as_str().expect("its bytes")).unwrap();
        let verified = tilo_peer_state::join::decode_and_verify_invitation(
            &signed,
            None,
            &tilo_user_identity::RevocationList::new(),
            tilo_clock::unix_now_ns(),
        )
        .unwrap();
        let hint = &verified.grant().transport[0];
        assert!(
            relayed(hint),
            "the invitation names the relay: {}",
            hint.opaque_json
        );

        let (key, cert) = host::stranger("Bob's phone");
        tilo_peer_state::join_transport::WorkspaceJoinClient::new(host::endpoint(&key).await)
            .redeem_hint(hint, signed, cert, &key, [7; 32])
            .await
            .expect("another person's computer joins through the relay");

        let named = host.daemon().workspaces().hosts(&workspace).await.unwrap();
        assert!(
            named.iter().any(relayed),
            "the workspace names the daemon at its relay: {named:?}"
        );
    });
}

/// What the workspace keeps outlives its daemon: an invitation made before the computer restarts
/// it is listed after, to a device of the person's dialling the next daemon, and a key reads the
/// workspace again.
#[test]
fn what_the_workspace_keeps_is_there_after_the_daemon_restarts() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let governance = tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER;
        let at =
            |operation| tilo_actions::remote::workspace_governance_address(&workspace, operation);
        let invited = host
            .member("Ada's laptop", &workspace)
            .await
            .call(governance, at("invite"), &json!({"role": "editor"}))
            .await;
        let Ok(tilo_actions::InvocationUpdate::Succeeded { output, .. }) = &invited else {
            panic!("the person's laptop invites: {invited:?}");
        };
        let invite = output.value.as_ref().expect("the invitation")["invite"]
            .as_str()
            .expect("its id")
            .to_owned();

        host.restart().await;

        let listed = host
            .member("Ada's phone", &workspace)
            .await
            .call(governance, at("invitation.list"), &json!({}))
            .await;
        let Ok(tilo_actions::InvocationUpdate::Succeeded { output, .. }) = &listed else {
            panic!("the person's phone lists the invitations: {listed:?}");
        };
        let listed = output.value.as_ref().expect("the invitations").to_string();
        assert!(listed.contains(&invite), "{invite} is still open: {listed}");
        let (status, read) = host
            .key("Visa backend")
            .get(&format!("/v1/workspaces/{workspace}/resources"))
            .await;
        assert_eq!(status, 200, "{read}");
    });
}
