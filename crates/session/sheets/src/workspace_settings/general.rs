//! The General tab: the workspace's name, inviting people, the workspace at
//! a glance, the devices it shows on and its place among the person's
//! workspaces, and leaving it, last and apart.

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
    /// The row that takes the workspace off this device.
    pub leave: Group,
    pub leave_label: String,
    pub leavable: bool,
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
    // Leaving stands apart, last and under a rule, where nothing else is
    // reached for on the way to it.
    groups.push(Box::new(Section::new(vec![
        Box::new(Divider::horizontal()),
        Box::new(
            ListRow::new("ws-general-leave-row", general.leave.title.clone())
                .secondary(general.leave.description.clone())
                .two_line_secondary()
                .trailing(Box::new(
                    button(parts, "ws-general-leave", &general.leave_label, || {
                        WorkspaceSettingsEffect::Delete
                    })
                    .role(ButtonRole::Destructive)
                    .with_enabled(general.leavable),
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

/// The devices that show the workspace, each shown or hidden, and, with more
/// than one of them, the buttons that move the workspace up or down their
/// sidebars.
fn devices(general: &General, parts: &Parts) -> Element {
    let mut devices: Vec<Element> = general
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
    // One device has no order to choose between.
    if general.device_rows.len() > 1 {
        devices.push(buttons(vec![
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
    }
    described(&general.devices, devices)
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
            .on_submit_view(move |_| save.commit_name()),
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
    Button::new(id, label).on_effect(move || press.ask(effect()))
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
        leave: Group::new(
            "Leave workspace",
            "Takes it off this device, along with the windows and groups that \
             belong to it. The workspace, its history and everyone else's \
             windows stay.",
        ),
        leave_label: "Leave".to_owned(),
        leavable: true,
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
        ..sample()
    }
}
