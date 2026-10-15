//! The agent over a peer-to-peer D-Bus link to a stand-in `NetworkManager`,
//! answered through the same sheet controller the compositor draws, in the
//! shell's words.

use std::sync::{Arc, PoisonError};
use std::time::Duration;

use tilo_network_secret_agent::service::{AGENT_IDENTIFIER, CAPABILITY_VPN_HINTS};
use tilo_network_secret_agent::store::SecretStore;
use tilo_network_secret_agent::testing::{
    HANG_BOUND, Link, MemoryStore, WIFI_PATH, capture_logs, eap_connection, gsm_connection,
    request_for, string_of, vpn_connection, wifi_psk_connection,
};
use tilo_network_secret_agent::{
    FLAG_ALLOW_INTERACTION, FLAG_REQUEST_NEW, FLAG_USER_REQUESTED, MESSAGE_KEYS, SettingSecrets,
};
use winit::keyboard::{Key, NamedKey};
use zbus::zvariant::ObjectPath;

use super::*;
use crate::shell::portal_dialog::{PortalDialogController, PortalDialogQueue};

/// The agent served to a stand-in `NetworkManager`, asking in `queue`'s
/// sheets as the session does.
async fn link(queue: PortalDialogQueue, store: Arc<dyn SecretStore>) -> Link {
    tilo_network_secret_agent::testing::link(Arc::new(DialogSheets::new(queue)), store).await
}

#[allow(
    clippy::future_not_send,
    reason = "the sheet's text fields are UI widgets; each test awaits this on its own \
    current-thread runtime and never sends it"
)]
async fn sheet_up(controller: &mut PortalDialogController) -> serde_json::Value {
    let deadline = std::time::Instant::now()
        .checked_add(HANG_BOUND)
        .expect("the hang bound fits in an Instant");
    while std::time::Instant::now() < deadline {
        controller.pump();
        let state = controller.state_json();
        if !state.is_null() {
            return state;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no sheet appeared");
}

fn type_text(controller: &mut PortalDialogController, text: &str) {
    for ch in text.chars() {
        let key = if ch == ' ' {
            Key::Named(NamedKey::Space)
        } else {
            Key::Character(ch.to_string().into())
        };
        assert!(edit(controller, &key));
    }
}

fn edit(controller: &mut PortalDialogController, key: &Key) -> bool {
    let event = ui::synthetic_input::key_input(key, winit::event::ElementState::Pressed);
    crate::shell::test_support::with_scratch_event_ctx(|ctx| controller.edit_secret(&event, ctx))
}

#[tokio::test]
async fn a_wifi_password_is_asked_in_the_sheet_and_returned_only_to_networkmanager() {
    let (logs, _guard) = capture_logs();
    let queue = PortalDialogQueue::default();
    let store = Arc::new(MemoryStore::default());
    let link = link(queue.clone(), Arc::clone(&store) as Arc<dyn SecretStore>).await;
    assert_eq!(
        link.registered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_slice(),
        &[(AGENT_IDENTIFIER.to_string(), CAPABILITY_VPN_HINTS)],
        "registered once, under the stable identifier"
    );

    let reply = link
        .get_secrets(
            wifi_psk_connection(),
            "802-11-wireless-security",
            vec!["psk"],
            FLAG_ALLOW_INTERACTION,
        )
        .await;
    let mut controller = PortalDialogController::new(queue);
    let state = sheet_up(&mut controller).await;
    assert_eq!(state["kind"], "network_secrets");
    assert_eq!(state["title"], "Wi-Fi password required");
    assert!(
        state["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("“Home Net”")),
        "{state}"
    );
    assert_eq!(state["fields"][0]["key"], "psk");
    assert_eq!(state["fields"][0]["masked"], true);
    assert_eq!(state["remember"], true);

    // Too short for WPA: the sheet stays and says why.
    type_text(&mut controller, "short");
    assert!(!controller.confirm());
    assert!(controller.state_json()["error"].is_string());

    type_text(&mut controller, " horse");
    let state = controller.state_json();
    assert_eq!(state["fields"][0]["length"], 11);
    assert!(
        state["fields"][0].get("value").is_none(),
        "a masked value never reaches state-dump"
    );
    assert!(!state.to_string().contains("short horse"));
    assert!(controller.confirm());

    let reply = reply.await.expect("call task").expect("secrets returned");
    assert_eq!(
        string_of(&reply, "802-11-wireless-security", "psk"),
        "short horse"
    );
    assert_eq!(reply.len(), 1, "only the setting that was asked for");
    assert_eq!(
        store
            .get(
                "5f2c7c9e-0000-4000-8000-000000000001",
                "802-11-wireless-security"
            )
            .and_then(|secrets| secrets.get("psk").map(str::to_string))
            .as_deref(),
        Some("short horse"),
        "remembered, as the sheet offered"
    );
    assert!(controller.state_json().is_null(), "the sheet closed");

    let logs = logs.text();
    assert!(
        logs.contains("network secrets request finished"),
        "the capture must observe the agent: {logs}"
    );
    assert!(
        !logs.contains("short horse"),
        "a secret reached the log: {logs}"
    );
}

#[tokio::test]
async fn an_8021x_sign_in_returns_identity_and_password_in_the_8021x_setting() {
    let (logs, _guard) = capture_logs();
    let queue = PortalDialogQueue::default();
    let store = Arc::new(MemoryStore::default());
    let link = link(queue.clone(), Arc::clone(&store) as Arc<dyn SecretStore>).await;
    let reply = link
        .get_secrets(
            eap_connection(),
            "802-1x",
            Vec::new(),
            FLAG_ALLOW_INTERACTION | FLAG_USER_REQUESTED,
        )
        .await;
    let mut controller = PortalDialogController::new(queue);
    let state = sheet_up(&mut controller).await;
    assert_eq!(state["title"], "Network sign-in");
    let keys: Vec<&str> = state["fields"]
        .as_array()
        .expect("fields")
        .iter()
        .filter_map(|field| field["key"].as_str())
        .collect();
    assert_eq!(keys, ["identity", "anonymous-identity", "password"]);
    assert_eq!(state["fields"][0]["value"], "quinn@example.edu");
    assert_eq!(
        state["fields"][2]["focused"], true,
        "starts on the password"
    );
    assert!(
        state["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("certificate is not checked")),
        "an unchecked server certificate is said out loud: {state}"
    );

    type_text(&mut controller, "tr0ub4dor&3");
    // Untick "Remember": Tab to its row, Space.
    assert!(controller.select_next(false));
    assert!(edit(&mut controller, &Key::Named(NamedKey::Space)));
    assert_eq!(controller.state_json()["remember"], false);
    assert!(controller.confirm());

    let reply = reply.await.expect("call task").expect("secrets returned");
    let setting = reply.get("802-1x").expect("the 802-1x setting");
    assert_eq!(string_of(&reply, "802-1x", "identity"), "quinn@example.edu");
    assert_eq!(string_of(&reply, "802-1x", "password"), "tr0ub4dor&3");
    assert!(
        !setting.contains_key("anonymous-identity"),
        "an empty optional field is left out"
    );
    assert!(
        store
            .get("5f2c7c9e-0000-4000-8000-000000000002", "802-1x")
            .is_none(),
        "not remembered when unticked"
    );
    assert!(!logs.text().contains("tr0ub4dor"));
}

#[tokio::test]
async fn networkmanager_cancelling_takes_the_sheet_down() {
    let queue = PortalDialogQueue::default();
    let link = link(queue.clone(), Arc::new(MemoryStore::default())).await;
    let reply = link
        .get_secrets(
            wifi_psk_connection(),
            "802-11-wireless-security",
            Vec::new(),
            FLAG_ALLOW_INTERACTION,
        )
        .await;
    let mut controller = PortalDialogController::new(queue);
    sheet_up(&mut controller).await;
    type_text(&mut controller, "half typed");

    link.agent()
        .await
        .call::<_, _, ()>(
            "CancelGetSecrets",
            &(
                ObjectPath::from_static_str_unchecked(WIFI_PATH),
                "802-11-wireless-security",
            ),
        )
        .await
        .expect("cancel accepted");
    let error = reply
        .await
        .expect("call task")
        .expect_err("a withdrawn request has no secrets");
    assert!(
        error
            .to_string()
            .contains("org.freedesktop.NetworkManager.SecretAgent.AgentCanceled"),
        "{error}"
    );
    assert!(controller.pump(), "the withdrawn sheet is let go");
    assert!(controller.state_json().is_null());
    assert!(controller.secret_fields().is_empty());
}

#[tokio::test]
async fn dismissing_the_sheet_is_a_user_cancel() {
    let queue = PortalDialogQueue::default();
    let link = link(queue.clone(), Arc::new(MemoryStore::default())).await;
    let reply = link
        .get_secrets(
            wifi_psk_connection(),
            "802-11-wireless-security",
            Vec::new(),
            FLAG_ALLOW_INTERACTION,
        )
        .await;
    let mut controller = PortalDialogController::new(queue);
    sheet_up(&mut controller).await;
    assert!(controller.cancel());
    let error = reply.await.expect("call task").expect_err("dismissed");
    assert!(
        error
            .to_string()
            .contains("org.freedesktop.NetworkManager.SecretAgent.UserCanceled"),
        "{error}"
    );
}

#[tokio::test]
async fn remembered_secrets_answer_without_asking_unless_new_ones_are_wanted() {
    let queue = PortalDialogQueue::default();
    let store = Arc::new(MemoryStore::default());
    let mut remembered = SettingSecrets::default();
    remembered.insert("psk", "correct horse");
    store.put(
        "5f2c7c9e-0000-4000-8000-000000000001",
        "802-11-wireless-security",
        remembered,
    );
    let link = link(queue.clone(), Arc::clone(&store) as Arc<dyn SecretStore>).await;

    // Background activation: no interaction allowed, remembered answer.
    let reply = link
        .get_secrets(
            wifi_psk_connection(),
            "802-11-wireless-security",
            Vec::new(),
            0,
        )
        .await
        .await
        .expect("call task")
        .expect("remembered secrets");
    assert_eq!(
        string_of(&reply, "802-11-wireless-security", "psk"),
        "correct horse"
    );
    let mut idle = PortalDialogController::new(queue.clone());
    assert!(!idle.pump(), "answered without a sheet");

    // The remembered password was rejected: ask again, and say why.
    let reply = link
        .get_secrets(
            wifi_psk_connection(),
            "802-11-wireless-security",
            Vec::new(),
            FLAG_ALLOW_INTERACTION | FLAG_REQUEST_NEW,
        )
        .await;
    let mut controller = PortalDialogController::new(queue);
    let state = sheet_up(&mut controller).await;
    assert!(
        state["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("did not work")),
        "{state}"
    );
    type_text(&mut controller, "battery staple");
    assert!(controller.confirm());
    let reply = reply.await.expect("call task").expect("new secrets");
    assert_eq!(
        string_of(&reply, "802-11-wireless-security", "psk"),
        "battery staple"
    );
}

/// The agent words its sheets in keys; every one must read as the shell's
/// copy, never as the key.
#[test]
fn every_message_the_agent_words_a_sheet_with_reads_as_shell_copy() {
    for key in MESSAGE_KEYS {
        let copy = resolve_message(&Message::new(key));
        assert_ne!(copy, *key, "{key} has no entry in the shell's catalogue");
        assert!(!copy.trim().is_empty(), "{key} resolves to nothing");
    }
}

#[test]
fn a_sim_pin_sheet_reads_as_a_sim_pin() {
    let request = request_for(gsm_connection(), "gsm", &["pin"]);
    let sheet = sheet_request("nm-secrets:0:test/gsm", &request);
    assert_eq!(sheet.title, "SIM PIN required");
    assert_eq!(sheet.fields[0].label, "SIM PIN");
    assert_eq!(
        sheet.detail,
        "Enter the PIN to unlock the SIM for \u{201c}Carrier\u{201d}."
    );
}

#[test]
fn a_vpn_sheet_says_the_plugin_message_as_it_came() {
    let request = request_for(
        vpn_connection(),
        "vpn",
        &["cert-pass", "x-vpn-message:Use your one-time code"],
    );
    let sheet = sheet_request("nm-secrets:0:test/vpn", &request);
    assert_eq!(sheet.title, "VPN sign-in");
    assert!(sheet.detail.contains("one-time code"), "{}", sheet.detail);
    assert_eq!(
        sheet.detail,
        "Enter the secrets for \u{201c}Office\u{201d}.\nUse your one-time code"
    );
    assert_eq!(
        sheet.fields[0].label, "Cert pass",
        "a plugin's key, humanized"
    );
    assert_eq!(sheet.remember, Some(false));
}

/// A connection without a name is "this network", in the shell's words.
#[test]
fn an_unnamed_connection_is_this_network() {
    let mut connection = gsm_connection();
    connection
        .get_mut("connection")
        .expect("connection")
        .remove("id");
    let request = request_for(connection, "gsm", &[]);
    let sheet = sheet_request("nm-secrets:0:test/gsm", &request);
    assert_eq!(sheet.title, "Mobile broadband password required");
    assert_eq!(
        sheet.detail,
        "Enter the password for \u{201c}this network\u{201d}."
    );
}
