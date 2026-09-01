//! Small composition helpers shared across the settings panels.
//!
//! These build on `ui`'s widgets — they don't reimplement rendering, they
//! just bundle the recurring shapes (a titled card, a label/value row, a
//! status pill) so every panel reads consistently and the panels stay
//! short. Nothing here is settings-specific beyond the visual grammar.
//!
//! Grouped surfaces come from the shared [`ui::widget::Card`], so every card in
//! the OS reads identically and tracks the theme; the helpers here just bundle
//! the settings-specific row/section shapes on top of it.

use ui::widget::{Badge, Button, Column, Container, Row, Spacer, Text, Widget};

/// Type alias for the boxed-widget currency the `ui` tree speaks.
pub type El = Box<dyn Widget>;

/// Vertical gap (logical px) between stacked cards in a panel. The page
/// heading and this deliberate air do the grouping work, so card outlines do
/// not have to carry every level of hierarchy.
pub const CARD_GAP: f32 = 20.0;

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

/// A fixed-width empty box for deliberate *horizontal* space inside a `Row`
/// (e.g. a gutter between an icon and its label). Pinned width, so it never
/// flex-grows like a bare spacer would.
pub fn hgap(px: f32) -> El {
    Box::new(Container::new().with_no_padding().with_size(px, 1.0))
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

/// The unboxed heading at the start of every settings destination.
///
/// A category is a real page, not merely the selected row in a navigation
/// list. Keeping its identity in plain type (rather than another raised card)
/// establishes where the person is while avoiding the old card-within-page
/// treatment. The small "Settings" eyebrow also makes the shared shell's
/// arrow unambiguous on a narrow content screen: it returns to the Settings
/// list, not to a previous app/window.
pub fn page_heading(title: &str, subtitle: &str) -> El {
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(
                Text::new("SETTINGS").with_size(12.0).with_color(dim()),
            ))
            .push(vgap(5.0))
            .push(Box::new(Text::new(title).with_size(24.0)))
            .push(vgap(5.0))
            .push(Box::new(
                Text::new(subtitle).with_size(13.0).with_color(dim()),
            )),
    )
}

/// A titled "card": a heading line followed by a bordered, padded surface
/// holding `body`. The qOS grouping primitive — every panel is a stack of
/// these. Delegates to the shared [`ui::widget::Card`] so every card in the OS
/// reads identically and tracks the theme (radius/border/fill all from
/// `Theme`); the heading sits inside the surface, above the body.
pub fn card(title: &str, body: El) -> El {
    Box::new(ui::widget::Card::titled(title, body))
}

/// A card whose title row also carries a trailing element (e.g. an "Add"
/// button or a count badge) on the same line as the title. Delegates to the
/// shared [`ui::widget::Card`].
pub fn card_with_action(title: &str, action: El, body: El) -> El {
    Box::new(ui::widget::Card::titled_with_action(title, action, body))
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
///
/// The label and control sit inline on the top line, with the description on
/// its own line beneath. A single leading `Text` keeps the control on the
/// label's line at the right edge — a multi-line block on the left would seize
/// the row and wrap the control below it, dropping it out of alignment.
pub fn setting_row_described(label: &str, description: &str, control: El) -> El {
    let top = Row::new()
        .push(Box::new(Text::new(label).with_size(13.0)))
        .push(Box::new(Spacer::new()))
        .push(control);
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(top))
            .push(Box::new(
                Text::new(description).with_size(12.0).with_color(dim()),
            )),
    )
}

/// A dim helper-text line, for captions and "best-effort" notes.
pub fn caption(text: &str) -> El {
    Box::new(Text::new(text).with_size(12.0).with_color(dim()))
}

/// Default width (logical px) a compact right-aligned control (dropdown,
/// segmented picker) is pinned to, so a short-option control reads as a
/// trailing chooser rather than a full-width form field. Wide enough for the
/// longest option label the settings dropdowns offer.
pub const COMPACT_CONTROL_W: f32 = 220.0;

/// Pin a trailing control to a fixed width so it aligns to the row's right
/// edge instead of stretching across the whole card. `ui`'s `Dropdown` fills
/// its parent (`percent(1.0)` width); wrapping it here gives every settings
/// chooser the same compact, right-aligned footprint — the GNOME/Adwaita
/// look — rather than a form field that spans the card.
pub fn compact_control(control: El) -> El {
    compact_control_w(COMPACT_CONTROL_W, control)
}

/// [`compact_control`] with an explicit width, for controls that need more
/// (long device names) or less room.
pub fn compact_control_w(width: f32, control: El) -> El {
    Box::new(
        Container::new()
            .with_no_padding()
            .with_width(width)
            .with_child(control),
    )
}

/// A settings row whose trailing control is a compact, right-aligned chooser
/// (see [`compact_control`]). Use for dropdown/segmented settings so the
/// label stays left and the control sits at the row's right edge on one line,
/// rather than the control stretching full-width and pushing the label above
/// it.
pub fn choice_row(label: &str, control: El) -> El {
    setting_row(label, compact_control(control))
}

/// [`choice_row`] with a helper line under the label.
pub fn choice_row_described(label: &str, description: &str, control: El) -> El {
    setting_row_described(label, description, compact_control(control))
}

/// A compact, square icon button — a [`ui::widget::Button`] sized down from
/// its default 120×40 pill to a 34×34 icon target. Use for row-level actions
/// (remove, edit) where a full pill would dominate the row.
pub fn icon_button(id: &str, glyph: &str, key: &str) -> El {
    Box::new(Button::new(id, glyph, Some(key)).with_size(34.0, 34.0))
}

/// A small rounded status chip (e.g. "Connected", "Paired", "Offline").
/// Backed by the toolkit's [`Badge::pill`] so the tint carries the meaning
/// and the shape is consistent with the rest of the system.
pub fn status_pill(label: &str, tint: vello::peniko::Color) -> El {
    Box::new(Badge::pill(label, tint))
}

/// A prominent identity block for a pane's first card: a large accent glyph,
/// a bold title, and a dim one-line subtitle stacked beside it. Used where a
/// pane benefits from a hero (About's device identity, Power's battery).
pub fn hero(glyph: &str, title: &str, subtitle: &str) -> El {
    let accent = ui::Theme::dark().accent_primary;
    let text_block = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(Box::new(Text::new(title).with_size(20.0)))
        .push(vgap(3.0))
        .push(Box::new(
            Text::new(subtitle).with_size(12.0).with_color(dim()),
        ));
    let row = Row::new()
        .push(Box::new(
            Text::new(glyph).with_size(34.0).with_color(accent),
        ))
        .push(hgap(14.0))
        .push(Box::new(text_block))
        .push(Box::new(Spacer::new()));
    Box::new(ui::widget::Card::new(Box::new(row)))
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
pub const ROW_GAP: f32 = 12.0;

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
