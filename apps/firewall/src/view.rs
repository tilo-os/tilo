//! The window's content, built from the toolkit's widgets.
//!
//! The zones are a [`NavList`] in the shell's sidebar; the selected zone is
//! one scrolling page: its state at a glance, then the rules (with the
//! add-rule form) or the services it lets through. Nothing here paints,
//! sizes or colours anything: every control says what it is and the theme
//! decides how it looks. Interaction leaves as an [`Intent`] (or, for a
//! switch, a changed [`Switches`] state) that the app applies to the shared
//! document.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ui::LocalState;
use ui::a11y::AccessNode;
use ui::icons::Icon;
use ui::text_render::TextKind;
use ui::tokens::{Rhythm, TextRole};
use ui::widget::data_table::{CellAlign, ColumnDef, ColumnWidth, DataTable, TableSort};
use ui::widget::toggle_group::Segment;
use ui::widget::{
    Badge, Banner, Button, ButtonRole, ChipStyle, Column, Decorator, Dropdown, Element, EmptyState,
    Field, FormControl, LabelPlacement, ModalDialog, NavItem, NavList, NavSection, Property,
    PropertyList, Row, Scrollable, Section, SegmentedStyle, Spacer, StatGroup, StatTile, Text,
    TextInput, TextInputKind, Toggle, ToggleGroup, Tone, Widget,
};

use crate::model::{Action, FirewallConfig, Proto, Rule, RuleId, ServiceCatalog, Zone, ZoneKind};

/// What a control in the window asks for. The app applies each one to the
/// shared document or to its own view state, the same way for a click, a
/// key or an accessibility action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Intent {
    SelectZone(ZoneKind),
    SelectRule(RuleId),
    SetTab(Tab),
    SetMatchService(bool),
    SetProto(Proto),
    SetAction(Action),
    OpenAddRule,
    CancelAddRule,
    AddRule,
    AskDelete(RuleId),
    ConfirmDelete,
    CancelDelete,
    MakeActive,
    Apply,
    Retry,
}

/// Rules that let traffic through come before the ones that stop it.
fn action_rank(action: Action) -> u8 {
    match action {
        Action::Allow => 0,
        Action::Deny => 1,
    }
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
}

/// What a switch on the page turns on or off.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SwitchKey {
    Rule(RuleId),
    Service(&'static str),
}

/// One switch: what it turns on or off, its state, and the value it shows.
type SwitchEntry = (SwitchKey, LocalState<bool>, bool);

/// The switches on the page, each bound to its own local state. The app
/// compares every state with the value the page was built from and applies
/// the ones the person flipped.
#[derive(Clone, Default)]
pub(crate) struct Switches(Rc<RefCell<Vec<SwitchEntry>>>);

impl Switches {
    pub(crate) fn clear(&self) {
        self.0.borrow_mut().clear();
    }

    /// The state a switch for `key` binds to, showing `value`.
    pub(crate) fn state(&self, key: SwitchKey, value: bool) -> LocalState<bool> {
        let mut entries = self.0.borrow_mut();
        if let Some((_, state, _)) = entries.iter().find(|(k, _, _)| *k == key) {
            return state.clone();
        }
        let state = LocalState::new(value);
        entries.push((key, state.clone(), value));
        state
    }

    /// The switches whose state differs from the value they were built
    /// with, and their new value. Each is reported once.
    pub(crate) fn flipped(&self) -> Vec<(SwitchKey, bool)> {
        let mut entries = self.0.borrow_mut();
        let mut out = Vec::new();
        for (key, state, shown) in entries.iter_mut() {
            let now = state.with(|on| *on);
            if now != *shown {
                *shown = now;
                out.push((key.clone(), now));
            }
        }
        out
    }
}

/// The two halves of a zone's page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tab {
    Rules,
    Services,
}

impl Tab {
    pub(crate) const ALL: [Tab; 2] = [Tab::Rules, Tab::Services];

    fn label(self) -> &'static str {
        match self {
            Tab::Rules => "Rules",
            Tab::Services => "Services",
        }
    }
}

/// The add-rule draft. The typed values live in local state so they
/// survive a rebuild of the page; the choices are the app's.
pub(crate) struct Draft {
    pub service: bool,
    pub proto: Proto,
    pub action: Action,
    pub service_id: LocalState<String>,
    pub port: LocalState<String>,
    pub source: LocalState<String>,
    pub port_error: Option<String>,
    pub source_error: Option<String>,
}

impl Draft {
    pub(crate) fn new() -> Self {
        Self {
            service: true,
            proto: Proto::Tcp,
            action: Action::Allow,
            service_id: LocalState::new(
                ServiceCatalog::ALL
                    .first()
                    .map_or_else(String::new, |s| s.id.to_string()),
            ),
            port: LocalState::new(String::new()),
            source: LocalState::new(String::new()),
            port_error: None,
            source_error: None,
        }
    }

    pub(crate) fn port(&self) -> String {
        self.port.with(|s| s.trim().to_string())
    }

    pub(crate) fn source(&self) -> String {
        self.source.with(|s| s.trim().to_string())
    }

    pub(crate) fn service_id(&self) -> String {
        self.service_id.with(Clone::clone)
    }
}

/// The zone list, in firewalld's order from least to most trusted; the
/// active zone says so on its row.
pub(crate) fn zone_list(config: &FirewallConfig, selected: ZoneKind, intents: &Intents) -> NavList {
    let zones: Vec<NavItem> = ZoneKind::ALL
        .iter()
        .filter_map(|kind| config.zone(*kind))
        .map(|zone: &Zone| {
            let item = NavItem::new(zone_id(zone.kind), zone.kind.title()).without_actions();
            if zone.kind == config.default_zone {
                item.status_label("Active", Tone::Success)
            } else {
                item
            }
        })
        .collect();
    let intents = intents.clone();
    NavList::new(vec![NavSection::untitled("zones", zones)])
        .selected(&zone_id(selected))
        .with_accessible_name("Firewall zones")
        .on_select(move |id| {
            if let Some(zone) = id.strip_prefix("zone:").and_then(ZoneKind::from_name) {
                intents.push(Intent::SelectZone(zone));
            }
        })
}

pub(crate) fn zone_id(zone: ZoneKind) -> String {
    format!("zone:{}", zone.name())
}

/// The content header's controls: the firewall's two switches and Apply.
pub(crate) fn header_actions(
    intents: &Intents,
    enabled: &LocalState<bool>,
    panic: &LocalState<bool>,
) -> Vec<Element> {
    let apply = intents.clone();
    vec![
        Box::new(LabelledSwitch::new(
            "Firewall",
            "firewall-enabled",
            enabled.clone(),
        )),
        Box::new(LabelledSwitch::new(
            "Panic mode",
            "firewall-panic",
            panic.clone(),
        )),
        Box::new(
            Button::new("firewall-apply", "Apply")
                .role(ButtonRole::Primary)
                .tooltip("Apply the configuration to the system")
                .on_effect(move || apply.push(Intent::Apply)),
        ),
    ]
}

/// A switch with its name beside it, as one header action: the row keeps
/// the name and the switch together, and the overflow menu lists it by that
/// name with its state.
struct LabelledSwitch {
    row: Row,
    name: &'static str,
    id: String,
    state: LocalState<bool>,
}

impl LabelledSwitch {
    fn new(name: &'static str, id: &str, state: LocalState<bool>) -> Self {
        let row = Row::new()
            .no_wrap()
            .push(Box::new(Text::new(name).with_role(TextRole::Label)))
            .push(Box::new(
                Toggle::from_state(&state)
                    .with_id(id)
                    .with_accessible_name(name),
            ));
        Self {
            row,
            name,
            id: id.to_string(),
            state,
        }
    }
}

impl Decorator for LabelledSwitch {
    type Inner = Row;

    fn inner(&self) -> &Row {
        &self.row
    }

    fn inner_mut(&mut self) -> &mut Row {
        &mut self.row
    }

    fn accessibility(&self) -> Option<AccessNode> {
        Some(
            ui::widget::toggle_accessibility(
                self.state.with(|on| *on),
                false,
                true,
                Some(self.name),
            )
            .with_semantic_key(&self.id)
            .with_action_target(&self.id),
        )
    }

    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ui::widget::ModelPath,
    ) -> bool {
        match action {
            ui::a11y::Action::Activate | ui::a11y::Action::ToggleCheck => {
                self.state.update(|on| *on = !*on);
                true
            }
            _ => self.row.perform_action(action, state, path),
        }
    }
}

/// Nothing to show: firewalld cannot be read. One sentence, one action.
pub(crate) fn unavailable(detail: Option<&str>, intents: &Intents) -> Element {
    let state = match detail {
        Some(detail) => EmptyState::error("Firewall service unavailable", detail),
        None => EmptyState::new("Firewall service unavailable")
            .with_message("Start firewalld, then try again. No rules were changed."),
    };
    let intents = intents.clone();
    Box::new(
        state
            .with_icon(Icon::Shield)
            .with_action("Try again")
            .with_action_id("firewall.retry")
            .on_effect(move || intents.push(Intent::Retry)),
    )
}

/// Everything a zone's page shows, borrowed from the app.
pub(crate) struct PageModel<'a> {
    pub config: &'a FirewallConfig,
    pub zone: &'a Zone,
    pub tab: Tab,
    pub selected_rule: Option<RuleId>,
    /// The order a person chose for the rules, kept across rebuilds.
    pub rule_sort: &'a LocalState<Option<TableSort>>,
    pub pending_delete: Option<&'a Rule>,
    pub draft: &'a Draft,
    /// The add-rule dialog is open.
    pub adding: bool,
    /// The content column is wide enough for the services' descriptions.
    pub wide: bool,
}

/// The selected zone's page.
pub(crate) fn zone_page(
    model: &PageModel<'_>,
    intents: &Intents,
    switches: &Switches,
    offset: &Rc<Cell<(f32, f32)>>,
) -> Element {
    // The banners, the zone's state, its numbers, its details and the
    // tabbed rules are groups of the page, a group apart; a tab sits a
    // sibling step over its content.
    let mut page = Column::new()
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Group);
    let banners = banners(model.config);
    if !banners.is_empty() {
        page = page.push(Box::new(banners.into_iter().fold(
            Column::new().with_no_padding().with_intrinsic_height(),
            Column::push,
        )));
    }
    let tab = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(Box::new(tab_bar(model.tab, intents)));
    let tab = match model.tab {
        Tab::Rules => tab.push(rules(model, intents, switches)),
        Tab::Services => tab.push(Box::new(services(model.zone, model.wide, switches))),
    };
    page = page
        .push(Box::new(zone_status(model, intents)))
        .push(Box::new(stats(model.zone)))
        .push(Box::new(details(model.zone)))
        .push(Box::new(tab));
    if model.adding {
        page = page.push(Box::new(add_rule_dialog(
            model.zone.kind,
            model.draft,
            intents,
        )));
    }
    if let Some(rule) = model.pending_delete {
        page = page.push(Box::new(delete_dialog(rule, intents)));
    }
    Box::new(
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_persisted_offset(Rc::clone(offset))
            .with_child(Box::new(page)),
    )
}

/// The firewall-wide states that change what every rule means.
fn banners(config: &FirewallConfig) -> Vec<Element> {
    let mut out: Vec<Element> = Vec::new();
    if config.panic_mode {
        out.push(Box::new(Banner::error("Panic mode is on").with_message(
            "All incoming and outgoing network traffic is dropped.",
        )));
    }
    if !config.enabled {
        out.push(Box::new(
            Banner::warning("The firewall is stopped")
                .with_message("No rules are enforced until you turn it on."),
        ));
    }
    out
}

/// Whether this zone is the active one, and what it is for.
fn zone_status(model: &PageModel<'_>, intents: &Intents) -> Column {
    let active = model.zone.kind == model.config.default_zone;
    let status: Element = if active {
        Box::new(Badge::text("Active zone").style(ChipStyle::Tinted(Tone::Success)))
    } else {
        let intents = intents.clone();
        Box::new(
            Button::new("firewall-make-active", "Make active zone")
                .on_effect(move || intents.push(Intent::MakeActive)),
        )
    };
    Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .push(Box::new(Row::new().push(status)))
        .push(Box::new(
            Text::new(model.zone.kind.description())
                .with_role(TextRole::Body)
                .dimmed(),
        ))
}

fn stats(zone: &Zone) -> StatGroup {
    let (allow, deny) = zone.rule_counts();
    StatGroup::new(vec![
        Box::new(StatTile::new(
            "Allowed services",
            zone.services.len().to_string(),
        )),
        Box::new(StatTile::new("Allow rules", allow.to_string())),
        Box::new(StatTile::new("Deny rules", deny.to_string())),
    ])
}

fn details(zone: &Zone) -> Section {
    let interfaces = if zone.interfaces.is_empty() {
        Property::new("Interfaces", "None")
    } else {
        Property::new("Interfaces", zone.interfaces.join(", ")).with_kind(TextKind::Identifier)
    };
    let masquerading = if zone.masquerade { "On" } else { "Off" };
    Section::titled(
        "Details",
        vec![Box::new(PropertyList::new(vec![
            interfaces,
            Property::new("Unmatched traffic", zone.target.outcome()),
            Property::new("Masquerading", masquerading),
        ]))],
    )
}

/// The page's two halves, and on the rules half the command that adds one.
fn tab_bar(selected: Tab, intents: &Intents) -> Row {
    let tab_intents = intents.clone();
    let index = Tab::ALL
        .iter()
        .position(|tab| *tab == selected)
        .unwrap_or(0);
    let tabs = ToggleGroup::new(
        Tab::ALL
            .iter()
            .map(|tab| Segment::new(tab.label()))
            .collect(),
        index,
    )
    .with_style(SegmentedStyle::Tabs)
    .on_view("firewall-tabs", move |i| {
        if let Some(tab) = Tab::ALL.get(i) {
            tab_intents.push(Intent::SetTab(*tab));
        }
    });
    let bar = Row::new().no_wrap().push(Box::new(tabs));
    if selected != Tab::Rules {
        return bar;
    }
    let add = intents.clone();
    bar.push(Box::new(Spacer::new())).push(Box::new(
        Button::new("firewall-rule-new", "Add rule").on_view(move || add.push(Intent::OpenAddRule)),
    ))
}

// ---- rules ---------------------------------------------------------------

/// One row of the rules table.
#[derive(Clone)]
struct RuleRow {
    id: RuleId,
    title: String,
    protocol: &'static str,
    action: Action,
    source: Option<String>,
    enabled: bool,
}

impl RuleRow {
    fn of(rule: &Rule) -> Self {
        let source = rule.source.trim();
        Self {
            id: rule.id,
            title: rule.matcher.title(),
            protocol: rule.matcher.proto_title(),
            action: rule.action,
            source: (!source.is_empty()).then(|| source.to_string()),
            enabled: rule.enabled,
        }
    }
}

/// The zone's rules in one list: what each does is a badge in its own
/// column, and the list sorts by any of its text columns.
fn rules(model: &PageModel<'_>, intents: &Intents, switches: &Switches) -> Element {
    if model.zone.rules.is_empty() {
        return Box::new(
            Text::new("This zone has no rules yet.")
                .with_role(TextRole::Body)
                .dimmed(),
        );
    }
    let rows: Vec<RuleRow> = model.zone.rules.iter().map(RuleRow::of).collect();
    Box::new(rule_table(
        rows,
        model.rule_sort,
        model.selected_rule,
        intents,
        switches,
    ))
}

fn rule_table(
    rows: Vec<RuleRow>,
    sort: &LocalState<Option<TableSort>>,
    selected: Option<RuleId>,
    intents: &Intents,
    switches: &Switches,
) -> DataTable<RuleRow> {
    let toggles = switches.clone();
    let delete = intents.clone();
    let mut table = DataTable::new(vec![
        ColumnDef::new("action", "Action", ColumnWidth::Auto)
            .sorted_by(|a: &RuleRow, b: &RuleRow| action_rank(a.action).cmp(&action_rank(b.action)))
            .with_align(CellAlign::Center)
            .with_badge(|row: &RuleRow| {
                let tone = match row.action {
                    Action::Allow => Tone::Success,
                    Action::Deny => Tone::Danger,
                };
                (row.action.title().to_string(), tone)
            }),
        ColumnDef::new("rule", "Rule", ColumnWidth::Flex(1.0))
            .min_width(160.0)
            .sortable(true)
            .with_text(|row: &RuleRow| row.title.clone()),
        ColumnDef::new("protocol", "Protocol", ColumnWidth::Auto)
            .priority(1)
            .sortable(true)
            .with_text(|row: &RuleRow| row.protocol.to_string()),
        ColumnDef::new("source", "Source", ColumnWidth::Flex(1.0))
            .min_width(160.0)
            .priority(2)
            .sortable(true)
            .with_kind(TextKind::Identifier)
            .with_optional_text(|row: &RuleRow| row.source.clone(), "Anywhere"),
        ColumnDef::new("enabled", "Enabled", ColumnWidth::Auto)
            .with_align(CellAlign::Center)
            .with_accessible_value(|row: &RuleRow| {
                if row.enabled { "On" } else { "Off" }.to_string()
            })
            .with_widget(move |row: &RuleRow, _| {
                let state = toggles.state(SwitchKey::Rule(row.id), row.enabled);
                Box::new(
                    Toggle::from_state(&state)
                        .with_id(&format!("firewall-rule-enabled-{}", row.id))
                        .with_accessible_name(&format!("Enable the {} rule", row.title)),
                )
            }),
        ColumnDef::new("delete", "", ColumnWidth::Auto).with_widget(move |row: &RuleRow, _| {
            let intents = delete.clone();
            let id = row.id;
            Box::new(
                Button::icon_only(
                    &format!("firewall-rule-delete-{id}"),
                    Icon::Trash2,
                    &format!("Delete the {} rule", row.title),
                )
                .on_view(move || intents.push(Intent::AskDelete(id))),
            )
        }),
    ])
    .with_id("firewall-rules")
    .with_accessible_name("Rules")
    .with_sort_state(sort)
    .selectable(true);
    let ids: Vec<RuleId> = rows.iter().map(|row| row.id).collect();
    table.set_rows(rows);
    if let Some(index) = selected.and_then(|id| ids.iter().position(|row| *row == id)) {
        table.select_row(index);
    }
    let select = intents.clone();
    table.on_row_click(move |index, _, _| {
        if let Some(id) = ids.get(index) {
            select.push(Intent::SelectRule(*id));
        }
    })
}

/// A segmented choice that takes its accessible name from the form field
/// it sits in, like the toolkit's own form controls.
struct Choice {
    group: ToggleGroup,
    name: Option<String>,
}

impl Choice {
    fn new(group: ToggleGroup) -> Self {
        Self { group, name: None }
    }
}

impl Decorator for Choice {
    type Inner = ToggleGroup;

    fn inner(&self) -> &ToggleGroup {
        &self.group
    }

    fn inner_mut(&mut self) -> &mut ToggleGroup {
        &mut self.group
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let node = self.group.accessibility()?;
        Some(match &self.name {
            Some(name) => node.with_name(name.clone()),
            None => node,
        })
    }
}

impl FormControl for Choice {
    fn with_form_accessible_name(mut self, name: &str) -> Self {
        self.name = Some(name.to_string());
        self
    }
}

fn choice<T: Copy + PartialEq + 'static>(
    id: &str,
    options: &[(T, &str)],
    selected: T,
    intents: &Intents,
    intent: fn(T) -> Intent,
) -> Choice {
    let values: Vec<T> = options.iter().map(|(value, _)| *value).collect();
    let index = values.iter().position(|v| *v == selected).unwrap_or(0);
    let intents = intents.clone();
    Choice::new(
        ToggleGroup::new(
            options
                .iter()
                .map(|(_, label)| Segment::new(*label))
                .collect(),
            index,
        )
        .on_view(id, move |i| {
            if let Some(value) = values.get(i) {
                intents.push(intent(*value));
            }
        }),
    )
}

/// A field of the add-rule dialog: its label above the control, so every
/// control in the dialog starts on one edge.
fn stacked<C: FormControl + 'static>(label: &str, control: C) -> Field {
    Field::new(label, control).with_label_placement(Some(LabelPlacement::Above))
}

/// A field whose control keeps its own width (a segmented choice).
fn choice_field(label: &str, control: Choice) -> Field {
    stacked(label, control).with_trailing_control()
}

/// The add-rule dialog: what the rule matches, what it does and where the
/// traffic comes from. Each field says what is wrong with it.
fn add_rule_dialog(zone: ZoneKind, draft: &Draft, intents: &Intents) -> ModalDialog<Column> {
    let mut fields: Vec<Field> = vec![choice_field(
        "Match",
        choice(
            "firewall-rule-match",
            &[(true, "Service"), (false, "Port")],
            draft.service,
            intents,
            Intent::SetMatchService,
        ),
    )];
    if draft.service {
        let mut services =
            Dropdown::from_state(Vec::new(), &draft.service_id).with_id("firewall-rule-service");
        services.set_value_options(
            ServiceCatalog::ALL
                .iter()
                .map(|s| (s.id.to_string(), s.name.to_string()))
                .collect(),
        );
        fields.push(stacked("Service", services));
    } else {
        let mut port = stacked(
            "Port",
            TextInput::from_state(&draft.port)
                .with_id("firewall-rule-port")
                .with_kind(TextInputKind::Number)
                .with_placeholder("8080 or 8000-8100"),
        );
        if let Some(error) = &draft.port_error {
            port = port.with_error(error);
        }
        fields.push(port);
        fields.push(choice_field(
            "Protocol",
            choice(
                "firewall-rule-protocol",
                &[(Proto::Tcp, "TCP"), (Proto::Udp, "UDP")],
                draft.proto,
                intents,
                Intent::SetProto,
            ),
        ));
    }
    fields.push(choice_field(
        "Action",
        choice(
            "firewall-rule-action",
            &[(Action::Allow, "Allow"), (Action::Deny, "Deny")],
            draft.action,
            intents,
            Intent::SetAction,
        ),
    ));
    let source = stacked(
        "Source",
        TextInput::from_state(&draft.source)
            .with_id("firewall-rule-source")
            .with_kind(TextInputKind::Number)
            .with_placeholder("Anywhere"),
    );
    fields.push(match &draft.source_error {
        Some(error) => source.with_error(error),
        None => source.with_help("An IPv4 address or network, such as 192.168.1.0/24"),
    });
    let body = fields.into_iter().fold(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(Rhythm::Group),
        |body, field| body.push(Box::new(field)),
    );
    let add = intents.clone();
    let cancel = intents.clone();
    let dismiss = intents.clone();
    ModalDialog::new(format!("Add a rule to {}", zone.title()), body)
        .with_test_id("firewall-add-rule-dialog")
        .primary_action("Add rule", move || add.push(Intent::AddRule))
        .secondary_action("Cancel", move || cancel.push(Intent::CancelAddRule))
        .on_dismiss(move || dismiss.push(Intent::CancelAddRule))
        .open(true)
}

/// Asks before a rule is deleted; the filled destructive face lives here.
fn delete_dialog(rule: &Rule, intents: &Intents) -> ModalDialog<Text> {
    let what = RuleRow::of(rule);
    let source = what.source.as_deref().unwrap_or("anywhere");
    let body = format!(
        "{} over {} from {} will no longer be {}.",
        what.title,
        what.protocol,
        source,
        match what.action {
            Action::Allow => "allowed",
            Action::Deny => "denied",
        }
    );
    let confirm = intents.clone();
    let cancel = intents.clone();
    let dismiss = intents.clone();
    ModalDialog::new(
        "Delete this rule?",
        Text::new(&body).with_role(TextRole::Body),
    )
    .with_test_id("firewall-delete-dialog")
    .with_destructive(true)
    .primary_action("Delete rule", move || confirm.push(Intent::ConfirmDelete))
    .secondary_action("Cancel", move || cancel.push(Intent::CancelDelete))
    .on_dismiss(move || dismiss.push(Intent::CancelDelete))
    .open(true)
}

// ---- services ------------------------------------------------------------

/// One row of the services table.
#[derive(Clone)]
struct ServiceRow {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    ports: &'static str,
    protocol: &'static str,
    allowed: bool,
}

/// The catalog's services, each with a switch that lets it through the zone.
/// A column too narrow for the descriptions leaves them out rather than
/// cutting each one short.
fn services(zone: &Zone, wide: bool, switches: &Switches) -> DataTable<ServiceRow> {
    let toggles = switches.clone();
    let mut columns = Vec::new();
    if wide {
        columns.push(
            ColumnDef::new("service", "Service", ColumnWidth::Auto)
                .with_text(|row: &ServiceRow| row.name.to_string()),
        );
        columns.push(
            ColumnDef::new("description", "Description", ColumnWidth::Flex(1.0))
                .with_text(|row: &ServiceRow| row.description.to_string()),
        );
    } else {
        columns.push(
            ColumnDef::new("service", "Service", ColumnWidth::Flex(1.0))
                .with_text(|row: &ServiceRow| row.name.to_string()),
        );
    }
    columns.extend([
        ColumnDef::new("ports", "Ports", ColumnWidth::Auto)
            .with_kind(TextKind::Numeric)
            .with_text(|row: &ServiceRow| row.ports.to_string()),
        ColumnDef::new("protocol", "Protocol", ColumnWidth::Auto)
            .with_text(|row: &ServiceRow| row.protocol.to_string()),
        ColumnDef::new("allowed", "Allowed", ColumnWidth::Auto)
            .with_align(CellAlign::Center)
            .with_accessible_value(|row: &ServiceRow| {
                if row.allowed { "On" } else { "Off" }.to_string()
            })
            .with_widget(move |row: &ServiceRow, _| {
                let state = toggles.state(SwitchKey::Service(row.id), row.allowed);
                Box::new(
                    Toggle::from_state(&state)
                        .with_id(&format!("firewall-service-{}", row.id))
                        .with_accessible_name(&format!("Allow {}", row.name)),
                )
            }),
    ]);
    let mut table = DataTable::new(columns)
        .with_id("firewall-services")
        .with_accessible_name("Services");
    table.set_rows(
        ServiceCatalog::ALL
            .iter()
            .map(|service| ServiceRow {
                id: service.id,
                name: service.name,
                description: service.description,
                ports: service.ports,
                protocol: ServiceCatalog::proto_title(service.proto),
                allowed: zone.has_service(service.id),
            })
            .collect(),
    );
    table
}
