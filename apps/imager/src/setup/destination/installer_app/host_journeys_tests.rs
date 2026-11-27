//! The installer's journeys as the host runs them: every step driven through
//! `imager.destination`'s published actions, as a button, the palette or an
//! agent invokes them, with fakes in place of disks and radios. Each
//! test asserts what the person sees in the window and what the machine was
//! asked to do.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::*;
use crate::backend::destination::network_step::{
    Change, Connectivity, Device, Network, NetworkStep, Readings, Wifi, WifiNetwork,
};
use crate::hosting::testing::TestHost;
use crate::setup::destination::effects::{REVIEW, STORAGE};
use crate::setup::destination::testing::{FakeBackend, KEY};

type App = TestHost;

/// A strong account passphrase, as the installer asks for one.
const ACCOUNT: &str = "correct horse battery staple";
/// A separate disk passphrase, strong and not the account's.
const DISK_SECRET: &str = "waltz badger violet quartz lantern";

enum Armed {
    /// No test here armed a window: the live installer is built as it is.
    None,
    /// The installer the next window build opens.
    Waiting(InstallerApp),
    /// A journey took it. The host builds a window once, so this is only
    /// reached by a test that opens a second one, which a shot's state suits
    /// and which never reads the machine it runs on.
    Spent,
}

thread_local! {
    static ARMED: std::cell::RefCell<Armed> =
        const { std::cell::RefCell::new(Armed::None) };
}

/// Give the next live installer window `app` instead of one probed from the
/// machine this test runs on.
fn arm(app: InstallerApp) {
    ARMED.with(|armed| *armed.borrow_mut() = Armed::Waiting(app));
}

/// The installer a live window opens: the armed one, once.
pub(crate) fn armed_window() -> Option<InstallerApp> {
    ARMED.with(
        |armed| match std::mem::replace(&mut *armed.borrow_mut(), Armed::Spent) {
            Armed::Waiting(app) => Some(app),
            Armed::Spent => Some(InstallerApp::for_shot(Step::Keyboard)),
            Armed::None => None,
        },
    )
}

/// The device a test lends the step instead of this machine's radio.
struct RecordedNetwork {
    state: Mutex<NetworkState>,
}

struct NetworkState {
    network: Network,
    /// Why every change is refused, as a machine without `NetworkManager`.
    refusal: Option<String>,
    keys: HashMap<String, String>,
    attempts: Vec<(String, Option<String>)>,
}

impl RecordedNetwork {
    fn new(connectivity: Connectivity) -> Arc<Self> {
        Self::over(connectivity, true, Vec::new())
    }

    fn offering(connectivity: Connectivity, networks: Vec<WifiNetwork>) -> Arc<Self> {
        Self::over(connectivity, true, networks)
    }

    /// A machine whose `NetworkManager` does not answer, so no Wi-Fi is had.
    fn refusing(why: &str) -> Arc<Self> {
        let recorded = Self::over(Connectivity::Unknown, false, Vec::new());
        recorded.state().refusal = Some(why.to_owned());
        recorded
    }

    /// The machine again: its radio answers, and it is offline.
    fn now_offline(&self) {
        let mut state = self.state();
        state.refusal = None;
        state.network.connectivity = Connectivity::Offline;
    }

    fn portal(uri: &str) -> Arc<Self> {
        let recorded = Self::new(Connectivity::Portal);
        recorded.state().network.portal_uri = Some(uri.to_owned());
        recorded
    }

    fn over(connectivity: Connectivity, wifi: bool, networks: Vec<WifiNetwork>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(NetworkState {
                network: Network {
                    wifi: Wifi {
                        available: wifi,
                        on: wifi,
                        networks,
                        ..Wifi::default()
                    },
                    connectivity,
                    ..Network::default()
                },
                refusal: None,
                keys: HashMap::new(),
                attempts: Vec::new(),
            }),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, NetworkState> {
        self.state.lock().unwrap()
    }

    fn accepting(&self, ssid: &str, key: &str) {
        self.state().keys.insert(ssid.to_owned(), key.to_owned());
    }

    fn attempts(&self) -> Vec<(String, Option<String>)> {
        self.state().attempts.clone()
    }
}

impl Device for RecordedNetwork {
    fn read(&self) -> Readings {
        Readings {
            network: self.state().network.clone(),
            ..Readings::default()
        }
    }

    fn change(&self, change: &Change) -> Result<(), String> {
        let mut state = self.state();
        if let Some(refusal) = &state.refusal {
            return Err(refusal.clone());
        }
        if let Change::Join {
            network, password, ..
        } = change
        {
            state.attempts.push((network.clone(), password.clone()));
            if let Some(wanted) = state.keys.get(network)
                && password.as_deref() != Some(wanted.as_str())
            {
                return Err(
                    "Connection activation failed: the password was not accepted".to_owned(),
                );
            }
            for row in &mut state.network.wifi.networks {
                row.connected = &row.name == network;
            }
            state.network.connectivity = Connectivity::Full;
        }
        Ok(())
    }
}

fn wifi(ssid: &str, secure: bool, signal: u8) -> WifiNetwork {
    WifiNetwork {
        name: ssid.to_owned(),
        signal,
        secure,
        ..WifiNetwork::default()
    }
}

/// The installer window a test drives: `backend` in place of the machine's
/// disks and install helpers, and `network` in place of its radio.
fn installer(backend: Arc<FakeBackend>, network: Arc<RecordedNetwork>) -> InstallerApp {
    let mut app = InstallerApp::with_disks(Vec::new()).with_backend(backend);
    // The network worker and the install helper report through this mailbox;
    // the test's frames take what it holds.
    app.mailbox = Some(Mailbox::new(|| {}));
    app.network = NetworkStep::for_test(network, app.network_wake());
    // No TPM here, so the secrets are passphrases the test can type.
    app.tpm2 = false;
    app.secure_boot = false;
    app.kernel_lockdown = false;
    app.target_verified_boot = false;
    app
}

/// Arm `app` and launch the installer window over it.
fn opened(app: InstallerApp) -> App {
    arm(app);
    TestHost::launched(&["--destination"])
}

/// What the installer shows now: its published state.
fn status(app: &mut App) -> Value {
    app.effect(&DestinationEffect::Status)
        .unwrap()
        .value
        .unwrap()
}

fn step(app: &mut App) -> String {
    status(app)["step"].as_str().unwrap().to_owned()
}

/// Every name and value the window shows to a screen reader.
fn texts(app: &mut App) -> String {
    tilo_ui_base::a11y::texts(app.harness().root_widget()).join("\n")
}

/// Drive frames until the window shows `needle`, as the person would see the
/// worker's answer arrive.
fn wait_for_text(app: &mut App, needle: &str) {
    let seen = app
        .harness()
        .wait_until(tilo_test_env::hang_bound(), |harness| {
            tilo_ui_base::a11y::texts(harness.root_widget())
                .iter()
                .any(|text| text.contains(needle))
        });
    assert!(
        seen,
        "the installer never showed {needle:?}; it showed:\n{}",
        texts(app)
    );
}

/// Drive frames until the window stops showing `needle`: a worker's spinner
/// gone, so its answer is in.
fn wait_until_gone(app: &mut App, needle: &str) {
    let gone = app
        .harness()
        .wait_until(tilo_test_env::hang_bound(), |harness| {
            !tilo_ui_base::a11y::texts(harness.root_widget())
                .iter()
                .any(|text| text.contains(needle))
        });
    assert!(
        gone,
        "the installer still showed {needle:?}; it showed:\n{}",
        texts(app)
    );
}

fn refused_with<T>(result: &Result<T, ui::tilo_actions::ApiError>, code: &str) {
    match result {
        Err(error) => assert_eq!(error.code(), code, "{error}"),
        Ok(_) => panic!("the installer ran it, expected {code}"),
    }
}

/// The installer refused `result`, telling the person `said`.
fn refused_saying<T>(result: &Result<T, ui::tilo_actions::ApiError>, said: &str) {
    match result {
        Err(error) => {
            let words = format!("{error}");
            assert!(
                words.contains(said),
                "refused saying {words:?}, not {said:?}"
            );
        }
        Ok(_) => panic!("the installer ran it, expected a refusal saying {said:?}"),
    }
}

/// The storage step's row for `device`, as it is published.
fn disk_row_of(device: &str) -> String {
    crate::setup::destination::effects::disk_row(device)
}

/// The disk step's first usable device.
fn offered_device(app: &mut App) -> String {
    status(app)["storage"]["disks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|disk| disk["read_only"] == json!(false) && disk["installer_medium"] == json!(false))
        .map(|disk| disk["device"].as_str().unwrap().to_owned())
        .unwrap()
}

/// Walk the steps after storage as the person does: name the machine and its
/// account, then acknowledge the recovery key.
fn finish_walk(app: &mut App, account: &str, disk_secret: &str) {
    assert_eq!(step(app), "account");
    app.effect(&DestinationEffect::SetIdentity {
        username: Some("ada".into()),
        full_name: Some("Ada Lovelace".into()),
    })
    .unwrap();
    app.effect(&DestinationEffect::SetPassphrase {
        passphrase: account.into(),
        confirmation: account.into(),
    })
    .unwrap();
    if !disk_secret.is_empty() {
        app.effect(&DestinationEffect::SetDiskPassphrase {
            passphrase: disk_secret.into(),
            confirmation: disk_secret.into(),
            mode: Some("passphrase".into()),
        })
        .unwrap();
    }
    app.effect(&DestinationEffect::Next).unwrap();
    assert_eq!(step(app), "options");
    app.effect(&DestinationEffect::SetHostname {
        hostname: "lab-1".into(),
    })
    .unwrap();
    app.effect(&DestinationEffect::Next).unwrap();
    assert_eq!(step(app), "media");
    app.effect(&DestinationEffect::Next).unwrap();
    assert_eq!(step(app), "review");
    app.effect(&DestinationEffect::SetRecoveryPromise { acknowledged: true })
        .unwrap();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one person's install, read as the steps they take"
)]
fn an_offline_erase_install_ends_on_its_recovery_key_and_power() {
    let (services, recorder) = ui::process::ProcessServices::recording();
    let _host = ui::process::enter(&services);
    let backend = FakeBackend::new();
    let network = RecordedNetwork::refusing("NetworkManager is not running");
    let mut app = opened(installer(backend.clone(), network.clone()));

    assert_eq!(step(&mut app), "keyboard");
    // A step's own effects are refused before the installer reaches it.
    refused_saying(
        &app.effect(&DestinationEffect::SetHostname {
            hostname: "lab-1".into(),
        }),
        "Options",
    );
    assert!(
        app.effect(&DestinationEffect::Begin {
            target: String::new(),
            allocation: String::new(),
        })
        .is_err(),
        "begin is never offered unbound, where its target could be typed"
    );
    app.effect(&DestinationEffect::SetLayout {
        layout: "German".into(),
    })
    .unwrap();
    assert_eq!(status(&mut app)["keyboard"]["xkb_layout"], "de");

    app.effect(&DestinationEffect::Next).unwrap();
    assert_eq!(step(&mut app), "network");
    // No `NetworkManager` here: the step says why there is no Wi-Fi, and the
    // install goes on without one.
    wait_for_text(&mut app, "NetworkManager is not running");
    wait_for_text(&mut app, "You can still install Tilo offline");
    assert_eq!(status(&mut app)["network"]["networks"], json!([]));
    // The machine comes back, offline: the radio answers, nothing is in reach.
    network.now_offline();
    app.effect(&DestinationEffect::RefreshNetworks).unwrap();
    wait_for_text(&mut app, "install completely offline");
    app.effect(&DestinationEffect::RefreshNetworks).unwrap();
    wait_until_gone(
        &mut app,
        &crate::setup::strings::t("imager-installer-checking-networks"),
    );
    assert_eq!(status(&mut app)["network"]["busy"], json!(false));

    app.effect(&DestinationEffect::Next).unwrap();
    assert_eq!(step(&mut app), "disk");

    let device = offered_device(&mut app);
    app.effect_on(
        &disk_row_of(&device),
        &DestinationEffect::SelectDisk {
            device: device.clone(),
        },
    )
    .unwrap();
    // The disk's own partitions make alongside the disk step's first choice;
    // the person asks for a clean erase instead.
    app.effect(&DestinationEffect::SetMethod {
        method: "erase".into(),
    })
    .unwrap();
    assert_eq!(status(&mut app)["storage"]["method"], "erase");
    assert_eq!(status(&mut app)["storage"]["selected"], device);
    assert_eq!(status(&mut app)["storage"]["confirmation_target"], device);
    app.effect(&DestinationEffect::Next).unwrap();
    assert_eq!(step(&mut app), "account");

    refused_with(
        &app.effect(&DestinationEffect::SetIdentity {
            username: Some("Root".into()),
            full_name: None,
        }),
        "action_failed",
    );
    app.effect(&DestinationEffect::SetIdentity {
        username: Some("ada".into()),
        full_name: Some("Ada Lovelace".into()),
    })
    .unwrap();
    // A mistyped confirmation is refused; the step stays until it matches.
    refused_with(
        &app.effect(&DestinationEffect::SetPassphrase {
            passphrase: ACCOUNT.into(),
            confirmation: "correct horse battery stapleX".into(),
        }),
        "action_failed",
    );
    app.effect(&DestinationEffect::SetPassphrase {
        passphrase: ACCOUNT.into(),
        confirmation: ACCOUNT.into(),
    })
    .unwrap();
    // A separate disk secret: too weak, then mismatched, then accepted.
    refused_with(
        &app.effect(&DestinationEffect::SetDiskPassphrase {
            passphrase: "short".into(),
            confirmation: "short".into(),
            mode: Some("passphrase".into()),
        }),
        "action_failed",
    );
    refused_with(
        &app.effect(&DestinationEffect::SetDiskPassphrase {
            passphrase: DISK_SECRET.into(),
            confirmation: "another disk passphrase".into(),
            mode: None,
        }),
        "action_failed",
    );
    app.effect(&DestinationEffect::SetDiskPassphrase {
        passphrase: DISK_SECRET.into(),
        confirmation: DISK_SECRET.into(),
        mode: Some("passphrase".into()),
    })
    .unwrap();

    app.effect(&DestinationEffect::Next).unwrap();
    assert_eq!(step(&mut app), "options");
    let account = status(&mut app);
    assert_eq!(
        (
            account["account"]["username"].clone(),
            account["account"]["secret_policy"].clone()
        ),
        (json!("ada"), json!("passphrase"))
    );
    app.effect(&DestinationEffect::SetLocale {
        locale: "de_DE.UTF-8".into(),
    })
    .unwrap();
    app.effect(&DestinationEffect::SetTimezone {
        timezone: "Europe/Berlin".into(),
    })
    .unwrap();
    app.effect(&DestinationEffect::SetHostname {
        hostname: "lab-1".into(),
    })
    .unwrap();
    let options = status(&mut app);
    assert_eq!(options["options"]["locale"], "de_DE.UTF-8");
    assert_eq!(options["options"]["timezone"], "Europe/Berlin");
    assert_eq!(options["options"]["hostname"], "lab-1");
    refused_with(
        &app.effect(&DestinationEffect::SetTimezone {
            timezone: "Mars/Olympus".into(),
        }),
        "action_failed",
    );

    app.effect(&DestinationEffect::Next).unwrap();
    assert_eq!(step(&mut app), "media");
    app.effect(&DestinationEffect::SetMediaCodecs { enabled: true })
        .unwrap();
    assert_eq!(status(&mut app)["media"]["codecs"], json!(true));
    app.effect(&DestinationEffect::Next).unwrap();
    assert_eq!(step(&mut app), "review");

    // The review's Begin is refused until the person promises the key, even
    // for a caller that has confirmed it.
    match app.call_confirmed_on(REVIEW, "begin", json!({})) {
        Err(ui::tilo_actions::ApiError::Unavailable { code, .. }) => {
            assert_eq!(code, "recovery_key_promise");
        }
        other => panic!("the review began without the promise: {other:?}"),
    }
    assert!(backend.installs.lock().unwrap().is_empty());
    app.effect(&DestinationEffect::SetRecoveryPromise { acknowledged: true })
        .unwrap();
    assert_eq!(status(&mut app)["review"]["recovery_promise"], json!(true));
    // Destructive: a caller that has not gone through the host's confirmation
    // is refused, whatever it claims.
    refused_with(
        &app.call_on(REVIEW, "begin", json!({ "confirmation_phrase": device })),
        "action_confirmation_required",
    );
    assert!(
        backend.installs.lock().unwrap().is_empty(),
        "nothing was installed before the person confirmed"
    );

    app.call_confirmed_on(REVIEW, "begin", json!({})).unwrap();
    wait_for_text(&mut app, &crate::setup::strings::t("installer-installed"));
    let finished = status(&mut app);
    assert_eq!(finished["install"]["state"], "done");
    assert_eq!(finished["recovery_key"]["available"], json!(true));
    assert_eq!(finished["recovery_key"]["saved"], json!(false));

    let installs = backend.installs.lock().unwrap().clone();
    assert_eq!(installs.len(), 1);
    let (disk, user, password, opts) = &installs[0];
    assert_eq!(
        (disk.as_str(), user.as_str(), password.as_str()),
        (device.as_str(), "ada", ACCOUNT)
    );
    assert_eq!(opts.hostname, "lab-1");
    assert_eq!(opts.keymap, "de");
    assert_eq!(opts.locale, "de_DE.UTF-8");
    assert_eq!(opts.timezone, "Europe/Berlin");
    assert_eq!(opts.filesystem, "btrfs");
    assert_eq!(opts.purpose, "computer");
    assert_eq!(
        opts.pin.as_str(),
        DISK_SECRET,
        "the separate disk secret unlocks the disk"
    );
    assert_eq!(opts.full_name, "Ada Lovelace");

    // Restarting waits for the key the person saved.
    refused_saying(
        &app.effect(&DestinationEffect::Reboot),
        "Save the recovery key",
    );
    assert!(backend.power.lock().unwrap().is_empty());
    app.effect(&DestinationEffect::CopyRecoveryKey).unwrap();
    assert_eq!(tilo_ui_base::clipboard_paste().as_deref(), Some(KEY));
    assert_eq!(
        recorder.secret_copies(),
        [KEY],
        "the key leaves through a secret copy, not the ordinary clipboard"
    );
    app.effect(&DestinationEffect::ConfirmRecoverySaved { saved: true })
        .unwrap();
    assert_eq!(status(&mut app)["recovery_key"]["saved"], json!(true));
    app.effect(&DestinationEffect::PowerOff).unwrap();
    assert_eq!(*backend.power.lock().unwrap(), [PowerAction::PowerOff]);
}

#[test]
fn a_captive_portal_says_where_to_sign_in() {
    let backend = FakeBackend::new();
    let network = RecordedNetwork::portal("http://nm-check.example/");
    let mut app = opened(installer(backend, network));

    app.effect(&DestinationEffect::SetLayout {
        layout: "English (US)".into(),
    })
    .unwrap();
    app.effect(&DestinationEffect::Next).unwrap();
    assert_eq!(step(&mut app), "network");
    wait_for_text(&mut app, "http://nm-check.example/");
    wait_for_text(&mut app, "sign-in page");
    assert!(
        status(&mut app)["network"]["summary"]
            .as_str()
            .unwrap()
            .contains("Sign-in required")
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one person's install, read as the steps they take"
)]
fn an_online_install_joins_wifi() {
    let backend = FakeBackend::new();
    let network = RecordedNetwork::offering(
        Connectivity::Full,
        vec![wifi("Cafe", true, 4), wifi("Library", false, 2)],
    );
    network.accepting("Cafe", "darjeeling-and-biscuits");
    let mut app = opened(installer(backend.clone(), network.clone()));

    app.effect(&DestinationEffect::SetLayout {
        layout: "English (US)".into(),
    })
    .unwrap();
    app.effect(&DestinationEffect::Next).unwrap();
    assert_eq!(step(&mut app), "network");
    wait_for_text(&mut app, "Internet access is available");
    assert_eq!(status(&mut app)["network"]["networks"][0]["ssid"], "Cafe");

    app.effect(&DestinationEffect::RefreshNetworks).unwrap();
    wait_until_gone(
        &mut app,
        &crate::setup::strings::t("imager-installer-checking-networks"),
    );
    let listed = status(&mut app);
    assert_eq!(listed["network"]["networks"].as_array().unwrap().len(), 2);

    app.effect_on(
        "installer.network:Cafe",
        &DestinationEffect::SelectNetwork {
            ssid: "Cafe".into(),
        },
    )
    .unwrap();
    assert_eq!(status(&mut app)["network"]["selected"], "Cafe");

    // A wrong password is reported to the person and leaves them on the step.
    app.effect(&DestinationEffect::ConnectNetwork {
        password: Some("oolong".into()),
    })
    .unwrap();
    wait_for_text(&mut app, "Could not connect to Cafe");
    assert_eq!(step(&mut app), "network");
    assert_eq!(status(&mut app)["network"]["selected"], "Cafe");

    app.effect(&DestinationEffect::ConnectNetwork {
        password: Some("darjeeling-and-biscuits".into()),
    })
    .unwrap();
    wait_for_text(&mut app, "Connected to Cafe");
    assert_eq!(
        network.attempts(),
        [
            ("Cafe".to_owned(), Some("oolong".to_owned())),
            (
                "Cafe".to_owned(),
                Some("darjeeling-and-biscuits".to_owned())
            ),
        ]
    );
    let joined = status(&mut app);
    assert_eq!(joined["network"]["networks"][0]["connected"], json!(true));

    app.effect(&DestinationEffect::Next).unwrap();
    assert_eq!(step(&mut app), "disk");
    let device = offered_device(&mut app);
    app.effect_on(
        &disk_row_of(&device),
        &DestinationEffect::SelectDisk {
            device: device.clone(),
        },
    )
    .unwrap();
    app.effect(&DestinationEffect::SetMethod {
        method: "erase".into(),
    })
    .unwrap();
    app.effect(&DestinationEffect::Next).unwrap();
    finish_walk(&mut app, ACCOUNT, "");
    app.call_confirmed_on(REVIEW, "begin", json!({})).unwrap();
    wait_for_text(&mut app, &crate::setup::strings::t("installer-installed"));

    let installs = backend.installs.lock().unwrap().clone();
    assert_eq!(installs.len(), 1);
    let (disk, user, password, opts) = &installs[0];
    assert_eq!(
        (disk.as_str(), user.as_str(), password.as_str()),
        (device.as_str(), "ada", ACCOUNT)
    );
    assert!(
        opts.network_static.is_empty(),
        "a Wi-Fi join leaves the image's own network alone"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one person's install, read as the steps they take"
)]
fn an_install_beside_an_existing_system_takes_the_reviewed_free_space() {
    let disk = super::tests::disk_with_two_guided_extents();
    let device = disk.name.clone();
    let backend = FakeBackend::with_disks(vec![disk]);
    let network = RecordedNetwork::new(Connectivity::Offline);
    let mut app = opened(installer(backend.clone(), network));

    app.effect(&DestinationEffect::SetLayout {
        layout: "English (US)".into(),
    })
    .unwrap();
    app.effect(&DestinationEffect::Next).unwrap();
    app.effect(&DestinationEffect::Next).unwrap();
    assert_eq!(step(&mut app), "disk");

    app.effect(&DestinationEffect::SetMethod {
        method: "alongside".into(),
    })
    .unwrap();
    app.effect_on(
        &disk_row_of(&device),
        &DestinationEffect::SelectDisk {
            device: device.clone(),
        },
    )
    .unwrap();
    assert_eq!(status(&mut app)["storage"]["method"], "alongside");
    // Two free extents: the person chooses the one to install into.
    app.effect_on(
        &crate::setup::destination::effects::region_row(0),
        &DestinationEffect::SelectRegion { index: 0 },
    )
    .unwrap();
    assert_eq!(status(&mut app)["storage"]["selected_region"], json!(0));
    app.call_on(STORAGE, "set-size", json!({ "gib": 40 }))
        .unwrap();
    refused_with(
        &app.call_on(STORAGE, "set-size", json!({ "gib": 20 })),
        "action_failed",
    );

    // Leaving storage names the machine and its account; the reviewed extent
    // is the one the backend will be given.
    app.effect(&DestinationEffect::Next).unwrap();
    wait_for_text(
        &mut app,
        &crate::setup::strings::t("installer-account-create"),
    );
    finish_walk(&mut app, ACCOUNT, "");
    assert_eq!(
        status(&mut app)["storage"]["confirmation_target"],
        device,
        "a guided install confirms the disk it appends to"
    );
    refused_with(
        &app.call_on(REVIEW, "begin", json!({})),
        "action_confirmation_required",
    );
    app.call_confirmed_on(REVIEW, "begin", json!({})).unwrap();
    wait_for_text(&mut app, &crate::setup::strings::t("installer-installed"));

    let installs = backend.installs.lock().unwrap().clone();
    assert_eq!(installs.len(), 1);
    let (disk, _, _, opts) = &installs[0];
    assert_eq!(disk, &device);
    assert_eq!(opts.alongside_root, "", "nothing existing is reformatted");
    // The capacity chosen above is 40 GiB: the ESP and boot take 3 GiB and
    // the root takes the rest.
    assert_eq!(opts.guided_free_size_bytes, 40 << 30);
    assert_eq!(opts.guided_root_size_bytes, 37 << 30);
    assert!(opts.guided_free_start_bytes > 0);
    assert!(
        opts.expected_partitions.len() == 1,
        "the reviewed disk's existing partition stays"
    );
}
