//! The pages of Wi-Fi networks: every network in range, one network's page,
//! the known networks, and joining a network that hides its name.

use os_actions::network_settings::WifiBand;
use os_actions::{SavedNetworkProfile, WifiNetworkInfo};
use ui::LocalState;
use ui::icons::Icon;
use ui::widget::{
    Button, ButtonRole, Dropdown, ListRow, NumberInput, QrWidget, RowMenu, SignalLevel, TextInput,
    Toggle,
};

use super::{
    IPV6_METHODS, MAC_POLICIES, METERED_POLICIES, NetworkEffect, NetworkModel, PROXY_MODES,
    ipv6_shown, mac_from_label, mac_id, mac_shown, metered_from_label, metered_id, metered_shown,
    order, proxy_shown, security_shown,
};
use crate::effects::effect;
use crate::i18n::{t, t_args};
use crate::widgets::{
    El, Item, SettingsSection, block, card, error_caption, note, row, rows, setting_row,
    setting_row_described, value_row,
};

/// A priority as the effect takes it: a whole number from -999 to 999.
#[expect(
    clippy::cast_possible_truncation,
    reason = "clamped to -999..=999 and rounded first"
)]
fn priority_of(value: f64) -> i32 {
    value.clamp(-999.0, 999.0).round() as i32
}

/// When a network was last joined, in words.
fn last_joined(profile: &SavedNetworkProfile) -> String {
    match profile.last_used.and_then(|at| i64::try_from(at).ok()) {
        Some(at) if at > 0 => t_args(
            "net-last-connected",
            &[(
                "when",
                ui::format::list_date(&ui::format::UnixSecs(at)).into(),
            )],
        ),
        _ => t("net-never-connected"),
    }
}

impl NetworkModel {
    /// Every network in range, narrowed by the window bar's search, and the
    /// way into one that hides its name.
    pub(super) fn all_page(&self) -> Vec<SettingsSection> {
        let mut content = self.wifi_status_rows();
        if self.wifi_enabled && !self.wifi_absent() {
            let wanted = self.filter.trim().to_lowercase();
            let arranged: Vec<WifiNetworkInfo> =
                order::arranged(&self.networks, |net| self.knows(net))
                    .into_iter()
                    .filter(|net| wanted.is_empty() || net.ssid.to_lowercase().contains(&wanted))
                    .cloned()
                    .collect();
            let shown = self
                .all_list
                .borrow_mut()
                .rows(arranged, self.held_all.get());
            if shown.is_empty() {
                content.push(note(&t(if !wanted.is_empty() {
                    "net-none-match"
                } else if self.scan_in_flight {
                    "net-scanning"
                } else {
                    "net-none-in-range"
                })));
            } else {
                content.push(self.network_list(&shown, &self.held_all));
            }
        }
        content.push(row(Box::new(self.drill_row(
            "settings.network.hidden",
            ui::icons::Icon::Plus,
            &t("net-hidden-entry"),
            None,
            NetworkEffect::ShowHiddenNetwork,
        ))));
        vec![card(
            &t_args(
                "net-in-range-count",
                &[("count", self.networks.len().into())],
            ),
            rows(content),
        )]
    }

    /// Every Wi-Fi network this device knows, the last joined first: how
    /// strong it is while in range, when it was last joined, and Forget.
    pub(super) fn known_page(&self) -> Vec<SettingsSection> {
        if let Some(error) = &self.read_errors.profiles {
            return vec![card(
                &t("net-known"),
                rows(vec![row(error_caption(&t_args(
                    "net-profiles-error",
                    &[("error", error.as_str().into())],
                )))]),
            )];
        }
        let mut known: Vec<&SavedNetworkProfile> = self.saved_wifi().collect();
        known.sort_by(|a, b| {
            b.last_used
                .cmp(&a.last_used)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        let mut content = Vec::new();
        if let Some(notice) = &self.notice {
            content.push(note(notice));
        }
        if known.is_empty() {
            content.push(note(&t("net-known-empty")));
        } else {
            let lines: Vec<El> = known
                .iter()
                .map(|profile| self.known_row(profile))
                .collect();
            content.push(block(Box::new(ui::widget::Section::new(lines))));
        }
        vec![card(
            &t_args("net-known-count", &[("count", known.len().into())]),
            rows(content),
        )]
    }

    /// One known network: the meter while it is in range, when it was last
    /// joined, and a menu to forget it. Choosing it opens its page.
    fn known_row(&self, profile: &SavedNetworkProfile) -> El {
        let resource = self.wifi_live(&profile.name).into_resource();
        let in_range = self
            .wifi_enabled
            .then(|| self.in_range(&profile.name))
            .flatten();
        let mut line = ListRow::new(
            format!("settings.network.known.{}", profile.uuid),
            profile.name.clone(),
        )
        .secondary(last_joined(profile));
        line = match in_range {
            Some(net) => line.signal(SignalLevel::from_bars(net.signal)),
            None => line
                .icon(Icon::WifiOff)
                .leading_tooltip(t("net-out-of-range")),
        };
        if in_range.is_some_and(|net| net.connected) {
            line = line.value(self.connected_state());
        }
        let menu = RowMenu::new(
            format!("settings.network.known.menu.{}", profile.uuid),
            &t_args("net-more-for", &[("name", profile.name.as_str().into())]),
        )
        .action(
            t("net-network-settings"),
            self.cx.row_invocation(
                &resource,
                NetworkEffect::OpenNetworkPage {
                    ssid: profile.name.clone(),
                },
            ),
        )
        .destructive(
            t("net-forget"),
            self.cx.row_invocation(
                &resource,
                NetworkEffect::ForgetNetwork {
                    profile: profile.uuid.clone(),
                },
            ),
        );
        line = line.trailing(Box::new(menu));
        Box::new(
            match self.cx.row_invocation(
                &resource,
                NetworkEffect::OpenNetworkPage {
                    ssid: profile.name.clone(),
                },
            ) {
                Some(binding) => line.on_action(binding),
                None => line.with_enabled(false),
            },
        )
    }

    /// A network's page: how it stands now, its settings (also while it is
    /// out of range) and what can be done with it.
    pub(super) fn network_page(&self, ssid: &str) -> Vec<SettingsSection> {
        let net = self.in_range(ssid).filter(|_| self.wifi_enabled);
        let profile = self.saved_profile_of(ssid);
        let mut sections = vec![card(
            &t("net-status"),
            rows(self.status_rows(ssid, net, profile)),
        )];
        if let Some(profile) = profile {
            sections.push(card(&t("net-settings"), rows(self.settings_rows(profile))));
            sections.push(card(&t("net-addresses"), rows(self.address_rows(profile))));
        }
        sections.push(card(
            &t("net-actions"),
            rows(self.action_rows(ssid, net, profile)),
        ));
        sections
    }

    /// How the network stands: its signal, and while it is the one in use
    /// its band, speed, security and addresses.
    fn status_rows(
        &self,
        ssid: &str,
        net: Option<&WifiNetworkInfo>,
        profile: Option<&SavedNetworkProfile>,
    ) -> Vec<Item> {
        let mut out = Vec::new();
        if let Some(notice) = &self.notice {
            out.push(note(notice));
        }
        let live = self.wifi_live(ssid).into_resource();
        let Some(net) = net else {
            out.push(row(Box::new(
                ListRow::new("settings.network.page.signal", t("net-out-of-range"))
                    .icon(Icon::WifiOff)
                    .secondary(profile.map_or_else(|| t("net-never-connected"), last_joined)),
            )));
            return out;
        };
        let state = if net.connected {
            self.connected_state()
        } else if self.connecting.contains(&net.ssid) {
            t("net-connecting")
        } else {
            t("net-in-range")
        };
        let level = SignalLevel::from_bars(net.signal);
        let mut signal = ListRow::new("settings.network.page.signal", level.name())
            .signal(level)
            .secondary(state);
        if !net.connected && !self.connecting.contains(&net.ssid) {
            signal = signal.trailing(Box::new(
                self.cx.bind_row(
                    Button::new("settings.network.page.connect", &t("net-join"))
                        .role(ButtonRole::Primary),
                    &live,
                    NetworkEffect::ConnectNetwork {
                        ssid: net.ssid.clone(),
                        password: None,
                    },
                ),
            ));
        }
        out.push(row(Box::new(signal)));
        out.push(value_row(&t("net-security"), &security_shown(net.security)));
        if net.connected {
            out.extend(self.portal_row());
            if let Some(link) = &self.link {
                if let Some(mhz) = link.frequency_mhz {
                    let band = match WifiBand::of_frequency(mhz) {
                        Some(WifiBand::TwoPointFour) => t("net-band-24"),
                        Some(WifiBand::Five) => t("net-band-5"),
                        Some(WifiBand::Six) => t("net-band-6"),
                        None => String::new(),
                    };
                    let shown = if band.is_empty() {
                        format!("{mhz} MHz")
                    } else {
                        format!("{band} ({mhz} MHz)")
                    };
                    out.push(value_row(&t("net-band"), &shown));
                }
                if let Some(rate) = &link.rate {
                    out.push(value_row(&t("net-link-speed"), rate));
                }
                let listed = |values: &[String]| values.join(", ");
                if !link.ipv4.is_empty() {
                    out.push(value_row(&t("net-ipv4"), &listed(&link.ipv4)));
                }
                if !link.ipv6.is_empty() {
                    out.push(value_row(&t("net-ipv6"), &listed(&link.ipv6)));
                }
                if let Some(gateway) = &link.gateway {
                    out.push(value_row(&t("net-gateway"), gateway));
                }
                if !link.dns.is_empty() {
                    out.push(value_row(&t("net-dns"), &listed(&link.dns)));
                }
                if let Some(mac) = &link.hardware_address {
                    out.push(value_row(&t("net-hardware-address"), mac));
                }
            }
        }
        out
    }

    /// A saved network's settings: joining it by itself and first, metered
    /// data, the address it shows, whether it hides its name, its password
    /// and, on a work or school network, who it knows this device as.
    #[expect(clippy::too_many_lines, reason = "one row per setting")]
    fn settings_rows(&self, profile: &SavedNetworkProfile) -> Vec<Item> {
        // Every setting here acts on the network's own row.
        let live = self.profile_live(profile).into_resource();
        let uuid = profile.uuid.clone();
        let mut out = vec![setting_row(
            &t("net-autoconnect"),
            Box::new(
                self.cx.bind_on_row(
                    &live,
                    Toggle::from_state(&LocalState::new(profile.autoconnect))
                        .with_id(&format!("network.profile.autoconnect.{uuid}")),
                    effect(NetworkEffect::SET_AUTOCONNECT).map({
                        let uuid = uuid.clone();
                        move |on: bool| (uuid.clone(), on)
                    }),
                ),
            ),
        )];
        let Some(settings) = self.profile_settings.get(&uuid) else {
            out.push(note(&t("net-notice-policy-unread")));
            return out;
        };
        out.push(setting_row_described(
            &t("net-priority"),
            &t("net-priority-caption"),
            Box::new(
                self.cx.bind_on_row(
                    &live,
                    NumberInput::new(f64::from(settings.priority.unwrap_or(0)))
                        .with_range(-999.0, 999.0)
                        .with_step(1.0)
                        .with_precision(0)
                        .with_id(&format!("network.profile.priority.{uuid}"))
                        .with_accessible_name(t("net-priority")),
                    effect(NetworkEffect::SET_PRIORITY).map({
                        let uuid = uuid.clone();
                        move |value: f64| (uuid.clone(), priority_of(value))
                    }),
                ),
            ),
        ));
        let metered = settings.metered.map_or("Automatic", metered_id);
        out.push(setting_row_described(
            &t("net-metered-connection"),
            &t("net-metered-caption"),
            Box::new(
                self.cx.bind_on_row(
                    &live,
                    Dropdown::from_state(
                        METERED_POLICIES
                            .iter()
                            .map(|id| metered_shown(id))
                            .collect(),
                        &LocalState::new(metered_shown(metered)),
                    )
                    .with_id(&format!("network.profile.metered.{uuid}"))
                    .with_placeholder(&metered_shown(metered)),
                    effect(NetworkEffect::SET_METERED_POLICY).map({
                        let uuid = uuid.clone();
                        move |label: String| {
                            let id = metered_from_label(&label)
                                .map_or(label, |policy| metered_id(policy).to_owned());
                            (uuid.clone(), id)
                        }
                    }),
                ),
            ),
        ));
        let mac = settings.mac.map_or("Random for this network", mac_id);
        out.push(setting_row_described(
            &t("net-hardware-address"),
            &t("net-mac-caption"),
            Box::new(
                self.cx.bind_on_row(
                    &live,
                    Dropdown::from_state(
                        MAC_POLICIES.iter().map(|id| mac_shown(id)).collect(),
                        &LocalState::new(mac_shown(mac)),
                    )
                    .with_id(&format!("network.profile.mac.{uuid}"))
                    .with_placeholder(&mac_shown(mac)),
                    effect(NetworkEffect::SET_MAC_ADDRESS).map({
                        let uuid = uuid.clone();
                        move |label: String| {
                            let id = mac_from_label(&label)
                                .map_or(label, |policy| mac_id(policy).to_owned());
                            (uuid.clone(), id)
                        }
                    }),
                ),
            ),
        ));
        out.push(setting_row_described(
            &t("net-hidden-network"),
            &t("net-hidden-network-caption"),
            Box::new(
                self.cx.bind_on_row(
                    &live,
                    Toggle::from_state(&LocalState::new(settings.hidden.unwrap_or(false)))
                        .with_id(&format!("network.profile.hidden.{uuid}")),
                    effect(NetworkEffect::SET_HIDDEN_NETWORK).map({
                        let uuid = uuid.clone();
                        move |on: bool| (uuid.clone(), on)
                    }),
                ),
            ),
        ));
        out.extend(self.password_rows(profile));
        if let (Some(_), Some(form)) = (&settings.enterprise, self.identity_forms.get(&uuid)) {
            out.extend([
                Self::text_field(
                    &form.identity,
                    &format!("network.profile.identity.{uuid}"),
                    &t("net-identity"),
                ),
                Self::text_field(
                    &form.ca,
                    &format!("network.profile.ca.{uuid}"),
                    &t("net-ca-path"),
                ),
                self.row_button_row(
                    &live,
                    &format!("settings.network.profile.identity.apply.{uuid}"),
                    &t("net-apply-identity"),
                    NetworkEffect::SetEnterpriseIdentity {
                        profile: uuid.clone(),
                        identity: None,
                        ca_certificate: None,
                    },
                ),
            ]);
        }
        out.extend(self.wake_rows(profile));
        out
    }

    /// The saved password: shown on request (the system asks the person to
    /// authenticate first) and changed in place.
    fn password_rows(&self, profile: &SavedNetworkProfile) -> Vec<Item> {
        let live = self.wifi_live(&profile.name).into_resource();
        let revealed = self
            .revealed
            .as_ref()
            .filter(|shown| shown.of == profile.uuid);
        let show = self.cx.bind_row(
            Button::icon_only(
                &format!("settings.network.profile.password.show.{}", profile.uuid),
                if revealed.is_some() {
                    Icon::EyeOff
                } else {
                    Icon::Eye
                },
                &t(if revealed.is_some() {
                    "net-hide-password"
                } else {
                    "net-show-password"
                }),
            )
            .role(ButtonRole::Plain),
            &live,
            NetworkEffect::ShowNetworkPassword {
                profile: profile.uuid.clone(),
            },
        );
        let mut out = vec![row(Box::new(
            ListRow::new(
                format!("settings.network.profile.password.{}", profile.uuid),
                t("net-password"),
            )
            .value(revealed.map_or_else(
                || t("net-password-hidden"),
                |shown| shown.secret.to_string(),
            ))
            .trailing(Box::new(show)),
        ))];
        out.push(row(Box::new(
            ui::widget::Row::new()
                .push(Box::new(self.cx.bind_row(
                    Button::new(
                        &format!("settings.network.profile.password.set.{}", profile.uuid),
                        &t("net-change-password"),
                    ),
                    &live,
                    NetworkEffect::SetNetworkPassword {
                        profile: profile.uuid.clone(),
                        password: None,
                    },
                )))
                .push(Box::new(ui::widget::Spacer::new())),
        )));
        out
    }

    /// A saved network's addresses: IPv4, its name servers, IPv6 and the
    /// proxy its connections go through.
    fn address_rows(&self, profile: &SavedNetworkProfile) -> Vec<Item> {
        // Every setting here acts on the network's own row.
        let live = self.profile_live(profile).into_resource();
        let uuid = profile.uuid.clone();
        let id = format!("settings.network.profile.ipv4.{uuid}");
        let mut out = self.ipv4_rows(profile, &id);
        if let Some(ipv4) = self
            .profile_settings
            .get(&uuid)
            .and_then(|settings| settings.ipv4.as_ref())
        {
            out.push(setting_row_described(
                &t("net-dns-servers"),
                &t("net-dns-caption"),
                Box::new(
                    self.cx.bind_on_row(
                        &live,
                        TextInput::from_state(&LocalState::new(ipv4.dns.join(", ")))
                            .with_id(&format!("network.profile.dns.{uuid}"))
                            .with_placeholder(t("net-dns-automatic")),
                        effect(NetworkEffect::SET_DNS).map({
                            let uuid = uuid.clone();
                            move |servers: String| (uuid.clone(), servers)
                        }),
                    ),
                ),
            ));
        }
        if let Some(form) = self.ipv6_forms.get(&uuid) {
            out.extend([
                setting_row(
                    &t("net-ipv6-method"),
                    Box::new(
                        Dropdown::from_state(
                            IPV6_METHODS.iter().map(|id| ipv6_shown(id)).collect(),
                            &form.method,
                        )
                        .with_id(&format!("network.profile.ipv6.method.{uuid}"))
                        .with_placeholder(&form.method.get()),
                    ),
                ),
                Self::text_field(
                    &form.addresses,
                    &format!("network.profile.ipv6.address.{uuid}"),
                    &t("net-ipv6-addresses"),
                ),
                Self::text_field(
                    &form.gateway,
                    &format!("network.profile.ipv6.gateway.{uuid}"),
                    &t("net-gateway"),
                ),
                self.row_button_row(
                    &live,
                    &format!("settings.network.profile.ipv6.apply.{uuid}"),
                    &t("net-apply-ipv6"),
                    NetworkEffect::SetIpv6 {
                        profile: uuid.clone(),
                        method: None,
                        addresses: None,
                        gateway: None,
                    },
                ),
            ]);
        }
        if let Some(form) = self.proxy_forms.get(&uuid) {
            out.extend([
                setting_row(
                    &t("net-proxy"),
                    Box::new(
                        Dropdown::from_state(
                            PROXY_MODES.iter().map(|id| proxy_shown(id)).collect(),
                            &form.mode,
                        )
                        .with_id(&format!("network.profile.proxy.mode.{uuid}"))
                        .with_placeholder(&form.mode.get()),
                    ),
                ),
                Self::text_field(
                    &form.address,
                    &format!("network.profile.proxy.address.{uuid}"),
                    &t("net-proxy-address"),
                ),
                self.row_button_row(
                    &live,
                    &format!("settings.network.profile.proxy.apply.{uuid}"),
                    &t("net-apply-proxy"),
                    NetworkEffect::SetProxy {
                        profile: uuid.clone(),
                        mode: None,
                        address: None,
                    },
                ),
                note(&t("net-proxy-caption")),
            ]);
        }
        out
    }

    /// What can be done with a network: share it by QR code, with the
    /// person's own devices, and forget it.
    fn action_rows(
        &self,
        ssid: &str,
        net: Option<&WifiNetworkInfo>,
        profile: Option<&SavedNetworkProfile>,
    ) -> Vec<Item> {
        let live = self.wifi_live(ssid).into_resource();
        let mut out = Vec::new();
        if net.is_some() {
            let shown = self.code.as_ref().filter(|code| code.of == ssid);
            out.push(setting_row_described(
                &t("net-share-qr-title"),
                &t("net-share-qr-caption"),
                Box::new(
                    self.cx.bind_row(
                        Button::new(
                            "settings.network.page.qr",
                            &t(if shown.is_some() {
                                "net-hide-qr"
                            } else {
                                "net-share-qr"
                            }),
                        )
                        .with_icon(Icon::QrCode),
                        &live,
                        NetworkEffect::ShowNetworkCode {
                            ssid: ssid.to_owned(),
                        },
                    ),
                ),
            ));
            if let Some(code) = shown {
                out.push(block(Box::new(QrWidget::new(
                    code.secret.as_bytes().to_vec(),
                ))));
                out.push(note(&t("net-qr-network-caption")));
            }
        }
        if let Some(profile) = profile {
            out.push(setting_row_described(
                &t("net-share-devices"),
                &t("net-share-caption"),
                Box::new(self.cx.bind_row(
                    Button::new(
                        &format!("settings.network.profile.share.{}", profile.uuid),
                        &t("net-share"),
                    ),
                    &live,
                    NetworkEffect::ShareNetwork {
                        profile: profile.uuid.clone(),
                    },
                )),
            ));
            out.push(setting_row(
                &t("net-forget-network"),
                Box::new(
                    self.cx.bind_row(
                        Button::new(
                            &format!("settings.network.profile.forget.{}", profile.uuid),
                            &t("net-forget"),
                        )
                        .role(ButtonRole::Destructive),
                        &live,
                        NetworkEffect::ForgetNetwork {
                            profile: profile.uuid.clone(),
                        },
                    ),
                ),
            ));
        }
        if out.is_empty() {
            out.push(note(&t("net-gone")));
        }
        out
    }
}
