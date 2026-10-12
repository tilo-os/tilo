//! The window's content, built from the toolkit's widgets.
//!
//! The saved connections are the shell's overview list; the selected
//! connection is one scrolling page of badges, tabs, stat tiles, the
//! throughput chart and its details. Nothing here sizes or colours chrome:
//! every control says what it is and the theme decides how it looks. The one
//! piece of content drawing is the throughput plot, painted by the library
//! chart from theme values. Interaction leaves as an [`Intent`] the app
//! applies to the shared document and the machine.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, TaffyTree};
use vello::peniko::Color;

use ui::LocalState;
use ui::icons::Icon;
use ui::text_render::TextKind;
use ui::theme::Theme;
use ui::tokens::{SpacingStep, TextRole};
use ui::widget::toggle_group::Segment;
use ui::widget::{
    AxisTick, Badge, Banner, Button, ButtonRole, CartesianPlot, CartesianStyle, ChipStyle, Column,
    Container, ControlSize, Element, EmptyState, Field, FormSection, LegendPosition, ListRow,
    PlotAxis, PlotDatum, PlotSeries, Property, PropertyList, Row, Scrollable, Section, SeriesKind,
    Spacer, StatGroup, StatTile, Text, Toggle, ToggleGroup, Tone, Widget,
};

use crate::model::{ConnId, ConnKind, ConnStatus, Connection, LocalProfileDetails, human_ago};
use crate::net::Transfer;

/// Which tab of the detail page is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum DetailTab {
    Status,
    Config,
    Activity,
}

impl DetailTab {
    pub(crate) const ALL: [DetailTab; 3] =
        [DetailTab::Status, DetailTab::Config, DetailTab::Activity];

    pub(crate) fn label(self) -> &'static str {
        match self {
            DetailTab::Status => "Status",
            DetailTab::Config => "Configuration",
            DetailTab::Activity => "Activity",
        }
    }
}

/// What a control in the window asks for. The app applies each one the same
/// way for a click, a key or an accessibility action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Intent {
    Select(ConnId),
    SetTab(DetailTab),
    /// Connect, disconnect, or set the profile up on this device: whatever
    /// the connection's main action is right now.
    MainAction(ConnId),
    ToggleTor(ConnId),
    ToggleTorGlobal,
    OpenImport,
    SetImportKind(ConnKind),
    ChooseFile,
    CloseImport,
    Confirm,
    CancelConfirm,
    RetryImport,
    DismissError,
}

/// The queue controls write to and the app drains.
pub(crate) type Intents = ui::Intents<Intent>;

/// What the main action of a connection is, given where it stands on this
/// device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MainAction {
    SetUp,
    Connect,
    Disconnect,
    Connecting,
}

impl MainAction {
    pub(crate) fn of(connection: &Connection, provisioned: bool) -> Self {
        match connection.status {
            ConnStatus::Connecting => MainAction::Connecting,
            _ if !provisioned => MainAction::SetUp,
            ConnStatus::Connected => MainAction::Disconnect,
            ConnStatus::Disconnected => MainAction::Connect,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            MainAction::SetUp => "Set up on this device",
            MainAction::Connect => "Connect",
            MainAction::Disconnect => "Disconnect",
            MainAction::Connecting => "Connecting…",
        }
    }
}

/// Measured transfer rates for one tunnel: the per-sample series and the
/// latest cumulative counters.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Rates {
    pub down: Vec<f64>,
    pub up: Vec<f64>,
    pub latest: Option<Transfer>,
}

impl Rates {
    pub(crate) fn is_live(&self) -> bool {
        !self.down.is_empty()
    }
}

/// One connection as the list shows it.
pub(crate) struct ListEntry<'a> {
    pub connection: &'a Connection,
    pub details: Option<&'a LocalProfileDetails>,
    pub provisioned: bool,
}

/// Everything the overview shows.
pub(crate) struct OverviewModel<'a> {
    pub entries: Vec<ListEntry<'a>>,
    pub selected: Option<ConnId>,
    pub tor_global: bool,
}

/// The saved connections, then the preference that applies to all of them.
pub(crate) fn overview(
    model: &OverviewModel<'_>,
    intents: &Intents,
    offset: &Rc<Cell<(f32, f32)>>,
) -> Element {
    let rows: Vec<Element> = model
        .entries
        .iter()
        .map(|entry| -> Element {
            Box::new(ConnectionRow {
                row: connection_row(entry, model.selected, intents),
                label: format!(
                    "{}, {}, {}",
                    entry.connection.name,
                    entry.connection.kind.long_name(),
                    entry.connection.status.label()
                ),
            })
        })
        .collect();
    let tor = Toggle::from_state(&LocalState::new(model.tor_global))
        .with_id("vpn-tor-global")
        .on_change({
            let intents = intents.clone();
            move |_| intents.push(Intent::ToggleTorGlobal)
        });
    let routing = FormSection::new("Tor")
        .grouped()
        .field(Field::new("Route all connections over Tor", tor));
    let page = Column::new()
        .with_intrinsic_height()
        .push(Box::new(Section::new(rows)))
        .push(Box::new(Spacer::token(SpacingStep::S0)))
        .push(Box::new(routing));
    Box::new(
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_persisted_offset(Rc::clone(offset))
            .with_child(Box::new(page)),
    )
}

/// The server part of an endpoint: `vpn.example.net:1194 (UDP)` and
/// `[2001:db8::1]:51820` name `vpn.example.net` and `2001:db8::1`. The port
/// and the transport are details of the connection, not how a person tells
/// one connection from another.
fn endpoint_host(endpoint: &str) -> &str {
    let address = endpoint.split(" (").next().unwrap_or(endpoint).trim();
    if let Some(bracketed) = address.strip_prefix('[') {
        return bracketed.split(']').next().unwrap_or(bracketed);
    }
    match address.rsplit_once(':') {
        Some((host, port))
            if !host.contains(':')
                && !port.is_empty()
                && port.chars().all(|c| c.is_ascii_digit()) =>
        {
            host
        }
        _ => address,
    }
}

/// One saved connection: where it goes (or that it still has to be set up
/// here) and whether it is up.
fn connection_row(entry: &ListEntry<'_>, selected: Option<ConnId>, intents: &Intents) -> ListRow {
    let connection = entry.connection;
    let id = connection.id;
    let secondary = entry
        .details
        .map_or("Not set up on this device", |details| {
            endpoint_host(&details.endpoint)
        });
    // The protocol at a glance: the list shares one badge column, so every
    // title starts on one edge whichever protocols it holds.
    let row = ListRow::new(format!("vpn-connection-{id}"), &connection.name)
        .badge(connection.kind.badge())
        .leading_tooltip(connection.kind.long_name())
        .secondary(secondary)
        .selected(selected == Some(id))
        .on_view(intents.sender(Intent::Select(id)));
    let row = match connection.status {
        ConnStatus::Connected => row.active(),
        ConnStatus::Connecting => row.warning(),
        ConnStatus::Disconnected => row.idle(),
    };
    let name = &connection.name;
    let label = match MainAction::of(connection, entry.provisioned) {
        MainAction::Connecting => return row,
        MainAction::SetUp => format!("Set up {name} on this device"),
        MainAction::Connect => format!("Connect {name}"),
        MainAction::Disconnect => format!("Disconnect {name}"),
    };
    row.action(Icon::Power, label, intents.sender(Intent::MainAction(id)))
}

/// The overview's one header action.
pub(crate) fn import_button(intents: &Intents) -> Button {
    Button::icon_only("vpn-import", Icon::Plus, "Import configuration")
        .on_view(intents.sender(Intent::OpenImport))
}

/// Nothing saved yet: one sentence and the one way forward.
pub(crate) fn empty_state(intents: &Intents) -> Element {
    Box::new(
        EmptyState::new("No VPN connections")
            .with_icon(Icon::Shield)
            .with_message("Import a WireGuard or OpenVPN configuration from your provider.")
            .with_action("Import configuration")
            .on_view(intents.sender(Intent::OpenImport)),
    )
}

/// A problem the person has to act on, above the content it concerns.
pub(crate) struct Notice<'a> {
    pub kind: NoticeKind,
    pub message: &'a str,
    pub retry: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoticeKind {
    /// Waiting for the file picker.
    Waiting(ConnKind),
    Error,
}

fn notice(notice: &Notice<'_>, intents: &Intents) -> Element {
    match notice.kind {
        NoticeKind::Waiting(kind) => Box::new(Banner::info(format!(
            "Choose a .{} file in the file picker",
            kind.ext()
        ))),
        NoticeKind::Error => {
            // Closing the banner dismisses the error; trying again is the
            // banner's own action.
            let dismiss = intents.sender(Intent::DismissError);
            let mut banner = Banner::error(notice.message)
                .with_id("vpn-error")
                .on_dismiss(dismiss);
            if notice.retry {
                banner = banner.with_action(
                    Button::new("vpn-retry-import", "Try again")
                        .on_effect(intents.sender(Intent::RetryImport)),
                );
            }
            Box::new(banner)
        }
    }
}

/// Everything the detail page shows, borrowed from the app.
pub(crate) struct DetailModel<'a> {
    pub connection: &'a Connection,
    pub details: Option<&'a LocalProfileDetails>,
    pub tab: DetailTab,
    pub rates: &'a Rates,
    pub activity: Vec<String>,
    pub notice: Option<Notice<'a>>,
    /// Every connection goes over Tor, whatever this one's own preference.
    pub tor_global: bool,
}

/// The selected connection: where it stands, its main action, and one tab
/// of detail.
pub(crate) fn detail(
    model: &DetailModel<'_>,
    intents: &Intents,
    offset: &Rc<Cell<(f32, f32)>>,
) -> Element {
    let mut page = Column::new().with_intrinsic_height();
    if let Some(n) = &model.notice {
        page = page.push(notice(n, intents));
    }
    page = page.push(Box::new(summary(model)));
    page = page.push(Box::new(tabs(model.tab, intents)));
    page = page.push(match model.tab {
        DetailTab::Status => status(model, intents),
        DetailTab::Config => config(model),
        DetailTab::Activity => activity(model),
    });
    Box::new(
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_persisted_offset(Rc::clone(offset))
            .with_child(Box::new(page)),
    )
}

/// A page with only a notice on it: nothing is saved yet, but an import is
/// in flight or failed.
pub(crate) fn notice_page(n: &Notice<'_>, intents: &Intents) -> Element {
    Box::new(
        Column::new()
            .with_intrinsic_height()
            .push(notice(n, intents)),
    )
}

fn status_badge(status: ConnStatus) -> Badge {
    let tone = match status {
        ConnStatus::Connected => Tone::Success,
        ConnStatus::Connecting => Tone::Warning,
        ConnStatus::Disconnected => Tone::Neutral,
    };
    Badge::text(status.label()).style(ChipStyle::Tinted(tone))
}

/// The status and protocol at a glance.
fn summary(model: &DetailModel<'_>) -> Row {
    let connection = model.connection;
    let mut row = Row::new()
        .no_wrap()
        .with_align_items(taffy::AlignItems::Center)
        .push(Box::new(status_badge(connection.status)))
        .push(Box::new(
            Badge::text(connection.kind.long_name()).style(ChipStyle::Neutral),
        ));
    if connection.tor || model.tor_global {
        row = row.push(Box::new(Badge::text("Tor").style(ChipStyle::Neutral)));
    }
    row
}

/// The selected connection's main action, as the detail toolbar shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ToolbarAction {
    pub id: ConnId,
    pub action: MainAction,
}

/// Where the app says what the detail toolbar's main action is; the
/// toolbar button follows it.
#[derive(Clone, Default)]
pub(crate) struct ToolbarActionSlot(Rc<Cell<Option<ToolbarAction>>>);

impl ToolbarActionSlot {
    pub(crate) fn set(&self, action: Option<ToolbarAction>) {
        self.0.set(action);
    }

    fn get(&self) -> Option<ToolbarAction> {
        self.0.get()
    }
}

/// The detail toolbar's main action: Connect, Disconnect or Set up on this
/// device for the selected connection, and nothing while it is changing
/// state (its status says so).
pub(crate) struct MainActionButton {
    slot: ToolbarActionSlot,
    intents: Intents,
    shown: Option<ToolbarAction>,
    row: Row,
    changed: bool,
}

impl MainActionButton {
    pub(crate) fn new(slot: &ToolbarActionSlot, intents: &Intents) -> Self {
        Self {
            slot: slot.clone(),
            intents: intents.clone(),
            shown: None,
            row: Row::new().no_wrap(),
            changed: false,
        }
    }

    /// Rebuild the button when the main action changed since it was built.
    fn follow(&mut self) -> bool {
        let wanted = self.slot.get();
        if wanted == self.shown {
            return false;
        }
        self.shown = wanted;
        let mut row = Row::new().no_wrap();
        if let Some(ToolbarAction { id, action }) =
            wanted.filter(|toolbar| toolbar.action != MainAction::Connecting)
        {
            let role = match action {
                MainAction::Disconnect => ButtonRole::Secondary,
                _ => ButtonRole::Primary,
            };
            row = row.push(Box::new(
                Button::new("vpn-main-action", action.label())
                    .role(role)
                    .control_size(ControlSize::Small)
                    .on_effect(self.intents.sender(Intent::MainAction(id))),
            ));
        }
        self.row = row;
        self.changed = true;
        true
    }
}

impl ui::widget::Decorator for MainActionButton {
    type Inner = Row;

    fn inner(&self) -> &Row {
        &self.row
    }

    fn inner_mut(&mut self) -> &mut Row {
        &mut self.row
    }

    fn sync(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) {
        self.follow();
        Widget::sync(&mut self.row, state, path);
    }

    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        let changed = self.follow();
        Widget::update(&mut self.row, state, path) || changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.follow();
        self.changed = false;
        Widget::build_layout(&mut self.row, taffy, theme, node_map)
    }

    fn take_structure_dirty(&mut self) -> bool {
        let changed = self.follow() || self.changed;
        Widget::take_structure_dirty(&mut self.row) || changed
    }

    fn is_layout_dirty(&self) -> bool {
        self.changed || self.slot.get() != self.shown || Widget::is_layout_dirty(&self.row)
    }
}

fn tabs(selected: DetailTab, intents: &Intents) -> ToggleGroup {
    let index = DetailTab::ALL
        .iter()
        .position(|tab| *tab == selected)
        .unwrap_or(0);
    let intents = intents.clone();
    ToggleGroup::new(
        DetailTab::ALL
            .iter()
            .map(|tab| Segment::new(tab.label()))
            .collect(),
        index,
    )
    .with_id("vpn-tabs")
    .on_change(move |i| {
        if let Some(tab) = DetailTab::ALL.get(i) {
            intents.push(Intent::SetTab(*tab));
        }
    })
}

// ---- Status -------------------------------------------------------------

fn status(model: &DetailModel<'_>, intents: &Intents) -> Element {
    let connection = model.connection;
    let mut page = Column::new().with_intrinsic_height().with_no_padding();
    let Some(details) = model.details else {
        page = page.push(Box::new(
            Text::new(
                "The keys for this connection are not on this device. Set it up here with \
                 the configuration file from your provider; it stays in NetworkManager and is \
                 never synced.",
            )
            .with_role(TextRole::Body),
        ));
        return Box::new(page);
    };
    if connection.is_connected() {
        page = page.push(Box::new(StatGroup::new(rate_tiles(model.rates))));
        page = page.push(Box::new(Section::titled(
            "Throughput",
            vec![throughput(model.rates)],
        )));
    }
    page = page.push(Box::new(Section::titled(
        "Details",
        vec![Box::new(PropertyList::new(facts(connection, details)))],
    )));
    // While every connection goes over Tor this one does too; its own
    // preference only matters once that is turned off.
    let routing = FormSection::new("Routing");
    let routing = if model.tor_global {
        routing.content(Box::new(
            Text::new("All connections are routed over Tor.").with_role(TextRole::Body),
        ))
    } else {
        let tor = Toggle::from_state(&LocalState::new(connection.tor))
            .with_id("vpn-tor")
            .on_change({
                let intents = intents.clone();
                let id = connection.id;
                move |_| intents.push(Intent::ToggleTor(id))
            });
        routing
            .grouped()
            .field(Field::new("Route this connection over Tor", tor))
    };
    page = page.push(Box::new(routing));
    Box::new(page)
}

/// A formatted quantity split into its number and its unit ("6.4 MiB/s").
fn split_unit(formatted: &str) -> (String, String) {
    match formatted.rsplit_once(' ') {
        Some((value, unit)) => (value.to_owned(), unit.to_owned()),
        None => (formatted.to_owned(), String::new()),
    }
}

fn tile(label: &str, formatted: &str) -> Element {
    let (value, unit) = split_unit(formatted);
    let tile = StatTile::new(label, value);
    Box::new(if unit.is_empty() {
        tile
    } else {
        tile.unit(unit)
    })
}

fn rate_tiles(rates: &Rates) -> Vec<Element> {
    let rate = |series: &[f64]| {
        series
            .last()
            .map_or_else(|| ui::format::rate(0.0), |bps| ui::format::rate(*bps))
    };
    let mut tiles = vec![
        tile("Download", &rate(&rates.down)),
        tile("Upload", &rate(&rates.up)),
    ];
    if let Some(total) = rates.latest {
        tiles.push(tile("Received", &ui::format::bytes(total.rx)));
        tiles.push(tile("Sent", &ui::format::bytes(total.tx)));
    }
    tiles
}

fn facts(connection: &Connection, details: &LocalProfileDetails) -> Vec<Property> {
    let mut facts = vec![
        Property::new("Endpoint", details.endpoint.clone()).with_kind(TextKind::Identifier),
        Property::new("Address", details.address.clone()).with_kind(TextKind::Identifier),
        Property::new("DNS", details.dns.clone()).with_kind(TextKind::Identifier),
        Property::new("Allowed IPs", details.allowed_ips.clone()).with_kind(TextKind::Identifier),
    ];
    if !details.public_key.is_empty() {
        facts.push(
            Property::new("Public key", details.public_key.clone())
                .with_kind(TextKind::Identifier)
                .copyable(),
        );
    }
    if let Some(secs) = connection
        .last_handshake_secs
        .filter(|_| connection.is_connected())
    {
        facts.push(Property::new("Last handshake", human_ago(secs)).with_kind(TextKind::Numeric));
    }
    facts
}

fn throughput(rates: &Rates) -> Element {
    if !rates.is_live() {
        let note = if rates.latest.is_some() {
            "Measuring…"
        } else {
            "This interface reports no traffic counters."
        };
        return Box::new(Text::new(note).with_role(TextRole::Body).dimmed());
    }
    Box::new(ThroughputPlot::new(rates.down.clone(), rates.up.clone()))
}

// ---- Configuration and activity ---------------------------------------------

fn config(model: &DetailModel<'_>) -> Element {
    let Some(details) = model.details else {
        return Box::new(
            Text::new(
                "The configuration is kept by NetworkManager on the device that set this \
                 connection up. Set it up here to see it.",
            )
            .with_role(TextRole::Body),
        );
    };
    // The file reads as the provider wrote it, on the page itself.
    Box::new(
        Text::new(&render_config(model.connection, details))
            .with_role(TextRole::Code)
            .with_kind(TextKind::Code),
    )
}

/// What happened to this connection on this device, newest first.
fn activity(model: &DetailModel<'_>) -> Element {
    Box::new(Section::new(
        model
            .activity
            .iter()
            .map(|line| -> Element { Box::new(Text::new(line).with_role(TextRole::Body)) })
            .collect(),
    ))
}

/// The exact command a sheet is about to run, on a well.
fn code_block(text: &str) -> Container {
    Container::new().with_background().with_child(Box::new(
        Text::new(text)
            .with_role(TextRole::Code)
            .with_kind(TextKind::Code),
    ))
}

/// The configuration as the provider file writes it, with secrets left in
/// `NetworkManager`.
pub(crate) fn render_config(connection: &Connection, details: &LocalProfileDetails) -> String {
    match connection.kind {
        ConnKind::WireGuard => format!(
            "[Interface]\nPrivateKey = (kept by NetworkManager)\nAddress = {}\nDNS = {}\n\n[Peer]\nPublicKey = {}\nEndpoint = {}\nAllowedIPs = {}",
            details.address, details.dns, details.public_key, details.endpoint, details.allowed_ips
        ),
        ConnKind::OpenVpn => format!(
            "client\ndev tun\nremote {}\n\n# Routes: {}\n# DNS: {}\n# Certificates and keys are kept by NetworkManager",
            details.endpoint, details.allowed_ips, details.dns
        ),
    }
}

// ---- Sheets -------------------------------------------------------------------

/// The import sheet's body: the protocol (unless an existing connection
/// fixes it) and what happens to the file.
pub(crate) fn import_body(kind: ConnKind, fixed: bool, intents: &Intents) -> Column {
    let mut body = Column::new().with_intrinsic_height().with_no_padding();
    if !fixed {
        let kinds = [ConnKind::WireGuard, ConnKind::OpenVpn];
        let selected = kinds.iter().position(|k| *k == kind).unwrap_or(0);
        let intents = intents.clone();
        body = body.push(Box::new(
            ToggleGroup::new(
                kinds.iter().map(|k| Segment::new(k.long_name())).collect(),
                selected,
            )
            .with_id("vpn-import-kind")
            .on_change(move |i| {
                if let Some(kind) = kinds.get(i) {
                    intents.push(Intent::SetImportKind(*kind));
                }
            }),
        ));
    }
    body.push(Box::new(
        Text::new(&format!(
            "Choose the complete .{} file from your provider. Its keys, server and routes are \
             checked before NetworkManager saves the connection; nothing is saved if a check fails.",
            kind.ext()
        ))
        .with_role(TextRole::Body),
    ))
    .push(Box::new(
        Text::new("Keys stay in NetworkManager on this device and are never synced.")
            .with_role(TextRole::Body)
            .dimmed(),
    ))
}

/// The confirm sheet's body: what will happen, and the exact command.
pub(crate) fn confirm_body(summary: &str, command: &str) -> Column {
    Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .push(Box::new(Text::new(summary).with_role(TextRole::Body)))
        .push(Box::new(code_block(&shell_lines(command))))
}

/// A command as a shell would accept it. An argument longer than everything
/// before it (a profile id, a path) continues on its own line after a `\`,
/// so the sheet never has to break inside it.
fn shell_lines(command: &str) -> String {
    match command.rsplit_once(' ') {
        Some((head, last)) if last.chars().count() > head.chars().count() => {
            format!("{head} \\\n  {last}")
        }
        _ => command.to_owned(),
    }
}

// ---- Throughput chart ---------------------------------------------------------

/// A rate axis that ends on a round number of one binary unit, with the
/// ticks it is labelled at: `(ceiling, ticks)` in bytes per second.
fn rate_axis(peak: f64) -> (f64, Vec<AxisTick>) {
    const UNITS: [&str; 4] = ["B/s", "KiB/s", "MiB/s", "GiB/s"];
    const DIVISIONS: u32 = 4;
    let peak = peak.max(1.0);
    let (mut unit, mut scale) = (0, 1.0_f64);
    while peak / scale >= 1024.0 && unit + 1 < UNITS.len() {
        unit += 1;
        scale *= 1024.0;
    }
    // A step of 1, 2, 2.5 or 5 times a power of ten, so every tick reads
    // as a round number of the unit.
    let raw = peak / scale / f64::from(DIVISIONS);
    let magnitude = 10_f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 2.5, 5.0, 10.0]
        .into_iter()
        .map(|m| m * magnitude)
        .find(|step| *step >= raw)
        .unwrap_or(10.0 * magnitude);
    let name = UNITS.get(unit).copied().unwrap_or("B/s");
    let ticks = (0..=DIVISIONS)
        .map(|i| {
            let value = step * f64::from(i);
            let label = if value == 0.0 {
                "0".to_owned()
            } else if value.fract() == 0.0 {
                format!("{value:.0} {name}")
            } else {
                format!("{value:.1} {name}")
            };
            AxisTick::new(value * scale, label)
        })
        .collect();
    (step * f64::from(DIVISIONS) * scale, ticks)
}

/// The measured download and upload rates on one shared scale, the newest at
/// the trailing edge. The library plot draws axes, legend and inspection;
/// the series take the theme's ink, so a measurement reads as content and
/// not as a person or a status.
struct ThroughputPlot {
    down: Vec<f64>,
    up: Vec<f64>,
    plot: CartesianPlot,
    /// The ink the plot was built in; a new theme rebuilds it.
    ink: Option<(Color, Color)>,
    /// Whether keyboard focus is on the plot.
    focused: bool,
}

impl ThroughputPlot {
    fn new(down: Vec<f64>, up: Vec<f64>) -> Self {
        Self {
            down,
            up,
            plot: CartesianPlot::new(Vec::new()),
            ink: None,
            focused: false,
        }
    }

    fn series(id: &str, name: &str, color: Color, rates: &[f64]) -> PlotSeries {
        let data = rates
            .iter()
            .enumerate()
            .map(|(i, rate)| {
                PlotDatum::new(format!("{id}-{i}"), i as f64, *rate)
                    .with_detail(ui::format::rate(*rate))
            })
            .collect();
        PlotSeries::new(id, name, color, data)
            .kind(SeriesKind::Area)
            .sorted_by_x()
    }

    fn build(&self, theme: &Theme) -> CartesianPlot {
        let colors = &theme.tokens.colors;
        let peak = self
            .down
            .iter()
            .chain(&self.up)
            .copied()
            .fold(0.0, f64::max);
        let (ceiling, ticks) = rate_axis(peak);
        let last = self.down.len().max(self.up.len()).saturating_sub(1) as f64;
        let x_axis = PlotAxis {
            grid: false,
            ..PlotAxis::default()
        }
        .fixed(0.0, last.max(1.0))
        .with_ticks([AxisTick::new(last, "Now")]);
        // Every rate tick is labelled, the rules one heading apart at least.
        let y_axis = PlotAxis::default()
            .fixed(0.0, ceiling)
            .with_ticks(ticks)
            .with_tick_density(5, f64::from(theme.tokens.spacing.s8));
        let style = CartesianStyle {
            x_axis,
            y_axis,
            legend: LegendPosition::Top,
            interactive_legend: false,
            ..CartesianStyle::default()
        };
        CartesianPlot::new(vec![
            Self::series("download", "Download", colors.text_primary, &self.down),
            Self::series("upload", "Upload", colors.text_dim, &self.up),
        ])
        .styled(style)
        .with_height(theme.tokens.metrics.header_height * 5.0)
    }
}

impl ui::widget::Decorator for ThroughputPlot {
    type Inner = CartesianPlot;

    fn inner(&self) -> &CartesianPlot {
        &self.plot
    }

    fn inner_mut(&mut self) -> &mut CartesianPlot {
        &mut self.plot
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let colors = &theme.tokens.colors;
        let ink = (colors.text_primary, colors.text_dim);
        if self.ink != Some(ink) {
            self.plot = self.build(theme);
            self.ink = Some(ink);
        }
        Widget::build_layout(&mut self.plot, taffy, theme, node_map)
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        Widget::set_focus(&mut self.plot, focused);
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn handle_event(
        &mut self,
        event: &ui::winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        // The plot steps through its readings with the arrows, Escape and
        // Enter; those keys belong to the window unless the plot has focus.
        if !self.focused && matches!(event, ui::winit::event::WindowEvent::KeyboardInput { .. }) {
            return;
        }
        Widget::handle_event(&mut self.plot, event, ctx);
    }
}

/// The shared row retains hit testing and actions; its name announces this
/// device's observed tunnel state as well as the profile's shared identity.
struct ConnectionRow {
    row: ListRow,
    label: String,
}
impl ui::widget::Decorator for ConnectionRow {
    type Inner = ListRow;
    fn inner(&self) -> &ListRow {
        &self.row
    }
    fn inner_mut(&mut self) -> &mut ListRow {
        &mut self.row
    }
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.row.accessibility().map(|mut node| {
            node.name = Some(self.label.clone());
            node
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_formatted_rate_splits_into_value_and_unit() {
        assert_eq!(split_unit("6.4 MiB/s"), ("6.4".into(), "MiB/s".into()));
        assert_eq!(split_unit("Never"), ("Never".into(), String::new()));
    }

    #[test]
    fn a_row_names_the_server_without_port_or_transport() {
        assert_eq!(
            endpoint_host("lab.kestrel-research.org:443 (TCP)"),
            "lab.kestrel-research.org"
        );
        assert_eq!(
            endpoint_host("home.vandenberg.net:51820"),
            "home.vandenberg.net"
        );
        assert_eq!(endpoint_host("[2001:db8::1]:51820"), "2001:db8::1");
        assert_eq!(endpoint_host("2001:db8::1"), "2001:db8::1");
        assert_eq!(endpoint_host("vpn.example.net"), "vpn.example.net");
    }

    #[test]
    fn a_long_command_breaks_between_words_only() {
        let uuid = "8b1e47d2-0c93-4f6a-b5d8-71e2a9c34f60";
        let lines = shell_lines(&format!("nmcli connection up uuid {uuid}"));
        assert_eq!(lines, format!("nmcli connection up uuid \\\n  {uuid}"));
        assert_eq!(
            shell_lines("systemctl start tor.service"),
            "systemctl start tor.service"
        );
    }

    #[test]
    fn the_rate_axis_ends_on_a_round_number_of_one_unit() {
        const MIB: f64 = 1024.0 * 1024.0;
        let (ceiling, ticks) = rate_axis(7.6 * MIB);
        assert!((ceiling - 8.0 * MIB).abs() < 1.0);
        let labels: Vec<&str> = ticks.iter().map(|tick| tick.label.as_str()).collect();
        assert_eq!(labels, ["0", "2 MiB/s", "4 MiB/s", "6 MiB/s", "8 MiB/s"]);
        let (ceiling, _) = rate_axis(300.0 * 1024.0);
        assert!((ceiling - 400.0 * 1024.0).abs() < 1.0);
    }
}
