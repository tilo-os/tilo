//! The desktop's control hints on the native desktop: the same
//! [`ControlHints`] the page runs, fed what this frame shows (the shown
//! strip's row, the bar's own avatar, the search chord as configured) and
//! drawn above the windows. What the person learned is kept with their
//! personal settings, which roam to their other devices.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;
use settings_store::control_hints::LEARNED;
use settings_store::hardware::ShortcutAction;
use tilo_desktop::control_hints::{ControlHints, HintFrame};
use ui::control_hints::Quiet;

impl ShellOverlayHost {
    /// What this frame shows the hints.
    fn control_hint_frame(&self, now: Instant) -> HintFrame {
        // The strip's hints (move a window past another, resize one, scroll
        // sideways to the windows out of view) are about the strip's row:
        // a workspace showing its canvas has none, whatever its strips would
        // lay out behind it.
        let row = self
            .active_strip_id()
            .filter(|_| !self.canvas_shown())
            .and_then(|id| self.group_row_ptrs.get(&id).copied())
            // SAFETY: see field doc on `group_row_ptrs`.
            .map(|ptr| unsafe { &*ptr }.hint_places())
            .unwrap_or_default();
        let area = if row.viewport.is_zero_area() {
            let display = self.overlay_area();
            Rect::new(
                display.x0,
                display.y0 + f64::from(tilo_desktop::topbar::TOPBAR_HEIGHT_PX),
                display.x1,
                display.y1,
            )
        } else {
            row.viewport
        };
        let search_keys = self
            .shortcut_config()
            .bindings
            .get(&ShortcutAction::Search)
            .map(|chord| {
                ui::keycaps::active()
                    .chord_keys(chord.modifiers.iter().map(String::as_str), &chord.key)
            })
            .unwrap_or_default();
        let avatar = self.topbar_shown().own_avatar;
        let colour = self
            .chrome
            .as_ref()
            .map(|chrome| crate::shell::topbar_chrome::own_rgb(&chrome.borrow()));
        HintFrame {
            row,
            area,
            search_keys,
            avatar: (!avatar.is_zero_area()).then_some(avatar),
            colour,
            quiet: self.control_hints_quiet(now),
            // Setup owns the screen until the person reaches the desktop.
            presented: !self.onboarding_active(),
            touch: self.interaction.pointer_precision == ui::PointerPrecision::Coarse,
        }
    }

    /// Why no hint may begin now. In a call or following someone, one that
    /// shows goes too.
    fn control_hints_quiet(&self, now: Instant) -> Option<Quiet> {
        if self.own_voice_call().is_some() {
            return Some(Quiet::Call);
        }
        if self.followed_peer.is_some() {
            return Some(Quiet::Following);
        }
        if self
            .chrome
            .as_ref()
            .is_some_and(|chrome| chrome.borrow().typing_at(now))
        {
            return Some(Quiet::Typing);
        }
        let flags = self.overlay_flags();
        let busy = self.palette_visible
            || flags.overview
            || flags.quick_settings
            || flags.ws_settings
            || flags.collaborators
            || flags.shortcut_help
            || flags.desktop_tour
            || flags.capture
            || flags.projection
            || self.topbar_shown().modal
            || self.onboarding_active()
            || self.modal_sheet_owns_keyboard()
            || self
                .chrome
                .as_ref()
                .is_some_and(|chrome| chrome.borrow().is_fullscreen());
        busy.then_some(Quiet::Busy)
    }

    /// Fold in the person's record as `document`, the session's settings,
    /// now holds it: what their other devices learned, or the switch turned
    /// in Settings. The compositor calls this when the record's section
    /// changed, so painting never reads the settings.
    pub fn follow_control_hints(&mut self, document: &serde_json::Map<String, serde_json::Value>) {
        self.control_hints
            .merge(&LEARNED.from_document(document), Instant::now());
    }

    /// Advance the hints to `now`, and keep what the person learned.
    pub(super) fn prepare_control_hints(&mut self, now: Instant) {
        let frame = self.control_hint_frame(now);
        self.control_hints.update(&frame, now);
        self.control_hints_frame = frame;
        self.keep_control_hints();
    }

    /// Turn the hints on or off, as the profile card's switch asks.
    pub(super) fn set_control_hints_enabled(&mut self, on: bool) {
        self.control_hints.set_enabled(on, Instant::now());
        self.keep_control_hints();
    }

    /// Keep every hint away, the person's record untouched: for tests that
    /// count what something other than the hints costs. Only a change of the
    /// record in the settings turns them back on.
    #[cfg(test)]
    pub(crate) fn rest_control_hints(&mut self) {
        self.control_hints.set_enabled(false, Instant::now());
        let _ = self.control_hints.take_changed();
    }

    pub(crate) fn control_hints_enabled(&self) -> bool {
        self.control_hints.enabled()
    }

    fn keep_control_hints(&mut self) {
        if let Some(learned) = self.control_hints.take_changed()
            && let Err(error) = LEARNED.set(learned)
        {
            tracing::warn!(%error, "could not keep the control hints learned");
        }
    }

    pub(super) fn draw_control_hints(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::theme::Theme,
    ) {
        self.control_hints.draw(
            &self.control_hints_frame,
            scene,
            transform,
            text_renderer,
            theme,
            Instant::now(),
        );
    }

    pub(super) fn control_hints_animating(&self, now: Instant) -> bool {
        self.control_hints.needs_animation_frame(now)
    }

    pub(super) fn control_hints_wake_deadline(&self, now: Instant) -> Option<Instant> {
        self.control_hints.next_wake_deadline(now)
    }

    /// The hint on screen, for assistive technology.
    pub(super) fn control_hint_accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.control_hints.accessibility()
    }
}

/// The hints a native desktop starts with: the person's record as kept.
pub(super) fn load() -> ControlHints {
    ControlHints::new(LEARNED.get())
}

#[cfg(test)]
mod tests {
    use crate::shell::state::WorkspaceTree;
    use crate::shell::test_support::PopulatedShellFixture;
    use tilo_desktop_model::{Arrangement, ArrangementKind, CanvasPlace};

    /// What the hints are shown of the desktop `tree` lays out on a narrow
    /// screen, once it has drawn.
    fn hint_row(tree: WorkspaceTree) -> (ui::widget::HintPlaces, bool) {
        let mut fixture =
            PopulatedShellFixture::with_tree((640, 480), ui::theme::Theme::dark(), tree);
        // Frames as the compositor's loop makes them: the pump takes the
        // workspace's arrangement (its canvas) before the layout.
        let mut doc = automerge::AutoCommit::new();
        for _ in 0..3 {
            let _ = fixture.frame_of(&mut doc);
        }
        let row = fixture.host().control_hint_frame(ui::time::now()).row;
        let shown = fixture.host().canvas_shown();
        drop(fixture);
        (row, shown)
    }

    /// The demo's windows, every one its own group, on a strip or on the
    /// canvas.
    fn demo(canvas: bool) -> WorkspaceTree {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let workspace = &mut tree.desktop.workspaces[0];
        let strip = &mut workspace.strips[0];
        let first = strip.window_groups.remove(0);
        let mut groups = Vec::new();
        for (id, tab) in (930_001_u64..).zip(first.tabs) {
            groups.push(crate::shell::state::WindowGroup {
                id,
                active_tab: tab.id,
                tabs: vec![tab],
                width: crate::shell::state::WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            });
        }
        groups.append(&mut strip.window_groups);
        if canvas {
            workspace.arrangement = Arrangement {
                kind: ArrangementKind::Canvas,
                places: (0_i32..)
                    .zip(&groups)
                    .map(|(index, group)| {
                        let x = index.saturating_mul(220).saturating_add(20);
                        (group.id, CanvasPlace::new(x, 20, 200, 160))
                    })
                    .collect(),
            };
        }
        strip.window_groups = groups;
        tree
    }

    /// The strip's hints (scroll sideways to the windows out of view, move a
    /// window past another, resize one) are not offered over the canvas,
    /// where windows stand where they are placed and no strip row shows.
    #[test]
    fn the_canvas_offers_none_of_the_strips_hints() {
        let (strip, shown) = hint_row(demo(false));
        assert!(!shown, "the strips show");
        assert!(
            strip.windows >= 2 && (strip.beyond.0 || strip.beyond.1),
            "the strip's windows run past a narrow screen: {strip:?}"
        );
        let (canvas, shown) = hint_row(demo(true));
        assert!(shown, "the canvas shows");
        assert_eq!(canvas.windows, 0, "{canvas:?}");
        assert_eq!(canvas.beyond, (false, false), "{canvas:?}");
    }
}
