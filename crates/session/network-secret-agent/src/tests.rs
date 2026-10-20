//! The agent over a peer-to-peer D-Bus link to a stand-in `NetworkManager`,
//! answered by scripted sheets. The shell's tests drive the same agent into
//! the sheet the compositor draws, in the shell's words.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zbus::zvariant::{ObjectPath, Value};
use zeroize::Zeroizing;

use super::service::{AGENT_IDENTIFIER, CAPABILITY_VPN_HINTS, SheetReply, Sheets};
use super::store::{RoamingWifi, RoamingWifiSlot, SecretStore, StoreKey, VaultStore};
use super::testing::{
    HANG_BOUND, MemoryStore, WIFI_PATH, capture_logs, eap_connection, gsm_connection, link,
    request_for, string_of, vpn_connection, wifi_psk_connection,
};
use super::*;
use tilo_lock::MutexRecover as _;

/// Sheets that answer from a script, in order; with the script spent a
/// sheet stays up until it is taken down.
#[derive(Default)]
struct ScriptedSheets {
    answers: Mutex<VecDeque<Option<SheetAnswer>>>,
    asked: Mutex<Vec<(String, SecretRequest)>>,
    cancelled: Mutex<Vec<String>>,
    /// Requests the agent could not ask for, by connection id.
    cannot_ask: Mutex<Vec<String>>,
}

impl ScriptedSheets {
    fn answering(answers: impl IntoIterator<Item = Option<SheetAnswer>>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers.into_iter().collect()),
            ..Self::default()
        })
    }

    fn asked(&self) -> Vec<(String, SecretRequest)> {
        self.asked.lock_recover().clone()
    }

    fn cancelled(&self) -> Vec<String> {
        self.cancelled.lock_recover().clone()
    }

    /// The first `count` sheets asked for, once they have been.
    async fn sheets_up(&self, count: usize) -> Vec<(String, SecretRequest)> {
        let deadline = std::time::Instant::now()
            .checked_add(HANG_BOUND)
            .expect("the hang bound fits in an Instant");
        while std::time::Instant::now() < deadline {
            let asked = self.asked();
            if asked.len() >= count {
                return asked;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("fewer than {count} sheets appeared");
    }

    /// The first sheet asked for, once it has been.
    async fn sheet_up(&self) -> (String, SecretRequest) {
        self.sheets_up(1).await.into_iter().next().expect("a sheet")
    }
}

impl Sheets for ScriptedSheets {
    fn ask(&self, handle: &str, request: &SecretRequest) -> SheetReply {
        self.asked
            .lock_recover()
            .push((handle.to_owned(), request.clone()));
        let next = self.answers.lock_recover().pop_front();
        match next {
            Some(answer) => Box::pin(async move { answer }),
            None => Box::pin(std::future::pending()),
        }
    }

    fn cancel(&self, handle: &str) {
        self.cancelled.lock_recover().push(handle.to_owned());
    }

    fn cannot_ask(&self, request: &SecretRequest) {
        self.cannot_ask
            .lock_recover()
            .push(request.connection_id.clone());
    }
}

fn typed(values: &[&str], remember: bool) -> SheetAnswer {
    SheetAnswer {
        values: values
            .iter()
            .map(|value| Zeroizing::new((*value).to_string()))
            .collect(),
        remember,
    }
}

fn message(key: &'static str, args: &[(&'static str, Line)]) -> Line {
    Line::Message(Message {
        key,
        args: args.to_vec(),
    })
}

#[tokio::test]
async fn a_wifi_password_is_asked_in_the_sheet_and_returned_only_to_networkmanager() {
    let (logs, _guard) = capture_logs();
    let sheets = ScriptedSheets::answering([Some(typed(&["short horse"], true))]);
    let store = Arc::new(MemoryStore::default());
    let link = link(
        Arc::clone(&sheets) as Arc<dyn Sheets>,
        Arc::clone(&store) as Arc<dyn SecretStore>,
    )
    .await;
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
    let reply = reply.await.expect("call task").expect("secrets returned");

    let asked = sheets.asked();
    assert_eq!(asked.len(), 1, "one sheet");
    let (handle, request) = &asked[0];
    assert_eq!(request.title, Message::new("portal-network-wifi-title"));
    assert_eq!(
        request.detail,
        [message(
            "portal-network-enter-password",
            &[("network", Line::from("Home Net"))]
        )],
        "worded with the network's name"
    );
    assert_eq!(request.fields.len(), 1);
    assert_eq!(request.fields[0].key, "psk");
    assert!(request.fields[0].masked);
    assert_eq!(request.fields[0].rule, SecretFieldRule::WpaPassphrase);
    assert!(request.remember_default);
    assert_eq!(
        sheets.cancelled(),
        std::slice::from_ref(handle),
        "the answered sheet is let go"
    );

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
    // Identity kept, anonymous identity left empty, "Remember" unticked.
    let sheets = ScriptedSheets::answering([Some(typed(
        &["quinn@example.edu", "", "tr0ub4dor&3"],
        false,
    ))]);
    let store = Arc::new(MemoryStore::default());
    let link = link(
        Arc::clone(&sheets) as Arc<dyn Sheets>,
        Arc::clone(&store) as Arc<dyn SecretStore>,
    )
    .await;
    let reply = link
        .get_secrets(
            eap_connection(),
            "802-1x",
            Vec::new(),
            FLAG_ALLOW_INTERACTION | FLAG_USER_REQUESTED,
        )
        .await;
    let reply = reply.await.expect("call task").expect("secrets returned");

    let (_, request) = sheets.sheet_up().await;
    assert_eq!(request.title, Message::new("portal-network-sign-in-title"));
    let keys: Vec<&str> = request
        .fields
        .iter()
        .map(|field| field.key.as_str())
        .collect();
    assert_eq!(keys, ["identity", "anonymous-identity", "password"]);
    assert_eq!(request.fields[0].initial, "quinn@example.edu");
    assert!(
        !request.fields[1].required,
        "the anonymous identity is optional"
    );
    assert!(request.fields[2].masked);
    assert_eq!(
        request.detail.last(),
        Some(&message("portal-network-cert-unchecked", &[])),
        "an unchecked server certificate is said out loud: {request:?}"
    );

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
    let sheets = Arc::new(ScriptedSheets::default());
    let link = link(
        Arc::clone(&sheets) as Arc<dyn Sheets>,
        Arc::new(MemoryStore::default()),
    )
    .await;
    let reply = link
        .get_secrets(
            wifi_psk_connection(),
            "802-11-wireless-security",
            Vec::new(),
            FLAG_ALLOW_INTERACTION,
        )
        .await;
    let (handle, _) = sheets.sheet_up().await;

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
    assert_eq!(
        sheets.cancelled(),
        [handle],
        "the withdrawn sheet is let go"
    );
}

#[tokio::test]
async fn dismissing_the_sheet_is_a_user_cancel() {
    let sheets = ScriptedSheets::answering([None]);
    let link = link(
        Arc::clone(&sheets) as Arc<dyn Sheets>,
        Arc::new(MemoryStore::default()),
    )
    .await;
    let reply = link
        .get_secrets(
            wifi_psk_connection(),
            "802-11-wireless-security",
            Vec::new(),
            FLAG_ALLOW_INTERACTION,
        )
        .await;
    let error = reply.await.expect("call task").expect_err("dismissed");
    assert!(
        error
            .to_string()
            .contains("org.freedesktop.NetworkManager.SecretAgent.UserCanceled"),
        "{error}"
    );
    assert_eq!(sheets.asked().len(), 1, "dismissed in the sheet");
}

/// A request `NetworkManager` replaces is withdrawn; its successor's sheet
/// has its own handle, so letting the first go never takes the second down.
#[tokio::test]
async fn a_replaced_request_never_takes_down_its_successors_sheet() {
    let sheets = Arc::new(ScriptedSheets::default());
    let link = link(
        Arc::clone(&sheets) as Arc<dyn Sheets>,
        Arc::new(MemoryStore::default()),
    )
    .await;
    let first = link
        .get_secrets(
            wifi_psk_connection(),
            "802-11-wireless-security",
            Vec::new(),
            FLAG_ALLOW_INTERACTION,
        )
        .await;
    let (first_handle, _) = sheets.sheet_up().await;
    let _second = link
        .get_secrets(
            wifi_psk_connection(),
            "802-11-wireless-security",
            Vec::new(),
            FLAG_ALLOW_INTERACTION,
        )
        .await;
    let error = first
        .await
        .expect("call task")
        .expect_err("the replaced request is withdrawn");
    assert!(
        error
            .to_string()
            .contains("org.freedesktop.NetworkManager.SecretAgent.AgentCanceled"),
        "{error}"
    );
    let asked = sheets.sheets_up(2).await;
    assert_eq!(asked.len(), 2, "the successor asked: {asked:?}");
    assert_ne!(asked[0].0, asked[1].0, "each request has its own sheet");
    assert_eq!(sheets.cancelled(), [first_handle]);
}

#[tokio::test]
async fn remembered_secrets_answer_without_asking_unless_new_ones_are_wanted() {
    let sheets = ScriptedSheets::answering([Some(typed(&["battery staple"], true))]);
    let store = Arc::new(MemoryStore::default());
    let mut remembered = SettingSecrets::default();
    remembered.insert("psk", "correct horse");
    store.put(
        "5f2c7c9e-0000-4000-8000-000000000001",
        "802-11-wireless-security",
        remembered,
    );
    let link = link(
        Arc::clone(&sheets) as Arc<dyn Sheets>,
        Arc::clone(&store) as Arc<dyn SecretStore>,
    )
    .await;

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
    assert!(sheets.asked().is_empty(), "answered without a sheet");

    // The remembered password was rejected: ask again, and say why.
    let reply = link
        .get_secrets(
            wifi_psk_connection(),
            "802-11-wireless-security",
            Vec::new(),
            FLAG_ALLOW_INTERACTION | FLAG_REQUEST_NEW,
        )
        .await;
    let reply = reply.await.expect("call task").expect("new secrets");
    let (_, request) = sheets.sheet_up().await;
    assert_eq!(
        request.detail.first(),
        Some(&message("portal-network-retry", &[])),
        "{request:?}"
    );
    assert_eq!(
        string_of(&reply, "802-11-wireless-security", "psk"),
        "battery staple"
    );
}

#[tokio::test]
async fn nothing_remembered_and_no_asking_is_no_secrets() {
    let link = link(
        Arc::new(ScriptedSheets::default()),
        Arc::new(MemoryStore::default()),
    )
    .await;
    let error = link
        .get_secrets(
            wifi_psk_connection(),
            "802-11-wireless-security",
            Vec::new(),
            0,
        )
        .await
        .await
        .expect("call task")
        .expect_err("no secrets");
    assert!(
        error
            .to_string()
            .contains("org.freedesktop.NetworkManager.SecretAgent.NoSecrets"),
        "{error}"
    );
}

#[tokio::test]
async fn saved_agent_owned_secrets_are_kept_and_deleted_with_the_connection() {
    let store = Arc::new(MemoryStore::default());
    let link = link(
        Arc::new(ScriptedSheets::default()),
        Arc::clone(&store) as Arc<dyn SecretStore>,
    )
    .await;
    let mut connection = wifi_psk_connection();
    connection
        .get_mut("802-11-wireless-security")
        .expect("security")
        .extend([
            ("psk", Value::from("correct horse")),
            ("psk-flags", Value::from(SECRET_AGENT_OWNED)),
            // A system-owned secret stays NetworkManager's business.
            ("leap-password", Value::from("not ours")),
        ]);
    let agent = link.agent().await;
    agent
        .call::<_, _, ()>(
            "SaveSecrets",
            &(
                &connection,
                ObjectPath::from_static_str_unchecked(WIFI_PATH),
            ),
        )
        .await
        .expect("saved");
    let kept = store
        .get(
            "5f2c7c9e-0000-4000-8000-000000000001",
            "802-11-wireless-security",
        )
        .expect("kept");
    assert_eq!(kept.get("psk"), Some("correct horse"));
    assert_eq!(kept.get("leap-password"), None);

    agent
        .call::<_, _, ()>(
            "DeleteSecrets",
            &(
                &connection,
                ObjectPath::from_static_str_unchecked(WIFI_PATH),
            ),
        )
        .await
        .expect("deleted");
    assert!(
        store
            .get(
                "5f2c7c9e-0000-4000-8000-000000000001",
                "802-11-wireless-security"
            )
            .is_none()
    );
}

#[test]
fn vpn_secrets_come_from_hints_and_go_back_under_vpn_secrets() {
    let request = request_for(
        vpn_connection(),
        "vpn",
        &["password", "x-vpn-message:Use your one-time code"],
    );
    assert_eq!(request.fields.len(), 1);
    assert_eq!(request.fields[0].key, "password");
    assert!(
        request
            .detail
            .contains(&Line::from("Use your one-time code")),
        "the plugin's message, as it came: {:?}",
        request.detail
    );
    assert!(
        !request.remember_default,
        "a VPN secret is remembered by choice"
    );
    let mut secrets = SettingSecrets::default();
    secrets.insert("password", "123456");
    let reply = request.response(&secrets);
    let vpn = reply.get("vpn").expect("vpn setting");
    let inner = HashMap::<String, String>::try_from(
        vpn.get("secrets")
            .expect("secrets")
            .try_clone()
            .expect("clone"),
    )
    .expect("a{ss}");
    assert_eq!(inner.get("password").map(String::as_str), Some("123456"));
}

#[test]
fn a_sim_pin_is_asked_as_a_pin() {
    let request = request_for(gsm_connection(), "gsm", &["pin"]);
    assert_eq!(request.title, Message::new("portal-network-sim-pin-title"));
    assert_eq!(request.fields[0].key, "pin");
    assert!(request.fields[0].masked);
}

/// The shell's catalogue is checked against [`MESSAGE_KEYS`]; a key the
/// agent words a sheet with but does not list there would go unchecked.
#[test]
fn every_key_the_agent_words_a_sheet_with_is_listed() {
    let source = include_str!("lib.rs");
    // Everything but the list itself.
    let (before, list) = source
        .split_once("pub const MESSAGE_KEYS")
        .expect("the list");
    let (_, after) = list.split_once("];").expect("the list's end");
    let code = format!("{before}{after}");
    let mut used: Vec<String> = code
        .split("\"portal-network-")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .map(|suffix| format!("portal-network-{suffix}"))
        .collect();
    used.sort_unstable();
    used.dedup();
    assert!(!used.is_empty(), "the scan must find the agent's keys");
    let mut listed: Vec<String> = MESSAGE_KEYS.iter().map(ToString::to_string).collect();
    listed.sort_unstable();
    assert_eq!(used, listed);
}

/// The person's devices share one copy of a passphrase; a stand-in that
/// records what was shared.
#[derive(Default)]
struct FakeRoaming {
    shared: Mutex<Vec<(String, os_actions::WifiSecurity, String)>>,
}

impl RoamingWifi for FakeRoaming {
    fn password(&self, ssid: &str, security: os_actions::WifiSecurity) -> Option<String> {
        self.shared
            .lock_recover()
            .iter()
            .find(|(name, mode, _)| name == ssid && *mode == security)
            .map(|(_, _, password)| password.clone())
    }

    fn share(
        &self,
        ssid: &str,
        security: os_actions::WifiSecurity,
        password: &str,
    ) -> Result<(), String> {
        self.shared
            .lock_recover()
            .push((ssid.into(), security, password.into()));
        Ok(())
    }
}

#[test]
fn the_vault_keeps_secrets_sealed_and_wpa_passphrases_roam() {
    let dir = tempfile::tempdir().expect("temp dir");
    let roaming = RoamingWifiSlot::default();
    let vault = VaultStore::at(
        dir.path().join("network-secrets"),
        secret_vault::Custody::LocalFile,
        roaming.clone(),
    );
    let eap = StoreKey {
        uuid: "uuid-eap".into(),
        setting: "802-1x".into(),
        roaming_wifi: None,
    };
    let mut secrets = SettingSecrets::default();
    secrets.insert("identity", "quinn@example.edu");
    secrets.insert("password", "tr0ub4dor&3");
    vault.keep(&eap, &secrets).expect("kept");
    let sealed = std::fs::read(vault.dir().join(secret_vault::VAULT_FILE)).expect("vault file");
    assert!(
        !String::from_utf8_lossy(&sealed).contains("tr0ub4dor"),
        "the vault file is sealed"
    );
    assert_eq!(vault.load(&eap), Some(secrets));

    // With the person's devices sharing Wi-Fi, a WPA passphrase roams and
    // is not also kept locally.
    let fake = Arc::new(FakeRoaming::default());
    roaming.set(Arc::clone(&fake) as Arc<dyn RoamingWifi>);
    let wifi = StoreKey {
        uuid: "uuid-wifi".into(),
        setting: "802-11-wireless-security".into(),
        roaming_wifi: Some(("Home Net".into(), os_actions::WifiSecurity::Wpa2)),
    };
    let mut psk = SettingSecrets::default();
    psk.insert("psk", "correct horse");
    vault.keep(&wifi, &psk).expect("shared");
    assert_eq!(
        fake.shared.lock_recover().as_slice(),
        &[(
            "Home Net".to_string(),
            os_actions::WifiSecurity::Wpa2,
            "correct horse".to_string()
        )]
    );
    assert_eq!(
        vault
            .load(&wifi)
            .and_then(|s| s.get("psk").map(str::to_string))
            .as_deref(),
        Some("correct horse")
    );

    vault.forget_connection("uuid-eap").expect("forgotten");
    assert_eq!(vault.load(&eap), None);
}

/// The session serves the agent from its multi-thread runtime; choosing the
/// vault's custody there probes the secret store over blocking D-Bus, which
/// once aborted the whole session at startup.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serving_from_the_session_runtime_never_blocks_a_runtime_thread() {
    let config = tempfile::tempdir().expect("config dir");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_CONFIG_HOME", config.path());
    env.set(
        "DBUS_SESSION_BUS_ADDRESS",
        "unix:path=/nonexistent/session-bus",
    );
    env.set(
        "DBUS_SYSTEM_BUS_ADDRESS",
        "unix:path=/nonexistent/system-bus",
    );
    let served = tokio::time::timeout(
        HANG_BOUND,
        service::serve(
            Arc::new(ScriptedSheets::default()),
            RoamingWifiSlot::default(),
        ),
    )
    .await
    .expect("serve answers");
    assert!(served.is_err(), "no system bus to serve on");
}

/// Wipes and redaction hold wherever the agent goes: a secret never shows
/// in `Debug`.
#[test]
fn setting_secrets_debug_shows_keys_only() {
    let mut secrets = SettingSecrets::default();
    secrets.insert("psk", "correct horse");
    let shown = format!("{secrets:?}");
    assert!(shown.contains("psk"), "{shown}");
    assert!(!shown.contains("correct horse"), "{shown}");
}

/// On 2026-10-19 the laptop's Wi-Fi never came up: each time
/// `NetworkManager` connected on its own, the session's agent answered with
/// a remembered password the network rejected, and asked nobody. A
/// remembered secret `NetworkManager` rejected is never answered again on
/// its own; the person is told (`cannot_ask`) instead, and what they type
/// next is remembered and answered from then on.
#[tokio::test]
async fn a_rejected_remembered_secret_is_never_answered_again_and_the_person_is_told() {
    const SETTING: &str = "802-11-wireless-security";
    const UUID: &str = "5f2c7c9e-0000-4000-8000-000000000001";
    // A subscriber of its own while it runs: a callsite first hit on a
    // thread with none is cached as uninteresting, and another test's log
    // capture would miss it.
    let (_logs, _guard) = capture_logs();
    let sheets = ScriptedSheets::answering([Some(typed(&["battery staple"], true))]);
    let store = Arc::new(MemoryStore::default());
    let mut stale = SettingSecrets::default();
    stale.insert("psk", "old horse");
    store.put(UUID, SETTING, stale);
    let link = link(
        Arc::clone(&sheets) as Arc<dyn Sheets>,
        Arc::clone(&store) as Arc<dyn SecretStore>,
    )
    .await;
    let ask = |flags: u32| link.get_secrets(wifi_psk_connection(), SETTING, Vec::new(), flags);

    // Connecting on its own: the remembered password is the answer.
    let reply = ask(0).await.await.expect("call task").expect("remembered");
    assert_eq!(string_of(&reply, SETTING, "psk"), "old horse");

    // The network rejected it; nobody can be asked now.
    let error = ask(FLAG_REQUEST_NEW)
        .await
        .await
        .expect("call task")
        .expect_err("no new secrets without asking");
    assert!(
        error.to_string().contains("asking is not allowed"),
        "{error}"
    );

    // The next connection attempt of its own is not answered with it again.
    ask(0)
        .await
        .await
        .expect("call task")
        .expect_err("the rejected password is not answered");
    assert_eq!(
        sheets.cannot_ask.lock_recover().as_slice(),
        ["Home".to_owned(), "Home".to_owned()],
        "the person is told each time it could not join"
    );
    assert!(sheets.asked().is_empty());

    // The person joins and types the new password: remembered and answered.
    let reply = ask(FLAG_ALLOW_INTERACTION | FLAG_REQUEST_NEW)
        .await
        .await
        .expect("call task")
        .expect("typed");
    assert_eq!(string_of(&reply, SETTING, "psk"), "battery staple");
    let reply = ask(0).await.await.expect("call task").expect("remembered");
    assert_eq!(string_of(&reply, SETTING, "psk"), "battery staple");
}
