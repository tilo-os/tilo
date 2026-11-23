use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, Style, TaffyTree};
use tilo_desktop_model::commands::DesktopCommand;
use tilo_desktop_model::{Layout, Strip, StripId, TabId, Workspace, WorkspaceId};
use tilo_desktop_surface::{DesktopHandle, HostAction, HostActionSource, NotificationLayer};
use tilo_resource::Effect;
use tilo_session_core::desktop::names;
use tilo_session_core::desktop::navigation::{
    ActivityView, ChromeEffect, Destination, LayoutEffect,
};
use ui::presence::PresenceCtx;
use ui::shortcut_table::{FixedAction, ShortcutAction};
use ui::widget::DrawChild;
use ui::widget::{AppModel, EventCtx, ModelPath, TaffyTreeExt as _, Widget};
use ui::{TextRenderer, Theme};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};

mod overview;
mod shortcut_actions;
#[cfg(test)]
mod tests;

use super::{PlainOverview, column, fill, grow, wake_deadline};
use crate::about::AboutSheet;
use crate::chrome::{ChromeIntent, TopBarPresence};
use crate::control_hints::{ControlHints, HintFrame, HintsFeed};
use crate::display_identify::DisplayIdentify;
use crate::emoji::EmojiLink;
use crate::host_contract::{
    DesktopAsk, DesktopRules, JOIN_WORKSPACE, NEW_WORKSPACE, WORKSPACE_SETTINGS,
};
use crate::overlays::{Overlays, Part};
use crate::overview::workspace_cards;
use crate::overview_panel::{OverviewCards, OverviewPanel, OverviewPick};
use crate::search_overlay::SearchOverlay;
use crate::shortcuts::{app_window_fixed_action, page_fixed_action, page_shortcut_action};
use crate::strip_navigator::{StripFrame, StripNavigation, StripNavigator, Viewer};
use crate::strip_switcher::Command as StripCommand;
use crate::super_peek::SuperPeek;
use crate::topbar::{Placement, Reveals, TOPBAR_HEIGHT_PX, TopBarChrome};
use crate::verbs::chrome_resource;
use crate::window_flight::WindowFlight;
use crate::window_switcher::{Switcher, SwitcherAsk};
use crate::workspace_menu::{
    MenuPlace, WorkspaceAction, WorkspaceEntry, WorkspaceMenu, WorkspaceMenuChoice,
    WorkspaceMenuModel,
};

/// Where a page host is shown: in a browser tab, or installed as an app window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PageWindow {
    Tab,
    App,
}

/// What lies above the windows: the top bar, the strip ribbon and rail,
/// the overview panel and the menus the bar opens; and the desktop's
/// chords.
pub struct DesktopChrome {
    bar: TopBarChrome,
    /// The bars of the other displays, each on its own: a display besides the one in use carries
    /// the compact bar.
    compact: Vec<TopBarChrome>,
    /// What the shell says in passing ("Link copied", "Notes closed" with its Undo), over the
    /// windows on the display the person works on.
    notice: ui::widget::toast::NoticeLayer,
    /// Where the bars stood at the bar's source last reading, and the snapshot revision it was read
    /// at.
    placement: Placement,
    bars_revision: u64,
    overview: OverviewPanel,
    about: AboutSheet,
    workspace_menu: WorkspaceMenu,
    /// What the person can do to the window in front, at its window. Where the person is in the
    /// workspace (the strip ribbon grown into the strip switcher, and the window rail).
    navigator: StripNavigator,
    /// The emoji card: Super+. opens it at the caret of the field the person types into, and every
    /// pick is written into that field by the host.
    emoji: ui::widget::EmojiPicker,
    /// Where the card's next opening goes, and what the person picked.
    emoji_link: EmojiLink,
    /// The theme the chrome last drew in: what a card opened between two frames is sized and placed
    /// by.
    theme: ui::Theme,
    /// Where the navigator's ribbon and rail stand and what they report.
    navigator_frame: StripFrame,
    /// What the navigator last read: the navigation and who-is-here revisions, whether the overview
    /// covered the desktop, and the content area.
    navigator_seen: Option<(u64, u64, bool, Rect, bool)>,
    /// The tree revision the open overview shows.
    overview_revision: u64,
    /// What the overview of the person's Tilo shows below their workspaces, as the embedding
    /// hosts it ([`Self::set_tilo`]).
    tilo: crate::overview_tilo::TiloOverview,
    /// Whether the overview is open, shared with the host whose clock flies its windows in and
    /// out and whose surfaces open and close it ([`Self::follow_overview`]).
    overview_open: Option<Rc<Cell<bool>>>,
    /// What the overview's panel last said of itself.
    overview_said: bool,
    /// Whether the bar last drew its Tilo mark pressed (the overview open).
    menu_shown: Cell<bool>,
    handle: DesktopHandle,
    generation: String,
    rules: Rc<dyn DesktopRules>,
    /// The shell's shortcut table, shared with the session: the chords the shell answers and the
    /// ones its rows show are one table.
    shortcuts: crate::shortcuts::Shared,
    /// Chords read the host's stand-in for Super as a Mac's.
    mac: bool,
    /// Where a page host is shown: an app window is handed Ctrl+W, Ctrl+T and
    /// Ctrl+N, which a tab's browser keeps ([`app_window_fixed_action`]).
    page_window: PageWindow,
    /// The pointer, in logical pixels.
    cursor: Option<Point>,
    /// The area under the bar, as last laid out.
    content: Rect,
    rect: Rect,
    asks: Rc<RefCell<Vec<DesktopAsk>>>,
    /// What a host's bar source raised, carried out on the next update.
    intents: Rc<RefCell<Vec<ChromeIntent>>>,
    /// The surfaces the chrome draws, as the session's overlay stack holds them: which are up, what
    /// kind each is, and which holds the keyboard.
    overlays: Overlays,
    /// Whether the reader's focus sits on the chrome, as the focus framework put it there; cleared
    /// as soon as no surface the chrome draws holds the keyboard, which the stack says.
    focused: bool,
    /// When each display's bar shows over the window covering it: shared with the shell's band, so
    /// the bar's room and the bars it is in agree.
    reveals: Rc<RefCell<Reveals>>,
    /// Which screen is which, said on each display for a moment after Settings asks.
    identify: DisplayIdentify,
    /// Where the displays are the identify cards stand on, each by its monitor, as last read from
    /// the shell.
    identify_displays: Vec<(String, Rect)>,
    /// The Identify request answered, so one press shows the cards once.
    answered: Option<u64>,
    /// The shell's search palette, over the windows the shell draws: it is the one surface here that
    /// must cover a window in front, so the chrome draws it rather than the shell under them.
    search: SearchOverlay,
    /// The shell's notifications, over its windows for the same reason: the centre, and the banners
    /// where the host shows none of its own. `None` on a shell without notifications.
    notices: Option<NotificationLayer>,
    /// Alt+Tab's panel, as the shell's walk last said what it shows.
    switcher: Switcher,
    /// The keys a held Super reaches, as the session says them.
    peek: SuperPeek,
    /// The desktop's control hints, what this frame shows them, and the person's record as the
    /// settings last held it.
    hints: ControlHints,
    hints_frame: HintFrame,
    /// What the host lends the hints: their record and the reasons it alone knows, and what the
    /// person learned since it last looked.
    hints_feed: Option<HintsFeed>,
    /// The feed's record as the hints last folded it in.
    hints_revision: u64,
    /// How the person points, as the host last learned it.
    interaction: ui::InteractionEnvironment,
    /// What the verb just carried out answers its caller, when it answers anything: a branch's new
    /// workspace.
    output: Option<serde_json::Value>,
}

/// The overlays the chrome paints itself, as opposed to the retained widgets it hands to layout:
/// what a screen reader only reaches if the chrome puts them in its own tree.
#[derive(Clone, Copy)]
enum PaintedOverlay {
    /// The strip ribbon's labels, open for the keyboard or under the pointer.
    StripLabels,
    /// The emoji card.
    Emoji,
    /// Which screen is which, after Settings asked.
    Identify,
    /// The search palette.
    Search,
    /// Alt+Tab's panel of windows.
    Switcher,
    /// The keys a held Super reaches.
    Peek,
}

impl PaintedOverlay {
    /// Every painted overlay, bottom to top.
    const ALL: [Self; 6] = [
        Self::StripLabels,
        Self::Emoji,
        Self::Identify,
        Self::Search,
        Self::Switcher,
        Self::Peek,
    ];

    /// The key its node keeps in the chrome's tree, so the id survives the other overlays opening
    /// and closing.
    fn key(self) -> u64 {
        ui::a11y::hash_key(&match self {
            Self::StripLabels => "chrome-strip-labels",
            Self::Emoji => "chrome-emoji-card",
            Self::Identify => "chrome-display-identify",
            Self::Search => "chrome-search-palette",
            Self::Switcher => "chrome-window-switcher",
            Self::Peek => "chrome-super-peek",
        })
    }

    /// Whether `chrome` has it on screen. The strip column is published at rest too: the New strip
    /// button that stands at its foot is one of the navigator's own controls, not only the labels.
    fn is_open(self, chrome: &DesktopChrome) -> bool {
        match self {
            Self::StripLabels => {
                chrome.navigator.is_open() || chrome.navigator_frame.new_strip_offered
            }
            Self::Emoji => chrome.emoji.is_open(),
            Self::Identify => chrome.identify.is_up(ui::time::now()),
            Self::Search => chrome.search.is_open(),
            Self::Switcher => chrome.switcher.is_up(),
            Self::Peek => chrome.peek.is_up(),
        }
    }
}

impl DesktopChrome {
    /// The chrome over the desktop `handle` drives: `bar` (its source
    /// answering the clock, the person and their notifications) and
    /// `about`, under `rules`. Commands it sends carry `generation`;
    /// `mac` reads chords with a Mac's keys.
    #[must_use]
    pub fn new(
        handle: DesktopHandle,
        generation: String,
        rules: Rc<dyn DesktopRules>,
        bar: TopBarChrome,
        about: AboutSheet,
        mac: bool,
    ) -> Self {
        let search = SearchOverlay::new(handle.clone());
        let notices = handle.notifications().map(NotificationLayer::new);
        Self {
            bar,
            compact: Vec::new(),
            notice: ui::widget::toast::NoticeLayer::new(handle.notice(), {
                let handle = handle.clone();
                move |action| {
                    handle.press_notice(action);
                }
            }),
            placement: Placement::default(),
            bars_revision: u64::MAX,
            overview: OverviewPanel::new(),
            about,
            workspace_menu: WorkspaceMenu::new(),
            navigator: StripNavigator::default(),
            emoji: ui::widget::EmojiPicker::sheet(),
            emoji_link: EmojiLink::default(),
            theme: ui::Theme::dark(),
            navigator_frame: StripFrame::empty(Rect::ZERO),
            navigator_seen: None,
            overview_revision: 0,
            tilo: crate::overview_tilo::TiloOverview::default(),
            overview_open: None,
            overview_said: false,
            menu_shown: Cell::new(false),
            handle,
            generation,
            rules,
            shortcuts: crate::shortcuts::Shared::default(),
            mac,
            page_window: PageWindow::Tab,
            cursor: None,
            content: Rect::ZERO,
            rect: Rect::ZERO,
            asks: Rc::default(),
            intents: Rc::default(),
            overlays: Overlays::new(),
            focused: false,
            reveals: Rc::default(),
            identify: DisplayIdentify::default(),
            identify_displays: Vec::new(),
            answered: None,
            output: None,
            search,
            notices,
            switcher: Switcher::default(),
            peek: SuperPeek::default(),
            hints: ControlHints::new(ui::control_hints::Learned::default()),
            hints_frame: HintFrame::default(),
            hints_feed: None,
            hints_revision: 0,
            interaction: ui::InteractionEnvironment::default(),
        }
    }

    /// Follow the shell's walk with `switcher` : what Alt+Tab's panel shows, and the windows whose
    /// pictures stand in its wells.
    pub fn set_switcher(&mut self, switcher: Switcher) {
        self.switcher = switcher;
    }

    /// Draw the keys a held Super reaches as `peek` says them.
    pub fn set_super_peek(&mut self, peek: SuperPeek) {
        self.peek = peek;
    }

    /// Follow the bars' reveal with `reveals` , which the shell's band reads too: when each bar
    /// shows over the window covering its display.
    pub fn set_reveals(&mut self, reveals: Rc<RefCell<Reveals>>) {
        self.reveals = reveals;
    }

    /// The surfaces the chrome draws, as the session's stack holds them.
    #[must_use]
    pub const fn overlays(&self) -> &Overlays {
        &self.overlays
    }

    /// Publish the chrome's surfaces to `overlays` : the session's stack, which says which of them
    /// holds the keyboard and what a press outside them closes.
    pub fn set_overlays(&mut self, overlays: Overlays) {
        self.overlays = overlays;
        self.publish_overlays();
    }

    /// Keep the emoji card's openings and picks through `link` : the session says where the focused
    /// field's caret is and writes each pick into that field.
    pub fn set_emoji(&mut self, link: EmojiLink) {
        self.emoji_link = link;
    }

    /// The emoji card, as the host sees it: open or not, where it stands and what the person
    /// picked.
    #[must_use]
    pub const fn emoji(&self) -> &ui::widget::EmojiPicker {
        &self.emoji
    }

    /// Use `shortcuts` for the desktop's chords (the person's bindings).
    pub fn set_shortcuts(&mut self, shortcuts: crate::shortcuts::Shared) {
        self.shortcuts = shortcuts;
    }

    /// Lend the hints the host's side of them: the person's record, the reasons only the host
    /// knows, and a place for what the person learned.
    pub fn set_hints_feed(&mut self, feed: HintsFeed) {
        self.hints_feed = Some(feed);
    }

    /// The desktop's control hints, as this chrome drives them.
    #[must_use]
    pub fn hints(&self) -> &ControlHints {
        &self.hints
    }

    /// Take the host's record and reasons, and advance the hints to `now`.
    fn advance_hints(&mut self, now: ui::time::Instant) {
        if let Some(feed) = &self.hints_feed {
            let (revision, record) = feed.record();
            if revision != self.hints_revision {
                self.hints_revision = revision;
                self.hints.merge(&record, now);
            }
        }
        let frame = self.hint_frame();
        self.hints.update(&frame, now);
        self.handle.read_hint_ink(self.hints.reads_ink(now));
        self.hints_frame = frame;
        if let (Some(feed), Some(learned)) = (&self.hints_feed, self.hints.take_changed()) {
            feed.note_learned(learned);
        }
    }

    /// What this frame shows the hints.
    fn hint_frame(&self) -> HintFrame {
        // The desktop under the bar: where cards may stand.
        let area = if self.content.is_zero_area() {
            Rect::new(
                self.rect.x0,
                self.rect.y0 + f64::from(TOPBAR_HEIGHT_PX),
                self.rect.x1,
                self.rect.y1,
            )
        } else {
            self.content
        };
        // The keys that open search here, as the host spells them for this keyboard.
        let search_keys = self
            .hints_feed
            .as_ref()
            .map(HintsFeed::search_keys)
            .unwrap_or_default();
        let avatar = self.bar.own_avatar_rect();
        HintFrame {
            // The overview covers the shell: none of its windows' bars or edges is on screen, so no
            // hint anchored to them stands, and one showing leaves.
            row: if self.overview.is_open() {
                ui::widget::HintPlaces::default()
            } else {
                self.handle.hint_places()
            },
            area,
            search_keys,
            avatar: (!avatar.is_zero_area()).then_some(avatar),
            colour: self.bar.own_chip().map(|own| own.rgb),
            quiet: self.hints_quiet(),
            presented: self.hints_presented(),
            touch: self.interaction.pointer_precision == ui::PointerPrecision::Coarse,
        }
    }

    /// Whether the person has reached the desktop past setup: hints that wait for a while count
    /// from the first frame that says so.
    fn hints_presented(&self) -> bool {
        self.hints_feed
            .as_ref()
            .is_some_and(|feed| feed.reasons().1)
    }

    /// Why no hint may begin now: the host's own reason, else something of the chrome holding the
    /// person's attention (search, a menu, the overview, the emoji card, the switcher).
    fn hints_quiet(&self) -> Option<ui::control_hints::Quiet> {
        if let Some(quiet) = self.hints_feed.as_ref().and_then(|feed| feed.reasons().0) {
            return Some(quiet);
        }
        let busy = self.menu_open()
            || self.overview.is_open()
            || self.emoji.is_open()
            || self.switcher.is_up()
            || self.overlays.target().part().is_some()
            || !self.placement.fullscreen.is_empty();
        busy.then_some(ui::control_hints::Quiet::Busy)
    }

    /// The top bar.
    #[must_use]
    pub const fn bar(&self) -> &TopBarChrome {
        &self.bar
    }

    /// The top bar, to open what it anchors.
    pub fn bar_mut(&mut self) -> &mut TopBarChrome {
        &mut self.bar
    }

    /// Where every display's bar stands, as the bar's source last said.
    #[must_use]
    pub const fn placement(&self) -> &Placement {
        &self.placement
    }

    /// Every bar the chrome shows: the full one on the display in use,
    /// then the compact one of each other display in the placement's
    /// order, with the display each stands on (`None` for the full bar).
    #[must_use]
    pub fn bars(&self) -> Vec<(Option<u32>, &TopBarChrome)> {
        std::iter::once((self.bar.display(), &self.bar))
            .chain(self.compact.iter().map(|bar| (bar.display(), bar)))
            .collect()
    }

    /// Whether the overview is open over the shell.
    #[must_use]
    pub fn overview_open(&self) -> bool {
        self.overview.is_open()
    }

    /// Whether a menu the chrome opened is up.
    #[must_use]
    pub fn menu_open(&self) -> bool {
        self.workspace_menu.is_open()
    }

    /// Say what the chrome's own surfaces are to the stack: which are up and which are gone.
    /// Reached from everywhere they open, close or take a key, and by the host once the actions a
    /// pass ran are carried out, so the stack's answer — which of them holds the keyboard, what a
    /// press outside one closes — is what the chrome shows in the frame that pass draws.
    pub fn publish_overlays(&mut self) {
        // Where the chrome's own menus paint themselves, for the stack to judge a press that misses
        // them: a press on the shell reaches the shell, not the chrome over it.
        self.overlays
            .stands(Part::WorkspaceMenu, self.workspace_menu.panel_rect());
        let centre = self.notices.as_ref().and_then(NotificationLayer::panel);
        self.overlays.stands(Part::Centre, centre);
        for (part, open) in [
            (Part::Emoji, self.emoji.is_open()),
            (Part::WorkspaceMenu, self.workspace_menu.is_open()),
            (Part::Overview, self.overview.is_open()),
            (Part::Strips, self.navigator.takes_keys()),
            (Part::Search, self.search.is_open()),
            (Part::About, self.about.is_about_open()),
            (Part::Centre, centre.is_some()),
        ] {
            // The stack took it down — a press outside it, another surface opening — and the widget
            // has yet to follow: what it still shows is the state being left behind, not an opening
            // to raise again.
            if !self.overlays.is_open(part) && self.overlays.awaiting_close(part) {
                continue;
            }
            if open == self.overlays.is_open(part) {
                continue;
            }
            if open {
                self.overlays.open(part, None);
            } else {
                self.overlays.close(part);
            }
        }
        if !self.is_focusable() {
            self.focused = false;
        }
        self.follow_the_stack();
    }

    /// Take down the widgets whose surface the stack took down.
    /// The stack is the one log of what stands over the shell: when another surface comes up —
    /// the person opened it, the session did, or Escape took one away — the part closes there, and
    /// the widget that draws it closes here so the two never disagree about what the person sees.
    /// Each closing is reported back to the stack, which is how it hears the widget followed
    /// ([`Overlays::close`]): what it still shows until then is the state the stack left behind,
    /// and the widget opening again after that is a new surface to publish, not that state coming
    /// back.
    fn follow_the_stack(&mut self) {
        if self.emoji.is_open() && !self.overlays.is_open(Part::Emoji) {
            self.emoji.close();
            self.overlays.close(Part::Emoji);
        }
        if self.workspace_menu.is_open() && !self.overlays.is_open(Part::WorkspaceMenu) {
            self.workspace_menu.close();
            self.overlays.close(Part::WorkspaceMenu);
        }
        if self.navigator.takes_keys() && !self.overlays.is_open(Part::Strips) {
            self.navigator.close();
            self.overlays.close(Part::Strips);
        }
        if self.search.is_open() && !self.overlays.is_open(Part::Search) {
            self.search.close();
            self.overlays.close(Part::Search);
        }
        if self.about.is_about_open() && !self.overlays.is_open(Part::About) {
            self.about.set_about_open(false);
            self.overlays.close(Part::About);
        }
        if let Some(notices) = self.notices.as_mut()
            && notices.center_open()
            && !self.overlays.is_open(Part::Centre)
        {
            notices.set_center_open(false);
            self.overlays.close(Part::Centre);
        }
    }

    /// Open or close the notification centre as the person last asked (the bell, a chord, a
    /// verb), and stand it where the stack places it: under the bar's trailing end, as quick
    /// settings stands. Whether it opened or closed.
    fn follow_notices(&mut self) -> bool {
        let fallback =
            crate::overlays::place_on(self.rect, Part::Centre.anchor(), Part::Centre.size())
                .map(|placement| placement.room);
        let place = self
            .overlays
            .place(Part::Centre)
            .map(|placement| placement.room)
            .or(fallback);
        let Some(notices) = self.notices.as_mut() else {
            return false;
        };
        if place.is_some() {
            notices.set_place(place);
        }
        let asked = notices.notifications().take_centre_ask();
        if let Some(open) = asked {
            notices.set_center_open(open);
        }
        asked.is_some()
    }

    /// The notifications take what they cover, the keyboard while the centre is up, and the press
    /// that dismisses it.
    fn notices_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>, node: Option<NodeId>) {
        if let (false, Some(layer), Some(node)) = (*ctx.handled, self.notices.as_mut(), node) {
            layer.handle_event(event, &mut ctx.reborrow(ctx.path, node));
        }
    }

    /// The node the notifications were laid out at among the chrome's children after search, and
    /// the other displays' bars after it.
    fn notices_node(laid_out: bool, rest: &[NodeId]) -> (Option<NodeId>, &[NodeId]) {
        match (laid_out, rest) {
            (true, [node, compact @ ..]) => (Some(*node), compact),
            _ => (None, rest),
        }
    }

    /// The chrome's own accessibility node: the group over the shell that
    /// carries what the chrome paints itself. The retained parts (the bars,
    /// the overview, the menus) are children widgets, which the host's walk
    /// exports on its own.
    fn accessibility_group(&self) -> ui::a11y::AccessNode {
        let holds = self.is_focusable();
        ui::a11y::AccessNode::new(ui::a11y::Role::Group).with_state(
            ui::a11y::AccessState::new()
                .focusable(holds)
                .focused(self.focused),
        )
    }

    /// The painted overlays on screen now, bottom to top — the order a reader meets them in.
    fn painted_overlays(&self) -> Vec<PaintedOverlay> {
        PaintedOverlay::ALL
            .into_iter()
            .filter(|overlay| overlay.is_open(self))
            .collect()
    }

    /// What one painted overlay says to assistive technology, while it is up.
    fn overlay_accessibility(&self, overlay: PaintedOverlay) -> Option<ui::a11y::AccessNode> {
        match overlay {
            PaintedOverlay::StripLabels => self.navigator.accessibility(&self.navigator_frame),
            PaintedOverlay::Emoji => self.emoji.accessibility(),
            PaintedOverlay::Identify => self.identify.accessibility(ui::time::now()),
            PaintedOverlay::Search => self.search.accessibility(),
            PaintedOverlay::Switcher => self.switcher.accessibility(),
            PaintedOverlay::Peek => self.peek.accessibility(),
        }
    }

    /// Open the emoji picker at the caret of the field the person types into,
    /// centred on the display they are working on when that field reported
    /// none. What is picked goes to the host, which writes it into that
    /// field.
    pub fn open_emoji(&mut self) {
        let place = self.emoji_link.place().unwrap_or_default();
        let viewport = place
            .viewport
            .or_else(|| self.working_display())
            .unwrap_or(self.rect);
        self.emoji.open_at(place.caret, viewport, &self.theme);
        self.publish_overlays();
    }

    /// The display the person is working on, while the shell spans several: where a card without a
    /// caret is centred.
    fn working_display(&self) -> Option<Rect> {
        tilo_session_core::desktop::working_display(&self.rules.tree())
    }

    /// Where the overview stands while the shell spans several displays: on the one the person works
    /// on, under its bar, never across the border.
    fn overview_area(&self) -> Option<Rect> {
        self.working_display().map(|display| Rect {
            y0: display.y0 + f64::from(TOPBAR_HEIGHT_PX),
            ..display
        })
    }

    /// Whether `event` is the pointer's, over the shown bar: it moves there, or presses there.
    fn pointer_on_bar(&self, event: &WindowEvent) -> bool {
        matches!(
            event,
            WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. }
        ) && self.bar.presence() != TopBarPresence::Hidden
            && self
                .cursor
                .is_some_and(|point| self.bar.get_rect().contains(point))
    }

    /// The search palette stands over everything, a window in front included: while it is up,
    /// every key and press is its, and its own chord or a press off the card leaves search. While
    /// it is down its chord opens it, before a menu or the bar takes that key. The bar stands clear
    /// of its scrim and stays live: the pointer over it is the bar's, and a press there leaves
    /// search and does what it was pressed for, the workspace's menu or the person's own card.
    /// Whether search took `event`.
    fn search_takes(
        &mut self,
        event: &WindowEvent,
        ctx: &mut EventCtx<'_>,
        search: NodeId,
    ) -> bool {
        if self.search.is_open() && self.pointer_on_bar(event) {
            if matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                }
            ) {
                self.search.close();
            }
            return false;
        }
        self.search
            .handle_event(event, &mut ctx.reborrow(ctx.path, search));
        *ctx.handled
    }

    /// While Alt+Tab's panel is up it is what a press reaches: the pointer chooses a card on sight,
    /// a press on one goes to it, and a press off the panel ends the walk instead of reaching the
    /// window behind it.
    fn walk_the_pointer(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        self.switcher_event(event);
        *ctx.handled = true;
        self.publish_overlays();
    }

    /// What the pointer does to the walk while the switcher's panel is up: the card under it is
    /// chosen on sight, a press on one goes to it, and a press off the panel ends the walk where it
    /// stood.
    fn switcher_event(&self, event: &WindowEvent) {
        match event {
            WindowEvent::CursorMoved { .. } => {
                let card = self.cursor.and_then(|cursor| self.switcher.card_at(cursor));
                if card == self.switcher.hover() {
                    return;
                }
                self.switcher.set_hover(card);
                if let Some(at) = card {
                    self.ask(DesktopAsk::Switcher(SwitcherAsk::Hover(at)));
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let ask = match self.cursor {
                    Some(cursor) => match self.switcher.card_at(cursor) {
                        Some(at) => Some(SwitcherAsk::Choose(at)),
                        // The panel's own surface is no choice, and no press on the shell either.
                        None if self.switcher.on_panel(cursor) => None,
                        None => Some(SwitcherAsk::Dismiss),
                    },
                    None => Some(SwitcherAsk::Dismiss),
                };
                if let Some(ask) = ask {
                    self.ask(DesktopAsk::Switcher(ask));
                }
            }
            _ => {}
        }
    }

    /// Close the emoji picker without inserting anything.
    pub fn close_emoji(&mut self) {
        self.emoji.close();
        self.publish_overlays();
    }

    /// Super+. opens the picker, and closes it again while it is up.
    pub fn toggle_emoji(&mut self) {
        if self.emoji.is_open() {
            self.close_emoji();
        } else {
            self.open_emoji();
        }
    }

    /// Hand the host the emoji the person picked since the card last ran, for it to write into the
    /// field they were typing into.
    fn note_emoji_picks(&mut self) {
        for happened in self.emoji.take_events() {
            if let ui::widget::PickerEvent::Picked(text) = happened {
                self.emoji_link.note_picked(text);
            }
        }
    }

    /// Whether a wheel at `point` steps strips (over the ribbon or its labels) rather than panning
    /// the shell.
    #[must_use]
    pub fn takes_wheel_at(&self, point: Point) -> bool {
        self.navigator.takes_wheel_at(&self.navigator_frame, point)
    }

    /// What the person asked of the host since the last call.
    pub fn take_asks(&mut self) -> Vec<DesktopAsk> {
        std::mem::take(&mut *self.asks.borrow_mut())
    }

    /// Queue `ask` for the host, and ask it for the pass that answers it
    /// ([`super::ask`]): the bar's choices are carried out on the host's next pass, not whenever it
    /// happens to draw next.
    fn ask(&self, asked: DesktopAsk) {
        super::ask(&self.handle, &self.asks, asked);
    }

    /// Put the chrome's asks in `asks` , shared with a host that reads them outside the widget tree
    /// the chrome is drawn in.
    pub fn set_asks(&mut self, asks: Rc<RefCell<Vec<DesktopAsk>>>) {
        self.asks = asks;
    }

    /// Take the intents a host's bar source raises from `intents`,
    /// shared with that source.
    pub fn set_intents(&mut self, intents: Rc<RefCell<Vec<ChromeIntent>>>) {
        self.intents = intents;
    }

    /// Have the overview's windows drawn by the host through `flight` , their pictures resting
    /// where the panel draws them — flying between the shell and the overview on the clock the host
    /// keeps there; `open` is whether it is open, which the host's surfaces set to open or close
    /// it and the panel sets as it moves itself.
    pub fn set_overview(&mut self, flight: WindowFlight, open: Rc<Cell<bool>>) {
        self.overview.set_flight(flight);
        self.overview_open = Some(open);
    }

    /// The ground the overview stands on is `wallpaper` , the shell's one source: the same picture
    /// the backdrop under it shows, blurred, rather than a copy the chrome keeps.
    pub fn set_wallpaper(&mut self, wallpaper: ui::backdrop::wallpaper::Source) {
        self.overview.set_wallpaper(wallpaper);
    }

    /// Close the menus the chrome opened: the host opens a popover of its own, one at a time.
    pub fn close_menus(&mut self) {
        self.workspace_menu.close();
    }

    /// Say what became of an action the person asked for, and say nothing when it went through.
    fn report(&self, done: Result<(), String>) {
        match done {
            Ok(()) => {}
            Err(refusal) => self.handle.acknowledge(refusal),
        }
    }

    fn dispatch(&self, verb: impl Into<tilo_session_core::desktop::navigation::Verb>) {
        if let Err(error) = self.handle.dispatch(verb) {
            tracing::warn!(%error, "desktop chrome");
        }
    }

    /// History of the active workspace, showing only what `author` did when one is named.
    pub fn activity_by(&self, author: Option<String>) {
        let workspace = self
            .handle
            .read(|tree| tree.active_workspace().map(|w| w.id().to_string()));
        if let Some(workspace) = workspace {
            self.dispatch(ChromeEffect::Activity {
                workspace,
                view: ActivityView::CatchUp,
                author,
            });
        }
    }

    /// Carry out what the bar asked for; what only the host answers is asked of it
    /// ([`DesktopAsk::Intent`]).
    pub fn apply_intent(&mut self, intent: ChromeIntent) {
        match intent {
            ChromeIntent::SwitchWorkspace(delta) => {
                let next = self.handle.read(|tree| {
                    let index = tree.workspaces.iter().position(|w| w.id() == tree.active)?;
                    let next = index.checked_add_signed(isize::try_from(delta).ok()?)?;
                    tree.workspaces.get(next).map(|w| w.id().to_string())
                });
                match next {
                    Some(workspace) => self.dispatch(ChromeEffect::SwitchWorkspace {
                        workspace: Some(workspace),
                        delta: None,
                    }),
                    // Stepping past the end says which end it was.
                    None if delta > 0 => self
                        .handle
                        .acknowledge(crate::i18n::t("shell-last-workspace")),
                    None if delta < 0 => {
                        self.handle
                            .acknowledge(crate::i18n::t("shell-first-workspace"));
                    }
                    None => {}
                }
            }
            ChromeIntent::ShowActivity(_) => self.activity_by(None),
            // The window covering the display shows no bar of its own: a secondary press on the
            // desktop's bar over it opens that window's menu where the person pressed.
            ChromeIntent::CoveringWindowMenu { at: (x, y) } => {
                self.handle
                    .ask_covering_window_menu(Point::new(f64::from(x), f64::from(y)));
            }
            ChromeIntent::ToggleWorkspaceMenu { at } => {
                // The press that took it down is still being delivered, and the bar answers it: the
                // same press does not raise it again. A secondary press elsewhere asks for the menu
                // there, which is the point it names, not this one.
                if at.is_none() && self.overlays.dismissed_recently(Part::WorkspaceMenu) {
                    return;
                }
                let model = self.workspace_menu_model(if at.is_some() {
                    MenuPlace::Shell
                } else {
                    MenuPlace::Bar
                });
                match at {
                    Some((x, y)) => self.workspace_menu.open_at(
                        &model,
                        Point::new(f64::from(x), f64::from(y)),
                        self.rect,
                    ),
                    None => {
                        self.workspace_menu.toggle(
                            &model,
                            self.bar.workspace_menu_rect(),
                            self.rect,
                        );
                    }
                }
            }
            // The Tilo mark zooms out of the workspace to the overview of the person's Tilo, and
            // back in; the bar's popovers close under it.
            ChromeIntent::ToggleOverview => {
                self.workspace_menu.close();
                self.toggle_overview();
            }
            // The bell in the status cluster: the centre it counts is this layer's to open and
            // close, where quick settings is the session's.
            ChromeIntent::ToggleNotifications => {
                if let Some(notices) = self.notices.as_ref() {
                    notices.notifications().ask_centre(!notices.center_open());
                }
            }
            ChromeIntent::RenameWorkspace { name } => self.rename_workspace(name),
            ChromeIntent::LeaveWorkspace => {
                if let Err(refusal) = self.leave_workspace(None) {
                    self.handle.acknowledge(refusal);
                }
            }
            ChromeIntent::DeleteWorkspace => {
                if let Err(refusal) = self.delete_workspace(None, None) {
                    self.handle.acknowledge(refusal);
                }
            }
            ChromeIntent::DeleteWorkspaceConfirmed => {
                if let Err(refusal) = self.delete_workspace(None, Some(true)) {
                    self.handle.acknowledge(refusal);
                }
            }
            // The strip's name in the bar lists the strips under itself, where the person pressed;
            // where the bar names no strip (too narrow for it), the strip column's labels open.
            ChromeIntent::ShowStrips => self.show_strips(),
            ChromeIntent::ToggleStrips => self
                .navigator
                .toggle_from_keyboard(&self.navigator_frame, ui::time::now()),
            ChromeIntent::OpenAvatarAsk => {
                self.handle.set_personal_ask_open(true);
            }
            ChromeIntent::CollapseAvatarAsk => {
                self.handle.set_personal_ask_open(false);
            }
            ChromeIntent::AnswerAvatarAsk => {
                self.handle.answer_personal_ask();
            }
            ChromeIntent::PutOffAvatarAsk => {
                self.handle.put_off_personal_ask();
            }
            other => self.ask(DesktopAsk::Intent(other)),
        }
    }

    /// Whether the workspace menu offers joining another workspace: the session published the
    /// action that opens the directory workspaces are joined from.
    fn may_join_workspace(&self) -> bool {
        self.handle
            .host_actions()
            .iter()
            .any(|action| action.id == JOIN_WORKSPACE)
    }

    /// What the workspace menu offers now, opened at `place` . Behind the bar's name it is the
    /// switcher: the person's own desktop where the page offers one, apart above every workspace
    /// of this desktop and the person's others the session knows (the one shown marked), then
    /// making or joining one, then the shown workspace's settings. Behind the
    /// strip's name it lists the workspace's strips and makes one. Opened on the shell, it holds
    /// the shell's own commands instead: the welcome verbs its launchpad declares, which the same
    /// actions run from the cards, search and an agent, and a new strip.
    fn workspace_menu_model(&self, place: MenuPlace) -> WorkspaceMenuModel {
        let actions = self.handle.host_actions();
        let row = |action: &HostAction| WorkspaceAction {
            id: action.id.clone(),
            title: action.title.clone(),
            icon: action.icon.as_deref().and_then(ui::icons::Icon::from_name),
            destructive: action.destructive,
        };
        if place == MenuPlace::Strips {
            let shown = self.navigator_frame.strip;
            let strips = self.handle.read(|tree| {
                tree.active_workspace()
                    .map(|workspace| {
                        workspace
                            .strips
                            .iter()
                            .enumerate()
                            .map(|(at, strip)| WorkspaceEntry {
                                name: names::strip_named(&strip.name, at),
                                current: Some(strip.id) == shown,
                                choice: WorkspaceMenuChoice::Strip(strip.id),
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            });
            return WorkspaceMenuModel {
                place,
                strips,
                can_add_strip: self.may_add_strip(),
                ..WorkspaceMenuModel::default()
            };
        }
        if place == MenuPlace::Shell {
            return WorkspaceMenuModel {
                place: MenuPlace::Shell,
                shell: actions
                    .iter()
                    .filter(|action| action.source.is_none())
                    .map(row)
                    .collect(),
                can_add_strip: self.may_add_strip(),
                ..WorkspaceMenuModel::default()
            };
        }
        let mut workspaces: Vec<WorkspaceEntry> = self.handle.read(|tree| {
            tree.workspaces
                .iter()
                .map(|workspace| WorkspaceEntry {
                    name: names::workspace(&workspace.name),
                    current: workspace.id() == tree.active,
                    choice: WorkspaceMenuChoice::Switch(workspace.id().to_string()),
                })
                .collect()
        });
        // The person's own desktop is no workspace: the page offers it as its own kind of action,
        // and the switcher shows it above the workspaces, apart from them.
        let own_desktop = actions
            .iter()
            .find(|action| action.source == Some(HostActionSource::Desktop))
            .map(|action| {
                let mut own = row(action);
                own.icon = own.icon.or(Some(ui::icons::Icon::User));
                own
            });
        // The person's workspaces this desktop does not hold: going to one is the host's.
        workspaces.extend(
            actions
                .iter()
                .filter(|action| action.source == Some(HostActionSource::Workspaces))
                .map(|action| WorkspaceEntry {
                    name: action.label.clone().unwrap_or_else(|| action.title.clone()),
                    current: false,
                    choice: WorkspaceMenuChoice::Action(action.id.clone()),
                }),
        );
        WorkspaceMenuModel {
            place: MenuPlace::Bar,
            workspaces,
            own_desktop,
            // Only the ways to another workspace: what is about this one is in its settings.
            actions: self
                .new_workspace_row(&actions)
                .into_iter()
                .chain(
                    actions
                        .iter()
                        .find(|action| action.id == JOIN_WORKSPACE)
                        .map(|action| WorkspaceAction {
                            icon: Some(ui::icons::Icon::Link),
                            ..row(action)
                        }),
                )
                .collect(),
            // What is about this workspace (its name, its people, leaving it) is in its settings.
            settings: Some(WorkspaceAction {
                id: WORKSPACE_SETTINGS.to_owned(),
                title: crate::i18n::t("workspace-menu-settings"),
                icon: Some(ui::icons::Icon::Settings),
                destructive: false,
            }),
            ..WorkspaceMenuModel::default()
        }
    }

    /// The switcher's New workspace: the session's own, where it makes workspaces itself, else
    /// the shell's, where the person may make one here.
    fn new_workspace_row(&self, actions: &[HostAction]) -> Option<WorkspaceAction> {
        if let Some(action) = actions.iter().find(|action| action.id == NEW_WORKSPACE) {
            return Some(WorkspaceAction {
                id: action.id.clone(),
                title: action.title.clone(),
                icon: Some(ui::icons::Icon::Plus),
                destructive: false,
            });
        }
        self.rules
            .may_create_workspace()
            .is_ok()
            .then(|| WorkspaceAction {
                id: NEW_WORKSPACE.to_owned(),
                title: crate::i18n::t("overview-new-workspace"),
                icon: Some(ui::icons::Icon::Plus),
                destructive: false,
            })
    }

    /// Whether the person may add a strip to the workspace shown.
    /// The bar's strip name lists the workspace's strips under itself, where it was pressed; with
    /// no name on the bar, the strip column's labels open instead.
    fn show_strips(&mut self) {
        let anchor = self.bar.strip_menu_rect();
        if anchor.is_zero_area() {
            self.navigator
                .open_for_choosing(&self.navigator_frame, ui::time::now());
        } else if !self.overlays.dismissed_recently(Part::WorkspaceMenu) {
            let model = self.workspace_menu_model(MenuPlace::Strips);
            self.workspace_menu.toggle(&model, anchor, self.rect);
        }
    }

    fn may_add_strip(&self) -> bool {
        self.rules
            .may(&DesktopCommand::CreateStrip {
                name: String::new(),
            })
            .is_ok()
    }

    fn apply_workspace_choice(&mut self, choice: WorkspaceMenuChoice) {
        match choice {
            WorkspaceMenuChoice::Switch(workspace) => {
                self.dispatch(ChromeEffect::SwitchWorkspace {
                    workspace: Some(workspace),
                    delta: None,
                });
            }
            // A host that makes no workspaces itself leaves making one to the shell.
            WorkspaceMenuChoice::Action(id)
                if id == NEW_WORKSPACE && !self.handle.offers_host_action(NEW_WORKSPACE) =>
            {
                self.new_workspace(None, None, false);
            }
            WorkspaceMenuChoice::Action(id) => {
                self.ask(DesktopAsk::HostAction(id));
            }
            WorkspaceMenuChoice::Strip(strip) => {
                self.apply_navigation(StripNavigation::Strip(StripCommand::Focus(strip)));
            }
            // Made and shown at once: the bar then names it, where it is renamed from the strip
            // column's labels.
            WorkspaceMenuChoice::NewStrip => {
                self.apply_navigation(StripNavigation::Strip(StripCommand::Create(String::new())));
            }
        }
    }

    /// Run a workspace or strip `command` on the active workspace as the model's command, once the
    /// rules allow it; a refusal is said at once. The strip it made, if any.
    fn command(&self, command: &DesktopCommand) -> Option<StripId> {
        let applied = self
            .rules
            .may(command)
            .and_then(|()| self.edit_workspace(command));
        match applied {
            Ok(made) => made,
            Err(refusal) => {
                self.handle.acknowledge(refusal);
                None
            }
        }
    }

    /// Add a strip named `name` to the active workspace, kept as this person's, and go there.
    /// The strip, when it was made.
    fn add_strip(&self, name: String) -> Option<StripId> {
        let strip = self.command(&DesktopCommand::CreateStrip { name })?;
        self.rules.made_strip(strip);
        Some(strip)
    }

    fn rename_workspace(&self, name: String) {
        let unchanged = self.handle.read(|tree| {
            tree.active_workspace()
                .is_some_and(|workspace| workspace.name == name.trim())
        });
        if !unchanged {
            self.command(&DesktopCommand::RenameWorkspace { name });
        }
    }

    /// Apply a strip or workspace edit through the desktop's one command path and replicate the
    /// result; the strip it made, if any.
    fn edit_workspace(&self, command: &DesktopCommand) -> Result<Option<StripId>, String> {
        let made = self.handle.apply_command(command)?;
        if let DesktopCommand::RenameWorkspace { name } = command {
            // The workspace's name is its governance's, which every device and list of it reads:
            // the host writes it there, and it is projected back onto this shell.
            let workspace = self.handle.read(|tree| {
                tree.active_workspace()
                    .map(tilo_desktop_model::Workspace::id)
            });
            if let Some(workspace) = workspace {
                self.ask(DesktopAsk::RenamedWorkspace {
                    workspace,
                    name: name.trim().to_owned(),
                });
            }
        }
        Ok(made)
    }

    /// Keep the panel and the host's flag on one answer: the host's surfaces open and close the
    /// overview through the flag, and the panel opens over the shell as it stands then; what the
    /// panel does itself (its action, Escape, a pick) is told back. Whether the panel moved.
    pub(super) fn follow_overview(&mut self) -> bool {
        let Some(open) = self.overview_open.clone() else {
            return false;
        };
        let asked = open.get();
        let moved = asked != self.overview_said && asked != self.overview.is_open();
        if moved {
            self.toggle_overview();
        }
        let said = self.overview.is_open();
        if said != self.overview_said {
            open.set(said);
            self.overview_said = said;
        }
        moved
    }

    /// Open or close the overview.
    pub fn toggle_overview(&mut self) {
        let open = !self.overview.is_open();
        self.overview_revision = self.handle.revision();
        self.overview
            .set_new_workspace(self.rules.may_create_workspace().is_ok());
        if open {
            if let Some(area) = self.overview_area() {
                self.overview.stand_in(area);
            }
            let shell = self.overview();
            self.overview.open_on(true, shell);
        } else {
            self.overview.open_on(false, OverviewCards::default());
        }
    }

    /// The switcher's shell as this host offers it: the workspaces it shows and what can be done to
    /// them, including joining another workspace when the session published the action that opens
    /// the directory.
    fn cards(&self) -> Vec<crate::overview::WorkspaceCard> {
        let host = PlainOverview {
            user: ui::collaboration::local_user_id()
                .person_id()
                .unwrap_or_default()
                .to_owned(),
        };
        let now = ui::time::unix_now();
        self.handle.read(|tree| workspace_cards(tree, &host, now))
    }

    fn apply_pick(&mut self, pick: &OverviewPick) {
        if pick.closes() {
            self.overview.open_on(false, OverviewCards::default());
        }
        // The ones with a published verb are that verb; the rest are the shell's own.
        let verb = match pick {
            OverviewPick::Window(tab) => {
                self.handle.focus_tab(*tab);
                None
            }
            OverviewPick::Workspace(id) => Some(ChromeEffect::SwitchWorkspace {
                workspace: Some(id.to_string()),
                delta: None,
            }),
            OverviewPick::NewWorkspace => {
                self.new_workspace(None, None, false);
                None
            }
            OverviewPick::JoinWorkspace => Some(ChromeEffect::JoinWorkspace),
            OverviewPick::LeaveWorkspace(id) => Some(ChromeEffect::LeaveWorkspace {
                workspace: Some(id.to_string()),
            }),
            OverviewPick::CloseWindow(tab) => {
                self.close_window(*tab);
                None
            }
            OverviewPick::MoveWindow { tab, to } => Some(ChromeEffect::MoveTab {
                tab: tab.to_string(),
                to: Destination {
                    workspace: Some(to.to_string()),
                    ..Destination::default()
                },
            }),
            OverviewPick::MoveWindowToStrip { tab, strip: to } => Some(ChromeEffect::MoveTab {
                tab: tab.to_string(),
                to: Destination {
                    strip: Some(to.0.to_string()),
                    ..Destination::default()
                },
            }),
            OverviewPick::RenameStrip { strip: id, name } => Some(ChromeEffect::Rename {
                strip: Some(id.0.to_string()),
                name: name.clone(),
                tab: None,
                group: None,
                workspace: None,
            }),
            OverviewPick::MoveStrip { strip: id, before } => Some(ChromeEffect::MoveStrip {
                strip: id.0.to_string(),
                before: before.map(|strip| strip.0.to_string()),
            }),
            OverviewPick::Dismiss => None,
            OverviewPick::OpenPage(url) => {
                // A provider's own page, never a form of ours: only a secure web page opens.
                if url.starts_with("https://") {
                    ui::widget::open_external_url(url);
                }
                None
            }
            OverviewPick::RevokeGrant(grant) => {
                self.ask(DesktopAsk::RevokeAgentGrant(grant.clone()));
                None
            }
            OverviewPick::OpenAgent(client) => {
                self.ask(DesktopAsk::OpenAgent(client.clone()));
                None
            }
            OverviewPick::HostAction(id) => {
                self.ask(DesktopAsk::HostAction(id.clone()));
                None
            }
        };
        if let Some(verb) = verb {
            let _ = self.perform(&verb);
        }
    }

    /// Close the window `tab` here, as the close on its own bar does.
    fn close_window(&self, tab: TabId) {
        self.dispatch(LayoutEffect::CloseTab {
            tab: tab.to_string(),
        });
    }

    /// The workspace `key` names: its id, or its name.
    fn workspace_named(&self, key: &str) -> Result<WorkspaceId, String> {
        let key = key.trim();
        self.handle
            .read(|tree| {
                tree.workspaces
                    .iter()
                    .find(|workspace| workspace.name.eq_ignore_ascii_case(key))
                    .or_else(|| {
                        // The id it is shared under, the same on every device.
                        WorkspaceId::parse(key).and_then(|id| {
                            tree.workspaces
                                .iter()
                                .find(|workspace| workspace.id() == id)
                        })
                    })
                    .map(tilo_desktop_model::Workspace::id)
            })
            .ok_or_else(|| crate::i18n::t_args("desktop-no-workspace", &[("name", key.into())]))
    }

    /// The strip `key` names in the workspace the person is in: its id, or the name a person
    /// reads, which is a strip nobody named by where it stands ("Strip 1").
    fn strip_named(&self, key: &str) -> Result<StripId, String> {
        let key = key.trim();
        let missing = || crate::i18n::t_args("desktop-no-strip", &[("name", key.into())]);
        self.handle
            .read(|tree| {
                let workspace = tree.active_workspace()?;
                workspace
                    .strips
                    .iter()
                    .enumerate()
                    .find(|(at, strip)| {
                        // The name a person reads: the strip's own, or the shell's word for a
                        // strip nobody named ("Strip 1", where it stands).
                        names::strip_named(&strip.name, *at).eq_ignore_ascii_case(key)
                    })
                    .map(|(_, strip)| strip)
                    .or_else(|| {
                        key.parse::<u64>()
                            .ok()
                            .and_then(|id| workspace.strips.iter().find(|strip| strip.id.0 == id))
                    })
                    .map(|strip| strip.id)
            })
            .ok_or_else(missing)
    }

    /// Leave the workspace `id` (the one the person is in when absent): struck from this device's
    /// desktop, the person going to the next one when it was the one they were in.
    fn leave_workspace(&self, id: Option<WorkspaceId>) -> Result<(), String> {
        if self.handle.read(|tree| tree.workspaces.len()) <= 1 {
            return Err(crate::i18n::t("desktop-only-workspace"));
        }
        let left = self.take_workspace_off(id)?;
        self.ask(DesktopAsk::LeftWorkspace(left));
        Ok(())
    }

    /// Delete the workspace `id` (the one the person is in when absent).
    ///
    /// Deleting takes everything in it, so it is asked first: the person
    /// answers in the host's own confirm ([`DesktopAsk::DeleteWorkspace`]),
    /// and only an answer that says so deletes.
    fn delete_workspace(
        &self,
        id: Option<WorkspaceId>,
        confirmed: Option<bool>,
    ) -> Result<(), String> {
        let here = id.or_else(|| {
            self.handle.read(|tree| {
                tree.active_workspace()
                    .map(tilo_desktop_model::Workspace::id)
            })
        });
        let id = here.ok_or_else(|| crate::i18n::t("desktop-no-workspace-to-delete"))?;
        self.rules.may_delete_workspace(id)?;
        let named = self
            .handle
            .read(|tree| {
                tree.workspaces
                    .iter()
                    .find(|workspace| workspace.id() == id)
                    .map(|workspace| (workspace.id(), names::workspace(&workspace.name)))
            })
            .ok_or_else(|| crate::i18n::t("desktop-workspace-gone"))?;
        let (workspace, name) = named;
        if confirmed != Some(true) {
            // The person answers first: nothing of it goes until they do.
            self.ask(DesktopAsk::DeleteWorkspace {
                workspace: id,
                name,
            });
            return Ok(());
        }
        let name = self.take_workspace_off(Some(id))?;
        if self.handle.read(|tree| tree.workspaces.is_empty()) {
            self.fresh_workspace();
        }
        // Whoever asked for it — the menu, the settings, an agent — the host hears of the deletion
        // the same way, from the shell that made it.
        self.ask(DesktopAsk::DeletedWorkspace { workspace, name });
        Ok(())
    }

    /// Take the workspace `id` (the one the person is in when absent) off this session, the person
    /// going to the next one along when it was the one they were in. Answers what it was called.
    fn take_workspace_off(&self, id: Option<WorkspaceId>) -> Result<String, String> {
        let mut tree = self.handle.read(Clone::clone);
        let id = id.or_else(|| {
            tree.active_workspace()
                .map(tilo_desktop_model::Workspace::id)
        });
        let id = id.ok_or_else(|| crate::i18n::t("desktop-no-workspace-to-leave"))?;
        let left = tree
            .take_off(id)
            .ok_or_else(|| crate::i18n::t("desktop-workspace-gone"))?;
        self.handle.replace_tree(tree);
        Ok(left.name)
    }

    /// A new empty workspace after the others, with the strip its first window goes in: what stands
    /// on the shell when the last one is deleted.
    fn fresh_workspace(&self) {
        let tree = self.handle.read(Clone::clone);
        let workspace = Workspace::new(
            WorkspaceId::new(),
            crate::workspace_names::made(None, &tree.workspaces),
        );
        self.open_workspace(tree, workspace, true, true);
    }

    /// Put `workspace` after the others and, when `go` , go there, with a strip of its own made
    /// here when `fresh_strip` — a workspace a site lets this computer into comes with the strips
    /// its site holds, and a strip made here would stand on this session only. A shell that shows no
    /// workspace goes to the one it gets whatever `go` says: there is nowhere else to stay.
    fn open_workspace(
        &self,
        mut tree: Layout,
        mut workspace: Workspace,
        fresh_strip: bool,
        go: bool,
    ) {
        let id = workspace.id();
        let go = go || tree.active_workspace().is_none();
        if fresh_strip {
            let strip = Strip::new(StripId(self.handle.mint_id()), "");
            let strip_id = strip.id;
            workspace.strips.push(strip);
            tree.active_strip.insert(id, strip_id);
        }
        tree.workspaces.push(workspace);
        if go {
            tree.active = id;
        }
        self.handle.replace_tree(tree);
    }

    /// Make a workspace after the others and go there; with `join` , the workspace holding that id,
    /// joining it when this computer has none (a workspace that exists is a workspace that exists,
    /// so joining one is holding it, which needs no workspace of the person's own to make).
    /// In the `background` the workspace is put on the shell and the person stays where they are:
    /// one their list brings from another of their devices arrives while they are somewhere else,
    /// and what they open next belongs to the workspace they are in.
    fn new_workspace(&self, join: Option<&str>, name: Option<&str>, background: bool) {
        let mut tree = self.handle.read(Clone::clone);
        let joining = join.is_some();
        let workspace = if let Some(id) = join {
            let Some(joined) = WorkspaceId::parse(id) else {
                self.handle.acknowledge(tilo_session_core::desktop::i18n::t(
                    "desktop-joined-workspace-unknown",
                ));
                return;
            };
            if let Some(held) = tree.workspaces.iter().find(|held| held.id() == joined) {
                // Already held, here or from another device: go there rather than make a second
                // workspace of one workspace. In the background the person stays where they are,
                // and the shell is replaced all the same, as it always was here: a replaced shell is
                // one the session reads and writes its workspaces' records for again, and this
                // workspace has just taken this device in.
                if !background {
                    tree.active = held.id();
                }
                self.handle.replace_tree(tree);
                return;
            }
            // A workspace that exists comes on this shell once this device holds it: put on bare,
            // it would be a second workspace under its id.
            if let Err(refusal) = self.rules.may_open_workspace(joined) {
                self.handle.acknowledge(refusal);
                return;
            }
            Workspace::new(joined, crate::workspace_names::made(name, &tree.workspaces))
        } else {
            if let Err(refusal) = self.rules.may_create_workspace() {
                self.handle.acknowledge(refusal);
                return;
            }
            let name = crate::workspace_names::made(name, &tree.workspaces);
            Workspace::new(WorkspaceId::new(), name)
        };
        self.open_workspace(tree, workspace, !joining, !background);
    }

    /// Branch the workspace `source` names into a new one called `name` (the source's name with
    /// "branch" when none is given): the new workspace's id is made here, and the host
    /// makes the workspace with what the source holds and puts it on the shell once it does
    /// ([`DesktopAsk::BranchWorkspace`]), so nothing opens in it before its records are there.
    /// Answers the new workspace's id.
    fn branch_workspace(&self, source: &str, name: Option<&str>) -> Result<WorkspaceId, String> {
        let source = self.workspace_named(source)?;
        self.rules.may_branch_workspace(source)?;
        let tree = self.handle.read(Clone::clone);
        let held = tree
            .workspaces
            .iter()
            .find(|held| held.id() == source)
            .ok_or_else(|| crate::i18n::t("desktop-workspace-gone"))?;
        let asked = name.map(str::trim).filter(|name| !name.is_empty());
        let name = asked.map_or_else(
            || {
                crate::i18n::t_args(
                    "desktop-branch-workspace-name",
                    &[("name", held.name.clone().into())],
                )
            },
            str::to_owned,
        );
        let name = crate::workspace_names::made(Some(&name), &tree.workspaces);
        let workspace = WorkspaceId::new();
        self.ask(DesktopAsk::BranchWorkspace {
            source,
            workspace,
            name,
        });
        Ok(workspace)
    }

    /// The window `step` places from the focused one in the shown strip, or the strip's `nth`
    /// window.
    fn window_tab(&self, step: Option<isize>, nth: Option<usize>) -> Option<u64> {
        self.handle.read(|tree| {
            let workspace = tree.active_workspace()?;
            let strip_id = tree.active_strip.get(&workspace.id()).copied()?;
            let strip = workspace.strips.iter().find(|s| s.id == strip_id)?;
            let index = match (step, nth) {
                (_, Some(nth)) => nth,
                (Some(step), None) => {
                    let focused = tree
                        .active_group_for(strip.id)
                        .and_then(|g| strip.window_groups.iter().position(|w| w.id == g))
                        .unwrap_or(0);
                    focused.checked_add_signed(step)?
                }
                (None, None) => return None,
            };
            strip.window_groups.get(index).map(|group| group.active_tab)
        })
    }

    /// Go to the strip `pick` chooses from the shown one's index, in the active workspace; whether
    /// there was one.
    fn go_to_strip(&self, pick: impl FnOnce(usize) -> Option<usize>) -> bool {
        let next = self.handle.read(|tree| {
            let workspace = tree.active_workspace()?;
            let current = tree
                .active_strip
                .get(&workspace.id())
                .and_then(|id| workspace.strips.iter().position(|s| s.id == *id))
                .unwrap_or(0);
            let strip = workspace.strips.get(pick(current)?)?;
            Some((workspace.id().to_string(), strip.id.0.to_string()))
        });
        if let Some((workspace, strip)) = next {
            self.dispatch(ChromeEffect::ShowStrip { workspace, strip });
            true
        } else {
            false
        }
    }

    /// The pointer over the notice's button ("Undo"), drawn over the windows: hovering lights
    /// it, and a press that begins on it is its own. Whether the event was the button's.
    fn notice_takes(&mut self, event: &WindowEvent) -> bool {
        match event {
            WindowEvent::CursorMoved { .. } | WindowEvent::CursorLeft { .. } => {
                self.notice.pointer_at(self.cursor);
                false
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => self
                .cursor
                .is_some_and(|at| self.notice.press(at, *state == ElementState::Pressed)),
            _ => false,
        }
    }

    /// Step strips by `steps` , saying so when the person steps past an end.
    fn step_strip(&self, steps: isize) {
        if !self.go_to_strip(|current| current.checked_add_signed(steps)) {
            if steps > 0 {
                self.handle.acknowledge(crate::i18n::t("shell-last-strip"));
            } else if steps < 0 {
                self.handle.acknowledge(crate::i18n::t("shell-first-strip"));
            }
        }
    }

    /// Whether chords are read with a Mac's keys.
    #[must_use]
    pub fn mac(&self) -> bool {
        self.mac
    }

    /// The host is a page installed as an app window, or a page in a browser
    /// tab: an app window also takes Ctrl+W, Ctrl+T and Ctrl+N as any app does.
    pub fn set_app_window(&mut self, app_window: bool) {
        self.page_window = if app_window {
            PageWindow::App
        } else {
            PageWindow::Tab
        };
    }

    /// A desktop chord, applied; false when `event` is not one. The chord is looked up in the
    /// desktop's one shortcut table, with a host's stand-in for Super counting as Super
    /// ([`page_shortcut_action`] and [`page_fixed_action`]).
    pub fn chord(&mut self, event: &winit::event::KeyEvent, modifiers: &Modifiers) -> bool {
        if event.state != ElementState::Pressed {
            return false;
        }
        // The chords Settings cannot rebind come before any program's, as on the native desktop:
        // each runs the verb its menu row, its palette row and an agent's invocation name, and the
        // ones the shell has no part in are asked of the page. The shell claims the press either way,
        // so the browser's own key for it is not run too.
        let app_window = (self.page_window == PageWindow::App)
            .then(|| app_window_fixed_action(event.physical_key, *modifiers, self.mac))
            .flatten();
        if let Some(action) = app_window.or_else(|| {
            page_fixed_action(&event.logical_key, event.physical_key, *modifiers, self.mac)
        }) {
            self.fixed_shortcut(action);
            return true;
        }
        let action = page_shortcut_action(
            &self.shortcuts.borrow(),
            &event.logical_key,
            event.physical_key,
            *modifiers,
            self.mac,
        );
        if let Some(action) = action {
            return self.shortcut(action);
        }
        false
    }

    /// Where the strip ribbon and rail stand: under the bar on the display the person works on,
    /// never across the border.
    fn navigation_area(&self) -> Rect {
        self.working_display()
            .map_or(self.content, |display| display.intersect(self.content))
    }

    /// Read where the person is into the navigator, when the tree, the strip's pan, who is here,
    /// the overview or the area it stands in changed since it last read.
    fn observe_navigation(&mut self) {
        let covered = self.overview.is_open();
        let area = self.navigation_area();
        // Whether the person may add a strip is read each frame too: what they may do in a
        // workspace they just made arrives after its layout does, with no change to the layout to
        // say so, and the New strip button waits on it.
        let may_add = self.may_add_strip();
        let seen = (
            self.handle.navigation_revision(),
            self.rules.people_revision(),
            covered,
            area,
            may_add,
        );
        if self.navigator_seen.replace(seen) == Some(seen) {
            return;
        }
        let tree = self.rules.tree();
        let author = self.rules.author();
        // Who names a strip is the desktop's one policy: whoever governs the workspace, and anyone
        // the strips they made.
        let may_rename = |strip: StripId| {
            self.rules
                .may(&DesktopCommand::RenameStrip {
                    strip: strip.0,
                    name: String::new(),
                })
                .is_ok()
        };
        let shown = self.handle.shown_tabs();
        let scroll = self.handle.presentation().pan.map_or(0.0, |pan| pan.offset);
        self.navigator.set_model(StripNavigator::model_of(
            &tree,
            &Viewer {
                local_user: &author,
                own_id: &author,
                may_rename: &may_rename,
                // Offered as the workspace menu's is: to whom the rules let add one.
                may_add,
            },
        ));
        self.navigator_frame = StripFrame::of_tree(&tree, area, (&shown, scroll), covered, may_add);
        self.navigator_frame.ribbon.new_len = self.navigator.new_strip_len();
        // A display one column wide (a phone) has no edge to spare: the tabs would stand over the
        // window's own controls, so its strips are listed and added from the bar's strip name.
        if ui::tokens::Breakpoint::of(area.width(), &self.theme.tokens.metrics).is_compact() {
            self.navigator_frame.ribbon_offered = false;
            self.navigator_frame.new_strip_offered = false;
        }
        self.navigator
            .observe(&self.navigator_frame, ui::time::now());
    }

    /// Carry out what the strip ribbon, its labels or the rail asked for.
    fn apply_navigation(&mut self, navigation: StripNavigation) {
        let refused = match navigation {
            StripNavigation::Window { strip, group } => {
                let tab = self.handle.read(|tree| {
                    tree.active_workspace()?
                        .strips
                        .iter()
                        .find(|s| s.id == strip)?
                        .window_groups
                        .iter()
                        .find(|g| g.id == group)
                        .map(|g| g.active_tab)
                });
                if let Some(tab) = tab {
                    self.handle.focus_tab(tab);
                }
                false
            }
            StripNavigation::Strip(StripCommand::Focus(strip)) => {
                let index = self.handle.read(|tree| {
                    tree.active_workspace()?
                        .strips
                        .iter()
                        .position(|s| s.id == strip)
                });
                self.go_to_strip(|_| index);
                false
            }
            StripNavigation::Strip(StripCommand::Step(steps)) => {
                self.step_strip(isize::try_from(steps).unwrap_or_default());
                false
            }
            StripNavigation::Strip(StripCommand::Create(name)) => self.add_strip(name).is_none(),
            StripNavigation::Strip(StripCommand::Rename(strip, name)) => self
                .edit_workspace(&DesktopCommand::RenameStrip {
                    strip: strip.0,
                    name,
                })
                .is_err(),
            StripNavigation::Strip(StripCommand::Remove(strip)) => self
                .edit_workspace(&DesktopCommand::RemoveStrip { strip: strip.0 })
                .is_err(),
        };
        if refused {
            self.handle
                .acknowledge(crate::i18n::t("strip-switcher-refused"));
        }
        // A new strip is listed at once, so its name goes in place.
        self.navigator_seen = None;
        self.observe_navigation();
    }

    /// Whether the strip ribbon, its labels or the rail took `event` .
    fn navigation_event(&mut self, event: &WindowEvent, scale: f64, modifiers: Modifiers) -> bool {
        use winit::event::{Ime, MouseButton};
        let now = ui::time::now();
        let navigation = match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale, position.y / scale);
                self.navigator.pointer_moved(&self.navigator_frame, p, now);
                return false;
            }
            WindowEvent::CursorLeft { .. } => {
                self.navigator.pointer_left(now);
                return false;
            }
            WindowEvent::KeyboardInput { event: key, .. } if self.navigator.takes_keys() => {
                if key.state != ElementState::Pressed {
                    return true;
                }
                self.navigator
                    .key(&self.navigator_frame, &key.logical_key, modifiers.state())
            }
            WindowEvent::Ime(Ime::Commit(text)) if self.navigator.takes_keys() => {
                return self.navigator.commit_text(text);
            }
            WindowEvent::MouseInput {
                state,
                button: button @ (MouseButton::Left | MouseButton::Right),
                ..
            } => {
                if *state == ElementState::Released {
                    return self.navigator.release();
                }
                let Some(p) = self.cursor else {
                    return false;
                };
                // A secondary press (a long-press by touch) puts a strip's actions out beside its
                // label.
                let answer = if *button == MouseButton::Right {
                    self.navigator
                        .secondary_press(&self.navigator_frame, p, now)
                } else {
                    self.navigator.press(&self.navigator_frame, p, now)
                };
                let Some(navigation) = answer else {
                    return false;
                };
                navigation
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let Some(p) = self.cursor else {
                    return false;
                };
                let Some(navigation) = self.navigator.wheel(&self.navigator_frame, p, *delta, now)
                else {
                    return false;
                };
                navigation
            }
            _ => return false,
        };
        if let Some(navigation) = navigation {
            self.apply_navigation(navigation);
        }
        true
    }

    /// Whether anything of the chrome is due a redraw.
    fn chrome_needs_redraw(&self) -> bool {
        // An intent waiting to be carried out asks for the frame that carries it.
        !self.intents.borrow().is_empty()
            || (self.emoji.needs_redraw() || self.emoji.needs_animation_frame())
            || self.navigator.needs_animation_frame(ui::time::now())
            || self.identify.needs_redraw(ui::time::now())
            || self.switcher.needs_redraw()
            || self.peek.needs_redraw(ui::time::now())
            || self
                .children()
                .iter()
                .any(|child| child.needs_redraw() || child.needs_animation_frame())
    }
}

impl DesktopChrome {
    /// Put every display's bar where the shell's source says, when what it says moved: a display
    /// arriving or going, the person moving to another display, or a window going fullscreen there.
    /// Returns whether anything moved.
    /// A shell of one display carries no places: the full bar then stands where layout puts it,
    /// across the whole shell.
    fn follow_bars(&mut self) -> bool {
        let Some(chrome) = self.bar.chrome().cloned() else {
            return false;
        };
        let revision = chrome.snapshot().revision;
        if revision == self.bars_revision {
            return false;
        }
        self.bars_revision = revision;
        let placement = chrome.bars();
        if placement == self.placement {
            return false;
        }
        // The full bar stands on the display in use, where the person is working; each of the
        // others carries the compact bar on its own display, so each follows its own display's
        // rectangle.
        let full = placement.bars.iter().find(|place| place.full).copied();
        self.bar.set_placement(full.map(|place| place.rect));
        let tree = placement
            .bars
            .iter()
            .any(|place| !place.full)
            .then(|| chrome.tree());
        let mut kept = std::mem::take(&mut self.compact);
        let mut bars = Vec::with_capacity(placement.bars.len().saturating_sub(1));
        for place in placement.bars.iter().filter(|place| !place.full) {
            let reused = kept
                .iter()
                .position(|bar| bar.display() == Some(place.display))
                .map(|at| kept.remove(at));
            let mut bar = if let Some(bar) = reused {
                bar
            } else {
                let Some(tree) = tree.as_ref() else {
                    continue;
                };
                TopBarChrome::from_tree(tree)
                    .compact()
                    .on_display(place.display)
                    .with_chrome(Rc::clone(&chrome))
            };
            bar.set_placement(Some(place.rect));
            bars.push(bar);
        }
        self.compact = bars;
        self.placement = placement;
        true
    }

    /// Bring the bars' reveal up to date at `now` : where each display's bar stands, which displays
    /// a window covers, whether the person hides the bar (`hide`), and what of the chrome holds it
    /// open (`held_extra`). Returns whether any bar's presence may have changed.
    /// A bar shows over the window covering its display for the idle time after a window enters
    /// full screen there, and while the person reaches for it, uses it or keeps one of its menus
    /// open; else it is hidden. The person's own "hide the top bar" is the same machinery without a
    /// fullscreen window.
    pub fn follow_reveals(&mut self, hide: bool, held_extra: bool, now: ui::time::Instant) -> bool {
        let mut changed = self.follow_bars();
        let height = self.bar.bar_rect().height();
        let height = if height > 0.0 {
            height
        } else {
            f64::from(TOPBAR_HEIGHT_PX)
        };
        let full = self.placement.bars.iter().find(|place| place.full);
        // The display in use is the one the full bar stands on; a shell that needs no placing is one
        // display, which the window covers or not.
        let in_use_fullscreen = full.map_or(!self.placement.fullscreen.is_empty(), |place| {
            self.placement.fullscreen.contains(&Some(place.display))
        });
        let mut places = Vec::with_capacity(self.compact.len().saturating_add(1));
        places.push((
            None,
            full.map_or(self.rect, |place| place.rect),
            in_use_fullscreen,
        ));
        for place in self.placement.bars.iter().filter(|place| !place.full) {
            places.push((
                Some(place.display),
                place.rect,
                self.placement.fullscreen.contains(&Some(place.display)),
            ));
        }
        let held = held_extra || self.holds_bar_open();
        let mut reveals = self.reveals.borrow_mut();
        reveals.set_height(height);
        reveals.set_hide(hide);
        reveals.follow(&places, now);
        reveals.set_below(self.handle.covering_bar().map(|(_, bar)| bar));
        changed |= reveals.settle(now, |_| held);
        let full_presence = reveals.presence(None, held);
        let compact: Vec<(Option<u32>, TopBarPresence)> = self
            .compact
            .iter()
            .map(|bar| (bar.display(), reveals.presence(bar.display(), held)))
            .collect();
        drop(reveals);
        if self.bar.presence() != full_presence {
            self.bar.set_presence(full_presence);
            changed = true;
        }
        // A bar shown over the shell tells the windows how much of its top edge it covers, so their
        // own bars move down whole and none of their controls — a title, a Back, a clock of their
        // own — is left under the bar's, and the bar's clock never reads across them.
        changed |= self
            .handle
            .set_top_overlay_inset(if full_presence == TopBarPresence::Shown {
                height
            } else {
                0.0
            });
        for (bar, (_, presence)) in self.compact.iter_mut().zip(compact) {
            if bar.presence() != presence {
                bar.set_presence(presence);
                changed = true;
            }
        }
        // Settings asked which screen is which: each display says its number and name for a moment.
        changed |= self.follow_identify(now);
        changed
    }

    /// Answer Settings' Identify request, once per press: each display says the number its tile on
    /// the canvas carries and the name Settings gives it. The displays are read while the cards
    /// stand, so a card follows the display it names when the person moves one mid-answer.
    fn follow_identify(&mut self, now: ui::time::Instant) -> bool {
        let Some(chrome) = self.bar.chrome().cloned() else {
            return false;
        };
        let ask = chrome.snapshot().identify;
        let asked = ask.as_ref().map(|ask| ask.id);
        let is_up = self.identify.is_up(now);
        self.identify.follow(ask, now);
        if self.identify.is_up(now) {
            let tree = chrome.tree();
            self.identify_displays = tree
                .outputs
                .displays
                .iter()
                .map(|display| {
                    (
                        display.monitor.clone(),
                        tilo_session_core::desktop::display_rect(display),
                    )
                })
                .collect();
        }
        let up = self.identify.is_up(now);
        let changed = asked != self.answered || is_up != up;
        self.answered = asked;
        changed
    }

    /// Whether something of the chrome holds the bars shown: one of the bar's menus or cards, an
    /// overlay it opened, or the keyboard in it.
    fn holds_bar_open(&self) -> bool {
        self.workspace_menu.is_open()
            || self.navigator.is_open()
            || self.emoji.is_open()
            || self.bar.holds_itself_shown()
            || self.compact.iter().any(TopBarChrome::holds_itself_shown)
    }

    /// Carry out a verb of the chrome's: whether it did, or why it could not, which the action API that
    /// asked answers as a refusal.
    fn perform(&mut self, verb: &ChromeEffect) -> Result<bool, String> {
        if let Some(done) = self.perform_workspace_action(verb) {
            return Ok(done);
        }
        if let Some(done) = self.perform_surface_verb(verb) {
            self.report(done.clone());
            return done.map(|()| true);
        }
        let intent = match verb {
            ChromeEffect::RevealTopBar => {
                self.reveals.borrow_mut().reveal_by_key(ui::time::now());
                return Ok(true);
            }
            ChromeEffect::Overview { open } => {
                let open = open.unwrap_or(!self.overview_open());
                if self.overview_open() != open {
                    self.toggle_overview();
                }
                return Ok(true);
            }
            // The About sheet, as the chrome's `about` verb shows it.
            ChromeEffect::About { open } => {
                let open = open.unwrap_or(!self.about.is_about_open());
                if self.about.is_about_open() != open {
                    self.about.set_about_open(open);
                }
                return Ok(true);
            }
            ChromeEffect::QuickSettings => ChromeIntent::ToggleQuickSettings,
            ChromeEffect::WorkspaceMenu => ChromeIntent::ToggleWorkspaceMenu { at: None },
            ChromeEffect::SwitchWorkspace {
                delta: Some(delta), ..
            } => ChromeIntent::SwitchWorkspace(i32::try_from(*delta).unwrap_or_default()),
            ChromeEffect::ShowPerson { person } => ChromeIntent::ShowPerson(person.clone()),
            ChromeEffect::TogglePeople => ChromeIntent::TogglePeople,
            ChromeEffect::ShowStrips => ChromeIntent::ShowStrips,
            ChromeEffect::StepStrip { steps } => {
                self.step_strip(isize::try_from(*steps).unwrap_or_default());
                return Ok(true);
            }
            // The workspace edit path of the strip switcher's own button,
            // so an agent or a page makes a strip the way the person does.
            ChromeEffect::NewStrip { name } => {
                // A strip made here keeps no name until the person gives it one: the shell names it
                // by its place, in the language of whoever reads it.
                let name = name.clone().unwrap_or_default();
                if self.add_strip(name).is_none() {
                    return Err(crate::i18n::t("strip-switcher-refused"));
                }
                return Ok(true);
            }
            ChromeEffect::NewWorkspace {
                workspace,
                name,
                background,
            } => {
                self.new_workspace(
                    workspace.as_deref(),
                    name.as_deref(),
                    background.unwrap_or(false),
                );
                return Ok(true);
            }
            ChromeEffect::BranchWorkspace { source, name } => {
                let branch = self.branch_workspace(source, name.as_deref())?;
                self.output = Some(serde_json::Value::String(branch.to_string()));
                return Ok(true);
            }
            ChromeEffect::Shortcut { action } => {
                let Ok(action) = serde_json::from_value::<ShortcutAction>(
                    serde_json::Value::String(action.clone()),
                ) else {
                    return Ok(false);
                };
                // The shell carries out its own and asks its host for the rest, once: the shortcut's
                // own answer is the whole of it.
                self.shortcut(action);
                return Ok(true);
            }
            ChromeEffect::FixedShortcut { action } => {
                let Some(action) = FixedAction::from_id(action) else {
                    return Ok(false);
                };
                // The same split as a configurable shortcut's: the shell's windows here, the device
                // under them at its host.
                self.fixed_shortcut(action);
                return Ok(true);
            }
            _ => return Ok(false),
        };
        self.apply_intent(intent);
        Ok(true)
    }

    /// Carry out a verb of the surface's, the ones a page's commands carry too: the shell admits
    /// each against the tree it changes and applies it on its next pass, so nothing here decides
    /// twice.
    fn perform_surface_verb(&self, verb: &ChromeEffect) -> Option<Result<(), String>> {
        let surface = matches!(
            verb,
            ChromeEffect::OpenApp { .. }
                | ChromeEffect::ShowApp { .. }
                | ChromeEffect::MoveTab { .. }
                | ChromeEffect::CloseWindow { .. }
                | ChromeEffect::SplitRight { .. }
                | ChromeEffect::NewGroup { .. }
                | ChromeEffect::SetWidth { .. }
                | ChromeEffect::Equalize { .. }
                | ChromeEffect::Maximize { .. }
                | ChromeEffect::Fullscreen { .. }
                | ChromeEffect::Float { .. }
                | ChromeEffect::Rename { .. }
                | ChromeEffect::ReopenClosed { .. }
                | ChromeEffect::ShareWindow { .. }
                | ChromeEffect::StopDesktopControl { .. }
                | ChromeEffect::SwitchWorkspace {
                    workspace: Some(_),
                    ..
                }
                | ChromeEffect::ShowStrip { .. }
                | ChromeEffect::Pan { .. }
                | ChromeEffect::Scroll { .. }
                | ChromeEffect::History { .. }
                | ChromeEffect::Activity { .. }
                | ChromeEffect::CloseActivity { .. }
                | ChromeEffect::Notifications { .. }
        );
        surface.then(|| self.handle.dispatch(verb.clone()))
    }

    /// What the shell itself does to offers of workspaces and strips, by address: `None` for an
    /// action of another kind.
    fn perform_workspace_action(&self, verb: &ChromeEffect) -> Option<bool> {
        match verb {
            ChromeEffect::Rename {
                strip: Some(strip),
                name,
                ..
            } => {
                let renamed = self.strip_named(strip).and_then(|strip| {
                    self.edit_workspace(&DesktopCommand::RenameStrip {
                        strip: strip.0,
                        name: name.clone(),
                    })
                    .map(drop)
                });
                self.report(renamed);
                Some(true)
            }
            ChromeEffect::Rename {
                workspace: Some(workspace),
                name,
                ..
            } => {
                let renamed = self.workspace_named(workspace).and_then(|workspace| {
                    let active = self.handle.read(|tree| tree.active);
                    (active == workspace)
                        .then_some(())
                        .ok_or_else(|| "Workspace is no longer active".to_owned())?;
                    self.edit_workspace(&DesktopCommand::RenameWorkspace { name: name.clone() })
                        .map(drop)
                });
                self.report(renamed);
                Some(true)
            }
            ChromeEffect::MoveStrip { strip, before } => {
                let moved = self.strip_named(strip).and_then(|strip| {
                    let before = before
                        .as_deref()
                        .map(|before| self.strip_named(before))
                        .transpose()?;
                    self.edit_workspace(&DesktopCommand::MoveStrip {
                        strip: strip.0,
                        before: before.map(|before| before.0),
                    })
                    .map(drop)
                });
                self.report(moved);
                Some(true)
            }
            ChromeEffect::JoinWorkspace => {
                self.ask(DesktopAsk::HostAction(JOIN_WORKSPACE.to_owned()));
                Some(true)
            }
            ChromeEffect::OpenSettings { link } => {
                self.ask(DesktopAsk::OpenSettings(link.clone()));
                Some(true)
            }
            ChromeEffect::LeaveWorkspace { workspace } => {
                let left = workspace
                    .as_deref()
                    .map_or(Ok(None), |id| self.workspace_named(id).map(Some))
                    .and_then(|id| self.leave_workspace(id));
                self.report(left);
                Some(true)
            }
            ChromeEffect::DeleteWorkspace {
                workspace,
                confirmed,
            } => {
                let deleted = workspace
                    .as_deref()
                    .map_or(Ok(None), |id| self.workspace_named(id).map(Some))
                    .and_then(|id| self.delete_workspace(id, *confirmed));
                self.report(deleted);
                Some(true)
            }
            ChromeEffect::WorkspaceSettings => {
                self.ask(DesktopAsk::HostAction(WORKSPACE_SETTINGS.to_owned()));
                Some(true)
            }
            _ => None,
        }
    }
}

impl ui::resources::PublishesResources for DesktopChrome {
    fn resources(&self, _: &ui::automerge::AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
        vec![chrome_resource(&self.shortcuts.borrow())]
    }
}

impl ui::host_operation::ResourceOperations for DesktopChrome {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[ChromeEffect::OPERATION]
    }

    fn start_operation(
        &mut self,
        op: &ui::host_operation::ResourceOperation<'_>,
        doc: &mut ui::widget::Edits<'_>,
        path: &ui::automerge::ObjId,
    ) -> Result<Option<ui::host_operation::ResourceOperationStart>, String> {
        if let Some(request) = op.unscoped()
            && request.type_id == ChromeEffect::OPERATION
            && let Ok(verb) = ChromeEffect::from_payload(request.payload)
        {
            self.output = None;
            if self.perform(&verb)? {
                return Ok(Some(ui::host_operation::ResourceOperationStart::Completed(
                    tilo_actions::ActionOutput {
                        value: self.output.take(),
                        ..tilo_actions::ActionOutput::default()
                    },
                )));
            }
        }
        ui::host_operation::start_in_children(self, op, doc, path)
    }
}

impl Widget for DesktopChrome {
    ui::provides!(
        dyn ui::resources::PublishesResources,
        dyn ui::host_operation::ResourceOperations
    );

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let intents = std::mem::take(&mut *self.intents.borrow_mut());
        let mut changed = !intents.is_empty();
        for intent in intents {
            self.apply_intent(intent);
        }
        if let Some(pick) = self.overview.take_pick() {
            self.apply_pick(&pick);
            changed = true;
        }
        if self.overview.is_open() && self.overview_revision != self.handle.revision() {
            self.overview_revision = self.handle.revision();
            let shell = self.overview();
            self.overview.set_shell(shell);
            changed = true;
        }
        changed |= self.about.settle();
        if let Some(link) = self.about.take_settings_link() {
            self.ask(DesktopAsk::OpenSettings(link.to_owned()));
            changed = true;
        }
        if let Some(choice) = self.workspace_menu.take_choice() {
            self.apply_workspace_choice(choice);
            changed = true;
        }
        for tab in self.handle.take_new_strip_moves() {
            self.move_to_new_strip(tab);
            changed = true;
        }
        // The mark reads as pressed while the overview it zoomed out to is up.
        let overview_open = self.overview.is_open();
        if self.menu_shown.replace(overview_open) != overview_open {
            self.bar.set_overview_open(overview_open);
            changed = true;
        }
        changed |= self.bar.update(state, path);
        changed |= self.follow_bars();
        // Each display's bar keeps its own copy of the layout, and its own clock under it.
        for bar in &mut self.compact {
            changed |= bar.update(state, path);
        }
        changed |= self.emoji.needs_redraw() || self.emoji.needs_animation_frame();
        changed |= self.search.update(state, path);
        changed |= self.follow_notices();
        if let Some(notices) = self.notices.as_mut() {
            changed |= notices.update(state, path);
        }
        changed |= self.switcher.needs_redraw();
        changed |= self.follow_overview();
        self.observe_navigation();
        self.publish_overlays();
        self.advance_hints(ui::time::now());
        changed | !self.asks.borrow().is_empty()
    }

    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &ui::automerge::ObjId) {
        for child in self.children_mut() {
            child.settle(edits, path);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        use taffy::{LengthPercentageAuto, Position, Size};
        let bar = self.bar.build_layout(taffy, theme, map);
        let content = taffy.new_leaf(Style::default()).unwrap_or(bar);
        grow(taffy, content);
        // Over the area under the bar, which stays usable while it is open.
        let overview = self.overview.build_layout(taffy, theme, map);
        let mut style = taffy.style(overview).cloned().unwrap_or_default();
        style.position = Position::Absolute;
        style.inset = taffy::Rect {
            left: LengthPercentageAuto::length(0.0),
            right: LengthPercentageAuto::length(0.0),
            top: LengthPercentageAuto::length(TOPBAR_HEIGHT_PX),
            bottom: LengthPercentageAuto::length(0.0),
        };
        style.size = Size::auto();
        let _ = taffy.set_style(overview, style);
        let about = self.about.build_layout(taffy, theme, map);
        let workspace_menu = self.workspace_menu.build_layout(taffy, theme, map);
        let search = self.search.build_layout(taffy, theme, map);
        // Laid out as the other layers are, so assistive technology and the session's controls
        // reach its button.
        let notice = self.notice.build_layout(taffy, theme, map);
        let mut kids = vec![
            bar,
            content,
            overview,
            about,
            workspace_menu,
            search,
            notice,
        ];
        if let Some(notices) = self.notices.as_mut() {
            kids.push(notices.build_layout(taffy, theme, map));
        }
        // Every other display's bar stands over its own display's top
        // edge, laid out across the shell: it takes no room in the column,
        // its place carrying it where its display is.
        for compact in &mut self.compact {
            let node = compact.build_layout(taffy, theme, map);
            let mut style = taffy.style(node).cloned().unwrap_or_default();
            style.position = Position::Absolute;
            style.inset = taffy::Rect {
                left: LengthPercentageAuto::length(0.0),
                right: LengthPercentageAuto::auto(),
                top: LengthPercentageAuto::length(0.0),
                bottom: LengthPercentageAuto::auto(),
            };
            let _ = taffy.set_style(node, style);
            kids.push(node);
        }
        let node = taffy.widget_node(column(), &kids, map, self);
        fill(taffy, node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let inner = Point::new(self.rect.x0, self.rect.y0);
        let kids = taffy.children(node).unwrap_or_default();
        let [
            bar,
            content,
            overview,
            about,
            workspace_menu,
            search,
            notice,
            compact @ ..,
        ] = &kids[..]
        else {
            return;
        };
        self.bar.apply_layout(taffy, *bar, inner);
        self.content = ui::widget::layout_rect(taffy, *content, inner);
        let under_bar = ui::widget::layout_rect(taffy, *overview, inner);
        let area = self.overview_area().unwrap_or(under_bar);
        self.overview.stand_in(area);
        self.notice.apply_layout(taffy, *notice, inner);
        self.notice
            .place(self.working_display().unwrap_or(self.rect));
        self.about.apply_layout(taffy, *about, inner);
        self.workspace_menu
            .apply_layout(taffy, *workspace_menu, inner);
        self.search.apply_layout(taffy, *search, inner);
        let (notices, compact) = Self::notices_node(self.notices.is_some(), compact);
        if let (Some(layer), Some(node)) = (self.notices.as_mut(), notices) {
            layer.apply_layout(taffy, node, inner);
        }
        for (bar, node) in self.compact.iter_mut().zip(compact) {
            bar.apply_layout(taffy, *node, inner);
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
        // A card opened between two frames is sized and placed in this one.
        self.theme = theme.clone();
        self.navigator.draw(
            &self.navigator_frame,
            scene,
            transform,
            text,
            theme,
            ui::motion::frame_now(),
        );
        // The New strip button is as long as its words measured as it was drawn: it is pressed
        // where it stands from the next event on.
        self.navigator_frame.ribbon.new_len = self.navigator.new_strip_len();
        self.overview
            .draw_child(scene, transform, text, theme, presence);
        for bar in &mut self.compact {
            if bar.presence() != TopBarPresence::Hidden {
                bar.draw_child(scene, transform, text, theme, presence);
            }
        }
        // Last, so what the bar opens below its band stays above the rest.
        self.bar.draw_child(scene, transform, text, theme, presence);
        // The centre hangs under the bar's trailing end, over the windows.
        if let Some(notices) = self.notices.as_mut() {
            notices.draw_child(scene, transform, text, theme, presence);
        }
        // Above all of it: the emoji picker is modal, the one thing the person works with until it
        // closes.
        if self.emoji.is_open() {
            self.emoji.advance_scroll();
            ui::widget::DrawChild::draw_child(
                &mut self.emoji,
                scene,
                transform,
                text,
                theme,
                presence,
            );
        }
        // Drawn or not, the picker is as this frame shows it: one that closed is gone from it.
        self.emoji.mark_drawn();
        // The desktop's own hints, above the windows and the bar they point at: the person learns
        // each control once, where it is done.
        self.hints.draw(
            &self.hints_frame,
            scene,
            transform,
            text,
            theme,
            ui::motion::frame_now(),
        );
        // Settings' Identify, said on each display so the canvas tile and the physical screen carry
        // the same number.
        self.identify
            .draw(&self.identify_displays, scene, transform, text, theme);
        // Topmost: the search palette dims what it stands over, a window in front included, and the
        // card over it.
        self.search
            .draw_child(scene, transform, text, theme, presence);
        // Above even search: Alt+Tab's panel, which is the walk the person is in the middle of and
        // the only thing a press reaches while it is up.
        let host = self.working_display().unwrap_or(self.rect);
        self.notice.place(host);
        self.notice
            .draw_child(scene, transform, text, theme, presence);
        self.switcher.draw(scene, transform, text, theme, host);
        // Over all of it, and in the way of nothing: the keys a held Super reaches take no input,
        // so the switcher a chord of them opens is never under them for long.
        self.peek.draw(scene, transform, text, theme, host);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        let kids = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        let [
            bar,
            _,
            overview,
            about,
            workspace_menu,
            search,
            _notice,
            compact @ ..,
        ] = &kids[..]
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
        if self.switcher.is_up() {
            return self.walk_the_pointer(event, ctx);
        }
        if self.notice_takes(event) {
            *ctx.handled = true;
            return;
        }
        self.hints.heard(event, ui::time::now());
        if self.search_takes(event, ctx, *search) {
            self.publish_overlays();
            return;
        }
        // The emoji picker is modal: its own chord closes it, and every other key and press is the
        // card's, reaching nothing behind it.
        if self.emoji.is_open() {
            let own = matches!(event, WindowEvent::KeyboardInput { event: key, .. }
                if key.state == ElementState::Pressed
                    && page_shortcut_action(
                        &self.shortcuts.borrow(),
                        &key.logical_key,
                        key.physical_key,
                        *ctx.modifiers,
                        self.mac,
                    ) == Some(ShortcutAction::EmojiPicker));
            if own {
                self.close_emoji();
            } else {
                self.emoji.handle_event(event, ctx);
            }
            self.note_emoji_picks();
            *ctx.handled = true;
            self.publish_overlays();
            return;
        }
        // The workspace's menu takes what it covers, the keyboard and the press that dismisses it,
        // in the order the stack stands it.
        if self.overlays.is_open(Part::WorkspaceMenu) {
            self.workspace_menu
                .handle_event(event, &mut ctx.reborrow(ctx.path, *workspace_menu));
        }
        if !*ctx.handled && self.about.is_about_open() {
            self.about
                .handle_event(event, &mut ctx.reborrow(ctx.path, *about));
        }
        let (notices, compact) = Self::notices_node(self.notices.is_some(), compact);
        self.notices_event(event, ctx, notices);
        if *ctx.handled {
            self.publish_overlays();
            return;
        }
        // The strip switcher's name field and keyboard choosing come before the desktop's chords,
        // as any field's typing does.
        if self.navigation_event(event, ctx.scale_factor, *ctx.modifiers) {
            *ctx.handled = true;
            self.publish_overlays();
            return;
        }
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && self.chord(key, ctx.modifiers)
        {
            *ctx.handled = true;
            self.publish_overlays();
            return;
        }
        // The open overview takes the keyboard and what it covers; the bar above it stays usable.
        if self.overview.is_open() {
            self.overview
                .handle_event(event, &mut ctx.reborrow(ctx.path, *overview));
        }
        if !*ctx.handled {
            self.bar
                .handle_event(event, &mut ctx.reborrow(ctx.path, *bar));
        }
        // The other displays' bars take what is theirs on their own display; a hidden one takes
        // nothing.
        if !*ctx.handled {
            for (bar, node) in self.compact.iter_mut().zip(compact) {
                if bar.presence() == TopBarPresence::Hidden {
                    continue;
                }
                bar.handle_event(event, &mut ctx.reborrow(ctx.path, *node));
                if *ctx.handled {
                    break;
                }
            }
        }
        self.publish_overlays();
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = vec![
            &self.bar,
            &self.overview,
            &self.about,
            &self.workspace_menu,
            &self.search,
            &self.notice,
        ];
        children.extend(self.notices.iter().map(|layer| layer as &dyn Widget));
        children.extend(self.compact.iter().map(|bar| bar as &dyn Widget));
        children
    }

    /// Focus reaches the chrome while a surface it draws holds the keyboard,
    /// so a reader placement in the emoji card stays there as the chrome
    /// rebuilds under it rather than falling back to the window.
    fn is_focusable(&self) -> bool {
        self.overlays.target().part().is_some_and(Part::is_chrome)
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    /// The shell's overlays a screen reader can reach: the labels and the card the chrome paints,
    /// plus the children widgets the walk adds below them.
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let mut children: Vec<ui::a11y::AccessNode> = self
            .painted_overlays()
            .into_iter()
            .filter_map(|overlay| {
                self.overlay_accessibility(overlay)
                    .map(|node| node.with_key(overlay.key()))
            })
            .collect();
        // The hint on screen, said in a polite label of its own.
        children.extend(self.hints.accessibility());
        Some(self.accessibility_group().with_children(children))
    }

    fn accessibility_root(&self) -> Option<ui::a11y::AccessNode> {
        Some(self.accessibility_group())
    }

    /// Where the overlay element at `path` is drawn, so assistive activation presses it where the
    /// person sees it.
    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let (&overlay, rest) = path.split_first()?;
        let overlay = *self.painted_overlays().get(overlay)?;
        let mut node = self.overlay_accessibility(overlay)?;
        for &step in rest {
            node = node.children.into_iter().nth(step)?;
        }
        node.bounds
            .map(ui::a11y::AccessBounds::to_rect)
            .filter(|rect| rect.area() > 0.0)
    }

    /// A strip label's semantic target, pressed as a click on the label is;
    /// and search's card, whose rows are the targets its host gave them,
    /// activated as the person's click on the row. The card names no
    /// targets otherwise: its elements are not carried out by a target but
    /// pressed where they are drawn, which the host does through
    /// [`Widget::synthetic_accessibility_rect`].
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        if target.starts_with(crate::strip_switcher::TARGET_PREFIX)
            && matches!(action, ui::a11y::Action::Activate)
        {
            let Some(answer) =
                self.navigator
                    .activate_target(&self.navigator_frame, target, ui::time::now())
            else {
                return false;
            };
            if let Some(navigation) = answer {
                self.apply_navigation(navigation);
            }
            return true;
        }
        // Search is drawn from here, over the windows: a row read and activated there reaches the
        // card as a click on it does.
        self.search
            .perform_semantic_action(target, action, state, path)
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut children: Vec<&mut (dyn Widget + '_)> = vec![
            &mut self.bar,
            &mut self.overview,
            &mut self.about,
            &mut self.workspace_menu,
            &mut self.search,
            &mut self.notice,
        ];
        children.extend(
            self.notices
                .iter_mut()
                .map(|layer| layer as &mut dyn Widget),
        );
        children.extend(self.compact.iter_mut().map(|bar| bar as &mut dyn Widget));
        children
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

    fn take_structure_change(&mut self) -> ui::widget::StructureChange {
        ui::widget::StructureChange::of_children(self.children_mut())
    }

    fn needs_redraw(&self) -> bool {
        self.chrome_needs_redraw()
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if self.chrome_needs_redraw() {
            regions.push(self.rect);
        }
    }

    fn needs_animation_frame(&self) -> bool {
        let now = ui::motion::frame_now();
        self.hints.needs_animation_frame(now)
            || self.emoji.needs_animation_frame()
            || self.peek.is_moving(now)
            || self.identify.is_moving(now)
            || self.navigator.needs_animation_frame(now)
            || self
                .children()
                .iter()
                .any(|child| child.needs_animation_frame())
    }

    /// The bar's own deadline turns its clock over on the minute, on every display that shows one.
    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        [
            &self.bar as &dyn Widget,
            &self.overview,
            &self.about,
            &self.search,
            &self.notice,
        ]
        .into_iter()
        .chain(self.notices.iter().map(|layer| layer as &dyn Widget))
        .chain(self.compact.iter().map(|bar| bar as &dyn Widget))
        .filter_map(wake_deadline)
        .chain(
            self.navigator
                .next_wake_deadline(&self.navigator_frame, ui::time::now()),
        )
        .chain(self.reveals.borrow().deadline())
        .chain(self.peek.next_wake_deadline(ui::time::now()))
        .chain(self.identify.next_wake_deadline(ui::time::now()))
        .chain(self.hints.next_wake_deadline(ui::time::now()))
        .min()
    }

    fn name(&self) -> &'static str {
        "DesktopChrome"
    }

    /// Its bar, the overview, the sheets and menus it lays out, and every display's bar, each
    /// where the column stands it.
    fn html(&self, cx: &mut ui::composition::HtmlCx<'_>) {
        cx.element("div");
        cx.children();
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// The strip navigator's own affordances — the ribbon, each strip's label while the labels
    /// are open, New strip and a strip's Rename and Remove. The navigator paints them into its
    /// own rect instead of composing children, so they are named here for the harnesses and
    /// agents that press what they see.
    fn test_rects(&self) -> Vec<(String, Rect)> {
        self.navigator.test_rects(&self.navigator_frame)
    }

    /// The shell's search field while its card stands over the windows. The card takes the keys
    /// by the shell's own rule, so no focus in the tree reaches the field: named here, the input
    /// method serves it (and the on-screen keyboard shows for it) as it serves any focused field.
    fn ime_field(&self) -> Option<ui::ime::ImeWindowState> {
        if !self.search.is_open() {
            return None;
        }
        let search = self.handle.search_palette();
        let search = search.borrow();
        ui::ime::ImeWindowState::from_widget(search.palette().field_widget())
    }
}
