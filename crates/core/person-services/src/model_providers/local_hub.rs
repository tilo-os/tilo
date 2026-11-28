//! Hugging Face searches and downloads for a workspace's AI page, asked of this computer's
//! model manager.
//!
//! The manager is a device helper and keeps no credential: the Hugging Face sign-in is the key
//! of this computer's local model provider in the workspace's `tilo.models`, kept in the
//! workspace's vault. Each request carries that key, and only that request: nothing writes it
//! down, and no answer carries it back.

use std::path::Path;

use agent_provider_config::local::snapshot::{
    AcceleratorSetupState, HardwareFit, InProcessLocalModel, InstalledLocalModel,
    LocalModelPackage, LocalModelPhase, LocalModelSnapshot,
};
use agent_provider_config::{EndpointLocation, ProviderConfig, ProviderProfile};
#[cfg(unix)]
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tilo_workspace_records::models::{
    LocalHardware, LocalModel, LocalModels, LocalPhase, ModelsAction,
};

use crate::person_settings::{ProviderKeys, provider_config};

/// Every request and answer the manager's service speaks.
const RPC_VERSION: u16 = 1;
/// The manager's framing: a big-endian `u32` length, then JSON.
#[cfg(unix)]
const RPC_FRAMING: wire_frame::Framing = wire_frame::Framing::big_endian(1024 * 1024);

/// The request's body for the Hugging Face action `wanted` (a search, a model's description
/// or a download), with the workspace's sign-in when it keeps one.
fn body(wanted: ModelsAction, token: Option<&str>) -> Value {
    let mut action = Map::new();
    match wanted {
        ModelsAction::SearchHubModels { query, format } => {
            action.insert("verb".into(), "hub_search".into());
            action.insert("query".into(), query.into());
            action.insert(
                "format".into(),
                format.unwrap_or_else(|| "gguf".into()).into(),
            );
        }
        ModelsAction::ShowHubModel { model } => {
            action.insert("verb".into(), "hub_model".into());
            action.insert("reference".into(), model.into());
        }
        ModelsAction::InstallHubModel {
            repo,
            revision,
            variant,
            accept_license,
        } => {
            action.insert("verb".into(), "hub_install".into());
            action.insert("repo".into(), repo.into());
            action.insert("revision".into(), revision.into());
            action.insert("variant".into(), variant.into());
            action.insert(
                "accept_license".into(),
                accept_license.unwrap_or_default().into(),
            );
        }
        // The providers send the manager only these three.
        _ => {}
    }
    if let Some(token) = token {
        action.insert("token".into(), token.into());
    }
    json!({ "version": RPC_VERSION, "action": action })
}

/// The manager's answer.
#[cfg(unix)]
#[derive(Deserialize)]
struct Answer {
    ok: bool,
    #[serde(default)]
    output: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// The request's body for a local model action the manager owns: what it has installed and how
/// far a setup has got, and the changes to that ([`LocalModels`] and the actions beside it).
fn local_body(wanted: &ModelsAction) -> Value {
    let mut action = Map::new();
    match wanted {
        ModelsAction::LocalModels => {
            action.insert("verb".into(), "snapshot".into());
        }
        ModelsAction::InstallLocalModel {
            model,
            revision,
            accept_license,
        } => {
            action.insert("verb".into(), "install".into());
            action.insert("model".into(), model.clone().into());
            action.insert("revision".into(), revision.clone().into());
            action.insert(
                "accept_license".into(),
                accept_license.unwrap_or_default().into(),
            );
        }
        ModelsAction::StartLocalModel { model } => {
            action.insert("verb".into(), "start".into());
            action.insert("model".into(), model.clone().into());
        }
        ModelsAction::StopLocalModel { model } => {
            action.insert("verb".into(), "stop".into());
            action.insert("model".into(), model.clone().into());
        }
        ModelsAction::RemoveLocalModel { model } => {
            action.insert("verb".into(), "remove".into());
            action.insert("model".into(), model.clone().into());
        }
        ModelsAction::StageAccelerator => {
            action.insert("verb".into(), "stage_accelerator".into());
        }
        // The providers send the manager only these.
        _ => {}
    }
    json!({ "version": RPC_VERSION, "action": action })
}

/// What this computer's model manager has installed and offers, as a page reads it. A computer
/// with no manager running answers that, rather than failing the page.
pub(super) async fn models() -> LocalModels {
    models_at(&tilo_paths::xdg::service_socket("models")).await
}

/// [`models`], asked of the manager at `socket`.
pub(super) async fn models_at(socket: &Path) -> LocalModels {
    let asked = local_body(&ModelsAction::LocalModels);
    match send_at(socket, &asked).await {
        Ok(answer) => read_snapshot(answer),
        Err(problem) => LocalModels {
            available: false,
            problem: Some(problem),
            ..LocalModels::default()
        },
    }
}

/// Make one of the manager's changes to this computer's models: install one, start it, stop it,
/// remove it, or stage the accelerator pack it needs.
pub(super) async fn run(asked: &ModelsAction) -> Result<Value, String> {
    run_at(&tilo_paths::xdg::service_socket("models"), asked).await
}

/// [`run`], asked of the manager at `socket`.
pub(super) async fn run_at(socket: &Path, asked: &ModelsAction) -> Result<Value, String> {
    send_at(socket, &local_body(asked)).await
}

/// What the manager's snapshot says, as a page reads it.
fn read_snapshot(answer: Value) -> LocalModels {
    match serde_json::from_value::<LocalModelSnapshot>(answer) {
        Ok(snapshot) => LocalModels {
            available: true,
            problem: None,
            hardware: LocalHardware {
                fit: fit_name(snapshot.hardware.fit).to_owned(),
                memory_gib: i64::from(snapshot.hardware.memory_gib),
                accelerator: snapshot.hardware.accelerator,
                accelerator_memory_gib: snapshot
                    .hardware
                    .accelerator_memory_gib
                    .map_or(0, i64::from),
                free_storage_gib: snapshot.hardware.free_storage_gib.map_or(0, i64::from),
                reason: snapshot.hardware.reason,
            },
            recommendation: snapshot
                .recommendation
                .as_ref()
                .map(|package| model_of(package, None)),
            models: snapshot
                .models
                .iter()
                .map(|InstalledLocalModel { package, phase }| model_of(package, Some(phase)))
                .collect(),
            in_process: snapshot
                .in_process
                .iter()
                .map(|InProcessLocalModel { package, phase }| model_of(package, phase.as_ref()))
                .collect(),
            accelerator: snapshot
                .accelerator_setup
                .as_ref()
                .map_or_else(String::new, |setup| {
                    accelerator_name(&setup.status).to_owned()
                }),
        },
        Err(error) => LocalModels {
            available: false,
            problem: Some(format!(
                "this computer's model service answered something that does not read: {error}"
            )),
            ..LocalModels::default()
        },
    }
}

/// One model as a page reads it.
fn model_of(package: &LocalModelPackage, phase: Option<&LocalModelPhase>) -> LocalModel {
    LocalModel {
        id: package.id.clone(),
        name: package.name.clone(),
        description: package.description.clone(),
        revision: package.revision.clone(),
        download_bytes: i64::try_from(package.download_bytes).unwrap_or(i64::MAX),
        installed_bytes: i64::try_from(package.installed_bytes).unwrap_or(i64::MAX),
        vision: package.vision,
        in_process: package.in_process,
        segmentation: package.segmentation,
        license: package.license.clone(),
        license_url: package.license_url.clone(),
        phase: phase.map_or_else(LocalPhase::default, phase_of),
    }
}

/// Where setting a model up has got, as a page reads it.
fn phase_of(phase: &LocalModelPhase) -> LocalPhase {
    let mut shown = LocalPhase::default();
    match phase {
        LocalModelPhase::Downloading {
            downloaded_bytes,
            total_bytes,
        } => {
            shown.state = "downloading".into();
            shown.downloaded_bytes = i64::try_from(*downloaded_bytes).unwrap_or(i64::MAX);
            shown.total_bytes = i64::try_from(*total_bytes).unwrap_or(i64::MAX);
            shown.percent = downloaded_bytes
                .saturating_mul(100)
                .checked_div(*total_bytes)
                .and_then(|percent| i64::try_from(percent).ok())
                .unwrap_or(0);
        }
        LocalModelPhase::Installing { percent, message } => {
            shown.state = "installing".into();
            shown.percent = i64::from(percent.unwrap_or_default());
            shown.message.clone_from(message);
        }
        LocalModelPhase::Ready { running, endpoint } => {
            shown.state = "ready".into();
            shown.running = *running;
            shown.endpoint = endpoint
                .as_ref()
                .map_or_else(String::new, |end| end.url.clone());
        }
        LocalModelPhase::Error { message } => {
            shown.state = "error".into();
            shown.message.clone_from(message);
        }
    }
    shown
}

fn fit_name(fit: HardwareFit) -> &'static str {
    match fit {
        HardwareFit::Excellent => "excellent",
        HardwareFit::Good => "good",
        HardwareFit::Tight => "tight",
        HardwareFit::Unsupported => "unsupported",
    }
}

fn accelerator_name(state: &AcceleratorSetupState) -> &'static str {
    match state {
        AcceleratorSetupState::Available => "available",
        AcceleratorSetupState::Queued => "queued",
        AcceleratorSetupState::Staging => "staging",
        AcceleratorSetupState::RestartRequired => "restart-required",
        AcceleratorSetupState::Active => "active",
        AcceleratorSetupState::Error { .. } => "error",
    }
}

/// Send `wanted` to this login's model manager with the workspace's local model provider key.
pub(super) async fn ask(
    (keys, root): (ProviderKeys<'_>, &Path),
    wanted: ModelsAction,
) -> Result<Value, String> {
    ask_at(
        (keys, root),
        &tilo_paths::xdg::service_socket("models"),
        wanted,
    )
    .await
}

/// Send `wanted` to the manager at `socket`, signed in with the workspace's key when it keeps
/// one.
pub(super) async fn ask_at(
    (keys, root): (ProviderKeys<'_>, &Path),
    socket: &Path,
    wanted: ModelsAction,
) -> Result<Value, String> {
    let token = key_of(keys, root).await?;
    let body = body(wanted, token.as_deref().map(String::as_str));
    send_at(socket, &body).await
}

/// The key of this computer's local model provider in the workspace, when it keeps one: the
/// Hugging Face sign-in its downloads use.
async fn key_of(
    keys: ProviderKeys<'_>,
    root: &Path,
) -> Result<Option<zeroize::Zeroizing<String>>, String> {
    let Some(profile) = local_profile(&provider_config(root)) else {
        return Ok(None);
    };
    keys.get(&profile).await
}

/// The model provider this computer runs, as the manager's terminal sign-in names it.
fn local_profile(config: &ProviderConfig) -> Option<ProviderProfile> {
    config
        .profiles
        .iter()
        .find(|profile| profile.location == EndpointLocation::ThisDevice)
        .cloned()
}

/// Send `body` to the manager listening at `socket` and answer what it said, for tests that
/// hand their own socket.
async fn send_at(socket: &Path, body: &Value) -> Result<Value, String> {
    let socket = socket.to_owned();
    let body = serde_json::to_vec(body).map_err(|error| error.to_string())?;
    let answered = tokio::task::spawn_blocking(move || exchange(&socket, &body))
        .await
        .map_err(|_| "the model service stopped before it answered".to_owned())??;
    match answered {
        Some(output) if !output.trim().is_empty() => {
            serde_json::from_str(&output).map_err(|error| {
                format!("the model service answered something that does not read: {error}")
            })
        }
        _ => Ok(Value::Null),
    }
}

/// One request and its answer over the manager's service socket, framed as a `u32` length
/// then JSON, as the manager reads it.
#[cfg(unix)]
fn exchange(socket: &Path, body: &[u8]) -> Result<Option<String>, String> {
    /// How long the manager may take to answer: a search reads Hugging Face first.
    const ANSWER_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

    let mut stream = tilo_files::native::socket::connect(socket).map_err(|error| {
        format!(
            "this computer's model service did not answer ({}): {error}",
            socket.display()
        )
    })?;
    stream
        .set_read_timeout(Some(ANSWER_WAIT))
        .and_then(|()| stream.set_write_timeout(Some(ANSWER_WAIT)))
        .map_err(|error| format!("this computer's model service did not answer: {error}"))?;
    RPC_FRAMING
        .write(&mut stream, body)
        .map_err(|error| format!("this computer's model service did not answer: {error}"))?;
    let Some(answered) = RPC_FRAMING.read(&mut stream).map_err(|error| {
        format!("the model service answered something that does not read: {error}")
    })?
    else {
        return Err("the model service closed before it answered".to_owned());
    };
    let answer: Answer = serde_json::from_slice(&answered).map_err(|error| {
        format!("the model service answered something that does not read: {error}")
    })?;
    if answer.ok {
        Ok(answer.output)
    } else {
        Err(answer
            .error
            .unwrap_or_else(|| "the model service refused the request".to_owned()))
    }
}

/// The manager's service socket is a Unix one: a host without it runs no model manager.
#[cfg(not(unix))]
fn exchange(_socket: &Path, _body: &[u8]) -> Result<Option<String>, String> {
    Err("this computer runs no model service".to_owned())
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]
    #![allow(
        clippy::significant_drop_tightening,
        reason = "the test holds what it drove until it has asserted on it"
    )]

    use std::io::{Read as _, Write as _};
    use std::os::unix::net::UnixListener;

    use agent_provider_config::local::snapshot::{
        AcceleratorSetup, HardwareFit, HardwareRecommendation,
    };

    use super::*;

    /// A model package, as the signed catalogue names one.
    fn package(id: &str, in_process: bool) -> LocalModelPackage {
        LocalModelPackage {
            id: id.into(),
            name: id.to_owned(),
            description: "A model".into(),
            source: "huggingface".into(),
            revision: "rev".into(),
            license: "Apache-2.0".into(),
            license_url: "https://example.org/licence".into(),
            download_bytes: 2048,
            installed_bytes: 4096,
            vision: in_process,
            face_recognition: false,
            in_process,
            segmentation: in_process,
            no_runtime: false,
        }
    }

    /// What the manager answers reads as the page sees it: the machine, the recommendation,
    /// a download's progress, a model not set up yet, and the accelerator.
    #[test]
    fn a_snapshot_reads_as_the_page_sees_it() {
        let snapshot = LocalModelSnapshot {
            hardware: HardwareRecommendation {
                fit: HardwareFit::Good,
                memory_gib: 32,
                accelerator: "GPU".into(),
                accelerator_memory_gib: Some(8),
                free_storage_gib: Some(120),
                reason: "A good fit".into(),
            },
            recommendation: Some(package("recommended", false)),
            models: vec![InstalledLocalModel {
                package: package("chat", false),
                phase: LocalModelPhase::Downloading {
                    downloaded_bytes: 512,
                    total_bytes: 2048,
                },
            }],
            in_process: vec![InProcessLocalModel {
                package: package("eyes", true),
                phase: None,
            }],
            accelerator_setup: Some(AcceleratorSetup {
                boot_pack_id: "pack".into(),
                status: AcceleratorSetupState::Available,
            }),
        };
        let shown = read_snapshot(serde_json::to_value(&snapshot).unwrap());
        assert!(shown.available, "{shown:?}");
        assert!(shown.problem.is_none(), "{shown:?}");
        assert_eq!(shown.hardware.fit, "good");
        assert_eq!(shown.hardware.accelerator_memory_gib, 8);
        assert_eq!(shown.hardware.free_storage_gib, 120);
        assert_eq!(shown.recommendation.unwrap().id, "recommended");
        assert_eq!(shown.models[0].id, "chat");
        assert_eq!(shown.models[0].phase.state, "downloading");
        assert_eq!(shown.models[0].phase.percent, 25);
        assert_eq!(shown.models[0].phase.downloaded_bytes, 512);
        assert_eq!(shown.in_process[0].id, "eyes");
        assert_eq!(shown.in_process[0].phase.state, "");
        assert_eq!(shown.accelerator, "available");
    }

    /// A computer running no manager says so, instead of failing the page's read.
    #[tokio::test]
    async fn a_computer_without_a_manager_says_so() {
        let root = tempfile::tempdir().unwrap();
        let shown = models_at(&root.path().join("no-manager.sock")).await;
        assert!(!shown.available, "{shown:?}");
        assert!(
            shown.problem.unwrap().contains("did not answer"),
            "the reason names the manager"
        );
    }

    /// Each change reaches the manager as the verb it is, with its arguments.
    #[tokio::test]
    async fn a_change_reaches_the_manager_as_its_verb() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("models.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let asked: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>> =
            std::sync::Arc::default();
        let seen = std::sync::Arc::clone(&asked);
        let serving = std::thread::spawn(move || {
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut size = [0_u8; 4];
                stream.read_exact(&mut size).unwrap();
                let mut body = vec![0_u8; usize::try_from(u32::from_be_bytes(size)).unwrap()];
                stream.read_exact(&mut body).unwrap();
                seen.lock()
                    .unwrap()
                    .push(serde_json::from_slice(&body).unwrap());
                let answer = br#"{"ok":true,"output":null}"#;
                stream
                    .write_all(&u32::try_from(answer.len()).unwrap().to_be_bytes())
                    .and_then(|()| stream.write_all(answer))
                    .unwrap();
            }
        });
        run_at(
            &socket,
            &ModelsAction::InstallLocalModel {
                model: "chat".into(),
                revision: "rev".into(),
                accept_license: Some(true),
            },
        )
        .await
        .unwrap();
        run_at(
            &socket,
            &ModelsAction::StartLocalModel {
                model: "chat".into(),
            },
        )
        .await
        .unwrap();
        run_at(&socket, &ModelsAction::StageAccelerator)
            .await
            .unwrap();
        serving.join().unwrap();
        let asked = asked.lock().unwrap();
        assert_eq!(asked[0]["action"]["verb"], "install");
        assert_eq!(asked[0]["action"]["model"], "chat");
        assert_eq!(asked[0]["action"]["accept_license"], true);
        assert_eq!(asked[1]["action"]["verb"], "start");
        assert_eq!(asked[2]["action"]["verb"], "stage_accelerator");
    }
}
