//! Rows whose items need a visible command beside them: a [`ListRow`] offers
//! only hover actions, so its commands sit next to it in a [`Row`].

use ui::theme::Theme;
use ui::widget::{Button, Column, Container, Divider, Element, ListRow, ProgressBar, Row, Widget};

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

/// Whether every word of `query` is in one of `fields`, case aside.
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
