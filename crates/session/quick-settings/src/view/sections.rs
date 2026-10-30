//! The sections that open in place on the controls page: Wi-Fi's networks
//! (with the password prompt under the network asking for one), Bluetooth's
//! accessories, the VPN profiles, the power modes, the battery, where sound
//! plays, the person's devices and the clipboard history.

use ui::LocalState;
use ui::TextRole;
use ui::framework::{ActionControl as _, Actions, ViewCx};
use ui::icons::Icon;
use ui::tokens::Rhythm;
use ui::widget::{
    Button, ButtonRole, Column, Container, DisclosureBand, Element, ListRow, ProgressBar, Row,
    Scrollable, Section, Segment, SignalLevel, Text, TextInput, Toggle, ToggleGroup,
};

use super::Parts;
use crate::QsSection;
use crate::effect::QsEffect;
use crate::faces::{Item, NetworkFace};
use crate::hosted::{HostedActions, HostedRow};
use crate::i18n::t;
use crate::resource::QuickSettings;

/// `section` open in place under the control that opens it: it opens and
/// closes on the disclosure motion, known by its section, so switching
/// from one section to another closes the one as the other opens.
pub(super) fn in_place(cx: &ViewCx<QuickSettings>, section: QsSection, parts: &Parts) -> Element {
    Box::new(DisclosureBand::new(
        format!("qs-section-{}", section_id(section)),
        self::section(cx, section, parts),
    ))
}

/// `section`, built again whenever what it lists changes.
fn section(cx: &ViewCx<QuickSettings>, section: QsSection, parts: &Parts) -> Element {
    if section == QsSection::Clipboard {
        return clipboard(cx, parts, false);
    }
    cx.switch(&parts.section(section), move |cx, items| {
        let rows: Vec<Element> = items
            .iter()
            .flat_map(|item| item_rows(cx, section, item))
            .collect();
        Box::new(
            Container::new()
                .with_id(format!("qs-section-{}", section_id(section)))
                .with_no_padding()
                .with_child(Box::new(Section::new(rows))),
        )
    })
}

/// A section's stable name in test ids.
fn section_id(section: QsSection) -> &'static str {
    match section {
        QsSection::Wifi => "wifi",
        QsSection::Bluetooth => "bluetooth",
        QsSection::Vpn => "vpn",
        QsSection::PowerMode => "power-mode",
        QsSection::Battery => "battery",
        QsSection::Sound => "sound",
        QsSection::Devices => "devices",
        QsSection::Clipboard => "clipboard",
    }
}

/// The widgets one item is shown as.
#[allow(
    clippy::too_many_lines,
    reason = "one arm per kind of item, each a few widgets"
)]
fn item_rows(cx: &ViewCx<QuickSettings>, section: QsSection, item: &Item) -> Vec<Element> {
    let actions = cx.actions();
    match item {
        Item::Message(text) => vec![Box::new(
            Text::new(text)
                .with_role(TextRole::Caption)
                .with_id(format!("qs-{}-message", section_id(section))),
        )],
        Item::Network(network) => network_rows(cx, network),
        Item::Wired { connected, detail } => {
            let switch = actions.bind_effect(
                Toggle::from_state(&LocalState::new(*connected))
                    .with_id("qs-wired-switch")
                    .with_accessible_name(&t("qs-wired-network")),
                QsEffect::SET_WIRED,
            );
            let mut row = ListRow::new("qs-wired", t("qs-wired-network"))
                .icon(Icon::EthernetPort)
                .selected(*connected)
                .trailing(Box::new(switch));
            if let Some(detail) = detail {
                row = row.secondary(detail);
            }
            vec![Box::new(row)]
        }
        Item::Accessory(device) => {
            let mut row = ListRow::new(format!("qs-accessory-{}", device.address), &device.name)
                .icon(Icon::Bluetooth)
                .secondary(t(if device.connected {
                    "qs-connected"
                } else {
                    "qs-bluetooth-paired"
                }))
                .selected(device.connected);
            if device.battery > 0 {
                row = row.value(format!("{}%", device.battery));
            }
            if device.connected
                && let Some(binding) =
                    actions.invocation(QuickSettings::SHOW_ACCESSORY.with(device.address.clone()))
            {
                row = row.action_binding(
                    Icon::ChevronRight,
                    t("qs-bluetooth-use-elsewhere"),
                    binding,
                );
            }
            if let Some(forget) = actions.effect(&QsEffect::ForgetBluetooth {
                address: device.address.clone(),
            }) {
                row = row.trailing(Box::new(
                    Row::new()
                        .no_wrap()
                        .with_rhythm(Rhythm::Related)
                        .push(Box::new(
                            Button::icon_only(
                                &format!("qs-bluetooth-forget-{}", device.address),
                                Icon::Trash2,
                                &t("qs-bluetooth-forget"),
                            )
                            .bind_action(forget),
                        ) as Element),
                ));
            }
            let press = if device.connected {
                QsEffect::DisconnectBluetooth {
                    address: device.address.clone(),
                }
            } else {
                QsEffect::ConnectBluetooth {
                    address: device.address.clone(),
                }
            };
            vec![Box::new(actions.bind_effect_invocation(row, &press))]
        }
        Item::LookForDevices { looking } => {
            let row = ListRow::new(
                "qs-bluetooth-look",
                t(if *looking {
                    "qs-bluetooth-looking"
                } else {
                    "qs-bluetooth-look"
                }),
            )
            .icon(Icon::Search);
            vec![Box::new(actions.bind_effect_invocation(
                row,
                &QsEffect::ScanBluetooth { on: !looking },
            ))]
        }
        Item::Nearby(device) => {
            let mut row = ListRow::new(format!("qs-nearby-{}", device.address), &device.name)
                .icon(Icon::Bluetooth)
                .secondary(t("qs-bluetooth-in-reach"));
            if let Some(signal) = device.signal {
                row = row.value(format!("{signal} dBm"));
            }
            vec![Box::new(actions.bind_effect_invocation(
                row,
                &QsEffect::PairBluetooth {
                    address: device.address.clone(),
                },
            ))]
        }
        Item::Saved(saved) => {
            let auto = actions.bind_effect(
                Toggle::from_state(&LocalState::new(saved.autoconnect))
                    .with_id(&format!("qs-autoconnect-{}", saved.profile))
                    .with_accessible_name(&t("qs-wifi-autoconnect")),
                QsEffect::SET_NETWORK_AUTOCONNECT.map({
                    let profile = saved.profile.clone();
                    move |on: bool| (profile.clone(), on)
                }),
            );
            let mut trailing = Row::new().no_wrap().with_rhythm(Rhythm::Related);
            trailing = trailing.push(Box::new(auto) as Element);
            trailing = trailing.push(Box::new(forget_network(&actions, &saved.profile)) as Element);
            vec![Box::new(
                ListRow::new(format!("qs-saved-network-{}", saved.profile), &saved.name)
                    .icon(Icon::Wifi)
                    .secondary(t("qs-wifi-saved"))
                    .trailing(Box::new(trailing)),
            )]
        }
        Item::Hidden { prompt } => match prompt {
            Some(prompt) => vec![hidden_prompt(cx, prompt)],
            None => vec![Box::new(actions.bind_effect_invocation(
                ListRow::new("qs-wifi-hidden", t("qs-wifi-hidden")).icon(Icon::Wifi),
                &QsEffect::AskHiddenNetwork,
            ))],
        },
        Item::Profile { name, active } => {
            let switch = actions.bind_effect(
                Toggle::from_state(&LocalState::new(*active)).with_accessible_name(name),
                QsEffect::SET_VPN.map({
                    let name = name.clone();
                    move |on: bool| (name.clone(), on)
                }),
            );
            vec![Box::new(
                ListRow::new(format!("qs-vpn-{name}"), name)
                    .icon(Icon::Shield)
                    .trailing(Box::new(switch)),
            )]
        }
        Item::Battery {
            percent,
            state,
            time,
        } => {
            let mut column = Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .with_rhythm(Rhythm::Tight)
                .with_id("qs-battery-charge")
                .push(Box::new(
                    Row::new()
                        .no_wrap()
                        .with_align_items(ui::taffy::AlignItems::Center)
                        .with_rhythm(Rhythm::Related)
                        .push(Box::new(
                            Text::new(&format!("{percent}%"))
                                .with_role(TextRole::Title)
                                .numeric(),
                        ))
                        .push(Box::new(
                            Container::new()
                                .with_no_padding()
                                .with_flex_grow()
                                .with_child(Box::new(
                                    ProgressBar::new(f32::from(*percent) / 100.0)
                                        .with_label(t("qs-battery")),
                                )),
                        )),
                ))
                .push(Box::new(Text::new(state).with_role(TextRole::Caption)));
            if let Some(time) = time {
                column = column.push(Box::new(Text::new(time).with_role(TextRole::Caption)));
            }
            vec![Box::new(column)]
        }
        Item::Modes { offered, current } => {
            let selected = offered
                .iter()
                .position(|mode| mode == current)
                .unwrap_or(ui::widget::toggle_group::NONE_CHOSEN);
            let segments = offered
                .iter()
                .map(|mode| Segment::new(mode.label()))
                .collect();
            let modes = offered.clone();
            vec![Box::new(
                actions.bind_effect(
                    ToggleGroup::new(segments, selected)
                        .with_id("qs-power-modes")
                        .with_accessible_name(t("qs-power-mode")),
                    QsEffect::SET_POWER_MODE
                        .map(move |index: usize| modes.get(index).copied().unwrap_or_default()),
                ),
            )]
        }
        Item::PowerModeLink { mode, tile } => {
            let row = ListRow::new("qs-battery-power-mode", t("qs-power-mode"))
                .icon(Icon::Gauge)
                .value(mode)
                .disclosure();
            vec![Box::new(if *tile {
                actions.bind_invocation(row, QuickSettings::OPEN_SECTION.with(QsSection::PowerMode))
            } else {
                actions.bind_effect_invocation(
                    row,
                    &QsEffect::OpenSettings {
                        link: "settings://power".into(),
                    },
                )
            })]
        }
        Item::Awake(text) => vec![Box::new(
            ListRow::new("qs-battery-awake", text).icon(Icon::Monitor),
        )],
        Item::Output(output) => {
            let lower = output.name.to_lowercase();
            let icon = if lower.contains("headphone") || lower.contains("headset") {
                Icon::Headphones
            } else {
                Icon::Speaker
            };
            let row = ListRow::new(format!("qs-output-{}", output.node), &output.name)
                .icon(icon)
                .checked(output.current);
            vec![Box::new(if output.current {
                row
            } else {
                actions.bind_effect_invocation(row, &QsEffect::RouteSound { node: output.node })
            })]
        }
        Item::Device(row) => vec![device_row(cx, row)],
        Item::Copies(_) => Vec::new(),
        Item::Settings { label, link } => vec![Box::new(
            actions.bind_effect_invocation(
                ListRow::new(format!("qs-{}-settings", section_id(section)), label)
                    .icon(Icon::Settings2)
                    .disclosure(),
                &QsEffect::OpenSettings {
                    link: (*link).to_owned(),
                },
            ),
        )],
    }
}

/// A network, and the password prompt under it while it asks for one.
fn network_rows(cx: &ViewCx<QuickSettings>, network: &NetworkFace) -> Vec<Element> {
    let actions = cx.actions();
    let mut row = ListRow::new(format!("qs-network-{}", network.ssid), &network.ssid)
        .signal(SignalLevel::from_bars(network.signal))
        .selected(network.connected);
    if network.secure {
        row = row.mark(Icon::Lock);
    }
    if let Some(value) = &network.value {
        row = row.value(value);
    }
    if let Some(profile) = &network.profile {
        row = row.trailing(Box::new(forget_network(&actions, profile)));
    }
    let row: Element = Box::new(match &network.press {
        Some(press) => actions.bind_effect_invocation(row, press),
        None => row,
    });
    let mut rows = vec![row];
    if let Some(prompt) = &network.asking {
        rows.push(password_prompt(cx, &network.ssid, prompt));
    }
    rows
}

/// The button that forgets the saved profile `profile`, after the person
/// confirms it.
fn forget_network(actions: &Actions<QuickSettings>, profile: &str) -> Row {
    let effect = QsEffect::ForgetNetwork {
        profile: profile.to_owned(),
    };
    let button = Button::icon_only(
        &format!("qs-forget-network-{profile}"),
        Icon::Trash2,
        &t("qs-wifi-forget"),
    );
    match actions.effect(&effect) {
        Some(binding) => Row::new().push(Box::new(button.bind_action(binding)) as Element),
        None => Row::new().push(Box::new(button) as Element),
    }
}

/// The prompt for a protected network's password: the field (whose text
/// never leaves it but to join the network), what went wrong last time,
/// Cancel and Connect.
fn password_prompt(
    cx: &ViewCx<QuickSettings>,
    ssid: &str,
    prompt: &crate::WifiPasswordPrompt,
) -> Element {
    let actions = cx.actions();
    let password = LocalState::new(String::new());
    let network = ssid.to_owned();
    let connect = QsEffect::CONNECT_WIFI.map(move |password: String| (network.clone(), password));
    let mut field = TextInput::from_state(&password)
        .with_id(&format!("qs-wifi-password-{}", prompt.id))
        .with_secret(true)
        .with_placeholder(t("qs-wifi-password"))
        .disabled(prompt.connecting);
    if let Some(binding) = actions.effect_binding(connect) {
        field = field.on_submit_action(binding);
    }
    let mut column = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_rhythm(Rhythm::Related)
        .with_id("qs-wifi-password")
        .push(Box::new(
            Text::new(&t("qs-wifi-password-for")).with_role(TextRole::Caption),
        ))
        .push(Box::new(field));
    if let Some(error) = &prompt.error {
        column = column.push(Box::new(
            Text::new(error)
                .with_role(TextRole::Caption)
                .with_id("qs-wifi-password-error")
                .with_live(ui::a11y::Politeness::Polite),
        ));
    } else if prompt.connecting {
        column = column.push(Box::new(
            Text::new(&t("qs-connecting"))
                .with_role(TextRole::Caption)
                .with_live(ui::a11y::Politeness::Polite),
        ));
    }
    let cancel = actions.bind_effect_invocation(
        Button::new("qs-wifi-password-cancel", &t("qs-wifi-password-cancel")),
        &QsEffect::CancelWifiPassword,
    );
    let network = ssid.to_owned();
    let submitted = password;
    let join = actions.bind_effect(
        Button::new("qs-wifi-password-connect", &t("qs-wifi-password-connect"))
            .role(ButtonRole::Primary)
            .with_enabled(!prompt.connecting),
        QsEffect::CONNECT_WIFI.map(move |(): ()| (network.clone(), submitted.with(Clone::clone))),
    );
    column
        .push(Box::new(
            Row::new()
                .no_wrap()
                .with_justify(ui::taffy::JustifyContent::FlexEnd)
                .with_rhythm(Rhythm::Related)
                .push(Box::new(cancel))
                .push(Box::new(join)),
        ))
        .into_widget()
}

/// The form for joining a network that broadcasts no name: its name and
/// password (which never leave the fields but to join it), what went wrong
/// last time, Cancel and Connect.
fn hidden_prompt(cx: &ViewCx<QuickSettings>, prompt: &crate::HiddenNetworkPrompt) -> Element {
    let actions = cx.actions();
    let name = LocalState::new(String::new());
    let password = LocalState::new(String::new());
    let mut column = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_rhythm(Rhythm::Related)
        .with_id("qs-wifi-hidden-form")
        .push(Box::new(
            Text::new(&t("qs-wifi-hidden-caption")).with_role(TextRole::Caption),
        ))
        .push(Box::new(
            TextInput::from_state(&name)
                .with_id(&format!("qs-hidden-network-name-{}", prompt.id))
                .with_placeholder(t("qs-wifi-hidden-name"))
                .disabled(prompt.connecting),
        ))
        .push(Box::new(
            TextInput::from_state(&password)
                .with_id(&format!("qs-hidden-network-password-{}", prompt.id))
                .with_secret(true)
                .with_placeholder(t("qs-wifi-password"))
                .disabled(prompt.connecting),
        ));
    if let Some(error) = &prompt.error {
        column = column.push(Box::new(
            Text::new(error)
                .with_role(TextRole::Caption)
                .with_id("qs-hidden-network-error")
                .with_live(ui::a11y::Politeness::Polite),
        ));
    } else if prompt.connecting {
        column = column.push(Box::new(
            Text::new(&t("qs-connecting"))
                .with_role(TextRole::Caption)
                .with_live(ui::a11y::Politeness::Polite),
        ));
    }
    let cancel = actions.bind_effect_invocation(
        Button::new("qs-hidden-network-cancel", &t("qs-wifi-password-cancel")),
        &QsEffect::CancelHiddenNetwork,
    );
    let join = actions.bind_effect(
        Button::new("qs-hidden-network-connect", &t("qs-wifi-password-connect"))
            .role(ButtonRole::Primary)
            .with_enabled(!prompt.connecting),
        QsEffect::CONNECT_HIDDEN_WIFI
            .map(move |(): ()| (name.with(Clone::clone), password.with(Clone::clone))),
    );
    column
        .push(Box::new(
            Row::new()
                .no_wrap()
                .with_justify(ui::taffy::JustifyContent::FlexEnd)
                .with_rhythm(Rhythm::Related)
                .push(Box::new(cancel))
                .push(Box::new(join)),
        ))
        .into_widget()
}

/// A row the desktop describes (a device, an output, a request), with the
/// buttons it offers.
fn device_row(cx: &ViewCx<QuickSettings>, face: &HostedRow) -> Element {
    let actions = cx.actions();
    let press = |part: crate::RowPart| QsEffect::PressDevice {
        row: face.key.clone(),
        part,
    };
    let mut row = ListRow::new(format!("qs-device-{}", face.key), &face.title)
        .icon(face.icon)
        .secondary(&face.detail)
        .selected(face.selected)
        .with_enabled(face.enabled);
    let mut buttons = Row::new().no_wrap().with_rhythm(Rhythm::Related);
    let mut any = false;
    match &face.actions {
        HostedActions::None | HostedActions::Clipboard { .. } => {}
        HostedActions::Select(label) => {
            any = true;
            buttons = buttons.push(Box::new(
                actions.bind_effect_invocation(
                    Button::new(&format!("qs-device-{}-action", face.key), label)
                        .with_enabled(face.enabled),
                    &press(crate::RowPart::Action),
                ),
            ));
        }
        HostedActions::Pair { first, second } => {
            any = true;
            buttons = buttons
                .push(Box::new(actions.bind_effect_invocation(
                    Button::icon_only(&format!("qs-device-{}-allow", face.key), first.0, &first.1),
                    &press(crate::RowPart::Allow),
                )))
                .push(Box::new(actions.bind_effect_invocation(
                    Button::icon_only(
                        &format!("qs-device-{}-decline", face.key),
                        second.0,
                        &second.1,
                    ),
                    &press(crate::RowPart::Decline),
                )));
        }
        HostedActions::Control {
            name,
            controlling,
            controllable,
        } => {
            any = true;
            let mut control = Button::icon_only(
                &format!("qs-device-{}-control", face.key),
                Icon::Mouse,
                name,
            )
            .toggled(*controlling)
            .with_enabled(*controllable);
            if *controlling {
                control = control.role(ButtonRole::Primary);
            }
            buttons = buttons.push(Box::new(
                actions.bind_effect_invocation(control, &press(crate::RowPart::Control)),
            ));
        }
    }
    if any {
        row = row.trailing(Box::new(buttons));
    }
    Box::new(actions.bind_effect_invocation(row, &press(crate::RowPart::Row)))
}

/// How many copies the clipboard section shows before it scrolls.
const COPIES_SHOWN: f32 = 6.0;

/// The clipboard history: a search field, the copies that match it (each
/// pasted when chosen, with its pin and remove), and Clear history. Opened
/// to paste, it is the whole panel.
pub(super) fn clipboard(cx: &ViewCx<QuickSettings>, parts: &Parts, pasting: bool) -> Element {
    let query = parts.query.clone();
    let field = TextInput::from_state(&query)
        .with_id("qs-clipboard-search")
        .with_placeholder(t("qs-clipboard-search"))
        .with_accessible_name(t("qs-clipboard-search"));
    let copies = parts.section(QsSection::Clipboard);
    let list = cx.switch(&copies, move |cx, items| {
        let query = query.clone();
        let items = items.clone();
        cx.switch(&query, move |cx, typed| copies_list(cx, &items, typed))
    });
    Box::new(
        Column::new()
            .with_intrinsic_height()
            .with_no_padding()
            .with_rhythm(Rhythm::Related)
            .with_id(if pasting {
                "qs-paste"
            } else {
                "qs-section-clipboard"
            })
            .push(Box::new(field))
            .push(list),
    )
}

fn copies_list(cx: &ViewCx<QuickSettings>, items: &[Item], typed: &str) -> Element {
    let actions = cx.actions();
    let copies: &[HostedRow] = items
        .iter()
        .find_map(|item| match item {
            Item::Copies(copies) => Some(copies.as_slice()),
            _ => None,
        })
        .unwrap_or_default();
    let shown = crate::faces::matching(copies, typed);
    let mut column = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_rhythm(Rhythm::Tight);
    if copies.is_empty() {
        return column
            .push(Box::new(
                Text::new(&t("qs-clipboard-empty-hint"))
                    .with_role(TextRole::Caption)
                    .with_id("qs-clipboard-message"),
            ))
            .into_widget();
    }
    let caption = crate::faces::copies_caption(typed, shown.len());
    if !caption.is_empty() {
        column = column.push(Box::new(
            Text::new(&caption)
                .with_role(TextRole::Caption)
                .with_id("qs-clipboard-caption")
                .with_live(ui::a11y::Politeness::Polite),
        ));
    }
    let rows: Vec<Element> = shown.into_iter().map(|copy| copy_row(cx, copy)).collect();
    let row_height = ui::tokens::Metrics::BASE.row_double;
    column = column.push(Box::new(
        Scrollable::new_vertical()
            .with_max_height(row_height * COPIES_SHOWN)
            .fit_content_height()
            .with_label(t("qs-clipboard"))
            .with_child(Box::new(Section::new(rows))),
    ));
    column
        .push(Box::new(actions.bind_effect_invocation(
            ListRow::new("qs-clipboard-clear", t("qs-clipboard-clear-history")).icon(Icon::Trash2),
            &QsEffect::ClearClipboard,
        )))
        .into_widget()
}

/// One copy: pasted when chosen, with its pin and its remove.
fn copy_row(cx: &ViewCx<QuickSettings>, copy: &HostedRow) -> Element {
    let actions = cx.actions();
    let pinned = matches!(copy.actions, HostedActions::Clipboard { pinned: true, .. });
    let pin = actions.bind_effect_invocation(
        Button::icon_only(
            &format!("qs-copy-{}-pin", copy.key),
            Icon::Pin,
            &t(if pinned {
                "qs-clipboard-unpin"
            } else {
                "qs-clipboard-pin"
            }),
        )
        .toggled(pinned),
        &QsEffect::PinCopy {
            copy: copy.key.clone(),
            pinned: !pinned,
        },
    );
    let remove = actions.bind_effect_invocation(
        Button::icon_only(
            &format!("qs-copy-{}-remove", copy.key),
            Icon::X,
            &t("qs-clipboard-remove"),
        ),
        &QsEffect::RemoveCopy {
            copy: copy.key.clone(),
        },
    );
    let row = ListRow::new(format!("qs-copy-{}", copy.key), &copy.title)
        .icon(copy.icon)
        .secondary(&copy.detail)
        .selected(copy.selected)
        .trailing(Box::new(
            Row::new()
                .no_wrap()
                .with_rhythm(Rhythm::Related)
                .push(Box::new(pin))
                .push(Box::new(remove)),
        ));
    Box::new(actions.bind_effect_invocation(
        row,
        &QsEffect::PasteCopy {
            copy: copy.key.clone(),
        },
    ))
}
