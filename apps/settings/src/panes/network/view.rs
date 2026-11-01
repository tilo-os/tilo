//! The Network page as view code ([`view`], [`Page`]): the first page — the
//! Wi-Fi list, the wired connection and the controls under them — and the
//! pages those controls open that are not a network's: the hotspot and the
//! advanced settings. The modem's card and page are built over its own
//! model (`network/mobile.rs`).

use std::rc::Rc;
use std::sync::{Arc, Mutex};

use os_actions::{ConnectivityState, SavedNetworkProfile, WifiNetworkInfo};
use ui::LocalState;
use ui::framework::{ActionControl, Actions, Dispatcher, ViewCx};
use ui::icons::Icon;
use ui::tilo_resource::Binds;
use ui::widget::{
    Button, ButtonRole, Dropdown, Element, ListRow, QrWidget, Row, SignalLevel, Spacer, TextInput,
    Toggle,
};

use super::{
    HOTSPOT_BANDS, NetworkEffect, NetworkModel, PAGE_ADVANCED, PAGE_ALL, PAGE_HOTSPOT, PAGE_KNOWN,
    PAGE_MOBILE, PAGE_USAGE, PAGE_WIFI, PAGE_WIRED, area, band_shown, connectivity_shown, mobile,
    network_detail, order, profile_label, wake_description,
};
use crate::effects::{SettingsEffect, effect};
use crate::i18n::{t, t_args};
use crate::prefs::DeviceVpnPrefs;
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use provider_settings_ui::widgets::{
    El, Item, SettingsSection, block, card, card_with_action, error_caption, error_row, field_row,
    note, row, rows, setting_row, setting_row_described, value_row,
};

/// The Network page as view code over the model the area keeps and this
/// device's VPN choices (`DeviceVpnPrefs`: the kill switches and the routes
/// through the person's devices): the page on top of the window's trail,
/// built again whenever the section changes or what the model shows does
/// ([`super::NetworkArea::poll`]). The device, and the modems, are followed
/// while the page is shown.
pub(crate) fn view(cx: &ViewCx<Settings>, app: &Rc<SettingsApp>) -> Option<Element> {
    let held = area(app)?;
    let model = Rc::clone(held.model());
    // The modem's card and page are the network page's.
    if let Some(mobile) = mobile::area(app) {
        model.borrow_mut().host_mobile(mobile.model());
    }
    let following = model.borrow_mut().follow();
    // What landed meanwhile is taken in before the page is built: the page
    // shows it.
    model.borrow_mut().poll();
    let (shown, actions, dispatcher) = (held.shown.clone(), cx.actions(), cx.dispatcher());
    cx.record(&cx.settings::<DeviceVpnPrefs>(), move |cx| {
        let (model, following, shown, actions, dispatcher) = (
            Rc::clone(&model),
            Rc::clone(&following),
            shown.clone(),
            actions.clone(),
            dispatcher.clone(),
        );
        cx.switch(
            &cx.value(|prefs: &DeviceVpnPrefs| prefs.clone()),
            move |cx, _| {
                let (model, following, actions, dispatcher) = (
                    Rc::clone(&model),
                    Rc::clone(&following),
                    actions.clone(),
                    dispatcher.clone(),
                );
                cx.switch(&shown, move |cx, _| {
                    // The part keeps the device followed while it is shown.
                    let _followed = &following;
                    Page {
                        model: &model.borrow(),
                        cx,
                        actions: actions.clone(),
                        dispatcher: dispatcher.clone(),
                    }
                    .build()
                })
            },
        )
    })
}

/// One page of Network as the view builds it: the model as it is now, the
/// view of the VPN choices it is built in, and the app's actions and
/// dispatcher its controls bind through. It reads the model's state
/// through [`std::ops::Deref`].
pub(super) struct Page<'a> {
    model: &'a NetworkModel,
    pub(super) cx: &'a ViewCx<DeviceVpnPrefs>,
    pub(super) actions: Actions<Settings>,
    dispatcher: Dispatcher<Settings>,
}

impl std::ops::Deref for Page<'_> {
    type Target = NetworkModel;

    fn deref(&self) -> &NetworkModel {
        self.model
    }
}

impl Page<'_> {
    /// `control`, invoking `effect` with each committed value.
    pub(super) fn bind<I: 'static, C: ActionControl<I>>(
        &self,
        control: C,
        effect: impl Binds<SettingsEffect, I>,
    ) -> C {
        self.actions.bind_effect(control, effect)
    }

    /// `control` (a button), invoking the fully specified `effect`.
    pub(super) fn bind_invocation<C: ActionControl<()>>(
        &self,
        control: C,
        effect: impl Into<SettingsEffect>,
    ) -> C {
        self.actions.bind_effect_invocation(control, &effect.into())
    }

    /// The binding of the fully specified `effect`, for a row's own press.
    pub(super) fn invocation_binding(
        &self,
        effect: impl Into<SettingsEffect>,
    ) -> Option<ui::tilo_actions::ActionBinding<()>> {
        self.actions.effect(&effect.into())
    }

    /// `control`, invoking `effect` with each committed value on the live
    /// row `row` ([`crate::on_row::bind`]).
    pub(super) fn bind_on_row<I: 'static, C: ActionControl<I>>(
        &self,
        row: &ui::tilo_actions::ResourceSpec,
        control: C,
        effect: impl Binds<SettingsEffect, I>,
    ) -> C {
        crate::on_row::bind(&self.dispatcher, row, control, effect)
    }

    /// `control` (a button), invoking the fully specified `effect` on the
    /// live row `row` ([`crate::on_row::bind_invocation`]).
    pub(super) fn bind_row<C: ActionControl<()>>(
        &self,
        control: C,
        row: &ui::tilo_actions::ResourceSpec,
        effect: impl Into<SettingsEffect>,
    ) -> C {
        crate::on_row::bind_invocation(&self.dispatcher, row, control, effect)
    }

    /// The binding of the fully specified `effect` on the live row `row`
    /// ([`crate::on_row::invocation`]).
    pub(super) fn row_invocation(
        &self,
        row: &ui::tilo_actions::ResourceSpec,
        effect: impl Into<SettingsEffect>,
    ) -> Option<ui::tilo_actions::ActionBinding<()>> {
        crate::on_row::invocation(&self.dispatcher, row, effect)
    }
}

impl Page<'_> {
    /// The page on top of the trail.
    pub(super) fn build(&self) -> Element {
        if !self.loaded {
            let mut pending = vec![note(&t("net-pending"))];
            if let Some(notice) = &self.notice {
                pending.push(note(notice));
            }
            return provider_settings_ui::widgets::cards(vec![card(
                &t("net-pending-title"),
                rows(pending),
            )]);
        }
        let page = self.page_key();
        let sections = match page.as_deref() {
            None => self.first_page(),
            Some(PAGE_ALL) => self.all_page(),
            Some(PAGE_KNOWN) => self.known_page(),
            Some(PAGE_HOTSPOT) => vec![self.hotspot_card()],
            Some(PAGE_ADVANCED) => self.advanced_page(),
            Some(PAGE_WIRED) => self.wired_page(),
            Some(PAGE_USAGE) => self.usage_page(),
            Some(PAGE_MOBILE) => self.mobile_page().unwrap_or_else(|| self.first_page()),
            Some(key) => match key.strip_prefix(PAGE_WIFI) {
                Some(ssid) => self.network_page(ssid),
                None => self.vpn_pages(key).unwrap_or_else(|| self.first_page()),
            },
        };
        provider_settings_ui::widgets::cards(sections)
    }

    /// A button invoking `effect`.
    pub(super) fn button(
        &self,
        id: &str,
        label: &str,
        effect: impl Into<crate::effects::SettingsEffect>,
    ) -> Button {
        self.bind_invocation(Button::new(id, label), effect)
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
        on: &ui::tilo_actions::ResourceSpec,
        id: &str,
        label: &str,
        effect: impl Into<crate::effects::SettingsEffect>,
    ) -> Item {
        row(Box::new(
            Row::new()
                .push(Box::new(self.bind_row(Button::new(id, label), on, effect)))
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
        self.bind(
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
        match self.invocation_binding(effect) {
            Some(binding) => line.on_action(binding),
            None => line.with_enabled(false),
        }
    }

    /// The modem's card on the first page, when the device has a modem.
    fn mobile_card(&self) -> Option<Vec<SettingsSection>> {
        let mobile = self.mobile.as_ref()?.try_borrow().ok()?;
        Some(mobile::Page::new(&mobile, self.cx, &self.actions).card())
            .filter(|sections| !sections.is_empty())
    }

    /// The modem's page, when the device has a modem.
    fn mobile_page(&self) -> Option<Vec<SettingsSection>> {
        let mobile = self.mobile.as_ref()?.try_borrow().ok()?;
        Some(mobile::Page::new(&mobile, self.cx, &self.actions).page_sections())
            .filter(|sections| !sections.is_empty())
    }

    /// The first page: each connection (Wi-Fi, mobile data, the cable),
    /// then the controls. A connection's details are its own page.
    fn first_page(&self) -> Vec<SettingsSection> {
        let mut cards = vec![self.wifi_card()];
        cards.extend(self.mobile_card().unwrap_or_default());
        if self.wired.device.is_some() || self.read_errors.wired.is_some() {
            cards.push(self.wired_card());
        }
        cards.push(self.controls_card());
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
        let page = self.bind_row(
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
        Box::new(match self.row_invocation(&resource, choose) {
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

    /// The controls under the connections, each a row of one list: VPN,
    /// the hotspot, airplane mode, data usage and the advanced settings. A
    /// row that opens a page has a chevron; a row with a switch also opens
    /// its page.
    fn controls_card(&self) -> SettingsSection {
        let mut lines: Vec<El> = vec![Box::new(self.drill_row(
            "settings.network.vpn",
            Icon::Shield,
            &t("net-vpn"),
            Some(self.vpn_summary()),
            super::vpn::VpnEffect::ShowVpns,
        ))];
        if !self.wifi_absent() {
            lines.push(Box::new(
                self.drill_row(
                    "settings.network.hotspot",
                    Icon::MonitorSmartphone,
                    &t("net-hotspot"),
                    None,
                    NetworkEffect::ShowHotspotSetup,
                )
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
            "settings.network.usage",
            Icon::BarChart,
            &t("net-usage"),
            self.usage_summary(),
            NetworkEffect::ShowDataUsage,
        )));
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
        provider_settings_ui::widgets::untitled(rows(content))
    }

    /// The wired connection on the first page: its switch (which
    /// `NetworkManager` applies to this interface only) and how it stands;
    /// the row opens its page.
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
        let status = self.drill_row(
            "settings.network.wired",
            Icon::EthernetPort,
            &t("net-ethernet"),
            Some(self.wired_status()),
            NetworkEffect::ShowWired,
        );
        let content = vec![row(Box::new(status))];
        card_with_action(
            &t("net-wired"),
            Box::new(self.switch_control(
                "network.wired",
                self.wired.connected,
                NetworkEffect::SET_WIRED,
            )),
            rows(content),
        )
    }

    /// How the cable stands, in a few words.
    fn wired_status(&self) -> String {
        if !self.wired.connected {
            return t("net-not-connected");
        }
        match &self.wired.speed {
            Some(s) => t_args("net-connected-speed", &[("speed", s.as_str().into())]),
            None => t("net-connected"),
        }
    }

    /// The wired connection's page: how it stands, then its profile's
    /// addresses and waking this machine over it.
    fn wired_page(&self) -> Vec<SettingsSection> {
        let mut status = vec![value_row(&t("net-ethernet"), &self.wired_status())];
        if self.wired.connected {
            // On the cable, the network's sign-in page is this page's to open.
            if !self
                .networks
                .iter()
                .any(|net| net.connected && self.wifi_enabled)
            {
                status.extend(self.portal_row());
            }
            if let Some(dev) = &self.wired.device {
                status.push(value_row(&t("net-interface"), dev));
            }
            status.push(value_row(
                &t("net-ipv4"),
                self.wired.ipv4.as_deref().unwrap_or("—"),
            ));
        } else {
            status.push(note(&t("net-wired-empty")));
        }
        let wired_profiles: Vec<&SavedNetworkProfile> = self
            .profiles
            .iter()
            .filter(|p| p.kind == "802-3-ethernet")
            .collect();
        let profile = Self::selected(&wired_profiles, &self.wired_profile);
        if let Some(usage) =
            profile.and_then(|profile| self.usage_row(&format!("wired:{}", profile.uuid)))
        {
            status.insert(1, usage);
        }
        let mut cards = vec![card_with_action(
            &t("net-status"),
            Box::new(self.switch_control(
                "network.wired",
                self.wired.connected,
                NetworkEffect::SET_WIRED,
            )),
            rows(status),
        )];
        let Some(profile) = profile else {
            return cards;
        };
        let mut settings = Vec::new();
        if wired_profiles.len() > 1 {
            settings.push(setting_row(
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
        settings.extend(self.ipv4_rows(profile, "settings.network.ipv4"));
        settings.extend(self.wake_rows(profile));
        cards.push(card(&t("net-settings"), rows(settings)));
        cards
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
                let toggle = self.bind_on_row(
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
        let button = self.bind_invocation(
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

    /// Why the radios, and so the hotspot's state, could not be read.
    fn hotspot_error_row(&self) -> Option<Item> {
        let error = self.read_errors.radios.as_ref()?;
        Some(row(error_caption(&t_args(
            "net-radios-error",
            &[("error", error.as_str().into())],
        ))))
    }

    /// The hotspot's page: its switch, name, password and band, the QR
    /// code that shares it and (while it runs) the devices on it.
    fn hotspot_card(&self) -> SettingsSection {
        let mut hotspot_rows: Vec<Item> = self.hotspot_error_row().into_iter().collect();
        hotspot_rows.extend([
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
        ]);
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
            .with_extra_bottom_padding(provider_settings_ui::widgets::spacing().rhythm.sibling)
            .push(Box::new(
                provider_settings_ui::widgets::untitled(rows(self.enterprise_rows()))
                    .into_section(),
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
