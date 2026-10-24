//! Arriving where people are, said in the bar: the faces of who is here
//! pulse (the host bounces them on the presence the bar paints) and a short
//! name slides out of them, then goes on its own
//! ([`crate::chrome::ArrivalNotice`]). Nothing is drawn over the windows.
//! The first time in a public workspace it says what the people there see
//! of the person instead, and a press on it opens the person's own menu,
//! where they choose otherwise.
use super::{Affine, ChromeIntent, Point, Rect, Scene, TextRenderer, Theme, TopBarChrome};
use ui::time::{Duration, Instant};
use vello::peniko::{BlendMode, Fill};

/// How long the bar says who is here.
pub(super) const ARRIVAL_NOTICE_FOR: Duration = Duration::from_millis(4000);

/// The status as it stands at one instant.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ArrivalShown {
    pub text: String,
    /// It tells what the people here see of the person: a press opens
    /// their menu.
    pub seen: bool,
    /// How far it has slid out of the faces, `0..=1`.
    pub reveal: f64,
    pub opacity: f32,
}

impl ArrivalShown {
    /// What changes the bar's paint: quantized, so a still status caches.
    pub(super) fn key(&self) -> (String, bool, u16, u16) {
        let quantize = |v: f64| {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a unit fraction scaled to a thousand fits u16"
            )]
            let q = (v.clamp(0.0, 1.0) * 1000.0).round() as u16;
            q
        };
        (
            self.text.clone(),
            self.seen,
            quantize(self.reveal),
            quantize(f64::from(self.opacity)),
        )
    }
}

impl TopBarChrome {
    /// The motion the person allows, as of the last paint.
    fn arrival_motion(&self) -> ui::tokens::EffectiveMotion {
        self.cache_theme.as_ref().map_or_else(
            || ui::tokens::EffectiveMotion::new(ui::tokens::Motion::default(), false),
            Theme::motion,
        )
    }

    /// The status at `now`; `None` once it has been up its while, or with
    /// nothing to say.
    pub(super) fn arrival_shown(&self, now: Instant) -> Option<ArrivalShown> {
        if self.is_compact() {
            return None;
        }
        let notice = self.chrome.as_ref()?.snapshot().arrived?;
        let ends = notice.at.checked_add(ARRIVAL_NOTICE_FOR)?;
        if now >= ends {
            return None;
        }
        let text = notice.text();
        if text.is_empty() {
            return None;
        }
        let motion = self.arrival_motion();
        let spec = ui::motion::Transient::READOUT;
        let reveal = spec.progress_after(
            ui::motion::Direction::Enter,
            motion,
            ui::tokens::MotionSource::Pointer,
            now.saturating_duration_since(notice.at),
        );
        Some(ArrivalShown {
            text,
            seen: notice.seen.is_some(),
            reveal,
            opacity: ui::motion::Transient::opacity(spec, (notice.at, ends), now, motion),
        })
    }

    /// When the status up now goes.
    pub(super) fn arrival_notice_ends(&self, now: Instant) -> Option<Instant> {
        let notice = self.chrome.as_ref()?.snapshot().arrived?;
        notice
            .at
            .checked_add(ARRIVAL_NOTICE_FOR)
            .filter(|ends| now < *ends)
    }

    /// Whether the status slides or fades at `now`, so the bar owes frames.
    pub(super) fn arrival_notice_moving(&self, now: Instant) -> bool {
        let Some(notice) = self
            .chrome
            .as_ref()
            .and_then(|chrome| chrome.snapshot().arrived)
        else {
            return false;
        };
        let Some(ends) = notice.at.checked_add(ARRIVAL_NOTICE_FOR) else {
            return false;
        };
        ui::motion::Transient::is_moving(
            ui::motion::Transient::READOUT,
            (notice.at, ends),
            now,
            self.arrival_motion(),
        )
    }

    /// The status, sliding out of the faces that end at `right_x` and
    /// yielding to the centre band; returns where the controls before it
    /// end.
    pub(super) fn draw_arrival_notice(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        (center_y, right_x): (f64, f64),
    ) -> f64 {
        use ui::widget::{ChipFace, ChipLead};
        self.arrival_rect = Rect::ZERO;
        self.arrival_pressable = false;
        let Some(shown) = self.arrival_shown(ui::time::now()) else {
            return right_x;
        };
        let height = ChipFace::height(theme);
        let right = right_x - f64::from(theme.tokens.spacing.s2);
        let left_limit = self.center_lane.x1 + f64::from(theme.tokens.spacing.s4);
        let width =
            ChipFace::intrinsic_width(&shown.text, ChipLead::None, false, text_renderer, theme)
                .min(right - left_limit);
        // No room for the words: the pulsing faces say it alone.
        if width < 96.0 {
            return right_x;
        }
        let rect = Rect::new(
            right - width,
            center_y - height / 2.0,
            right,
            center_y + height / 2.0,
        );
        // It comes out of the faces: uncovered from their side, the words
        // travelling with the edge.
        let shown_w = width * shown.reveal.clamp(0.0, 1.0);
        let clip = Rect::new(rect.x1 - shown_w, rect.y0 - 2.0, rect.x1, rect.y1 + 2.0);
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            shown.opacity,
            transform,
            &clip,
        );
        let slid = Rect::new(
            rect.x0 + width - shown_w,
            rect.y0,
            rect.x1 + width - shown_w,
            rect.y1,
        );
        let hovered = shown.seen && self.last_cursor.is_some_and(|p| rect.contains(p));
        ChipFace::new(slid, &shown.text).hovered(hovered).paint(
            scene,
            transform,
            text_renderer,
            theme,
        );
        scene.pop_layer();
        self.arrival_rect = rect;
        self.arrival_pressable = shown.seen;
        rect.x0
    }

    pub(super) const ARRIVAL_TARGET: &'static str = "topbar:arrival";

    /// The status for assistive technology, when a press on it opens the
    /// person's menu.
    pub(super) fn arrival_accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        if self.arrival_rect.is_zero_area() || !self.arrival_pressable {
            return None;
        }
        let shown = self.arrival_shown(ui::time::now())?;
        Some(
            AccessNode::new(Role::Button)
                .with_action_target(Self::ARRIVAL_TARGET)
                .with_name(shown.text)
                .with_bounds(self.arrival_rect)
                .with_state(AccessState {
                    focusable: true,
                    ..Default::default()
                }),
        )
    }

    /// What a press at `point` on the status asks for: the person's own
    /// menu, when it tells what the people here see of them.
    pub(super) fn arrival_intent_at(&self, point: Point) -> Option<ChromeIntent> {
        (self.arrival_pressable && self.arrival_rect.contains(point))
            .then_some(ChromeIntent::ToggleProfile)
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixture::FixtureChrome;
    use super::*;
    use crate::chrome::ArrivalNotice;
    use tilo_desktop_model::{Collaborator, DesktopTree, Workspace, WorkspaceId};

    fn room() -> DesktopTree {
        let mut workspace = Workspace::new(WorkspaceId(1), "Hacked");
        workspace.collaborators = vec![
            Collaborator::new("ada", "Ada Lovelace"),
            Collaborator::new("bo", "Bo Brave"),
        ];
        DesktopTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        }
    }

    /// Arriving says who is here on the bar's own faces, in a few words
    /// beside them, never in a surface of its own; then it lets go.
    #[test]
    fn arriving_slides_a_short_name_out_of_the_faces_for_a_moment() {
        ui::i18n::I18nManager::init("en-US");
        let tree = room();
        let chrome = FixtureChrome::new(tree.clone());
        let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome.clone());
        bar.sync_from_tree(&tree);
        let now = ui::time::now();
        chrome.borrow_mut().arrived = Some(ArrivalNotice {
            people: vec!["ada".into(), "bo".into()],
            names: vec!["Ada Lovelace".into(), "Bo Brave".into()],
            seen: None,
            at: now,
        });
        let shown = bar.arrival_shown(now).expect("said");
        assert_eq!(shown.text, "Ada and one other are here");
        super::super::tests::lay_out_and_draw(&mut bar, 1600.0);
        let later = now.checked_add(ARRIVAL_NOTICE_FOR).unwrap();
        assert_eq!(bar.arrival_notice_ends(now), Some(later));
        assert_eq!(bar.arrival_shown(later), None, "it goes on its own");
    }

    /// The first time in a public workspace it says what its people see,
    /// and a press opens the person's own menu to choose otherwise.
    #[test]
    fn what_the_people_here_see_opens_your_menu() {
        ui::i18n::I18nManager::init("en-US");
        let tree = room();
        let chrome = FixtureChrome::new(tree.clone());
        let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome.clone());
        bar.sync_from_tree(&tree);
        chrome.borrow_mut().arrived = Some(ArrivalNotice {
            people: vec!["ada".into()],
            names: vec!["Ada Lovelace".into()],
            seen: Some("They see you're here".into()),
            at: ui::time::now(),
        });
        let _settled = ui::motion::testing::settled();
        super::super::tests::lay_out_and_draw(&mut bar, 1600.0);
        let status = bar.arrival_rect;
        assert!(!status.is_zero_area());
        assert!(
            status.x1 <= bar.avatar_stack_rect.x0 + 0.5,
            "it comes out of the faces: {status:?} then {:?}",
            bar.avatar_stack_rect
        );
        assert_eq!(
            bar.arrival_intent_at(status.center()),
            Some(ChromeIntent::ToggleProfile)
        );
    }
}
