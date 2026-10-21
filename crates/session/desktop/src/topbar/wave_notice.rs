//! Someone waved at this person: the bar says who, beside the faces, for a
//! moment ([`crate::chrome::WaveNotice`]), while the waver's face bounces.
//! Ambient, like the faces themselves: it takes no click and goes on its
//! own.
use super::{Affine, Scene, TextRenderer, Theme, TopBarChrome, t_args};
use ui::time::{Duration, Instant};

/// How long the bar says who waved.
pub(super) const WAVE_NOTICE_FOR: Duration = Duration::from_secs(6);

impl TopBarChrome {
    /// What the bar says about the last wave at this person at `now`;
    /// `None` once it has been up its while.
    pub(super) fn wave_notice_text(&self, now: Instant) -> Option<String> {
        let notice = self.chrome.as_ref()?.snapshot().waved?;
        (now.saturating_duration_since(notice.at) < WAVE_NOTICE_FOR && !self.is_compact())
            .then(|| t_args("topbar-waved-at-you", &[("name", notice.from.into())]))
    }

    /// When the notice up now goes.
    pub(super) fn wave_notice_ends(&self, now: Instant) -> Option<Instant> {
        let notice = self.chrome.as_ref()?.snapshot().waved?;
        notice
            .at
            .checked_add(WAVE_NOTICE_FOR)
            .filter(|ends| now < *ends)
    }

    /// The notice, ending at `right_x` and yielding to the centre band;
    /// returns where the controls before it end.
    pub(super) fn draw_wave_notice(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        (center_y, right_x): (f64, f64),
    ) -> f64 {
        use ui::widget::{ChipFace, ChipLead};
        let Some(text) = self.wave_notice_text(ui::time::now()) else {
            return right_x;
        };
        let height = 24.0;
        let right = right_x - f64::from(theme.tokens.spacing.s2);
        let left_limit = self.center_lane.x1 + f64::from(theme.tokens.spacing.s4);
        let lead = ChipLead::Glyph(ui::icons::Icon::Hand);
        let width = ChipFace::intrinsic_width(&text, lead, false, text_renderer, theme)
            .min(right - left_limit);
        // No room for the words: the bouncing face says it alone.
        if width < 96.0 {
            return right_x;
        }
        let rect = vello::kurbo::Rect::new(
            right - width,
            center_y - height / 2.0,
            right,
            center_y + height / 2.0,
        );
        ChipFace::new(rect, &text)
            .lead(lead)
            .paint(scene, transform, text_renderer, theme);
        rect.x0
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixture::FixtureChrome;
    use super::*;
    use crate::chrome::WaveNotice;
    use tilo_desktop_model::{DesktopTree, Workspace, WorkspaceId};

    /// The bar says who waved for a moment, then lets it go and wakes to
    /// take it away.
    #[test]
    fn the_bar_says_who_waved_for_a_moment() {
        ui::i18n::I18nManager::init("en-US");
        let workspace = Workspace::new(WorkspaceId(1), "Hacked");
        let tree = DesktopTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        };
        let chrome = FixtureChrome::new(tree.clone());
        let bar = TopBarChrome::from_tree(&tree).with_chrome(chrome.clone());
        let now = ui::time::now();
        assert_eq!(bar.wave_notice_text(now), None, "nobody waved");
        chrome.borrow_mut().waved = Some(WaveNotice {
            from: "Eager Dolphin".into(),
            at: now,
        });
        assert_eq!(
            bar.wave_notice_text(now).as_deref(),
            Some("Eager Dolphin waved at you")
        );
        let later = now.checked_add(WAVE_NOTICE_FOR).unwrap();
        assert_eq!(bar.wave_notice_ends(now), Some(later));
        assert_eq!(bar.wave_notice_text(later), None);
        assert_eq!(bar.wave_notice_ends(later), None);
    }
}
