use std::fs;
use std::sync::Mutex;

use super::*;

fn package() -> LocalModelPackage {
    LocalModelPackage {
        id: "test/model".into(),
        name: "Helpful 7B".into(),
        description: "Test package".into(),
        source: "huggingface.co/test/model".into(),
        revision: "0123456789abcdef".into(),
        license: "Apache 2.0".into(),
        license_url: "https://example.test/license".into(),
        download_bytes: 5_368_709_120,
        installed_bytes: 10_737_418_240,
        vision: false,
        face_recognition: false,
        in_process: false,
        segmentation: false,
        no_runtime: false,
    }
}

fn subject_package() -> LocalModelPackage {
    LocalModelPackage {
        id: "u2net-subject".into(),
        name: "Subject selection (U²-Net)".into(),
        description: "Finds the main subject of a photo".into(),
        source: "https://artifacts.example.test/u2net.onnx".into(),
        revision: "8d10f".into(),
        license: "Apache-2.0".into(),
        license_url: "https://artifacts.example.test/LICENSE".into(),
        download_bytes: 175_997_641,
        installed_bytes: 175_997_641,
        vision: false,
        face_recognition: false,
        in_process: true,
        segmentation: true,
        no_runtime: false,
    }
}

fn with_subject(phase: Option<LocalModelPhase>) -> LocalModelSnapshot {
    LocalModelSnapshot {
        in_process: vec![InProcessLocalModel {
            package: subject_package(),
            phase,
        }],
        ..snapshot(None)
    }
}

pub(super) fn rendered_names(model: &AgentModelsModel) -> Vec<String> {
    let composition = model.build_composition();
    let mut nodes = Vec::new();
    collect_accessibility(composition.as_ref(), &mut nodes);
    nodes.into_iter().filter_map(|node| node.name).collect()
}

/// An in-process model has its own card with the recommendation's
/// licence confirmation, a link opens it at that confirmation without
/// downloading anything, and the install action is itself the
/// acceptance of the revision it was offered with.
#[test]
fn in_process_models_are_set_up_with_the_same_confirmation() {
    // Case `setup_confirms_before_install`.
    {
        let control = mock(with_subject(None));
        let model = model_over(control.clone());
        assert_eq!(
            rendered_names(&model)
                .iter()
                .filter(|name| *name == "Set up on this device")
                .count(),
            2
        );
        model.reviewing_local.set(Some("u2net-subject".into()));
        assert!(
            rendered_names(&model)
                .iter()
                .any(|name| name == "Accept license & download")
        );
        assert!(control.calls.lock().unwrap().is_empty());
        model
            .manager_work(&AgentModelsEffect::InstallModel {
                model: "u2net-subject".into(),
                revision: "8d10f".into(),
            })
            .unwrap()
            .run(control.as_ref())
            .unwrap();
        assert_eq!(
            control.calls.lock().unwrap().as_slice(),
            ["install:u2net-subject@8d10f"]
        );
        assert!(
            model
                .manager_work(&AgentModelsEffect::InstallModel {
                    model: "u2net-subject".into(),
                    revision: "stale".into(),
                })
                .is_err(),
            "the acceptance holds only for the revision it was offered with"
        );
    }

    // Case `a_link_reveals_the_card_at_its_confirmation`.
    {
        let control = mock(with_subject(None));
        let mut model = model_over(control.clone());
        model.reveal("u2net-subject").unwrap();
        assert_eq!(
            model.reviewing_local.with(Clone::clone).as_deref(),
            Some("u2net-subject")
        );
        assert_eq!(model.revealed.as_deref(), Some("u2net-subject"));
        assert!(control.calls.lock().unwrap().is_empty());
        assert!(model.reveal("no-such-model").is_err());
    }

    // Case `each_model_is_a_row_with_its_install`.
    {
        let model = model_over(mock(with_subject(None)));
        let area = AgentModelsArea {
            cx: model.cx.clone(),
            model: std::cell::OnceCell::from(Rc::new(std::cell::RefCell::new(model))),
            read: crate::device_read::DeviceRead::default(),
        };
        let published = area
            .rows()
            .into_iter()
            .find(|row| row.id() == "settings.agent-model:u2net-subject")
            .expect("the in-process model is a row")
            .into_resource();
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "install-model")
        );
        assert!(
            !published
                .actions
                .iter()
                .any(|action| action.id == "remove-model")
        );
    }
}

/// A ready in-process model offers only Remove, a destructive action the
/// host confirms, and never joins the local chat profile.
#[test]
fn ready_in_process_models_offer_only_a_confirmed_remove() {
    let ready = LocalModelPhase::Ready {
        running: false,
        endpoint: None,
    };
    let control = mock(with_subject(Some(ready.clone())));
    let model = model_over(control.clone());
    let names = rendered_names(&model);
    assert!(!names.iter().any(|name| name == "Start"));
    assert_eq!(
        names
            .iter()
            .filter(|name| *name == "Set up on this device")
            .count(),
        1,
        "only the recommendation still offers setup"
    );
    let remove = SettingsEffect::from(AgentModelsEffect::RemoveModel {
        model: "u2net-subject".into(),
    })
    .bound();
    assert_eq!(
        remove.contract.confirmation,
        ui::tilo_actions::ConfirmationPolicy::Confirm
    );
    assert!(
        model
            .manager_work(&AgentModelsEffect::StartModel {
                model: "u2net-subject".into(),
            })
            .is_err(),
        "an in-process model is never started"
    );
    model
        .manager_work(&AgentModelsEffect::RemoveModel {
            model: "u2net-subject".into(),
        })
        .unwrap()
        .run(control.as_ref())
        .unwrap();
    assert_eq!(
        control.calls.lock().unwrap().as_slice(),
        ["remove:u2net-subject"]
    );

    let installed = vec![InstalledLocalModel {
        package: subject_package(),
        phase: ready,
    }];
    assert!(synced_local_profile(None, &installed).is_none());
}

pub(super) fn snapshot(phase: Option<LocalModelPhase>) -> LocalModelSnapshot {
    let package = package();
    LocalModelSnapshot {
        hardware: HardwareRecommendation {
            fit: HardwareFit::Good,
            memory_gib: 24,
            accelerator: "GPU with 12 GiB".into(),
            accelerator_memory_gib: Some(12),
            free_storage_gib: Some(80),
            reason: "Balanced for this device".into(),
        },
        recommendation: Some(package.clone()),
        models: phase
            .map(|phase| vec![InstalledLocalModel { package, phase }])
            .unwrap_or_default(),
        in_process: Vec::new(),
        accelerator_setup: None,
    }
}

pub(super) struct MockControl {
    snapshot: Mutex<LocalModelSnapshot>,
    pub(super) calls: Mutex<Vec<String>>,
}

impl LocalModelControl for MockControl {
    fn snapshot(&self) -> Result<LocalModelSnapshot, String> {
        Ok(self.snapshot.lock().unwrap().clone())
    }
    fn install(&self, model: &LocalModelPackage) -> Result<(), String> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("install:{}@{}", model.id, model.revision));
        Ok(())
    }
    fn start(&self, id: &str) -> Result<(), String> {
        self.calls.lock().unwrap().push(format!("start:{id}"));
        Ok(())
    }
    fn stop(&self, id: &str) -> Result<(), String> {
        self.calls.lock().unwrap().push(format!("stop:{id}"));
        Ok(())
    }
    fn remove(&self, id: &str) -> Result<(), String> {
        self.calls.lock().unwrap().push(format!("remove:{id}"));
        Ok(())
    }
    fn stage_accelerator(&self) -> Result<(), String> {
        self.calls.lock().unwrap().push("stage-accelerator".into());
        Ok(())
    }
}

impl super::hub::HubControl for MockControl {
    fn hub_install(&self, repo: &str, revision: &str, variant: &str) -> Result<(), String> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("hub-install:{repo}@{revision}:{variant}"));
        Ok(())
    }
    fn choose_model_file(&self) -> Result<Option<(String, std::fs::File)>, String> {
        let file = std::fs::File::open("/dev/null").map_err(|error| error.to_string())?;
        Ok(Some(("Tune.Q4_K_M.gguf".into(), file)))
    }
    fn import_file(&self, name: &str, _file: std::fs::File) -> Result<(), String> {
        self.calls.lock().unwrap().push(format!("import:{name}"));
        Ok(())
    }
    fn save_hub_token(&self, token: &str) -> Result<(), String> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("hub-token:{token}"));
        Ok(())
    }
}

fn collect_accessibility(widget: &dyn Widget, nodes: &mut Vec<ui::a11y::AccessNode>) {
    if let Some(node) = widget.accessibility() {
        nodes.push(node);
    }
    for child in widget.children() {
        collect_accessibility(child, nodes);
    }
}

/// A hosted provider whose key this device's vault lacks says so and
/// offers to take one; once the vault holds it, it says it is stored.
#[test]
fn a_hosted_provider_without_a_key_here_asks_for_one() {
    let mut pane = model_over(mock(snapshot(None)));
    pane.change_providers(ProviderEffect::AddProvider {
        preset: "anthropic".into(),
    })
    .unwrap();
    let profile = pane.providers.get().config.profiles[0].clone();
    pane.keys
        .insert(profile.id.clone(), Ok(CredentialStatus::Missing));
    assert!(pane.needs_key(&profile));
    let text = crate::widgets::announced_text(pane.compose().as_ref());
    assert!(
        text.iter().any(|line| line == "Needs a key on this device"),
        "{text:#?}"
    );

    pane.keys
        .insert(profile.id.clone(), Ok(CredentialStatus::Configured));
    assert!(!pane.needs_key(&profile));
    let save = SettingsEffect::from(ProviderEffect::SaveProviderKey {
        provider: profile.id,
        key: None,
    })
    .bound_asking(&["key"]);
    assert!(save.params().iter().any(|param| param.name == "key"));
}

#[test]
fn a_legacy_key_requires_an_explicit_endpoint_and_offers_removal() {
    let mut pane = model_over(mock(snapshot(None)));
    pane.change_providers(ProviderEffect::AddProvider {
        preset: "anthropic".into(),
    })
    .unwrap();
    let profile = pane.providers.get().config.profiles[0].clone();
    pane.keys
        .insert(profile.id.clone(), Ok(CredentialStatus::MigrationRequired));
    assert!(pane.needs_key(&profile));
    let text = crate::widgets::announced_text(pane.compose().as_ref());
    assert!(
        text.iter()
            .any(|line| line.contains("unbound legacy API key")),
        "{text:#?}"
    );
    assert!(
        text.iter().any(|line| line == "Remove unbound legacy key"),
        "{text:#?}"
    );
    let migration = SettingsEffect::from(ProviderEffect::MigrateProviderKey {
        provider: profile.id,
        original_endpoint: String::new(),
    })
    .bound_asking(&["original_endpoint"]);
    assert!(
        migration
            .params()
            .iter()
            .any(|param| param.name == "original_endpoint")
    );
}

/// Google Vertex AI is added only once signed in, with the project,
/// location and model chosen; it shows its sign-in rather than a key,
/// and removing it forgets its own grant and no other.
#[test]
fn a_vertex_provider_signs_in_and_forgets_only_its_own_grant() {
    use account_custody::TokenSet;
    use account_custody::cloud_projects::CloudProject;
    let vault = tempfile::tempdir().unwrap();
    let mut pane = model_over(mock(snapshot(None)));
    pane.custodian = account_custody::Custodian::for_root(Some(vault.path().to_path_buf()));
    let grant = |refresh: &str| TokenSet {
        access_token: "access".into(),
        scope: Some("https://www.googleapis.com/auth/cloud-platform".into()),
        refresh_token: refresh.into(),
        expires_in: 3600,
        obtained_at: 0,
    };
    let mail = accounts::AccountId::new("ada@example.com");
    pane.custodian
        .seal_oauth(&mail, &grant("mail-refresh"))
        .unwrap();

    pane.vertex = Some(VertexSetup::adding());
    let id = pane.vertex.as_ref().unwrap().profile_id.clone();
    let add = |project: &str| VertexChange::Vertex {
        provider: None,
        project: project.into(),
        location: None,
        model: None,
    };
    assert_eq!(
        pane.change_vertex(&add("tilo-509618")),
        Err(t("agent-vertex-sign-in-first")),
        "nothing is saved before the sign-in"
    );
    pane.custodian
        .seal_oauth(&agent_vertex::grant_id(&id), &grant("vertex-refresh"))
        .unwrap();
    pane.vertex
        .as_mut()
        .unwrap()
        .signed_in_for_test(vec![CloudProject {
            id: "tilo-509618".into(),
            name: "Tilo".into(),
        }]);
    let text = crate::widgets::announced_text(pane.compose().as_ref());
    assert!(
        text.iter().any(|line| line == "Tilo (tilo-509618)"),
        "{text:#?}"
    );
    assert_eq!(
        pane.change_vertex(&add("Not A Project")),
        Err(t("agent-vertex-project-invalid"))
    );
    assert!(pane.providers.get().config.profiles.is_empty());

    pane.change_vertex(&add("tilo-509618")).unwrap();
    assert!(pane.vertex.is_none(), "the setup ends once saved");
    let stored = pane.cx.settings::<AgentProviders>().get().config;
    let profile = &stored.profiles[0];
    assert_eq!(profile.id, id, "the grant was sealed for this provider");
    assert!(profile.signs_in_with_google());
    assert!(profile.credential.is_none());
    assert_eq!(
        profile.endpoint,
        "https://aiplatform.googleapis.com/v1/projects/tilo-509618/locations/global/endpoints/openapi"
    );
    assert_eq!(
        profile.sign_in.as_ref().unwrap().billing_project.as_deref(),
        Some("tilo-509618")
    );
    assert_eq!(
        profile.default_model.as_deref(),
        Some("google/gemini-4-argon")
    );
    assert!(stored.validation_errors().is_empty());

    shows_its_sign_in_and_forgets_only_its_grant(&mut pane, &id, &mail);
}

fn shows_its_sign_in_and_forgets_only_its_grant(
    pane: &mut AgentModelsModel,
    id: &str,
    mail: &accounts::AccountId,
) {
    use account_custody::GrantState;
    let id = id.to_string();
    pane.vertex_keys.insert(id.clone(), Ok(false));
    let text = crate::widgets::announced_text(pane.compose().as_ref());
    assert!(text.iter().any(|line| line == "Sign in again"), "{text:#?}");
    assert!(!text.iter().any(|line| line == "Save key"), "{text:#?}");
    pane.vertex_keys.insert(id.clone(), Ok(true));
    let text = crate::widgets::announced_text(pane.compose().as_ref());
    assert!(
        text.iter()
            .any(|line| line == "Signed in with Google on this device."),
        "{text:#?}"
    );

    pane.change_vertex(&VertexChange::Vertex {
        provider: Some(id.clone()),
        project: "other-project".into(),
        location: Some("europe-west4".into()),
        model: Some("gemini-2.5-pro".into()),
    })
    .unwrap();
    let profile = pane.cx.settings::<AgentProviders>().get().config.profiles[0].clone();
    assert_eq!(
        profile.endpoint,
        "https://europe-west4-aiplatform.googleapis.com/v1/projects/other-project/locations/europe-west4/endpoints/openapi"
    );
    assert_eq!(
        profile.default_model.as_deref(),
        Some("google/gemini-2.5-pro")
    );
    assert_eq!(
        profile.sign_in.unwrap().billing_project.as_deref(),
        Some("other-project")
    );

    pane.change_vertex(&VertexChange::Remove {
        provider: id.clone(),
    })
    .unwrap();
    assert!(pane.providers.get().config.profiles.is_empty());
    assert_eq!(
        pane.custodian
            .oauth_grant_state(&agent_vertex::grant_id(&id))
            .unwrap(),
        GrantState::Missing
    );
    assert_eq!(
        pane.custodian.oauth_grant_state(mail).unwrap(),
        GrantState::Usable,
        "Mail's grant of the same person is untouched"
    );
}

/// Cancelling the setup of a provider being added leaves no grant.
#[test]
fn cancelling_a_vertex_setup_forgets_its_grant() {
    let vault = tempfile::tempdir().unwrap();
    let mut pane = model_over(mock(snapshot(None)));
    pane.custodian = account_custody::Custodian::for_root(Some(vault.path().to_path_buf()));
    pane.vertex = Some(VertexSetup::adding());
    let id = pane.vertex.as_ref().unwrap().profile_id.clone();
    pane.custodian
        .seal_oauth(
            &agent_vertex::grant_id(&id),
            &account_custody::TokenSet {
                access_token: "a".into(),
                scope: None,
                refresh_token: "r".into(),
                expires_in: 3600,
                obtained_at: 0,
            },
        )
        .unwrap();
    pane.cancel_vertex();
    assert!(pane.vertex.is_none());
    assert!(!agent_vertex::signed_in(&pane.custodian, &id));
}

pub(super) fn model_over(control: Arc<MockControl>) -> AgentModelsModel {
    let profile = tempfile::tempdir().unwrap();
    let cx = PaneCx::in_file(profile.path().join("settings.json"));
    let mut model = AgentModelsModel::new(cx, control);
    model.follow_local_profile = false;
    model
}

pub(super) fn mock(snapshot: LocalModelSnapshot) -> Arc<MockControl> {
    Arc::new(MockControl {
        snapshot: Mutex::new(snapshot),
        calls: Mutex::new(Vec::new()),
    })
}

/// No fabricated recommendations; installs are of the reported
/// package, and progress is bounded and readable.
#[test]
fn the_catalog_is_honest_and_installs_what_it_reported() {
    let fallback = fallback_snapshot();
    assert!(fallback.recommendation.is_none());
    let pane = model_over(mock(fallback));
    let composition = pane.build_composition();
    let mut nodes = Vec::new();
    collect_accessibility(composition.as_ref(), &mut nodes);
    let setup = nodes
        .iter()
        .find(|node| node.name.as_deref() == Some("Set up unavailable"))
        .expect("disabled setup remains understandable to assistive technology");
    assert!(setup.state.disabled);

    let control = mock(snapshot(None));
    let pane = model_over(control.clone());
    let work = pane
        .manager_work(&AgentModelsEffect::InstallModel {
            model: "test/model".into(),
            revision: "0123456789abcdef".into(),
        })
        .unwrap();
    work.run(control.as_ref()).unwrap();
    assert_eq!(
        control.calls.lock().unwrap().as_slice(),
        ["install:test/model@0123456789abcdef"]
    );
    assert!(
        pane.manager_work(&AgentModelsEffect::InstallModel {
            model: "test/model".into(),
            revision: "stale".into(),
        })
        .is_err(),
        "only the reported revision is installed"
    );

    let phase = LocalModelPhase::Downloading {
        downloaded_bytes: 3,
        total_bytes: 4,
    };
    assert_eq!(phase_progress(&phase), Some(0.75));
    let progress = ProgressBar::new(phase_progress(&phase).unwrap()).with_label("Download");
    let access = Widget::accessibility(&progress).expect("labelled progress is accessible");
    assert_eq!(access.value.as_deref(), Some("75%"));
}

/// Only the controls a model's phase allows reach the manager.
#[test]
fn local_model_lifecycle_exposes_only_safe_controls() {
    assert_eq!(
        available_actions(&LocalModelPhase::Ready {
            running: false,
            endpoint: None
        }),
        [ModelAction::Start, ModelAction::Remove]
    );
    assert_eq!(
        available_actions(&LocalModelPhase::Ready {
            running: true,
            endpoint: None
        }),
        [ModelAction::Stop, ModelAction::Remove]
    );
    assert!(
        available_actions(&LocalModelPhase::Downloading {
            downloaded_bytes: 4,
            total_bytes: 10,
        })
        .is_empty()
    );
    let control = mock(LocalModelSnapshot {
        accelerator_setup: Some(AcceleratorSetup {
            boot_pack_id: "gpu-stack".into(),
            status: AcceleratorSetupState::Available,
        }),
        ..snapshot(Some(LocalModelPhase::Ready {
            running: false,
            endpoint: None,
        }))
    });
    let pane = model_over(control.clone());
    for effect in [
        AgentModelsEffect::StartModel {
            model: "test/model".into(),
        },
        AgentModelsEffect::RemoveModel {
            model: "test/model".into(),
        },
        AgentModelsEffect::SetUpAccelerator,
    ] {
        pane.manager_work(&effect)
            .unwrap()
            .run(control.as_ref())
            .unwrap();
    }
    assert!(
        pane.manager_work(&AgentModelsEffect::StopModel {
            model: "test/model".into()
        })
        .is_err(),
        "a stopped model is not stopped again"
    );
    assert_eq!(
        control.calls.lock().unwrap().as_slice(),
        ["start:test/model", "remove:test/model", "stage-accelerator"]
    );
}

/// The local profile follows the manager's ready endpoint, not a fixed
/// port.
#[test]
fn local_profile_uses_the_ready_manager_endpoint_not_port_8000() {
    let mut package = package();
    package.vision = true;
    package.face_recognition = true;
    let model_id = package.id.clone();
    let models = vec![InstalledLocalModel {
        package,
        phase: LocalModelPhase::Ready {
            running: true,
            endpoint: Some(agent_provider_config::local::LocalEndpoint {
                url: "http://127.0.0.1:43127/v1".into(),
                model_id,
            }),
        },
    }];
    let profile = synced_local_profile(None, &models).expect("ready local profile");
    assert_eq!(profile.endpoint, "http://127.0.0.1:43127/v1");
    assert_eq!(profile.default_model.as_deref(), Some("test/model"));
    assert_eq!(
        profile.models[0].revision.as_deref(),
        Some("0123456789abcdef")
    );
    assert!(profile.models[0].vision);
    assert!(profile.models[0].face_recognition);
    assert!(profile.enabled);
    let mut configured = profile;
    configured.models[0].context_window = Some(262_144);
    configured.models[0].request_limits = Some(ModelRequestLimits {
        max_output_tokens: 512,
        runtime_context_tokens: Some(4096),
        plan_timeout_ms: Some(120_000),
    });
    let refreshed = synced_local_profile(Some(configured.clone()), &models).unwrap();
    assert_eq!(
        refreshed.models[0].request_limits,
        configured.models[0].request_limits
    );
    assert_eq!(refreshed.models[0].context_window, Some(262_144));
}

#[test]
fn model_request_limits_use_the_published_settings_effects() {
    use ui::framework::testing::TestApp;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    let mut profile = ProviderProfile::hosted(ProviderProtocol::OpenAiCompatible);
    profile.models = vec![
        ModelDescriptor::new("test-model"),
        ModelDescriptor::new("test_model"),
    ];
    profile.models[0].context_window = Some(262_144);
    let profile_id = profile.id.clone();
    ProviderConfig {
        profiles: vec![profile],
        default_profile: None,
        ..Default::default()
    }
    .save_at(&path)
    .unwrap();
    let mut app = TestApp::<crate::resource::SettingsShell>::new().unwrap();
    app.surface_mut::<SettingsApp>()
        .attach_host(PaneCx::in_file(path.clone()), None);
    app.surface_mut::<SettingsApp>()
        .areas()
        .find::<crate::panes::agent_models::AgentModelsArea>()
        .unwrap()
        .model()
        .borrow_mut()
        .follow_local_profile = false;
    let open = crate::categories::Category::AgentModels;
    app.effect_on(
        &crate::effects::pane_row_id(open),
        &crate::effects::open_pane(open),
    )
    .unwrap();
    app.settle();
    let resource = format!(
        "settings.agent-provider-model:{}:{}",
        hex::encode(&profile_id),
        hex::encode("test-model")
    );
    app.call_on(
        &resource,
        "set-model-request-limits",
        serde_json::json!({"max_output_tokens":512,"runtime_context_tokens":4096,"plan_timeout_ms":120_000}),
    )
    .unwrap();
    app.settle();
    let value = app
        .call_on(
            &resource,
            "inspect-model-request-limits",
            serde_json::json!({}),
        )
        .unwrap()
        .unwrap();
    assert_eq!(value["request_limits"]["max_output_tokens"], 512);
    assert_eq!(value["request_limits"]["plan_timeout_ms"], 120_000);
    assert_eq!(value["plan_timeout_ceiling_ms"], 120_000);
    assert_eq!(value["caller_may_request_shorter_deadline"], true);
    assert_eq!(value["source_context_tokens"], 262_144);
    assert_eq!(value["runtime_context_attested"], false);
    assert_eq!(value["prompt_fit_proven"], false);
    let other = format!(
        "settings.agent-provider-model:{}:{}",
        hex::encode(&profile_id),
        hex::encode("test_model")
    );
    let other_value = app
        .call_on(
            &other,
            "inspect-model-request-limits",
            serde_json::json!({}),
        )
        .unwrap()
        .unwrap();
    assert!(other_value["request_limits"].is_null());
    assert_eq!(other_value["plan_timeout_ceiling_ms"], 60_000);
    let saved = fs::read(&path).unwrap();
    for invalid in [
        serde_json::json!({"max_output_tokens":4096,"runtime_context_tokens":4096}),
        serde_json::json!({"max_output_tokens":512,"plan_timeout_ms":0}),
        serde_json::json!({"max_output_tokens":512,"plan_timeout_ms":900_001}),
        serde_json::json!({"plan_timeout_ms":120_000}),
        serde_json::json!({"model":"absent-model","max_output_tokens":512}),
    ] {
        assert!(
            app.call_on(&resource, "set-model-request-limits", invalid)
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), saved);
    }
    app.call_on(&resource, "set-model-request-limits", serde_json::json!({}))
        .unwrap();
    assert!(
        ProviderConfig::load_at(&path).profiles[0].models[0]
            .request_limits
            .is_none()
    );
    assert!(app.harness().component_contract_violations().is_empty());
}

/// Providers are the person's settings: added, renamed, made default
/// and removed through effects, which refuse a provider that is gone.
#[test]
fn providers_are_the_persons_settings() {
    let mut pane = model_over(mock(snapshot(None)));
    pane.change_providers(ProviderEffect::AddProvider {
        preset: "anthropic".into(),
    })
    .unwrap();
    let id = pane.providers.get().config.profiles[0].id.clone();
    pane.change_providers(ProviderEffect::SetProviderDetails {
        provider: id.clone(),
        label: "Work".into(),
        endpoint: "https://api.example.test".into(),
        model: Some("big-model".into()),
    })
    .unwrap();
    pane.change_providers(ProviderEffect::SetDefaultProvider {
        provider: id.clone(),
    })
    .unwrap();
    let stored = pane.cx.settings::<AgentProviders>().get().config;
    assert_eq!(stored.default_profile.as_deref(), Some(id.as_str()));
    assert_eq!(stored.profiles[0].label, "Work");
    assert!(
        pane.change_providers(ProviderEffect::SetProviderEnabled {
            provider: "gone".into(),
            on: true,
        })
        .is_err()
    );
}

/// A model manager that takes requests at once but does not report
/// its snapshot until it is let go, as a wedged `tilo-model-manager`.
/// Once let go it reports a model ready.
struct StalledManager(std::sync::Arc<crate::stall::Stall>);

fn one_model(name: &str, phase: LocalModelPhase) -> LocalModelSnapshot {
    LocalModelSnapshot {
        models: vec![InstalledLocalModel {
            package: LocalModelPackage {
                name: name.into(),
                ..package()
            },
            phase,
        }],
        ..snapshot(None)
    }
}

impl super::hub::HubControl for StalledManager {}

impl LocalModelControl for StalledManager {
    fn snapshot(&self) -> Result<LocalModelSnapshot, String> {
        crate::device_read::off_ui_thread();
        if !self.0.wait() {
            return Err("the model manager never answered".into());
        }
        Ok(one_model(
            "Porch Model",
            LocalModelPhase::Ready {
                running: false,
                endpoint: None,
            },
        ))
    }
    fn install(&self, _: &LocalModelPackage) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Ok(())
    }
    fn start(&self, _: &str) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Ok(())
    }
    fn stop(&self, _: &str) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Ok(())
    }
    fn remove(&self, _: &str) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Ok(())
    }
    fn stage_accelerator(&self) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Ok(())
    }
}

/// A click is answered and the window keeps drawing while the model
/// manager takes its time to report: neither the snapshot asked after
/// the change nor the one a busy model's polling asks holds the window,
/// and the snapshot lands once the manager answers.
#[test]
fn a_stalled_model_manager_never_holds_the_window() {
    use ui::framework::testing::TestApp;

    let mut app = TestApp::<crate::resource::SettingsShell>::new().unwrap();
    crate::effects::show_pane(&mut app, crate::categories::Category::AgentModels);
    let gate = crate::stall::Stall::new();
    let stalled = Arc::clone(&gate);
    app.with_widget::<SettingsApp, _>(move |settings| {
        let model = crate::panes::agent_models::area(settings).unwrap().model();
        let mut model = model.borrow_mut();
        model.follow_local_profile = false;
        model.control = Arc::new(StalledManager(stalled));
        // A model downloading: the pane asks the manager again soon.
        model.snapshot = one_model(
            "Busy Model",
            LocalModelPhase::Downloading {
                downloaded_bytes: 1,
                total_bytes: 4,
            },
        );
        model.last_poll = ui::time::now()
            .checked_sub(POLL_INTERVAL)
            .unwrap_or_else(ui::time::now);
        model.changed = true;
    })
    .expect("the Settings window is mounted");

    app.start_effect(&SettingsEffect::from(AgentModelsEffect::SetUpAccelerator))
        .expect("the accelerator setup runs as a job");
    app.harness()
        .poll_frame()
        .expect("the window draws its next frame");
    gate.assert_held("the click");
    let shown = |app: &mut TestApp<crate::resource::SettingsShell>| {
        app.with_widget::<SettingsApp, _>(|settings| {
            let model = crate::panes::agent_models::area(settings)
                .unwrap()
                .model()
                .borrow();
            model
                .snapshot
                .models
                .iter()
                .map(|model| model.package.name.clone())
                .collect::<Vec<_>>()
        })
        .unwrap()
    };
    assert_eq!(
        shown(&mut app),
        ["Busy Model"],
        "the last snapshot is shown until the manager answers"
    );

    gate.release();
    let deadline = tilo_test_env::hang_deadline();
    while shown(&mut app) != ["Porch Model"] {
        assert!(
            std::time::Instant::now() < deadline,
            "the snapshot the manager reported is shown"
        );
        app.settle();
        std::thread::yield_now();
    }
}

/// Wait for a background read to end, as the warm-up's wake would.
fn until_finished(finished: impl Fn() -> bool) {
    let deadline = tilo_test_env::hang_deadline();
    while !finished() {
        assert!(std::time::Instant::now() < deadline, "the read ends");
        std::thread::yield_now();
    }
}

/// The warm-up asks the manager off the UI thread once; the model is
/// built from that answer, and nothing is asked again once it is.
#[test]
fn the_area_builds_its_model_from_the_read_it_started() {
    let profile = tempfile::tempdir().unwrap();
    let cx = PaneCx::in_file(profile.path().join("settings.json"));
    let area = AgentModelsArea::new(&cx);
    assert!(area.start_read(), "the first read starts");
    assert!(!area.start_read(), "one read at a time");
    assert!(!area.is_open(), "a running read publishes nothing yet");
    until_finished(|| area.read_finished());
    let installed = area.model().borrow().snapshot.recommendation.clone();
    assert!(area.is_open());
    assert_eq!(
        installed.map(|package| package.id),
        SampleManager
            .snapshot()
            .unwrap()
            .recommendation
            .map(|package| package.id),
        "the model shows what the read asked"
    );
    assert!(!area.start_read(), "a built model is not read again");
}

/// What the read asked is what the model shows: the manager's answer,
/// or why it could not answer.
#[test]
fn a_model_shows_what_its_read_asked() {
    let profile = tempfile::tempdir().unwrap();
    let cx = PaneCx::in_file(profile.path().join("settings.json"));
    let control = mock(snapshot(None));
    let read = AgentModelsRead::of(control.as_ref(), Vec::new());
    let model = AgentModelsModel::from_read(cx.clone(), control.clone(), read);
    assert!(model.manager_available);
    assert_eq!(
        model
            .snapshot
            .recommendation
            .as_ref()
            .map(|package| package.id.clone()),
        Some(package().id)
    );
    let down = AgentModelsRead {
        snapshot: Err("the manager is not running".into()),
        keys: HashMap::new(),
        hub_token: Ok(HubTokenState::None),
    };
    let model = AgentModelsModel::from_read(cx, control, down);
    assert!(!model.manager_available);
    assert_eq!(
        model.notice,
        Some(t_args(
            "agent-notice-service-down",
            &[("error", "the manager is not running".into())]
        )),
        "the person is told the manager is down"
    );
}

/// Settings publishes the provider panel's effects as the panel declares
/// them: the same ids, labels, parameters (a key stays a secret), kinds,
/// risks and confirmation, read through Settings' own union. A second,
/// area-local copy would drift from the controls the panel binds.
#[test]
fn the_provider_effects_are_published_as_the_panel_declares_them() {
    use ui::tilo_actions::{ActionKind, ParamType, RiskLevel};

    let published = |id: &str| {
        SettingsEffect::declarations()
            .into_iter()
            .find(|decl| decl.id == id)
            .unwrap_or_else(|| panic!("`{id}` is published by Settings"))
    };
    let shared = ProviderEffect::declarations();
    assert_eq!(shared.len(), 16, "the provider panel's whole vocabulary");
    for declaration in &shared {
        let union = published(declaration.id);
        assert_eq!(union.label, declaration.label, "{}", declaration.id);
        assert_eq!(
            union.description, declaration.description,
            "{}",
            declaration.id
        );
        assert_eq!(union.kind, declaration.kind, "{}", declaration.id);
        assert_eq!(union.row, declaration.row, "{}", declaration.id);
        assert_eq!(union.risk, declaration.risk, "{}", declaration.id);
        assert_eq!(
            union.action_kind, declaration.action_kind,
            "{}",
            declaration.id
        );
        assert_eq!(
            union.idempotent, declaration.idempotent,
            "{}",
            declaration.id
        );
        assert_eq!(union.params, declaration.params, "{}", declaration.id);
    }
    let save = published("save-provider-key");
    assert_eq!(save.params[0].name, "provider");
    assert_eq!(save.params[1].ty, ParamType::Secret, "a key is a secret");
    assert_eq!(published("remove-provider").risk, RiskLevel::Destructive);
    assert_eq!(
        published("inspect-model-request-limits").action_kind,
        ActionKind::Read
    );
    assert_eq!(published("refresh-providers").label, "Refresh providers");
}

/// The provider panel's effects, with every argument a caller would give:
/// each round-trips through Settings' union by its published id.
fn every_provider_effect() -> Vec<ProviderEffect> {
    vec![
        ProviderEffect::RefreshProviders,
        ProviderEffect::AddProvider {
            preset: "openai".into(),
        },
        ProviderEffect::ListProviderModels {
            provider: "p".into(),
        },
        ProviderEffect::SetProviderModel {
            provider: "p".into(),
            model: "m".into(),
        },
        ProviderEffect::SetProviderProtocol {
            provider: "p".into(),
            protocol: "anthropic".into(),
        },
        ProviderEffect::SetProviderEnabled {
            provider: "p".into(),
            on: false,
        },
        ProviderEffect::SetProviderDetails {
            provider: "p".into(),
            label: "Provider".into(),
            endpoint: "https://models.example.test/v1".into(),
            model: None,
        },
        ProviderEffect::SetModelRequestLimits {
            provider: "p".into(),
            model: "m".into(),
            max_output_tokens: None,
            runtime_context_tokens: None,
            plan_timeout_ms: None,
        },
        ProviderEffect::InspectModelRequestLimits {
            provider: "p".into(),
            model: "m".into(),
        },
        ProviderEffect::SetDefaultProvider {
            provider: "p".into(),
        },
        ProviderEffect::SetAgentModel {
            agent: "a".into(),
            provider: None,
            model: None,
        },
        ProviderEffect::RemoveProvider {
            provider: "p".into(),
        },
        ProviderEffect::SaveProviderKey {
            provider: "p".into(),
            key: None,
        },
        ProviderEffect::MigrateProviderKey {
            provider: "p".into(),
            original_endpoint: "https://models.example.test/v1".into(),
        },
        ProviderEffect::RemoveLegacyProviderKey {
            provider: "p".into(),
        },
        ProviderEffect::RemoveProviderKey {
            provider: "p".into(),
        },
    ]
}

/// Each provider effect decodes back out of Settings' union by the id and
/// arguments an invoker gives it, as the palette and agents invoke it.
#[test]
fn every_provider_effect_round_trips_through_settings_effects() {
    use ui::tilo_resource::Effect as _;

    let effects = every_provider_effect();
    assert_eq!(effects.len(), ProviderEffect::declarations().len());
    for effect in effects {
        let decoded = SettingsEffect::decode(effect.id(), &effect.arguments())
            .unwrap_or_else(|error| panic!("{}: {error}", effect.id()));
        assert_eq!(decoded.id(), effect.id());
        assert_eq!(decoded.arguments(), effect.arguments());
    }
}
