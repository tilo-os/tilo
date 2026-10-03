//! The window's content, built from the toolkit's widgets.
//!
//! The saved connections are the shell's overview list; the selected
//! connection is one scrolling page of badges, tabs, stat tiles, the
//! throughput chart and its details. Nothing here sizes or colours chrome:
//! every control says what it is and the theme decides how it looks. The one
//! piece of content drawing is the throughput plot, painted by the library
//! chart from theme values. Interaction leaves as an [`Intent`] the app
//! applies to the shared document and the machine.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use ui::LocalState;
use ui::charts::{Chart, ChartLabels, ChartStyle, Series, TrendRange};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::{TextKind, TextRenderer};
use ui::theme::Theme;
use ui::tokens::{SpacingStep, TextRole};
use ui::widget::toggle_group::Segment;
use ui::widget::{
    Badge, Banner, Button, ButtonRole, ChipStyle, Column, Container, Element, EmptyState, Field,
    FormSection, ListRow, Property, PropertyList, Row, Scrollable, Section, SegmentedStyle, Spacer,
    StatGroup, StatTile, TaffyTreeExt, Text, Toggle, ToggleGroup, Tone, Widget,
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
#[derive(Clone, Default)]
pub(crate) struct Intents(Rc<RefCell<Vec<Intent>>>);

impl Intents {
    pub(crate) fn push(&self, intent: Intent) {
        self.0.borrow_mut().push(intent);
    }

    pub(crate) fn take(&self) -> Vec<Intent> {
        std::mem::take(&mut *self.0.borrow_mut())
    }

    pub(crate) fn sender(&self, intent: Intent) -> impl FnMut() + 'static {
        let intents = self.clone();
        move || intents.push(intent)
    }
}

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

/// The saved connections, then the window-wide routing preference.
pub(crate) fn overview(
    model: &OverviewModel<'_>,
    intents: &Intents,
    offset: &Rc<Cell<(f32, f32)>>,
) -> Element {
    let rows: Vec<Element> = model
        .entries
        .iter()
        .map(|entry| -> Element { Box::new(connection_row(entry, model.selected, intents)) })
        .collect();
    let tor = Toggle::from_state(&LocalState::new(model.tor_global))
        .with_id("vpn-tor-global")
        .on_change({
            let intents = intents.clone();
            move |_| intents.push(Intent::ToggleTorGlobal)
        });
    let routing = FormSection::new("Routing")
        .grouped()
        .field(Field::new("Route all traffic over Tor", tor));
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

/// One saved connection: its protocol, endpoint, when it was last used and
/// whether it is up.
fn connection_row(entry: &ListEntry<'_>, selected: Option<ConnId>, intents: &Intents) -> ListRow {
    let connection = entry.connection;
    let id = connection.id;
    let secondary = entry
        .details
        .map_or("Not set up on this device", |details| {
            details.endpoint.as_str()
        });
    let row = ListRow::new(format!("vpn-connection-{id}"), &connection.name)
        .badge(connection.kind.badge())
        .secondary(secondary)
        .selected(selected == Some(id))
        .on_activate(intents.sender(Intent::Select(id)));
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
        .on_click(intents.sender(Intent::OpenImport))
}

/// Nothing saved yet: one sentence and the one way forward.
pub(crate) fn empty_state(intents: &Intents) -> Element {
    Box::new(ImportOnAction {
        state: EmptyState::new("No VPN connections")
            .with_icon(Icon::Shield)
            .with_message("Import a WireGuard or OpenVPN configuration from your provider.")
            .with_action("Import configuration"),
        intents: intents.clone(),
    })
}

/// An empty state whose one action opens the import sheet.
struct ImportOnAction {
    state: EmptyState,
    intents: Intents,
}

impl ImportOnAction {
    fn forward(&mut self) {
        if self.state.take_action() {
            self.intents.push(Intent::OpenImport);
        }
    }
}

impl ui::widget::Decorator for ImportOnAction {
    type Inner = EmptyState;

    fn inner(&self) -> &EmptyState {
        &self.state
    }

    fn inner_mut(&mut self) -> &mut EmptyState {
        &mut self.state
    }

    fn handle_event(
        &mut self,
        event: &ui::winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        Widget::handle_event(&mut self.state, event, ctx);
        self.forward();
    }

    fn update(&mut self, state: &mut ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        self.forward();
        Widget::update(&mut self.state, state, path)
    }
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
            let mut actions = Row::new().no_wrap();
            if notice.retry {
                actions = actions.push(Box::new(
                    Button::new("vpn-retry-import", "Try again", None)
                        .control_size(ui::widget::ControlSize::Small)
                        .on_click(intents.sender(Intent::RetryImport)),
                ));
            }
            actions = actions.push(Box::new(
                Button::new("vpn-dismiss-error", "Dismiss", None)
                    .role(ButtonRole::Plain)
                    .control_size(ui::widget::ControlSize::Small)
                    .on_click(intents.sender(Intent::DismissError)),
            ));
            Box::new(Section::new(vec![
                Box::new(Banner::error(notice.message)),
                Box::new(actions),
            ]))
        }
    }
}

/// Everything the detail page shows, borrowed from the app.
pub(crate) struct DetailModel<'a> {
    pub connection: &'a Connection,
    pub details: Option<&'a LocalProfileDetails>,
    pub provisioned: bool,
    pub tab: DetailTab,
    pub rates: &'a Rates,
    pub activity: Vec<String>,
    pub notice: Option<Notice<'a>>,
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
    page = page.push(Box::new(summary(model, intents)));
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

/// The status and protocol at a glance, and the connection's main action.
fn summary(model: &DetailModel<'_>, intents: &Intents) -> Row {
    let connection = model.connection;
    let mut row = Row::new()
        .no_wrap()
        .with_align_items(taffy::AlignItems::Center)
        .push(Box::new(status_badge(connection.status)))
        .push(Box::new(
            Badge::text(connection.kind.long_name()).style(ChipStyle::Neutral),
        ));
    if connection.tor {
        row = row.push(Box::new(Badge::text("Tor").style(ChipStyle::Neutral)));
    }
    let action = MainAction::of(connection, model.provisioned);
    let role = match action {
        MainAction::Disconnect => ButtonRole::Secondary,
        _ => ButtonRole::Primary,
    };
    row.push(Box::new(Spacer::new())).push(Box::new(
        Button::new("vpn-main-action", action.label(), None)
            .role(role)
            .with_enabled(action != MainAction::Connecting)
            .on_click(intents.sender(Intent::MainAction(connection.id))),
    ))
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
    .with_style(SegmentedStyle::Tabs)
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
    let tor = Toggle::from_state(&LocalState::new(connection.tor))
        .with_id("vpn-tor")
        .on_change({
            let intents = intents.clone();
            let id = connection.id;
            move |_| intents.push(Intent::ToggleTor(id))
        });
    page = page.push(Box::new(
        FormSection::new("Routing")
            .grouped()
            .field(Field::new("Route over Tor", tor)),
    ));
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
    Box::new(ThroughputChart {
        down: rates.down.iter().map(|r| *r as f32).collect(),
        up: rates.up.iter().map(|r| *r as f32).collect(),
        rect: Rect::ZERO,
    })
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
    Box::new(code_block(&render_config(model.connection, details)))
}

fn activity(model: &DetailModel<'_>) -> Element {
    Box::new(code_block(&model.activity.join("\n")))
}

/// Monospaced text on a well: configuration and activity read as they are.
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
        .push(Box::new(code_block(command)))
}

// ---- Throughput chart ---------------------------------------------------------

/// The measured download and upload rates on one shared scale, drawn by the
/// library chart from theme values.
struct ThroughputChart {
    down: Vec<f32>,
    up: Vec<f32>,
    rect: Rect,
}

impl ThroughputChart {
    fn peak(&self) -> f64 {
        f64::from(
            self.down
                .iter()
                .chain(&self.up)
                .copied()
                .fold(1.0_f32, f32::max),
        )
    }
}

impl Widget for ThroughputChart {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let height = theme.tokens.metrics.header_height * 3.0;
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(height),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::from_origin_size(
            (
                origin.x + f64::from(l.location.x),
                origin.y + f64::from(l.location.y),
            ),
            (f64::from(l.size.width), f64::from(l.size.height)),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let colors = &theme.tokens.colors;
        let series = [
            Series::new(self.down.clone(), colors.presence.series(0)).with_legend("Download"),
            Series::new(self.up.clone(), colors.presence.series(1)).with_legend("Upload"),
        ];
        // Headroom over the peak so the newest reading never rides the top rule.
        let ceiling = self.peak() * 1.2;
        let top = ui::format::rate(ceiling);
        let style = ChartStyle::default().with_grid(
            ChartStyle::default().grid_divisions,
            Some(ui::surface::edge_on(colors.surfaces.content)),
        );
        Chart::draw_styled(
            &mut PaintCtx::new(scene, transform, text, theme),
            self.rect,
            &series,
            TrendRange::up_to(ceiling),
            ChartLabels {
                title: "",
                axis_hi: &top,
                axis_lo: "0",
            },
            &style,
        );
    }

    fn name(&self) -> &'static str {
        "ThroughputChart"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let last = |series: &[f32]| {
            series.last().map_or_else(
                || ui::format::rate(0.0),
                |r| ui::format::rate(f64::from(*r)),
            )
        };
        Some(
            ui::a11y::AccessNode::new(ui::a11y::Role::Image)
                .with_name("Throughput")
                .with_description(format!(
                    "Download {}, upload {}",
                    last(&self.down),
                    last(&self.up)
                ))
                .with_bounds(self.rect),
        )
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
}
