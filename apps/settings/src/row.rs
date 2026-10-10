//! A settings row, declared: its label, what it explains, and the control
//! that changes the setting — a field of a section, a verb of one, or one
//! of the app's effects. Every pane builds its rows this way, so a row looks,
//! reads to assistive technology and binds the same everywhere.

use ui::LocalState;
use ui::tilo_resource::{Binds, Model, Path, Settable};
use ui::widget::{Dropdown, Slider, Toggle};

use crate::effects::SettingsEffect;
use crate::host::{PaneCx, Section};
use crate::widgets::{El, setting_row, setting_row_described, value_row};

/// One labelled row of a settings card ([`PaneCx::row`]).
#[must_use]
pub struct Row<'a> {
    cx: &'a PaneCx,
    label: String,
    description: Option<String>,
    id: Option<String>,
    name: Option<String>,
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

    /// The row around `control`, which the pane bound itself.
    pub fn control(self, control: El) -> El {
        match &self.description {
            Some(description) => setting_row_described(&self.label, description, control),
            None => setting_row(&self.label, control),
        }
    }

    /// The row showing `value`, read only.
    pub fn value(self, value: &str) -> El {
        value_row(&self.label, value)
    }

    /// A toggle showing `on`, setting the section's field `field`.
    pub fn toggle<T: Section>(self, on: &LocalState<bool>, field: Path<T, bool>) -> El {
        let toggle = self.cx.bind_field(self.toggle_widget(on), field);
        self.control(Box::new(toggle))
    }

    /// A toggle showing `on`, setting the section's optional field `field`
    /// (unset follows a shared choice) to each flip.
    pub fn toggle_some<T: Section>(
        self,
        on: &LocalState<bool>,
        field: Path<T, Option<bool>>,
    ) -> El {
        let toggle = self
            .cx
            .bind_field_with(self.toggle_widget(on), field, |on| Ok(Some(on)));
        self.control(Box::new(toggle))
    }

    /// A toggle showing `on`, making each flip the edit `edit` builds of the
    /// section's field `field` (one entry of a map).
    pub fn toggle_edit<T: Section, V: 'static>(
        self,
        on: &LocalState<bool>,
        field: Path<T, V>,
        edit: impl Fn(bool) -> ui::tilo_resource::Edit<T> + Send + Sync + 'static,
    ) -> El {
        let toggle = self.cx.bind_edit(self.toggle_widget(on), field, edit);
        self.control(Box::new(toggle))
    }

    /// A toggle showing `on`, invoking `effect` with each flip.
    pub fn toggle_effect(
        self,
        on: &LocalState<bool>,
        effect: impl Binds<SettingsEffect, bool>,
    ) -> El {
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
    ) -> El
    where
        T: Section,
        V: Model + Settable + 'static,
    {
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
    ) -> El {
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
    ) -> El {
        let chooser = self.cx.bind(self.chooser(options, current), effect);
        self.control(Box::new(chooser))
    }

    /// A slider showing `value`, setting the section's field `field`.
    pub fn slider<T: Section>(self, value: &LocalState<f64>, field: Path<T, f64>) -> El {
        let slider = self.cx.bind_field(self.slider_widget(value), field);
        self.control(Box::new(slider))
    }

    /// A slider showing `value`, setting the section's field `field` to what
    /// `value_of` makes of each committed position.
    pub fn slider_with<T, V>(
        self,
        value: &LocalState<f64>,
        field: Path<T, V>,
        value_of: impl Fn(f64) -> Result<V, String> + Send + Sync + 'static,
    ) -> El
    where
        T: Section,
        V: Model + Settable + 'static,
    {
        let slider = self
            .cx
            .bind_field_with(self.slider_widget(value), field, value_of);
        self.control(Box::new(slider))
    }

    /// A slider showing `value`, invoking the section's verb `verb` with each
    /// committed position.
    pub fn slider_verb<T: Section>(self, value: &LocalState<f64>, verb: impl Binds<T, f64>) -> El {
        let slider = self.cx.bind_verb(self.slider_widget(value), verb);
        self.control(Box::new(slider))
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
    ) -> El
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
    ) -> El {
        let slider = self.cx.bind(self.slider_widget(value), effect);
        self.control(Box::new(slider))
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

    fn slider_widget(&self, value: &LocalState<f64>) -> Slider {
        let mut slider = Slider::from_state(value);
        if let Some(id) = &self.id {
            slider = slider.with_id(id);
        }
        match &self.name {
            Some(name) => slider.with_accessible_name(name),
            None => slider,
        }
    }
}
