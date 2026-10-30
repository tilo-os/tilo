//! The General tab: the workspace's name, inviting people, the workspace at
//! a glance, the devices it shows on and its place among the person's
//! workspaces, whether it is public, the workspaces archived here, and
//! deleting it, last and apart.

use ui::prelude::*;
use ui::tokens::Rhythm;
use ui::widget::{
    Button, ButtonRole, Column, Divider, Element, ListRow, Row, Section, StatGroup, StatTile, Text,
    TextInput,
};

use super::{Parts, Settings, WorkspaceSettingsEffect};

/// One group of the tab: its title and what it explains.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Group {
    pub title: String,
    pub description: String,
}

impl Group {
    fn new(title: &str, description: &str) -> Self {
        Self {
            title: title.to_owned(),
            description: description.to_owned(),
        }
    }
}

/// One of the person's devices: its name, its toggle's label, and whether
/// the workspace shows there.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Device {
    /// What the device is asked for by.
    pub key: String,
    pub label: String,
    pub toggle: String,
    pub shown: bool,
}

/// The Public group's working controls, once the workspace's host
/// answered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PublicControls {
    pub public: bool,
    pub switch: String,
    pub role: String,
    pub describe: String,
}

/// Whether the workspace is listed in its host's directory, what it says
/// there, and the controls that change it once the host answered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Public {
    pub group: Group,
    pub controls: Option<PublicControls>,
}

/// A workspace archived on this device, with its Restore and Delete.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Archived {
    /// What it is asked for by.
    pub key: String,
    pub name: String,
    pub restore: String,
    pub delete: String,
}

/// Everything the General tab shows, in the person's words.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct General {
    pub identity: Group,
    /// What the name field is called for assistive tools.
    pub name_label: String,
    /// Whether the person may rename the workspace.
    pub can_rename: bool,
    /// Why the name cannot be changed here, when it cannot.
    pub rename_note: Option<String>,
    pub invite: Group,
    pub invite_label: String,
    pub can_invite: bool,
    pub glance_title: String,
    /// Each statistic's label and value.
    pub stats: Vec<(String, String)>,
    pub devices: Group,
    pub device_rows: Vec<Device>,
    pub move_labels: [String; 2],
    pub move_enabled: [bool; 2],
    /// The Public group, where there is something to say about it.
    pub public: Option<Public>,
    pub archived_title: String,
    pub archived: Vec<Archived>,
    pub delete: Group,
    pub delete_label: String,
    pub deletable: bool,
}

/// The General tab of `settings`.
pub(super) fn pane(settings: &Settings, parts: &Parts) -> Element {
    let general = &settings.general;
    let mut identity = vec![name_field(general, parts)];
    if let Some(note) = &general.rename_note {
        identity.push(caption(note));
    }
    let invite = button(parts, "ws-general-invite", &general.invite_label, || {
        WorkspaceSettingsEffect::Invite
    })
    .role(ButtonRole::Primary)
    .with_enabled(general.can_invite);
    let mut groups = vec![
        described(&general.identity, identity),
        described(&general.invite, vec![buttons(vec![invite])]),
        Box::new(Section::titled(
            general.glance_title.clone(),
            vec![Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_id("ws-general-glance")
                    .push(Box::new(StatGroup::new(
                        general
                            .stats
                            .iter()
                            .map(|(label, value)| {
                                Box::new(StatTile::new(label.clone(), value.clone())) as Element
                            })
                            .collect(),
                    ))),
            )],
        )),
    ];
    groups.push(devices(general, parts));
    if let Some(public) = &general.public {
        groups.push(public_group(public, parts));
    }
    if !general.archived.is_empty() {
        groups.push(archived(general, parts));
    }
    // Deleting stands apart, last and under a rule, where nothing else is
    // reached for on the way to it.
    groups.push(Box::new(Section::new(vec![
        Box::new(Divider::horizontal()),
        Box::new(
            ListRow::new("ws-general-delete-row", general.delete.title.clone())
                .secondary(general.delete.description.clone())
                .two_line_secondary()
                .trailing(Box::new(
                    button(parts, "ws-general-delete", &general.delete_label, || {
                        WorkspaceSettingsEffect::Delete
                    })
                    .role(ButtonRole::Destructive)
                    .with_enabled(general.deletable),
                )),
        ),
    ])));
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

/// The name, typed in place; Enter saves it, as leaving the tab or the
/// sheet does.
fn name_field(general: &General, parts: &Parts) -> Element {
    let save = parts.clone();
    Box::new(
        TextInput::from_state(&parts.name)
            .with_id("ws-general-name")
            .with_accessible_name(general.name_label.clone())
            .with_read_only(!general.can_rename)
            .on_submit(move |_| save.commit_name()),
    )
}

/// A button asking the shell for what `effect` makes.
pub(super) fn button(
    parts: &Parts,
    id: &str,
    label: &str,
    effect: impl Fn() -> WorkspaceSettingsEffect + 'static,
) -> Button {
    let press = parts.clone();
    Button::new(id, label).on_click(move || press.ask(effect()))
}

pub(super) fn caption(text: &str) -> Element {
    Box::new(Text::new(text).with_role(TextRole::Caption).dimmed())
}

fn described(group: &Group, mut items: Vec<Element>) -> Element {
    items.insert(0, caption(&group.description));
    Box::new(Section::titled(group.title.clone(), items))
}

/// A line of buttons that wraps onto further lines where it runs out of
/// room, never overlapping.
pub(super) fn buttons(buttons: Vec<Button>) -> Element {
    Box::new(buttons.into_iter().fold(
        Row::new().with_rhythm(Rhythm::Related).align_start(),
        |row, button| row.push(Box::new(button)),
    ))
}

/// The devices the workspace shows on, each shown or hidden, and its place among the
/// person's workspaces.
fn devices(general: &General, parts: &Parts) -> Element {
    let mut rows: Vec<Element> = general
        .device_rows
        .iter()
        .map(|device| {
            let key = device.key.clone();
            Box::new(
                ListRow::new(
                    format!("ws-general-device-row-{}", device.key),
                    device.label.clone(),
                )
                .trailing(Box::new(
                    button(
                        parts,
                        &format!("ws-general-device-{}", device.key),
                        &device.toggle,
                        move || WorkspaceSettingsEffect::ToggleDevice {
                            device: key.clone(),
                        },
                    )
                    .toggled(device.shown),
                )),
            ) as Element
        })
        .collect();
    rows.push(buttons(vec![
        button(parts, "ws-general-move-up", &general.move_labels[0], || {
            WorkspaceSettingsEffect::Move { step: -1 }
        })
        .with_enabled(general.move_enabled[0]),
        button(
            parts,
            "ws-general-move-down",
            &general.move_labels[1],
            || WorkspaceSettingsEffect::Move { step: 1 },
        )
        .with_enabled(general.move_enabled[1]),
    ]));
    described(&general.devices, rows)
}

/// Whether anyone may find and join the workspace, and how.
fn public_group(public: &Public, parts: &Parts) -> Element {
    let items = public.controls.as_ref().map_or_else(Vec::new, |controls| {
        let make_public = !controls.public;
        vec![buttons(vec![
            button(parts, "ws-general-public", &controls.switch, move || {
                WorkspaceSettingsEffect::SetPublic {
                    public: make_public,
                }
            })
            .toggled(controls.public),
            button(parts, "ws-general-public-role", &controls.role, || {
                WorkspaceSettingsEffect::NextJoinRole
            }),
            button(
                parts,
                "ws-general-public-describe",
                &controls.describe,
                || WorkspaceSettingsEffect::Describe,
            ),
        ])]
    });
    described(&public.group, items)
}

/// The archived workspaces, each with Restore and Delete.
fn archived(general: &General, parts: &Parts) -> Element {
    Box::new(Section::titled(
        general.archived_title.clone(),
        general
            .archived
            .iter()
            .map(|archived| {
                let (restore, delete) = (archived.key.clone(), archived.key.clone());
                Box::new(
                    ListRow::new(
                        format!("ws-general-archived-{}", archived.key),
                        archived.name.clone(),
                    )
                    .trailing(buttons(vec![
                        button(
                            parts,
                            &format!("ws-general-restore-{}", archived.key),
                            &archived.restore,
                            move || WorkspaceSettingsEffect::Restore {
                                workspace: restore.clone(),
                            },
                        ),
                        button(
                            parts,
                            &format!("ws-general-delete-archived-{}", archived.key),
                            &archived.delete,
                            move || WorkspaceSettingsEffect::DeleteArchived {
                                workspace: delete.clone(),
                            },
                        )
                        .role(ButtonRole::Destructive),
                    ])),
                ) as Element
            })
            .collect(),
    ))
}

/// The General tab as a workspace's owner sees it, for captures.
pub(super) fn sample() -> General {
    General {
        identity: Group::new("Name", "What everyone in the workspace sees it called."),
        name_label: "Workspace name".to_owned(),
        can_rename: true,
        rename_note: None,
        invite: Group::new(
            "Invite people",
            "Make a link or a code for someone to join with.",
        ),
        invite_label: "Create invitation".to_owned(),
        can_invite: true,
        glance_title: "At a glance".to_owned(),
        stats: vec![
            ("3 members".to_owned(), "3".to_owned()),
            ("2 permissions given".to_owned(), "2".to_owned()),
            ("1 app rule".to_owned(), "1".to_owned()),
        ],
        devices: Group::new(
            "Devices",
            "Choose which of your devices show this workspace.",
        ),
        device_rows: vec![
            Device {
                key: "laptop".to_owned(),
                label: "Laptop (this device)".to_owned(),
                toggle: "Shown".to_owned(),
                shown: true,
            },
            Device {
                key: "tablet".to_owned(),
                label: "Tablet".to_owned(),
                toggle: "Hidden".to_owned(),
                shown: false,
            },
        ],
        move_labels: ["Move earlier".to_owned(), "Move later".to_owned()],
        move_enabled: [false, true],
        public: Some(Public {
            group: Group::new("Public", "Not listed in the directory."),
            controls: Some(PublicControls {
                public: false,
                switch: "Make public".to_owned(),
                role: "Joins as Viewer".to_owned(),
                describe: "Describe…".to_owned(),
            }),
        }),
        archived_title: "Archived here".to_owned(),
        archived: vec![Archived {
            key: "old-project".to_owned(),
            name: "Old project".to_owned(),
            restore: "Restore".to_owned(),
            delete: "Delete".to_owned(),
        }],
        delete: Group::new(
            "Delete workspace",
            "Removes it and everything in it from every device.",
        ),
        delete_label: "Delete…".to_owned(),
        deletable: true,
    }
}

/// The General tab as a viewer sees it: nothing to change but where it
/// shows.
pub(super) fn sample_viewer() -> General {
    General {
        can_rename: false,
        rename_note: Some("Only admins can rename the workspace.".to_owned()),
        invite: Group::new("Invite people", "Only admins can invite people."),
        can_invite: false,
        public: None,
        archived: Vec::new(),
        ..sample()
    }
}
