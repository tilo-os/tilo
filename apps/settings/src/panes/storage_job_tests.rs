use super::*;
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

struct HeldCleanup {
    entered: mpsc::Sender<std::thread::ThreadId>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl StorageCleanupSource for HeldCleanup {
    fn clean(&self, preview: CleanupPreview, roots: StorageRoots) -> Result<StorageRead, String> {
        self.entered.send(std::thread::current().id()).unwrap();
        self.release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        DeviceStorageCleanup.clean(preview, roots)
    }
}

#[test]
fn mounted_confirmed_cleanup_does_not_wait_for_filesystem_work() {
    let files = tempfile::tempdir().unwrap();
    let cache = files.path().join("cache");
    let other = files.path().join("protected-fixture");
    std::fs::create_dir_all(cache.join("owned-app")).unwrap();
    std::fs::write(cache.join("owned-app/payload"), [0_u8; 24]).unwrap();
    std::fs::write(&other, "preserve").unwrap();
    let (entered, readers) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let holder = std::thread::spawn(move || {
        let thread = readers.recv_timeout(Duration::from_secs(5)).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        release.send(()).unwrap();
        thread
    });
    let mut app = ui::prelude::TestApp::<crate::resource::SettingsShell>::new().unwrap();
    let area = app.surface_mut::<SettingsApp>().storage_mut();
    let mut model = StorageModel::with_roots(
        area.cx.clone(),
        StorageRoots {
            app_payloads: None,
            room_data: None,
            app_cache: Some(cache.clone()),
            sync_replica: None,
        },
    );
    model.cleanup_source = Arc::new(HeldCleanup {
        entered,
        release: Mutex::new(gate),
    });
    area.model.take();
    area.model
        .set(std::rc::Rc::new(std::cell::RefCell::new(model)))
        .ok()
        .unwrap();
    let address = crate::effects::open_pane_address(crate::categories::Category::Storage);
    app.harness()
        .invoke_action(&address, &serde_json::Map::new(), false, None)
        .unwrap();
    assert!(
        app.effect(&crate::effects::SettingsEffect::from(
            StorageEffect::CleanUpCaches
        ))
        .is_err()
    );
    assert!(
        cache.join("owned-app/payload").exists(),
        "confirmation refusal preserves actual bytes"
    );
    let ui_thread = std::thread::current().id();
    let started = Instant::now();
    let job = app.start_effect(&crate::effects::SettingsEffect::from(
        StorageEffect::CleanUpCaches,
    ));
    let elapsed = started.elapsed();
    let reader = holder.join().unwrap();
    assert!(
        elapsed < Duration::from_millis(150),
        "confirmed cleanup blocked UI: {elapsed:?}, reader={reader:?}, UI={ui_thread:?}"
    );
    assert_ne!(reader, ui_thread);
    let job = job.unwrap();
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(3), |h| h
                .job_updates(job)
                .iter()
                .any(ui::tilo_actions::InvocationUpdate::is_terminal))
    );
    assert!(matches!(
        app.harness().job_updates(job).last(),
        Some(ui::tilo_actions::InvocationUpdate::Succeeded { .. })
    ));
    assert!(!cache.join("owned-app").exists());
    assert_eq!(std::fs::read_to_string(other).unwrap(), "preserve");
    assert_eq!(
        app.surface_mut::<SettingsApp>()
            .storage()
            .model()
            .borrow()
            .snapshot
            .reclaimable_bytes(),
        0
    );
}

#[test]
fn pending_cleanup_retains_original_reply_and_revalidates_preview_before_removal() {
    let files = tempfile::tempdir().unwrap();
    let cache = files.path().join("cache");
    std::fs::create_dir(&cache).unwrap();
    let payload = cache.join("owned-payload");
    std::fs::write(&payload, [0_u8; 24]).unwrap();
    let (entered, readers) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let mut app = ui::prelude::TestApp::<crate::resource::SettingsShell>::new().unwrap();
    let area = app.surface_mut::<SettingsApp>().storage_mut();
    let mut model = StorageModel::with_roots(
        area.cx.clone(),
        StorageRoots {
            app_payloads: None,
            room_data: None,
            app_cache: Some(cache),
            sync_replica: None,
        },
    );
    model.cleanup_source = Arc::new(HeldCleanup {
        entered,
        release: Mutex::new(gate),
    });
    area.model.take();
    area.model
        .set(std::rc::Rc::new(std::cell::RefCell::new(model)))
        .ok()
        .unwrap();
    let address = crate::effects::open_pane_address(crate::categories::Category::Storage);
    app.harness()
        .invoke_action(&address, &serde_json::Map::new(), false, None)
        .unwrap();
    let effect = crate::effects::SettingsEffect::from(StorageEffect::CleanUpCaches);
    let first = app.start_effect(&effect).unwrap();
    assert_ne!(
        readers.recv_timeout(Duration::from_secs(3)).unwrap(),
        std::thread::current().id()
    );
    let second = app.effect_confirmed(&effect).unwrap_err();
    assert!(second.to_string().contains(&t("settings-storage-cleaning")));
    assert!(
        !app.harness()
            .job_updates(first)
            .iter()
            .any(ui::tilo_actions::InvocationUpdate::is_terminal)
    );
    for theme in [
        ui::Theme::light(),
        ui::Theme::dark(),
        ui::Theme::high_contrast(),
    ] {
        app.harness().relayout_themed((440, 820), theme).unwrap();
        assert!(
            app.harness()
                .inspection_json()
                .unwrap()
                .contains(&t("settings-storage-cleaning"))
        );
        assert!(!app.harness().targets().contains_key("storage.clean"));
        assert!(app.harness().native_app_audit().is_certified());
    }
    std::fs::write(&payload, [1_u8; 25]).unwrap();
    release.send(()).unwrap();
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(3), |h| h
                .job_updates(first)
                .iter()
                .any(ui::tilo_actions::InvocationUpdate::is_terminal))
    );
    assert!(matches!(
        app.harness().job_updates(first).last(),
        Some(ui::tilo_actions::InvocationUpdate::Failed { .. })
    ));
    assert_eq!(std::fs::read(&payload).unwrap(), [1_u8; 25]);
    assert!(
        app.surface_mut::<SettingsApp>()
            .storage()
            .model()
            .borrow()
            .cleanup_job
            .is_none()
    );
}
