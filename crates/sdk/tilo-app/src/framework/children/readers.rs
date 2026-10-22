//! Choosing who may read a child the room keeps to its readers, and saying
//! who does: the one control every app with such children shows.
//!
//! The room's members are offered by name, the participant choosing always
//! among the readers and the room's owner reading it anyway; agents are
//! named by id, since an agent reads a kept record only when it is named
//! itself, whoever it acts for. The control's button is the container's
//! `take`, naming them: the room keeps the record to them, then the item
//! moves into it. While the room has not answered, the control says so; a
//! refusal is told beneath the button.

use tilo_resource::Id;
use ui_core::LocalState;
use ui_core::i18n::I18nManager;
use ui_core::permissions::{TreePath, policy};
use ui_core::widget::{Button, Checkbox, Column, Element, Text, TextInput};

use super::{ChildRecord, Children, ChildrenState};
use crate::framework::People;

impl<C: ChildRecord> ChildrenState<C> {
    /// The room's members besides its owner, who reads everything in it,
    /// as the policy here names them.
    fn members(&self) -> Vec<String> {
        let Some(permissions) = &self.room.permissions else {
            return Vec::new();
        };
        let current = permissions.current();
        let room = &current.standing().policy;
        room.members
            .keys()
            .filter(|member| room.owner.as_ref() != Some(*member))
            .cloned()
            .collect()
    }

    /// Who the room keeps child `id` to, when it keeps it to some.
    fn readers_of(&self, id: &Id) -> Option<Vec<policy::Subject>> {
        let permissions = self.room.permissions.as_ref()?;
        let record = Self::record(id).ok()?;
        let current = permissions.current();
        let standing = current.standing();
        let node = standing.node(
            &TreePath::root()
                .app(self.room.app_id)
                .resource(record.route()),
        );
        standing
            .policy
            .grants
            .iter()
            .find_map(|grant| match &grant.subject {
                policy::Subject::AllBut(readers)
                    if grant.node == node && grant.answer == policy::Answer::Deny =>
                {
                    Some(readers.clone())
                }
                _ => None,
            })
    }
}

impl<C: ChildRecord> Children<C> {
    /// Who the room keeps child `id` to, when it keeps it to some.
    #[must_use]
    pub fn readers(&self, id: &Id) -> Option<Vec<policy::Subject>> {
        self.state.readers_of(id)
    }

    /// Who reads child `id`, by name: the people first, then the agents.
    #[must_use]
    pub fn reader_names(&self, people: &People, id: &Id) -> Option<Vec<String>> {
        let readers = self.readers(id)?;
        let mut names: Vec<String> = readers
            .iter()
            .filter_map(|reader| match reader {
                policy::Subject::Person(person) => Some(people.name(person)),
                _ => None,
            })
            .collect();
        names.extend(readers.iter().filter_map(|reader| match reader {
            policy::Subject::Agent(agent) => Some(agent.clone()),
            _ => None,
        }));
        Some(names)
    }

    /// The control that takes the parent's item `item` into a child kept
    /// to the readers chosen in it ([`Self::take`]): the room's members by
    /// name, agents by id, and the button, which reads `label`. `key` names
    /// its controls.
    #[must_use]
    pub fn readers_chooser(&self, people: &People, item: &Id, key: &str, label: &str) -> Element {
        let mut column = Column::new()
            .with_rhythm(ui_core::tokens::Rhythm::Related)
            .push(Box::new(
                Text::new(&I18nManager::lookup_plain("readers-heading"))
                    .with_role(ui_core::tokens::TextRole::Subheading),
            ));
        let mut chosen = Vec::new();
        for member in self.state.members() {
            if member == self.me {
                continue;
            }
            let checked = LocalState::new(false);
            column = column.push(Box::new(
                Checkbox::from_state(&checked)
                    .with_id(&format!("{key}-reader-{member}"))
                    .with_label(people.name(&member)),
            ));
            chosen.push((member, checked));
        }
        let agents = LocalState::new(String::new());
        column = column.push(Box::new(
            TextInput::from_state(&agents)
                .with_id(&format!("{key}-reader-agents"))
                .with_placeholder("readers-agents"),
        ));
        let (children, take, entered) = (self.clone(), item.clone(), agents);
        column = column.push(Box::new(
            Button::new(&format!("{key}-take"), label).on_click(move || {
                let mut readers: Vec<policy::Subject> = chosen
                    .iter()
                    .filter(|(_, checked)| checked.get())
                    .map(|(member, _)| policy::Subject::person(member))
                    .collect();
                readers.extend(
                    entered
                        .get()
                        .split(|c: char| c == ',' || c.is_whitespace())
                        .map(|agent| agent.trim().trim_start_matches("agent:"))
                        .filter(|agent| !agent.is_empty())
                        .map(policy::Subject::agent),
                );
                if let Err(error) = children.take(&take, Some(readers)) {
                    children
                        .state
                        .keeping
                        .refused()
                        .set(Some(error.to_string()));
                }
            }),
        ));
        let (waiting, refused) = ((self.clone(), item.clone()), self.refused());
        column = column.push(Box::new(
            Text::dynamic(move || {
                if waiting.0.keeping(&waiting.1) {
                    I18nManager::lookup_plain("readers-waiting")
                } else {
                    refused.get().unwrap_or_default()
                }
            })
            .with_id(format!("{key}-take-status"))
            .with_role(ui_core::tokens::TextRole::Caption),
        ));
        Box::new(column)
    }
}
