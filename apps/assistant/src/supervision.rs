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
    let state = ListRow::new("assistant-state", assistant.name.clone()).icon(Icon::Bot);
    let model = Property::new(
        tr("assistant-model"),
        assistant
            .model
            .clone()
            .unwrap_or_else(|| tr("assistant-model-from-settings")),
    );
    let settings = cx.actions().bind_effect_invocation(
        Button::new("assistant-model-change", "assistant-model-change")
            .with_icon(Icon::Settings)
            .opening_workspace_item(ui::apps::SETTINGS, &crate::model_settings()),
        &crate::AssistantEffect::OpenModelSettings,
    );
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
    let learned = match hands.reads.learned() {
        Read::NotHere => Property::new(tr("assistant-learned"), tr("assistant-learned-in-chat")),
        Read::Here(learned) => Property::new(
            tr("assistant-learned"),
            match learned.split_first() {
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
        ),
    };
    let nav = hands.nav.clone();
    let review = Button::new("assistant-review-proposals", "assistant-review-proposals")
        .with_icon(Icon::ListChecks)
        .on_view(move || nav.show(&crate::Place::Proposals));
    page(vec![
        Box::new(state),
        hands.chat_button(&assistant.id, &assistant.name),
        Box::new(review),
        Box::new(settings),
        Box::new(
            PropertyList::new(vec![model, spent, learned]).with_id("assistant-supervision-facts"),
        ),
    ])
}
