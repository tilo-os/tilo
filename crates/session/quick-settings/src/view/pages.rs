//! The panel's page of one Bluetooth accessory with the devices it can move
//! to; its header is its way back to the controls.

use ui::TextRole;
use ui::framework::ViewCx;
use ui::icons::Icon;
use ui::tokens::Rhythm;
use ui::widget::{Button, ButtonRole, Column, Element, ListRow, Row, Section, Text};

use super::Parts;
use crate::effect::QsEffect;
use crate::i18n::{t, t_args};
use crate::resource::QuickSettings;

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
            .push(Box::new(
                Row::new()
                    .no_wrap()
                    .with_align_items(ui::taffy::AlignItems::Center)
                    .with_rhythm(Rhythm::Related)
                    .push(Box::new(
                        actions.bind_invocation(
                            Button::icon_only(
                                "qs-back",
                                Icon::ChevronLeft,
                                &t("qs-back-to-controls"),
                            )
                            .role(ButtonRole::Plain),
                            QuickSettings::BACK.invoke(),
                        ),
                    ))
                    .push(Box::new(
                        Text::new(&t_args("qs-bluetooth-move", &[("accessory", name.into())]))
                            .with_role(TextRole::Heading)
                            .with_heading_level(2)
                            .with_id("qs-page-title"),
                    )),
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
