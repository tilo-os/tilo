use std::sync::Arc;
use std::time::Duration;

use tilo_network_secret_agent::service::{AGENT_IDENTIFIER, CAPABILITY_VPN_HINTS};
use tilo_network_secret_agent::store::SecretStore;
use tilo_network_secret_agent::testing::{
    Link, MemoryStore, WIFI_PATH, capture_logs, eap_connection, gsm_connection, request_for,
    string_of, vpn_connection, wifi_psk_connection,
};
use tilo_network_secret_agent::{
    FLAG_ALLOW_INTERACTION, FLAG_REQUEST_NEW, FLAG_USER_REQUESTED, MESSAGE_KEYS, SettingSecrets,
};
use winit::keyboard::{Key, NamedKey};
use zbus::zvariant::ObjectPath;

use super::*;
use crate::shell::portal_dialog::{PortalDialogController, PortalDialogQueue};
use tilo_lock::MutexRecover as _;
use ui::widget::SecretPrompt;

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
    let deadline = tilo_test_env::hang_deadline();
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
        link.registered.lock_recover().as_slice(),
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
    assert!(controller.prompt().is_none(), "nothing typed is kept");
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

const SIM_PATH: &str = "/org/freedesktop/ModemManager1/SIM/0";

#[test]
fn a_sim_pin_sheet_reads_as_a_sim_pin() {
    let request = request_for(gsm_connection(), "gsm", &["pin"]);
    let sim = SimUnlock::of(
        os_actions::SimLock::Pin,
        os_actions::UnlockRetries {
            pin: Some(3),
            puk: Some(10),
        },
        Some(SIM_PATH.into()),
    );
    let sheet = sheet_request("nm-secrets:0:test/gsm", &request, Some(sim));
    assert_eq!(sheet.title, "SIM PIN required");
    assert_eq!(sheet.fields[0].label, "SIM PIN");
    assert_eq!(
        sheet.detail,
        "Enter the PIN to unlock the SIM for \u{201c}Carrier\u{201d}."
    );
    assert_eq!(sheet.remember, None, "a SIM PIN is never remembered");
    assert_eq!(
        sheet.sim,
        Some(SimUnlock {
            step: SimUnlockStep::Pin,
            attempts_left: Some(3),
            sim_path: Some(SIM_PATH.into()),
        })
    );
}

#[test]
fn a_sim_pin_without_a_modem_answer_is_still_asked_as_a_sim_pin() {
    let request = request_for(gsm_connection(), "gsm", &["pin"]);
    let sheet = sheet_request("nm-secrets:0:test/gsm", &request, None);
    assert_eq!(sheet.remember, None);
    assert_eq!(
        sheet.sim,
        Some(SimUnlock {
            step: SimUnlockStep::Pin,
            attempts_left: None,
            sim_path: None,
        })
    );
}

#[test]
fn a_mobile_password_is_no_sim_pin() {
    let request = request_for(gsm_connection(), "gsm", &["password"]);
    let sheet = sheet_request("nm-secrets:0:test/gsm", &request, None);
    assert_eq!(sheet.sim, None);
    assert!(sheet.remember.is_some());
}

#[test]
fn a_sim_its_puk_locks_is_asked_for_the_puk_and_a_new_pin() {
    let request = request_for(gsm_connection(), "gsm", &["pin"]);
    let sim = SimUnlock::of(
        os_actions::SimLock::Puk,
        os_actions::UnlockRetries {
            pin: Some(0),
            puk: Some(10),
        },
        Some(SIM_PATH.into()),
    );
    let sheet = sheet_request("nm-secrets:0:test/gsm", &request, Some(sim));
    assert_eq!(sheet.title, t("portal-sim-puk-title"));
    assert!(
        sheet.detail.contains("\u{201c}Carrier\u{201d}"),
        "{}",
        sheet.detail
    );
    assert_eq!(sheet.remember, None);
    let sim = sheet.sim.expect("a SIM request");
    assert_eq!(sim.step, SimUnlockStep::Puk);
    assert_eq!(
        sim.attempts_left,
        Some(10),
        "the PUK's attempts, not the PIN's"
    );
}

fn modem(path: &str, lock: os_actions::SimLock, sim: &str) -> os_actions::ModemInfo {
    os_actions::ModemInfo {
        path: path.into(),
        name: "Quectel EM05".into(),
        state: os_actions::ModemState::Locked,
        failure: None,
        lock,
        retries: os_actions::UnlockRetries {
            pin: Some(2),
            puk: Some(10),
        },
        pin_enabled: Some(true),
        signal: None,
        technology: None,
        operator: None,
        registration: os_actions::Registration::Idle,
        modes: None,
        supported_modes: Vec::new(),
        sims: vec![os_actions::SimCard {
            path: sim.into(),
            slot: None,
            active: true,
            operator: None,
            operator_code: None,
            iccid: None,
            kind: os_actions::SimKind::Physical,
        }],
        numbers: Vec::new(),
        traffic: None,
    }
}

#[test]
fn the_sim_asked_for_is_the_one_a_lock_holds() {
    let reading = os_actions::MobileReading {
        modems: vec![
            modem(
                "/org/freedesktop/ModemManager1/Modem/0",
                os_actions::SimLock::None,
                "/org/freedesktop/ModemManager1/SIM/0",
            ),
            modem(
                "/org/freedesktop/ModemManager1/Modem/1",
                os_actions::SimLock::Puk,
                "/org/freedesktop/ModemManager1/SIM/1",
            ),
        ],
    };
    assert_eq!(
        sim_unlock(&reading),
        Some(SimUnlock {
            step: SimUnlockStep::Puk,
            attempts_left: Some(10),
            sim_path: Some("/org/freedesktop/ModemManager1/SIM/1".into()),
        })
    );
    assert_eq!(sim_unlock(&os_actions::MobileReading::default()), None);
}

/// The SIM's state as a test sets it, and the connections started again.
#[derive(Default)]
struct FixedModem {
    sim: std::sync::Mutex<Option<SimUnlock>>,
    rejoined: std::sync::Mutex<Vec<String>>,
}

impl ModemSide for FixedModem {
    fn read_sim(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<SimUnlock>> + Send>> {
        let sim = self.sim.lock_recover().clone();
        Box::pin(async move { sim })
    }

    fn rejoin(&self, connection_path: &str) {
        self.rejoined
            .lock_recover()
            .push(connection_path.to_owned());
    }
}

/// Unlocks every SIM it is handed a PUK for, and keeps what it was handed.
#[derive(Default)]
struct AcceptingUnlocker {
    sent: std::sync::Mutex<Vec<(String, String, String)>>,
}

impl SimUnlocker for AcceptingUnlocker {
    fn send_puk(
        &self,
        sim_path: &str,
        puk: Zeroizing<String>,
        new_pin: Zeroizing<String>,
    ) -> Result<(), PukRefusal> {
        self.sent.lock_recover().push((
            sim_path.to_owned(),
            puk.as_str().to_owned(),
            new_pin.as_str().to_owned(),
        ));
        Ok(())
    }
}

#[tokio::test]
async fn a_sim_pin_goes_back_to_networkmanager_with_the_attempts_left_shown() {
    let queue = PortalDialogQueue::default();
    let modem = Arc::new(FixedModem::default());
    *modem.sim.lock_recover() = Some(SimUnlock {
        step: SimUnlockStep::Pin,
        attempts_left: Some(3),
        sim_path: Some(SIM_PATH.into()),
    });
    let store = Arc::new(MemoryStore::default());
    let link = tilo_network_secret_agent::testing::link(
        Arc::new(DialogSheets::with_modem(
            queue.clone(),
            Arc::clone(&modem) as Arc<dyn ModemSide>,
        )),
        Arc::clone(&store) as Arc<dyn SecretStore>,
    )
    .await;
    let reply = link
        .get_secrets(gsm_connection(), "gsm", vec!["pin"], FLAG_ALLOW_INTERACTION)
        .await;
    let mut controller = PortalDialogController::new(queue);
    let state = sheet_up(&mut controller).await;
    assert_eq!(state["sim"]["step"], "pin");
    assert_eq!(state["sim"]["attempts_left"], 3);
    assert_eq!(state["confirm_label"], t("portal-sim-unlock"));
    assert_eq!(state["remember"], serde_json::Value::Null);
    type_text(&mut controller, "1234");
    assert!(controller.confirm());
    let reply = reply.await.expect("call task").expect("the PIN returned");
    assert_eq!(string_of(&reply, "gsm", "pin"), "1234");
    assert!(
        store
            .get("5f2c7c9e-0000-4000-8000-000000000004", "gsm")
            .is_none(),
        "a SIM PIN is never kept"
    );
    assert!(modem.rejoined.lock_recover().is_empty());
}

#[tokio::test]
async fn a_sim_locked_to_its_puk_is_unlocked_through_the_modem_and_joined_again() {
    let queue = PortalDialogQueue::default();
    let modem = Arc::new(FixedModem::default());
    *modem.sim.lock_recover() = Some(SimUnlock {
        step: SimUnlockStep::Puk,
        attempts_left: Some(10),
        sim_path: Some(SIM_PATH.into()),
    });
    let link = tilo_network_secret_agent::testing::link(
        Arc::new(DialogSheets::with_modem(
            queue.clone(),
            Arc::clone(&modem) as Arc<dyn ModemSide>,
        )),
        Arc::new(MemoryStore::default()),
    )
    .await;
    // NetworkManager asks for the PIN again after the last wrong one.
    let reply = link
        .get_secrets(
            gsm_connection(),
            "gsm",
            vec!["pin"],
            FLAG_ALLOW_INTERACTION | FLAG_REQUEST_NEW,
        )
        .await;
    let unlocker = Arc::new(AcceptingUnlocker::default());
    let mut controller = PortalDialogController::new(queue)
        .with_sim_unlocker(Arc::clone(&unlocker) as Arc<dyn SimUnlocker>);
    let state = sheet_up(&mut controller).await;
    assert_eq!(state["sim"]["step"], "puk");
    assert_eq!(state["title"], t("portal-sim-puk-title"));
    assert_eq!(
        state["fields"].as_array().map(|fields| fields
            .iter()
            .map(|field| field["key"].clone())
            .collect::<Vec<_>>()),
        Some(vec!["puk".into(), "new-pin".into(), "confirm-pin".into()])
    );
    assert!(
        controller.prompt().and_then(SecretPrompt::error).is_none(),
        "a PUK sheet starts without the PIN's error"
    );
    type_text(&mut controller, "12345678");
    assert!(edit(&mut controller, &Key::Named(NamedKey::Tab)));
    type_text(&mut controller, "2468");
    assert!(edit(&mut controller, &Key::Named(NamedKey::Tab)));
    type_text(&mut controller, "2468");
    assert!(!controller.confirm(), "the sheet waits for the modem");
    assert!(controller.wait_for_puk_attempt());
    assert!(controller.state_json().is_null(), "the sheet closed");

    let error = reply
        .await
        .expect("call task")
        .expect_err("NetworkManager gets no PIN for an unlocked SIM");
    assert!(
        error
            .to_string()
            .contains("org.freedesktop.NetworkManager.SecretAgent.UserCanceled"),
        "{error}"
    );
    assert_eq!(
        unlocker.sent.lock_recover().as_slice(),
        [(
            SIM_PATH.to_owned(),
            "12345678".to_owned(),
            "2468".to_owned()
        )]
    );
    assert_eq!(
        modem.rejoined.lock_recover().as_slice(),
        [WIFI_PATH.to_owned()],
        "the connection starts again on the unlocked SIM"
    );
}

#[test]
fn a_vpn_sheet_says_the_plugin_message_as_it_came() {
    let request = request_for(
        vpn_connection(),
        "vpn",
        &["cert-pass", "x-vpn-message:Use your one-time code"],
    );
    let sheet = sheet_request("nm-secrets:0:test/vpn", &request, None);
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
    let sheet = sheet_request("nm-secrets:0:test/gsm", &request, None);
    assert_eq!(sheet.title, "Mobile broadband password required");
    assert_eq!(
        sheet.detail,
        "Enter the password for \u{201c}this network\u{201d}."
    );
}

/// A network `NetworkManager` could not join on its own for want of a usable
/// password: the person reads "Couldn't join <network>" once, and its
/// action joins that connection as them, which asks for the password.
#[test]
fn a_network_that_could_not_be_joined_is_told_once_with_a_join_action() {
    let request = request_for(wifi_psk_connection(), "802-11-wireless-security", &[]);
    let notice = CannotJoin::new(&request).expect("a network to name");
    assert_eq!(notice.network, "Home Net", "named by its SSID");
    assert_eq!(
        notice.action(),
        format!("{JOIN_ACTION_PREFIX}{}", request.connection_path)
    );
    let shown = notice.notification();
    assert!(shown.title.contains("Home Net"), "{}", shown.title);

    let before = drain_cannot_join().len();
    assert_eq!(before, 0);
    notice.clone().queue();
    notice.queue();
    let told = drain_cannot_join();
    assert_eq!(told.len(), 1, "told once per network");
    assert_eq!(
        told[0].default_action_key(),
        Some(format!("{JOIN_ACTION_PREFIX}{}", request.connection_path)),
        "the notification joins the network"
    );
}
