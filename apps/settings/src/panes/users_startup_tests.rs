// Mounted all-pane publication crosses the real per-account property reader.
#[test]
fn mounted_settings_progresses_while_accounts_service_property_is_held() {
    use std::sync::{Condvar, mpsc};
    struct HeldRead {
        entered: mpsc::Sender<()>,
        finished: mpsc::Sender<bool>,
        gate: Arc<(Mutex<bool>, Condvar)>,
    }
    impl UsersRead for HeldRead {
        fn read(&self) -> Result<Vec<DeviceUser>, String> {
            let mut users = read_passwd_users(
                "fixture:x:1001:1001:Fixture Account:/fixture:/bin/bash",
                None,
                &HashSet::new(),
            );
            read_automatic_login(&mut users, &|_| {
                self.entered.send(()).unwrap();
                let (lock, ready) = self.gate.as_ref();
                let (released, timeout) = ready
                    .wait_timeout_while(
                        lock.lock().unwrap(),
                        std::time::Duration::from_secs(5),
                        |released| !*released,
                    )
                    .unwrap();
                let completed = *released && !timeout.timed_out();
                drop(released);
                if completed {
                    Ok(true)
                } else {
                    Err("owned read was not released".into())
                }
            });
            self.finished
                .send(users[0].automatic_login_error.is_none())
                .unwrap();
            Ok(users)
        }
    }
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let (entered, properties) = mpsc::channel();
    let (finished, completion) = mpsc::channel();
    let (done, frames) = mpsc::channel();
    let reader = Arc::new(HeldRead {
        entered,
        finished,
        gate: gate.clone(),
    });
    let ui = std::thread::spawn(move || {
        let mut app = ui::prelude::TestApp::<crate::resource::SettingsShell>::new().unwrap();
        let area = app.surface_mut::<SettingsApp>().users_mut();
        area.model.take();
        area.reader = reader;
        area.device = true;
        app.harness().relayout();
        let operations = app.actions().unwrap();
        done.send(
            operations
                .iter()
                .any(|row| row.address.action == crate::effects::NavigationEffect::OPEN_PANE.id),
        )
        .unwrap();
    });
    properties
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let progressed = frames
        .recv_timeout(std::time::Duration::from_millis(150))
        .ok();
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    ui.join().unwrap();
    assert!(
        completion
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap(),
        "owned property read must finish after release"
    );
    assert_eq!(
        progressed,
        Some(true),
        "mounted Settings must publish usable navigation before AccountsService responds"
    );
}

type AccountResult = Result<Vec<DeviceUser>, String>;
type AccountReply = std::sync::mpsc::Sender<AccountResult>;
struct ControlledUsersRead {
    calls: std::sync::mpsc::Sender<AccountReply>,
}
impl UsersRead for ControlledUsersRead {
    fn read(&self) -> Result<Vec<DeviceUser>, String> {
        let (done, result) = std::sync::mpsc::channel();
        self.calls.send(done).unwrap();
        result
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
    }
}
fn controlled_users_area() -> (
    UsersArea,
    std::sync::mpsc::Receiver<AccountReply>,
    tempfile::TempDir,
) {
    let profile = tempfile::tempdir().unwrap();
    let (calls, reads) = std::sync::mpsc::channel();
    let area = UsersArea {
        cx: PaneCx::in_file(profile.path().join("settings.json")),
        model: OnceCell::new(),
        reader: Arc::new(ControlledUsersRead { calls }),
        device: true,
    };
    (area, reads, profile)
}
fn fixture_accounts() -> Vec<DeviceUser> {
    let mut users = read_passwd_users(
        "fixture:x:1001:1001:Fixture Account:/fixture:/bin/bash",
        None,
        &HashSet::new(),
    );
    read_automatic_login(&mut users, &|_| Ok(true));
    users
}

#[test]
fn users_read_wakes_owner_and_publishes_observed_accounts() {
    let mut scope = ui::WakeScope::new();
    let (woke, wakes) = std::sync::mpsc::channel();
    scope
        .bind(move |_| {
            let _ = woke.send(());
        })
        .unwrap();
    let _entry = scope.enter();
    let (area, reads, _profile) = controlled_users_area();
    assert!(area.rows().is_empty());
    let done = reads
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    let text = crate::widgets::announced_text(area.model().borrow().build_composition().as_ref());
    let loading = text.contains(&t("settings-users-loading-accounts"));
    done.send(Ok(fixture_accounts())).unwrap();
    wakes
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    let rows = area.rows();
    assert!(
        loading,
        "pending read must render loading rather than no accounts"
    );
    assert_eq!(rows.len(), 1);
    let resource = rows.into_iter().next().unwrap().into_resource();
    let autologin = SettingsEffect::from(UsersEffect::SetAutomaticLogin {
        user: "fixture".into(),
        on: false,
    })
    .bound();
    assert!(
        resource.actions.iter().any(|action| {
            action.id == autologin.id
                && action.build_json(&serde_json::Map::new()).unwrap()
                    == autologin.build_json(&serde_json::Map::new()).unwrap()
        }),
        "published toggle uses observed state"
    );
    let model = area.model().borrow();
    assert_eq!(model.users[0].username, "fixture");
    assert!(model.users[0].automatic_login);
    assert!(model.reading.is_none());
    assert!(model.users_error.is_none());
}

#[test]
fn users_mutation_refresh_discards_older_read_and_preserves_failure() {
    let mut scope = ui::WakeScope::new();
    let (woke, wakes) = std::sync::mpsc::channel();
    scope
        .bind(move |_| {
            let _ = woke.send(());
        })
        .unwrap();
    let _entry = scope.enter();
    let (area, reads, _profile) = controlled_users_area();
    assert!(area.rows().is_empty());
    let first = reads
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    area.model().borrow_mut().reload();
    area.model().borrow_mut().reload();
    assert!(reads.try_recv().is_err(), "one owned read may be in flight");
    first.send(Ok(fixture_accounts())).unwrap();
    wakes
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    assert!(
        area.rows().is_empty(),
        "older read cannot publish after mutation"
    );
    let latest = reads
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    let mut accounts = fixture_accounts();
    accounts[0].automatic_login = false;
    latest.send(Ok(accounts)).unwrap();
    wakes
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    assert_eq!(area.rows().len(), 1);
    assert!(!area.model().borrow().users[0].automatic_login);
    area.model().borrow_mut().reload();
    let failed = reads
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    failed
        .send(Err("AccountsService unavailable".into()))
        .unwrap();
    wakes
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    assert!(
        area.rows().is_empty(),
        "failed read offers no guessed account actions"
    );
    let model = area.model().borrow();
    assert_eq!(model.users.len(), 1, "last observed data is retained");
    assert_eq!(
        model.users_error.as_deref(),
        Some("AccountsService unavailable")
    );
    let text = crate::widgets::announced_text(model.build_composition().as_ref());
    assert!(
        text.iter()
            .any(|line| line.contains("AccountsService unavailable"))
    );
    assert!(!text.contains(&t("settings-users-empty-accounts")));
}
