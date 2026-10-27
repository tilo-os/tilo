//! A window's content held still while a screenshot is chosen.
//!
//! When the screenshot tool opens, the shell holds every window still: a
//! client window's buffers in the compositor's frame store, an app the shell
//! runs itself here. [`TileHost::hold_content`] records what the window
//! draws at that moment (and, for an app that can paint its whole document,
//! that too, for a capture of the complete window); until
//! [`release_windows`], the window draws the recording, moved to
//! wherever the window has moved since. The app keeps running underneath:
//! it handles input, updates and lays out as usual, and what it draws
//! meanwhile shows once the hold is released.
//!
//! The recording is a scene, a list of drawing commands that share the
//! app's glyphs and images; nothing is rasterised to hold it.

use std::cell::Cell;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};

use super::{TileContent, TileHost};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

thread_local! {
    /// The hold in place on this thread's windows, numbered so a window
    /// can tell a recording of an earlier hold from the current one.
    static HOLD: Cell<(u64, bool)> = const { Cell::new((0, false)) };
}

/// Begin holding windows still: each window then held with
/// [`TileHost::hold_content`] draws its recording until
/// [`release_windows`]. Returns whether a hold began.
pub fn hold_windows() -> bool {
    HOLD.with(|hold| {
        let (number, held) = hold.get();
        hold.set((number.wrapping_add(u64::from(!held)), true));
        !held
    })
}

/// End the hold: every window draws live again, and drops its recording
/// the next time it draws.
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

/// What a window drew when it was held, in desktop coordinates of the
/// moment it was held.
pub(super) struct HeldContent {
    /// The hold it was recorded for.
    hold: u64,
    /// The window's top-left corner then.
    origin: Point,
    /// The window as it was on screen.
    window: Scene,
    /// The complete window an app can paint whole, and where it lay.
    complete: Option<(Scene, Rect)>,
}

impl TileHost {
    /// Hold this window still as it is now, under the hold
    /// [`hold_windows`] began: from here on it draws what it draws now,
    /// wherever it moves, until [`release_windows`]. Without a hold, or
    /// already held under this one, nothing changes.
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

    /// Whether this window is held still under the hold in place.
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

    /// The window was drawn: a recording whose hold has ended goes now,
    /// once the frame that replaced it with the live window (and reported
    /// the window damaged, [`Self::held_redraw`]) is done.
    pub(super) fn drop_ended_hold(&mut self) {
        if !self.held_now() {
            self.held = None;
        }
    }

    /// How far the window has moved since it was held.
    fn held_offset(&self, held: &HeldContent) -> Vec2 {
        self.rect.origin() - held.origin
    }

    /// The window: the held recording while held, else live.
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

    /// Where the held complete window lies now.
    pub(super) fn held_complete_rect(&self, held: &HeldContent) -> Option<Rect> {
        let (_, rect) = held.complete.as_ref()?;
        Some(*rect + self.held_offset(held))
    }

    /// Draw the held complete window; `None` when it had none.
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
