//! `MediaTransport`: what is playing and the controls that steer it, as one
//! band of real controls: the art and the song, like and lyrics, shuffle,
//! previous, play/pause, next and repeat over the scrubber, the queue and
//! the volume.
//!
//! Playback belongs to the window, not to the page open, so the transport
//! is meant for [`NavShell::bottom_band`](crate::widget::NavShell::bottom_band):
//! it spans the window under every column.
//!
//! The app's player is the one truth. It declares what plays through a
//! [`TransportFeed`] ([`TransportFeed::show`], cheap when nothing changed)
//! and hands in the actions the controls invoke ([`TransportActions`]): play
//! and pause, previous and next, shuffle, repeat, like, mute, the scrubber
//! and the volume each run one of the app's own resource actions, as an
//! agent invoking it would, so what the person presses is never a callback
//! beside the app's actions. A control the app hands no action for is not
//! there. What only changes what the window shows (the queue, the lyrics,
//! the compact form's More menu, the art) and a swipe across the band come
//! back as [`TransportEvent`]s ([`TransportFeed::take_events`]). The
//! transport keeps nothing of its own but where the knob is while a finger
//! or the pointer holds it.
//!
//! The transport has two forms, chosen by its width:
//!
//! * **Wide**: the song on the left, the transport centred over the
//!   scrubber, the queue and the volume on the right.
//! * **Compact** (narrower than two large media cards): the song beside
//!   previous, play and next, the volume and a More button that the app
//!   answers with a menu holding what the wide form shows inline.
//!
//! The volume is reachable at every width: a slider beside the mute toggle
//! where it fits, else a speaker button whose popover holds the slider.
//! The art opens what plays when the app says what that is
//! ([`TransportState::opens`]), and a horizontal swipe across the band,
//! by finger or pointer, skips to the next or previous song.
//!
//! A control the app leaves out (`None`: a local file can't be liked) is not
//! there at all, and everything rests disabled while nothing can play.
//!
//! ```ignore
//! let transport = TransportFeed::new();
//! let actions = TransportActions { play_pause: Some(play_pause), ..TransportActions::default() };
//! let shell = NavShell::new().bottom_band(Box::new(MediaTransport::new(&transport, actions)));
//! // each sync:
//! transport.show(TransportState { title, playing, .. });
//! for event in transport.take_events() { /* show the queue, the lyrics, a menu */ }
//! ```

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::ImageData;

use crate::icons::Icon;
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::media_card::MediaSize;
use crate::widget::now_playing::{Song, SongFace};
use tilo_actions::ActionBinding;

use crate::widget::{
    Button, ButtonFace, ControlSize, Element, Manual, ManualContext, PanelAlign, PopoverSide,
    Slider, SliderFace, TaffyTreeExt, TransportControl, TransportFace, When, Widget,
    WidgetLayoutError,
};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

/// The song's art in the transport.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum TransportArt {
    /// No song: no art.
    #[default]
    None,
    /// A song without a picture (yet): the generated cover seeded with
    /// [`TransportState::art_name`].
    Placeholder,
    /// The cover.
    Image(ImageData),
}

/// What a repeat control repeats.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TransportRepeat {
    #[default]
    Off,
    All,
    One,
}

/// What plays, as the app's player has it: everything the transport shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TransportState {
    /// The song's name, or what the transport says with nothing playing.
    pub title: String,
    /// Who it is by, or why it can't play.
    pub subtitle: String,
    /// The subtitle says why it can't play: it reads as an error.
    pub error: bool,
    pub art: TransportArt,
    /// The name the placeholder cover takes its initials and colour from
    /// (the album).
    pub art_name: String,
    /// What a press on the art opens ("Open album", "Show lyrics"), as it
    /// is announced; `None` leaves the art a picture
    /// ([`TransportEvent::OpenPlaying`]).
    pub opens: Option<String>,
    pub playing: bool,
    /// Nothing can play: every control rests disabled.
    pub idle: bool,
    pub position_ms: f64,
    pub duration_ms: f64,
    /// The volume, `0..=1`; zero is muted.
    pub volume: f64,
    /// Each optional control is left out while `None`, and shown on or off.
    pub shuffle: Option<bool>,
    pub repeat: Option<TransportRepeat>,
    pub liked: Option<bool>,
    pub lyrics: Option<bool>,
    pub queue: Option<bool>,
    /// The compact form has a More button, which the app answers with a
    /// menu ([`TransportEvent::More`]).
    pub more: bool,
}

/// The app's actions the transport's controls invoke, each through the
/// resource/action door. A control whose action is `None` is left out.
#[derive(Clone, Default)]
pub struct TransportActions {
    pub play_pause: Option<ActionBinding<()>>,
    pub previous: Option<ActionBinding<()>>,
    pub next: Option<ActionBinding<()>>,
    pub shuffle: Option<ActionBinding<()>>,
    pub repeat: Option<ActionBinding<()>>,
    /// Like or unlike what plays.
    pub like: Option<ActionBinding<()>>,
    pub mute: Option<ActionBinding<()>>,
    /// Go to a fraction (`0..=1`) of the song, once the scrubber is let go.
    pub seek: Option<ActionBinding<f64>>,
    /// Set the volume, `0..=1`.
    pub volume: Option<ActionBinding<f64>>,
}

/// What the person asked for that changes what the window shows, rather
/// than what plays ([`TransportActions`]), and the swipe across the band.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TransportEvent {
    /// A swipe across the band, towards the previous song.
    Previous,
    /// A swipe across the band, towards the next song.
    Next,
    Lyrics,
    Queue,
    /// Open the More menu under the button, at this rect in window
    /// coordinates.
    More(Rect),
    /// Open what plays: the art was pressed ([`TransportState::opens`]).
    OpenPlaying,
}

/// Which form the transport takes at its width.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Form {
    Compact,
    /// Wide, with the volume a mute toggle only.
    #[default]
    Wide,
    /// Wide, with the volume slider beside the mute toggle.
    WideWithVolume,
}

/// The sizes a layout of the transport is made of, from the theme and the
/// text measured in it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Metrics {
    control: f64,
    play: f64,
    skip: f64,
    caption: f64,
    song: f64,
    time: f64,
    large: f64,
    small: f64,
    group: f64,
    section: f64,
    sibling: f64,
    related: f64,
    tight: f64,
    /// How far a drag across the band travels before it skips a song: two
    /// pointer targets.
    swipe: f64,
}

impl Metrics {
    fn of(theme: &Theme) -> Self {
        let rhythm = &theme.tokens.spacing.rhythm;
        let line = |role| f64::from(theme.text(role).line_height_px());
        Self {
            control: ButtonFace::height(ControlSize::Regular, theme),
            play: TransportFace::side(TransportControl::PlayPause { playing: false }, theme),
            skip: TransportFace::side(TransportControl::Next, theme),
            caption: line(TextRole::Caption),
            song: line(TextRole::Body) + line(TextRole::Caption),
            time: 0.0,
            large: MediaSize::Large.extent(theme),
            small: MediaSize::Small.extent(theme),
            group: f64::from(rhythm.group),
            section: f64::from(rhythm.section),
            sibling: f64::from(rhythm.sibling),
            related: f64::from(rhythm.related),
            tight: f64::from(rhythm.tight),
            swipe: 2.0 * f64::from(theme.tokens.metrics.min_pointer_target),
        }
    }

    /// The scrubber's row: a caption line, never under a comfortable
    /// pointer target.
    fn scrub(&self) -> f64 {
        self.caption.max(self.section + self.related)
    }

    fn form(&self, width: f64) -> Form {
        if width < 2.0 * self.large {
            Form::Compact
        } else if width < 3.0 * self.large {
            Form::Wide
        } else {
            Form::WideWithVolume
        }
    }

    fn top(&self, form: Form) -> f64 {
        let control = self.control.max(self.play);
        if form == Form::Compact {
            control.max(self.song)
        } else {
            control
        }
    }

    fn height(&self, form: Form) -> f64 {
        2.0 * self.sibling + self.top(form) + self.tight + self.scrub()
    }
}

/// What the app and the transport's controls share.
#[derive(Default)]
struct Shared {
    state: RefCell<TransportState>,
    revision: Cell<u64>,
    events: RefCell<Vec<TransportEvent>>,
    metrics: Cell<Metrics>,
    form: Cell<Form>,
    /// The More button, in the transport's own coordinates.
    more: Cell<Rect>,
    /// The transport's origin in the window at its last layout.
    origin: Cell<Point>,
    /// Where each child stands (zero while it isn't there), in the
    /// transport's own coordinates.
    placed: Cell<[Rect; CHILDREN]>,
    /// Which children have what they invoke: an action the app handed in,
    /// or a request of the window's own.
    handed: Cell<[bool; CHILDREN]>,
    /// The song as the art and the lines show it, cross-fading to the next.
    song: RefCell<SongFace>,
    /// Where the scrubber's knob is held (`0..=1`), while a finger or the
    /// pointer holds it.
    scrub: Cell<Option<f64>>,
    /// A page showing the song large has the cover in flight: the band
    /// leaves its place empty until it lands back.
    art_lent: Cell<bool>,
}

impl Shared {
    fn push(&self, event: TransportEvent) {
        self.events.borrow_mut().push(event);
    }

    /// Show the declared song from the frame at `now`.
    fn follow_song(&self, theme: &Theme, now: crate::time::Instant) {
        let song = song_of(&self.state.borrow());
        self.song.borrow_mut().show(&song, theme.motion(), now);
    }

    fn song_moving(&self) -> bool {
        self.song.borrow().is_moving(crate::time::now())
    }

    /// The time a caption shows: the knob's while it is held.
    fn caption(&self, side: Caption) -> String {
        let state = self.state.borrow();
        if state.idle {
            return String::new();
        }
        match side {
            Caption::Elapsed => time(
                self.scrub
                    .get()
                    .map_or(state.position_ms, |held| held * state.duration_ms),
            ),
            Caption::Total => time(state.duration_ms),
        }
    }
}

/// The song `state` declares, as the art and the lines show it.
fn song_of(state: &TransportState) -> Song {
    Song {
        title: state.title.clone(),
        subtitle: state.subtitle.clone(),
        detail: String::new(),
        error: state.error,
        art: state.art.clone(),
        art_name: state.art_name.clone(),
    }
}

/// Which of the scrubber's times a caption shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Caption {
    Elapsed,
    Total,
}

/// Where an app declares what plays and takes what the person asked for.
/// Clones share one transport.
#[derive(Clone, Default)]
pub struct TransportFeed(Rc<Shared>);

impl TransportFeed {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Show `state`. Cheap when it is what the transport shows already.
    pub fn show(&self, state: TransportState) {
        if *self.0.state.borrow() == state {
            return;
        }
        *self.0.state.borrow_mut() = state;
        self.0.revision.set(self.0.revision.get().wrapping_add(1));
    }

    /// What the transport shows.
    #[must_use]
    pub fn shown(&self) -> TransportState {
        self.0.state.borrow().clone()
    }

    /// What the person asked for since the last call, in order.
    #[must_use]
    pub fn take_events(&self) -> Vec<TransportEvent> {
        std::mem::take(&mut *self.0.events.borrow_mut())
    }

    /// Where the art stands in the window, while it is there: what a page
    /// showing the song large zooms from and back to.
    #[must_use]
    pub fn art_rect(&self) -> Option<Rect> {
        let art = self.0.placed.get()[ART];
        (art.width() > 0.0).then(|| art + self.0.origin.get().to_vec2())
    }

    /// The corners of the band's art.
    #[must_use]
    pub fn art_radius(theme: &Theme) -> f64 {
        f64::from(theme.tokens.radii.control)
    }

    /// The song as the band shows it.
    #[must_use]
    pub fn song(&self) -> Song {
        song_of(&self.0.state.borrow())
    }

    /// Whether a page has the cover in flight to or from it: while it does,
    /// the band leaves the art's place empty, so one cover moves, not two.
    pub fn lend_art(&self, lent: bool) {
        if self.0.art_lent.replace(lent) != lent {
            self.0.revision.set(self.0.revision.get().wrapping_add(1));
        }
    }
}

/// A control whose look follows the declared state: `apply` sets it from
/// the state whenever the state changes, and says whether that changed
/// what it paints beyond what the control marks itself.
struct Follow<W> {
    widget: W,
    shared: Rc<Shared>,
    seen: Option<u64>,
    fresh: bool,
    apply: fn(&mut W, &TransportState) -> bool,
}

impl<W: Widget + 'static> Follow<W> {
    fn new(widget: W, shared: &Rc<Shared>, apply: fn(&mut W, &TransportState) -> bool) -> Self {
        let mut follow = Self {
            widget,
            shared: shared.clone(),
            seen: None,
            fresh: false,
            apply,
        };
        follow.follow();
        follow
    }

    fn follow(&mut self) -> bool {
        let revision = self.shared.revision.get();
        if self.seen == Some(revision) {
            return false;
        }
        self.seen = Some(revision);
        self.fresh |= (self.apply)(&mut self.widget, &self.shared.state.borrow());
        true
    }
}

impl<W: Widget + 'static> crate::widget::Decorator for Follow<W> {
    type Inner = W;

    fn inner(&self) -> &W {
        &self.widget
    }

    fn inner_mut(&mut self) -> &mut W {
        &mut self.widget
    }

    fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
        self.follow();
        self.widget.sync(state, path);
    }

    fn update(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) -> bool {
        let followed = self.follow();
        self.widget.update(state, path) | followed
    }

    fn needs_redraw(&self) -> bool {
        self.fresh || self.widget.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.fresh = false;
        self.widget.mark_drawn();
    }
}

/// The song's art: the cover, or its generated placeholder. While the app
/// says what it opens, a press (or Enter on it) asks for that.
struct Art {
    shared: Rc<Shared>,
    seen: Option<u64>,
    rect: Rect,
    dirty: bool,
    pressed: bool,
    focused: bool,
    cursor: Point,
}

impl Art {
    fn opens(&self) -> Option<String> {
        self.shared.state.borrow().opens.clone()
    }

    fn open(&self) {
        if self.opens().is_some() {
            self.shared.push(TransportEvent::OpenPlaying);
        }
    }
}

impl Widget for Art {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(taffy::Style::default(), node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = crate::widget::layout_rect(taffy, node, origin);
        self.dirty |= rect != self.rect;
        self.rect = rect;
    }

    fn update(&mut self, _: &crate::widget::AppModel, _: &crate::widget::ModelPath) -> bool {
        let revision = self.shared.revision.get();
        if self.seen == Some(revision) {
            return false;
        }
        self.seen = Some(revision);
        self.dirty = true;
        true
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if self.rect.width() <= 0.0 {
            return;
        }
        let now = crate::time::now();
        self.shared.follow_song(theme, now);
        if self.shared.art_lent.get() {
            return;
        }
        let radius = TransportFeed::art_radius(theme);
        let ctx = &mut PaintCtx::new(scene, transform, text, theme);
        self.shared
            .song
            .borrow()
            .paint_art(ctx, self.rect, radius, now);
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.shared.song_moving()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn needs_animation_frame(&self) -> bool {
        self.shared.song_moving()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "TransportArt"
    }

    fn test_id(&self) -> Option<&str> {
        Some("transport.art")
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if self.opens().is_none() {
            self.pressed = false;
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => self.cursor = ctx.logical(position),
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                let inside = self.rect.contains(self.cursor);
                match state {
                    ElementState::Pressed => {
                        self.pressed = inside;
                        if inside {
                            *ctx.focus_target = Some(ctx.node_id);
                            *ctx.handled = true;
                        }
                    }
                    ElementState::Released => {
                        if std::mem::take(&mut self.pressed) && inside {
                            self.open();
                            *ctx.handled = true;
                        }
                    }
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Enter | NamedKey::Space),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if self.focused => {
                self.open();
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        (self.opens().is_some() && self.rect.contains(point))
            .then_some(winit::window::CursorIcon::Pointer)
    }

    fn is_focusable(&self) -> bool {
        self.opens().is_some()
    }

    fn set_focus(&mut self, focused: bool) {
        self.dirty |= self.focused != focused;
        self.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _root: &crate::widget::ModelPath,
    ) -> bool {
        if *action == crate::a11y::Action::Activate && self.opens().is_some() {
            self.open();
            return true;
        }
        false
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let name = self.opens()?;
        Some(
            AccessNode::new(Role::Button)
                .with_name(name)
                .with_state(AccessState::new().focusable(true).focused(self.focused)),
        )
    }
}

/// The song's lines, cross-fading with the art when the song changes.
struct SongLines {
    shared: Rc<Shared>,
    seen: Option<u64>,
    rect: Rect,
    dirty: bool,
}

impl Widget for SongLines {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(taffy::Style::default(), node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = crate::widget::layout_rect(taffy, node, origin);
        self.dirty |= rect != self.rect;
        self.rect = rect;
    }

    fn update(&mut self, _: &crate::widget::AppModel, _: &crate::widget::ModelPath) -> bool {
        let revision = self.shared.revision.get();
        if self.seen == Some(revision) {
            return false;
        }
        self.seen = Some(revision);
        self.dirty = true;
        true
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if self.rect.width() <= 0.0 {
            return;
        }
        let now = crate::time::now();
        self.shared.follow_song(theme, now);
        let ctx = &mut PaintCtx::new(scene, transform, text, theme);
        self.shared
            .song
            .borrow()
            .paint_lines(ctx, self.rect, None, now);
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.shared.song_moving()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn needs_animation_frame(&self) -> bool {
        self.shared.song_moving()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "TransportSong"
    }

    fn test_id(&self) -> Option<&str> {
        Some("transport.song")
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        let state = self.shared.state.borrow();
        let song = song_of(&state);
        let spoken = crate::widget::TextStackFace::new(&song.title)
            .secondary(&song.subtitle)
            .spoken();
        Some(AccessNode::new(Role::StaticText).with_name(spoken))
    }
}

/// One of the scrubber's times: the elapsed one follows the knob while it
/// is held, so the time under the finger reads beside the bar too.
struct TimeLabel {
    shared: Rc<Shared>,
    side: Caption,
    rect: Rect,
    drawn: Option<String>,
}

impl Widget for TimeLabel {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(taffy::Style::default(), node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = crate::widget::layout_rect(taffy, node, origin);
        if rect != self.rect {
            self.drawn = None;
        }
        self.rect = rect;
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let label = self.shared.caption(self.side);
        if !label.is_empty() {
            let style = theme.text(TextRole::Caption).numeric();
            let (width, height) = text.measure_style(&label, None, &style);
            let x = match self.side {
                Caption::Elapsed => self.rect.x1 - f64::from(width),
                Caption::Total => self.rect.x0,
            };
            let y = self.rect.y0 + (self.rect.height() - f64::from(height)) / 2.0;
            text.draw_style(
                scene,
                &label,
                theme.text_dim,
                transform.pre_translate((x, y).into()),
                None,
                &style,
            );
        }
        self.drawn = Some(label);
    }

    fn needs_redraw(&self) -> bool {
        self.drawn.as_deref() != Some(self.shared.caption(self.side).as_str())
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "TransportTime"
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        let label = self.shared.caption(self.side);
        (!label.is_empty()).then(|| AccessNode::new(Role::StaticText).with_name(label))
    }
}

/// The scrubber: the position slider, telling the transport where its knob
/// is held so the elapsed time follows it.
struct Scrubber {
    slider: Follow<Slider>,
}

impl crate::widget::Decorator for Scrubber {
    type Inner = Follow<Slider>;

    fn inner(&self) -> &Follow<Slider> {
        &self.slider
    }

    fn inner_mut(&mut self) -> &mut Follow<Slider> {
        &mut self.slider
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        Widget::handle_event(&mut self.slider, event, ctx);
        let slider = &self.slider.widget;
        self.slider
            .shared
            .scrub
            .set(slider.is_held().then(|| slider.value()));
    }
}

// The transport's children, in placement order.
const ART: usize = 0;
const SONG: usize = 1;
const LIKE: usize = 2;
const LYRICS: usize = 3;
const SHUFFLE: usize = 4;
const PREVIOUS: usize = 5;
const PLAY: usize = 6;
const NEXT: usize = 7;
const REPEAT: usize = 8;
const ELAPSED: usize = 9;
const SCRUBBER: usize = 10;
const TOTAL: usize = 11;
const QUEUE: usize = 12;
const MUTE: usize = 13;
const VOLUME: usize = 14;
const MORE: usize = 15;
/// The speaker whose popover holds the volume, where the slider doesn't
/// fit inline.
const VOLUME_MENU: usize = 16;
const CHILDREN: usize = 17;

/// Which children have what they invoke, given the app's `actions`.
fn handed(actions: &TransportActions) -> [bool; CHILDREN] {
    std::array::from_fn(|index| match index {
        PLAY => actions.play_pause.is_some(),
        PREVIOUS => actions.previous.is_some(),
        NEXT => actions.next.is_some(),
        SHUFFLE => actions.shuffle.is_some(),
        REPEAT => actions.repeat.is_some(),
        LIKE => actions.like.is_some(),
        MUTE => actions.mute.is_some(),
        SCRUBBER => actions.seek.is_some(),
        VOLUME | VOLUME_MENU => actions.volume.is_some(),
        _ => true,
    })
}

/// Whether child `index` is there in `form` for `state`, with what it
/// invokes `handed`.
fn present(index: usize, state: &TransportState, form: Form, handed: &[bool; CHILDREN]) -> bool {
    if !handed[index] {
        return false;
    }
    let wide = form != Form::Compact;
    match index {
        ART => state.art != TransportArt::None,
        LIKE => wide && state.liked.is_some(),
        LYRICS => wide && state.lyrics.is_some(),
        SHUFFLE => wide && state.shuffle.is_some(),
        REPEAT => wide && state.repeat.is_some(),
        QUEUE => wide && state.queue.is_some(),
        VOLUME | MUTE => form == Form::WideWithVolume,
        VOLUME_MENU => form != Form::WideWithVolume,
        MORE => !wide && state.more,
        _ => true,
    }
}

/// A song this long or longer counts its time in hours.
const HOUR_MS: f64 = 3_600_000.0;

fn time(ms: f64) -> String {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a song's position in whole seconds, clamped at zero"
    )]
    let seconds = (ms.max(0.0) / 1000.0) as u64;
    crate::format::timecode(seconds)
}

/// A button that invokes the app's `action`; one the app handed none for
/// invokes nothing and is left out ([`present`]).
fn bound(button: Button, action: Option<&ActionBinding<()>>) -> Button {
    match action {
        Some(action) => button.on_action(action.clone()),
        None => button,
    }
}

fn button(id: &str, icon: Icon, name: &str, action: Option<&ActionBinding<()>>) -> Button {
    bound(
        Button::icon_only(id, icon, name).control_size(ControlSize::Regular),
        action,
    )
}

/// A button that changes what the window shows: a view action of its own,
/// which the app answers with `event`.
fn view_button(
    id: &str,
    icon: Icon,
    name: &str,
    shared: &Rc<Shared>,
    event: TransportEvent,
) -> Button {
    let shared = shared.clone();
    Button::icon_only(id, icon, name)
        .control_size(ControlSize::Regular)
        .on_view(move || shared.push(event))
}

/// A slider that invokes the app's `action` with its value.
fn bound_slider(slider: Slider, action: Option<&ActionBinding<f64>>) -> Slider {
    match action {
        Some(action) => slider.on_action(action.clone()),
        None => slider,
    }
}

/// A toggle that is on while `on` holds, rests while idle.
fn toggle(button: &mut Button, on: Option<bool>, idle: bool) -> bool {
    button.set_toggled(on == Some(true) && !idle);
    button.set_enabled(!idle);
    false
}

/// The volume slider in the speaker's popover, a comfortable width.
struct VolumePanel {
    slider: Slider,
}

impl crate::widget::Decorator for VolumePanel {
    type Inner = Slider;

    fn inner(&self) -> &Slider {
        &self.slider
    }

    fn inner_mut(&mut self) -> &mut Slider {
        &mut self.slider
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = self.slider.build_layout(taffy, theme, node_map);
        if let Ok(style) = taffy.style(node) {
            let mut style = style.clone();
            style.size.width =
                taffy::Dimension::length(crate::num::f32_of_f64(MediaSize::Large.extent(theme)));
            let _ = taffy.set_style(node, style);
        }
        node
    }
}

/// Set a volume slider from the declared volume, unless it is held.
fn follow_volume(slider: &mut Slider, state: &TransportState) -> bool {
    let before = slider.value();
    if !slider.is_held() {
        slider.set_value(state.volume.clamp(0.0, 1.0));
    }
    (before - slider.value()).abs() > f64::EPSILON
}

/// The speaker icon and name for `volume`.
fn speaker(volume: f64) -> Icon {
    if volume <= 0.0 {
        Icon::VolumeX
    } else {
        Icon::Volume2
    }
}

fn children(shared: &Rc<Shared>, actions: &TransportActions) -> Vec<Element> {
    let parts: [Element; CHILDREN] = [
        Box::new(Art {
            shared: shared.clone(),
            seen: None,
            rect: Rect::ZERO,
            dirty: true,
            pressed: false,
            focused: false,
            cursor: Point::ZERO,
        }),
        Box::new(SongLines {
            shared: shared.clone(),
            seen: None,
            rect: Rect::ZERO,
            dirty: true,
        }),
        Box::new(Follow::new(
            button("transport.like", Icon::Heart, "Like", actions.like.as_ref()),
            shared,
            |b, state| {
                b.set_accessible_name(if state.liked == Some(true) {
                    "Unlike"
                } else {
                    "Like"
                });
                toggle(b, state.liked, false)
            },
        )),
        Box::new(Follow::new(
            view_button(
                "transport.lyrics",
                Icon::Mic,
                "Lyrics",
                shared,
                TransportEvent::Lyrics,
            ),
            shared,
            |b, state| toggle(b, state.lyrics, false),
        )),
        Box::new(Follow::new(
            button(
                "transport.shuffle",
                Icon::Shuffle,
                "Shuffle",
                actions.shuffle.as_ref(),
            ),
            shared,
            |b, state| toggle(b, state.shuffle, state.idle),
        )),
        Box::new(Follow::new(
            bound(
                Button::transport("transport.previous", TransportControl::Previous),
                actions.previous.as_ref(),
            ),
            shared,
            |b, state| {
                b.set_enabled(!state.idle);
                false
            },
        )),
        Box::new(Follow::new(
            bound(
                Button::transport(
                    "transport.play",
                    TransportControl::PlayPause { playing: false },
                ),
                actions.play_pause.as_ref(),
            ),
            shared,
            |b, state| {
                b.set_transport(TransportControl::PlayPause {
                    playing: state.playing,
                });
                b.set_enabled(!state.idle);
                false
            },
        )),
        Box::new(Follow::new(
            bound(
                Button::transport("transport.next", TransportControl::Next),
                actions.next.as_ref(),
            ),
            shared,
            |b, state| {
                b.set_enabled(!state.idle);
                false
            },
        )),
        Box::new(Follow::new(
            button(
                "transport.repeat",
                Icon::Repeat,
                "Repeat",
                actions.repeat.as_ref(),
            ),
            shared,
            |b, state| {
                let repeat = state.repeat.unwrap_or_default();
                let (icon, name) = match repeat {
                    TransportRepeat::One => (Icon::Repeat1, "Repeat one"),
                    TransportRepeat::All => (Icon::Repeat, "Repeat all"),
                    TransportRepeat::Off => (Icon::Repeat, "Repeat"),
                };
                b.set_icon(icon);
                b.set_accessible_name(name);
                b.set_tooltip(Some(name.to_owned()));
                toggle(b, Some(repeat != TransportRepeat::Off), state.idle)
            },
        )),
        Box::new(TimeLabel {
            shared: shared.clone(),
            side: Caption::Elapsed,
            rect: Rect::ZERO,
            drawn: None,
        }),
        Box::new(Scrubber {
            slider: Follow::new(
                bound_slider(
                    Slider::from_state(&crate::LocalState::new(0.0))
                        .with_id("transport.position")
                        .with_accessible_name("Position")
                        .value_text({
                            let shared = shared.clone();
                            move |fraction| time(fraction * shared.state.borrow().duration_ms)
                        })
                        .reads_out_while_held()
                        .commits_on_release(),
                    actions.seek.as_ref(),
                ),
                shared,
                |slider, state| {
                    let before = (slider.value(), slider.is_enabled());
                    slider.set_enabled(!state.idle);
                    if !slider.is_held() {
                        let fraction = if state.duration_ms > 0.0 {
                            (state.position_ms / state.duration_ms).clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        slider.set_value(fraction);
                    }
                    (before.0 - slider.value()).abs() > f64::EPSILON
                        || before.1 != slider.is_enabled()
                },
            ),
        }),
        Box::new(TimeLabel {
            shared: shared.clone(),
            side: Caption::Total,
            rect: Rect::ZERO,
            drawn: None,
        }),
        Box::new(Follow::new(
            view_button(
                "transport.queue",
                Icon::ListMusic,
                "Queue",
                shared,
                TransportEvent::Queue,
            ),
            shared,
            |b, state| toggle(b, state.queue, false),
        )),
        Box::new(Follow::new(
            button(
                "transport.mute",
                Icon::Volume2,
                "Mute",
                actions.mute.as_ref(),
            ),
            shared,
            |b, state| {
                let muted = state.volume <= 0.0;
                b.set_icon(speaker(state.volume));
                let name = if muted { "Unmute" } else { "Mute" };
                b.set_accessible_name(name);
                b.set_tooltip(Some(name.to_owned()));
                false
            },
        )),
        Box::new(Follow::new(
            bound_slider(
                Slider::from_state(&crate::LocalState::new(1.0))
                    .with_id("transport.volume")
                    .with_accessible_name("Volume"),
                actions.volume.as_ref(),
            ),
            shared,
            follow_volume,
        )),
        Box::new({
            let shared = shared.clone();
            Button::icon_only("transport.more", Icon::MoreHorizontal, "More controls")
                .control_size(ControlSize::Regular)
                .on_view(move || {
                    let origin = shared.origin.get().to_vec2();
                    shared.push(TransportEvent::More(shared.more.get() + origin));
                })
        }),
        Box::new(Follow::new(
            Button::icon_only("transport.volume-menu", Icon::Volume2, "Volume")
                .control_size(ControlSize::Regular)
                .with_popover(VolumePanel {
                    slider: bound_slider(
                        Slider::from_state(&crate::LocalState::new(1.0))
                            .with_id("transport.volume-menu.slider")
                            .with_accessible_name("Volume"),
                        actions.volume.as_ref(),
                    ),
                })
                .side(PopoverSide::Above)
                .aligned(PanelAlign::Trailing),
            shared,
            |menu, state| {
                menu.button_mut().set_icon(speaker(state.volume));
                follow_volume(&mut menu.popover_mut().content_mut().slider, state)
            },
        )),
    ];
    parts
        .into_iter()
        .enumerate()
        .map(|(index, part)| {
            let shared = shared.clone();
            Box::new(When::new(part, move || {
                present(
                    index,
                    &shared.state.borrow(),
                    shared.form.get(),
                    &shared.handed.get(),
                )
            })) as Element
        })
        .collect()
}

/// A square of `side` centred on `mid` vertically, from `x`.
fn square(x: f64, mid: f64, side: f64) -> Rect {
    Rect::new(x, mid - side / 2.0, x + side, mid + side / 2.0)
}

/// Lay controls out from `x` towards the start (`leftwards`) or the end,
/// skipping the ones that aren't there.
struct Run<'a> {
    rects: &'a mut [Rect; CHILDREN],
    on: [bool; CHILDREN],
    x: f64,
    mid: f64,
}

impl Run<'_> {
    /// Put `index` (a square of `side`) after the run, then `gap`.
    fn push(&mut self, index: usize, side: f64, gap: f64) {
        if self.on[index] {
            self.rects[index] = square(self.x, self.mid, side);
            self.x += side + gap;
        }
    }

    /// Put `index` before the run (it grows leftwards), then `gap`.
    fn push_back(&mut self, index: usize, width: f64, side: f64, gap: f64) {
        if self.on[index] {
            self.rects[index] = Rect::new(
                self.x - width,
                self.mid - side / 2.0,
                self.x,
                self.mid + side / 2.0,
            );
            self.x -= width + gap;
        }
    }
}

/// Place the song (art and lines) from `x0` to `x1` around `mid`.
fn place_song(
    rects: &mut [Rect; CHILDREN],
    on: [bool; CHILDREN],
    m: &Metrics,
    span: (f64, f64),
    art: f64,
    mid: f64,
) {
    let (mut x0, x1) = span;
    if on[ART] {
        rects[ART] = square(x0, mid, art);
        x0 += art + m.sibling;
    }
    rects[SONG] = Rect::new(x0, mid - m.song / 2.0, x1.max(x0), mid + m.song / 2.0);
}

/// The scrubber's row from `x0` to `x1` at `y`: elapsed, the knob, total.
fn place_scrub(rects: &mut [Rect; CHILDREN], m: &Metrics, x0: f64, x1: f64, y: f64) {
    let row = Rect::new(x0, y, x1, y + m.scrub());
    let text = |a: f64, b: f64| {
        Rect::new(
            a,
            row.center().y - m.caption / 2.0,
            b,
            row.center().y + m.caption / 2.0,
        )
    };
    rects[ELAPSED] = text(row.x0, row.x0 + m.time);
    rects[TOTAL] = text(row.x1 - m.time, row.x1);
    // The held knob reaches past the track's ends: the gap keeps it off
    // the times at either end.
    let gap = m.related + SliderFace::overhang();
    let knob0 = row.x0 + m.time + gap;
    let knob1 = (row.x1 - m.time - gap).max(knob0);
    rects[SCRUBBER] = Rect::new(knob0, row.y0, knob1, row.y1);
}

fn layout(
    state: &TransportState,
    handed: &[bool; CHILDREN],
    m: &Metrics,
    width: f64,
) -> (Form, [Rect; CHILDREN], f64) {
    let form = m.form(width);
    let on = std::array::from_fn(|index| present(index, state, form, handed));
    let mut rects = [Rect::ZERO; CHILDREN];
    let height = m.height(form);
    let (x0, x1) = (m.section, (width - m.section).max(m.section));
    let y0 = m.sibling;
    let top = m.top(form);
    let mid = y0 + top / 2.0;
    let scrub_y = y0 + top + m.tight;
    if form == Form::Compact {
        let mut run = Run {
            rects: &mut rects,
            on,
            x: x1,
            mid,
        };
        run.push_back(MORE, m.control, m.control, m.sibling);
        run.push_back(VOLUME_MENU, m.control, m.control, m.sibling);
        run.push_back(NEXT, m.skip, m.skip, m.related);
        run.push_back(PLAY, m.play, m.play, m.related);
        run.push_back(PREVIOUS, m.skip, m.skip, m.sibling);
        let end = run.x;
        place_song(&mut rects, on, m, (x0, end), top, mid);
        place_scrub(&mut rects, m, x0, x1, scrub_y);
        return (form, rects, height);
    }
    let inner = x1 - x0;
    let middle = (y0 + height - m.sibling) / 2.0;
    // The song: art and lines, then like and lyrics at the column's end.
    let left = x0 + (inner * 0.36).min(m.large);
    let mut run = Run {
        rects: &mut rects,
        on,
        x: left,
        mid: middle,
    };
    run.push_back(LYRICS, m.control, m.control, m.tight);
    run.push_back(LIKE, m.control, m.control, m.related);
    let song_end = run.x;
    place_song(&mut rects, on, m, (x0, song_end), m.play, middle);
    // The queue and the volume.
    let mut run = Run {
        rects: &mut rects,
        on,
        x: x1,
        mid: middle,
    };
    run.push_back(VOLUME, m.small / 2.0, m.control, m.tight);
    run.push_back(MUTE, m.control, m.control, m.related);
    run.push_back(VOLUME_MENU, m.control, m.control, m.related);
    run.push_back(QUEUE, m.control, m.control, m.related);
    let right = run.x + m.related;
    // The transport, centred over the scrubber between the two.
    let (c0, c1) = (left + m.group, (right - m.group).max(left + m.group));
    let flank = |index: usize| {
        if on[index] {
            m.control + m.sibling
        } else {
            0.0
        }
    };
    let row = flank(SHUFFLE) + flank(REPEAT) + 2.0 * m.skip + m.play + 2.0 * m.related;
    let mut run = Run {
        rects: &mut rects,
        on,
        x: f64::midpoint(c0, c1) - row / 2.0,
        mid,
    };
    run.push(SHUFFLE, m.control, m.sibling);
    run.push(PREVIOUS, m.skip, m.related);
    run.push(PLAY, m.play, m.related);
    run.push(NEXT, m.skip, m.sibling);
    run.push(REPEAT, m.control, 0.0);
    place_scrub(&mut rects, m, c0, c1, scrub_y);
    (form, rects, height)
}

fn policy(shared: &Shared, ctx: &mut ManualContext<'_>) -> Result<Size<f32>, WidgetLayoutError> {
    let metrics = shared.metrics.get();
    let width = f64::from(ctx.available().width);
    let width = if width.is_finite() {
        width.max(0.0)
    } else {
        0.0
    };
    let (form, rects, height) = layout(
        &shared.state.borrow(),
        &shared.handed.get(),
        &metrics,
        width,
    );
    shared.form.set(form);
    shared.more.set(rects[MORE]);
    shared.placed.set(rects);
    for (index, rect) in rects.into_iter().enumerate() {
        ctx.place(index, rect)?;
    }
    Ok(Size {
        width: crate::num::f32_of_f64(width),
        height: crate::num::f32_of_f64(height),
    })
}

/// The media transport: see the [module docs](self).
pub struct MediaTransport {
    content: Manual,
    shared: Rc<Shared>,
    theme: Option<Theme>,
    /// The form the children's presence was last built for.
    built_form: Form,
    structure_dirty: bool,
    /// Where a press that may become a swipe began, in window coordinates.
    swipe: Option<Point>,
    cursor: Point,
}

impl MediaTransport {
    /// Whether a drag from `point` (window coordinates) swipes the band:
    /// it lands on the band, but on no control other than the song.
    fn swipes_from(&self, point: Point) -> bool {
        let rect = self.content.get_rect();
        if !rect.contains(point) || self.shared.state.borrow().idle {
            return false;
        }
        let local = point - rect.origin().to_vec2();
        !self
            .shared
            .placed
            .get()
            .iter()
            .enumerate()
            .any(|(index, placed)| index != ART && index != SONG && placed.contains(local))
    }

    /// End a press: a long enough drag across the band skips a song.
    fn end_swipe(&mut self) -> bool {
        let Some(start) = self.swipe.take() else {
            return false;
        };
        let (dx, dy) = (self.cursor.x - start.x, self.cursor.y - start.y);
        if dx.abs() < self.shared.metrics.get().swipe || dx.abs() <= dy.abs() {
            return false;
        }
        self.shared.push(if dx < 0.0 {
            TransportEvent::Next
        } else {
            TransportEvent::Previous
        });
        true
    }
    /// The transport showing what `feed` declares, its controls invoking
    /// the app's `actions`.
    #[must_use]
    pub fn new(feed: &TransportFeed, actions: TransportActions) -> Self {
        let shared = feed.0.clone();
        shared.handed.set(handed(&actions));
        let placing = shared.clone();
        let mut content = Manual::new(move |ctx| policy(&placing, ctx));
        for child in children(&shared, &actions) {
            content = content.child(child);
        }
        Self {
            content,
            built_form: shared.form.get(),
            shared,
            theme: None,
            structure_dirty: false,
            swipe: None,
            cursor: Point::ZERO,
        }
    }
}

impl crate::widget::decorator::concrete::Decorator for MediaTransport {
    type Inner = Manual;

    fn inner(&self) -> &Manual {
        &self.content
    }

    fn inner_mut(&mut self) -> &mut Manual {
        &mut self.content
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        if self.theme.as_ref() != Some(theme) {
            let time = self.shared.metrics.get().time;
            self.shared.metrics.set(Metrics {
                time,
                ..Metrics::of(theme)
            });
            self.theme = Some(theme.clone());
        }
        self.built_form = self.shared.form.get();
        Widget::build_layout(&mut self.content, taffy, theme, node_map)
    }

    fn adjust_layout_style(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        let mut changed = Widget::adjust_layout_style(&mut self.content, taffy, node, text);
        if let Some(theme) = &self.theme {
            let style = theme.text(TextRole::Caption).numeric();
            // As wide as the song's longest time, so the bar keeps its
            // length through the song.
            let widest = if self.shared.state.borrow().duration_ms >= HOUR_MS {
                "00:00:00"
            } else {
                "00:00"
            };
            let time = f64::from(text.measure_style(widest, None, &style).0).ceil();
            let metrics = self.shared.metrics.get();
            if (metrics.time - time).abs() > f64::EPSILON {
                self.shared.metrics.set(Metrics { time, ..metrics });
                self.content.invalidation().invalidate();
                changed = true;
            }
        }
        changed
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        Widget::apply_layout(&mut self.content, taffy, node, origin);
        self.shared.origin.set(self.content.get_rect().origin());
        // A width that crosses into the other form changes which controls
        // are there: the children follow at the next structure pass.
        if self.shared.form.get() != self.built_form {
            self.built_form = self.shared.form.get();
            self.structure_dirty = true;
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => self.cursor = ctx.logical(position),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.swipes_from(self.cursor) => self.swipe = Some(self.cursor),
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.end_swipe() => {
                // The swipe was the gesture: the art under it opens nothing.
                *ctx.handled = true;
                return;
            }
            _ => {}
        }
        Widget::handle_event(&mut self.content, event, ctx);
    }

    /// A finger dragged across the band swipes it, as the pointer does.
    fn touch_claim(&self, point: Point) -> crate::interaction::TouchClaim {
        if self.swipes_from(point) {
            crate::interaction::TouchClaim::Drag
        } else {
            Widget::touch_claim(&self.content, point)
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.swipe.is_some() || Widget::wants_pointer_capture(&self.content)
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        Widget::take_structure_dirty(&mut self.content) | own
    }

    fn needs_redraw(&self) -> bool {
        self.structure_dirty || Widget::needs_redraw(&self.content)
    }

    fn name(&self) -> &str {
        "MediaTransport"
    }
}

#[cfg(test)]
#[path = "media_transport_tests.rs"]
mod tests;
