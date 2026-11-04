//! Tabs of one app, opened and closed by the app itself.
//!
//! Applications have no tab strips: a second page, file or conversation is a
//! second window of the app in a tab of the desktop's window group. An app
//! asks for that over its authenticated `shell-ui` connection
//! ([`ui::shell_tabs`]): another window right after its own tab, its own tab
//! closed, the last closed tab back, its group stepped on, or its next window
//! placed beside its own. The connection vouches for the app and the
//! workspace it runs in; an app can never place, close or step another app's tab, and a
//! request refused changes nothing.
//!
//! The socket is served here, and the asks become desk commands on the
//! session loop's own pass: the desk is never read from another thread.
//!
//! An app also says here that its windows show a secret right now
//! ([`FloatingSurfaceClientMessage::SetCaptureProtection`]): the session
//! marks the app private for the compositor, which every capture asks, and
//! answers on the same connection that the app may show the secret. The mark
//! lasts while a connection of that app asks for it, and goes when the last
//! one stops asking or closes.
//!
//! Every connection also receives the appearance of the window it belongs to
//! ([`FloatingSurfaceHostMessage::Appearance`]): the temperature of the
//! workspace that window is in, the person's own colour, the theme, and
//! whether the workspace it stands in is in sync. It is sent when the connection
//! opens and again whenever any of that changes.
//!
//! The same connections are made for a window the session opens in an app's
//! running process ([`tilo_app_launch::Connects`]): each window reaches the
//! collaboration broker and the shell-ui socket as itself, so what it asks is
//! answered for its own tab.

use std::collections::BTreeMap;
use std::io::{BufReader, Write};
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use launcher::services::{SHELL_UI_LAUNCH_PROTOCOL, SHELL_UI_SERVICE};
use serde::Deserialize;
use tilo_app_launch::routing;
use tilo_app_launch::{Connects, Launch, Open, WindowConnections};
use tilo_compositor::{Compositor, ReadableWatch};
use tilo_desktop_policy::DesktopPolicy;
use tilo_surface_host::DesktopHost;
use ui::Theme;
use ui::WindowAppearance;
use ui::floating_surface::{
    FLOATING_SURFACE_PROTOCOL_VERSION, FloatingSurfaceClientMessage, FloatingSurfaceHostMessage,
    HostNetworkStatus,
};
use ui::theme::ThemeDocument;
use ui::workspace_sync::WorkspaceSync;

use crate::Launches;
use crate::capture_studio::CaptureStudio;
use crate::desktop_services::DesktopServices;
use crate::mount::apps::Apps;
use crate::mount::host_shell::HostShellAsks;

/// The longest frame an app may send: a paint stream and a marker
/// inventory both fit, and nothing else comes close.
const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// The longest address an app may ask to open in a new tab: as long as the
/// shell's own link opening takes.
const MAX_TARGET_BYTES: usize = 8 * 1024;

/// How the session makes a window's own connections to its services: the
/// collaboration broker and the shell-ui socket, each of the window's own, so
/// what the window asks is answered for it alone.
pub struct ServiceLinks {
    launches: Arc<Launches>,
}

impl ServiceLinks {
    /// The connections of a window of the apps whose launches `launches`
    /// records.
    #[must_use]
    pub fn new(launches: Arc<Launches>) -> Self {
        Self { launches }
    }

    /// The window's collaboration connection, attested as that window: the
    /// record is the session's own, spent by the connection that names the
    /// window's instance.
    fn collaboration(
        &self,
        app: &str,
        workspace: &str,
        instance: u64,
        launch: Option<u32>,
    ) -> Option<OwnedFd> {
        let mut stream = tilo_files::native::socket::connect(&launcher::services::collab_socket())
            .inspect_err(|error| {
                tracing::warn!(%error, %app, "a window could not reach the collaboration broker");
            })
            .ok()?;
        self.launches.window(app, workspace, instance, launch);
        if let Err(error) =
            launcher::services::write_collab_window_handoff(&mut stream, app, workspace, instance)
        {
            self.launches.window_withdrawn(app, workspace, instance);
            tracing::warn!(%error, %app, "a window's collaboration connection was not named");
            return None;
        }
        Some(OwnedFd::from(stream))
    }

    /// The window's shell-ui connection, attributed to the launch whose
    /// process `launcher` is: the app and the workspace it runs in, without
    /// which
    /// what it asks reaches no desk.
    fn shell_ui(&self, app: &str, launcher: Option<u32>) -> Option<OwnedFd> {
        let launcher = launcher?;
        self.launches.window_shell_ui(app, launcher);
        let connected = (|| {
            let mut stream = tilo_files::native::socket::connect(&launcher::services::socket_for(
                SHELL_UI_SERVICE,
            ))?;
            let mut frame = serde_json::to_vec(&serde_json::json!({
                "protocol": SHELL_UI_LAUNCH_PROTOCOL,
                "app_id": app,
                // What the app may know of the network was decided for its
                // launch; the window does not widen it.
                "network_status_allowed": false,
            }))
            .map_err(std::io::Error::other)?;
            frame.push(b'\n');
            stream.write_all(&frame)?;
            stream.flush()?;
            Ok::<_, std::io::Error>(stream)
        })();
        match connected {
            Ok(stream) => Some(OwnedFd::from(stream)),
            Err(error) => {
                self.launches.take_window_shell_ui(app);
                tracing::warn!(%error, %app, "a window could not reach the shell-ui service");
                None
            }
        }
    }
}

impl Connects for ServiceLinks {
    fn window(
        &self,
        app: &str,
        workspace: &str,
        instance: u64,
        launcher_pid: Option<u32>,
    ) -> WindowConnections {
        WindowConnections {
            collaboration: self.collaboration(app, workspace, instance, launcher_pid),
            shell_ui: self.shell_ui(app, launcher_pid),
        }
    }
}

/// The shell-ui socket this session serves: what an app asks about its own
/// tabs arrives here, and the session's loop answers it.
pub struct AppTabs {
    requests: mpsc::Receiver<Request>,
    /// The connections that asked for their app's windows to be withheld
    /// from captures, and the app each speaks for: the mark lasts while one
    /// of them is open.
    withheld: BTreeMap<u64, String>,
    /// The open connections, by the app each vouches for and what it was
    /// last told of its window's appearance.
    links: BTreeMap<u64, Link>,
    /// The questions the person is answering for Terminal's own shell.
    host_shell: HostShellAsks,
    /// The listener the apps connect to, taken off on the session's own pass,
    /// and the socket's path, which goes when the session ends.
    socket: Socket,
}

/// An open shell-ui connection: the app it vouches for, whether its launch
/// let the app know the network's state, and what its window was last told.
struct Link {
    app: String,
    /// Whether this app's launch let it know the network's state; a window
    /// never widens that for itself.
    network_allowed: bool,
    /// Writing to the connection.
    writer: Arc<Mutex<UnixStream>>,
    /// The appearance the window was last told, so a pass that changes
    /// nothing sends nothing.
    told: Option<Delivered>,
}

/// What an appearance message carried, to tell whether a window needs
/// another one.
#[derive(PartialEq)]
struct Delivered {
    window: WindowAppearance,
    theme: ThemeDocument,
    network: Option<HostNetworkStatus>,
    sync: Option<WorkspaceSync>,
}

/// What a connection said that this service answers.
enum Ask {
    /// What the app asks of its own tabs.
    Message(Box<FloatingSurfaceClientMessage>),
    /// The window joined: its side of the connection is open, and it is told
    /// its window's appearance from the next pass on.
    Connected { network_allowed: bool },
    /// The connection ended: what it asked about captures goes with it, and
    /// its window is told no more.
    Closed,
}

/// One ask an app made over its own authenticated connection.
struct Request {
    /// The app the launcher vouched for.
    app: String,
    /// The workspace its launch runs it in.
    workspace: String,
    /// The connection the ask came in on, which its answer goes back on and
    /// which its capture protection lasts as long as.
    connection: u64,
    /// Writing to that connection.
    reply: Arc<Mutex<UnixStream>>,
    ask: Ask,
}

impl AppTabs {
    /// Serve the shell-ui socket, waking the session's loop with each ask.
    ///
    /// # Errors
    /// The service's directory cannot be made, or its socket cannot be bound
    /// or served.
    pub fn start(launches: Arc<Launches>, wake: tilo_service::Wake) -> Result<Self, String> {
        let root = launcher::services::ensure_services_root()
            .map_err(|error| format!("create the shell-ui service directory: {error}"))?;
        let path = root.join(format!("{SHELL_UI_SERVICE}.sock"));
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|error| format!("remove the stale shell-ui socket: {error}"))?;
        }
        let listener = tilo_files::native::socket::bind(&path)
            .map_err(|error| format!("bind the shell-ui socket {}: {error}", path.display()))?;
        let (requests, answers) = mpsc::channel();
        let socket = Socket::serve(listener, path, launches, requests, wake)?;
        Ok(Self {
            requests: answers,
            withheld: BTreeMap::new(),
            links: BTreeMap::new(),
            host_shell: HostShellAsks::default(),
            socket,
        })
    }

    /// End the loop's wait when an app connects, so its window's first ask is
    /// answered on the pass that follows.
    ///
    /// # Errors
    /// The loop took no more sources.
    pub(crate) fn wake_on(&mut self, compositor: &mut Compositor) -> std::io::Result<()> {
        self.socket.watch = Some(crate::sockets::watch(compositor, &self.socket.listener)?);
        Ok(())
    }

    /// Answer what the apps asked since the last pass: each ask is a desk
    /// command, so the desk is read and changed on this thread alone.
    pub(crate) fn follow(
        &mut self,
        desktop: &DesktopHost,
        apps: Option<&Apps>,
        compositor: &mut Compositor,
        services: Option<&DesktopServices>,
        studio: Option<&CaptureStudio>,
    ) {
        // The windows that connected since the last pass join here, so an ask
        // they write is answered on this pass and no thread of the session's
        // waits on a client.
        self.socket.take();
        let mut answered = Answers {
            desktop,
            apps,
            compositor,
            services,
            studio,
            withheld: &mut self.withheld,
            links: &mut self.links,
        };
        for request in self.requests.try_iter() {
            answered.answer(&mut self.host_shell, request);
        }
        // What the person did in the windows' bars goes back to the app whose
        // connection declared the slots there.
        answered.deliver_bar_events();
        // A control of a window's own content, pressed where the desk lists
        // it, goes to the app drawing that window.
        answered.deliver_target_presses();
        // The person may have answered a question about the device's own
        // shell since the last pass.
        self.host_shell.settle(services);
    }

    /// Give every open window the appearance of the desk it stands on: the
    /// temperature of its workspace, the person's colour, the theme, and its
    /// workspace's sync state, each when it differs from what that window
    /// was last told. A window opens with one, and gets another whenever any of
    /// it changes; a pass that changes nothing writes nothing.
    pub fn publish_window_warmth(
        &mut self,
        appearance_of: impl Fn(&str) -> WindowAppearance,
        sync_of: impl Fn(&str) -> Option<WorkspaceSync>,
        theme: &Theme,
        network: HostNetworkStatus,
    ) {
        for link in self.links.values_mut() {
            let network = link.network_allowed.then_some(network);
            let window = appearance_of(&link.app);
            let sync = sync_of(&link.app);
            let unchanged = link.told.as_ref().is_some_and(|last| {
                last.window == window
                    && last.theme == theme.document
                    && last.network == network
                    && last.sync == sync
            });
            if unchanged {
                continue;
            }
            let mut message = FloatingSurfaceHostMessage::appearance_with_theme(&window, theme);
            if let FloatingSurfaceHostMessage::Appearance {
                network_status,
                workspace_sync,
                ..
            } = &mut message
            {
                *network_status = network;
                workspace_sync.clone_from(&sync);
            }
            let written = link
                .writer
                .lock()
                .map_err(|_| "the connection's writer is gone".to_string())
                .and_then(|mut stream| {
                    write_message(&mut stream, &message).map_err(|error| error.to_string())
                });
            if let Err(error) = written {
                tracing::warn!(%error, app = %link.app, "a window's appearance was not written");
                continue;
            }
            link.told = Some(Delivered {
                window,
                theme: theme.document.clone(),
                network,
                sync,
            });
        }
    }
}

/// What one pass of answers reaches: the desk, the services the person
/// answers through, and the connections the answers go back on.
struct Answers<'a> {
    desktop: &'a DesktopHost,
    apps: Option<&'a Apps>,
    compositor: &'a mut Compositor,
    services: Option<&'a DesktopServices>,
    studio: Option<&'a CaptureStudio>,
    withheld: &'a mut BTreeMap<u64, String>,
    links: &'a mut BTreeMap<u64, Link>,
}

impl Answers<'_> {
    /// Answer one ask of the app it came from: an ask never changes another
    /// app's tab, and an app whose ask this session cannot answer is told so
    /// rather than left waiting.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per message, so every one of them is answered here"
    )]
    fn answer(&mut self, host_shell: &mut HostShellAsks, request: Request) {
        let Request {
            app,
            workspace,
            connection,
            reply,
            ask,
        } = request;
        let message = match ask {
            Ask::Closed => {
                withhold(
                    self.withheld,
                    self.desktop,
                    self.compositor,
                    connection,
                    &app,
                    false,
                );
                self.links.remove(&connection);
                // What this connection declared for its window's bar goes
                // with it, as does the question it waits on.
                self.desktop
                    .with_policy(|policy| policy.handle().bar_connection_closed(connection));
                host_shell.closed(connection);
                return;
            }
            Ask::Connected { network_allowed } => {
                self.links.insert(
                    connection,
                    Link {
                        app,
                        network_allowed,
                        writer: reply,
                        told: None,
                    },
                );
                return;
            }
            Ask::Message(message) => *message,
        };
        let anchor = self
            .desktop
            .with_policy(|policy| policy.anchor_for(&app).map(|(tab, ..)| tab));
        let name = name_of(&message);
        match message {
            FloatingSurfaceClientMessage::OpenTabBeside {
                target, background, ..
            } => self.answer_open_beside(&app, &workspace, target, background, anchor),
            FloatingSurfaceClientMessage::CloseTab { .. } => {
                self.desktop.with_policy(|policy| policy.close_tab_of(&app));
            }
            FloatingSurfaceClientMessage::ReopenClosedTab => {
                self.desktop.with_policy(DesktopPolicy::reopen_closed);
            }
            FloatingSurfaceClientMessage::CycleTab { forward, .. } => {
                self.desktop
                    .with_policy(|policy| policy.cycle_tab(&app, forward));
            }
            FloatingSurfaceClientMessage::PlaceNextWindowBeside { background, .. } => {
                self.desktop
                    .with_policy(|policy| policy.place_next_beside(&app, anchor, background));
            }
            FloatingSurfaceClientMessage::SetCaptureProtection {
                protected,
                generation,
            } => self.answer_protection(connection, &app, &reply, protected, generation),
            FloatingSurfaceClientMessage::Toolbar { toolbar, .. } => {
                // The slots this app shows in its window's bar, where its
                // window is a tab of the desktop's.
                match anchor {
                    Some(tab) => self.desktop.with_policy(|policy| {
                        policy.handle().declare_bar(tab, connection, toolbar);
                    }),
                    None => refuse(&reply, &app, &name, NO_WINDOW),
                }
            }
            FloatingSurfaceClientMessage::WindowTargets { targets, .. } => {
                // The controls of the window's own content, where its window
                // is a tab of the desktop's, so a session names and presses
                // them as it names the bar's.
                match anchor {
                    Some(tab) => self.desktop.with_policy(|policy| {
                        policy.handle().declare_targets(tab, connection, targets);
                    }),
                    None => refuse(&reply, &app, &name, NO_WINDOW),
                }
            }
            FloatingSurfaceClientMessage::EditLocation { .. } => match anchor {
                Some(tab) => self
                    .desktop
                    .with_policy(|policy| policy.handle().edit_bar_location(tab)),
                None => refuse(&reply, &app, &name, NO_WINDOW),
            },
            FloatingSurfaceClientMessage::WindowOffer { offer, .. } => match anchor {
                Some(tab) => self.desktop.with_policy(|policy| {
                    policy.handle().offer_bar(tab, connection, offer);
                }),
                None => refuse(&reply, &app, &name, NO_WINDOW),
            },
            FloatingSurfaceClientMessage::OpenSearch { opening, .. } => match anchor {
                Some(tab) => self
                    .desktop
                    .with_policy(|policy| policy.handle().open_bar_search(tab, opening)),
                None => refuse(&reply, &app, &name, NO_WINDOW),
            },
            FloatingSurfaceClientMessage::OpenAppSearch { request, .. } => match anchor {
                Some(tab) => self
                    .desktop
                    .with_policy(|policy| policy.handle().search_app(tab, request)),
                None => refuse(&reply, &app, &name, NO_WINDOW),
            },
            FloatingSurfaceClientMessage::AllowHostShell => {
                // The person allows the device's own shell, and the runtime
                // (`ush`) records their answer; the app hears it here.
                host_shell.ask(self.services, &app, connection, Arc::clone(&reply));
            }
            FloatingSurfaceClientMessage::OpenCaptureStudio { mode } => match self.studio {
                Some(studio) => studio.open_asked(mode),
                None => refuse(&reply, &app, &name, NO_CAPTURE),
            },
            // A window paints its own action markers, at the control an agent
            // would press, so nothing here draws a second one; and the session
            // names none of them to assistive technology or opens what one
            // points at, so the inventory is read and adds nothing.
            FloatingSurfaceClientMessage::ActionMarkers { .. } => {}
            // A picture in a picture and an app's second surface of its own
            // belong to a shell that floats surfaces over the desktop. This
            // session places every window of an app in a tab, and has no layer
            // to float one in.
            FloatingSurfaceClientMessage::Present { .. }
            | FloatingSurfaceClientMessage::UpdateRegion { .. }
            | FloatingSurfaceClientMessage::SurfaceGeometry { .. }
            | FloatingSurfaceClientMessage::SourceCleared { .. }
            | FloatingSurfaceClientMessage::DestinationPresented { .. }
            | FloatingSurfaceClientMessage::Suspend { .. }
            | FloatingSurfaceClientMessage::Dismiss { .. } => {
                refuse(&reply, &app, &name, NO_FLOATING);
            }
            // What a window shares live, the embeds it draws of other apps'
            // resources, and the selection an agent works from: the session
            // serves none of them over this connection yet.
            FloatingSurfaceClientMessage::WindowRecord { .. }
            | FloatingSurfaceClientMessage::Embeds { .. }
            | FloatingSurfaceClientMessage::SharedWindows { .. } => {
                refuse(&reply, &app, &name, NO_WINDOW_SHARE);
            }
            // The resource a window shows is what the composer asks the
            // agent about it, so it goes with the selection it seeds.
            FloatingSurfaceClientMessage::SelectionCapable
            | FloatingSurfaceClientMessage::WindowResource { .. }
            | FloatingSurfaceClientMessage::AskAgent { .. }
            | FloatingSurfaceClientMessage::Pointed { .. }
            | FloatingSurfaceClientMessage::PickAnswered { .. } => {
                refuse(&reply, &app, &name, NO_AGENT);
            }
            // What an app's page may be shared as, instead of video streamed
            // from it: the session asks no window to paint its page.
            FloatingSurfaceClientMessage::PaintCapable
            | FloatingSurfaceClientMessage::PaintStream { .. } => {
                refuse(&reply, &app, &name, NO_PAINT);
            }
            // The device's displays are placed in Settings, and its own shell
            // is what a window of Terminal asks about: neither is asked here.
            FloatingSurfaceClientMessage::MoveDeviceDisplay { .. }
            | FloatingSurfaceClientMessage::DeviceShellOutcome { .. } => {
                refuse(&reply, &app, &name, NO_DEVICE);
            }
            // What is kept of a window's changes, in the person's words: the
            // session keeps no model to describe them with.
            FloatingSurfaceClientMessage::DescribeHistory { .. } => {
                refuse(&reply, &app, &name, NO_HISTORY);
            }
            // The app under the pointer did not scroll with a sideways wheel:
            // the strip its window stands in pans by it, as a wheel over the
            // strip's own canvas does. Nothing to answer — the app took what
            // it scrolled with, and this is what it left.
            FloatingSurfaceClientMessage::SidewaysWheel { travel } => {
                if let Some(tab) = anchor {
                    self.desktop
                        .with_policy(|policy| policy.handle().sideways_wheel(tab, travel));
                }
            }
            // The connection said its version when it opened; a second one
            // is answered with the one this session speaks.
            FloatingSurfaceClientMessage::Hello { .. } => refuse(&reply, &app, &name, SAID),
        }
    }

    /// Open the place a window of `app` asked for: beside the one that
    /// asked, and a launch that starts nothing takes the place back.
    fn answer_open_beside(
        &self,
        app: &str,
        workspace: &str,
        target: Option<String>,
        background: bool,
        anchor: Option<u64>,
    ) {
        let Some(apps) = self.apps else {
            return;
        };
        let open = if let Some(target) = target {
            let Some(open) = target_open(app, &target) else {
                tracing::info!(%app, "an app asked to open an address it does not claim");
                return;
            };
            Some(open)
        } else {
            None
        };
        let launch = Launch::Package {
            app: app.to_owned(),
            open,
        };
        // The place it opens into is there from this frame; a launch that
        // starts nothing takes it back.
        let held = self
            .desktop
            .with_policy(|policy| policy.hold_beside(app, anchor, background));
        if let Err(error) = apps.start_beside(&launch, &apps.workspace_named(workspace), None) {
            if held {
                self.desktop.with_policy(|policy| policy.release_held(app));
            }
            tracing::warn!(%error, %app, "a window the app asked for did not start");
        }
    }

    /// Withhold `app`'s windows from captures, as its own ask narrows what
    /// leaves the display and never widens it, and confirm the change.
    fn answer_protection(
        &mut self,
        connection: u64,
        app: &str,
        reply: &Arc<Mutex<UnixStream>>,
        protected: bool,
        generation: Option<u64>,
    ) {
        withhold(
            self.withheld,
            self.desktop,
            self.compositor,
            connection,
            app,
            protected,
        );
        if let Some(generation) = generation {
            tell(
                reply,
                &FloatingSurfaceHostMessage::CaptureProtectionApplied {
                    generation,
                    protected,
                },
                app,
            );
        }
        tracing::debug!(connection, %app, protected, "capture protection asked");
    }

    /// Write what the person did in the windows' bars to the app whose
    /// connection declared the slots there.
    fn deliver_bar_events(&self) {
        let events = self
            .desktop
            .with_policy(|policy| policy.handle().take_bar_events());
        for (connection, event) in events {
            let Some(link) = self.links.get(&connection) else {
                continue;
            };
            // The app had been sent no input of this session's when the
            // person used its bar: it runs the event with the next input it
            // reads.
            tell(
                &link.writer,
                &FloatingSurfaceHostMessage::ToolbarEvent {
                    event,
                    after_input: None,
                },
                &link.app,
            );
        }
    }

    /// Write the controls pressed in the windows' own content to the apps
    /// whose connections described them
    /// ([`ui::floating_surface::FloatingSurfaceClientMessage::WindowTargets`]),
    /// which answer as an assistive technology's activation does.
    fn deliver_target_presses(&self) {
        let presses = self
            .desktop
            .with_policy(|policy| policy.handle().take_target_presses());
        for (connection, name, role, action) in presses {
            let Some(link) = self.links.get(&connection) else {
                continue;
            };
            tell(
                &link.writer,
                &FloatingSurfaceHostMessage::PressTarget { name, role, action },
                &link.app,
            );
        }
    }
}

/// An app asked of a window this session shows in no tab: it has none open,
/// or it is not the app of the tab that asked.
const NO_WINDOW: &str = "this session shows no window of that app";

/// An app asked for the capture studio where the session has none.
const NO_CAPTURE: &str = "this session has no capture studio";

/// An app asked to float a surface over the desktop, which this session has
/// no layer for.
const NO_FLOATING: &str = "this session floats no surface over its windows";

/// An app asked to share a window live or embed another's resource.
const NO_WINDOW_SHARE: &str = "this session shares no window live";

/// An app asked about what a person selected, and for an agent to work from
/// it.
const NO_AGENT: &str = "this session asks no window about a selection";

/// An app offered its page as a paint stream.
const NO_PAINT: &str = "this session asks no window to paint its page";

/// An app asked to move a device's display, or answered a device-shell
/// operation.
const NO_DEVICE: &str = "this session moves no device display here";

/// An app asked for its changes described.
const NO_HISTORY: &str = "this session describes no history";

/// A second greeting on a connection whose version was read when it opened.
const SAID: &str = "this connection said its version when it opened";

/// The name `message` is sent under, as an app reads it in
/// [`FloatingSurfaceHostMessage::Unanswered`].
fn name_of(message: &FloatingSurfaceClientMessage) -> String {
    match serde_json::to_value(message) {
        Ok(serde_json::Value::Object(fields)) => fields
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        _ => String::new(),
    }
}

/// Tell the app on `reply` that this session does not answer `name`, and
/// why, so nothing of the app's waits on an answer that will not come.
fn refuse(reply: &Arc<Mutex<UnixStream>>, app: &str, name: &str, reason: &str) {
    tell(
        reply,
        &FloatingSurfaceHostMessage::Unanswered {
            message: name.to_owned(),
            reason: reason.to_owned(),
        },
        app,
    );
}

/// Write `message` to the app on its connection, saying so when it cannot be
/// written.
fn tell(reply: &Arc<Mutex<UnixStream>>, message: &FloatingSurfaceHostMessage, app: &str) -> bool {
    let written = reply
        .lock()
        .map_err(|_| "the connection's writer is gone".to_string())
        .and_then(|mut stream| {
            write_message(&mut stream, message).map_err(|error| error.to_string())
        });
    match written {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(%error, %app, "a shell-ui answer was not written");
            false
        }
    }
}

/// Mark the windows of `app` private (`private`) or showable again, as
/// `connection` asks: the mark lasts while one of the app's connections asks
/// for it, so it goes when the last one stops asking or closes.
fn withhold(
    withheld: &mut BTreeMap<u64, String>,
    desktop: &DesktopHost,
    compositor: &mut Compositor,
    connection: u64,
    app: &str,
    private: bool,
) {
    let asked = withheld.remove(&connection).is_some();
    if private {
        withheld.insert(connection, app.to_owned());
        compositor.protect_app(app, true);
    } else if asked && !withheld.values().any(|named| named == app) {
        compositor.protect_app(app, false);
    }
    // The desk knows the same apps, so nothing it names for the compositor to
    // draw is named for an app that keeps its content, and a card in the
    // switcher or the overview says so instead of showing what the window
    // holds back.
    desktop.set_withheld_apps(withheld.values().cloned().collect());
}

/// The address `app` asks to open, when its manifest claims it: a link it
/// declares the scheme of, as its own menu would open one.
fn target_open(app: &str, target: &str) -> Option<Open> {
    if target.is_empty()
        || target.len() > MAX_TARGET_BYTES
        || target.trim() != target
        || target.chars().any(char::is_control)
    {
        return None;
    }
    let scheme = routing::scheme_of(target)?;
    let installed = tilo_installed_apps::read_one_from_env(app).ok().flatten()?;
    installed
        .opens_uri_schemes
        .iter()
        .any(|claims| claims.eq_ignore_ascii_case(&scheme))
        .then(|| Open::Uri(target.to_owned()))
}

/// The listener the apps connect to, its path, and what a connection taken
/// off it is served with.
///
/// The connections are taken on the session's own pass ([`Socket::take`]): a
/// thread waiting in `accept` could only be joined once a client woke it, and
/// a session another one took the path from, or one that outlives its clients,
/// would never be woken at all.
struct Socket {
    listener: UnixListener,
    /// What ends the loop's wait while an app connects or writes.
    watch: Option<ReadableWatch>,
    path: PathBuf,
    /// The socket standing at `path` when it was bound: a session that takes
    /// the path later binds one of its own, which this one leaves standing.
    standing: Option<(u64, u64)>,
    /// The connection number the next connection taken is given.
    next: AtomicU64,
    /// The launches a connection is checked against, the channel its asks
    /// arrive on, and the wake that brings the session to answer them.
    launches: Arc<Launches>,
    requests: mpsc::Sender<Request>,
    wake: tilo_service::Wake,
}

impl Socket {
    fn serve(
        listener: UnixListener,
        path: PathBuf,
        launches: Arc<Launches>,
        requests: mpsc::Sender<Request>,
        wake: tilo_service::Wake,
    ) -> Result<Self, String> {
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("serve the shell-ui socket: {error}"))?;
        Ok(Self {
            standing: standing(&path),
            listener,
            watch: None,
            path,
            next: AtomicU64::new(1),
            launches,
            requests,
            wake,
        })
    }

    /// Take every connection waiting, each served on a thread of its own: what
    /// it asks arrives on the session's channel, for the session to answer on
    /// this pass.
    fn take(&self) {
        loop {
            let stream = match self.listener.accept() {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    tracing::warn!(%error, "the shell-ui service could not accept");
                    return;
                }
            };
            let connection = self.next.fetch_add(1, Ordering::Relaxed);
            let (launches, requests, wake) = (
                Arc::clone(&self.launches),
                self.requests.clone(),
                Arc::clone(&self.wake),
            );
            if let Err(error) = std::thread::Builder::new()
                .name(format!("tilo-shell-ui-{connection}"))
                .spawn(move || serve_connection(connection, stream, &launches, &requests, &wake))
            {
                // Nobody serves this connection: the app it belongs to waits.
                tracing::warn!(%error, connection, "a shell-ui connection worker could not start");
            }
        }
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        // A session started while this one ran takes the path for its own: the
        // socket standing there is that session's, and stays.
        if standing(&self.path) == self.standing {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// The socket standing at `path`, as the file system names it: the device and
/// inode of the node there. A bound socket's own file descriptor names an
/// inode of the socket layer, not this one.
fn standing(path: &std::path::Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::symlink_metadata(path).ok()?;
    Some((meta.dev(), meta.ino()))
}

/// The launcher's word for a connection, written before the sandbox starts:
/// which app it is, as the launcher knows it, and whether that launch let
/// the app know the network's state.
#[derive(Deserialize)]
struct LauncherHandshake {
    protocol: u16,
    app_id: String,
    /// Absent from a launcher that predates the field: such an app is told
    /// nothing of the network.
    #[serde(default)]
    network_status_allowed: bool,
}

/// The app's first frame as every shell-ui version reads it:
/// `{"type": "hello", "version": N}`, whatever else a later version adds.
#[derive(Deserialize)]
struct Hello {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    version: Option<u16>,
}

/// The shell-ui version `frame` says, when it is a hello.
fn hello_version(frame: &[u8]) -> Option<u16> {
    serde_json::from_slice::<Hello>(frame)
        // ignored-failure: a frame that does not read back is not a hello
        .ok()
        .filter(|hello| hello.kind == "hello")
        .and_then(|hello| hello.version)
}

// The channel handles are deliberately owned by the per-connection worker,
// which outlives the pass that took the connection off the socket.
#[allow(
    clippy::needless_pass_by_value,
    reason = "the per-connection worker owns its handles and outlives the session's pass"
)]
fn serve_connection(
    connection: u64,
    stream: UnixStream,
    launches: &Launches,
    requests: &mpsc::Sender<Request>,
    wake: &tilo_service::Wake,
) {
    // The process at the other end, as the socket says it: a window the app
    // opened, or the session's own connection for one it opened in the app.
    let peer = peer_pid(&stream);
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let Some(handshake_frame) = wire_frame::read_line(&mut reader, MAX_FRAME_BYTES)
        .ok()
        .flatten()
    else {
        return;
    };
    // A client that does not speak this protocol is dropped.
    // ignored-failure: a handshake that does not read back is not one of ours
    let Ok(handshake) = serde_json::from_slice::<LauncherHandshake>(&handshake_frame) else {
        return;
    };
    if handshake.protocol != SHELL_UI_LAUNCH_PROTOCOL
        || handshake.app_id.trim().is_empty()
        || handshake.app_id.len() > 255
    {
        return;
    }
    // The app's first frame says the version it speaks; nothing else is read
    // from an app at another one.
    let Ok(Some(hello)) = wire_frame::read_line(&mut reader, MAX_FRAME_BYTES) else {
        return;
    };
    let app_version = hello_version(&hello);
    if app_version != Some(FLOATING_SURFACE_PROTOCOL_VERSION) {
        refuse_version(&mut writer, &handshake.app_id, app_version);
        return;
    }
    let app = handshake.app_id;
    let Some(workspace) = launched_workspace(launches, peer, &app) else {
        tracing::info!(%app, "a shell-ui connection names no app this session started");
        return;
    };
    let reply = Arc::new(Mutex::new(writer));
    // From here the window is told the appearance of the desk it stands on,
    // and it is told once more whenever that changes.
    if requests
        .send(Request {
            app: app.clone(),
            workspace: workspace.clone(),
            connection,
            reply: Arc::clone(&reply),
            ask: Ask::Connected {
                network_allowed: handshake.network_status_allowed,
            },
        })
        .is_err()
    {
        return;
    }
    wake();
    loop {
        let frame = match wire_frame::read_line(&mut reader, MAX_FRAME_BYTES) {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(%error, %app, "a shell-ui frame was refused");
                break;
            }
        };
        let Ok(message) = serde_json::from_slice::<FloatingSurfaceClientMessage>(&frame) else {
            tracing::warn!(%app, "a shell-ui message was refused");
            break;
        };
        // Every message is answered: what this session acts on it acts on,
        // and what it cannot is told to the app
        // ([`FloatingSurfaceHostMessage::Unanswered`]), never left waiting.
        if requests
            .send(Request {
                app: app.clone(),
                workspace: workspace.clone(),
                connection,
                reply: Arc::clone(&reply),
                ask: Ask::Message(Box::new(message)),
            })
            .is_err()
        {
            break;
        }
        wake();
    }
    // What this connection asked of what a capture may show goes with it.
    let _ = requests.send(Request {
        app: app.clone(),
        workspace,
        connection,
        reply,
        ask: Ask::Closed,
    });
    wake();
    tracing::debug!(connection, %app, "shell-ui connection closed");
}

/// The process at the other end of `stream`, as the kernel names it: the
/// app's own process, or the session itself for a window it connected.
fn peer_pid(stream: &UnixStream) -> Option<u32> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>()).ok()?;
    // SAFETY: both pointers name initialized storage of `length` bytes and
    // the socket stays open for the duration of `getsockopt`.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::from_mut(&mut credentials).cast(),
            std::ptr::from_mut(&mut length),
        )
    };
    if result != 0 || usize::try_from(length).ok()? < std::mem::size_of::<libc::ucred>() {
        return None;
    }
    u32::try_from(credentials.pid).ok().filter(|pid| *pid > 0)
}

/// The workspace the app the connection names runs in, when this session
/// started it: the launch the peer's process belongs to.
fn launched_workspace(launches: &Launches, peer: Option<u32>, app: &str) -> Option<String> {
    let pid = match peer {
        // The session connects a window it opens in an app's running process
        // itself: what it recorded for that connection names the launch.
        Some(pid) if pid == std::process::id() => launches.take_window_shell_ui(app)?,
        peer => peer?,
    };
    launches.workspace_of(pid, app)
}

/// Tell an app speaking another shell-ui version which one this session
/// speaks, so the one behind can be updated.
fn refuse_version(writer: &mut UnixStream, app: &str, app_version: Option<u16>) {
    if let Err(error) = write_message(
        writer,
        &FloatingSurfaceHostMessage::VersionMismatch {
            shell: FLOATING_SURFACE_PROTOCOL_VERSION,
        },
    ) {
        tracing::warn!(%error, app, "an app was not told this session speaks another shell-ui version");
    }
    tracing::info!(
        %app,
        ?app_version,
        shell = FLOATING_SURFACE_PROTOCOL_VERSION,
        "shell-ui version mismatch"
    );
}

/// Write one message to an app, as the one-line frame every shell-ui version
/// reads.
fn write_message(
    writer: &mut UnixStream,
    message: &FloatingSurfaceHostMessage,
) -> std::io::Result<()> {
    let mut frame = serde_json::to_vec(message).map_err(std::io::Error::other)?;
    frame.push(b'\n');
    writer.write_all(&frame).and_then(|()| writer.flush())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the launcher writes before an app's sandbox starts.
    fn handshake(app: &str) -> Vec<u8> {
        let mut frame = serde_json::to_vec(&serde_json::json!({
            "protocol": SHELL_UI_LAUNCH_PROTOCOL,
            "app_id": app,
            "network_status_allowed": false,
        }))
        .unwrap();
        frame.push(b'\n');
        frame
    }

    /// A socket on a path of the test's own, with the channel its asks land
    /// on and a wake that does nothing.
    fn socket_at(path: &std::path::Path) -> (Socket, mpsc::Receiver<Request>) {
        let (requests, answers) = mpsc::channel();
        let socket = Socket::serve(
            tilo_files::native::socket::bind(path).unwrap(),
            path.to_path_buf(),
            Arc::new(Launches::default()),
            requests,
            Arc::new(|| {}),
        )
        .unwrap();
        (socket, answers)
    }

    /// A session started while another ran takes the shell-ui path for itself.
    /// The one that lost it ends without a client waking a thread for it, and
    /// leaves the socket standing there alone.
    #[test]
    fn a_session_ending_leaves_the_socket_another_one_took() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shell-ui.sock");
        let (first, _first_answers) = socket_at(&path);
        std::fs::remove_file(&path).unwrap();
        let (second, _second_answers) = socket_at(&path);
        // Nothing connects to the socket the first one bound: ending it
        // returns all the same, on the thread that serves it.
        drop(first);
        assert!(path.exists(), "the second session's socket stands");
        drop(second);
        assert!(
            !path.exists(),
            "the socket goes with the session that bound it"
        );
    }

    /// A window that connected before the pass is taken off the socket by it,
    /// and is served from there on.
    #[test]
    fn a_connection_waiting_is_taken_on_the_sessions_pass() {
        use std::io::{Read as _, Write as _};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shell-ui.sock");
        let (socket, answers) = socket_at(&path);
        let mut client = tilo_files::native::socket::connect(&path).unwrap();
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        socket.take();
        let mut frames = handshake("so.tilo.notes");
        let mut hello = serde_json::to_vec(&FloatingSurfaceClientMessage::Hello {
            version: FLOATING_SURFACE_PROTOCOL_VERSION,
        })
        .unwrap();
        hello.push(b'\n');
        frames.extend_from_slice(&hello);
        client.write_all(&frames).unwrap();
        // An app this session started nothing of is told nothing, and the
        // connection closes.
        let mut rest = Vec::new();
        client.read_to_end(&mut rest).unwrap();
        assert!(rest.is_empty(), "nothing is written to that connection");
        assert!(
            answers.try_recv().is_err(),
            "no ask of an app this session never started"
        );
    }

    /// What the session cannot do of an ask is written back on the app's
    /// own connection, naming the message it sent, so nothing of the app's
    /// waits on an answer that never comes.
    #[test]
    fn an_ask_the_session_does_not_answer_is_told_so() {
        use std::io::BufRead as _;

        let (app, session) = UnixStream::pair().unwrap();
        app.set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let message = FloatingSurfaceClientMessage::PaintCapable;
        refuse(
            &Arc::new(Mutex::new(session)),
            "so.tilo.notes",
            &name_of(&message),
            NO_PAINT,
        );
        let mut line = String::new();
        std::io::BufReader::new(&app).read_line(&mut line).unwrap();
        assert_eq!(
            serde_json::from_str::<FloatingSurfaceHostMessage>(line.trim()).unwrap(),
            FloatingSurfaceHostMessage::Unanswered {
                message: "paint_capable".to_owned(),
                reason: NO_PAINT.to_owned(),
            }
        );
    }

    /// An ask is named to the app the way the app sent it, so the reply says
    /// which of its messages this session could not answer.
    #[test]
    fn an_ask_is_named_as_the_app_sent_it() {
        assert_eq!(
            name_of(&FloatingSurfaceClientMessage::Hello { version: 21 }),
            "hello"
        );
        assert_eq!(
            name_of(&FloatingSurfaceClientMessage::AllowHostShell),
            "allow_host_shell"
        );
        assert_eq!(
            name_of(&FloatingSurfaceClientMessage::OpenCaptureStudio {
                mode: ui::floating_surface::CaptureStudioProtocolMode::PictureInPicture,
            }),
            "open_capture_studio"
        );
    }

    #[test]
    fn the_launcher_s_handshake_and_the_app_s_hello_are_read() {
        let frame = handshake("so.tilo.notes");
        let parsed: LauncherHandshake = serde_json::from_slice(&frame[..frame.len() - 1]).unwrap();
        assert_eq!(parsed.protocol, SHELL_UI_LAUNCH_PROTOCOL);
        assert_eq!(parsed.app_id, "so.tilo.notes");
        assert!(!parsed.network_status_allowed);
        // What the launcher says of the network reaches the window; one that
        // predates the field leaves it out.
        let allowed: LauncherHandshake = serde_json::from_value(serde_json::json!({
            "protocol": SHELL_UI_LAUNCH_PROTOCOL,
            "app_id": "so.tilo.notes",
            "network_status_allowed": true,
        }))
        .unwrap();
        assert!(allowed.network_status_allowed);
        let old: LauncherHandshake = serde_json::from_value(serde_json::json!({
            "protocol": SHELL_UI_LAUNCH_PROTOCOL,
            "app_id": "so.tilo.notes",
        }))
        .unwrap();
        assert!(!old.network_status_allowed);
        let mut hello = serde_json::to_vec(&FloatingSurfaceClientMessage::Hello {
            version: FLOATING_SURFACE_PROTOCOL_VERSION,
        })
        .unwrap();
        hello.push(b'\n');
        assert_eq!(
            hello_version(&hello[..hello.len() - 1]),
            Some(FLOATING_SURFACE_PROTOCOL_VERSION)
        );
        assert_eq!(
            hello_version(b"{\"type\":\"hello\",\"version\":0}"),
            Some(0)
        );
        assert_eq!(hello_version(b"{\"type\":\"other\"}"), None);
    }
}
