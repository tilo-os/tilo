//! The People tab: how many share the workspace and where its ownership
//! stands, with the one ownership action the person can take, then a row
//! per person whose role and Remove move under the name where the two do
//! not fit beside it.
//!
//! A role is chosen from the role button's menu, each role with what it
//! allows. Removing someone asks first, with the way to their activity
//! beside it ([`Decision`]).

use ui::icons::Icon;
use ui::tokens::Rhythm;
use ui::widget::{
    Button, ButtonRole, ChipStyle, Column, ControlSize, Element, EmptyState, ListRow, MenuItem,
    Row, Section,
};

use super::general::{button, caption};
use super::{Confirm, Parts, Settings, WorkspaceSettingsEffect};

/// What a person's row offers the viewer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Offers {
    /// Nothing: the owner's role is a fact of the row, not a choice.
    #[default]
    Nothing,
    /// Their role, but not Remove: no one removes themselves here.
    Role,
    RoleAndRemove,
}

/// One person of the list, in the viewer's words.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Person {
    /// Who they are: what their role and removal are asked for by.
    pub key: String,
    pub name: String,
    pub you: bool,
    /// Their role, as the row says it.
    pub role: String,
    /// Their role's key among [`People::roles`], where it is one of them.
    pub role_key: Option<String>,
    /// Their extra permissions, only when they have any.
    pub extra: Option<String>,
    pub offers: Offers,
    /// Whether the viewer may change their role and remove them.
    pub can_manage: bool,
}

/// A role a person can be given, with what it allows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoleChoice {
    pub key: String,
    pub label: String,
    pub allows: String,
}

/// A question the sheet asks before it changes something that cannot be
/// taken back with a press: keep things as they are, look at what was
/// done first, or go ahead.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Decision {
    pub title: String,
    pub description: String,
    pub keep: String,
    pub review: String,
    pub confirm: String,
}

/// Everything the People tab shows, in the person's words.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct People {
    pub count: String,
    /// Where ownership stands, or what removing someone does.
    pub status: String,
    /// The ownership action the viewer can take, by what it does.
    pub ownership: Option<String>,
    pub header: String,
    pub you_tag: String,
    pub remove: String,
    pub people: Vec<Person>,
    /// The roles a person's role button offers.
    pub roles: Vec<RoleChoice>,
    /// What an empty list says instead: its title and detail.
    pub empty: (String, String),
    pub note: Option<String>,
    /// The question removing someone asks.
    pub remove_decision: Decision,
}

/// The People tab of `settings`.
pub(super) fn pane(settings: &Settings, parts: &Parts) -> Element {
    let people = &settings.people;
    let mut summary = vec![caption(&people.status)];
    if let Some(label) = &people.ownership {
        summary.push(Box::new(
            Row::new()
                .with_rhythm(Rhythm::Related)
                .align_start()
                .push(Box::new(button(
                    parts,
                    "ws-people-ownership",
                    label,
                    || WorkspaceSettingsEffect::Ownership,
                ))),
        ));
    }
    let mut groups: Vec<Element> = vec![Box::new(Section::titled(people.count.clone(), summary))];
    if people.people.is_empty() {
        let (title, detail) = &people.empty;
        groups.push(Box::new(
            EmptyState::new(title.clone())
                .with_icon(Icon::Shield)
                .with_message(detail.clone())
                .inline(),
        ));
    } else {
        groups.push(Box::new(Section::titled(
            people.header.clone(),
            people
                .people
                .iter()
                .map(|person| row(people, person, parts))
                .collect(),
        )));
    }
    if let Some(note) = &people.note {
        groups.push(caption(note));
    }
    Box::new(
        groups.into_iter().fold(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_rhythm(Rhythm::Section),
            Column::push,
        ),
    )
}

/// One person's row: their role and Remove trail the name where both fit
/// and wrap under it where they do not, so the name is never cut short.
fn row(people: &People, person: &Person, parts: &Parts) -> Element {
    let mut row = ListRow::new(
        format!("ws-people-person-{}", person.key),
        person.name.clone(),
    )
    .two_line_title()
    .avatar(person.name.clone(), person.key.clone());
    if person.you {
        row = row.tags(&[(people.you_tag.as_str(), ChipStyle::Neutral)]);
    }
    if let Some(extra) = &person.extra {
        row = row.secondary(extra.clone());
    }
    if person.offers == Offers::Nothing {
        return Box::new(row.value(person.role.clone()));
    }
    let roles = people
        .roles
        .iter()
        .map(|choice| {
            let press = parts.clone();
            let effect = WorkspaceSettingsEffect::SetRole {
                member: person.key.clone(),
                role: choice.key.clone(),
            };
            MenuItem::action(choice.label.clone(), move || press.ask(effect.clone()))
                .with_detail(choice.allows.clone())
                .with_checked(person.role_key.as_deref() == Some(choice.key.as_str()))
        })
        .collect();
    let mut controls = Row::new()
        .with_rhythm(Rhythm::Related)
        .align_start()
        .push(Box::new(
            Button::new(&format!("ws-people-role-{}", person.key), &person.role)
                .control_size(ControlSize::Small)
                .with_enabled(person.can_manage)
                .with_menu(roles),
        ));
    if person.offers == Offers::RoleAndRemove {
        let confirm = parts.clone();
        let (member, name) = (person.key.clone(), person.name.clone());
        controls = controls.push(Box::new(
            Button::new(&format!("ws-people-remove-{}", person.key), &people.remove)
                .role(ButtonRole::Destructive)
                .control_size(ControlSize::Small)
                .with_enabled(person.can_manage)
                .on_click(move || {
                    confirm.confirm(Confirm::RemoveMember {
                        member: member.clone(),
                        name: name.clone(),
                    });
                }),
        ));
    }
    Box::new(row.trailing(Box::new(controls)))
}

/// The roles a person is given here, for captures.
fn sample_roles() -> Vec<RoleChoice> {
    [
        ("viewer", "Viewer", "Can see everything, change nothing"),
        ("commenter", "Commenter", "Can see and comment"),
        ("editor", "Editor", "Can see, comment and change things"),
    ]
    .into_iter()
    .map(|(key, label, allows)| RoleChoice {
        key: key.to_owned(),
        label: label.to_owned(),
        allows: allows.to_owned(),
    })
    .collect()
}

/// The People tab as the workspace's owner sees it, for captures.
pub(super) fn sample() -> People {
    let person = |key: &str, name: &str, role: &str, offers: Offers| Person {
        key: key.to_owned(),
        name: name.to_owned(),
        you: key == "owner",
        role: role.to_owned(),
        role_key: Some(role.to_lowercase()),
        extra: None,
        offers,
        can_manage: offers != Offers::Nothing,
    };
    People {
        count: "3 members".to_owned(),
        status: "You own this workspace.".to_owned(),
        ownership: Some("Transfer ownership".to_owned()),
        header: "People".to_owned(),
        you_tag: "You".to_owned(),
        remove: "Remove".to_owned(),
        people: vec![
            person("owner", "Bob Hellauer", "Owner", Offers::Nothing),
            Person {
                extra: Some("2 extra permissions".to_owned()),
                ..person("ada", "Ada Lovelace-Byron", "Editor", Offers::RoleAndRemove)
            },
            person("bo", "Bo Li Andersson", "Viewer", Offers::RoleAndRemove),
        ],
        roles: sample_roles(),
        empty: (
            "Just you".to_owned(),
            "Invite people from the General tab.".to_owned(),
        ),
        note: None,
        remove_decision: Decision {
            title: "Remove this person?".to_owned(),
            description: "They lose access to the workspace and everything in it.".to_owned(),
            keep: "Keep access".to_owned(),
            review: "Review activity".to_owned(),
            confirm: "Remove person".to_owned(),
        },
    }
}

/// The People tab as an editor sees it: everyone, and nothing to change.
pub(super) fn sample_member() -> People {
    let mut people = sample();
    people.ownership = None;
    "Bob Hellauer owns this workspace.".clone_into(&mut people.status);
    for person in &mut people.people {
        person.you = person.key == "ada";
        person.can_manage = false;
        if person.you {
            person.offers = Offers::Role;
        }
    }
    people.note = Some("Only admins can change roles or remove people.".to_owned());
    people
}
