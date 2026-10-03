//! The now-playing card: artwork, title and artist, transport, a seek bar
//! when the player can seek, a player picker when several are running and an
//! output picker over the device fabric's route targets.
//!
//! Quick Settings shows the full card beside its root page. The lock screen
//! reuses [`transport_rects`] and [`paint_transport`] for its compact card,
//! which carries transport only: no player switching and no output changes
//! while the session is locked.

use std::sync::Arc;
use std::time::Duration;

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, Slider, SliderFace};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::Fill;

use crate::shell::now_playing::{
    NowPlayingView, OutputKind, PlaybackStatus, PlayerCommand, PlayerSnapshot, format_time, tr,
};
use tilo_desktop::drill_in::{
    CONTROL_INSET, GLYPH, PANEL_PAD, RELATED_GAP, ROW_HEIGHT, SIBLING_GAP, TARGET,
    centred_text_top, control_radius, leading_glyph, paint_control, trailing_glyph, type_size,
};

/// The artwork square: a list row plus one sibling step, so the three text
/// lines beside it sit comfortably at 1×.
const ART: f64 = ROW_HEIGHT + SIBLING_GAP;
/// The most picker rows shown at once.
const MAX_PICKER_ROWS: usize = 6;
/// How far one arrow key moves the playhead.
const SEEK_STEP: Duration = Duration::from_secs(5);

/// The three transport buttons, the only controls the lock screen offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    Previous,
    PlayPause,
    Next,
}

impl Transport {
    pub const ALL: [Self; 3] = [Self::Previous, Self::PlayPause, Self::Next];

    #[must_use]
    pub fn command(self) -> PlayerCommand {
        match self {
            Self::Previous => PlayerCommand::Previous,
            Self::PlayPause => PlayerCommand::PlayPause,
            Self::Next => PlayerCommand::Next,
        }
    }

    #[must_use]
    pub fn enabled(self, player: &PlayerSnapshot) -> bool {
        match self {
            Self::Previous => player.can_go_previous,
            Self::PlayPause => player.can_toggle(),
            Self::Next => player.can_go_next,
        }
    }

    /// The spoken name, which for play/pause follows the player's state.
    #[must_use]
    pub fn label(self, player: &PlayerSnapshot) -> String {
        tr(match self {
            Self::Previous => "now-playing-previous",
            Self::Next => "now-playing-next",
            Self::PlayPause if player.status == PlaybackStatus::Playing => "now-playing-pause",
            Self::PlayPause => "now-playing-play",
        })
    }

    #[must_use]
    pub fn icon(self, player: &PlayerSnapshot) -> ui::icons::Icon {
        match self {
            Self::Previous => ui::icons::Icon::SkipBack,
            Self::Next => ui::icons::Icon::SkipForward,
            Self::PlayPause if player.status == PlaybackStatus::Playing => ui::icons::Icon::Pause,
            Self::PlayPause => ui::icons::Icon::Play,
        }
    }
}

/// Previous, play/pause and next centred on `row`: pointer-floor squares
/// either side of a row-height play button, one sibling step apart, all
/// grown by `scale` (the lock screen's own layout scale; 1 in Quick
/// Settings) but never under the pointer floor.
#[must_use]
pub fn transport_rects(row: Rect, scale: f64) -> [(Transport, Rect); 3] {
    let scale = scale.max(1.0);
    let side = TARGET * scale;
    let play = row.height().min(ROW_HEIGHT * scale).max(side);
    let gap = SIBLING_GAP * scale;
    let centre = row.center();
    let play_rect = Rect::from_center_size(centre, (play, play));
    let previous =
        Rect::from_center_size((play_rect.x0 - gap - side / 2.0, centre.y), (side, side));
    let next = Rect::from_center_size((play_rect.x1 + gap + side / 2.0, centre.y), (side, side));
    [
        (Transport::Previous, previous),
        (Transport::PlayPause, play_rect),
        (Transport::Next, next),
    ]
}

/// Paint the transport buttons with the shared `Button` face: play/pause is
/// the primary action, previous and next are ghost buttons, and a control the
/// player does not offer is disabled rather than hidden so the row keeps its
/// shape.
pub fn paint_transport(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rects: &[(Transport, Rect); 3],
    player: &PlayerSnapshot,
    cursor: Option<Point>,
    focused: Option<Transport>,
) {
    for (transport, rect) in rects {
        let label = transport.label(player);
        let enabled = transport.enabled(player);
        let mut face = ButtonFace::new(*rect, "")
            .icon(transport.icon(player))
            .hovered(enabled && cursor.is_some_and(|p| rect.contains(p)))
            .focused(focused == Some(*transport))
            .disabled(!enabled)
            .radius(rect.height() / 2.0)
            .accessible_name(&label);
        face = if *transport == Transport::PlayPause {
            face.primary()
        } else {
            face.ghost()
        };
        Button::paint(scene, transform, text, theme, &face);
    }
}

/// Paint `player`'s artwork into `rect`, or its application monogram on the
/// control floor when the player offers no local artwork.
pub fn paint_artwork(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    player: &PlayerSnapshot,
    image: Option<&vello::peniko::ImageData>,
) {
    let radius = control_radius(theme, rect);
    let shape = RoundedRect::from_rect(rect, radius);
    if let (Some(image), Some(art)) = (image, player.artwork.as_ref()) {
        let scale = (rect.width() / f64::from(art.width.max(1)))
            .max(rect.height() / f64::from(art.height.max(1)));
        let drawn = (f64::from(art.width) * scale, f64::from(art.height) * scale);
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &shape,
        );
        scene.fill(
            Fill::NonZero,
            transform
                .pre_translate(
                    (
                        rect.center().x - drawn.0 / 2.0,
                        rect.center().y - drawn.1 / 2.0,
                    )
                        .into(),
                )
                .pre_scale(scale),
            &vello::peniko::ImageBrush::new(image.clone()),
            None,
            &Rect::new(0.0, 0.0, f64::from(art.width), f64::from(art.height)),
        );
        scene.pop_layer();
        return;
    }
    ui::surface::card(
        scene,
        transform,
        rect,
        radius,
        theme.tokens.colors.lift(0.6),
    );
    let monogram = player.monogram();
    if monogram.is_empty() {
        let glyph = GLYPH.min(rect.height());
        ui::icons::draw_icon(
            scene,
            transform,
            Rect::from_center_size(rect.center(), (glyph, glyph)),
            ui::icons::Icon::Music,
            theme.tokens.colors.text_dim,
            1.5,
        );
        return;
    }
    let size = type_size(text, theme.tokens.typography.scale.lg);
    let (w, h) = text.measure_weighted(&monogram, size, None, TextWeight::Semibold);
    text.draw_text_weighted(
        scene,
        &monogram,
        size,
        theme.tokens.colors.text_primary,
        transform.pre_translate(
            (
                rect.center().x - f64::from(w) / 2.0,
                rect.center().y - f64::from(h) / 2.0,
            )
                .into(),
        ),
        None,
        TextWeight::Semibold,
    );
}

/// Every interactive region of the full card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardTarget {
    Seek,
    Players,
    Transport(Transport),
    Outputs,
    Player(usize),
    Output(usize),
}

/// Which picker the card has unfolded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Picker {
    Players,
    Outputs,
}

/// What activating a region asks the host to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CardAction {
    None,
    /// Only the card's own presentation changed (a picker opened or closed).
    Redraw,
    /// Send this command to exactly this player.
    Command {
        bus_name: String,
        command: PlayerCommand,
    },
    /// Show this player from now on.
    SelectPlayer(String),
    /// Route sound to the destination with this continuity token.
    Route(String),
}

/// The full card's retained interaction state. Geometry is recomputed from
/// the view on every call, so paint, hit-testing, focus and accessibility
/// cannot disagree.
#[derive(Default)]
pub struct NowPlayingCard {
    rect: Rect,
    focus: Option<CardTarget>,
    picker: Option<Picker>,
    text_scale: f64,
    /// The last artwork handed to vello, keyed by its pixel buffer, so an
    /// unchanged cover is not re-uploaded every frame.
    image: Option<(usize, vello::peniko::ImageData)>,
}

impl NowPlayingCard {
    #[must_use]
    pub fn new() -> Self {
        Self {
            text_scale: 1.0,
            ..Self::default()
        }
    }

    pub fn set_text_scale(&mut self, scale: f32) {
        self.text_scale = f64::from(scale).clamp(0.5, 4.0);
    }

    fn line(&self, base_px: f32) -> f64 {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a text scale is a small ratio; f32 is the toolkit's type precision"
        )]
        let scale = self.text_scale.max(0.5) as f32;
        f64::from(ui::text_render::TextStyle::new(base_px * scale).line_height_px())
    }

    fn title_line(&self) -> f64 {
        self.line(ui::tokens::TypeScale::default().sm)
    }

    fn caption_line(&self) -> f64 {
        self.line(ui::tokens::TypeScale::default().xs)
    }

    /// Artwork beside title, artist and application.
    fn header_height(&self) -> f64 {
        ART.max(self.title_line() + 2.0 * self.caption_line())
    }

    fn seek_height(&self) -> f64 {
        TARGET.max(self.caption_line())
    }

    fn picker_rows(&self, view: &NowPlayingView) -> usize {
        match self.picker {
            Some(Picker::Players) if view.has_player_choice() => view.players.len(),
            Some(Picker::Outputs) => view.outputs.len(),
            _ => 0,
        }
        .min(MAX_PICKER_ROWS)
    }

    fn row_height(&self) -> f64 {
        ROW_HEIGHT.max(2.0 * RELATED_GAP + self.title_line() + self.caption_line())
    }

    #[must_use]
    pub fn preferred_height(&self, view: &NowPlayingView) -> f64 {
        let seek = if view.progress().is_some() {
            self.seek_height() + RELATED_GAP
        } else {
            0.0
        };
        let rows = self.picker_rows(view);
        let picker = if rows > 0 {
            RELATED_GAP + self.row_height() * f64::from(u32::try_from(rows).unwrap_or(u32::MAX))
        } else {
            0.0
        };
        let speakers = if view.speakers.is_some() {
            self.caption_line() + RELATED_GAP
        } else {
            0.0
        };
        2.0 * PANEL_PAD + self.header_height() + RELATED_GAP + seek + speakers + ROW_HEIGHT + picker
    }

    fn inner(&self) -> Rect {
        self.rect.inset(-PANEL_PAD)
    }

    fn art_rect(&self) -> Rect {
        let inner = self.inner();
        let top = inner.y0 + (self.header_height() - ART) / 2.0;
        Rect::new(inner.x0, top, inner.x0 + ART, top + ART)
    }

    fn header_rect(&self) -> Rect {
        let inner = self.inner();
        Rect::new(
            inner.x0,
            inner.y0,
            inner.x1,
            inner.y0 + self.header_height(),
        )
    }

    fn seek_rect(&self, view: &NowPlayingView) -> Option<Rect> {
        view.progress()?;
        let inner = self.inner();
        let top = self.header_rect().y1 + RELATED_GAP;
        Some(Rect::new(inner.x0, top, inner.x1, top + self.seek_height()))
    }

    /// The seek bar's own face, between the elapsed and remaining labels.
    fn seek_face(&self, view: &NowPlayingView) -> Option<Rect> {
        let band = self.seek_rect(view)?;
        let label = self.time_label_width();
        Some(Rect::new(
            band.x0 + label + RELATED_GAP,
            band.y0,
            band.x1 - label - RELATED_GAP,
            band.y1,
        ))
    }

    /// Room for "1:02:05" at the caption size, estimated without a renderer
    /// so geometry is available before the first paint.
    fn time_label_width(&self) -> f64 {
        self.caption_line() * 3.0
    }

    /// The speaker-group line under the track, when this device plays to
    /// one.
    fn speakers_rect(&self, view: &NowPlayingView) -> Option<Rect> {
        view.speakers.as_ref()?;
        let inner = self.inner();
        let top = self
            .seek_rect(view)
            .map_or_else(|| self.header_rect().y1, |seek| seek.y1)
            + RELATED_GAP;
        Some(Rect::new(
            inner.x0,
            top,
            inner.x1,
            top + self.caption_line(),
        ))
    }

    fn transport_row(&self, view: &NowPlayingView) -> Rect {
        let inner = self.inner();
        let top = self
            .speakers_rect(view)
            .or_else(|| self.seek_rect(view))
            .map_or_else(|| self.header_rect().y1, |above| above.y1)
            + RELATED_GAP;
        Rect::new(inner.x0, top, inner.x1, top + ROW_HEIGHT)
    }

    fn players_button(&self, view: &NowPlayingView) -> Option<Rect> {
        if !view.has_player_choice() {
            return None;
        }
        let row = self.transport_row(view);
        Some(Rect::from_center_size(
            (row.x0 + TARGET / 2.0, row.center().y),
            (TARGET, TARGET),
        ))
    }

    fn outputs_button(&self, view: &NowPlayingView) -> Option<Rect> {
        if view.outputs.is_empty() {
            return None;
        }
        let row = self.transport_row(view);
        Some(Rect::from_center_size(
            (row.x1 - TARGET / 2.0, row.center().y),
            (TARGET, TARGET),
        ))
    }

    fn picker_row_rect(&self, view: &NowPlayingView, index: usize) -> Rect {
        let inner = self.inner();
        let row_h = self.row_height();
        let top = self.transport_row(view).y1
            + RELATED_GAP
            + row_h * f64::from(u32::try_from(index).unwrap_or(u32::MAX));
        Rect::new(inner.x0, top, inner.x1, top + row_h)
    }

    /// Every interactive region in reading order — also the focus order and
    /// the accessibility children's order.
    #[must_use]
    pub fn targets(&self, view: &NowPlayingView) -> Vec<(CardTarget, Rect)> {
        let mut targets = Vec::new();
        if view.player().seekable()
            && let Some(face) = self.seek_face(view)
        {
            targets.push((CardTarget::Seek, face));
        }
        if let Some(players) = self.players_button(view) {
            targets.push((CardTarget::Players, players));
        }
        for (transport, rect) in transport_rects(self.transport_row(view), 1.0) {
            targets.push((CardTarget::Transport(transport), rect));
        }
        if let Some(outputs) = self.outputs_button(view) {
            targets.push((CardTarget::Outputs, outputs));
        }
        let rows = self.picker_rows(view);
        for index in 0..rows {
            let target = match self.picker {
                Some(Picker::Players) => CardTarget::Player(index),
                _ => CardTarget::Output(index),
            };
            targets.push((target, self.picker_row_rect(view, index)));
        }
        targets
    }

    /// Lay the card out in `rect` without painting, so hosts can hit-test
    /// and publish accessibility before the first frame.
    pub fn layout(&mut self, rect: Rect) {
        self.rect = rect;
    }

    #[must_use]
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// This card's focus and pickers laid out in `rect`, without its artwork
    /// cache — for read-only callers such as the accessibility tree.
    #[must_use]
    pub fn laid_out(&self, rect: Rect) -> Self {
        Self {
            rect,
            focus: self.focus,
            picker: self.picker,
            text_scale: self.text_scale,
            image: None,
        }
    }

    #[must_use]
    pub fn picker(&self) -> Option<Picker> {
        self.picker
    }

    #[must_use]
    pub fn has_focus(&self) -> bool {
        self.focus.is_some()
    }

    /// Closing the popover forgets focus and folds any picker away.
    pub fn reset(&mut self) {
        self.focus = None;
        self.picker = None;
    }

    #[must_use]
    pub fn target_at(&self, view: &NowPlayingView, point: Point) -> Option<CardTarget> {
        self.targets(view)
            .into_iter()
            .find(|(target, rect)| {
                let rect = if *target == CardTarget::Seek {
                    rect.inflate(0.0, RELATED_GAP)
                } else {
                    *rect
                };
                rect.contains(point)
            })
            .map(|(target, _)| target)
    }

    /// A pointer press. Clicking the seek bar jumps to that point.
    pub fn click(&mut self, view: &NowPlayingView, point: Point) -> CardAction {
        let Some(target) = self.target_at(view, point) else {
            return CardAction::None;
        };
        if target == CardTarget::Seek {
            let Some(face) = self.seek_face(view) else {
                return CardAction::None;
            };
            let track = SliderFace::new(face, 0.0).track_rect();
            let fraction = ((point.x - track.x0) / track.width().max(1.0)).clamp(0.0, 1.0);
            return Self::command(view, view.seek_command(fraction));
        }
        self.activate(view, target)
    }

    fn command(view: &NowPlayingView, command: Option<PlayerCommand>) -> CardAction {
        command.map_or(CardAction::None, |command| CardAction::Command {
            bus_name: view.player().bus_name.clone(),
            command,
        })
    }

    /// Activate one region, by pointer, keyboard or assistive technology.
    pub fn activate(&mut self, view: &NowPlayingView, target: CardTarget) -> CardAction {
        match target {
            CardTarget::Seek => CardAction::None,
            CardTarget::Transport(transport) => {
                if transport.enabled(view.player()) {
                    Self::command(view, Some(transport.command()))
                } else {
                    CardAction::None
                }
            }
            CardTarget::Players => {
                self.toggle_picker(Picker::Players);
                CardAction::Redraw
            }
            CardTarget::Outputs => {
                self.toggle_picker(Picker::Outputs);
                CardAction::Redraw
            }
            CardTarget::Player(index) => {
                self.picker = None;
                self.focus = self.focus.map(|_| CardTarget::Players);
                view.players.get(index).map_or(CardAction::None, |player| {
                    CardAction::SelectPlayer(player.bus_name.clone())
                })
            }
            CardTarget::Output(index) => {
                self.picker = None;
                self.focus = self.focus.map(|_| CardTarget::Outputs);
                match view.outputs.get(index) {
                    Some(output) if !output.current => CardAction::Route(output.token.clone()),
                    _ => CardAction::Redraw,
                }
            }
        }
    }

    fn toggle_picker(&mut self, picker: Picker) {
        self.picker = if self.picker == Some(picker) {
            None
        } else {
            Some(picker)
        };
    }

    /// Move keyboard focus. Returns `false` when focus leaves the card past
    /// either end, so the host can hand it to its neighbour.
    pub fn focus_next(&mut self, view: &NowPlayingView, backwards: bool) -> bool {
        let targets = self.targets(view);
        let current = self
            .focus
            .and_then(|focus| targets.iter().position(|(target, _)| *target == focus));
        let next = match (current, backwards) {
            (None, false) => Some(0),
            (None, true) => targets.len().checked_sub(1),
            (Some(index), false) => index.checked_add(1).filter(|next| *next < targets.len()),
            (Some(index), true) => index.checked_sub(1),
        };
        self.focus = next.and_then(|index| targets.get(index)).map(|(t, _)| *t);
        self.focus.is_some()
    }

    /// Enter or Space on the focused region.
    pub fn activate_focused(&mut self, view: &NowPlayingView) -> CardAction {
        match self.focus {
            Some(target) => self.activate(view, target),
            None => CardAction::None,
        }
    }

    /// Left/Right on the focused seek bar move the playhead.
    pub fn adjust_focused(&mut self, view: &NowPlayingView, forward: bool) -> CardAction {
        if self.focus != Some(CardTarget::Seek) {
            return CardAction::None;
        }
        Self::command(view, view.step_command(SEEK_STEP, forward))
    }

    /// Focus the region at an accessibility path, as a screen reader moves.
    pub fn focus_path(&mut self, view: &NowPlayingView, path: &[usize]) -> bool {
        let [index] = path else {
            return false;
        };
        let Some((target, _)) = self.targets(view).get(*index).copied() else {
            return false;
        };
        self.focus = Some(target);
        true
    }

    #[must_use]
    pub fn rect_for_path(&self, view: &NowPlayingView, path: &[usize]) -> Option<Rect> {
        match path {
            [] => Some(self.rect),
            [index] => self.targets(view).get(*index).map(|(_, rect)| *rect),
            _ => None,
        }
    }

    fn target_name(view: &NowPlayingView, target: CardTarget) -> String {
        match target {
            CardTarget::Seek => tr("now-playing-seek"),
            CardTarget::Players => tr("now-playing-choose-player"),
            CardTarget::Outputs => tr("now-playing-choose-output"),
            CardTarget::Transport(transport) => transport.label(view.player()),
            CardTarget::Player(index) => view
                .players
                .get(index)
                .map(|player| player.identity.clone())
                .unwrap_or_default(),
            CardTarget::Output(index) => view
                .outputs
                .get(index)
                .map(|output| output.label.clone())
                .unwrap_or_default(),
        }
    }

    /// The card's semantic tree: one group named for what is playing, whose
    /// children are exactly [`Self::targets`] in order.
    pub fn accessibility_snapshot(&self, view: &NowPlayingView) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, Role};
        let player = view.player();
        let mut description = player.headline().to_string();
        if let Some(subline) = player.subline() {
            description.push_str(", ");
            description.push_str(subline);
        }
        if let Some(speakers) = &view.speakers {
            description.push_str(". ");
            description.push_str(&speakers.text());
        }
        let mut group = AccessNode::new(Role::Group)
            .with_name(tr("now-playing-title"))
            .with_description(description)
            .with_bounds(self.rect);
        for (target, rect) in self.targets(view) {
            let name = Self::target_name(view, target);
            let mut node = match target {
                CardTarget::Seek => AccessNode::new(Role::Slider)
                    .with_name(name)
                    .with_value(format!(
                        "{} / {}",
                        format_time(player.position_at(view.now)),
                        format_time(player.length_us.unwrap_or(0))
                    ))
                    .with_description(tr("now-playing-seek-help")),
                CardTarget::Player(index) => {
                    let mut node = AccessNode::new(Role::Button).with_name(name);
                    node.state.selected = index == view.active;
                    node
                }
                CardTarget::Output(index) => {
                    let mut node = AccessNode::new(Role::Button).with_name(name);
                    if let Some(output) = view.outputs.get(index) {
                        node = node.with_description(tr(match output.kind {
                            OutputKind::Local => "now-playing-output-local",
                            OutputKind::Device => "now-playing-output-device",
                            OutputKind::SpeakerGroup => "now-playing-output-speaker-group",
                        }));
                        node.state.selected = output.current;
                    }
                    node
                }
                CardTarget::Players | CardTarget::Outputs => {
                    let mut node = AccessNode::new(Role::Button).with_name(name);
                    node.state.expanded = Some(
                        self.picker
                            == Some(if target == CardTarget::Players {
                                Picker::Players
                            } else {
                                Picker::Outputs
                            }),
                    );
                    node
                }
                CardTarget::Transport(transport) => {
                    let mut node = AccessNode::new(Role::Button).with_name(name);
                    node.state.disabled = !transport.enabled(player);
                    node
                }
            };
            node.state.focusable = true;
            node.state.focused = self.focus == Some(target);
            group.children.push(node.with_bounds(rect));
        }
        group
    }

    fn artwork_image(&mut self, player: &PlayerSnapshot) -> Option<vello::peniko::ImageData> {
        let art = player.artwork.as_ref()?;
        let identity = Arc::as_ptr(&art.rgba) as usize;
        if let Some((cached, image)) = &self.image
            && *cached == identity
        {
            return Some(image.clone());
        }
        let image = vello::peniko::ImageData {
            data: vello::peniko::Blob::new(art.rgba.clone()),
            format: vello::peniko::ImageFormat::Rgba8,
            alpha_type: vello::peniko::ImageAlphaType::Alpha,
            width: art.width,
            height: art.height,
        };
        self.image = Some((identity, image.clone()));
        Some(image)
    }

    /// Paint the whole card, its Panel surface included, into `rect`.
    #[expect(
        clippy::too_many_lines,
        reason = "one ordered paint of artwork, text, seek bar, transport and picker keeps each region beside the geometry it reads"
    )]
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        cursor: Option<Point>,
        view: &NowPlayingView,
    ) {
        self.rect = rect;
        let colors = &theme.tokens.colors;
        let player = view.player().clone();
        let image = self.artwork_image(&player);
        let art = self.art_rect();
        paint_artwork(scene, transform, text, theme, art, &player, image.as_ref());

        // Title, artist, application: one measured block on the header's
        // centre line, elided to the room beside the artwork.
        let header = self.header_rect();
        let text_x = art.x1 + CONTROL_INSET;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "card widths are bounded by the output; f32 is the shaping precision"
        )]
        let width = (header.x1 - text_x).max(1.0) as f32;
        let title_size = type_size(text, theme.tokens.typography.scale.sm);
        let caption_size = type_size(text, theme.tokens.typography.scale.xs);
        let title = text.elide_to_width_weighted(
            player.headline(),
            title_size,
            width,
            TextWeight::Semibold,
        );
        let subline = player
            .subline()
            .map(|subline| text.elide_to_width(subline, caption_size, width));
        let app = (player.subline() != Some(player.identity.as_str())
            && player.headline() != player.identity)
            .then(|| text.elide_to_width(&player.identity, caption_size, width));
        let (_, title_h) = text.measure_weighted(&title, title_size, None, TextWeight::Semibold);
        let caption_h = f64::from(text.measure("Ag", caption_size, None).1);
        let lines = [subline.is_some(), app.is_some()]
            .into_iter()
            .filter(|line| *line)
            .count();
        let block = f64::from(title_h) + caption_h * f64::from(u32::try_from(lines).unwrap_or(0));
        let mut y = centred_text_top(header, block);
        text.draw_text_weighted(
            scene,
            &title,
            title_size,
            colors.text_primary,
            transform.pre_translate((text_x, y).into()),
            None,
            TextWeight::Semibold,
        );
        y += f64::from(title_h);
        for line in [subline, app].into_iter().flatten() {
            text.draw_text(
                scene,
                &line,
                caption_size,
                colors.text_dim,
                transform.pre_translate((text_x, y).into()),
                None,
            );
            y += caption_h;
        }

        if let (Some(band), Some(face), Some(progress)) =
            (self.seek_rect(view), self.seek_face(view), view.progress())
        {
            let elapsed = format_time(player.position_at(view.now));
            let remaining = format!(
                "-{}",
                format_time(
                    player
                        .length_us
                        .unwrap_or(0)
                        .saturating_sub(player.position_at(view.now))
                )
            );
            let (_, time_h) = text.measure(&elapsed, caption_size, None);
            let time_y = centred_text_top(band, f64::from(time_h));
            text.draw_text(
                scene,
                &elapsed,
                caption_size,
                colors.text_dim,
                transform.pre_translate((band.x0, time_y).into()),
                None,
            );
            let (remaining_w, _) = text.measure(&remaining, caption_size, None);
            text.draw_text(
                scene,
                &remaining,
                caption_size,
                colors.text_dim,
                transform.pre_translate((band.x1 - f64::from(remaining_w), time_y).into()),
                None,
            );
            let seekable = player.seekable();
            Slider::paint(
                scene,
                transform,
                theme,
                &SliderFace::new(face, progress)
                    .hovered(seekable && cursor.is_some_and(|p| face.contains(p)))
                    .focused(self.focus == Some(CardTarget::Seek))
                    .disabled(!seekable),
            );
        }

        if let (Some(band), Some(speakers)) = (self.speakers_rect(view), &view.speakers) {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "card widths are bounded by the output; f32 is the shaping precision"
            )]
            let line = text.elide_to_width(&speakers.text(), caption_size, band.width() as f32);
            text.draw_text(
                scene,
                &line,
                caption_size,
                if speakers.failed() {
                    colors.semantic.danger
                } else {
                    colors.text_dim
                },
                transform.pre_translate((band.x0, band.y0).into()),
                None,
            );
        }

        let row = self.transport_row(view);
        let rects = transport_rects(row, 1.0);
        let focused = match self.focus {
            Some(CardTarget::Transport(transport)) => Some(transport),
            _ => None,
        };
        paint_transport(
            scene, transform, text, theme, &rects, &player, cursor, focused,
        );
        for (target, rect, icon) in [
            (
                CardTarget::Players,
                self.players_button(view),
                ui::icons::Icon::Layers,
            ),
            (
                CardTarget::Outputs,
                self.outputs_button(view),
                ui::icons::Icon::Volume2,
            ),
        ] {
            let Some(rect) = rect else {
                continue;
            };
            let name = Self::target_name(view, target);
            let open = self.picker
                == Some(if target == CardTarget::Players {
                    Picker::Players
                } else {
                    Picker::Outputs
                });
            Button::paint(
                scene,
                transform,
                text,
                theme,
                &ButtonFace::new(rect, "")
                    .icon(icon)
                    .ghost()
                    .pressed(open)
                    .hovered(cursor.is_some_and(|p| rect.contains(p)))
                    .focused(self.focus == Some(target))
                    .accessible_name(&name),
            );
        }

        let rows = self.picker_rows(view);
        for index in 0..rows {
            let rect = self.picker_row_rect(view, index);
            let row = if self.picker == Some(Picker::Players) {
                view.players.get(index).map(|candidate| {
                    (
                        CardTarget::Player(index),
                        candidate.identity.clone(),
                        candidate.headline().to_string(),
                        index == view.active,
                        ui::icons::Icon::Music,
                    )
                })
            } else {
                view.outputs.get(index).map(|output| {
                    (
                        CardTarget::Output(index),
                        output.label.clone(),
                        tr(match output.kind {
                            OutputKind::Local => "now-playing-output-local",
                            OutputKind::Device => "now-playing-output-device",
                            OutputKind::SpeakerGroup => "now-playing-output-speaker-group",
                        }),
                        output.current,
                        match output.kind {
                            OutputKind::Local => ui::icons::Icon::Volume2,
                            OutputKind::Device => ui::icons::Icon::Monitor,
                            OutputKind::SpeakerGroup => ui::icons::Icon::Layers,
                        },
                    )
                })
            };
            let Some((target, label, detail, selected, icon)) = row else {
                continue;
            };
            Self::draw_picker_row(
                scene,
                transform,
                text,
                theme,
                rect,
                (&label, &detail, selected, icon),
                cursor.is_some_and(|p| rect.contains(p)),
                self.focus == Some(target),
            );
        }
    }

    fn draw_picker_row(
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        (label, detail, selected, icon): (&str, &str, bool, ui::icons::Icon),
        hovered: bool,
        focused: bool,
    ) {
        let colors = &theme.tokens.colors;
        paint_control(scene, transform, theme, rect, hovered, focused);
        let glyph = leading_glyph(rect, CONTROL_INSET, GLYPH);
        ui::icons::draw_icon(scene, transform, glyph, icon, colors.text_dim, 1.5);
        let check = trailing_glyph(rect, CONTROL_INSET, GLYPH);
        if selected {
            ui::icons::draw_icon(
                scene,
                transform,
                check,
                ui::icons::Icon::Check,
                colors.intent.inverted,
                1.5,
            );
        }
        let text_x = glyph.x1 + CONTROL_INSET;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "row widths are bounded by the output; f32 is the shaping precision"
        )]
        let width = (check.x0 - RELATED_GAP - text_x).max(1.0) as f32;
        let label_size = type_size(text, theme.tokens.typography.scale.sm);
        let detail_size = type_size(text, theme.tokens.typography.scale.xs);
        let label = text.elide_to_width_weighted(label, label_size, width, TextWeight::Medium);
        let detail = text.elide_to_width(detail, detail_size, width);
        let (_, label_h) = text.measure_weighted(&label, label_size, None, TextWeight::Medium);
        let (_, detail_h) = text.measure(&detail, detail_size, None);
        let top = centred_text_top(rect, f64::from(label_h) + f64::from(detail_h));
        text.draw_text_weighted(
            scene,
            &label,
            label_size,
            colors.text_primary,
            transform.pre_translate((text_x, top).into()),
            None,
            TextWeight::Medium,
        );
        text.draw_text(
            scene,
            &detail,
            detail_size,
            colors.text_dim,
            transform.pre_translate((text_x, top + f64::from(label_h)).into()),
            None,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::now_playing::tests::{FakeSource, player};
    use crate::shell::now_playing::{NowPlayingSource, OutputTarget};

    fn outputs() -> Vec<OutputTarget> {
        vec![
            OutputTarget {
                token: "audio.route.aaaa".into(),
                label: "Headphones".into(),
                kind: OutputKind::Local,
                current: true,
            },
            OutputTarget {
                token: "audio.route.bbbb".into(),
                label: "Speakers".into(),
                kind: OutputKind::Local,
                current: false,
            },
            OutputTarget {
                token: "media.handoff.cccc.dddd".into(),
                label: "Living room".into(),
                kind: OutputKind::Device,
                current: false,
            },
        ]
    }

    fn view_from(source: &FakeSource, outputs: Vec<OutputTarget>) -> NowPlayingView {
        let players = source.players();
        let now = players[0].observed_at;
        NowPlayingView::new(players, None, outputs, now).expect("a player")
    }

    fn laid_out(view: &NowPlayingView) -> NowPlayingCard {
        let mut card = NowPlayingCard::new();
        let height = card.preferred_height(view);
        card.layout(Rect::new(0.0, 0.0, 360.0, height));
        card
    }

    fn centre(card: &NowPlayingCard, view: &NowPlayingView, target: CardTarget) -> Point {
        card.targets(view)
            .into_iter()
            .find(|(t, _)| *t == target)
            .map_or_else(
                || panic!("{target:?} is not on the card"),
                |(_, rect)| rect.center(),
            )
    }

    fn dispatch(source: &FakeSource, action: CardAction) {
        if let CardAction::Command { bus_name, command } = action {
            assert!(source.send(&bus_name, command));
        }
    }

    #[test]
    fn transport_buttons_drive_exactly_the_shown_player() {
        let source = FakeSource {
            players: vec![
                player("idle", "Idle", PlaybackStatus::Paused),
                player("music", "Music", PlaybackStatus::Playing),
            ],
            ..FakeSource::default()
        };
        let view = view_from(&source, Vec::new());
        let mut card = laid_out(&view);
        for transport in Transport::ALL {
            let point = centre(&card, &view, CardTarget::Transport(transport));
            let action = card.click(&view, point);
            dispatch(&source, action);
        }
        assert_eq!(
            source.sent.borrow().as_slice(),
            [
                (
                    "org.mpris.MediaPlayer2.music".to_string(),
                    PlayerCommand::Previous
                ),
                (
                    "org.mpris.MediaPlayer2.music".to_string(),
                    PlayerCommand::PlayPause
                ),
                (
                    "org.mpris.MediaPlayer2.music".to_string(),
                    PlayerCommand::Next
                ),
            ]
        );
    }

    #[test]
    fn unsupported_transport_is_disabled_not_sent() {
        let mut only_play = player("radio", "Radio", PlaybackStatus::Playing);
        only_play.can_go_next = false;
        only_play.can_go_previous = false;
        only_play.can_seek = false;
        let source = FakeSource {
            players: vec![only_play],
            ..FakeSource::default()
        };
        let view = view_from(&source, Vec::new());
        let mut card = laid_out(&view);
        for transport in [Transport::Previous, Transport::Next] {
            let point = centre(&card, &view, CardTarget::Transport(transport));
            assert_eq!(card.click(&view, point), CardAction::None);
        }
        assert!(
            !card
                .targets(&view)
                .iter()
                .any(|(t, _)| *t == CardTarget::Seek),
            "a player that cannot seek gets a progress bar, not a seek control"
        );
        let a11y = card.accessibility_snapshot(&view);
        let next = a11y
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some(tr("now-playing-next").as_str()))
            .expect("next is published");
        assert!(next.state.disabled);
    }

    #[test]
    fn clicking_the_seek_bar_sets_an_absolute_position() {
        let source = FakeSource {
            players: vec![player("music", "Music", PlaybackStatus::Paused)],
            ..FakeSource::default()
        };
        let view = view_from(&source, Vec::new());
        let mut card = laid_out(&view);
        let face = card.seek_face(&view).expect("seek bar");
        let track = SliderFace::new(face, 0.0).track_rect();
        let action = card.click(
            &view,
            Point::new(track.x0 + track.width() * 0.25, face.center().y),
        );
        assert_eq!(
            action,
            CardAction::Command {
                bus_name: "org.mpris.MediaPlayer2.music".into(),
                command: PlayerCommand::SetPosition {
                    track_id: "/org/tilo/track/1".into(),
                    position_us: 50_000_000,
                },
            }
        );
    }

    #[test]
    fn a_player_picker_appears_only_with_several_players_and_selects_one() {
        let single = FakeSource {
            players: vec![player("music", "Music", PlaybackStatus::Playing)],
            ..FakeSource::default()
        };
        let view = view_from(&single, Vec::new());
        let card = laid_out(&view);
        assert!(
            !card
                .targets(&view)
                .iter()
                .any(|(t, _)| *t == CardTarget::Players)
        );

        let several = FakeSource {
            players: vec![
                player("music", "Music", PlaybackStatus::Playing),
                player("video", "Video", PlaybackStatus::Paused),
            ],
            ..FakeSource::default()
        };
        let view = view_from(&several, Vec::new());
        let mut card = laid_out(&view);
        let closed = card.preferred_height(&view);
        assert_eq!(
            card.click(&view, centre(&card, &view, CardTarget::Players)),
            CardAction::Redraw
        );
        assert_eq!(card.picker(), Some(Picker::Players));
        assert!(card.preferred_height(&view) > closed, "the picker unfolds");
        card.layout(Rect::new(0.0, 0.0, 360.0, card.preferred_height(&view)));
        assert_eq!(
            card.click(&view, centre(&card, &view, CardTarget::Player(1))),
            CardAction::SelectPlayer("org.mpris.MediaPlayer2.video".into())
        );
        assert_eq!(card.picker(), None);
    }

    #[test]
    fn the_output_picker_routes_by_token_and_ignores_the_current_output() {
        let source = FakeSource {
            players: vec![player("music", "Music", PlaybackStatus::Playing)],
            ..FakeSource::default()
        };
        let view = view_from(&source, outputs());
        let mut card = laid_out(&view);
        card.click(&view, centre(&card, &view, CardTarget::Outputs));
        assert_eq!(card.picker(), Some(Picker::Outputs));
        card.layout(Rect::new(0.0, 0.0, 360.0, card.preferred_height(&view)));
        let rows = card
            .targets(&view)
            .into_iter()
            .filter(|(t, _)| matches!(t, CardTarget::Output(_)))
            .count();
        assert_eq!(rows, 3, "local outputs and devices share one list");
        assert_eq!(
            card.click(&view, centre(&card, &view, CardTarget::Output(0))),
            CardAction::Redraw
        );
        card.click(&view, centre(&card, &view, CardTarget::Outputs));
        card.layout(Rect::new(0.0, 0.0, 360.0, card.preferred_height(&view)));
        assert_eq!(
            card.click(&view, centre(&card, &view, CardTarget::Output(2))),
            CardAction::Route("media.handoff.cccc.dddd".into())
        );
    }

    #[test]
    fn keyboard_walks_every_target_then_hands_focus_back() {
        let source = FakeSource {
            players: vec![
                player("music", "Music", PlaybackStatus::Playing),
                player("video", "Video", PlaybackStatus::Paused),
            ],
            ..FakeSource::default()
        };
        let view = view_from(&source, outputs());
        let mut card = laid_out(&view);
        let order = card
            .targets(&view)
            .into_iter()
            .map(|(t, _)| t)
            .collect::<Vec<_>>();
        assert_eq!(
            order,
            vec![
                CardTarget::Seek,
                CardTarget::Players,
                CardTarget::Transport(Transport::Previous),
                CardTarget::Transport(Transport::PlayPause),
                CardTarget::Transport(Transport::Next),
                CardTarget::Outputs,
            ]
        );
        let mut visited = Vec::new();
        while card.focus_next(&view, false) {
            visited.push(card.focus.expect("focused"));
        }
        assert_eq!(visited, order);
        assert!(!card.has_focus(), "focus leaves past the last target");
        assert!(card.focus_next(&view, true));
        assert_eq!(card.focus, Some(CardTarget::Outputs));

        card.focus = Some(CardTarget::Seek);
        assert!(matches!(
            card.adjust_focused(&view, true),
            CardAction::Command {
                command: PlayerCommand::SetPosition { .. },
                ..
            }
        ));
        card.focus = Some(CardTarget::Transport(Transport::PlayPause));
        assert_eq!(
            card.activate_focused(&view),
            CardAction::Command {
                bus_name: "org.mpris.MediaPlayer2.music".into(),
                command: PlayerCommand::PlayPause,
            }
        );
    }

    #[test]
    fn accessibility_children_match_the_hit_targets() {
        let source = FakeSource {
            players: vec![
                player("music", "Music", PlaybackStatus::Playing),
                player("video", "Video", PlaybackStatus::Paused),
            ],
            ..FakeSource::default()
        };
        let view = view_from(&source, outputs());
        let card = laid_out(&view);
        let a11y = card.accessibility_snapshot(&view);
        let targets = card.targets(&view);
        assert_eq!(a11y.children.len(), targets.len());
        for (index, ((_, rect), node)) in targets.iter().zip(&a11y.children).enumerate() {
            assert!(node.bounds.is_some());
            assert_eq!(card.rect_for_path(&view, &[index]), Some(*rect));
            assert!(node.name.as_deref().is_some_and(|name| !name.is_empty()));
        }
        assert_eq!(a11y.name, Some(tr("now-playing-title")));
        assert!(
            a11y.description
                .as_deref()
                .is_some_and(|description| description.contains("Music track"))
        );
    }

    #[test]
    fn the_card_paints_in_every_theme_with_and_without_artwork() {
        let mut with_art = player("music", "Music", PlaybackStatus::Playing);
        with_art.artwork = Some(crate::shell::now_playing::Artwork {
            width: 2,
            height: 2,
            rgba: Arc::new(vec![255; 16]),
        });
        for players in [
            vec![with_art.clone()],
            vec![with_art, player("video", "Video", PlaybackStatus::Paused)],
        ] {
            let source = FakeSource {
                players,
                ..FakeSource::default()
            };
            let view = view_from(&source, outputs());
            for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
                let mut card = laid_out(&view);
                let mut text = TextRenderer::new();
                let mut scene = Scene::new();
                let rect = card.rect();
                card.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut text,
                    &theme,
                    rect,
                    None,
                    &view,
                );
                assert!(!scene.encoding().is_empty());
            }
        }
    }

    #[test]
    fn transport_rects_never_overlap_and_meet_the_pointer_floor() {
        let rects = transport_rects(Rect::new(0.0, 0.0, 300.0, ROW_HEIGHT), 1.0);
        for (index, (_, rect)) in rects.iter().enumerate() {
            assert!(rect.width() >= TARGET && rect.height() >= TARGET);
            for (_, other) in rects.iter().skip(index + 1) {
                assert!(rect.intersect(*other).area() <= 0.0);
            }
        }
    }
}
