//! The workspace's emoji: Chat keeps the record, and a Code window on each
//! of two devices reads it and adds to it through its own launch. Both
//! windows converge on the one record, Chat's, which travels on Chat's
//! sealed route through the room's host.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

use tilo_app::framework::WorkspaceRecord;
use tilo_app::tilo_workspace_emoji::{self as emoji, WorkspaceEmoji, workspace_emoji as verbs};
use tilo_user_identity::DeviceCapabilities;
use ui::permissions::UserId;
use ui::record::workspace::WorkspaceRecords;

use super::cold_reads::{AppLaunch, open_launched};
use super::{Device, Person, RoomId, Wakes, hosted_workspace, scratch, until};

/// The app whose windows add to the workspace's emoji.
const CODE: &str = "so.tilo.code-editor";

fn record_id() -> String {
    collab_types::workspace_record_id(emoji::KIND)
}

/// What a Code window showed of the workspace's emoji on its last turn.
#[derive(Clone, Debug, Default)]
struct Shown {
    /// The emoji's names, once the window holds the record.
    names: Option<Vec<String>>,
    /// The record's route as the window holds it (`None` until it has one).
    route: Option<serde_json::Value>,
}

impl Shown {
    /// The route joined, its committed read applied, and no join refused.
    fn joined(&self) -> bool {
        self.route.as_ref().is_some_and(|phase| {
            phase["committed_read_ready"] == true && phase["join_failure"].is_null()
        })
    }
}

/// A Code window on its own thread, as it runs in its own process beside
/// its session: it holds the workspace's emoji record and adds the names it
/// is told to.
struct CodeWindow {
    add: mpsc::Sender<String>,
    seen: mpsc::Receiver<Shown>,
    last: Shown,
    done: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl CodeWindow {
    fn open(launch: AppLaunch, profile: std::path::PathBuf, room: RoomId, person: String) -> Self {
        let (asked, told) = mpsc::channel::<String>();
        let (shown, seen) = mpsc::channel();
        let done = Arc::new(AtomicBool::new(false));
        let thread = {
            let done = Arc::clone(&done);
            std::thread::spawn(move || {
                let (mut app, owners) = open_launched(
                    &profile,
                    code_editor::definition(),
                    room,
                    &person,
                    launch.connect(),
                );
                // The records every app of the room opens, where the window
                // opens them: beside its own, in the same owners.
                let workspace = WorkspaceRecords::in_room(owners, room.to_string(), UserId(person));
                let mut record = None;
                let mut waiting = Vec::new();
                while !done.load(Ordering::Acquire) {
                    app.pump_hosted_model();
                    if record.is_none() {
                        record = WorkspaceRecord::<WorkspaceEmoji>::open(
                            &workspace,
                            emoji::OWNER,
                            (CODE, "Code"),
                            None,
                            None,
                        );
                    }
                    waiting.extend(told.try_iter());
                    if let Some(record) = &record {
                        for name in std::mem::take(&mut waiting) {
                            add(record, &name);
                        }
                    }
                    let route = app
                        .controlled_document_merge_phase_for_test()
                        .into_iter()
                        .find(|phase| {
                            phase["app"] == emoji::OWNER && phase["record"] == record_id().as_str()
                        });
                    let names = record.as_ref().map(|record| {
                        record
                            .get()
                            .names()
                            .into_iter()
                            .map(str::to_owned)
                            .collect()
                    });
                    if shown.send(Shown { names, route }).is_err() {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            })
        };
        Self {
            add: asked,
            seen,
            last: Shown::default(),
            done,
            thread: Some(thread),
        }
    }

    /// What the window showed last.
    fn shown(&mut self) -> &Shown {
        if let Some(last) = self.seen.try_iter().last() {
            self.last = last;
        }
        &self.last
    }

    fn add(&self, name: &str) {
        self.add.send(name.to_owned()).expect("the window is open");
    }

    fn running(&self) -> bool {
        self.thread
            .as_ref()
            .is_some_and(|thread| !thread.is_finished())
    }
}

impl Drop for CodeWindow {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn add(record: &WorkspaceRecord<WorkspaceEmoji>, name: &str) {
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend_from_slice(&[0; 64]);
    record
        .invoke(&WorkspaceEmoji::ADD_EMOJI.with(verbs::AddEmoji {
            name: name.into(),
            image: emoji::EmojiImage(png).to_base64(),
        }))
        .expect("an editor adds an emoji from any app's window");
}

/// Run both devices until both windows hold the emoji `expected` and have
/// joined the record's route.
fn converge(devices: &[&Device], windows: &mut [CodeWindow], expected: &[&str]) {
    until(
        devices,
        &format!("both Code windows to converge on the workspace's emoji {expected:?}"),
        || {
            for window in windows.iter() {
                assert!(window.running(), "a Code window stopped");
            }
            windows
                .iter_mut()
                .all(|window| {
                    let shown = window.shown();
                    shown.joined()
                        && shown
                            .names
                            .as_deref()
                            .is_some_and(|names| names == expected)
                })
                .then_some(())
        },
    );
}

#[test]
#[serial_test::serial]
fn code_windows_on_two_devices_converge_on_the_workspace_emoji() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    // The brokers bind only inside an owner-only directory.
    std::fs::set_permissions(
        dir.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    let wakes = Arc::new(Wakes::default());
    let ada = Person::new(84);
    home.env().set("TILO_USER_ID", ada.id());
    let mut desktop = Device::start(
        &ada,
        "Ada's desktop",
        &dir.path().join("desktop"),
        0x0A71,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let mut laptop = Device::start(
        &ada,
        "Ada's laptop",
        &dir.path().join("laptop"),
        0x0A72,
        DeviceCapabilities::member(),
        &wakes,
    );
    let (_, room) = hosted_workspace(&desktop, &[&laptop], "Projects");
    laptop.reach(&desktop);
    until(
        &[&desktop, &laptop],
        "the laptop to hold the workspace",
        || laptop.holds(room).then_some(()),
    );
    // Code keeps its own record in the room, and Chat the workspace's emoji.
    desktop.open_app_first(room, CODE);
    desktop.open_app_first(room, emoji::OWNER);

    // A Code window on each device, launched through that device's broker.
    let person = hex::encode(ada.user());
    let desktop_broker = desktop.start_broker(&dir.path().join("desktop-collab.sock"));
    let laptop_broker = laptop.start_broker(&dir.path().join("laptop-collab.sock"));
    let mut windows = [
        CodeWindow::open(
            desktop.app_launch(&dir.path().join("desktop-collab.sock"), CODE, room),
            dir.path().join("desktop-code"),
            room,
            person.clone(),
        ),
        CodeWindow::open(
            laptop.app_launch(&dir.path().join("laptop-collab.sock"), CODE, room),
            dir.path().join("laptop-code"),
            room,
            person,
        ),
    ];
    let devices = [&desktop, &laptop];

    // Each window joins Chat's record through its own launch, as Code.
    converge(&devices, &mut windows, &[]);
    windows[0].add("party");
    converge(&devices, &mut windows, &["party"]);
    windows[1].add("ship_it");
    converge(&devices, &mut windows, &["party", "ship_it"]);

    drop(windows);
    drop((desktop_broker, laptop_broker));
    drop((desktop, laptop));
}
