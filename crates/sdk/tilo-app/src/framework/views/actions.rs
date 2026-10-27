//! Actions as the UI: a control is an action of the app rendered as a
//! control, so searching the app's actions searches its real controls.
//!
//! An app says how each of its actions looks as a control
//! ([`ControlDecl`]: a button or a switch and its state, its icon, and
//! where in the app the control lives). A query view lists the actions that
//! match in a small trailing group, "In this app" ([`action_hits`]), each as
//! that same control, bound to the same action ([`actions_group`]), and
//! offers to take the person to where it lives.

use std::rc::Rc;

use tilo_actions::ActionSpec;
use tilo_resource::Resource;
use tilo_search::{FieldWeight, ListIndex};
use ui_core::icons::Icon;
use ui_core::widget::{Button, ButtonRole, Column, ControlSize, Element, Row, Text, Toggle};

use super::super::Actions;

/// How an action looks as a control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlKind {
    /// A button that runs it.
    Button,
    /// A switch, on or off now, that runs it when flipped.
    Switch { on: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlDecl {
    /// The id of the published action or effect.
    pub action: String,
    pub kind: ControlKind,
    pub icon: Option<Icon>,
    /// The control's own name where it lives ("Shuffle"), which a person
    /// knows it by, when it differs from the action's label ("Toggle
    /// shuffle").
    pub label: Option<String>,
    /// Where the control lives, in the app's own words (a designed view's
    /// id): a hit offers to go there.
    pub home: Option<String>,
}

impl ControlDecl {
    #[must_use]
    pub fn button(action: impl Into<String>) -> Self {
        Self {
            action: action.into(),
            kind: ControlKind::Button,
            icon: None,
            label: None,
            home: None,
        }
    }

    #[must_use]
    pub fn switch(action: impl Into<String>, on: bool) -> Self {
        Self {
            kind: ControlKind::Switch { on },
            ..Self::button(action)
        }
    }

    #[must_use]
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// The control is called `label` where it lives.
    #[must_use]
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// The control lives in the view `home`.
    #[must_use]
    pub fn home(mut self, home: impl Into<String>) -> Self {
        self.home = Some(home.into());
        self
    }
}

/// An action that matches a query, as the control it is.
#[derive(Clone)]
pub struct ActionHit {
    pub spec: ActionSpec,
    pub control: ControlDecl,
}

impl ActionHit {
    /// What the hit reads: the control's own name, else the action's label.
    #[must_use]
    pub fn label(&self) -> &str {
        self.control.label.as_deref().unwrap_or(&self.spec.label)
    }
}

/// Whether a person can run `spec` from a control right now: offered to
/// people, available, a verb of its own, and asking for nothing more unless
/// the app declares the control (`declared`), which asks itself (New
/// playlist asks for its name).
fn controllable(spec: &ActionSpec, declared: bool) -> bool {
    spec.audience().reaches_people()
        && matches!(
            spec.contract.availability,
            tilo_actions::Availability::Available
        )
        && !spec.carries_actions()
        && (declared || !spec.params().iter().any(|param| param.required))
}

/// The actions of `specs` that match `query`, best first, at most `limit`:
/// each as its declaration in `controls` says, a button where none does.
/// Only actions a control can run are listed: those asking for nothing
/// more, and those whose declared control asks for it.
#[must_use]
pub fn action_hits(
    specs: &[ActionSpec],
    controls: &[ControlDecl],
    query: &str,
    limit: usize,
) -> Vec<ActionHit> {
    if query.trim().is_empty() || limit == 0 {
        return Vec::new();
    }
    let offered: Vec<&ActionSpec> = specs
        .iter()
        .filter(|spec| controllable(spec, controls.iter().any(|decl| decl.action == spec.id)))
        .collect();
    let index = ListIndex::default();
    let ranks = index.ranks(
        query,
        offered.iter().map(|spec| {
            let mut fields = vec![(spec.label.as_str(), FieldWeight::TITLE)];
            if let Some(label) = controls
                .iter()
                .find(|decl| decl.action == spec.id)
                .and_then(|decl| decl.label.as_deref())
            {
                fields.push((label, FieldWeight::KEYWORD));
            }
            if let Some(description) = &spec.description {
                fields.push((description.as_str(), FieldWeight::BODY));
            }
            fields
        }),
    );
    let mut found: Vec<(i32, &ActionSpec)> = offered
        .into_iter()
        .zip(ranks)
        .filter_map(|(spec, rank)| rank.map(|rank| (rank, spec)))
        .collect();
    found
        .sort_by(|(a, a_spec), (b, b_spec)| b.cmp(a).then_with(|| a_spec.label.cmp(&b_spec.label)));
    let mut seen = std::collections::HashSet::new();
    found
        .into_iter()
        .filter(|(_, spec)| seen.insert(spec.id.clone()))
        .take(limit)
        .map(|(_, spec)| ActionHit {
            spec: spec.clone(),
            control: controls
                .iter()
                .find(|decl| decl.action == spec.id)
                .cloned()
                .unwrap_or_else(|| ControlDecl::button(spec.id.clone())),
        })
        .collect()
}

/// The control `hit` is, bound to its action through `actions`; an action
/// the resource no longer publishes shows disabled.
#[must_use]
pub fn action_control<R: Resource>(actions: &Actions<R>, hit: &ActionHit) -> Element {
    let label = hit.label();
    let id = format!("view-action:{}", hit.spec.id);
    match hit.control.kind {
        ControlKind::Button => {
            let mut button = Button::new(&id, label)
                .role(ButtonRole::Secondary)
                .control_size(ControlSize::Small);
            if let Some(icon) = hit.control.icon {
                button = button.with_icon(icon);
            }
            match actions.spec_binding(&hit.spec) {
                Some(binding) => Box::new(button.on_action(binding)),
                None => Box::new(button.with_enabled(false)),
            }
        }
        ControlKind::Switch { on } => {
            let mut toggle = Toggle::new(&id, "").with_accessible_name(label);
            toggle.set_checked(on);
            let control: Element = match actions.spec_switch_binding(&hit.spec) {
                Some(binding) => Box::new(toggle.on_action(binding)),
                None => Box::new(toggle.with_enabled(false)),
            };
            Box::new(
                Row::new()
                    .with_gap(8.0)
                    .with_align_items(ui_core::taffy::AlignItems::Center)
                    .push(control)
                    .push(Box::new(Text::new(label))),
            )
        }
    }
}

/// The trailing "In this app" group of a query view: its heading, then each
/// hit as its real control, with a button that takes the person to where the
/// control lives (`show`, given the control's home) for those that have
/// one. `heading` and `show_label` are the app's words for them.
#[must_use]
pub fn actions_group<R: Resource>(
    actions: &Actions<R>,
    hits: &[ActionHit],
    heading: &str,
    show_label: &str,
    show: &Rc<dyn Fn(&str)>,
) -> Element {
    let mut column = Column::new().with_no_padding().with_gap(4.0).push(Box::new(
        Text::new(heading).with_role(ui_core::tokens::TextRole::Heading),
    ));
    for hit in hits {
        let mut row = Row::new()
            .with_gap(8.0)
            .with_align_items(ui_core::taffy::AlignItems::Center)
            .push(action_control(actions, hit));
        if let Some(home) = hit.control.home.clone() {
            let show = Rc::clone(show);
            row = row.push(Box::new(
                Button::new(&format!("view-action-show:{}", hit.spec.id), show_label)
                    .role(ButtonRole::Plain)
                    .control_size(ControlSize::Small)
                    .with_accessible_name(&format!("{show_label}: {}", hit.label()))
                    .on_click(move || show(&home)),
            ));
        }
        column = column.push(Box::new(row));
    }
    Box::new(column)
}
