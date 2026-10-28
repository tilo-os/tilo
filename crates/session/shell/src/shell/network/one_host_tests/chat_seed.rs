//! A new workspace's Chat starts with its first two channels, `#chat` and
//! `#voice`: the app that makes the room's chat on its sealed channel seeds
//! them, and every member that opens the chat later reads them. It offers
//! `post` there at once, and what is posted reaches the person's other
//! device.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tilo_user_identity::DeviceCapabilities;

use super::cold_reads::{CHAT, chat_channels, open_launched, open_record_cold, until_broker_took};
use super::{Device, Person, RoomId, Wakes, hosted_workspace, scratch, until, until_or_say};

/// The windows the desktop's and the laptop's sessions open the app in.
const DESKTOP_WINDOW: u64 = 1;
const LAPTOP_WINDOW: u64 = 2;

/// The room's first channels, as the chat seeds them.
const FIRST_CHANNELS: [&str; 2] = [chat::record::CHAT_CHANNEL, chat::record::VOICE_CHANNEL];

/// An app [`drive`] started, kept open until dropped so its channel keeps
/// serving what it wrote.
pub(super) struct Running {
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

/// What one turn of a [`drive`]n app found.
pub(super) enum Step<T> {
    /// Not yet; what the app shows meanwhile.
    Waiting(String),
    Done(T),
    Failed(String),
}

/// Whether `channels` lists both first channels.
fn seeded(channels: &[String]) -> bool {
    FIRST_CHANNELS
        .iter()
        .all(|id| channels.iter().any(|have| have == id))
}

/// Run `open` (an app opening the chat, on its own thread, as it runs in
/// its own process beside the session) while `devices` go on, taking `step`
/// after each turn of the app's model until it is done or failed: what it
/// was done with, or why not (with what the app showed last). The app runs
/// on until the answer's [`Running`] is dropped.
pub(super) fn drive<T: Send + 'static>(
    devices: &[&Device],
    what: &str,
    open: impl FnOnce() -> (ui::App, ui::record::RecordOwners) + Send + 'static,
    mut step: impl FnMut(&mut ui::App, &ui::record::RecordOwners) -> Step<T> + Send + 'static,
) -> (Result<T, String>, Running) {
    let (told, heard) = std::sync::mpsc::channel();
    let done = Arc::new(AtomicBool::new(false));
    let thread = {
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            let (mut app, owners) = open();
            let mut finished = false;
            while !done.load(Ordering::Acquire) {
                app.pump_hosted_model();
                if !finished {
                    let found = step(&mut app, &owners);
                    finished = !matches!(found, Step::Waiting(_));
                    if told.send(found).is_err() {
                        finished = true;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        })
    };
    // What the app showed last, said if the wait runs out.
    let last = std::cell::RefCell::new(String::from("nothing"));
    let found = until_or_say(
        devices,
        what,
        || {
            for found in heard.try_iter() {
                match found {
                    Step::Waiting(seen) => *last.borrow_mut() = seen,
                    Step::Done(value) => return Some(Ok(value)),
                    Step::Failed(why) => return Some(Err(why)),
                }
            }
            thread.is_finished().then(|| {
                Err(format!(
                    "the app stopped, after it showed {}",
                    last.borrow()
                ))
            })
        },
        || format!("; the app showed {}", last.borrow()),
    );
    let running = Running {
        done,
        thread: Some(thread),
    };
    (found, running)
}

/// The texts of the messages in the chat's first part in `room`, as the app
/// holding `owners` reads it, once it holds that part.
fn first_part_messages(owners: &ui::record::RecordOwners, room: RoomId) -> Option<Vec<String>> {
    use tilo_resource::Resource as _;
    let mut owners = owners.clone();
    let records = owners.open(CHAT, &room.to_string()).ok()?;
    records
        .read(&chat::parts::part(0), |document| {
            chat::parts::ChatPart::read(document).ok().map(|part| {
                part.messages
                    .values()
                    .map(|message| message.text.clone())
                    .collect::<Vec<_>>()
            })
        })
        .flatten()
}

/// The published resource of `resources` (or of their children) that
/// offers `action`.
pub(super) fn offering<'a>(
    resources: &'a [tilo_actions::Resource],
    action: &str,
) -> Option<&'a tilo_actions::Resource> {
    resources.iter().find_map(|resource| {
        if resource.actions.iter().any(|spec| spec.id == action) {
            Some(resource)
        } else {
            offering(&resource.children, action)
        }
    })
}

/// Each published resource of `resources` with its actions, for a failure
/// to say what the app offered.
pub(super) fn offered(resources: &[tilo_actions::Resource]) -> Vec<String> {
    resources
        .iter()
        .flat_map(|resource| {
            std::iter::once(format!(
                "{} ({}): {:?}",
                resource.id.as_str(),
                resource.kind,
                resource
                    .actions
                    .iter()
                    .map(|spec| spec.id.as_str())
                    .collect::<Vec<_>>()
            ))
            .chain(offered(&resource.children))
        })
        .collect()
}

/// Each action the session's broker lists in `room`, by app, for a failure
/// to say what the session offered.
pub(super) fn session_listing(
    listed: &crate::shell::collab_broker::AppActionBroker,
    room: &str,
) -> Vec<String> {
    listed
        .surfaces()
        .into_iter()
        .filter(|surface| surface.route.cluster_id == room)
        .map(|surface| {
            format!(
                "{} (live {}): {:?}",
                surface.app_id,
                surface.live,
                surface
                    .actions
                    .iter()
                    .map(|action| format!("{} {}", action.resource, action.action))
                    .collect::<Vec<_>>()
            )
        })
        .collect()
}

/// A person's desktop hosting a fresh workspace, and their laptop holding
/// it: the devices, the room and the person's id.
pub(super) struct NewWorkspace {
    pub(super) desktop: Device,
    pub(super) laptop: Device,
    pub(super) room: RoomId,
    person: String,
}

/// `keys`, each in hex.
fn hex_keys(keys: &[[u8; 32]]) -> Vec<String> {
    keys.iter().map(hex::encode).collect()
}

pub(super) fn new_workspace(
    home: &mut tilo_test_env::TestHome,
    dir: &std::path::Path,
    seed: u8,
    ports: u16,
) -> NewWorkspace {
    let wakes = Arc::new(Wakes::default());
    let ada = Person::new(seed);
    home.env().set("TILO_USER_ID", ada.id());
    // Ada set the desktop up from another of her devices, as she sets up
    // any device but her first: it admits no devices itself.
    let desktop = Device::start(
        &ada,
        "Ada's desktop",
        &dir.join("desktop"),
        ports,
        DeviceCapabilities {
            can_admit_devices: false,
            ..DeviceCapabilities::primary()
        },
        &wakes,
    );
    let laptop = Device::start(
        &ada,
        "Ada's laptop",
        &dir.join("laptop"),
        ports
            .checked_add(1)
            .expect("the laptop has a port after the desktop's"),
        DeviceCapabilities::member(),
        &wakes,
    );
    let (_, room) = hosted_workspace(&desktop, &[&laptop], "Projects");
    laptop.reach(&desktop);
    until_or_say(
        &[&desktop, &laptop],
        "the laptop to hold the workspace",
        || laptop.holds(room).then_some(()),
        || {
            format!(
                "; the desktop {} counts as Ada's devices {:?}, the laptop {} counts {:?}",
                hex::encode(desktop.cert().device_public_key),
                hex_keys(&desktop.net().personal_device_keys()),
                hex::encode(laptop.cert().device_public_key),
                hex_keys(&laptop.net().personal_device_keys()),
            )
        },
    );
    // The desktop keeps its identity on this machine as such a device
    // does, so its session's broker signs what it admits as the desktop.
    // One process runs both devices and the identity tree is the
    // process's, so it names the desktop only once the laptop holds the
    // workspace: Ada's devices met each other as their sessions do.
    home.env()
        .set("TILO_IDENTITY_ROOT", dir.join("desktop"))
        .set(tilo_identity_host::provisioning::FILE_SECRETS_ENV, "1");
    desktop.keep_identity_on_disk(
        &runtime_identity::identity_dir_from_env().expect("the identity directory"),
    );
    NewWorkspace {
        desktop,
        laptop,
        room,
        person: hex::encode(ada.user()),
    }
}

/// A scratch directory the brokers may bind in: owner-only.
pub(super) fn broker_scratch() -> tempfile::TempDir {
    let dir = scratch();
    std::fs::set_permissions(
        dir.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    dir
}

impl NewWorkspace {
    /// The desktop's Chat in the new workspace, launched as the session
    /// launches a window of it: nothing of the room's chat exists yet, so
    /// the app makes it. Its broker, then the step `step` makes from the
    /// actions the broker lists [`drive`]n on it.
    fn launch_on_desktop<T: Send + 'static, S>(
        &mut self,
        dir: &std::path::Path,
        what: &str,
        step: impl FnOnce(crate::shell::collab_broker::AppActionBroker) -> S,
    ) -> (
        crate::shell::collab_broker::BrokerHandle,
        Result<T, String>,
        Running,
    )
    where
        S: FnMut(&mut ui::App, &ui::record::RecordOwners) -> Step<T> + Send + 'static,
    {
        self.launch_app_on_desktop(dir, CHAT, chat::definition(), what, step)
    }

    /// The desktop's `app` of `definition` in the new workspace, launched
    /// as the session launches a window of it: its broker, then the step
    /// `step` makes from the actions the broker lists [`drive`]n on it.
    pub(super) fn launch_app_on_desktop<T: Send + 'static, S>(
        &mut self,
        dir: &std::path::Path,
        app: &'static str,
        definition: &'static ui::application::ApplicationDefinition,
        what: &str,
        step: impl FnOnce(crate::shell::collab_broker::AppActionBroker) -> S,
    ) -> (
        crate::shell::collab_broker::BrokerHandle,
        Result<T, String>,
        Running,
    )
    where
        S: FnMut(&mut ui::App, &ui::record::RecordOwners) -> Step<T> + Send + 'static,
    {
        self.desktop.open_app_first(self.room, app);
        let socket = dir.join("desktop-collab.sock");
        let broker = self.desktop.start_broker(&socket);
        let step = step(
            broker
                .app_action_broker()
                .expect("the session's broker lists its apps' actions"),
        );
        let launch = self
            .desktop
            .app_window_launch(&socket, app, self.room, DESKTOP_WINDOW);
        let profile = dir.join(format!("desktop-{app}"));
        let (room, person) = (self.room, self.person.clone());
        let (found, running) = drive(
            &[&self.desktop, &self.laptop],
            what,
            move || open_launched(&profile, definition, room, &person, launch.connect()),
            step,
        );
        (broker, found, running)
    }

    /// The laptop's Chat, opened cold through its own broker once the
    /// desktop's writes reached the room's host: its broker, then `step`
    /// [`drive`]n on it.
    fn open_on_laptop<T: Send + 'static>(
        &mut self,
        dir: &std::path::Path,
        what: &str,
        step: impl FnMut(&mut ui::App, &ui::record::RecordOwners) -> Step<T> + Send + 'static,
    ) -> (
        crate::shell::collab_broker::BrokerHandle,
        Result<T, String>,
        Running,
    ) {
        let primary =
            ui::record::RecordCheckout::package_primary(chat::definition().record_kind.clone());
        self.open_app_on_laptop(dir, CHAT, chat::definition(), primary, what, step)
    }

    /// The laptop's `app` of `definition` on the record `checkout` names,
    /// opened cold through its own broker once the desktop's writes reached
    /// the room's host: its broker, then `step` [`drive`]n on it.
    pub(super) fn open_app_on_laptop<T: Send + 'static>(
        &mut self,
        dir: &std::path::Path,
        app: &'static str,
        definition: &'static ui::application::ApplicationDefinition,
        checkout: ui::record::RecordCheckout,
        what: &str,
        step: impl FnMut(&mut ui::App, &ui::record::RecordOwners) -> Step<T> + Send + 'static,
    ) -> (
        crate::shell::collab_broker::BrokerHandle,
        Result<T, String>,
        Running,
    ) {
        let socket = dir.join("laptop-collab.sock");
        let broker = self.laptop.start_broker(&socket);
        until_broker_took(&[&self.desktop, &self.laptop], &self.laptop, app);
        let launch = self
            .laptop
            .app_window_launch(&socket, app, self.room, LAPTOP_WINDOW)
            .on_record(&checkout.id);
        let profile = dir.join(format!("laptop-{app}"));
        let (room, person) = (self.room, self.person.clone());
        let (found, running) = drive(
            &[&self.desktop, &self.laptop],
            what,
            move || {
                open_record_cold(
                    &profile,
                    definition,
                    room,
                    &person,
                    launch.connect(),
                    checkout,
                )
            },
            step,
        );
        (broker, found, running)
    }
}

/// [`Step`] for the chat listing both first channels.
fn listing_first_channels(
    room: RoomId,
) -> impl FnMut(&mut ui::App, &ui::record::RecordOwners) -> Step<Vec<String>> {
    move |_, owners| match chat_channels(owners, room) {
        Some(channels) if seeded(&channels) => Step::Done(channels),
        channels => Step::Waiting(format!("the channels {channels:?}")),
    }
}

#[test]
#[serial_test::serial]
fn a_new_workspaces_chat_lists_chat_and_voice_on_the_device_that_made_it_and_on_another() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = broker_scratch();
    let mut workspace = new_workspace(&mut home, dir.path(), 85, 0x0A71);
    let room = workspace.room;

    let (desktop_broker, made, desktop_app) = workspace.launch_on_desktop(
        dir.path(),
        "the new chat's first channels in the app that made it",
        |_| listing_first_channels(room),
    );
    let made = made.unwrap_or_else(|failure| {
        panic!("the chat the desktop made has no #chat and #voice: {failure}")
    });
    assert!(
        seeded(&made),
        "the desktop's chat lists #chat and #voice, found {made:?}"
    );
    until(
        &[&workspace.desktop, &workspace.laptop],
        "the new chat's writes on the room's host",
        || (workspace.desktop.retained_writes(room, CHAT) >= 1).then_some(()),
    );

    // The laptop opens the chat afterwards: it reads the channels the
    // desktop's app seeded.
    let (laptop_broker, read, laptop_app) = workspace.open_on_laptop(
        dir.path(),
        "the chat's first channels on the laptop",
        listing_first_channels(room),
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
    drop(workspace);
}

#[test]
#[serial_test::serial]
fn a_message_posted_in_a_new_workspaces_chat_reaches_the_persons_other_device() {
    const TEXT: &str = "The first message of the new workspace";
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = broker_scratch();
    let mut workspace = new_workspace(&mut home, dir.path(), 86, 0x0A73);
    let room = workspace.room;

    // The desktop's new Chat offers `post` once it lists #chat, and the
    // message posted through it is in the chat's messages.
    // How the post started, once it was invoked.
    let mut posted: Option<String> = None;
    let (desktop_broker, sent, desktop_app) = workspace.launch_on_desktop(
        dir.path(),
        "the message posted in the new chat",
        move |listed| {
            move |app, owners| {
                let channels = chat_channels(owners, room);
                if !channels.as_deref().is_some_and(seeded) {
                    return Step::Waiting(format!("the channels {channels:?}"));
                }
                if posted.is_none() {
                    let resources = app.collect_resources();
                    let Some(post) = offering(&resources, "post") else {
                        return Step::Waiting(format!(
                            "no post offered; the app offers {:?}",
                            offered(&resources)
                        ));
                    };
                    // The session lists it among Chat's actions in the room,
                    // where a person or an agent finds it.
                    let in_room = room.to_string();
                    let session_lists = listed.surfaces().into_iter().any(|surface| {
                        surface.route.cluster_id == in_room
                            && surface.actions.iter().any(|action| action.action == "post")
                    });
                    if !session_lists {
                        return Step::Waiting(format!(
                            "the app offers post on {}, but the session lists {:?}",
                            post.id.as_str(),
                            session_listing(&listed, &in_room)
                        ));
                    }
                    let address = tilo_actions::ActionAddress::new(post.id.as_str(), "post");
                    let arguments = serde_json::json!({
                        "channel": chat::record::CHAT_CHANNEL,
                        "text": TEXT,
                    });
                    match app.invoke_local_resource_action(
                        &address,
                        arguments.as_object().expect("the arguments are an object"),
                    ) {
                        Ok(started) => {
                            posted =
                                Some(format!("post on {} started {started:?}", post.id.as_str()));
                        }
                        Err(refused) => {
                            return Step::Failed(format!(
                                "post on {} was refused: {refused:?}",
                                post.id.as_str()
                            ));
                        }
                    }
                }
                if let Some(refused) = app.controlled_refused_action_for_test() {
                    return Step::Failed(format!(
                        "{}; the session's action host refused it: {refused}",
                        posted.as_deref().unwrap_or_default()
                    ));
                }
                match first_part_messages(owners, room) {
                    Some(messages) if messages.iter().any(|text| text == TEXT) => {
                        Step::Done(messages)
                    }
                    messages => Step::Waiting(format!(
                        "{}; the messages {messages:?}",
                        posted.as_deref().unwrap_or_default()
                    )),
                }
            }
        },
    );
    let sent = sent.unwrap_or_else(|failure| {
        panic!("the desktop's new chat did not post a message: {failure}")
    });
    assert!(
        sent.iter().any(|text| text == TEXT),
        "the desktop's chat holds the message, found {sent:?}"
    );

    // The laptop opens the chat afterwards and reads the message.
    let (laptop_broker, read, laptop_app) = workspace.open_on_laptop(
        dir.path(),
        "the posted message on the laptop",
        move |_, owners| match first_part_messages(owners, room) {
            Some(messages) if messages.iter().any(|text| text == TEXT) => Step::Done(messages),
            messages => Step::Waiting(format!("the messages {messages:?}")),
        },
    );
    drop((laptop_app, desktop_app));
    let read = read.unwrap_or_else(|failure| {
        panic!("the laptop does not read the message posted on the desktop: {failure}")
    });
    assert!(
        read.iter().any(|text| text == TEXT),
        "the laptop's chat holds the message, found {read:?}"
    );
    drop((laptop_broker, desktop_broker));
    drop(workspace);
}
