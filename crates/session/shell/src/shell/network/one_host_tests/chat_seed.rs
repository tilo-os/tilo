//! A new workspace's Chat starts with its first two channels, `#chat` and
//! `#voice`: the app that makes the room's chat on its sealed channel seeds
//! them, and every member that opens the chat later reads them.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tilo_user_identity::DeviceCapabilities;

use super::cold_reads::{CHAT, chat_channels, open_cold, open_launched, until_broker_took};
use super::{Device, Person, RoomId, Wakes, hosted_workspace, scratch, until};

/// The room's first channels, as the chat seeds them.
const FIRST_CHANNELS: [&str; 2] = [chat::record::CHAT_CHANNEL, chat::record::VOICE_CHANNEL];

/// An app [`channels_shown`] started, kept open until dropped so its
/// channel keeps serving what it wrote.
struct Running {
    done: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Whether `channels` lists both first channels.
fn seeded(channels: &[String]) -> bool {
    FIRST_CHANNELS
        .iter()
        .all(|id| channels.iter().any(|have| have == id))
}

/// Run `open` (an app opening the chat, on its own thread, as it runs in
/// its own process beside the session) while `devices` go on, until the
/// chat it holds lists both first channels: those channels, or what it
/// listed last when it stopped first. The app runs on until the answer's
/// [`Running`] is dropped.
fn channels_shown(
    devices: &[&Device],
    room: RoomId,
    what: &str,
    open: impl FnOnce() -> (ui::App, ui::record::RecordOwners) + Send + 'static,
) -> (Result<Vec<String>, String>, Running) {
    let (shown, seen) = std::sync::mpsc::channel();
    let done = Arc::new(AtomicBool::new(false));
    let thread = {
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            let (mut app, owners) = open();
            let mut reported = false;
            while !done.load(Ordering::Acquire) {
                app.pump_hosted_model();
                if !reported {
                    let channels = chat_channels(&owners, room);
                    reported = channels.as_deref().is_some_and(seeded);
                    if shown.send(channels).is_err() {
                        reported = true;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        })
    };
    let mut last = None;
    let found = until(devices, what, || {
        for channels in seen.try_iter() {
            if let Some(channels) = channels.as_ref().filter(|channels| seeded(channels)) {
                return Some(Some(channels.clone()));
            }
            last = Some(channels);
        }
        thread.is_finished().then_some(None)
    });
    let running = Running {
        done,
        thread: Some(thread),
    };
    let result = found.ok_or_else(|| {
        format!("the app stopped before it listed both first channels, after it listed {last:?}")
    });
    (result, running)
}

#[test]
#[serial_test::serial]
fn a_new_workspaces_chat_lists_chat_and_voice_on_the_device_that_made_it_and_on_another() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    // The brokers bind only inside an owner-only directory.
    std::fs::set_permissions(
        dir.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    let wakes = Arc::new(Wakes::default());
    let ada = Person::new(85);
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
    let person = hex::encode(ada.user());

    // The desktop opens Chat in the new workspace, as a window the session
    // launches: nothing of the room's chat exists yet, so the app makes it.
    desktop.open_app_first(room, CHAT);
    let desktop_socket = dir.path().join("desktop-collab.sock");
    let desktop_broker = desktop.start_broker(&desktop_socket);
    let launch = desktop.app_launch(&desktop_socket, CHAT, room);
    let profile = dir.path().join("desktop-chat");
    let (made, desktop_app) = {
        let person = person.clone();
        channels_shown(
            &[&desktop, &laptop],
            room,
            "the new chat's first channels in the app that made it",
            move || {
                open_launched(
                    &profile,
                    chat::definition(),
                    room,
                    &person,
                    launch.connect(),
                )
            },
        )
    };
    let made = made.unwrap_or_else(|failure| {
        panic!("the chat the desktop made has no #chat and #voice: {failure}")
    });
    assert!(
        seeded(&made),
        "the desktop's chat lists #chat and #voice, found {made:?}"
    );
    until(
        &[&desktop, &laptop],
        "the new chat's writes on the room's host",
        || (desktop.retained_writes(room, CHAT) >= 1).then_some(()),
    );

    // The laptop opens the chat afterwards, cold, through its own broker:
    // it reads the channels the desktop's app seeded.
    let laptop_socket = dir.path().join("laptop-collab.sock");
    let laptop_broker = laptop.start_broker(&laptop_socket);
    until_broker_took(&[&desktop, &laptop], &laptop, CHAT);
    let launch = laptop.app_launch(&laptop_socket, CHAT, room);
    let profile = dir.path().join("laptop-chat");
    let (read, laptop_app) = channels_shown(
        &[&desktop, &laptop],
        room,
        "the chat's first channels on the laptop",
        move || {
            open_cold(
                &profile,
                chat::definition(),
                room,
                &person,
                launch.connect(),
            )
        },
    );
    drop((laptop_app, desktop_app));
    let read = read.unwrap_or_else(|failure| {
        panic!("the laptop does not read the chat's #chat and #voice: {failure}")
    });
    assert!(
        seeded(&read),
        "the laptop's chat lists #chat and #voice, found {read:?}"
    );
    drop((laptop_broker, desktop_broker));
    drop((desktop, laptop));
}
