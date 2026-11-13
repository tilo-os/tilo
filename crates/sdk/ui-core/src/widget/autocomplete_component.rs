//! [`Autocomplete`] as a described component.

use crate::component::{Bound, Component, Described, Event, Person};

use super::{Autocomplete, RowRole, Suggestion, SuggestionPerson};
use crate::widget::TextInput;

crate::component::part_slot!(SuggestionProps);

/// A field that suggests completions under it as it is typed into: a search over a site's
/// catalogue, a mention, a command. The suggestions are what the view gives it; a chosen one
/// fills the field and is reported.
#[derive(Component)]
#[component(name = "Autocomplete", role = "ComboBox", label = "placeholder")]
pub struct AutocompleteProps {
    /// The text typed, and where the edit of it goes. Taking a suggestion writes its value here
    /// and reports it.
    #[component(bind(merge = "splice"))]
    pub value: Bound<String>,
    /// The completions to offer, in order.
    #[component(slot(default))]
    pub suggestions: Vec<SuggestionProps>,
    /// What to type, shown while it is empty, and what names it to assistive technology.
    pub placeholder: Option<String>,
    /// How many suggestion rows show before the list scrolls.
    pub max_visible: Option<i64>,
    /// Whether it can be typed into now.
    pub enabled: bool,
    /// The person took a suggestion, or the text they typed: carries what the field holds now.
    #[component(event(value = String))]
    pub accept: Event,
    /// The person dismissed the list, keeping nothing.
    #[component(event)]
    pub cancel: Event,
}

impl Default for AutocompleteProps {
    fn default() -> Self {
        Self {
            value: Bound::default(),
            suggestions: Vec::new(),
            placeholder: None,
            max_visible: None,
            enabled: true,
            accept: Event::None,
            cancel: Event::None,
        }
    }
}

/// One completion an [`Autocomplete`] offers.
#[derive(Clone, Component, Default)]
#[component(name = "Suggestion", role = "ListItem", label = "label")]
pub struct SuggestionProps {
    /// What taking it puts in the field.
    #[component(required, positional)]
    #[component(data)]
    pub value: String,
    /// What its row says; the value unless said.
    pub label: Option<String>,
    /// Dim text at its trailing end: a kind, a count.
    pub detail: Option<String>,
    /// A dim line under the label: a page's address under its title.
    pub subtitle: Option<String>,
    /// A glyph before it.
    pub icon: Option<crate::icons::Icon>,
    /// The person it names, shown as their avatar.
    pub who: Option<Person>,
}

impl From<SuggestionProps> for Suggestion {
    fn from(props: SuggestionProps) -> Self {
        let mut suggestion = match props.label {
            Some(label) => Suggestion::labelled(props.value, label),
            None => Suggestion::new(props.value),
        };
        if let Some(detail) = props.detail {
            suggestion = suggestion.with_detail(detail);
        }
        if let Some(subtitle) = props.subtitle {
            suggestion = suggestion.with_subtitle(subtitle);
        }
        if let Some(icon) = props.icon {
            suggestion = suggestion.with_icon(icon);
        }
        if let Some(who) = props.who {
            suggestion = suggestion.with_person(SuggestionPerson {
                identity: who.identity,
                hue: crate::tokens::PresenceHue::of_identity(&who.name),
                agent: false,
            });
        }
        suggestion.role = RowRole::Item;
        suggestion
    }
}

impl Described for Autocomplete {
    type Props = AutocompleteProps;

    fn from_props(key: &str, props: AutocompleteProps) -> Self {
        let AutocompleteProps {
            value,
            suggestions,
            placeholder,
            max_visible,
            enabled,
            accept,
            cancel: _,
        } = props;
        let _ = enabled;
        let edits = value.clone();
        let mut field =
            Self::with_input(TextInput::from_state(&value.state).with_id(&format!("{key}.field")))
                .with_id(key);
        if let Some(placeholder) = placeholder {
            field = field.with_placeholder(&placeholder);
        }
        if let Some(max_visible) = max_visible {
            field = field.with_max_visible(usize::try_from(max_visible.max(1)).unwrap_or(1));
        }
        field.set_suggestions(suggestions.into_iter().map(Suggestion::from).collect());
        // Each edit of the field, typed or taken from a suggestion, is reported when the host
        // polls what the field holds.
        field.on_edit(edits, accept)
    }

    /// The text typed and the suggestions change in place.
    fn set_props(&mut self, props: AutocompleteProps) -> bool {
        let text = props.value.state.get();
        if text != self.text() {
            self.set_text(&text);
        }
        self.set_suggestions(
            props
                .suggestions
                .into_iter()
                .map(Suggestion::from)
                .collect(),
        );
        true
    }
}
