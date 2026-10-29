//! Arranging the panel: the controls it shows, in order (dragged, moved
//! with Ctrl+Up and Ctrl+Down as in every reorderable list, or with each
//! row's Move up and Move down), each removable but Wi-Fi, which is how a
//! new desktop gets online; the controls this machine could add; Reset
//! defaults and Done.

use ui::TextRole;
use ui::framework::ViewCx;
use ui::icons::Icon;
use ui::tokens::Rhythm;
use ui::widget::virtual_list::{ListModel as _, VirtualList};
use ui::widget::{Button, ButtonRole, Column, Container, Element, ListRow, Row, Spacer, Text};

use super::{Arrangement, Parts};
use crate::effect::QsEffect;
use crate::faces::{control_id, control_label};
use crate::i18n::t;
use crate::resource::QuickSettings;

pub(super) fn page(cx: &ViewCx<QuickSettings>, parts: &Parts) -> Element {
    cx.switch(&parts.arrangement, move |cx, arrangement| {
        editor(cx, arrangement)
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "the editor's parts in the order they stand"
)]
fn editor(cx: &ViewCx<QuickSettings>, arrangement: &Arrangement) -> Element {
    let actions = cx.actions();
    let done = actions.bind_invocation(
        Button::new("qs-edit-done", &t("qs-edit-done")).role(ButtonRole::Primary),
        QuickSettings::EDIT.with(false),
    );
    let mut page = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_rhythm(Rhythm::Related)
        .with_id("qs-edit")
        .push(Box::new(
            Row::new()
                .no_wrap()
                .with_align_items(ui::taffy::AlignItems::Center)
                .push(Box::new(
                    Text::new(&t("qs-edit-controls"))
                        .with_role(TextRole::Heading)
                        .with_heading_level(2),
                ))
                .push(Box::new(Spacer::new()))
                .push(Box::new(done)),
        ))
        .push(Box::new(
            Text::new(&t("qs-edit-help")).with_role(TextRole::Caption),
        ))
        .push(Box::new(
            Text::new(&t("qs-edit-active-heading"))
                .with_role(TextRole::Label)
                .strong(),
        ));
    let active = arrangement.active.clone();
    let count = active.len();
    let rows_cx = cx.actions();
    let list = VirtualList::from_items(
        active,
        ui::tokens::Metrics::BASE.row_single,
        |control| control_id(*control),
        move |control, index, _| {
            let mut row = ListRow::new(
                format!("qs-edit-{}", control_id(*control)),
                control_label(*control),
            )
            .icon(Icon::ArrowUpDown)
            .leading_tooltip(t(if control.required() {
                "qs-edit-required-control-a11y"
            } else {
                "qs-edit-optional-control-a11y"
            }));
            let moved = |to: usize| QsEffect::MoveControl {
                control: control_id(*control).to_owned(),
                to: u32::try_from(to).unwrap_or(u32::MAX),
            };
            if let Some(up) = index.checked_sub(1)
                && let Some(binding) = rows_cx.effect(&moved(up))
            {
                row = row.action_binding(Icon::ArrowUp, t("qs-edit-move-up"), binding);
            }
            if let Some(down) = index.checked_add(1).filter(|down| *down < count)
                && let Some(binding) = rows_cx.effect(&moved(down))
            {
                row = row.action_binding(Icon::ArrowDown, t("qs-edit-move-down"), binding);
            }
            if control.required() {
                row = row.value(t("qs-edit-required"));
            } else if let Some(binding) = rows_cx.effect(&QsEffect::HideControl {
                control: control_id(*control).to_owned(),
            }) {
                row = row.action_binding(Icon::Minus, t("qs-edit-remove-a11y"), binding);
            }
            Box::new(row) as Element
        },
    );
    if let Ok(list) = list {
        let dispatcher = cx.dispatcher();
        let list = list.with_reorder(
            |_, source, target| {
                target.placement != ui::reorder::DropPlacement::Inside && *source != target.key
            },
            move |model, dropped| {
                let (Some(from), Some(target)) = (
                    model.index_of(&dropped.source),
                    model.index_of(&dropped.target.key),
                ) else {
                    return false;
                };
                let boundary = match dropped.target.placement {
                    ui::reorder::DropPlacement::Before => target,
                    ui::reorder::DropPlacement::After => target.saturating_add(1),
                    ui::reorder::DropPlacement::Inside => return false,
                };
                let to = if boundary > from {
                    boundary.saturating_sub(1)
                } else {
                    boundary
                };
                dispatcher.effect(&QsEffect::MoveControl {
                    control: dropped.source.to_owned(),
                    to: u32::try_from(to).unwrap_or(u32::MAX),
                });
                model.move_item(from, to)
            },
        );
        let row_height = ui::tokens::Metrics::BASE.row_single;
        #[allow(
            clippy::cast_precision_loss,
            reason = "a handful of controls, well inside f32's exact integers"
        )]
        let height = row_height * count as f32;
        page = page.push(Box::new(
            Container::new()
                .with_no_padding()
                .with_height(height)
                .with_child(Box::new(list)),
        ));
    }
    if !arrangement.available.is_empty() {
        page = page.push(Box::new(
            Text::new(&t("qs-edit-available-heading"))
                .with_role(TextRole::Label)
                .strong(),
        ));
        let mut chips = Row::new().with_rhythm(Rhythm::Sibling);
        for control in &arrangement.available {
            chips = chips.push(Box::new(
                actions.bind_effect_invocation(
                    Button::new(
                        &format!("qs-edit-add-{}", control_id(*control)),
                        &control_label(*control),
                    )
                    .with_icon(Icon::Plus)
                    .tooltip(t("qs-edit-add-a11y")),
                    &QsEffect::ShowControl {
                        control: control_id(*control).to_owned(),
                    },
                ),
            ));
        }
        page = page.push(Box::new(chips));
    }
    page.push(Box::new(actions.bind_effect_invocation(
        Button::new("qs-edit-reset", &t("qs-edit-reset")).tooltip(t("qs-edit-reset-a11y")),
        &QsEffect::ResetControls,
    )))
    .into_widget()
}
