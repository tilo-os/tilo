//! A settings row, declared: its label, what it explains, and the control
//! that changes the setting — a field of a section, a verb of one, or one
//! of the app's effects. Every pane builds its rows this way, so a row looks,
//! reads to assistive technology and binds the same everywhere.
//!
//! A setting that is a field of a section the pane follows is one call:
//! [`Row::of`] names the field, and the stock control its value wants
//! ([`SettingRow::toggle`], [`SettingRow::choose`], ...) shows the field as
//! it is now and changes it through the section's published field edit.
//! That edit, not the row, is the setting: the palette, agents and scripts
//! change it the same way, and the settings index finds it by the row's id.
//! So a pane is free to draw any setting its own way
//! ([`SettingRow::custom`], or a whole card of its own) without losing
//! either.

use ui::LocalState;
use ui::tilo_resource::{Binds, Model, Path, Settable};
use ui::widget::{Button, Dropdown, Segment, Slider, Toggle, ToggleGroup};

use crate::effects::SettingsEffect;
use crate::host::{Followed, PaneCx, Section};
use crate::widgets::{
    El, Item, choice_row_stacked, field_row_described, setting_row, setting_row_described,
};

/// One labelled row of a settings card ([`PaneCx::row`]).
#[must_use]
pub struct Row<'a> {
    cx: &'a PaneCx,
    label: String,
    description: Option<String>,
    id: Option<String>,
    name: Option<String>,
    stacked: bool,
}

impl PaneCx {
    /// A row labelled `label`.
    pub fn row(&self, label: impl Into<String>) -> Row<'_> {
        Row {
            cx: self,
            label: label.into(),
            description: None,
            id: None,
            name: None,
            stacked: false,
        }
    }
}

impl Row<'_> {
    /// With a helper line under the label.
    pub fn described(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Its control's widget id, as tests, captures and agents find it.
    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Its slider's accessible name, where the slider names itself apart
    /// from the row.
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// With the control on its own line beneath the label, spanning the
    /// section at every width: a slider wants the whole measure.
    pub fn stacked(mut self) -> Self {
        self.stacked = true;
        self
    }

    /// The row around `control`, which the pane bound itself.
    pub fn control(self, control: El) -> Item {
        match (&self.description, self.stacked) {
            (Some(description), true) => field_row_described(&self.label, description, control),
            (None, true) => choice_row_stacked(&self.label, control),
            (Some(description), false) => setting_row_described(&self.label, description, control),
            (None, false) => setting_row(&self.label, control),
        }
    }

    /// A toggle showing `on`, setting the section's field `field`.
    pub fn toggle<T: Section>(self, on: &LocalState<bool>, field: Path<T, bool>) -> Item {
        let toggle = self.cx.bind_field(self.toggle_widget(on), field);
        self.control(Box::new(toggle))
    }

    /// A toggle showing `on`, making each flip the edit `edit` builds of the
    /// section's field `field` (one entry of a map).
    pub fn toggle_edit<T: Section, V: 'static>(
        self,
        on: &LocalState<bool>,
        field: Path<T, V>,
        edit: impl Fn(bool) -> ui::tilo_resource::Edit<T> + Send + Sync + 'static,
    ) -> Item {
        let toggle = self.cx.bind_edit(self.toggle_widget(on), field, edit);
        self.control(Box::new(toggle))
    }

    /// A toggle showing `on`, invoking the section's verb `verb` with each
    /// flip.
    pub fn toggle_verb<T: Section>(self, on: &LocalState<bool>, verb: impl Binds<T, bool>) -> Item {
        let toggle = self.cx.bind_verb(self.toggle_widget(on), verb);
        self.control(Box::new(toggle))
    }

    /// A toggle showing `on`, invoking `effect` with each flip.
    pub fn toggle_effect(
        self,
        on: &LocalState<bool>,
        effect: impl Binds<SettingsEffect, bool>,
    ) -> Item {
        let toggle = self.cx.bind(self.toggle_widget(on), effect);
        self.control(Box::new(toggle))
    }

    /// A chooser of `options` showing `current`, setting the section's field
    /// `field` to what `value` makes of the chosen option.
    pub fn choice<T, V>(
        self,
        options: impl IntoIterator<Item = impl Into<String>>,
        current: &LocalState<String>,
        field: Path<T, V>,
        value: impl Fn(String) -> Result<V, String> + Send + Sync + 'static,
    ) -> Item
    where
        T: Section,
        V: Model + Settable + 'static,
    {
        let options: Vec<String> = options.into_iter().map(Into::into).collect();
        if let Some(segments) = self.segments(&options, current) {
            let control = self
                .cx
                .bind_field_with(segments, field, move |index: usize| {
                    let option = options
                        .get(index)
                        .ok_or_else(|| format!("no choice {index}"))?;
                    value(option.clone())
                });
            return self.control(Box::new(control));
        }
        let chooser = self
            .cx
            .bind_field_with(self.chooser(options, current), field, value);
        self.control(Box::new(chooser))
    }

    /// A chooser of `options` showing `current`, invoking the section's verb
    /// `verb` with the chosen option.
    pub fn choice_verb<T: Section>(
        self,
        options: impl IntoIterator<Item = impl Into<String>>,
        current: &LocalState<String>,
        verb: impl Binds<T, String>,
    ) -> Item {
        let options: Vec<String> = options.into_iter().map(Into::into).collect();
        if let Some(segments) = self.segments(&options, current) {
            let control = self.cx.bind_verb(
                segments,
                Indexed {
                    inner: verb,
                    options,
                },
            );
            return self.control(Box::new(control));
        }
        let chooser = self.cx.bind_verb(self.chooser(options, current), verb);
        self.control(Box::new(chooser))
    }

    /// A chooser of `options` showing `current`, invoking `effect` with the
    /// chosen option.
    pub fn choice_effect(
        self,
        options: impl IntoIterator<Item = impl Into<String>>,
        current: &LocalState<String>,
        effect: impl Binds<SettingsEffect, String>,
    ) -> Item {
        let options: Vec<String> = options.into_iter().map(Into::into).collect();
        if let Some(segments) = self.segments(&options, current) {
            let control = self.cx.bind(
                segments,
                Indexed {
                    inner: effect,
                    options,
                },
            );
            return self.control(Box::new(control));
        }
        let chooser = self.cx.bind(self.chooser(options, current), effect);
        self.control(Box::new(chooser))
    }

    /// A button `id` reading `text`, invoking `effect` when pressed.
    pub fn button(self, id: &str, text: &str, effect: impl Into<SettingsEffect>) -> Item {
        let button = self.cx.bind_invocation(Button::new(id, text), effect);
        self.control(Box::new(button))
    }

    /// A slider showing `value`, setting the section's field `field`.
    pub fn slider<T: Section>(self, value: &LocalState<f64>, field: Path<T, f64>) -> Item {
        let slider = self.cx.bind_field(self.slider_widget(value), field);
        self.slider_row(slider)
    }

    /// A slider showing `value`, setting the section's field `field` to what
    /// `value_of` makes of each committed position.
    pub fn slider_with<T, V>(
        self,
        value: &LocalState<f64>,
        field: Path<T, V>,
        value_of: impl Fn(f64) -> Result<V, String> + Send + Sync + 'static,
    ) -> Item
    where
        T: Section,
        V: Model + Settable + 'static,
    {
        let slider = self
            .cx
            .bind_field_with(self.slider_widget(value), field, value_of);
        self.slider_row(slider)
    }

    /// A slider showing `value`, invoking the section's verb `verb` with each
    /// committed position.
    pub fn slider_verb<T: Section>(
        self,
        value: &LocalState<f64>,
        verb: impl Binds<T, f64>,
    ) -> Item {
        let slider = self.cx.bind_verb(self.slider_widget(value), verb);
        self.slider_row(slider)
    }

    /// A chooser of `choices` (a stable id and the label shown for it), on
    /// `selected`, named by the row's label, setting the section's field
    /// `field` to what `value` makes of the chosen id.
    pub fn keyed_choice<T, V>(
        self,
        choices: Vec<(String, String)>,
        selected: &str,
        field: Path<T, V>,
        value: impl Fn(String) -> Result<V, String> + Send + Sync + 'static,
    ) -> Item
    where
        T: Section,
        V: Model + Settable + 'static,
    {
        let mut chooser = Dropdown::from_state(
            choices.iter().map(|(id, _)| id.clone()).collect(),
            &LocalState::new(selected.to_owned()),
        )
        .with_resolved_accessible_name(&self.label);
        if let Some(id) = &self.id {
            chooser = chooser.with_id(id);
        }
        chooser.set_value_options(choices);
        let chooser = self.cx.bind_field_with(chooser, field, value);
        self.control(Box::new(chooser))
    }

    /// A slider showing `value`, invoking `effect` with each committed value.
    pub fn slider_effect(
        self,
        value: &LocalState<f64>,
        effect: impl Binds<SettingsEffect, f64>,
    ) -> Item {
        let slider = self.cx.bind(self.slider_widget(value), effect);
        self.slider_row(slider)
    }

    /// The row around a bound `slider`: on the trailing edge at one width
    /// for every slider of a page, or spanning the group when stacked.
    fn slider_row(self, slider: Slider) -> Item {
        if self.stacked {
            return self.control(Box::new(slider));
        }
        self.control(crate::widgets::trailing_slider(Box::new(slider)))
    }

    fn toggle_widget(&self, on: &LocalState<bool>) -> Toggle {
        let toggle = Toggle::from_state(on);
        match &self.id {
            Some(id) => toggle.with_id(id),
            None => toggle,
        }
    }

    fn chooser(
        &self,
        options: impl IntoIterator<Item = impl Into<String>>,
        current: &LocalState<String>,
    ) -> Dropdown {
        let placeholder = current.get();
        let chooser = Dropdown::from_state(options.into_iter().map(Into::into).collect(), current)
            .with_placeholder(&placeholder);
        match &self.id {
            Some(id) => chooser.with_id(id),
            None => chooser,
        }
    }

    /// A few short options as segments, every one in sight and one press
    /// away; `None` where a menu reads better (many options, long names).
    fn segments(&self, options: &[String], current: &LocalState<String>) -> Option<ToggleGroup> {
        let letters: usize = options.iter().map(|option| option.chars().count()).sum();
        if !(2..=MAX_SEGMENTS).contains(&options.len()) || letters > MAX_SEGMENT_LETTERS {
            return None;
        }
        let group = ToggleGroup::from_state(
            options
                .iter()
                .map(|option| (option.clone(), Segment::new(option.clone())))
                .collect(),
            current,
        )
        .with_accessible_name(self.label.clone());
        Some(match &self.id {
            Some(id) => group.with_id(id),
            None => group,
        })
    }

    fn slider_widget(&self, value: &LocalState<f64>) -> Slider {
        // Every settings slider says where it is: "72%" beside the track.
        let mut slider = Slider::from_state(value).shows_value();
        if let Some(id) = &self.id {
            slider = slider.with_id(id);
        }
        match &self.name {
            Some(name) => slider.with_accessible_name(name),
            None => slider,
        }
    }
}

impl<'a> Row<'a> {
    /// The row of the field at `path` of `followed`: its control shows the
    /// field and sets it through the section's field edit.
    pub fn of<T: Section, V>(
        self,
        followed: &'a Followed<T>,
        path: Path<T, V>,
    ) -> SettingRow<'a, T, V> {
        SettingRow {
            row: self,
            followed,
            path,
        }
    }
}

/// The row of one field of a followed section ([`Row::of`]).
#[must_use]
pub struct SettingRow<'a, T, V> {
    row: Row<'a>,
    followed: &'a Followed<T>,
    path: Path<T, V>,
}

impl<T: Section> SettingRow<'_, T, bool> {
    /// A toggle.
    pub fn toggle(self) -> Item {
        let on = self.followed.shown(&self.path, bool::clone);
        self.row.toggle(&on, self.path)
    }
}

impl<T: Section> SettingRow<'_, T, f64> {
    /// A slider.
    pub fn slider(self) -> Item {
        let value = self.followed.shown(&self.path, f64::clone);
        self.row.slider(&value, self.path)
    }
}

impl<T, V> SettingRow<'_, T, V>
where
    T: Section,
    V: Model + Settable + 'static,
{
    /// A chooser of `options`, showing the field as `label` names it and
    /// setting it to what `value` makes of the chosen option.
    pub fn choice(
        self,
        options: impl IntoIterator<Item = impl Into<String>>,
        label: impl Fn(&V) -> String + 'static,
        value: impl Fn(String) -> Result<V, String> + Send + Sync + 'static,
    ) -> Item {
        let current = self.followed.shown(&self.path, label);
        self.row.choice(options, &current, self.path, value)
    }

    /// A chooser of `values`, each offered as `label` names it.
    pub fn among(
        self,
        values: impl IntoIterator<Item = V>,
        label: impl Fn(&V) -> String + Clone + Send + Sync + 'static,
    ) -> Item
    where
        V: Clone + Send + Sync,
    {
        let (options, value) = labelled(values, label.clone());
        self.choice(options, label, value)
    }

    /// A menu of `values`, each offered as `label` names it, however few:
    /// for a card of choices that read as one list (formats, units).
    pub fn menu(
        self,
        values: impl IntoIterator<Item = V>,
        label: impl Fn(&V) -> String + Clone + Send + Sync + 'static,
    ) -> Item
    where
        V: Clone + Send + Sync,
    {
        let (options, value) = labelled(values, label.clone());
        let current = self.followed.shown(&self.path, label);
        let menu =
            self.row
                .cx
                .bind_field_with(self.row.chooser(options, &current), self.path, value);
        self.row.control(Box::new(menu))
    }

    /// A chooser of every value the field's type offers, by its label.
    pub fn choose(self) -> Item
    where
        V: settings_store::Labelled + Send + Sync,
    {
        self.among(V::CHOICES.iter().copied(), |choice: &V| {
            settings_store::Labelled::label(*choice).to_owned()
        })
    }

    /// The row around a control of the pane's own: `build` makes it of the
    /// field as `show` makes it, kept current, and binds it to the field
    /// ([`PaneCx::bind_field`]) or a verb that changes it.
    pub fn custom<S>(
        self,
        show: impl Fn(&V) -> S + 'static,
        build: impl FnOnce(&LocalState<S>, Path<T, V>) -> El,
    ) -> Item
    where
        S: Clone + Default + PartialEq + 'static,
    {
        let shown = self.followed.shown(&self.path, show);
        let control = build(&shown, self.path);
        self.row.control(control)
    }
}

/// The labels of `values`, and the value a chosen label stands for.
fn labelled<V: Clone + Send + Sync + 'static>(
    values: impl IntoIterator<Item = V>,
    label: impl Fn(&V) -> String + Send + Sync + 'static,
) -> (
    Vec<String>,
    impl Fn(String) -> Result<V, String> + Send + Sync + 'static,
) {
    let values: Vec<V> = values.into_iter().collect();
    let options = values.iter().map(&label).collect();
    let value = move |chosen: String| {
        values
            .iter()
            .find(|value| label(value) == chosen)
            .cloned()
            .ok_or_else(|| format!("`{chosen}` is not offered"))
    };
    (options, value)
}

/// The most options a row shows as segments rather than a menu.
const MAX_SEGMENTS: usize = 4;
/// The most letters, all options together, that fit as segments beside a
/// row's label.
const MAX_SEGMENT_LETTERS: usize = 32;

/// A binding taking an option's text, bound to the segment index that
/// shows it.
struct Indexed<B> {
    inner: B,
    options: Vec<String>,
}

impl<R, B: Binds<R, String>> Binds<R, usize> for Indexed<B> {
    fn action_id(&self) -> &'static str {
        self.inner.action_id()
    }

    fn arguments(
        &self,
        input: usize,
    ) -> Result<serde_json::Map<String, serde_json::Value>, String> {
        let option = self
            .options
            .get(input)
            .ok_or_else(|| format!("no choice {input}"))?;
        self.inner.arguments(option.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_few_short_options_are_segments_and_many_are_a_menu() {
        let cx = PaneCx::detached(ui::storage::AppStorage::for_app("so.tilo.settings"));
        let current = LocalState::new("Dark".to_owned());
        let few = ["Light", "Dark", "Automatic"].map(String::from);
        assert!(cx.row("Style").segments(&few, &current).is_some());
        let many = ["90%", "100%", "110%", "125%", "150%"].map(String::from);
        assert!(cx.row("Scale").segments(&many, &current).is_none());
        let long = ["Bottom-right corner", "Two-finger press"].map(String::from);
        assert!(cx.row("Click").segments(&long, &current).is_none());
    }
}
