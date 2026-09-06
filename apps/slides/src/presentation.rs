//! Presentation-mode state, input routing, and presentation-only chrome.
//!
//! The editor owns the deck and applies navigation commands. This component owns
//! every transient concern of a running talk: timer, blanking, presenter/notes
//! views, cross-fades, and the pointer/key policy that chooses a command.

use std::time::{Duration, Instant};

use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke, Vec2};
use vello::peniko::{BlendMode, Color, Fill};
use winit::keyboard::{Key, NamedKey};

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::assets::ImageStore;
use crate::model::{Deck, Palette, PaletteKind, SLIDE_H, SLIDE_W};
use crate::{chrome, render};

const TRANSITION_MS: u128 = 240;

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
    show_notes: bool,
    /// Shaped settled audience slide. Presentation frames keep painting live
    /// chrome (timer, notes, progress), but the authored slide only needs to be
    /// reshaped when its cache key changes.
    cache: Option<PresentCache>,
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
            show_notes: false,
            cache: None,
        }
    }
}

impl Presentation {
    pub(crate) fn start(&mut self) {
        self.blank = Blank::None;
        self.transition = None;
        self.started = Some(Instant::now());
        self.paused_elapsed = None;
        self.paused_total = Duration::ZERO;
    }

    pub(crate) fn leave(&mut self) {
        self.blank = Blank::None;
        self.presenter_view = false;
        self.transition = None;
    }

    /// Drop the shaped audience slide after an image decode changes pixels
    /// without changing the deck revision/cache key.
    pub(crate) fn invalidate_cache(&mut self) {
        self.cache = None;
    }

    pub(crate) fn start_transition(&mut self, from: usize) {
        self.transition = Some((from, Instant::now()));
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
        } else if self.presenter_view {
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
        self.started = Some(Instant::now());
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
        if self.presenter_view {
            self.draw_presenter(scene, transform, text, theme, frame, &palette);
            return;
        }

        chrome::fill_rect(scene, transform, frame.rect, Color::BLACK);
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
                scene,
                transform,
                text,
                theme,
                &palette,
                previous,
                destination,
                0.0,
                frame.images,
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
                    scene,
                    transform,
                    text,
                    theme,
                    &palette,
                    slide,
                    destination,
                    0.0,
                    frame.images,
                );
                scene.pop_layer();
            }
        } else {
            self.append_settled_slide(scene, transform, text, theme, frame, &palette, destination);
        }

        if let Some(panel) = notes_panel {
            self.draw_notes_panel(scene, transform, text, theme, frame, &palette, panel);
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "render target, shared text/theme state, presentation frame, palette and destination are independent inputs"
    )]
    fn append_settled_slide(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        frame: &PresentationFrame<'_>,
        palette: &Palette,
        destination: Rect,
    ) -> bool {
        let stale = self.cache.as_ref().is_none_or(|cache| {
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
                    &mut fragment,
                    Affine::IDENTITY,
                    text,
                    theme,
                    palette,
                    slide,
                    destination,
                    0.0,
                    frame.images,
                );
            }
            let (_, _, _, dest) = PresentCache::key(
                destination,
                frame.current,
                frame.revision,
                frame.deck.palette,
            );
            self.cache = Some(PresentCache {
                slide: frame.current,
                revision: frame.revision,
                palette: frame.deck.palette,
                dest,
                scene: fragment,
            });
        }
        if let Some(cache) = &self.cache {
            scene.append(&cache.scene, Some(transform));
        }
        stale
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "render target, shared text/theme state, presentation frame, palette and panel geometry are independent inputs"
    )]
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
        chrome::fill_rect(scene, transform, panel, theme.tokens.colors.surfaces.raised);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &Line::new(
                Point::new(panel.x0, panel.y0),
                Point::new(panel.x1, panel.y0),
            ),
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
                scene,
                transform,
                text,
                theme,
                palette,
                next,
                preview,
                6.0,
                frame.images,
            );
            chrome::stroke_rrect(
                scene,
                transform,
                preview,
                6.0,
                render::with_alpha(Color::WHITE, 40),
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
        let colors = &theme.tokens.colors;
        chrome::fill_rect(scene, transform, frame.rect, colors.surfaces.base);
        let outer = 28.0;
        let body = Rect::new(
            frame.rect.x0 + outer,
            frame.rect.y0 + 60.0,
            frame.rect.x1 - outer,
            frame.rect.y1 - 40.0,
        );
        self.draw_presenter_header(scene, transform, text, frame, colors, palette, outer);

        let left_width = (body.width() * 0.62).min(body.width() - 320.0);
        let current = Rect::new(body.x0, body.y0, body.x0 + left_width, body.y1);
        let right = Rect::new(current.x1 + 24.0, body.y0, body.x1, body.y1);
        draw_presenter_current(
            scene, transform, text, theme, frame, colors, palette, current,
        );
        let next =
            draw_presenter_next(scene, transform, text, theme, frame, colors, palette, right);
        draw_presenter_notes(scene, transform, text, frame, colors, right, body, next);
        draw_presenter_footer(scene, transform, text, frame, colors, palette, outer);
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "render target, presentation frame, resolved colors and header geometry are independent inputs"
    )]
    fn draw_presenter_header(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        frame: &PresentationFrame<'_>,
        colors: &ui::tokens::Colors,
        palette: &Palette,
        outer: f64,
    ) {
        text.draw_text_weighted(
            scene,
            &frame.deck.title,
            18.0,
            colors.text_primary,
            transform * Affine::translate((frame.rect.x0 + outer, frame.rect.y0 + 18.0)),
            None,
            TextWeight::Bold,
        );
        text.draw_text_weighted(
            scene,
            "PRESENTER VIEW",
            11.0,
            palette.accent,
            transform * Affine::translate((frame.rect.x0 + outer, frame.rect.y0 + 40.0)),
            None,
            TextWeight::Semibold,
        );
        let clock = self.clock();
        let (clock_width, _) = text.measure_weighted(&clock, 34.0, None, TextWeight::Bold);
        text.draw_text_weighted(
            scene,
            &clock,
            34.0,
            if self.is_paused() {
                colors.text_dim
            } else {
                colors.text_primary
            },
            transform
                * Affine::translate((
                    frame.rect.x1 - outer - f64::from(clock_width),
                    frame.rect.y0 + 12.0,
                )),
            None,
            TextWeight::Bold,
        );
        let position = format!(
            "{}  ·  slide {} / {}",
            if self.is_paused() {
                "PAUSED"
            } else {
                "ELAPSED"
            },
            frame.current + 1,
            frame.deck.slides.len()
        );
        let (position_width, _) =
            text.measure_weighted(&position, 12.0, None, TextWeight::Semibold);
        text.draw_text_weighted(
            scene,
            &position,
            12.0,
            colors.text_dim,
            transform
                * Affine::translate((
                    frame.rect.x1 - outer - f64::from(position_width),
                    frame.rect.y0 + 44.0,
                )),
            None,
            TextWeight::Semibold,
        );
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

#[allow(clippy::too_many_arguments)]
fn draw_presenter_current(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    frame: &PresentationFrame<'_>,
    colors: &ui::tokens::Colors,
    palette: &Palette,
    area: Rect,
) {
    text.draw_text_weighted(
        scene,
        "CURRENT",
        11.0,
        colors.text_dim,
        transform * Affine::translate((area.x0, area.y0)),
        None,
        TextWeight::Semibold,
    );
    let destination = fit_slide_top(Rect::new(area.x0, area.y0 + 20.0, area.x1, area.y1));
    chrome::fill_rrect(
        scene,
        transform,
        (destination + Vec2::new(0.0, 6.0)).inflate(2.0, 2.0),
        12.0,
        render::with_alpha(Color::BLACK, 80),
    );
    if let Some(slide) = frame.slide() {
        render::draw_slide(
            scene,
            transform,
            text,
            theme,
            palette,
            slide,
            destination,
            12.0,
            frame.images,
        );
    }
    chrome::stroke_rrect(
        scene,
        transform,
        destination,
        12.0,
        render::with_alpha(colors.text_primary, 30),
        1.0,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_presenter_next(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    frame: &PresentationFrame<'_>,
    colors: &ui::tokens::Colors,
    palette: &Palette,
    area: Rect,
) -> Rect {
    text.draw_text_weighted(
        scene,
        "NEXT",
        11.0,
        colors.text_dim,
        transform * Affine::translate((area.x0, area.y0)),
        None,
        TextWeight::Semibold,
    );
    let top = area.y0 + 20.0;
    let destination = Rect::new(
        area.x0,
        top,
        area.x1,
        top + area.width() * SLIDE_H / SLIDE_W,
    );
    if let Some(next) = frame.deck.slides.get(frame.current + 1) {
        render::draw_slide(
            scene,
            transform,
            text,
            theme,
            palette,
            next,
            destination,
            8.0,
            frame.images,
        );
        chrome::stroke_rrect(
            scene,
            transform,
            destination,
            8.0,
            render::with_alpha(colors.text_primary, 30),
            1.0,
        );
    } else {
        chrome::fill_rrect(scene, transform, destination, 8.0, colors.surfaces.raised);
        chrome::stroke_rrect(
            scene,
            transform,
            destination,
            8.0,
            render::with_alpha(colors.text_primary, 24),
            1.0,
        );
        chrome::text_centered(
            scene,
            transform,
            text,
            "End of deck",
            destination,
            14.0,
            colors.text_dim,
            TextWeight::Semibold,
        );
    }
    destination
}

#[allow(clippy::too_many_arguments)]
fn draw_presenter_notes(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    frame: &PresentationFrame<'_>,
    colors: &ui::tokens::Colors,
    right: Rect,
    body: Rect,
    next: Rect,
) {
    let top = next.y1 + 22.0;
    text.draw_text_weighted(
        scene,
        "SPEAKER NOTES",
        11.0,
        colors.text_dim,
        transform * Affine::translate((right.x0, top)),
        None,
        TextWeight::Semibold,
    );
    let panel = Rect::new(right.x0, top + 20.0, right.x1, body.y1);
    chrome::fill_rrect(scene, transform, panel, 10.0, colors.surfaces.raised);
    chrome::stroke_rrect(
        scene,
        transform,
        panel,
        10.0,
        render::with_alpha(colors.text_primary, 22),
        1.0,
    );
    let notes = frame.slide().map_or("", |slide| slide.notes.as_str());
    text.draw_text_weighted(
        scene,
        if notes.is_empty() {
            "No notes for this slide."
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

fn draw_presenter_footer(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    frame: &PresentationFrame<'_>,
    colors: &ui::tokens::Colors,
    palette: &Palette,
    outer: f64,
) {
    text.draw_text_weighted(
        scene,
        "← →  navigate    Home / End  first · last    P  hide dashboard    K  pause timer    0  reset    Esc  exit",
        12.0,
        colors.text_dim,
        transform * Affine::translate((frame.rect.x0 + outer, frame.rect.y1 - 26.0)),
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

    #[test]
    fn blank_navigation_resumes_before_advancing() {
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

    #[test]
    fn presenter_click_never_advances() {
        let mut p = Presentation::default();
        p.set_presenter_view(true);
        assert_eq!(p.pointer_command(), Command::None);
    }

    #[test]
    fn paused_timer_is_stable_and_reset_resumes() {
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

    #[test]
    fn settled_slide_cache_reuses_an_unchanged_render() {
        let deck = crate::model::seed_deck();
        let images = ImageStore::new();
        let frame =
            PresentationFrame::new(Rect::new(0.0, 0.0, 1280.0, 720.0), &deck, 1, 7, &images);
        let theme = Theme::dark();
        let palette = Palette::resolve(deck.palette, &theme);
        let destination = Rect::new(100.0, 50.0, 1180.0, 660.0);
        let mut presentation = Presentation::default();
        let mut text = TextRenderer::new();

        assert!(presentation.append_settled_slide(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut text,
            &theme,
            &frame,
            &palette,
            destination,
        ));
        assert!(!presentation.append_settled_slide(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut text,
            &theme,
            &frame,
            &palette,
            destination,
        ));

        presentation.invalidate_cache();
        assert!(presentation.append_settled_slide(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut text,
            &theme,
            &frame,
            &palette,
            destination,
        ));
    }

    #[test]
    fn settled_slide_cache_key_covers_all_render_inputs() {
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
