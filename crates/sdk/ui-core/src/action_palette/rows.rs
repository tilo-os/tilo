//! The rows a host feeds the palette itself, and the argument it collects for
//! one.
//!
//! Rows are data: what the list shows (the shared search row model) and what
//! choosing one needs. A host that searches somewhere the app's published
//! actions cannot reach (the desktop's apps and windows, a launcher's own
//! source) hands them in with [`ActionPalette::set_entries`](crate::ActionPalette::set_entries)
//! and takes the chosen row's id back
//! ([`ActionPalette::take_outcome`](crate::ActionPalette::take_outcome)).

use crate::icons::{self, Icon};
use crate::platform::{PlatformConventions, Shortcut};
pub use crate::search::{ActivationModifier, Alternate, RowFace};
use tilo_actions::{ActionDescriptor, ActionInput, ActionState, FlatAction, InputSpec};

/// What an argument step collects.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum ArgumentKind {
    /// Free text.
    #[default]
    Text,
    /// A password or key: painted as one bullet per character, never put on
    /// the clipboard, and reported to assistive technology as protected with
    /// no value.
    Secret,
    /// A number: Up and Down step it by `step` inside `min..=max`.
    Number {
        min: Option<f64>,
        max: Option<f64>,
        step: f64,
    },
}

impl ArgumentKind {
    /// `value` stepped `direction` times by this kind's step, kept inside its
    /// bounds; `None` when the kind is not a number or the value is not one.
    #[must_use]
    pub fn stepped(self, value: &str, direction: f64) -> Option<String> {
        let Self::Number { min, max, step } = self else {
            return None;
        };
        let current: f64 = value.trim().parse().ok()?;
        let mut next = current + step * direction;
        if let Some(min) = min {
            next = next.max(min);
        }
        if let Some(max) = max {
            next = next.min(max);
        }
        if (next - current).abs() < f64::EPSILON {
            return None;
        }
        Some(format_number(next, step))
    }
}

/// The number `value` written with the decimals `step` needs: an even step
/// keeps none, a tenth keeps one, and so on.
fn format_number(value: f64, step: f64) -> String {
    let mut decimals: usize = 0;
    let mut probe = step.abs().max(f64::EPSILON);
    while decimals < 6 && (probe - probe.round()).abs() > 1e-9 {
        probe *= 10.0;
        decimals = decimals.saturating_add(1);
    }
    format!("{value:.decimals$}")
}

/// What one step of an argument asks: a heading, a field and what it holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArgumentSpec {
    pub prompt: String,
    pub placeholder: String,
    pub prefill: Option<String>,
    /// What the field holds, which decides how it is shown and edited.
    pub kind: ArgumentKind,
}

/// One row a host feeds the palette: what it reads as, and what choosing it
/// needs.
#[derive(Clone)]
pub struct CommandEntry {
    pub id: String,
    pub label: String,
    /// Rendered under the label. `None` → label only.
    pub description: Option<String>,
    /// Short status shown as a trailing pill. Unlike a keybinding it is
    /// descriptive and announced as part of the row.
    pub badge: Option<String>,
    /// Rendered right-aligned in the row. Purely informational — the palette
    /// never binds it.
    pub keybinding: Option<String>,
    /// Preferred semantic form of a shortcut. When present, it is rendered
    /// with the host platform's native command modifier and notation.
    pub shortcut: Option<Shortcut>,
    /// The one value choosing this row collects in the field first.
    pub argument: Option<ArgumentSpec>,
    /// Consecutive rows sharing a group render under one header; `None` joins
    /// whatever section precedes it.
    pub group: Option<String>,
    /// Real row count for this group before any host-side cap, so a capped
    /// section reports what it's hiding instead of the painted count.
    pub group_total: Option<usize>,
    pub icon: Option<Icon>,
    /// The application's own artwork, painted in place of `icon` when it
    /// decodes.
    pub image: Option<icons::AppImage>,
    /// A person as `(name, key)`: their avatar in place of `icon`, its
    /// initials from the name and its colour from the key.
    pub avatar: Option<(String, String)>,
    /// The people the row is about, as a stack of faces at its trailing end.
    pub faces: Vec<RowFace>,
    /// What choosing the row does, shown at its trailing edge ("Join",
    /// "Open"), for a row whose outcome is not its title.
    pub outcome: Option<String>,
    /// The section the row is in ("Happening now"), above its group: a header
    /// of its own, and a named group to assistive technology.
    pub section: Option<String>,
    /// Other ways to activate the row, each behind a held modifier. The
    /// selected row shows the one whose modifier is down, or the first as a
    /// hint; the host decides what each does.
    pub alternates: Vec<Alternate>,
    /// The description is a path, so it loses its middle rather than its end
    /// when it doesn't fit. Prose loses its end.
    pub description_is_path: bool,
    /// The description as separate facts, drawn as runs of their own with
    /// space between them ([`Self::facts`]). Empty: one run.
    pub description_facts: Vec<String>,
    /// Its value steps with ←/→ while the row is selected and the query's
    /// caret is at its end ([`ActionPalette::take_adjustment`]): a volume, a
    /// timeout, a mode.
    ///
    /// [`ActionPalette::take_adjustment`]: crate::ActionPalette::take_adjustment
    pub adjustable: bool,
}

impl std::fmt::Debug for CommandEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandEntry")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("description", &self.description)
            .field("badge", &self.badge)
            .field("keybinding", &self.keybinding)
            .field("argument", &self.argument)
            .field("group", &self.group)
            .field("adjustable", &self.adjustable)
            .finish_non_exhaustive()
    }
}

impl CommandEntry {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            description: None,
            badge: None,
            keybinding: None,
            shortcut: None,
            argument: None,
            group: None,
            group_total: None,
            icon: None,
            image: None,
            avatar: None,
            faces: Vec::new(),
            outcome: None,
            section: None,
            alternates: Vec::new(),
            description_is_path: false,
            description_facts: Vec::new(),
            adjustable: false,
        }
    }

    /// Let ←/→ step the row's value ([`Self::adjustable`]).
    #[must_use]
    pub fn adjustable(mut self) -> Self {
        self.adjustable = true;
        self
    }

    /// Offer another activation behind `modifier` ([`Alternate`]).
    #[must_use]
    pub fn alternate(mut self, modifier: ActivationModifier, label: impl Into<String>) -> Self {
        self.alternates.push(Alternate {
            modifier,
            label: label.into(),
        });
        self
    }

    /// Show the description as separate facts ([`Self::description_facts`]).
    #[must_use]
    pub fn facts(mut self, facts: Vec<String>) -> Self {
        if !facts.is_empty() {
            self.description = Some(facts.join(", "));
        }
        self.description_facts = facts;
        self
    }

    /// Mark the description as a path ([`Self::description_is_path`]).
    #[must_use]
    pub fn path_description(mut self) -> Self {
        self.description_is_path = true;
        self
    }

    #[must_use]
    pub fn description(mut self, d: impl Into<String>) -> Self {
        self.description = Some(d.into());
        self
    }

    #[must_use]
    pub fn badge(mut self, badge: impl Into<String>) -> Self {
        self.badge = Some(badge.into());
        self
    }

    #[must_use]
    pub fn group(mut self, g: impl Into<String>) -> Self {
        self.group = Some(g.into());
        self
    }

    #[must_use]
    pub fn group_total(mut self, total: usize) -> Self {
        self.group_total = Some(total);
        self
    }

    #[must_use]
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    #[must_use]
    pub fn image(mut self, image: icons::AppImage) -> Self {
        self.image = Some(image);
        self
    }

    /// Show the row as a person ([`Self::avatar`]).
    #[must_use]
    pub fn avatar(mut self, name: impl Into<String>, key: impl Into<String>) -> Self {
        self.avatar = Some((name.into(), key.into()));
        self
    }

    /// Show the people the row is about as faces at its end ([`Self::faces`]).
    #[must_use]
    pub fn faces(mut self, faces: Vec<RowFace>) -> Self {
        self.faces = faces;
        self
    }

    /// What choosing the row does, at its trailing edge.
    #[must_use]
    pub fn outcome(mut self, outcome: impl Into<String>) -> Self {
        self.outcome = Some(outcome.into());
        self
    }

    /// Put the row in section `section` ([`Self::section`]).
    #[must_use]
    pub fn in_section(mut self, section: impl Into<String>) -> Self {
        self.section = Some(section.into());
        self
    }

    /// The row for one exported action of a resource, as inert data: its
    /// label, and what it acts on or means under it
    /// (`tilo_actions::listed_row`), never "Feeds — New folder".
    #[must_use]
    pub fn from_flat_action(fa: &FlatAction<'_>) -> Self {
        let row = fa.row();
        let mut entry = Self::new(fa.key.clone(), row.title);
        if let Some(subtitle) = row.subtitle {
            entry = entry.description(subtitle);
        }
        if let ActionInput::Prompted {
            prompt,
            placeholder,
            ..
        } = fa.action.input()
        {
            entry = entry.with_argument(prompt.to_owned(), placeholder.to_owned());
        }
        entry.show_state(&fa.action.state);
        entry
    }

    /// The row for one published action, as inert data: its label, what it
    /// says of itself, the state it shows and the one line it asks for. The
    /// host runs it by the action's own address when it is chosen.
    #[must_use]
    pub fn from_descriptor(key: impl Into<String>, action: &ActionDescriptor) -> Self {
        let mut entry = Self::new(key, action.action_label.clone());
        if let Some(description) = &action.description {
            entry = entry.description(description.clone());
        }
        if let InputSpec::Text {
            prompt,
            placeholder,
            ..
        } = &action.input
        {
            entry = entry.with_argument(prompt.clone(), placeholder.clone());
        }
        entry.show_state(&action.state);
        entry
    }

    /// Show what an action's row shows of the state it acts on.
    fn show_state(&mut self, state: &ActionState) {
        self.badge.clone_from(&state.badge);
        self.keybinding.clone_from(&state.shortcut);
    }

    #[must_use]
    pub fn keybinding(mut self, k: impl Into<String>) -> Self {
        self.keybinding = Some(k.into());
        self
    }

    /// Adds a platform-formatted shortcut hint. Prefer this to
    /// [`Self::keybinding`] for commands using the native command modifier.
    #[must_use]
    pub fn shortcut(mut self, shortcut: Shortcut) -> Self {
        self.shortcut = Some(shortcut);
        self
    }

    /// Collect `prompt`'s value before choosing the row.
    #[must_use]
    pub fn with_argument(
        mut self,
        prompt: impl Into<String>,
        placeholder: impl Into<String>,
    ) -> Self {
        let (prefill, kind) = self
            .argument
            .take()
            .map_or((None, ArgumentKind::Text), |spec| (spec.prefill, spec.kind));
        self.argument = Some(ArgumentSpec {
            prompt: prompt.into(),
            placeholder: placeholder.into(),
            prefill,
            kind,
        });
        self
    }

    /// Prefill is fully selected when the argument step opens, so the first
    /// keystroke replaces it.
    #[must_use]
    pub fn with_argument_prefill(mut self, prefill: impl Into<String>) -> Self {
        let prefill = prefill.into();
        match &mut self.argument {
            Some(spec) => spec.prefill = Some(prefill),
            None => {
                self.argument = Some(ArgumentSpec {
                    prefill: Some(prefill),
                    ..ArgumentSpec::default()
                });
            }
        }
        self
    }

    /// The row already carries the value it runs with, so choosing it asks
    /// for none: a search hit an app's `open` names its item itself.
    #[must_use]
    pub fn with_preset(mut self) -> Self {
        self.argument = None;
        self
    }

    /// The row's shortcut in `platform`'s notation, else its free-text
    /// keybinding.
    #[must_use]
    pub fn keybinding_label(&self, platform: PlatformConventions) -> Option<String> {
        self.shortcut
            .as_ref()
            .map(|shortcut| shortcut.label(platform))
            .or_else(|| self.keycap_labels(platform).map(|caps| caps.join("+")))
    }

    /// Its shortcut's keycap legends in `platform`'s notation, else its
    /// free-text keybinding split into one cap per `+`-separated part, each in
    /// the legends of the active keyboard layout.
    pub(crate) fn keycap_labels(&self, platform: PlatformConventions) -> Option<Vec<String>> {
        self.shortcut
            .as_ref()
            .map(|shortcut| shortcut.keycap_labels(platform))
            .or_else(|| {
                let keycaps = crate::keycaps::active();
                self.keybinding.as_ref().map(|binding| {
                    binding
                        .split('+')
                        .map(str::trim)
                        .filter(|part| !part.is_empty())
                        .map(|part| keycaps.token(part))
                        .collect()
                })
            })
    }
}

/// The result of pressing Enter on a row: its `id` plus the free-text
/// `argument` (only `Some` for rows that asked for one). Drained via
/// [`ActionPalette::take_outcome`](crate::ActionPalette::take_outcome).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandOutcome {
    pub id: String,
    pub argument: Option<String>,
}
