// The mounted public Settings surface must not await model-service status.
struct HeldModelControl {
    entered: std::sync::mpsc::Sender<()>,
    gate: Arc<(Mutex<bool>, std::sync::Condvar)>,
    returned: Arc<std::sync::atomic::AtomicBool>,
}
impl LocalModelControl for HeldModelControl {
    fn snapshot(&self) -> Result<LocalModelSnapshot, String> {
        self.entered.send(()).unwrap();
        let (lock, ready) = self.gate.as_ref();
        let (released, _) = ready
            .wait_timeout_while(
                lock.lock().unwrap(),
                std::time::Duration::from_secs(5),
                |released| !*released,
            )
            .unwrap();
        let completed = *released;
        drop(released);
        self.returned
            .store(true, std::sync::atomic::Ordering::Release);
        if completed {
            SampleManager.snapshot()
        } else {
            Err("owned model read not released".into())
        }
    }
    fn install(&self, model: &LocalModelPackage) -> Result<(), String> {
        SampleManager.install(model)
    }
    fn start(&self, id: &str) -> Result<(), String> {
        SampleManager.start(id)
    }
    fn stop(&self, id: &str) -> Result<(), String> {
        SampleManager.stop(id)
    }
    fn remove(&self, id: &str) -> Result<(), String> {
        SampleManager.remove(id)
    }
    fn stage_accelerator(&self) -> Result<(), String> {
        SampleManager.stage_accelerator()
    }
}
#[test]
fn mounted_settings_search_navigation_progresses_while_model_service_is_held() {
    mounted_model_hold(std::time::Duration::from_millis(150));
}
#[test]
fn mounted_settings_search_navigation_adopts_before_model_backend_returns() {
    mounted_model_hold(std::time::Duration::from_secs(2));
}
fn mounted_model_hold(deadline: std::time::Duration) {
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let (entered, properties) = std::sync::mpsc::channel();
    let (done, frames) = std::sync::mpsc::channel();
    let returned = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let control = Arc::new(HeldModelControl {
        entered,
        gate: gate.clone(),
        returned: returned.clone(),
    });
    let ui = std::thread::spawn(move || {
        let mut app = ui::prelude::TestApp::<crate::resource::SettingsShell>::new().unwrap();
        let area = app.surface_mut::<SettingsApp>().agent_models_mut();
        area.model.take();
        area.control = control;
        area.device = true;
        let started = std::time::Instant::now();
        app.harness().relayout();
        let publication = started.elapsed();
        let started = std::time::Instant::now();
        let address = crate::effects::open_pane_address(crate::categories::Category::Appearance);
        let outcome = app
            .harness()
            .invoke_action(&address, &serde_json::Map::new(), false, None)
            .unwrap();
        let invocation = started.elapsed();
        let started = std::time::Instant::now();
        app.harness().relayout();
        let adoption = started.elapsed();
        let started = std::time::Instant::now();
        let selected = Some(app.surface_mut::<SettingsApp>().selected());
        println!(
            "MODEL-HOLD publication={publication:?} invocation={invocation:?} adoption={adoption:?} observation={:?}",
            started.elapsed()
        );
        done.send((
            outcome.completed().is_some()
                && selected == Some(crate::categories::Category::Appearance),
            returned.load(std::sync::atomic::Ordering::Acquire),
        ))
        .unwrap();
    });
    properties
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let progressed = frames.recv_timeout(deadline).ok();
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    ui.join().unwrap();
    assert_eq!(
        progressed,
        Some((true, false)),
        "mounted Settings search action must complete before model service responds"
    );
}

type SnapshotReply = std::sync::mpsc::Sender<Result<LocalModelSnapshot, String>>;
struct ControlledModelControl(std::sync::mpsc::Sender<SnapshotReply>);
impl LocalModelControl for ControlledModelControl {
    fn snapshot(&self) -> Result<LocalModelSnapshot, String> {
        let (done, result) = std::sync::mpsc::channel();
        self.0.send(done).unwrap();
        result
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
    }
    fn install(&self, _: &LocalModelPackage) -> Result<(), String> {
        Err("unexpected install".into())
    }
    fn start(&self, _: &str) -> Result<(), String> {
        Err("unexpected start".into())
    }
    fn stop(&self, _: &str) -> Result<(), String> {
        Err("unexpected stop".into())
    }
    fn remove(&self, _: &str) -> Result<(), String> {
        Err("unexpected remove".into())
    }
    fn stage_accelerator(&self) -> Result<(), String> {
        Err("unexpected setup".into())
    }
}
fn controlled_model_area() -> (
    AgentModelsArea,
    std::sync::mpsc::Receiver<SnapshotReply>,
    tempfile::TempDir,
) {
    let profile = tempfile::tempdir().unwrap();
    let (calls, reads) = std::sync::mpsc::channel();
    let area = AgentModelsArea {
        cx: PaneCx::in_file(profile.path().join("settings.json")),
        model: std::cell::OnceCell::new(),
        control: Arc::new(ControlledModelControl(calls)),
        device: true,
    };
    (area, reads, profile)
}
fn model_owner_wake() -> (ui::WakeScope, std::sync::mpsc::Receiver<()>) {
    let mut scope = ui::WakeScope::new();
    let (woke, wakes) = std::sync::mpsc::channel();
    scope
        .bind(move |_| {
            let _ = woke.send(());
        })
        .unwrap();
    (scope, wakes)
}
#[test]
fn background_model_status_wakes_owner_and_publishes_exact_observed_install() {
    let (scope, wakes) = model_owner_wake();
    let _entry = scope.enter();
    let (area, reads, _profile) = controlled_model_area();
    assert!(
        area.rows().is_empty(),
        "pending status offers no invented models"
    );
    let done = reads
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    let text = crate::widgets::announced_text(area.model().borrow().build_composition().as_ref());
    let loading = text.contains(&t("agent-reading-status"));
    let observed = SampleManager.snapshot().unwrap();
    let package = observed.recommendation.clone().unwrap();
    done.send(Ok(observed.clone())).unwrap();
    wakes
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    let rows = area.rows();
    assert!(
        loading,
        "pending service status is not an unavailable service"
    );
    assert_eq!(area.model().borrow().snapshot, observed);
    assert!(area.model().borrow().manager_available);
    let resource = rows
        .into_iter()
        .find(|row| row.id() == format!("settings.agent-model-recommendation:{}", package.id))
        .unwrap()
        .into_resource();
    let expected = SettingsEffect::from(AgentModelsEffect::InstallModel {
        model: package.id,
        revision: package.revision,
    })
    .bound();
    let operation = resource
        .actions
        .iter()
        .find(|action| action.id == expected.id)
        .unwrap();
    assert_eq!(
        operation.build_json(&serde_json::Map::new()).unwrap(),
        expected.build_json(&serde_json::Map::new()).unwrap()
    );
}
#[test]
fn background_model_refresh_coalesces_and_rejects_older_snapshot() {
    let (scope, wakes) = model_owner_wake();
    let _entry = scope.enter();
    let (area, reads, _profile) = controlled_model_area();
    assert!(area.rows().is_empty());
    let first = reads
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    area.model().borrow_mut().notice = Some("successful operation confirmation".into());
    area.model().borrow_mut().refresh();
    area.model().borrow_mut().refresh();
    assert!(
        reads.try_recv().is_err(),
        "only one backend read is in flight"
    );
    first.send(Ok(SampleManager.snapshot().unwrap())).unwrap();
    wakes
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    assert!(
        area.rows().is_empty(),
        "pre-mutation status cannot be adopted"
    );
    let latest = reads
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    latest
        .send(Err("model service unavailable".into()))
        .unwrap();
    wakes
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    assert!(area.rows().is_empty());
    {
        let model = area.model().borrow();
        assert!(!model.manager_available);
        assert!(model.reading.is_none());
        assert_eq!(
            model.read_error.as_deref(),
            Some(t("agent-notice-refresh-err").as_str())
        );
    }
    area.model().borrow_mut().refresh();
    let fresh = reads
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    let observed = SampleManager.snapshot().unwrap();
    fresh.send(Ok(observed.clone())).unwrap();
    wakes
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    assert!(!area.rows().is_empty());
    let model = area.model().borrow();
    assert_eq!(model.snapshot, observed);
    assert!(model.manager_available);
    assert!(model.read_error.is_none());
    assert_eq!(
        model.notice.as_deref(),
        Some("successful operation confirmation"),
        "passive refresh preserves actual operation notice"
    );
}
