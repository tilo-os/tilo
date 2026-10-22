use agent_provider_config::local::HardwareSnapshot;
use agent_provider_config::local::hub::{ApiModelInfo, HubModelOffer};

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

/// A file is added by its absolute path to a GGUF file, nothing else.
#[test]
fn a_model_file_is_an_absolute_gguf_path() {
    let control = mock(snapshot(None));
    let pane = model_over(control.clone());
    for refused in ["", "model.gguf", "/home/me/model.safetensors"] {
        assert!(
            pane.manager_work(&AgentModelsEffect::AddModelFile {
                path: refused.into()
            })
            .is_err(),
            "{refused}"
        );
    }
    pane.manager_work(&AgentModelsEffect::AddModelFile {
        path: " /home/me/Tune.Q4_K_M.gguf ".into(),
    })
    .unwrap()
    .run(control.as_ref())
    .unwrap();
    assert_eq!(
        control.calls.lock().unwrap().as_slice(),
        ["import:/home/me/Tune.Q4_K_M.gguf"]
    );
}
