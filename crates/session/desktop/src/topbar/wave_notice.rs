//! Someone waved at this person: the bar says who, beside the faces, for a
//! moment ([`crate::chrome::WaveNotice`]), while the waver's face bounces.
//! Ambient, like the faces themselves: it takes no click and goes on its
//! own.
use super::{Affine, Scene, TextRenderer, Theme, TopBarChrome, t_args};
use ui::time::{Duration, Instant};
use ui::widget::{ChipFace, ChipLead};

/// How long the bar says who waved.
pub(super) const WAVE_NOTICE_FOR: Duration = Duration::from_secs(6);

const WAVE_NOTICE_LEAD: ChipLead = ChipLead::Glyph(ui::icons::Icon::Hand);

impl TopBarChrome {
    /// What the bar says about the last wave at this person at `now`;
    /// `None` once it has been up its while.
    pub(super) fn wave_notice_text(&self, now: Instant) -> Option<String> {
        let notice = self.chrome.as_ref()?.snapshot().waved?;
        (now.saturating_duration_since(notice.at) < WAVE_NOTICE_FOR && !self.is_compact())
            .then(|| t_args("topbar-waved-at-you", &[("name", notice.from.into())]))
    }

    /// The notice up at `now`, worded to fit `room` px of chip: the
    /// waver's name gives way first, cut short with an ellipsis, so the
    /// words saying what happened always stand whole. `None` when not
    /// even the name's first letter fits beside them.
    pub(super) fn wave_notice_fitted(
        &self,
        now: Instant,
        room: f64,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> Option<String> {
        let whole = self.wave_notice_text(now)?;
        let mut fits = |text: &str| {
            ChipFace::intrinsic_width(text, WAVE_NOTICE_LEAD, false, text_renderer, theme) <= room
        };
        if fits(&whole) {
            return Some(whole);
        }
        let name = self.chrome.as_ref()?.snapshot().waved?.from;
        (1..name.chars().count())
            .rev()
            .map(|kept| {
                let head: String = name.chars().take(kept).collect();
                t_args(
                    "topbar-waved-at-you",
                    &[("name", format!("{}\u{2026}", head.trim_end()).into())],
                )
            })
            .find(|text| fits(text))
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
        let height = 24.0;
        let right = right_x - f64::from(theme.tokens.spacing.s2);
        // It yields to what the centre draws (the clock, or the activity
        // pill across the whole lane), not to the lane kept for an
        // activity: at a laptop's width that lane alone left the notice
        // no room for its words.
        let left_limit = self.center_rect.x1 + f64::from(theme.tokens.spacing.s4);
        // No room for the words: the bouncing face says it alone.
        let Some(text) =
            self.wave_notice_fitted(ui::time::now(), right - left_limit, text_renderer, theme)
        else {
            return right_x;
        };
        let width = ChipFace::intrinsic_width(&text, WAVE_NOTICE_LEAD, false, text_renderer, theme);
        let rect = vello::kurbo::Rect::new(
            right - width,
            center_y - height / 2.0,
            right,
            center_y + height / 2.0,
        );
        ChipFace::new(rect, &text).lead(WAVE_NOTICE_LEAD).paint(
            scene,
            transform,
            text_renderer,
            theme,
        );
        rect.x0
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixture::FixtureChrome;
    use super::*;
    use crate::chrome::WaveNotice;
    use tilo_desktop_model::{DesktopTree, Workspace, WorkspaceId};

    fn bar_waved_at_by(from: &str, at: Instant) -> TopBarChrome {
        ui::i18n::I18nManager::init("en-US");
        let workspace = Workspace::new(WorkspaceId(1), "Hacked");
        let tree = DesktopTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        };
        let chrome = FixtureChrome::new(tree.clone());
        chrome.borrow_mut().waved = Some(WaveNotice {
            from: from.into(),
            at,
        });
        TopBarChrome::from_tree(&tree).with_chrome(chrome)
    }

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

    /// Where the bar is narrow, the waver's name gives way, never the
    /// words saying they waved.
    #[test]
    fn a_narrow_bar_shortens_the_name_never_the_words() {
        let now = ui::time::now();
        let bar = bar_waved_at_by("Gentle Dolphin", now);
        let theme = Theme::default();
        let mut text = TextRenderer::new();
        let mut width =
            |s: &str| ChipFace::intrinsic_width(s, WAVE_NOTICE_LEAD, false, &mut text, &theme);
        let whole = width("Gentle Dolphin waved at you");
        let tight = width("Gentle D\u{2026} waved at you");
        let least = width("G\u{2026} waved at you");
        let mut text = TextRenderer::new();
        assert_eq!(
            bar.wave_notice_fitted(now, whole, &mut text, &theme)
                .as_deref(),
            Some("Gentle Dolphin waved at you"),
            "room for it all"
        );
        for room in [
            whole - 1.0,
            f64::midpoint(whole, tight),
            tight,
            f64::midpoint(tight, least),
            least,
        ] {
            let fitted = bar
                .wave_notice_fitted(now, room, &mut text, &theme)
                .expect("the words fit");
            assert!(
                fitted.starts_with('G') && fitted.ends_with("\u{2026} waved at you"),
                "{room}px: {fitted:?}"
            );
            assert!(
                ChipFace::intrinsic_width(&fitted, WAVE_NOTICE_LEAD, false, &mut text, &theme)
                    <= room,
                "{fitted:?} fits {room}px"
            );
        }
        assert_eq!(
            bar.wave_notice_fitted(now, tight, &mut text, &theme)
                .as_deref(),
            Some("Gentle D\u{2026} waved at you")
        );
        assert_eq!(
            bar.wave_notice_fitted(now, least - 1.0, &mut text, &theme),
            None,
            "no room for the words: the bouncing face says it alone"
        );
    }
}
