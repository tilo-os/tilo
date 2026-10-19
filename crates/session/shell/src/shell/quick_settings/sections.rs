//! What Quick Settings' in-place sections hold: Wi-Fi's networks, the
//! paired Bluetooth accessories, the VPN profiles, the sound outputs, the
//! battery, and the rows of Devices and the Clipboard (whose faces the
//! overlay host paints, since it holds the device fabric and the history).
//!
//! A section is laid out once at its full height, as a list of items with
//! rects relative to its own top-left corner; the root page places it under
//! the control it belongs to and reveals it with the disclosure motion
//! ([`ui::motion::Disclose`]). Paint, hit-testing, keyboard focus and the
//! accessibility tree all read the same items, so they cannot disagree.

use vello::kurbo::{Affine, Point, Rect};

use ui::TextRole;
use ui::paint::PaintCtx;
use ui::text_render::TextWeight;
use ui::widget::{
    Button, ButtonFace, ButtonRole, Interaction, ListRowFace, RowLeading, SegmentedFace, Toggle,
    ToggleFace,
};

use crate::shell::chrome_state::QuickSettingsState;
use crate::shell::i18n::{t, t_args};
use crate::shell::quick_settings::{PowerProfile, QsSection, WifiNetwork};
use tilo_desktop::drill_in::{RELATED_GAP, TOGGLE_H, TOGGLE_W};

/// A focus stop inside the open section. Indices name a network, an
/// accessory or a profile in the state's own lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SectionStop {
    /// "Turn on" while the radio is off.
    TurnOn,
    /// A network in `wifi_networks`.
    Network(usize),
    PasswordCancel,
    PasswordConnect,
    /// The section's way into Settings.
    Settings,
    /// An accessory in `bt_devices`: connect or disconnect.
    Device(usize),
    /// A connected accessory's use on the person's other devices.
    DeviceDetails(usize),
    /// A profile in `vpns`.
    Profile(usize),
    /// One of the three power modes.
    Mode(PowerProfile),
    /// A sound output in `sound_outputs`: play sound there.
    Output(usize),
    /// A row the overlay host paints: a Devices row or a clipboard copy.
    Hosted(usize),
    /// Clear the clipboard history.
    ClearClipboard,
}

/// The sizes a section is laid out with, from the root page's metrics.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SectionMetrics {
    /// A single-line row.
    pub row: f64,
    /// A row with a second line.
    pub two_line: f64,
    /// A button or field.
    pub control: f64,
    /// One caption line.
    pub caption: f64,
    /// One title line, as the battery's charge is set.
    pub title: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ItemKind {
    /// A sentence standing for an empty or unavailable list.
    Message(String),
    /// A caption over a list, on its rows' leading edge.
    Caption(String),
    /// "Turn on" under a radio that is off.
    TurnOn(String),
    Network(usize),
    /// The inline password for the network above it: its field and status.
    Password,
    /// Cancel and Connect under the password.
    PasswordButtons,
    /// The section's link into Settings.
    Settings(String),
    Device(usize),
    Profile(usize),
    /// The charge, its state and the time left.
    Battery,
    /// The three-way power mode.
    Modes,
    /// The apps keeping the machine awake.
    Awake(String),
    /// A sound output.
    Output(usize),
    /// A row the overlay host paints (see [`SectionStop::Hosted`]).
    Hosted(usize),
    /// "Clear history" under the clipboard's copies.
    ClearClipboard(String),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Item {
    pub kind: ItemKind,
    /// Relative to the section's top-left corner.
    pub rect: Rect,
}

/// A section laid out at `width`: its items and full height.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SectionLayout {
    pub items: Vec<Item>,
    pub height: f64,
}

/// The Wi-Fi networks in the order the section lists them: the one in use,
/// then the ones this machine knows, then by signal, then by name.
pub(crate) fn ordered_networks(networks: &[WifiNetwork]) -> Vec<usize> {
    let mut order: Vec<(usize, &WifiNetwork)> = networks.iter().enumerate().collect();
    order.sort_by(|(_, a), (_, b)| {
        b.connected
            .cmp(&a.connected)
            .then(b.known.cmp(&a.known))
            .then(b.signal.cmp(&a.signal))
            .then_with(|| a.ssid.to_lowercase().cmp(&b.ssid.to_lowercase()))
    });
    order.into_iter().map(|(index, _)| index).collect()
}

/// The power modes the daemon offers, in their natural order. A machine
/// whose daemon has not answered offers none.
pub(crate) fn offered_modes(state: &QuickSettingsState) -> Vec<PowerProfile> {
    [
        PowerProfile::Saver,
        PowerProfile::Balanced,
        PowerProfile::Performance,
    ]
    .into_iter()
    .filter(|mode| state.power_profiles.contains(mode))
    .collect()
}

/// What the Wi-Fi tile and section say about a radio that is off.
pub(crate) fn wifi_off_message(state: &QuickSettingsState) -> String {
    if state.radios.wifi_hardware_blocked {
        crate::shell::quick_settings::radio::WifiProblem::HardwareSwitch.reason()
    } else if let Some(problem) = state.wifi_problem {
        problem.reason()
    } else {
        t("qs-wifi-off")
    }
}

/// The apps keeping the machine awake, as one sentence, or `None`.
pub(crate) fn awake_sentence(state: &QuickSettingsState) -> Option<String> {
    match state.awake_holders.as_slice() {
        [] => None,
        [app, rest @ ..] if rest.iter().all(|other| other == app) => {
            Some(t_args("qs-awake-app", &[("app", app.as_str().into())]))
        }
        many => Some(t_args("qs-awake-apps", &[("count", many.len().into())])),
    }
}

/// The battery's charge state, in words.
pub(crate) fn battery_state_text(state: &QuickSettingsState) -> String {
    use tilo_power_mgmt::BatteryState;
    if !state.battery.present {
        return t("qs-power-ac");
    }
    t(match state.battery.state {
        BatteryState::Charging => "qs-battery-charging",
        BatteryState::Full => "qs-battery-full",
        BatteryState::PendingCharge => "qs-battery-not-charging",
        _ => "qs-battery-on-battery",
    })
}

/// How long until empty or full, in words, when the battery says.
pub(crate) fn battery_time_text(state: &QuickSettingsState) -> Option<String> {
    use tilo_power_mgmt::BatteryTime;
    if !state.battery.present {
        return None;
    }
    let (key, minutes) = match state.battery_time? {
        BatteryTime::ToEmpty(minutes) => ("qs-battery-time-left", minutes),
        BatteryTime::ToFull(minutes) => ("qs-battery-time-to-full", minutes),
    };
    if minutes == 0 {
        return None;
    }
    Some(t_args(
        key,
        &[
            ("hours", (minutes / 60).into()),
            ("minutes", (minutes % 60).into()),
        ],
    ))
}

/// Lay `section` out at `width` for `state`.
pub(crate) fn layout(
    section: QsSection,
    state: &QuickSettingsState,
    width: f64,
    m: SectionMetrics,
) -> SectionLayout {
    let mut builder = Builder {
        width,
        y: RELATED_GAP,
        items: Vec::new(),
    };
    match section {
        QsSection::Wifi => wifi(&mut builder, state, m),
        QsSection::Bluetooth => bluetooth(&mut builder, state, m),
        QsSection::Vpn => vpn(&mut builder, state, m),
        QsSection::Battery => battery(&mut builder, state, m),
        QsSection::Sound => sound(&mut builder, state, m),
        QsSection::Devices => devices(&mut builder, state, m),
        QsSection::Clipboard => clipboard(&mut builder, state, m),
    }
    let height = builder.y + RELATED_GAP;
    SectionLayout {
        items: builder.items,
        height,
    }
}

struct Builder {
    width: f64,
    y: f64,
    items: Vec<Item>,
}

impl Builder {
    fn push(&mut self, kind: ItemKind, height: f64) {
        self.items.push(Item {
            kind,
            rect: Rect::new(0.0, self.y, self.width, self.y + height),
        });
        self.y += height;
    }

    /// An item that is not a row (a field, buttons, a segmented strip):
    /// it keeps the related step from the section's sides, where a row's
    /// text starts.
    fn push_inset(&mut self, kind: ItemKind, height: f64) {
        self.items.push(Item {
            kind,
            rect: Rect::new(
                RELATED_GAP,
                self.y,
                self.width - RELATED_GAP,
                self.y + height,
            ),
        });
        self.y += height;
    }

    fn gap(&mut self) {
        self.y += RELATED_GAP;
    }
}

fn wifi(b: &mut Builder, state: &QuickSettingsState, m: SectionMetrics) {
    if state.radio_change_pending {
        let on = matches!(
            state.radio_change,
            Some(os_actions::RadioChange::Wifi(true) | os_actions::RadioChange::Airplane(false))
        );
        b.push(
            ItemKind::Message(t(if on {
                "qs-turning-on"
            } else {
                "qs-turning-off"
            })),
            m.row,
        );
    } else if !state.wifi {
        b.push(ItemKind::Message(wifi_off_message(state)), m.row);
        if !state.radios.wifi_hardware_blocked {
            b.push(ItemKind::TurnOn(t("qs-wifi-turn-on")), m.control);
            b.gap();
        }
    } else {
        let order = ordered_networks(&state.wifi_networks);
        let prompt = state
            .wifi_password
            .as_ref()
            .map(|prompt| prompt.ssid.as_str());
        for index in order {
            b.push(ItemKind::Network(index), m.row);
            let asked = state
                .wifi_networks
                .get(index)
                .is_some_and(|network| prompt == Some(network.ssid.as_str()));
            if asked {
                b.gap();
                b.push_inset(ItemKind::Password, m.control + m.caption + RELATED_GAP);
                b.push_inset(ItemKind::PasswordButtons, m.control);
                b.gap();
            }
        }
        if state.wifi_networks.is_empty() {
            let key = if state.wifi_scanning {
                "qs-wifi-scanning"
            } else if state.wifi_scan_error.is_some() {
                "qs-wifi-scan-failed"
            } else {
                "qs-wifi-none"
            };
            b.push(ItemKind::Message(t(key)), m.row);
        }
    }
    b.push(ItemKind::Settings(t("qs-wifi-settings")), m.row);
}

fn bluetooth(b: &mut Builder, state: &QuickSettingsState, m: SectionMetrics) {
    if !state.bluetooth {
        b.push(ItemKind::Message(t("qs-bluetooth-off")), m.row);
        b.push(ItemKind::TurnOn(t("qs-bluetooth-turn-on")), m.control);
        b.gap();
    } else {
        // Connected first, then by name: what is in use is what a person
        // came to change.
        let mut order: Vec<(usize, &crate::shell::quick_settings::BtDevice)> =
            state.bt_devices.iter().enumerate().collect();
        order.sort_by(|(_, a), (_, b)| {
            b.connected
                .cmp(&a.connected)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        for (index, _) in order {
            b.push(ItemKind::Device(index), m.two_line);
        }
        if state.bt_devices.is_empty() {
            b.push(ItemKind::Message(t("qs-bluetooth-empty")), m.row);
        }
    }
    b.push(ItemKind::Settings(t("qs-bluetooth-pair")), m.row);
}

fn vpn(b: &mut Builder, state: &QuickSettingsState, m: SectionMetrics) {
    for index in 0..state.vpns.len() {
        b.push(ItemKind::Profile(index), m.row);
    }
    if state.vpns.is_empty() {
        b.push(ItemKind::Message(t("qs-vpn-empty")), m.row);
    }
    b.push(ItemKind::Settings(t("qs-vpn-settings")), m.row);
}

fn battery(b: &mut Builder, state: &QuickSettingsState, m: SectionMetrics) {
    if state.battery.present {
        let lines = if battery_time_text(state).is_some() {
            2.0
        } else {
            1.0
        };
        b.push(ItemKind::Battery, m.title + lines * m.caption + RELATED_GAP);
        b.gap();
    }
    if !offered_modes(state).is_empty() {
        b.push_inset(ItemKind::Modes, m.control);
        b.gap();
    }
    if let Some(awake) = awake_sentence(state) {
        b.push(ItemKind::Awake(awake), m.row);
    }
    b.push(
        ItemKind::Settings(t(if state.battery.present {
            "qs-battery-settings"
        } else {
            "qs-power-settings"
        })),
        m.row,
    );
}

fn sound(b: &mut Builder, state: &QuickSettingsState, m: SectionMetrics) {
    for index in 0..state.sound_outputs.len() {
        b.push(ItemKind::Output(index), m.row);
    }
    b.push(ItemKind::Settings(t("qs-sound-settings")), m.row);
}

fn devices(b: &mut Builder, state: &QuickSettingsState, m: SectionMetrics) {
    if state.personal_devices.is_none() && state.device_rows.is_empty() {
        b.push(ItemKind::Message(t("qs-devices-no-identity")), m.row);
    } else if state.device_rows.is_empty() {
        b.push(ItemKind::Message(t("qs-devices-empty")), m.row);
    }
    for index in 0..state.device_rows.len() {
        b.push(ItemKind::Hosted(index), m.two_line);
    }
    b.push(ItemKind::Settings(t("qs-devices-settings")), m.row);
}

fn clipboard(b: &mut Builder, state: &QuickSettingsState, m: SectionMetrics) {
    if state.clipboard_rows.is_empty() {
        // Nothing copied, or nothing matching what was typed.
        let message = if state.clipboard_caption.is_empty() {
            t("qs-clipboard-empty-hint")
        } else {
            state.clipboard_caption.clone()
        };
        b.push(ItemKind::Message(message), m.row);
        return;
    }
    if !state.clipboard_caption.is_empty() {
        b.push(ItemKind::Caption(state.clipboard_caption.clone()), m.row);
    }
    for index in 0..state.clipboard_rows.len() {
        b.push(ItemKind::Hosted(index), m.two_line);
    }
    b.push(
        ItemKind::ClearClipboard(t("qs-clipboard-clear-history")),
        m.row,
    );
}

/// The rows the overlay host describes for `section`.
pub(crate) fn hosted_rows(
    section: QsSection,
    state: &QuickSettingsState,
) -> &[super::hosted::HostedRow] {
    match section {
        QsSection::Devices => &state.device_rows,
        QsSection::Clipboard => &state.clipboard_rows,
        _ => &[],
    }
}

/// The segments of the power-mode strip in `rect`, one per offered mode.
pub(crate) fn mode_segments(state: &QuickSettingsState, rect: Rect) -> Vec<(PowerProfile, Rect)> {
    let modes = offered_modes(state);
    let count = u32::try_from(modes.len()).unwrap_or(1).max(1);
    let width = rect.width() / f64::from(count);
    modes
        .into_iter()
        .enumerate()
        .map(|(i, mode)| {
            let x = rect.x0 + width * f64::from(u32::try_from(i).unwrap_or(0));
            (mode, Rect::new(x, rect.y0, x + width, rect.y1))
        })
        .collect()
}

/// Cancel and Connect, side by side in `rect`, one related step apart.
pub(crate) fn password_buttons(rect: Rect) -> (Rect, Rect) {
    let width = (rect.width() - RELATED_GAP) / 2.0;
    (
        Rect::new(rect.x0, rect.y0, rect.x0 + width, rect.y1),
        Rect::new(rect.x1 - width, rect.y0, rect.x1, rect.y1),
    )
}

/// The password field inside a [`ItemKind::Password`] item's rect.
pub(crate) fn password_field(rect: Rect, m: SectionMetrics) -> Rect {
    Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + m.control)
}

/// "Turn on", centred under its message at its natural width.
pub(crate) fn turn_on_button(rect: Rect) -> Rect {
    let width = rect.width().min(160.0);
    Rect::new(
        rect.center().x - width / 2.0,
        rect.y0,
        rect.center().x + width / 2.0,
        rect.y1,
    )
}

/// The trailing "use on another device" button of a connected accessory.
pub(crate) fn device_details_button(rect: Rect, m: SectionMetrics) -> Rect {
    let size = m.control;
    Rect::new(
        rect.x1 - size,
        rect.center().y - size / 2.0,
        rect.x1,
        rect.center().y + size / 2.0,
    )
}

/// Every focus stop in `layout`, in visual order, with the rect it paints
/// and is hit at (relative to the section, like the items).
pub(crate) fn stops(
    layout: &SectionLayout,
    state: &QuickSettingsState,
    m: SectionMetrics,
) -> Vec<(SectionStop, Rect)> {
    let mut out = Vec::new();
    for item in &layout.items {
        match &item.kind {
            ItemKind::TurnOn(_) => out.push((SectionStop::TurnOn, turn_on_button(item.rect))),
            ItemKind::Network(index) => {
                // The network in use has nothing to do on a press.
                if state
                    .wifi_networks
                    .get(*index)
                    .is_some_and(|network| !network.connected)
                {
                    out.push((SectionStop::Network(*index), item.rect));
                }
            }
            ItemKind::PasswordButtons => {
                let (cancel, connect) = password_buttons(item.rect);
                out.push((SectionStop::PasswordCancel, cancel));
                out.push((SectionStop::PasswordConnect, connect));
            }
            ItemKind::Settings(_) => out.push((SectionStop::Settings, item.rect)),
            ItemKind::Device(index) => {
                out.push((SectionStop::Device(*index), item.rect));
                if state
                    .bt_devices
                    .get(*index)
                    .is_some_and(|device| device.connected)
                {
                    out.push((
                        SectionStop::DeviceDetails(*index),
                        device_details_button(item.rect, m),
                    ));
                }
            }
            ItemKind::Profile(index) => out.push((SectionStop::Profile(*index), item.rect)),
            ItemKind::Output(index) => out.push((SectionStop::Output(*index), item.rect)),
            ItemKind::Hosted(index) => out.push((SectionStop::Hosted(*index), item.rect)),
            ItemKind::ClearClipboard(_) => out.push((SectionStop::ClearClipboard, item.rect)),
            ItemKind::Modes => out.extend(
                mode_segments(state, item.rect)
                    .into_iter()
                    .map(|(mode, rect)| (SectionStop::Mode(mode), rect)),
            ),
            ItemKind::Message(_)
            | ItemKind::Caption(_)
            | ItemKind::Password
            | ItemKind::Battery
            | ItemKind::Awake(_) => {}
        }
    }
    out
}

/// The stop under `point` (section-relative). A device's details button
/// wins over its row.
pub(crate) fn stop_at(
    layout: &SectionLayout,
    state: &QuickSettingsState,
    m: SectionMetrics,
    point: Point,
) -> Option<SectionStop> {
    let stops = stops(layout, state, m);
    stops
        .iter()
        .find(|(stop, rect)| matches!(stop, SectionStop::DeviceDetails(_)) && rect.contains(point))
        .or_else(|| stops.iter().find(|(_, rect)| rect.contains(point)))
        .map(|(stop, _)| *stop)
}

/// How a section is painted this frame.
pub(crate) struct SectionPaint<'a> {
    pub section: QsSection,
    pub state: &'a QuickSettingsState,
    pub metrics: SectionMetrics,
    /// Where the section's top-left corner is.
    pub origin: Point,
    /// The pointer, in the section's coordinates.
    pub cursor: Option<Point>,
    pub focused: Option<SectionStop>,
    /// The password typed so far can be submitted.
    pub password_ready: bool,
}

/// Paint `layout`'s items.
pub(crate) fn paint(ctx: &mut PaintCtx<'_>, layout: &SectionLayout, p: &SectionPaint<'_>) {
    let xform = ctx.transform * Affine::translate(p.origin.to_vec2());
    let mut ctx = PaintCtx::new(&mut *ctx.scene, xform, &mut *ctx.text, ctx.theme);
    let hovered = |rect: Rect| p.cursor.is_some_and(|point| rect.contains(point));
    let focused = |stop: SectionStop| p.focused == Some(stop);
    // Network rows paint their signal in a wider leading column; the other
    // rows of that list share it so every title starts on one edge.
    let column = layout
        .items
        .iter()
        .any(|item| matches!(item.kind, ItemKind::Network(_)))
        .then(|| f64::from(ctx.theme.tokens.metrics.icon_lg));
    for item in &layout.items {
        let rect = item.rect;
        match &item.kind {
            ItemKind::Message(text) => paint_message(&mut ctx, rect, text),
            ItemKind::Caption(text) => paint_caption(&mut ctx, rect, text),
            ItemKind::TurnOn(label) => {
                let button = turn_on_button(rect);
                Button::paint(
                    ctx.scene,
                    ctx.transform,
                    ctx.text,
                    ctx.theme,
                    &ButtonFace::new(button, label)
                        .hovered(hovered(button))
                        .focused(focused(SectionStop::TurnOn)),
                );
            }
            ItemKind::Network(index) => {
                paint_network(&mut ctx, rect, p, *index, hovered(rect));
            }
            ItemKind::Password => paint_password_status(&mut ctx, rect, p),
            ItemKind::PasswordButtons => {
                let (cancel, connect) = password_buttons(rect);
                let connecting = p
                    .state
                    .wifi_password
                    .as_ref()
                    .is_some_and(|prompt| prompt.connecting);
                Button::paint(
                    ctx.scene,
                    ctx.transform,
                    ctx.text,
                    ctx.theme,
                    &ButtonFace::new(cancel, &t("portal-cancel"))
                        .hovered(hovered(cancel))
                        .focused(focused(SectionStop::PasswordCancel)),
                );
                Button::paint(
                    ctx.scene,
                    ctx.transform,
                    ctx.text,
                    ctx.theme,
                    &ButtonFace::new(
                        connect,
                        &t(if connecting {
                            "qs-connecting"
                        } else {
                            "portal-connect"
                        }),
                    )
                    .role(ButtonRole::Primary)
                    .disabled(!p.password_ready)
                    .hovered(hovered(connect))
                    .focused(focused(SectionStop::PasswordConnect)),
                );
            }
            ItemKind::Settings(label) => {
                let mut face =
                    ListRowFace::new(label).leading(RowLeading::Icon(ui::icons::Icon::Settings2));
                if let Some(column) = column {
                    face = face.leading_column(column);
                }
                face.disclosure()
                    .interaction(Interaction {
                        hovered: hovered(rect),
                        focused: focused(SectionStop::Settings),
                        ..Interaction::default()
                    })
                    .paint(&mut ctx, rect);
                focus_ring(&mut ctx, rect, focused(SectionStop::Settings));
            }
            ItemKind::Device(index) => paint_device(&mut ctx, rect, p, *index),
            ItemKind::Profile(index) => paint_profile(&mut ctx, rect, p, *index),
            ItemKind::Output(index) => paint_output(&mut ctx, rect, p, *index),
            ItemKind::Battery => paint_battery(&mut ctx, rect, p.state, p.metrics),
            ItemKind::Modes => paint_modes(&mut ctx, rect, p),
            ItemKind::Awake(text) => {
                ListRowFace::new(text)
                    .leading(RowLeading::Icon(ui::icons::Icon::Monitor))
                    .paint(&mut ctx, rect);
            }
            ItemKind::Hosted(index) => {
                if let Some(row) = hosted_rows(p.section, p.state).get(*index) {
                    let stop = SectionStop::Hosted(*index);
                    super::hosted::paint(&mut ctx, rect, row, p.cursor, focused(stop));
                    focus_ring(&mut ctx, rect, focused(stop));
                }
            }
            ItemKind::ClearClipboard(label) => {
                ListRowFace::new(label)
                    .leading(RowLeading::Icon(ui::icons::Icon::Trash2))
                    .interaction(Interaction {
                        hovered: hovered(rect),
                        focused: focused(SectionStop::ClearClipboard),
                        ..Interaction::default()
                    })
                    .paint(&mut ctx, rect);
                focus_ring(&mut ctx, rect, focused(SectionStop::ClearClipboard));
            }
        }
    }
}

/// The keyboard ring of a row-shaped stop.
fn focus_ring(ctx: &mut PaintCtx<'_>, rect: Rect, focused: bool) {
    if focused {
        ui::widget::paint_focus_ring(
            ctx.scene,
            ctx.transform,
            rect,
            f64::from(ctx.theme.tokens.radii.control),
            ctx.theme,
        );
    }
}

/// A dim caption on the rows' leading edge, centred on its line.
fn paint_caption(ctx: &mut PaintCtx<'_>, rect: Rect, text: &str) {
    let style = ctx.theme.text(TextRole::Caption);
    let inset = ui::chrome::row_bleed(ctx.theme);
    let width = ui::num::f32_of_f64((rect.width() - 2.0 * inset).max(1.0));
    let text = ctx.text.elide_to_width_style(text, width, &style);
    let (_, h) = ctx.text.measure_style(&text, None, &style);
    ctx.text.draw_style(
        ctx.scene,
        &text,
        ctx.theme.tokens.colors.text_dim,
        ctx.transform
            .pre_translate((rect.x0 + inset, rect.center().y - f64::from(h) / 2.0).into()),
        None,
        &style,
    );
}

fn paint_message(ctx: &mut PaintCtx<'_>, rect: Rect, text: &str) {
    let style = ctx.theme.text(TextRole::Caption);
    let width = ui::num::f32_of_f64(rect.width().max(1.0));
    let text = ctx.text.elide_to_width_style(text, width, &style);
    let (w, h) = ctx.text.measure_style(&text, None, &style);
    ctx.text.draw_style(
        ctx.scene,
        &text,
        ctx.theme.tokens.colors.text_dim,
        ctx.transform.pre_translate(
            (
                rect.center().x - f64::from(w) / 2.0,
                rect.center().y - f64::from(h) / 2.0,
            )
                .into(),
        ),
        None,
        &style,
    );
}

/// A network: its signal as the Wi-Fi glyph lit by strength, its name with
/// a lock when it has a password, and "Connected", "Connecting…" or
/// "Saved" on the trailing side.
fn paint_network(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    p: &SectionPaint<'_>,
    index: usize,
    hovered: bool,
) {
    let Some(network) = p.state.wifi_networks.get(index) else {
        return;
    };
    let theme = ctx.theme;
    let ink = theme.tokens.colors.text_primary;
    // An unlit arc of the signal: the quietest ink.
    let faint = theme.tokens.colors.text_tertiary;
    let signal = network.signal;
    let side = f64::from(theme.tokens.metrics.icon_md);
    let bars = move |c: &mut PaintCtx<'_>, slot: Rect| {
        ui::icons::draw_icon_layered(
            c.scene,
            c.transform,
            Rect::from_center_size(slot.center(), (side, side)),
            ui::icons::Icon::Wifi,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            // [dot, outer, middle, inner]: a weak network lights fewer arcs.
            |arc| {
                let needed = match arc {
                    0 => 1,
                    1 => 4,
                    2 => 3,
                    _ => 2,
                };
                if signal >= needed { ink } else { faint }
            },
        );
    };
    let joining = p.state.wifi_joining.as_deref() == Some(network.ssid.as_str());
    let value = if joining {
        Some(t("qs-connecting"))
    } else if network.connected {
        Some(t("qs-connected"))
    } else if network.known {
        Some(t("qs-wifi-known"))
    } else {
        None
    };
    let mut face = ListRowFace::new(&network.ssid)
        .leading(RowLeading::Paint(&bars))
        .selected(network.connected)
        .interaction(Interaction {
            hovered: hovered && !network.connected,
            focused: p.focused == Some(SectionStop::Network(index)),
            ..Interaction::default()
        });
    if network.secure {
        face = face.mark(ui::icons::Icon::Lock);
    }
    if let Some(value) = value.as_deref() {
        face = face.value(value);
    }
    face.paint(ctx, rect);
    focus_ring(ctx, rect, p.focused == Some(SectionStop::Network(index)));
}

/// The password's status line under its field (the field itself is the
/// overlay host's secret input, drawn over this item).
fn paint_password_status(ctx: &mut PaintCtx<'_>, rect: Rect, p: &SectionPaint<'_>) {
    let Some(prompt) = p.state.wifi_password.as_ref() else {
        return;
    };
    let field = password_field(rect, p.metrics);
    let colors = &ctx.theme.tokens.colors;
    let (text, color) = if let Some(error) = prompt.error.as_ref() {
        (error.clone(), colors.semantic.danger)
    } else if prompt.connecting {
        (t("qs-connecting"), colors.text_dim)
    } else {
        (t("qs-wifi-password-for"), colors.text_dim)
    };
    let style = ctx.theme.text(TextRole::Caption);
    let width = ui::num::f32_of_f64(rect.width().max(1.0));
    let text = ctx.text.elide_to_width_style(&text, width, &style);
    ctx.text.draw_style(
        ctx.scene,
        &text,
        color,
        ctx.transform
            .pre_translate((rect.x0, field.y1 + RELATED_GAP / 2.0).into()),
        None,
        &style,
    );
}

fn paint_device(ctx: &mut PaintCtx<'_>, rect: Rect, p: &SectionPaint<'_>, index: usize) {
    let Some(device) = p.state.bt_devices.get(index) else {
        return;
    };
    let secondary = if device.connected {
        t("qs-connected")
    } else {
        t("qs-bluetooth-paired")
    };
    let battery = (device.battery > 0).then(|| format!("{}%", device.battery));
    let details = device_details_button(rect, p.metrics);
    let in_details = p.cursor.is_some_and(|point| details.contains(point));
    let mut face = ListRowFace::new(&device.name)
        .leading(RowLeading::Icon(ui::icons::Icon::Bluetooth))
        .secondary(&secondary)
        .selected(device.connected)
        .interaction(Interaction {
            hovered: p.cursor.is_some_and(|point| rect.contains(point)) && !in_details,
            focused: p.focused == Some(SectionStop::Device(index)),
            ..Interaction::default()
        });
    if let Some(battery) = battery.as_deref() {
        face = face.value(battery);
    }
    if device.connected {
        face = face.trailing_width(details.width());
    }
    face.paint(ctx, rect);
    focus_ring(ctx, rect, p.focused == Some(SectionStop::Device(index)));
    if device.connected {
        let name = t("qs-bluetooth-use-elsewhere");
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            ctx.theme,
            &ButtonFace::icon_only(details, ui::icons::Icon::ChevronRight, &name)
                .role(ButtonRole::Plain)
                .hovered(in_details)
                .focused(p.focused == Some(SectionStop::DeviceDetails(index))),
        );
    }
}

/// A sound output: its name, checked where sound plays now.
fn paint_output(ctx: &mut PaintCtx<'_>, rect: Rect, p: &SectionPaint<'_>, index: usize) {
    let Some(output) = p.state.sound_outputs.get(index) else {
        return;
    };
    let lower = output.name.to_lowercase();
    let icon = if lower.contains("headphone") || lower.contains("headset") {
        ui::icons::Icon::Headphones
    } else {
        ui::icons::Icon::Speaker
    };
    let focused = p.focused == Some(SectionStop::Output(index));
    ListRowFace::new(&output.name)
        .leading(RowLeading::Icon(icon))
        .checked(output.current)
        .interaction(Interaction {
            hovered: p.cursor.is_some_and(|point| rect.contains(point)) && !output.current,
            focused,
            ..Interaction::default()
        })
        .paint(ctx, rect);
    focus_ring(ctx, rect, focused);
}

fn paint_profile(ctx: &mut PaintCtx<'_>, rect: Rect, p: &SectionPaint<'_>, index: usize) {
    let Some(profile) = p.state.vpns.get(index) else {
        return;
    };
    let hovered = p.cursor.is_some_and(|point| rect.contains(point));
    ListRowFace::new(&profile.name)
        .leading(RowLeading::Icon(ui::icons::Icon::Shield))
        .trailing_width(TOGGLE_W)
        .interaction(Interaction {
            hovered,
            focused: p.focused == Some(SectionStop::Profile(index)),
            ..Interaction::default()
        })
        .paint(ctx, rect);
    focus_ring(ctx, rect, p.focused == Some(SectionStop::Profile(index)));
    let bleed = ui::chrome::row_bleed(ctx.theme);
    let switch = Rect::new(
        rect.x1 - bleed - TOGGLE_W,
        rect.center().y - TOGGLE_H / 2.0,
        rect.x1 - bleed,
        rect.center().y + TOGGLE_H / 2.0,
    );
    Toggle::paint(
        ctx.scene,
        ctx.transform,
        ctx.theme,
        &ToggleFace::new(switch, profile.active).hovered(hovered),
    );
}

/// The charge as a large number, its state and the time left under it.
fn paint_battery(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    state: &QuickSettingsState,
    m: SectionMetrics,
) {
    let colors = &ctx.theme.tokens.colors;
    let bleed = ui::chrome::row_bleed(ctx.theme);
    let x = rect.x0 + bleed;
    let title = ctx
        .theme
        .text(TextRole::Title)
        .with_weight(TextWeight::Semibold);
    let percent = format!("{}%", state.battery.percent);
    ctx.text.draw_style(
        ctx.scene,
        &percent,
        colors.text_primary,
        ctx.transform.pre_translate((x, rect.y0).into()),
        None,
        &title,
    );
    let caption = ctx.theme.text(TextRole::Caption);
    let mut y = rect.y0 + m.title + RELATED_GAP / 2.0;
    for line in std::iter::once(battery_state_text(state)).chain(battery_time_text(state)) {
        ctx.text.draw_style(
            ctx.scene,
            &line,
            colors.text_dim,
            ctx.transform.pre_translate((x, y).into()),
            None,
            &caption,
        );
        y += m.caption;
    }
    // A gauge on the trailing side: the charge at a glance.
    let gauge_h = m.title * 0.6;
    let gauge = Rect::new(
        rect.x1 - bleed - gauge_h * 2.0,
        rect.y0 + (m.title - gauge_h) / 2.0,
        rect.x1 - bleed,
        rect.y0 + (m.title + gauge_h) / 2.0,
    );
    ui::widget::primitives::paint_level_gauge(
        ctx.scene,
        ctx.transform,
        gauge,
        f64::from(state.battery.percent.min(100)) / 100.0,
        ctx.theme,
    );
}

fn paint_modes(ctx: &mut PaintCtx<'_>, rect: Rect, p: &SectionPaint<'_>) {
    let segments = mode_segments(p.state, rect);
    let rects: Vec<Rect> = segments.iter().map(|(_, rect)| *rect).collect();
    let labels: Vec<String> = segments.iter().map(|(mode, _)| mode.label()).collect();
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    let selected = segments
        .iter()
        .position(|(mode, _)| *mode == p.state.power_profile)
        .unwrap_or(ui::widget::toggle_group::NONE_CHOSEN);
    let hovered = p
        .cursor
        .and_then(|point| rects.iter().position(|rect| rect.contains(point)));
    let focused = matches!(p.focused, Some(SectionStop::Mode(_)));
    ui::widget::ToggleGroup::paint(
        ctx.scene,
        ctx.transform,
        ctx.text,
        ctx.theme,
        &SegmentedFace::new(&rects, &labels, selected)
            .hovered(hovered)
            .focused(focused),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> SectionMetrics {
        SectionMetrics {
            row: 36.0,
            two_line: 52.0,
            control: 32.0,
            caption: 16.0,
            title: 28.0,
        }
    }

    fn network(ssid: &str, signal: u8, connected: bool, known: bool) -> WifiNetwork {
        WifiNetwork {
            ssid: ssid.into(),
            signal,
            secure: true,
            security: os_actions::WifiSecurity::Wpa2,
            connected,
            known,
        }
    }

    #[test]
    fn networks_list_the_one_in_use_then_known_ones_then_by_signal() {
        let networks = vec![
            network("Strong stranger", 4, false, false),
            network("Weak known", 1, false, true),
            network("Home", 3, true, true),
            network("another", 2, false, false),
            network("Beta", 2, false, false),
        ];
        let order: Vec<&str> = ordered_networks(&networks)
            .into_iter()
            .map(|i| networks[i].ssid.as_str())
            .collect();
        assert_eq!(
            order,
            ["Home", "Weak known", "Strong stranger", "another", "Beta"]
        );
    }

    #[test]
    fn wifi_off_offers_to_turn_it_on_and_says_why_it_is_off() {
        let mut state = QuickSettingsState::fixture();
        state.wifi = false;
        state.radios.wifi_available = true;
        let laid = layout(QsSection::Wifi, &state, 360.0, metrics());
        assert_eq!(laid.items[0].kind, ItemKind::Message("Wi-Fi is off".into()));
        assert!(matches!(laid.items[1].kind, ItemKind::TurnOn(_)));
        assert!(matches!(
            laid.items.last().map(|item| &item.kind),
            Some(ItemKind::Settings(_))
        ));
        assert_eq!(
            stops(&laid, &state, metrics())
                .first()
                .map(|(stop, _)| *stop),
            Some(SectionStop::TurnOn)
        );

        state.radios.wifi_hardware_blocked = true;
        let laid = layout(QsSection::Wifi, &state, 360.0, metrics());
        assert_eq!(
            laid.items[0].kind,
            ItemKind::Message("Off with the hardware switch".into())
        );
        assert!(
            !laid
                .items
                .iter()
                .any(|item| matches!(item.kind, ItemKind::TurnOn(_))),
            "nothing in software turns a hardware switch back"
        );
    }

    #[test]
    fn a_protected_network_asks_for_its_password_in_place_under_its_row() {
        let mut state = QuickSettingsState::fixture();
        state.wifi_networks = vec![
            network("Home", 3, true, true),
            network("Cafe", 2, false, false),
        ];
        state.wifi_password = Some(crate::shell::quick_settings::WifiPasswordPrompt::new(
            "Cafe",
        ));
        let laid = layout(QsSection::Wifi, &state, 360.0, metrics());
        let kinds: Vec<&ItemKind> = laid.items.iter().map(|item| &item.kind).collect();
        assert_eq!(
            kinds,
            [
                &ItemKind::Network(0),
                &ItemKind::Network(1),
                &ItemKind::Password,
                &ItemKind::PasswordButtons,
                &ItemKind::Settings("Wi-Fi settings".into()),
            ]
        );
        let stops: Vec<SectionStop> = stops(&laid, &state, metrics())
            .into_iter()
            .map(|(stop, _)| stop)
            .collect();
        assert_eq!(
            stops,
            [
                SectionStop::Network(1),
                SectionStop::PasswordCancel,
                SectionStop::PasswordConnect,
                SectionStop::Settings,
            ],
            "the network in use is not a stop: pressing it does nothing"
        );
        // Items never overlap, and the section is as tall as they are.
        for pair in laid.items.windows(2) {
            assert!(pair[0].rect.y1 <= pair[1].rect.y0 + 1e-9);
        }
        assert!(laid.height >= laid.items.last().unwrap().rect.y1);
    }

    #[test]
    fn the_battery_section_offers_only_the_modes_the_daemon_has() {
        let mut state = QuickSettingsState::fixture();
        state.battery = tilo_power_mgmt::BatterySnapshot {
            present: true,
            percent: 82,
            state: tilo_power_mgmt::BatteryState::Discharging,
        };
        state.battery_time = Some(tilo_power_mgmt::BatteryTime::ToEmpty(190));
        state.power_profiles = vec![PowerProfile::Saver, PowerProfile::Balanced];
        state.awake_holders = vec!["Firefox".into()];
        let laid = layout(QsSection::Battery, &state, 360.0, metrics());
        let kinds: Vec<&ItemKind> = laid.items.iter().map(|item| &item.kind).collect();
        assert_eq!(
            kinds,
            [
                &ItemKind::Battery,
                &ItemKind::Modes,
                &ItemKind::Awake("Firefox keeps the screen on".into()),
                &ItemKind::Settings("Battery settings".into()),
            ]
        );
        assert_eq!(
            battery_time_text(&state).as_deref(),
            Some("3 h 10 min left")
        );
        let modes: Vec<SectionStop> = stops(&laid, &state, metrics())
            .into_iter()
            .map(|(stop, _)| stop)
            .filter(|stop| matches!(stop, SectionStop::Mode(_)))
            .collect();
        assert_eq!(
            modes,
            [
                SectionStop::Mode(PowerProfile::Saver),
                SectionStop::Mode(PowerProfile::Balanced)
            ]
        );
    }
}
