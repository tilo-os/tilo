//! The place a window will open in, held from the moment it is asked for.
//!
//! A launch can take seconds before the app has a window (a cold start, an
//! install first), and the tab only becomes the app once that window maps.
//! Until then the tile is this card: the app's name over the same moving
//! indicator the tile shows for an arriving window, plus a line saying what
//! is happening when that is worth knowing — an install under way and how
//! much of it has arrived, or a start that is taking longer than it should.
//! A failure or a missed deadline turns the tab into the retry card instead
//! (`peer_windows::settle_local_restore`).

use std::collections::HashMap;

use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use ui::Theme;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::time::{Duration, Instant};
use ui::widget::{AppKind, AppWidget, IconDescriptor, Spinner, TaffyTreeExt, Widget};

use crate::shell::catalogue::{self, InstallProgress};
use crate::shell::i18n::{t, t_args};

/// Past this, a wait with nothing measured to show says that it is slow.
/// Most apps map well inside it; the launch deadline that gives up is far
/// later.
pub(crate) const SLOW_AFTER: Duration = Duration::from_secs(5);

/// Under reduced motion nothing moves, so the card wakes this often to
/// show new bytes.
const PROGRESS_WAKE: Duration = Duration::from_millis(250);

/// What the place is waiting for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wait {
    /// A start: nothing to say until it is slow.
    Opening,
    /// A window coming back, which says so from the start.
    Restoring,
    /// The app is fetched and installed before it starts.
    Installing { package_id: String, app: String },
}

pub struct OpeningCard {
    title: String,
    wait: Wait,
    since: Instant,
    spinner: Spinner,
    rect: Rect,
    reduce_motion: bool,
    /// The status line last drawn, so a change in it (new bytes, the slow
    /// line) asks for a frame even when nothing moves.
    drawn: String,
}

impl OpeningCard {
    pub fn new(title: &str, wait: Wait) -> Self {
        Self {
            title: title.to_owned(),
            wait,
            since: ui::time::now(),
            spinner: ui::widget::opening_card_spinner(),
            rect: Rect::ZERO,
            reduce_motion: false,
            drawn: String::new(),
        }
    }

    /// The line under the indicator right now, and how much of a measured
    /// download is done.
    pub(crate) fn status_at(&self, now: Instant) -> (String, Option<f32>) {
        let slow = now.saturating_duration_since(self.since) >= SLOW_AFTER;
        match &self.wait {
            Wait::Opening if slow => (
                t_args(
                    "shell-window-opening-slow",
                    &[("app", self.title.clone().into())],
                ),
                None,
            ),
            Wait::Opening => (String::new(), None),
            Wait::Restoring => (t("shell-window-restoring"), None),
            Wait::Installing { package_id, app } => {
                let app = || ("app", app.clone().into());
                match catalogue::install_progress(package_id) {
                    Some(InstallProgress::Downloading {
                        received,
                        total: Some(total),
                    }) if total > 0 => {
                        #[expect(
                            clippy::cast_precision_loss,
                            reason = "a bar's fraction, drawn a few hundred pixels wide"
                        )]
                        let done = received.min(total) as f32 / total as f32;
                        (
                            t_args(
                                "shell-window-installing-of",
                                &[
                                    app(),
                                    ("received", ui::format::bytes_si(received).into()),
                                    ("total", ui::format::bytes_si(total).into()),
                                ],
                            ),
                            Some(done),
                        )
                    }
                    Some(InstallProgress::Downloading { received, .. }) => (
                        t_args(
                            "shell-window-installing-received",
                            &[app(), ("received", ui::format::bytes_si(received).into())],
                        ),
                        None,
                    ),
                    None if slow => (t_args("shell-window-installing-slow", &[app()]), None),
                    Some(InstallProgress::Installing) | None => {
                        (t_args("peer-window-installing", &[app()]), None)
                    }
                }
            }
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
        let (status, progress) = self.status_at(ui::time::now());
        let below = ui::widget::paint_opening_card(
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
        if let Some(progress) = progress {
            // Narrow, and close under the line it measures.
            let cx = self.rect.center().x;
            let half =
                (f64::from(theme.tokens.metrics.menu_min_width) / 2.0).min(self.rect.width() / 4.0);
            let top = below + f64::from(theme.tokens.spacing.s2);
            ui::widget::ProgressBarFace::new(progress).paint(
                &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                Rect::new(
                    cx - half,
                    top,
                    cx + half,
                    top + ui::widget::ProgressBarFace::height(theme),
                ),
            );
        }
        self.drawn = status;
    }

    fn name(&self) -> &'static str {
        "OpeningCard"
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Politeness, Role};
        // The indicator says "opening" to the eye; a screen reader needs
        // the word.
        let (status, _) = self.status_at(ui::time::now());
        let description = if status.is_empty() {
            t("shell-window-opening")
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

    /// The indicator moves for as long as the card stands. Under reduced
    /// motion it is still, but the card keeps waking until its slow line
    /// is due, and draws again whenever its line has changed.
    fn needs_animation_frame(&self) -> bool {
        let now = ui::time::now();
        !self.reduce_motion
            || now.saturating_duration_since(self.since) < SLOW_AFTER
            || self.status_at(now).0 != self.drawn
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        (self.reduce_motion && matches!(self.wait, Wait::Installing { .. }))
            .then(|| ui::time::now().checked_add(PROGRESS_WAKE))
            .flatten()
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

    fn installing(package_id: &str) -> Wait {
        Wait::Installing {
            package_id: package_id.to_owned(),
            app: "Slides".to_owned(),
        }
    }

    #[test]
    fn a_quiet_start_says_it_is_slow_only_once_it_is_and_names_the_app() {
        let mut card = OpeningCard::new("Notes", Wait::Opening);
        assert_eq!(card.status_at(ui::time::now()), (String::new(), None));
        card.backdate_for_test(SLOW_AFTER);
        assert_eq!(
            card.status_at(ui::time::now()).0,
            t_args("shell-window-opening-slow", &[("app", "Notes".into())])
        );
        assert!(card.status_at(ui::time::now()).0.contains("Notes"));
    }

    #[test]
    fn an_install_says_how_much_has_arrived_and_when_it_is_slow() {
        let package = "so.tilo.test.opening-card";
        let mut card = OpeningCard::new("Slides", installing(package));
        assert_eq!(
            card.status_at(ui::time::now()),
            (
                t_args("peer-window-installing", &[("app", "Slides".into())]),
                None
            )
        );
        card.backdate_for_test(SLOW_AFTER);
        assert_eq!(
            card.status_at(ui::time::now()).0,
            t_args("shell-window-installing-slow", &[("app", "Slides".into())])
        );

        catalogue::set_install_progress(
            package,
            Some(InstallProgress::Downloading {
                received: 4_200_000,
                total: Some(18_000_000),
            }),
        );
        let (status, progress) = card.status_at(ui::time::now());
        catalogue::set_install_progress(package, None);
        assert_eq!(
            status,
            t_args(
                "shell-window-installing-of",
                &[
                    ("app", "Slides".into()),
                    ("received", "4.2 MB".into()),
                    ("total", "18.0 MB".into()),
                ]
            )
        );
        assert!((progress.unwrap() - 4.2 / 18.0).abs() < 1e-6);
    }

    #[test]
    fn the_card_is_announced_as_busy() {
        let card = OpeningCard::new("Notes", Wait::Restoring);
        let node = card.accessibility().expect("card is accessible");
        assert_eq!(node.name.as_deref(), Some("Notes"));
        assert_eq!(node.description, Some(t("shell-window-restoring")));
        assert!(node.state.busy);
        let quiet = OpeningCard::new("Notes", Wait::Opening)
            .accessibility()
            .expect("card is accessible");
        assert_eq!(quiet.description, Some(t("shell-window-opening")));
    }
}
