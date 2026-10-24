use std::collections::HashMap;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::{Rhythm, TextRole};
use ui::widget::{
    Button, ButtonRole, Column, Container, ControlSize, Divider, Element, Embedded, FieldFace,
    ListRow, Row, Section, StatGroup, StatTile, Text, Widget,
};

/// The controls of the General tab the lens reports hits for, by the id
/// each carries in the page.
pub(super) const NAME: &str = "ws-general-name";
pub(super) const INVITE: &str = "ws-general-invite";
pub(super) const GLANCE: &str = "ws-general-glance";
pub(super) const MOVE_UP: &str = "ws-general-move-up";
pub(super) const MOVE_DOWN: &str = "ws-general-move-down";
pub(super) const PUBLIC_SWITCH: &str = "ws-general-public";
pub(super) const PUBLIC_ROLE: &str = "ws-general-public-role";
pub(super) const PUBLIC_DESCRIBE: &str = "ws-general-public-describe";
pub(super) const DELETE: &str = "ws-general-delete";

pub(super) fn device_id(index: usize) -> String {
    format!("ws-general-device-{index}")
}

pub(super) fn restore_id(index: usize) -> String {
    format!("ws-general-restore-{index}")
}

pub(super) fn delete_archived_id(index: usize) -> String {
    format!("ws-general-delete-archived-{index}")
}

/// One group of the page: its title and what it explains, wrapped.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Group {
    pub title: String,
    pub description: String,
}

/// One of the person's devices: its name, its toggle's label, and whether
/// the workspace shows there.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct DeviceRow {
    pub label: String,
    pub toggle: String,
    pub shown: bool,
}

/// The Public group's working controls, once its host answered.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct PublicControls {
    pub public: bool,
    pub switch: String,
    pub role: String,
    pub describe: String,
}

/// Everything the General tab shows, resolved to the person's words. The
/// page is rebuilt only when this changes.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct GeneralContent {
    pub identity: Group,
    /// Why the name cannot be changed here, when it cannot.
    pub rename_note: Option<String>,
    pub invite: Group,
    pub invite_label: String,
    pub can_invite: bool,
    pub glance_title: String,
    /// Each statistic's label and value.
    pub stats: [(String, String); 3],
    pub devices: Group,
    pub device_rows: Vec<DeviceRow>,
    pub move_labels: [String; 2],
    pub move_enabled: [bool; 2],
    /// The Public group, where there is something to say about it.
    pub public: Option<(Group, Option<PublicControls>)>,
    pub archived_title: String,
    /// Each archived workspace's name, with its Restore and Delete labels.
    pub archived: Vec<(String, String, String)>,
    pub delete: Group,
    pub delete_label: String,
    pub deletable: bool,
}

/// The General tab's body: groups of rows on the sheet's ground, laid out
/// by the toolkit so every description wraps and every row of buttons
/// wraps before it overlaps.
#[derive(Default)]
pub(super) struct GeneralPage {
    page: Option<(GeneralContent, f64, Embedded<Column>)>,
    /// Each control's rect in the page's own space, from its last layout.
    rects: HashMap<String, Rect>,
    height: f64,
    /// Where the page's own space starts in the lens's.
    offset: Vec2,
}

impl GeneralPage {
    /// Lay the page out `width` wide for `content`, rebuilding it when the
    /// content or the field's height changed. Returns the page's height.
    pub(super) fn lay_out(
        &mut self,
        content: &GeneralContent,
        width: f64,
        theme: &Theme,
        text: &mut TextRenderer,
    ) -> f64 {
        let field = FieldFace::height(ControlSize::Regular, theme);
        let stale = self.page.as_ref().is_none_or(|(built, built_field, _)| {
            built != content || built_field.to_bits() != field.to_bits()
        });
        if stale {
            self.page = Some((content.clone(), field, Embedded::new(build(content, field))));
        }
        let Some((_, _, page)) = self.page.as_mut() else {
            return 0.0;
        };
        self.height = page.layout(Rect::new(0.0, 0.0, width, 0.0), theme, text);
        self.rects.clear();
        collect_rects(page.widget(), &mut self.rects);
        // A control that takes no input answers no press either.
        for (id, enabled) in [
            (MOVE_UP, content.move_enabled[0]),
            (MOVE_DOWN, content.move_enabled[1]),
            (DELETE, content.deletable),
        ] {
            if !enabled {
                self.rects.remove(id);
            }
        }
        self.height
    }

    /// Where the page's top-left corner stands in the lens.
    pub(super) fn place(&mut self, origin: Point) {
        self.offset = origin.to_vec2();
        if let Some((_, _, page)) = self.page.as_mut() {
            page.set_transform(Affine::translate(self.offset));
        }
    }

    pub(super) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        if let Some((_, _, page)) = self.page.as_mut() {
            page.draw(scene, transform, text, theme, &PresenceCtx::default());
        }
    }

    pub(super) fn height(&self) -> f64 {
        self.height
    }

    /// The laid-out page, for tests that read what it shows.
    #[cfg(test)]
    pub(super) fn widget(&self) -> Option<&dyn Widget> {
        self.page
            .as_ref()
            .map(|(_, _, page)| page.widget() as &dyn Widget)
    }

    /// The control `id`'s rect in the lens, or zero where the page has none.
    pub(super) fn rect(&self, id: &str) -> Rect {
        self.rects.get(id).map_or(Rect::ZERO, |rect| {
            Affine::translate(self.offset).transform_rect_bbox(*rect)
        })
    }
}

fn collect_rects(widget: &dyn Widget, out: &mut HashMap<String, Rect>) {
    if let Some(id) = widget.test_id() {
        out.insert(id.to_owned(), widget.get_rect());
    }
    for child in widget.children() {
        collect_rects(child, out);
    }
}

fn described(group: &Group, mut items: Vec<Element>) -> Element {
    items.insert(0, caption(&group.description));
    Box::new(Section::titled(group.title.clone(), items))
}

fn caption(text: &str) -> Element {
    Box::new(Text::new(text).with_role(TextRole::Caption).dimmed())
}

fn button(id: &str, label: &str, enabled: bool) -> Button {
    Button::new(id, label).with_enabled(enabled)
}

/// A line of buttons that wraps onto further lines where it runs out of
/// room, never overlapping.
fn buttons(buttons: Vec<Button>) -> Element {
    let row = buttons.into_iter().fold(
        Row::new().with_rhythm(Rhythm::Related).align_start(),
        |row, button| row.push(Box::new(button)),
    );
    Box::new(row)
}

/// The archived workspaces, each with Restore and Delete.
fn archived(content: &GeneralContent) -> Element {
    Box::new(Section::titled(
        content.archived_title.clone(),
        content
            .archived
            .iter()
            .enumerate()
            .map(|(index, (name, restore, delete))| {
                Box::new(
                    ListRow::new(format!("ws-general-archived-{index}"), name.clone()).trailing(
                        buttons(vec![
                            Button::new(&restore_id(index), restore),
                            Button::new(&delete_archived_id(index), delete)
                                .role(ButtonRole::Destructive),
                        ]),
                    ),
                ) as Element
            })
            .collect(),
    ))
}

fn build(content: &GeneralContent, field: f64) -> Column {
    let mut identity = vec![Box::new(
        // The lens paints the name field here itself, across the
        // group, so it keeps its caret and selection while typing.
        Container::new()
            .with_no_padding()
            .with_height(ui::num::f32_of_f64(field))
            .with_id(NAME),
    ) as Element];
    if let Some(note) = &content.rename_note {
        identity.push(caption(note));
    }
    let mut groups = vec![
        described(&content.identity, identity),
        described(
            &content.invite,
            vec![buttons(vec![
                button(INVITE, &content.invite_label, content.can_invite).role(ButtonRole::Primary),
            ])],
        ),
        Box::new(Section::titled(
            content.glance_title.clone(),
            vec![Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_id(GLANCE)
                    .push(Box::new(StatGroup::new(
                        content
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
    let mut devices: Vec<Element> = content
        .device_rows
        .iter()
        .enumerate()
        .map(|(index, device)| {
            Box::new(
                ListRow::new(
                    format!("ws-general-device-row-{index}"),
                    device.label.clone(),
                )
                .trailing(Box::new(
                    Button::new(&device_id(index), &device.toggle).toggled(device.shown),
                )),
            ) as Element
        })
        .collect();
    devices.push(buttons(vec![
        button(MOVE_UP, &content.move_labels[0], content.move_enabled[0]),
        button(MOVE_DOWN, &content.move_labels[1], content.move_enabled[1]),
    ]));
    groups.push(described(&content.devices, devices));
    if let Some((group, controls)) = &content.public {
        let items = controls.as_ref().map_or_else(Vec::new, |controls| {
            vec![buttons(vec![
                Button::new(PUBLIC_SWITCH, &controls.switch).toggled(controls.public),
                Button::new(PUBLIC_ROLE, &controls.role),
                Button::new(PUBLIC_DESCRIBE, &controls.describe),
            ])]
        });
        groups.push(described(group, items));
    }
    if !content.archived.is_empty() {
        groups.push(archived(content));
    }
    // Deleting stands apart, last and under a rule, where nothing else is
    // reached for on the way to it.
    groups.push(Box::new(Section::new(vec![
        Box::new(Divider::horizontal()),
        Box::new(
            ListRow::new("ws-general-delete-row", content.delete.title.clone())
                .secondary(content.delete.description.clone())
                .two_line_secondary()
                .trailing(Box::new(
                    button(DELETE, &content.delete_label, content.deletable)
                        .role(ButtonRole::Destructive),
                )),
        ),
    ])));
    groups.into_iter().fold(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(Rhythm::Section),
        Column::push,
    )
}

/// What the General tab shows for `model`, in the person's words.
pub(super) fn content(model: &super::WorkspaceSettingsModel<'_>) -> GeneralContent {
    use crate::authority::WorkspaceAuthority as _;
    use crate::i18n::{t, t_args};

    let group = |title: &str, description: &str| Group {
        title: t(title),
        description: t(description),
    };
    let workspace = model.desktop.active_workspace();
    let can_invite = workspace.is_some_and(|workspace| {
        model.governs_active_room && workspace.may_administer(model.acting_user)
    });
    let invite_description = if can_invite {
        "workspace-settings-invite-description"
    } else {
        "workspace-settings-invite-admin-only"
    };
    let (members, grants, app_rules) = workspace.map_or((0, 0, 0), |ws| {
        let sandbox = ws
            .sandbox
            .iter()
            .filter(|cap| cap.effect == collab_types::caps::Effect::Grant);
        let given = ws
            .policy
            .grants
            .iter()
            .filter(|grant| !matches!(grant.subject, tilo_action_policy::Subject::Role(_)));
        (
            ws.roster().len(),
            sandbox.count().saturating_add(given.count()),
            ws.default_apps.policy.len(),
        )
    });
    let stat =
        |key: &str, count: usize| (t_args(key, &[("count", count.into())]), count.to_string());
    let workspaces = &model.desktop.workspaces;
    let first = workspaces
        .first()
        .is_some_and(|workspace| workspace.id == model.desktop.active);
    let last = workspaces
        .last()
        .is_some_and(|workspace| workspace.id == model.desktop.active);
    GeneralContent {
        identity: group(
            "workspace-settings-identity-title",
            "workspace-settings-identity-description",
        ),
        rename_note: (!model.owner_controls).then(|| t("workspace-settings-rename-admin-only")),
        invite: group("workspace-settings-invite-title", invite_description),
        invite_label: t("workspace-settings-invite-create"),
        can_invite,
        glance_title: t("workspace-settings-glance-title"),
        stats: [
            stat("workspace-settings-stat-members", members),
            stat("workspace-settings-stat-grants", grants),
            stat("workspace-settings-stat-app-rules", app_rules),
        ],
        devices: group(
            "workspace-settings-devices-title",
            "workspace-settings-devices-local",
        ),
        device_rows: device_rows(model.devices),
        move_labels: [
            t("workspace-settings-move-earlier"),
            t("workspace-settings-move-later"),
        ],
        move_enabled: [!first, !last],
        public: public(model.public),
        archived_title: t("workspace-settings-archived-title"),
        archived: model
            .archived
            .iter()
            .map(|archived| {
                (
                    archived.name.clone(),
                    t("workspace-settings-archived-restore"),
                    t("workspace-settings-archived-delete"),
                )
            })
            .collect(),
        delete: group(
            "workspace-settings-delete",
            "workspace-settings-delete-description",
        ),
        delete_label: t("workspace-settings-delete-action"),
        deletable: workspaces.len() > 1,
    }
}

/// Each of the person's devices as the Devices group lists it.
fn device_rows(devices: &[super::WorkspaceDevice]) -> Vec<DeviceRow> {
    use crate::i18n::{t, t_args};

    devices
        .iter()
        .map(|device| DeviceRow {
            label: if device.here {
                t_args(
                    "workspace-settings-device-here",
                    &[("device", device.label.clone().into())],
                )
            } else {
                device.label.clone()
            },
            toggle: t(if device.shown {
                "workspace-settings-device-shown"
            } else {
                "workspace-settings-device-hidden"
            }),
            shown: device.shown,
        })
        .collect()
}

/// The Public group: whether the workspace is listed in its host's
/// directory and what it says there, with the controls that change it once
/// the host answered. Nothing while there is no network to ask its host
/// through: the person can neither learn nor change it from here.
fn public(card: &super::PublicCard) -> Option<(Group, Option<PublicControls>)> {
    use super::PublicCard;
    use crate::i18n::{t, t_args};

    let title = t("workspace-settings-public-title");
    let (line, controls) = match card {
        PublicCard::Offline => return None,
        PublicCard::Asking => (t("workspace-settings-public-asking"), None),
        PublicCard::Unavailable(why) => (why.clone(), None),
        PublicCard::Known {
            public,
            description,
            default_role,
        } => (
            if *public && !description.is_empty() {
                description.clone()
            } else if *public {
                t("workspace-settings-public-on")
            } else {
                t("workspace-settings-public-off")
            },
            Some(PublicControls {
                public: *public,
                switch: t(if *public {
                    "workspace-settings-public-switch-on"
                } else {
                    "workspace-settings-public-switch-off"
                }),
                role: t_args(
                    "workspace-settings-public-role",
                    &[("role", super::super::role_label(*default_role).into())],
                ),
                describe: t("workspace-settings-public-describe"),
            }),
        ),
    };
    Some((
        Group {
            title,
            description: line,
        },
        controls,
    ))
}
