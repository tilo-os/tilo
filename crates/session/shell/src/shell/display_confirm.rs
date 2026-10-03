//! Keep or revert a risky display change a tool made.
//!
//! A layout tool on the privileged display socket (`kanshi`, `wlr-randr`)
//! stages the same display transaction Settings does. Most of its changes
//! are kept as soon as they apply; one that could leave a screen blank or
//! turn off the one in use waits for the person, and the backend restores
//! the previous layout unless they keep it. Settings and the Super+P sheet
//! ask in their own UI; for a tool's change the compositor asks here: a
//! compact card with the countdown, Keep and Revert on every display, so it
//! is visible even when the new layout blanked one of them. Enter keeps,
//! Escape reverts, and running out of time reverts.
//!
//! When the tool is known, a third button keeps the change and stops asking
//! about that tool's risky changes ([`TrustedDisplayTools`]), which
//! Settings › Displays can revoke.
//!
//! The Keep/Revert band is the one the Super+P sheet shows
//! ([`paint_keep_revert_band`]), so a staged layout looks and reads the same
//! wherever it came from.

use std::time::{Duration, Instant};

use settings_store::display_tools::{DisplayTool, TrustedDisplayTools};
use settings_store::hardware::{DisplayTransaction, DisplayTransactionState};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::i18n::ArgValue;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::shell::i18n::{t, t_args};
use crate::shell::overlay_host::layers::{
    LayerCx, LayerGeometry, LayerId, LayerIntent, LayerPaint, Routed, ShellLayer,
};
use ui::TextRole;

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
    focus: Option<BandFocus>,
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
            .style(ui::widget::ButtonStyle::Primary)
            .focused(focus == Some(BandFocus::Keep)),
    );
    ui::widget::Button::paint(
        scene,
        transform,
        text_renderer,
        theme,
        &ui::widget::ButtonFace::new(revert, &revert_label)
            .focused(focus == Some(BandFocus::Revert)),
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

/// Keyboard focus inside the prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Keep,
    Revert,
    AlwaysKeep,
}

/// One display's copy of the prompt.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ConfirmCard {
    pub card: Rect,
    pub keep: Rect,
    pub revert: Rect,
    /// Zero when the tool is unknown and cannot be trusted.
    pub always_keep: Rect,
}

struct Pending {
    transaction: u64,
    tool: Option<DisplayTool>,
    deadline_unix_ms: Option<u64>,
    polled_at: Instant,
    focus: Focus,
}

/// The compositor-owned Keep/Revert prompt for a tool's display change.
#[derive(Default)]
pub(crate) struct DisplayConfirmPrompt {
    pending: Option<Pending>,
    /// The theme's spacing and metrics as of the last paint, so hit-testing
    /// between paints uses the geometry that was drawn.
    spacing: ui::tokens::Spacing,
    metrics: ui::tokens::Metrics,
}

impl DisplayConfirmPrompt {
    /// Ask about transaction `transaction`, which `tool` asked for.
    pub(crate) fn show(&mut self, transaction: u64, tool: Option<DisplayTool>, now: Instant) {
        let deadline_unix_ms = DisplayTransaction::load()
            .filter(|staged| staged.id == transaction)
            .and_then(|staged| staged.deadline_unix_ms);
        self.pending = Some(Pending {
            transaction,
            tool,
            deadline_unix_ms,
            polled_at: now,
            focus: Focus::Keep,
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
        let Some(mut current) = current else {
            self.pending = None;
            return true;
        };
        pending.deadline_unix_ms = current.deadline_unix_ms;
        if current.expired(tilo_clock::unix_now_ms()) {
            if let Err(error) = current.request_revert() {
                tracing::warn!(%error, "display prompt: could not restore the previous layout");
            }
            self.pending = None;
            return true;
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
        self.finish(DisplayTransaction::request_confirmation);
    }

    /// Restore the previous layout now.
    pub(crate) fn revert(&mut self) {
        self.finish(DisplayTransaction::request_revert);
    }

    /// Keep the change and every later one from the same tool.
    pub(crate) fn always_keep(&mut self) {
        let Some(tool) = self.tool().cloned() else {
            return;
        };
        if let Err(error) = TrustedDisplayTools::trust(tool) {
            tracing::warn!(%error, "display prompt: could not remember the trusted tool");
        }
        self.keep();
    }

    fn finish(&mut self, answer: fn(&mut DisplayTransaction) -> std::io::Result<()>) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        if let Some(mut staged) =
            DisplayTransaction::load().filter(|staged| staged.id == pending.transaction)
            && let Err(error) = answer(&mut staged)
        {
            tracing::warn!(%error, "display prompt: could not record the answer");
        }
    }

    fn focus_order(&self) -> &'static [Focus] {
        if self.tool().is_some() {
            &[Focus::Keep, Focus::Revert, Focus::AlwaysKeep]
        } else {
            &[Focus::Keep, Focus::Revert]
        }
    }

    fn move_focus(&mut self, forward: bool) {
        let order = self.focus_order();
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        let current = pending.focus;
        let next = if forward {
            order
                .iter()
                .cycle()
                .skip_while(|focus| **focus != current)
                .nth(1)
        } else {
            order
                .iter()
                .rev()
                .cycle()
                .skip_while(|focus| **focus != current)
                .nth(1)
        };
        if let Some(focus) = next {
            pending.focus = *focus;
        }
    }

    fn activate(&mut self, focus: Focus) {
        match focus {
            Focus::Keep => self.keep(),
            Focus::Revert => self.revert(),
            Focus::AlwaysKeep => self.always_keep(),
        }
    }

    /// Route an input event while the prompt is up. It owns the keyboard
    /// and the pointer: what it does not act on reaches nothing else.
    /// Returns whether the event was absorbed.
    pub(crate) fn handle_event(
        &mut self,
        event: &WindowEvent,
        cursor: Option<Point>,
        outputs: &[Rect],
    ) -> bool {
        let Some(focus) = self.pending.as_ref().map(|pending| pending.focus) else {
            return false;
        };
        match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                match logical_key {
                    Key::Named(NamedKey::Enter | NamedKey::Space) => self.activate(focus),
                    Key::Named(NamedKey::Escape) => self.revert(),
                    Key::Named(NamedKey::Tab | NamedKey::ArrowRight | NamedKey::ArrowDown) => {
                        self.move_focus(true);
                    }
                    Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowUp) => self.move_focus(false),
                    _ => {}
                }
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(point) = cursor {
                    self.click(point, outputs);
                }
                true
            }
            WindowEvent::KeyboardInput { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::Touch(_) => true,
            _ => false,
        }
    }

    fn click(&mut self, point: Point, outputs: &[Rect]) {
        for card in self.cards(outputs) {
            if card.keep.contains(point) {
                return self.keep();
            }
            if card.revert.contains(point) {
                return self.revert();
            }
            if card.always_keep.area() > 0.0 && card.always_keep.contains(point) {
                return self.always_keep();
            }
        }
    }

    /// One card per display, centred on it.
    pub(crate) fn cards(&self, outputs: &[Rect]) -> Vec<ConfirmCard> {
        if !self.visible() {
            return Vec::new();
        }
        let trusted_row = self.tool().is_some();
        let padding = f64::from(self.spacing.s3);
        let row = f64::from(self.metrics.panel_row_height);
        let height = padding * 2.0 + band_height() + if trusted_row { row } else { 0.0 };
        let edge = f64::from(self.spacing.s4);
        outputs
            .iter()
            .filter(|output| output.width() > 0.0 && output.height() > 0.0)
            .map(|output| {
                let width = f64::from(self.metrics.panel_width_sheet)
                    .min(output.width() - edge * 2.0)
                    .max(f64::from(self.metrics.panel_width_utility));
                let card = Rect::from_center_size(output.center(), (width, height));
                let band = self.band_rect(card);
                let BandButtons { keep, revert } = band_buttons(band);
                let always_keep = if trusted_row {
                    Rect::new(
                        band.x0,
                        band.y1 + f64::from(self.spacing.s2),
                        band.x1,
                        band.y1 + row - f64::from(self.spacing.s1),
                    )
                } else {
                    Rect::ZERO
                };
                ConfirmCard {
                    card,
                    keep,
                    revert,
                    always_keep,
                }
            })
            .collect()
    }

    fn band_rect(&self, card: Rect) -> Rect {
        let padding = f64::from(self.spacing.s3);
        Rect::new(
            card.x0 + padding,
            card.y0 + padding,
            card.x1 - padding,
            card.y0 + padding + band_height(),
        )
    }

    fn title(&self) -> String {
        self.tool().map_or_else(
            || t("display-confirm-tool-unknown-title"),
            |tool| {
                t_args(
                    "display-confirm-tool-title",
                    &[("tool", ArgValue::Text(tool.name().to_owned()))],
                )
            },
        )
    }

    fn always_label(&self) -> Option<String> {
        self.tool().map(|tool| {
            t_args(
                "display-confirm-always",
                &[("tool", ArgValue::Text(tool.name().to_owned()))],
            )
        })
    }

    /// Frames are needed while the countdown runs.
    pub(crate) fn needs_frame(&self) -> bool {
        self.visible()
    }

    /// The prompt as assistive technology sees it: a modal dialog whose
    /// buttons are the first display's copy.
    pub(crate) fn accessibility(&self, outputs: &[Rect]) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};

        let pending = self.pending.as_ref()?;
        let card = self.cards(outputs).into_iter().next();
        let status = countdown_status(self.seconds_left(tilo_clock::unix_now_ms()));
        let mut dialog = AccessNode::new(Role::Dialog)
            .with_name(self.title())
            .with_description(format!(
                "{status}. {}",
                t("display-confirm-a11y-description")
            ))
            .with_bounds(card.map(|card| card.card).unwrap_or_default())
            .with_state(AccessState::new().modal(true));
        let mut buttons = vec![
            (
                Focus::Keep,
                t("display-confirm-keep"),
                card.map(|card| card.keep),
            ),
            (
                Focus::Revert,
                t("display-confirm-revert"),
                card.map(|card| card.revert),
            ),
        ];
        if let Some(label) = self.always_label() {
            buttons.push((Focus::AlwaysKeep, label, card.map(|card| card.always_keep)));
        }
        for (focus, name, bounds) in buttons {
            dialog.children.push(
                AccessNode::new(Role::Button)
                    .with_name(name)
                    .with_bounds(bounds.unwrap_or_default())
                    .with_state(AccessState::new().focused(pending.focus == focus)),
            );
        }
        Some(dialog)
    }

    pub(crate) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        outputs: &[Rect],
    ) {
        self.spacing = theme.tokens.spacing;
        self.metrics = theme.tokens.metrics;
        let Some(pending) = self.pending.as_ref() else {
            return;
        };
        // Theme type tokens already carry the user's text scale (HIG §7a).
        let renderer_scale = text_renderer.text_scale();
        text_renderer.set_text_scale(1.0);
        let title = self.title();
        let status = countdown_status(self.seconds_left(tilo_clock::unix_now_ms()));
        let always_label = self.always_label();
        let band_focus = match pending.focus {
            Focus::Keep => Some(BandFocus::Keep),
            Focus::Revert => Some(BandFocus::Revert),
            Focus::AlwaysKeep => None,
        };
        for card in self.cards(outputs) {
            let radius = ui::chrome::ChromeRole::Panel.radius(theme, card.card.height());
            ui::chrome::shell::panel_shadow(scene, transform, card.card, radius, theme, 1.0);
            scene.fill(
                Fill::NonZero,
                transform,
                ui::chrome::shell::panel_ground(theme),
                None,
                &RoundedRect::from_rect(card.card, radius),
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                ui::chrome::shell::panel_border(theme),
                None,
                &RoundedRect::from_rect(card.card, radius),
            );
            paint_keep_revert_band(
                scene,
                transform,
                text_renderer,
                theme,
                self.band_rect(card.card),
                &title,
                &status,
                band_focus,
            );
            if let Some(label) = always_label.as_deref() {
                ui::widget::Button::paint(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    &ui::widget::ButtonFace::new(card.always_keep, label)
                        .focused(pending.focus == Focus::AlwaysKeep),
                );
            }
        }
        text_renderer.set_text_scale(renderer_scale);
    }
}

/// Above every desktop surface while it waits; Escape reverts, as in its
/// own keyboard model.
impl ShellLayer for DisplayConfirmPrompt {
    fn id(&self) -> LayerId {
        LayerId::DisplayConfirmation
    }

    fn is_open(&self) -> bool {
        self.visible()
    }

    fn handle_event(&mut self, event: &WindowEvent, cx: &mut LayerCx<'_, '_>) -> Routed {
        if Self::handle_event(self, event, cx.cursor, cx.geometry.outputs) {
            cx.request(LayerIntent::Repaint);
            Routed::Consumed
        } else {
            Routed::Pass
        }
    }

    fn draw(&mut self, cx: &mut LayerPaint<'_>) {
        Self::draw(
            self,
            cx.scene,
            cx.transform,
            cx.text_renderer,
            cx.theme,
            cx.geometry.outputs,
        );
    }

    fn accessibility(&self, geometry: LayerGeometry<'_>) -> Option<ui::a11y::AccessNode> {
        Self::accessibility(self, geometry.outputs)
    }

    fn accessibility_rect(&self, _geometry: LayerGeometry<'_>, _path: &[usize]) -> Option<Rect> {
        None
    }

    fn focus_accessibility(&mut self, _path: &[usize]) -> bool {
        false
    }

    fn dismiss(&mut self) -> bool {
        let open = self.visible();
        self.revert();
        open
    }

    fn is_animating(&self, _now: Instant) -> bool {
        self.needs_frame()
    }

    /// Follows the transaction between polls: the prompt closes when it is
    /// answered elsewhere or its time runs out.
    fn tick(&mut self, now: Instant) -> bool {
        self.poll(now, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_card_sits_on_every_display() {
        let mut prompt = DisplayConfirmPrompt::default();
        let outputs = [
            Rect::new(0.0, 0.0, 1280.0, 800.0),
            Rect::new(1280.0, 0.0, 3840.0, 1440.0),
        ];
        assert!(prompt.cards(&outputs).is_empty());
        prompt.show(7, Some(DisplayTool::new("/usr/bin/kanshi")), Instant::now());
        let cards = prompt.cards(&outputs);
        assert_eq!(cards.len(), 2);
        for (card, output) in cards.iter().zip(&outputs) {
            assert!(output.contains(card.card.center()));
            assert!(card.card.contains(card.keep.center()));
            assert!(card.card.contains(card.always_keep.center()));
        }
    }

    #[test]
    fn an_unknown_tool_cannot_be_trusted() {
        let mut prompt = DisplayConfirmPrompt::default();
        prompt.show(7, None, Instant::now());
        let cards = prompt.cards(&[Rect::new(0.0, 0.0, 1280.0, 800.0)]);
        assert_eq!(cards[0].always_keep, Rect::ZERO);
        assert_eq!(prompt.focus_order().len(), 2);
    }
}
