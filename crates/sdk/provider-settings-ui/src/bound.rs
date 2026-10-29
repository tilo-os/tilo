//! Settings rows in a view, bound to the record the view part shows: a
//! settings section held with `cx.record(&section, |cx| ..)`, or any other
//! record whose fields are settings.
//!
//! A row is its label, an optional explanation and the control that changes
//! the setting. A setting that is a field of the record is one call:
//! `cx.setting(label).id(id).toggle(paths.field)` shows the field as it is
//! now, follows every change of it, and changes it through the record's
//! published field edit. That edit, not the row, is the setting: the
//! palette, agents and scripts change it the same way, and the settings
//! index finds the row by its control's id.
//!
//! The rows are the page vocabulary's ([`crate::widgets`]): a row built
//! here sits in a group beside any other.

use ui::LocalState;
use ui::framework::{ActionControl, Actions, OtherRecord, ViewCx};
use ui::tilo_resource::{Binds, Effect, Model, Path, Resource, Settable};
use ui::widget::{
    Button, Dropdown, Picture, PictureChoice, PictureOption, Segment, Slider, Toggle, ToggleGroup,
};

use crate::widgets::{
    El, Item, ItemShape, choice_row_stacked, field_row_described, nested_setting_row, setting_row,
    setting_row_described, trailing_slider,
};

/// The rows of a view over a record `R`.
pub trait SettingRows<R: Resource> {
    /// A row labelled `label`.
    fn setting(&self, label: impl Into<String>) -> Setting<'_, R>;
}

impl<R: Resource> SettingRows<R> for ViewCx<R> {
    fn setting(&self, label: impl Into<String>) -> Setting<'_, R> {
        Setting {
            cx: self,
            label: label.into(),
            description: None,
            id: None,
            name: None,
            stacked: false,
            nested: false,
            enabled: true,
        }
    }
}

/// One labelled row of a settings group ([`SettingRows::setting`]).
#[must_use]
pub struct Setting<'a, R: Resource> {
    cx: &'a ViewCx<R>,
    label: String,
    description: Option<String>,
    id: Option<String>,
    name: Option<String>,
    stacked: bool,
    nested: bool,
    enabled: bool,
}

impl<R: Resource<State = R> + Model> Setting<'_, R> {
    /// With a helper line under the label.
    pub fn described(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Its control's widget id, as the settings index, tests, captures and
    /// agents find it.
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
    /// group at every width: a slider wants the whole measure.
    pub fn stacked(mut self) -> Self {
        self.stacked = true;
        self
    }

    /// Indented under the row above it: an option that refines the one
    /// above and that the view shows only while that one is on.
    pub fn nested(mut self) -> Self {
        self.nested = true;
        self
    }

    /// With its control disabled unless `enabled`: a setting that only
    /// matters while another is on. The row's description says why.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// The row around `control`, which the view bound itself.
    pub fn control(self, control: El) -> Item {
        if self.nested && !self.stacked {
            return nested_setting_row(&self.label, self.description.as_deref(), control);
        }
        match (&self.description, self.stacked) {
            (Some(description), true) => field_row_described(&self.label, description, control),
            (None, true) => choice_row_stacked(&self.label, control),
            (Some(description), false) => setting_row_described(&self.label, description, control),
            (None, false) => setting_row(&self.label, control),
        }
    }

    /// A toggle for the flag at `field`.
    pub fn toggle(self, field: Path<R, bool>) -> Item {
        let on = self.cx.value_at(&field);
        let toggle = bound(
            self.toggle_widget(&on),
            self.cx.actions().field_binding(field),
        );
        self.control(Box::new(toggle))
    }

    /// A toggle showing what `read` makes of the record, invoking `verb`
    /// with each flip.
    pub fn toggle_verb(
        self,
        read: impl Fn(&R) -> bool + 'static,
        verb: impl Binds<R, bool>,
    ) -> Item {
        let on = self.cx.value(read);
        let toggle = self.cx.actions().bind(self.toggle_widget(&on), verb);
        self.control(Box::new(toggle))
    }

    /// A slider for the number at `field`.
    pub fn slider(self, field: Path<R, f64>) -> Item {
        let value = self.cx.value_at(&field);
        let slider = bound(
            self.slider_widget(&value),
            self.cx.actions().field_binding(field),
        );
        self.slider_row(slider)
    }

    /// A slider showing what `read` makes of the record, setting the field
    /// at `field` to what `value_of` makes of each committed position.
    pub fn slider_with<V>(
        self,
        read: impl Fn(&R) -> f64 + 'static,
        field: Path<R, V>,
        value_of: impl Fn(f64) -> Result<V, String> + Send + Sync + 'static,
    ) -> Item
    where
        V: Model + Settable + 'static,
    {
        let value = self.cx.value(read);
        let slider = bound(
            self.slider_widget(&value),
            self.cx.actions().set_binding(field, value_of),
        );
        self.slider_row(slider)
    }

    /// A slider showing what `read` makes of the record, invoking `verb`
    /// with each committed position.
    pub fn slider_verb(self, read: impl Fn(&R) -> f64 + 'static, verb: impl Binds<R, f64>) -> Item {
        let value = self.cx.value(read);
        let slider = self.cx.actions().bind(self.slider_widget(&value), verb);
        self.slider_row(slider)
    }

    /// A chooser of `values` for the field at `field`, each offered as
    /// `label` names it: a few short ones as segments, else a menu.
    pub fn among<V>(
        self,
        field: Path<R, V>,
        values: impl IntoIterator<Item = V>,
        label: impl Fn(&V) -> String + Clone + Send + Sync + 'static,
    ) -> Item
    where
        V: Model + Settable + Clone + Send + Sync + 'static,
    {
        let (options, value) = labelled(values, label.clone());
        let current = self.shown(&field, label);
        if let Some(segments) = self.segments(&options, &current) {
            let binding = self.cx.actions().set_binding(field, move |index: usize| {
                let option = options
                    .get(index)
                    .ok_or_else(|| format!("no choice {index}"))?;
                value(option.clone())
            });
            let segments = bound(segments, binding);
            return self.control(Box::new(segments));
        }
        let binding = self.cx.actions().set_binding(field, value);
        let chooser = bound(self.chooser(options, &current), binding);
        self.control(Box::new(chooser))
    }

    /// A menu of `values` for the field at `field`, each offered as `label`
    /// names it, however few: for a group of choices that read as one list
    /// (formats, units).
    pub fn menu<V>(
        self,
        field: Path<R, V>,
        values: impl IntoIterator<Item = V>,
        label: impl Fn(&V) -> String + Clone + Send + Sync + 'static,
    ) -> Item
    where
        V: Model + Settable + Clone + Send + Sync + 'static,
    {
        let (options, value) = labelled(values, label.clone());
        let current = self.shown(&field, label);
        let binding = self.cx.actions().set_binding(field, value);
        let menu = bound(self.chooser(options, &current), binding);
        self.control(Box::new(menu))
    }

    /// A chooser of `choices` (a stable key and the label shown for it) for
    /// the field at `field`, showing the key `key` reads of the record, and
    /// setting the field to what `value` makes of the chosen key.
    pub fn keyed<V>(
        self,
        field: Path<R, V>,
        choices: Vec<(String, String)>,
        key: impl Fn(&R) -> String + 'static,
        value: impl Fn(String) -> Result<V, String> + Send + Sync + 'static,
    ) -> Item
    where
        V: Model + Settable + 'static,
    {
        let selected = self.cx.value(key);
        let mut chooser = Dropdown::from_state(
            choices.iter().map(|(key, _)| key.clone()).collect(),
            &selected,
        )
        .with_resolved_accessible_name(&self.label)
        .with_enabled(self.enabled);
        if let Some(id) = &self.id {
            chooser = chooser.with_id(id);
        }
        chooser.set_value_options(choices);
        let chooser = bound(chooser, self.cx.actions().set_binding(field, value));
        self.control(Box::new(chooser))
    }

    /// A chooser of `options` showing what `read` makes of the record,
    /// invoking `verb` with the chosen option.
    pub fn choice_verb(
        self,
        options: impl IntoIterator<Item = impl Into<String>>,
        read: impl Fn(&R) -> String + 'static,
        verb: impl Binds<R, String>,
    ) -> Item {
        let options: Vec<String> = options.into_iter().map(Into::into).collect();
        let current = self.cx.value(read);
        if let Some(segments) = self.segments(&options, &current) {
            let segments = self.cx.actions().bind(
                segments,
                Indexed {
                    inner: verb,
                    options,
                },
            );
            return self.control(Box::new(segments));
        }
        let chooser = self
            .cx
            .actions()
            .bind(self.chooser(options, &current), verb);
        self.control(Box::new(chooser))
    }

    /// A chooser of `options` showing `current`, invoking the effect
    /// `effect` of the app `actions` publishes with the chosen option: a
    /// choice of the device's (a power profile) rather than of the record.
    pub fn choice_effect<A: Resource, E: Effect>(
        self,
        options: impl IntoIterator<Item = impl Into<String>>,
        current: &LocalState<String>,
        actions: &Actions<A>,
        effect: impl Binds<E, String>,
    ) -> Item {
        let options: Vec<String> = options.into_iter().map(Into::into).collect();
        if let Some(segments) = self.segments(&options, current) {
            let segments = actions.bind_effect(
                segments,
                Indexed {
                    inner: effect,
                    options,
                },
            );
            return self.control(Box::new(segments));
        }
        let chooser = actions.bind_effect(self.chooser(options, current), effect);
        self.control(Box::new(chooser))
    }

    /// A toggle showing `on`, invoking the effect `effect` of the app
    /// `actions` publishes with each flip.
    pub fn toggle_effect<A: Resource, E: Effect>(
        self,
        on: &LocalState<bool>,
        actions: &Actions<A>,
        effect: impl Binds<E, bool>,
    ) -> Item {
        let toggle = actions.bind_effect(self.toggle_widget(on), effect);
        self.control(Box::new(toggle))
    }

    /// A slider showing `value`, invoking the effect `effect` of the app
    /// `actions` publishes with each committed position.
    pub fn slider_effect<A: Resource, E: Effect>(
        self,
        value: &LocalState<f64>,
        actions: &Actions<A>,
        effect: impl Binds<E, f64>,
    ) -> Item {
        let slider = actions.bind_effect(self.slider_widget(value), effect);
        self.slider_row(slider)
    }

    /// A choice of `values` for the field at `field`, each a tile showing
    /// `picture` of it over its name as `label` gives it, on the line under
    /// the row's label: for a setting whose meaning is visual (a text size,
    /// a colour filter), where a word would make the person imagine it.
    pub fn pictures<V>(
        mut self,
        field: Path<R, V>,
        values: impl IntoIterator<Item = V>,
        label: impl Fn(&V) -> String + Clone + Send + Sync + 'static,
        picture: impl Fn(&V) -> Picture,
    ) -> Item
    where
        V: Model + Settable + Clone + Send + Sync + 'static,
    {
        let values: Vec<V> = values.into_iter().collect();
        let options = values
            .iter()
            .map(|value| {
                let name = label(value);
                (name.clone(), PictureOption::new(name, picture(value)))
            })
            .collect();
        let current = self.shown(&field, label);
        let mut picker = PictureChoice::from_state(options, &current)
            .with_accessible_name(self.label.clone())
            .with_enabled(self.enabled);
        if let Some(id) = &self.id {
            picker = picker.with_id(id);
        }
        let binding = self.cx.actions().set_binding(field, move |index: usize| {
            values
                .get(index)
                .cloned()
                .ok_or_else(|| format!("no choice {index}"))
        });
        let picker = bound(picker, binding);
        self.stacked = true;
        self.control(Box::new(picker))
    }

    /// The row around a control of the view's own: `build` makes it showing
    /// what `read` makes of the record, kept current, and binds it.
    pub fn custom<S>(
        self,
        read: impl Fn(&R) -> S + 'static,
        build: impl FnOnce(&LocalState<S>) -> El,
    ) -> Item
    where
        S: Clone + Default + PartialEq + 'static,
    {
        let shown = self.cx.value(read);
        let control = build(&shown);
        self.control(control)
    }

    /// A button reading `text`, which the caller binds (an effect of the
    /// app, a verb of the record), its id the row's.
    pub fn button(self, text: &str, bind: impl FnOnce(Button) -> Button) -> Item {
        let id = self.id.clone().unwrap_or_else(|| self.label.clone());
        let button = bind(Button::new(&id, text).with_enabled(self.enabled));
        self.control(Box::new(button))
    }

    /// What `label` makes of the field at `field`, kept current.
    fn shown<V: 'static>(
        &self,
        field: &Path<R, V>,
        label: impl Fn(&V) -> String + 'static,
    ) -> LocalState<String> {
        let field = field.clone();
        self.cx
            .value(move |record| field.get(record).map(&label).unwrap_or_default())
    }

    /// `item`, a row built over another record than this one (`record`), as
    /// a part of the view over that record.
    pub fn of_record<P: Resource>(
        &self,
        record: &impl OtherRecord<P>,
        item: impl FnOnce(&ViewCx<P>) -> Item,
    ) -> Option<Item> {
        record_item(self.cx, record, item)
    }

    /// The row around a bound `slider`: on the trailing edge at one width
    /// for every slider of a page, or spanning the group when stacked.
    fn slider_row(self, slider: Slider) -> Item {
        if self.stacked {
            return self.control(Box::new(slider));
        }
        self.control(trailing_slider(Box::new(slider)))
    }

    fn toggle_widget(&self, on: &LocalState<bool>) -> Toggle {
        let toggle = Toggle::from_state(on).with_enabled(self.enabled);
        match &self.id {
            Some(id) => toggle.with_id(id),
            None => toggle,
        }
    }

    fn chooser(&self, options: Vec<String>, current: &LocalState<String>) -> Dropdown {
        let placeholder = current.get();
        let chooser = Dropdown::from_state(options, current)
            .with_placeholder(&placeholder)
            .with_enabled(self.enabled);
        match &self.id {
            Some(id) => chooser.with_id(id),
            None => chooser,
        }
    }

    /// A few short options as segments, every one in sight and one press
    /// away; `None` where a menu reads better (many options, long names).
    fn segments(&self, options: &[String], current: &LocalState<String>) -> Option<ToggleGroup> {
        if !as_segments(options) {
            return None;
        }
        let group = ToggleGroup::from_state(
            options
                .iter()
                .map(|option| (option.clone(), Segment::new(option.clone())))
                .collect(),
            current,
        )
        .with_accessible_name(self.label.clone())
        .with_enabled(self.enabled);
        Some(match &self.id {
            Some(id) => group.with_id(id),
            None => group,
        })
    }

    fn slider_widget(&self, value: &LocalState<f64>) -> Slider {
        // Every settings slider says where it is: "72%" beside the track.
        let mut slider = Slider::from_state(value)
            .shows_value()
            .with_enabled(self.enabled);
        if let Some(id) = &self.id {
            slider = slider.with_id(id);
        }
        match &self.name {
            Some(name) => slider.with_accessible_name(name),
            None => slider,
        }
    }
}

/// `item`, a row built over `record` (a settings section, the person's own
/// record), as a part of the view `cx` over that record: its controls bind
/// that record's fields and verbs, and it follows every change of it.
/// `None` where the record cannot be held (a view of a past version).
pub fn record_item<R: Resource, T: 'static, P: Resource>(
    cx: &ViewCx<R, T>,
    record: &impl OtherRecord<P>,
    item: impl FnOnce(&ViewCx<P>) -> Item,
) -> Option<Item> {
    let shape = std::cell::Cell::new(ItemShape::Row);
    let element = cx.record(record, |cx| {
        let (built, element) = item(cx).into_parts();
        shape.set(built);
        element
    })?;
    Some(Item::from_parts(shape.get(), element))
}

/// `control` bound to `binding`, or disabled where the record does not
/// publish it.
fn bound<I, C: ActionControl<I>>(
    control: C,
    binding: Option<ui::tilo_actions::ActionBinding<I>>,
) -> C {
    match binding {
        Some(binding) => control.bind_action(binding),
        None => control.unavailable(),
    }
}

/// The most options a row shows as segments rather than a menu.
const MAX_SEGMENTS: usize = 4;
/// The most letters, all options together, that fit as segments beside a
/// row's label.
const MAX_SEGMENT_LETTERS: usize = 32;

/// Whether `options` read best as segments: a few short ones.
#[must_use]
pub fn as_segments(options: &[String]) -> bool {
    let letters: usize = options.iter().map(|option| option.chars().count()).sum();
    (2..=MAX_SEGMENTS).contains(&options.len()) && letters <= MAX_SEGMENT_LETTERS
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
    use super::as_segments;

    #[test]
    fn a_few_short_options_are_segments_and_many_are_a_menu() {
        let few = ["Light", "Dark", "Automatic"].map(String::from);
        assert!(as_segments(&few));
        let many = ["90%", "100%", "110%", "125%", "150%"].map(String::from);
        assert!(!as_segments(&many));
        let long = ["Bottom-right corner", "Two-finger press"].map(String::from);
        assert!(!as_segments(&long));
    }
}
