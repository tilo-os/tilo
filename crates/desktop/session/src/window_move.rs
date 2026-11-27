//! Carrying a window the person moves to another workspace.
//! A workspace is the sandbox the window's app runs in: its grants, services,
//! authority and records. None of that follows a tab, so what crosses is the tab
//! and, with it, the record the window shows: the workspaces' store moves that
//! record into the destination ([`crate::window_transfer`]), the app starts again
//! there under its grants, on the record, and the window left behind closes once
//! the new window is up. A move into or out of a workspace other people are in is
//! asked about first; one that cannot be made puts the tab back where it was, with
//! the record where it was, and says why. Nothing in a move waits on the workspaces'
//! store on the session's own thread.
//!
//! A window moved to another of the person's devices goes as continuity's offer made to that
//! device alone ([`crate::continuity`]), and closes here once that device says it opened it.
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use collab_types::handoff::HandoffReport;
use tilo_app_launch::Launch;
use tilo_compositor::Compositor;
use tilo_desktop_model::{Layout, TabId, TileContent, Workspace, WorkspaceId};
use ui::resource::DocumentId;

use crate::Shared;
use crate::apps::launch_of;
use crate::device_workspaces::DeviceWorkspaces;
use crate::i18n::{t, t_args};
use crate::platform::notice::{Notice, Notices};
use crate::window_follow::WindowFollow;
use crate::window_transfer::{self, Fault, Profile};
use tilo_session_core::desktop::navigation::LayoutEffect;
use tilo_session_core::desktop::state::window_menu::OtherScreen;
use tilo_workspace_service::workspaces::person_workspace::devices::{
    ContinuityOffer, Devices, MAX_CONTINUITY_SHOWN_MS,
};

/// The action a move's notice offers: carry the window into its destination.
const CARRY: &str = "window-move-carry";

/// The action a move's notice offers: leave the window where it is.
const STAY: &str = "window-move-stay";

/// How long the app started where the tab went may take to open a window there.
const OPENING: Duration = Duration::from_secs(30);

/// [`OPENING`] as a name the environment reads: milliseconds. A test that watches a move give up
/// on a window that never opens waits this rather than the production half minute.
pub const OPENING_ENV: &str = "TILO_WINDOW_MOVE_OPENING_MS";

/// [`OPENING`], or the period the environment names.
fn opening_period() -> Duration {
    let Some(named) = std::env::var(OPENING_ENV).ok() else {
        return OPENING;
    };
    match named.trim().parse::<u64>() {
        Ok(millis) => Duration::from_millis(millis),
        Err(error) => {
            tracing::warn!(%error, "a move's opening period that is not milliseconds is not used");
            OPENING
        }
    }
}

/// A move the person was asked about, waiting for their answer.
struct Asked {
    tab: TabId,
    /// The workspace it is in now, where it stays unless they say otherwise.
    from: WorkspaceId,
    /// What the workspace it leaves is called, for the notice that answers a failed move.
    from_name: String,
    to: WorkspaceId,
    /// What the destination is called and its identity: what the app is started into,
    /// and where the record it shows is kept.
    to_name: String,
    to_id: String,
    app: String,
    app_name: String,
    /// The window it shows now, which the move leaves behind.
    window: u64,
    /// Whether the window shows a record of its own, which moves with it.
    carries: bool,
}

/// The record a window shows, on its way to the workspace the person carried it to.
struct Travelling {
    /// Its id where the window showed it.
    id: DocumentId,
    /// Where the window's saved presentation is read and kept.
    workspaces: DeviceWorkspaces,
    /// The device's daemon, which runs the store's move as the person.
    daemon: tilo_session_client::daemon_documents::DaemonShell,
    /// The app's records where the window showed it, and where it is carried to.
    source: Profile,
    destination: Profile,
    /// Its id where it went, once the store moved it there.
    moved: Option<DocumentId>,
}

/// A move under way.
struct Crossing {
    tab: TabId,
    from: WorkspaceId,
    from_name: String,
    from_id: String,
    to_name: String,
    to_id: String,
    app: String,
    app_name: String,
    /// The window the tab showed where it was, which the move leaves behind.
    window: u64,
    /// What the window showed there, when it showed a record of its own.
    record: Option<Travelling>,
    /// The offer of the window to another device of the person's, when it moves there: `to_id`
    /// is that device's key.
    handing: Option<ContinuityOffer>,
    phase: Phase,
}

/// What a move waits for.
enum Phase {
    /// The window's app was asked to make what it shows durable: its answer, or `None` when it
    /// gave none (its connection closed first, or no app shows the window).
    Checkpointing(Receiver<Option<HandoffReport>>),
    /// The workspaces' store is moving the record to where the tab went.
    Moving(Receiver<Result<DocumentId, Fault>>),
    /// The tab waits for the window of the app started in the destination, or the device the
    /// window was offered to says it opened it.
    Opening { since: Instant },
    /// Nothing is waited for: the step that left it here is done.
    Finished,
}

/// The windows the person carries between workspaces, and what they answer before one crosses.
pub(crate) struct Moves {
    /// The moves the person was asked about, by the tag of the notice that asked.
    asking: RefCell<BTreeMap<String, Asked>>,
    crossing: RefCell<Vec<Crossing>>,
    /// What the person's notices are shown on, where the question and its answer go.
    notices: Rc<dyn Notices>,
    /// Wakes the session's loop when a window's app answered.
    wake: tilo_service::Wake,
    tokens: Cell<u64>,
    /// How long the app started where a tab went may take to open a window there
    /// ([`opening_period`]).
    opening: Duration,
}

impl Moves {
    /// Carries asked about through `notices` , whose answers come back here; `wake` wakes the
    /// session's loop when a window's app answers.
    pub(crate) fn new(notices: Rc<dyn Notices>, wake: tilo_service::Wake) -> Self {
        Self {
            asking: RefCell::new(BTreeMap::new()),
            crossing: RefCell::new(Vec::new()),
            notices,
            wake,
            tokens: Cell::new(0),
            opening: opening_period(),
        }
    }

    /// Carry out the person's answers and carries, and take every move in flight a step further.
    pub(crate) fn follow(
        &self,
        shared: &Shared,
        compositor: &mut Compositor,
        handoff: Option<&WindowFollow>,
    ) {
        for (tag, key) in self.notices.take_chosen_actions() {
            let Some(asked) = self.asking.borrow_mut().remove(&tag) else {
                continue;
            };
            if key == CARRY {
                self.carry(shared, handoff, asked);
            }
            // Any other answer keeps the window where it is, which is where it never left.
        }
        for carried in shared.desktop.with_policy(|policy| policy.take_carried()) {
            self.consider(shared, handoff, carried.tab, carried.to);
        }
        for (tab, screen) in shared.handle.take_device_moves() {
            self.send(shared, handoff, tab, screen);
        }
        let moving = std::mem::take(&mut *self.crossing.borrow_mut());
        for one in moving {
            self.step(shared, compositor, one);
        }
    }

    /// The person carried `tab` to the workspace at `to` : the app it shows starts again there on
    /// the record it shows, and a move that touches other people is asked about first.
    fn consider(
        &self,
        shared: &Shared,
        handoff: Option<&WindowFollow>,
        tab: TabId,
        to: WorkspaceId,
    ) {
        let read = shared.desktop.with_policy(|policy| {
            policy.handle().read(|tree| {
                let here = tab_window(tree, tab, &shared.person, &shared.earlier)?;
                let there = workspace_at(tree, to, &shared.person, &shared.earlier)?;
                Some((here, there))
            })
        });
        let Some((here, there)) = read else {
            // Nothing of an app shows in the tab any more, or its destination went: the tab goes
            // where the person put it.
            let _ = shared.desktop.with_policy(|policy| policy.carry(tab, to));
            return;
        };
        let Some(Launch::Package { app, .. }) = launch_of(&here.app) else {
            // One of this machine's own programs runs with no workspace around it, so its tab
            // crosses as any tab does.
            let _ = shared.desktop.with_policy(|policy| policy.carry(tab, to));
            return;
        };
        let asked = Asked {
            tab,
            from: here.workspace.id,
            from_name: here.workspace.name,
            to,
            to_name: there.name,
            to_id: there.id.to_string(),
            app_name: app_name(&app),
            app,
            window: here.window,
            carries: Self::travelling(shared, tab).is_some(),
        };
        if here.workspace.others || there.others {
            self.ask(asked, here.workspace.others, there.others);
        } else {
            self.carry(shared, handoff, asked);
        }
    }

    /// Ask the person what the move means before anything crosses: the window and the record it
    /// takes are other people's to see, or leaving takes them away.
    fn ask(&self, asked: Asked, here: bool, there: bool) {
        let title = t_args(
            "window-move-ask-title",
            &[
                ("app", asked.app_name.clone().into()),
                ("workspace", asked.to_name.clone().into()),
            ],
        );
        let mut body = t(if asked.carries {
            "window-move-ask-carries"
        } else {
            "window-move-ask-what"
        });
        if there {
            body.push(' ');
            body.push_str(&t_args(
                "window-move-ask-they-see",
                &[("workspace", asked.to_name.clone().into())],
            ));
        }
        if here {
            body.push(' ');
            body.push_str(&t_args(
                "window-move-ask-you-leave",
                &[("workspace", asked.from_name.clone().into())],
            ));
        }
        let notice = Notice::new(t("window-move-app-name"), 1, title, body).choosing(vec![
            (CARRY.to_owned(), t("window-move-ask-carry")),
            (STAY.to_owned(), t("window-move-ask-stay")),
        ]);
        self.asking.borrow_mut().insert(notice.tag.clone(), asked);
        self.notices.show(&notice);
    }

    /// Carry the tab to the workspace the person chose, and take the record the window shows along.
    fn carry(&self, shared: &Shared, handoff: Option<&WindowFollow>, asked: Asked) {
        let Asked {
            tab,
            from,
            from_name,
            to,
            to_name,
            to_id,
            app,
            app_name,
            window,
            ..
        } = asked;
        // The tab takes the first window the app opens in the destination, and waits there.
        if !shared
            .desktop
            .with_policy(|policy| policy.adopt(&app, tab, None))
        {
            return;
        }
        if let Err(error) = shared.desktop.with_policy(|policy| policy.carry(tab, to)) {
            shared
                .desktop
                .with_policy(|policy| policy.give_up_held(&app));
            tracing::warn!(%error, %app, "the window was not carried");
            return;
        }
        let Some(from_id) = shared.desktop.with_policy(|policy| {
            policy
                .handle()
                .read(|tree| workspace_at(tree, from, &shared.person, &shared.earlier))
        }) else {
            Self::put_back(shared, tab, from, &app);
            self.stayed(
                &app_name,
                &to_name,
                &from_name,
                &t("window-move-reason-storage"),
            );
            return;
        };
        let from_id = from_id.id.to_string();
        let mut crossing = Crossing {
            tab,
            from,
            from_name,
            from_id: from_id.clone(),
            to_name,
            to_id,
            app,
            app_name,
            window,
            record: None,
            handing: None,
            phase: Phase::Finished,
        };
        let Some(id) = Self::travelling(shared, crossing.tab) else {
            // Nothing of the window's own travels: its app opens what the destination keeps.
            self.start(shared, crossing);
            return;
        };
        let Some(record) = Self::record(shared, &crossing, &from_id, id) else {
            self.give_up(shared, &crossing, &t("window-move-reason-storage"));
            return;
        };
        let asked_for = match handoff {
            Some(handoff) => self.save_first(handoff, &crossing),
            // No app of this session can be reached: the move reads what it last saved.
            None => Phase::Finished,
        };
        crossing.record = Some(record);
        if matches!(asked_for, Phase::Finished) {
            self.carry_record(shared, crossing);
        } else {
            self.waiting(crossing, asked_for);
        }
    }

    /// What the window's app holds in memory is asked to reach the workspace before it is read.
    fn save_first(&self, handoff: &WindowFollow, crossing: &Crossing) -> Phase {
        let (answered, answers) = std::sync::mpsc::channel();
        let wake = Arc::clone(&self.wake);
        let token = self.next_token();
        handoff.save(
            &crossing.from_id,
            &crossing.app,
            crossing.tab,
            token,
            move |report| {
                // The pass that takes it up may already have given the move up.
                let _ = answered.send(report);
                wake();
            },
        );
        Phase::Checkpointing(answers)
    }

    /// The person moved `tab` onto `screen` of another of their devices: what its app holds is
    /// saved first, and the device is offered the window, unless it is offline.
    fn send(
        &self,
        shared: &Shared,
        handoff: Option<&WindowFollow>,
        tab: TabId,
        screen: OtherScreen,
    ) {
        let Some((from, from_name)) = shared.desktop.with_policy(|policy| {
            policy.handle().read(|tree| {
                tab_workspace(tree, tab).map(|workspace| (workspace.id(), workspace.name.clone()))
            })
        }) else {
            return;
        };
        let key = tab.to_string();
        let Some(place) = shared
            .layout
            .borrow()
            .reading()
            .places
            .into_iter()
            .find(|place| place.key == key && !place.shows.is_empty())
        else {
            return;
        };
        let offer = ContinuityOffer {
            app_id: place.shows.clone(),
            app_name: app_name(&place.shows),
            resource: place.resource,
            view: place.shown_as,
            to: screen.device.clone(),
            output: screen.output,
            workspace: from.to_string(),
            ..ContinuityOffer::default()
        };
        let crossing = Crossing {
            tab,
            from,
            from_name,
            from_id: from.to_string(),
            to_name: screen.device_name,
            to_id: screen.device,
            app_name: offer.app_name.clone(),
            app: place.shows,
            window: shared.handle.window_of_tab(tab).unwrap_or_default(),
            record: None,
            handing: Some(offer),
            phase: Phase::Finished,
        };
        if !screen.online {
            let why = t_args(
                "window-move-reason-offline",
                &[("device", crossing.to_name.clone().into())],
            );
            self.give_up(shared, &crossing, &why);
            return;
        }
        match handoff {
            // A window of an app this device runs saves what it holds before the other device
            // reads the record.
            Some(handoff) if crossing.window != 0 => {
                let phase = self.save_first(handoff, &crossing);
                self.waiting(crossing, phase);
            }
            _ => self.hand(shared, crossing),
        }
    }

    /// Offer the window to the device it moves to, through the record of the person's devices.
    fn hand(&self, shared: &Shared, mut crossing: Crossing) {
        let Some(devices) = shared.owner_devices.as_ref() else {
            self.give_up(shared, &crossing, &t("window-move-reason-storage"));
            return;
        };
        let Some(offer) = crossing.handing.as_mut() else {
            return;
        };
        let at_ms = i64::try_from(tilo_clock::unix_now_ms()).unwrap_or(i64::MAX);
        offer.from = devices.who().1;
        offer.shown_ms = at_ms;
        offer.until_ms = at_ms.saturating_add(MAX_CONTINUITY_SHOWN_MS);
        devices
            .record()
            .0
            .invoke_unwaited(&Devices::offering_continuity(offer), |made| {
                if let Err(error) = made {
                    tracing::warn!(%error, "a window was not offered to the device it moves to");
                }
            });
        let since = Instant::now();
        self.waiting(crossing, Phase::Opening { since });
    }

    /// The record the window at `tab` shows, when it is a record of its own that may leave the
    /// workspace: one the workspace keeps for the app goes with the workspace.
    fn travelling(shared: &Shared, tab: TabId) -> Option<DocumentId> {
        let (id, _) = shared.notes.window_record(tab)?;
        DocumentId::parse(&id).filter(|id| !id.is_package_primary())
    }

    /// The record `id` the window shows, as the move takes it from where the window shows it to
    /// the workspace the tab went to. `None` while the session holds no workspaces.
    fn record(
        shared: &Shared,
        crossing: &Crossing,
        from_id: &str,
        id: DocumentId,
    ) -> Option<Travelling> {
        let workspaces = shared.device_workspaces.borrow().clone()?;
        let home = launcher::appdir::host_data_home()?;
        Some(Travelling {
            id,
            workspaces,
            daemon: shared.daemon.clone(),
            source: Profile::of(&home, from_id, &crossing.app),
            destination: Profile::of(&home, &crossing.to_id, &crossing.app),
            moved: None,
        })
    }

    /// Take one step of one move, and put it back in flight when it waits for something.
    fn step(&self, shared: &Shared, compositor: &mut Compositor, mut crossing: Crossing) {
        match std::mem::replace(&mut crossing.phase, Phase::Finished) {
            Phase::Checkpointing(answers) => self.checkpointing(shared, crossing, answers),
            Phase::Moving(answers) => self.moving(shared, crossing, answers),
            Phase::Opening { since } => self.opening(shared, compositor, crossing, since),
            Phase::Finished => {}
        }
    }

    /// What the window's app answered: the record moves once the workspace holds everything the
    /// window wrote, and stays where it is when the app did not say so, however long it took to
    /// answer: a big document on a loaded device takes a while, and only its answer, or its
    /// connection closing, says how the save went.
    fn checkpointing(
        &self,
        shared: &Shared,
        crossing: Crossing,
        answers: Receiver<Option<HandoffReport>>,
    ) {
        match answers.try_recv() {
            Err(TryRecvError::Empty) => self.waiting(crossing, Phase::Checkpointing(answers)),
            Ok(Some(HandoffReport::Done { .. })) if crossing.handing.is_some() => {
                self.hand(shared, crossing);
            }
            Ok(Some(HandoffReport::Done { .. })) => self.carry_record(shared, crossing),
            Ok(Some(HandoffReport::Failed { reason, .. })) => {
                tracing::warn!(app = %crossing.app, %reason, "the window's app did not save what it shows");
                self.give_up(shared, &crossing, &t("window-move-reason-unsaved"));
            }
            Ok(None) | Err(TryRecvError::Disconnected) => {
                tracing::warn!(app = %crossing.app, "the window's app gave no answer to the save");
                self.give_up(shared, &crossing, &t("window-move-reason-unsaved"));
            }
        }
    }

    /// Whether the move of `tab` waits on its window's app to save what it shows.
    #[cfg(test)]
    fn checkpoint_asked(&self, tab: TabId) -> bool {
        self.crossing.borrow().iter().any(|crossing| {
            crossing.tab == tab && matches!(crossing.phase, Phase::Checkpointing(_))
        })
    }

    /// What moving the record to where the tab went came to.
    fn moving(
        &self,
        shared: &Shared,
        mut crossing: Crossing,
        answers: Receiver<Result<DocumentId, Fault>>,
    ) {
        match answers.try_recv() {
            Err(TryRecvError::Empty) => self.waiting(crossing, Phase::Moving(answers)),
            Ok(Ok(moved)) => {
                if let Some(record) = crossing.record.as_mut() {
                    record.moved = Some(moved);
                }
                self.start(shared, crossing);
            }
            Ok(Err(fault)) => self.give_up(shared, &crossing, &reason(&fault)),
            Err(TryRecvError::Disconnected) => {
                self.give_up(shared, &crossing, &t("window-move-reason-storage"));
            }
        }
    }

    /// Whether the window the app was started for came where the tab waits: once it did, the
    /// window the tab left closes.
    fn opening(
        &self,
        shared: &Shared,
        compositor: &mut Compositor,
        crossing: Crossing,
        since: Instant,
    ) {
        let landed = match &crossing.handing {
            Some(offer) => answered(shared, offer),
            None => landed(shared, crossing.tab, crossing.window),
        };
        match landed {
            // A window the shell draws, moved to another device, closes as its place.
            Landed::Came if crossing.window == 0 => {
                let _ = shared.handle.dispatch(LayoutEffect::CloseTab {
                    tab: crossing.tab.to_string(),
                });
            }
            Landed::Came => compositor.close_window(crossing.window),
            Landed::Refused(why) if !why.is_empty() => self.give_up(shared, &crossing, &why),
            Landed::Waiting if since.elapsed() < self.opening => {
                self.waiting(crossing, Phase::Opening { since });
            }
            // The tab went while its app started: the record is where the person put it.
            Landed::Gone => {}
            _ if crossing.handing.is_some() => {
                let why = t_args(
                    "window-move-reason-no-answer",
                    &[("device", crossing.to_name.clone().into())],
                );
                self.give_up(shared, &crossing, &why);
            }
            _ => self.give_up(shared, &crossing, &t("window-move-reason-no-window")),
        }
    }

    /// Move the record the window shows to the workspace the tab went to.
    fn carry_record(&self, shared: &Shared, crossing: Crossing) {
        let Some(record) = crossing.record.as_ref() else {
            self.start(shared, crossing);
            return;
        };
        let (daemon, workspaces, source, destination, id, tab) = (
            record.daemon.clone(),
            record.workspaces.clone(),
            record.source.clone(),
            record.destination.clone(),
            record.id.clone(),
            crossing.tab,
        );
        let Some(answers) = window_transfer::off_loop(move || {
            window_transfer::carry(&daemon, &workspaces, (&source, &destination), &id, tab)
        }) else {
            self.give_up(shared, &crossing, &t("window-move-reason-storage"));
            return;
        };
        self.waiting(crossing, Phase::Moving(answers));
    }

    /// Start the window's app in the workspace the person carried the tab to, on the record that
    /// came along when one did.
    fn start(&self, shared: &Shared, crossing: Crossing) {
        let on = crossing
            .record
            .as_ref()
            .and_then(|record| record.moved.as_ref())
            .map(|moved| Launch::Record {
                app: crossing.app.clone(),
                record: moved.as_str().to_owned(),
                visitor: false,
            });
        let launch = on.unwrap_or_else(|| Launch::Package {
            app: crossing.app.clone(),
            open: None,
        });
        let started = shared.launch.as_deref().is_some_and(|apps| {
            let workspace = apps.workspace_named(&crossing.to_id);
            match apps.start_beside(&launch, &workspace, Some(crossing.tab), None) {
                Ok(()) => true,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        app = %crossing.app,
                        workspace = %crossing.to_name,
                        "an app did not start where the person carried its window"
                    );
                    false
                }
            }
        });
        if !started {
            self.give_up(shared, &crossing, &t("window-move-reason-no-start"));
            return;
        }
        let since = Instant::now();
        self.waiting(crossing, Phase::Opening { since });
    }

    /// The record the move took to where the tab went goes back where it was, as the person: the
    /// window the tab goes back to shows it there.
    fn bring_back(crossing: &Crossing) {
        let Some(record) = crossing.record.as_ref() else {
            return;
        };
        let Some(moved) = record.moved.clone() else {
            return;
        };
        let (daemon, source, destination, id) = (
            record.daemon.clone(),
            record.source.clone(),
            record.destination.clone(),
            record.id.clone(),
        );
        if let Err(error) = window_transfer::off_loop_detached(move || {
            if let Err(fault) =
                window_transfer::move_record(&daemon, &destination, &source, &moved, &id)
            {
                tracing::warn!(?fault, "a record stayed where a failed move took it");
            }
        }) {
            tracing::warn!(%error, "a record stayed where a failed move took it");
        }
    }

    /// The move cannot be made: the record goes back where it was, the tab goes back too, and the
    /// person is told why.
    fn give_up(&self, shared: &Shared, crossing: &Crossing, why: &str) {
        // A window offered to another device never left: nothing of it comes back.
        if crossing.handing.is_none() {
            Self::bring_back(crossing);
            Self::put_back(shared, crossing.tab, crossing.from, &crossing.app);
        }
        self.stayed(
            &crossing.app_name,
            &crossing.to_name,
            &crossing.from_name,
            why,
        );
    }

    /// Nothing crossed: the tab goes back where it was, still showing the window it had.
    fn put_back(shared: &Shared, tab: TabId, from: WorkspaceId, app: &str) {
        shared
            .desktop
            .with_policy(|policy| policy.give_up_held(app));
        let _ = shared.desktop.with_policy(|policy| policy.carry(tab, from));
    }

    /// Say the window `app` shows did not move to `place` (a workspace or a device) and stayed in
    /// the workspace `source`, and why.
    fn stayed(&self, app: &str, place: &str, source: &str, why: &str) {
        let title = t_args(
            "window-move-stayed-title",
            &[
                ("app", app.to_owned().into()),
                ("place", place.to_owned().into()),
            ],
        );
        let body = t_args(
            "window-move-stayed-body",
            &[
                ("workspace", source.to_owned().into()),
                ("reason", why.to_owned().into()),
            ],
        );
        self.notices
            .show(&Notice::new(t("window-move-app-name"), 1, title, body));
    }

    /// Put the move back in flight, waiting for `phase` .
    fn waiting(&self, mut crossing: Crossing, phase: Phase) {
        crossing.phase = phase;
        self.crossing.borrow_mut().push(crossing);
    }

    fn next_token(&self) -> u64 {
        let token = self.tokens.get().wrapping_add(1);
        self.tokens.set(token);
        token
    }
}

impl crate::Session {
    /// Carry out the person's answers and carries since the last pass: asked first
    /// where other people are involved, then the record moves along, the app runs there,
    /// and the window it left closes once the app has one there.
    pub(crate) fn follow_window_moves(&mut self) {
        // The window's app is asked through the device's dispatch, as the person, on the shell's
        // link.
        let handoff = self
            .shared
            .daemon
            .link()
            .ok()
            .map(|link| crate::window_follow::WindowFollow::new(Arc::new(link.dispatch())));
        self.shared
            .moves
            .follow(&self.shared, &mut self.compositor, handoff.as_ref());
    }
}

/// Whether the tab a move waits on took the window the app started for it.
enum Landed {
    /// The tab holds another window of the app now: the move landed.
    Came,
    /// The window has not mapped yet: the tab still shows the one it had.
    Waiting,
    /// The tab is gone.
    Gone,
    /// The device the window was offered to could not open it, and says why.
    Refused(String),
}

/// Whether the tab at `tab` took a window other than `window` .
fn landed(shared: &Shared, tab: TabId, window: u64) -> Landed {
    shared.desktop.with_policy(|policy| {
        policy.handle().read(|tree| match tab_tile(tree, tab) {
            None => Landed::Gone,
            Some(TileContent::Foreign { surface_id, .. }) if *surface_id != window => Landed::Came,
            Some(_) => Landed::Waiting,
        })
    })
}

/// What the device `offer` was made to said of it: it opened it, could not and why, or nothing yet.
fn answered(shared: &Shared, offer: &ContinuityOffer) -> Landed {
    let answer = shared.owner_devices.as_ref().and_then(|devices| {
        let record = devices.devices();
        let kept = record
            .continuity
            .get(&tilo_resource::Id::from_raw(offer.from.clone()))?;
        (kept.shown_ms == offer.shown_ms).then(|| kept.answered_by(&offer.to).cloned())?
    });
    match answer {
        Some(answer) if answer.taken => Landed::Came,
        Some(answer) => Landed::Refused(answer.reason),
        None => Landed::Waiting,
    }
}

/// Why a step of a move failed, as the person is told it.
fn reason(fault: &Fault) -> String {
    match fault {
        Fault::Refused(error) => {
            tracing::warn!(%error, "a record could not be moved");
            t("window-move-reason-record")
        }
        Fault::Profile(error) => {
            tracing::warn!(%error, "a window's app profile could not be read");
            t("window-move-reason-storage")
        }
    }
}

/// A workspace as a move reads it.
struct WorkspaceRead {
    id: WorkspaceId,
    name: String,
    /// Whether anyone besides this person belongs to it: what it holds is shared with them whether
    /// or not they are here.
    others: bool,
}

/// What stands at `tab` : the window filling it, the app that window belongs to, and the workspace
/// it is in.
struct TabWindow {
    app: String,
    window: u64,
    workspace: WorkspaceRead,
}

/// The tile `tab` holds, as the shell has it.
fn tab_tile(tree: &Layout, tab: TabId) -> Option<&TileContent> {
    tree.workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .find(|held| held.id == tab)
        .map(|held| &held.tile)
}

/// The workspace the tab `tab` stands in.
fn tab_workspace(tree: &Layout, tab: TabId) -> Option<&Workspace> {
    tree.workspaces.iter().find(|workspace| {
        workspace.strips.iter().any(|strip| {
            strip
                .window_groups
                .iter()
                .any(|group| group.tabs.iter().any(|held| held.id == tab))
        })
    })
}

/// The window `tab` shows and the workspace the tab stands in; `None` when the tab is not on the
/// shell or holds no window of an app this device started.
fn tab_window(tree: &Layout, tab: TabId, person: &str, earlier: &[String]) -> Option<TabWindow> {
    let TileContent::Foreign { app_id, surface_id } = tab_tile(tree, tab)? else {
        return None;
    };
    let workspace = tab_workspace(tree, tab)?;
    Some(TabWindow {
        app: app_id.clone(),
        window: *surface_id,
        workspace: read(workspace, person, earlier),
    })
}

fn workspace_at(
    tree: &Layout,
    id: WorkspaceId,
    person: &str,
    earlier: &[String],
) -> Option<WorkspaceRead> {
    tree.workspaces
        .iter()
        .find(|workspace| workspace.id() == id)
        .map(|workspace| read(workspace, person, earlier))
}

fn read(workspace: &Workspace, person: &str, earlier: &[String]) -> WorkspaceRead {
    let mine = |id: &str| id == person || earlier.iter().any(|old| old == id);
    WorkspaceRead {
        id: workspace.id(),
        name: workspace.name.clone(),
        others: workspace
            .policy
            .readers_at(tilo_clock::unix_now())
            .into_iter()
            .any(|(reader, _)| !mine(&reader)),
    }
}

/// What the person calls `app` , when this machine knows a package by that name.
fn app_name(app: &str) -> String {
    app_catalogue::apps::read_one_from_env(app)
        .ok()
        .flatten()
        .map(|installed| installed.display_name)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| app.to_owned())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use collab_types::store::wire::{Command, Edit, Reply};
    use tilo_action_policy::Role;
    use tilo_compositor::OutputConfig;
    use tilo_desktop_model::{Strip, StripId};
    use tilo_desktop_policy::Window;
    use tilo_session_core::desktop::state::banners::Banners as _;
    use tilo_session_core::desktop::state::window_menu::move_to_workspace_id;

    use crate::apps::Apps;
    use crate::{AppActions, Options, Session};

    use super::*;

    /// The app a test installs: a Tilo package whose window fills a tab.
    const PROBE: &str = "so.tilo.probe";

    /// The kind of record the app keeps a window of: a note of its own.
    const NOTE: &str = "probe.note";

    /// The person a workspace of the tests' is founded for, when the person themselves may not
    /// write there.
    const ANOTHER: [u8; 32] = [0x9e; 32];

    /// What the app's package declares of its records: the note, and the verb that writes one.
    const DECLARED: &str = r#"format = 2

[[resources]]
kind = "probe.note"
label = "Note"
version = 1
fields = [
  { path = "body", type = "text" },
]
verbs = [
  { id = "write", label = "Write", kind = "undoable" },
]
"#;

    /// The display every headless session here draws on.
    const WIDTH: i32 = 1280;
    /// See [`WIDTH`].
    const HEIGHT: i32 = 800;

    /// A workspace owned by `owner` , with `members` in it.
    fn workspace(owner: &str, members: &[&str]) -> Workspace {
        let mut workspace = Workspace::new(WorkspaceId::new(), "Studio");
        workspace.policy.set_owner(owner.to_owned());
        for member in members {
            workspace
                .policy
                .add_member((*member).to_owned(), Role::Editor);
        }
        workspace
    }

    /// A move crosses a sharing boundary exactly when someone besides the person may read the
    /// workspace it leaves or enters: the window and the record it takes are theirs to see, or
    /// leaving takes it from them.
    #[test]
    fn a_move_is_shared_when_someone_else_may_read_either_workspace() {
        let alone = workspace("pat", &[]);
        assert!(
            !read(&alone, "pat", &[]).others,
            "the person's own workspace"
        );
        assert!(
            !read(&alone, "pat", &["pat-was".to_owned()]).others,
            "a workspace under the name this person held before"
        );
        assert!(
            !read(&workspace("pat-was", &[]), "pat", &["pat-was".to_owned()]).others,
            "one of the person's own is its owner"
        );
        assert!(
            read(&workspace("pat", &["ada"]), "pat", &[]).others,
            "a member is in it"
        );
        assert!(
            read(&workspace("ada", &["pat"]), "pat", &[]).others,
            "someone else owns it"
        );
    }

    /// A shell with a session on it, and the window of [`PROBE`] in a tab of the workspace in front.
    struct Shell {
        _home: tilo_test_env::TestHome,
        /// What the window's app is told about each save it is asked, and where it says it
        /// listens: [`window_app`].
        app: std::os::unix::net::UnixStream,
        /// Where every start of the app connects to be told: kept, so a later start finds it.
        _apps: std::os::unix::net::UnixListener,
        /// The device's daemon, holding both workspaces on a real workspace service: the
        /// session's shell link, where the move runs as whoever the session is of.
        device: crate::daemon_fixture::TestDaemon,
        /// Where each start is written down.
        started: PathBuf,
        /// What each start was handed.
        arguments: PathBuf,
        session: Session,
        /// The workspace the window is in, and the one behind it.
        from: String,
        to: WorkspaceId,
        to_id: String,
        /// The tab the window fills.
        tab: TabId,
    }

    impl Shell {
        /// A shell of two workspaces of the person's own.
        fn new() -> Self {
            Self::new_elsewhere(None)
        }

        /// A shell of two workspaces, the one `elsewhere` names founded for [`ANOTHER`]: this
        /// device stays one of its administrators, and the person is not its owner.
        #[expect(
            clippy::too_many_lines,
            reason = "one test's whole flow, from its setup to what it proves"
        )]
        fn new_elsewhere(elsewhere: Option<WorkspaceId>) -> Self {
            let mut home = tilo_test_env::TestHome::new().expect("a home");
            let started = home.home().join("started");
            let arguments = home.home().join("arguments");
            install(&mut home);
            // The launches the session records, which the device's daemon admits each app's
            // connection by.
            let launches = std::sync::Arc::new(crate::Launches::default());
            // The workspace in front and the one behind it, as the shell below shows them, kept by
            // the device's daemon, whose store knows the app's records as its package declares them.
            let (from, to) = (WorkspaceId::from_u128(1), WorkspaceId::from_u128(2));
            let (from_id, to_id) = (from.to_string(), to.to_string());
            let elsewhere = elsewhere.map(|workspace| (workspace.to_string(), ANOTHER));
            let device = crate::daemon_fixture::TestDaemon::writing_launched_for(
                &[&from_id, &to_id],
                elsewhere
                    .as_ref()
                    .map(|(workspace, owner)| (workspace.as_str(), *owner)),
                Some(std::sync::Arc::clone(&launches)
                    as std::sync::Arc<
                        dyn tilo_app_host::services::LauncherServices,
                    >),
            );
            let control = home.home().join("app.sock");
            let apps_told =
                std::os::unix::net::UnixListener::bind(&control).expect("the app's control socket");
            // The launcher records what it was asked, then runs the window's app as the launcher
            // runs a package's: the app connects to the device's daemon itself.
            let stand_in = tilo_test_env::stand_in(
                home.home().join("tilo-launcher"),
                &format!(
                    "printf '%s\\n' \"$$\" >> '{started}'\n\
                     printf '%s\\n' \"$*\" >> '{arguments}'\n\
                     exec '{app}' '{socket}' '{control}' '{PROBE}' \"${{TILO_CONTINUITY_INSTANCE:-}}\" \"$@\"\n",
                    started = started.display(),
                    arguments = arguments.display(),
                    app = window_app().display(),
                    socket = device.socket().display(),
                    control = control.display(),
                ),
            )
            .expect("a launcher");
            device.workspaces().declare_package_surface(
                PROBE,
                tilo_surface_compile::compile(DECLARED)
                    .expect("the surface compiles")
                    .as_bytes(),
            );
            let link = device.shell_link();
            let state = home.state().to_path_buf();
            let mut environment = tilo_app_launch::SessionEnvironment::from_process();
            environment.wayland_socket = Some(home.runtime().join("wayland-7"));
            let apps = Rc::new(Apps::with_launcher(
                environment,
                None,
                Some(stand_in.path().to_owned()),
            ));
            let mut session = Session::headless(
                Options {
                    presentation: ui::headless::fixture_runner_inputs,
                    collab: false,
                    launches,
                    apps: Some(Rc::clone(&apps) as Rc<dyn AppActions>),
                    launch: Some(Rc::clone(&apps)),
                    state: Some(state),
                    shell_link: Some(link),
                    ..Options::default()
                },
                &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
            )
            .expect("a session");
            // The shell this device shows, as a session that came back to one puts it in front.
            session.handle().replace_tree(two_workspaces());
            *session.shared.device_workspaces.borrow_mut() = Some(device.device_workspaces());
            // The person opens the app: the shell holds a tab for its window, which the window's
            // app is started as.
            session
                .desktop()
                .with_policy(|policy| {
                    policy.handle().dispatch(
                        tilo_session_core::desktop::navigation::ChromeEffect::OpenApp {
                            app: PROBE.into(),
                            beside: None,
                            background: None,
                            float: None,
                        },
                    )
                })
                .expect("the person opens the app");
            wait(&mut session, "the app is started", |_| {
                (starts(&started) == 1).then_some(())
            });
            let (app, instance) = listening(&mut session, &apps_told);
            session
                .desktop()
                .with_policy(|policy| policy.map(window(11, PROBE)))
                .expect("the window maps");
            let tab = wait(&mut session, "the window takes a tab", |session| {
                let tabs = tabs_of(session);
                tabs.iter().find_map(|tab| match tile_of(session, *tab) {
                    Some(TileContent::Foreign { surface_id: 11, .. }) => Some(*tab),
                    _ => None,
                })
            });
            assert_eq!(
                tab, instance,
                "the window takes the tab the shell held for it, which its app was started as"
            );
            Self {
                _home: home,
                app,
                _apps: apps_told,
                device,
                started,
                arguments,
                session,
                from: from_id,
                to,
                to_id,
                tab,
            }
        }

        /// The record the window shows: a note the app wrote in the workspace it is in.
        fn showing(&self, body: &str) -> DocumentId {
            let id = DocumentId::minted();
            let stamp = tilo_resource_store::Stamp {
                verb: "write".to_owned(),
                invocation: uuid::Uuid::new_v4().to_string(),
                ..tilo_resource_store::Stamp::default()
            };
            let written = self.command(
                &self.from,
                Command::Batch(vec![
                    Command::Put {
                        kind: NOTE.to_owned(),
                        id: id.as_str().to_owned(),
                        resource: collab_types::store::Resource {
                            readers: collab_types::EVERY_MEMBER.to_owned(),
                            parent: None,
                            order: None,
                            fields: std::collections::BTreeMap::new(),
                        },
                        stamp: stamp.clone(),
                    },
                    Command::Edit {
                        kind: NOTE.to_owned(),
                        id: id.as_str().to_owned(),
                        edits: vec![Edit::Splice {
                            field: "body".to_owned(),
                            at: 0,
                            delete: 0,
                            insert: body.to_owned(),
                        }],
                        stamp,
                    },
                ]),
            );
            assert!(
                matches!(written, Ok(Reply::Written)),
                "the workspace keeps the note: {written:?}"
            );
            // The window's app holds the record it shows, as its window reports it, which the
            // session keeps beside the window: what the daemon's broker reads afresh each pass
            // would name nothing until the app holds it.
            self.session
                .shared
                .notes
                .note(self.tab, PROBE, &self.from, id.as_str());
            id
        }

        /// `command` as the app's store answers it in `workspace`, for the device's own person.
        fn command(&self, workspace: &str, command: Command) -> Result<Reply, String> {
            command_in(self.device.workspaces(), workspace, command)
        }

        /// What the note `id` of `workspace` holds; `None` while the workspace holds no such note.
        fn note(&self, workspace: &str, id: &DocumentId) -> Option<String> {
            note_in(self.device.workspaces(), workspace, id)
        }

        /// The notes of `workspace`, by id.
        fn notes(&self, workspace: &str) -> Vec<String> {
            notes_in(self.device.workspaces(), workspace)
        }

        /// The person moves the window to the workspace behind.
        fn carry(&mut self) {
            self.carry_asking();
            self.saved();
        }

        /// The person moves the window to the workspace behind, and the move asks the window's
        /// app to make what it shows durable, and waits on it.
        fn carry_asking(&mut self) {
            self.session
                .handle()
                .choose_window_menu(self.tab, &move_to_workspace_id(self.to), None)
                .expect("the person moves the window");
            let tab = self.tab;
            wait(
                &mut self.session,
                "the move asks the app to save",
                |session| session.shared.moves.checkpoint_asked(tab).then_some(()),
            );
        }

        /// The window's app goes on with the save it was asked as `word` says: `saved`, `failed`,
        /// or `quit` mid-save.
        fn answer(&mut self, word: &str) {
            use std::io::Write as _;
            writeln!(self.app, "{word}").expect("the app hears how its save goes");
        }

        /// The window's app wrote everything it shows to the workspace, and says so.
        fn saved(&mut self) {
            self.answer("saved");
        }

        /// What the person typed last in the note `id`, `typed` at its end, as the window's app
        /// writes it to the workspace when it saves.
        fn typed(&self, id: &DocumentId, typed: &str) {
            let held = self.note(&self.from, id).expect("the note is here");
            let written = self.command(
                &self.from,
                Command::Edit {
                    kind: NOTE.to_owned(),
                    id: id.as_str().to_owned(),
                    edits: vec![Edit::Splice {
                        field: "body".to_owned(),
                        at: held.chars().count(),
                        delete: 0,
                        insert: typed.to_owned(),
                    }],
                    stamp: tilo_resource_store::Stamp {
                        verb: "write".to_owned(),
                        invocation: uuid::Uuid::new_v4().to_string(),
                        ..tilo_resource_store::Stamp::default()
                    },
                },
            );
            assert!(
                matches!(written, Ok(Reply::Written)),
                "the workspace keeps what was typed: {written:?}"
            );
        }

        /// `workspace` answers what the person writes there with `answer`, as its owners
        /// granted: reading stays theirs, so what they did there still reads.
        fn grant(&self, workspace: &str, answer: tilo_action_policy::Answer) {
            use tilo_action_policy::{Grant, Grantee, TreePath};
            use tilo_action_types::KindSet;
            let person = hex::encode(self.device.certificate().user_public_key);
            let workspaces = self.device.workspaces().clone();
            let workspace = workspace.to_owned();
            tilo_rt::app()
                .block_on(async move {
                    tilo_workspace_service::workspaces::fixtures::record_grants(
                        &workspaces,
                        &workspace,
                        &[Grant::new(
                            "mover",
                            Grantee::person(person.clone()),
                            TreePath::workspace(&workspace),
                            KindSet::UNDOABLE,
                            answer,
                            Grantee::person(person),
                        )],
                    )
                    .await;
                })
                .expect("the runtime runs");
        }

        /// Wait for the app the move starts where the window went, and what it was handed: the
        /// workspace, and the record it opens.
        fn arrived(&mut self) -> (String, DocumentId) {
            let told = wait(
                &mut self.session,
                "the app runs where the window went",
                |session| {
                    // The launcher writes a start's pid before what it was handed, so a second
                    // pid is not yet a second start to read: wait for the second start's
                    // arguments, which is what the assertions below read. A start that names
                    // another workspace than the move's is read and reported there.
                    let started_twice = started_with(&self.arguments) == 2
                        && handed(&self.arguments, "--workspace-id").is_some();
                    if started_twice {
                        return Some(None);
                    }
                    session
                        .notifications()
                        .feed()
                        .banners
                        .into_iter()
                        .find(|banner| banner.notification.title.contains("Probe"))
                        .map(|banner| Some(banner.notification.body))
                },
            );
            assert_eq!(told, None, "the move is made, not put back");
            assert_eq!(
                handed(&self.arguments, "--workspace-id").as_deref(),
                Some(self.to_id.as_str()),
                "the app runs under the grants of the workspace the window went to"
            );
            let opened = handed(&self.arguments, "--open-uri")
                .expect("the launch opens the record it shows");
            let id = DocumentId::from_uri(&opened)
                .expect("the launch opens the record the window carried");
            (opened, id)
        }

        /// The window the app opened in the destination takes the tab.
        fn land(&mut self) {
            self.session
                .desktop()
                .with_policy(|policy| policy.map(window(12, PROBE)))
                .expect("the window maps");
            wait(
                &mut self.session,
                "the new window takes the tab",
                |session| {
                    matches!(
                        tile_of(session, self.tab),
                        Some(TileContent::Foreign { surface_id: 12, .. })
                    )
                    .then_some(())
                },
            );
        }

        /// What the person is told about the app's window, once they are: its body.
        fn told(&mut self) -> String {
            wait(&mut self.session, "the person is told", |session| {
                session
                    .notifications()
                    .feed()
                    .banners
                    .into_iter()
                    .find(|banner| banner.notification.title.contains("Probe"))
                    .map(|banner| banner.notification.body)
            })
        }
    }

    /// `command` as the app's store in `workspace` of `workspaces` answers it, for the device's
    /// own person.
    fn command_in(
        workspaces: &tilo_workspace_service::workspaces::Workspaces,
        workspace: &str,
        command: Command,
    ) -> Result<Reply, String> {
        tilo_rt::app()
            .block_on(workspaces.own_resource_command(workspace, PROBE, command, Vec::new()))
            .expect("the runtime runs")
    }

    /// The notes of `workspace` the store keeps, by id.
    fn notes_in(
        workspaces: &tilo_workspace_service::workspaces::Workspaces,
        workspace: &str,
    ) -> Vec<String> {
        workspaces
            .stored_records(workspace)
            .expect("the device keeps the workspace")
            .into_iter()
            .filter(|(app, _)| app == PROBE)
            .map(|(_, id)| id)
            .collect()
    }

    /// What the note `id` of `workspace` holds; `None` while the workspace holds no such note.
    fn note_in(
        workspaces: &tilo_workspace_service::workspaces::Workspaces,
        workspace: &str,
        id: &DocumentId,
    ) -> Option<String> {
        let found = command_in(
            workspaces,
            workspace,
            Command::Get {
                kind: NOTE.to_owned(),
                id: id.as_str().to_owned(),
            },
        );
        if !matches!(found, Ok(Reply::Resource(Some(_)))) {
            return None;
        }
        match command_in(
            workspaces,
            workspace,
            Command::Content {
                kind: NOTE.to_owned(),
                id: id.as_str().to_owned(),
            },
        ) {
            Ok(Reply::Content(Some(content))) => Some(
                content
                    .get("body")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            ),
            other => panic!("the note's content reads: {other:?}"),
        }
    }

    /// The shell the person is at: the workspace in front, with a strip the app's window takes, and
    /// one behind it.
    fn two_workspaces() -> Layout {
        let mut home = Workspace::new(WorkspaceId::from_u128(1), "Home");
        home.strips = vec![Strip::new(StripId(1), "Main")];
        let mut tree = Layout {
            active: home.id(),
            workspaces: vec![home],
            ..Layout::default()
        };
        tree.active_strip
            .insert(WorkspaceId::from_u128(1), StripId(1));
        tree.set_active_group(StripId(1), 10);
        let mut away = Workspace::new(WorkspaceId::from_u128(2), "Away");
        away.strips = vec![Strip::new(StripId(3), "Later")];
        tree.workspaces.push(away);
        tree
    }

    /// Install [`PROBE`] in the home's own app store: a package whose entrypoint is the launcher's
    /// to start.
    fn install(home: &mut tilo_test_env::TestHome) {
        let store = home.data().join("tilo-apps");
        let payload = store.join("payload").join(PROBE);
        fs::create_dir_all(payload.join("bin")).expect("the package's directory");
        let manifest = payload.join("manifest.toml");
        fs::write(
            &manifest,
            format!(
                "[package]\n\
                 id = \"{PROBE}\"\n\
                 version = \"1.0.0\"\n\
                 kind = \"app\"\n\
                 name = \"Probe\"\n\
                 \n\
                 [publisher]\n\
                 identity = \"ed25519:{key}\"\n\
                 signature = \"\"\n\
                 \n\
                 [sandbox]\n\
                 namespaces = [\"user\"]\n\
                 seccomp = \"tilo-default\"\n\
                 landlock = \"strict\"\n\
                 \n\
                 [sandbox.net]\n\
                 mode = \"off\"\n\
                 \n\
                 [entrypoint]\n\
                 bin = \"bin/app\"\n\
                 type = \"gui\"\n\
                 \n\
                 [providers]\n\
                 schemes = [\"probe\"]\n",
                key = "11".repeat(32),
            ),
        )
        .expect("the package's manifest");
        fs::write(
            store.join("registry.toml"),
            format!(
                "[[entry]]\nid = \"{PROBE}\"\nversion = \"1.0.0\"\n\
                 payload_root = \"{payload}\"\nmanifest_path = \"{manifest}\"\n",
                payload = payload.display(),
                manifest = manifest.display(),
            ),
        )
        .expect("the app registry");
        home.env().set("TILO_APPS_DATA_DIR", &store);
    }

    /// The window's app the launcher runs: [`crate`]'s `tilo-window-probe`.
    fn window_app() -> PathBuf {
        tilo_test_env::sibling_binary("tilo-session", "tilo-window-probe")
    }

    /// The window's app the session started, once it says it listens on its window's route: what
    /// it is told about each save from then on, and the window it was started as.
    fn listening(
        session: &mut Session,
        apps: &std::os::unix::net::UnixListener,
    ) -> (std::os::unix::net::UnixStream, TabId) {
        use std::io::BufRead as _;
        apps.set_nonblocking(true).expect("the control socket");
        let (app, _) = wait(session, "the app connects", |_| apps.accept().ok());
        app.set_nonblocking(false)
            .expect("the app's control connection");
        app.set_read_timeout(Some(tilo_test_env::hang_bound()))
            .expect("the app's control connection");
        let mut said = String::new();
        std::io::BufReader::new(app.try_clone().expect("the app's control connection"))
            .read_line(&mut said)
            .expect("the app says it listens");
        let instance = said
            .trim()
            .strip_prefix("listening ")
            .and_then(|instance| instance.parse().ok())
            .unwrap_or_else(|| panic!("the app listens on its window's route: {said:?}"));
        (app, instance)
    }

    /// Run frames until `asked` answers.
    #[track_caller]
    fn wait<T>(
        session: &mut Session,
        what: &str,
        mut asked: impl FnMut(&Session) -> Option<T>,
    ) -> T {
        let deadline = std::time::Instant::now()
            .checked_add(tilo_test_env::hang_bound())
            .expect("a test deadline");
        loop {
            if let Some(answer) = asked(session) {
                return answer;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "{what}: not reached in {:?}",
                tilo_test_env::hang_bound()
            );
            session.step(Duration::from_millis(1)).expect("a frame");
        }
    }

    /// How many starts the launcher wrote down.
    fn starts(started: &Path) -> usize {
        fs::read_to_string(started)
            .map(|written| written.lines().count())
            .unwrap_or_default()
    }

    /// How many starts the launcher wrote down the arguments of: it writes a start's pid before
    /// what it was handed, so a start is whole here a moment after [`starts`] sees it.
    fn started_with(arguments: &Path) -> usize {
        fs::read_to_string(arguments)
            .map(|written| written.lines().count())
            .unwrap_or_default()
    }

    /// What the last start the launcher recorded was handed for `flag` , while it recorded one that
    /// names `flag` : a start's pid reaches the record before what it was handed, so a caller that
    /// needs a start's arguments waits until they are there.
    fn handed(arguments: &Path, flag: &str) -> Option<String> {
        let recorded = fs::read_to_string(arguments).ok()?;
        let arguments: Vec<&str> = recorded.lines().next_back()?.split_whitespace().collect();
        let at = arguments.iter().position(|argument| *argument == flag)?;
        arguments
            .get(at.saturating_add(1))
            .map(|value| (*value).to_owned())
    }

    /// Every tab of the shell.
    fn tabs_of(session: &Session) -> Vec<TabId> {
        session.handle().read(|tree| {
            tree.workspaces
                .iter()
                .flat_map(|workspace| &workspace.strips)
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .map(|tab| tab.id)
                .collect()
        })
    }

    /// What the tab `id` holds.
    fn tile_of(session: &Session, id: TabId) -> Option<TileContent> {
        session.handle().read(|tree| {
            tree.workspaces
                .iter()
                .flat_map(|workspace| &workspace.strips)
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .find(|tab| tab.id == id)
                .map(|tab| tab.tile.clone())
        })
    }

    /// The workspace the tab `id` stands in.
    fn workspace_of(session: &Session, id: TabId) -> Option<WorkspaceId> {
        session.handle().read(|tree| {
            tree.workspaces
                .iter()
                .find(|workspace| {
                    workspace.strips.iter().any(|strip| {
                        strip
                            .window_groups
                            .iter()
                            .any(|group| group.tabs.iter().any(|tab| tab.id == id))
                    })
                })
                .map(tilo_desktop_model::Workspace::id)
        })
    }

    /// A window of `app` the compositor mapped.
    fn window(id: u64, app: &str) -> Window {
        Window {
            id,
            app_id: Some(app.into()),
            title: Some(app.into()),
            ..Window::default()
        }
    }

    /// A window the person moves to another workspace takes the record it shows along: the
    /// workspace it went to holds the record, under an id of its own there, with what the person
    /// wrote in it; the app runs there on it, its window takes the tab, and the workspace it left
    /// holds it no more.
    #[test]
    fn a_moved_window_takes_the_record_it_shows_to_the_workspace_it_moves_to() {
        let _settled = tilo_ui_base::motion::testing::settled();
        let mut shell = Shell::new();
        let showing = shell.showing("first draft");
        shell.carry();
        let (_, arrived) = shell.arrived();
        assert_ne!(
            arrived, showing,
            "the record arrives under an id of the workspace it went to"
        );
        shell.land();
        assert_eq!(
            shell.note(&shell.to_id, &arrived).as_deref(),
            Some("first draft"),
            "the record is kept where it went, holding what the person wrote"
        );
        assert_eq!(
            shell.note(&shell.from, &showing),
            None,
            "the workspace it left holds it no more"
        );
        assert_eq!(
            workspace_of(&shell.session, shell.tab),
            Some(shell.to),
            "the tab is where the person put it"
        );
    }

    /// An app that opens no window where the person put its tab: the tab goes back where it was,
    /// still showing the window it had, the record goes back to the workspace it left, holding
    /// what it said, nothing of it stays where the tab went, and the person is told why.
    #[test]
    fn a_move_whose_app_opens_no_window_puts_the_tab_and_its_record_back() {
        // The move gives up on the window a moment from now, rather than after production's half
        // minute: what the person is told and what the tab does are the same, sooner.
        const GIVING_UP: Duration = Duration::from_millis(300);
        let _settled = tilo_ui_base::motion::testing::settled();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(OPENING_ENV, GIVING_UP.as_millis().to_string());
        let mut shell = Shell::new();
        let showing = shell.showing("first draft");
        shell.carry();
        let _ = shell.arrived();
        // The app starts and runs, but never opens a window there.
        let said = shell.told();
        assert!(!said.trim().is_empty(), "and why: {said:?}");
        assert_eq!(
            workspace_of(&shell.session, shell.tab),
            Some(WorkspaceId::from_u128(1)),
            "the tab went back where it was"
        );
        assert_eq!(
            tile_of(&shell.session, shell.tab),
            Some(TileContent::Foreign {
                surface_id: 11,
                app_id: PROBE.into()
            }),
            "still showing the window it had"
        );
        let (workspaces, from) = (shell.device.workspaces().clone(), shell.from.clone());
        let back = wait(
            &mut shell.session,
            "the record is back where it was",
            |_| note_in(&workspaces, &from, &showing),
        );
        assert_eq!(back, "first draft", "saying what it said");
        let (workspaces, to) = (shell.device.workspaces().clone(), shell.to_id.clone());
        wait(
            &mut shell.session,
            "nothing of it stays where the tab went",
            |_| notes_in(&workspaces, &to).is_empty().then_some(()),
        );
    }

    /// What the person typed just before moving the window is in the record where it went: the
    /// move waits until the window's app saved what it shows, and nothing moves before.
    #[test]
    fn an_edit_made_just_before_a_move_arrives_where_the_window_went() {
        let _settled = tilo_ui_base::motion::testing::settled();
        let mut shell = Shell::new();
        let showing = shell.showing("first draft");
        shell.carry_asking();
        for _ in 0..5 {
            shell
                .session
                .step(Duration::from_millis(1))
                .expect("a frame");
        }
        assert_eq!(
            shell.note(&shell.from, &showing).as_deref(),
            Some("first draft"),
            "nothing moves before the app saved"
        );
        shell.typed(&showing, ", and the last word");
        shell.saved();
        let (_, arrived) = shell.arrived();
        shell.land();
        assert_eq!(
            shell.note(&shell.to_id, &arrived).as_deref(),
            Some("first draft, and the last word"),
            "the record arrives with everything the person typed"
        );
        assert_eq!(shell.note(&shell.from, &showing), None);
    }

    /// A window whose app cannot save what it shows stays where it is with its record, and the
    /// person is told: moving it would leave what they typed last behind.
    #[test]
    fn a_move_whose_app_cannot_save_leaves_everything_where_it_was() {
        let _settled = tilo_ui_base::motion::testing::settled();
        let mut shell = Shell::new();
        let showing = shell.showing("first draft");
        shell.carry_asking();
        shell.answer("failed");
        refused(&mut shell, &showing, "window-move-reason-unsaved");
    }

    /// A window whose app goes away while it saves (it crashed, or the person quit it) answers
    /// nothing, however long the move would wait: its connection closing ends the save, and the
    /// window stays where it is with its record, and the person is told.
    #[test]
    fn a_move_whose_app_goes_away_while_it_saves_leaves_everything_where_it_was() {
        let _settled = tilo_ui_base::motion::testing::settled();
        let mut shell = Shell::new();
        let showing = shell.showing("first draft");
        shell.carry_asking();
        shell.answer("quit");
        refused(&mut shell, &showing, "window-move-reason-unsaved");
    }

    /// A move into a workspace where the person may not write the app's records is refused: the
    /// tab stays where it was with the window it had, the record stays in the workspace it is in,
    /// saying what it said, nothing of it is written where the tab was going, and the person is
    /// told.
    #[test]
    fn a_move_into_a_workspace_the_person_may_not_write_leaves_everything_where_it_was() {
        let _settled = tilo_ui_base::motion::testing::settled();
        // The workspace behind belongs to someone else, who let the person write none of its
        // records.
        let mut shell = Shell::new_elsewhere(Some(WorkspaceId::from_u128(2)));
        let showing = shell.showing("first draft");
        shell.grant(&shell.from.clone(), tilo_action_policy::Answer::Allow);
        shell.grant(&shell.to_id.clone(), tilo_action_policy::Answer::Deny);
        shell.carry();
        refused(&mut shell, &showing, "window-move-reason-record");
    }

    /// A move out of a workspace where the person may not remove the app's records is refused
    /// alike, though they may write them where the tab was going.
    #[test]
    fn a_move_out_of_a_workspace_the_person_may_not_remove_from_leaves_everything_where_it_was() {
        let _settled = tilo_ui_base::motion::testing::settled();
        // The workspace the window is in belongs to someone else: the person writes the note
        // there as one of its editors, and may not take it out.
        let mut shell = Shell::new_elsewhere(Some(WorkspaceId::from_u128(1)));
        shell.grant(&shell.from.clone(), tilo_action_policy::Answer::Allow);
        let showing = shell.showing("first draft");
        shell.grant(&shell.from.clone(), tilo_action_policy::Answer::Deny);
        shell.grant(&shell.to_id.clone(), tilo_action_policy::Answer::Allow);
        shell.carry();
        refused(&mut shell, &showing, "window-move-reason-record");
    }

    /// The move `shell` made of the window showing `showing` was not made, and the person was told
    /// `why` (a message key); nothing of it was done in either workspace.
    #[track_caller]
    fn refused(shell: &mut Shell, showing: &DocumentId, why: &str) {
        let said = shell.told();
        assert!(said.contains(&t(why)), "the person is told why: {said:?}");
        assert_eq!(
            starts(&shell.started),
            1,
            "no app starts where the tab was going"
        );
        assert_eq!(
            workspace_of(&shell.session, shell.tab),
            Some(WorkspaceId::from_u128(1)),
            "the tab is where it was"
        );
        assert_eq!(
            tile_of(&shell.session, shell.tab),
            Some(TileContent::Foreign {
                surface_id: 11,
                app_id: PROBE.into()
            }),
            "showing the window it had"
        );
        assert_eq!(
            shell.note(&shell.from, showing).as_deref(),
            Some("first draft"),
            "the record is where it was, saying what it said"
        );
        assert!(
            shell.notes(&shell.to_id).is_empty(),
            "nothing of it is written where the tab was going"
        );
    }

    /// A move of a record someone removed while its window was open, which the workspace keeps
    /// no more and so cannot move: the tab stays where it was with the window it had, nothing is
    /// written where the tab was going, and the person is told.
    #[test]
    fn a_move_of_a_record_removed_meanwhile_leaves_the_tab_where_it_was() {
        let _settled = tilo_ui_base::motion::testing::settled();
        let mut shell = Shell::new();
        let showing = shell.showing("first draft");
        let removed = shell.command(
            &shell.from,
            Command::Remove {
                kind: NOTE.to_owned(),
                id: showing.as_str().to_owned(),
                stamp: tilo_resource_store::Stamp {
                    verb: "write".to_owned(),
                    invocation: uuid::Uuid::new_v4().to_string(),
                    ..tilo_resource_store::Stamp::default()
                },
            },
        );
        assert!(
            matches!(removed, Ok(Reply::Written)),
            "someone removes the note: {removed:?}"
        );
        shell.carry();
        let said = shell.told();
        assert!(
            said.contains(&t("window-move-reason-record")),
            "the person is told the record could not be moved: {said:?}"
        );
        assert_eq!(
            starts(&shell.started),
            1,
            "no app starts where the tab was going"
        );
        assert_eq!(
            workspace_of(&shell.session, shell.tab),
            Some(WorkspaceId::from_u128(1)),
            "the tab is where it was"
        );
        assert_eq!(
            tile_of(&shell.session, shell.tab),
            Some(TileContent::Foreign {
                surface_id: 11,
                app_id: PROBE.into()
            }),
            "showing the window it had"
        );
        assert!(
            shell.notes(&shell.to_id).is_empty(),
            "nothing of it is written where the tab was going"
        );
    }
}
