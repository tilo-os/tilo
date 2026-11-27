//! The People tab: how many share the workspace, then a row per person whose
//! role and Remove move under the name where the two do not fit beside it.
//!
//! A role is chosen from the role button's menu, each role with what it
//! allows. Removing someone asks first, with the way to their activity
//! beside it ([`Decision`]). Under the people come the work members gave
//! servers, each with until when and, on the device that signed it, Revoke.

use ui::icons::Icon;
use ui::tokens::Rhythm;
use ui::widget::{
    Button, ButtonRole, ChipStyle, Column, ControlSize, Element, EmptyState, ListRow, Row, Section,
    TextInput,
};

use super::general::caption;
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
    pub additional_roles: Vec<String>,
    /// The parts of the workspace their role is over, when it is not all of it.
    pub scope: Option<String>,
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

/// One holder of the workspace: what it goes by, and what it can read of the
/// workspace.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Holder {
    /// What it goes by.
    pub name: String,
    /// What it can read of the workspace.
    pub can_read: String,
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

/// Controls for maintaining the workspace's custom roles.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoleEditor {
    pub manage: bool,
    pub title: String,
    pub create: String,
    pub name: String,
    pub rename: String,
    pub save: String,
    pub remove_decision: Decision,
}

/// Everything the People tab shows, in the person's words.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct People {
    pub count: String,
    /// What removing someone does.
    pub status: String,
    pub header: String,
    pub you_tag: String,
    pub remove: String,
    pub people: Vec<Person>,
    /// The roles a person's role button offers.
    pub roles: Vec<RoleChoice>,
    pub custom_roles: Vec<RoleChoice>,
    pub role_editor: RoleEditor,
    /// What an empty list says instead: its title and detail.
    pub empty: (String, String),
    pub note: Option<String>,
    /// The question removing someone asks.
    pub remove_decision: Decision,
    /// The holders of the workspace, under their own heading.
    pub holders_header: String,
    pub holders: Vec<Holder>,
}

/// The People tab of `settings`.
pub(super) fn pane(settings: &Settings, parts: &Parts) -> Element {
    let people = &settings.people;
    let summary = vec![caption(&people.status)];
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
    if people.role_editor.manage || !people.custom_roles.is_empty() {
        groups.push(roles(people, parts));
    }
    if let Some(note) = &people.note {
        groups.push(caption(note));
    }
    if !people.holders.is_empty() {
        groups.push(Box::new(Section::titled(
            people.holders_header.clone(),
            people.holders.iter().map(holder_row).collect(),
        )));
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

fn roles(people: &People, parts: &Parts) -> Element {
    let editor = &people.role_editor;
    let mut rows: Vec<Element> = people
        .custom_roles
        .iter()
        .map(|role| {
            let mut row = ListRow::new(format!("ws-custom-role-{}", role.key), role.label.clone());
            if editor.manage {
                let edit = parts.clone();
                let key = role.key.clone();
                let name = role.label.clone();
                let remove = parts.clone();
                let change = Confirm::RemoveRole {
                    role: key.clone(),
                    name: name.clone(),
                };
                row = row.trailing(Box::new(
                    Row::new()
                        .with_rhythm(Rhythm::Related)
                        .push(Box::new(
                            Button::new(&format!("ws-role-rename-{}", role.key), &editor.rename)
                                .on_click(move || {
                                    edit.role_name.set(name.clone());
                                    edit.local
                                        .update(|local| local.editing_role = Some(key.clone()));
                                }),
                        ))
                        .push(Box::new(
                            Button::new(&format!("ws-role-remove-{}", role.key), &people.remove)
                                .role(ButtonRole::Destructive)
                                .on_click(move || remove.confirm(change.clone())),
                        )),
                ));
            }
            Box::new(row) as Element
        })
        .collect();
    if editor.manage {
        let create = parts.clone();
        rows.push(Box::new(
            Button::new("ws-role-create", &editor.create).on_click(move || {
                create.role_name.set(String::new());
                create
                    .local
                    .update(|local| local.editing_role = Some(String::new()));
            }),
        ));
        if let Some(role) = parts.local.get().editing_role {
            let edit = parts.clone();
            let save = move || {
                let name = edit.role_name.get().trim().to_owned();
                if !name.is_empty() {
                    edit.ask(WorkspaceSettingsEffect::SaveCustomRole {
                        role: role.clone(),
                        name,
                    });
                    edit.local.update(|local| local.editing_role = None);
                }
            };
            let submit = save.clone();
            rows.push(Box::new(
                Row::new()
                    .with_rhythm(Rhythm::Related)
                    .push(Box::new(
                        TextInput::from_state(&parts.role_name)
                            .with_id("ws-role-name")
                            .with_placeholder(editor.name.clone())
                            .with_accessible_name(editor.name.clone())
                            .on_submit(move |_| submit()),
                    ))
                    .push(Box::new(
                        Button::new("ws-role-save", &editor.save)
                            .role(ButtonRole::Primary)
                            .on_click(save),
                    )),
            ));
        }
    }
    Box::new(Section::titled(editor.title.clone(), rows))
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
    let details: Vec<&str> = person
        .scope
        .as_deref()
        .into_iter()
        .chain(
            people
                .custom_roles
                .iter()
                .filter(|role| person.additional_roles.contains(&role.key))
                .map(|role| role.label.as_str()),
        )
        .chain(person.extra.as_deref())
        .collect();
    if !details.is_empty() {
        row = row.secondary(details.join(", "));
    }
    if person.offers == Offers::Nothing {
        return Box::new(row.value(person.role.clone()));
    }
    let roles = people
        .roles
        .iter()
        .map(|choice| {
            let effect = WorkspaceSettingsEffect::SetRole {
                member: person.key.clone(),
                role: choice.key.clone(),
            };
            parts
                .choice(choice.label.clone(), &effect)
                .with_detail(choice.allows.clone())
                .with_checked(person.role_key.as_deref() == Some(choice.key.as_str()))
        })
        .chain(people.custom_roles.iter().map(|choice| {
            let assigned = person.additional_roles.contains(&choice.key);
            let effect = WorkspaceSettingsEffect::SetCustomRole {
                member: person.key.clone(),
                role: choice.key.clone(),
                assigned: !assigned,
            };
            parts
                .choice(choice.label.clone(), &effect)
                .with_checked(assigned)
        }))
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

/// One holder's row: what it goes by, and what it can read of the workspace
/// under it.
fn holder_row(holder: &Holder) -> Element {
    Box::new(
        ListRow::new(
            format!("ws-people-holder-{}", holder.name),
            holder.name.clone(),
        )
        .two_line_title()
        .avatar(holder.name.clone(), holder.name.clone())
        .square()
        .avatar_glyph(Icon::HardDrive)
        .secondary(holder.can_read.clone()),
    )
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
        additional_roles: Vec::new(),
        scope: None,
        extra: None,
        offers,
        can_manage: offers != Offers::Nothing,
    };
    People {
        count: "3 members".to_owned(),
        status: "Removing someone blocks future sync and edits.".to_owned(),
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
        custom_roles: Vec::new(),
        role_editor: RoleEditor::default(),
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
        holders_header: "Where it is kept".to_owned(),
        holders: vec![
            Holder {
                name: "Kitchen NAS".to_owned(),
                can_read: "Reads the workspace".to_owned(),
            },
            Holder {
                name: "tilo.sh".to_owned(),
                can_read: "Reads the workspace, and so can the cloud provider".to_owned(),
            },
            Holder {
                name: "Backup box".to_owned(),
                can_read: "Keeps an encrypted copy and reads none of it".to_owned(),
            },
        ],
    }
}

/// The People tab as an editor sees it: everyone, and nothing to change.
pub(super) fn sample_member() -> People {
    let mut people = sample();
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
