//! The people overlay and following, in the page.
//!
//! Pinned out of the People popover, the roster floats over the windows
//! (`tilo_desktop::people_overlay`), waiting at an edge as a desk widget
//! does and kept there in the person's settings as the widgets keep
//! theirs. Its peek draws each person's screen from their live layout,
//! the windows on records this page holds pictured live from its own
//! replica. Following someone brings forward, here, the window of what
//! they are in each time it changes, under the same banner the native
//! shell shows; the person's own input takes their view back.
//!
//! The peek's message box talks to the person privately, in Chat's own
//! conversation with them: it is open while their peek is (`ui::person_chat`,
//! the room governor's private door), its last lines show above the box,
//! and a message is sent by Chat's `send-direct-message`, which the page
//! runs ([`OwnAsk`]), the one way a direct message goes.
//!
//! The overlay's code may come in its own module the first time the roster
//! is pinned ([`crate::pinned_roster`]); following and the banner are here
//! from the start.

use settings_store::desk_widgets::{PEOPLE_OVERLAY, PeopleOverlayPrefs};
use tilo_desktop::collaborators::RosterRow;
use tilo_desktop::desk_edges::DockEdge;
use tilo_desktop::follow_banner::{BannerFrame, FollowBanner};
use tilo_desktop::here::Person;
use tilo_desktop::people_overlay::{
    LivePictures, MessageLine, PeerMessages, PeerScreen, PeerWindow, PeopleHit, PeopleOverlayModel,
};
use tilo_desktop_model::{DesktopTree, TabId};
use tilo_desktop_surface::DesktopHandle;
use tilo_relay_protocol::live::LiveRecord;
use ui::kurbo::{Affine, Point, Rect};
use ui::tokens::{EffectiveMotion, MotionSource};
use ui::vello::Scene;
use ui::winit::event::{ElementState, Modifiers, WindowEvent};
use ui::winit::keyboard::{Key, NamedKey};
use ui::{TextRenderer, Theme};

use crate::pinned_roster::PinnedRoster;

/// What the overlay and the banner need from the page this frame.
pub(super) struct Frame<'a> {
    /// The page's desktop, the bar included.
    pub rect: Rect,
    pub rows: &'a [RosterRow],
    /// This page's person, whose own row leads the list.
    pub own: Option<&'a RosterRow>,
    /// Each row's person, `(device, user root)`, both hex, where the host
    /// said whose device it is.
    pub persons: &'a [(String, String)],
    pub people: &'a [Person],
    pub tree: &'a DesktopTree,
    /// The records this page shows, by the window showing each, as the
    /// live layer names them: `(tab, app, record)`.
    pub records: &'a [(TabId, String, String)],
    pub cursor: Option<Point>,
    /// Where the windows' content starts, below their bars: the banner
    /// hangs there, as the page's notices do, clear of the windows' own
    /// controls.
    pub content_top: f64,
}

/// How many of the last lines of a conversation the message box shows.
const SHOWN_LINES: usize = 3;

/// The private conversation with the person whose peek is open, held open
/// while it is.
struct Lane {
    /// The person's roster id.
    id: String,
    /// Their name, to say so if a message to them did not go.
    name: String,
    /// Their public key, which names the conversation.
    key: String,
    /// The open, until the room answers it.
    opening: Option<ui::person_chat::Pending<ui::person_chat::Conversation>>,
    /// The open lane, once the room answered.
    token: Option<uuid::Uuid>,
    /// The room would not open it.
    failed: bool,
}

/// A message on its way, until Chat took or refused it.
struct Sending {
    /// To whom, by name, to say so if it did not go.
    name: String,
    /// The page's answer, by [`OwnAsk::id`].
    id: u32,
}

/// What the person asked of Chat themselves, on a card or in a peek: the
/// page finds Chat's conversation with `to` and hands `id` back to
/// [`take_own_ask`], which is what lets Chat's private action run as the
/// person's own act.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct OwnAsk {
    pub id: u32,
    /// The person (root key, hex).
    pub to: String,
    /// Chat's action on its conversation with them.
    pub action: &'static str,
    /// Its arguments.
    #[serde(skip)]
    pub arguments: serde_json::Map<String, serde_json::Value>,
}

thread_local! {
    /// The last message id handed out, and the page's answers not taken yet.
    static ANSWERS: std::cell::RefCell<(u32, std::collections::HashMap<u32, Result<(), String>>)> =
        std::cell::RefCell::default();
    /// The person's own asks the page has not run yet.
    static OWN_ASKS: std::cell::RefCell<Vec<OwnAsk>> = std::cell::RefCell::default();
}

/// Keep the person's own ask of Chat's `action` on their conversation with
/// `to`, for the page to run.
pub(crate) fn ask_own(
    to: &str,
    action: &'static str,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> OwnAsk {
    let ask = OwnAsk {
        id: next_id(),
        to: to.to_owned(),
        action,
        arguments,
    };
    OWN_ASKS.with(|asks| {
        let mut asks = asks.borrow_mut();
        // A handful at most are ever in flight; an unrun one is not kept forever.
        if asks.len() >= 16 {
            asks.remove(0);
        }
        asks.push(ask.clone());
    });
    ask
}

/// The person's own ask `id`, once: none for an id they never made.
pub(crate) fn take_own_ask(id: u32) -> Option<OwnAsk> {
    OWN_ASKS.with(|asks| {
        let mut asks = asks.borrow_mut();
        let at = asks.iter().position(|ask| ask.id == id)?;
        Some(asks.remove(at))
    })
}

/// What came of the message `id`: Chat took it, or why not.
pub(crate) fn answer(id: u32, result: Result<(), String>) {
    ANSWERS.with(|answers| {
        answers.borrow_mut().1.insert(id, result);
    });
}

fn next_id() -> u32 {
    ANSWERS.with(|answers| {
        let mut answers = answers.borrow_mut();
        answers.0 = answers.0.wrapping_add(1);
        answers.0
    })
}

fn take_answer(id: u32) -> Option<Result<(), String>> {
    ANSWERS.with(|answers| answers.borrow_mut().1.remove(&id))
}

/// What following last brought forward.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Tracked {
    Window(TabId),
    App(String),
}

/// The overlay, following and the banner across frames.
pub(super) struct PagePeople {
    /// The pinned roster, once its code is in and it was first pinned.
    overlay: Option<Box<dyn PinnedRoster>>,
    /// Whether the roster is pinned (shown once its code is in).
    pinned: bool,
    edge: DockEdge,
    banner: FollowBanner,
    /// Whom this page follows, by roster id.
    followed: Option<String>,
    /// What was last brought forward for them: their window here, or
    /// their app where no window here shows their record.
    tracked: Option<Tracked>,
    motion: EffectiveMotion,
    /// The conversation with whom the peek shows.
    lane: Option<Lane>,
    /// Opens a later peek replaced, each released once it answers.
    abandoned: Vec<(
        String,
        ui::person_chat::Pending<ui::person_chat::Conversation>,
    )>,
    sending: Option<Sending>,
    /// A message that did not go: to whom, and why.
    failed_send: Option<(String, String)>,
    /// The desktop it was last drawn over.
    rect: Rect,
    /// What assistive technology pressed since the host last looked.
    pressed: Vec<PeopleHit>,
    /// The bar's people panel as a reader finds it while it is open (the
    /// host paints it), and the control pressed on it since the host last
    /// looked, by its target.
    panel: Option<ui::a11y::AccessNode>,
    panel_pressed: Option<String>,
}

impl PagePeople {
    /// As the person's settings left it: pinned or not, at its edge.
    pub(super) fn new() -> Self {
        let prefs = PEOPLE_OVERLAY.get();
        Self {
            overlay: None,
            pinned: prefs.pinned,
            edge: prefs.edge,
            banner: FollowBanner::default(),
            followed: None,
            tracked: None,
            motion: EffectiveMotion::default(),
            lane: None,
            abandoned: Vec::new(),
            sending: None,
            failed_send: None,
            rect: Rect::ZERO,
            pressed: Vec::new(),
            panel: None,
            panel_pressed: None,
        }
    }

    /// What the bar's people panel shows a reader now; `None` once closed.
    pub(super) fn set_panel(&mut self, panel: Option<ui::a11y::AccessNode>) {
        self.panel = panel;
    }

    /// The panel control a reader pressed since the last call, by target.
    pub(super) fn take_panel_pressed(&mut self) -> Option<String> {
        self.panel_pressed.take()
    }

    /// What assistive technology pressed on the overlay since the last
    /// call, for the host to apply as it applies a press.
    pub(super) fn take_pressed(&mut self) -> Vec<PeopleHit> {
        std::mem::take(&mut self.pressed)
    }

    /// Hold the conversation with `peeked` open, by roster id (none when
    /// no peek is open), releasing the one before.
    fn follow_lane(&mut self, peeked: Option<&RosterRow>, persons: &[(String, String)]) {
        self.abandoned
            .retain(|(key, opening)| match opening.take() {
                Some(Ok(_)) => {
                    ui::person_chat::close(key);
                    false
                }
                Some(Err(_)) => false,
                None => true,
            });
        let wanted = peeked.and_then(|row| Some((row, person_key(row, persons)?)));
        if self
            .lane
            .as_ref()
            .map(|lane| (lane.id.as_str(), lane.key.as_str()))
            != wanted
                .as_ref()
                .map(|(row, key)| (row.id.as_str(), key.as_str()))
        {
            if let Some(lane) = self.lane.take() {
                if let Some(token) = lane.token {
                    ui::person_chat::close_current(&lane.key, token);
                }
                if let Some(opening) = lane.opening {
                    self.abandoned.push((lane.key, opening));
                }
            }
            self.lane = wanted.map(|(row, key)| Lane {
                opening: Some(ui::person_chat::open(&key)),
                id: row.id.clone(),
                name: row.label.clone(),
                key,
                token: None,
                failed: false,
            });
        }
        if let Some(lane) = self.lane.as_mut()
            && let Some(answer) = lane
                .opening
                .as_ref()
                .and_then(ui::person_chat::Pending::take)
        {
            lane.opening = None;
            match answer {
                Ok(_) => lane.token = ui::person_chat::subscription_token(&lane.key),
                Err(error) => {
                    web_sys::console::warn_1(
                        &format!("opening a private conversation: {error}").into(),
                    );
                    lane.failed = true;
                }
            }
        }
        // The room ended the lane (a member left, a certificate lapsed).
        if let Some(lane) = self.lane.as_mut()
            && let Some(token) = lane.token
            && ui::person_chat::subscription_token(&lane.key) != Some(token)
        {
            lane.token = None;
            lane.failed = true;
        }
    }

    /// The last lines of the open conversation, and why the box cannot
    /// send instead of offering to, if it cannot.
    fn lane_messages(&self) -> (Vec<PeerMessages>, Option<String>) {
        let Some(lane) = &self.lane else {
            return (Vec::new(), None);
        };
        let unavailable = (lane.failed || lane.opening.is_none() && lane.token.is_none())
            .then(|| tilo_desktop::i18n::t("people-overlay-messages-unavailable"));
        let lines = ui::person_chat::snapshot(&lane.key)
            .map(|conversation| {
                let start = conversation.messages.len().saturating_sub(SHOWN_LINES);
                conversation
                    .messages
                    .iter()
                    .skip(start)
                    .map(|message| MessageLine {
                        mine: message.author != lane.key,
                        text: message.text.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        (
            vec![PeerMessages {
                id: lane.id.clone(),
                lines,
            }],
            unavailable,
        )
    }

    /// Send `text` to the person with roster id `to`, whose peek is open:
    /// what the page sends by Chat's action, if it may go now.
    pub(super) fn send(&mut self, to: &str, text: &str) -> Option<OwnAsk> {
        let Some(lane) = self.lane.as_ref().filter(|lane| lane.id == to) else {
            self.failed_send = Some((
                to.to_owned(),
                tilo_desktop::i18n::t("people-overlay-messages-unavailable"),
            ));
            return None;
        };
        if self.sending.is_some() {
            return None;
        }
        let mut arguments = serde_json::Map::new();
        arguments.insert("text".to_owned(), text.into());
        let ask = ask_own(&lane.key, "send-direct-message", arguments);
        self.sending = Some(Sending {
            name: lane.name.clone(),
            id: ask.id,
        });
        Some(ask)
    }

    /// A message that did not go since the last ask: to whom, and why.
    pub(super) fn take_failed_send(&mut self) -> Option<(String, String)> {
        self.failed_send.take()
    }

    pub(super) fn is_pinned(&self) -> bool {
        self.pinned
    }

    /// The roster while pinned: made the first time its code is in (asking
    /// for it until then).
    fn roster(&mut self) -> Option<&mut (dyn PinnedRoster + 'static)> {
        if self.overlay.is_none() && self.pinned {
            let make = crate::pinned_roster::code()?;
            let mut overlay = make(self.edge);
            overlay.show(true);
            self.overlay = Some(overlay);
        }
        self.overlay.as_deref_mut()
    }

    /// The roster as it is now, if made.
    fn made(&self) -> Option<&dyn PinnedRoster> {
        self.overlay.as_deref()
    }

    /// Pin the roster to the desk, or put it back in the bar; kept.
    pub(super) fn pin(&mut self, pinned: bool) {
        self.pinned = pinned;
        if let Some(overlay) = self.overlay.as_deref_mut() {
            overlay.show(pinned);
        }
        if let Err(error) = PEOPLE_OVERLAY.update(|prefs| PeopleOverlayPrefs { pinned, ..prefs }) {
            web_sys::console::warn_1(&format!("keeping the people overlay's pin: {error}").into());
        }
    }

    pub(super) fn followed(&self) -> Option<&str> {
        self.followed.as_deref()
    }

    /// Follow `id`, or stop following them. The banner finds their row in
    /// the next frame's roster.
    pub(super) fn toggle_follow(&mut self, id: &str) {
        if self.followed.as_deref() == Some(id) {
            self.stop_following(false);
            return;
        }
        self.followed = Some(id.to_owned());
        self.tracked = None;
        self.banner
            .started(None, self.motion, MotionSource::Pointer, ui::time::now());
    }

    /// Stop following; `by_own_input` when the person took their view back
    /// by doing something of their own (the banner says so a moment).
    pub(super) fn stop_following(&mut self, by_own_input: bool) {
        if self.followed.take().is_none() {
            return;
        }
        self.tracked = None;
        self.banner.ended(
            by_own_input,
            self.motion,
            MotionSource::Pointer,
            ui::time::now(),
        );
    }

    /// While following: bring forward the window of what they are in, each
    /// time that changes; where no window here shows it (a room whose
    /// layout is each person's own), `open_app` shows their app here.
    /// Returns whether anything was brought forward.
    pub(super) fn track(
        &mut self,
        people: &[Person],
        handle: &DesktopHandle,
        open_app: &dyn Fn(&str) -> bool,
    ) -> bool {
        let Some(id) = self.followed.as_deref() else {
            return false;
        };
        let Some(person) = people.iter().find(|person| person.id == id) else {
            // They left: following them ends with them.
            self.stop_following(false);
            return false;
        };
        let focused_app = person
            .screen
            .as_ref()
            .and_then(|screen| screen.windows.iter().find(|window| window.focused))
            .map(|window| window.app.clone());
        let want = match (person.tab, focused_app) {
            (Some(tab), _) => Tracked::Window(tab),
            (None, Some(app)) => Tracked::App(app),
            (None, None) => return false,
        };
        if self.tracked.as_ref() == Some(&want) {
            return false;
        }
        let brought = match &want {
            Tracked::Window(tab) => handle.focus_tab(*tab),
            Tracked::App(app) => open_app(app),
        };
        self.tracked = Some(want);
        brought
    }

    pub(super) fn needs_animation_frame(&self) -> bool {
        self.made().is_some_and(PinnedRoster::needs_animation_frame)
            || self.banner.is_moving(ui::time::now())
    }

    pub(super) fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        [
            self.made().and_then(PinnedRoster::next_wake_deadline),
            self.banner.next_wake_deadline(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Whether `point` is on the overlay, its peek, or the banner.
    pub(super) fn contains(&self, point: Point) -> bool {
        self.made().is_some_and(|overlay| overlay.contains(point))
            || self.banner.chip_rect().contains(point)
    }

    pub(super) fn wants_pointer_capture(&self) -> bool {
        self.made().is_some_and(PinnedRoster::wants_pointer_capture)
    }

    pub(super) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        frame: &Frame<'_>,
    ) {
        self.motion = theme.motion();
        self.rect = frame.rect;
        let bar = f64::from(tilo_desktop::topbar::topbar_height(theme));
        let below_bar = Rect::new(
            frame.rect.x0,
            frame.rect.y0 + bar,
            frame.rect.x1,
            frame.rect.y1,
        );
        let following = self
            .followed
            .as_deref()
            .map(|id| frame.rows.iter().find(|row| row.id == id).cloned());
        let kept = self.banner.draw(
            scene,
            transform,
            text,
            theme,
            BannerFrame {
                work_area: below_bar,
                viewport: Rect::new(
                    below_bar.x0,
                    below_bar.y0.max(frame.content_top),
                    below_bar.x1,
                    below_bar.y1,
                ),
                window_bars: &[],
                cursor: frame.cursor,
                motion: self.motion,
            },
            following,
        );
        if !kept {
            self.stop_following(false);
        }
        let followed = self.followed.clone();
        let peeked = self
            .made()
            .filter(|overlay| overlay.is_visible())
            .and_then(PinnedRoster::peeked)
            .and_then(|id| frame.rows.iter().find(|row| row.id == id));
        self.follow_lane(peeked, frame.persons);
        refresh_contacts(frame.persons);
        let (messages, unavailable) = self.lane_messages();
        let sent = match self
            .sending
            .as_ref()
            .and_then(|sending| take_answer(sending.id))
        {
            Some(answer) => {
                let sending = self.sending.take();
                match answer {
                    Ok(_) => true,
                    Err(reason) => {
                        self.failed_send = sending.map(|sending| (sending.name, reason));
                        false
                    }
                }
            }
            None => false,
        };
        let Some(overlay) = self.roster().filter(|overlay| overlay.is_visible()) else {
            ui::window_pictures::want(&[]);
            return;
        };
        if sent {
            overlay.sent();
        }
        let screens = screens(frame);
        // Live pictures only of the windows the open peek shows.
        let wanted: Vec<u64> = overlay
            .peeked()
            .and_then(|id| screens.iter().find(|screen| screen.id == id))
            .into_iter()
            .flat_map(|screen| &screen.windows)
            .filter_map(|window| window.picture)
            .collect();
        ui::window_pictures::want(&wanted);
        overlay.draw(
            scene,
            transform,
            text,
            theme,
            &PeopleOverlayModel {
                host: below_bar,
                occupied: &[],
                rows: frame.rows,
                screens: &screens,
                messages: &messages,
                followed_id: followed.as_deref(),
                messaging_unavailable: unavailable.as_deref(),
                own: frame.own,
            },
            &mut LivePictures,
        );
    }

    /// Route `event`; marks it handled when it was the overlay's or the
    /// banner's. Returns what the host applies.
    pub(super) fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        cursor: Option<Point>,
        handled: &mut bool,
    ) -> Option<PeopleHit> {
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            ..
        } = event
            && let Some(point) = cursor
            && self.banner.stop_rect().contains(point)
        {
            self.stop_following(false);
            *handled = true;
            return None;
        }
        let hit = self
            .overlay
            .as_deref_mut()
            .and_then(|overlay| overlay.handle_event(event, scale, handled));
        if let Some(PeopleHit::Moved(edge)) = &hit {
            self.edge = *edge;
        }
        if let Some(PeopleHit::Moved(edge)) = &hit
            && let Err(error) = PEOPLE_OVERLAY.update(|prefs| PeopleOverlayPrefs {
                edge: *edge,
                ..prefs
            })
        {
            web_sys::console::warn_1(&format!("keeping the people overlay's edge: {error}").into());
        }
        hit
    }

    /// A key, before the desktop's: the message box's while it holds the
    /// keyboard; while following, Escape stops and any other key takes the
    /// view back. Returns whether the key was taken.
    pub(super) fn key(
        &mut self,
        event: &WindowEvent,
        modifiers: &Modifiers,
    ) -> (bool, Option<PeopleHit>) {
        let WindowEvent::KeyboardInput { event: key, .. } = event else {
            return (false, None);
        };
        if key.state != ElementState::Pressed {
            return (self.made().is_some_and(PinnedRoster::composing), None);
        }
        if let Some(hit) = self
            .overlay
            .as_deref_mut()
            .and_then(|overlay| overlay.key(&key.logical_key, modifiers.state()))
        {
            return (true, hit);
        }
        if self.followed.is_some() {
            let escape = matches!(key.logical_key, Key::Named(NamedKey::Escape));
            self.stop_following(!escape);
            return (escape, None);
        }
        (false, None)
    }

    /// A press on the desktop, not on the overlay or the banner: the
    /// person going back to their own view.
    pub(super) fn pressed_elsewhere(&mut self) {
        self.stop_following(true);
    }
}

/// The overlay and the banner as assistive technology reads them. The
/// host paints them and routes their pointer and keys itself; as a widget
/// they only say what is there and take what a reader presses.
impl ui::widget::Widget for PagePeople {
    fn build_layout(
        &mut self,
        taffy: &mut ui::taffy::TaffyTree<()>,
        _theme: &Theme,
        map: &mut std::collections::HashMap<ui::taffy::NodeId, *mut dyn ui::widget::Widget>,
    ) -> ui::taffy::NodeId {
        use ui::widget::TaffyTreeExt as _;
        let style = ui::taffy::Style {
            position: ui::taffy::Position::Absolute,
            ..Default::default()
        };
        taffy.widget_node(style, &[], map, self)
    }

    fn apply_layout(
        &mut self,
        _taffy: &ui::taffy::TaffyTree<()>,
        _node: ui::taffy::NodeId,
        _origin: Point,
    ) {
    }

    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text: &mut TextRenderer,
        _theme: &Theme,
        _presence: &ui::presence::PresenceCtx,
    ) {
    }

    fn handle_event(&mut self, _event: &WindowEvent, _ctx: &mut ui::widget::EventCtx<'_>) {}

    fn children(&self) -> Vec<&dyn ui::widget::Widget> {
        Vec::new()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn ui::widget::Widget + '_)> {
        Vec::new()
    }

    fn needs_redraw(&self) -> bool {
        false
    }

    fn is_layout_dirty(&self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "PeopleOverlay"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// The bar's people panel while open, the pinned roster (who is here,
    /// each with their controls) and the banner of whom this view follows,
    /// with its Stop.
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, Role};
        let mut children: Vec<AccessNode> = self.panel.iter().cloned().collect();
        if let Some(overlay) = self.made().filter(|overlay| overlay.is_visible()) {
            children.push(
                AccessNode::new(Role::Group)
                    .with_hashed_key("people-overlay")
                    .with_name(tilo_desktop_surface::i18n::t("a11y-people-overlay"))
                    .with_children(overlay.accessibility_children()),
            );
        }
        children.extend(self.banner.accessibility());
        if children.is_empty() {
            return None;
        }
        Some(AccessNode::new(Role::Group).with_children(children))
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        _state: &mut ui::widget::Edits<'_>,
        _path: &ui::widget::ModelPath,
    ) -> bool {
        if target.starts_with("people:") {
            if matches!(action, ui::a11y::Action::Activate) {
                self.panel_pressed = Some(target.to_owned());
            }
            return self.panel.is_some();
        }
        if target == tilo_desktop::follow_banner::STOP_TARGET {
            if matches!(action, ui::a11y::Action::Activate) {
                self.stop_following(false);
            }
            return true;
        }
        let Some(overlay) = self.overlay.as_deref_mut() else {
            return false;
        };
        let ours = overlay
            .accessibility_children()
            .iter()
            .any(|node| node.action_target.as_deref() == Some(target));
        if let Some(hit) = overlay.perform_semantic_action(action, target) {
            self.pressed.push(hit);
        }
        ours
    }
}

/// Someone is here whom the room's last list of people did not have: ask
/// for it again, so Chat's direct conversations list them too. The native
/// host hears of new members from the room itself; a page sees them here.
fn refresh_contacts(persons: &[(String, String)]) {
    let contacts = ui::person_chat::contacts_snapshot();
    let own = ui::local_identity::load().map(|own| hex::encode(own.group_public_key));
    let unknown = persons.iter().any(|(_, user)| {
        own.as_deref() != Some(user.as_str())
            && !contacts
                .iter()
                .any(|contact| contact.public_key.eq_ignore_ascii_case(user))
    });
    if unknown {
        ui::person_chat::refresh_contacts();
    }
}

/// The person `row` is a device of: as the host said, else the contact
/// who goes by its name.
fn person_key(row: &RosterRow, persons: &[(String, String)]) -> Option<String> {
    if let Some((_, user)) = persons.iter().find(|(device, _)| *device == row.id) {
        return Some(user.to_ascii_lowercase());
    }
    ui::person_chat::contacts()
        .into_iter()
        .find(|contact| contact.display_name.eq_ignore_ascii_case(&row.label))
        .map(|contact| contact.public_key)
}

/// Each person's screen as their live layout gives it, each window matched
/// against the records this page holds: a record held here is shared with
/// this person, and its window here stands for theirs (its title, its live
/// picture); any other shows as its app alone.
fn screens(frame: &Frame<'_>) -> Vec<PeerScreen> {
    frame
        .people
        .iter()
        .filter_map(|person| {
            let screen = person.screen.as_ref()?;
            let (width, height) = screen.size;
            let windows = screen
                .windows
                .iter()
                .map(|window| {
                    let here = frame.records.iter().find(|(_, app, record)| {
                        *app == window.app
                            && LiveRecord {
                                app: app.clone(),
                                record: record.clone(),
                            }
                            .short_digest()
                                == window.record
                    });
                    let tab = here.map(|(tab, _, _)| *tab);
                    PeerWindow {
                        rect: window.rect,
                        app: window.app.clone(),
                        app_name: crate::desktop::known_application(&window.app)
                            .map_or_else(|| window.app.clone(), str::to_owned),
                        title: tab.and_then(|tab| tab_title(frame.tree, tab)),
                        picture: tab.and_then(|tab| group_of(frame.tree, tab)),
                        focused: window.focused,
                    }
                })
                .collect();
            Some(PeerScreen {
                id: person.id.clone(),
                aspect: if height > 0.0 {
                    width / height
                } else {
                    16.0 / 10.0
                },
                windows,
            })
        })
        .collect()
}

fn tab_title(tree: &DesktopTree, tab: TabId) -> Option<String> {
    tree.workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .find(|t| t.id == tab)
        .map(|t| t.title.clone())
        .filter(|title| !title.trim().is_empty())
}

/// The window group `tab` is in, whose live picture stands for it.
fn group_of(tree: &DesktopTree, tab: TabId) -> Option<u64> {
    tree.workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .find(|group| group.tabs.iter().any(|t| t.id == tab))
        .map(|group| group.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop::here::{Screen, ScreenWindow};
    use tilo_desktop_model::IdMinter;

    /// A window on someone's screen showing a record this page shows too
    /// stands for theirs, with its title and live picture; any other shows
    /// as its app and nothing more.
    #[test]
    fn only_what_this_page_holds_is_named_and_pictured() {
        let mut minter = IdMinter::new(3);
        let tab = tilo_desktop_model::Tab {
            id: minter.mint(),
            title: "Plans".into(),
            tile: tilo_desktop_model::TileContent::LocalApp {
                app_id: "so.tilo.notes".into(),
                label: "Notes".into(),
                note: String::new(),
            },
            app_title: None,
        };
        let tab_id = tab.id;
        let tree = tilo_desktop_surface::seed_tree(&mut minter, "Lounge", vec![tab]);
        let digest = |record: &str| {
            LiveRecord {
                app: "so.tilo.notes".into(),
                record: record.into(),
            }
            .short_digest()
        };
        let window = |record: &str, focused| ScreenWindow {
            app: "so.tilo.notes".into(),
            record: digest(record),
            rect: Rect::new(0.0, 0.0, 0.5, 1.0),
            focused,
        };
        let person = Person {
            id: "ada".into(),
            user: None,
            label: "Ada".into(),
            rgb: (1, 2, 3),
            agent: false,
            tab: None,
            place: None,
            status: None,
            recent: None,
            screen: Some(Screen {
                size: (1600.0, 1000.0),
                windows: vec![window("plans", true), window("diary", false)],
            }),
        };
        let records = [(tab_id, "so.tilo.notes".to_owned(), "plans".to_owned())];
        let screens = screens(&Frame {
            rect: Rect::new(0.0, 0.0, 1440.0, 900.0),
            rows: &[],
            people: std::slice::from_ref(&person),
            tree: &tree,
            records: &records,
            cursor: None,
            content_top: 0.0,
        });
        let [screen] = screens.as_slice() else {
            panic!("one screen: {screens:?}");
        };
        assert!((screen.aspect - 1.6).abs() < 1e-9);
        let [shared, private] = screen.windows.as_slice() else {
            panic!("two windows: {:?}", screen.windows);
        };
        assert_eq!(shared.title.as_deref(), Some("Plans"));
        assert_eq!(shared.picture, group_of(&tree, tab_id));
        assert!(shared.picture.is_some() && shared.focused);
        assert!(!private.shared(), "a record this page does not hold");
        assert_eq!((private.title.as_deref(), private.picture), (None, None));
        assert_eq!(private.app, "so.tilo.notes", "its app is all it says");
    }
}
