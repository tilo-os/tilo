//! Carrying a window the person moves to another workspace.
//! A workspace is the sandbox the window's app runs in: its grants, services,
//! authority and records. None of that follows a tab, so what crosses is the tab
//! and, with it, the record the window shows: the app starts again in the
//! destination under its grants, on a copy of that record kept in the destination's
//! profile ([`crate::window_transfer`]), and the instance left behind closes once
//! the new window is up. A move into or out of a workspace other people are in is
//! asked about first; one that cannot be made puts the tab back where it was and
//! says why. Nothing in a move touches the disk on the session's own thread.
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ChangeHash};
use collab_types::handoff::{HandoffReport, HandoffRequest, Successor};
use tilo_action_broker::collab_broker::SessionFollowDispatcher;
use tilo_app_launch::Launch;
use tilo_compositor::Compositor;
use tilo_desktop_model::{DesktopTree, TabId, TileContent, Workspace, WorkspaceSlot};
use ui::record::RecordId;
use ui::record::transfer::{Origin, Retirement, TransferError};

use crate::Shared;
use crate::apps::launch_of;
use crate::continuations::{Continuations, HandoffNote};
use crate::i18n::{t, t_args};
use crate::platform::notice::{Notice, Notices};
use crate::window_transfer::{self, Arrived, Fault, Merged, Profile};

/// The action a move's notice offers: carry the window into its destination.
const CARRY: &str = "window-move-carry";

/// The action a move's notice offers: leave the window where it is.
const STAY: &str = "window-move-stay";

/// How long a window's app may take to make what it shows durable before it is read.
const CHECKPOINT: Duration = Duration::from_secs(3);

/// How long the app started where the tab went may take to open a window there.
const OPENING: Duration = Duration::from_secs(30);

/// How long a window's app may take to say its record moved.
const RETIRING: Duration = Duration::from_secs(3);

/// How long the app the tab left may take to let go of its record.
const RELEASING: Duration = Duration::from_secs(10);

/// How long the destination's window may take to close so its last changes can be merged in.
const CLOSING: Duration = Duration::from_secs(10);

/// A move the person was asked about, waiting for their answer.
struct Asked {
    tab: TabId,
    /// The workspace it is in now, where it stays unless they say otherwise.
    from: WorkspaceSlot,
    /// What the workspace it leaves is called, for the notice that answers a failed move.
    from_name: String,
    to: WorkspaceSlot,
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
    id: RecordId,
    /// The profile it is kept in where the window showed it, and the one it is carried to.
    source: Profile,
    destination: Profile,
    /// What it became there, once it arrived.
    arrived: Option<Arrived>,
    /// The heads its app gave before it stamped the record retired: what the move brings along.
    retired_at: Option<Vec<ChangeHash>>,
}

/// A move under way.
struct Crossing {
    tab: TabId,
    from: WorkspaceSlot,
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
    phase: Phase,
}

/// What a move waits for.
enum Phase {
    /// The window's app was asked to make what it shows durable.
    Checkpointing { token: u64, since: Instant },
    /// The record is being read where it was, and kept where the tab went.
    Importing(Receiver<Result<Arrived, Fault>>),
    /// The tab waits for the window of the app started in the destination.
    Opening { since: Instant, reopening: bool },
    /// The window's app was asked to stamp its record retired, naming the record that arrived.
    Retiring { token: u64, since: Instant },
    /// The app the tab left is letting go of its record.
    Releasing {
        /// The heads its app gave with the retirement: the record as it was before the stamp, which
        /// a retired record cannot be read as it stands.
        until: Option<Vec<ChangeHash>>,
        since: Instant,
        poll: Option<Receiver<Result<Option<AutoCommit>, Fault>>>,
    },
    /// What the app the tab left wrote last is being merged into the record; `reopen` is whether
    /// the destination's window was closed for it.
    Catching {
        source: AutoCommit,
        answers: Receiver<Result<Merged, Fault>>,
        reopen: bool,
    },
    /// The destination's window is closing so the merge can be written.
    Closing { source: AutoCommit, since: Instant },
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
    /// What each window's app answered about a handoff step, taken up once a pass.
    notes: RefCell<Vec<HandoffNote>>,
    continuing: Arc<Continuations>,
    tokens: Cell<u64>,
}

impl Moves {
    /// Carries asked about through `notices` , whose answers come back here, and whose windows are
    /// reached through `continuing` .
    pub(crate) fn new(notices: Rc<dyn Notices>, continuing: Arc<Continuations>) -> Self {
        Self {
            asking: RefCell::new(BTreeMap::new()),
            crossing: RefCell::new(Vec::new()),
            notices,
            notes: RefCell::new(Vec::new()),
            continuing,
            tokens: Cell::new(0),
        }
    }

    /// Carry out the person's answers and carries, and take every move in flight a step further.
    pub(crate) fn follow(
        &self,
        shared: &Shared,
        compositor: &mut Compositor,
        handoff: Option<&SessionFollowDispatcher>,
    ) {
        self.notes
            .borrow_mut()
            .extend(self.continuing.take_handoffs());
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
        let moving = std::mem::take(&mut *self.crossing.borrow_mut());
        for one in moving {
            self.step(shared, compositor, handoff, one);
        }
        // An answer is taken up by the pass that asked for it.
        self.notes.borrow_mut().clear();
    }

    /// The person carried `tab` to the workspace at `to` : the app it shows starts again there on
    /// the record it shows, and a move that touches other people is asked about first.
    fn consider(
        &self,
        shared: &Shared,
        handoff: Option<&SessionFollowDispatcher>,
        tab: TabId,
        to: WorkspaceSlot,
    ) {
        let read = shared.desktop.with_policy(|policy| {
            policy.handle().read(|tree| {
                let here = standing(tree, tab, &shared.person, &shared.earlier)?;
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
            from: here.workspace.slot,
            from_name: here.workspace.name,
            to,
            to_name: there.name,
            to_id: there.id,
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
    fn carry(&self, shared: &Shared, handoff: Option<&SessionFollowDispatcher>, asked: Asked) {
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
        if !shared.desktop.with_policy(|policy| policy.adopt(&app, tab)) {
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
            self.failed(&app_name, &to_name, &from_name);
            return;
        };
        let from_id = from_id.id;
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
            phase: Phase::Finished,
        };
        let Some(record) = Self::record(shared, &crossing, &from_id) else {
            // Nothing of the window's own travels: its app opens what the destination keeps.
            self.start(shared, crossing, false);
            return;
        };
        let token = self.next_token();
        let asked_for = match handoff {
            // What the window's app holds in memory is asked to reach the disk before it is read.
            Some(handoff) => {
                handoff.request_handoff(
                    &crossing.from_id,
                    &crossing.app,
                    tab,
                    HandoffRequest::Checkpoint { token },
                );
                Phase::Checkpointing {
                    token,
                    since: Instant::now(),
                }
            }
            // No app of this session can be reached: the move reads what it last saved.
            None => Phase::Finished,
        };
        crossing.record = Some(record);
        if matches!(asked_for, Phase::Finished) {
            self.import(shared, crossing);
        } else {
            self.waiting(crossing, asked_for);
        }
    }

    /// The record the window at `tab` shows, when it is a record of its own that may leave the
    /// workspace: one the workspace keeps for the app goes with the workspace.
    fn travelling(shared: &Shared, tab: TabId) -> Option<RecordId> {
        let (id, _) = shared.notes.window_record(tab)?;
        RecordId::parse(&id).filter(|id| !id.is_package_primary())
    }

    /// [`Self::travelling`], naming the profiles it is kept in: where the window shows it, and the
    /// one of the workspace the tab went to.
    fn record(shared: &Shared, crossing: &Crossing, from_id: &str) -> Option<Travelling> {
        let id = Self::travelling(shared, crossing.tab)?;
        let home = launcher::appdir::host_data_home()?;
        Some(Travelling {
            id,
            source: Profile::of(&home, from_id, &crossing.app),
            destination: Profile::of(&home, &crossing.to_id, &crossing.app),
            arrived: None,
            retired_at: None,
        })
    }

    /// Take one step of one move, and put it back in flight when it waits for something.
    fn step(
        &self,
        shared: &Shared,
        compositor: &mut Compositor,
        handoff: Option<&SessionFollowDispatcher>,
        mut crossing: Crossing,
    ) {
        match std::mem::replace(&mut crossing.phase, Phase::Finished) {
            Phase::Checkpointing { token, since } => {
                self.checkpointing(shared, crossing, token, since);
            }
            Phase::Importing(answers) => self.importing(shared, crossing, answers),
            Phase::Opening { since, reopening } => {
                self.opening(shared, compositor, handoff, crossing, since, reopening);
            }
            Phase::Retiring { token, since } => {
                self.retiring(shared, compositor, crossing, token, since);
            }
            Phase::Releasing { until, since, poll } => {
                self.releasing(shared, crossing, until, since, poll);
            }
            Phase::Catching {
                source,
                answers,
                reopen,
            } => self.catching(shared, compositor, crossing, source, answers, reopen),
            Phase::Closing { source, since } => {
                self.closing(shared, crossing, source, since);
            }
            Phase::Finished => {}
        }
    }

    /// What the window's app made durable: either way what is on the disk is what is read.
    fn checkpointing(&self, shared: &Shared, crossing: Crossing, token: u64, since: Instant) {
        if self.claim(&crossing, token).is_some() || since.elapsed() >= CHECKPOINT {
            self.import(shared, crossing);
        } else {
            self.waiting(crossing, Phase::Checkpointing { token, since });
        }
    }

    /// What reading the record where it was, and keeping it where the tab went, came to.
    fn importing(
        &self,
        shared: &Shared,
        mut crossing: Crossing,
        answers: Receiver<Result<Arrived, Fault>>,
    ) {
        match answers.try_recv() {
            Err(TryRecvError::Empty) => self.waiting(crossing, Phase::Importing(answers)),
            Ok(Ok(arrived)) => {
                if let Some(record) = crossing.record.as_mut() {
                    record.arrived = Some(arrived);
                }
                self.start(shared, crossing, false);
            }
            Ok(Err(fault)) => self.give_up(shared, &crossing, &reason(&fault)),
            Err(TryRecvError::Disconnected) => {
                self.give_up(shared, &crossing, &t("window-move-reason-storage"));
            }
        }
    }

    /// Whether the window the app was started for came where the tab waits.
    fn opening(
        &self,
        shared: &Shared,
        compositor: &mut Compositor,
        handoff: Option<&SessionFollowDispatcher>,
        crossing: Crossing,
        since: Instant,
        reopening: bool,
    ) {
        match landed(shared, crossing.tab, crossing.window) {
            Landed::Came if reopening => Self::done(shared, &crossing),
            Landed::Came => self.retire(shared, compositor, handoff, crossing),
            Landed::Waiting if since.elapsed() < OPENING => {
                self.waiting(crossing, Phase::Opening { since, reopening });
            }
            Landed::Waiting => {
                self.give_up(shared, &crossing, &t("window-move-reason-no-window"));
            }
            // The tab went while its app started: nothing of the move is left to keep.
            Landed::Gone => Self::closed(&crossing),
        }
    }

    /// Whether the app the tab left said its record moved, and what it said.
    fn retiring(
        &self,
        shared: &Shared,
        compositor: &mut Compositor,
        mut crossing: Crossing,
        token: u64,
        since: Instant,
    ) {
        let Some(report) = self.claim(&crossing, token) else {
            if since.elapsed() < RETIRING {
                self.waiting(crossing, Phase::Retiring { token, since });
            } else {
                tracing::warn!(
                    app = %crossing.app,
                    "an app did not say its record moved: it is read as it stands"
                );
                self.release(shared, compositor, crossing);
            }
            return;
        };
        if let Some(record) = crossing.record.as_mut() {
            record.retired_at = report.heads().filter(|heads| !heads.is_empty());
        }
        if let HandoffReport::Failed { reason: why, .. } = &report {
            tracing::warn!(%why, app = %crossing.app, "an app could not say its record moved");
        }
        self.release(shared, compositor, crossing);
    }

    /// Whether the app the tab left let go of its record, and what it wrote last.
    fn releasing(
        &self,
        shared: &Shared,
        crossing: Crossing,
        until: Option<Vec<ChangeHash>>,
        since: Instant,
        poll: Option<Receiver<Result<Option<AutoCommit>, Fault>>>,
    ) {
        let Some(answers) = poll else {
            if since.elapsed() >= RELEASING {
                tracing::warn!(
                    app = %crossing.app,
                    "an app kept its record open: the window moved without its last changes"
                );
                Self::done(shared, &crossing);
                return;
            }
            match Self::poll_release(&crossing, until.clone()) {
                Some(poll) => {
                    self.waiting(
                        crossing,
                        Phase::Releasing {
                            until,
                            since,
                            poll: Some(poll),
                        },
                    );
                }
                None => self.give_up(shared, &crossing, &t("window-move-reason-storage")),
            }
            return;
        };
        match answers.try_recv() {
            Err(TryRecvError::Empty) => {
                self.waiting(
                    crossing,
                    Phase::Releasing {
                        until,
                        since,
                        poll: Some(answers),
                    },
                );
            }
            Ok(Ok(Some(source))) => match Self::merge(&crossing, &source) {
                Some(answers) => self.waiting(
                    crossing,
                    Phase::Catching {
                        source,
                        answers,
                        reopen: false,
                    },
                ),
                None => self.give_up(shared, &crossing, &t("window-move-reason-storage")),
            },
            // It has not let go yet: ask again.
            Ok(Ok(None)) => self.waiting(
                crossing,
                Phase::Releasing {
                    until,
                    since,
                    poll: None,
                },
            ),
            Ok(Err(fault)) => self.give_up(shared, &crossing, &reason(&fault)),
            Err(TryRecvError::Disconnected) => {
                self.give_up(shared, &crossing, &t("window-move-reason-storage"));
            }
        }
    }

    /// What merging the app's last changes into the record came to.
    fn catching(
        &self,
        shared: &Shared,
        compositor: &mut Compositor,
        crossing: Crossing,
        source: AutoCommit,
        answers: Receiver<Result<Merged, Fault>>,
        reopen: bool,
    ) {
        match answers.try_recv() {
            Err(TryRecvError::Empty) => {
                self.waiting(
                    crossing,
                    Phase::Catching {
                        source,
                        answers,
                        reopen,
                    },
                );
            }
            Ok(Ok(Merged::Held)) if reopen => {
                // The window its app opened again took the record before the last changes could be
                // written.
                self.give_up(shared, &crossing, &t("window-move-reason-busy"));
            }
            Ok(Ok(Merged::Held)) => {
                // The window that came holds the record: it closes, the merge is written where
                // nothing holds it, and the app opens it again on what arrived.
                self.close(shared, compositor, crossing, source);
            }
            Ok(Ok(Merged::Written(changes))) => {
                tracing::info!(
                    changes,
                    app = %crossing.app,
                    "what the moved window's app wrote last arrived"
                );
                if reopen {
                    self.start(shared, crossing, true);
                } else {
                    Self::done(shared, &crossing);
                }
            }
            Ok(Ok(Merged::Nothing)) => {
                if reopen {
                    self.start(shared, crossing, true);
                } else {
                    Self::done(shared, &crossing);
                }
            }
            Ok(Err(fault)) => self.give_up(shared, &crossing, &reason(&fault)),
            Err(TryRecvError::Disconnected) => {
                self.give_up(shared, &crossing, &t("window-move-reason-storage"));
            }
        }
    }

    /// Whether the destination's window closed so the merge can be written.
    fn closing(&self, shared: &Shared, crossing: Crossing, source: AutoCommit, since: Instant) {
        if window_of(shared, crossing.tab).is_none() {
            match Self::merge(&crossing, &source) {
                Some(answers) => self.waiting(
                    crossing,
                    Phase::Catching {
                        source,
                        answers,
                        reopen: true,
                    },
                ),
                None => self.give_up(shared, &crossing, &t("window-move-reason-storage")),
            }
            return;
        }
        if since.elapsed() >= CLOSING {
            self.give_up(shared, &crossing, &t("window-move-reason-busy"));
            return;
        }
        self.waiting(crossing, Phase::Closing { source, since });
    }

    /// Read the record where the window shows it, and keep it in the workspace the tab went to.
    fn import(&self, shared: &Shared, crossing: Crossing) {
        let Some(record) = crossing.record.as_ref() else {
            self.start(shared, crossing, false);
            return;
        };
        let (source, destination, id, instance) = (
            record.source.clone(),
            record.destination.clone(),
            record.id.clone(),
            crossing.tab,
        );
        let Some(answers) = window_transfer::off_loop(move || {
            window_transfer::take(&source, &destination, &id, instance)
        }) else {
            self.give_up(shared, &crossing, &t("window-move-reason-storage"));
            return;
        };
        self.waiting(crossing, Phase::Importing(answers));
    }

    /// Start the window's app in the workspace the person carried the tab to, on the record that
    /// came along when one did.
    fn start(&self, shared: &Shared, crossing: Crossing, reopening: bool) {
        let on = crossing
            .record
            .as_ref()
            .and_then(|record| record.arrived.as_ref())
            .map(|arrived| Launch::Record {
                app: crossing.app.clone(),
                record: arrived.target.id.as_str().to_owned(),
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
        self.waiting(crossing, Phase::Opening { since, reopening });
    }

    /// The new window is up: the window the tab left is asked to stamp its record retired, and then
    /// closes.
    fn retire(
        &self,
        shared: &Shared,
        compositor: &mut Compositor,
        handoff: Option<&SessionFollowDispatcher>,
        crossing: Crossing,
    ) {
        let Some(target) = crossing
            .record
            .as_ref()
            .and_then(|record| record.arrived.as_ref())
            .map(|arrived| arrived.target.id.as_str().to_owned())
        else {
            // Nothing travelled: the window left shows what its workspace keeps for it.
            compositor.close_window(crossing.window);
            Self::done(shared, &crossing);
            return;
        };
        let Some(handoff) = handoff else {
            self.release(shared, compositor, crossing);
            return;
        };
        let token = self.next_token();
        handoff.request_handoff(
            &crossing.from_id,
            &crossing.app,
            crossing.tab,
            HandoffRequest::Retire {
                token,
                successor: Successor {
                    room: crossing.to_id.clone(),
                    record: target,
                    by: shared.person.clone(),
                },
            },
        );
        let since = Instant::now();
        self.waiting(crossing, Phase::Retiring { token, since });
    }

    /// The window the tab left is closed: read what its app wrote last, for a merge where the
    /// destination can take it.
    fn release(&self, shared: &Shared, compositor: &mut Compositor, crossing: Crossing) {
        compositor.close_window(crossing.window);
        let until = crossing
            .record
            .as_ref()
            .and_then(|record| record.retired_at.clone());
        match Self::poll_release(&crossing, until.clone()) {
            Some(poll) => {
                let since = Instant::now();
                self.waiting(
                    crossing,
                    Phase::Releasing {
                        until,
                        since,
                        poll: Some(poll),
                    },
                );
            }
            None => self.give_up(shared, &crossing, &t("window-move-reason-storage")),
        }
    }

    /// Ask what the app the tab left holds of its record now, and whether it let go.
    fn poll_release(
        crossing: &Crossing,
        until: Option<Vec<ChangeHash>>,
    ) -> Option<Receiver<Result<Option<AutoCommit>, Fault>>> {
        let record = crossing.record.as_ref()?;
        let arrived = record.arrived.as_ref()?;
        let (source, origin) = (record.source.clone(), arrived.origin.clone());
        window_transfer::off_loop(move || {
            window_transfer::released(&source, &origin, until.as_deref())
        })
    }

    /// Merge what the app the tab left wrote last into the record where it arrived.
    fn merge(crossing: &Crossing, source: &AutoCommit) -> Option<Receiver<Result<Merged, Fault>>> {
        let record = crossing.record.as_ref()?;
        let destination = record.destination.clone();
        let target = record.arrived.as_ref()?.target.clone();
        let source = source.clone();
        window_transfer::off_loop(move || window_transfer::merge(&destination, &target, &source))
    }

    /// The destination's window holds the record the last changes belong in: put a card in its tab,
    /// close it, and merge once it has let go.
    fn close(
        &self,
        shared: &Shared,
        compositor: &mut Compositor,
        crossing: Crossing,
        source: AutoCommit,
    ) {
        let window = window_of(shared, crossing.tab);
        if !shared.desktop.with_policy(|policy| {
            policy.note_in_tab(crossing.tab, &t("window-move-bringing-edits"))
        }) {
            Self::closed(&crossing);
            return;
        }
        if let Some(window) = window {
            compositor.close_window(window);
        }
        let since = Instant::now();
        self.waiting(crossing, Phase::Closing { source, since });
    }

    /// The move is done: the record it left says where it went, wherever its app did not.
    fn done(shared: &Shared, crossing: &Crossing) {
        let Some(record) = crossing.record.as_ref() else {
            return;
        };
        let Some(arrived) = record.arrived.as_ref() else {
            return;
        };
        let source = record.source.clone();
        let origin = arrived.origin.clone();
        let successor = Successor {
            room: crossing.to_id.clone(),
            record: arrived.target.id.as_str().to_owned(),
            by: shared.person.clone(),
        };
        if let Err(error) = window_transfer::off_loop_detached(move || {
            if let Err(fault) =
                window_transfer::retire(&source, &origin, &Retirement::Moved(Some(successor)))
            {
                tracing::warn!(?fault, "a moved record could not say where it went");
            }
        }) {
            tracing::warn!(%error, "a moved record could not say where it went");
        }
    }

    /// The tab went while its record was on its way: the copy it made where it arrived is retired
    /// as one nothing opened.
    fn closed(crossing: &Crossing) {
        let Some(record) = crossing.record.as_ref() else {
            return;
        };
        let Some(arrived) = record.arrived.as_ref() else {
            return;
        };
        let destination = record.destination.clone();
        // The copy is a record of the destination with its own id there; the one it was read from
        // exists only where the window was.
        let copy = Origin::Record(arrived.target.clone());
        if let Err(error) = window_transfer::off_loop_detached(move || {
            if let Err(fault) = window_transfer::retire(&destination, &copy, &Retirement::Abandoned)
            {
                tracing::warn!(
                    ?fault,
                    "a record copied into a workspace was left unretired"
                );
            }
        }) {
            tracing::warn!(
                %error,
                "a record copied into a workspace was left unretired"
            );
        }
    }

    /// The move cannot be made: the copy is retired as one nothing opened, the tab goes back where
    /// it was, and the person is told why.
    fn give_up(&self, shared: &Shared, crossing: &Crossing, why: &str) {
        Self::closed(crossing);
        Self::put_back(shared, crossing.tab, crossing.from, &crossing.app);
        if crossing.record.is_some() {
            self.record_failed(
                &crossing.app_name,
                &crossing.to_name,
                &crossing.from_name,
                why,
            );
        } else {
            self.failed(&crossing.app_name, &crossing.to_name, &crossing.from_name);
        }
    }

    /// Nothing crossed: the tab goes back where it was, still showing the window it had.
    fn put_back(shared: &Shared, tab: TabId, from: WorkspaceSlot, app: &str) {
        shared
            .desktop
            .with_policy(|policy| policy.give_up_held(app));
        let _ = shared.desktop.with_policy(|policy| policy.carry(tab, from));
    }

    /// Say the window stayed where it was, and where.
    fn failed(&self, app: &str, workspace: &str, source: &str) {
        let title = t_args(
            "window-move-failed-title",
            &[
                ("app", app.to_owned().into()),
                ("workspace", workspace.to_owned().into()),
            ],
        );
        let body = t_args(
            "window-move-failed-body",
            &[("workspace", source.to_owned().into())],
        );
        self.notices
            .show(&Notice::new(t("window-move-app-name"), 1, title, body));
    }

    /// Say the record the window shows could not be moved, where it stayed and why.
    fn record_failed(&self, app: &str, workspace: &str, source: &str, why: &str) {
        let title = t_args(
            "window-move-record-failed-title",
            &[
                ("app", app.to_owned().into()),
                ("workspace", workspace.to_owned().into()),
            ],
        );
        let body = t_args(
            "window-move-record-failed-body",
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

    /// What the window's app said about the step asked under `token` , if it said anything.
    fn claim(&self, crossing: &Crossing, token: u64) -> Option<HandoffReport> {
        let (from, app, instance) = (
            crossing.from_id.as_str(),
            crossing.app.as_str(),
            crossing.tab,
        );
        let mut notes = self.notes.borrow_mut();
        let at = notes.iter().position(|note| {
            note.workspace == from
                && note.app == app
                && note.instance == instance
                && matches!(
                    &note.report,
                    HandoffReport::Done { token: said, .. }
                        | HandoffReport::Failed { token: said, .. } if *said == token
                )
        })?;
        Some(notes.remove(at).report)
    }

    fn next_token(&self) -> u64 {
        let token = self.tokens.get().wrapping_add(1);
        self.tokens.set(token);
        token
    }
}

impl crate::Session {
    /// Carry out the person's answers and carries since the last pass: asked first
    /// where other people are involved, then the record goes along, the app runs there,
    /// and the window it left closes once the app has one there.
    pub(crate) fn follow_window_moves(&mut self) {
        let handoff = self.action_plane.as_ref().map(crate::plane::Plane::handoff);
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
}

/// The window the tab at `tab` holds.
fn window_of(shared: &Shared, tab: TabId) -> Option<u64> {
    shared.desktop.with_policy(|policy| {
        policy.handle().read(|tree| match tab_tile(tree, tab) {
            Some(TileContent::Foreign { surface_id, .. }) => Some(*surface_id),
            _ => None,
        })
    })
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

/// Why a step of a move failed, as the person is told it.
fn reason(fault: &Fault) -> String {
    match fault {
        Fault::Transfer(TransferError::Busy) => t("window-move-reason-busy"),
        Fault::Transfer(error) => {
            tracing::warn!(%error, "a record could not be moved");
            t("window-move-reason-record")
        }
        Fault::Profile(error) => {
            tracing::warn!(%error, "a window's app profile could not be read");
            t("window-move-reason-storage")
        }
        Fault::Unwritten => t("window-move-reason-storage"),
    }
}

/// A workspace as a move reads it.
struct WorkspaceRead {
    slot: WorkspaceSlot,
    id: String,
    name: String,
    /// Whether anyone besides this person belongs to it: what it holds is shared with them whether
    /// or not they are here.
    others: bool,
}

/// What stands at `tab` : the window filling it, the app that window belongs to, and the workspace
/// it is in.
struct Standing {
    app: String,
    window: u64,
    workspace: WorkspaceRead,
}

/// The tile `tab` holds, as the desk has it.
fn tab_tile(tree: &DesktopTree, tab: TabId) -> Option<&TileContent> {
    tree.workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .find(|held| held.id == tab)
        .map(|held| &held.tile)
}

/// The window `tab` shows and the workspace the tab stands in; `None` when the tab is not on the
/// desk or holds no window of an app this device started.
fn standing(tree: &DesktopTree, tab: TabId, person: &str, earlier: &[String]) -> Option<Standing> {
    let TileContent::Foreign { app_id, surface_id } = tab_tile(tree, tab)? else {
        return None;
    };
    let workspace = tree.workspaces.iter().find(|workspace| {
        workspace.strips.iter().any(|strip| {
            strip
                .window_groups
                .iter()
                .any(|group| group.tabs.iter().any(|held| held.id == tab))
        })
    })?;
    Some(Standing {
        app: app_id.clone(),
        window: *surface_id,
        workspace: read(workspace, person, earlier),
    })
}

fn workspace_at(
    tree: &DesktopTree,
    slot: WorkspaceSlot,
    person: &str,
    earlier: &[String],
) -> Option<WorkspaceRead> {
    tree.workspaces
        .iter()
        .find(|workspace| workspace.slot == slot)
        .map(|workspace| read(workspace, person, earlier))
}

fn read(workspace: &Workspace, person: &str, earlier: &[String]) -> WorkspaceRead {
    let mine = |id: &str| id == person || earlier.iter().any(|old| old == id);
    WorkspaceRead {
        slot: workspace.slot,
        id: workspace.id().to_string(),
        name: workspace.name.clone(),
        others: workspace
            .workspace_readers_at(tilo_clock::unix_now())
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

    use automerge::ReadDoc as _;
    use automerge::transaction::Transactable as _;
    use durable_store::{DurableStore as _, StorageKey};
    use tilo_action_policy::Role;
    use tilo_broker_core::route::CrdtRoute;
    use tilo_compositor::OutputConfig;
    use tilo_desktop_model::{Strip, StripId};
    use tilo_desktop_policy::Window;
    use tilo_desktop_surface::surface::window_menu::MOVE_TO_WORKSPACE;
    use ui::record::transfer::retirement;
    use ui::record::{RecordCheckout, RecordKind, RecordOwners};

    use crate::apps::Apps;
    use crate::{AppActions, Options, Session};

    use super::*;

    /// The app a test installs: a Tilo package whose window fills a tab.
    const PROBE: &str = "so.tilo.probe";

    /// The kind of record the app keeps a window of: a note of its own.
    const NOTE: &str = "probe.note";

    /// The display every headless session here draws on.
    const WIDTH: i32 = 1280;
    /// See [`WIDTH`].
    const HEIGHT: i32 = 800;

    /// A workspace owned by `owner` , with `members` in it.
    fn workspace(owner: &str, members: &[&str]) -> Workspace {
        let mut workspace = Workspace::create("Studio");
        workspace.set_owner(owner.to_owned());
        for member in members {
            workspace.admit((*member).to_owned(), Role::Editor);
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

    /// A desk with a session on it, and the window of [`PROBE`] in a tab of the workspace in front,
    /// showing `record` .
    struct Desk {
        _home: tilo_test_env::TestHome,
        /// Where each start is written down.
        started: PathBuf,
        /// What each start was handed.
        arguments: PathBuf,
        session: Session,
        /// The workspace the window is in, and the one behind it.
        from: String,
        to: WorkspaceSlot,
        to_id: String,
        /// The tab the window fills.
        tab: TabId,
    }

    impl Desk {
        /// A desk of two workspaces whose window shows a record of its own.
        fn new() -> Self {
            let mut home = tilo_test_env::TestHome::new().expect("a home");
            let started = home.home().join("started");
            let arguments = home.home().join("arguments");
            install(&mut home);
            let launcher = tilo_test_env::stand_in(
                home.home().join("tilo-launcher"),
                &format!(
                    "printf '%s\\n' \"$$\" >> '{started}'\n\
                     printf '%s\\n' \"$*\" >> '{arguments}'\n\
                     hold\n",
                    started = started.display(),
                    arguments = arguments.display(),
                ),
            )
            .expect("a launcher");
            let state = home.state().to_path_buf();
            save_desk(&state);
            let mut environment = tilo_app_launch::SessionEnvironment::from_process();
            environment.wayland_socket = Some(home.runtime().join("wayland-7"));
            let apps = Rc::new(Apps::with_launcher(
                environment,
                None,
                Some(launcher.path().to_owned()),
            ));
            let mut session = Session::headless(
                Options {
                    presentation: ui::headless::fixture_runner_inputs,
                    collab: false,
                    apps: Some(Rc::clone(&apps) as Rc<dyn AppActions>),
                    launch: Some(Rc::clone(&apps)),
                    state: Some(state),
                    ..Options::default()
                },
                &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
            )
            .expect("a session");
            let (from, to, to_id) = session
                .handle()
                .read(|tree| {
                    let front = tree.active_workspace()?.id().to_string();
                    let away = tree
                        .workspaces
                        .iter()
                        .find(|held| held.slot != WorkspaceSlot(1))?;
                    Some((front, away.slot, away.id().to_string()))
                })
                .expect("a workspace in front and one behind it");
            // The app's window, as the compositor would map it.
            apps.start(
                &Launch::Package {
                    app: PROBE.into(),
                    open: None,
                },
                &apps.workspace_named(&from),
            )
            .expect("the desk starts the app");
            wait(&mut session, "the app is started", |_| {
                (starts(&started) == 1).then_some(())
            });
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
            Self {
                _home: home,
                started,
                arguments,
                session,
                from,
                to,
                to_id,
                tab,
            }
        }

        /// Where this app's records of `workspace` are kept on this device.
        fn profile(workspace: &str) -> PathBuf {
            let data_home = launcher::appdir::host_data_home().expect("a data home");
            let slug = launcher::appdir::workspace_slug(Some(workspace));
            launcher::appdir::host_app_dir(&data_home, &slug, PROBE).join(".local/share")
        }

        /// The record the window shows: one the app wrote in the workspace it is in.
        fn showing(&self, body: &str) -> RecordCheckout {
            let root = Self::profile(&self.from);
            let checkout = RecordCheckout::new(RecordId::minted(), RecordKind::new(NOTE));
            let mut owners = RecordOwners::new(root);
            let records = owners
                .open(PROBE, &self.from)
                .expect("the app's records of its workspace");
            records
                .create_record(&checkout)
                .expect("the record the window shows");
            records
                .with(&checkout.id, |doc| {
                    doc.put(automerge::ROOT, "body", body)
                        .expect("the note's body");
                })
                .expect("the record is open");
            records.try_flush().expect("the record is written");
            // The session notes what a window shows as its app holds it: this one holds the record
            // it was launched on.
            self.session.shared.notes.read(&[(
                self.tab,
                CrdtRoute::record(PROBE, self.from.clone(), checkout.id.as_str()),
            )]);
            checkout
        }

        /// The person moves the window to the workspace behind.
        fn carry(&mut self) {
            self.session
                .handle()
                .choose_window_menu(
                    self.tab,
                    &format!("{MOVE_TO_WORKSPACE}:{}", self.to.0),
                    None,
                )
                .expect("the person moves the window");
            self.session
                .step(Duration::from_millis(1))
                .expect("a frame");
        }

        /// Wait for the app the move starts where the window went, and what it was handed: the
        /// workspace, and the record it opens.
        fn arrived(&mut self) -> (String, RecordId) {
            wait(
                &mut self.session,
                "the app runs where the window went",
                |_| (starts(&self.started) == 2).then_some(()),
            );
            assert_eq!(
                handed(&self.arguments, "--workspace-id"),
                self.to_id,
                "the app runs under the grants of the workspace the window went to"
            );
            let opened = handed(&self.arguments, "--open-uri");
            let id = RecordId::from_uri(&opened)
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

        /// What the record `id` holds in `workspace` on this device.
        fn stored(workspace: &str, id: &RecordId) -> Option<automerge::AutoCommit> {
            stored_at(&Self::profile(workspace), workspace, id)
        }

        /// The app of the tab writes once more where the window was.
        fn edit(&self, id: &RecordId, body: &str) {
            edit_at(&Self::profile(&self.from), &self.from, id, body);
        }
    }

    /// The note a record holds.
    fn body(doc: &automerge::AutoCommit) -> Option<String> {
        match doc.get(automerge::ROOT, "body") {
            Ok(Some((automerge::Value::Scalar(value), _))) => value.as_str().map(str::to_owned),
            _ => None,
        }
    }

    /// What the record `id` of `workspace` holds in the profile at `root` .
    fn stored_at(root: &Path, workspace: &str, id: &RecordId) -> Option<automerge::AutoCommit> {
        let store = app_state::FsStore::new(root.to_path_buf());
        let key = StorageKey::record(PROBE, workspace, id.storage_key());
        let stored = store.read(&key).ok().flatten()?;
        let (doc, _) =
            app_state::decode_canonical(&stored.bytes, false, Path::new("a record")).ok()?;
        Some(doc)
    }

    /// The app writes `body` into the record `id` of `workspace` : what it does while a window
    /// moves.
    fn edit_at(root: &Path, workspace: &str, id: &RecordId, body: &str) {
        let mut owners = RecordOwners::new(root.to_path_buf());
        let records = owners
            .open(PROBE, workspace)
            .expect("the app's records of its workspace");
        records.open_record(&RecordCheckout::new(id.clone(), RecordKind::new(NOTE)));
        records
            .with(id, |doc| {
                doc.put(automerge::ROOT, "body", body)
                    .expect("the note's body");
            })
            .expect("the record is open");
        records.try_flush().expect("the record is written");
    }

    /// The desk this device left: the workspace in front with a strip the app's window takes, and
    /// one behind it.
    fn save_desk(state: &Path) {
        let mut home = Workspace::new(WorkspaceSlot(1), "Home");
        home.strips = vec![Strip::new(StripId(1), "Main")];
        let mut tree = DesktopTree {
            active: home.slot,
            workspaces: vec![home],
            ..DesktopTree::default()
        };
        tree.active_strip.insert(WorkspaceSlot(1), StripId(1));
        tree.set_active_group(StripId(1), 10);
        let mut away = Workspace::new(WorkspaceSlot(2), "Away");
        away.strips = vec![Strip::new(StripId(3), "Later")];
        tree.workspaces.push(away);
        let path = state.join("tilo").join("desk.json");
        fs::create_dir_all(path.parent().expect("a tilo directory")).expect("the state directory");
        fs::write(&path, serde_json::to_string(&tree).expect("a desk")).expect("the desk is saved");
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

    /// What the last start the launcher recorded was handed for `flag` .
    fn handed(arguments: &Path, flag: &str) -> String {
        let recorded = fs::read_to_string(arguments).expect("the launcher recorded its start");
        let arguments: Vec<&str> = recorded
            .lines()
            .next_back()
            .expect("a line per start")
            .split_whitespace()
            .collect();
        let at = arguments
            .iter()
            .position(|argument| *argument == flag)
            .unwrap_or_else(|| panic!("the launch carries {flag}: {arguments:?}"));
        arguments
            .get(at.saturating_add(1))
            .unwrap_or_else(|| panic!("{flag} names a value: {arguments:?}"))
            .to_string()
    }

    /// Every tab of the desk.
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
    fn workspace_of(session: &Session, id: TabId) -> Option<WorkspaceSlot> {
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
                .map(|workspace| workspace.slot)
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

    /// A window the person moves to another workspace takes the record it shows along: it is kept
    /// where it went, under an id of its own there, what the window's app wrote before it closed is
    /// merged in, and the record it left says where it went.
    #[test]
    fn a_moved_window_takes_the_record_it_shows_to_the_workspace_it_moves_to() {
        let _settled = ui::motion::testing::settled();
        let mut desk = Desk::new();
        let showing = desk.showing("first draft");
        desk.carry();
        let (_, arrived) = desk.arrived();
        assert_ne!(
            arrived.as_str(),
            showing.id.as_str(),
            "the record arrives under an id of the workspace it went to"
        );
        // The app it left writes once more before its window closes.
        desk.edit(&showing.id, "first draft, last words");
        desk.land();
        let successor = Successor {
            room: desk.to_id.clone(),
            record: arrived.as_str().to_owned(),
            by: desk.session.shared.person.clone(),
        };
        let source = Desk::profile(&desk.from);
        let from = desk.from.clone();
        let left = wait(
            &mut desk.session,
            "the record it left says where it went",
            |_| stored_at(&source, &from, &showing.id).and_then(|doc| retirement(&doc)),
        );
        assert_eq!(
            left,
            Retirement::Moved(Some(successor)),
            "the record it left names where it went and who moved it"
        );
        let kept = Desk::stored(&desk.to_id, &arrived).expect("the record is kept where it went");
        assert_eq!(
            body(&kept).as_deref(),
            Some("first draft, last words"),
            "what the app wrote before it closed came along"
        );
        assert_eq!(
            ui::record::transfer::provenance(&kept),
            vec![showing.id.clone()],
            "and the record it arrived from is its provenance"
        );
        assert_eq!(
            workspace_of(&desk.session, desk.tab),
            Some(desk.to),
            "the tab is where the person put it"
        );
    }

    /// An app that opens no window where the person put its tab: the tab goes back where it was,
    /// still showing the window it had, the copy the move made where it arrived is retired as one
    /// nothing opened, the record it left lives on, and the person is told why.
    #[test]
    fn a_move_whose_app_opens_no_window_puts_the_tab_back() {
        let _settled = ui::motion::testing::settled();
        let mut desk = Desk::new();
        let showing = desk.showing("first draft");
        desk.carry();
        let (_, arrived) = desk.arrived();
        // The app starts and runs, but never opens a window there.
        let said = wait_maybe(
            &mut desk.session,
            OPENING.saturating_add(tilo_test_env::hang_bound()),
            |session| {
                session
                    .notifications()
                    .feed()
                    .banners
                    .into_iter()
                    .find(|banner| banner.notification.title.contains("Probe"))
            },
        )
        .expect("the person is told the record could not go");
        assert!(
            !said.notification.body.trim().is_empty(),
            "and why: {:?}",
            said.notification.body
        );
        assert_eq!(
            workspace_of(&desk.session, desk.tab),
            Some(WorkspaceSlot(1)),
            "the tab went back where it was"
        );
        assert_eq!(
            tile_of(&desk.session, desk.tab),
            Some(TileContent::Foreign {
                surface_id: 11,
                app_id: PROBE.into()
            }),
            "still showing the window it had"
        );
        let to = Desk::profile(&desk.to_id);
        let to_id = desk.to_id.clone();
        let retired = wait(&mut desk.session, "the copy it made is retired", |_| {
            stored_at(&to, &to_id, &arrived).and_then(|doc| retirement(&doc))
        });
        assert_eq!(
            retired,
            Retirement::Abandoned,
            "the copy is retired as one nothing opened"
        );
        let left = Desk::stored(&desk.from, &showing.id).expect("the record it showed lives on");
        assert_eq!(
            retirement(&left),
            None,
            "the record it left still belongs where it is"
        );
        assert_eq!(body(&left).as_deref(), Some("first draft"));
    }

    /// [`wait`] with a bound of its own: a step of the product is timed out before the move
    /// answers.
    #[track_caller]
    fn wait_maybe<T>(
        session: &mut Session,
        within: Duration,
        mut asked: impl FnMut(&Session) -> Option<T>,
    ) -> Option<T> {
        let deadline = std::time::Instant::now()
            .checked_add(within)
            .expect("a test deadline");
        loop {
            if let Some(answer) = asked(session) {
                return Some(answer);
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            session.step(Duration::from_millis(1)).expect("a frame");
        }
    }
}
