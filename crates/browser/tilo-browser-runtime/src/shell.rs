//! The desktop's chrome in the page, from `tilo-desktop`'s widgets: the
//! native shell's backdrop, its top bar over the strip, the launchpad on a
//! desk with no windows yet, the overview panel, and the menu behind
//! the bar's Tilo mark with the About sheet saying what this is.
//!
//! Desktop chords are the native shell's, read from the same shortcut
//! table ([`tilo_desktop::shortcuts`]), where the browser passes Super
//! through, and the same keys with the page's stand-in for Super, which no
//! browser or system binds: Alt+Shift, or Ctrl+Option on a Mac (Option+Shift
//! selects by words there). Super+Tab, the overview, is the system's in most
//! browsers, so O stands in for its Tab. Chords the page has no use for
//! (locking, the magnifier, …) reach the window as ordinary keys.
//!
//! The bar reads a [`DesktopChromeSource`] the page answers from the
//! surface's tree and the page's clock; what it asks for comes back as
//! [`ChromeIntent`]s, applied as the surface's own navigation commands on the
//! next update pass. What only the embedding page can do (inviting someone)
//! is queued as a [`HostRequest`] for the mount's status.
//!
//! Who is here comes from the page ([`crate::here`]): their avatars join
//! the bar's from a copy of the tree, the bar's avatars open the People
//! popover with each person's place, status and recent summary, and the
//! windows they are in carry their faces. Pinned, the roster floats over
//! the windows as the people overlay, and following someone tracks their
//! view ([`people`]).

mod control_hints;
pub(crate) mod people;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use tilo_desktop::about::{AboutVariant, TiloMenu};
use tilo_desktop::chrome::{
    AvatarAsk, ChromeIntent, ClockSnapshot, DesktopChromeSnapshot, DesktopChromeSource,
};
use tilo_desktop::collaborators::{
    CollaboratorsHit, CollaboratorsModel, CollaboratorsOverlay, RecentChange, RosterRow,
    RosterStatus,
};
use tilo_desktop::desktop_host::{DeskLayer, DesktopAsk, DesktopChrome, DesktopRules};
use tilo_desktop::launchpad::{CollaborationLaunchpad, LaunchpadAction, LaunchpadCopy};
use tilo_desktop::profile_card::{
    LazyProfileCard, ProfileAction, ProfileChoice, ProfileGroup, ProfileModel,
};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop_model::commands::DesktopCommand;
use tilo_desktop_model::{DesktopTree, StripId, TabId};
use tilo_desktop_surface::navigation::Command;
use tilo_desktop_surface::{DesktopHandle, DesktopSurface, HostActionSource};
use ui::kurbo::{Affine, Point, Rect};
use ui::presence::PresenceCtx;
use ui::taffy::{NodeId, Style, TaffyTree};
use ui::vello::Scene;
use ui::widget::{AppModel, EventCtx, ModelPath, TaffyTreeExt as _, Widget};
use ui::winit::event::{ElementState, WindowEvent};
use ui::{TextRenderer, Theme};

/// What the desktop asks of the page that embeds it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum HostRequest {
    /// Invite someone into this workspace (`mountDesktop({onInvite})`).
    Invite,
    /// Perform the host action the person chose in search or on a banner
    /// (`mountDesktop({hostActions, onHostAction})`), by id.
    Action(String),
    /// Go by this name, as the person typed it in their profile card: the
    /// page's naming action, already answered (`onHostAction('set-name',
    /// name)`).
    Named(String),
    /// Have the room's host run an action of a room record for the person
    /// (`peer.actions`), and answer it with `desktop_room_action_answer`.
    RoomAction(crate::desktop::RoomActionRequest),
    /// Open Chat's direct conversation with this person, through Chat's own
    /// action, as following a link to it does (`desktop_run_own_ask`).
    Message(people::OwnAsk),
    /// Send a direct message through Chat's own `send-direct-message`
    /// (`desktop_run_own_ask`), and answer `id` with `desktop_message_answer`.
    Send(people::OwnAsk),
    /// Keep what the person says they are doing, and how they share it, in
    /// their profile, which each of their devices reads
    /// (`desktop_set_own_status`): set here, it shows there too.
    Status { status: String, sharing: String },
    /// Go to this address, leaving the page: a service's consent page,
    /// which comes back to the room's host and from there to this entry.
    Open(String),
    /// The person's linked accounts, as the room's host answered: their
    /// profile names them, and wears an account's picture until the person
    /// uploads their own (`onHostAction('set-linked', {accounts})`).
    Linked(Vec<tilo_app::linked_accounts::ProfileLink>),
}

/// The page's host action that lets the person choose their name; the
/// People panel offers it on the own name when the page lists it.
const RENAME_ACTION: &str = "set-name";
/// The avatar menu's "Report a problem…", which the page answers itself.
const REPORT_PROBLEM: &str = "report-problem";
/// Chat, which holds person-to-person conversations.
const CHAT_APP: &str = "so.tilo.chat";

/// The popovers the bar opens, one at a time.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BarPopover {
    /// The desktop chrome's own: the Tilo menu or the workspace menu.
    Chrome,
    Profile,
    People,
    /// The ask about the person, open beside their avatar.
    Ask,
    /// None of them: the page's own invite sheet takes over.
    Invite,
}

/// Requests the desktop queued since the page last read its status.
pub(crate) type HostRequests = Rc<RefCell<Vec<HostRequest>>>;

/// What the embedding page offers the desktop.
#[derive(Clone, Debug, Default)]
pub(crate) struct HostOffers {
    /// The page takes [`HostRequest::Invite`].
    pub invite: bool,
    /// The page runs on macOS: its command key is Cmd.
    pub mac: bool,
    /// The host of the room shown (`example.org`), named beside the
    /// workspace; `None` on the person's own desktop.
    pub host: Option<String>,
    /// The deployment's links the About sheet offers.
    pub about: tilo_desktop::about::AboutLinks,
    /// The build this page runs (its commit), which a report names.
    pub build: Option<String>,
    /// A linked-account offer's token the page arrived with: the profile
    /// card opens on it, asking the person to confirm.
    pub link_offer: Option<String>,
}

/// Who this desktop's person is when they make something in the room,
/// and who made what there.
#[derive(Clone, Default)]
pub(crate) struct Room {
    /// Who made each strip and opened each window.
    pub authors: crate::desktop::Authors,
    /// The person this desktop acts for.
    pub author: String,
}

/// The bar's view of the page: the surface's tree, the wall clock, and the
/// intents it raised since the last update pass.
struct PageChrome {
    handle: DesktopHandle,
    shared: crate::desktop::Shared,
    here: tilo_desktop::here::Here,
    room: Room,
    notifications: tilo_desktop_surface::DesktopNotifications,
    intents: RefCell<Vec<ChromeIntent>>,
    /// `(tree revision, minute, who-is-here revision, notifications
    /// revision, standing)` the snapshot revision was last bumped for.
    seen: Cell<(u64, i64, u64, u64, u8)>,
    revision: Cell<u64>,
    wake: Arc<dyn Fn() + Send + Sync>,
    /// The page takes invitations ([`HostOffers::invite`]): the bar keeps
    /// its invite button.
    invite: bool,
}

impl DesktopChromeSource for PageChrome {
    fn snapshot(&self) -> DesktopChromeSnapshot {
        let clock = ClockSnapshot::now();
        let seen = (
            self.handle.revision(),
            clock.unix_seconds.div_euclid(60),
            self.here.borrow().revision(),
            self.notifications.revision(),
            self.shared.standing.code(),
        );
        if self.seen.get() != seen {
            self.seen.set(seen);
            self.revision.set(self.revision.get().wrapping_add(1));
        }
        DesktopChromeSnapshot {
            revision: self.revision.get(),
            clock,
            watching: self.here.borrow().watching(),
            own: Some(self.own_chip()),
            // The page has no quick settings: the cluster opens the
            // notification centre, so it is that centre's bell.
            notifications_only: true,
            notification_badge: u32::try_from(self.notifications.unread()).unwrap_or(u32::MAX),
            invite: tilo_desktop::chrome::Invite::when(self.may_invite()),
            waved: self.here.borrow().wave_notice(),
            ..DesktopChromeSnapshot::default()
        }
    }

    /// The surface's tree with who is here as the active workspace's
    /// collaborators, for the bar's avatars.
    fn tree(&self) -> DesktopTree {
        let mut tree = self.handle.read(Clone::clone);
        let people = self.people(&tree);
        tilo_desktop::here::overlay_collaborators(&mut tree, &people);
        tree
    }

    fn intent(&self, intent: ChromeIntent) -> bool {
        self.intents.borrow_mut().push(intent);
        (self.wake)();
        true
    }

    /// What the page asks about the person themselves (keeping who they
    /// are), waiting on their avatar rather than as a banner over windows.
    fn avatar_ask(&self) -> Option<AvatarAsk> {
        self.handle.personal_ask().map(|ask| AvatarAsk {
            question: ask.title,
            detail: ask.body,
            answer: ask.action.title,
            open: ask.open,
        })
    }
}

impl PageChrome {
    /// Whether the bar offers its invite button: the page takes invitations
    /// and the room lets this person write in it. Someone it lets only look,
    /// a spectator among them, brings nobody in; until the room answered,
    /// the button stands, as for everyone it lets in.
    fn may_invite(&self) -> bool {
        self.invite
            && self
                .shared
                .standing
                .role()
                .is_none_or(|role| role.includes(tilo_action_policy::Role::Editor))
    }

    fn may(&self, command: &DesktopCommand) -> Result<(), String> {
        let members = self.shared.members.borrow();
        let standing = self.shared.standing_of(&self.room.author, members.as_ref());
        let authors = self.room.authors.borrow();
        self.handle.read(|tree| {
            let workspace = tree
                .active_workspace()
                .ok_or_else(|| "There is no workspace open".to_owned())?;
            tilo_desktop_model::commands::may_command(&standing, &authors, workspace, command)
                .map_err(|refusal| refusal.to_string())
        })
    }

    /// Whether this desktop's person may add a workspace to the room, by the
    /// desktop's one policy.
    fn may_create_workspace(&self) -> Result<(), String> {
        let members = self.shared.members.borrow();
        let standing = self.shared.standing_of(&self.room.author, members.as_ref());
        tilo_desktop_model::commands::may_create_workspace(&standing)
            .map_err(|refusal| refusal.to_string())
    }

    fn people(&self, tree: &DesktopTree) -> Vec<tilo_desktop::here::Person> {
        here_people(&self.here, &self.shared, tree)
    }

    /// The roster's rows for `people`, each follower's saying whom they
    /// follow, and those `(follower, followed)` pairs.
    fn roster_rows(
        &self,
        people: &[tilo_desktop::here::Person],
        theme: &Theme,
    ) -> (Vec<RosterRow>, Vec<(String, String)>) {
        let mut rows: Vec<RosterRow> = people
            .iter()
            .map(|person| roster_row(person, theme))
            .collect();
        let here = self.here.borrow();
        let pairs = here.following_pairs(people);
        tilo_desktop::collaborators::note_following(&mut rows, &pairs, here.own_id());
        (rows, pairs)
    }

    /// This page's person as everyone here sees them: their name (numbered
    /// when someone else here shares it) and colour.
    fn own_chip(&self) -> tilo_desktop::chrome::OwnChip {
        let here = self.here.borrow();
        let (name, rgb) = own_identity(&here);
        // Their root key, which wears the picture they chose.
        let identity = here.own_user().unwrap_or_default().to_owned();
        tilo_desktop::chrome::OwnChip {
            name,
            rgb,
            identity,
        }
    }
}

/// The page's answers to the desktop's layers: the room's policy for this
/// page's person, and who is here.
struct PageRules(Rc<PageChrome>, Option<String>);

impl DesktopRules for PageRules {
    fn tree(&self) -> DesktopTree {
        DesktopChromeSource::tree(&*self.0)
    }

    fn people_revision(&self) -> u64 {
        self.0.here.borrow().revision()
    }

    fn author(&self) -> String {
        self.0.room.author.clone()
    }

    fn may(&self, command: &DesktopCommand) -> Result<(), String> {
        self.0.may(command)
    }

    fn may_create_workspace(&self) -> Result<(), String> {
        self.0.may_create_workspace()
    }

    fn made_strip(&self, strip: StripId) {
        self.0
            .room
            .authors
            .borrow_mut()
            .insert(strip.0, self.0.room.author.clone());
    }

    fn workspace_host(&self) -> Option<String> {
        self.1.clone()
    }
}

/// This page's person as everyone here sees them, and as everything on the
/// page names them: their name, a guest's while they gave none, and colour.
pub(crate) fn own_identity(here: &tilo_desktop::here::HereState) -> (String, (u8, u8, u8)) {
    let name = here
        .own_shown()
        .unwrap_or_else(tilo_desktop::here::guest_label);
    let rgb = here
        .own_rgb()
        .unwrap_or_else(|| ui::presence::user_color(&name));
    (name, rgb)
}

/// Who is here, placed in `tree` and the records its windows show.
pub(crate) fn here_people(
    here: &tilo_desktop::here::Here,
    shared: &crate::desktop::Shared,
    tree: &DesktopTree,
) -> Vec<tilo_desktop::here::Person> {
    let records: Vec<(TabId, String)> = shared
        .bound_records()
        .into_iter()
        .map(|(tab, checkout)| (tab, checkout.id.as_str().to_owned()))
        .collect();
    let app_name = |app: &str| crate::desktop::known_application(app).map(str::to_owned);
    here.borrow().people(tree, &records, &app_name, now_ms())
}

/// Unix milliseconds.
pub(crate) fn now_ms() -> u64 {
    ui::time::unix_now_ms()
}

/// One person as the People popover lists them.
fn roster_row(person: &tilo_desktop::here::Person, theme: &Theme) -> RosterRow {
    let status = |line: &tilo_desktop::here::Line| RosterStatus {
        text: line.text.clone(),
        source: line.source,
    };
    RosterRow {
        id: person.id.clone(),
        label: person.label.clone(),
        initial: person
            .label
            .chars()
            .next()
            .map_or('\u{2022}', |c| c.to_uppercase().next().unwrap_or(c)),
        color: theme.tokens.colors.presence.of_rgb(person.rgb),
        is_agent: person.agent,
        is_bot: false,
        role: None,
        invited: false,
        activity: person.place.clone(),
        status: person.status.as_ref().map(status),
        // A page has no History entries to open: what they did recently
        // is one line, opening the workspace's History.
        recent: person
            .recent
            .iter()
            .map(|line| RecentChange {
                text: line.text.clone(),
                when: String::new(),
                entry: None,
            })
            .collect(),
        // Following brings forward, here, the window of what they are in.
        followable: true,
        availability: ui::Availability::Available,
        call: None,
        join: None,
        own_device: false,
    }
}

/// This page's person as the people here read them, for their own row in
/// the pinned roster: their name, colour, and what they are doing (their
/// own words, else the host's automatic line while they share one).
fn own_row(here: &tilo_desktop::here::HereState, theme: &Theme) -> RosterRow {
    let (label, rgb) = own_identity(here);
    RosterRow::own(
        ui::collaboration::local_user_id().0,
        label,
        theme.tokens.colors.presence.of_rgb(rgb),
        here.own_doing().map(|line| RosterStatus {
            text: line.text,
            source: line.source,
        }),
    )
}

/// The wall clock's minute, which the bar shows.
fn minute() -> i64 {
    ui::time::unix_now_i64().div_euclid(60)
}

/// The desktop the page mounts: the desktop's own layers (backdrop, top
/// bar, the strip with the launchpad beneath it while the desk shown is
/// empty), and what only the page shows over them.
pub(crate) struct BrowserShell {
    /// Under the windows: backdrop, launchpad and the strip.
    desk: DeskLayer,
    /// Over them: the bar, the strip ribbon and rail, the overview and
    /// the bar's menus.
    top: DesktopChrome,
    /// The minute the bar's clock last showed.
    drawn_minute: Cell<i64>,
    rect: Rect,
    chrome: Rc<PageChrome>,
    handle: DesktopHandle,
    generation: String,
    offers: HostOffers,
    requests: HostRequests,
    profile: LazyProfileCard,
    /// Whose card the profile card shows, by roster id; `None` for the
    /// person's own.
    profile_of: Option<String>,
    /// The People popover the bar's avatars open.
    people: CollaboratorsOverlay,
    people_open: bool,
    /// The roster pinned out of the popover, and following someone.
    pinned: people::PagePeople,
    /// The linked accounts' revision the card last showed.
    linked_seen: Cell<u64>,
    /// Asks again while the card shows a code, until the account links.
    linking: tilo_app::linked_accounts::LinkingRecheck,
    /// The linked accounts last handed to the page for the person's profile.
    profile_links: Option<Vec<tilo_app::linked_accounts::ProfileLink>>,
    /// How often, and when last, the room was asked for them unopened.
    links_asked: (u32, Option<ui::time::Instant>),
    /// The person whose recent summary the popover shows.
    expanded: Option<String>,
    /// The pointer, in logical pixels, for the popover's targets.
    cursor: Option<Point>,
    /// Pointing at what is wrong, to report it.
    picker: crate::feedback_pick::PagePicker,
    /// The desktop's control hints, and what the page's frame last showed
    /// them.
    hints: tilo_desktop::control_hints::ControlHints,
    hints_frame: tilo_desktop::control_hints::HintFrame,
    /// The page's settings as the hints last took the person's record from
    /// them.
    hints_settings: Option<settings_store::SettingsStamp>,
    /// How the person points, as the page last learned it: hints name what
    /// a finger does once they touch the screen.
    interaction: ui::InteractionEnvironment,
    /// What the hints painted in the frame before the last one drawn: a
    /// moving hint damages only there and where it is now.
    hints_painted_before: Option<Rect>,
}

/// What the top bar's chrome reads from the page besides the desktop tree.
pub struct ChromeSources {
    pub here: tilo_desktop::here::Here,
    pub shared: crate::desktop::Shared,
    pub room: Room,
    /// The page's notifications, whose unread ones the bell counts.
    pub notifications: tilo_desktop_surface::DesktopNotifications,
    pub wake: Arc<dyn Fn() + Send + Sync>,
}

impl BrowserShell {
    /// The open People popover takes the keyboard and the pointer over it; a
    /// press elsewhere closes it, and the bar's avatars toggle it. Answers
    /// whether the popover took the event.
    fn people_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        if !(self.people_open || self.people.wants_pointer_capture()) {
            return false;
        }
        let on_bar = self
            .cursor
            .is_some_and(|point| point.y < f64::from(tilo_desktop::topbar::TOPBAR_HEIGHT_PX));
        let hit = self
            .people
            .handle_event(event, ctx, self.people_open, self.cursor);
        if let Some(hit) = hit {
            if !(hit == CollaboratorsHit::Dismiss && on_bar) {
                self.apply_people_hit(hit);
            }
            *ctx.handled = !(hit == CollaboratorsHit::Dismiss && on_bar);
        }
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && self.people_open
            && !*ctx.handled
            && key.state == ElementState::Pressed
            && let Some(hit) = self
                .people
                .key(&key.logical_key, ctx.modifiers.state().shift_key())
        {
            self.apply_people_hit(hit);
            *ctx.handled = true;
        }
        *ctx.handled
    }

    /// Whether anything of the page besides a hint changed since it was
    /// drawn: the clock's minute, a menu or picker in motion, or a child.
    fn page_needs_redraw(&self) -> bool {
        self.drawn_minute.get() != minute()
            || self.picker.needs_animation_frame()
            || (self.people_open && self.people.needs_animation_frame())
            || self.pinned.needs_animation_frame()
            || [&self.desk as &dyn Widget, &self.top, &self.profile]
                .iter()
                .any(|child| child.needs_redraw())
    }

    pub fn new(
        surface: DesktopSurface,
        handle: DesktopHandle,
        generation: String,
        offers: HostOffers,
        requests: HostRequests,
        sources: ChromeSources,
    ) -> Self {
        let ChromeSources {
            here,
            shared,
            room,
            notifications,
            wake,
        } = sources;
        let chrome = Rc::new(PageChrome {
            handle: handle.clone(),
            shared,
            here,
            room,
            notifications,
            intents: RefCell::default(),
            seen: Cell::new((u64::MAX, 0, 0, 0, u8::MAX)),
            revision: Cell::new(0),
            wake,
            invite: offers.invite,
        });
        let tree = handle.read(Clone::clone);
        let bar = TopBarChrome::from_tree(&tree)
            .with_chrome(chrome.clone())
            .with_tilo_mark();
        let mut tilo_menu = TiloMenu::new(AboutVariant::Browser, &offers.about);
        // Offered only where the page holds a destination for reports.
        if chrome.shared.feedback.borrow().is_some() {
            tilo_menu = tilo_menu.with_feedback();
        }
        let launchpad = CollaborationLaunchpad::new().with_copy(welcome_copy(offers.mac));
        let hints = control_hints::load();
        let desk = DeskLayer::new(
            Box::new(surface),
            handle.clone(),
            launchpad,
            vec![Box::new(hints.announcer())],
        );
        // The desktop's shortcut table keeps the native defaults: the page
        // keeps no device settings to rebind them in.
        let top = DesktopChrome::new(
            handle.clone(),
            generation.clone(),
            Rc::new(PageRules(Rc::clone(&chrome), offers.host.clone())),
            bar,
            tilo_menu,
            offers.mac,
        );
        Self {
            desk,
            top,
            drawn_minute: Cell::new(i64::MIN),
            rect: Rect::ZERO,
            chrome,
            handle,
            generation,
            offers,
            requests,
            profile: LazyProfileCard::new(),
            profile_of: None,
            people: CollaboratorsOverlay::default(),
            people_open: false,
            pinned: people::PagePeople::new(),
            linked_seen: Cell::new(0),
            linking: tilo_app::linked_accounts::LinkingRecheck::default(),
            profile_links: None,
            links_asked: (0, None),
            expanded: None,
            cursor: None,
            picker: crate::feedback_pick::PagePicker::default(),
            hints,
            hints_frame: tilo_desktop::control_hints::HintFrame::default(),
            hints_settings: settings_store::stamp(),
            interaction: ui::InteractionEnvironment::default(),
            hints_painted_before: None,
        }
    }

    fn draw_people(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tree = self.handle.read(Clone::clone);
        let people = self.chrome.people(&tree);
        let (rows, following_pairs) = self.chrome.roster_rows(&people, theme);
        if self
            .expanded
            .as_ref()
            .is_some_and(|id| !rows.iter().any(|row| row.id == *id))
        {
            self.expanded = None;
        }
        let watching = usize::try_from(self.chrome.here.borrow().watching()).unwrap_or(usize::MAX);
        let workspace = self
            .handle
            .read(|tree| tree.active_workspace().map(|w| w.name.clone()))
            .unwrap_or_default();
        self.people.draw(
            scene,
            transform,
            text,
            theme,
            &CollaboratorsModel {
                host: self.rect,
                workspace: &workspace,
                rows: &rows,
                unavailable: None,
                exposure: None,
                may_invite: self.chrome.may_invite(),
                may_pin: !self.pinned.is_pinned(),
                followed_id: self.pinned.followed(),
                following_pairs: &following_pairs,
                expanded_id: self.expanded.as_deref(),
                watching,
                waved: &self.chrome.here.borrow().waved(now_ms()),
                spectating: self.chrome.shared.watching.get(),
            },
            self.cursor,
        );
    }

    /// The pinned roster and the following banner, over the windows and
    /// under what the bar opens. While following, the window of what they
    /// are in comes forward here first.
    fn draw_pinned_people(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tree = self.handle.read(Clone::clone);
        let people = self.chrome.people(&tree);
        let (handle, generation) = (&self.handle, &self.generation);
        // As following a link to their app does: its window here, else one.
        let open_app = |app: &str| {
            crate::desktop::known_application(app).is_some()
                && handle
                    .dispatch(
                        &Command::Open {
                            generation: generation.clone(),
                            app: app.to_owned(),
                        },
                        generation,
                    )
                    .is_ok()
        };
        if self.pinned.track(&people, handle, &open_app) {
            (self.chrome.wake)();
        }
        // Everyone here hears whom this person follows, while they do.
        let followed = self.pinned.followed().map(str::to_owned);
        self.chrome.here.borrow_mut().set_following(followed);
        let (rows, _) = self.chrome.roster_rows(&people, theme);
        // Whose device each row is: a row is someone's live publisher, a
        // conversation is with the person.
        let persons: Vec<(String, String)> = {
            let here = self.chrome.here.borrow();
            rows.iter()
                .filter_map(|row| Some((row.id.clone(), here.user_of(&row.id)?.to_owned())))
                .collect()
        };
        // Every window here, by the record it shows as the live layer names
        // it: a window on someone's screen showing one of these is shared.
        // A spectator only watches: nobody here reads them, so no row.
        let own = (!self.chrome.shared.watching.get())
            .then(|| own_row(&self.chrome.here.borrow(), theme));
        let records: Vec<(TabId, String, String)> = tree
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .filter_map(|tab| {
                let (app, record) = self.chrome.shared.live_record_of(&tree, tab.id)?;
                Some((tab.id, app, record))
            })
            .collect();
        self.pinned.draw(
            scene,
            transform,
            text,
            theme,
            &people::Frame {
                rect: self.rect,
                rows: &rows,
                own: own.as_ref(),
                persons: &persons,
                people: &people,
                tree: &tree,
                records: &records,
                cursor: self.cursor,
                content_top: self
                    .handle
                    .shown_tabs()
                    .iter()
                    .map(|(_, rect)| rect.y0)
                    .fold(0.0, f64::max),
            },
        );
        if let Some((name, reason)) = self.pinned.take_failed_send() {
            let title = tilo_desktop_surface::i18n::t_args(
                "people-overlay-message-failed",
                &[("name", name.into()), ("reason", reason.into())],
            );
            self.chrome
                .notifications
                .post(tilo_desktop_surface::notifications::Notification::new(
                    &title, "",
                ));
            (self.chrome.wake)();
        }
    }

    /// The pinned roster's and following's share of `event`: what lands on
    /// the overlay or the banner, the message box's keys, and the person's
    /// own input, which takes their view back from whom they follow.
    /// Returns whether the event was taken.
    fn pinned_people_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        let (taken, hit) = self.pinned.key(event, ctx.modifiers);
        if let Some(hit) = hit {
            self.apply_pinned_hit(hit);
        }
        if taken {
            *ctx.handled = true;
            return true;
        }
        let mut handled = false;
        if let Some(hit) =
            self.pinned
                .handle_event(event, ctx.scale_factor, self.cursor, &mut handled)
        {
            self.apply_pinned_hit(hit);
        }
        if handled {
            *ctx.handled = true;
            return true;
        }
        // A press on the desktop itself, not the bar: the person's own.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            ..
        } = event
            && self
                .cursor
                .is_some_and(|point| point.y >= f64::from(tilo_desktop::topbar::TOPBAR_HEIGHT_PX))
        {
            self.pinned.pressed_elsewhere();
        }
        false
    }

    fn apply_pinned_hit(&mut self, hit: tilo_desktop::people_overlay::PeopleHit) {
        use tilo_desktop::people_overlay::PeopleHit;
        match hit {
            PeopleHit::ToggleFollow(id) => self.pinned.toggle_follow(&id),
            PeopleHit::Unpin => self.pinned.pin(false),
            // Through Chat's own action, the one way a direct message goes.
            PeopleHit::Send { to, text } => {
                if let Some(outgoing) = self.pinned.send(&to, &text) {
                    self.requests.borrow_mut().push(HostRequest::Send(outgoing));
                }
            }
            // The edge was kept as it moved.
            PeopleHit::Moved(_) => {}
            PeopleHit::ShowPerson { id, anchor } => self.show_person(&id, None, anchor),
            PeopleHit::SetStatus(words) => {
                self.chrome.here.borrow_mut().set_own_words(Some(words));
                self.keep_own_status();
            }
        }
        (self.chrome.wake)();
    }

    fn apply_people_hit(&mut self, hit: CollaboratorsHit) {
        match hit {
            CollaboratorsHit::Dismiss => self.people_open = false,
            CollaboratorsHit::Invite => {
                self.people_open = false;
                self.invite();
            }
            // History at what this person did, as the native panel opens it.
            CollaboratorsHit::OpenChange(index, _) | CollaboratorsHit::OpenChanges(index) => {
                let author = self.people.changes_author(index).map(str::to_owned);
                self.people_open = false;
                self.top.activity_by(author);
            }
            CollaboratorsHit::ToggleDetails(index) => {
                let id = self.people.details_target(index).map(str::to_owned);
                self.expanded = if self.expanded == id { None } else { id };
            }
            CollaboratorsHit::Wave(index) => {
                // Through the room's live presence: the person waved at
                // sees it, and the row shows it sent.
                if let Some(target) = self.people.wave_target(index).map(str::to_owned)
                    && self.chrome.here.borrow_mut().wave_at(&target, now_ms())
                {
                    (self.chrome.wake)();
                }
            }
            CollaboratorsHit::ToggleFollow(index) => {
                if let Some(id) = self.people.follow_target(index).map(str::to_owned) {
                    self.pinned.toggle_follow(&id);
                }
            }
            CollaboratorsHit::Pin => {
                self.people_open = false;
                self.people.clear();
                self.pinned.pin(true);
            }
            CollaboratorsHit::JoinCall(_) | CollaboratorsHit::Absorb => {}
        }
    }

    fn dispatch(&self, command: &Command) {
        if let Err(error) = self.handle.dispatch(command, &self.generation) {
            web_sys::console::warn_1(&format!("desktop chrome: {error}").into());
        }
    }

    fn launch(&self, app: &str) {
        if crate::desktop::known_application(app).is_some() {
            ui::control_hints::performed::note(
                ui::control_hints::performed::Performed::OpenedWindow,
            );
            self.dispatch(&Command::Launch {
                generation: self.generation.clone(),
                app: app.into(),
            });
        } else {
            self.handle.open_search(None);
        }
    }

    /// The window in front, as a report about it names it: its app and
    /// the record it shows, which a recording of it records.
    fn front(&self) -> tilo_desktop::feedback::Focused {
        self.handle.read(|tree| {
            let focused = tilo_desktop::feedback::focused(tree, |app| {
                crate::desktop::known_application(app).map(str::to_owned)
            });
            match focused {
                tilo_desktop::feedback::Focused::App(mut subject) => {
                    subject.record = tilo_desktop::feedback::focused_tab(tree)
                        .and_then(|tab| self.chrome.shared.shown_record(tree, tab));
                    tilo_desktop::feedback::Focused::App(subject)
                }
                other => other,
            }
        })
    }

    /// Feedback about the window in front, in the one private composer.
    fn open_feedback(&self) {
        let subject = match self.front() {
            // Asked again from the composer: it keeps its subject.
            tilo_desktop::feedback::Focused::Composer => None,
            tilo_desktop::feedback::Focused::Nothing => Some(None),
            tilo_desktop::feedback::Focused::App(subject) => Some(Some(subject)),
        };
        if let (Some(subject), Some(feedback)) =
            (subject, self.chrome.shared.feedback.borrow().as_ref())
            && let Err(error) = feedback.about(subject)
        {
            web_sys::console::warn_1(&format!("feedback: {error}").into());
        }
        match self.handle.read(tilo_desktop::feedback::composer_tab) {
            Some((.., tab)) => {
                self.handle.focus_tab(tab);
            }
            None => self.launch(tilo_desktop::feedback::PACKAGE),
        }
    }

    /// Point at what is wrong: the picker, where the page holds a
    /// destination for reports.
    fn report_problem(&mut self) {
        if self.chrome.shared.feedback.borrow().is_some() {
            self.profile.close();
            self.picker.open();
        }
    }

    /// The windows on screen, as a report names what is in them.
    fn pick_windows(&self) -> Vec<crate::feedback_pick::PickWindow> {
        let records = self.chrome.shared.bound_records();
        let shown = self.handle.shown_tabs();
        self.handle.read(|tree| {
            shown
                .into_iter()
                .filter_map(|(tab, rect)| {
                    let subject = tilo_desktop::feedback::tab_subject(tree, tab, |app| {
                        crate::desktop::known_application(app).map(str::to_owned)
                    })?;
                    Some(crate::feedback_pick::PickWindow {
                        rect,
                        instance: tab,
                        app: subject.app,
                        name: subject.name,
                        record: records
                            .iter()
                            .find(|(shown, _)| *shown == tab)
                            .map(|(_, checkout)| checkout.id.as_str().to_owned()),
                    })
                })
                .collect()
        })
    }

    /// Hand what the person picked to the composer, and open it on it.
    fn follow_picker(&mut self) -> bool {
        let names = {
            let tree = self.chrome.tree();
            let mut names: Vec<String> = self
                .chrome
                .people(&tree)
                .into_iter()
                .map(|person| person.label)
                .collect();
            names.push(self.chrome.own_chip().name);
            names
        };
        let cx = crate::feedback_pick::ReportContext {
            windows: self.pick_windows(),
            build: self.offers.build.clone(),
            names,
        };
        if let Some((draft, crops)) = self.picker.take_report(self.desk.area(), &cx) {
            // The frame drawn next has no picker over it: the parts'
            // pictures are copied from it, then the composer opens.
            crate::feedback_crops::want(crate::feedback_crops::Wanted {
                draft,
                crops,
                wake: Arc::clone(&self.chrome.wake),
            });
            return true;
        }
        let Some(report) = crate::feedback_crops::take_ready() else {
            return false;
        };
        let subject = tilo_desktop::feedback::picked_subject(&report.draft, self.front());
        let started = self
            .chrome
            .shared
            .feedback
            .borrow()
            .as_ref()
            .map(|feedback| {
                feedback
                    .about(subject)
                    .and_then(|()| feedback.begin_report(report))
            });
        match started {
            Some(Ok(())) => match self.handle.read(tilo_desktop::feedback::composer_tab) {
                Some((.., tab)) => {
                    self.handle.focus_tab(tab);
                }
                None => self.launch(tilo_desktop::feedback::PACKAGE),
            },
            Some(Err(error)) => {
                web_sys::console::warn_1(&format!("feedback: {error}").into());
            }
            None => {}
        }
        true
    }

    fn activity(&self) {
        self.top.activity_by(None);
    }

    fn invite(&self) {
        if self.offers.invite {
            self.requests.borrow_mut().push(HostRequest::Invite);
        }
    }

    /// Carry out what the bar asked for: the page's own popovers here, the
    /// rest in the desktop's chrome.
    fn apply_intent(&mut self, intent: ChromeIntent) {
        let Some(intent) = self.page_intent(intent) else {
            return;
        };
        if matches!(
            intent,
            ChromeIntent::ToggleTiloMenu | ChromeIntent::ToggleWorkspaceMenu
        ) {
            // One popover from the bar at a time.
            self.close_bar_popovers_except(BarPopover::Chrome);
        }
        self.top.apply_intent(intent);
    }

    /// An intent only the page answers, carried out; any other is handed
    /// back.
    fn page_intent(&mut self, intent: ChromeIntent) -> Option<ChromeIntent> {
        match intent {
            ChromeIntent::ToggleQuickSettings | ChromeIntent::ShowQuickSettings => {
                // The page has no quick settings: the cluster opens the
                // notification centre.
                let open = self
                    .handle
                    .presentation()
                    .notifications
                    .is_some_and(|n| n.center_open);
                self.dispatch(&Command::NotificationCenter {
                    generation: self.generation.clone(),
                    open: !open || matches!(intent, ChromeIntent::ShowQuickSettings),
                });
            }
            ChromeIntent::ToggleCollaborators => {
                self.close_bar_popovers_except(BarPopover::People);
                self.people_open = !self.people_open && !self.top.menu_open();
                if !self.people_open {
                    self.people.clear();
                }
            }
            ChromeIntent::Invite => {
                self.close_bar_popovers_except(BarPopover::Invite);
                self.invite();
            }
            ChromeIntent::ShowPerson(id) => {
                // Their card, under their face.
                let anchor = self
                    .top
                    .bar()
                    .face_rect(&id)
                    .unwrap_or_else(|| self.top.bar().collaborators_rect());
                self.show_person(&id, None, anchor);
            }
            ChromeIntent::OpenAvatarAsk => {
                self.close_bar_popovers_except(BarPopover::Ask);
                self.handle.set_personal_ask_open(true);
            }
            ChromeIntent::ToggleProfile => self.toggle_profile(),
            // The bar asks for these only for what this page never shows it,
            // so none reaches here: no clock overlay or clock activity in
            // the snapshot (`HideClock`, `ClockCommand`), no continuity offer
            // from another device (`AcceptContinuity`, `DismissContinuity`),
            // and no tray items from running applications (`AppTray`).
            // Listed rather than caught by a wildcard, so a new request is
            // handled or explained here.
            ChromeIntent::HideClock
            | ChromeIntent::ClockCommand { .. }
            | ChromeIntent::AcceptContinuity
            | ChromeIntent::DismissContinuity
            | ChromeIntent::AppTray { .. } => {}
            other => return Some(other),
        }
        None
    }

    /// What the desktop's layers asked of the page since it last looked.
    fn follow_asks(&mut self) -> bool {
        let asks: Vec<DesktopAsk> = self
            .top
            .take_asks()
            .into_iter()
            .chain(self.desk.take_asks())
            .collect();
        let changed = !asks.is_empty();
        for ask in asks {
            match ask {
                DesktopAsk::Intent(intent) => {
                    let _ = self.page_intent(intent);
                }
                DesktopAsk::HostAction(id) => {
                    self.requests.borrow_mut().push(HostRequest::Action(id));
                }
                DesktopAsk::ReportProblem => self.report_problem(),
                DesktopAsk::Launchpad(action) => self.apply_launchpad(action),
                DesktopAsk::Shortcut(ui::shortcut_table::ShortcutAction::QuickSettings) => {
                    let _ = self.page_intent(ChromeIntent::ToggleQuickSettings);
                }
                // The page's own keys reach the browser's shortcuts; the
                // rest belong to a native session.
                DesktopAsk::Shortcut(_) => {}
            }
        }
        changed
    }

    /// Close every popover the bar opens but `keep`: one at a time.
    fn close_bar_popovers_except(&mut self, keep: BarPopover) {
        if keep != BarPopover::People && self.people_open {
            self.people_open = false;
            self.people.clear();
        }
        if keep != BarPopover::Chrome {
            self.top.close_menus();
        }
        if keep != BarPopover::Profile {
            self.profile.close();
        }
        if keep != BarPopover::Ask {
            // The ask waits as the avatar's dot while another popover is up.
            self.handle.set_personal_ask_open(false);
        }
    }

    /// The one card of a person, wherever they were clicked (a face on the
    /// bar, a row of the people overlay, an author in Chat), under
    /// `anchor`: `who` is their roster id or their root key, `name` what
    /// the click showed them as. The person's own opens their own card. A
    /// second ask for the card open closes it.
    fn show_person(&mut self, who: &str, name: Option<&str>, anchor: Rect) {
        let own = {
            let here = self.chrome.here.borrow();
            here.own_id()
                .is_some_and(|own| own.eq_ignore_ascii_case(who))
                || here
                    .own_user()
                    .is_some_and(|own| own.eq_ignore_ascii_case(who))
                || ui::collaboration::local_user_id()
                    .0
                    .eq_ignore_ascii_case(who)
        };
        let again = self.profile.is_open()
            && if own {
                self.profile_of.is_none()
            } else {
                self.profile_of.as_deref() == Some(who)
            };
        self.close_bar_popovers_except(BarPopover::Profile);
        self.profile.close();
        let _ = self.profile.take_choice();
        self.profile_of = None;
        if again {
            return;
        }
        if own {
            self.chrome.shared.linked.refresh();
            let model = self.profile_model();
            self.profile.toggle(&model, anchor, self.rect);
        } else if let Some(model) = self.person_model(who, name) {
            self.profile.toggle(&model, anchor, self.rect);
            self.profile_of = Some(who.to_owned());
        }
    }

    /// The card of the person `who` (their roster id, or their root key):
    /// who they are, what they are doing (their own words, or marked
    /// automatic), their id that stays theirs, and following them or
    /// writing to them in Chat. Someone not here now (an author of an old
    /// message) still has a card: their name, picture and id, and Message.
    fn person_model(&self, who: &str, name: Option<&str>) -> Option<ProfileModel> {
        let tree = self.handle.read(Clone::clone);
        let people = self.chrome.people(&tree);
        let person = people.iter().find(|person| person.id == who).or_else(|| {
            let here = self.chrome.here.borrow();
            people.iter().find(|person| {
                here.user_of(&person.id)
                    .or(person.user.as_deref())
                    .is_some_and(|user| user.eq_ignore_ascii_case(who))
            })
        });
        let Some(person) = person else {
            return absent_person(who, name?);
        };
        let id = person.id.as_str();
        let (rows, _) = self.chrome.roster_rows(&people, &Theme::dark());
        let row = rows.iter().find(|row| row.id == id)?;
        let key = self
            .chrome
            .here
            .borrow()
            .user_of(id)
            .map(str::to_owned)
            .or_else(|| person.user.clone());
        // Chat holds person-only conversations with people it lists; one
        // without a known person (a guest before the host's line) has none.
        let can_message =
            !person.agent && key.is_some() && crate::desktop::known_application(CHAT_APP).is_some();
        let linked = key
            .as_deref()
            .map(|key| self.chrome.here.borrow().linked_of(key).to_vec())
            .unwrap_or_default();
        Some(ProfileModel {
            name: row.label.clone(),
            identity: person.id.clone(),
            colour: Some(row.color),
            key,
            other: Some(tilo_desktop::profile_card::OtherPerson {
                id: id.to_owned(),
                status: row.status.clone(),
                place: row.activity.clone(),
                following: self.pinned.followed() == Some(id),
                can_follow: row.followable && !person.agent,
                can_message,
                linked,
            }),
            ..ProfileModel::default()
        })
    }

    /// Show the card asked for by an app (an author in Chat), at the
    /// pointer. Whether one was asked for.
    fn take_person_card_ask(&mut self) -> bool {
        let Some(asked) = ui::person_card::take() else {
            return false;
        };
        let at = self.cursor.unwrap_or_else(|| self.rect.center());
        let anchor = Rect::from_center_size(at, (2.0, 2.0));
        self.show_person(&asked.person, Some(&asked.name), anchor);
        true
    }

    /// Who the profile card is about, and what the page offers about them.
    fn profile_model(&self) -> ProfileModel {
        let offered = self.handle.host_actions();
        // Keeping (a passkey) and signing in as someone kept elsewhere are
        // offered only to a person this tab has not kept yet. Until then
        // they live in this tab alone, and pairing would hand another
        // device a guest that clearing this tab's storage ends: the card
        // offers keeping them first, pairing after.
        let unkept = offered
            .iter()
            .any(|action| matches!(action.id.as_str(), "keep-identity" | "sign-in-passkey"));
        let actions = offered
            .into_iter()
            .filter(|action| action.id != RENAME_ACTION)
            .filter(|action| !(unkept && action.source == Some(HostActionSource::Devices)))
            .filter_map(|action| {
                let group = match action.source? {
                    HostActionSource::Identity => ProfileGroup::Identity,
                    HostActionSource::Devices => ProfileGroup::Devices,
                    HostActionSource::Workspaces | HostActionSource::Workspace => return None,
                };
                let icon = match action.id.as_str() {
                    "save-recovery-key" => ui::icons::Icon::Download,
                    "sign-in-passkey" => ui::icons::Icon::User,
                    _ if group == ProfileGroup::Devices => ui::icons::Icon::MonitorSmartphone,
                    _ => ui::icons::Icon::Fingerprint,
                };
                Some(ProfileAction {
                    id: action.id,
                    title: action.title,
                    group,
                    icon,
                })
            })
            .collect::<Vec<_>>();
        // Reporting a problem, where the page holds a destination for it.
        let mut actions = actions;
        if self.chrome.shared.feedback.borrow().is_some() {
            actions.push(ProfileAction {
                id: REPORT_PROBLEM.into(),
                title: tilo_desktop::i18n::t("feedback-report-problem"),
                group: ProfileGroup::Together,
                icon: ui::icons::Icon::Flag,
            });
        }
        let rgb = Some(self.chrome.own_chip().rgb);
        // The name in the field is the person's own, without the number
        // others are shown beside it.
        let name = self
            .chrome
            .here
            .borrow()
            .own_label()
            .map_or_else(tilo_desktop::here::guest_label, str::to_owned);
        let watching = self.chrome.shared.watching.get();
        ProfileModel {
            identity: ui::collaboration::local_user_id().0,
            name,
            colour: rgb.map(|(r, g, b)| ui::vello::peniko::Color::from_rgb8(r, g, b)),
            can_rename: self.handle.offers_host_action(RENAME_ACTION),
            ask_name: false,
            actions,
            linked: self.chrome.shared.linked.accounts(),
            // What the page shares of what the person is doing is theirs to
            // choose here, beside their name; a spectator shares nothing,
            // since nobody here sees them.
            sharing: (!watching).then(|| self.chrome.here.borrow().sharing()),
            // What they are doing, in their own words: the card's field
            // beside how it is shared.
            status: self
                .chrome
                .here
                .borrow()
                .own_words()
                .unwrap_or_default()
                .to_owned(),
            seen: if watching {
                tilo_desktop::profile_card::Seen::Watching
            } else {
                tilo_desktop::profile_card::Seen::Here
            },
            control_hints: Some(self.hints.enabled()),
            key: self.chrome.here.borrow().own_user().map(str::to_owned),
            // Status, colour and what people see are the desktop's to keep;
            // a page shows the person as their host names them.
            ..ProfileModel::default()
        }
    }

    fn apply_profile_choice(&mut self, choice: ProfileChoice) {
        let request = match choice {
            ProfileChoice::ControlHints(on) => return self.set_hints_enabled(on),
            ProfileChoice::Rename(name) => HostRequest::Named(name),
            ProfileChoice::Action(id) if id == REPORT_PROBLEM => {
                self.report_problem();
                return;
            }
            ProfileChoice::Action(id) => HostRequest::Action(id),
            ProfileChoice::Link(app) => return self.link_account(&app, true),
            ProfileChoice::Unlink(app) => return self.link_account(&app, false),
            ProfileChoice::Accept(app) => return self.chrome.shared.linked.accept(&app),
            ProfileChoice::Decline(app) => return self.chrome.shared.linked.decline(&app),
            ProfileChoice::Sharing(mode) => {
                self.chrome.here.borrow_mut().set_sharing(mode);
                self.keep_own_status();
                return;
            }
            ProfileChoice::Status(words) => {
                self.chrome.here.borrow_mut().set_own_words(Some(words));
                self.keep_own_status();
                return;
            }
            ProfileChoice::Follow(id) => {
                self.pinned.toggle_follow(&id);
                (self.chrome.wake)();
                return;
            }
            ProfileChoice::Message(key) => HostRequest::Message(people::ask_own(
                &key,
                "open-direct-conversation",
                serde_json::Map::new(),
            )),
            ProfileChoice::Availability(_)
            | ProfileChoice::Colour(_)
            | ProfileChoice::Visibility(_) => return,
        };
        self.requests.borrow_mut().push(request);
    }

    /// Keep the person's own words and how they share what they do in
    /// their profile, so their other devices show the same.
    fn keep_own_status(&self) {
        let here = self.chrome.here.borrow();
        let request = HostRequest::Status {
            status: here.own_words().unwrap_or_default().to_owned(),
            sharing: here.sharing().word().to_owned(),
        };
        drop(here);
        self.requests.borrow_mut().push(request);
    }

    /// Ask the room's host to link (or unlink) the person's account with
    /// `app`'s service; the open card shows what came of it once the host
    /// answered ([`Self::follow_room_actions`]).
    fn link_account(&self, app: &str, link: bool) {
        let linked = &self.chrome.shared.linked;
        if link {
            linked.link(app);
        } else {
            linked.unlink(app);
        }
    }

    /// Open or close the profile card from the bar's own avatar.
    fn toggle_profile(&mut self) {
        self.close_bar_popovers_except(BarPopover::Profile);
        if !self.profile.is_open() {
            // Where the person stands with each connected service is asked
            // of the room's host; the card shows it once answered.
            self.chrome.shared.linked.refresh();
        }
        if self.profile_of.take().is_some() {
            // Someone else's card gives way to the person's own.
            self.profile.close();
            let _ = self.profile.take_choice();
        }
        let model = self.profile_model();
        self.profile
            .toggle(&model, self.top.bar().own_avatar_rect(), self.rect);
    }

    /// The page arrived with an account to link (`#?link=`): once the bar
    /// is drawn, the profile card opens on it and asks the person to
    /// confirm. Whether it opened.
    fn take_link_offer(&mut self) -> bool {
        // The card opens under the own avatar, which the bar knows once it
        // drew it, and asks in the person's name, once the room knows it.
        if !link_offer_ready(
            self.rect,
            self.top.bar().own_avatar_rect(),
            self.chrome.here.borrow().own_label(),
        ) {
            return false;
        }
        let Some(token) = self.offers.link_offer.take() else {
            return false;
        };
        self.chrome.shared.linked.offer(&token);
        if !self.profile.is_open() {
            self.toggle_profile();
        }
        true
    }

    /// The person's profile names their linked accounts and wears their
    /// picture, so the room's host is asked where they stand once the room
    /// is known, without the card open; what it answers goes to the page
    /// whenever it changed.
    fn follow_profile_links(&mut self) {
        /// How many times, and how far apart, the room is asked unopened:
        /// its records arrive a moment after the page.
        const TRIES: u32 = 6;
        const APART: std::time::Duration = std::time::Duration::from_secs(5);
        let linked = &self.chrome.shared.linked;
        let links = linked.profile_links();
        if links.is_none() && self.chrome.here.borrow().own_user().is_some() {
            let now = ui::time::now();
            let (asked, last) = self.links_asked;
            let due = last.is_none_or(|last| now.saturating_duration_since(last) >= APART);
            if asked < TRIES && due {
                linked.refresh();
                self.links_asked = (asked.saturating_add(1), Some(now));
            }
        }
        if let Some(links) = links
            && self.profile_links.as_ref() != Some(&links)
        {
            self.profile_links = Some(links.clone());
            self.requests.borrow_mut().push(HostRequest::Linked(links));
        }
    }

    /// Hand the room actions the chrome asked for to the page, which has the
    /// room's host run them, and show the linked accounts again when an
    /// answer changed them. Whether anything changed.
    fn follow_room_actions(&mut self) -> bool {
        // A waiting offer draws again until the bar is there to open it.
        let mut offered = self.take_link_offer() || self.offers.link_offer.is_some();
        if self.profile_of.is_none() {
            offered |= self
                .profile
                .follow_anchor(self.top.bar().own_avatar_rect(), self.rect);
        }
        // Before the requests are taken, so the asks it makes go out now.
        // Only the person's own card shows their linked accounts.
        self.linking.follow(
            &self.chrome.shared.linked,
            self.profile.is_open() && self.profile_of.is_none(),
            ui::time::now(),
        );
        self.follow_profile_links();
        let asked = self.chrome.shared.room_actions.take_requests();
        let opens = self.chrome.shared.room_actions.take_opens();
        let changed = offered || !asked.is_empty() || !opens.is_empty();
        self.requests
            .borrow_mut()
            .extend(asked.into_iter().map(HostRequest::RoomAction));
        self.requests
            .borrow_mut()
            .extend(opens.into_iter().map(HostRequest::Open));
        let linked = &self.chrome.shared.linked;
        let revision = linked.revision();
        if self.linked_seen.replace(revision) != revision {
            self.profile.set_linked(linked.accounts());
            return true;
        }
        changed
    }

    fn apply_launchpad(&self, action: LaunchpadAction) {
        match action {
            LaunchpadAction::Invite => self.invite(),
            LaunchpadAction::Activity => self.activity(),
            other => {
                // A card that names an action of its app does it in one
                // step, as on a native desktop: the session opens the app
                // and hands it the action.
                if let Some(target) = other.app_action()
                    && crate::desktop::known_application(target.app_id).is_some()
                {
                    self.chrome.shared.ask_app_action(target);
                } else if let Some((app, _)) = other.app_target() {
                    self.launch(app);
                }
            }
        }
    }
}

/// The launchpad's words in the page: what Tilo is and that work is kept
/// safe, and the page's own search chord.
fn welcome_copy(mac: bool) -> LaunchpadCopy {
    let t = tilo_desktop_surface::i18n::t;
    LaunchpadCopy {
        title: Some(t("browser-welcome-title")),
        subtitle: Some(t("browser-welcome-subtitle")),
        keyboard_hint: Some(t(if mac {
            "browser-welcome-hint-mac"
        } else {
            "browser-welcome-hint"
        })),
    }
}

impl Widget for BrowserShell {
    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let intents = std::mem::take(&mut *self.chrome.intents.borrow_mut());
        let mut changed = !intents.is_empty();
        for intent in intents {
            self.apply_intent(intent);
        }
        if let Some(choice) = self.profile.take_choice() {
            self.apply_profile_choice(choice);
            changed = true;
        }
        changed |= self.follow_room_actions();
        changed |= self.take_person_card_ask();
        // What a reader pressed on the pinned roster or the bar's people
        // panel, as a press is.
        for hit in self.pinned.take_pressed() {
            self.apply_pinned_hit(hit);
            changed = true;
        }
        if let Some(target) = self.pinned.take_panel_pressed()
            && let Some(hit) = self.people.hit_for_target(&target)
        {
            self.apply_people_hit(hit);
            changed = true;
        }
        if self.top.tilo_menu_mut().take_feedback() {
            self.open_feedback();
            changed = true;
        }
        if self.chrome.shared.adopt_feedback() {
            // The page holds a destination now: the Tilo menu offers it too.
            self.top.tilo_menu_mut().offer_feedback();
            changed = true;
        }
        changed |= self.follow_picker();
        changed |= self.top.update(state, path);
        changed |= self.desk.update(state, path);
        changed |= self.follow_asks();
        self.update_hints();
        // Chosen while the surface updated (search, a banner): the page
        // performs them when it reads this frame's status.
        let choices = self.handle.take_host_choices();
        changed |= !choices.is_empty();
        self.requests
            .borrow_mut()
            .extend(choices.into_iter().map(HostRequest::Action));
        changed
    }

    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &ui::automerge::ObjId) {
        for child in self.children_mut() {
            child.settle(edits, path);
        }
        // The launchpad's cards, at once.
        self.follow_asks();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        use ui::taffy::{Display, FlexDirection, Size};
        let desk = self.desk.build_layout(taffy, theme, map);
        let top = self.top.build_layout(taffy, theme, map);
        let profile = self.profile.build_layout(taffy, theme, map);
        let pinned = self.pinned.build_layout(taffy, theme, map);
        let style = Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            size: Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        taffy.widget_node(style, &[desk, top, profile, pinned], map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let inner = Point::new(self.rect.x0, self.rect.y0);
        if let [desk, top, profile, pinned] = taffy.children(node).unwrap_or_default()[..] {
            self.desk.apply_layout(taffy, desk, inner);
            self.top.apply_layout(taffy, top, inner);
            self.profile.apply_layout(taffy, profile, inner);
            self.pinned.apply_layout(taffy, pinned, inner);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let started = ui::time::now();
        self.drawn_minute.set(minute());
        self.desk.draw(scene, transform, text, theme, presence);
        // Last of the desktop, so what the bar opens below its band stays
        // above windows.
        self.top.draw(scene, transform, text, theme, presence);
        self.draw_pinned_people(scene, transform, text, theme);
        // Above the bar: a learned hint's dot settles into the avatar.
        self.draw_hints(scene, transform, text, theme);
        if self.people_open {
            self.draw_people(scene, transform, text, theme);
            self.pinned
                .set_panel(Some(self.people.accessibility_with_targets()));
        } else {
            self.people.clear();
            self.pinned.set_panel(None);
        }
        // Over everything, while picking what to report.
        self.picker.draw(scene, transform, text, theme, self.rect);
        let built = ui::time::now().saturating_duration_since(started);
        self.picker
            .frame_built(u32::try_from(built.as_micros()).unwrap_or(u32::MAX));
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        let [desk, top, profile, _] = ctx.taffy.children(ctx.node_id).unwrap_or_default()[..]
        else {
            return;
        };
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Some(Point::new(
                position.x / ctx.scale_factor,
                position.y / ctx.scale_factor,
            ));
        } else if matches!(event, WindowEvent::CursorLeft { .. }) {
            self.cursor = None;
        }
        // Picking what to report takes every event: nothing underneath
        // reacts to a pick.
        if self.picker.is_open() {
            let windows = self.pick_windows();
            let modifiers = ctx.modifiers.state();
            *ctx.handled =
                self.picker
                    .handle_event(event, self.cursor, modifiers, self.desk.area(), &windows);
            return;
        }
        self.hints.heard(event, ui::time::now());
        if self.people_event(event, ctx) {
            return;
        }
        if self.pinned_people_event(event, ctx) {
            return;
        }
        // The profile card, like the bar's menus, takes what it covers, the
        // keyboard and the press that dismisses it.
        if self.profile.is_open() {
            self.profile
                .handle_event(event, &mut ctx.reborrow(ctx.path, profile));
            if *ctx.handled {
                return;
            }
        }
        self.top
            .handle_event(event, &mut ctx.reborrow(ctx.path, top));
        if *ctx.handled {
            self.follow_asks();
        } else if !self.top.overview_open() {
            self.desk
                .handle_event(event, &mut ctx.reborrow(ctx.path, desk));
        }
    }

    /// The desktop surface's strip pan is view-only, while nothing above
    /// the strip takes the wheel first: the People popover, a menu, the
    /// overview, or the bar under the pointer (its centre cycles widgets).
    /// The surface is the area's top layer, so a wheel it takes never
    /// reaches the empty desk's launchpad beneath.
    fn view_only_scroll(&self, event: &WindowEvent) -> bool {
        !self.people_open
            && !self.people.wants_pointer_capture()
            && !self.pinned.wants_pointer_capture()
            && self.cursor.is_none_or(|point| !self.pinned.contains(point))
            && !self.top.menu_open()
            && !self.profile.is_open()
            && !self.top.overview_open()
            && self
                .cursor
                .is_none_or(|point| point.y >= f64::from(tilo_desktop::topbar::TOPBAR_HEIGHT_PX))
            // A wheel over the strip ribbon or its labels steps strips.
            && self
                .cursor
                .is_none_or(|point| !self.top.takes_wheel_at(point))
            && self.desk.surface_view_only_scroll(event)
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.desk, &self.top, &self.profile, &self.pinned]
    }

    fn visit_retained_components(
        &mut self,
        request: &mut ui::bound_dispatch::RetainedComponentRequest<'_, '_>,
    ) -> Result<(), ui::bound_dispatch::BoundDispatchError> {
        for child in self.children_mut() {
            request.visit(child)?;
        }
        Ok(())
    }

    fn set_interaction_environment(&mut self, environment: ui::InteractionEnvironment) {
        self.interaction = environment;
        for child in self.children_mut() {
            child.set_interaction_environment(environment);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![
            &mut self.desk,
            &mut self.top,
            &mut self.profile,
            &mut self.pinned,
        ]
    }

    /// The page adds no structure of its own: a change a window carries out
    /// in place stays in place, and the rest of the desktop's layout is kept.
    fn take_structure_change(&mut self) -> ui::widget::StructureChange {
        ui::widget::StructureChange::of_children(self.children_mut())
    }

    fn needs_redraw(&self) -> bool {
        self.page_needs_redraw() || self.hints.needs_animation_frame(ui::time::now())
    }

    /// The page repaints whole for anything of its own that changed; a
    /// hint that moves alone changes only what it painted, there and where
    /// it was the frame before.
    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if self.page_needs_redraw() {
            regions.push(self.get_rect());
            return;
        }
        let now = self.hints.painted_rect();
        if self.hints.needs_animation_frame(ui::time::now()) || now != self.hints_painted_before {
            regions.extend(match (self.hints_painted_before, now) {
                (Some(before), Some(now)) => Some(before.union(now)),
                (before, now) => before.or(now),
            });
        }
    }

    /// The page asks for animation frames by this, not by
    /// [`Self::needs_redraw`]: a hint drawn by the shell itself, not by a
    /// child, says here that it moves.
    fn needs_animation_frame(&self) -> bool {
        self.hints.needs_animation_frame(ui::time::now())
            || [&self.desk as &dyn Widget, &self.top, &self.profile]
                .iter()
                .any(|child| child.needs_animation_frame())
    }

    /// The earliest wake any part of the page asked for.
    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        [&self.desk as &dyn Widget, &self.top]
            .iter()
            .filter_map(|child| child.next_wake_deadline())
            .chain(
                self.people_open
                    .then(|| self.people.next_wake_deadline())
                    .flatten(),
            )
            .chain(self.pinned.next_wake_deadline())
            .chain(self.linking.due())
            .chain(self.hints.next_wake_deadline(ui::time::now()))
            .min()
    }

    fn name(&self) -> &'static str {
        "BrowserShell"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// Whether a link offer the page arrived with can open the card: the bar
/// is drawn (the card opens under the own avatar) and the room named the
/// person, since the card asks "Link Discord @name to <you>?" in their own
/// name and must never ask it of "Guest".
fn link_offer_ready(page: Rect, own_avatar: Rect, own_name: Option<&str>) -> bool {
    !page.is_zero_area()
        && !own_avatar.is_zero_area()
        && own_name.is_some_and(|name| !name.trim().is_empty())
}

/// The card of someone not here now, known by their root key `key` and the
/// name they were shown by: no status or place, nothing to follow, and
/// Message where Chat holds person-only conversations.
fn absent_person(key: &str, name: &str) -> Option<ProfileModel> {
    let key = key.to_ascii_lowercase();
    if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let (r, g, b) = ui::presence::user_color(&key);
    Some(ProfileModel {
        name: name.to_owned(),
        identity: key.clone(),
        colour: Some(ui::vello::peniko::Color::from_rgb8(r, g, b)),
        other: Some(tilo_desktop::profile_card::OtherPerson {
            id: key.clone(),
            status: None,
            place: None,
            following: false,
            can_follow: false,
            can_message: crate::desktop::known_application(CHAT_APP).is_some(),
            linked: Vec::new(),
        }),
        key: Some(key),
        ..ProfileModel::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use tilo_action_policy::Role;
    use tilo_desktop_model::IdMinter;
    use tilo_desktop_model::commands::Standing;

    const GUEST: &str = "guest";
    const OTHER_GUEST: &str = "other-guest";

    /// The public Lounge as a page holds it: one strip the room lists, a
    /// window the guest opened and one another guest opened.
    fn lounge() -> (DesktopTree, BTreeMap<u64, String>) {
        let mut minter = IdMinter::new(3);
        let tabs = ["so.tilo.notes", "so.tilo.chat"]
            .into_iter()
            .map(|app| tilo_desktop_model::Tab {
                id: minter.mint(),
                title: app.into(),
                tile: tilo_desktop_model::TileContent::LocalApp {
                    app_id: app.into(),
                    label: app.into(),
                    note: String::new(),
                },
                app_title: None,
            })
            .collect::<Vec<_>>();
        let authors = [(tabs[0].id, GUEST), (tabs[1].id, OTHER_GUEST)]
            .into_iter()
            .map(|(id, who)| (id, who.to_owned()))
            .collect();
        let tree = tilo_desktop_surface::seed_tree(&mut minter, "Lounge", tabs);
        (tree, authors)
    }

    /// What a person may change about a workspace, as its menu and the
    /// bar offer it.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    struct OfferedEdits {
        rename: bool,
        add_desk: bool,
    }

    /// What `standing` may change about `tree`'s active workspace, whose
    /// strips and windows `authors` made, as the desktop's one policy
    /// answers the menu.
    fn offered_edits(
        standing: &Standing<'_>,
        authors: &BTreeMap<u64, String>,
        tree: &DesktopTree,
    ) -> OfferedEdits {
        let Some(workspace) = tree.active_workspace() else {
            return OfferedEdits::default();
        };
        let may = |command: DesktopCommand| {
            tilo_desktop_model::commands::may_command(standing, authors, workspace, &command)
                .is_ok()
        };
        OfferedEdits {
            rename: may(DesktopCommand::RenameWorkspace {
                name: String::new(),
            }),
            add_desk: may(DesktopCommand::CreateStrip {
                name: String::new(),
            }),
        }
    }

    fn standing(person: &str, role: Option<Role>) -> Standing<'_> {
        Standing {
            person,
            role,
            layout_shared: true,
            members: None,
        }
    }

    /// The confirm a `/link` button opens asks in the person's own name:
    /// it waits for the room to name them rather than ask it of "Guest".
    #[test]
    fn a_link_offer_waits_for_the_persons_own_name() {
        let page = Rect::new(0.0, 0.0, 1440.0, 900.0);
        let avatar = Rect::new(1380.0, 4.0, 1404.0, 28.0);
        assert!(!link_offer_ready(page, avatar, None), "not named yet");
        assert!(!link_offer_ready(page, avatar, Some("  ")));
        assert!(
            !link_offer_ready(page, Rect::ZERO, Some("Quinn Davis")),
            "no bar yet"
        );
        assert!(!link_offer_ready(Rect::ZERO, avatar, Some("Quinn Davis")));
        assert!(link_offer_ready(page, avatar, Some("Quinn Davis")));
    }

    #[test]
    fn the_workspace_menu_offers_rename_and_new_desk_only_to_who_governs() {
        let (tree, authors) = lounge();
        let offers = |person, role| offered_edits(&standing(person, role), &authors, &tree);
        let none = OfferedEdits {
            rename: false,
            add_desk: false,
        };
        let all = OfferedEdits {
            rename: true,
            add_desk: true,
        };
        // A guest and a member of the public workspace write in it, and
        // leave its name and desks to its owner and moderators.
        assert_eq!(offers(GUEST, Some(Role::Editor)), none);
        assert_eq!(offers("member", Some(Role::Editor)), none);
        assert_eq!(offers("moderator", Some(Role::Moderator)), all);
        assert_eq!(offers("owner", Some(Role::Owner)), all);
        // Until the room has answered, nothing is offered to take back.
        assert_eq!(offers(GUEST, None), none);
        let own = crate::desktop::Standing::default();
        assert_eq!(offered_edits(&own.of(GUEST), &authors, &tree), all);
    }

    #[test]
    fn a_guest_is_offered_no_close_on_another_guests_window() {
        let (tree, authors) = lounge();
        let shared = crate::desktop::Shared::default();
        shared.authors.borrow_mut().clone_from(&authors);
        let mut person = |id: &str| (id.to_owned(), (1, 2, 3));
        let theirs = authors
            .iter()
            .find(|(_, who)| *who == OTHER_GUEST)
            .map(|(id, _)| *id)
            .unwrap();
        for role in [Role::Editor, Role::Moderator] {
            shared.standing.set(Some(role));
            let held = shared.holders(&tree, GUEST, "Lounge", &mut person);
            if role == Role::Moderator {
                assert!(held.is_empty(), "a moderator closes anyone's window");
            } else {
                // Their own window offers its close; the other guest's
                // names who opened it instead.
                assert_eq!(held.keys().copied().collect::<Vec<_>>(), vec![theirs]);
                assert_eq!(held[&theirs].name, OTHER_GUEST);
            }
        }
    }

    /// A window whose opener has left the room holds nobody: once the
    /// room's members are heard without them, it offers its close.
    #[test]
    fn a_window_of_someone_who_left_offers_its_close() {
        let (tree, authors) = lounge();
        let shared = crate::desktop::Shared::default();
        shared.authors.borrow_mut().clone_from(&authors);
        shared.standing.set(Some(Role::Editor));
        let mut person = |id: &str| (id.to_owned(), (1, 2, 3));
        shared
            .members
            .replace(Some([GUEST, OTHER_GUEST].map(str::to_owned).into()));
        assert!(
            !shared
                .holders(&tree, GUEST, "Lounge", &mut person)
                .is_empty(),
            "the other guest is still here"
        );
        shared.members.replace(Some([GUEST.to_owned()].into()));
        let held = shared.holders(&tree, GUEST, "Lounge", &mut person);
        assert!(
            held.values().all(|holder| holder.name != OTHER_GUEST),
            "the other guest left: {held:?}"
        );
    }

    /// A public room shares no member's layout: once it said so, every
    /// window on the visitor's desktop is theirs to close (nothing is ever
    /// undone), and the workspace's name and desks are offered to nobody.
    #[test]
    fn in_a_public_room_the_desktop_is_the_visitors_own() {
        let (tree, authors) = lounge();
        let shared = crate::desktop::Shared::default();
        shared.authors.borrow_mut().clone_from(&authors);
        shared.own_layout.set(true);
        let mut person = |id: &str| (id.to_owned(), (1, 2, 3));
        shared.standing.set(Some(Role::Editor));
        assert!(
            shared
                .holders(&tree, GUEST, "Lounge", &mut person)
                .is_empty()
        );
        for role in [Role::Editor, Role::Moderator] {
            shared.standing.set(Some(role));
            let edits = offered_edits(&shared.standing_of(GUEST, None), &authors, &tree);
            assert_eq!(edits, OfferedEdits::default(), "{role:?}");
        }
    }
}
