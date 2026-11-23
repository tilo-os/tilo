//! A window's content held still while a screenshot is chosen.
use std::cell::Cell;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};

use super::{TileContent, TileHost};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

thread_local! {
    static HOLD: Cell<(u64, bool)> = const { Cell::new((0, false)) };
}

pub fn hold_windows() -> bool {
    HOLD.with(|hold| {
        let (number, held) = hold.get();
        hold.set((number.wrapping_add(u64::from(!held)), true));
        !held
    })
}

pub fn release_windows() -> bool {
    HOLD.with(|hold| {
        let (number, held) = hold.get();
        hold.set((number, false));
        held
    })
}

fn current_hold() -> Option<u64> {
    HOLD.with(|hold| {
        let (number, held) = hold.get();
        held.then_some(number)
    })
}

pub(super) struct HeldContent {
    hold: u64,
    origin: Point,
    window: Scene,
    complete: Option<(Scene, Rect)>,
}

impl TileHost {
    pub fn hold_content(&mut self, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx) {
        let Some(hold) = current_hold() else {
            return;
        };
        if self.held_now() {
            return;
        }
        self.held = None;
        let mut window = Scene::new();
        self.draw_steady_state(&mut window, Affine::IDENTITY, tr, theme, presence);
        let complete = if matches!(self.content, TileContent::App(_)) {
            let mut scene = Scene::new();
            self.draw_complete_content_capture(&mut scene, Affine::IDENTITY, tr, theme, presence)
                .map(|rect| (scene, rect))
        } else {
            None
        };
        self.held = Some(HeldContent {
            hold,
            origin: self.rect.origin(),
            window,
            complete,
        });
    }

    pub fn content_held(&self) -> bool {
        self.held_now()
    }

    fn held_now(&self) -> bool {
        self.held
            .as_ref()
            .is_some_and(|held| current_hold() == Some(held.hold))
    }

    /// Whether the window must be drawn again, while it has a recording:
    /// never while held (what the app draws meanwhile is not shown), and
    /// as soon as the hold has ended, so the live window replaces the
    /// recording even if the app has gone quiet. `None` without one.
    pub(super) fn held_redraw(&self) -> Option<bool> {
        self.held.as_ref().map(|_| !self.held_now())
    }

    /// The recording to draw instead of the live window, while its hold
    /// lasts.
    fn current_held(&self) -> Option<&HeldContent> {
        self.held.as_ref().filter(|_| self.held_now())
    }

    pub(super) fn drop_ended_hold(&mut self) {
        if !self.held_now() {
            self.held = None;
        }
    }

    fn held_offset(&self, held: &HeldContent) -> Vec2 {
        self.rect.origin() - held.origin
    }

    pub(super) fn draw_window(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        match self.current_held() {
            Some(held) => {
                let moved = Affine::translate(self.held_offset(held));
                scene.append(&held.window, Some(transform * moved));
            }
            None => self.draw_steady_state(scene, transform, tr, theme, presence),
        }
    }

    pub(super) fn held_complete_rect(&self, held: &HeldContent) -> Option<Rect> {
        let (_, rect) = held.complete.as_ref()?;
        Some(*rect + self.held_offset(held))
    }

    pub(super) fn draw_held_complete_content(
        &self,
        scene: &mut Scene,
        transform: Affine,
    ) -> Option<Rect> {
        let held = self.current_held()?;
        let (complete, _) = held.complete.as_ref()?;
        let moved = Affine::translate(self.held_offset(held));
        scene.append(complete, Some(transform * moved));
        self.held_complete_rect(held)
    }
}
