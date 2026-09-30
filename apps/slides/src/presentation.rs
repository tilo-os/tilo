//! Presentation-mode state, input routing, and presentation-only chrome.
//!
//! The editor owns the deck and applies navigation commands. This component owns
//! every transient concern of a running talk: timer, blanking, presenter/notes
//! views, cross-fades, and the pointer/key policy that chooses a command.

use ui::time::{Duration, Instant};

use vello::Scene;
use vello::kurbo::{Affine, Rect, Vec2};
use vello::peniko::{BlendMode, Color, Fill};
use winit::keyboard::{Key, NamedKey};

use ui::paint::PaintCtx;
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;

use ui::i18n::ArgValue;

use crate::assets::ImageStore;
use crate::model::{Deck, Palette, PaletteKind, SLIDE_H, SLIDE_W};
use crate::render::DeckAssets;
use crate::strings::{tr, tr_args};
use crate::{chrome, render};

const TRANSITION_MS: u128 = 240;
/// The presentation / presenter stage ground every frame and panel sits on.
const STAGE: Color = Color::BLACK;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Blank {
    None,
    Black,
    White,
}

impl Blank {
    pub(crate) fn is_blanked(self) -> bool {
        !matches!(self, Self::None)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Command {
    None,
    Advance,
    Reverse,
    First,
    Last,
    Exit,
}

/// All app-local state for an active presentation.
pub(crate) struct Presentation {
    started: Option<Instant>,
    paused_elapsed: Option<Duration>,
    paused_total: Duration,
    transition: Option<(usize, Instant)>,
    blank: Blank,
    presenter_view: bool,
    /// A real, host-owned audience toplevel is live. The app's original
    /// window stays local and renders the dashboard in this state.
    external_audience: bool,
    show_notes: bool,
    /// Shaped settled audience slide. Presentation frames keep painting live
    /// chrome (timer, notes, progress), but the authored slide only needs to be
    /// reshaped when its cache key changes.
    cache: Option<PresentCache>,
    /// Separate cache because the audience has its own output dimensions.
    audience_cache: Option<PresentCache>,
}

/// Read-only app state needed to render one presentation frame. Deck mutation
/// and navigation remain in `SlidesApp`; this view lets the presentation own
/// all transient drawing and cache behavior without reaching into the editor.
pub(crate) struct PresentationFrame<'a> {
    rect: Rect,
    deck: &'a Deck,
    current: usize,
    revision: u64,
    images: &'a ImageStore,
}

impl<'a> PresentationFrame<'a> {
    pub(crate) fn new(
        rect: Rect,
        deck: &'a Deck,
        current: usize,
        revision: u64,
        images: &'a ImageStore,
    ) -> Self {
        Self {
            rect,
            deck,
            current,
            revision,
            images,
        }
    }

    fn slide(&self) -> Option<&crate::model::Slide> {
        self.deck.slides.get(self.current)
    }
}

/// One shaped audience slide, retained across present-mode frames.
struct PresentCache {
    slide: usize,
    revision: u64,
    palette: PaletteKind,
    /// Pixel-rounded destination so harmless subpixel jitter does not reshape.
    dest: (i64, i64, i64, i64),
    /// Painted at `Affine::IDENTITY`; the live transform is applied on append.
    scene: Scene,
}

impl PresentCache {
    fn key(
        dest: Rect,
        slide: usize,
        revision: u64,
        palette: PaletteKind,
    ) -> (usize, u64, PaletteKind, (i64, i64, i64, i64)) {
        (
            slide,
            revision,
            palette,
            (
                dest.x0.round() as i64,
                dest.y0.round() as i64,
                dest.x1.round() as i64,
                dest.y1.round() as i64,
            ),
        )
    }

    fn matches(&self, dest: Rect, slide: usize, revision: u64, palette: PaletteKind) -> bool {
        (self.slide, self.revision, self.palette, self.dest)
            == Self::key(dest, slide, revision, palette)
    }
}

impl Default for Presentation {
    fn default() -> Self {
        Self {
            started: None,
            paused_elapsed: None,
            paused_total: Duration::ZERO,
            transition: None,
            blank: Blank::None,
            presenter_view: false,
            external_audience: false,
            show_notes: false,
            cache: None,
            audience_cache: None,
        }
    }
}

impl Presentation {
    pub(crate) fn start(&mut self) {
        self.blank = Blank::None;
        self.transition = None;
        self.started = Some(ui::time::now());
        self.paused_elapsed = None;
        self.paused_total = Duration::ZERO;
    }

    pub(crate) fn leave(&mut self) {
        self.blank = Blank::None;
        self.presenter_view = false;
        self.external_audience = false;
        self.transition = None;
    }

    /// Drop the shaped audience slide after an image decode changes pixels
    /// without changing the deck revision/cache key.
    pub(crate) fn invalidate_cache(&mut self) {
        self.cache = None;
        self.audience_cache = None;
    }

    pub(crate) fn start_transition(&mut self, from: usize) {
        self.transition = Some((from, ui::time::now()));
        self.blank = Blank::None;
    }

    pub(crate) fn expire_transition(&mut self, reduce_motion: bool) {
        if reduce_motion
            || self
                .transition
                .is_some_and(|(_, at)| at.elapsed().as_millis() >= TRANSITION_MS)
        {
            self.transition = None;
        }
    }

    pub(crate) fn transition_alpha(&self, reduce_motion: bool) -> f32 {
        if reduce_motion {
            return 1.0;
        }
        match self.transition {
            Some((_, at)) => {
                let p = at.elapsed().as_millis().min(TRANSITION_MS) as f32 / TRANSITION_MS as f32;
                (p * p * (3.0 - 2.0 * p)).clamp(0.0, 1.0)
            }
            None => 1.0,
        }
    }

    pub(crate) fn needs_animation_frame(&self) -> bool {
        self.transition.is_some()
    }

    pub(crate) fn elapsed(&self) -> Duration {
        if let Some(frozen) = self.paused_elapsed {
            return frozen;
        }
        self.started.map_or(Duration::ZERO, |start| {
            start.elapsed().saturating_sub(self.paused_total)
        })
    }

    pub(crate) fn clock(&self) -> String {
        ui::format::timecode(self.elapsed().as_secs())
    }

    pub(crate) fn is_paused(&self) -> bool {
        self.paused_elapsed.is_some()
    }

    #[cfg(test)]
    pub(crate) fn is_presenter_view(&self) -> bool {
        self.presenter_view
    }

    pub(crate) fn set_show_notes(&mut self, show: bool) {
        self.show_notes = show;
    }

    pub(crate) fn set_presenter_view(&mut self, show: bool) {
        self.presenter_view = show;
    }

    pub(crate) fn set_external_audience(&mut self, active: bool) {
        self.external_audience = active;
        if !active {
            self.audience_cache = None;
        }
    }

    pub(crate) fn has_external_audience(&self) -> bool {
        self.external_audience
    }

    pub(crate) fn set_blank(&mut self, blank: Blank) {
        self.blank = blank;
    }

    #[cfg(test)]
    pub(crate) fn blank(&self) -> Blank {
        self.blank
    }

    pub(crate) fn pointer_command(&mut self) -> Command {
        if self.blank.is_blanked() {
            self.blank = Blank::None;
            Command::None
        } else if self.presenter_view || self.external_audience {
            Command::None
        } else {
            Command::Advance
        }
    }

    /// Route presentation-only keyboard input. Deck navigation is returned as
    /// a command so this component never reaches into the shared document.
    pub(crate) fn handle_key(&mut self, key: &Key) -> Option<Command> {
        let command = match key {
            Key::Named(
                NamedKey::ArrowRight
                | NamedKey::ArrowDown
                | NamedKey::Space
                | NamedKey::Enter
                | NamedKey::PageDown,
            ) => self.resume_or(Command::Advance),
            Key::Named(
                NamedKey::ArrowLeft | NamedKey::ArrowUp | NamedKey::PageUp | NamedKey::Backspace,
            ) => self.resume_or(Command::Reverse),
            Key::Named(NamedKey::Home) => Command::First,
            Key::Named(NamedKey::End) => Command::Last,
            Key::Named(NamedKey::Escape) => Command::Exit,
            Key::Character(c) if c.eq_ignore_ascii_case("p") => {
                self.presenter_view = !self.presenter_view;
                Command::None
            }
            Key::Character(c) if c.eq_ignore_ascii_case("s") => {
                self.show_notes = !self.show_notes;
                Command::None
            }
            Key::Character(c) if c.eq_ignore_ascii_case("b") => {
                self.blank = if self.blank == Blank::Black {
                    Blank::None
                } else {
                    Blank::Black
                };
                Command::None
            }
            Key::Character(c) if c.eq_ignore_ascii_case("w") => {
                self.blank = if self.blank == Blank::White {
                    Blank::None
                } else {
                    Blank::White
                };
                Command::None
            }
            Key::Character(c) if c.eq_ignore_ascii_case("k") => {
                self.toggle_timer();
                Command::None
            }
            Key::Character(c) if c.as_str() == "0" => {
                self.reset_timer();
                Command::None
            }
            _ => return None,
        };
        Some(command)
    }

    fn resume_or(&mut self, command: Command) -> Command {
        if self.blank.is_blanked() {
            self.blank = Blank::None;
            Command::None
        } else {
            command
        }
    }

    fn toggle_timer(&mut self) {
        match self.paused_elapsed.take() {
            Some(frozen) => {
                if let Some(start) = self.started {
                    self.paused_total = start.elapsed().saturating_sub(frozen);
                }
            }
            None => self.paused_elapsed = Some(self.elapsed()),
        }
    }

    fn reset_timer(&mut self) {
        self.started = Some(ui::time::now());
        self.paused_elapsed = None;
        self.paused_total = Duration::ZERO;
    }

    /// Draw a blanked audience surface. Returns true when it fully owns the
    /// frame, allowing the caller to skip deck rendering.
    pub(crate) fn draw_blank(&self, scene: &mut Scene, transform: Affine, rect: Rect) -> bool {
        let background = match self.blank {
            Blank::None => return false,
            Blank::White => Color::WHITE,
            Blank::Black => Color::BLACK,
        };
        chrome::fill_rect(scene, transform, rect, background);
        true
    }

    pub(crate) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        frame: &PresentationFrame<'_>,
    ) {
        let palette = Palette::resolve(frame.deck.palette, theme);
        if self.presenter_view || self.external_audience {
            self.draw_presenter(scene, transform, text, theme, frame, &palette);
            return;
        }

        chrome::fill_rect(scene, transform, frame.rect, STAGE);
        if self.draw_blank(scene, transform, frame.rect) {
            return;
        }

        let (area, notes_panel) = if self.show_notes {
            let split = frame.rect.y0 + frame.rect.height() * 0.66;
            (
                Rect::new(frame.rect.x0, frame.rect.y0, frame.rect.x1, split),
                Some(Rect::new(
                    frame.rect.x0,
                    split,
                    frame.rect.x1,
                    frame.rect.y1,
                )),
            )
        } else {
            (frame.rect, None)
        };
        let available_width = area.width() - 48.0;
        let available_height = area.height() - 48.0;
        let scale = (available_width / SLIDE_W).min(available_height / SLIDE_H);
        let width = SLIDE_W * scale;
        let height = SLIDE_H * scale;
        let x = area.x0 + (area.width() - width) / 2.0;
        let y = area.y0 + (area.height() - height) / 2.0;
        let destination = Rect::new(x, y, x + width, y + height);

        let alpha = self.transition_alpha(theme.reduce_motion);
        if let Some((from, _)) = self.transition
            && alpha < 1.0
            && let Some(previous) = frame.deck.slides.get(from)
        {
            render::draw_slide(
                &mut PaintCtx::new(scene, transform, text, theme),
                DeckAssets {
                    palette: &palette,
                    images: frame.images,
                },
                previous,
                destination,
                0.0,
            );
        }
        if alpha < 1.0 {
            if let Some(slide) = frame.slide() {
                scene.push_layer(
                    Fill::NonZero,
                    BlendMode::default(),
                    alpha,
                    transform,
                    &destination,
                );
                render::draw_slide(
                    &mut PaintCtx::new(scene, transform, text, theme),
                    DeckAssets {
                        palette: &palette,
                        images: frame.images,
                    },
                    slide,
                    destination,
                    0.0,
                );
                scene.pop_layer();
            }
        } else {
            Self::append_settled_slide(
                &mut self.cache,
                scene,
                transform,
                text,
                theme,
                frame,
                &palette,
                destination,
            );
        }

        if let Some(panel) = notes_panel {
            self.draw_notes_panel(scene, transform, text, theme, frame, &palette, panel);
        }
    }

    fn append_settled_slide(
        cache_slot: &mut Option<PresentCache>,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        frame: &PresentationFrame<'_>,
        palette: &Palette,
        destination: Rect,
    ) -> bool {
        let stale = cache_slot.as_ref().is_none_or(|cache| {
            !cache.matches(
                destination,
                frame.current,
                frame.revision,
                frame.deck.palette,
            )
        });
        if stale {
            let mut fragment = Scene::new();
            if let Some(slide) = frame.slide() {
                render::draw_slide(
                    &mut PaintCtx::new(&mut fragment, Affine::IDENTITY, text, theme),
                    DeckAssets {
                        palette,
                        images: frame.images,
                    },
                    slide,
                    destination,
                    0.0,
                );
            }
            let (_, _, _, dest) = PresentCache::key(
                destination,
                frame.current,
                frame.revision,
                frame.deck.palette,
            );
            *cache_slot = Some(PresentCache {
                slide: frame.current,
                revision: frame.revision,
                palette: frame.deck.palette,
                dest,
                scene: fragment,
            });
        }
        if let Some(cache) = cache_slot {
            scene.append(&cache.scene, Some(transform));
        }
        stale
    }

    /// Paint the clean audience projection for the dedicated toplevel. It
    /// deliberately bypasses presenter/notes toggles: those are local control
    /// state and must never leak onto the room display.
    pub(crate) fn draw_audience(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        frame: &PresentationFrame<'_>,
    ) {
        let palette = Palette::resolve(frame.deck.palette, theme);
        chrome::fill_rect(scene, transform, frame.rect, STAGE);
        if self.draw_blank(scene, transform, frame.rect) {
            return;
        }
        let available_width = frame.rect.width() - 48.0;
        let available_height = frame.rect.height() - 48.0;
        let scale = (available_width / SLIDE_W).min(available_height / SLIDE_H);
        let width = SLIDE_W * scale;
        let height = SLIDE_H * scale;
        let x = frame.rect.x0 + (frame.rect.width() - width) / 2.0;
        let y = frame.rect.y0 + (frame.rect.height() - height) / 2.0;
        let destination = Rect::new(x, y, x + width, y + height);
        let alpha = self.transition_alpha(theme.reduce_motion);
        if let Some((from, _)) = self.transition
            && alpha < 1.0
            && let Some(previous) = frame.deck.slides.get(from)
        {
            render::draw_slide(
                &mut PaintCtx::new(scene, transform, text, theme),
                DeckAssets {
                    palette: &palette,
                    images: frame.images,
                },
                previous,
                destination,
                0.0,
            );
        }
        if alpha < 1.0 {
            if let Some(slide) = frame.slide() {
                scene.push_layer(
                    Fill::NonZero,
                    BlendMode::default(),
                    alpha,
                    transform,
                    &destination,
                );
                render::draw_slide(
                    &mut PaintCtx::new(scene, transform, text, theme),
                    DeckAssets {
                        palette: &palette,
                        images: frame.images,
                    },
                    slide,
                    destination,
                    0.0,
                );
                scene.pop_layer();
            }
        } else {
            Self::append_settled_slide(
                &mut self.audience_cache,
                scene,
                transform,
                text,
                theme,
                frame,
                &palette,
                destination,
            );
        }
    }

    fn draw_notes_panel(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        frame: &PresentationFrame<'_>,
        palette: &Palette,
        panel: Rect,
    ) {
        let raised = theme.tokens.colors.surfaces.raised;
        chrome::fill_rect(scene, transform, panel, raised);
        // The panel's top rule is its own derived edge, crisp on its first
        // pixel row.
        chrome::hairline_h(
            scene,
            transform,
            panel.x0,
            panel.x1,
            panel.y0,
            ui::surface::edge_on(raised),
        );
        let x = panel.x0 + 40.0;
        text.draw_text_weighted(
            scene,
            "SPEAKER NOTES",
            12.0,
            palette.accent,
            transform * Affine::translate((x, panel.y0 + 20.0)),
            None,
            TextWeight::Semibold,
        );
        let preview_width = panel.width() * 0.26;
        let preview_x = panel.x1 - 40.0 - preview_width;
        let clock = self.clock();
        let (clock_width, _) = text.measure_weighted(&clock, 30.0, None, TextWeight::Bold);
        text.draw_text_weighted(
            scene,
            &clock,
            30.0,
            theme.tokens.colors.text_primary,
            transform
                * Affine::translate((preview_x - 36.0 - f64::from(clock_width), panel.y0 + 16.0)),
            None,
            TextWeight::Bold,
        );
        let notes = frame.slide().map_or("", |slide| slide.notes.as_str());
        text.draw_text_weighted(
            scene,
            if notes.is_empty() { "—" } else { notes },
            18.0,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((x, panel.y0 + 44.0)),
            Some((panel.width() * 0.6) as f32),
            TextWeight::Regular,
        );
        if let Some(next) = frame.deck.slides.get(frame.current + 1) {
            let preview_height = preview_width * SLIDE_H / SLIDE_W;
            let preview = Rect::new(
                preview_x,
                panel.y0 + 28.0,
                panel.x1 - 40.0,
                panel.y0 + 28.0 + preview_height,
            );
            text.draw_text_weighted(
                scene,
                "NEXT",
                11.0,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((preview.x0, preview.y0 - 18.0)),
                None,
                TextWeight::Semibold,
            );
            render::draw_slide(
                &mut PaintCtx::new(scene, transform, text, theme),
                DeckAssets {
                    palette,
                    images: frame.images,
                },
                next,
                preview,
                6.0,
            );
            // A frame on the raised notes panel: its derived edge.
            chrome::stroke_round(
                scene,
                transform,
                preview,
                6.0,
                ui::surface::edge_on(raised),
                1.0,
            );
        }
    }

    fn draw_presenter(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        frame: &PresentationFrame<'_>,
        palette: &Palette,
    ) {
        chrome::fill_rect(
            scene,
            transform,
            frame.rect,
            theme.tokens.colors.surfaces.base,
        );
        // Header and footer bands follow the measured line boxes of the text
        // they hold (classic 60 / 40 px as the 1× floors), so a 2× title
        // never paints over the PRESENTER VIEW caption or the position line
        // over the timer, and the key hints keep clear of the notes panel.
        let m = PresenterMetrics::resolve(text, theme);
        let outer = 28.0;
        let body = Rect::new(
            frame.rect.x0 + outer,
            frame.rect.y0 + m.header_h(),
            frame.rect.x1 - outer,
            frame.rect.y1 - m.footer_h(),
        );
        let view = PresenterView {
            frame,
            palette,
            outer,
            m,
        };
        let ctx = &mut PaintCtx::new(scene, transform, text, theme);
        self.draw_presenter_header(ctx, &view);

        let left_width = (body.width() * 0.62).min(body.width() - 320.0);
        let current = Rect::new(body.x0, body.y0, body.x0 + left_width, body.y1);
        let right = Rect::new(current.x1 + 24.0, body.y0, body.x1, body.y1);
        draw_presenter_current(ctx, &view, current);
        let next = draw_presenter_next(ctx, &view, right);
        draw_presenter_notes(ctx, &view, Rect::new(right.x0, next.y1, right.x1, body.y1));
        draw_presenter_footer(ctx, &view);
    }

    fn draw_presenter_header(&self, ctx: &mut PaintCtx<'_>, view: &PresenterView<'_>) {
        let (scene, transform, text) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text);
        let colors = &ctx.theme.tokens.colors;
        let PresenterView {
            frame,
            palette,
            outer,
            m,
        } = view;
        let clock = self.clock();
        let (clock_width, _) = text.measure_weighted(&clock, FONT_CLOCK, None, TextWeight::Bold);
        let position = tr_args(
            "slides-position",
            &[
                (
                    "state",
                    ArgValue::from(tr(if self.is_paused() {
                        "slides-paused"
                    } else {
                        "slides-elapsed"
                    })),
                ),
                ("current", ArgValue::from(frame.current + 1)),
                ("total", ArgValue::from(frame.deck.slides.len())),
            ],
        );
        let (position_width, _) =
            text.measure_weighted(&position, FONT_POSITION, None, TextWeight::Semibold);
        // The title yields to whichever right-hand line is wider, `s4` away,
        // and is elided rather than running under the clock.
        let right_w = f64::from(clock_width).max(f64::from(position_width));
        let title_w = (frame.rect.width() - 2.0 * outer - right_w - m.s4).max(0.0);
        let title = text.elide_to_width_weighted(
            &frame.deck.title,
            FONT_TITLE,
            title_w as f32,
            TextWeight::Bold,
        );
        text.draw_text_weighted(
            scene,
            &title,
            FONT_TITLE,
            colors.text_primary,
            transform * Affine::translate((frame.rect.x0 + outer, frame.rect.y0 + TITLE_TOP)),
            None,
            TextWeight::Bold,
        );
        text.draw_text_weighted(
            scene,
            &tr("slides-presenter-view"),
            FONT_CAPTION,
            palette.accent,
            transform * Affine::translate((frame.rect.x0 + outer, frame.rect.y0 + m.caption_top())),
            None,
            TextWeight::Semibold,
        );
        text.draw_text_weighted(
            scene,
            &clock,
            FONT_CLOCK,
            if self.is_paused() {
                colors.text_dim
            } else {
                colors.text_primary
            },
            transform
                * Affine::translate((
                    frame.rect.x1 - outer - f64::from(clock_width),
                    frame.rect.y0 + CLOCK_TOP,
                )),
            None,
            TextWeight::Bold,
        );
        text.draw_text_weighted(
            scene,
            &position,
            FONT_POSITION,
            colors.text_dim,
            transform
                * Affine::translate((
                    frame.rect.x1 - outer - f64::from(position_width),
                    frame.rect.y0 + m.position_top(),
                )),
            None,
            TextWeight::Semibold,
        );
    }
}

/// One presenter-dashboard frame: the deck view, its resolved palette, the
/// side inset of its header, body and footer, and the measured chrome metrics
/// every panel lays out against.
struct PresenterView<'a> {
    frame: &'a PresentationFrame<'a>,
    palette: &'a Palette,
    outer: f64,
    m: PresenterMetrics,
}

/// Presenter-view chrome fonts. Passed unscaled to the renderer, which
/// applies the accessibility scale once.
const FONT_TITLE: f32 = 18.0;
const FONT_CAPTION: f32 = 11.0;
const FONT_CLOCK: f32 = 34.0;
const FONT_POSITION: f32 = 12.0;
const FONT_FOOTER: f32 = 12.0;
/// Classic 1× offsets of the header's two columns.
const TITLE_TOP: f64 = 18.0;
const CLOCK_TOP: f64 = 12.0;
/// Classic 1× advances, kept as the floors of the measured ones.
const CAPTION_ADV_1X: f64 = 22.0;
const SECTION_CAPTION_ADV_1X: f64 = 20.0;
const CLOCK_ADV_1X: f64 = 32.0;
const HEADER_H_1X: f64 = 60.0;
const FOOTER_H_1X: f64 = 40.0;

/// Measured line boxes of the presenter view's chrome text plus the theme's
/// spacing tokens: every band and caption advance below derives from these,
/// with the classic 1× values as floors so the 1× dashboard is unchanged.
struct PresenterMetrics {
    lh_title: f64,
    lh_caption: f64,
    lh_clock: f64,
    lh_position: f64,
    lh_footer: f64,
    s1: f64,
    s3: f64,
    s4: f64,
}

impl PresenterMetrics {
    fn resolve(text: &mut TextRenderer, theme: &Theme) -> Self {
        let lh = |text: &mut TextRenderer, font: f32, weight: TextWeight| {
            f64::from(text.measure_weighted("Ag", font, None, weight).1)
        };
        let sp = &theme.tokens.spacing;
        Self {
            lh_title: lh(text, FONT_TITLE, TextWeight::Bold),
            lh_caption: lh(text, FONT_CAPTION, TextWeight::Semibold),
            lh_clock: lh(text, FONT_CLOCK, TextWeight::Bold),
            lh_position: lh(text, FONT_POSITION, TextWeight::Semibold),
            lh_footer: lh(text, FONT_FOOTER, TextWeight::Regular),
            s1: f64::from(sp.s1),
            s3: f64::from(sp.s3),
            s4: f64::from(sp.s4),
        }
    }

    /// Top of the PRESENTER VIEW caption: under the title's line box.
    fn caption_top(&self) -> f64 {
        TITLE_TOP + self.lh_title.max(CAPTION_ADV_1X)
    }

    /// Top of the "ELAPSED · slide n / m" line: under the clock's digits
    /// (the clock line box carries descender room the digits never use, so
    /// `s3` of it is reclaimed, never less than the classic advance).
    fn position_top(&self) -> f64 {
        CLOCK_TOP + (self.lh_clock - self.s3).max(CLOCK_ADV_1X)
    }

    /// Advance from a section caption's top to the content under it.
    fn caption_adv(&self) -> f64 {
        (self.lh_caption + self.s1).max(SECTION_CAPTION_ADV_1X)
    }

    /// Header band: whichever column ends lower, never below the classic.
    fn header_h(&self) -> f64 {
        (self.caption_top() + self.caption_adv())
            .max(self.position_top() + self.lh_position)
            .max(HEADER_H_1X)
    }

    /// Footer band holding the key hints and the progress track.
    fn footer_h(&self) -> f64 {
        (self.lh_footer + 2.0 * self.s3).max(FOOTER_H_1X)
    }

    /// Top of the key-hint line, centred in the footer band above `y1`.
    fn footer_text_top(&self, y1: f64) -> f64 {
        let h = self.footer_h();
        y1 - h + (h - self.lh_footer) / 2.0
    }
}

fn draw_progress(
    scene: &mut Scene,
    transform: Affine,
    frame: &PresentationFrame<'_>,
    palette: &Palette,
    height: f64,
    track_color: Color,
    track_alpha: u8,
) {
    let fraction = (frame.current + 1) as f64 / frame.deck.slides.len().max(1) as f64;
    let track = Rect::new(
        frame.rect.x0,
        frame.rect.y1 - height,
        frame.rect.x1,
        frame.rect.y1,
    );
    chrome::fill_rect(
        scene,
        transform,
        track,
        render::with_alpha(track_color, track_alpha),
    );
    chrome::fill_rect(
        scene,
        transform,
        Rect::new(
            track.x0,
            track.y0,
            track.x0 + track.width() * fraction,
            track.y1,
        ),
        palette.accent,
    );
}

fn draw_presenter_current(ctx: &mut PaintCtx<'_>, view: &PresenterView<'_>, area: Rect) {
    let PresenterView {
        frame, palette, m, ..
    } = view;
    let (scene, transform) = (&mut *ctx.scene, ctx.transform);
    ctx.text.draw_text_weighted(
        scene,
        &tr("slides-current"),
        FONT_CAPTION,
        ctx.theme.tokens.colors.text_dim,
        transform * Affine::translate((area.x0, area.y0)),
        None,
        TextWeight::Semibold,
    );
    let destination = fit_slide_top(Rect::new(
        area.x0,
        area.y0 + m.caption_adv(),
        area.x1,
        area.y1,
    ));
    chrome::fill_round(
        scene,
        transform,
        (destination + Vec2::new(0.0, 6.0)).inflate(2.0, 2.0),
        12.0,
        render::with_alpha(Color::BLACK, 80),
    );
    if let Some(slide) = frame.slide() {
        render::draw_slide(
            ctx,
            DeckAssets {
                palette,
                images: frame.images,
            },
            slide,
            destination,
            12.0,
        );
    }
    // The slide frame is the stage's derived edge (the presenter stage is
    // painted `Color::BLACK`, see `draw_presenter`).
    chrome::stroke_round(
        ctx.scene,
        transform,
        destination,
        12.0,
        ui::surface::edge_on(STAGE),
        1.0,
    );
}

fn draw_presenter_next(ctx: &mut PaintCtx<'_>, view: &PresenterView<'_>, area: Rect) -> Rect {
    let PresenterView {
        frame, palette, m, ..
    } = view;
    let (transform, colors) = (ctx.transform, &ctx.theme.tokens.colors);
    ctx.text.draw_text_weighted(
        ctx.scene,
        &tr("slides-next"),
        FONT_CAPTION,
        colors.text_dim,
        transform * Affine::translate((area.x0, area.y0)),
        None,
        TextWeight::Semibold,
    );
    let top = area.y0 + m.caption_adv();
    let destination = Rect::new(
        area.x0,
        top,
        area.x1,
        top + area.width() * SLIDE_H / SLIDE_W,
    );
    if let Some(next) = frame.deck.slides.get(frame.current + 1) {
        render::draw_slide(
            ctx,
            DeckAssets {
                palette,
                images: frame.images,
            },
            next,
            destination,
            8.0,
        );
        chrome::stroke_round(
            ctx.scene,
            transform,
            destination,
            8.0,
            ui::surface::edge_on(STAGE),
            1.0,
        );
    } else {
        ui::surface::card(
            ctx.scene,
            transform,
            destination,
            8.0,
            colors.surfaces.raised,
        );
        chrome::text_centered(
            ctx,
            &tr("slides-end-of-deck"),
            destination,
            &TextStyle::new(14.0).with_weight(TextWeight::Semibold),
            colors.text_dim,
        );
    }
    destination
}

/// The speaker-notes caption and panel filling `column`, whose top is the
/// bottom of the next-slide preview.
fn draw_presenter_notes(ctx: &mut PaintCtx<'_>, view: &PresenterView<'_>, column: Rect) {
    let PresenterView { frame, m, .. } = view;
    let (scene, transform, text) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text);
    let colors = &ctx.theme.tokens.colors;
    let top = column.y0 + 22.0;
    text.draw_text_weighted(
        scene,
        &tr("slides-speaker-notes"),
        FONT_CAPTION,
        colors.text_dim,
        transform * Affine::translate((column.x0, top)),
        None,
        TextWeight::Semibold,
    );
    let panel = Rect::new(column.x0, top + m.caption_adv(), column.x1, column.y1);
    ui::surface::card(scene, transform, panel, 10.0, colors.surfaces.raised);
    let notes = frame.slide().map_or("", |slide| slide.notes.as_str());
    let no_notes = tr("slides-no-notes");
    text.draw_text_weighted(
        scene,
        if notes.is_empty() {
            no_notes.as_str()
        } else {
            notes
        },
        16.0,
        if notes.is_empty() {
            colors.text_dim
        } else {
            colors.text_primary
        },
        transform * Affine::translate((panel.x0 + 16.0, panel.y0 + 14.0)),
        Some((panel.width() - 32.0) as f32),
        TextWeight::Regular,
    );
}

fn draw_presenter_footer(ctx: &mut PaintCtx<'_>, view: &PresenterView<'_>) {
    let PresenterView {
        frame,
        palette,
        outer,
        m,
    } = view;
    let (scene, transform, text) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text);
    let colors = &ctx.theme.tokens.colors;
    // Centred in the footer band and elided to the window, so the 2× hints
    // end inside the frame instead of running off its right edge.
    let hints = text.elide_to_width_weighted(
        &tr("slides-presenter-keys"),
        FONT_FOOTER,
        (frame.rect.width() - 2.0 * outer).max(0.0) as f32,
        TextWeight::Regular,
    );
    text.draw_text_weighted(
        scene,
        &hints,
        FONT_FOOTER,
        colors.text_dim,
        transform * Affine::translate((frame.rect.x0 + outer, m.footer_text_top(frame.rect.y1))),
        None,
        TextWeight::Regular,
    );
    draw_progress(
        scene,
        transform,
        frame,
        palette,
        3.0,
        colors.text_primary,
        26,
    );
}

fn fit_slide_top(area: Rect) -> Rect {
    let scale = (area.width() / SLIDE_W).min(area.height() / SLIDE_H);
    let width = SLIDE_W * scale;
    let height = SLIDE_H * scale;
    let x = area.x0 + (area.width() - width) / 2.0;
    Rect::new(x, area.y0, x + width, area.y0 + height)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Blank navigation, presenter clicks and the paused timer.
    #[test]
    fn presentation_navigation_and_timer_state() {
        // Case `blank_navigation_resumes_before_advancing`.
        {
            let mut p = Presentation::default();
            p.set_blank(Blank::Black);
            assert_eq!(
                p.handle_key(&Key::Named(NamedKey::ArrowRight)),
                Some(Command::None)
            );
            assert_eq!(
                p.handle_key(&Key::Named(NamedKey::ArrowRight)),
                Some(Command::Advance)
            );
        }

        // Case `presenter_click_never_advances`.
        {
            let mut p = Presentation::default();
            p.set_presenter_view(true);
            assert_eq!(p.pointer_command(), Command::None);
        }

        // Case `paused_timer_is_stable_and_reset_resumes`.
        {
            let mut p = Presentation::default();
            p.start();
            p.handle_key(&Key::Character("k".into()));
            let frozen = p.elapsed();
            std::thread::sleep(Duration::from_millis(10));
            assert_eq!(p.elapsed(), frozen);
            p.handle_key(&Key::Character("0".into()));
            assert!(!p.is_paused());
            assert!(p.elapsed() < Duration::from_millis(50));
        }
    }

    /// The settled slide cache reuses unchanged renders and keys every input.
    #[test]
    fn settled_slide_cache_reuses_and_keys_every_input() {
        // Case `settled_slide_cache_reuses_an_unchanged_render`.
        {
            let deck = crate::model::seed_deck();
            let images = ImageStore::new(tilo_content::ContentStore::memory());
            let frame =
                PresentationFrame::new(Rect::new(0.0, 0.0, 1280.0, 720.0), &deck, 1, 7, &images);
            let theme = Theme::dark();
            let palette = Palette::resolve(deck.palette, &theme);
            let destination = Rect::new(100.0, 50.0, 1180.0, 660.0);
            let mut presentation = Presentation::default();
            let mut text = TextRenderer::new();

            assert!(Presentation::append_settled_slide(
                &mut presentation.cache,
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut text,
                &theme,
                &frame,
                &palette,
                destination,
            ));
            assert!(!Presentation::append_settled_slide(
                &mut presentation.cache,
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut text,
                &theme,
                &frame,
                &palette,
                destination,
            ));

            presentation.invalidate_cache();
            assert!(Presentation::append_settled_slide(
                &mut presentation.cache,
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut text,
                &theme,
                &frame,
                &palette,
                destination,
            ));
        }

        // Case `settled_slide_cache_key_covers_all_render_inputs`.
        {
            let destination = Rect::new(100.2, 50.2, 1180.2, 660.2);
            let base = PresentCache::key(destination, 1, 7, PaletteKind::Indigo);
            assert_eq!(
                base,
                PresentCache::key(
                    Rect::new(100.4, 50.4, 1180.4, 660.4),
                    1,
                    7,
                    PaletteKind::Indigo,
                ),
                "subpixel destination jitter remains a cache hit"
            );
            assert_ne!(
                base,
                PresentCache::key(destination, 2, 7, PaletteKind::Indigo)
            );
            assert_ne!(
                base,
                PresentCache::key(destination, 1, 8, PaletteKind::Indigo)
            );
            assert_ne!(
                base,
                PresentCache::key(destination, 1, 7, PaletteKind::Amber)
            );
            assert_ne!(
                base,
                PresentCache::key(
                    Rect::new(101.2, 50.2, 1181.2, 660.2),
                    1,
                    7,
                    PaletteKind::Indigo,
                )
            );
        }
    }
}
