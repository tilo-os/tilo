//! Rows that carry their own buttons: library lists whose items need a visible
//! command (cancel, retry, remove) that keyboard and assistive-technology users
//! can reach.
//!
//! A [`ListRow`] only offers hover actions, so a row with a persistent command
//! sits beside its buttons in a [`Row`]. The helpers here keep those rows
//! reading as one list: they end at the same trailing edge as the page header,
//! and a hairline separates them the way a `Section` list does.

use ui::theme::Theme;
use ui::widget::{Button, Column, Container, Divider, Element, ListRow, ProgressBar, Row, Widget};

/// A row with its commands beside it, ending at the list's trailing edge.
pub fn with_buttons(row: ListRow, buttons: Vec<Button>) -> Row {
    let mut line = Row::new().no_wrap().push(Box::new(
        Container::new()
            .with_no_padding()
            .with_flex_grow()
            .with_child(Box::new(row)),
    ));
    for button in buttons {
        line = line.push(Box::new(button));
    }
    line
}

/// `line` with a progress bar under its text, on the text's leading edge.
pub fn with_progress(line: impl Widget + 'static, bar: ProgressBar, theme: &Theme) -> Element {
    let spacing = &theme.tokens.spacing;
    Box::new(
        Column::new()
            .with_no_padding()
            .with_gap(spacing.s0)
            .with_intrinsic_height()
            .push(Box::new(line))
            .push(Box::new(
                Row::new()
                    .no_wrap()
                    .with_padding(spacing.s0, spacing.s0)
                    .push(Box::new(bar)),
            )),
    )
}

/// `rows` as one list: touching, with a hairline between neighbours inset
/// like the separators of a plain `Section` list.
pub fn separated(rows: Vec<Element>, theme: &Theme) -> Element {
    let spacing = &theme.tokens.spacing;
    let count = rows.len();
    let mut list = Column::new()
        .with_no_padding()
        .with_gap(spacing.s0)
        .with_intrinsic_height();
    for (index, row) in rows.into_iter().enumerate() {
        list = list.push(row);
        if index + 1 < count {
            list = list.push(Box::new(
                Row::new()
                    .no_wrap()
                    .with_padding(spacing.s3, spacing.s0)
                    .push(Box::new(Divider::horizontal())),
            ));
        }
    }
    Box::new(list)
}

/// A determinate bar for `received` of `total` bytes; with no known total,
/// the indeterminate one.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "a transfer fraction only needs a few digits"
)]
pub fn transfer_bar(received: u64, total: Option<u64>) -> ProgressBar {
    match total {
        Some(total) if total > 0 => {
            ProgressBar::new((received.min(total) as f64 / total as f64) as f32)
        }
        _ => ProgressBar::indeterminate(),
    }
}

/// Whether every word of `query` is in one of `fields`, case aside: what the
/// library's filter field keeps.
#[must_use]
pub fn library_filter_matches(query: &str, fields: &[&str]) -> bool {
    let terms: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
    terms.is_empty()
        || terms.iter().all(|term| {
            fields
                .iter()
                .any(|field| field.to_lowercase().contains(term))
        })
}
