//! Presentation-mode state, input routing, and presentation-only chrome.
//!
//! The editor owns the deck and applies navigation commands. This component owns
//! every transient concern of a running talk: timer, blanking, presenter/notes
//! views, cross-fades, and the pointer/key policy that chooses a command.

use std::time::{Duration, Instant};

use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::Color;
use winit::keyboard::{Key, NamedKey};

use ui::text_render::{TextRenderer, TextWeight};

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

    pub(crate) fn start_transition(&mut self, from: usize) {
        self.transition = Some((from, Instant::now()));
        self.blank = Blank::None;
    }

    pub(crate) fn transition(&self) -> Option<(usize, Instant)> {
        self.transition
    }

    pub(crate) fn expire_transition(&mut self) {
        if self
            .transition
            .is_some_and(|(_, at)| at.elapsed().as_millis() >= TRANSITION_MS)
        {
            self.transition = None;
        }
    }

    pub(crate) fn transition_alpha(&self) -> f32 {
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
        let seconds = self.elapsed().as_secs();
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }

    pub(crate) fn is_paused(&self) -> bool {
        self.paused_elapsed.is_some()
    }

    pub(crate) fn is_presenter_view(&self) -> bool {
        self.presenter_view
    }

    pub(crate) fn show_notes(&self) -> bool {
        self.show_notes
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
    pub(crate) fn draw_blank(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        rect: Rect,
    ) -> bool {
        let (background, foreground, message) = match self.blank {
            Blank::None => return false,
            Blank::White => (
                Color::WHITE,
                render::with_alpha(Color::BLACK, 120),
                "Screen whited out  ·  press W to resume",
            ),
            Blank::Black => (
                Color::BLACK,
                render::with_alpha(Color::WHITE, 120),
                "Screen blacked out  ·  press B to resume",
            ),
        };
        chrome::fill_rect(scene, transform, rect, background);
        text.draw_text_weighted(
            scene,
            message,
            14.0,
            foreground,
            transform * Affine::translate((rect.x0 + 40.0, rect.y1 - 40.0)),
            None,
            TextWeight::Regular,
        );
        true
    }

    pub(crate) fn draw_timer_pill(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        rect: Rect,
    ) {
        let paused = self.is_paused();
        let clock = if paused {
            format!("II  {}", self.clock())
        } else {
            self.clock()
        };
        let width = chrome::est_width(&clock, 13.0) + 26.0;
        let pill = Rect::new(
            rect.x1 - 20.0 - width,
            rect.y0 + 16.0,
            rect.x1 - 20.0,
            rect.y0 + 44.0,
        );
        chrome::fill_rrect(
            scene,
            transform,
            pill,
            14.0,
            render::with_alpha(Color::WHITE, if paused { 14 } else { 24 }),
        );
        chrome::text_centered(
            scene,
            transform,
            text,
            &clock,
            pill,
            13.0,
            render::with_alpha(Color::WHITE, if paused { 150 } else { 255 }),
            TextWeight::Semibold,
        );
    }
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
}
