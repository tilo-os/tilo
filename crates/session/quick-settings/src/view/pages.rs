//! The panel's pages: the notifications kept since they arrived (grouped by
//! app, with Clear all and its Undo, and why fresh ones are held), one
//! notification read in full, and one Bluetooth accessory with the devices
//! it can move to.

use ui::TextRole;
use ui::framework::ViewCx;
use ui::icons::Icon;
use ui::tokens::Rhythm;
use ui::widget::{
    Button, ButtonRole, Column, Element, ListRow, Row, Section, SectionHeader, Spacer, Text,
};

use super::Parts;
use crate::effect::QsEffect;
use crate::i18n::{t, t_args};
use crate::resource::QuickSettings;
use crate::{NotificationRow, NotificationsView};

/// A page's header: Back, named for where it goes, and the page's title.
fn header(
    cx: &ViewCx<QuickSettings>,
    title: &str,
    back: &str,
    trailing: Option<Element>,
) -> Element {
    let back = cx.actions().bind_invocation(
        Button::icon_only("qs-back", Icon::ChevronLeft, back).role(ButtonRole::Plain),
        QuickSettings::BACK.invoke(),
    );
    let mut row = Row::new()
        .no_wrap()
        .with_align_items(ui::taffy::AlignItems::Center)
        .with_rhythm(Rhythm::Related)
        .push(Box::new(back))
        .push(Box::new(
            Text::new(title)
                .with_role(TextRole::Heading)
                .with_heading_level(2)
                .with_id("qs-page-title"),
        ))
        .push(Box::new(Spacer::new()));
    if let Some(trailing) = trailing {
        row = row.push(trailing);
    }
    Box::new(row)
}

/// The notifications page.
pub(super) fn notifications(cx: &ViewCx<QuickSettings>, parts: &Parts) -> Element {
    let folded = parts.folded.clone();
    let notifications = parts.notifications.clone();
    cx.switch(&notifications, move |cx, view| {
        let (folded, view) = (folded.clone(), view.clone());
        cx.switch(&folded.clone(), move |cx, collapsed| {
            notification_list(cx, &view, collapsed, &folded)
        })
    })
}

fn notification_list(
    cx: &ViewCx<QuickSettings>,
    view: &NotificationsView,
    collapsed: &[String],
    folded: &ui::LocalState<Vec<String>>,
) -> Element {
    let actions = cx.actions();
    let trailing: Option<Element> = if view.can_undo {
        Some(Box::new(actions.bind_effect_invocation(
            Button::new("qs-notifications-undo", &t("qs-notifications-undo")),
            &QsEffect::UndoClearNotifications,
        )))
    } else if view.rows.is_empty() {
        None
    } else {
        Some(Box::new(actions.bind_effect_invocation(
            Button::new("qs-notifications-clear", &t("qs-notifications-clear")),
            &QsEffect::ClearNotifications,
        )))
    };
    let mut page = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_rhythm(Rhythm::Related)
        .with_id("qs-notifications")
        .push(header(
            cx,
            &t("qs-notifications"),
            &t("qs-back-to-controls"),
            trailing,
        ));
    if let Some(quiet) = &view.quiet {
        page = page.push(Box::new(
            ListRow::new("qs-notifications-quiet", quiet).icon(Icon::BellOff),
        ));
    }
    if view.rows.is_empty() {
        return page
            .push(Box::new(
                Text::new(&t("qs-notifications-empty"))
                    .with_role(TextRole::Caption)
                    .with_id("qs-notifications-empty"),
            ))
            .into_widget();
    }
    // Newest first, one group per app in the order its newest arrived.
    let mut groups: Vec<(&str, &str, Vec<&NotificationRow>)> = Vec::new();
    for row in &view.rows {
        match groups.iter_mut().find(|(key, _, _)| *key == row.group) {
            Some((_, _, rows)) => rows.push(row),
            None => groups.push((&row.group, &row.app, vec![row])),
        }
    }
    for (key, app, rows) in groups {
        let items: Vec<Element> = rows.iter().map(|row| notification_row(cx, row)).collect();
        // One notification from an app stands on its own; several are one
        // group, folded and dismissed together.
        if rows.len() < 2 {
            page = page.push(Box::new(Section::new(items)));
            continue;
        }
        let dismiss_all = actions.bind_effect_invocation(
            Button::new(
                &format!("qs-notifications-dismiss-{key}"),
                &t_args("qs-notifications-dismiss-all", &[("app", app.into())]),
            )
            .role(ButtonRole::Plain),
            &QsEffect::DismissNotificationGroup {
                group: key.to_owned(),
            },
        );
        let header = SectionHeader::group(app)
            .count(rows.len())
            .action(dismiss_all);
        let open = !collapsed.iter().any(|group| group == key);
        let section = Section::new(items).with_header(header);
        let section = if open {
            section.collapsible()
        } else {
            section.collapsed()
        };
        let (folded, group) = (folded.clone(), key.to_owned());
        let section = section.on_expand(move |expanded| {
            folded.update(|groups| {
                groups.retain(|kept| *kept != group);
                if !expanded {
                    groups.push(group.clone());
                }
            });
        });
        page = page.push(Box::new(
            section.disclosure_id(format!("qs-notifications-group-{key}")),
        ));
    }
    page.into_widget()
}

/// One notification: opened when chosen (read in full when nothing can
/// open it), with its actions, Dismiss and, where an agent is live, Hand to.
fn notification_row(cx: &ViewCx<QuickSettings>, row: &NotificationRow) -> Element {
    let actions = cx.actions();
    let mut item = ListRow::new(format!("qs-notification-{}", row.id), &row.title)
        .secondary(&row.body)
        .secondary_lines(2)
        .value(&row.age);
    if let Some(binding) = actions.effect(&QsEffect::DismissNotification {
        notification: row.id,
    }) {
        item = item.action_binding(Icon::X, t("qs-notification-dismiss"), binding);
    }
    if let Some(agent) = &row.agent
        && let Some(binding) = actions.effect(&QsEffect::HandNotificationToAgent {
            notification: row.id,
        })
    {
        item = item.action_binding(
            Icon::Bot,
            t_args(
                "qs-notification-hand-to",
                &[("agent", agent.as_str().into())],
            ),
            binding,
        );
    }
    let item: Element = Box::new(if row.opens {
        actions.bind_effect_invocation(
            item,
            &QsEffect::OpenNotification {
                notification: row.id,
            },
        )
    } else {
        actions.bind_invocation(item, QuickSettings::READ_NOTIFICATION.with(row.id))
    });
    if row.actions.is_empty() {
        return item;
    }
    // The sender's choices stand under the notification, where its text
    // keeps the row's width.
    let mut buttons = Row::new().with_rhythm(Rhythm::Related);
    for (key, label) in &row.actions {
        buttons = buttons.push(Box::new(actions.bind_effect_invocation(
            Button::new(&format!("qs-notification-{}-{key}", row.id), label),
            &QsEffect::RunNotificationAction {
                notification: row.id,
                action: key.clone(),
            },
        )));
    }
    Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_rhythm(Rhythm::Tight)
        .push(item)
        .push(Box::new(buttons))
        .into_widget()
}

/// One notification read in full.
pub(super) fn reader(cx: &ViewCx<QuickSettings>, id: u64, parts: &Parts) -> Element {
    cx.switch(&parts.notifications, move |cx, view| {
        let actions = cx.actions();
        let mut page = Column::new()
            .with_intrinsic_height()
            .with_no_padding()
            .with_rhythm(Rhythm::Related)
            .with_id("qs-notification-reader");
        let Some(row) = view.rows.iter().find(|row| row.id == id) else {
            return page
                .push(header(
                    cx,
                    &t("qs-notifications"),
                    &t("qs-back-to-notifications"),
                    None,
                ))
                .push(Box::new(
                    Text::new(&t("qs-notification-gone")).with_role(TextRole::Caption),
                ))
                .into_widget();
        };
        let dismiss = actions.bind_effect_invocation(
            Button::icon_only(
                "qs-notification-dismiss",
                Icon::X,
                &t("qs-notification-dismiss"),
            )
            .role(ButtonRole::Plain),
            &QsEffect::DismissNotification {
                notification: row.id,
            },
        );
        page = page
            .push(header(
                cx,
                &row.app,
                &t("qs-back-to-notifications"),
                Some(Box::new(dismiss)),
            ))
            .push(Box::new(Text::new(&row.age).with_role(TextRole::Caption)))
            .push(Box::new(
                Text::new(&row.title)
                    .with_role(TextRole::Title)
                    .with_id("qs-notification-title"),
            ))
            .push(Box::new(
                Text::new(&row.body).with_id("qs-notification-body"),
            ));
        if !row.actions.is_empty() {
            let mut buttons = Row::new().with_rhythm(Rhythm::Related);
            for (key, label) in &row.actions {
                buttons = buttons.push(Box::new(actions.bind_effect_invocation(
                    Button::new(&format!("qs-notification-{key}"), label),
                    &QsEffect::RunNotificationAction {
                        notification: row.id,
                        action: key.clone(),
                    },
                )));
            }
            page = page.push(Box::new(buttons));
        }
        page.into_widget()
    })
}

/// One accessory: disconnect it here, or move it to another of the
/// person's devices (one already paired with it).
pub(super) fn accessory(cx: &ViewCx<QuickSettings>, address: &str, parts: &Parts) -> Element {
    let address = address.to_owned();
    cx.switch(&parts.accessory, move |cx, accessory| {
        let actions = cx.actions();
        let device = accessory
            .devices
            .iter()
            .find(|device| device.address.eq_ignore_ascii_case(&address));
        let fallback = t("qs-bluetooth-accessory");
        let name = device.map_or(fallback.as_str(), |device| device.name.as_str());
        let mut page = Column::new()
            .with_intrinsic_height()
            .with_no_padding()
            .with_rhythm(Rhythm::Related)
            .with_id("qs-accessory")
            .push(header(
                cx,
                &t_args("qs-bluetooth-move", &[("accessory", name.into())]),
                &t("qs-back-to-controls"),
                None,
            ))
            .push(Box::new(
                Text::new(&t("qs-bluetooth-move-note")).with_role(TextRole::Caption),
            ));
        let here = ListRow::new("qs-accessory-here", t("qs-bluetooth-this-device"))
            .icon(Icon::Bluetooth)
            .secondary(t_args(
                "qs-bluetooth-disconnect",
                &[("accessory", name.into())],
            ))
            .with_enabled(device.is_some_and(|device| device.connected));
        let mut rows: Vec<Element> = vec![Box::new(actions.bind_effect_invocation(
            here,
            &QsEffect::DisconnectBluetooth {
                address: address.clone(),
            },
        ))];
        match &accessory.targets {
            None => {
                page = page.push(Box::new(Section::new(rows)));
                return page
                    .push(Box::new(
                        Text::new(&t("qs-devices-fabric-unavailable"))
                            .with_role(TextRole::Caption)
                            .with_id("qs-accessory-unavailable"),
                    ))
                    .into_widget();
            }
            Some(targets) => {
                for target in targets {
                    let movable = !target.local && target.online;
                    let row = ListRow::new(format!("qs-accessory-to-{}", target.id), &target.label)
                        .icon(Icon::Monitor)
                        .secondary(t(if target.local {
                            "qs-bluetooth-this-certified"
                        } else if target.online {
                            "qs-bluetooth-move-here"
                        } else {
                            "qs-devices-offline"
                        }))
                        .with_enabled(movable);
                    rows.push(Box::new(if movable {
                        actions.bind_effect_invocation(
                            row,
                            &QsEffect::HandOffBluetooth {
                                address: address.clone(),
                                device: target.id.clone(),
                            },
                        )
                    } else {
                        row
                    }));
                }
            }
        }
        page.push(Box::new(Section::new(rows))).into_widget()
    })
}
