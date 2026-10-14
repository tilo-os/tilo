//! The place a window will open in, held from the moment it is asked for.
//!
//! A launch can take seconds before the app has a window (a cold start, an
//! install first), and the tab only becomes the app once that window maps.
//! Until then the tile is this card: the app's name over the same moving
//! indicator the tile shows for an arriving window, plus a line saying what
//! is happening when that is worth knowing — an install under way, or a start
//! that is taking longer than it should. A failure or a missed deadline turns
//! the tab into the retry card instead (`peer_windows::settle_local_restore`).

use std::collections::HashMap;

use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use ui::Theme;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::time::{Duration, Instant};
use ui::widget::{AppKind, AppWidget, IconDescriptor, Spinner, TaffyTreeExt, Widget};

/// Past this, a start with nothing else to say says that it is slow. Most
/// apps map well inside it; the launch deadline that gives up is far later.
pub(crate) const SLOW_AFTER: Duration = Duration::from_secs(5);

/// Matches the tile's own opening indicator, so the handover is seamless.
const SPINNER_PX: f32 = 36.0;

pub struct OpeningCard {
    title: String,
    status: String,
    since: Instant,
    spinner: Spinner,
    rect: Rect,
    reduce_motion: bool,
}

impl OpeningCard {
    /// `status` empty means the wait needs no words yet.
    pub fn new(title: &str, status: &str) -> Self {
        Self {
            title: title.to_owned(),
            status: status.to_owned(),
            since: ui::time::now(),
            spinner: Spinner::with_size(SPINNER_PX),
            rect: Rect::ZERO,
            reduce_motion: false,
        }
    }

    /// The line under the indicator right now.
    pub(crate) fn status_at(&self, now: Instant) -> String {
        if self.status.is_empty() && now.saturating_duration_since(self.since) >= SLOW_AFTER {
            crate::shell::i18n::t("shell-window-opening-slow")
        } else {
            self.status.clone()
        }
    }

    #[cfg(test)]
    pub(crate) fn backdate_for_test(&mut self, age: Duration) {
        self.since = self
            .since
            .checked_sub(age)
            .expect("test ages are small constants");
    }
}

impl Widget for OpeningCard {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(
            Style {
                size: Size::from_percent(1.0, 1.0),
                ..Default::default()
            },
            node_map,
            self,
        )
    }

    ui::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.reduce_motion = theme.reduce_motion;
        let status = self.status_at(ui::time::now());
        ui::widget::paint_opening_card(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            self.rect,
            ui::widget::OpeningCardText {
                name: &self.title,
                status: &status,
            },
            &mut self.spinner,
        );
    }

    fn name(&self) -> &'static str {
        "OpeningCard"
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Politeness, Role};
        // The indicator says "opening" to the eye; a screen reader needs
        // the word.
        let status = self.status_at(ui::time::now());
        let description = if status.is_empty() {
            crate::shell::i18n::t("shell-window-opening")
        } else {
            status
        };
        Some(
            AccessNode::new(Role::Group)
                .with_name(self.title.clone())
                .with_description(description)
                .with_state(AccessState::new().busy(true))
                .with_live(Politeness::Polite)
                .with_bounds(self.rect),
        )
    }

    /// The indicator moves for as long as the card stands; under reduced
    /// motion it is still, but the card keeps waking until its slow line is
    /// due so that line still appears on time.
    fn needs_animation_frame(&self) -> bool {
        !self.reduce_motion
            || (self.status.is_empty()
                && ui::time::now().saturating_duration_since(self.since) < SLOW_AFTER)
    }
}

impl AppWidget for OpeningCard {
    fn uses_host_record(&self) -> bool {
        false
    }

    fn app_kind(&self) -> AppKind {
        AppKind::Settings
    }

    fn app_title(&self) -> &str {
        &self.title
    }

    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("󰏗")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quiet_start_says_it_is_slow_only_once_it_is() {
        let mut card = OpeningCard::new("Notes", "");
        assert_eq!(card.status_at(ui::time::now()), "");
        card.backdate_for_test(SLOW_AFTER);
        assert_eq!(
            card.status_at(ui::time::now()),
            crate::shell::i18n::t("shell-window-opening-slow")
        );
    }

    #[test]
    fn an_install_keeps_saying_what_it_is_doing() {
        let mut card = OpeningCard::new("Notes", "Installing Notes…");
        card.backdate_for_test(SLOW_AFTER * 3);
        assert_eq!(card.status_at(ui::time::now()), "Installing Notes…");
    }

    #[test]
    fn the_card_is_announced_as_busy() {
        let card = OpeningCard::new("Notes", "Installing Notes…");
        let node = card.accessibility().expect("card is accessible");
        assert_eq!(node.name.as_deref(), Some("Notes"));
        assert_eq!(node.description.as_deref(), Some("Installing Notes…"));
        assert!(node.state.busy);
        let quiet = OpeningCard::new("Notes", "")
            .accessibility()
            .expect("card is accessible");
        assert_eq!(
            quiet.description,
            Some(crate::shell::i18n::t("shell-window-opening"))
        );
    }
}
