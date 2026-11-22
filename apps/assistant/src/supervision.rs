//! Supervision: what the assistant is, what it runs on and what it spent today. It is for
//! looking: the model is chosen in Settings, and everything else is told to the assistant in
//! the main chat.

use ui::icons::Icon;
use ui::prelude::*;
use ui::widget::{ListRow, Property, PropertyList};

use crate::sources::{Assistant, Read};
use crate::{Hands, Window, page, tr, trf};

pub(crate) fn view(
    cx: &ViewCx<Window>,
    hands: &Hands,
    assistant: &Read<Option<Assistant>>,
) -> Element {
    let assistant = match assistant {
        Read::Here(Some(assistant)) => assistant,
        Read::Here(None) => return crate::no_assistant(cx, hands),
        Read::NotHere => return crate::agents_not_here(),
    };
    let state = ListRow::new(
        "assistant-state",
        trf(
            "assistant-is-on",
            &[("name", assistant.name.clone().into())],
        ),
    )
    .icon(Icon::Bot);
    let model = Property::new(
        tr("assistant-model"),
        assistant
            .model
            .clone()
            .unwrap_or_else(|| tr("assistant-model-from-settings")),
    );
    // The model is chosen in Settings: the row's action is the window's own effect that
    // opens it, so a press, the palette and an agent run the same thing.
    let model = match cx
        .actions()
        .effect(&crate::AssistantEffect::OpenModelSettings)
    {
        Some(open) => model.action_binding(Icon::Settings, tr("assistant-model-change"), open),
        None => model,
    };
    let spent = ui::format::money(assistant.spent_micros);
    let spent = Property::new(
        tr("assistant-spent-today"),
        match assistant.limit_micros {
            Some(limit) => trf(
                "assistant-spent-of-limit",
                &[
                    ("spent", spent.into()),
                    ("limit", ui::format::money(limit).into()),
                ],
            ),
            None => trf("assistant-spent-no-limit", &[("spent", spent.into())]),
        },
    );
    let not_here = || tr("assistant-not-here");
    let learned = Property::new(
        tr("assistant-learned"),
        match hands.reads.learned() {
            Read::NotHere => not_here(),
            Read::Here(learned) => match learned.split_first() {
                None => tr("assistant-learned-none"),
                Some((first, [])) => first.clone(),
                Some((first, rest)) => trf(
                    "assistant-learned-more",
                    &[
                        ("fact", first.clone().into()),
                        ("count", rest.len().to_string().into()),
                    ],
                ),
            },
        },
    );
    page(vec![
        Box::new(state),
        Box::new(
            PropertyList::new(vec![
                model,
                spent,
                Property::new(tr("assistant-connections"), not_here()),
                Property::new(tr("assistant-logins"), not_here()),
                Property::new(tr("assistant-important"), not_here()),
                learned,
            ])
            .with_id("assistant-supervision-facts"),
        ),
    ])
}
