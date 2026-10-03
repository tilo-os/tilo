//! Presentation-mode state, input routing, and presentation-only chrome.
//!
//! The editor owns the deck and applies navigation commands. This component owns
//! every transient concern of a running talk: timer, blanking, presenter/notes
//! views, cross-fades, and the pointer/key policy that chooses a command.

use ui::time::{Duration, Instant};

use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{BlendMode, Color, Fill};
use winit::keyboard::{Key, NamedKey};

use vello::kurbo::Point;

use ui::TextRole;
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{ProgressBar, Widget};

use ui::i18n::ArgValue;

use crate::assets::ImageStore;
use crate::model::{Deck, Palette, PaletteKind, SLIDE_H, SLIDE_W};
use crate::render;
use crate::render::DeckAssets;
use crate::strings::{tr, tr_args};

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
    /// How long a slide change cross-fades: the theme's base motion.
    fade: Duration,
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
            fade: Theme::dark().tokens.motion.durations.base(),
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

    /// Follow the theme's motion: a slide change fades over its base
    /// duration.
    pub(crate) fn set_fade(&mut self, fade: Duration) {
        self.fade = fade;
    }

    pub(crate) fn start_transition(&mut self, from: usize) {
        self.transition = Some((from, ui::time::now()));
        self.blank = Blank::None;
    }

    pub(crate) fn expire_transition(&mut self, reduce_motion: bool) {
        if reduce_motion
            || self
                .transition
                .is_some_and(|(_, at)| at.elapsed() >= self.fade)
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
                let fade = self.fade.as_secs_f32().max(f32::EPSILON);
                let p = (at.elapsed().as_secs_f32() / fade).min(1.0);
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
    pub(crate) fn draw_blank(
        &self,
        scene: &mut Scene,
        transform: Affine,
        rect: Rect,
        theme: &Theme,
    ) -> bool {
        let background = match self.blank {
            Blank::None => return false,
            // A white-out lights the room: the projector shows pure white.
            Blank::White => Color::WHITE,
            Blank::Black => theme.tokens.colors.media.letterbox,
        };
        ui::paint::fill_rect(scene, transform, rect, background);
        true
    }

    /// Where the slide sits on a stage of `area`: as large as it fits with
    /// the content gutter round it, centred.
    fn stage_dest(area: Rect, theme: &Theme) -> Rect {
        let gutter = f64::from(theme.tokens.spacing.s6);
        let available_width = (area.width() - 2.0 * gutter).max(1.0);
        let available_height = (area.height() - 2.0 * gutter).max(1.0);
        let scale = (available_width / SLIDE_W).min(available_height / SLIDE_H);
        let width = SLIDE_W * scale;
        let height = SLIDE_H * scale;
        let x = area.x0 + (area.width() - width) / 2.0;
        let y = area.y0 + (area.height() - height) / 2.0;
        Rect::new(x, y, x + width, y + height)
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

        // The slide is media on a letterbox, the same in every theme.
        ui::paint::fill_rect(
            scene,
            transform,
            frame.rect,
            theme.tokens.colors.media.letterbox,
        );
        if self.draw_blank(scene, transform, frame.rect, theme) {
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
        let destination = Self::stage_dest(area, theme);
        self.draw_stage_slide(
            &mut PaintCtx::new(scene, transform, text, theme),
            frame,
            &palette,
            destination,
            false,
        );

        if let Some(panel) = notes_panel {
            self.draw_notes_panel(scene, transform, text, theme, frame, &palette, panel);
        }
    }

    /// The current slide (cross-fading from the previous one) at
    /// `destination`, from the settled cache once the fade is over.
    fn draw_stage_slide(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        frame: &PresentationFrame<'_>,
        palette: &Palette,
        destination: Rect,
        audience: bool,
    ) {
        let (scene, transform, text, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let alpha = self.transition_alpha(theme.reduce_motion);
        if let Some((from, _)) = self.transition
            && alpha < 1.0
            && let Some(previous) = frame.deck.slides.get(from)
        {
            render::draw_slide(
                &mut PaintCtx::new(scene, transform, text, theme),
                DeckAssets {
                    palette,
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
                        palette,
                        images: frame.images,
                    },
                    slide,
                    destination,
                    0.0,
                );
                scene.pop_layer();
            }
        } else {
            let cache = if audience {
                &mut self.audience_cache
            } else {
                &mut self.cache
            };
            Self::append_settled_slide(
                cache,
                scene,
                transform,
                text,
                theme,
                frame,
                palette,
                destination,
            );
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
        ui::paint::fill_rect(
            scene,
            transform,
            frame.rect,
            theme.tokens.colors.media.letterbox,
        );
        if self.draw_blank(scene, transform, frame.rect, theme) {
            return;
        }
        let destination = Self::stage_dest(frame.rect, theme);
        self.draw_stage_slide(
            &mut PaintCtx::new(scene, transform, text, theme),
            frame,
            &palette,
            destination,
            true,
        );
    }

    /// The speaker notes over the bottom of the stage while presenting
    /// without the dashboard: chrome over media, on the floating material.
    fn draw_notes_panel(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        frame: &PresentationFrame<'_>,
        palette: &Palette,
        band: Rect,
    ) {
        let sp = &theme.tokens.spacing;
        let (s2, s4, s6) = (f64::from(sp.s2), f64::from(sp.s4), f64::from(sp.s6));
        let panel = Rect::new(band.x0 + s4, band.y0, band.x1 - s4, band.y1 - s4);
        ui::widget::floating_surface::paint(scene, transform, panel, theme);
        let local = theme.on_ground(ui::chrome::panel_ground(theme));
        let ink = &local.tokens.colors;
        let line = |role: TextRole| f64::from(local.text(role).line_height_px());
        let inner = panel.inset(-s6);
        let preview_width = inner.width() * 0.26;
        let preview_x = inner.x1 - preview_width;
        let mut ctx = PaintCtx::new(scene, transform, text, &local);

        // Leading column: the notes, with the elapsed time beside their label.
        let notes_x1 = preview_x - s6;
        ctx.text_role(
            &tr("slides-speaker-notes"),
            TextRole::Subheading,
            ink.text_primary,
            Point::new(inner.x0, inner.y0),
            None,
        );
        let clock = self.clock();
        let clock_style = local.text(TextRole::Title).numeric();
        let (clock_w, _) = ctx.text.measure_style(&clock, None, &clock_style);
        ctx.text(
            &clock,
            &clock_style,
            ink.text_primary,
            Point::new(notes_x1 - f64::from(clock_w), inner.y0),
            None,
        );
        let notes = frame.slide().map_or("", |slide| slide.notes.as_str());
        let (notes, notes_ink) = if notes.is_empty() {
            (tr("slides-no-notes"), ink.text_dim)
        } else {
            (notes.to_owned(), ink.text_primary)
        };
        ctx.text_role(
            &notes,
            TextRole::Reading,
            notes_ink,
            Point::new(inner.x0, inner.y0 + line(TextRole::Title) + s2),
            Some((notes_x1 - inner.x0).max(1.0) as f32),
        );

        // Trailing column: the next slide.
        if let Some(next) = frame.deck.slides.get(frame.current + 1) {
            ctx.text_role(
                &tr("slides-next"),
                TextRole::Subheading,
                ink.text_primary,
                Point::new(preview_x, inner.y0),
                None,
            );
            let top = inner.y0 + line(TextRole::Subheading) + s2;
            let preview = Rect::new(
                preview_x,
                top,
                inner.x1,
                top + preview_width * SLIDE_H / SLIDE_W,
            );
            let radius = f64::from(local.tokens.radii.control);
            render::draw_slide(
                &mut ctx,
                DeckAssets {
                    palette,
                    images: frame.images,
                },
                next,
                preview,
                radius,
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
        // The dashboard stands on the window ground the framework painted.
        let m = PresenterMetrics::of(theme);
        let body = Rect::new(
            frame.rect.x0 + m.s6,
            frame.rect.y0 + m.header_h(),
            frame.rect.x1 - m.s6,
            frame.rect.y1 - m.footer_h(),
        );
        let view = PresenterView { frame, palette, m };
        let ctx = &mut PaintCtx::new(scene, transform, text, theme);
        self.draw_presenter_header(ctx, &view);

        let left_width = (body.width() * 0.62).min(body.width() - 320.0);
        let current = Rect::new(body.x0, body.y0, body.x0 + left_width, body.y1);
        let right = Rect::new(current.x1 + m.s6, body.y0, body.x1, body.y1);
        draw_presenter_current(ctx, &view, current);
        let next = draw_presenter_next(ctx, &view, right);
        draw_presenter_notes(
            ctx,
            &view,
            Rect::new(right.x0, next.y1 + m.s6, right.x1, body.y1),
        );
        draw_presenter_footer(ctx, &view);
    }

    fn draw_presenter_header(&self, ctx: &mut PaintCtx<'_>, view: &PresenterView<'_>) {
        let PresenterView { frame, m, .. } = view;
        let colors = &ctx.theme.tokens.colors;
        let (primary, dim) = (colors.text_primary, colors.text_dim);
        let top = frame.rect.y0 + m.s4;
        let right = frame.rect.x1 - m.s6;
        // The elapsed time is the one hero number on the dashboard.
        let clock = self.clock();
        let clock_style = ctx.theme.text(TextRole::Display).numeric();
        let (clock_w, _) = ctx.text.measure_style(&clock, None, &clock_style);
        ctx.text(
            &clock,
            &clock_style,
            if self.is_paused() { dim } else { primary },
            Point::new(right - f64::from(clock_w), top),
            None,
        );
        let position = tr_args(
            if self.is_paused() {
                "slides-position-paused"
            } else {
                "slides-position"
            },
            &[
                ("current", ArgValue::from(frame.current + 1)),
                ("total", ArgValue::from(frame.deck.slides.len())),
            ],
        );
        let position_style = ctx.theme.text(TextRole::Caption).numeric();
        let (position_w, _) = ctx.text.measure_style(&position, None, &position_style);
        ctx.text(
            &position,
            &position_style,
            dim,
            Point::new(right - f64::from(position_w), top + m.lh_display),
            None,
        );
        // The deck's title yields to the wider of the two right-hand lines.
        let right_w = f64::from(clock_w).max(f64::from(position_w));
        let title_w = (right - right_w - m.s6 - (frame.rect.x0 + m.s6)).max(0.0);
        let style = ctx.theme.text(TextRole::Title);
        let title = ctx
            .text
            .elide_to_width_style(&frame.deck.title, title_w as f32, &style);
        ctx.text(
            &title,
            &style,
            primary,
            Point::new(frame.rect.x0 + m.s6, top),
            None,
        );
    }
}

/// One presenter-dashboard frame: the deck view, its resolved palette and
/// the measures every panel lays out against.
struct PresenterView<'a> {
    frame: &'a PresentationFrame<'a>,
    palette: &'a Palette,
    m: PresenterMetrics,
}

/// The presenter dashboard's measures, from the theme: its spacing and the
/// line boxes of the roles it sets.
#[derive(Clone, Copy)]
struct PresenterMetrics {
    lh_display: f64,
    lh_caption: f64,
    lh_subheading: f64,
    s1: f64,
    s2: f64,
    s3: f64,
    s4: f64,
    s6: f64,
}

impl PresenterMetrics {
    fn of(theme: &Theme) -> Self {
        let sp = &theme.tokens.spacing;
        let line = |role: TextRole| f64::from(theme.text(role).line_height_px());
        Self {
            lh_display: line(TextRole::Display),
            lh_caption: line(TextRole::Caption),
            lh_subheading: line(TextRole::Subheading),
            s1: f64::from(sp.s1),
            s2: f64::from(sp.s2),
            s3: f64::from(sp.s3),
            s4: f64::from(sp.s4),
            s6: f64::from(sp.s6),
        }
    }

    /// Advance from a section label's top to the content under it.
    fn label_adv(&self) -> f64 {
        self.lh_subheading + self.s2
    }

    /// Header band: the clock over the slide position.
    fn header_h(&self) -> f64 {
        self.s4 + self.lh_display + self.lh_caption + self.s6
    }

    /// Footer band holding the key hints and the deck's progress.
    fn footer_h(&self) -> f64 {
        self.lh_caption + 2.0 * self.s4
    }
}

fn draw_presenter_current(ctx: &mut PaintCtx<'_>, view: &PresenterView<'_>, area: Rect) {
    let PresenterView {
        frame, palette, m, ..
    } = view;
    ctx.text_role(
        &tr("slides-current"),
        TextRole::Subheading,
        ctx.theme.tokens.colors.text_primary,
        Point::new(area.x0, area.y0),
        None,
    );
    let destination = fit_slide_top(Rect::new(
        area.x0,
        area.y0 + m.label_adv(),
        area.x1,
        area.y1,
    ));
    let radius = f64::from(ctx.theme.tokens.radii.control);
    if let Some(slide) = frame.slide() {
        render::draw_slide(
            ctx,
            DeckAssets {
                palette,
                images: frame.images,
            },
            slide,
            destination,
            radius,
        );
    }
}

fn draw_presenter_next(ctx: &mut PaintCtx<'_>, view: &PresenterView<'_>, area: Rect) -> Rect {
    let PresenterView {
        frame, palette, m, ..
    } = view;
    let theme = ctx.theme;
    ctx.text_role(
        &tr("slides-next"),
        TextRole::Subheading,
        theme.tokens.colors.text_primary,
        Point::new(area.x0, area.y0),
        None,
    );
    let top = area.y0 + m.label_adv();
    let destination = Rect::new(
        area.x0,
        top,
        area.x1,
        top + area.width() * SLIDE_H / SLIDE_W,
    );
    let radius = f64::from(theme.tokens.radii.control);
    if let Some(next) = frame.deck.slides.get(frame.current + 1) {
        render::draw_slide(
            ctx,
            DeckAssets {
                palette,
                images: frame.images,
            },
            next,
            destination,
            radius,
        );
    } else {
        // After the last slide: an empty well saying so.
        ui::surface::card(
            ctx.scene,
            ctx.transform,
            destination,
            radius,
            theme.tokens.colors.surfaces.raised,
        );
        let label = tr("slides-end-of-deck");
        let size = ctx.measure_role(&label, TextRole::Body, None);
        ctx.text_role_in_band(
            &label,
            TextRole::Body,
            theme.tokens.colors.text_dim,
            destination.center().x - size.width / 2.0,
            destination,
            None,
        );
    }
    destination
}

/// The speaker-notes label and well filling `column`.
fn draw_presenter_notes(ctx: &mut PaintCtx<'_>, view: &PresenterView<'_>, column: Rect) {
    let PresenterView { frame, m, .. } = view;
    let theme = ctx.theme;
    let colors = &theme.tokens.colors;
    ctx.text_role(
        &tr("slides-speaker-notes"),
        TextRole::Subheading,
        colors.text_primary,
        Point::new(column.x0, column.y0),
        None,
    );
    let well = Rect::new(
        column.x0,
        column.y0 + m.label_adv(),
        column.x1,
        column.y1.max(column.y0 + m.label_adv()),
    );
    ui::surface::card(
        ctx.scene,
        ctx.transform,
        well,
        f64::from(theme.tokens.radii.control),
        colors.surfaces.raised,
    );
    let notes = frame.slide().map_or("", |slide| slide.notes.as_str());
    let (notes, ink) = if notes.is_empty() {
        (tr("slides-no-notes"), colors.text_dim)
    } else {
        (notes.to_owned(), colors.text_primary)
    };
    ctx.text_role(
        &notes,
        TextRole::Reading,
        ink,
        Point::new(well.x0 + m.s4, well.y0 + m.s3),
        Some((well.width() - 2.0 * m.s4).max(1.0) as f32),
    );
}

fn draw_presenter_footer(ctx: &mut PaintCtx<'_>, view: &PresenterView<'_>) {
    let PresenterView { frame, m, .. } = view;
    let theme = ctx.theme;
    let band = Rect::new(
        frame.rect.x0 + m.s6,
        frame.rect.y1 - m.footer_h(),
        frame.rect.x1 - m.s6,
        frame.rect.y1,
    );
    // The deck's progress along the bottom edge.
    let fraction = (frame.current + 1) as f32 / frame.deck.slides.len().max(1) as f32;
    let mut progress = ProgressBar::new(fraction);
    progress.apply_layout_external(Rect::new(
        frame.rect.x0,
        frame.rect.y1 - m.s1,
        frame.rect.x1,
        frame.rect.y1,
    ));
    progress.draw(
        ctx.scene,
        ctx.transform,
        ctx.text,
        theme,
        &ui::presence::PresenceCtx::empty(),
    );
    // The keys the dashboard answers to, elided to the window.
    let style = theme.text(TextRole::Caption);
    let hints = ctx.text.elide_to_width_style(
        &tr("slides-presenter-keys"),
        band.width().max(0.0) as f32,
        &style,
    );
    ctx.text_role_in_band(
        &hints,
        TextRole::Caption,
        theme.tokens.colors.text_dim,
        band.x0,
        Rect::new(band.x0, band.y0, band.x1, band.y1 - m.s1),
        None,
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
