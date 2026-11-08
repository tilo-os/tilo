//! The way out of full screen, said once over the window that fills it.
//!
//! Full screen gives the whole display to the app and hides the desktop's own
//! chrome, with the control that leaves it. A person who entered it (a page
//! asking for full screen, a key) is otherwise left with no visible way back,
//! so the desktop says it, once, over the window.
//!
//! - It names the leaving key as the person's keyboard labels it, so the pill
//!   reads "Press Esc to leave full screen" on a US layout and the same
//!   sentence with `Esc` spelled the local way on another.
//! - It is a polite live region: a screen reader says it once, after what it
//!   is already saying, and it takes no focus and no pointer.
//! - It goes on its own after [`HOLD`], fading out on the standard
//!   confirmation's exit: what it says is known by then, and there is nothing
//!   to dismiss. Leaving full screen takes the words away with it.

use ui::a11y::{AccessNode, Politeness, Role};
use ui::motion::Transient;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::{Duration, Instant};
use ui::tokens::TextRole;
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use crate::i18n::t_args;

/// How long the way out is said over a window that just filled the display: long enough to read
/// once, short enough to be gone before the person has settled into what they opened.
pub const HOLD: Duration = Duration::from_secs(3);

/// The key that leaves full screen, as the desktop's shortcuts name it.
const LEAVING_KEY: &str = "Esc";

/// What the words say, as the person's keyboard labels the leaving key.
fn words() -> String {
    t_args(
        "fullscreen-way-out",
        &[("key", ui::keycaps::active().key(LEAVING_KEY).into())],
    )
}

/// A window that took a display full screen: where it did, when, and when the words go.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Said {
    display: Rect,
    entered: Instant,
    gone: Instant,
}

/// The way out as the chrome holds it: which display a window filled, and the pill once it is
/// painted.
#[derive(Debug, Default)]
pub struct FullscreenHint {
    said: Option<Said>,
    /// When the words begin to fade, as the theme read at the last paint says.
    fade_from: Option<Instant>,
    /// The pill as painted, for assistive technology; empty while it is not.
    pill: Rect,
}

impl FullscreenHint {
    /// A window took `display` full screen at `entered`, or left it (`None`):
    /// leaving takes the words away with it. Each entry says them once,
    /// whichever door it came through.
    pub fn follow(&mut self, entered: Option<(Rect, Instant)>) {
        let said = entered.map(|(display, at)| Said {
            display,
            entered: at,
            gone: at.checked_add(HOLD).unwrap_or(at),
        });
        if self.said != said {
            self.said = said;
            self.fade_from = None;
            self.pill = Rect::ZERO;
        }
    }

    /// The window that filled a display, while its words are up at `now` .
    fn up(&self, now: Instant) -> Option<Said> {
        self.said.filter(|said| now < said.gone)
    }

    /// Whether the words are up at `now` .
    #[must_use]
    pub fn is_up(&self, now: Instant) -> bool {
        self.up(now).is_some()
    }

    /// The pill as painted, `Rect::ZERO` while there is none.
    #[must_use]
    pub fn pill_rect(&self) -> Rect {
        self.pill
    }

    /// Whether a frame is owed at `now` : the words are up, or the last ones still stand in the
    /// scene and must be taken away.
    #[must_use]
    pub fn needs_redraw(&self, now: Instant) -> bool {
        self.is_up(now) || !self.pill.is_zero_area()
    }

    /// Whether the words fade at `now` , so the frame clock owes frames.
    #[must_use]
    pub fn is_moving(&self, now: Instant) -> bool {
        self.up(now).is_some_and(|said| {
            self.fade_from
                .is_some_and(|from| from < said.gone && now >= from)
        })
    }

    /// When the words next need a frame without input.
    #[must_use]
    pub fn next_wake_deadline(&self, now: Instant) -> Option<Instant> {
        let said = self.up(now)?;
        Some(match self.fade_from.filter(|from| *from < said.gone) {
            Some(from) if now < from => from,
            _ => said.gone,
        })
    }

    /// What it says to assistive technology, while it is up at `now` .
    #[must_use]
    pub fn accessibility(&self, now: Instant) -> Option<AccessNode> {
        self.up(now)?;
        Some(
            AccessNode::new(Role::StaticText)
                .with_hashed_key("chrome-fullscreen-way-out")
                .with_live(Politeness::Polite)
                .with_value(words())
                .with_bounds(self.pill),
        )
    }

    /// Paint the words over the display a window filled, if they are still up.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let now = ui::time::now();
        let motion = theme.motion();
        let Some(said) = self.up(now) else {
            self.fade_from = None;
            self.pill = Rect::ZERO;
            return;
        };
        let fade_from = Transient::exit_starts(Transient::CONFIRMATION, said.gone, motion);
        self.fade_from = Some(fade_from);
        let alpha = Transient::opacity(
            Transient::CONFIRMATION,
            (said.entered, said.gone),
            now,
            motion,
        );
        if alpha <= 0.0 {
            self.pill = Rect::ZERO;
            return;
        }
        let words = words();
        let style = theme.text(TextRole::Body);
        let (width, height) = text.measure_style(&words, None, &style);
        let pill = pill_rect(said.display, (width, height), theme);
        self.pill = pill;
        let faded = alpha < 0.999;
        if faded {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                alpha,
                transform,
                &ui::widget::floating_surface::painted_bounds(pill, theme),
            );
        }
        ui::widget::floating_surface::paint_card(scene, transform, pill, theme);
        let ink = theme
            .tokens
            .colors
            .on_surface(ui::widget::floating_surface::card_ground(theme))
            .ink;
        let pad_x = f64::from(theme.tokens.spacing.s4);
        let pad_y = f64::from(theme.tokens.spacing.s2);
        text.draw_style(
            scene,
            &words,
            ink,
            transform.pre_translate((pill.x0 + pad_x, pill.y0 + pad_y).into()),
            Some(text_width(pill, pad_x)),
            &style,
        );
        if faded {
            scene.pop_layer();
        }
    }
}

/// The width the pill's text wraps at, inside its padding.
fn text_width(pill: Rect, pad_x: f64) -> f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a pill's text width is a few hundred logical pixels"
    )]
    {
        (pill.width() - 2.0 * pad_x).max(0.0) as f32
    }
}

/// Where the pill goes over `display` , for a `text` run and the theme's spacing: centred across
/// it, clear of its top edge on a narrow one.
fn pill_rect(display: Rect, (text_width, text_height): (f32, f32), theme: &Theme) -> Rect {
    let pad_x = f64::from(theme.tokens.spacing.s4);
    let pad_y = f64::from(theme.tokens.spacing.s2);
    let inset = f64::from(theme.tokens.spacing.s6);
    let width = (f64::from(text_width) + 2.0 * pad_x).min((display.width() - 2.0 * inset).max(0.0));
    let height = f64::from(text_height) + 2.0 * pad_y;
    let center_x = display.center().x;
    let y0 = display.y0 + inset;
    Rect::new(
        center_x - width / 2.0,
        y0,
        center_x + width / 2.0,
        y0 + height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISPLAY: Rect = Rect::new(0.0, 0.0, 1280.0, 800.0);

    fn ms(t: Instant, n: u64) -> Instant {
        t.checked_add(Duration::from_millis(n))
            .expect("test instants stay in range")
    }

    #[test]
    fn a_window_that_filled_the_display_says_the_way_out_once_then_goes() {
        let t0 = Instant::now();
        let mut hint = FullscreenHint::default();
        assert!(
            !hint.is_up(t0),
            "nothing is said before a window fills a display"
        );
        hint.follow(Some((DISPLAY, t0)));
        assert!(hint.is_up(t0), "entering full screen says it at once");
        assert_eq!(
            hint.next_wake_deadline(t0),
            Some(ms(t0, 3_000)),
            "a frame is owed when it goes"
        );
        assert!(hint.is_up(ms(t0, 2_900)), "it stays up long enough to read");
        assert!(
            !hint.is_up(ms(t0, 3_000)),
            "and is gone after three seconds"
        );
        // Following the same full screen again does not say it a second time.
        hint.follow(Some((DISPLAY, t0)));
        assert!(!hint.is_up(ms(t0, 3_500)));
    }

    #[test]
    fn leaving_full_screen_takes_the_words_away() {
        let t0 = Instant::now();
        let later = ms(t0, 500);
        let mut hint = FullscreenHint::default();
        hint.follow(Some((DISPLAY, later)));
        assert!(hint.is_up(later));
        hint.follow(None);
        assert!(!hint.is_up(later), "leaving takes the words with it");
        assert_eq!(hint.next_wake_deadline(later), None);
        assert!(!hint.needs_redraw(later), "and nothing is owed a frame");
    }
}
