//! The page's control hints: the desktop's own ([`ControlHints`]), fed what
//! the page's frame shows and drawn above its windows, as the native shell
//! does. What the person learned is kept with their settings.

use super::BrowserShell;
use settings_store::control_hints::LEARNED;
use tilo_desktop::control_hints::{ControlHints, HintFrame};
use ui::control_hints::Quiet;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::vello::Scene;
use ui::vello::kurbo::Affine;
use ui::widget::Widget;

/// The hints over the person's record as this browser last kept it.
pub(super) fn load() -> ControlHints {
    ControlHints::new(LEARNED.get())
}

impl BrowserShell {
    /// What the page's frame shows the hints now.
    fn hint_frame(&self) -> HintFrame {
        let area = self.desk.area().get_rect();
        // The chord the page's search answers to on this platform.
        let keys = tilo_desktop_surface::search_chord_keys(ui::platform::PlatformConventions::new(
            if self.offers.mac {
                ui::platform::Platform::MacOs
            } else {
                ui::platform::Platform::Linux
            },
        ));
        let avatar = self.top.bar().own_avatar_rect();
        HintFrame {
            row: self.handle.hint_places(),
            area,
            search_keys: keys,
            avatar: (!avatar.is_zero_area()).then_some(avatar),
            colour: Some(self.chrome.own_chip().rgb),
            quiet: self.hints_quiet(),
            presented: self.chrome.shared.presented.get(),
            touch: self.interaction.pointer_precision == ui::PointerPrecision::Coarse,
        }
    }

    /// Why no hint may begin now: following someone, or something holding
    /// the person's attention (search, a menu, the overview, the strip
    /// switcher, picking what to report).
    fn hints_quiet(&self) -> Option<Quiet> {
        if self.pinned.followed().is_some() {
            return Some(Quiet::Following);
        }
        let busy = self.report_composer_open()
            || self.handle.search_open()
            || self.picker.is_open()
            || self.people_open
            || self.profile.is_open()
            || self.top.wants_keyboard();
        busy.then_some(Quiet::Busy)
    }

    /// Whether a window of the report composer is open. It asks the person a
    /// question from the moment it opens ("What happened, and what did you
    /// expect?"), so a hint would stand over what they are being asked; on
    /// the native desktop it is a sheet, which keeps hints quiet already.
    fn report_composer_open(&self) -> bool {
        use tilo_desktop_model::TileContent;
        self.handle.read(|tree| {
            tree.workspaces.iter().any(|workspace| {
                workspace.strips.iter().any(|strip| {
                    strip.window_groups.iter().any(|group| {
                        group.tabs.iter().any(|tab| {
                            matches!(&tab.tile,
                                TileContent::LocalApp { app_id, .. }
                                    if app_id == tilo_desktop::feedback::PACKAGE)
                        })
                    })
                })
            })
        })
    }

    /// Advance the hints to now, and keep what the person learned.
    pub(super) fn update_hints(&mut self) {
        let now = ui::time::now();
        // The page's settings document moves its revision with every change
        // (Settings, or this page's own writes): the record is taken again
        // then, never on a schedule.
        let settings = settings_store::stamp();
        if settings != self.hints_settings {
            self.hints_settings = settings;
            self.hints.merge(&LEARNED.get(), now);
        }
        let frame = self.hint_frame();
        self.hints.update(&frame, now);
        self.hints_frame = frame;
        self.keep_hints();
    }

    /// Turn the hints on or off, as the profile card's switch asks.
    pub(super) fn set_hints_enabled(&mut self, on: bool) {
        self.hints.set_enabled(on, ui::time::now());
        self.keep_hints();
    }

    fn keep_hints(&mut self) {
        if let Some(learned) = self.hints.take_changed() {
            // Not kept, a hint at worst shows once more: nothing to tell.
            LEARNED.set(learned).ok();
        }
    }

    pub(super) fn draw_hints(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.hints_painted_before = self.hints.painted_rect();
        self.hints.draw(
            &self.hints_frame,
            scene,
            transform,
            text,
            theme,
            ui::time::now(),
        );
    }
}
