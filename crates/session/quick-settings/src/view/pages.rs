//! The panel's pages: one Bluetooth accessory with the devices it can move
//! to. A page's header is its way back to the controls.

use ui::TextRole;
use ui::framework::ViewCx;
use ui::icons::Icon;
use ui::tokens::Rhythm;
use ui::widget::{Button, ButtonRole, Column, Element, ListRow, Row, Section, Spacer, Text};

use super::Parts;
use crate::effect::QsEffect;
use crate::i18n::{t, t_args};
use crate::resource::QuickSettings;

/// A page's header: Back, named for where it goes, and the page's title.
pub(super) fn header(
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
                            "qs-bluetooth-this-device"
                        } else if target.online {
                            "qs-devices-online"
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
