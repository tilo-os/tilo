//! Callout blocks: a paragraph set apart as a note, a tip, something
//! important, a warning or a caution.
//!
//! The kinds are GitHub-flavoured Markdown's alerts, so a callout reads and
//! writes as `> [!WARNING]` followed by its quoted text, and stores as
//! `callout:warning`. A callout paints as a status wash with its kind's icon
//! and name above the text: information is `Info`, a tip `Success`, a
//! warning `Warning`, a caution `Danger`; something important has no status
//! hue of its own and takes the neutral wash.

use vello::kurbo::Rect;

use crate::icons::Icon;
use crate::tokens::TextRole;
use crate::tokens::colors::WashStrength;
use crate::widget::{Tone, Widget};

use super::{Block, BlockDrawCtx};

/// The five alerts GitHub-flavoured Markdown defines, in its order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CalloutKind {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl CalloutKind {
    pub const ALL: [Self; 5] = [
        Self::Note,
        Self::Tip,
        Self::Important,
        Self::Warning,
        Self::Caution,
    ];

    /// The stored, lower-case name (`warning`): the `callout:` spelling,
    /// the document attribute and the agent action's kind name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::Tip => "tip",
            Self::Important => "important",
            Self::Warning => "warning",
            Self::Caution => "caution",
        }
    }

    /// The kind named `name`, in any case (`WARNING` as an alert marker
    /// spells it, `warning` as stored).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.name().eq_ignore_ascii_case(name.trim()))
    }

    /// The alert marker that opens the callout in Markdown: `[!WARNING]`.
    #[must_use]
    pub fn marker(self) -> String {
        format!("[!{}]", self.name().to_ascii_uppercase())
    }

    /// The callout an alert marker line names: `[!NOTE]` (after the quote's
    /// `> `), with nothing but spaces after it.
    #[must_use]
    pub fn from_marker(line: &str) -> Option<Self> {
        let name = line.trim().strip_prefix("[!")?.strip_suffix(']')?;
        Self::from_name(name)
    }

    /// The name a person reads on the callout and in the slash menu.
    #[must_use]
    pub fn label(self) -> String {
        crate::i18n!(match self {
            Self::Note => "block-callout-note",
            Self::Tip => "block-callout-tip",
            Self::Important => "block-callout-important",
            Self::Warning => "block-callout-warning",
            Self::Caution => "block-callout-caution",
        })
    }

    /// The status the callout's wash carries.
    #[must_use]
    pub const fn tone(self) -> Tone {
        match self {
            Self::Note => Tone::Info,
            Self::Tip => Tone::Success,
            Self::Important => Tone::Neutral,
            Self::Warning => Tone::Warning,
            Self::Caution => Tone::Danger,
        }
    }

    #[must_use]
    pub const fn icon(self) -> Icon {
        match self {
            Self::Note => Icon::Info,
            Self::Tip => Icon::Lightbulb,
            Self::Important => Icon::Flag,
            Self::Warning => Icon::TriangleAlert,
            Self::Caution => Icon::CircleAlert,
        }
    }
}

/// Height of the band above a callout's text that holds its icon and name,
/// at 1× text: one body line and a tight gap on either side.
pub(super) fn callout_header_height() -> f64 {
    let body = crate::tokens::typography::stock().style(TextRole::Body);
    let tight = crate::tokens::spacing::RhythmScale::BASE.tight;
    f64::from(body.size * body.line_height + 2.0 * tight)
}

/// Paint a callout: its wash from the content edge `x0` to the line's end,
/// over the header band and the text, the icon and name in the header, and
/// the text in the wash's ink.
pub(super) fn draw_callout(b: &mut Block, kind: CalloutKind, x0: f64, ctx: &mut BlockDrawCtx<'_>) {
    let theme = ctx.theme;
    let tokens = &theme.tokens;
    let scale = f64::from(theme.text_scale);
    let line_rect = b.line.get_rect();
    let header = callout_header_height() * scale;
    let surface = Rect::new(x0, line_rect.y0 - header, line_rect.x1, line_rect.y1);
    let wash = tokens.colors.wash_on(
        tokens.colors.surfaces.content,
        kind.tone(),
        WashStrength::Rest,
    );
    ctx.scene.fill(
        vello::peniko::Fill::NonZero,
        ctx.transform,
        wash.fill,
        None,
        &vello::kurbo::RoundedRect::from_rect(surface, f64::from(tokens.radii.control)),
    );
    // The icon and the name start where the text does.
    let text_x = line_rect.x0 + b.line.text_inset(theme);
    let label_style = theme.text(TextRole::Body).strong();
    let label = kind.label();
    let (_, label_h) = ctx.text_renderer.measure_style(&label, None, &label_style);
    let label_h = f64::from(label_h);
    let label_y = surface.y0 + (header - label_h).max(0.0) / 2.0;
    let icon_side = f64::from(tokens.metrics.icon_sm * theme.text_scale);
    let icon_rect = Rect::new(
        text_x,
        label_y + (label_h - icon_side) / 2.0,
        text_x + icon_side,
        label_y + (label_h + icon_side) / 2.0,
    );
    crate::icons::draw_icon(
        ctx.scene,
        ctx.transform,
        icon_rect,
        kind.icon(),
        wash.mark,
        crate::icons::LUCIDE_DEFAULT_STROKE_PX,
    );
    ctx.text_renderer.draw_style(
        ctx.scene,
        &label,
        wash.mark,
        super::translated(
            ctx.transform,
            icon_rect.x1 + f64::from(tokens.metrics.icon_label_gap) * scale,
            label_y,
        ),
        None,
        &label_style,
    );
    let mut inked = theme.clone();
    inked.text_primary = wash.ink;
    inked.tokens.colors.text_primary = wash.ink;
    inked.text_dim = wash.ink_dim;
    inked.tokens.colors.text_dim = wash.ink_dim;
    b.line.draw(
        ctx.scene,
        ctx.transform,
        ctx.text_renderer,
        &inked,
        ctx.presence,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alert_markers_name_each_kind_in_any_case() {
        for kind in CalloutKind::ALL {
            assert_eq!(CalloutKind::from_marker(&kind.marker()), Some(kind));
            assert_eq!(CalloutKind::from_name(kind.name()), Some(kind));
        }
        assert_eq!(
            CalloutKind::from_marker("[!warning] "),
            Some(CalloutKind::Warning)
        );
        assert_eq!(CalloutKind::from_marker("[!DANGER]"), None);
        assert_eq!(CalloutKind::from_marker("[!NOTE] trailing"), None);
    }
}
