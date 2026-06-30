//! Small composition helpers shared across the settings panels.
//!
//! These build on `ui`'s widgets — they don't reimplement rendering, they
//! just bundle the recurring shapes (a titled card, a label/value row, a
//! status pill) so every panel reads consistently and the panels stay
//! short. Nothing here is settings-specific beyond the visual grammar.
//!
//! ## Known `ui` gaps these helpers work around
//! - There is no fixed-width container and no per-widget background colour,
//!   so a "card" is a `Container::with_background()` (theme `bg_primary`)
//!   and visual width is governed by flex, not a hard pixel value.
//! - There is no standalone divider widget; section spacing is done with
//!   headers + cards rather than rules.

use ui::widget::{Column, Container, Row, Spacer, Text, Widget};

/// Type alias for the boxed-widget currency the `ui` tree speaks.
pub type El = Box<dyn Widget>;

/// Vertical gap (logical px) between stacked cards in a panel. Slightly
/// larger than the toolkit's default 12 px inter-child gap so each card
/// reads as a distinct group with clear air around it.
pub const CARD_GAP: f32 = 16.0;

/// A fixed-height empty box used to inject deliberate vertical space.
///
/// The toolkit's `Column` applies a *uniform* 12 px gap between children
/// — and crucially, an intrinsic-height column lays out as a CSS **block**
/// stack, where the taffy `gap` property is ignored entirely. So a plain
/// stack of cards renders with the cards *touching*. We therefore place
/// these explicit spacer rows between cards (and to tune the rhythm around
/// section headers), which gives us per-gap control the `Column` API does
/// not expose.
pub fn vgap(px: f32) -> El {
    Box::new(Container::new().with_no_padding().with_height(px))
}

/// Stack a list of cards with a deliberate [`CARD_GAP`] of air between each
/// one. This is the standard body for a panel: it replaces the bare
/// `Column::new().with_intrinsic_height().push(card_a).push(card_b)…`
/// pattern, which renders the cards flush against each other because an
/// intrinsic-height (block-display) column ignores the toolkit gap.
pub fn cards(items: Vec<El>) -> El {
    let mut col = Column::new().with_no_padding().with_intrinsic_height();
    let last = items.len().saturating_sub(1);
    for (i, item) in items.into_iter().enumerate() {
        col = col.push(item);
        if i != last {
            col = col.push(vgap(CARD_GAP));
        }
    }
    Box::new(col)
}

/// A titled "card": a heading line followed by a bordered, padded surface
/// holding `body`. The qOS grouping primitive — every panel is a stack of
/// these. The heading sits *above* the card so the surface reads as one
/// cohesive group.
pub fn card(title: &str, body: El) -> El {
    // The title is the first row *inside* the bordered surface, above the
    // body. Keeping the title within the background `Container` (rather
    // than as a bare sibling above it) is what makes it render reliably:
    // a `Container`'s child is always drawn, whereas a `Text` sitting
    // directly in a column can be dropped by the column's cached draw path.
    let inner = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(Box::new(
            Row::new().push(Box::new(Text::new(title).with_size(15.0))),
        ))
        .push(vgap(CARD_TITLE_GAP))
        .push(body);
    Box::new(Container::new().with_background().with_child(Box::new(inner)))
}

/// Vertical gap (logical px) between a card's title and its body, so the
/// heading clearly sits *above* its group rather than crowding the first
/// row.
pub const CARD_TITLE_GAP: f32 = 12.0;

/// A card whose title row also carries a trailing element (e.g. an "Add"
/// button or a count badge) on the same line as the title.
pub fn card_with_action(title: &str, action: El, body: El) -> El {
    let title_row = Row::new()
        .push(Box::new(Text::new(title).with_size(15.0)))
        .push(Box::new(Spacer::new()))
        .push(action);
    let inner = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(Box::new(title_row))
        .push(vgap(CARD_TITLE_GAP))
        .push(body);
    Box::new(Container::new().with_background().with_child(Box::new(inner)))
}

/// A label-on-the-left, control-on-the-right row — the workhorse settings
/// line. The `Spacer` pushes the control to the trailing edge.
pub fn setting_row(label: &str, control: El) -> El {
    Box::new(
        Row::new()
            .push(Box::new(Text::new(label).with_size(13.0)))
            .push(Box::new(Spacer::new()))
            .push(control),
    )
}

/// A label/value row where the value is plain read-only text.
pub fn value_row(label: &str, value: &str) -> El {
    setting_row(label, Box::new(Text::new(value).with_size(13.0)))
}

/// A label with a short helper line beneath it, paired with a trailing
/// control. Used where a setting needs a one-line explanation.
pub fn setting_row_described(label: &str, description: &str, control: El) -> El {
    let text_block = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(Box::new(Text::new(label).with_size(13.0)))
        .push(Box::new(
            Text::new(description)
                .with_size(11.0)
                .with_color(dim()),
        ));
    Box::new(
        Row::new()
            .push(Box::new(text_block))
            .push(Box::new(Spacer::new()))
            .push(control),
    )
}

/// A dim helper-text line, for captions and "best-effort" notes.
pub fn caption(text: &str) -> El {
    Box::new(Text::new(text).with_size(11.0).with_color(dim()))
}

/// The dim foreground colour. Pulled from the theme at draw time normally,
/// but the helper builders run before `draw`, so we read the dark theme's
/// token here — the same value the OS dark theme ships. Panels that need
/// exact theme-following colour should set it in their own `draw`; for the
/// captions/descriptions this is indistinguishable in practice.
fn dim() -> vello::peniko::Color {
    ui::Theme::dark().text_dim
}

/// Vertical gap (logical px) between rows inside a single card. Smaller
/// than [`CARD_GAP`] so rows feel related within a group while still
/// having visible separation.
pub const ROW_GAP: f32 = 10.0;

/// A vertical stack of pre-built rows inside a column with intrinsic
/// height — the standard body for a card.
///
/// Like [`cards`], we interleave explicit fixed-height spacers because the
/// intrinsic-height column lays out as a block stack that ignores the
/// toolkit's inter-child gap; without them the rows would render flush
/// against each other.
pub fn rows(items: Vec<El>) -> El {
    let mut col = Column::new().with_no_padding().with_intrinsic_height();
    let last = items.len().saturating_sub(1);
    for (i, item) in items.into_iter().enumerate() {
        col = col.push(item);
        if i != last {
            col = col.push(vgap(ROW_GAP));
        }
    }
    Box::new(col)
}
