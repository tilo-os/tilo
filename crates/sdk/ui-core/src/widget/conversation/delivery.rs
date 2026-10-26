//! One delivery vocabulary, rendered the same in every conversation.
//!
//! A message leaves a composer and may be queued, in flight, handed to a
//! transport, acknowledged by its server, delivered to the other person's
//! device, read, refused, or left uncertain when the transport cannot say. Chat
//! and Messages learn those facts from different systems, in different words,
//! and each used to print its own: Messages had five honest labels, Chat had
//! none at all except a line saying a send was refused.
//!
//! [`DeliveryState`] is the one vocabulary. A transport maps its own states onto
//! [`DeliveryStep`] — a plain rename, no judgement — and [`DeliveryState`]
//! decides the word, the glyph and the tone. Both apps paint it through
//! [`DeliveryFace`], so "delivered" looks the same wherever a person meets it,
//! and a state that cannot be known says so instead of pretending.

use vello::kurbo::{Point, Rect};

use crate::icons::Icon;
use crate::paint::PaintCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::Tone;

/// What a transport reported, before this toolkit decides how to say it.
///
/// A transport's own enum maps onto this one by renaming its variants; nothing
/// is inferred and no step is skipped. Where a transport reports nothing at
/// all, there is no step and no state — an empty delivery line, not a guess.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryStep {
    /// Written here, waiting for this device to send it.
    Queued,
    /// In flight: the transport has it and is working.
    Sending,
    /// Handed to the transport, which has not yet answered.
    Submitted,
    /// The transport's server acknowledged it.
    Acknowledged,
    /// The other person's device has it.
    Delivered,
    /// The other person has read it.
    Read,
    /// The transport refused it.
    Refused,
    /// The transport could not say whether it arrived.
    Uncertain,
}

/// The state a line shows, in the one vocabulary both apps speak.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryState {
    Queued,
    Sending,
    Submitted,
    Sent,
    Delivered,
    Read,
    Failed,
    Uncertain,
}

impl DeliveryState {
    /// The state a transport's step means here.
    #[must_use]
    pub const fn of_step(step: DeliveryStep) -> Self {
        match step {
            DeliveryStep::Queued => Self::Queued,
            DeliveryStep::Sending => Self::Sending,
            DeliveryStep::Submitted => Self::Submitted,
            DeliveryStep::Acknowledged => Self::Sent,
            DeliveryStep::Delivered => Self::Delivered,
            DeliveryStep::Read => Self::Read,
            DeliveryStep::Refused => Self::Failed,
            DeliveryStep::Uncertain => Self::Uncertain,
        }
    }

    /// The catalogue key for this state's word, in the desktop's language.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Queued => "conversation-delivery-queued",
            Self::Sending => "conversation-delivery-sending",
            Self::Submitted => "conversation-delivery-submitted",
            Self::Sent => "conversation-delivery-sent",
            Self::Delivered => "conversation-delivery-delivered",
            Self::Read => "conversation-delivery-read",
            Self::Failed => "conversation-delivery-failed",
            Self::Uncertain => "conversation-delivery-uncertain",
        }
    }

    /// The state's word, in the desktop's language.
    #[must_use]
    pub fn label(self) -> String {
        super::t(self.key())
    }

    /// The glyph this state carries: a clock while it is on its way, a tick
    /// once a transport has it, a circle-check once the other person's device
    /// has it — and a warning where the answer is a refusal or a silence. A
    /// step within a stage (`Submitted` and `Sent`, `Delivered` and `Read`) is
    /// told by its word; the glyph tells the stage, which is what a glance
    /// reads.
    #[must_use]
    pub const fn icon(self) -> Icon {
        match self {
            Self::Queued | Self::Sending => Icon::Clock,
            Self::Submitted | Self::Sent => Icon::Check,
            Self::Delivered | Self::Read => Icon::CircleCheck,
            Self::Failed => Icon::CircleAlert,
            Self::Uncertain => Icon::CircleDot,
        }
    }

    /// The tone the state reads in. Progress is quiet — it is a fact about a
    /// message, not a call on the person's attention — while a refusal and an
    /// unknown are the two states that need them.
    #[must_use]
    pub const fn tone(self) -> Tone {
        match self {
            Self::Failed => Tone::Danger,
            Self::Uncertain => Tone::Warning,
            Self::Queued
            | Self::Sending
            | Self::Submitted
            | Self::Sent
            | Self::Delivered
            | Self::Read => Tone::Neutral,
        }
    }

    /// How far along the one progression this state stands. A later state never
    /// replaces an earlier one, so a stale `Delivered` cannot walk a line back
    /// to `Sending`.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            // A failure or an unknown is not a step: it is an answer about
            // the progression, so it stands at its start.
            Self::Queued | Self::Failed | Self::Uncertain => 0,
            Self::Sending => 1,
            Self::Submitted => 2,
            Self::Sent => 3,
            Self::Delivered => 4,
            Self::Read => 5,
        }
    }

    /// Whether this is the last thing the transport will say about a line.
    #[must_use]
    pub const fn is_final(self) -> bool {
        matches!(self, Self::Read | Self::Failed)
    }

    /// Whether the state asks for the person's attention (a refusal, or an
    /// answer the transport could not give).
    #[must_use]
    pub const fn needs_attention(self) -> bool {
        matches!(self, Self::Failed | Self::Uncertain)
    }

    /// The state a line on `self` should show, given that `later` was just
    /// reported: the later one, unless it would move a final state backwards.
    #[must_use]
    pub fn then(self, later: Self) -> Self {
        if self.is_final() && !later.needs_attention() {
            return self;
        }
        if later.rank() >= self.rank() || later.needs_attention() {
            later
        } else {
            self
        }
    }
}

/// A delivery state as it is painted: a small glyph and its word, in one line.
#[derive(Clone, Debug)]
pub struct DeliveryFace {
    state: DeliveryState,
    label: String,
}

impl DeliveryFace {
    #[must_use]
    pub fn new(state: DeliveryState) -> Self {
        Self {
            state,
            label: state.label(),
        }
    }

    /// The same state, worded by the caller (a refusal carries its reason
    /// beside it in the app's own sentence).
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    #[must_use]
    pub const fn state(&self) -> DeliveryState {
        self.state
    }

    /// What the line says, for the accessible name.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The icon's side: the theme's small glyph, which the text scale grows.
    #[must_use]
    pub fn icon_side(theme: &Theme) -> f64 {
        f64::from(theme.tokens.metrics.icon_sm)
    }

    /// The width the face takes on one line, glyph and gap and word included.
    pub fn width(&self, text: &mut TextRenderer, theme: &Theme) -> f64 {
        let style = theme.text(TextRole::Caption);
        let word = f64::from(text.measure_style(&self.label, None, &style).0);
        if self.label.is_empty() {
            return Self::icon_side(theme);
        }
        Self::icon_side(theme) + f64::from(theme.tokens.spacing.s1) + word
    }

    /// Paint the face with its glyph's left edge at `x`, vertically centred in
    /// `band`. Returns the width painted.
    pub fn paint(&self, paint: &mut PaintCtx<'_>, x: f64, band: Rect) -> f64 {
        let theme = paint.theme;
        let ink = self.state.tone().colour(theme);
        let side = Self::icon_side(theme);
        let mut cursor = x;
        if self.label.is_empty() {
            self.state.icon().draw(
                paint.scene,
                paint.transform,
                Rect::new(cursor, band.y0, cursor + side, band.y1),
                ink,
            );
            return side;
        }
        let box_ = Rect::new(
            cursor,
            band.center().y - side / 2.0,
            cursor + side,
            band.center().y + side / 2.0,
        );
        self.state
            .icon()
            .draw(paint.scene, paint.transform, box_, ink);
        cursor += side + f64::from(theme.tokens.spacing.s1);
        let style = theme.text(TextRole::Caption);
        let line = f64::from(paint.text.measure_style("Ag", None, &style).1);
        let top = band.center().y - line / 2.0;
        paint.text(&self.label, &style, ink, Point::new(cursor, top), None);
        cursor - x + f64::from(paint.text.measure_style(&self.label, None, &style).0)
    }
}
