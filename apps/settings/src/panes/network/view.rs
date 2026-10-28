//! The pane's first page — the Wi-Fi list, the wired connection and the
//! controls under them — and the pages those controls open that are not a
//! network's: the hotspot and the advanced settings. The modem's card and
//! page are composed by its own model (`network/mobile.rs`).

use std::sync::{Arc, Mutex};

use os_actions::{ConnectivityState, SavedNetworkProfile, WifiNetworkInfo};
use ui::LocalState;
use ui::icons::Icon;
use ui::widget::{
    Button, ButtonRole, Dropdown, ListRow, QrWidget, Row, SignalLevel, Spacer, TextInput, Toggle,
    Widget,
};

use super::{
    HOTSPOT_BANDS, NetworkEffect, NetworkModel, PAGE_ADVANCED, PAGE_ALL, PAGE_HOTSPOT, PAGE_KNOWN,
    PAGE_MOBILE, PAGE_WIFI, band_shown, connectivity_shown, network_detail, order, profile_label,
    wake_description,
};
use crate::effects::effect;
use crate::i18n::{t, t_args};
use crate::widgets::{
    El, Item, SettingsSection, block, card, card_with_action, error_caption, error_row, field_row,
    note, row, rows, setting_row, setting_row_described, value_row,
};

impl NetworkModel {
    /// The page on top of the trail, composed.
    pub(super) fn build_composition(&self) -> Box<dyn Widget> {
        if !self.loaded {
            let mut pending = vec![note(&t("net-pending"))];
            if let Some(notice) = &self.notice {
                pending.push(note(notice));
            }
            return crate::widgets::cards(vec![card(&t("net-pending-title"), rows(pending))]);
        }
        let page = self.page_key();
        let sections = match page.as_deref() {
            None => self.first_page(),
            Some(PAGE_ALL) => self.all_page(),
            Some(PAGE_KNOWN) => self.known_page(),
            Some(PAGE_HOTSPOT) => vec![self.hotspot_card()],
            Some(PAGE_ADVANCED) => self.advanced_page(),
            Some(PAGE_MOBILE) => self
                .mobile_sections(super::mobile::MobileModel::page_sections)
                .unwrap_or_else(|| self.first_page()),
            Some(key) => match key.strip_prefix(PAGE_WIFI) {
                Some(ssid) => self.network_page(ssid),
                None => self.vpn_pages(key).unwrap_or_else(|| self.first_page()),
            },
        };
        crate::widgets::cards(sections)
    }

    /// A button invoking `effect`.
    pub(super) fn button(
        &self,
        id: &str,
        label: &str,
        effect: impl Into<crate::effects::SettingsEffect>,
    ) -> Button {
        self.cx.bind_invocation(Button::new(id, label), effect)
    }

    /// `button`, sized to its label and pushed to the row's start.
    pub(super) fn button_row(
        &self,
        id: &str,
        label: &str,
        effect: impl Into<crate::effects::SettingsEffect>,
    ) -> Item {
        row(Box::new(
            Row::new()
                .push(Box::new(self.button(id, label, effect)))
                .push(Box::new(Spacer::new())),
        ))
    }

    /// `button_row` acting on the live row `on`, which offers `effect`.
    pub(super) fn row_button_row(
        &self,
        on: &ui::tilo_actions::Resource,
        id: &str,
        label: &str,
        effect: impl Into<crate::effects::SettingsEffect>,
    ) -> Item {
        row(Box::new(
            Row::new()
                .push(Box::new(self.cx.bind_row(
                    Button::new(id, label),
                    on,
                    effect,
                )))
                .push(Box::new(Spacer::new())),
        ))
    }

    /// A switch showing `on`, invoking `set`.
    pub(super) fn switch_control(
        &self,
        id: &str,
        on: bool,
        set: ui::tilo_resource::ActionRef<NetworkEffect, bool>,
    ) -> Toggle {
        self.cx.bind(
            Toggle::from_state(&LocalState::new(on)).with_id(id),
            effect(set),
        )
    }

    /// A row that drills into a page: `effect` opens it.
    pub(super) fn drill_row(
        &self,
        id: &str,
        icon: Icon,
        title: &str,
        value: Option<String>,
        effect: impl Into<crate::effects::SettingsEffect>,
    ) -> ListRow {
        let mut line = ListRow::new(id, title).icon(icon).disclosure();
        if let Some(value) = value {
            line = line.value(value);
        }
        match self.cx.invocation_binding(effect) {
            Some(binding) => line.on_action(binding),
            None => line.with_enabled(false),
        }
    }

    /// What the modem's model composes, when the device has a modem.
    fn mobile_sections(
        &self,
        compose: impl FnOnce(&super::mobile::MobileModel) -> Vec<SettingsSection>,
    ) -> Option<Vec<SettingsSection>> {
        let mobile = self.mobile.as_ref()?.try_borrow().ok()?;
        Some(compose(&mobile)).filter(|sections| !sections.is_empty())
    }

    /// The first page: Wi-Fi and mobile data, the controls, and the wired
    /// connection.
    fn first_page(&self) -> Vec<SettingsSection> {
        // The controls follow the radios at once, so they stay in view on a
        // normal window; the wired connection comes after them.
        let mut cards = vec![self.wifi_card()];
        cards.extend(
            self.mobile_sections(super::mobile::MobileModel::card)
                .unwrap_or_default(),
        );
        cards.push(self.controls_card());
        if self.wired.device.is_some() || self.read_errors.wired.is_some() {
            cards.push(self.wired_card());
        }
        cards
    }

    /// Wi-Fi: the switch says whether the radio is on (the machine's state,
    /// read back after every change); under it the short list of networks
    /// ([`order::shortlist`]) and the way to every other one.
    fn wifi_card(&self) -> SettingsSection {
        let wifi_toggle =
            self.switch_control("network.wifi", self.wifi_enabled, NetworkEffect::SET_WIFI);
        // An unread radio state, or a radio a switch holds off, is not
        // offered as a switch position.
        let usable = self.read_errors.wifi_enabled.is_none()
            && !self.wifi_absent()
            && !self.radios.wifi_hardware_blocked;
        // Only ever turned off here: the binding already turned off a switch
        // whose action is not offered.
        let wifi_toggle: El = if usable {
            Box::new(wifi_toggle)
        } else {
            Box::new(wifi_toggle.with_enabled(false))
        };
        let mut wifi_rows = self.wifi_status_rows();
        if self.wifi_enabled && !self.wifi_absent() {
            let arranged = order::arranged(&self.networks, |net| self.knows(net));
            let short = order::shortlist(&arranged, |net| self.knows(net));
            let shown = self
                .short_list
                .borrow_mut()
                .rows(short.into_iter().cloned().collect(), self.held_short.get());
            if !shown.is_empty() {
                wifi_rows.push(self.network_list(&shown, &self.held_short));
            }
            if self.networks.is_empty() && self.read_errors.networks.is_none() {
                wifi_rows.push(note(&t(if self.scan_in_flight {
                    "net-scanning"
                } else {
                    "net-none-in-range"
                })));
            }
            wifi_rows.push(row(Box::new(self.drill_row(
                "settings.network.all",
                Icon::List,
                &t_args("net-show-all", &[("count", self.networks.len().into())]),
                None,
                NetworkEffect::ShowAllNetworks,
            ))));
        }
        if self.in_flight > 0 {
            wifi_rows.push(note(&t("net-applying")));
        }
        card_with_action(&t("net-wifi"), wifi_toggle, rows(wifi_rows))
    }

    /// Why the Wi-Fi list is empty or cannot be told, and what the pane
    /// last said.
    pub(super) fn wifi_status_rows(&self) -> Vec<Item> {
        let mut out = Vec::new();
        if self.wifi_absent() {
            out.push(note(&t("net-no-wifi-adapter")));
        } else if let Some(error) = self.wifi_status_error() {
            out.push(error_row(&t("net-status"), &error));
        } else if !self.wifi_enabled {
            out.push(note(&t(if self.radios.wifi_hardware_blocked {
                "net-wifi-hardware-switch"
            } else if self.radios.airplane_mode && self.read_errors.radios.is_none() {
                "net-now-airplane"
            } else {
                "net-wifi-off"
            })));
        }
        if let Some(notice) = &self.notice {
            out.push(note(notice));
        }
        out
    }

    /// `networks` as one list, held in place while the pointer is over it.
    pub(super) fn network_list(
        &self,
        networks: &[WifiNetworkInfo],
        held: &std::rc::Rc<std::cell::Cell<bool>>,
    ) -> Item {
        let lines: Vec<El> = networks.iter().map(|net| self.network_row(net)).collect();
        block(Box::new(
            ui::widget::Section::new(lines).pointer_over(std::rc::Rc::clone(held)),
        ))
    }

    /// One network in range: how strong it is, its name, whether it is
    /// secured, and its state. Choosing it joins it (a password it needs is
    /// asked for in place, under it); the chevron opens its page.
    fn network_row(&self, net: &WifiNetworkInfo) -> El {
        let detail = network_detail(net);
        let level = SignalLevel::from_bars(net.signal);
        let resource = self.wifi_live(&net.ssid).into_resource();
        let page = self.cx.bind_row(
            Button::icon_only(
                &format!("settings.network.page.{}", net.ssid),
                Icon::ChevronRight,
                &t_args(
                    "net-network-settings-of",
                    &[("ssid", net.ssid.as_str().into())],
                ),
            )
            .role(ButtonRole::Plain),
            &resource,
            NetworkEffect::OpenNetworkPage {
                ssid: net.ssid.clone(),
            },
        );
        let mut line = ListRow::new(
            format!("settings.network.connect.{}", net.ssid),
            net.ssid.clone(),
        )
        .signal(level)
        .secondary(detail)
        .trailing_disclosure(Box::new(page));
        if net.secure {
            line = line.mark(Icon::Lock);
        }
        let choose = if net.connected {
            line = line.value(self.connected_state());
            NetworkEffect::OpenNetworkPage {
                ssid: net.ssid.clone(),
            }
        } else if self.connecting.contains(&net.ssid) {
            line = line.value(t("net-connecting"));
            return Box::new(line);
        } else {
            if self.knows(net) {
                line = line.value(t("net-known-short"));
            }
            NetworkEffect::ConnectNetwork {
                ssid: net.ssid.clone(),
                password: None,
            }
        };
        Box::new(match self.cx.row_invocation(&resource, choose) {
            Some(binding) => line.on_action(binding),
            None => line.with_enabled(false),
        })
    }

    /// How the network in use stands: connected, or connected but without
    /// the internet, or waiting for its sign-in page.
    pub(super) fn connected_state(&self) -> String {
        t(match self.connectivity {
            ConnectivityState::Portal => "net-now-portal",
            ConnectivityState::Limited | ConnectivityState::Offline => "net-connected-no-internet",
            ConnectivityState::Unknown | ConnectivityState::Full => "net-connected",
        })
    }

    /// The sign-in page of the network in use, while it asks for one.
    pub(super) fn portal_row(&self) -> Option<Item> {
        (self.connectivity == ConnectivityState::Portal).then(|| {
            setting_row(
                &t("net-now-portal"),
                Box::new(
                    self.button(
                        "settings.network.captive-portal",
                        &t("net-open-signin"),
                        NetworkEffect::OpenNetworkSignIn,
                    )
                    .role(ButtonRole::Primary),
                ),
            )
        })
    }

    /// The controls under the Wi-Fi list, each a row of one list: the known
    /// networks, VPN, the hotspot, airplane mode and the
    /// advanced settings. A row that opens a page has a chevron; a row
    /// with a switch also opens its page.
    fn controls_card(&self) -> SettingsSection {
        let saved = self.saved_wifi().count();
        let mut lines: Vec<El> = vec![
            Box::new(self.drill_row(
                "settings.network.known",
                Icon::History,
                &t_args("net-known-count", &[("count", saved.into())]),
                None,
                NetworkEffect::ShowKnownNetworks,
            )),
            Box::new(self.drill_row(
                "settings.network.vpn",
                Icon::Shield,
                &t("net-vpn"),
                Some(self.vpn_summary()),
                super::vpn::VpnEffect::ShowVpns,
            )),
        ];
        if !self.wifi_absent() {
            lines.push(Box::new(
                self.drill_row(
                    "settings.network.hotspot",
                    Icon::MonitorSmartphone,
                    &t("net-hotspot"),
                    None,
                    NetworkEffect::ShowHotspotSetup,
                )
                .secondary(t(if self.radios.hotspot_active {
                    "net-hotspot-active"
                } else {
                    "net-hotspot-off"
                }))
                .trailing(Box::new(self.switch_control(
                    "network.hotspot",
                    self.radios.hotspot_active,
                    NetworkEffect::SET_HOTSPOT,
                ))),
            ));
        }
        lines.push(Box::new(
            ListRow::new("settings.network.airplane", t("net-airplane"))
                .icon(Icon::Plane)
                .secondary(t("net-airplane-caption"))
                .trailing(Box::new({
                    let toggle = self.switch_control(
                        "network.airplane",
                        self.radios.airplane_mode,
                        NetworkEffect::SET_AIRPLANE_MODE,
                    );
                    // An unread radio state is not offered as a position.
                    if self.read_errors.radios.is_none() {
                        toggle
                    } else {
                        toggle.with_enabled(false)
                    }
                })),
        ));
        lines.push(Box::new(self.drill_row(
            "settings.network.advanced",
            Icon::Settings2,
            &t("net-advanced"),
            None,
            NetworkEffect::ShowAdvancedNetworkSettings,
        )));
        let mut content = Vec::new();
        if let Some(error) = &self.read_errors.radios {
            content.push(row(error_caption(&t_args(
                "net-radios-error",
                &[("error", error.as_str().into())],
            ))));
        }
        content.push(block(Box::new(ui::widget::Section::new(lines))));
        crate::widgets::untitled(rows(content))
    }

    /// Wired card. `NetworkManager` applies the switch to this interface
    /// only; Wi-Fi and VPN stay untouched.
    fn wired_card(&self) -> SettingsSection {
        if let Some(error) = &self.read_errors.wired {
            return card(
                &t("net-wired"),
                rows(vec![row(error_caption(&t_args(
                    "net-wired-error",
                    &[("error", error.as_str().into())],
                )))]),
            );
        }
        if self.wired.device.is_some() {
            card_with_action(
                &t("net-wired"),
                Box::new(self.switch_control(
                    "network.wired",
                    self.wired.connected,
                    NetworkEffect::SET_WIRED,
                )),
                rows(self.wired_rows()),
            )
        } else {
            card(&t("net-wired"), rows(self.wired_rows()))
        }
    }

    fn selected<'a>(
        profiles: &[&'a SavedNetworkProfile],
        selection: &LocalState<String>,
    ) -> Option<&'a SavedNetworkProfile> {
        let label = selection.get();
        profiles
            .iter()
            .find(|p| profile_label(p) == label)
            .or_else(|| profiles.first())
            .copied()
    }

    /// Rows for the Wired card: the link, and the selected wired profile's
    /// IPv4 settings.
    fn wired_rows(&self) -> Vec<Item> {
        if !self.wired.connected {
            return vec![
                value_row(&t("net-ethernet"), &t("net-not-connected")),
                note(&t("net-wired-empty")),
            ];
        }
        let mut out = Vec::new();
        let status = match &self.wired.speed {
            Some(s) => t_args("net-connected-speed", &[("speed", s.as_str().into())]),
            None => t("net-connected"),
        };
        out.push(value_row(&t("net-ethernet"), &status));
        // On the cable, the network's sign-in page is this card's to open.
        if !self
            .networks
            .iter()
            .any(|net| net.connected && self.wifi_enabled)
        {
            out.extend(self.portal_row());
        }
        if let Some(dev) = &self.wired.device {
            out.push(value_row(&t("net-interface"), dev));
        }
        out.push(value_row(
            &t("net-ipv4"),
            self.wired.ipv4.as_deref().unwrap_or("—"),
        ));
        let wired_profiles: Vec<&SavedNetworkProfile> = self
            .profiles
            .iter()
            .filter(|p| p.kind == "802-3-ethernet")
            .collect();
        let Some(profile) = Self::selected(&wired_profiles, &self.wired_profile) else {
            return out;
        };
        if wired_profiles.len() > 1 {
            out.push(setting_row(
                &t("net-edit-profile"),
                Box::new(
                    Dropdown::from_state(
                        wired_profiles.iter().map(|p| profile_label(p)).collect(),
                        &self.wired_profile,
                    )
                    .with_placeholder(&profile_label(profile)),
                ),
            ));
        }
        out.extend(self.ipv4_rows(profile, "settings.network.ipv4"));
        out.extend(self.wake_rows(profile));
        out
    }

    /// The IPv4 form of `profile` and its Apply.
    pub(super) fn ipv4_rows(&self, profile: &SavedNetworkProfile, id: &str) -> Vec<Item> {
        let Some(form) = self.ipv4_forms.get(&profile.uuid) else {
            return vec![note(&t("net-ipv4-unsupported"))];
        };
        let mut out = vec![setting_row(
            &t("net-auto-ipv4"),
            Box::new(Toggle::from_state(&form.automatic).with_id(&format!("{id}.automatic"))),
        )];
        out.extend([
            Self::text_field(
                &form.addresses,
                &format!("{id}.address"),
                &t("net-address-prefix"),
            ),
            Self::text_field(&form.gateway, &format!("{id}.gateway"), &t("net-gateway")),
            self.button_row(
                &format!("{id}.apply"),
                &t("net-apply-ip"),
                NetworkEffect::SetIpv4 {
                    profile: profile.uuid.clone(),
                    automatic: None,
                    addresses: None,
                    gateway: None,
                    dns: None,
                },
            ),
            note(&t("net-ipv4-caption")),
        ]);
        out
    }

    /// Whether `profile` wakes this machine over the network, where the
    /// host can tell.
    pub(super) fn wake_rows(&self, profile: &SavedNetworkProfile) -> Vec<Item> {
        let label = t_args(
            "net-wake-network",
            &[("name", profile.name.as_str().into())],
        );
        match self.wake_settings.get(&profile.uuid) {
            Some(Ok(status)) => {
                let uuid = profile.uuid.clone();
                let live = self.profile_live(profile).into_resource();
                let toggle = self.cx.bind_on_row(
                    &live,
                    Toggle::from_state(&LocalState::new(status.configured))
                        .with_id(&format!("network.profile.wake.{}", profile.uuid))
                        .with_accessible_name(&label)
                        .with_enabled(status.configured || status.supported == Some(true)),
                    effect(NetworkEffect::SET_NETWORK_WAKE).map(move |on: bool| (uuid.clone(), on)),
                );
                let mut rows = vec![setting_row_described(
                    &label,
                    &t(wake_description(status)),
                    Box::new(toggle),
                )];
                if let Some(problem) = &status.problem {
                    rows.push(row(error_caption(problem)));
                }
                rows
            }
            Some(Err(error)) => vec![
                value_row(&label, &t("net-wake-unavailable")),
                row(error_caption(error)),
            ],
            None => Vec::new(),
        }
    }

    /// A secret held in `buffer`, which never leaves this process: whether
    /// it is given, and a button asking for it with the password prompt.
    pub(super) fn secret_row(
        &self,
        id: &str,
        label: &str,
        buffer: &Arc<Mutex<String>>,
        ask: impl Into<crate::effects::SettingsEffect>,
    ) -> Item {
        let given = buffer.lock().is_ok_and(|held| !held.is_empty());
        let button = self.cx.bind_invocation(
            Button::new(
                id,
                &t(if given {
                    "net-secret-change"
                } else {
                    "net-secret-enter"
                }),
            ),
            ask,
        );
        row(Box::new(
            ListRow::new(format!("{id}.row"), label.to_owned())
                .value(t(if given {
                    "net-secret-given"
                } else {
                    "net-secret-none"
                }))
                .trailing(Box::new(button)),
        ))
    }

    /// A text field over `state`.
    pub(super) fn text_field(state: &LocalState<String>, id: &str, placeholder: &str) -> Item {
        field_row(
            placeholder,
            Box::new(
                TextInput::from_state(state)
                    .with_id(id)
                    .with_placeholder(placeholder),
            ),
        )
    }

    /// The hotspot's on/off state, or why the radios could not be read.
    fn hotspot_status_row(&self) -> Item {
        let status = t("net-status");
        if let Some(error) = &self.read_errors.radios {
            return error_row(
                &status,
                &t_args("net-radios-error", &[("error", error.as_str().into())]),
            );
        }
        let value = if self.radios.hotspot_active {
            t("net-hotspot-active")
        } else {
            t("net-hotspot-off")
        };
        value_row(&status, &value)
    }

    /// The hotspot's page: its switch, name, password and band, the QR
    /// code that shares it and (while it runs) the devices on it.
    fn hotspot_card(&self) -> SettingsSection {
        let mut hotspot_rows: Vec<Item> = vec![
            self.hotspot_status_row(),
            field_row(
                &t("net-hotspot-name"),
                Box::new(
                    TextInput::from_state(&self.hotspot_name)
                        .with_id("network.hotspot.ssid")
                        .with_placeholder(t("net-hotspot-name")),
                ),
            ),
            self.secret_row(
                "network.hotspot.password",
                &t("net-hotspot-password"),
                &self.hotspot_password,
                NetworkEffect::AskHotspotPassword,
            ),
            setting_row(
                &t("net-band"),
                Box::new(
                    Dropdown::from_state(
                        HOTSPOT_BANDS.iter().map(|band| band_shown(band)).collect(),
                        &self.hotspot_band,
                    )
                    .with_id("network.hotspot.band")
                    .with_placeholder(&self.hotspot_band.get()),
                ),
            ),
            row(Box::new(
                Row::new()
                    .push(Box::new(self.button(
                        "settings.network.hotspot.apply",
                        &t("net-apply-start"),
                        NetworkEffect::StartHotspot {
                            name: None,
                            password: None,
                            band: None,
                        },
                    )))
                    .push(Box::new(self.button(
                        "settings.network.hotspot.qr",
                        &if self.hotspot_code_shown {
                            t("net-hide-qr")
                        } else {
                            t("net-share-qr")
                        },
                        NetworkEffect::ShowHotspotCode,
                    )))
                    .push(Box::new(Spacer::new())),
            )),
        ];
        if self.hotspot_code_shown
            && let Ok(payload) = self
                .hotspot(None, None, None)
                .and_then(|config| config.qr_payload())
        {
            hotspot_rows.push(block(Box::new(QrWidget::new(payload.into_bytes()))));
            hotspot_rows.push(note(&t("net-qr-caption")));
        }
        if let Some(error) = &self.read_errors.hotspot_clients
            && self.radios.hotspot_active
        {
            hotspot_rows.push(row(error_caption(&t_args(
                "net-hotspot-clients-error",
                &[("error", error.as_str().into())],
            ))));
        } else if self.radios.hotspot_active {
            let clients = t("net-hotspot-clients");
            hotspot_rows.push(value_row(&clients, &self.hotspot_clients.len().to_string()));
            for client in &self.hotspot_clients {
                let signal = client
                    .signal_dbm
                    .map_or_else(|| t("net-signal-unknown"), |dbm| format!("{dbm} dBm"));
                hotspot_rows.push(value_row(&client.mac, &signal));
            }
            hotspot_rows.push(note(&t("net-hotspot-clients-caption")));
        }
        card_with_action(
            &t("net-hotspot"),
            Box::new(self.switch_control(
                "network.hotspot",
                self.radios.hotspot_active,
                NetworkEffect::SET_HOTSPOT,
            )),
            rows(hotspot_rows),
        )
    }

    /// The advanced settings: work or school networks, diagnostics. The
    /// work or school network's setup opens in place under its row.
    fn advanced_page(&self) -> Vec<SettingsSection> {
        let setup = ui::widget::Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_extra_bottom_padding(crate::widgets::spacing().rhythm.sibling)
            .push(Box::new(
                crate::widgets::untitled(rows(self.enterprise_rows())).into_section(),
            ));
        let enterprise = ui::widget::DisclosureRow::new(t("net-enterprise-entry"), Box::new(setup))
            .with_state(&self.enterprise_setup)
            .icon(Icon::Landmark)
            .secondary(t("net-enterprise-entry-detail"));
        vec![
            card(
                &t("net-enterprise-title"),
                rows(vec![row(Box::new(enterprise))]),
            ),
            card(&t("net-diag-title"), rows(self.diagnostic_rows())),
        ]
    }

    fn diagnostic_rows(&self) -> Vec<Item> {
        let mut out: Vec<Item> = vec![setting_row_described(
            &t("net-diag-check"),
            &t("net-diag-check-caption"),
            Box::new(self.button(
                "settings.network.diagnostics",
                &t("net-run-diagnostics"),
                NetworkEffect::RunDiagnostics,
            )),
        )];
        if let Some(d) = &self.diagnostics {
            out.push(value_row("NetworkManager", &d.network_manager_state));
            out.push(value_row(
                &t("net-diag-internet"),
                &connectivity_shown(d.connectivity),
            ));
            if let Some(v) = &d.device {
                out.push(value_row(&t("net-diag-device"), v));
            }
            if let Some(v) = &d.address {
                out.push(value_row(&t("net-diag-address"), v));
            }
            if let Some(v) = &d.gateway {
                out.push(value_row(&t("net-diag-gateway"), v));
            }
            let dns = if d.dns.is_empty() {
                t("net-diag-dns-none")
            } else {
                d.dns.join(", ")
            };
            out.push(value_row(&t("net-diag-dns"), &dns));
            for recommendation in &d.recommendations {
                out.push(note(recommendation));
            }
        } else {
            out.push(note(&t("net-diag-idle")));
        }
        out
    }

    fn enterprise_rows(&self) -> Vec<Item> {
        let form = &self.enterprise;
        vec![
            setting_row(
                &t("net-method"),
                Box::new(
                    Dropdown::from_state(
                        vec!["PEAP".into(), "TTLS".into(), "TLS".into()],
                        &form.method,
                    )
                    .with_placeholder(&form.method.get()),
                ),
            ),
            setting_row(
                &t("net-inner-auth"),
                Box::new(
                    Dropdown::from_state(
                        vec!["MSCHAPv2".into(), "PAP".into(), "CHAP".into()],
                        &form.inner_auth,
                    )
                    .with_placeholder(&form.inner_auth.get()),
                ),
            ),
            Self::text_field(
                &form.profile,
                "network.enterprise.profile",
                &t("net-profile-name"),
            ),
            Self::text_field(
                &form.client_certificate,
                "network.enterprise.client_cert",
                &t("net-client-cert"),
            ),
            Self::text_field(
                &form.private_key,
                "network.enterprise.private_key",
                &t("net-private-key"),
            ),
            Self::text_field(&form.ssid, "network.enterprise.ssid", &t("net-ssid")),
            Self::text_field(
                &form.identity,
                "network.enterprise.identity",
                &t("net-identity"),
            ),
            Self::text_field(
                &form.anonymous,
                "network.enterprise.anonymous",
                &t("net-anonymous"),
            ),
            Self::text_field(&form.ca, "network.enterprise.ca", &t("net-ca-path")),
            self.button_row(
                "settings.network.enterprise.ca.choose",
                &t("net-choose-ca"),
                NetworkEffect::ChooseCaCertificate,
            ),
            self.button_row(
                "settings.network.enterprise.connect",
                &t("net-save-connect"),
                NetworkEffect::ConnectEnterpriseNetwork {
                    profile: None,
                    ssid: None,
                    identity: None,
                    anonymous_identity: None,
                    ca_certificate: None,
                    method: None,
                    inner_auth: None,
                    client_certificate: None,
                    private_key: None,
                    password: None,
                },
            ),
        ]
    }
}
