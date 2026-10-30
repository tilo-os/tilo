//! The way out of full screen, said once over the window that fills it.
//!
//! Full screen hides the OS's own chrome: the window's bar, with the control
//! that leaves it, is gone. The person who entered (a page asking for full
//! screen, a remote session going immersive, a key) is otherwise left with no
//! visible way back, so the shell says it, once, over the window.
//!
//! - One hint for every app. An app declares what fills its window and hands
//!   the full screen to the shell; none of them draws chrome of its own.
//! - It names the leaving key as the person's keyboard labels it, so the pill
//!   reads "Press Esc to leave full screen" on a US layout and the same
//!   sentence with `Esc` spelled the local way on another.
//! - It is a polite live region: a screen reader says it once, after what it
//!   is already saying, and it takes no focus and no pointer.
//! - It goes on its own after [`FULLSCREEN_HINT_HOLD`], fading out on the
//!   standard confirmation's exit: what it says is known by then, and there
//!   is nothing to dismiss.

use std::time::{Duration, Instant};

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use ui::a11y::{AccessNode, Politeness, Role};
use ui::i18n::ArgValue;
use ui::motion::Transient;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;

use super::ShellOverlayHost;
use crate::shell::i18n::t_args;

/// How long the way out is said over a window that just filled the display:
/// long enough to read once, short enough to be gone before the person has
/// settled into what they opened.
pub(crate) const FULLSCREEN_HINT_HOLD: Duration = Duration::from_secs(3);

/// When a hint that began at `entered` goes. Saturating, because an `Instant`
/// that has run out of room must end the hint rather than wrap it into
/// staying up forever.
pub(crate) fn hint_gone_at(entered: Instant) -> Instant {
    entered.checked_add(FULLSCREEN_HINT_HOLD).unwrap_or(entered)
}

/// The key that leaves full screen, as the desktop's shortcuts name it.
const LEAVING_KEY: &str = "Esc";

fn words() -> String {
    t_args(
        "shell-fullscreen-way-out",
        &[(
            "key",
            ArgValue::Text(ui::keycaps::active().key(LEAVING_KEY)),
        )],
    )
}

/// The width the pill's text wraps at, inside its padding.
fn width_limit(rect: Rect, pad_x: f64) -> f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a pill's text width is a few hundred logical pixels"
    )]
    {
        (rect.width() - 2.0 * pad_x).max(0.0) as f32
    }
}

impl ShellOverlayHost {
    /// Say the way out of the window that just filled the display at
    /// `entered`: once per entry, whichever door it came through. The entry
    /// is remembered after the words have gone, so a later follow of the same
    /// full screen does not say them again.
    pub(super) fn say_fullscreen_way_out(&mut self, entered: Instant) {
        self.fullscreen_hint = Some(entered);
    }

    /// Forget the hint: the window left full screen, so there is nothing to
    /// leave and nothing to say.
    pub(super) fn forget_fullscreen_way_out(&mut self) {
        self.fullscreen_hint = None;
    }

    /// When the hint began, while it is up at `now`: the entry is kept after
    /// the words have gone, so a later follow of the same full screen does not
    /// say them again.
    pub(super) fn fullscreen_hint_shown(&self, now: Instant) -> Option<Instant> {
        self.fullscreen_hint
            .filter(|entered| now >= *entered && now < hint_gone_at(*entered))
    }

    /// When the hint next needs a frame without input.
    pub(super) fn fullscreen_hint_deadline(&self, now: Instant) -> Option<Instant> {
        let theme = &self.layout_theme;
        let entered = self.fullscreen_hint_shown(now)?;
        let gone = hint_gone_at(entered);
        let fade_from = Transient::exit_starts(Transient::CONFIRMATION, gone, theme.motion());
        if !theme.reduce_motion && now < fade_from {
            Some(fade_from)
        } else if now < gone {
            Some(gone)
        } else {
            None
        }
    }

    /// Whether the hint is fading now, so the frame clock owes frames.
    pub(super) fn fullscreen_hint_fading(&self, now: Instant) -> bool {
        let theme = &self.layout_theme;
        self.fullscreen_hint_shown(now).is_some_and(|entered| {
            Transient::is_moving(
                Transient::CONFIRMATION,
                (entered, hint_gone_at(entered)),
                now,
                theme.motion(),
            )
        })
    }

    /// The pill the hint paints over `display`, for a `text` run and the
    /// theme's spacing: clear of the display's own edges on a narrow one.
    fn fullscreen_hint_rect(
        display: Rect,
        (text_width, text_height): (f32, f32),
        theme: &Theme,
    ) -> Rect {
        let pad_x = f64::from(theme.tokens.spacing.s4);
        let pad_y = f64::from(theme.tokens.spacing.s2);
        let inset = f64::from(theme.tokens.spacing.s6);
        let width =
            (f64::from(text_width) + 2.0 * pad_x).min((display.width() - 2.0 * inset).max(0.0));
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

    /// Paint the way out over the display, if it is still up.
    pub(super) fn draw_fullscreen_hint(
        &self,
        scene_sink: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let now = ui::time::now();
        let display = self.overlay_area();
        let Some(entered) = self.fullscreen_hint_shown(now) else {
            return;
        };
        let alpha = Transient::opacity(
            Transient::CONFIRMATION,
            (entered, hint_gone_at(entered)),
            now,
            theme.motion(),
        );
        if alpha <= 0.0 {
            return;
        }
        let text = words();
        let style = theme.text(TextRole::Body);
        let (width, height) = text_renderer.measure_style(&text, None, &style);
        let rect = Self::fullscreen_hint_rect(display, (width, height), theme);
        let faded = alpha < 0.999;
        if faded {
            scene_sink.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                alpha,
                transform,
                &ui::widget::floating_surface::painted_bounds(rect, theme),
            );
        }
        ui::widget::floating_surface::paint_card(scene_sink, transform, rect, theme);
        let ink = theme
            .tokens
            .colors
            .on_surface(ui::widget::floating_surface::card_ground(theme))
            .ink;
        let pad_x = f64::from(theme.tokens.spacing.s4);
        let pad_y = f64::from(theme.tokens.spacing.s2);
        text_renderer.draw_style(
            scene_sink,
            &text,
            ink,
            transform.pre_translate((rect.x0 + pad_x, rect.y0 + pad_y).into()),
            Some(width_limit(rect, pad_x)),
            &style,
        );
        if faded {
            scene_sink.pop_layer();
        }
    }

    /// What the hint says, as a polite live region: said once after what the
    /// screen reader is already saying, and never focused.
    pub(super) fn fullscreen_hint_accessibility(&self, now: Instant) -> Option<AccessNode> {
        self.fullscreen_hint_shown(now)?;
        Some(hint_node())
    }
}

/// What the hint says, as a polite live region: said once after what the
/// screen reader is already saying, and never focused. It reads the hint's
/// own words, so it needs nothing of the host.
pub(super) fn hint_node() -> AccessNode {
    AccessNode::new(Role::StaticText)
        .with_hashed_key("shell-fullscreen-way-out")
        .with_live(Politeness::Polite)
        .with_value(words())
}
