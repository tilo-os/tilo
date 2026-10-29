//! A risky display change a tool made, waiting for the person.
//!
//! A layout tool on the privileged display socket (`kanshi`, `wlr-randr`)
//! stages the same display transaction Settings does. Most of its changes
//! are kept as soon as they apply; one that could leave a screen blank or
//! turn off the one in use waits for the person, and the backend restores
//! the previous layout unless they keep it. The question itself is an app
//! ([`tilo_sheets::display_confirm`]); this is what the shell holds for it:
//! the transaction, its deadline, and the answer recorded for it.
//!
//! The Keep/Revert band the Super+P sheet paints lives here too
//! (`paint_keep_revert_band`).

use std::time::{Duration, Instant};

use settings_store::display_tools::DisplayTool;
use settings_store::hardware::{DisplayTransaction, DisplayTransactionState};
use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect};
use vello::peniko::Fill;

use ui::i18n::ArgValue;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::shell::i18n::{t, t_args};
use ui::TextRole;
use ui::widget::ButtonRole;

/// How often a shown prompt re-reads the transaction, so it follows the
/// backend (applied elsewhere, restored on timeout) without per-frame I/O.
const POLL: Duration = Duration::from_millis(250);

/// Height of the Keep/Revert band: a title line and a status line with the
/// two buttons beside them.
pub(crate) fn band_height() -> f64 {
    76.0
}

/// Which button of a Keep/Revert band has keyboard focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BandFocus {
    Keep,
    Revert,
}

/// Where a painted band put its buttons.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct BandButtons {
    pub keep: Rect,
    pub revert: Rect,
}

/// Where Keep and Revert sit in `band`.
pub(crate) fn band_buttons(band: Rect) -> BandButtons {
    let revert = Rect::new(
        band.x1 - 86.0,
        band.y0 + 20.0,
        band.x1 - 12.0,
        band.y1 - 20.0,
    );
    let keep = Rect::new(
        revert.x0 - 82.0,
        band.y0 + 20.0,
        revert.x0 - 8.0,
        band.y1 - 20.0,
    );
    BandButtons { keep, revert }
}

/// State shared by the prompt and projection sheet's standard button faces.
#[derive(Clone, Copy)]
pub(crate) struct BandState {
    pub focus: Option<BandFocus>,
    /// The button under the pointer, if any: it lifts, as every button does.
    pub hover: Option<BandFocus>,
    pub disabled: bool,
}

/// Paint a Keep/Revert band into `band`: `title` over `status` on the left,
/// Keep and Revert on the right.
pub(crate) fn paint_keep_revert_band(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    band: Rect,
    title: &str,
    status: &str,
    state: BandState,
) -> BandButtons {
    let colors = &theme.tokens.colors;
    let type_scale = &theme.tokens.typography;
    let radius = ui::chrome::ChromeRole::Control.radius(theme, band.height());
    scene.fill(
        Fill::NonZero,
        transform,
        colors.lift(0.35),
        None,
        &RoundedRect::from_rect(band, radius),
    );
    let text_width = width_f32(band.width() - 190.0);
    text_renderer.draw_style(
        scene,
        title,
        colors.text_primary,
        transform.pre_translate((band.x0 + 14.0, band.y0 + 12.0).into()),
        Some(text_width),
        &type_scale
            .style(TextRole::Caption)
            .with_weight(TextWeight::Medium),
    );
    text_renderer.draw_style(
        scene,
        status,
        colors.text_dim,
        transform.pre_translate((band.x0 + 14.0, band.y0 + 42.0).into()),
        Some(text_width),
        &type_scale.style(TextRole::Caption),
    );
    let BandButtons { keep, revert } = band_buttons(band);
    let keep_label = t("display-confirm-keep");
    let revert_label = t("display-confirm-revert");
    ui::widget::Button::paint(
        scene,
        transform,
        text_renderer,
        theme,
        &ui::widget::ButtonFace::new(keep, &keep_label)
            .role(ButtonRole::Primary)
            .focused(state.focus == Some(BandFocus::Keep))
            .hovered(state.hover == Some(BandFocus::Keep))
            .disabled(state.disabled),
    );
    ui::widget::Button::paint(
        scene,
        transform,
        text_renderer,
        theme,
        &ui::widget::ButtonFace::new(revert, &revert_label)
            .focused(state.focus == Some(BandFocus::Revert))
            .hovered(state.hover == Some(BandFocus::Revert))
            .disabled(state.disabled),
    );
    BandButtons { keep, revert }
}

/// The countdown line under a band's title.
pub(crate) fn countdown_status(seconds_left: Option<u64>) -> String {
    seconds_left.map_or_else(
        || t("display-confirm-applying"),
        |seconds| {
            let seconds = f64::from(u32::try_from(seconds).unwrap_or(u32::MAX));
            t_args(
                "display-confirm-countdown",
                &[("seconds", ArgValue::Number(seconds))],
            )
        },
    )
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "a card-sized text width fits f32"
)]
fn width_f32(width: f64) -> f32 {
    width.max(0.0) as f32
}

struct Pending {
    transaction: u64,
    tool: Option<DisplayTool>,
    deadline_unix_ms: Option<u64>,
    polled_at: Instant,
}

/// The transaction a tool's display change waits on, and the answer being
/// recorded for it.
#[derive(Default)]
pub(crate) struct DisplayQuestion {
    pending: Option<Pending>,
    answer: Option<crate::shell::display_persistence::PendingAnswer>,
    notice: Option<String>,
}

impl DisplayQuestion {
    /// Ask about transaction `transaction`, which `tool` asked for.
    pub(crate) fn show(&mut self, transaction: u64, tool: Option<DisplayTool>, now: Instant) {
        let deadline_unix_ms = DisplayTransaction::load()
            .filter(|staged| staged.id == transaction)
            .and_then(|staged| staged.deadline_unix_ms);
        self.answer = None;
        self.notice = None;
        self.pending = Some(Pending {
            transaction,
            tool,
            deadline_unix_ms,
            polled_at: now,
        });
    }

    pub(crate) fn visible(&self) -> bool {
        self.pending.is_some()
    }

    /// The tool whose change is being asked about.
    pub(crate) fn tool(&self) -> Option<&DisplayTool> {
        self.pending.as_ref()?.tool.as_ref()
    }

    /// Follow the transaction: close once it is no longer waiting (kept,
    /// reverted or replaced), and revert it when its time is up. Returns
    /// whether the prompt closed.
    pub(crate) fn poll(&mut self, now: Instant, force: bool) -> bool {
        if let Some(answer) = self.answer.as_ref() {
            match answer.poll() {
                Ok(None) => return false,
                Ok(Some(())) => {
                    self.answer = None;
                    self.notice = None;
                    self.pending = None;
                    return true;
                }
                Err(error) => {
                    self.answer = None;
                    self.notice = Some(t_args(
                        "display-confirm-answer-failed",
                        &[("reason", ArgValue::Text(error.to_string()))],
                    ));
                    tracing::warn!(%error, "display prompt: could not record the answer");
                }
            }
        }
        let Some(pending) = self.pending.as_mut() else {
            return false;
        };
        if !force && now.duration_since(pending.polled_at) < POLL {
            return false;
        }
        pending.polled_at = now;
        let current = DisplayTransaction::load().filter(|staged| {
            staged.id == pending.transaction
                && staged.state == DisplayTransactionState::AwaitingConfirmation
        });
        let Some(current) = current else {
            self.pending = None;
            return true;
        };
        pending.deadline_unix_ms = current.deadline_unix_ms;
        if current.expired(tilo_clock::unix_now_ms()) {
            self.finish(crate::shell::display_persistence::Answer::Revert);
        }
        false
    }

    /// Whole seconds before the previous layout comes back.
    pub(crate) fn seconds_left(&self, now_unix_ms: u64) -> Option<u64> {
        let deadline = self.pending.as_ref()?.deadline_unix_ms?;
        Some(deadline.saturating_sub(now_unix_ms).div_ceil(1000))
    }

    /// Keep the change: the backend commits it as the layout for these
    /// displays.
    pub(crate) fn keep(&mut self) {
        self.finish(crate::shell::display_persistence::Answer::Keep);
    }

    /// Restore the previous layout now.
    pub(crate) fn revert(&mut self) {
        self.finish(crate::shell::display_persistence::Answer::Revert);
    }

    /// Keep the change and every later one from the same tool.
    pub(crate) fn always_keep(&mut self) {
        if self.answer.is_some() {
            return;
        }
        let Some(pending) = self.pending.as_ref() else {
            return;
        };
        let Some(tool) = pending.tool.clone() else {
            return;
        };
        self.accept_queued_answer(crate::shell::display_persistence::answer_and_trust(
            pending.transaction,
            tool,
        ));
    }

    fn finish(&mut self, answer: crate::shell::display_persistence::Answer) {
        if self.answer.is_some() {
            return;
        }
        let Some(pending) = self.pending.as_ref() else {
            return;
        };
        self.accept_queued_answer(crate::shell::display_persistence::answer_for(
            answer,
            Some(pending.transaction),
        ));
    }

    fn accept_queued_answer(
        &mut self,
        queued: std::io::Result<crate::shell::display_persistence::PendingAnswer>,
    ) {
        match queued {
            Ok(answer) => {
                self.answer = Some(answer);
                self.notice = None;
            }
            Err(error) => {
                self.notice = Some(t_args(
                    "display-confirm-answer-failed",
                    &[("reason", ArgValue::Text(error.to_string()))],
                ));
                tracing::warn!(%error, "display prompt: could not queue the answer");
            }
        }
    }

    /// The countdown, or what became of the answer.
    pub(crate) fn status(&self) -> String {
        if self.answer.is_some() {
            t("display-confirm-answer-pending")
        } else if let Some(notice) = self.notice.as_ref() {
            notice.clone()
        } else {
            countdown_status(self.seconds_left(tilo_clock::unix_now_ms()))
        }
    }

    /// An answer is being recorded.
    pub(crate) fn answering(&self) -> bool {
        self.answer.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_nothing_staged_waits_without_a_countdown() {
        let mut question = DisplayQuestion::default();
        assert!(!question.visible());
        question.show(7, Some(DisplayTool::new("/usr/bin/kanshi")), Instant::now());
        assert!(question.visible());
        assert_eq!(question.seconds_left(0), None);
        assert_eq!(question.status(), t("display-confirm-applying"));
        assert_eq!(question.tool().map(DisplayTool::name), Some("kanshi"));
    }
}
