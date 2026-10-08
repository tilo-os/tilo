use super::*;
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

struct HeldRead {
    roots: StorageRoots,
    entered: mpsc::Sender<std::thread::ThreadId>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl StorageReadSource for HeldRead {
    fn read(&self) -> Result<StorageRead, String> {
        self.entered.send(std::thread::current().id()).unwrap();
        self.release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        Ok(read_roots(self.roots.clone()))
    }
}

#[test]
fn mounted_storage_navigation_does_not_wait_for_initial_filesystem_scan() {
    let files = tempfile::tempdir().unwrap();
    let cache = files.path().join("cache");
    std::fs::create_dir_all(cache.join("owned-app")).unwrap();
    std::fs::write(cache.join("owned-app/payload"), [0_u8; 24]).unwrap();
    let (entered, readers) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let gate = std::thread::spawn(move || {
        let reader = readers.recv_timeout(Duration::from_secs(5)).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        release.send(()).unwrap();
        reader
    });
    let mut app = ui::prelude::TestApp::<crate::resource::SettingsShell>::new().unwrap();
    let area = app.surface_mut::<SettingsApp>().storage_mut();
    area.model.take();
    area.device = true;
    area.reader = Some(Arc::new(HeldRead {
        roots: StorageRoots {
            app_payloads: None,
            room_data: None,
            app_cache: Some(cache),
            sync_replica: None,
        },
        entered,
        release: Mutex::new(released),
    }));
    let ui_thread = std::thread::current().id();
    let started = Instant::now();
    let address = crate::effects::open_pane_address(crate::categories::Category::Storage);
    let outcome = app
        .harness()
        .invoke_action(&address, &serde_json::Map::new(), false, None)
        .unwrap();
    let elapsed = started.elapsed();
    let reader = gate.join().unwrap();
    assert!(outcome.completed().is_some());
    assert_eq!(
        app.surface_mut::<SettingsApp>().selected(),
        crate::categories::Category::Storage
    );
    assert!(
        elapsed < Duration::from_millis(150),
        "mounted Storage navigation waited for filesystem scanning: {elapsed:?}, reader={reader:?}, UI={ui_thread:?}"
    );
    assert_ne!(
        reader, ui_thread,
        "the actual storage read must run off the UI thread"
    );
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(3), |h| {
                h.inspection_json().unwrap().contains("24 B")
            })
    );
    assert_eq!(
        app.surface_mut::<SettingsApp>()
            .storage()
            .model()
            .borrow()
            .snapshot
            .reclaimable_bytes(),
        24
    );
}

struct FailedRead(std::sync::atomic::AtomicUsize);

impl StorageReadSource for FailedRead {
    fn read(&self) -> Result<StorageRead, String> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err("owned scanner failure".into())
    }
}

#[test]
fn mounted_storage_failed_scan_is_truthful_and_never_repeats_on_relayout() {
    let mut app = ui::prelude::TestApp::<crate::resource::SettingsShell>::new().unwrap();
    let reader = Arc::new(FailedRead(std::sync::atomic::AtomicUsize::new(0)));
    let area = app.surface_mut::<SettingsApp>().storage_mut();
    area.model.take();
    area.device = true;
    area.reader = Some(reader.clone());
    let address = crate::effects::open_pane_address(crate::categories::Category::Storage);
    app.harness()
        .invoke_action(&address, &serde_json::Map::new(), false, None)
        .unwrap();
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(3), |h| {
                h.inspection_json()
                    .unwrap()
                    .contains("owned scanner failure")
            })
    );
    for theme in [
        ui::Theme::light(),
        ui::Theme::dark(),
        ui::Theme::high_contrast(),
    ] {
        app.harness().relayout_themed((440, 820), theme).unwrap();
        let view = app.harness().inspection_json().unwrap();
        assert!(view.contains("owned scanner failure"));
        assert!(!view.contains(&t("settings-storage-no-apps")));
        assert!(!view.contains(&t("settings-storage-cleanup-empty")));
        assert!(app.harness().native_app_audit().is_certified());
    }
    assert_eq!(reader.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    let refusal = app
        .effect_confirmed(&crate::effects::SettingsEffect::from(
            StorageEffect::CleanUpCaches,
        ))
        .unwrap_err();
    assert!(refusal.to_string().contains("owned scanner failure"));
}

#[test]
fn mounted_storage_pending_scan_refuses_cleanup_and_wakes_with_actual_cache() {
    let files = tempfile::tempdir().unwrap();
    let cache = files.path().join("owned-cache");
    std::fs::create_dir_all(cache.join("owned-app")).unwrap();
    std::fs::write(cache.join("owned-app/payload"), [0_u8; 24]).unwrap();
    let mut scope = ui::WakeScope::new();
    let (woke, wakes) = mpsc::channel();
    scope
        .bind(move |_| {
            let _ = woke.send(());
        })
        .unwrap();
    let _scope = scope.enter();
    let (entered, readers) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let mut app = ui::prelude::TestApp::<crate::resource::SettingsShell>::new().unwrap();
    let area = app.surface_mut::<SettingsApp>().storage_mut();
    area.model.take();
    area.device = true;
    let model = StorageModel::with_reader(
        area.cx.clone(),
        Arc::new(HeldRead {
            roots: StorageRoots {
                app_payloads: None,
                room_data: None,
                app_cache: Some(cache.clone()),
                sync_replica: None,
            },
            entered,
            release: Mutex::new(gate),
        }),
    );
    area.model
        .set(std::rc::Rc::new(std::cell::RefCell::new(model)))
        .ok()
        .unwrap();
    let address = crate::effects::open_pane_address(crate::categories::Category::Storage);
    app.harness()
        .invoke_action(&address, &serde_json::Map::new(), false, None)
        .unwrap();
    assert_ne!(
        readers.recv_timeout(Duration::from_secs(3)).unwrap(),
        std::thread::current().id()
    );
    for theme in [
        ui::Theme::light(),
        ui::Theme::dark(),
        ui::Theme::high_contrast(),
    ] {
        app.harness().relayout_themed((440, 820), theme).unwrap();
        let view = app.harness().inspection_json().unwrap();
        assert!(view.contains(&t("settings-storage-scan-loading")));
        assert!(!view.contains(&t("settings-storage-no-apps")));
        assert!(!view.contains(&t("settings-storage-cleanup-empty")));
        assert!(app.harness().native_app_audit().is_certified());
    }
    let refusal = app
        .effect_confirmed(&crate::effects::SettingsEffect::from(
            StorageEffect::CleanUpCaches,
        ))
        .unwrap_err();
    assert!(
        refusal
            .to_string()
            .contains(&t("settings-storage-scan-loading"))
    );
    assert_eq!(
        std::fs::metadata(cache.join("owned-app/payload"))
            .unwrap()
            .len(),
        24
    );
    release.send(()).unwrap();
    wakes.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(3), |h| {
                h.inspection_json().unwrap().contains("24 B")
            })
    );
    let view = app.harness().inspection_json().unwrap();
    assert!(!view.contains(&t("settings-storage-scan-loading")));
    assert!(app.harness().targets().contains_key("storage.clean"));
    assert_eq!(
        app.surface_mut::<SettingsApp>()
            .storage()
            .model()
            .borrow()
            .snapshot
            .reclaimable_bytes(),
        24
    );
}
