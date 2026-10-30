//! The catalogue of providers to add, and a provider's models to choose
//! from: two lists the person narrows with the framework's in-content
//! filter ([`ui::widget::search_field::Filtered`]) and scrolls inside the
//! page.
//!
//! A row of the catalogue is a provider: its name, what it is, how it signs
//! in, and its documentation. Choosing it adds a profile of that provider
//! (`add-provider`), whose form opens. Adding the same one again adds a
//! second account.
//!
//! A row of the models is one model the provider listed, with what it can
//! do: its context, whether it sees images and whether it calls tools.
//! Choosing it makes it the provider's model (`set-provider-model`).

use agent_provider_config::presets::{Authentication, Preset};
use agent_provider_config::{EndpointLocation, ModelDescriptor, ProviderProfile};
use ui::LocalState;
use ui::widget::search_field::Filtered;
use ui::widget::{
    Button, ChipStyle, Column, Filterable, Link, ListRow, Row, Scrollable, Tone, When, Widget as _,
};

use crate::i18n::{t, t_args};
use crate::widgets::El;
use crate::{PanelBindings, ProviderEffect};

/// How tall a list stands in the page before it scrolls: about seven rows.
const LIST_HEIGHT: f32 = 420.0;

/// Whether every word of `query` is in one of `texts`, as a filter matches.
fn matches(query: &str, texts: &[&str]) -> bool {
    let haystack = texts.join(" ").to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|word| haystack.contains(word))
}

/// The entries the catalogue offers in this host: a sign-in runs only where
/// the host offers it.
fn offered(include_sign_in: bool) -> impl Iterator<Item = Preset> {
    Preset::ALL
        .iter()
        .copied()
        .filter(move |preset| include_sign_in || !preset.signs_in_only())
}

/// How a preset's profile signs in, as its row says it.
fn method(preset: Preset) -> String {
    match preset.authentication() {
        Authentication::None => t("agent-preset-method-none"),
        Authentication::SignIn(issuer) => t_args(
            "agent-preset-method-sign-in",
            &[("issuer", issuer.label().into())],
        ),
        Authentication::ApiKey if preset.location() != EndpointLocation::Hosted => {
            t("agent-preset-method-own-server")
        }
        Authentication::ApiKey => t("agent-preset-method-key"),
    }
}

/// Whether a preset stays in the list whatever the filter says: the
/// generic entries, for a provider the catalogue does not know.
fn always_offered(preset: Preset) -> bool {
    preset.endpoint_editable()
        && preset.location() == EndpointLocation::Hosted
        && matches!(preset.slug(), "openai-compatible" | "anthropic-compatible")
}

/// What a filter matches a preset by.
fn preset_words(preset: Preset) -> Vec<String> {
    [
        preset.label(),
        preset.description(),
        preset.slug(),
        preset.protocol().label(),
        &method(preset),
    ]
    .map(str::to_owned)
    .to_vec()
}

/// One preset's row: what it is, how it signs in, its documentation and
/// the button that adds it.
fn preset_row<H: PanelBindings>(cx: &H, enabled: bool, preset: Preset) -> El {
    let add = Button::new(
        &format!("agent-models-preset-{}", preset.slug()),
        &t("agent-preset-add"),
    )
    .with_accessible_name(&t_args(
        "agent-preset-add-named",
        &[("provider", preset.label().into())],
    ))
    .with_enabled(enabled);
    let slug = preset.slug().to_owned();
    let add = cx.bind(
        add,
        ProviderEffect::ADD_PROVIDER.map(move |()| slug.clone()),
    );
    let trailing = Row::new()
        .no_wrap()
        .with_align_items(taffy::AlignItems::Center)
        .push(Box::new(
            Link::new(preset.docs()).with_label(&t("agent-preset-docs")),
        ))
        .push(crate::widgets::gap(ui::tokens::SpacingStep::S3))
        .push(Box::new(add));
    Box::new(
        ListRow::new(
            format!("agent-models-preset-row-{}", preset.slug()),
            preset.label(),
        )
        .secondary(format!("{}\n{}", preset.description(), method(preset)))
        .secondary_lines(2)
        .trailing(Box::new(trailing)),
    )
}

/// One entry of a narrowed list: its row, the words a filter matches it
/// by, and whether it stays whatever the filter says.
pub(crate) struct Entry {
    row: El,
    words: Vec<String>,
    always: bool,
}

impl Entry {
    pub(crate) fn new(row: El, words: Vec<String>) -> Self {
        Self {
            row,
            words,
            always: false,
        }
    }
}

/// Whether an entry matching `words` shows under `query`.
fn shows(query: &str, words: &[String], always: bool) -> bool {
    always || matches(query, &words.iter().map(String::as_str).collect::<Vec<_>>())
}

/// A list narrowed by the filter above it. Its rows are built, and their
/// controls bound, once with the page; the filter shows or hides them.
struct Narrowed {
    query: LocalState<String>,
    rows: Column,
    words: Vec<(Vec<String>, bool)>,
}

impl Filterable for Narrowed {
    fn set_filter(&mut self, query: &str) {
        if self.query.with(|shown| shown != query) {
            self.query.set(query.to_owned());
        }
    }

    fn filter_count(&self) -> Option<ui::widget::FilterCount> {
        let shown = self.query.with(|query| {
            self.words
                .iter()
                .filter(|(words, always)| shows(query, words, *always))
                .count()
        });
        Some(ui::widget::FilterCount::new(shown, self.words.len()))
    }
}

impl ui::widget::Decorator for Narrowed {
    type Inner = Column;

    fn inner(&self) -> &Column {
        &self.rows
    }

    fn inner_mut(&mut self) -> &mut Column {
        &mut self.rows
    }

    ui::provides!(dyn Filterable; inner = rows);

    fn name(&self) -> &'static str {
        "Narrowed"
    }
}

/// `entries` under a filter named `placeholder`, in a scrolling region of
/// their own. The filter starts empty with the page.
fn narrowed(id: &str, placeholder: &str, entries: Vec<Entry>) -> El {
    let query = LocalState::new(String::new());
    let mut rows = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(ui::tokens::Rhythm::Tight);
    let mut words = Vec::new();
    for entry in entries {
        let (matched, always) = (entry.words.clone(), entry.always);
        words.push((entry.words, entry.always));
        rows = rows.push(Box::new(When::from_state(
            &query,
            move |query: &String| shows(query, &matched, always),
            entry.row,
        )));
    }
    let list = Narrowed { query, rows, words };
    let mut filtered = Filtered::new(Box::new(
        Scrollable::new_vertical()
            .with_max_height(LIST_HEIGHT)
            .with_edge_fade()
            .with_semantic_key(format!("{id}-list"))
            .with_child(Box::new(list)),
    ))
    .with_placeholder(placeholder);
    let field = std::mem::replace(filtered.field_mut(), ui::widget::SearchField::new());
    *filtered.field_mut() = field.with_id(id);
    // A filtered view fills the region it stands in; here it stands in a
    // page, as tall as its field and list.
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(filtered)),
    )
}

/// The catalogue: every provider this host can add, narrowed by its filter.
/// Built while the host lends its bindings, so each Add is bound.
pub(crate) fn presets<H: PanelBindings>(cx: &H, enabled: bool) -> El {
    let entries = offered(false)
        .map(|preset| Entry {
            row: preset_row(cx, enabled, preset),
            words: preset_words(preset),
            always: always_offered(preset),
        })
        .collect();
    narrowed(
        "agent-models-add-provider",
        &t("agent-add-provider-filter"),
        entries,
    )
}

/// What a model can do, as tags after its name.
fn capability_tags(model: &ModelDescriptor) -> Vec<(String, ChipStyle)> {
    let mut tags = Vec::new();
    if model.vision {
        tags.push((t("agent-model-vision"), ChipStyle::Neutral));
    }
    if model.tool_calling {
        tags.push((t("agent-model-tools"), ChipStyle::Neutral));
    } else {
        tags.push((
            t("agent-model-no-tools-tag"),
            ChipStyle::Tinted(Tone::Warning),
        ));
    }
    tags
}

/// The second line of a model's row: its id where its name differs, and
/// its context.
fn model_details(model: &ModelDescriptor) -> Option<String> {
    let context = model.context_window.map(|tokens| {
        t_args(
            "agent-model-context",
            &[(
                "tokens",
                ui::format::compact_count(u64::from(tokens)).into(),
            )],
        )
    });
    let id = (model.display_label() != model.id).then(|| model.id.clone());
    match (id, context) {
        (None, None) => None,
        (Some(text), None) | (None, Some(text)) => Some(text),
        (Some(id), Some(context)) => Some(format!("{id}\n{context}")),
    }
}

/// One model's row: its name, id and context, and what it can do.
pub(crate) fn model_row(id: String, model: &ModelDescriptor, selected: bool) -> ListRow {
    let tags = capability_tags(model);
    let borrowed: Vec<(&str, ChipStyle)> = tags
        .iter()
        .map(|(label, style)| (label.as_str(), *style))
        .collect();
    let mut row = ListRow::new(id, model.display_label())
        .tags(&borrowed)
        .selected(selected);
    if let Some(details) = model_details(model) {
        let lines = u8::try_from(details.lines().count()).unwrap_or(1);
        row = row.secondary(details).secondary_lines(lines);
    }
    row
}

/// The models `profile` listed, narrowed by their filter, each with the
/// button `choose` binds to make it the provider's model.
pub(crate) fn models(
    profile: &ProviderProfile,
    listed: &[ModelDescriptor],
    choose: impl Fn(Button, String) -> Button,
) -> El {
    let chosen = profile.default_model.as_deref().unwrap_or_default();
    let entries = listed
        .iter()
        .map(|model| {
            let id = format!(
                "agent-models-model-{}-{}",
                profile.id,
                hex::encode(&model.id)
            );
            let taken = model.id == chosen;
            let mut row = model_row(id.clone(), model, taken);
            if !taken {
                let button = Button::new(&format!("{id}-use"), &t("agent-model-use"))
                    .with_accessible_name(&t_args(
                        "agent-model-use-named",
                        &[("model", model.display_label().into())],
                    ));
                row = row.trailing(Box::new(choose(button, model.id.clone())));
            }
            Entry::new(
                Box::new(row),
                vec![model.id.clone(), model.display_label().to_owned()],
            )
        })
        .collect();
    narrowed(
        &format!("agent-models-models-{}", profile.id),
        &t("agent-models-filter"),
        entries,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_filter_matches_every_word_anywhere() {
        assert!(matches(
            "anthropic glm",
            &["Z.ai (GLM), Anthropic API", "GLM models"]
        ));
        assert!(matches("", &["anything"]));
        assert!(!matches("kimi", &["Z.ai (GLM)"]));
    }

    #[test]
    fn the_generic_entries_stay_whatever_the_filter_says() {
        let kept: Vec<_> = offered(false)
            .filter(|preset| always_offered(*preset))
            .map(Preset::slug)
            .collect();
        assert_eq!(kept, ["openai-compatible", "anthropic-compatible"]);
        assert!(offered(false).all(|preset| !preset.signs_in_only()));
        assert!(offered(true).any(Preset::signs_in_only));
    }
}
