use std::sync::{Arc, Mutex};

use account_custody::hugging_face::{HUGGING_FACE_DOWNLOADS, HubCredential};
use agent_provider_config::local::HardwareSnapshot;
use agent_provider_config::local::hub::{ApiModelInfo, HubAccount, HubModelOffer, HubTokenState};
use test_http::{Reply, Responder};
use tilo_oauth::fake::{FakeAuthorizationServer, browse};

use crate::panes::model::PaneModel as _;

use super::super::tests::{mock, model_over, rendered_names, snapshot};
use super::super::{AgentModelsEffect, LocalModelPhase, ModelAction, model_actions};

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// A Hugging Face model as the manager offers it on a 32 GiB machine
/// without an accelerator: a quantization that fits and one that does not.
fn hub_offer() -> HubModelOffer {
    let info: ApiModelInfo = serde_json::from_value(serde_json::json!({
        "id": "acme/Chat-GGUF",
        "sha": COMMIT,
        "gated": "auto",
        "cardData": {"license": "apache-2.0"},
        "siblings": [
            {"rfilename": "chat.Q4_K_M.gguf", "size": 4_000_000_000_u64,
             "lfs": {"sha256": "a".repeat(64), "size": 4_000_000_000_u64}},
            {"rfilename": "chat.F32.gguf", "size": 90_000_000_000_u64,
             "lfs": {"sha256": "b".repeat(64), "size": 90_000_000_000_u64}},
        ]
    }))
    .unwrap();
    let machine = HardwareSnapshot {
        cpu_threads: 8,
        memory_mib: 32 * 1024,
        free_storage_mib: 500 * 1024,
        accelerators: Vec::new(),
    };
    HubModelOffer::of(&info, &machine, |_| false, false).unwrap()
}

/// A shown model offers its variants with fit and licence; only a variant
/// that fits, at the commit it was shown at, is downloaded, and the download
/// is itself the acceptance of the licence shown.
#[test]
fn a_hub_model_downloads_only_a_fitting_variant_it_showed() {
    let control = mock(snapshot(None));
    let mut pane = model_over(control.clone());
    pane.hub.offer = Some(Ok(hub_offer()));
    let names = rendered_names(&pane);
    assert!(
        names.iter().any(|name| name == "Open model page"),
        "{names:?}"
    );
    let install = |variant: &str, revision: &str| AgentModelsEffect::InstallHubModel {
        repo: "acme/Chat-GGUF".into(),
        revision: revision.into(),
        variant: variant.into(),
    };
    pane.manager_work(&install("Q4_K_M", COMMIT))
        .unwrap()
        .run(control.as_ref())
        .unwrap();
    assert_eq!(
        control.calls.lock().unwrap().as_slice(),
        [format!("hub-install:acme/Chat-GGUF@{COMMIT}:Q4_K_M")]
    );
    assert!(pane.manager_work(&install("F32", COMMIT)).is_err());
    assert!(
        pane.manager_work(&install("Q4_K_M", &"f".repeat(40)))
            .is_err()
    );
    assert!(pane.hub.downloadable("F32").is_none());

    // Agents reach the same doors: a row per variant, Download only where
    // it fits.
    let rows = pane.hub.rows();
    let actions = |name: &str| {
        rows.iter()
            .find(|row| row.id() == format!("settings.agent-hub-variant:acme/Chat-GGUF:{name}"))
            .map(|row| row.clone().into_resource().actions.len())
    };
    assert_eq!(actions("Q4_K_M"), Some(1));
    assert_eq!(actions("F32"), Some(0));
}

/// Without a signed runtime for its format, an installed model offers no
/// Start and says why.
#[test]
fn a_model_without_a_signed_runtime_is_not_offered_start() {
    let mut stopped = snapshot(Some(LocalModelPhase::Ready {
        running: false,
        endpoint: None,
    }));
    stopped.models[0].package.no_runtime = true;
    let pane = model_over(mock(stopped.clone()));
    assert_eq!(model_actions(&stopped.models[0]), [ModelAction::Remove]);
    assert!(
        pane.manager_work(&AgentModelsEffect::StartModel {
            model: stopped.models[0].package.id.clone(),
        })
        .is_err()
    );
    let names = rendered_names(&pane);
    assert!(!names.iter().any(|name| name == "Start"), "{names:?}");
}

/// A file is added as the person chose it in the file chooser: the model
/// store receives the open file and its name, never a path.
#[test]
fn a_model_file_is_handed_over_as_chosen() {
    let control = mock(snapshot(None));
    let pane = model_over(control.clone());
    pane.manager_work(&AgentModelsEffect::AddModelFile)
        .unwrap()
        .run(control.as_ref())
        .unwrap();
    assert_eq!(
        control.calls.lock().unwrap().as_slice(),
        ["import:Tune.Q4_K_M.gguf"]
    );
}

/// Plays the system browser: records what it opened and follows the
/// sign-in's redirects back to the loopback listener.
struct Browser(Mutex<Vec<String>>);

impl crate::panes::accounts::BrowserOpener for Browser {
    fn open(&self, url: &str) -> ui::portal::LaunchChannel {
        self.0.lock().unwrap().push(url.to_owned());
        let url = url.to_owned();
        std::thread::spawn(move || drop(browse(&url)));
        let (sender, receiver) = std::sync::mpsc::channel();
        let _ = sender.send(ui::portal::LaunchOutcome::Opened);
        receiver
    }
}

/// Signing in with Hugging Face in the browser stores the sign-in where a
/// pasted token goes, and the token group names who signed in and offers
/// Sign out, with pasting a token still there.
#[test]
fn a_hugging_face_sign_in_is_stored_where_a_token_goes_and_shows_who() {
    let server = FakeAuthorizationServer::start();
    let people = Responder::start(vec![
        Reply::status(200)
            .header("Content-Type", "application/json")
            .body(r#"{"sub":"1","preferred_username":"ada","name":"Ada Lovelace"}"#),
    ]);
    let control = mock(snapshot(None));
    let mut pane = model_over(control.clone());
    let browser = Arc::new(Browser(Mutex::new(Vec::new())));
    pane.custodian = account_custody::Custodian::for_root(None).with_use(
        HUGGING_FACE_DOWNLOADS,
        server.provider(),
        "hf-client",
    );
    pane.opener = browser.clone();
    pane.hub_userinfo = format!("{}/oauth/userinfo", people.url());
    let names = rendered_names(&pane);
    assert!(
        names.iter().any(|name| name == "Sign in with Hugging Face"),
        "{names:?}"
    );
    assert!(!names.iter().any(|name| name == "Sign out"), "{names:?}");

    let model = &mut pane;
    let control_dyn: Arc<dyn super::super::LocalModelControl> = control.clone();
    model
        .hub
        .sign_in(
            false,
            &model.custodian,
            &model.opener,
            (&model.hub_userinfo, &control_dyn),
        )
        .unwrap();
    assert!(pane.hub.signing_in());
    let notice = pane.hub.finish_sign_in_for_test().unwrap();
    assert!(notice.contains("Ada Lovelace (ada)"), "{notice}");
    assert!(browser.0.lock().unwrap()[0].starts_with(&format!("{}/authorize?", server.url())));

    let calls = control.calls.lock().unwrap().clone();
    let [stored] = calls.as_slice() else {
        panic!("one store: {calls:?}");
    };
    let stored = stored.strip_prefix("hub-token:").unwrap();
    let HubCredential::SignedIn(sign_in) = HubCredential::parse(stored) else {
        panic!("a sign-in was stored");
    };
    assert_eq!(sign_in.username, "ada");
    assert!(sign_in.tokens.refresh_token.starts_with("refresh-"));
    assert_eq!(
        pane.hub.token_state,
        Ok(HubTokenState::SignedIn {
            account: HubAccount {
                username: "ada".into(),
                name: Some("Ada Lovelace".into()),
            }
        })
    );
    let names = rendered_names(&pane);
    for shown in ["Sign out", "Save token", "Sign in with a code"] {
        assert!(names.iter().any(|name| name == shown), "{shown}: {names:?}");
    }
}

/// A sign-in that ended says so and offers signing in again.
#[test]
fn an_ended_sign_in_asks_to_sign_in_again() {
    let mut pane = model_over(mock(snapshot(None)));
    pane.hub.token_state = Ok(HubTokenState::Ended {
        account: HubAccount {
            username: "ada".into(),
            name: None,
        },
    });
    let text = crate::widgets::announced_text(pane.compose().as_ref());
    assert!(
        text.iter()
            .any(|line| line.contains("sign-in of ada has ended")),
        "{text:?}"
    );
}
