//! The native session's own assembly: the compositor half of [`crate::platform`]
//! composing the desktop and the services a Linux login runs.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use tilo_actions::remote::InvokeArgument;
use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_compositor::{
    Compositor, DisplayConfig, Error, LoopTurns, NestedConfig, OutputConfig, WindowPolicy,
};

use tilo_desktop::osd::{Hint, Osd, OsdKind};
use tilo_desktop::overlays::{Overlays, Slot};
use tilo_desktop_surface::DesktopHandle;
use tilo_liveness::watchdog::{Policy, Watchdog, end_process};
use tilo_lock::MutexRecover as _;
use tilo_session_core::door::Doors;
use tilo_surface_host::{DesktopHost, WidgetHost};
use ui::host_operation::ResourceOperationStart;

use crate::assembly::{Session, Shared};

pub use crate::automation::FinishedShortcutRun;
pub use crate::bars::BarShown;
pub use crate::channels::ChannelLink;
pub use crate::clock_wait::ClockWait;
pub use crate::config::SessionConfig;
pub use crate::controls::{
    CLIPBOARD_RESOURCE, INPUT_RESOURCE, MAGNIFIER_RESOURCE, Magnify, NIGHT_LIGHT_RESOURCE,
    SESSION_RESOURCE, SessionCommand,
};
pub use crate::devices::RESOURCE as DEVICES_RESOURCE;
pub use crate::engine::Person;
pub use crate::env::SessionEnvironment;
pub use crate::keys::{Invocation, QUICK_SETTINGS};
pub use crate::launches::Launches;
pub use crate::lists::{AppLists, Lists};
pub use crate::notifications::RESOURCE as NOTIFICATIONS_RESOURCE;
use crate::options::start_collaboration;
pub use crate::options::{ActionPlane, Machine, Options, no_session_actions};
pub use crate::platform::keys::{KEYBOARD_BACKLIGHT, MEDIA, MICROPHONE};
pub use crate::records::Records;
pub use crate::rotation::Accelerometer;
pub use crate::run::{Backend, Reach};
pub use crate::search::{AppActions, SearchOptions};
pub use crate::setup::BeforeDesk;
pub use crate::vpn::Vpn;
pub use crate::wake::{Seams as WakeSeams, WakeInterfaces};
pub use tilo_service::{Service, Services};

use crate::doors::plane::{mount_services, start_action_plane};
use crate::notifications::mount_banners;
use crate::panels::mount::mount_activities;
use crate::panels::mount::{KeyContext, mount_controls, mount_setup};
use crate::plane::Plane;
use crate::search::SessionSearch;
use tilo_app_launch::Doors as WindowDoors;

pub use os_actions::SessionAction;

pub use tilo_onboarding_host::view::OnboardingView;

/// The generation the session's own desktop snapshots carry: a snapshot of
/// an older one is stale.
pub(crate) const GENERATION: &str = "session";

/// How many times the session starts the lock screen's app for one lock
/// before it draws the lock itself: a screen whose app goes is started
/// again, and one that keeps going is not staying up at all. The lock stays,
/// and the session's own view of it takes over, because a lock is never
/// lifted for anything but the right password.
pub(crate) const LOCK_SCREEN_TRIES: u32 = 3;

/// The lock screen's passwords being checked, off the loop.
pub(crate) struct Unlocking {
    pub(crate) user: String,
    pub(crate) verifier: std::sync::Arc<dyn crate::unlock::PasswordVerifier>,
    pub(crate) verdicts: (
        std::sync::mpsc::Sender<crate::unlock::Verdict>,
        std::sync::mpsc::Receiver<crate::unlock::Verdict>,
    ),
    /// Wakes the loop when a verdict lands.
    pub(crate) wake: std::task::Waker,
}

/// The compositor failing is this session's host failing.
impl From<Error> for crate::Error {
    fn from(error: Error) -> Self {
        Self::host(error)
    }
}

impl Session {
    /// A session on the compositor `start` makes with the desktop's window
    /// policy.
    ///
    /// # Errors
    /// The compositor did not start.
    #[expect(
        clippy::too_many_lines,
        reason = "the session is wired in one pass, where every part sees the others"
    )]
    pub fn new(
        mut options: Options,
        start: impl FnOnce(Box<dyn WindowPolicy>) -> Result<Compositor, Error>,
    ) -> Result<Self, crate::Error> {
        tilo_startup_metrics::mark("session-build-start");
        // The words a notice or a query resolves later, in the person's
        // language, are read now: the loop that shows them reads no file.
        crate::i18n::ensure();
        tilo_startup_metrics::mark("session-i18n-done");
        // Startup frees its scratch at no single moment: one trim collects it
        // once startup is over, and the log says where the heap stood at the
        // start.
        tilo_compositor::heap_trim::log_milestone("start");
        tilo_compositor::heap_trim::request_after(tilo_compositor::heap_trim::STARTUP_SETTLED);
        // Who sits here, as the one id the session acts as: the person their
        // workspaces' governance names, which this device's certificate
        // carries. Everything the person does here — the workspaces they
        // own, what the desk records, what the sheets change — is theirs.
        let seat = crate::identity::seat(&options);
        options.earlier.clone_from(&seat.earlier);
        options.person = seat.person;
        // The ids this device mints are its own, as every workspace derives
        // them from its key: a computer with an identity mints under that
        // rather than the seed the caller passed.
        if let Some(salt) = seat.salt {
            options.salt = salt;
        }
        let sources = crate::config::Sources::from_config(&options.config);
        let mut doors = Doors::default();
        // The device VPN runs for the session's lifetime: the profiles' kill
        // switches from the start, the route through another of the
        // person's devices as the session's passes find this device's
        // endpoint, and this device as an exit for their other devices. What
        // all of it does is published for Settings, and the exit is served
        // on the endpoint the session binds next.
        let beside = options
            .vpn
            .as_ref()
            .map(crate::vpn::Vpn::served)
            .unwrap_or_default();
        let vpn = options.vpn.take().map(crate::vpn::Vpn::start);
        // What the desk and the chrome ask of the session, from their own
        // widget trees.
        let notices = tilo_desktop_surface::DesktopNotifications::default();
        // The centre opens with what it kept when this session last ended.
        let notice_history = crate::notifications::restore_history(&notices);
        // What the bar reads of the notifications, and the policy arrivals
        // are judged by: the person may change it at any time.
        let notice_signals = Rc::new(crate::notifications::NoticeSignals::default());
        let notification_policy = std::sync::Arc::new(std::sync::Mutex::new(
            settings_store::notification_policy::NotificationPolicy::load(),
        ));
        // The notifications the banners app shows and the desk keeps, and the link it asks back on.
        let banners = tilo_notification_banners::Link::new();
        // The person's notifications: what their apps sent the notifications service, and what the
        // session's roles tell them through it.
        let notifications = Rc::new(RefCell::new(
            crate::notifications::SessionNotifications::new(
                options
                    .desktop
                    .as_ref()
                    .map_or_else(tilo_notifications_service::Notifications::new, |desktop| {
                        desktop.notifications().clone()
                    }),
                notices.clone(),
                banners.clone(),
                std::sync::Arc::clone(&notification_policy),
                Rc::clone(&notice_signals),
                options.power.clone(),
                notice_history,
            ),
        ));
        let watched = std::sync::Arc::clone(&notification_policy);
        let woken = options.wake.handle();
        // A device's answer to another's ask about the person is worked out
        // off the loop; when it lands here, the loop wakes to put it to them.
        crate::certification::attach(options.wake.handle());
        // The pictures the desk shows are this device's, kept beside its
        // desk: a change of the person's reaches them the same way.
        let wallpapers = std::sync::Arc::new(crate::wallpapers::Wallpapers::new(
            options.state.clone(),
            std::sync::Arc::clone(&woken),
        ));
        let watched_wallpapers = std::sync::Arc::clone(&wallpapers);
        // The desk widgets' choices, kept current by the same watch.
        let widget_prefs = crate::desk::prefs();
        let watched_widgets = std::sync::Arc::clone(&widget_prefs);
        // What may continue on the person's other devices — what they play
        // here, the Wi-Fi passphrases they share — read from the same settings
        // document on the same watch.
        let continuity = crate::platform::fabric::Continuity::default();
        continuity.read(&settings_store::load());
        let watched_continuity = continuity.clone();
        // The bar's own setting, read on the same watch: it changes what the
        // desk's band takes and when the bars show.
        let hide_top_bar = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
            settings_store::desktop_controls::DesktopControlsConfig::load().hide_top_bar,
        ));
        let watched_hide = std::sync::Arc::clone(&hide_top_bar);
        // The night light's warmth and schedule, and whether the panel
        // follows the light in the room, read on the same watch.
        let (night_light, watched_night_light) = crate::night_light::NightLight::watched(
            options.config.night_light,
            options.config.kept,
        );
        let (auto_brightness, watched_auto) = crate::ambient_brightness::AutoBrightness::watched();
        // Artwork the session resolves off the loop wakes it when it is
        // ready, so the frame that painted the letter repaints with it; the
        // apps installed here are the lists' to resolve, on their worker.
        crate::app_icons::set_wake(options.wake.handle());
        crate::app_icons::set_lists(options.lists.clone());
        // An app installed or removed here changes what every window calls app-made changes: this
        // session's own views read the names at once, and the broker is told, so the windows it
        // serves are sent them again with the workspace's people.
        options.lists.on_change(|| {
            if let Some(lists) = crate::app_icons::app_lists() {
                crate::app_icons::tell_names(&lists);
            }
            tilo_identity_host::policy_change::announce();
        });
        let settings_watch = settings_store::watch::watch_settings(move || {
            let document = settings_store::load();
            *watched.lock_recover() =
                settings_store::notification_policy::NotificationPolicy::from_document(&document);
            watched_wallpapers.follow(&document);
            crate::desk::follow_prefs(&watched_widgets, &document);
            watched_continuity.read(&document);
            watched_night_light.read(&document);
            crate::ambient_brightness::read(&watched_auto, &document);
            watched_hide.store(
                settings_store::desktop_controls::DesktopControlsConfig::from_document(&document)
                    .hide_top_bar,
                std::sync::atomic::Ordering::Relaxed,
            );
            woken();
        })
        .map_err(|error| tracing::warn!(%error, "settings are not watched"))
        .ok();
        let presentation = (options.presentation)();
        tilo_startup_metrics::mark("session-presentation-done");
        let overview = crate::overview::Overview::new(
            presentation.theme.motion(),
            presentation.theme.reduce_motion,
        );
        // The desk's open card, which the keys fold on Escape and the
        // overview folds as it comes over the desk: the desk is under the
        // windows and hears neither itself.
        let fold = tilo_desktop::desk_widgets::DeskFold::default();
        // What plays, which the desk's music widget shows: the session keeps
        // it current from the player Quick Settings already reads, so the
        // widget is a view of that one reading.
        let music = tilo_desktop::desk_widgets::DeskMusic::default();
        // The workspace's activity: the desk's own document, and the records
        // its apps keep in the store. The desk records into it, the model
        // reads it, and the apps this session runs reach the same model.
        // This device's records, as its store keeps them: the workspaces' app records, which the
        // activity's history reads and the person's mentions are looked for in. Every write that
        // lands in that store is noted in the one change stream the roles that follow records
        // read, so neither of them watches the records itself.
        let records_store: Option<crate::platform::Store> = options
            .search
            .records
            .as_ref()
            .map(crate::records::Records::store);
        let changes = options
            .search
            .records
            .as_ref()
            .map_or_else(crate::platform::records::Changes::default, |records| {
                records.changes()
            });
        let activity = crate::activity::Activity::new(
            options.state.as_deref(),
            options.person.clone(),
            records_store.clone(),
            crate::platform::records::Changes::clone(&changes),
            Some(std::sync::Arc::new(options.lists.clone())
                as std::sync::Arc<dyn crate::platform::apps::Apps>),
            Some(options.wake.handle()),
        );
        tilo_startup_metrics::mark("session-activity-done");
        // When each display's bar shows over the window covering it: the desk's
        // band, the chrome and the input that reaches for the bar read the one
        // model.
        let reveals: Rc<RefCell<tilo_desktop::topbar::Reveals>> = Rc::default();
        // The desk a login shows is built before its first frame: this pair
        // is the phase between the compositor coming up and the frame that
        // draws it (tools/perf/startup_marks.py).
        tilo_startup_metrics::mark("desk-build-start");
        let (desktop, strips, asks) = crate::desk::desk(
            &options,
            &activity,
            notices.clone(),
            std::sync::Arc::clone(&wallpapers),
            crate::desk::DeskLook {
                widgets: widget_prefs,
                fold: fold.clone(),
                overview: overview.open_flag(),
                music: music.clone(),
                reveals: Rc::clone(&reveals),
            },
        );
        desktop.set_gestures(options.config.gestures);
        // What each open window is told of the desk it stands on, from the
        // settings as they are now; the desk wears the same theme.
        let warmth =
            crate::window_warmth::Windows::new(presentation.theme.clone(), &options.person);
        desktop.set_theme(presentation.theme.clone());
        tilo_startup_metrics::mark("desk-build-done");
        let handle = desktop.with_policy(|policy| policy.handle().clone());
        // The desk as this device left it: its workspaces, the windows they
        // held, and the apps those windows open again.
        let mut restore = crate::desk_restore::Restore::default();
        match activity.opened_desk() {
            crate::desk_doc::Kept::Desk(kept) => {
                let mut kept = *kept;
                // A launch that no window came for is not a window this start
                // restores, and keeping it would leave one more of them on the
                // desk with every restart.
                crate::desk_restore::without_unfilled(&mut kept);
                restore = crate::desk_restore::Restore::of(&kept);
                handle.replace_tree(kept);
            }
            // The person's desk is on disk and this session did not open it:
            // they are looking at a desk that is not theirs, and have to be
            // told where their own still is.
            crate::desk_doc::Kept::Refused(refusal) => {
                crate::notices::tell_desk_not_opened(&notifications, &refusal);
            }
            crate::desk_doc::Kept::None => {}
        }
        // What each window shows, as the broker knows it: the desk offers the
        // windows a person may share from, and the sharing doors read it.
        let notes = options.launches.notes();
        // The person's arrangement: the desk this session holds, which the
        // workspace shares as its own desk record.
        let arrangement = crate::arrangement::open(&options.person, handle.clone(), &activity);
        let arrangement = Rc::new(RefCell::new(arrangement));
        // Whether the person is mid-gesture or typing, as only the
        // compositor knows: what an arranging action that is not their own
        // reads before it moves anything.
        let busy = Rc::new(Cell::new(crate::arrangement::Busy::default()));
        let underway = Rc::clone(&busy);
        // The one search text typed on the canvas places through: the
        // session search's own, the same `find` the palette and an agent's
        // `tilo` tool run.
        // The search is made further down, from what the session mounts in
        // between; it is in place before the session runs any action.
        let session_search: Rc<std::cell::OnceCell<Rc<SessionSearch>>> = Rc::default();
        let find: crate::arrangement::Find = {
            let search = Rc::clone(&session_search);
            Rc::new(move |needle: &str| {
                search
                    .get()
                    .map(|search| search.find(needle))
                    .unwrap_or_default()
            })
        };
        // The conversations this session's agents answer in, and the agents each workspace keeps,
        // both read from Chat's records of them.
        let records = crate::engine_records::EngineRecords::new(options.wake.handle());
        let conversations = Rc::new(crate::conversations::Conversations::new(
            records.clone(),
            options.wake.handle(),
        ));
        let agents = Rc::new(crate::agents::Agents::new(
            records.clone(),
            options.wake.handle(),
        ));
        // The one send every sender runs: a person's box, an agent, a script, and the canvas.
        let sends = crate::send_to::Sends::default();
        // What text that asks does (shell.md §8.1, "Asking from the canvas"): the person's send to
        // the agent they keep in the workspace in front — a new one where they keep none — through
        // `session:send-to#send`, so it lands in their conversation with it exactly as a send-to
        // box's message does, and Chat opens it.
        let ask: crate::arrangement::Ask = {
            let handle = handle.clone();
            let person = options.person.clone();
            let agents = Rc::clone(&agents);
            let sends = sends.clone();
            Rc::new(move |text: &str| {
                let Some((workspace, policy)) = handle.read(|tree| {
                    tree.active_workspace()
                        .map(|held| (held.id().to_string(), held.policy.clone()))
                }) else {
                    return false;
                };
                let to = crate::agents::agent_recipient(agents.kept(&workspace, &policy, &person));
                let arguments = serde_json::json!({
                    "to": [to],
                    "text": text,
                    "workspace": workspace,
                });
                let serde_json::Value::Object(arguments) = arguments else {
                    return false;
                };
                match sends.send(None, &arguments) {
                    Ok(_) => true,
                    Err(error) => {
                        tracing::warn!(%error, "the canvas's ask was not sent");
                        false
                    }
                }
            })
        };
        doors.add(Rc::new(RefCell::new(
            crate::arrangement::ArrangementDoor::new(
                Rc::clone(&arrangement),
                Rc::new(move || underway.get()),
                find.clone(),
                ask,
            ),
        )));
        // What the person's settings say the desk shows: a device that kept
        // no picture yet takes the choice, and otherwise the workspaces keep
        // the pictures this device kept for them until the setting changes.
        wallpapers.set_active(handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.slot.0.to_string())
        }));
        wallpapers.start(&settings_store::load());
        // The apps the person opens read their workspace from this desktop, leave
        // their notice in it, and act as the person the workspaces' governance
        // names.
        if let Some(launch) = &options.launch {
            launch.attach(desktop.clone());
            launch.set_person(options.person.clone());
        }
        // One process per app per workspace: its later windows open in the
        // process the session started, with connections of their own. A
        // window mapping settles what its app's door serves, so an app built
        // before the door keeps a process per window from then on.
        let window_doors = WindowDoors::new(crate::app_tabs::ServiceLinks::new(
            std::sync::Arc::clone(&options.launches),
        ));
        let settling = Rc::clone(&window_doors);
        let mut policy = crate::desk::DeskPolicy::for_shell(options.shell, &desktop, strips);
        // A window opens in the workspace its process was launched into: an agent's or a service's
        // open of an app in a workspace that is not in front lands there, never in front.
        let launched = std::sync::Arc::clone(&options.launches);
        desktop.with_policy(|policy| {
            policy
                .reads_launch_workspaces(Rc::new(move |pid, app| launched.workspace_of(pid, app)));
        });
        // What each window shows: the desk offers the windows a person may
        // share from the note, and one whose app holds no record shows none.
        desktop.with_policy(|policy| {
            policy.reads_window_records(Rc::new(crate::continuations::DeskNotes(
                std::sync::Arc::clone(&notes),
            )));
        });
        let wells = policy.wells();
        policy.set_window_mapped(Rc::new(move |pid| settling.window_mapped(pid)));
        // The connectors of the apps' outside services: what the record service and every hosted
        // tilolang instance reach an origin through.
        let connectors = Rc::new(crate::connectors::ConnectorHost::new());
        if let Some(launch) = &options.launch {
            launch.lend_doors(Rc::clone(&window_doors), Rc::clone(&connectors));
        }

        // The programs the person downloaded: the registry takes the
        // windows the desk gives a tab as they appear, so one that names no
        // application directory is offered the first time it shows itself.
        crate::downloaded::install(&mut options, &mut policy);
        tilo_startup_metrics::mark("compositor-start");
        let mut compositor = start(policy.boxed())?;
        // The render context is up: every mark from here is the frame the
        // person waits for (tools/perf/startup_marks.py).
        tilo_startup_metrics::mark("window-render-context-done");
        // A session in a window of another desktop runs no idle timers: that
        // desktop owns idle, blanking, locking and suspend.
        compositor.set_hosted(!options.machine.is_owned());
        // What the services take in wakes the loop instead of waiting for
        // its next pass.
        options.wake.bind(compositor.waker());
        compositor.set_input_config(options.config.input.clone());
        crate::controls::apply_keyboard(&mut compositor, &options.config.keyboard);
        compositor.set_idle_config(options.config.idle);
        crate::a11y_settings::apply(&mut compositor, &options.config.a11y)?;
        compositor.set_backlight(Box::new(crate::display_settings::PanelBacklight::find()));
        let machine = crate::power::install(&mut compositor, &options);
        compositor.add_host(crate::shell::under(options.shell, desktop.clone()));
        // The desk draws every window whole — its card, the lift under it
        // and its edge, in the theme's colours, brighter on the window in
        // use. The compositor shapes a window's corners to that card and
        // draws nothing around it: one drawing, not two edges and two
        // shadows on the same pixel.
        compositor.set_window_frame(tilo_compositor::WindowFrame::corners(f64::from(
            presentation.theme.tokens.radii.tile,
        )));
        // What stands over the windows, opened by the chrome's own widgets, by the session's
        // verbs and by the views bound to them, and read by whoever asks through the door.
        let overlays = Overlays::new();
        let desk_overlays = Rc::new(crate::overlays::Overlays::new());
        // Alt+Tab's panel: the desk's walk says what it shows, the chrome draws it, and the
        // compositor pictures the windows its cards name.
        let switcher = tilo_desktop::window_switcher::Switcher::default();
        // The surfaces the desk shows over its windows, and where each stands: the shell's own
        // views, or the quick settings' app where the device has one and the desk draws it. The
        // desk's door and the chrome's door open them through it.
        let surfaces = crate::desk_resource::Surfaces {
            status_panel: crate::quick_settings::installed(&options.surfaces),
            shell: options.shell,
            panel: overlays.clone(),
            overlays: Rc::clone(&desk_overlays),
            overview: overview.open_flag(),
        };
        // The same handle, for the pass that keeps one surface up at a time.
        let one_surface = surfaces.clone();
        // What the keys ask the panel's own app, which the surface loop hands
        // the running app each pass. A device whose desk draws the panel has
        // one; the shell's own view of the quick settings answers its own keys.
        let panel_asks: Rc<RefCell<Vec<&'static str>>> = Rc::default();
        let panel_keys =
            (surfaces.panel_shows(crate::quick_settings::SURFACE)).then(|| Rc::clone(&panel_asks));
        let desk_door: tilo_session_core::door::Shared =
            crate::shell::desk_door(options.shell, desktop.clone(), surfaces.clone());
        doors.add(Rc::clone(&desk_door));
        // The chrome above the windows: its own tree, beside the desk's.
        let intents = Rc::new(RefCell::new(Vec::new()));
        let engine = activity.open_engine(&options, &handle);
        // This device's channels of its workspaces' sealed records: its keys,
        // and the devices each one admits, which its window streams carry
        // between. The broker the apps reach is the one the channel service
        // gates, so a sealed record opens before any of it reaches an app.
        let channels_broker = std::sync::Arc::new(std::sync::Mutex::new(
            tilo_broker_core::collab_core::BrokerChannels::default(),
        ));
        // What the broker shows the apps that hold a record's route open: the
        // collaboration plane hands it what another device wrote, so a record
        // an app holds shows everyone's writes, and the broker below shows
        // this device's own.
        let channels_fanout =
            std::sync::Arc::new(tilo_action_broker::collab_broker::FanoutState::new());
        let channel_link = crate::channels::ChannelLink::new();
        // Search reads the workspaces' records off the records themselves,
        // with no window of their app running: what this device holds is
        // found before anything changes, and every change after it. The same
        // changes are what the person's mentions and the activity follow
        // (`crate::platform::records`): one stream of them here, read by both,
        // and noted where every write lands — the records' store, and the
        // index of what the broker receives from the apps and from peers.
        changes.waking(options.wake.handle());
        let record_index = engine.as_ref().map(|engine| {
            crate::record_index::RecordIndex::start(
                std::sync::Arc::clone(engine)
                    as std::sync::Arc<dyn crate::record_index::RecordSource>,
                std::sync::Arc::new(crate::record_index::AppReader::of_device()),
                Some(std::sync::Arc::clone(&options.content)),
                crate::platform::records::Changes::clone(&changes),
            )
        });
        let record_changes = record_index.clone().map(|index| {
            index as std::sync::Arc<dyn tilo_broker_core::services::RecordIndexChanges>
        });
        let channels = engine.as_ref().and_then(|engine| {
            let settled = record_index.clone();
            crate::channels::Channels::open(
                engine,
                std::sync::Arc::clone(&channel_link),
                std::sync::Arc::clone(&channels_broker),
                move || {
                    if let Some(index) = &settled {
                        index.settled();
                    }
                },
            )
            .map_err(|error| tracing::warn!(%error, "the channel service runs no threads"))
            .ok()
        });
        // What this device holds is read once the channel service that opens its sealed records
        // runs: read before it, every sealed record reads as nothing.
        if let Some(index) = &record_index {
            index.sweep();
        }
        // The agent door toward the person's other devices: answered on this device's
        // endpoint, and served by the agent door once it opens.
        let remote = crate::remote_door::Remote::new(options.wake.handle());
        let peer_documents = crate::sync::PeerDocuments::new(
            std::sync::Arc::clone(&channels_broker),
            std::sync::Arc::clone(&channels_fanout),
            record_changes.clone(),
            engine.as_ref().map(|engine| engine.arrivals()),
        );
        let sync = start_collaboration(
            &options,
            continuity.clone(),
            engine.clone(),
            (beside, &remote),
            Some(peer_documents.clone()),
        );
        // The records the session itself writes are the workspaces' own, and its writes reach this
        // device's apps as a member's do.
        if let Some(engine) = &engine {
            records.attach(
                std::sync::Arc::clone(engine),
                &changes,
                Some(peer_documents),
            );
        }
        // The content the workspaces' records reference: kept beside their
        // records under this session's state, and asked of the workspace's
        // other devices on the topic the records travel.
        let record_content = crate::content::Plane::open(options.state.as_deref());
        if let Some(sync) = &sync {
            sync.attach_content(std::sync::Arc::clone(&record_content));
            // What this session says when another of the person's devices
            // offers it work: the inputs its own content already keeps.
            if let Some(actions) = sync.device_actions() {
                actions.attach_content(std::sync::Arc::clone(&record_content));
            }
        }
        // What other members send here is the workspaces' to admit: their
        // policies are read from the desk this session shows.
        if let Some(sync) = &sync {
            sync.attach_desk(handle.clone());
        }
        // The person's own workspaces, which their other devices hold with
        // them: kept beside the desk, and opened when this computer knows
        // whose they are.
        let personal = crate::workspaces::personal_for(options.state.as_deref());
        // The fabric the session acts on: one handed to it — a stand-in in a
        // test — or its own, when the person certified this device. A
        // session that takes part as nobody keeps none.
        let own_fabric = match &options.fabric {
            Some(_) => None,
            None => sync.as_ref().and_then(|sync| sync.fabric()),
        };
        let fabric: Option<crate::platform::fabric::SharedLink> =
            match (&options.fabric, own_fabric) {
                (Some(shared), _) => Some(Arc::clone(shared)),
                (None, Some(own)) => Some(own),
                (None, None) => None,
            };
        // The person's own record of what their devices answered when one of
        // them asked to be unlocked reads their fabric: what it carries is
        // written there as their history.
        if let Some(link) = &fabric {
            activity.watch_fabric(Arc::clone(link));
            // Notifications continue on the person's other devices.
            notifications.borrow().attach_fabric(Arc::clone(link));
        }
        // Nearby Send: one engine for every file that leaves this device for
        // another of the person's, whether a screenshot the studio sends or a
        // file Files hands over. It stages into this machine's own spool, and
        // the session's pass turns when a hand-over moves.
        let nearby = match crate::nearby_transfer::NearbyTransfer::open(
            fabric.clone(),
            crate::nearby_transfer::spool_root(),
            continuity,
            options.wake.handle(),
        ) {
            Ok(nearby) => Some(nearby),
            Err(error) => {
                tracing::warn!(%error, "no thread takes what is sent nearby");
                None
            }
        };
        if let (Some(sync), Some(nearby)) = (&sync, &nearby) {
            sync.attach_nearby(std::sync::Arc::clone(nearby));
        }
        // The machine's status, which the bar's status cluster shows as the
        // battery, network and volume glyphs, the lock screen shows under its
        // card and the desk reads: every part of it is read off the services
        // that publish it.
        let readings = Rc::new(RefCell::new(crate::status::Readings::default()));
        // Every app shows its live activity on this one board, which the
        // bar's pill reads and the floating layer draws as cards.
        let live_activities = std::sync::Arc::new(runtime_collab::LiveActivities::default());
        // A timer starting in the background must reach the bar on the next
        // frame, not the next minute.
        live_activities.set_host_wake(options.wake.handle());
        // The cards apps draw themselves, over the desk: the session
        // authorises what each presents, and the floating layer's place is
        // where the desk stands its window.
        let cards = crate::cards::Cards::new();
        desktop.with_policy(|policy| policy.use_cards(cards.placements()));
        let activities = Rc::new(crate::chrome::Activities::new(
            std::sync::Arc::clone(&live_activities),
            cards.clone(),
        ));
        // An application's tray item registering, changing or going away is
        // read off the loop: it must reach the bar on the next frame.
        let tray = options
            .desktop
            .as_ref()
            .and_then(|desktop| desktop.tray().cloned());
        if let Some(tray) = &tray {
            tray.set_host_wake(options.wake.handle());
        }
        // The displays a fullscreen window owns, which the loop takes from
        // the compositor every pass: the bar there steps aside for it.
        let fullscreen: Rc<RefCell<Vec<String>>> = Rc::default();
        // Whether the person may invite someone into the workspace in front,
        // which the loop answers every pass: the bar offers its invite button
        // and the roster its footer from it.
        let invitable: Rc<Cell<bool>> = Rc::default();
        // The windows this session shares with its workspaces: a share's
        // encoder wakes the loop when it has a frame, and the bar reads them.
        // A window another device shares is decoded on the same waker, so a
        // picture that comes out late lands without waiting for the next
        // frame the desk draws for something else.
        // A session in a workspace's VM, holding its Browser or its Linux apps, runs for the
        // workspace's members: nobody sits at it, so its windows are shared the moment they open.
        let hosted_browser = crate::hosted_browser::shares_by_default();
        let window_shares = crate::window_share::WindowShares::new(
            options.wake.handle(),
            options.person.clone(),
            options.salt,
            hosted_browser,
        );
        handle.set_peer_picture_wake(Some(options.wake.handle()));
        // The emoji picker is the chrome's; the session holds its picks,
        // writes them into the focused field and keeps its recents.
        let emoji = crate::emoji::Emoji::mount();
        // Settings' Identify answers: the session reads the request each pass
        // and the bar says each display's number and name on it.
        let numbers = crate::display_identify::Numbers::default();
        let published_displays = crate::shared_displays::Publisher::default();
        // One table for the keys the session answers and the guide that
        // explains them: a chord that works is a chord the guide and the
        // menus show, and a person's rebinding reaches every one of them.
        let shortcuts = Rc::new(RefCell::new(std::mem::take(&mut options.config.shortcuts)));
        // The desktop's control hints, shared by every scheme: the record as the person's
        // settings hold it and what this session alone knows, for the chrome that draws them.
        let hints = tilo_desktop::control_hints::HintsFeed::default();
        let chrome = Rc::new(RefCell::new(crate::chrome::host(
            &handle,
            &options,
            crate::chrome::ChromeWiring {
                intents: Rc::clone(&intents),
                asks: Rc::clone(&asks),
                shortcuts: Rc::clone(&shortcuts),
                feeds: crate::chrome::BarFeeds {
                    signals: Rc::clone(&notice_signals),
                    readings: Rc::clone(&readings),
                    activities: Rc::clone(&activities),
                    tray: tray.clone(),
                    fullscreen: Rc::clone(&fullscreen),
                    shares: Rc::clone(&window_shares),
                    reveals: Rc::clone(&reveals),
                    identify: numbers.ask(),
                    invitable: Rc::clone(&invitable),
                },
                engine: engine.clone(),
                hints: hints.clone(),
            },
            sync.clone(),
            &overview,
            &emoji.link(),
            &overlays,
            &switcher,
        )));
        compositor.add_host(crate::shell::under(options.shell, Rc::clone(&chrome)));
        // Search's code is asked for as the session starts, so it is in by the
        // time someone searches; a search asked for before then opens now,
        // and the desk publishes it on its next pass.
        handle.search_palette().borrow_mut().adopt();
        // The desk as the session publishes it, which the shell reads through the door.
        let chrome_door = crate::shell::chrome_door(options.shell, &chrome, surfaces.clone());
        doors.add(Rc::clone(&chrome_door));
        // The seam the agent door's connections ask this session's own doors on: made before
        // the plane starts, which serves the programs.
        let session_doors = std::sync::Arc::new(crate::agent_door::SessionDoors::new(
            options.wake.handle(),
            &options.person,
        ));
        let task_programs = Some(std::sync::Arc::new(
            crate::task_programs::TaskPrograms::new(std::sync::Arc::new(
                crate::task_programs_host::TaskProgramHost::new(std::sync::Arc::clone(
                    &session_doors,
                )),
            )),
        ));
        // The session's own actions, offered to a turn's agent as the broker's steps: the
        // arrangement, its undo, the windows and the notices, which the REPL already calls.
        // Each is answered on the loop's pass, where the arrangement is. The desk's styles, read
        // from the store, share the desk's reading (shell.md §6).
        let steps: std::sync::Arc<dyn crate::platform::steps::SessionSteps> =
            std::sync::Arc::clone(&session_doors)
                as std::sync::Arc<dyn crate::platform::steps::SessionSteps>;
        let session_actions = std::sync::Arc::new(crate::session_actions::SessionActions::new(
            std::sync::Arc::clone(&steps),
        ));
        session_doors.keep_actions(std::sync::Arc::clone(&session_actions));
        // What a watch of a whole workspace reads: the changes in this device's replica.
        if let Some(engine) = &engine {
            session_doors.keep_records(std::sync::Arc::clone(engine));
        }
        // The person's settings, answered under the provider Settings' window names for each
        // control it shows: the session keeps them in its settings service, and this is what the
        // broker hands a change of them, on the loop's pass, as it hands the session's own steps.
        let settings_steps = std::sync::Arc::new(crate::settings_steps::SettingsSteps::new(steps));
        // The runs the broker reads: each agent the person admits at the door works in a run of its
        // own among them, beside the conversations' turns.
        session_doors.keep_runs(conversations.runs());
        // The agents the person admits at its door, as the desk and the bar show them.
        let seats = crate::agent_door::Seats::new();
        let desk = crate::desk_resource::desk_from(crate::desk_resource::Shared {
            seats: std::sync::Arc::clone(&seats),
            handle: handle.clone(),
            person: options.person.clone(),
            signals: Rc::clone(&notice_signals),
            activities: Rc::clone(&activities),
            surfaces,
            sync: sync.clone(),
            notes: std::sync::Arc::clone(&notes),
            notices: notices.clone(),
            tray: tray.clone(),
            engine: engine.clone(),
            // The invite sheet is the session's own surface: the desk asks for it here.
            asks: Rc::clone(&asks),
        });
        let desk_service = desk();
        let shell = if options.shell {
            Some(
                crate::shell::Shell::mount(
                    &mut compositor,
                    &mut doors,
                    desk(),
                    wells,
                    presentation.theme.clone(),
                )
                .map_err(|why| {
                    Error::Backend(tilo_compositor::BackendError(format!(
                        "the shell did not start: {why}"
                    )))
                })?,
            )
        } else {
            None
        };
        // The surfaces the shell stands in for read as nothing, as they draw
        // nothing: the shell's parts are what a client finds there, while the
        // desktop's own desk and chrome keep their actions.
        let aside: Vec<tilo_session_core::door::Shared> = if shell.is_some() {
            vec![Rc::clone(&desk_door), Rc::clone(&chrome_door)]
        } else {
            Vec::new()
        };
        // Who is here, over the windows: the roster the bar's faces open and
        // where everyone else's pointer is.
        let people = crate::people::mount(
            &mut crate::platform::surfaces::Hosted::new(
                &mut compositor,
                &mut doors,
                presentation.theme.clone(),
            ),
            crate::people::Given {
                scale: options.scale,
                seated: options.person.clone(),
                notes: {
                    let notes = std::sync::Arc::clone(&notes);
                    Rc::new(move || notes.windows())
                },
                apps: Some(std::sync::Arc::new(options.lists.clone())
                    as std::sync::Arc<dyn crate::platform::apps::Apps>),
                handle: handle.clone(),
                sync: sync.clone(),
                // Who is here as the bar's faces are drawn from them: the plane's.
                here: {
                    let sync = sync.clone();
                    let notes = std::sync::Arc::clone(&notes);
                    Rc::new(move |tree: &mut tilo_desktop_model::DesktopTree| {
                        if let Some(sync) = &sync {
                            sync.show(tree, &notes, &|app: &str| {
                                Some(crate::app_names::app_name(app))
                            });
                        }
                    })
                },
                intents: Rc::clone(&intents),
                invitable: Rc::clone(&invitable),
            },
        );

        // What the floating layer keeps clear of: the banners' stack while a
        // banner shows, and the readout's band while it stands.
        let banner_stack = crate::panels::Reserved::new();
        let readout_band = crate::panels::Reserved::new();
        // Where each display stands and where the person types, filled in every pass: what the
        // readout and the floating layer keep clear of.
        let readout_place = crate::panels::ReadoutPlaces::default();
        let _activity_layer = mount_activities(
            &mut compositor,
            &mut doors,
            &activities,
            &cards,
            (banner_stack.clone(), readout_band.clone()),
            (&readout_place, &handle),
            &options,
        );
        mount_banners(
            &mut compositor,
            &mut doors,
            &notices,
            &handle,
            &options,
            banner_stack,
            banners.clone(),
        );

        // The sheets, over the desktop and its panels.
        let crate::sheets::mount::Sheets {
            power,
            workspace_settings,
            portal,
            file_picker,
            directory,
            invite,
            delete_workspace,
            display_confirm,
            displays: display_seat,
            shortcut_guide: guide_seat,
            add_app,
            uninstall,
            lock_view,
        } = crate::sheets::mount::mount_sheets(&mut compositor, &mut doors, &options);
        // Where a report about Tilo itself goes, as this computer is told:
        // the box a report is written in opens to it.
        let feedback = crate::feedback::Destination::configured();
        // What only the session can do, offered the same way to every
        // surface that shows it: the workspace menu, the Tilo menu, search.
        crate::host_actions::publish(
            &handle,
            crate::sheets::mount::power_offers(&options),
            directory.is_some(),
            feedback.is_some(),
        );

        // The one worker that encodes a recording and writes every capture's
        // file, and what it makes of the recorder's work.
        let (capture_io, capture_notes) = crate::capture_io::CaptureIo::new(options.wake.handle());
        // The capture studio, over everything it captures.
        let capture_studio = crate::capture_studio::mount(
            &mut compositor,
            &mut doors,
            &options,
            nearby.as_ref(),
            Rc::clone(&capture_io),
            capture_notes,
            &live_activities,
        );

        // The desktop's own chords win over every app's: the portal's
        // bindings step aside for the table the session answers first.
        let portal_shortcuts = options
            .desktop
            .as_ref()
            .and_then(|desktop| desktop.portal())
            .map(|portal| portal.shortcuts().clone());
        if let Some(portal) = &portal_shortcuts {
            portal.set_shortcut_table(shortcuts.borrow().clone());
        }
        // The controller the person last used: the guide leads with its
        // buttons, and a key of their own leads with the keyboard again.
        let controller: Rc<Cell<Option<ui::ControllerFamily>>> = Rc::new(Cell::new(None));
        let controller_keys: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        // The guide's state while it is up: the key surface holds back what
        // would act behind the guide and hands it the chord it waits for.
        let guide_state = Rc::new(tilo_sheets::shortcut_guide::State::new(
            Rc::clone(&shortcuts),
            portal_shortcuts.clone().map(|portal| {
                Rc::new(crate::shortcut_guide::PortalShortcuts(portal))
                    as Rc<dyn tilo_sheets::shortcut_guide::AppShortcuts>
            }),
            Rc::clone(&controller),
        ));
        let shortcut_guide =
            crate::shortcut_guide::ShortcutGuide::new(guide_seat, Rc::clone(&guide_state));
        let guide_keys = Rc::clone(&guide_state);
        let (commands, controls_reading, invocations, triggered, readout) = mount_controls(
            &mut compositor,
            &mut doors,
            Rc::clone(&shortcuts),
            portal_shortcuts,
            guide_keys,
            KeyContext {
                controller: Rc::clone(&controller),
                controller_keys: Rc::clone(&controller_keys),
                displays: display_seat
                    .as_ref()
                    .map(|seat| tilo_sheets::Seat::presentation(seat).clone()),
                fold,
                overlays: overlays.clone(),
                desk: Rc::clone(&desk_overlays),
                reveals: Rc::clone(&reveals),
                panel: panel_keys.clone(),
            },
            options.scale,
            (readout_band, &readout_place),
        );
        // The readout draws in the person's theme like the desk and the shell
        // do: its ground is the theme's floating surface, never a fixed fill.
        readout.borrow_mut().set_theme(presentation.theme);

        // Setup last.
        let setup = mount_setup(
            &mut compositor,
            &mut doors,
            options.setup.take(),
            &options.person,
            options.scale,
        );

        // What the broker admits a connection by, and what an app started
        // from here on is written into.
        if let Some(apps) = &options.launch {
            apps.lend_launches(std::sync::Arc::clone(&options.launches));
        }
        // A launch a service, an app or an agent asks the session for waits
        // for the pass that starts it: the same path the desk's own launches
        // take.
        options.launches.waking(options.wake.handle());

        // The shortcuts the person saved and the triggers that start them
        // without anyone invoking them: armed on the desk this device shows,
        // and started on the broker below. A run executes here, on a runtime
        // of its own, and what a person saves through the action door is one
        // of its workspace's settings, written into the workspace's records
        // and shown on the desk on the loop's next pass.
        let automation_runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("tilo-automation")
            .enable_all()
            .build()
            .map_err(crate::Error::host)?;
        let shortcut_library = crate::plane::ShortcutLibrary::new(
            automation_runtime.handle().clone(),
            engine.as_ref().map(std::sync::Arc::clone),
        );
        // What each of this device's workspaces revokes, read from the
        // workspaces the engine holds: the broker's decisions and the
        // automation's arming both refuse a device its workspace revoked.
        let revocations = crate::engine::revocations_of(engine.as_ref());
        // The typed opens an app asks this session for: one service, made
        // here so the broker below serves Files' waits for its lists'
        // changes, and started from the doors table.
        let tools = crate::tools::Handle::new(
            std::sync::Arc::clone(&options.launches),
            options.wake.handle(),
        );
        let action_plane = start_action_plane(
            &options,
            &live_activities,
            &handle,
            std::sync::Arc::new(shortcut_library),
            &activity,
            crate::doors::plane::Knitted {
                turns: Some(conversations.runs()),
                agents: Some(agents.roster()),
                tools: Some(tools.clone()),
            },
            crate::doors::plane::Collaboration {
                revocations: std::sync::Arc::clone(&revocations),
                sync: sync.as_deref(),
                engine: engine.as_ref().map(std::sync::Arc::clone),
                sealed: crate::plane::Sealed {
                    broker: std::sync::Arc::clone(&channels_broker),
                    keys: engine.as_ref().map(|engine| engine.keys()),
                },
                content: Some(std::sync::Arc::clone(&record_content)),
                records: record_index,
                fanout: std::sync::Arc::clone(&channels_fanout),
                task_programs,
                session_actions: Some(std::sync::Arc::clone(&session_actions)),
                settings_steps: Some(std::sync::Arc::clone(&settings_steps)),
                nearby: nearby.as_ref().map(std::sync::Arc::clone),
            },
        )?;
        // An action an app publishes, named by a search row, a link or a
        // person's card, is delivered through the broker: the app starts for
        // it when it is closed, and the action reaches it wherever it runs.
        if let Some(launch) = &options.launch {
            launch.lend_broker(action_plane.as_ref().and_then(Plane::agent_broker));
        }
        // A session in a workspace's VM is started for one workspace, and everything it does
        // of what a person's session does is the window it holds: it is told here what wakes its
        // loop when the relay answers, and what the broker admits of its window's page.
        if let Some(hosted) = &mut options.hosted_browser {
            hosted.waking(options.wake.handle());
            hosted.watching(
                action_plane.as_ref().and_then(Plane::agent_broker).as_ref(),
                action_plane.as_ref().and_then(Plane::hosted_workspaces),
                &options.person,
            );
        }
        // What Files may read, and where a received file may land, is what
        // this session's workspaces grant it: read off the desk the broker
        // decides against.
        if let (Some(nearby), Some(plane)) = (&nearby, &action_plane) {
            nearby.attach_authority(std::sync::Arc::clone(&plane.desk)
                as std::sync::Arc<dyn tilo_broker_core::services::WorkspaceAuthority>);
        }
        // The agent door: a program of this login asks here to work in a
        // workspace as an agent, and the person answers on their sheets. It
        // binds first, and is served from the broker the plane starts, which
        // already decides against the desk the loop shows.
        let questions = options.questions.clone().or_else(|| {
            options
                .desktop
                .as_ref()
                .map(|desktop| desktop.questions().clone())
        });
        // What an agent's run asks its person before a step its grants leave to them is put on
        // the same sheets ([`crate::run_questions`]).
        if let (Some(questions), Some(broker)) = (
            questions.clone(),
            action_plane.as_ref().and_then(Plane::agent_broker),
        ) {
            broker.set_app_action_consent(std::sync::Arc::new(crate::run_questions::Sheet::new(
                questions,
            )));
        }
        // Every service this session serves, from the one table that says
        // who serves each: the identity an app signs in through, the
        // machine's readings an app the launcher connects reads, and the
        // data this device used. They start here, before any app does.
        let table = crate::doors::start(crate::doors::Serving {
            desk: action_plane
                .as_ref()
                .map(|plane| std::sync::Arc::clone(&plane.desk)),
            person: options.person.clone(),
            render_node: compositor.render_node(),
            keys: engine
                .as_ref()
                .map_or_else(Default::default, |engine| engine.keys()),
            questions: questions.clone(),
            consents: options
                .desktop
                .as_ref()
                .and_then(|desktop| desktop.consents())
                .cloned(),
            tools: Some(tools),
            wake: options.wake.handle(),
        });
        tilo_startup_metrics::mark("doors-done");
        // The control door `tilo-control` reaches: the development socket and the device admin's,
        // both answered over the broker this session's plane starts. It exists whether or not a
        // socket opens, because a program inside an app this session started is served at it too.
        // The activity undoes an agent's run in the records it changed through the same door.
        let door_host = std::sync::Arc::new(
            crate::agent_door::Host::new(
                action_plane.as_ref().and_then(Plane::agent_broker),
                std::sync::Arc::clone(&session_doors),
            )
            .with_session_actions(Some(std::sync::Arc::clone(&session_actions))),
        );
        let undo_door: std::sync::Arc<dyn crate::task_undo::UndoDoor> = door_host.clone();
        activity.attach_door(undo_door);
        activity.attach_content(std::sync::Arc::clone(&record_content));
        let control = crate::test_control::Control::open(&options, door_host);
        tilo_startup_metrics::mark("control-door-open");
        let door = crate::agent_door::mount_named(
            &options,
            questions.clone(),
            std::sync::Arc::clone(&session_doors),
            control.door(),
            std::sync::Arc::clone(&seats),
        );
        // Beside the harness's control socket: whether the loop still turns,
        // and where every thread stands when it does not. A session without
        // one serves no such door, and no other session does either.
        let liveness = options.test_control.as_deref().and_then(|control| {
            compositor.serve_liveness(control, crate::agent_door::bind_private)
        });
        let agent_door = door.map(|door| {
            let mut door = door.serve(
                action_plane.as_ref().and_then(Plane::agent_broker),
                action_plane
                    .as_ref()
                    .map(|plane| std::sync::Arc::clone(&plane.desk)),
            );
            door.hear_remote(std::sync::Arc::clone(&remote));
            door
        });
        let automation = crate::automation::Automation::new(
            options.state.as_deref(),
            options.wake.handle(),
            handle.clone(),
            options.person.clone(),
            crate::automation::Feeds {
                // Which of the person's devices this one is, and which of
                // them are online: each schedule runs on one of them.
                fabric: sync.as_ref().and_then(|sync| sync.fabric()),
                // The apps this session starts, the person's focus modes, and
                // what its workspaces revoke: each is a trigger a shortcut can answer.
                launches: std::sync::Arc::clone(&options.launches),
                policy: std::sync::Arc::clone(&notification_policy),
                revocations,
            },
        );

        // The shell-ui socket this session serves: what an app asks about
        // its own tabs arrives there and is answered on this loop's pass.
        let tabs = crate::app_tabs::AppTabs::start(
            std::sync::Arc::clone(&options.launches),
            options.wake.handle(),
            cards,
        )
        .map_err(|error| tracing::warn!(%error, "the shell-ui service does not run"))
        .ok();
        // What the surface apps this session starts are of it: the launch
        // the broker admits each connection by, and the socket it answers
        // them on.
        let mut surfaces = crate::surfaces::Surfaces::default();
        surfaces.lend(
            std::sync::Arc::clone(&options.launches),
            action_plane.as_ref().map(|plane| plane.path.clone()),
            options.wake.handle(),
        );
        // The lock, and the screen that shows it: the session keeps what the
        // screen shows and publishes it, so the screen is an app of the
        // session's like any other.
        let lock = crate::lock_service::Lock::default();
        // The on-screen keyboard's state, published the same way: the app of
        // the session draws what the session says it does.
        let keyboard = crate::keyboard::Keyboard::default();
        let mut started = std::mem::take(&mut options.services);
        started.push(Box::new(desk_service));
        // A service that records who changed what records the person's, not
        // the id its caller made it with: the session knows who sits here
        // before it starts them.
        for service in &mut started {
            service.seated(&options.person, &seat.earlier);
        }
        let services = mount_services(&mut doors, started);
        // The records the services keep themselves: the activity lists an agent's run in one and
        // undoes it through the record's own history, as it does an app's.
        activity.attach_services(Rc::clone(&services));
        // The session's own controls, served beside its services: an app
        // reads what they are now and invokes what changes them the way it
        // does every other resource of this device's.
        services
            .borrow_mut()
            .add(Box::new(crate::controls::SessionControls::new(
                Rc::clone(&commands),
                Rc::clone(&controls_reading),
            )));
        // The turns' summonses, each offering Chat's own `record-work` to the agent answering it.
        services
            .borrow_mut()
            .add(Box::new(crate::conversations::TurnSummonses::new(
                Rc::clone(&conversations),
            )));
        services
            .borrow_mut()
            .add(Box::new(crate::send_to::SendToService::new(sends.clone())));
        // The shell's own records reach the door as any app's do the moment its package runs.
        if let Some(shell) = &shell {
            shell.share_records(&services, &session_doors);
        }
        // The shell's own readings and verbs, on the same door every other
        // service answers on.
        crate::shell::state::ShellService::mount(
            &services,
            &handle,
            overlays.clone(),
            Rc::clone(&shortcuts),
        );
        services
            .borrow_mut()
            .add(Box::new(crate::lock_service::LockService::new(
                lock.clone(),
            )));
        // A window shared with a workspace, and the windows its members share.
        services
            .borrow_mut()
            .add(Box::new(crate::window_share::WindowShareService::new(
                Rc::clone(&window_shares),
            )));
        // The displays, on the same door: their arrangements land as the
        // commands the loop carries out, their brightness is set at once, and
        // the reading says where each display stands.
        let displays_service = crate::displays::Displays::mount(
            &services,
            options.backlights.take(),
            Rc::clone(&commands),
        );
        // A brightness key or slider anywhere wakes the loop, so a display
        // dimmed in software follows at once.
        displays_service.watch(&options.wake.handle());
        services
            .borrow_mut()
            .add(Box::new(crate::keyboard::KeyboardService::new(
                keyboard.clone(),
            )));
        // Which controls Quick Settings shows, and in what order: this device's own setting, kept
        // here so the arrangement is the same whether the panel is drawn or not.
        services
            .borrow_mut()
            .add(Box::new(crate::quick_settings::QuickSettingsService::new()));
        // Dictation joins the services: its recognizer, microphone and
        // consent are only the session's, and the sheet asks about them.
        let dictation =
            crate::dictation::Dictation::mount(&mut compositor, &mut doors, &services, &options);
        // The questions about the person's apps answer through the registry
        // of their downloaded programs, which is one of the services.
        let app_sheets = crate::app_sheets::AppSheets::new(
            add_app,
            uninstall,
            Rc::clone(&services),
            crate::notifications::RoleNotices::of(&notifications),
        );

        // The apps search lists are this device's lists, the same ones the action door lists and
        // names apps from: an app a person sees is one app, whoever reads it.
        // What the session opens an app through, kept for the notices that name a place too.
        let action_apps = options.apps.clone();
        let search = Rc::new(SessionSearch::new(
            options.search,
            Some(Arc::new(options.lists.clone()) as Arc<dyn crate::platform::apps::Apps>),
            &options.person,
            options.apps,
            Some(options.wake.handle()),
            Some(Arc::new(crate::activity_models::Gateway)),
        ));
        session_search.get_or_init(|| Rc::clone(&search));
        let asker = crate::questions::Asker::new(
            questions,
            portal.clone(),
            file_picker.clone(),
            Rc::clone(&services),
        );
        let workspace_tabs =
            crate::workspace_settings::WorkspaceTabs::new(&records, options.wake.handle());
        let directory_store = Rc::new(crate::directory::Store::default());
        let clipboard = Rc::new(crate::clipboard::Clipboard::new(options.wake.handle()));
        // The desk's own fields copy and paste through the desktop's clipboard, as apps do.
        ui::host::install_default(
            ui::host::HostServices::null().with_clipboard(Arc::new(clipboard.desk())),
        );
        let boot_join = crate::boot_join::BootJoin::at_boot(&directory_store, &handle);
        let community =
            crate::community::Community::for_site(crate::platform::directory::community_site());
        // The person's mentions in the workspaces' records: told through those notifications, kept
        // in the records' store, and opened through this session's own door to the apps.
        let mentions = records_store.clone().map(|store| {
            let naming = handle.clone();
            crate::mentions::Mentions::over(crate::mentions::Given {
                person: vec![options.person.clone()],
                store: Some(store.clone()),
                records: Some(Rc::new(crate::platform::records::Linked::over(
                    store,
                    crate::platform::records::Changes::clone(&changes),
                ))),
                notices: Some(crate::notifications::RoleNotices::of(&notifications)),
                name: Rc::new(move |person: &str| {
                    naming
                        .read(|tree| {
                            tree.workspaces
                                .iter()
                                .flat_map(|workspace| workspace.collaborators.iter())
                                .find(|collaborator| collaborator.id == person)
                                .map(|collaborator| collaborator.name.clone())
                                .filter(|name| !name.is_empty())
                        })
                        .unwrap_or_else(tilo_desktop::here::guest_label)
                }),
                wake: Some(options.wake.handle()),
            })
        });
        let landings = Rc::new(crate::landing::Landings::new(options.wake.handle()));
        let inviting = crate::invite::Invite::new(
            Rc::new(crate::platform::invite::DeskWorkspace {
                landings: Rc::clone(&landings),
                handle: handle.clone(),
                person: options.person.clone(),
                sync: sync.is_some(),
                engine: engine.clone(),
                services: Rc::clone(&services),
                site: crate::platform::directory::community_site,
            }),
            Rc::clone(&clipboard) as Rc<dyn crate::platform::clipboard::Clipboard>,
            Rc::clone(&directory_store),
        );
        let mut shared = Shared {
            handle,
            desktop,
            activity,
            person: options.person.clone(),
            earlier: seat.earlier,
            search,
            launch: options.launch,
            apps: action_apps,
            restore: RefCell::new(restore),
            drives: options.drives,
            asks,
            send_from: Cell::default(),
            report_from: Cell::default(),
            feedback,
            intents,
            quick_settings: crate::quick_settings::installed(&options.surfaces),
            panel_asks: Rc::clone(&panel_asks),
            pending_paste: Cell::new(false),
            panel_back: Cell::new(None),
            overlays,
            surfaces: one_surface,
            desk_overlays,
            switcher,
            doors,
            invocations,
            triggered,
            commands,
            controls: Rc::clone(&controls_reading),
            waiting: RefCell::default(),
            readings,
            services,
            sends,
            table,
            window_shares,
            pages: crate::page_track::PageTracks::new(),
            seats,
            hosted: !options.machine.is_owned(),
            fabric: fabric.clone(),
            vpn,
            devices: crate::devices::press::Devices::default(),
            bluetooth: crate::bluetooth_handoff::Handoffs::default(),
            handoff: Rc::new(std::cell::RefCell::new(None)),
            waking: crate::wake::Wake::new(&options.wake_seams),
            wake_handle: options.wake.handle(),
            music,
            power,
            workspace_settings,
            ws_settings_fed: Cell::new(None),
            capabilities_fed: Cell::new(None),
            fs_authority: crate::fs_authority::Publisher::for_session(),
            workspace_tabs,
            conversations,
            agents,
            records,
            directory,
            invite,
            inviting,
            delete_workspace,
            delete_workspace_ask: RefCell::default(),
            directory_store,
            boot_join: RefCell::new(boot_join),
            hosted_browser: RefCell::new(options.hosted_browser.take()),
            community: RefCell::new(community),
            portal,
            file_picker,
            lock,
            keyboard,
            owner_session: crate::owner_session::SessionFollow::default(),
            display_confirm,
            notices: notices.clone(),
            moves: crate::window_move::Moves::new(
                crate::notifications::RoleNotices::of(&notifications),
                std::sync::Arc::clone(&notes),
            ),
            branches: crate::workspace_branch::Branches::default(),
            activities,
            tray,
            wallpapers,
            lock_layouts: options.config.keyboard_layouts.clone(),
            lock_layout: Cell::new(0),
            session_actions: options.session_actions,
            observed: RefCell::default(),
            here_at: Cell::new(0),
            here_seen: Cell::new(0),
            places_seen: RefCell::default(),
            notifications: Rc::clone(&notifications),
            mentions,
            asker,
            agent_door: RefCell::new(agent_door),
            session_doors,
            remote: std::sync::Arc::clone(&remote),
            folder_import: RefCell::new(None),
            sync,
            engine,
            desktops: crate::shared_desktop::SharedDesktops::new(
                std::sync::Arc::clone(&notes),
                options.wake.clone(),
            ),
            notes,
            membership_fed: std::cell::RefCell::new(None),
            landings,
            _channels: channels,
            personal,
            clipboard,
            arrangement,
            underway: busy,
            settings: crate::settings_sync::SettingsSync::new(),
            fleet_renewal: crate::renewal::FleetRenewal::for_joined_device(fabric)
                .map(std::sync::Arc::new),
            overview,
            dictation,
            emoji,
            hints,
            hints_stamp: Cell::default(),
            shortcut_guide,
            chrome,
            people,
            follow: RefCell::default(),
            fullscreen,
            invitable,
            hide_top_bar,
            controller,
            controller_keys,
            app_sheets,
        };
        // A package the person opens authors its own records in the workspace
        // it opens in, as the person there: the workspace's governance gives
        // it before the process is planned, so the app starts able to keep
        // what they do in it.
        // An app of a workspace this device waits for its host to admit it to is not started:
        // whoever asked hears why at once (`Apps::spawn`).
        if let (Some(launch), Some(engine)) = (&shared.launch, &shared.engine) {
            let (engine, handle) = (std::sync::Arc::clone(engine), shared.handle.clone());
            launch.lend_admission(Rc::new(move |workspace: &str| {
                crate::agent_door::awaited_reason(&engine, &handle, workspace)
            }));
        }
        if let Some(launch) = &shared.launch {
            let handle = shared.handle.clone();
            let engine = shared.engine.clone();
            let landings = Rc::clone(&shared.landings);
            let apps = Rc::downgrade(launch);
            launch.lend_authorship(Rc::new(
                move |workspace: &str, app: &str, then: Box<dyn FnOnce(&crate::apps::Apps)>| {
                    let apps = apps.clone();
                    let start = move || {
                        if let Some(apps) = apps.upgrade() {
                            then(&apps);
                        }
                    };
                    let desk = crate::governance::Desk(&handle);
                    let granting =
                        match crate::membership::authorship(engine.as_ref(), &desk, workspace, app)
                        {
                            Ok(Some(granting)) => granting,
                            Ok(None) => return start(),
                            Err(error) => {
                                not_authored(workspace, app, &error);
                                return start();
                            }
                        };
                    // The workspace's governance signs the grant off the loop; the launch starts
                    // once it landed, and asking again then takes the grant onto the desk.
                    let (engine, handle) = (engine.clone(), handle.clone());
                    let (workspace, app) = (workspace.to_owned(), app.to_owned());
                    landings.then(crate::landing::off_loop(granting), move |granted| {
                        let desk = crate::governance::Desk(&handle);
                        let taken = granted
                            .unwrap_or_else(|| Err("the grant was given up".into()))
                            .and_then(|()| {
                                crate::membership::authorship(
                                    engine.as_ref(),
                                    &desk,
                                    &workspace,
                                    &app,
                                )
                            });
                        if let Err(error) = taken {
                            not_authored(&workspace, &app, &error);
                        }
                        start();
                    });
                },
            ));
        }
        shared.start_search();
        // What the person copied is personal: the clipboard publishes the
        // copies itself, as a reading only a caller the workspace grants reads.
        shared
            .services
            .borrow_mut()
            .add(Box::new(crate::clipboard::Served::new(Rc::clone(
                &shared.clipboard,
            ))));
        // What people said to this person is personal too: the centre
        // publishes its rows itself, as a reading only a granted caller
        // reads, and the dismiss and act verbs the panel invokes.
        shared
            .services
            .borrow_mut()
            .add(Box::new(crate::notifications::Served::new(Rc::clone(
                &shared.notifications,
            ))));
        // The session's own diagnostics: the profiler and the heap trim, which
        // the device admin and a person's own scripts read and drive through
        // the same door as the desktop's other actions.
        shared
            .doors
            .add(Rc::new(std::cell::RefCell::new(crate::diagnostics::Served)));
        // The person's devices are what their other machines say of
        // themselves, which is nobody's personal content: their reading is
        // published for any app that may read the session's resources.
        if let Some(fabric) = shared.fabric.clone() {
            shared
                .services
                .borrow_mut()
                .add(Box::new(crate::devices::Served::new(
                    fabric,
                    Rc::clone(&shared.readings),
                    Rc::clone(&shared.devices.chosen),
                    Rc::clone(&shared.handoff),
                    Rc::clone(&shared.devices.pressed),
                    Rc::clone(&shared.devices.handed),
                )));
        }
        // The broker decides a granted reading when the app asks, which is
        // before this loop's first pass: what the services declare is read
        // here, with every service mounted, so a connection that arrives as
        // the session starts meets the workspace's grants and not an empty
        // declaration.
        if let Some(plane) = &action_plane {
            plane.link.declare(&shared.services.borrow());
        }
        // What the portal brings comes from any thread, and wakes the loop
        // instead of waiting for its next turn.
        if let Some(portal) = &options.portal {
            compositor.set_input_observer(Some(portal.input_observer()));
        }
        let loop_wake = compositor.waker();
        // Signing in does not change the login screens; what the person
        // changes here afterwards does, where they let it follow.
        let comforts = options.machine.is_owned().then(|| {
            let wake = loop_wake.clone();
            crate::comforts::Mirror::mount(std::path::PathBuf::from(tilo_auth::SOCKET), move || {
                wake.wake_by_ref();
            })
        });
        let surface_apps = std::mem::take(&mut options.surfaces);
        let slot_views = crate::slot_views::SlotViews::new(
            shared.handle.clone(),
            shared.overlays.clone(),
            options.scale,
        );
        // Assistive technology reads the desktop through the same endpoint an
        // app's window has: what a client asks wakes this loop, which is
        // where every tree and every action lives.
        let wake = loop_wake.clone();
        let a11y = crate::a11y::Accessibility::new(Arc::new(move || wake.wake_by_ref()), aside);
        // A session left locked by one that did not end in order comes back
        // locked, as the person left it.
        let starts_locked = options
            .recovery
            .as_ref()
            .is_some_and(crate::recovery::Recovery::starts_locked);
        let mut session = Self {
            compositor,
            shared,
            banners,
            input: options.config.input,
            setup,
            comforts,
            unlocking: Unlocking {
                user: options.user,
                verifier: options.verifier,
                verdicts: std::sync::mpsc::channel(),
                wake: loop_wake,
            },
            lock_starts: 0,
            lock_view: lock_view.map(crate::lock_screen::LockView::new),
            lock_shown: crate::lock_screen::LockShown::App,
            recovery: options.recovery.take(),
            update_finish: crate::update_finish::Finish::for_session(options.desktop.as_deref()),
            captures: options.captures,
            capture_io,
            capture_studio,
            windows: crate::window_motion::WindowMotions::new(
                (options.presentation)().theme.motion(),
            ),
            quick_settings_panel: crate::quick_settings::PanelMotion::new(
                (options.presentation)().theme.motion(),
            ),
            warmth,
            language: crate::i18n::Language::new(),
            formats: crate::formats::Formats::new(),
            night_light,
            auto_brightness,
            keyboard_preference: options.config.on_screen_keyboard,
            keyboard_run: crate::keyboard::Run::default(),
            keyboard_starts: 0,
            quick_settings_run: crate::quick_settings::Run::Down,
            quick_settings_starts: 0,
            quick_settings_keyboard: crate::quick_settings::PanelKeys::Settled,
            portal: options.portal,
            launches: std::sync::Arc::clone(&options.launches),
            action_plane,
            content: record_content,
            control,
            liveness,
            automation,
            automation_runtime,
            screen_locked: false,
            desktop: options.desktop,
            running: Vec::new(),
            pids: std::collections::BTreeMap::new(),
            window_artwork: std::collections::BTreeMap::new(),
            _settings_watch: settings_watch,
            surfaces,
            surface_apps,
            slots: crate::slots::Slots::default(),
            slot_views,
            displays: crate::display_settings::Displays::default(),
            displays_service,
            readout_place,
            readout,
            numbers,
            published_displays,
            confirmation: crate::display_confirm::Confirmation::default(),
            display_sheet: crate::display_sheet::DisplaySheet::new(
                display_seat,
                options.projector.clone(),
                &options.wake.handle(),
            ),
            projection_video: crate::projection_video::ProjectionVideo::new(),
            rotation: crate::rotation::Rotation::mount(options.accelerometer),
            sources,
            input_method: crate::input_method::InputMethod::default(),
            shell,
            a11y,
            ended: false,
            first_frame: false,
            active_workspace: None,
            published_workspace: crate::published_workspace::Published::none(),
            published_steps: Some(std::sync::Arc::clone(&session_actions)),
            settings_steps: Some(settings_steps),
            kept_at: Instant::now(),
            tool_input: std::time::Duration::ZERO,
            magnifier: (
                settings_store::magnifier::OFF,
                settings_store::magnifier::DEFAULT_ZOOM,
            ),
            machine,
            tabs,
            controllers: options
                .controllers
                .take()
                .map(crate::controllers::Controllers::mount),
            controller_wakes: std::collections::BTreeMap::new(),
            connectors,
        };
        session.watch_sockets()?;
        session.shared.directory_store.waking(options.wake.handle());
        crate::connectors::reconcile(&session);
        if starts_locked {
            session.compositor.lock_now()?;
        }
        tilo_startup_metrics::mark("session-build-done");
        Ok(session)
    }

    /// Make every socket this session serves end its loop's wait: an app that
    /// connects, an importer that asks, a program that drives the desktop is
    /// answered on the pass that follows, not on a tick of the loop.
    ///
    /// # Errors
    /// The loop took no more sources.
    pub(crate) fn watch_sockets(&mut self) -> Result<(), Error> {
        if let Some(tabs) = &mut self.tabs {
            tabs.wake_on(&mut self.compositor)?;
        }
        if let Some(door) = self.shared.agent_door.get_mut() {
            door.wake_on(&mut self.compositor)?;
        }
        if let Some(import) = self.shared.folder_import.get_mut().as_mut() {
            import.wake_on(&mut self.compositor)?;
        }
        Ok(())
    }

    /// A session whose outputs have no display: for tests and screenshots.
    ///
    /// # Errors
    /// The compositor or an output did not start.
    pub fn headless(options: Options, outputs: &[OutputConfig]) -> Result<Self, crate::Error> {
        let mut session = Self::new(options, Compositor::headless)?;
        for output in outputs {
            session.compositor.add_output(output)?;
        }
        session.warm()?;
        Ok(session)
    }

    /// A session on a bare TTY.
    ///
    /// # Errors
    /// The compositor did not start.
    pub fn drm(options: Options, config: DisplayConfig) -> Result<Self, crate::Error> {
        let mut session = Self::new(options, |policy| Compositor::drm(policy, config))?;
        session.warm()?;
        Ok(session)
    }

    /// A session in a window of the desktop it is started from, for
    /// development (see [`tilo_compositor::backend::nested`]).
    ///
    /// # Errors
    /// The compositor or its window did not start.
    pub fn nested(options: Options, config: &NestedConfig) -> Result<Self, crate::Error> {
        // The desktop this session runs inside hands it over as one
        // connection the window takes once, so the window opens before the
        // session builds anything of its own: the desk builds the surface
        // apps' renderers, whose adapter request connects to that desktop,
        // and after that the window would find no connection left.
        let window = tilo_compositor::NestedBackend::open_window(config)?;
        let mut session = Self::new(options, |policy| Compositor::nested(policy, config, window))?;
        session.warm()?;
        Ok(session)
    }

    /// Warm the displays when the person left the night light on, and start
    /// the surface apps: their windows wait for the outputs the session has
    /// now added.
    pub(crate) fn warm(&mut self) -> Result<(), Error> {
        self.follow_displays()?;
        self.follow_fullscreen();
        self.shared.follow_invitable();
        for surface in &self.surface_apps {
            // The lock screen is started when the session locks, the
            // on-screen keyboard when the person may need it and quick
            // settings while its slot is up: what their windows show is never
            // the desk's to open.
            if surface.lock
                || surface.name == crate::keyboard::SURFACE
                || surface.name == crate::quick_settings::SURFACE
            {
                continue;
            }
            let workspace = self.shared.active_workspace_id();
            self.surfaces
                .start_or_log(&mut self.compositor, surface, &workspace);
        }
        Ok(())
    }

    /// Bring the displays in line with the person's arrangement and the way up
    /// the device is held, and advance a change waiting for their answer.
    pub(crate) fn follow_displays(&mut self) -> Result<(), Error> {
        // The panel follows the light in the room while the person asked for
        // it, and the displays are warmed to what the night light shows now;
        // a new output takes both with the displays' arrangement.
        let night_light = self.follow_light()?;
        let turned = self.rotation.follow(&self.compositor.outputs());
        // What the displays service publishes follows what the compositor
        // shows: which ones there are, and which one the person works on.
        self.displays_service
            .land(&self.shared.handle, &self.compositor.outputs());
        // A display with neither a backlight nor DDC/CI keeps its brightness
        // control: the compositor darkens what it shows to it.
        self.displays_service.dim(&mut self.compositor)?;
        // Settings asked which screen is which: each display says its number
        // and name for a moment.
        self.numbers.follow(
            self.compositor.display_snapshot(),
            &self.compositor.outputs(),
        );
        // Where this device's screens stand reaches the person's other
        // devices, and what every device shows is published for the shared
        // arrangement the display sheet and Settings show.
        if let Some(fabric) = &self.shared.fabric {
            self.published_displays
                .follow(fabric, self.compositor.display_snapshot());
        }
        self.displays
            .follow(&mut self.compositor, night_light, turned)
    }

    /// Keep the readout's facts current: where the displays stand, where the
    /// person types, and a mode change waiting for the readout to confirm it.
    pub(crate) fn follow_readout(&self) {
        let caret = self.compositor.text_caret().map(|caret| {
            ui::kurbo::Rect::new(
                caret.x,
                caret.y,
                caret.x + caret.width,
                caret.y + caret.height,
            )
        });
        self.readout_place
            .borrow_mut()
            .observe(&self.compositor.outputs(), caret);
        if let Some(on) = self.shared.notifications.borrow().take_quiet_change() {
            self.show_hint(Hint {
                kind: OsdKind::DoNotDisturb,
                on,
                chord: None,
                name: None,
            });
        }
    }

    /// Put the clipboard panel up beside the caret once the field it was asked in has committed
    /// where its last key left the caret, or once the compositor stops waiting for that commit:
    /// the panel stands under the line the person sees, not one their app drew before.
    pub(crate) fn follow_pending_paste(&mut self) {
        if !self.shared.pending_paste.get() || !self.compositor.caret_settled() {
            return;
        }
        self.shared.pending_paste.set(false);
        // This pass's reading of the caret, which the overlays follow next pass: the panel is
        // placed beside where it stands now, not a pass behind.
        self.shared
            .overlays
            .set_caret(self.readout_place.borrow().caret());
        self.shared.open_clipboard_beside_caret();
    }

    /// Place the surfaces over the desk on the displays as they are now: the
    /// outputs' rectangles, and the one holding the pointer, which is the
    /// display the person works on.
    pub(crate) fn follow_overlays(&self) {
        let outputs: Vec<ui::kurbo::Rect> = self
            .compositor
            .outputs()
            .iter()
            .filter(|output| output.enabled)
            .filter_map(|output| output.rect)
            .map(|(x, y, w, h)| {
                ui::kurbo::Rect::new(
                    f64::from(x),
                    f64::from(y),
                    f64::from(x) + f64::from(w),
                    f64::from(y) + f64::from(h),
                )
            })
            .collect();
        let (x, y) = self.compositor.pointer_location();
        let focused = outputs
            .iter()
            .copied()
            .find(|output| output.contains(ui::kurbo::Point::new(x, y)));
        self.shared.overlays.set_displays(&outputs, focused);
        // Where the person types, for the surfaces a key opens at the caret.
        self.shared
            .overlays
            .set_caret(self.readout_place.borrow().caret());
    }

    /// Take the displays a fullscreen window owns from the compositor, so
    /// the bar on such a display steps aside this pass. The chrome reads
    /// them when it next builds its snapshot, which is what places the
    /// bars.
    pub(crate) fn follow_fullscreen(&self) {
        let monitors = self.compositor.fullscreen_monitors();
        let shell = &self.shared.fullscreen;
        if *shell.borrow() != monitors {
            shell.replace(monitors);
        }
    }

    /// The keyboard input sources, in the person's order.
    #[must_use]
    pub fn input_sources(&self) -> &[String] {
        &self.sources.ids
    }

    /// The input source the keyboard types in now.
    #[must_use]
    pub fn active_input_source(&self) -> &str {
        &self.sources.active
    }

    /// The compositor, to connect clients, route input and read frames.
    pub fn compositor(&mut self) -> &mut Compositor {
        &mut self.compositor
    }

    /// Watch the session's main loop and end the process when it has stopped for good: record why
    /// first, then exit, so the person comes back to the login screen rather than to a frozen
    /// display. The loop reports its turns ([`LoopTurns`]); a turn it never ends is judged by what
    /// the kernel says the thread is doing ([`tilo_liveness::watchdog`]).
    ///
    /// # Errors
    /// The watchdog thread could not be started.
    pub fn guard_main_loop(&mut self) -> std::io::Result<()> {
        let watchdog = Watchdog::watch_this_thread(Policy::COMPOSITOR, |hang| {
            match tilo_crash_records::record_freeze(
                hang.tid,
                hang.blocked_for,
                &hang.report,
                tilo_crash_records::RECORD_WITHIN,
            ) {
                Some(path) => eprintln!("tilo session: hang recorded in {}", path.display()),
                None => eprintln!("tilo session: the record of the hang was not written"),
            }
            end_process(hang);
        })?;
        self.compositor
            .watch_turns(Some(std::sync::Arc::new(Turns(watchdog))));
        Ok(())
    }

    /// The surface apps the session started, and those still running.
    #[must_use]
    pub const fn surfaces(&self) -> &crate::surfaces::Surfaces {
        &self.surfaces
    }

    /// Start `surface` now, as a client of the running compositor, as the app
    /// it runs for in the workspace in front.
    ///
    /// # Errors
    /// Its socket or process could not be made.
    pub fn start_surface(
        &mut self,
        surface: &crate::surfaces::Surface,
    ) -> Result<(), crate::surfaces::StartError> {
        let workspace = self.shared.active_workspace_id();
        self.surfaces
            .start(&mut self.compositor, surface, &workspace)
    }

    /// Stop the surface app `name`, if it runs.
    pub fn stop_surface(&mut self, name: &str) {
        self.surfaces.stop(name);
    }

    /// The surface the session starts as its lock screen, when it has one.
    #[must_use]
    pub fn lock_screen(&self) -> Option<&crate::surfaces::Surface> {
        self.surface_apps.iter().find(|surface| surface.lock)
    }

    /// Ask the surface app `name` to run `action` of `resource`, as one of
    /// the actions it publishes, and name the invocation `id`; `None` when
    /// no such app runs. Its answering carries on in the session's turns
    /// ([`Surfaces::outcome`](surfaces::Surfaces::outcome)).
    pub fn ask_surface(
        &mut self,
        name: &str,
        resource: &str,
        action: &str,
        argument: InvokeArgument,
    ) -> Option<u64> {
        self.surfaces.ask(name, resource, action, argument)
    }

    /// The desktop under the windows and its policy.
    #[must_use]
    pub fn desktop(&self) -> &DesktopHost {
        &self.shared.desktop
    }

    /// The host side of the desktop surface.
    #[must_use]
    pub fn handle(&self) -> &DesktopHandle {
        &self.shared.handle
    }

    /// The notification banners' link: the notifications service publishes
    /// on it and carries out what the banners ask.
    #[must_use]
    pub const fn banners(&self) -> &tilo_notification_banners::Link {
        &self.banners
    }

    /// The desk's notifications: the banners it is showing now and the
    /// centre it keeps behind them.
    #[must_use]
    pub fn notifications(&self) -> tilo_desktop_surface::DesktopNotifications {
        self.shared.notifications.borrow().desktop()
    }

    /// The board the apps show their live activities on, which the bar reads
    /// as its clock.
    #[must_use]
    pub fn live_activities(&self) -> &std::sync::Arc<runtime_collab::LiveActivities> {
        self.shared.activities.board()
    }

    /// The media service the apps reach through the session's plane, while
    /// it serves one: what a call joins through.
    #[cfg(test)]
    pub(crate) fn media_service(&self) -> Option<&tilo_native_media::NativeMediaService> {
        self.action_plane.as_ref()?.calls_service.as_ref()
    }

    /// Reach `surface` (a service the session runs in its process) through
    /// the session's door from now on: its resources are published beside
    /// the desktop's, and keys that ask for its actions reach it.
    pub fn publish(&mut self, surface: tilo_session_core::door::Shared) {
        self.shared.doors.add(surface);
    }

    /// Everything the session publishes now: the desk's resource (its
    /// windows among its children), the chrome's, the surface apps', the
    /// session's own and the services', with their actions.
    #[must_use]
    pub fn resources(&self) -> Vec<ResourceSpec> {
        self.shared.doors.resources()
    }

    /// The windows the overview pictures as the compositor draws them now:
    /// each window's id and the rect its picture rests at, logical
    /// `(x, y, width, height)`, bottom first. Empty while the overview is
    /// away, and before a window on the desk has a picture.
    #[must_use]
    pub fn remote_door(
        &self,
        devices: std::sync::Arc<dyn tilo_runtime::remote_door::PersonsDevices>,
    ) -> tilo_runtime::remote_door::RemoteDoor {
        self.shared.remote.door(devices)
    }

    /// The windows the overview pictures as the compositor draws them now: each window's id and
    /// the rect its picture rests at, logical `(x, y, width, height)` , bottom first. Empty while
    /// the overview is away, and before a window on the desk has a picture.
    #[must_use]
    pub fn overview_pictures(&self) -> Vec<(u64, (f64, f64, f64, f64))> {
        match self.shell.as_ref() {
            Some(shell) => shell.pictures(),
            None => self.shared.overview_pictures(),
        }
    }

    /// What the service publishing `resource` reads of it now, as JSON;
    /// `None` when no service publishes it.
    #[must_use]
    pub fn read(&self, resource: &str) -> Option<serde_json::Value> {
        self.shared.services.borrow().read(resource)
    }

    /// Wait until every device call the services started has landed, and
    /// take them in: what a headless run does before it reads.
    pub fn settle_services(&self) {
        self.shared.services.borrow_mut().settle();
    }

    /// The person's arrangement: the desk this session holds, with its history and undo.
    #[must_use]
    pub fn arrangement(&self) -> &Rc<RefCell<crate::arrangement::Arrangement>> {
        &self.shared.arrangement
    }

    /// The broker this session's apps and agents reach its actions through, while it serves
    /// one: what a test asks for the catalogue an agent working a turn is offered, and invokes
    /// a step of it through, as the agent door does.
    #[cfg(test)]
    #[must_use]
    pub fn action_broker(
        &self,
    ) -> Option<std::sync::Arc<tilo_action_broker::collab_app_broker::AppBrokerState>> {
        self.action_plane.as_ref().and_then(Plane::agent_broker)
    }

    /// The session's copy of `record` of `app` in `workspace` as it is stored: what a test reads
    /// back of Chat's records the session itself writes (its agents, its emoji).
    #[must_use]
    pub fn stored_record(&self, app: &str, workspace: &str, record: &str) -> Option<Vec<u8>> {
        self.shared.records.stored(app, workspace, record)
    }

    /// What the windows ask of the workspace's activity and follow, as this
    /// session serves it: the same door the desk's own pane reads through.
    #[must_use]
    pub fn conversation_runs(&self) -> std::sync::Arc<dyn tilo_broker_core::services::TrackerRuns> {
        self.shared.conversations.runs()
    }

    /// What the windows ask of the workspace's activity and follow, as this session serves it: the
    /// same door the desk's own pane reads through.
    #[must_use]
    pub fn activity(&self) -> tilo_desktop_surface::session::ActivityRequests {
        self.shared.activity.requests()
    }

    /// Undo the person's last arranging step, through the published action
    /// as any caller does.
    ///
    /// # Errors
    /// The door's refusal: there is no step to take back.
    pub fn undo_arrangement(&mut self) -> Result<(), DoorError> {
        let address = ActionAddress::new(crate::arrangement::RECORD_RESOURCE, "undo");
        self.shared.doors.invoke(
            &address,
            &serde_json::Map::new(),
            &mut crate::look::look(&mut self.compositor, Some(&self.content)),
        )?;
        Ok(())
    }

    /// Invoke a published action as `actor` (an agent, a script), beside
    /// [`Self::invoke`].
    ///
    /// # Errors
    /// The door's refusal: no such action, bad arguments, or one the caller
    /// may not run.
    pub fn invoke_with_actor(
        &mut self,
        actor: &str,
        resource: &str,
        action: &str,
        arguments: &serde_json::Value,
    ) -> Result<ResourceOperationStart, DoorError> {
        let empty = serde_json::Map::new();
        let arguments = arguments.as_object().unwrap_or(&empty);
        if let Ok(mut arrangement) = self.shared.arrangement.try_borrow_mut() {
            let _ = &mut arrangement;
        }
        self.shared.doors.invoke_as(
            actor,
            &ActionAddress::new(resource, action),
            arguments,
            &mut crate::look::look(&mut self.compositor, Some(&self.content)),
        )
    }

    /// A window was at `place`: where search's Continue reopens it.
    pub fn record_place(&self, place: tilo_desktop_search::providers::places::Place) {
        self.shared.search.record_place(place);
    }

    /// Invoke the published action `action` of resource `resource`, as
    /// any caller of the action protocol does, and carry out what it asked
    /// of the session. A service's action that runs on the device is
    /// accepted at once; its result lands in the service's reading.
    ///
    /// # Errors
    /// The door's refusal: no such resource or action, bad arguments, or
    /// one the device cannot do now.
    pub fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ResourceOperationStart, DoorError> {
        let address = ActionAddress::new(resource, action);
        let started = self.shared.doors.invoke(
            &address,
            arguments,
            &mut crate::look::look(&mut self.compositor, Some(&self.content)),
        )?;
        self.settle()
            .map_err(|error| DoorError::Failed(error.to_string()))?;
        Ok(started)
    }

    /// Invoke a service's action the person confirmed (restart, power
    /// off): one [`Session::invoke`] refused with
    /// [`DoorError::ConfirmationRequired`].
    ///
    /// # Errors
    /// The door's refusal: no such resource or action, bad arguments, or
    /// one the device cannot do now.
    pub fn invoke_confirmed(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ResourceOperationStart, DoorError> {
        match self
            .shared
            .services
            .borrow_mut()
            .invoke_confirmed(resource, action, arguments)
        {
            Some(started) => started.map(crate::doors::plane::answered),
            None => Err(DoorError::Unknown(ActionAddress::new(resource, action))),
        }
    }

    /// The labels of what the session shows to assistive technology: the
    /// desk's, the chrome's and every open surface app's.
    #[must_use]
    pub fn accessible_labels(&self) -> Vec<String> {
        self.shared.doors.accessible_labels()
    }

    /// The other people whose pointers the desk drew on this session, as the
    /// last frame drew them: their name and the point, in the surface's
    /// coordinates. Drawn over the window each is on, whatever the roster
    /// shows.
    #[must_use]
    pub fn drawn_peer_pointers(&self) -> Vec<(String, (f64, f64))> {
        self.shared.people.borrow().drawn_pointers()
    }

    /// What an assistive client reads of the session's own surfaces now: the
    /// place each surface publishes at, its window name and the tree behind
    /// it, as the AT-SPI exporter hands them over. What a test drives is the
    /// export a screen reader gets, not a parallel description of it.
    #[must_use]
    pub fn a11y_windows(&self) -> Vec<(usize, String, ui::accesskit::TreeUpdate)> {
        self.shared
            .doors
            .offering()
            .into_iter()
            .filter_map(|(place, _)| {
                let (name, tree) =
                    crate::a11y::Accessibility::window_at(&self.shared.doors, place)?;
                Some((place, name, tree))
            })
            .collect()
    }

    /// Perform `action` on `target` of the surface whose window is the
    /// `place`-th the exporter publishes: what a client does over AT-SPI.
    /// Answers whether that surface took the action.
    #[must_use]
    pub fn a11y_perform(
        &mut self,
        place: usize,
        target: ui::accesskit::NodeId,
        action: &ui::a11y::Action,
    ) -> bool {
        crate::a11y::Accessibility::perform(&self.shared.doors, place, target, action)
    }

    /// Whether the person logged out, which ends the session's loop.
    #[must_use]
    pub const fn ended(&self) -> bool {
        self.ended
    }

    /// What stands before the desk runs its own pass: the wait for the clock
    /// finishes itself and gives the layer to what a verified identity opens.
    pub(crate) fn follow_setup(&self) {
        if let Some(setup) = &self.setup {
            setup.borrow_mut().step();
        }
    }

    /// Whether setup is up: it withholds the desktop until it is done.
    #[must_use]
    pub fn setup_up(&self) -> bool {
        self.setup.as_ref().is_some_and(|setup| setup.borrow().up())
    }

    /// Serve the installed folder importer for this session's `display`, the
    /// Wayland socket it bound: whether first use is over, and the review the
    /// person left to be opened once it is.
    ///
    /// Add folders reads the answer before it continues a saved import, so
    /// nothing it carries lands under an identity ceremony still under way.
    ///
    /// # Errors
    /// There is no runtime directory, or the socket cannot be bound.
    pub fn serve_folder_import(&mut self, display: &std::ffi::OsStr) -> std::io::Result<()> {
        let mut continuation = crate::import_readiness::Continuation::serve(display)?;
        // The importer asks as the person clicks Add folders: its socket ends
        // the loop's wait, so the answer is drawn at once.
        continuation.wake_on(&mut self.compositor)?;
        *self.shared.folder_import.borrow_mut() = Some(continuation);
        Ok(())
    }

    /// The verified browser projection a carried-over setup brought: the
    /// browser starts with it.
    #[must_use]
    pub fn browser_continuity(&self) -> Option<std::path::PathBuf> {
        crate::setup::browser_continuity()
    }

    /// Whether quick settings is open.
    #[must_use]
    pub fn quick_settings_open(&self) -> bool {
        self.shared.overlays.is_open(Slot::QuickSettings)
    }

    /// The input preferences the compositor runs with now.
    #[must_use]
    pub const fn input_config(&self) -> &tilo_compositor::InputConfig {
        &self.input
    }

    /// Let each round of asks play out: a round can ask for another (a key's
    /// shortcut the chrome leaves to the session); a few rounds settle any
    /// chain.
    pub(crate) fn settle_asks(&mut self) -> Result<(), Error> {
        // Who may invite whom into the workspace in front is the workspace's
        // answer, and moves while the session runs: the bar's invite button
        // and the roster's footer follow it.
        self.shared.follow_invitable();
        // What the desktop's control hints teach, learn and stay quiet for: setup owns the screen
        // until the person reaches the desktop.
        let setup_up = self.setup.as_ref().is_some_and(|setup| setup.borrow().up());
        self.shared.follow_hints(setup_up);
        // The person's account with this session's host, in the overview of their Tilo, as it
        // changes.
        if let Some(account) = crate::host_account::changed() {
            self.with_chrome_mut(|chrome| {
                chrome.set_tilo(tilo_desktop::overview_tilo::TiloDesk {
                    hosts: vec![account],
                    ..tilo_desktop::overview_tilo::TiloDesk::default()
                });
            });
        }
        for _ in 0..4 {
            self.shared.follow_asks(&mut crate::look::look(
                &mut self.compositor,
                Some(&self.content),
            ));
            // What was plugged in since, and what the person asked of its
            // notice: an open starts Files like any other launch.
            if let Some(drives) = &self.shared.drives {
                drives.follow(self.shared.launch.as_deref());
            }
            // What the desktop asked to open, and what stopped since.
            if let Some(launch) = &self.shared.launch {
                launch.follow();
            }
            // What the tools service has to know of this pass — the media
            // attached and the person's apps — and the sheets it left for the
            // loop, which only a pass can put up.
            if let Some(tools) = self.shared.table.tools() {
                let media = self
                    .shared
                    .drives
                    .as_ref()
                    .map(|drives| drives.live())
                    .unwrap_or_default();
                self.shared.follow_tools(tools, media);
            }
            // The windows this device left open, one at a time, each into the
            // tab it was in.
            self.shared.follow_restore();
            // What the apps asked of their own tabs, and of what a capture
            // may show of them, as the desk commands they are.
            self.follow_app_asks();
            self.shared.follow_gestures();
            self.shared.follow_tab_menus();
            self.follow_window_moves();
            self.shared.follow_branches();
            let locked = self.compositor.locked();
            let ran = {
                let mut look = crate::look::look(&mut self.compositor, Some(&self.content));
                self.shared.run_invocations(locked, &mut look)
            };
            let commands = std::mem::take(&mut *self.shared.commands.borrow_mut());
            let commanded = !commands.is_empty();
            for command in commands {
                self.command(command)?;
            }
            if !ran
                && !commanded
                && self.shared.asks.borrow().is_empty()
                && !self.launches.waiting()
            {
                break;
            }
            // What ran may have queued desktop commands on the shared
            // handle (the chrome switching workspace): the desk carries
            // them out on its next pass.
            self.shared.desktop.with_widgets(WidgetHost::relayout);
        }
        Ok(())
    }

    /// Answer what the apps asked of their own tabs, and of what a capture
    /// may show of them, as the desk commands they are.
    pub(crate) fn follow_app_asks(&mut self) {
        if let Some(tabs) = &mut self.tabs {
            tabs.follow(
                &self.shared,
                &self.shared.desktop,
                self.shared.launch.as_deref(),
                &mut self.compositor,
                self.desktop.as_deref(),
                self.capture_studio.as_ref(),
            );
        }
    }

    /// Note what each window this session runs shows, as the broker knows the
    /// record its app holds: read every pass, so a window that opens a record,
    /// moves in it or closes is named by what it shows now.
    pub(crate) fn read_window_notes(&self) {
        let Some(plane) = &self.action_plane else {
            return;
        };
        let Some(broker) = plane.action_broker() else {
            return;
        };
        // The app each window runs, and the windows the desk shows: a record of another app is
        // not what a window shows, and a window the desk no longer shows says nothing.
        let (apps, live): (
            std::collections::BTreeMap<u64, String>,
            std::collections::BTreeSet<u64>,
        ) = self.shared.handle.read(|tree| {
            let tabs: Vec<(u64, Option<String>)> = tree
                .workspaces
                .iter()
                .flat_map(tilo_desktop_model::Workspace::tabs)
                .map(|tab| {
                    (
                        tab.id,
                        tilo_desktop_surface::host::records::app_of(tree, tab.id)
                            .map(str::to_owned),
                    )
                })
                .collect();
            (
                tabs.iter()
                    .filter_map(|(tab, app)| Some((*tab, app.clone()?)))
                    .collect(),
                tabs.iter().map(|(tab, _)| *tab).collect(),
            )
        });
        self.shared.notes.read(
            &broker.state().window_records(),
            &|instance| apps.get(&instance).cloned(),
            &live,
        );
    }

    /// Hand every agent following the catalogue the session's own actions again when what it
    /// offers moved: a place made, a place closed, a window opened, a notice put up. The rows
    /// are the same the REPL lists, so an agent arranging the screen sees what the person's own
    /// door publishes (shell.md §8.1, "Any action").
    pub(crate) fn follow_session_actions(&self) {
        let Some(actions) = &self.published_steps else {
            return;
        };
        // The rows read here, on the loop that holds the arrangement: nothing off this thread
        // reads a place. They are read again only once the door moved, and what is offered then
        // is compared with what was offered before, so the broker is told only when the rows
        // themselves moved.
        // A held pointer drags or resizes a window: every frame of it moves the desk, never what
        // the steps offer, so the rows are read once the person lets go.
        if self.shared.underway.get().gesture || !actions.door_moved(self.shared.doors.revision()) {
            return;
        }
        let rows = tilo_actions::door::survey_placed(&self.shared.doors.resources());
        if actions.offer(rows)
            && let Some(broker) = self.action_plane.as_ref().and_then(Plane::agent_broker)
        {
            broker.host_steps_changed();
        }
    }

    /// Hand the broker the settings the session publishes again when they moved: what Settings'
    /// window's controls are decided against, read here on the loop that holds the service, so a
    /// catalogue built on the broker's own thread never waits on it.
    pub(crate) fn follow_settings_steps(&self) {
        let Some(steps) = &self.settings_steps else {
            return;
        };
        if steps.offer(&self.shared.services.borrow())
            && let Some(broker) = self.action_plane.as_ref().and_then(Plane::agent_broker)
        {
            broker.host_steps_changed();
        }
    }

    /// Keep the broker's copy of the desk level with the desk as the pass leaves it: the workspaces
    /// the pass opened, and whose each of them is, are decided here and a person's own action is
    /// decided against that copy.
    pub(crate) fn publish_desk(&mut self) {
        let Some(plane) = &self.action_plane else {
            return;
        };
        let desk = std::sync::Arc::clone(&plane.desk);
        self.automation.publish_desk(Some(&desk));
    }

    /// Start the shortcuts whose triggers fired since the last pass, and keep
    /// the apps' broker's copy of the desk level with the desk shown: a run
    /// starting there checks the shortcut against it.
    pub(crate) fn follow_automation(&mut self) {
        if let Some(plane) = &self.action_plane {
            // The broker may take a moment to accept connections after it
            // starts; its runs start from the first pass that finds it.
            if self.automation.needs_broker()
                && let Some(broker) = plane.action_broker()
            {
                self.automation.attach(broker);
            }
            self.automation.settle(Some(&plane.desk));
        } else {
            self.automation.settle(None);
        }
        // The chords the armed shortcuts answer are the table's own chords
        // from here on: the keys claim them for the automation host instead
        // of the desktop, and the guide shows what each runs.
        self.shared
            .shortcut_guide
            .follow_trigger_chords(self.automation.trigger_chords());
        // A chord nothing else answered reaches the automation host, which
        // starts every shortcut whose keyboard trigger names it.
        let pressed: Vec<settings_store::hardware::KeyChord> =
            std::mem::take(&mut *self.shared.triggered.borrow_mut());
        for chord in pressed {
            self.automation.press(&chord);
        }
    }

    /// The shortcut runs that finished and no history recorder took yet,
    /// oldest first: what the history names its changes by.
    #[must_use]
    pub fn take_finished_shortcut_runs(&self) -> Vec<FinishedShortcutRun> {
        self.automation.take_finished()
    }

    /// Ask the system to suspend, restart or shut the machine down.
    pub(crate) fn session_action(&self, action: SessionAction) {
        self.shared.session_action(action);
    }

    /// Lock the session as the person asked: with a key, a command, or
    /// because one of their other devices locked. A session that could not be
    /// left again never locks ([`Session::can_lock`]). The desk goes the
    /// moment the lock is asked for and not when the screen has started, so
    /// nothing of what was on it stays up for the start of a process; the
    /// screen that shows the lock is started here, and asks the compositor for
    /// the lock itself.
    pub(crate) fn lock_by_person(&mut self) -> Result<(), Error> {
        if !self.can_lock() {
            return Ok(());
        }
        self.compositor.lock_now()?;
        self.start_lock_screen();
        Ok(())
    }

    /// Whether this session can lock at all: a password it can check, and a
    /// screen that shows the lock and takes that password. Missing either, a
    /// lock would leave the session with nothing that could lift it, so it
    /// stays unlocked and says which part is missing.
    pub(crate) fn can_lock(&self) -> bool {
        if !self.unlocking.verifier.available() {
            tracing::warn!("no password can be checked here: not locking");
            return false;
        }
        // A session whose lock screen's app is not there still locks: the
        // session draws the lock itself.
        if self.lock_screen().is_none() && self.lock_view.is_none() {
            tracing::warn!("nothing here shows the lock: not locking");
            return false;
        }
        true
    }

    /// Paste what was copied into `destination`, beside what is already there.
    pub(crate) fn paste_files(&self, destination: &str) {
        self.shared.clipboard.paste_files(
            std::path::Path::new(destination),
            crate::clipboard::paste::Conflict::KeepBoth,
        );
    }

    /// Switch the machine to virtual terminal `vt`, as `Ctrl+Alt+F1..F12`
    /// asks. A desktop cannot give the machine back any other way: the
    /// terminal is where the person logs in, ends a stuck program or starts
    /// Tilo again. A backend without virtual terminals has nothing to switch,
    /// and the session stays.
    #[expect(
        clippy::unnecessary_wraps,
        reason = "every arm of the command match answers with one result"
    )]
    pub(crate) fn switch_vt(&mut self, vt: i32) -> Result<(), Error> {
        if let Err(error) = self.compositor.change_vt(vt) {
            tracing::info!(%error, vt, "the virtual terminal was not switched");
        }
        Ok(())
    }

    /// Show a mode change in the readout, said in words: a switch flipped by
    /// a key or a switch has nothing to read as a level.
    pub(crate) fn hint(&self, kind: OsdKind, on: bool) {
        self.show_hint(Hint {
            kind,
            on,
            chord: None,
            name: None,
        });
    }

    /// Show `hint` in the readout.
    pub(crate) fn show_hint(&self, hint: Hint) {
        self.shared.show_readout(&Osd::from(hint));
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // A profile still recording is completed, its files written, before
        // the process goes.
        tilo_frame_metrics::profile::shutdown();
        // The desk the person left, written whatever ended the session.
        self.shared.keep_desk_now();
        // Its apps' display goes with it, and so do they: the apps outlive
        // no session, whoever else still holds what started them.
        if let Some(launch) = &self.shared.launch {
            launch.end_running();
        }
    }
}

/// The loop's turns as the watchdog counts them.
struct Turns(Watchdog);

impl LoopTurns for Turns {
    fn parking(&self) {
        self.0.parking();
    }

    fn unparked(&self) {
        self.0.unparked();
    }
}

/// Say that the app `app` opening in `workspace` was not let author its records there.
fn not_authored(workspace: &str, app: &str, error: &str) {
    tracing::warn!(%workspace, app, %error, "an opening app was not let author its workspace");
}

#[cfg(test)]
mod tests {
    //! What the session's own hosts tell the watchdog.

    use std::time::Duration;

    use tilo_liveness::watchdog::{Policy, Watchdog};

    use super::{LoopTurns as _, Turns};

    /// The compositor's hook marks the loop's turns as the watchdog counts them, so a loop that
    /// waits for events is never judged.
    #[test]
    fn the_compositor_hook_marks_the_loops_turns() {
        let watchdog = Watchdog::watch_this_thread(
            Policy {
                period: Duration::from_millis(50),
                hung_after: Duration::from_secs(60 * 60),
                report_within: Duration::from_secs(1),
            },
            |_| panic!("a loop that marks its turns is never judged hung"),
        )
        .expect("the watchdog thread starts");
        let turns = Turns(watchdog);
        for _ in 0..3 {
            turns.parking();
            std::thread::sleep(Duration::from_millis(20));
            turns.unparked();
        }
    }
}
