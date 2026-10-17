//! The place a window will open in, held from the moment it is asked for.
//!
//! A launch can take seconds before the app has a window (a cold start, an
//! install first), and the tab only becomes the app once that window maps.
//! Until then the tile is this card: the app's name over the same moving
//! indicator the tile shows for an arriving window, plus a line saying what
//! is happening when that is worth knowing — an install under way and how
//! much of it has arrived, or a start that is taking longer than it should.
//! Once an install reports its first bytes, a bar takes the indicator's
//! place, crossfading so nothing jumps, and stays until the window arrives.
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

/// What stands in the indicator's place.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Indicator {
    /// The moving dots: nothing is measured.
    Dots,
    /// A bar `Some(fraction)` full, or sweeping when the size is unknown.
    Bar(Option<f32>),
}

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
    /// When the first bytes arrived: the bar fades in from here, and stays.
    bar_since: Option<Instant>,
    /// How long the dots take to give way to the bar.
    crossfade: Duration,
    /// The dots' and the bar's opacity as last drawn.
    drawn_indicator: (f32, f32),
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
            bar_since: None,
            crossfade: Duration::ZERO,
            drawn_indicator: (0.0, 0.0),
        }
    }

    /// The line under the indicator right now, and what stands in the
    /// indicator's place.
    pub(crate) fn status_at(&self, now: Instant) -> (String, Indicator) {
        let slow = now.saturating_duration_since(self.since) >= SLOW_AFTER;
        match &self.wait {
            Wait::Opening if slow => (
                t_args(
                    "shell-window-opening-slow",
                    &[("app", self.title.clone().into())],
                ),
                Indicator::Dots,
            ),
            Wait::Opening => (String::new(), Indicator::Dots),
            Wait::Restoring => (t("shell-window-restoring"), Indicator::Dots),
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
                            Indicator::Bar(Some(done)),
                        )
                    }
                    Some(InstallProgress::Downloading { received, .. }) => (
                        t_args(
                            "shell-window-installing-received",
                            &[app(), ("received", ui::format::bytes_si(received).into())],
                        ),
                        Indicator::Bar(None),
                    ),
                    // Once a bar has stood here it stays, full, while the
                    // download is verified and the app starts.
                    _ if self.bar_since.is_some() => (
                        t_args("peer-window-installing", &[app()]),
                        Indicator::Bar(Some(1.0)),
                    ),
                    None if slow => (
                        t_args("shell-window-installing-slow", &[app()]),
                        Indicator::Dots,
                    ),
                    Some(InstallProgress::Installing) | None => {
                        (t_args("peer-window-installing", &[app()]), Indicator::Dots)
                    }
                }
            }
        }
    }

    /// The dots' and the bar's opacity at `now`: one, then the other,
    /// crossfading from when the first bytes arrived, at once under reduced
    /// motion.
    fn indicator_alphas(&self, indicator: Indicator, now: Instant) -> (f32, f32) {
        let (Indicator::Bar(_), Some(since)) = (indicator, self.bar_since) else {
            return (1.0, 0.0);
        };
        let elapsed = now.saturating_duration_since(since);
        if self.reduce_motion || elapsed >= self.crossfade {
            return (0.0, 1.0);
        }
        let shown = elapsed.as_secs_f32() / self.crossfade.as_secs_f32();
        (1.0 - shown, shown)
    }

    #[cfg(test)]
    pub(crate) fn drawn_indicator_for_test(&self) -> (f32, f32) {
        self.drawn_indicator
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
        self.crossfade = Duration::from_millis(u64::from(theme.tokens.motion.durations.base_ms));
        let now = ui::time::now();
        let (status, indicator) = self.status_at(now);
        if matches!(indicator, Indicator::Bar(_)) && self.bar_since.is_none() {
            self.bar_since = Some(now);
        }
        let (dots, bar) = self.indicator_alphas(indicator, now);
        let place = ui::widget::paint_opening_card(
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
            None,
        );
        let faded =
            |scene: &mut Scene, alpha: f32, bounds: Rect, paint: &mut dyn FnMut(&mut Scene)| {
                if alpha <= 0.0 {
                    return;
                }
                let layered = alpha < 1.0;
                if layered {
                    scene.push_layer(
                        vello::peniko::Fill::NonZero,
                        vello::peniko::BlendMode::default(),
                        alpha,
                        transform,
                        &bounds,
                    );
                }
                paint(scene);
                if layered {
                    scene.pop_layer();
                }
            };
        let spinner = &mut self.spinner;
        faded(scene, dots, place, &mut |scene| {
            spinner.set_rect(place);
            Widget::draw(spinner, scene, transform, text_renderer, theme, presence);
        });
        if let Indicator::Bar(fraction) = indicator {
            // In the dots' place, centred on them, so nothing below moves.
            let half =
                (f64::from(theme.tokens.metrics.menu_min_width) / 2.0).min(self.rect.width() / 4.0);
            let height = ui::widget::ProgressBarFace::height(theme);
            let (cx, cy) = (place.center().x, place.center().y);
            let track = Rect::new(cx - half, cy - height / 2.0, cx + half, cy + height / 2.0);
            let face = match fraction {
                Some(fraction) => ui::widget::ProgressBarFace::new(fraction),
                None => ui::widget::ProgressBarFace::indeterminate(self.bar_since.unwrap_or(now)),
            };
            faded(scene, bar, track, &mut |scene| {
                face.paint(
                    &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                    track,
                );
            });
        }
        self.drawn = status;
        self.drawn_indicator = (dots, bar);
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
    /// motion it is still: the card draws again only when its line or
    /// indicator has changed, and waits for the slow line on a deadline.
    fn needs_animation_frame(&self) -> bool {
        if !self.reduce_motion {
            return true;
        }
        let now = ui::time::now();
        let (status, indicator) = self.status_at(now);
        status != self.drawn || self.indicator_alphas(indicator, now) != self.drawn_indicator
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        if !self.reduce_motion {
            return None;
        }
        let now = ui::time::now();
        let slow_line = self.since.checked_add(SLOW_AFTER).filter(|due| *due > now);
        let progress = matches!(self.wait, Wait::Installing { .. })
            .then(|| now.checked_add(PROGRESS_WAKE))
            .flatten();
        slow_line.into_iter().chain(progress).min()
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
        assert_eq!(
            card.status_at(ui::time::now()),
            (String::new(), Indicator::Dots)
        );
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
                Indicator::Dots
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
        let (status, indicator) = card.status_at(ui::time::now());
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
        let Indicator::Bar(Some(fraction)) = indicator else {
            panic!("a known size is a filling bar, got {indicator:?}");
        };
        assert!((fraction - 4.2 / 18.0).abs() < 1e-6);
    }

    fn draw(card: &mut OpeningCard, theme: &Theme) {
        card.rect = Rect::new(0.0, 0.0, 800.0, 600.0);
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        card.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            theme,
            &PresenceCtx::default(),
        );
    }

    #[test]
    fn a_reduced_motion_card_waits_for_its_slow_line_on_a_deadline() {
        let mut still = Theme::dark();
        still.reduce_motion = true;
        let mut card = OpeningCard::new("Slides", Wait::Opening);
        draw(&mut card, &still);
        assert!(
            !card.needs_animation_frame(),
            "a still card asks for no frames"
        );
        let due = card
            .next_wake_deadline()
            .expect("the slow line is due later");
        assert!(due > ui::time::now() && due <= card.since + SLOW_AFTER);
        card.backdate_for_test(SLOW_AFTER);
        assert!(
            card.needs_animation_frame(),
            "the slow line is drawn when due"
        );
        draw(&mut card, &still);
        assert!(!card.needs_animation_frame());
        assert_eq!(card.next_wake_deadline(), None, "nothing more to wait for");
    }

    #[test]
    fn the_dots_give_way_to_the_bar_alone_once_bytes_arrive_and_it_stays() {
        let package = "so.tilo.test.opening-card-bar";
        let mut still = Theme::dark();
        still.reduce_motion = true;
        let mut card = OpeningCard::new("Slides", installing(package));
        draw(&mut card, &still);
        assert_eq!(card.drawn_indicator_for_test(), (1.0, 0.0), "dots, no bar");

        catalogue::set_install_progress(
            package,
            Some(InstallProgress::Downloading {
                received: 1_000,
                total: Some(10_000),
            }),
        );
        draw(&mut card, &still);
        assert_eq!(
            card.drawn_indicator_for_test(),
            (0.0, 1.0),
            "under reduced motion the bar replaces the dots at once"
        );

        catalogue::set_install_progress(package, None);
        draw(&mut card, &still);
        assert_eq!(
            card.drawn_indicator_for_test(),
            (0.0, 1.0),
            "the bar stays once it has appeared"
        );
        card.backdate_for_test(SLOW_AFTER);
        assert_eq!(
            card.status_at(ui::time::now()).0,
            t_args("peer-window-installing", &[("app", "Slides".into())]),
            "a measured install is never called slow"
        );

        let moving = Theme::dark();
        let mut card = OpeningCard::new("Slides", installing(package));
        catalogue::set_install_progress(
            package,
            Some(InstallProgress::Downloading {
                received: 1_000,
                total: None,
            }),
        );
        draw(&mut card, &moving);
        catalogue::set_install_progress(package, None);
        let (dots, bar) = card.drawn_indicator_for_test();
        assert!(
            dots > 0.0 && bar < 1.0 && (dots + bar - 1.0).abs() < 1e-6,
            "the first frame with bytes crossfades: {dots} {bar}"
        );
        card.bar_since = card
            .bar_since
            .and_then(|since| since.checked_sub(card.crossfade));
        assert_eq!(
            card.indicator_alphas(card.status_at(ui::time::now()).1, ui::time::now()),
            (0.0, 1.0),
            "then the bar stands alone"
        );
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
