//! Tabs of one app, opened and closed by the app itself. Applications have no tab strips: a second
//! page, file or conversation is a second window of the app in a tab of the desktop's window group.
//! An app asks for one over its authenticated `shell-ui` connection ([`ui::shell_tabs`]): another
//! window after its own tab, its own tab closed, the last closed tab back, its group stepped on, or
//! its next window beside its own. The connection vouches for the app and its workspace, so an app
//! can never place, close or step another app's tab, and a refused request changes nothing. The
//! asks become desk commands on the session loop's own pass: the desk is never read from another
//! thread. An app also says here that its windows show a secret right now, which marks it private
//! for the compositor that every capture asks, and every connection receives the appearance of its
//! window's workspace.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufReader, Write};
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use crate::platform::surfaces::DesktopHost;
use launcher::services::{SHELL_UI_LAUNCH_PROTOCOL, SHELL_UI_SERVICE};
use serde::Deserialize;
use tilo_app_launch::routing;
use tilo_app_launch::{Connects, Launch, Open};
use tilo_compositor::{Compositor, ReadableWatch};

use tilo_desktop::launchpad::LaunchpadAction;
use tilo_desktop_policy::{DesktopPolicy, app_ids_match};
use ui::Theme;
use ui::WindowAppearance;
use ui::floating_surface::{
    FLOATING_SURFACE_PROTOCOL_VERSION, FloatingSurfaceClientMessage, FloatingSurfaceHostMessage,
    HostNetworkStatus,
};
use ui::theme::ThemeDocument;
use ui::workspace_sync::WorkspaceSync;

use crate::Launches;
use crate::apps::Apps;
use crate::assembly::BoxAsk;
use crate::capture_studio::CaptureStudio;
use crate::desktop_services::DesktopServices;
use crate::host_shell::HostShellAsks;
use crate::page_track::{PageTracks, Watch};

/// The longest frame an app may send: a paint stream and a marker inventory both fit, and nothing
/// else comes close.
const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// The longest address an app may ask to open in a new tab: as long as the shell's own link opening
/// takes.
const MAX_TARGET_BYTES: usize = 8 * 1024;

/// How the session makes a window's own connections to its services: the collaboration broker and
/// the shell-ui socket, each of the window's own, so what the window asks is answered for it alone.
pub struct ServiceLinks {
    launches: Arc<Launches>,
}

impl ServiceLinks {
    /// The connections of a window of the apps whose launches `launches` records.
    #[must_use]
    pub fn new(launches: Arc<Launches>) -> Self {
        Self { launches }
    }

    /// The window's collaboration connection, attested as that window: the record is the session's
    /// own, spent by the connection that names the window's instance.
    fn window_collaboration(
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

    /// The window's shell-ui connection, attributed to the launch whose process `launcher` is: the
    /// app and the workspace it runs in, without which what it asks reaches no desk. `instance` is
    /// the tab the window takes, which is how its asks find it.
    fn window_shell_ui(&self, app: &str, launcher: Option<u32>, instance: u64) -> Option<OwnedFd> {
        let launcher = launcher?;
        self.launches.window_shell_ui(app, launcher, instance);
        let connected = (|| {
            let mut stream = tilo_files::native::socket::connect(&launcher::services::socket_for(
                SHELL_UI_SERVICE,
            ))?;
            let mut frame = serde_json::to_vec(&serde_json::json!({
                "protocol": SHELL_UI_LAUNCH_PROTOCOL,
                "app_id": app,
                // What the app may know of the network was decided for its launch; the window does
                // not widen it.
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
    fn collaboration(
        &self,
        app: &str,
        workspace: &str,
        instance: u64,
        launcher_pid: Option<u32>,
    ) -> Option<OwnedFd> {
        self.window_collaboration(app, workspace, instance, launcher_pid)
    }

    fn shell_ui(&self, app: &str, instance: u64, launcher_pid: Option<u32>) -> Option<OwnedFd> {
        self.window_shell_ui(app, launcher_pid, instance)
    }
}

/// The shell-ui socket this session serves: what an app asks about its own tabs arrives here, and
/// the session's loop answers it.
pub struct AppTabs {
    requests: mpsc::Receiver<Request>,
    /// The connections that asked for their app's windows to be withheld from captures, and the app
    /// each speaks for: the mark lasts while one of them is open.
    withheld: BTreeMap<u64, String>,
    /// The open connections, by the app each vouches for and what it was last told of its window's
    /// appearance.
    links: BTreeMap<u64, Link>,
    /// The cards the apps draw themselves, and where the layer gives them.
    cards: crate::cards::Cards,
    /// The apps this session started, whose launches name each app window: what tells a window of
    /// an app from another window of the same app.
    launches: Arc<Launches>,
    /// The questions the person is answering for Terminal's own shell.
    host_shell: HostShellAsks,
    /// The listener the apps connect to, taken off on the session's own pass,
    /// and the socket's path, which goes when the session ends.
    socket: Socket,
    /// What the windows' boxes sent and asked on this pass, carried out once every ask of theirs is
    /// answered: they reach the session's records and its search, which the answers themselves do
    /// not.
    sent: Vec<Sent>,
    asked: Vec<Asked>,
}

/// An open shell-ui connection: the app it vouches for, the launch whose
/// window it is, whether that launch let the app know the network's state,
/// and what its window was last told.
struct Link {
    app: String,
    /// The launch the window that speaks on this connection belongs to: what tells it apart from
    /// another window of the same app, and so whether the very window that draws an activity is on
    /// the desk.
    launch: u32,
    /// The tab the window takes, when the session connected it itself: what its asks about its own
    /// window are answered for. A window the launcher connected names its tab itself, in each ask.
    instance: Option<u64>,
    /// Whether this app's launch let it know the network's state; a window never widens that for
    /// itself.
    network_allowed: bool,
    /// Whether this window's app said it opens a send-to box
    /// ([`FloatingSurfaceClientMessage::SelectionCapable`]): only a window that answered is asked
    /// to open one.
    selection: bool,
    /// Writing to the connection.
    writer: Arc<Mutex<UnixStream>>,
    /// The appearance the window was last told, so a pass that changes nothing sends nothing.
    told: Option<Delivered>,
}

/// What an appearance message carried, to tell whether a window needs another one.
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
    /// The window joined: its side of the connection is open, and it is told its window's
    /// appearance from the next pass on.
    Connected { network_allowed: bool },
    /// The connection ended: what it asked about captures goes with it, and its window is told no
    /// more.
    Closed,
}

/// One ask an app made over its own authenticated connection.
struct Request {
    /// The app the launcher vouched for.
    app: String,
    /// The workspace its launch runs it in.
    workspace: String,
    /// The launch the window that speaks on this connection belongs to.
    launch: u32,
    /// The tab the window takes, when the session connected it itself.
    instance: Option<u64>,
    /// The connection the ask came in on, which its answer goes back on and which its capture
    /// protection lasts as long as.
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
    pub(crate) fn start(
        launches: Arc<Launches>,
        wake: tilo_service::Wake,
        cards: crate::cards::Cards,
    ) -> Result<Self, String> {
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
        let socket = Socket::serve(listener, path, Arc::clone(&launches), requests, wake)?;
        Ok(Self {
            requests: answers,
            withheld: BTreeMap::new(),
            links: BTreeMap::new(),
            cards,
            launches,
            host_shell: HostShellAsks::default(),
            socket,
            sent: Vec::new(),
            asked: Vec::new(),
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

    /// Answer what the apps asked since the last pass: each ask is a desk command, so the desk is
    /// read and changed on this thread alone.
    pub(crate) fn follow(
        &mut self,
        shared: &crate::Shared,
        desktop: &DesktopHost,
        apps: Option<&Apps>,
        compositor: &mut Compositor,
        services: Option<&DesktopServices>,
        studio: Option<&CaptureStudio>,
    ) {
        // The windows that connected since the last pass join here, so an ask they write is
        // answered on this pass and no thread of the session's waits on a client.
        self.socket.take();
        {
            let mut answered = Answers {
                desktop,
                apps,
                compositor,
                services,
                studio,
                withheld: &mut self.withheld,
                links: &mut self.links,
                cards: &self.cards,
                launches: &self.launches,
                sent: &mut self.sent,
                asked: &mut self.asked,
                pages: &shared.pages,
            };
            for request in self.requests.try_iter() {
                answered.answer(&mut self.host_shell, request);
            }
            // What the person did in the windows' bars goes back to the app whose connection
            // declared the slots there.
            answered.deliver_bar_events();
            // A control of a window's own content, pressed where the desk lists it, goes to the app
            // drawing that window.
            answered.deliver_target_presses();
        }
        // What the members' desks show of these windows' pages, and what the
        // apps sent of them since the last pass.
        self.follow_pages(shared);
        // What the windows' boxes sent and asked, now that every ask of
        // theirs is answered: a send is one action on the person's behalf,
        // and a search is the desk's own.
        self.deliver_sends(shared);
        self.deliver_searches(shared);
        self.deliver_box(shared, desktop, compositor);
        // Where each app's own card stands this pass: the floating layer's place, which the desk
        // lays the app's window out by.
        self.follow_cards(desktop, compositor);
        // The person may have answered a question about the device's own shell since the last pass.
        self.host_shell.settle(services);
    }

    /// Publish the pages someone shows, and tell the apps how their windows are watched: a window
    /// that gained a viewer is asked for a whole frame and starts sending, one that lost its last
    /// viewer is told to stop, and a window's whole frame the plane needs again (a viewer joined, or
    /// lost the chain of changes) is asked for as the app can only answer.
    fn follow_pages(&self, shared: &crate::Shared) {
        let planes = shared
            .sync
            .as_deref()
            .map(crate::calls::planes_of)
            .unwrap_or_default();
        let now_us = tilo_clock::unix_now_ms().saturating_mul(1000);
        let asks = shared.pages.borrow_mut().step(&planes, now_us);
        for (tab, watch) in asks {
            let Some(link) = self.links.values().find(|link| link.instance == Some(tab)) else {
                continue;
            };
            let message = match watch {
                Watch::Whole => FloatingSurfaceHostMessage::PaintKeyframe,
                Watch::Stop => FloatingSurfaceHostMessage::PaintStop,
            };
            tell(&link.writer, &message, &link.app);
        }
    }

    /// Send what the windows' boxes sent, each through the session's one send action as the app
    /// whose window it was, for the person at it: the action every agent and script runs too.
    fn deliver_sends(&mut self, shared: &crate::Shared) {
        for sent in std::mem::take(&mut self.sent) {
            let mut by = tilo_actions::ActionInvoker::actor(sent.app.clone());
            by.kind = tilo_actions::ActorKind::App;
            let taken = shared.services.borrow_mut().invoke_as(
                &by,
                None,
                crate::send_to::RESOURCE,
                "send",
                &sent.arguments,
            );
            if let Err(error) = taken {
                tracing::warn!(%error, app = %sent.app, "a window's send was not taken");
            }
        }
        shared.deliver_sends();
    }

    /// Answer what the windows' boxes asked of the search: who a message could go to, for what they
    /// typed.
    fn deliver_searches(&mut self, shared: &crate::Shared) {
        for asked in std::mem::take(&mut self.asked) {
            let rows = shared.recipients(&asked.query);
            tell(
                &asked.reply,
                &FloatingSurfaceHostMessage::Recipients {
                    request: asked.request,
                    rows,
                },
                &asked.app,
            );
        }
    }

    /// Ask the window in front to open its box, as the shell's send keys do
    /// ([`FloatingSurfaceHostMessage::ShareSelection`]): the app draws the box alone, with no menu
    /// around it, addressed to the people here or to the agent it makes by being sent to. Saying
    /// something to the people here addresses them; a report asks the window for the thing it is
    /// about first, and the box opens about what the person points at, to the one place this
    /// computer sends reports — or saying it is told nowhere, and still sending to an agent or a
    /// person. A window whose app cannot take a box — it never said it opens one, or it is gone —
    /// leaves the gesture to Chat, which a send has always opened.
    fn deliver_box(&self, shared: &crate::Shared, desktop: &DesktopHost, compositor: &Compositor) {
        let Some(ask) = shared.take_box_ask() else {
            return;
        };
        let to = match ask {
            BoxAsk::Agent => Vec::new(),
            BoxAsk::People => shared.people_recipients(),
            BoxAsk::Report => vec![shared.feedback_row()],
        };
        let pointing = ask == BoxAsk::Report;
        let asked = self.link_in_front(desktop, compositor).is_some_and(|link| {
            tell(
                &link.writer,
                &FloatingSurfaceHostMessage::ShareSelection { to, pointing },
                &link.app,
            )
        });
        if !asked {
            tracing::debug!("no window in front opens a box: the gesture opens Chat");
            shared.launchpad(LaunchpadAction::Chat);
        }
    }

    /// The connection of the window in front, among the windows whose app opens a box: one that
    /// never said it does is never sent a box it would not draw. A session that cannot tell which
    /// window is in front answers with its one window that takes boxes, and none when it holds
    /// more.
    fn link_in_front(&self, desktop: &DesktopHost, compositor: &Compositor) -> Option<&Link> {
        let ready = || self.links.values().filter(|link| link.selection);
        let focused = desktop
            .with_policy(|policy| policy.focused())
            .and_then(|window| {
                compositor
                    .windows()
                    .into_iter()
                    .find(|(held, _)| held.id == window)
            })
            .and_then(|(held, _)| held.pid)
            .and_then(|pid| self.launches.launch_of(pid));
        if let Some(launch) = focused
            && let Some(link) = ready().find(|link| link.launch == launch)
        {
            return Some(link);
        }
        let mut only = ready();
        let first = only.next()?;
        only.next().is_none().then_some(first)
    }

    /// Stand each app's own card where the floating layer put it this pass,
    /// and stand it down while the window that draws it is on the desk: there
    /// the app draws the activity itself, and its card waits out of sight.
    ///
    /// The layer decides the place; the app's own geometry is a request it
    /// made when presenting, and its window is laid out by what this says.
    fn follow_cards(&self, desktop: &DesktopHost, compositor: &mut Compositor) {
        let placements = self.cards.placements();
        // The windows on the desk this pass, and the launch each belongs to: what tells whether the
        // very window that draws an activity is on screen, rather than another window of the same
        // app.
        let shown: BTreeSet<u64> = desktop.with_policy(|policy| {
            policy
                .layout()
                .shown
                .into_iter()
                .map(|(window, _)| window)
                .collect()
        });
        let windows = compositor.windows();
        let shown_launches: BTreeSet<u32> = windows
            .iter()
            .filter(|(window, _)| shown.contains(&window.id))
            .filter_map(|(window, _)| window.pid)
            .filter_map(|pid| self.launches.launch_of(pid))
            .collect();
        let mut moved = false;
        for card in self.cards.presented() {
            let in_view = match self.links.get(&card.connection) {
                // The launch that draws the activity has a window on the desk: the app draws it
                // there, and another window of the same app elsewhere leaves the card to this one.
                Some(link) => shown_launches.contains(&link.launch),
                // A connection that has not said whose launch it is is no window of its own yet:
                // the app's windows stand for it.
                None => desktop.with_policy(|policy| policy.shows(&card.app)),
            };
            self.cards.see(&card.app, &card.activity, in_view);
            let wanted = if in_view || card.stood_down {
                None
            } else {
                self.cards
                    .slot(&card.app, &card.activity)
                    .map(|window| (window, card.body(window)))
            };
            let Some(link) = self.links.get(&card.connection) else {
                continue;
            };
            match (wanted, card.told) {
                (Some((_, body)), Some(told)) if body == told => {}
                (Some((window, body)), _) => {
                    self.cards.told(&card.app, &card.activity, Some(body));
                    tell(
                        &link.writer,
                        &FloatingSurfaceHostMessage::PrepareSurface {
                            id: card.activity.clone(),
                            token: card.token.clone(),
                            rect: ui::floating_surface::protocol_rect_array(window),
                            content_rect: ui::floating_surface::protocol_rect_array(body),
                            transfer_covered: false,
                            coordinate_offset: [body.x0 - card.local.x, body.y0 - card.local.y],
                        },
                        &card.app,
                    );
                    placements.place(
                        &card.token,
                        (whole(window.width()), whole(window.height())),
                        Some(window),
                    );
                    moved = true;
                }
                (None, None) => {}
                (None, Some(_)) => {
                    self.cards.told(&card.app, &card.activity, None);
                    placements.suspend(&card.token);
                    tell(
                        &link.writer,
                        &FloatingSurfaceHostMessage::Suspended {
                            id: card.activity.clone(),
                        },
                        &card.app,
                    );
                    moved = true;
                }
            }
            // The card's window is up and draws the activity: the app records the card as adopted,
            // so it knows the desk shows it.
            if let Some((_, body)) = wanted
                && placements.drawn(&card.token)
                && self.cards.adopt(&card.app, &card.activity)
            {
                tell(
                    &link.writer,
                    &FloatingSurfaceHostMessage::Adopted {
                        id: card.activity.clone(),
                        rect: ui::floating_surface::protocol_rect_array(body),
                    },
                    &card.app,
                );
            }
        }
        moved |= self.home_lost_cards();
        if moved {
            compositor.rearrange();
        }
    }

    /// A card's window that went away leaves the app drawing it on the desk with nothing to draw it
    /// in: send it home, and it presents the card again. Says whether the desk's cards changed.
    fn home_lost_cards(&self) -> bool {
        let placements = self.cards.placements();
        let mut moved = false;
        for token in self.cards.lost() {
            let Some(card) = self.cards.by_token(&token) else {
                continue;
            };
            placements.suspend(&token);
            self.cards.told(&card.app, &card.activity, None);
            if let Some(link) = self.links.get(&card.connection) {
                tell(
                    &link.writer,
                    &FloatingSurfaceHostMessage::Reattach {
                        id: card.activity.clone(),
                    },
                    &card.app,
                );
            }
            moved = true;
        }
        moved
    }

    /// Give every open window the appearance of the desk it stands on: the temperature of its
    /// workspace, the person's colour, the theme, and its workspace's sync state, each when it
    /// differs from what that window was last told. A window opens with one, and gets another
    /// whenever any of it changes; a pass that changes nothing writes nothing.
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

/// What one pass of answers reaches: the desk, the services the person answers through, and the
/// connections the answers go back on.
struct Answers<'a> {
    desktop: &'a DesktopHost,
    /// The launches of this session, which a window an app asks for starts through: it is planned
    /// here and started once the desk asks back for the tab it takes.
    apps: Option<&'a Apps>,
    compositor: &'a mut Compositor,
    services: Option<&'a DesktopServices>,
    studio: Option<&'a CaptureStudio>,
    withheld: &'a mut BTreeMap<u64, String>,
    links: &'a mut BTreeMap<u64, Link>,
    cards: &'a crate::cards::Cards,
    launches: &'a Launches,
    /// The messages windows sent from their own boxes, and the searches they asked, both carried
    /// out after the pass has every ask: they reach the session's records, which the answers
    /// themselves do not touch.
    sent: &'a mut Vec<Sent>,
    asked: &'a mut Vec<Asked>,
    /// The windows whose apps hand their page over as a paint stream.
    pages: &'a Rc<RefCell<PageTracks>>,
}

/// A message a window's box sent: the app whose window it was, and the arguments of the session's
/// one send action ([`crate::send_to::RESOURCE`]) it runs.
struct Sent {
    app: String,
    arguments: serde_json::Map<String, serde_json::Value>,
}

/// A window's box asking the search whom it could send to.
struct Asked {
    app: String,
    reply: Arc<Mutex<UnixStream>>,
    request: u64,
    query: String,
}

impl Answers<'_> {
    /// The app draws its activity in its own window again: the desk hides the card's window, the
    /// app is told it stands down, and the window that draws it comes forward — the person asked
    /// for the activity where it lives, not for a card that vanishes.
    fn stand_down(&mut self, card: &crate::cards::Presented, reply: &Arc<Mutex<UnixStream>>) {
        self.cards.told(&card.app, &card.activity, None);
        self.cards.placements().suspend(&card.token);
        self.compositor.rearrange();
        tell(
            reply,
            &FloatingSurfaceHostMessage::Suspended {
                id: card.activity.clone(),
            },
            &card.app,
        );
        let Some(launch) = self.links.get(&card.connection).map(|link| link.launch) else {
            return;
        };
        let window = self
            .compositor
            .windows()
            .into_iter()
            .find(|(window, _)| {
                window.pid.and_then(|pid| self.launches.launch_of(pid)) == Some(launch)
                    && window
                        .app_id
                        .as_deref()
                        .is_some_and(|app_id| app_ids_match(app_id, &card.app))
            })
            .map(|(window, _)| window.id);
        if let Some(window) = window {
            self.desktop.with_policy(|policy| policy.focus(window));
        }
    }

    /// Answer one ask of the app it came from: an ask never changes another app's tab, and an app
    /// whose ask this session cannot answer is told so rather than left waiting.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per message, so every one of them is answered here"
    )]
    fn answer(&mut self, host_shell: &mut HostShellAsks, request: Request) {
        let Request {
            app,
            workspace,
            launch,
            connection,
            reply,
            ask,
            instance,
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
                if let Some(tab) = self
                    .links
                    .remove(&connection)
                    .and_then(|link| link.instance)
                {
                    // A page nothing runs any more is published nowhere, and what its window said
                    // it shows goes with it.
                    self.pages.borrow_mut().closed(tab);
                    self.launches.notes().forget(tab);
                }
                // The cards it presented, and the windows the desk stood
                // them in, go with it.
                let mut gone = false;
                for card in self.cards.connection_closed(connection) {
                    self.cards.placements().withdraw(&card.token);
                    gone = true;
                }
                if gone {
                    self.compositor.rearrange();
                }
                // What this connection declared for its window's bar goes with it, as does the
                // question it waits on.
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
                        launch,
                        instance,
                        network_allowed,
                        selection: false,
                        writer: reply,
                        told: None,
                    },
                );
                return;
            }
            Ask::Message(message) => *message,
        };
        // The window the ask is about: the tab the session gave the connection, else the tab its
        // own ask names, and only a window of neither is the app's window in front — never
        // another window of the same app.
        let instance = self
            .links
            .get(&connection)
            .and_then(|link| link.instance)
            .or_else(|| instance_of(&message));
        let own = self.desktop.with_policy(|policy| match instance {
            Some(instance) => policy.tab_of_instance(&app, instance),
            None => policy.anchor_for(&app).map(|(tab, ..)| tab),
        });
        let name = name_of(&message);
        match message {
            FloatingSurfaceClientMessage::OpenTabBeside {
                target, background, ..
            } => self.answer_open_beside(&app, target, background, own),
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
                    .with_policy(|policy| policy.place_next_beside(&app, own, background));
            }
            FloatingSurfaceClientMessage::SetCaptureProtection {
                protected,
                generation,
            } => self.answer_protection(connection, &app, &reply, protected, generation),
            FloatingSurfaceClientMessage::Toolbar { toolbar, .. } => {
                // The slots this app shows in its window's bar, where its window is a tab of the
                // desktop's.
                match own {
                    Some(tab) => self.desktop.with_policy(|policy| {
                        policy.handle().declare_bar(tab, connection, toolbar);
                    }),
                    None => refuse(&reply, &app, &name, NO_WINDOW),
                }
            }
            FloatingSurfaceClientMessage::WindowTargets { targets, .. } => {
                // The controls of the window's own content, where its window is a tab of the
                // desktop's, so a session names and presses them as it names the bar's.
                match own {
                    Some(tab) => self.desktop.with_policy(|policy| {
                        policy.handle().declare_targets(tab, connection, targets);
                    }),
                    None => refuse(&reply, &app, &name, NO_WINDOW),
                }
            }
            FloatingSurfaceClientMessage::EditLocation { .. } => match own {
                Some(tab) => self
                    .desktop
                    .with_policy(|policy| policy.handle().edit_bar_location(tab)),
                None => refuse(&reply, &app, &name, NO_WINDOW),
            },
            FloatingSurfaceClientMessage::WindowOffer { offer, .. } => match own {
                Some(tab) => self.desktop.with_policy(|policy| {
                    policy.handle().offer_bar(tab, connection, offer);
                }),
                None => refuse(&reply, &app, &name, NO_WINDOW),
            },
            FloatingSurfaceClientMessage::OpenSearch { opening, .. } => match own {
                Some(tab) => self
                    .desktop
                    .with_policy(|policy| policy.handle().open_bar_search(tab, opening)),
                None => refuse(&reply, &app, &name, NO_WINDOW),
            },
            FloatingSurfaceClientMessage::OpenAppSearch { request, .. } => match own {
                Some(tab) => self
                    .desktop
                    .with_policy(|policy| policy.handle().search_app(tab, request)),
                None => refuse(&reply, &app, &name, NO_WINDOW),
            },
            FloatingSurfaceClientMessage::AllowHostShell => {
                // The person allows the device's own shell, and the runtime (`ush`) records their
                // answer; the app hears it here.
                host_shell.ask(self.services, &app, connection, Arc::clone(&reply));
            }
            FloatingSurfaceClientMessage::OpenCaptureStudio { mode } => match self.studio {
                Some(studio) => studio.open_asked(mode),
                None => refuse(&reply, &app, &name, NO_CAPTURE),
            },
            // A window paints its own action markers, at the control an agent would press, so
            // nothing here draws a second one; and the session names none of them to assistive
            // technology or opens what one points at, so the inventory is read and adds nothing. A
            // surface cleared or a destination presented is the app's own business.
            FloatingSurfaceClientMessage::ActionMarkers { .. }
            | FloatingSurfaceClientMessage::SourceCleared { .. }
            | FloatingSurfaceClientMessage::DestinationPresented { .. } => {}
            // The window opens a send-to box when the shell asks: from now on it is the one the
            // shell's send key asks, and no window that never said so is ever sent a box it would
            // not draw.
            FloatingSurfaceClientMessage::SelectionCapable => {
                if let Some(link) = self.links.get_mut(&connection) {
                    link.selection = true;
                    tracing::debug!(app = %link.app, "a window opens a send-to box");
                }
            }
            // An app's own card — a call, a route — is a window of its own: the app presents it,
            // the floating layer decides where it stands, and the app draws the activity inline
            // whenever its own window is on the desk.
            FloatingSurfaceClientMessage::Present { request } => {
                if let Err(reason) = self.cards.present(&app, connection, &request) {
                    tracing::warn!(%app, %reason, "card refused");
                    refuse(&reply, &app, &name, &reason);
                }
            }
            // The app's own geometry is a request: the layer's place stands. A card it presents
            // again after standing it down draws the activity there once more.
            FloatingSurfaceClientMessage::SurfaceGeometry { id, .. } => {
                self.cards.presented_again(&app, connection, &id);
            }
            // The app draws the activity in its own window again: its card stands down until it
            // presents one again.
            FloatingSurfaceClientMessage::Suspend { id, .. } => {
                if let Some(card) = self.cards.stood_down(&app, connection, &id) {
                    self.stand_down(&card, &reply);
                }
            }
            // The app ended the activity: its card goes, and the window the desk stood there goes
            // with it.
            FloatingSurfaceClientMessage::Dismiss { id } => {
                if let Some(card) = self.cards.dismissed(connection, &id) {
                    self.cards.placements().withdraw(&card.token);
                    self.compositor.rearrange();
                }
                tell(&reply, &FloatingSurfaceHostMessage::Closed { id }, &app);
            }
            // A mirror of a window's pixels has no native card here: what floats is drawn by the
            // app that owns it.
            FloatingSurfaceClientMessage::UpdateRegion { .. } => {
                refuse(&reply, &app, &name, NO_REGION);
            }
            // The record a window shows, as its app holds it: the one source of what a window
            // shows, which its place, the layout kept beside it and where anyone here is read
            // from. An app that keeps one document for the whole workspace speaks on a channel
            // that names no record, so the window itself has to say it.
            // The record a window shows, as its app holds it. An app that keeps one document for
            // the whole workspace has no record id to name it by (its route is empty), so the
            // document's kind names it: the same on every device, and the same for every window
            // showing it.
            FloatingSurfaceClientMessage::WindowRecord { record, kind, .. } => {
                let record = if record.trim().is_empty() {
                    &kind
                } else {
                    &record
                };
                if let Some(tab) = own {
                    self.launches.notes().note(tab, &app, &workspace, record);
                }
            }
            // What else a window shares live, the embeds it draws of other apps' resources, and
            // the selection an agent works from: the session serves none of them over this
            // connection yet.
            FloatingSurfaceClientMessage::Embeds { .. }
            | FloatingSurfaceClientMessage::SharedWindows { .. } => {
                refuse(&reply, &app, &name, NO_WINDOW_SHARE);
            }
            // The resource a window shows is what the composer asks the agent about it, so it goes
            // with the selection it seeds.
            FloatingSurfaceClientMessage::WindowResource { .. }
            | FloatingSurfaceClientMessage::Pointed { .. }
            | FloatingSurfaceClientMessage::PickAnswered { .. } => {
                refuse(&reply, &app, &name, NO_AGENT);
            }
            // The message its box sent, and whom its box could send to: both reach the session's
            // records and the one search, so they are carried out after this pass has every ask.
            FloatingSurfaceClientMessage::SendTo { to, attached, text } => {
                let arguments = serde_json::json!({
                    "to": to,
                    "attached": attached,
                    "text": text,
                    "workspace": workspace,
                });
                if let serde_json::Value::Object(arguments) = arguments {
                    self.sent.push(Sent { app, arguments });
                }
            }
            // The app's send landed on `anchor`: a window of the desk showing it, beside the
            // window that sent.
            FloatingSurfaceClientMessage::OpenFloated { anchor, .. } => {
                self.answer_open_after_send(&app, &anchor);
            }
            FloatingSurfaceClientMessage::SearchRecipients { request, query } => {
                self.asked.push(Asked {
                    app,
                    reply,
                    request,
                    query,
                });
            }
            // A window that runs a page engine offers its page as the thing the
            // workspace sees of it: what it draws is published on the live
            // plane as its window's page track, and only while a member shows
            // it. Tilo apps' own views are never streamed, and neither is
            // anything but a page.
            FloatingSurfaceClientMessage::PaintCapable => match own {
                Some(tab) => self.pages.borrow_mut().capable(tab, &workspace),
                None => refuse(&reply, &app, &name, NO_WINDOW),
            },
            FloatingSurfaceClientMessage::PaintStream { payload, .. } => match own {
                // `page` and `scale` place a viewer's input in the page, which
                // this session does not take: the page is drawn, not typed
                // into, by the members that show it.
                Some(tab) => {
                    use base64::Engine as _;
                    match base64::engine::general_purpose::STANDARD.decode(&payload) {
                        Ok(bytes) => {
                            let whole = ui::webview::paint::stream::wire::Payload::is_key(&bytes);
                            self.pages.borrow_mut().received(tab, whole, bytes);
                        }
                        Err(error) => {
                            tracing::warn!(%error, %app, "a window's page frame is not base64");
                        }
                    }
                }
                None => refuse(&reply, &app, &name, NO_WINDOW),
            },
            // The device's displays are placed in Settings, and its own shell is what a window of
            // Terminal asks about: neither is asked here.
            FloatingSurfaceClientMessage::MoveDeviceDisplay { .. }
            | FloatingSurfaceClientMessage::DeviceShellOutcome { .. } => {
                refuse(&reply, &app, &name, NO_DEVICE);
            }
            // What is kept of a window's changes, in the person's words: the session keeps no model
            // to describe them with.
            FloatingSurfaceClientMessage::DescribeHistory { .. } => {
                refuse(&reply, &app, &name, NO_HISTORY);
            }
            // The app under the pointer did not scroll with a sideways wheel: the strip its window
            // stands in pans by it, as a wheel over the strip's own canvas does. Nothing to answer
            // — the app took what it scrolled with, and this is what it left.
            FloatingSurfaceClientMessage::SidewaysWheel { travel } => {
                if let Some(tab) = own {
                    self.desktop
                        .with_policy(|policy| policy.handle().sideways_wheel(tab, travel));
                }
            }
            // The connection said its version when it opened; a second one is answered with the one
            // this session speaks.
            FloatingSurfaceClientMessage::Hello { .. } => refuse(&reply, &app, &name, SAID),
        }
    }

    /// Open the place a window of `app` asked for: beside the one that asked, and a launch that
    /// starts nothing takes the place back.
    fn answer_open_beside(
        &self,
        app: &str,
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
        // The tab is there from this frame and the window starts once the desk asks back, so the
        // tab is the window's instance.
        apps.ask_window(
            &Launch::Package {
                app: app.to_owned(),
                open,
            },
            anchor,
            background,
            false,
            None,
        );
    }

    /// Open a window of `app` beside the one that sent from its box, showing
    /// the item `anchor` names: the conversation a message from that box
    /// landed in. It opens in a window of its own, standing over the desk as
    /// the chord floats a window — over the place its strip gave it, the strip
    /// sharing out the card it draws at, so it stands beside what the person
    /// was reading instead of over it. The workspace's record keeps it there
    /// beside its sender, for every member, and the window that sent it goes on
    /// showing what it showed.
    fn answer_open_after_send(&self, app: &str, anchor: &str) {
        let Some(apps) = self.apps else {
            return;
        };
        // One window per item: the conversation a window already shows is that window, so a
        // second send into it raises the window it stands in instead of opening another on the
        // same conversation.
        if let Some(tab) = apps.window_on(app, anchor) {
            if self.desktop.with_policy(|policy| policy.focus_tab(tab)) {
                return;
            }
            // Its window closed: the item stands nowhere, and a window opens for it again.
            apps.forget_window_on(app, anchor);
        }
        // The tab is there from this frame and the window starts once the desk asks back, so the
        // tab is the window's instance.
        apps.ask_window(
            &Launch::Package {
                app: app.to_owned(),
                open: None,
            },
            None,
            false,
            true,
            Some(anchor),
        );
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

    /// Write what the person did in the windows' bars to the app whose connection declared the
    /// slots there.
    fn deliver_bar_events(&self) {
        let events = self
            .desktop
            .with_policy(|policy| policy.handle().take_bar_events());
        for (connection, event) in events {
            let Some(link) = self.links.get(&connection) else {
                continue;
            };
            // The app had been sent no input of this session's when the person used its bar: it
            // runs the event with the next input it reads.
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
        for press in presses {
            let Some(link) = self.links.get(&press.connection) else {
                continue;
            };
            tell(
                &link.writer,
                &FloatingSurfaceHostMessage::PressTarget {
                    name: press.name,
                    role: press.role,
                    identity: press.identity,
                    action: press.action,
                },
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
const NO_REGION: &str = "this session mirrors no floating surface's region";

/// An app asked to share a window live or embed another's resource.
const NO_WINDOW_SHARE: &str = "this session shares no window live";

/// An app asked about what a person selected, and for an agent to work from it.
const NO_AGENT: &str = "this session asks no window about a selection";

/// An app asked to move a device's display, or answered a device-shell operation.
const NO_DEVICE: &str = "this session moves no device display here";

/// An app asked for its changes described.
const NO_HISTORY: &str = "this session describes no history";

/// A second greeting on a connection whose version was read when it opened.
const SAID: &str = "this connection said its version when it opened";

/// `value` as a whole logical pixel.
#[allow(clippy::cast_possible_truncation, reason = "a card's size on a screen")]
fn whole(value: f64) -> i32 {
    value.round() as i32
}

/// The tab the window sending `message` names, when it names one: a window the
/// launcher connected says which tab it takes in each ask.
fn instance_of(message: &FloatingSurfaceClientMessage) -> Option<u64> {
    match message {
        FloatingSurfaceClientMessage::WindowTargets { instance, .. }
        | FloatingSurfaceClientMessage::Toolbar { instance, .. }
        | FloatingSurfaceClientMessage::WindowOffer { instance, .. }
        | FloatingSurfaceClientMessage::EditLocation { instance }
        | FloatingSurfaceClientMessage::OpenSearch { instance, .. }
        | FloatingSurfaceClientMessage::OpenAppSearch { instance, .. }
        | FloatingSurfaceClientMessage::OpenFloated { instance, .. }
        | FloatingSurfaceClientMessage::WindowRecord { instance, .. }
        | FloatingSurfaceClientMessage::Embeds { instance, .. } => *instance,
        _ => None,
    }
}

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

/// Tell the app on `reply` that this session does not answer `name` , and why, so nothing of the
/// app's waits on an answer that will not come.
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

/// Write `message` to the app on its connection, saying so when it cannot be written.
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

/// Mark the windows of `app` private (`private`) or showable again, as `connection` asks: the mark
/// lasts while one of the app's connections asks for it, so it goes when the last one stops asking
/// or closes.
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
    // The desk knows the same apps, so nothing it names for the compositor to draw is named for an
    // app that keeps its content, and a card in the switcher or the overview says so instead of
    // showing what the window holds back.
    desktop.set_withheld_apps(withheld.values().cloned().collect());
}

/// The address `app` asks to open, when its manifest claims it: a link it declares the scheme of,
/// as its own menu would open one.
fn target_open(app: &str, target: &str) -> Option<Open> {
    if target.is_empty()
        || target.len() > MAX_TARGET_BYTES
        || target.trim() != target
        || target.chars().any(char::is_control)
    {
        return None;
    }
    let scheme = routing::scheme_of(target)?;
    let installed = app_catalogue::apps::read_one_from_env(app).ok().flatten()?;
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
    /// The socket standing at `path` when it was bound: a session that takes the path later binds
    /// one of its own, which this one leaves standing.
    standing: Option<(u64, u64)>,
    /// The connection number the next connection taken is given.
    next: AtomicU64,
    /// The launches a connection is checked against, the channel its asks arrive on, and the wake
    /// that brings the session to answer them.
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

    /// Take every connection waiting, each served on a thread of its own: what it asks arrives on
    /// the session's channel, for the session to answer on this pass.
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
        // A session started while this one ran takes the path for its own: the socket standing
        // there is that session's, and stays.
        if standing(&self.path) == self.standing {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// The socket standing at `path` , as the file system names it: the device and inode of the node
/// there. A bound socket's own file descriptor names an inode of the socket layer, not this one.
fn standing(path: &std::path::Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::symlink_metadata(path).ok()?;
    Some((meta.dev(), meta.ino()))
}

/// The launcher's word for a connection, written before the sandbox starts: which app it is, as the
/// launcher knows it, and whether that launch let the app know the network's state.
#[derive(Deserialize)]
struct LauncherHandshake {
    protocol: u16,
    app_id: String,
    /// Absent from a launcher that predates the field: such an app is told nothing of the network.
    #[serde(default)]
    network_status_allowed: bool,
}

/// The app's first frame as every shell-ui version reads it: `{"type": "hello", "version": N}` ,
/// whatever else a later version adds.
#[derive(Deserialize)]
struct Hello {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    version: Option<u16>,
}

/// The shell-ui version `frame` says, when it is a hello.
fn hello_version(frame: &[u8]) -> Option<u16> {
    tilo_failure::ignored!(
        "a frame that does not read back is not a hello",
        serde_json::from_slice::<Hello>(frame)
            .ok()
            .filter(|hello| hello.kind == "hello")
            .and_then(|hello| hello.version)
    )
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
    // The process at the other end, as the socket says it: a window the app opened, or the
    // session's own connection for one it opened in the app.
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
    let Ok(handshake) = tilo_failure::ignored!(
        "a handshake that does not read back is not one of ours",
        serde_json::from_slice::<LauncherHandshake>(&handshake_frame)
    ) else {
        return;
    };
    if handshake.protocol != SHELL_UI_LAUNCH_PROTOCOL
        || handshake.app_id.trim().is_empty()
        || handshake.app_id.len() > 255
    {
        return;
    }
    // The app's first frame says the version it speaks; nothing else is read from an app at another
    // one.
    let Ok(Some(hello)) = wire_frame::read_line(&mut reader, MAX_FRAME_BYTES) else {
        return;
    };
    let app_version = hello_version(&hello);
    if app_version != Some(FLOATING_SURFACE_PROTOCOL_VERSION) {
        refuse_version(&mut writer, &handshake.app_id, app_version);
        return;
    }
    let app = handshake.app_id;
    let Some((workspace, pid, instance)) = launched(launches, peer, &app) else {
        tracing::info!(%app, "a shell-ui connection names no app this session started");
        return;
    };
    // The launch the window belongs to: an app runs one process per window tree, and a window that
    // connects from below it names that tree.
    let launch = launches.launch_of(pid).unwrap_or(pid);
    let reply = Arc::new(Mutex::new(writer));
    // From here the window is told the appearance of the desk it stands on,
    // and it is told once more whenever that changes.
    if requests
        .send(Request {
            app: app.clone(),
            workspace: workspace.clone(),
            launch,
            instance,
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
                launch,
                instance,
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
        launch,
        instance,
        connection,
        reply,
        ask: Ask::Closed,
    });
    wake();
    tracing::debug!(connection, %app, "shell-ui connection closed");
}

/// The process at the other end of `stream` , as the kernel names it: the app's own process, or the
/// session itself for a window it connected.
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

/// The workspace the app the connection names runs in and the process the window that speaks for it
/// runs in, when this session started it: the launch the peer's process belongs to, which is the
/// process the window's activity draws in.
fn launched(
    launches: &Launches,
    peer: Option<u32>,
    app: &str,
) -> Option<(String, u32, Option<u64>)> {
    let (pid, instance) = match peer {
        // The session connects a window it opens in an app's running process itself: what it
        // recorded for that connection names the launch and the tab the window takes.
        Some(pid) if pid == std::process::id() => {
            let (started, instance) = launches.take_window_shell_ui(app)?;
            (started, Some(instance))
        }
        peer => (peer?, None),
    };
    launches
        .workspace_of(pid, app)
        .map(|workspace| (workspace, pid, instance))
}

/// Tell an app speaking another shell-ui version which one this session speaks, so the one behind
/// can be updated.
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

/// Write one message to an app, as the one-line frame every shell-ui version reads.
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

    /// A socket on a path of the test's own, with the channel its asks land on and a wake that does
    /// nothing.
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

    /// A session started while another ran takes the shell-ui path for itself. The one that lost it
    /// ends without a client waking a thread for it, and leaves the socket standing there alone.
    #[test]
    fn a_session_ending_leaves_the_socket_another_one_took() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shell-ui.sock");
        let (first, _first_answers) = socket_at(&path);
        std::fs::remove_file(&path).unwrap();
        let (second, _second_answers) = socket_at(&path);
        // Nothing connects to the socket the first one bound: ending it returns all the same, on
        // the thread that serves it.
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
        // An app this session started nothing of is told nothing, and the connection closes.
        let mut rest = Vec::new();
        client.read_to_end(&mut rest).unwrap();
        assert!(rest.is_empty(), "nothing is written to that connection");
        assert!(
            answers.try_recv().is_err(),
            "no ask of an app this session never started"
        );
    }

    /// What the session cannot do of an ask is written back on the app's own connection, naming the
    /// message it sent, so nothing of the app's waits on an answer that never comes.
    #[test]
    fn an_ask_the_session_does_not_answer_is_told_so() {
        use std::io::BufRead as _;

        let (app, session) = UnixStream::pair().unwrap();
        app.set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let message = FloatingSurfaceClientMessage::Embeds {
            embeds: Vec::new(),
            instance: Some(1),
        };
        refuse(
            &Arc::new(Mutex::new(session)),
            "so.tilo.notes",
            &name_of(&message),
            NO_WINDOW_SHARE,
        );
        let mut line = String::new();
        std::io::BufReader::new(&app).read_line(&mut line).unwrap();
        assert_eq!(
            serde_json::from_str::<FloatingSurfaceHostMessage>(line.trim()).unwrap(),
            FloatingSurfaceHostMessage::Unanswered {
                message: "embeds".to_owned(),
                reason: NO_WINDOW_SHARE.to_owned(),
            }
        );
    }

    /// An ask is named to the app the way the app sent it, so the reply says which of its messages
    /// this session could not answer.
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
        // What the launcher says of the network reaches the window; one that predates the field
        // leaves it out.
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
