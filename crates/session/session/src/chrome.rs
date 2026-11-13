//! What the desktop's layers ask of the native session: the bar's source (the clock, the person,
//! their notifications) and the rules of what the person may change.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use crate::platform::surfaces::{Placement as SurfacePlacement, WidgetHost};
use tilo_action_policy::Role;
use tilo_desktop::about::{AboutLinks, AboutVariant, TiloMenu};
use tilo_desktop::chrome::{
    AppTrayIcon, AppTrayItem, AppTrayStatus, ChromeIntent, DesktopChromeSnapshot, OwnChip,
};
use tilo_desktop::desktop_host::{DesktopAsk, DesktopChrome};
use tilo_desktop::launchpad::LaunchpadAction;
use tilo_desktop::overlays::{Overlays, Slot, Target};
use tilo_desktop::topbar::{Placement, TopBarChrome};
use tilo_desktop_model::DesktopTree;
use tilo_desktop_model::commands::Standing;
use tilo_desktop_model::ownership::Authors;
use tilo_desktop_surface::DesktopHandle;
use tilo_desktop_surface::host::chrome::{ChromeSource, Facts, HostChrome, Notices};
use tilo_desktop_surface::navigation::ChromeEffect;
use ui::floating_layer::Card;
use ui::live_activity::{Published, Summary};
use ui::search::Scope;
use ui::vello::kurbo::Rect;

use crate::notifications::NoticeSignals;
use crate::platform::notice::{Notice, Notices as Told};

/// The app whose live activity is the clocks app's when the desk reads the running timer.
pub(crate) const CLOCKS_APP: &str = "so.tilo.clocks";

/// What the board published, parsed, at the revision it was read at.
type Parsed = (u64, Rc<Vec<(String, Published)>>);

/// The apps' live activities, as the session presents them: every summary
/// the apps publish on the board, and the person's own say over them.
///
/// Apps describe, the session places (the floating layer's layout does the
/// placing): this keeps what only the person decides — what they put away,
/// what they folded into the bar, what they pulled out of it, where they
/// left a card loose, and which card they touched last.
pub(crate) struct Activities {
    board: Arc<runtime_collab::LiveActivities>,
    /// The cards the apps draw themselves, and where the layer gives them.
    cards: crate::cards::Cards,
    /// What the board last published, parsed, with the revision it was read at: the bar and the
    /// layer read it several times a frame, and the activities are JSON.
    shown: RefCell<Option<Parsed>>,
    /// Per activity, the identity (its id and title) the person put away: it stays away until the
    /// app shows something with another identity.
    hidden: RefCell<std::collections::BTreeMap<String, (String, String)>>,
    /// Activities folded back into the bar.
    collapsed: RefCell<std::collections::BTreeSet<String>>,
    /// Activities the person pulled out of the bar's pill.
    pulled: RefCell<std::collections::BTreeSet<String>>,
    /// Every activity the person touched, most recent first.
    touched: RefCell<Vec<String>>,
    /// Where each card was left loose (this device only).
    loose: RefCell<std::collections::BTreeMap<String, Rect>>,
    /// The announcements posted, by `app:id` .
    announced: RefCell<std::collections::BTreeSet<String>>,
}

impl Activities {
    pub(crate) fn new(
        board: Arc<runtime_collab::LiveActivities>,
        cards: crate::cards::Cards,
    ) -> Self {
        Self {
            board,
            cards,
            shown: RefCell::default(),
            hidden: RefCell::default(),
            collapsed: RefCell::default(),
            pulled: RefCell::default(),
            touched: RefCell::default(),
            loose: RefCell::default(),
            announced: RefCell::default(),
        }
    }

    /// The board the apps show their live activities on.
    pub(crate) fn board(&self) -> &Arc<runtime_collab::LiveActivities> {
        &self.board
    }

    /// What one app publishes now.
    pub(crate) fn published(&self, app: &str) -> Option<Published> {
        let activity = self.board.activity(app)?;
        serde_json::from_value(activity)
            .inspect_err(|error| tracing::warn!(%error, app, "a live activity does not read back"))
            .ok()
    }

    /// Every activity shown now, as the bar's pill names them: the app and its summary, in app id
    /// order.
    pub(crate) fn summaries(&self) -> Vec<(String, Summary)> {
        self.read()
            .iter()
            .filter_map(|(app, published)| {
                let summary = published.summary.as_ref()?;
                self.shows(app, summary)
                    .then(|| (app.clone(), summary.clone()))
            })
            .collect()
    }

    /// The bar's pill: the activity that stands first and how many wait behind it.
    pub(crate) fn pill(&self) -> ui::floating_layer::Pill {
        ui::floating_layer::pill(&self.summaries())
    }

    /// The cards the floating layer draws: what asks to float and what the person pulled out of the
    /// bar, at most [`ui::floating_layer::MAX_CARDS`] — the least recently touched fold into the
    /// pill.
    pub(crate) fn cards(&self) -> Vec<Card> {
        let mut cards: Vec<(usize, Card)> = self
            .read()
            .iter()
            .filter_map(|(app, published)| {
                let summary = published.summary.clone()?;
                if !self.shows(app, &summary) {
                    return None;
                }
                let key = key(app, &summary);
                let pulled = self.pulled.borrow().contains(&key);
                let collapsed = self.collapsed.borrow().contains(&key);
                if collapsed || !(published.overlay || pulled) {
                    return None;
                }
                let mut card = Card::new(app.clone(), summary);
                // A call or a route is drawn by its app: the layer keeps its place, and its body
                // comes from the app's own window while that app has no window of its own on the
                // desk.
                if let Some(size) = self.cards.size(&card.app, &card.summary.id) {
                    if self.cards.in_view(&card.app, &card.summary.id) {
                        return None;
                    }
                    card.size = Some(size);
                    card.rich = self.cards.live(&card.app, &card.summary.id);
                }
                Some((self.recently_touched(&key).unwrap_or(usize::MAX), card))
            })
            .collect();
        cards.sort_by_key(|(order, _)| *order);
        cards.truncate(ui::floating_layer::MAX_CARDS);
        cards
            .into_iter()
            .map(|(_, mut card)| {
                card.loose = self
                    .loose
                    .borrow()
                    .get(&key(&card.app, &card.summary))
                    .copied();
                card
            })
            .collect()
    }

    /// Whether a phone-width card is pulled out of the bar.
    pub(crate) fn expanded(&self) -> bool {
        !self.pulled.borrow().is_empty()
    }

    /// A control the person pressed on an activity reaches its app.
    pub(crate) fn press(&self, app: &str, activity: &str, command: &str) {
        self.touch(app, activity);
        self.board.press(
            app,
            serde_json::json!({
                "command": command,
                "activity_id": (!activity.is_empty()).then_some(activity),
            }),
        );
    }

    /// The person dragged an activity's progress bar to `fraction` of it,
    /// from 0 to 1: its app goes there, through the same press a control on
    /// the card makes.
    pub(crate) fn seek(&self, app: &str, activity: &str, fraction: f64) {
        self.touch(app, activity);
        self.board.press(
            app,
            serde_json::json!({
                "command": "seek",
                "activity_id": (!activity.is_empty()).then_some(activity),
                "fraction": fraction,
            }),
        );
    }

    /// The person ended or put away an activity: an app that offers a close has it asked to end it,
    /// and one that cannot be stopped from outside goes away until it shows something else.
    pub(crate) fn close(&self, app: &str, activity: &str) {
        let summary = self
            .read()
            .iter()
            .find(|(shown, _)| shown == app)
            .and_then(|(_, published)| published.summary.clone())
            .filter(|summary| summary.id == activity);
        let Some(summary) = summary else {
            return;
        };
        let key = key(app, &summary);
        self.loose.borrow_mut().remove(&key);
        self.pulled.borrow_mut().remove(&key);
        self.collapsed.borrow_mut().remove(&key);
        self.touched.borrow_mut().retain(|touched| touched != &key);
        if let Some(close) = summary.close_control() {
            self.press(app, activity, &close.id);
        } else {
            self.hidden
                .borrow_mut()
                .insert(key, (summary.id, summary.title));
        }
    }

    /// The person folded the card back into the bar.
    pub(crate) fn collapse(&self, app: &str, activity: &str) {
        let key = format!("{app}:{activity}");
        self.pulled.borrow_mut().remove(&key);
        self.collapsed.borrow_mut().insert(key);
    }

    /// The person pulled the activity out of the bar's pill: its card shows again, however the app
    /// asked for it. An activity's id is its app's own, so the app the pill named is half of what
    /// it names.
    pub(crate) fn expand(&self, app: &str, activity: &str) {
        let summary = self
            .read()
            .iter()
            .find(|(shown, _)| shown == app)
            .and_then(|(_, published)| published.summary.clone())
            .filter(|summary| summary.id == activity);
        let Some(summary) = summary else {
            return;
        };
        let key = key(app, &summary);
        self.collapsed.borrow_mut().remove(&key);
        self.pulled.borrow_mut().insert(key);
        self.touch(app, activity);
    }

    /// The person left a card `at` a place of its own (`None`: back in the column the layer docks
    /// it in).
    pub(crate) fn moved(&self, app: &str, activity: &str, at: Option<Rect>) {
        let key = format!("{app}:{activity}");
        match at {
            Some(rect) => self.loose.borrow_mut().insert(key, rect),
            None => self.loose.borrow_mut().remove(&key),
        };
        self.touch(app, activity);
    }

    /// The due events the apps want announced, each told once.
    pub(crate) fn announce(&self, notices: &dyn Told) {
        for (app, published) in self.read().iter() {
            let Some(announcement) = &published.announce else {
                continue;
            };
            let key = format!("{app}:{}", announcement.id);
            if !self.announced.borrow_mut().insert(key) {
                continue;
            }
            let title = if announcement.title.is_empty() {
                crate::i18n::t("clock-due-title")
            } else {
                announcement.title.clone()
            };
            let body = if announcement.body.is_empty() {
                crate::i18n::t("clock-due-body")
            } else {
                announcement.body.clone()
            };
            notices.show(&Notice::new(String::new(), 1, title, body));
        }
    }

    /// What every app publishes now, by app, parsed once per board revision.
    fn read(&self) -> Rc<Vec<(String, Published)>> {
        let revision = self.board.revision();
        if let Some((at, shown)) = self.shown.borrow().as_ref()
            && *at == revision
        {
            return Rc::clone(shown);
        }
        let shown: Rc<Vec<(String, Published)>> = Rc::new(
            self.board
                .activities()
                .into_iter()
                .filter_map(|(app, activity)| {
                    serde_json::from_value(activity)
                        .inspect_err(|error| {
                            tracing::warn!(%error, app, "a live activity does not read back");
                        })
                        .ok()
                        .map(|mut published: Published| {
                            // A card's header names the app that shows the activity, in the
                            // person's language: the app knows its own copy, never the name this
                            // device calls it.
                            if let Some(summary) = &mut published.summary
                                && summary.source.is_empty()
                            {
                                summary.source = crate::app_names::app_name(&app);
                            }
                            (app, published)
                        })
                })
                .collect(),
        );
        self.shown.replace(Some((revision, Rc::clone(&shown))));
        shown
    }

    /// Whether `summary` shows: an activity the person put away stays away until the app shows
    /// another one.
    fn shows(&self, app: &str, summary: &Summary) -> bool {
        let key = key(app, summary);
        let hidden = self.hidden.borrow().get(&key).cloned();
        let identity = (summary.id.clone(), summary.title.clone());
        match hidden {
            Some(put_away) if put_away == identity => false,
            Some(_) => {
                // The app's answer: it shows something new, and shows again.
                self.hidden.borrow_mut().remove(&key);
                true
            }
            None => true,
        }
    }

    fn touch(&self, app: &str, activity: &str) {
        let key = format!("{app}:{activity}");
        let mut touched = self.touched.borrow_mut();
        touched.retain(|seen| seen != &key);
        touched.insert(0, key);
    }

    fn recently_touched(&self, key: &str) -> Option<usize> {
        self.touched
            .borrow()
            .iter()
            .position(|touched| touched == key)
    }
}

/// `<app>:<activity>` : what the person's say over one activity is kept under.
fn key(app: &str, summary: &Summary) -> String {
    format!("{app}:{}", summary.id)
}

/// What the bar reads of the session besides the desk's tree: the
/// notifications' state, the machine's own status, the apps' live activities,
/// and the running applications' tray items.
pub(crate) struct BarFeeds {
    pub(crate) signals: Rc<NoticeSignals>,
    pub(crate) readings: crate::status::Shared,
    pub(crate) activities: Rc<Activities>,
    /// The applications' tray items, when their watcher serves.
    pub(crate) tray: Option<Arc<tilo_tray_host::Tray>>,
    /// The monitors of the displays a fullscreen window owns, as the loop found them this pass.
    pub(crate) fullscreen: Rc<RefCell<Vec<String>>>,
    /// The windows this session shares with its workspaces, which the bar says are being shared
    /// while anyone views one.
    pub(crate) shares: Rc<RefCell<crate::window_share::WindowShares>>,
    /// Whether each display's bar shows over the window covering it, which the bar reads for itself
    /// and the chrome follows as the pointer, a key or a finger reaches for it.
    pub(crate) reveals: Rc<RefCell<tilo_desktop::topbar::Reveals>>,
    /// Settings' Identify as the session read it: each display's number and the name Settings gives
    /// it, which the chrome says on that display for a moment.
    pub(crate) identify: Rc<RefCell<Option<tilo_desktop::display_identify::Ask>>>,
    /// Whether the person may invite someone into the workspace in front, as the session's loop
    /// last answered it: the bar offers its invite button on it.
    pub(crate) invitable: Rc<Cell<bool>>,
}

/// What the chrome hands the session while it runs: the intents and asks its bar raises, over the
/// feeds that keep the bar current.
pub(crate) struct ChromeWiring {
    pub(crate) intents: Rc<RefCell<Vec<ChromeIntent>>>,
    pub(crate) asks: Rc<RefCell<Vec<DesktopAsk>>>,
    /// The desktop's one shortcut table, shared with the session: what the chrome matches a press
    /// against and what it shows beside its rows.
    pub(crate) shortcuts: crate::panels::mount::Shortcuts,
    pub(crate) feeds: BarFeeds,
    /// This device's workspaces, as their records hold them.
    pub(crate) engine: Option<Arc<crate::engine::Engine>>,
    /// What the desktop's control hints read and give back.
    pub(crate) hints: tilo_desktop::control_hints::HintsFeed,
    /// The keys a held Super reaches, as the session says them.
    pub(crate) peek: tilo_desktop::super_peek::SuperPeek,
}

/// The bar's view of the session: the desktop's tree, the wall clock, who is here, the machine's
/// readings and the apps' activities. What the bar shows of them is the shared chrome's
/// ([`tilo_desktop_surface::host::chrome::HostChrome`]); this answers only where the facts come
/// from.
pub(crate) struct SessionChrome {
    pub(crate) handle: DesktopHandle,
    /// Where the bar's intents go: the chrome carries them out.
    intents: Rc<RefCell<Vec<ChromeIntent>>>,
    /// Who this desktop acts for, and changes the workspace as.
    person: String,
    /// What the bar reads of the notifications: the unread count, the silenced bell and each app's
    /// own badge.
    signals: Rc<NoticeSignals>,
    /// The session has no quick settings panel: the status cluster is the bell that opens the
    /// notification centre.
    notifications_only: bool,
    /// Who is here, when this session takes part in the collaboration plane: the bar's trees carry
    /// them as the active workspace's collaborators.
    sync: Option<Rc<crate::sync::Client>>,
    /// What each window of the desk shows, as its app reports it: where a person's record stands,
    /// so the roster places them in the window they work in.
    notes: std::sync::Arc<crate::continuations::Continuations>,
    /// The machine as the services last published it: the status cluster's battery, network and
    /// volume glyphs, and the streams being captured.
    readings: crate::status::Shared,
    /// The apps' live activities, which the bar's pill names and the floating layer shows as cards.
    activities: Rc<Activities>,
    /// The running applications' status-notifier items, as their host keeps them.
    tray: Option<Arc<tilo_tray_host::Tray>>,
    /// The monitors of the displays a fullscreen window owns, which the loop takes from the
    /// compositor each pass.
    fullscreen: Rc<RefCell<Vec<String>>>,
    /// The windows this session shares, which the bar's sharing chip names.
    shares: Rc<RefCell<crate::window_share::WindowShares>>,
    /// Settings' Identify as the session read it.
    identify: Rc<RefCell<Option<tilo_desktop::display_identify::Ask>>>,
    /// Whether the person may invite someone into the workspace in front.
    invitable: Rc<Cell<bool>>,
    /// This device's workspaces, as their records hold them: what says whether a workspace may be
    /// deleted.
    engine: Option<Arc<crate::engine::Engine>>,
    /// Who made each strip: what its maker may change of it.
    authors: RefCell<Authors>,
}

impl SessionChrome {
    pub(crate) fn new(
        handle: DesktopHandle,
        intents: Rc<RefCell<Vec<ChromeIntent>>>,
        person: String,
        feeds: BarFeeds,
        notifications_only: bool,
        sync: Option<Rc<crate::sync::Client>>,
        notes: std::sync::Arc<crate::continuations::Continuations>,
        engine: Option<Arc<crate::engine::Engine>>,
    ) -> Self {
        let BarFeeds {
            signals,
            readings,
            activities,
            tray,
            fullscreen,
            shares,
            identify,
            invitable,
            ..
        } = feeds;
        Self {
            handle,
            intents,
            person,
            signals,
            notifications_only,
            sync,
            notes,
            readings,
            activities,
            tray,
            fullscreen,
            shares,
            identify,
            invitable,
            engine,
            authors: RefCell::new(Authors::new()),
        }
    }
}

impl ChromeSource for SessionChrome {
    fn handle(&self) -> &DesktopHandle {
        &self.handle
    }

    fn person(&self) -> &str {
        &self.person
    }

    /// Who is here, the window they are in and the call they are in reach the bar as its faces and
    /// roster, and none of it moves the tree: without this the face of everyone who arrives after
    /// the first frame never draws.
    fn people_revision(&self) -> u64 {
        self.sync.as_ref().map_or(0, |sync| sync.people_revision())
    }

    fn place_people(&self, tree: &mut DesktopTree) {
        if let Some(sync) = &self.sync {
            sync.show(tree, &self.notes, &|app: &str| {
                Some(crate::app_names::app_name(app))
            });
        }
    }

    /// The person at this device owns its desktop: the desktop's one policy answers what they may
    /// change, as it answers a workspace's owner.
    fn with_standing<R>(&self, answer: impl FnOnce(&Standing<'_>) -> R) -> R {
        answer(&Standing {
            person: &self.person,
            role: Some(Role::Owner),
            members: None,
        })
    }

    fn with_authors<R>(&self, answer: impl FnOnce(&mut Authors) -> R) -> R {
        answer(&mut self.authors.borrow_mut())
    }

    fn own_chip(&self) -> OwnChip {
        let own = self
            .sync
            .as_ref()
            .map_or_else(crate::sync::OwnPresence::default, |sync| sync.own());
        // The bar's own avatar is the same face their card shows: one name, one identity, one
        // colour, read where the card reads it.
        let (name, identity) = crate::people::own_face(&own, &self.person);
        let (red, green, blue) = crate::people::own_live_rgb(self.sync.as_deref(), &identity);
        OwnChip {
            // The name they go by, and the root key the picture they chose hangs on: the id is
            // never shown as their name.
            name,
            rgb: (red, green, blue),
            identity,
        }
    }

    fn notices(&self) -> Notices {
        Notices {
            unread: self.signals.unread(),
            quiet: self.signals.quiet(),
        }
    }

    fn may_invite(&self) -> bool {
        self.invitable.get()
    }

    fn waved(&self) -> Option<tilo_desktop::chrome::WaveNotice> {
        self.sync.as_ref().and_then(|sync| sync.wave_notice())
    }

    fn app_badge(&self, app: &str) -> u32 {
        self.signals.badge(app)
    }

    fn facts(&self, facts: &mut Facts) {
        // Who views the windows this session shares: the bar says the share is live while anyone
        // does.
        for exposure in self.shares.borrow().exposures() {
            facts.number(exposure.surface);
            facts.number(exposure.peer);
            facts.when(exposure.can_control);
            // What names a viewer may be heard after it started watching: the bar says it then.
            facts.words(&exposure.name);
        }
        facts.number(self.tray.as_ref().map_or(0, |tray| tray.revision()));
        // A fullscreen window on a display makes that display's bar step aside.
        for monitor in self.fullscreen.borrow().iter() {
            facts.words(monitor);
        }
        if let Some(ask) = self.identify.borrow().as_ref() {
            facts.number(ask.id);
        }
        facts.number(self.activities.board().revision());
        // The bar says its words in the person's language: another one relabels it.
        facts.number(ui::I18nManager::revision());
        // Renaming themselves or choosing a colour relabels and repaints their chip in the bar,
        // and the presence label they gave themselves moves it as their display name does.
        match ui::local_identity::load().map(|identity| identity.display_name) {
            Some(name) => facts.words(&name),
            None => facts.number(0),
        }
        match settings_store::identity::COLOUR.get() {
            Some(colour) => facts.words(&colour),
            None => facts.number(0),
        }
    }

    fn fill(&self, snapshot: &mut DesktopChromeSnapshot) {
        snapshot.status = self.readings.borrow().status();
        snapshot.activities = self.activities.pill();
        snapshot.notifications_only = self.notifications_only;
        snapshot.arrived = self.sync.as_ref().and_then(|sync| sync.arrival_notice());
        snapshot.active_exposures = self.shares.borrow().exposures();
        snapshot.identify.clone_from(&self.identify.borrow());
    }

    fn take_intent(&self, intent: ChromeIntent) -> bool {
        self.intents.borrow_mut().push(intent);
        true
    }

    /// One bar per display: the one in use full, the others compact, each on its own display's
    /// rectangle. A fullscreen window on a display names it, so that display's bar steps aside.
    fn bars(&self) -> Placement {
        let fullscreen = self.fullscreen.borrow();
        self.handle
            .read(|tree| Placement::of(tree, fullscreen.as_slice()))
    }

    /// A workspace may be branched by whoever reads all of it: the person is in it here and not
    /// removed. One this device holds no governance of is the desk's own, and the person's unless
    /// it names someone else as its owner.
    fn may_branch_workspace(
        &self,
        workspace: tilo_desktop_model::WorkspaceSlot,
    ) -> Result<(), String> {
        let (id, owner) = self
            .handle
            .read(|tree| {
                tree.workspaces
                    .iter()
                    .find(|held| held.slot == workspace)
                    .map(|held| (held.id(), held.owner().cloned()))
            })
            .ok_or_else(|| tilo_desktop_surface::i18n::t("desktop-workspace-gone"))?;
        let reads = match &self.engine {
            Some(engine) if engine.holds(id) => engine.reads_whole(id),
            _ => owner.is_none_or(|owner| owner == self.person),
        };
        if reads {
            Ok(())
        } else {
            Err(tilo_desktop_surface::i18n::t(
                "desktop-cannot-branch-workspace",
            ))
        }
    }

    fn may_delete_workspace(
        &self,
        workspace: tilo_desktop_model::WorkspaceSlot,
    ) -> Result<(), String> {
        let Some(engine) = &self.engine else {
            return Err(tilo_desktop_surface::i18n::t(
                "desktop-cannot-delete-workspace",
            ));
        };
        let id = self
            .handle
            .read(|tree| {
                tree.workspaces
                    .iter()
                    .find(|held| held.slot == workspace)
                    .map(tilo_desktop_model::Workspace::id)
            })
            .ok_or_else(|| tilo_desktop_surface::i18n::t("desktop-workspace-gone"))?;
        if engine.may_delete(id) {
            Ok(())
        } else {
            Err(tilo_desktop_surface::i18n::t("desktop-workspace-stays"))
        }
    }

    /// The running applications' tray items, in the order they registered: each one's own title,
    /// the app that registered it, and its picture or letter badge.
    fn app_tray(&self) -> Vec<AppTrayItem> {
        let Some(tray) = &self.tray else {
            return Vec::new();
        };
        tray.snapshot().into_iter().map(app_tray_item).collect()
    }

    /// Changes whenever an application's tray item appears, changes or goes, so the bar redraws
    /// that icon and nothing else.
    fn app_tray_revision(&self) -> u64 {
        self.tray.as_ref().map_or(0, |tray| tray.revision())
    }
}

/// One tray item as the bar draws it: the app's own words for it, the app it demonstrably belongs
/// to, and the pixels it sent or named, with a letter badge for an icon nothing could draw.
fn app_tray_item(item: tilo_tray_host::TrayItem) -> AppTrayItem {
    AppTrayItem {
        key: item.key,
        id: item.id.clone(),
        owner_app: item.address.owner_app.clone(),
        title: if item.title.trim().is_empty() {
            item.id
        } else {
            item.title
        },
        status: match item.status {
            tilo_tray_host::ItemStatus::Passive => AppTrayStatus::Passive,
            tilo_tray_host::ItemStatus::Active => AppTrayStatus::Active,
            tilo_tray_host::ItemStatus::NeedsAttention => AppTrayStatus::NeedsAttention,
        },
        icon: match item.icon {
            tilo_tray_host::TrayIcon::Pixels {
                width,
                height,
                rgba,
            } => AppTrayIcon::Pixels {
                width,
                height,
                rgba,
            },
            tilo_tray_host::TrayIcon::Letter(letter) => AppTrayIcon::Letter(letter),
        },
        item_is_menu: item.item_is_menu,
    }
}

impl crate::Session {
    /// The rows of the window menu the session shows now, in order, as a
    /// harness reads them: a submenu's rows read after the row that opens it,
    /// a rule between groups is no row at all, and the list is empty while the
    /// menu is closed.
    #[must_use]
    pub fn window_menu_rows(&self) -> Vec<String> {
        fn walked(rows: &[serde_json::Value], into: &mut Vec<String>) {
            for row in rows {
                if row.get("separator").and_then(serde_json::Value::as_bool) == Some(true) {
                    continue;
                }
                if let Some(label) = row.get("label").and_then(serde_json::Value::as_str) {
                    into.push(label.to_owned());
                }
                if let Some(children) = row.get("children").and_then(serde_json::Value::as_array) {
                    walked(children, into);
                }
            }
        }
        let Some(reading) = self.read(crate::shell::state::WINDOW_MENU) else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        if let Some(top) = reading.get("rows").and_then(serde_json::Value::as_array) {
            walked(top, &mut rows);
        }
        rows
    }
}

/// What holds the keyboard over the desk: a surface opened over the field the person types in,
/// else the desk's search from the pass it says it is open, and what the overlay stack says
/// otherwise.
/// The palette is the desk's own state while it is the chrome that draws it and publishes its
/// surface. A key typed between the desk opening it and the chrome drawing it is the palette's, not
/// the window's behind it: what the person typed goes into the search, and the frame that key asks
/// for is the one that publishes the palette's surface.
pub(crate) fn keyboard_target(handle: &DesktopHandle, overlays: &Overlays) -> Target {
    if let Some(over) = overlays.over() {
        Target::Surface(over)
    } else if handle.search_open() {
        Target::Surface(Slot::Search)
    } else {
        overlays.target()
    }
}

/// The chrome above the windows (the top bar, overview, menus) as the session hosts it: its own
/// tree beside the desk's, covering the outputs.
pub(crate) fn host(
    handle: &DesktopHandle,
    options: &crate::Options,
    wiring: ChromeWiring,
    sync: Option<Rc<crate::sync::Client>>,
    overview: &crate::overview::Overview,
    emoji: &tilo_desktop::emoji::EmojiLink,
    overlays: &Overlays,
    switcher: &tilo_desktop::window_switcher::Switcher,
) -> WidgetHost {
    let ChromeWiring {
        intents,
        asks,
        shortcuts,
        feeds,
        engine,
        hints,
        peek,
    } = wiring;
    let reveals = Rc::clone(&feeds.reveals);
    let tree = handle.read(Clone::clone);
    // The bar's source hands its intents to the chrome it sits in, and answers the desk's rules
    // from the same facts.
    let source = Rc::new(HostChrome::new(Rc::new(SessionChrome::new(
        handle.clone(),
        Rc::clone(&intents),
        options.person.clone(),
        feeds,
        !crate::quick_settings::installed(&options.surfaces),
        sync,
        options.launches.notes(),
        engine,
    ))));
    let bar = TopBarChrome::from_tree(&tree)
        .with_chrome(source.clone())
        .with_tilo_mark();
    let mut chrome = DesktopChrome::new(
        handle.clone(),
        crate::GENERATION.into(),
        source,
        bar,
        TiloMenu::new(AboutVariant::Native, &AboutLinks::default()),
        false,
    );
    chrome.set_intents(intents);
    chrome.set_asks(asks);
    chrome.set_overview(overview.flight(), overview.open_flag());
    chrome.set_emoji(emoji.clone());
    chrome.set_overlays(overlays.clone());
    chrome.set_switcher(switcher.clone());
    chrome.set_reveals(reveals);
    chrome.set_shortcuts(shortcuts);
    chrome.set_hints_feed(hints);
    chrome.set_super_peek(peek);
    // Keys come to the chrome while one of the surfaces it draws holds the keyboard: the stack says
    // which, and it says the window's again as soon as the top surface closes.
    let desk = handle.clone();
    let holds = overlays.clone();
    let keyboard = Rc::new(move || {
        keyboard_target(&desk, &holds)
            .slot()
            .is_some_and(Slot::is_chrome)
    });
    let placement = SurfacePlacement {
        origin: (0, 0),
        size: (1, 1),
        scale: options.scale,
    };
    let mut host = WidgetHost::new(Box::new(chrome), placement, false).named("Top bar");
    host.set_theme((options.presentation)().theme);
    // The chrome draws the overview, so the overview's flight asks frames of the chrome alone.
    overview.drawn_on(host.host().motion_surface());
    // What the compositor draws over the chrome: the overview's flying windows,
    // and the ones the switcher's panel shows in its cards.
    let pictures = overview.clone();
    let walking = switcher.clone();
    let host = host
        .with_window_pictures(Rc::new(move || {
            let mut shown = pictures.pictures();
            shown.extend(walking.pictures().into_iter().map(|picture| {
                let (x, y, width, height) = (
                    picture.rect.x0,
                    picture.rect.y0,
                    picture.rect.width(),
                    picture.rect.height(),
                );
                tilo_session_core::surface::WindowPicture {
                    window: picture.window,
                    rect: (x, y, width, height),
                    opacity: 1.0,
                    // A picture stays in its well: paging the cards never spills a window over the
                    // panel's edge.
                    clip: Some((x, y, width, height)),
                }
            }));
            shown
        }))
        .covering_outputs()
        .with_keyboard(keyboard);
    // The stack takes a surface down — a press outside it, another one opening — and the widget
    // drawing it has to take up a pass to close it: nothing else tells a host the chrome stands
    // over.
    overlays.wake_with(host.asking());
    host
}

impl crate::Shared {
    /// A verb of the empty desk's launchpad, as its card, its host action and its resource action
    /// all ask for it: the app action the verb declares, what only the session can do, and else the
    /// app itself. A verb whose app this device does not have opens nothing: the person is shown
    /// the apps it does have, never a window of an app that is not here.
    pub(crate) fn launchpad(&self, action: LaunchpadAction) {
        #[cfg(target_os = "linux")]
        if !crate::host_actions::offers(&self.lists, action) {
            return self.handle.open_search(Scope::WindowGroup(None), None);
        }
        if let Some(app_action) = action.app_action() {
            return self.invoke_app_action(app_action);
        }
        match action {
            // Bringing people in: the link into this workspace is made in the invite sheet.
            LaunchpadAction::Invite => self.open_invite(),
            LaunchpadAction::Activity => self
                .intents
                .borrow_mut()
                .push(ChromeIntent::ShowActivity(None)),
            other => match other.app_target() {
                Some((app, _)) => {
                    let launch = ChromeEffect::OpenApp {
                        app: app.into(),
                        beside: None,
                        background: None,
                        float: None,
                    };
                    if let Err(error) = self.handle.dispatch(launch) {
                        tracing::warn!(%error, app, "launchpad");
                    }
                }
                None => self.handle.open_search(Scope::WindowGroup(None), None),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::live_activity::{Control, ControlKind, Kind, Tone};

    /// A timer's summary as Clocks publishes it: the label and countdown every timer carries, and
    /// what `extra` adds.
    fn timer(id: &str, title: &str, countdown: i64) -> Summary {
        Summary::new(
            Kind::Timer,
            title,
            ui::format::timecode(countdown.unsigned_abs()),
        )
        .with_id(id)
        .with_countdown(countdown)
        .with_control(Control::new("toggle", "Pause", ControlKind::Primary))
        .with_control(Control::new("close", "End", ControlKind::Close))
    }

    fn publish(board: &runtime_collab::LiveActivities, app: &str, published: Published) {
        board.show(app, Some(serde_json::to_value(published).expect("JSON")));
    }

    /// A running timer is the bar's pill, and its card floats because Clocks asks for it.
    #[test]
    fn a_floating_timer_is_the_bars_pill_and_its_card() {
        let board = Arc::new(runtime_collab::LiveActivities::default());
        let activities = Activities::new(Arc::clone(&board), crate::cards::Cards::new());
        let mut published = Published::of(timer("timer-1", "Tea", 90));
        published.overlay = true;
        publish(&board, CLOCKS_APP, published);

        let summaries = activities.summaries();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].0, CLOCKS_APP);
        assert_eq!(summaries[0].1.title, "Tea");
        let cards = activities.cards();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].app, CLOCKS_APP);
        assert_eq!(cards[0].summary.id, "timer-1");

        // An activity that does not ask to float is the pill's only.
        let mut quiet = Published::of(timer("timer-2", "Bread", 600));
        quiet.overlay = false;
        publish(&board, CLOCKS_APP, quiet);
        assert_eq!(activities.summaries().len(), 1);
        assert!(activities.cards().is_empty());
    }

    /// A call's card stands where its app's presentation put it, and the layer paints the summary
    /// there until the app's own window is up: a card appears, and the screen never shows a blank
    /// card or two of them.
    #[test]
    fn a_call_card_is_the_layers_until_the_apps_own_window_is_up() {
        const CHAT_APP: &str = "so.tilo.chat";
        let board = Arc::new(runtime_collab::LiveActivities::default());
        let cards = crate::cards::Cards::new();
        let activities = Activities::new(Arc::clone(&board), cards.clone());
        let mut published = Published::of(
            Summary::new(Kind::Call, "Ada", "0:12")
                .with_id("chat-call:voice")
                .with_icon("phone"),
        );
        published.overlay = true;
        publish(&board, CHAT_APP, published);

        // The call is the layer's own card until its app presents one.
        let layer_drawn = activities.cards();
        assert_eq!(layer_drawn.len(), 1);
        assert!(layer_drawn[0].size.is_none() && !layer_drawn[0].rich);

        let request = ui::floating_surface::FloatingSurfaceProtocolRequest {
            id: "chat-call:voice".into(),
            title: "Ada".into(),
            role: ui::floating_surface::FloatingSurfaceProtocolRole::ActiveCall,
            source: ui::floating_surface::FloatingSurfaceProtocolSource::AppRegion {
                app_id: CHAT_APP.into(),
                region: [0.0, 0.0, 360.0, 240.0],
            },
            preferred_size: [360.0, 240.0],
            aspect_ratio: None,
            initial_rect: Some([40.0, 60.0, 400.0, 300.0]),
            presentation_rect: Some([32.0, 52.0, 408.0, 308.0]),
        };
        cards.present(CHAT_APP, 1, &request).expect("a card");
        let card = cards.presented().remove(0);

        // Its place is held for the app's body, and the summary stands in until that body's window
        // is up.
        let waiting = activities.cards();
        assert_eq!(waiting.len(), 1);
        assert_eq!(
            waiting[0].size,
            Some(ui::vello::kurbo::Size::new(376.0, 256.0))
        );
        assert!(!waiting[0].rich, "the summary is the card until then");

        cards.placements().place(
            &card.token,
            (376, 256),
            Some(Rect::new(100.0, 100.0, 476.0, 356.0)),
        );
        cards.placements().mapped(&card.token, 3);
        assert!(activities.cards()[0].rich, "the app draws it now");

        // Chat's own window comes on screen: the call is drawn there, and no card stands on the
        // desk at all.
        cards.see(CHAT_APP, "chat-call:voice", true);
        assert!(activities.cards().is_empty());
        cards.see(CHAT_APP, "chat-call:voice", false);
        assert_eq!(activities.cards().len(), 1, "and it comes back");
    }

    /// Ending an activity that offers a close asks its app to end it; one that cannot be stopped
    /// from outside is put away until the app shows another one, and comes back the moment it does.
    #[tokio::test]
    async fn an_activity_is_ended_by_its_app_or_put_away() {
        let board = Arc::new(runtime_collab::LiveActivities::default());
        let activities = Activities::new(Arc::clone(&board), crate::cards::Cards::new());
        let clocks = runtime_collab::live_activity::LiveActivityServer::new(
            CLOCKS_APP,
            Arc::clone(&board),
            Arc::new(|| Ok(())),
        );
        let mut published = Published::of(timer("timer-1", "Tea", 90));
        published.overlay = true;
        publish(&board, CLOCKS_APP, published);

        activities.close(CLOCKS_APP, "timer-1");
        let (_, controls) = clocks.controls(0).await.expect("Clocks takes the close");
        let controls: serde_json::Value = serde_json::from_slice(&controls).expect("JSON");
        assert_eq!(
            controls,
            serde_json::json!([{ "command": "close", "activity_id": "timer-1" }]),
            "the card's close asked the app to end the timer"
        );

        // An activity with no close to offer is put away instead.
        let music = Summary::new(Kind::Music, "A song", "").with_id("track-1");
        publish(&board, "so.tilo.music", Published::of(music.clone()));
        let shown = || {
            activities
                .summaries()
                .into_iter()
                .any(|(app, _)| app == "so.tilo.music")
        };
        activities.close("so.tilo.music", "track-1");
        assert!(!shown(), "it is away");

        // The same song ticking on is still the same activity.
        publish(&board, "so.tilo.music", Published::of(music));
        assert!(!shown(), "still away");

        // Another song is the app's answer: it shows again.
        publish(
            &board,
            "so.tilo.music",
            Published::of(Summary::new(Kind::Music, "Another song", "").with_id("track-2")),
        );
        assert!(shown(), "shown again");
    }

    /// A control pressed on a card reaches the app it belongs to, with the activity it names, and
    /// makes that activity the most recently touched.
    #[tokio::test]
    async fn a_pressed_control_reaches_its_app() {
        let board = Arc::new(runtime_collab::LiveActivities::default());
        let activities = Activities::new(Arc::clone(&board), crate::cards::Cards::new());
        let clocks = runtime_collab::live_activity::LiveActivityServer::new(
            CLOCKS_APP,
            Arc::clone(&board),
            Arc::new(|| Ok(())),
        );
        activities.press(CLOCKS_APP, "timer-1", "add-minute");
        let (_, controls) = clocks.controls(0).await.expect("Clocks takes the control");
        let controls: serde_json::Value = serde_json::from_slice(&controls).expect("JSON");
        assert_eq!(
            controls,
            serde_json::json!([{ "command": "add-minute", "activity_id": "timer-1" }])
        );
    }

    /// At most three cards stand at once: the least recently touched folds into the bar's pill.
    #[test]
    fn at_most_three_cards_keep_the_most_recently_touched() {
        let board = Arc::new(runtime_collab::LiveActivities::default());
        let activities = Activities::new(Arc::clone(&board), crate::cards::Cards::new());
        for (app, id) in [
            ("so.tilo.maps", "route-1"),
            ("so.tilo.music", "track-1"),
            (CLOCKS_APP, "timer-1"),
            ("so.tilo.chat", "call-1"),
        ] {
            let mut published = Published::of(Summary::new(Kind::Other, app, "").with_id(id));
            published.overlay = true;
            publish(&board, app, published);
            activities.press(app, id, "noop");
        }
        let cards = activities.cards();
        assert_eq!(cards.len(), ui::floating_layer::MAX_CARDS);
        let kept: Vec<&str> = cards.iter().map(|card| card.summary.id.as_str()).collect();
        assert!(
            kept.contains(&"call-1"),
            "the newest keeps its card: {kept:?}"
        );
        assert!(kept.contains(&"timer-1"), "{kept:?}");
        assert!(kept.contains(&"track-1"), "{kept:?}");
        assert!(
            !kept.contains(&"route-1"),
            "the least recent folded: {kept:?}"
        );
        // Every one of them is still in the pill.
        assert_eq!(activities.summaries().len(), 4);
    }

    /// Collapsing a card folds it into the bar; pulling an activity out brings its card back,
    /// however it asked.
    #[test]
    fn collapse_and_expand_move_an_activity_between_the_bar_and_a_card() {
        let board = Arc::new(runtime_collab::LiveActivities::default());
        let activities = Activities::new(Arc::clone(&board), crate::cards::Cards::new());
        let mut published = Published::of(timer("timer-1", "Tea", 90));
        published.overlay = true;
        publish(&board, CLOCKS_APP, published);

        activities.collapse(CLOCKS_APP, "timer-1");
        assert!(activities.cards().is_empty(), "folded into the bar");
        assert_eq!(activities.summaries().len(), 1, "the pill still names it");

        // An activity's id is its own app's: another app that names the same id is not the activity
        // the pill named.
        activities.expand("so.tilo.other", "timer-1");
        assert!(
            activities.cards().is_empty(),
            "another app's activity of that id stays folded"
        );

        activities.expand(CLOCKS_APP, "timer-1");
        assert_eq!(activities.cards().len(), 1, "pulled out");
        assert_eq!(activities.cards()[0].summary.tone, Tone::Neutral);
    }

    /// A due event is announced once, in the app's own words when it has them and in the desktop's
    /// otherwise.
    #[test]
    fn a_due_event_is_announced_once() {
        let board = Arc::new(runtime_collab::LiveActivities::default());
        let activities = Activities::new(Arc::clone(&board), crate::cards::Cards::new());
        let notices = crate::notifications::told::Told::default();
        let announced = || notices.told();
        publish(
            &board,
            CLOCKS_APP,
            Published::announcing(ui::live_activity::Announcement {
                id: "due-1".into(),
                title: "Bread".into(),
                body: "Out of the oven".into(),
            }),
        );
        activities.announce(&notices);
        let live = announced();
        assert_eq!(
            live,
            [(String::from("Bread"), String::from("Out of the oven"))]
        );

        // The same due event, read again, is not announced again.
        activities.announce(&notices);
        assert_eq!(announced().len(), 1, "still one");

        // An app that names its own due event in no words gets the desktop's.
        board.show(CLOCKS_APP, None);
        publish(
            &board,
            CLOCKS_APP,
            Published::announcing(ui::live_activity::Announcement {
                id: "due-2".into(),
                title: String::new(),
                body: String::new(),
            }),
        );
        activities.announce(&notices);
        let live = announced();
        assert_eq!(live.len(), 2, "the next one is its own announcement");
        assert_eq!(live[1].0, crate::i18n::t("clock-due-title"));
        assert_eq!(live[1].1, crate::i18n::t("clock-due-body"));
    }
}
