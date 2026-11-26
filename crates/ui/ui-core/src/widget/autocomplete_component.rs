//! [`Autocomplete`] as a described component.

use tilo_ui_base::{
    component::Bound, component::Component, component::Described, component::Event,
    component::Person,
};

use super::{Autocomplete, RowRole, Suggestion};
use crate::widget::TextInput;

tilo_ui_base::part_type!(SuggestionProps);

/// A field that suggests completions under it as it is typed into: a search over a site's catalogue, a
/// mention, a command.
#[derive(Component)]
#[component(name = "Autocomplete", role = "ComboBox", label = "placeholder")]
pub struct AutocompleteProps {
    /// The text typed, and where the edit of it goes.
    #[component(bind(merge = "splice"))]
    pub value: Bound<String>,
    /// The completions to offer, in order.
    #[component(part(default))]
    pub suggestions: Vec<SuggestionProps>,
    /// What to type, shown while it is empty, and what names it to assistive technology.
    pub placeholder: Option<String>,
    /// How many suggestion rows show before the list scrolls.
    pub max_visible: Option<i64>,
    /// Whether it can be typed into now.
    pub enabled: bool,
    /// It takes the keyboard as soon as it is in the window, once: a palette that opens to be typed
    /// into.
    pub autofocus: bool,
    /// The rows stand open whenever there are any, instead of opening as the person types or walks
    /// them: a palette whose empty field already lists everything.
    pub open_list: bool,
    /// Tab and Shift+Tab move the highlight to the next group's first row, instead of completing
    /// the field to the highlighted row: a palette whose rows are grouped.
    pub tab_groups: bool,
    /// The rows narrow to what is typed. Off, the rows are shown as they are offered: a host that
    /// already computed them for what is typed, or whose ranking is fuzzier than a substring.
    pub filter: bool,
    /// The kinds of result the rows can be narrowed to, in the order their chips stand over them
    /// (a kind's name, as the rows are grouped by it). Empty offers no chips.
    pub kinds: Vec<String>,
    /// The kinds whose chips are chosen. Empty narrows nothing.
    pub chosen: Vec<String>,
    /// A line shown under the field while there are no rows ("Searching…", "Nothing matches").
    pub status: Option<String>,
    /// The field's text after each edit: what the host reads as the person types. Carries the
    /// text alone; taking a row or the text is [`Self::accept`].
    #[component(event(value = String))]
    pub change: Event,
    /// The person took a suggestion, or the text they typed: carries what the field holds now.
    #[component(event(value = String))]
    pub accept: Event,
    /// The person took a row with a modifier held: the row's value, and the way they opened it
    /// (`here` for Shift, `beside` for Ctrl, `more` for Alt), as `"<way>\t<value>"`.
    #[component(event(value = String))]
    pub alternate: Event,
    /// The person pressed the chip of a kind: carries the kind's name.
    #[component(event(value = String))]
    pub filter_kind: Event,
    /// The person stepped the selected row's value: the step (`1` up, `-1` down) and the
    /// row's value, as `"<delta>\t<value>"`. Left and Right step a row its host marks
    /// adjustable while it is selected.
    #[component(event(value = String))]
    pub adjust: Event,
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
            autofocus: false,
            open_list: false,
            tab_groups: false,
            filter: true,
            kinds: Vec::new(),
            chosen: Vec::new(),
            status: None,
            change: Event::None,
            accept: Event::None,
            alternate: Event::None,
            filter_kind: Event::None,
            adjust: Event::None,
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
    /// The row is the heading of the rows under it, never armed or chosen: a group's title.
    pub header: bool,
    /// Dim text at its trailing end: a kind, a count.
    pub detail: Option<String>,
    /// A dim line under the label: a page's address under its title.
    pub subtitle: Option<String>,
    /// A glyph before it.
    pub icon: Option<tilo_ui_base::icons::Icon>,
    /// The person it names, shown as their avatar.
    pub who: Option<Person>,
    /// Its value can be stepped in place (a level, a volume): Left and Right on the selected
    /// row report the step through the field's `adjust`.
    pub adjustable: bool,
}

impl From<SuggestionProps> for Suggestion {
    fn from(props: SuggestionProps) -> Self {
        if props.header {
            let mut heading =
                Suggestion::heading(props.label.clone().unwrap_or_else(|| props.value.clone()));
            heading.value = props.value;
            return heading;
        }
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
            suggestion = suggestion.with_person(who);
        }
        suggestion.adjustable = props.adjustable;
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
            autofocus,
            open_list,
            tab_groups,
            filter,
            kinds,
            chosen,
            status,
            change,
            accept,
            alternate,
            filter_kind,
            adjust,
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
        field.set_open_list(open_list);
        field.set_tab_groups(tab_groups);
        field.set_filter(filter);
        field.set_kinds(kinds, chosen);
        field.set_status(status);
        field.on_alternate(alternate);
        field.on_filter(filter_kind);
        field.on_adjust(adjust);
        field.on_change(change);
        if autofocus {
            field.request_focus();
        }
        field.set_suggestions(suggestions.into_iter().map(Suggestion::from).collect());
        field.open_if_listed();
        // Each edit of the field, typed or taken from a suggestion, is reported when the host
        // polls what the field holds.
        field.on_edit(edits, accept)
    }

    fn set_props(&mut self, props: AutocompleteProps) -> bool {
        let text = props.value.state.get();
        if text != self.text() {
            self.set_text(&text);
        }
        self.set_open_list(props.open_list);
        self.set_tab_groups(props.tab_groups);
        self.set_filter(props.filter);
        self.set_kinds(props.kinds, props.chosen);
        self.set_status(props.status);
        self.on_alternate(props.alternate);
        self.on_filter(props.filter_kind);
        self.on_adjust(props.adjust);
        self.on_change(props.change);
        self.set_suggestions(
            props
                .suggestions
                .into_iter()
                .map(Suggestion::from)
                .collect(),
        );
        self.open_if_listed();
        true
    }
}
